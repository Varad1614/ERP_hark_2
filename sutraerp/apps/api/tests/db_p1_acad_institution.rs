//! Part 1 Stage 2 (acad) regression test for DEF-B-002 on real PostgreSQL: submitting an
//! institution for activation (`institution.manage`) never activates it, in either approval
//! mode; only the Institution Activation Authority (`institution.activation.decide`, an MFA
//! step-up permission) moves it to `active` through the decision endpoint.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use support::*;
use sutra_institution::shared::approval::ApprovalMode;
use sutra_institution::shared::{Actor, InstitutionState};
use sutra_kernel::auth::Permission;
use sutra_kernel::cache::AdvisoryCache;
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};

const API: &str = "/api/v1/institution";

fn tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, v)).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn ver(b: &Value) -> Option<i64> {
    b["version"].as_i64()
}

fn id(b: &Value) -> String {
    b["id"].as_str().unwrap().to_owned()
}

fn state(pool: &sqlx::PgPool, mode: ApprovalMode) -> InstitutionState {
    let mut st = InstitutionState::new(
        pool.clone(),
        AdvisoryCache::disabled(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    );
    st.approvals = mode;
    st
}

/// Takes a fresh institution to `configured` with every required readiness item satisfied.
async fn ready_institution(app: &Router, admin: &Login, pool: &sqlx::PgPool) -> String {
    let inst = register_institution(app, admin, "P1 Acad Activation University").await;
    let cur = read(app, admin, &format!("{API}/institutions/{inst}")).await;
    let cur = call(
        app,
        admin,
        "PUT",
        &format!("{API}/institutions/{inst}/profile"),
        json!({
            "shortName": "PAU", "institutionType": "university", "registrationNumber": "REG-PAU",
            "address": "1 Campus Road, Pune", "contactPhone": "+91 20 1234 5678",
            "officialEmail": "office@pau.edu", "academicPattern": "semester",
            "workingDays": ["mon", "tue", "wed", "thu", "fri"], "defaultLanguage": "en-IN",
            "administratorEmail": "admin@pau.edu"
        }),
        ver(&cur),
        StatusCode::OK,
    )
    .await;
    call(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{inst}/configure"),
        json!({}),
        ver(&cur),
        StatusCode::OK,
    )
    .await;
    let today: chrono::NaiveDate = sqlx::query_scalar(
        "SELECT (now() AT TIME ZONE timezone)::date FROM institution.institution WHERE id = $1::uuid",
    )
    .bind(&inst)
    .fetch_one(pool)
    .await
    .unwrap();
    let bp = call(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{inst}/org-type-blueprints"),
        json!({"effectiveFrom": today.to_string(), "unitTypes": [
            {"key": "university", "label": "University", "allowRoot": true},
        ]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        app,
        admin,
        "POST",
        &format!("{API}/org-type-blueprints/{}/publish", id(&bp)),
        json!({}),
        ver(&bp),
        StatusCode::OK,
    )
    .await;
    let root = call(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{inst}/org-units"),
        json!({"unitType": "university", "code": "ROOT", "name": "P1 Acad Activation University"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        app,
        admin,
        "POST",
        &format!("{API}/org-units/{}/activate", id(&root)),
        json!({}),
        ver(&root),
        StatusCode::OK,
    )
    .await;
    b2::satisfy_2b_2c(app, admin, &inst).await;
    let r = read(
        app,
        admin,
        &format!("{API}/institutions/{inst}/activation-readiness"),
    )
    .await;
    assert_eq!(r["ready"], true, "{r}");
    inst
}

async fn activated_events(pool: &sqlx::PgPool, inst: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE subject = $1 AND event_type LIKE '%institution.activated'",
    )
    .bind(inst)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn submit_never_activates_only_the_activation_authority_does() {
    // The authority's permission is a step-up permission; the submitter's is not.
    assert!(Permission::InstitutionActivationDecide.requires_step_up());
    assert!(!Permission::InstitutionManage.requires_step_up());

    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let inst = ready_institution(&app, &admin, &pool).await;
    let path = |s: &str| format!("{API}/institutions/{inst}{s}");

    // Deployed router: submit → pending_approval, the request stays pending, nothing activated.
    let cur = read(&app, &admin, &path("")).await;
    let pending = call(
        &app,
        &admin,
        "POST",
        &path("/submit"),
        json!({}),
        ver(&cur),
        StatusCode::OK,
    )
    .await;
    assert_eq!(pending["status"], "pending_approval", "{pending}");
    assert_eq!(
        read(&app, &admin, &path("")).await["status"],
        "pending_approval"
    );
    let reqs = read(&app, &admin, &path("/activation-requests")).await;
    assert_eq!(reqs["items"][0]["status"], "pending", "{reqs}");
    assert!(reqs["items"][0]["decidedBy"].is_null(), "{reqs}");
    assert_eq!(activated_events(&pool, &inst).await, 0);

    // Someone without institution.activation.decide cannot decide it.
    let (s, _, b) = send(
        &app,
        write(
            &faculty,
            "POST",
            &path("/decision"),
            json!({"decision": "approve"}),
            ver(&pending),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");

    // Return it, re-configure, and submit again in each approval mode through the repository:
    // the result is pending both times.
    let sc = ExecutionScope::institution(
        t.parse::<TenantId>().unwrap(),
        inst.parse::<InstitutionId>().unwrap(),
    );
    let iid: InstitutionId = inst.parse().unwrap();
    let actor = Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };
    let mut cur = pending;
    for mode in [ApprovalMode::AutoApprove, ApprovalMode::AwaitDecision] {
        let returned = call(
            &app,
            &admin,
            "POST",
            &path("/decision"),
            json!({"decision": "return", "reason": "Check the registration number"}),
            ver(&cur),
            StatusCode::OK,
        )
        .await;
        assert_eq!(returned["status"], "returned");
        let configured = call(
            &app,
            &admin,
            "POST",
            &path("/configure"),
            json!({}),
            ver(&returned),
            StatusCode::OK,
        )
        .await;
        let st = state(&pool, mode);
        let again = sutra_institution::lifecycle::repo::submit(
            &st,
            &sc,
            iid,
            Version(ver(&configured).unwrap()),
            &actor,
        )
        .await
        .unwrap();
        assert_eq!(
            again.status.as_str(),
            "pending_approval",
            "{mode:?}: submit activated the institution"
        );
        assert_eq!(activated_events(&pool, &inst).await, 0, "{mode:?}");
        cur = read(&app, &admin, &path("")).await;
    }

    // The Activation Authority approves → active, recorded as the authority's (user) decision.
    let active = call(
        &app,
        &admin,
        "POST",
        &path("/decision"),
        json!({"decision": "approve"}),
        ver(&cur),
        StatusCode::OK,
    )
    .await;
    assert_eq!(active["status"], "active", "{active}");
    assert_eq!(activated_events(&pool, &inst).await, 1);
    let reqs = read(&app, &admin, &path("/activation-requests")).await;
    let approved: Vec<&Value> = reqs["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["status"] == "approved")
        .collect();
    assert_eq!(approved.len(), 1, "{reqs}");
    assert_eq!(approved[0]["decidedBy"], "dev|admin@sutra.dev");
    let kinds: Vec<(String, String)> = sqlx::query_as(
        "SELECT actor_id, actor_kind FROM platform.audit_log WHERE target_id = $1 AND action = 'institution.institution.activate'",
    )
    .bind(&inst)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        kinds,
        vec![("dev|admin@sutra.dev".to_owned(), "user".to_owned())]
    );
}
