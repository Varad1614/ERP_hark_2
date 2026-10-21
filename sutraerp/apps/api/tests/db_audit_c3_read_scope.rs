//! Audit C3 (M-2 / C3 / M9) DB tests: read scope.
//!
//! * Faculty hold `student.read.assigned`, not `student.read`: they read only the students
//!   holding an `enrolled` seat in a section where they have an **active** faculty assignment
//!   (directory, Student 360, status history, registrations, progression decisions); any other
//!   student is `404`; admissions, documents, ledgers, guardians, registration windows and the
//!   student search are `403`; ending the assignment ends the access.
//! * Administrative academic configuration (credit-mapping rules, reconciliation runs,
//!   faculty-assignment policies) needs the manage permission (faculty `403`).
//! * Student-only sessions read the public institution subset (no administrator e-mail /
//!   registration number, cached copy included), no profile revisions, and the entitlement
//!   summary (modules + status, commercial terms blanked).
//! * M9: `org_type_blueprint_unit_type` / `org_restructure_move` carry `tenant_id` +
//!   `institution_id` (NOT NULL) under forced RLS, filled from the parent on insert.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use support::*;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const S: &str = "/api/v1/student";
const A: &str = "/api/v1/academic";
const ENT: &str = "/api/v1/platform/entitlement";
/// A second faculty member (no dev persona: served through an injected session).
const RAVI: &str = "dev|ravi.kulkarni@sutra.dev";

fn tenant() -> &'static str {
    Box::leak(Uuid::now_v7().to_string().into_boxed_str())
}

async fn status(app: &Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

async fn expect(app: &Router, who: &Login, path: &str, want: StatusCode) -> Value {
    let (s, b) = status(app, who, path).await;
    assert_eq!(s, want, "GET {path}: {b}");
    b
}

fn ids(list: &Value) -> Vec<String> {
    let mut v: Vec<String> = list["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items in {list}"))
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_owned())
        .collect();
    v.sort();
    v
}

/// Domain C router signed in as a faculty member `subject` (no cookie / CSRF layer).
fn faculty_router(pool: &sqlx::PgPool, tenant: &str, subject: &str) -> Router {
    let st = sutra_api::app::student_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    let (router, _) = sutra_student::router_with_state(st).split_for_parts();
    let ctx = AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![Role::Faculty],
        scope: ExecutionScope::tenant(tenant.parse::<TenantId>().unwrap()),
        csrf_token: "x".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        idle_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        mfa_level: MfaLevel::None,
        amr: vec!["pwd".into()],
        step_up_enforced: false,
        privileged_mfa_enforced: false,
    };
    router.layer(axum::middleware::from_fn(
        move |mut req: Request<Body>, next: axum::middleware::Next| {
            let ctx = ctx.clone();
            async move {
                req.extensions_mut().insert(ctx);
                next.run(req).await
            }
        },
    ))
}

fn injected() -> Login {
    Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    }
}

