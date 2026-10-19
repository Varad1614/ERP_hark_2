//! Part 1 Stage 2 (acad) regression tests for attendance governance, on real PostgreSQL:
//!
//! * DEF-D-002 — a correction of a finalised register is a request: never applied by the
//!   requesting teacher or by the automatic approver; an authorised approver other than the
//!   requester decides it (attendance.correct + MFA step-up), with an audit trail. Checked in both
//!   approval modes.
//! * DEF-D-003 — the institution decision queue behind the correction screen.
//! * DEF-D-004 — a register cannot be finalised without being submitted.
//! * DEF-D-009 — the database refuses edits of a finalised register outside the correction path.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use support::*;
use sutra_academic::attendance;
use sutra_academic::shared::approval::ApprovalMode;
use sutra_academic::shared::Actor;
use sutra_academic::AcademicState;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const T_FACULTY: &str = "0192f5a0-7c3e-7000-8000-000000a1ac01";
const T_MAKER_CHECKER: &str = "0192f5a0-7c3e-7000-8000-000000a1ac02";
const T_GUARD: &str = "0192f5a0-7c3e-7000-8000-000000a1ac03";
const T_SUBMIT: &str = "0192f5a0-7c3e-7000-8000-000000a1ac04";

const A: &str = "/api/v1/academic";
const ADMIN: &str = "dev|admin@sutra.dev";
const HOD: &str = "dev|hod.second-approver@sutra.dev";

async fn expect(app: &Router, req: Request<Body>, status: StatusCode) -> Value {
    let (s, _, body) = send(app, req).await;
    assert_eq!(s, status, "{body}");
    body
}

fn ver(v: &Value) -> i64 {
    v["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no version in {v}"))
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

async fn post(
    app: &Router,
    who: &Login,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    status: StatusCode,
) -> Value {
    expect(
        app,
        write(who, "POST", &format!("{A}{path}"), body, if_match),
        status,
    )
    .await
}

async fn get_as(app: &Router, who: &Login, path: &str, status: StatusCode) -> Value {
    expect(app, get(who, &format!("{A}{path}")), status).await
}

fn auth(subject: &str, role: Role, tenant: TenantId, mfa: MfaLevel, enforced: bool) -> AuthContext {
    AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![role],
        scope: ExecutionScope::tenant(tenant),
        csrf_token: "a".repeat(64),
        expires_at: Utc::now(),
        idle_expires_at: Utc::now(),
        mfa_level: mfa,
        amr: vec!["pwd".into()],
        step_up_enforced: enforced,
        privileged_mfa_enforced: false,
    }
}

fn actor(subject: &str) -> Actor {
    Actor {
        id: Some(subject.into()),
        correlation_id: None,
    }
}

fn state(pool: &sqlx::PgPool, mode: ApprovalMode) -> AcademicState {
    let mut st = sutra_api::app::academic_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = mode;
    st
}

struct Fixture {
    app: Router,
    pool: sqlx::PgPool,
    admin: Login,
    meera: Login,
    setup: b4::DeliverySetup,
    cs: b4::SeededOffering,
    aarav_id: Uuid,
    bina_id: Uuid,
}

async fn fixture(tenant: &'static str, name: &str) -> Fixture {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("error")
        .with_test_writer()
        .try_init();
    let (app, pool) = app(tenant).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let setup = b4::academic_setup(&app, &admin, &pool, tenant, name).await;
    let cs = b4::seed_offering(&pool, &setup, "CS101", "active").await;
    b4::seed_faculty(&pool, &setup, &cs, b4::MEERA, "active").await;
    let a = b4::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let b = b4::seed_student(&pool, &setup, "Bina", None).await;
    b4::seed_enrolment(&pool, &setup, &cs, &a, Some(b4::AARAV)).await;
    b4::seed_enrolment(&pool, &setup, &cs, &b, None).await;
    Fixture {
        app,
        pool,
        admin,
        meera,
        setup,
        cs,
        aarav_id: a.student_id,
        bina_id: b.student_id,
    }
}