#[tokio::test]
async fn faculty_read_only_the_students_of_their_own_sections() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let setup = b4::academic_setup(&app, &admin, &pool, t, "Read Scope University").await;
    let inst = setup.base.institution_id;

    // Meera teaches CS101 (Asha seated), Ravi teaches MD101 (Bilal seated); Chitra sits nowhere.
    let cs = b4::seed_offering(&pool, &setup, "CS101", "active").await;
    let md = b4::seed_offering(&pool, &setup, "MD101", "active").await;
    b4::seed_faculty(&pool, &setup, &cs, b4::MEERA, "active").await;
    b4::seed_faculty(&pool, &setup, &md, RAVI, "active").await;
    let asha = b4::seed_student(&pool, &setup, "Asha", None).await;
    let bilal = b4::seed_student(&pool, &setup, "Bilal", None).await;
    let chitra = b4::seed_student(&pool, &setup, "Chitra", None).await;
    b4::seed_enrolment(&pool, &setup, &cs, &asha, None).await;
    b4::seed_enrolment(&pool, &setup, &md, &bilal, None).await;
    let (a, b, c) = (
        asha.student_id.to_string(),
        bilal.student_id.to_string(),
        chitra.student_id.to_string(),
    );
    let dir = format!("{S}/institutions/{inst}/students");

    // Administrators read every student.
    let all = ids(&expect(&app, &admin, &dir, StatusCode::OK).await);
    for s in [&a, &b, &c] {
        assert!(all.contains(s), "admin directory lacks {s}");
    }

    // Meera: only Asha.
    assert_eq!(
        ids(&expect(&app, &meera, &dir, StatusCode::OK).await),
        vec![a.clone()]
    );
    for path in [
        format!("{S}/students/{a}"),
        format!("{S}/students/{a}/status-history"),
        format!("{S}/students/{a}/registrations"),
        format!("{S}/students/{a}/progression-decisions"),
    ] {
        expect(&app, &meera, &path, StatusCode::OK).await;
    }
    for s in [&b, &c] {
        for path in [
            format!("{S}/students/{s}"),
            format!("{S}/students/{s}/status-history"),
            format!("{S}/students/{s}/registrations"),
            format!("{S}/students/{s}/progression-decisions"),
        ] {
            expect(&app, &meera, &path, StatusCode::NOT_FOUND).await;
        }
    }
    // Policies are institution rules (any student read).
    expect(
        &app,
        &meera,
        &format!("{S}/institutions/{inst}/progression-policies"),
        StatusCode::OK,
    )
    .await;
    // Everything else stays `student.read` — even for a student Meera teaches.
    for path in [
        format!("{S}/institutions/{inst}/admission-applications"),
        format!("{S}/institutions/{inst}/registration-windows"),
        format!("{S}/students/{a}/documents"),
        format!("{S}/students/{a}/credit-ledger"),
        format!("{S}/students/{a}/progression"),
        format!("{S}/students/{a}/guardian-relationships"),
        "/api/v1/platform/search?projection=students&q=as".to_owned(),
    ] {
        let (s, body) = status(&app, &meera, &path).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "GET {path}: {body}");
    }

    // Ravi (second faculty member): only Bilal.
    let ravi = faculty_router(&pool, t, RAVI);
    let who = injected();
    assert_eq!(
        ids(&expect(&ravi, &who, &dir, StatusCode::OK).await),
        vec![b.clone()]
    );
    expect(&ravi, &who, &format!("{S}/students/{b}"), StatusCode::OK).await;
    expect(
        &ravi,
        &who,
        &format!("{S}/students/{a}"),
        StatusCode::NOT_FOUND,
    )
    .await;

    // Administrative academic configuration: manage permission only.
    for path in [
        format!("{A}/institutions/{inst}/credit-mapping-rules"),
        format!("{A}/institutions/{inst}/reconciliation-runs"),
        format!("{A}/institutions/{inst}/faculty-assignment-policies"),
    ] {
        expect(&app, &meera, &path, StatusCode::FORBIDDEN).await;
        expect(&app, &admin, &path, StatusCode::OK).await;
    }

    // A completed assignment ends the access; a withdrawn seat too.
    sqlx::query(
        "UPDATE academic.faculty_assignment SET status = 'completed' \
         WHERE tenant_id = $1 AND faculty_subject_ref = $2",
    )
    .bind(setup.base.tenant_id)
    .bind(b4::MEERA)
    .execute(&pool)
    .await
    .unwrap();
    assert!(ids(&expect(&app, &meera, &dir, StatusCode::OK).await).is_empty());
    expect(
        &app,
        &meera,
        &format!("{S}/students/{a}"),
        StatusCode::NOT_FOUND,
    )
    .await;
    sqlx::query(
        "UPDATE academic.section_enrolment SET status = 'withdrawn' \
         WHERE tenant_id = $1 AND student_id = $2",
    )
    .bind(setup.base.tenant_id)
    .bind(bilal.student_id)
    .execute(&pool)
    .await
    .unwrap();
    assert!(ids(&expect(&ravi, &who, &dir, StatusCode::OK).await).is_empty());
}