/// Session started and both students marked (register `capturing`).
async fn captured_session(f: &Fixture, date: &str, aarav: &str, bina: &str) -> Value {
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sections/{}/sessions", f.cs.section_id),
        json!({"sessionDate": date, "startsAt": "09:00", "endsAt": "23:00", "topic": "Intro"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&s);
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/start"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [{"studentId": f.aarav_id, "mark": aarav}, {"studentId": f.bina_id, "mark": bina}]}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await
}

/// Captured → submitted → completed → finalised.
async fn finalised_session(f: &Fixture, date: &str, aarav: &str, bina: &str) -> Value {
    let s = captured_session(f, date, aarav, bina).await;
    let sid = id(&s);
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/submit-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/complete"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/finalise-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["attendanceStatus"], "finalised");
    s
}

fn record_of(s: &Value, student: Uuid) -> Uuid {
    s["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["studentId"] == json!(student))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}

async fn mark(pool: &sqlx::PgPool, rec: Uuid) -> String {
    sqlx::query_scalar("SELECT mark FROM academic.attendance_record WHERE id = $1")
        .bind(rec)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn audit_actions(pool: &sqlx::PgPool, correction: Uuid) -> Vec<(String, Option<String>)> {
    sqlx::query_as(
        "SELECT action, actor_id FROM platform.audit_log WHERE target_id = $1 ORDER BY occurred_at",
    )
    .bind(correction.to_string())
    .fetch_all(pool)
    .await
    .unwrap()
}

/// DEF-D-002 / DEF-D-003 through the deployed router (AutoApprove composition): the teacher's
/// request stays `requested`, the mark is untouched, the teacher cannot decide, the request
/// shows in the institution queue (administrators only) and the administrator applies it.
#[tokio::test]
async fn faculty_correction_waits_for_an_approver_in_the_deployed_router() {
    let f = fixture(T_FACULTY, "P1 Acad Faculty University").await;
    let (app, admin, meera) = (&f.app, &f.admin, &f.meera);
    let inst = f.setup.base.institution_id;
    let s = finalised_session(&f, "2026-07-06", "absent", "present").await;
    let sid = id(&s);
    let rec = record_of(&s, f.aarav_id);

    let c = post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance-corrections"),
        json!({"recordId": rec, "toMark": "present", "reason": "Was at the lab practical"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        c["status"], "requested",
        "never applied at request time: {c}"
    );
    assert_eq!(c["requestedBy"], b4::MEERA);
    assert!(c["decidedBy"].is_null());
    assert_eq!(
        mark(&f.pool, rec).await,
        "absent",
        "the finalised fact is untouched"
    );
    let cid = id(&c);

    // the teacher can neither decide nor read the decision queue
    post(
        app,
        meera,
        &format!("/attendance-corrections/{cid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&c)),
        StatusCode::FORBIDDEN,
    )
    .await;
    get_as(
        app,
        meera,
        &format!("/institutions/{inst}/attendance-corrections?status=requested"),
        StatusCode::FORBIDDEN,
    )
    .await;

    // the administrator's queue lists it with the student and the session
    let q = get_as(
        app,
        admin,
        &format!("/institutions/{inst}/attendance-corrections?status=requested"),
        StatusCode::OK,
    )
    .await;
    let items = q["items"].as_array().unwrap();
    assert_eq!(items.len(), 1, "{q}");
    assert_eq!(items[0]["id"], json!(cid));
    assert_eq!(items[0]["sessionDate"], "2026-07-06");
    assert_eq!(items[0]["startsAt"], "09:00");
    assert_eq!(items[0]["fromMark"], "absent");
    assert_eq!(items[0]["toMark"], "present");
    assert_eq!(items[0]["sectionCode"], "A");
    assert!(!items[0]["displayName"].as_str().unwrap().is_empty());
    assert!(!items[0]["studentNumber"].as_str().unwrap().is_empty());

    // a second request on the same record while one is open is refused
    post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance-corrections"),
        json!({"recordId": rec, "toMark": "late", "reason": "Second try"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;

    let c = post(
        app,
        admin,
        &format!("/attendance-corrections/{cid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "applied");
    assert_eq!(c["decidedBy"], ADMIN);
    assert_eq!(mark(&f.pool, rec).await, "present");
    let q = get_as(
        app,
        admin,
        &format!("/institutions/{inst}/attendance-corrections?status=requested"),
        StatusCode::OK,
    )
    .await;
    assert!(q["items"].as_array().unwrap().is_empty(), "{q}");
    let actions = audit_actions(&f.pool, Uuid::parse_str(&cid).unwrap()).await;
    assert_eq!(
        actions,
        vec![
            (
                "academic.attendance_correction.request".to_owned(),
                Some(b4::MEERA.to_owned())
            ),
            (
                "academic.attendance_correction.apply".to_owned(),
                Some(ADMIN.to_owned())
            ),
        ]
    );
}

/// DEF-D-002 maker-checker and step-up, in both approval modes: an administrator who requested
/// a correction cannot decide it; a second approver needs a step-up (MFA) session when step-up is
/// enforced; the database refuses an applied correction decided by its requester.
#[tokio::test]
async fn requester_never_decides_and_the_approver_needs_step_up_in_both_modes() {
    let f = fixture(T_MAKER_CHECKER, "P1 Acad Maker Checker University").await;
    let tenant = f.setup.tenant();
    let requester = auth(ADMIN, Role::Administrator, tenant, MfaLevel::Mfa, true);
    let hod_no_mfa = auth(HOD, Role::Administrator, tenant, MfaLevel::None, true);
    let hod = auth(HOD, Role::Administrator, tenant, MfaLevel::Mfa, true);
    for (i, mode) in [ApprovalMode::AutoApprove, ApprovalMode::AwaitDecision]
        .into_iter()
        .enumerate()
    {
        let st = state(&f.pool, mode);
        let s = finalised_session(&f, &format!("2026-07-1{i}"), "absent", "present").await;
        let sid = Uuid::parse_str(&id(&s)).unwrap();
        let rec = record_of(&s, f.aarav_id);
        let cmd: attendance::repo::NewAttendanceCorrection = serde_json::from_value(
            json!({"recordId": rec, "toMark": "excused", "reason": "Medical certificate"}),
        )
        .unwrap();
        let c = attendance::repo::request_correction(&st, &requester, sid, cmd, &actor(ADMIN))
            .await
            .unwrap();
        assert_eq!(
            c.status.as_str(),
            "requested",
            "{mode:?}: the requester's own correction is not applied"
        );
        assert_eq!(mark(&f.pool, rec).await, "absent", "{mode:?}");

        let own = attendance::repo::decide_correction(
            &st,
            &requester,
            c.id,
            Version(c.version),
            true,
            None,
            &actor(ADMIN),
        )
        .await;
        assert!(
            matches!(own, Err(KernelError::Forbidden(_))),
            "{mode:?}: requester decided own correction: {own:?}"
        );
        let no_step_up = attendance::repo::decide_correction(
            &st,
            &hod_no_mfa,
            c.id,
            Version(c.version),
            true,
            None,
            &actor(HOD),
        )
        .await;
        assert!(
            matches!(no_step_up, Err(KernelError::StepUpRequired(_))),
            "{mode:?}: {no_step_up:?}"
        );
        let faculty = auth(b4::MEERA, Role::Faculty, tenant, MfaLevel::Mfa, true);
        let by_faculty = attendance::repo::decide_correction(
            &st,
            &faculty,
            c.id,
            Version(c.version),
            true,
            None,
            &actor(b4::MEERA),
        )
        .await;
        assert!(
            matches!(by_faculty, Err(KernelError::Forbidden(_))),
            "{mode:?}: {by_faculty:?}"
        );
        assert_eq!(
            mark(&f.pool, rec).await,
            "absent",
            "{mode:?}: refused decisions change nothing"
        );

        let done = attendance::repo::decide_correction(
            &st,
            &hod,
            c.id,
            Version(c.version),
            true,
            None,
            &actor(HOD),
        )
        .await
        .unwrap();
        assert_eq!(done.status.as_str(), "applied");
        assert_eq!(done.decided_by.as_deref(), Some(HOD));
        assert_eq!(done.requested_by.as_deref(), Some(ADMIN));
        assert_eq!(mark(&f.pool, rec).await, "excused", "{mode:?}");
        let hist: (String, String, Option<String>) = sqlx::query_as(
            "SELECT from_mark, to_mark, actor FROM academic.attendance_audit WHERE correction_id = $1",
        )
        .bind(c.id)
        .fetch_one(&f.pool)
        .await
        .unwrap();
        assert_eq!(hist, ("absent".into(), "excused".into(), Some(HOD.into())));
    }

    // defence in depth: the database itself refuses an applied correction decided by its requester
    let any: Uuid = sqlx::query_scalar(
        "SELECT id FROM academic.attendance_correction WHERE tenant_id = $1 LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    let err = sqlx::query(
        "UPDATE academic.attendance_correction SET decided_by = requested_by WHERE id = $1",
    )
    .bind(any)
    .execute(&f.pool)
    .await;
    assert!(err.is_err(), "maker-checker constraint");
}

/// DEF-D-009: once finalised, a register cannot be edited by a direct UPDATE; only the governed
/// correction path changes a mark. Open registers stay editable.
#[tokio::test]
async fn database_refuses_edits_of_a_finalised_register() {
    let f = fixture(T_GUARD, "P1 Acad Guard University").await;
    let open = captured_session(&f, "2026-07-06", "absent", "present").await;
    let open_rec = record_of(&open, f.aarav_id);
    sqlx::query("UPDATE academic.attendance_record SET mark = 'late' WHERE id = $1")
        .bind(open_rec)
        .execute(&f.pool)
        .await
        .expect("an open register can still be edited");

    let s = finalised_session(&f, "2026-07-07", "absent", "present").await;
    let rec = record_of(&s, f.aarav_id);
    for sql in [
        "UPDATE academic.attendance_record SET mark = 'present' WHERE id = $1",
        "UPDATE academic.attendance_record SET remarks = 'edited later' WHERE id = $1",
    ] {
        let r = sqlx::query(sql).bind(rec).execute(&f.pool).await;
        assert!(r.is_err(), "finalised record edited by: {sql}");
    }
    // a forged audit row without an open correction does not unlock the record
    let mut tx = f.pool.begin().await.unwrap();
    sqlx::query("INSERT INTO academic.attendance_audit (tenant_id, record_id, from_mark, to_mark, actor) VALUES ($1, $2, 'absent', 'present', 'forger')")
        .bind(f.setup.base.tenant_id)
        .bind(rec)
        .execute(&mut *tx)
        .await
        .unwrap();
    let r = sqlx::query("UPDATE academic.attendance_record SET mark = 'present' WHERE id = $1")
        .bind(rec)
        .execute(&mut *tx)
        .await;
    assert!(
        r.is_err(),
        "audit row without a correction must not unlock the record"
    );
    drop(tx);
    assert_eq!(mark(&f.pool, rec).await, "absent");

    // the API capture path refuses too
    post(
        &f.app,
        &f.meera,
        &format!("/sessions/{}/attendance", id(&s)),
        json!({"records": [{"studentId": f.aarav_id, "mark": "present"}]}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
}

/// DEF-D-004: `Capturing` cannot skip `Submitted`.
#[tokio::test]
async fn finalisation_requires_a_submitted_register() {
    let f = fixture(T_SUBMIT, "P1 Acad Submit University").await;
    let s = captured_session(&f, "2026-07-06", "present", "present").await;
    let sid = id(&s);
    assert_eq!(s["attendanceStatus"], "capturing");
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/complete"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let refused = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/finalise-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(refused.to_string().contains("capturing"), "{refused}");
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/submit-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["attendanceStatus"], "submitted");
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/finalise-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["attendanceStatus"], "finalised");
}