#[tokio::test]
async fn student_sessions_read_the_public_institution_and_entitlement_subset() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Public Profile College").await;
    sqlx::query(
        "UPDATE institution.institution SET administrator_email = 'head@ppc.example', \
         registration_number = 'REG-C3-1' WHERE id = $1::uuid",
    )
    .bind(&inst)
    .execute(&pool)
    .await
    .unwrap();

    // The administrator's read fills the cache with the full record …
    let path = format!("{INSTITUTIONS}/{inst}");
    let full = expect(&app, &admin, &path, StatusCode::OK).await;
    assert_eq!(full["profile"]["administratorEmail"], "head@ppc.example");
    assert_eq!(full["profile"]["registrationNumber"], "REG-C3-1");
    // … a student never sees it (cache hit and miss alike), staff still do.
    for _ in 0..2 {
        let p = expect(&app, &student, &path, StatusCode::OK).await;
        assert_eq!(p["profile"]["administratorEmail"], Value::Null, "{p}");
        assert_eq!(p["profile"]["registrationNumber"], Value::Null, "{p}");
        assert_eq!(p["name"], "Public Profile College");
    }
    let p = expect(&app, &faculty, &path, StatusCode::OK).await;
    assert_eq!(p["profile"]["administratorEmail"], "head@ppc.example");
    let list = expect(&app, &student, INSTITUTIONS, StatusCode::OK).await;
    let mine = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == inst.as_str())
        .unwrap_or_else(|| panic!("institution missing: {list}"));
    assert_eq!(mine["profile"]["administratorEmail"], Value::Null);
    assert_eq!(mine["profile"]["registrationNumber"], Value::Null);
    // Profile revisions carry the full master information: not for students.
    let rev = format!("{INSTITUTIONS}/{inst}/profile/revisions");
    expect(&app, &student, &rev, StatusCode::FORBIDDEN).await;
    expect(&app, &admin, &rev, StatusCode::OK).await;

    // Entitlement: modules / status / read-only for everyone, terms for managers + approvers.
    let e = expect(&app, &student, ENT, StatusCode::OK).await;
    assert!(!e["modules"].as_array().unwrap().is_empty(), "{e}");
    assert_eq!(e["status"], "active");
    assert_eq!(e["readOnly"], false);
    assert_eq!(e["package"], Value::Null, "{e}");
    assert_eq!(e["validUntil"], Value::Null);
    assert_eq!(e["graceEndsAt"], Value::Null);
    assert_eq!(e["daysRemaining"], Value::Null);
    let e = expect(&app, &faculty, ENT, StatusCode::OK).await;
    assert_eq!(e["package"], Value::Null, "{e}");
    let e = expect(&app, &admin, ENT, StatusCode::OK).await;
    assert!(e["package"].is_string(), "{e}");
}

#[tokio::test]
async fn org_child_tables_carry_tenant_scope_under_forced_rls() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    for table in ["org_type_blueprint_unit_type", "org_restructure_move"] {
        let cols: Vec<(String, String)> = sqlx::query_as(
            "SELECT column_name::text, is_nullable::text FROM information_schema.columns \
             WHERE table_schema = 'institution' AND table_name = $1 \
               AND column_name IN ('tenant_id', 'institution_id') ORDER BY 1",
        )
        .bind(table)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(
            cols,
            vec![
                ("institution_id".to_owned(), "NO".to_owned()),
                ("tenant_id".to_owned(), "NO".to_owned()),
            ],
            "{table}"
        );
        let (enabled, forced): (bool, bool) = sqlx::query_as(
            "SELECT relrowsecurity, relforcerowsecurity FROM pg_class \
             WHERE oid = ('institution.' || $1)::regclass",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(enabled && forced, "{table}: RLS enabled + forced");
        let gaps: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM platform.tenant_rls_gaps WHERE table_name = $1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(gaps, 0, "{table}");
    }

    // Inserts copy tenant + institution from the parent blueprint.
    let inst = register_institution(&app, &admin, "Blueprint College").await;
    let from = (chrono::Utc::now().date_naive() + chrono::Duration::days(1)).to_string();
    let (s, _, bp) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{INSTITUTIONS}/{inst}/org-type-blueprints"),
            json!({"effectiveFrom": from, "unitTypes": [
                {"key": "university", "label": "University", "allowRoot": true},
                {"key": "department", "label": "Department", "allowedParentTypes": ["university"]},
            ]}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{bp}");
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT tenant_id, institution_id FROM institution.org_type_blueprint_unit_type \
         WHERE blueprint_id = $1::uuid",
    )
    .bind(bp["id"].as_str().unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    for (tn, i) in rows {
        assert_eq!(tn.to_string(), t);
        assert_eq!(i.to_string(), inst);
    }
}
