//! Slice 3B DB tests (owned by 3B; see `docs/batch3-contracts.md` §11). Shared harness:
//! `support::*` and the Batch 3 helpers in `support::b3`.
//!
//! Scenarios: L3/L4-C-020 … C-038 and C-075 (registration windows, registration, add/drop,
//! cancellation, backlog/repeat, finalisation, holds; progression policy, decisions,
//! conditions, probation, detention/repeat, ledger, credit transfer, programme migration,
//! academic break + rejoin, qualification eligibility, ABC sync, decision revision), plus
//! `ApprovalMode::AwaitDecision`, tenant isolation and permissions.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::NaiveDate;
use serde_json::{json, Value};
use std::sync::Arc;
use support::*;
use sutra_contracts::finance::{
    AdmissionFinanceQuery, FinanceClearancePort, FinanceEvidence, StudentFinanceQuery,
};
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::{AbcCreditSubmission, AbcRegistryPort, ExternalSyncOutcome};
use sutra_contracts::student::status::{
    ProgressionDecisionStatus, ProgressionOutcome, RegistrationStatus,
};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_student::shared::approval::ApprovalMode;
use sutra_student::shared::{Actor, StudentState};
use uuid::Uuid;

const API: &str = "/api/v1/student";
const AARAV: &str = "dev|aarav.desai@sutra.dev";

// ------------------------------------------------------------------ helpers

async fn ok(
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

async fn read(app: &Router, who: &Login, path: &str, want: StatusCode) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, want, "GET {path}: {b}");
    b
}

fn id(b: &Value) -> String {
    b["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {b}"))
        .to_owned()
}

fn ver(b: &Value) -> Option<i64> {
    b["version"].as_i64()
}

async fn today(pool: &sqlx::PgPool) -> NaiveDate {
    sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Asia/Kolkata')::date")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn date(v: &Value) -> NaiveDate {
    v.as_str().unwrap().parse().unwrap()
}

/// (T1, T2) term ids and the term containing today.
fn terms(setup: &b3::AcademicSetup, today: NaiveDate) -> (String, String, String) {
    let ts = setup.calendar_version["terms"].as_array().unwrap();
    let mut ts: Vec<&Value> = ts.iter().collect();
    ts.sort_by_key(|t| t["sequence"].as_i64());
    let current = ts
        .iter()
        .find(|t| date(&t["startsOn"]) <= today && today <= date(&t["endsOn"]))
        .map(|t| id(t))
        .expect("today must fall in a seeded term");
    (id(ts[0]), id(ts[1]), current)
}

fn term_end(setup: &b3::AcademicSetup, term: &str) -> NaiveDate {
    let t = setup.calendar_version["terms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == term)
        .unwrap();
    date(&t["endsOn"])
}

fn comp(setup: &b3::AcademicSetup, slot: &str) -> String {
    let cs = setup.curriculum_version["components"].as_array().unwrap();
    id(cs
        .iter()
        .find(|c| c["slotCode"] == slot)
        .unwrap_or_else(|| panic!("no {slot}")))
}

/// Creates + opens a window for `term` around today.
async fn open_window(
    app: &Router,
    admin: &Login,
    pool: &sqlx::PgPool,
    setup: &b3::AcademicSetup,
    term: &str,
    min: Option<f64>,
) -> Value {
    let t = today(pool).await;
    let closes = (t + chrono::Days::new(10)).min(term_end(setup, term));
    let w = ok(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{}/registration-windows", setup.institution_id),
        json!({"academicTermId": term, "opensOn": t - chrono::Days::new(1), "closesOn": closes, "minCredits": min, "maxCredits": 28}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(w["status"], "scheduled");
    let w = ok(
        app,
        admin,
        "POST",
        &format!("{API}/registration-windows/{}/open", id(&w)),
        json!({}),
        ver(&w),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "open", "{w}");
    w
}

async fn events(pool: &sqlx::PgPool, subject: &str, suffix: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE subject = $1 AND event_type = $2",
    )
    .bind(subject)
    .bind(format!("sutraerp.student.{suffix}"))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn audits(pool: &sqlx::PgPool, target: &str, action: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE target_id = $1 AND action = $2",
    )
    .bind(target)
    .bind(action)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn ctx_term(pool: &sqlx::PgPool, ctx: Uuid) -> i32 {
    sqlx::query_scalar("SELECT current_term_no FROM student.student_context WHERE id = $1")
        .bind(ctx)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn set_term(pool: &sqlx::PgPool, ctx: Uuid, term: i32) {
    sqlx::query("UPDATE student.student_context SET current_term_no = $2 WHERE id = $1")
        .bind(ctx)
        .bind(term)
        .execute(pool)
        .await
        .unwrap();
}

async fn setup(
    tenant: &'static str,
    name: &str,
) -> (Router, sqlx::PgPool, Login, b3::AcademicSetup) {
    let (app, pool) = app(tenant).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let mut s = b3::academic_setup(&app, &admin, tenant, name).await;
    // Domain B's curriculum payload carries no component ids; tests read them (owner pool) so
    // registration lines can reference `curriculumComponentId`.
    let ids: Vec<(String, Uuid)> = sqlx::query_as(
        "SELECT slot_code, id FROM institution.curriculum_component WHERE curriculum_version_id = $1",
    )
    .bind(s.curriculum_version_id)
    .fetch_all(&pool)
    .await
    .unwrap();
    if let Some(cs) = s.curriculum_version["components"].as_array_mut() {
        for c in cs {
            if let Some((_, cid)) = ids.iter().find(|(slot, _)| c["slotCode"] == slot.as_str()) {
                c["id"] = json!(cid);
            }
        }
    }
    (app, pool, admin, s)
}

/// Registers `slots` for the student in the open window and submits (auto-approved → registered).
async fn register(app: &Router, admin: &Login, sid: Uuid, term: &str, lines: Value) -> Value {
    let r = ok(
        app,
        admin,
        "POST",
        &format!("{API}/students/{sid}/registrations"),
        json!({"academicTermId": term, "lines": lines}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let r = ok(
        app,
        admin,
        "POST",
        &format!("{API}/registrations/{}/submit", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "registered", "{r}");
    r
}

async fn policy(app: &Router, admin: &Login, inst: Uuid, code: &str, rules: Value) -> Value {
    let p = ok(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{inst}/progression-policies"),
        json!({"code": code, "name": "UG progression", "effectiveFrom": "2026-06-01", "rules": rules}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p = ok(
        app,
        admin,
        "POST",
        &format!("{API}/progression-policies/{}/submit", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "effective", "{p}");
    p
}

fn std_rules() -> Value {
    json!({"progressMinRatio": 1.0, "conditionalMinRatio": 0.75, "probationMinRatio": 0.5, "maxFailedForConditional": 1})
}

/// Drafts + submits a decision (auto-approved → effective).
async fn decide(app: &Router, admin: &Login, sid: Uuid, body: Value) -> Value {
    let d = ok(
        app,
        admin,
        "POST",
        &format!("{API}/students/{sid}/progression-decisions"),
        body,
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(d["status"], "draft", "{d}");
    let d = ok(
        app,
        admin,
        "POST",
        &format!("{API}/progression-decisions/{}/submit", id(&d)),
        json!({}),
        ver(&d),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "effective", "{d}");
    d
}

fn state(pool: &sqlx::PgPool) -> StudentState {
    sutra_api::app::student_deps_with(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
        // Part 1: explicit dev/test auto-approval opt-in (the subject here is not approval).
        sutra_kernel::approval::ApprovalMode::AutoApprove,
    )
    .into_state()
}

fn user() -> Actor {
    Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    }
}

// ------------------------------------------------------------------ L3-C-020

#[tokio::test]
async fn registration_window_opening_and_closure() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b01";
    let (app, pool, admin, s) = setup(T, "Window University").await;
    let t = today(&pool).await;
    let (_t1, _t2, cur) = terms(&s, t);
    let base = format!(
        "{API}/institutions/{}/registration-windows",
        s.institution_id
    );

    // No dates and no `registration_window` key date in the calendar → 422; unknown term → 422.
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"academicTermId": cur}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"academicTermId": Uuid::now_v7(), "opensOn": t, "closesOn": t}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // Faculty cannot schedule windows; Idempotency-Key is required.
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    ok(
        &app,
        &faculty,
        "POST",
        &base,
        json!({"academicTermId": cur, "opensOn": t, "closesOn": t}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let mut req = write(
        &admin,
        "POST",
        &base,
        json!({"academicTermId": cur, "opensOn": t, "closesOn": t}),
        None,
    );
    req.headers_mut().remove("idempotency-key");
    assert_eq!(send(&app, req).await.0, StatusCode::BAD_REQUEST);

    // Scheduled in the future: opening is refused until the date (L4-C-020-BH-004).
    let future = ok(&app, &admin, "POST", &base,
        json!({"academicTermId": cur, "opensOn": t + chrono::Days::new(1), "closesOn": (t + chrono::Days::new(2)).min(term_end(&s, &cur))}),
        None, StatusCode::CREATED).await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"academicTermId": cur, "opensOn": t, "closesOn": t}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/open", id(&future)),
        json!({}),
        ver(&future),
        StatusCode::CONFLICT,
    )
    .await;
    // Missing / stale If-Match.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/open", id(&future)),
        json!({}),
        None,
        StatusCode::PRECONDITION_REQUIRED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/cancel", id(&future)),
        json!({"reason": "x"}),
        Some(9),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    // Reschedule to today, then open.
    let w = ok(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/registration-windows/{}", id(&future)),
        json!({"opensOn": t}),
        ver(&future),
        StatusCode::OK,
    )
    .await;
    let w = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/open", id(&w)),
        json!({}),
        ver(&w),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "open");
    assert!(w["openedAt"].is_string());
    assert_eq!(
        events(&pool, &id(&w), "registration_window.opened").await,
        1
    );
    // Open windows keep their opening date; cancel only applies to scheduled windows.
    ok(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/registration-windows/{}", id(&w)),
        json!({"opensOn": t - chrono::Days::new(3)}),
        ver(&w),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/cancel", id(&w)),
        json!({"reason": "x"}),
        ver(&w),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/finalise", id(&w)),
        json!({}),
        ver(&w),
        StatusCode::CONFLICT,
    )
    .await;
    let w = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/close", id(&w)),
        json!({}),
        ver(&w),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "closed");
    assert_eq!(
        events(&pool, &id(&w), "registration_window.closed").await,
        1
    );
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/open", id(&w)),
        json!({}),
        ver(&w),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        audits(&pool, &id(&w), "student.registration_window.close").await,
        1
    );

    // A closed window frees the term for a new one; scheduled ones can be cancelled.
    let w2 = ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"academicTermId": cur, "opensOn": t, "closesOn": t}),
        None,
        StatusCode::CREATED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/cancel", id(&w2)),
        json!({}),
        ver(&w2),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    read(
        &app,
        &admin,
        &format!("{API}/registration-windows/{}", Uuid::now_v7()),
        StatusCode::NOT_FOUND,
    )
    .await;
    // Audit C3: registration windows are institution administration — faculty 403.
    read(&app, &faculty, &base, StatusCode::FORBIDDEN).await;
    let list = read(
        &app,
        &admin,
        &format!("{base}?status=scheduled"),
        StatusCode::OK,
    )
    .await;
    let sched = list["items"].as_array().unwrap();
    assert_eq!(sched.len(), 1);
    let c = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/cancel", id(&sched[0])),
        json!({"reason": "Replaced"}),
        ver(&sched[0]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "cancelled");
    assert_eq!(
        read(&app, &admin, &base, StatusCode::OK).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// ------------------------------------------------------------------ L3-C-021 / 022 / 025

#[tokio::test]
async fn registration_add_drop_and_finalisation() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b02";
    let (app, pool, admin, s) = setup(T, "Registration University").await;
    let t = today(&pool).await;
    let (_, _, cur) = terms(&s, t);
    let st = b3::seed_student(&pool, &s, "Asha", Some(AARAV)).await;
    let sid = st.student_id;
    let w = open_window(&app, &admin, &pool, &s, &cur, Some(10.0)).await;
    let regs = format!("{API}/students/{sid}/registrations");

    // Selections are validated against the curriculum term (L4-C-021-BH-003).
    ok(
        &app,
        &admin,
        "POST",
        &regs,
        json!({"academicTermId": cur, "lines": [{"curriculumComponentId": comp(&s, "CS201")}]}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &regs,
        json!({"academicTermId": cur, "lines": [{"curriculumComponentId": Uuid::now_v7()}]}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = ok(
        &app,
        &admin,
        "POST",
        &regs,
        json!({"academicTermId": cur, "lines": [{"curriculumComponentId": comp(&s, "MD101")}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["status"], "draft");
    assert_eq!(r["totalCredits"], 8.0);
    assert_eq!(r["origin"], "staff");
    let rid = id(&r);
    ok(
        &app,
        &admin,
        "POST",
        &regs,
        json!({"academicTermId": cur}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // Below the window minimum (10) → 422, registration stays draft.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/submit"),
        json!({}),
        ver(&r),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/lines"),
        json!({"curriculumComponentId": comp(&s, "MD101")}),
        ver(&r),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/lines"),
        json!({"curriculumComponentId": comp(&s, "CS101")}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["totalCredits"], 20.0);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/submit"),
        json!({}),
        None,
        StatusCode::PRECONDITION_REQUIRED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/submit"),
        json!({}),
        Some(99),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/submit"),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "registered", "auto-approved: {r}");
    assert!(r["processInstanceId"].is_string());
    assert!(r["lines"]
        .as_array()
        .unwrap()
        .iter()
        .all(|l| l["status"] == "registered"));
    for e in [
        "registration.submitted",
        "registration.approved",
        "registration.registered",
    ] {
        assert_eq!(events(&pool, &rid, e).await, 1, "{e}");
    }
    assert_eq!(audits(&pool, &rid, "student.registration.submit").await, 1);

    // Add/drop after registration while the window is open (L3-C-022).
    let md = r["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slotCode"] == "MD101")
        .unwrap()
        .clone();
    let cs = r["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slotCode"] == "CS101")
        .unwrap()
        .clone();
    let r = ok(
        &app,
        &admin,
        "DELETE",
        &format!("{API}/registrations/{rid}/lines/{}", id(&md)),
        json!({"reason": "Clash"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["totalCredits"], 12.0);
    ok(
        &app,
        &admin,
        "DELETE",
        &format!("{API}/registrations/{rid}/lines/{}", id(&cs)),
        json!({}),
        ver(&r),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/lines"),
        json!({"curriculumComponentId": comp(&s, "MD101")}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    let added = r["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["slotCode"] == "MD101" && l["status"] == "registered")
        .unwrap();
    assert_eq!(added["addedAfterRegistration"], true);
    assert_eq!(events(&pool, &rid, "registration.modified").await, 2);
    let h = read(
        &app,
        &admin,
        &format!("{API}/registrations/{rid}/history"),
        StatusCode::OK,
    )
    .await;
    assert!(h["items"].as_array().unwrap().len() >= 6, "{h}");

    // The student sees the registration and options; staff endpoints are forbidden.
    let me = login(&app, "aarav.desai@sutra.dev").await;
    let mine = read(
        &app,
        &me,
        &format!("{API}/me/registrations"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    read(
        &app,
        &me,
        &format!("{API}/me/registrations/{rid}"),
        StatusCode::OK,
    )
    .await;
    let opts = read(
        &app,
        &me,
        &format!("{API}/me/registration-windows"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(opts["termNo"], 1);
    assert_eq!(
        opts["components"].as_array().unwrap().len(),
        2,
        "term-1 components: {opts}"
    );
    assert_eq!(opts["windows"].as_array().unwrap().len(), 1);
    read(
        &app,
        &me,
        &format!("{API}/students/{sid}/registrations"),
        StatusCode::FORBIDDEN,
    )
    .await;
    read(
        &app,
        &admin,
        &format!("{API}/me/registrations"),
        StatusCode::FORBIDDEN,
    )
    .await;

    // Close the window: no more add/drop; finalise publishes the registration (L3-C-025).
    let w = read(
        &app,
        &admin,
        &format!("{API}/registration-windows/{}", id(&w)),
        StatusCode::OK,
    )
    .await;
    let w = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/close", id(&w)),
        json!({}),
        ver(&w),
        StatusCode::OK,
    )
    .await;
    ok(
        &app,
        &me,
        "POST",
        &format!("{API}/me/registrations/{rid}/lines"),
        json!({"curriculumComponentId": comp(&s, "CS101")}),
        ver(&r),
        StatusCode::CONFLICT,
    )
    .await;
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/finalise", id(&w)),
        json!({}),
        ver(&w),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["finalisedRegistrationIds"], json!([rid]));
    let r = read(
        &app,
        &admin,
        &format!("{API}/registrations/{rid}"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "finalised");
    assert!(r["finalisedAt"].is_string());
    assert_eq!(events(&pool, &rid, "registration.finalised").await, 1);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{rid}/cancel"),
        json!({"reason": "late"}),
        ver(&r),
        StatusCode::CONFLICT,
    )
    .await;
    // History is immutable.
    assert!(sqlx::query(
        "DELETE FROM student.registration_status_history WHERE registration_id = $1"
    )
    .bind(r["id"].as_str().unwrap().parse::<Uuid>().unwrap())
    .execute(&pool)
    .await
    .is_err());
}

// ------------------------------------------------------------------ L3-C-021 self-service, L3-C-023

#[tokio::test]
async fn self_registration_and_cancellation() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b03";
    let (app, pool, admin, s) = setup(T, "Self Service University").await;
    let (_, _, cur) = terms(&s, today(&pool).await);
    let st = b3::seed_student(&pool, &s, "Kiran", Some(AARAV)).await;
    open_window(&app, &admin, &pool, &s, &cur, None).await;
    let me = login(&app, "aarav.desai@sutra.dev").await;
    let lines = json!([{"curriculumComponentId": comp(&s, "CS101")}, {"curriculumComponentId": comp(&s, "MD101")}]);
    let r = ok(
        &app,
        &me,
        "POST",
        &format!("{API}/me/registrations"),
        json!({"academicTermId": cur, "lines": lines}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["origin"], "self");
    assert_eq!(r["studentId"], st.student_id.to_string());
    let r = ok(
        &app,
        &me,
        "POST",
        &format!("{API}/me/registrations/{}/submit", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "registered");
    // Cancellation needs a reason (L3-C-023).
    ok(
        &app,
        &me,
        "POST",
        &format!("{API}/me/registrations/{}/cancel", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = ok(
        &app,
        &me,
        "POST",
        &format!("{API}/me/registrations/{}/cancel", id(&r)),
        json!({"reason": "Changing my plan"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "cancelled");
    assert!(c["lines"]
        .as_array()
        .unwrap()
        .iter()
        .all(|l| l["status"] == "withdrawn"));
    assert_eq!(events(&pool, &id(&c), "registration.cancelled").await, 1);
    // A cancelled registration frees the term.
    ok(
        &app,
        &me,
        "POST",
        &format!("{API}/me/registrations"),
        json!({"academicTermId": cur, "lines": lines}),
        None,
        StatusCode::CREATED,
    )
    .await;
    // Someone else's registration is invisible to the student.
    let other = b3::seed_student(&pool, &s, "Other", None).await;
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/registrations", other.student_id),
        json!({"academicTermId": cur, "lines": lines}),
        None,
        StatusCode::CREATED,
    )
    .await;
    read(
        &app,
        &me,
        &format!("{API}/me/registrations/{}", id(&o)),
        StatusCode::NOT_FOUND,
    )
    .await;
    ok(
        &app,
        &me,
        "POST",
        &format!("{API}/me/registrations/{}/cancel", id(&o)),
        json!({"reason": "x"}),
        ver(&o),
        StatusCode::NOT_FOUND,
    )
    .await;
}

// ------------------------------------------------------------------ L3-C-026

struct Dues;

#[async_trait::async_trait]
impl FinanceClearancePort for Dues {
    async fn admission_clearance(
        &self,
        _: &ExecutionScope,
        _: AdmissionFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        Ok(FinanceEvidence::NotApplicable)
    }
    async fn student_dues(
        &self,
        _: &ExecutionScope,
        _: StudentFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        Ok(FinanceEvidence::Outstanding {
            detail: "semester fee".into(),
        })
    }
}

#[tokio::test]
async fn registration_holds_and_finance_evidence() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b04";
    let (app, pool, admin, s) = setup(T, "Hold University").await;
    let (_, _, cur) = terms(&s, today(&pool).await);
    let st = b3::seed_student(&pool, &s, "Dev", None).await;
    let sid = st.student_id;
    open_window(&app, &admin, &pool, &s, &cur, None).await;
    let holds = format!("{API}/students/{sid}/holds");
    ok(
        &app,
        &admin,
        "POST",
        &holds,
        json!({"source": "documents", "reason": " "}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let h = ok(
        &app,
        &admin,
        "POST",
        &holds,
        json!({"source": "documents", "reason": "Migration certificate pending"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(h["status"], "active");
    assert_eq!(events(&pool, &id(&h), "registration_hold.placed").await, 1);
    let lines = json!([{"curriculumComponentId": comp(&s, "CS101")}, {"curriculumComponentId": comp(&s, "MD101")}]);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/registrations"),
        json!({"academicTermId": cur, "lines": lines}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let list = read(&app, &admin, &holds, StatusCode::OK).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(list["financeEvidence"]["kind"], "notApplicable");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-holds/{}/release", id(&h)),
        json!({}),
        ver(&h),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let h = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-holds/{}/release", id(&h)),
        json!({"reason": "Certificate received"}),
        ver(&h),
        StatusCode::OK,
    )
    .await;
    assert_eq!(h["status"], "released");
    assert_eq!(
        events(&pool, &id(&h), "registration_hold.released").await,
        1
    );
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-holds/{}/release", id(&h)),
        json!({"reason": "again"}),
        ver(&h),
        StatusCode::CONFLICT,
    )
    .await;
    register(&app, &admin, sid, &cur, lines.clone()).await;

    // Finance-sourced facts come through FinanceClearancePort: outstanding dues block
    // registration, and a finance hold cannot be released while Finance reports dues.
    let mut fst = state(&pool);
    fst.finance = Arc::new(Dues);
    let tenant: TenantId = T.parse().unwrap();
    let other = b3::seed_student(&pool, &s, "Esha", None).await;
    let cmd = serde_json::from_value(json!({"academicTermId": cur, "lines": lines})).unwrap();
    let e = sutra_student::registration::repo::create_registration(
        &fst,
        tenant,
        StudentId(other.student_id),
        cmd,
        "staff",
        &user(),
    )
    .await;
    assert!(
        matches!(e, Err(KernelError::Conflict(ref m)) if m.contains("outstanding")),
        "{e:?}"
    );
    let fh = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/holds", other.student_id),
        json!({"source": "finance", "reason": "Fees"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let e = sutra_student::registration::repo::release_hold(
        &fst,
        tenant,
        id(&fh).parse().unwrap(),
        Version(1),
        "paid".into(),
        &user(),
    )
    .await;
    assert!(matches!(e, Err(KernelError::Conflict(_))), "{e:?}");
}

// ------------------------------------------------------------------ L3-C-024 + C-027/028

#[tokio::test]
async fn backlog_and_repeat_registration() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b05";
    let (app, pool, admin, s) = setup(T, "Backlog University").await;
    let t = today(&pool).await;
    let (t1, t2, _) = terms(&s, t);
    let st = b3::seed_student(&pool, &s, "Farah", None).await;
    let sid = st.student_id;
    // Term 1: register CS101 + MD101, finalise.
    let w1 = open_window(&app, &admin, &pool, &s, &t1, None).await;
    let r1 = register(&app, &admin, sid, &t1, json!([{"curriculumComponentId": comp(&s, "CS101")}, {"curriculumComponentId": comp(&s, "MD101")}])).await;
    let w1 = read(
        &app,
        &admin,
        &format!("{API}/registration-windows/{}", id(&w1)),
        StatusCode::OK,
    )
    .await;
    let w1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/close", id(&w1)),
        json!({}),
        ver(&w1),
        StatusCode::OK,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registration-windows/{}/finalise", id(&w1)),
        json!({}),
        ver(&w1),
        StatusCode::OK,
    )
    .await;
    // Decision with manual evidence: CS101 failed → probation; MD101 credits are earned per
    // registered component (L3-C-028), the student moves to term 2.
    let d = decide(&app, &admin, sid, json!({"academicTermId": t1, "outcome": "probation", "creditsAttempted": 20, "creditsEarned": 8,
        "failedComponentIds": [comp(&s, "CS101")], "evidenceNote": "Results entered from the board sheet"})).await;
    assert_eq!(d["evidenceSource"], "manual");
    assert_eq!(d["resultingTermNo"], 2);
    assert_eq!(ctx_term(&pool, st.context_id).await, 2);
    let ledger = read(
        &app,
        &admin,
        &format!("{API}/students/{sid}/credit-ledger"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ledger["totalCredits"], 8.0);
    assert_eq!(
        ledger["entries"][0]["curriculumComponentId"],
        comp(&s, "MD101")
    );
    let _ = r1;

    // Term 2 window: CS101 as backlog or repeat, MD101 is earned (not a backlog), regular term-2 work.
    open_window(&app, &admin, &pool, &s, &t2, None).await;
    let regs = format!("{API}/students/{sid}/registrations");
    ok(&app, &admin, "POST", &regs, json!({"academicTermId": t2, "lines": [{"curriculumComponentId": comp(&s, "MD101"), "kind": "backlog"}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    ok(&app, &admin, "POST", &regs, json!({"academicTermId": t2, "lines": [{"curriculumComponentId": comp(&s, "CS201"), "kind": "backlog"}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    ok(&app, &admin, "POST", &regs, json!({"academicTermId": t2, "lines": [{"curriculumComponentId": comp(&s, "MD201"), "kind": "repeat"}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let opts = read(
        &app,
        &admin,
        &format!("{API}/students/{sid}/registrations/options"),
        StatusCode::OK,
    )
    .await;
    let cs101 = opts["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["slotCode"] == "CS101")
        .unwrap();
    assert_eq!(cs101["suggestedKind"], "repeat");
    let md101 = opts["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["slotCode"] == "MD101")
        .unwrap();
    assert_eq!(md101["earned"], true);
    // 12 (repeat) + 12 + 8 = 32 > max 28 → credit limit.
    ok(&app, &admin, "POST", &regs, json!({"academicTermId": t2, "lines": [
        {"curriculumComponentId": comp(&s, "CS101"), "kind": "repeat"},
        {"curriculumComponentId": comp(&s, "CS201")}, {"curriculumComponentId": comp(&s, "MD201")}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let r = register(&app, &admin, sid, &t2, json!([{"curriculumComponentId": comp(&s, "CS101"), "kind": "repeat"}, {"curriculumComponentId": comp(&s, "CS201")}])).await;
    let kinds: Vec<_> = r["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["kind"].as_str().unwrap().to_owned())
        .collect();
    assert!(kinds.contains(&"repeat".to_owned()) && kinds.contains(&"regular".to_owned()));
    assert_eq!(r["termNo"], 2);
}

// ------------------------------------------------------------------ L3-C-075

#[tokio::test]
async fn progression_policy_lifecycle() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b06";
    let (app, pool, admin, s) = setup(T, "Policy University").await;
    let base = format!(
        "{API}/institutions/{}/progression-policies",
        s.institution_id
    );
    let bad =
        json!({"progressMinRatio": 0.5, "conditionalMinRatio": 0.75, "probationMinRatio": 0.6});
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"code": "UG", "name": "UG", "effectiveFrom": "2026-06-01", "rules": bad}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(&app, &admin, "POST", &base, json!({"code": "UG", "name": "UG", "effectiveFrom": "2026-06-01", "rules": std_rules(), "programmeId": Uuid::now_v7()}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    ok(
        &app,
        &faculty,
        "POST",
        &base,
        json!({"code": "UG", "name": "UG", "effectiveFrom": "2026-06-01", "rules": std_rules()}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let p = ok(&app, &admin, "POST", &base, json!({"code": "UG", "name": "UG", "effectiveFrom": "2026-06-01", "rules": std_rules(), "programmeId": s.programme_id}), None, StatusCode::CREATED).await;
    assert_eq!(p["status"], "draft");
    assert_eq!(p["versionNo"], 1);
    let p = ok(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/progression-policies/{}", id(&p)),
        json!({"name": "UG progression 2026"}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-policies/{}/revise", id(&p)),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let p = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-policies/{}/submit", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "effective");
    assert!(p["processInstanceId"].is_string());
    assert_eq!(
        events(&pool, &id(&p), "progression_policy.effective").await,
        1
    );
    ok(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/progression-policies/{}", id(&p)),
        json!({"name": "x"}),
        ver(&p),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"code": "UG", "name": "UG", "effectiveFrom": "2026-06-01", "rules": std_rules()}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // New version → effective; the old one is superseded (history preserved).
    let v2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-policies/{}/revise", id(&p)),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(v2["versionNo"], 2);
    assert_eq!(v2["supersedesPolicyId"], id(&p));
    let mut rules = std_rules();
    rules["allowRevision"] = json!(false);
    let v2 = ok(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/progression-policies/{}", id(&v2)),
        json!({"rules": rules}),
        ver(&v2),
        StatusCode::OK,
    )
    .await;
    let v2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-policies/{}/submit", id(&v2)),
        json!({}),
        ver(&v2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v2["status"], "effective");
    let old = read(
        &app,
        &admin,
        &format!("{API}/progression-policies/{}", id(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(old["status"], "superseded");
    // The effective version governs evaluations (summary) …
    let st = b3::seed_student(&pool, &s, "Gita", None).await;
    // Audit C3: the progression summary stays on `student.read` (faculty 403).
    read(
        &app,
        &faculty,
        &format!("{API}/students/{}/progression", st.student_id),
        StatusCode::FORBIDDEN,
    )
    .await;
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", st.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["effectivePolicy"]["id"], id(&v2));
    // … and forbids fresh evaluation of its decisions (L4-C-038 continuation).
    let (t1, _, _) = terms(&s, today(&pool).await);
    let d = decide(
        &app,
        &admin,
        st.student_id,
        json!({"academicTermId": t1, "creditsAttempted": 20, "creditsEarned": 20}),
    )
    .await;
    assert_eq!(d["outcome"], "progressed");
    assert_eq!(d["recommendedOutcome"], "progressed");
    assert_eq!(d["policyId"], id(&v2));
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-decisions/{}/revisions", id(&d)),
        json!({"trigger": "revaluation", "reason": "Re-evaluated", "creditsEarned": 16}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // Retire.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-policies/{}/retire", id(&v2)),
        json!({}),
        ver(&v2),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-policies/{}/retire", id(&v2)),
        json!({"reason": "Replaced by regulations 2027"}),
        ver(&v2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "retired");
    assert_eq!(
        events(&pool, &id(&v2), "progression_policy.retired").await,
        1
    );
    assert_eq!(
        read(&app, &faculty, &base, StatusCode::OK).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// ------------------------------------------------------------------ L3-C-027 / 029 / 030 / 031

#[tokio::test]
async fn progression_decisions_conditions_probation_detention() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b07";
    let (app, pool, admin, s) = setup(T, "Progression University").await;
    let (t1, t2, _) = terms(&s, today(&pool).await);
    policy(&app, &admin, s.institution_id, "UG", std_rules()).await;

    // Progressed (C-027): evidence required while Exams publishes nothing.
    let a = b3::seed_student(&pool, &s, "Hari", None).await;
    let decisions = format!("{API}/students/{}/progression-decisions", a.student_id);
    ok(
        &app,
        &admin,
        "POST",
        &decisions,
        json!({"academicTermId": t1}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let d = decide(
        &app,
        &admin,
        a.student_id,
        json!({"academicTermId": t1, "creditsAttempted": 20, "creditsEarned": 20}),
    )
    .await;
    assert_eq!(d["outcome"], "progressed");
    assert_eq!(ctx_term(&pool, a.context_id).await, 2);
    for e in ["progression.evaluated", "progression.student_progressed"] {
        assert_eq!(events(&pool, &id(&d), e).await, 1, "{e}");
    }
    assert_eq!(
        events(&pool, &a.student_id.to_string(), "credit_ledger.updated").await,
        1
    );
    ok(
        &app,
        &admin,
        "POST",
        &decisions,
        json!({"academicTermId": t1, "creditsEarned": 20}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", a.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "good_standing");
    assert_eq!(sum["totalCredits"], 20.0);
    assert_eq!(sum["currentTermNo"], 2);

    // Conditional promotion (C-029): conditions required; fulfilment restores good standing.
    let b = b3::seed_student(&pool, &s, "Isha", None).await;
    let bd = format!("{API}/students/{}/progression-decisions", b.student_id);
    ok(&app, &admin, "POST", &bd, json!({"academicTermId": t1, "creditsAttempted": 20, "creditsEarned": 16, "failedComponentIds": [comp(&s, "MD101")]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let d = decide(&app, &admin, b.student_id, json!({"academicTermId": t1, "creditsAttempted": 20, "creditsEarned": 16, "failedComponentIds": [comp(&s, "MD101")],
        "conditions": [{"description": "Clear MD101 in the supplementary exam", "curriculumComponentId": comp(&s, "MD101"), "dueOn": "2027-01-15"},
                       {"description": "Submit the lab record"}]})).await;
    assert_eq!(d["outcome"], "conditionally_progressed");
    assert_eq!(
        events(&pool, &id(&d), "progression.conditional_promotion_granted").await,
        1
    );
    let conds = d["conditions"].as_array().unwrap().clone();
    assert_eq!(conds.len(), 2);
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", b.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "conditional");
    assert_eq!(sum["pendingConditions"].as_array().unwrap().len(), 2);
    let c0 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-conditions/{}/fulfil", id(&conds[0])),
        json!({"reason": "Passed"}),
        ver(&conds[0]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c0["status"], "fulfilled");
    assert_eq!(
        events(&pool, &id(&c0), "progression_condition.fulfilled").await,
        1
    );
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-conditions/{}/fulfil", id(&c0)),
        json!({}),
        ver(&c0),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-conditions/{}/waive", id(&conds[1])),
        json!({}),
        ver(&conds[1]),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-conditions/{}/waive", id(&conds[1])),
        json!({"reason": "Lab record exempted"}),
        ver(&conds[1]),
        StatusCode::OK,
    )
    .await;
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", b.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "good_standing");

    // Failed condition → probation (C-029 failure path).
    let b2 = b3::seed_student(&pool, &s, "Jai", None).await;
    let d = decide(&app, &admin, b2.student_id, json!({"academicTermId": t1, "creditsAttempted": 20, "creditsEarned": 16, "failedComponentIds": [comp(&s, "MD101")],
        "conditions": [{"description": "Clear MD101"}]})).await;
    let c = &d["conditions"][0];
    let c = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-conditions/{}/fail", id(c)),
        json!({"reason": "Failed again"}),
        ver(c),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        events(&pool, &id(&c), "progression_condition.failed").await,
        1
    );
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", b2.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "probation");
    assert_eq!(sum["probationCases"][0]["status"], "active");

    // Probation (C-030) → escalation → detention on the next evaluation (C-031).
    let c3 = b3::seed_student(&pool, &s, "Kavya", None).await;
    let d = decide(&app, &admin, c3.student_id, json!({"academicTermId": t1, "creditsAttempted": 20, "creditsEarned": 12, "failedComponentIds": [comp(&s, "MD101")],
        "probationReviewDueOn": "2027-01-31"})).await;
    assert_eq!(d["outcome"], "probation");
    assert_eq!(
        events(&pool, &id(&d), "progression.academic_probation_assigned").await,
        1
    );
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", c3.student_id),
        StatusCode::OK,
    )
    .await;
    let pc = sum["probationCases"][0].clone();
    assert_eq!(pc["reviewDueOn"], "2027-01-31");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/probation-cases/{}/escalate", id(&pc)),
        json!({}),
        ver(&pc),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let pc = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/probation-cases/{}/escalate", id(&pc)),
        json!({"reason": "Review failed"}),
        ver(&pc),
        StatusCode::OK,
    )
    .await;
    assert_eq!(pc["status"], "escalated");
    assert_eq!(events(&pool, &id(&pc), "probation.resolved").await, 1);
    let d2 = ok(&app, &admin, "POST", &format!("{API}/students/{}/progression-decisions", c3.student_id),
        json!({"academicTermId": t2, "creditsAttempted": 20, "creditsEarned": 12, "failedComponentIds": [comp(&s, "CS201")]}), None, StatusCode::CREATED).await;
    assert_eq!(
        d2["recommendedOutcome"], "detained",
        "repeat probation escalates: {d2}"
    );
    assert_eq!(d2["repeatUnit"], "term");
    let d2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-decisions/{}/submit", id(&d2)),
        json!({}),
        ver(&d2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d2["resultingTermNo"], 2, "detained: repeats term 2");
    assert_eq!(
        events(&pool, &id(&d2), "progression.academic_detention_applied").await,
        1
    );
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", c3.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "detained");
    assert_eq!(sum["detentionCases"][0]["repeatUnit"], "term");

    // Clearing a probation returns good standing.
    let c4 = b3::seed_student(&pool, &s, "Lata", None).await;
    decide(&app, &admin, c4.student_id, json!({"academicTermId": t1, "creditsAttempted": 20, "creditsEarned": 12, "failedComponentIds": [comp(&s, "MD101")]})).await;
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", c4.student_id),
        StatusCode::OK,
    )
    .await;
    let pc = sum["probationCases"][0].clone();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/probation-cases/{}/clear", id(&pc)),
        json!({"reason": "Improved"}),
        ver(&pc),
        StatusCode::OK,
    )
    .await;
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", c4.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "good_standing");
    // A detained student registers repeat/backlog only; the list endpoint shows every decision.
    assert_eq!(
        read(
            &app,
            &admin,
            &format!("{API}/students/{}/progression-decisions", c3.student_id),
            StatusCode::OK
        )
        .await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// ------------------------------------------------------------------ L3-C-031 undetermined repeat, L3-C-038

#[tokio::test]
async fn detention_repeat_determination_and_decision_revision() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b08";
    let (app, pool, admin, s) = setup(T, "Revision University").await;
    let (t1, t2, _) = terms(&s, today(&pool).await);

    // No policy: the outcome must be chosen; the repeat consequence may stay undetermined.
    let a = b3::seed_student(&pool, &s, "Mina", None).await;
    set_term(&pool, a.context_id, 2).await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/progression-decisions", a.student_id),
        json!({"academicTermId": t2, "creditsEarned": 4}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let d = decide(&app, &admin, a.student_id, json!({"academicTermId": t2, "outcome": "detained", "creditsAttempted": 20, "creditsEarned": 4})).await;
    assert!(d["repeatUnit"].is_null());
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", a.student_id),
        StatusCode::OK,
    )
    .await;
    let dc = sum["detentionCases"][0].clone();
    assert!(dc["repeatUnit"].is_null() && dc["determinedAt"].is_null());
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/detention-cases/{}/determine", id(&dc)),
        json!({"repeatUnit": "decade"}),
        ver(&dc),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let dc = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/detention-cases/{}/determine", id(&dc)),
        json!({"repeatUnit": "year"}),
        ver(&dc),
        StatusCode::OK,
    )
    .await;
    assert_eq!(dc["repeatTermNo"], 1, "repeat the year → back to term 1");
    assert_eq!(ctx_term(&pool, a.context_id).await, 1);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/detention-cases/{}/determine", id(&dc)),
        json!({"repeatUnit": "term"}),
        ver(&dc),
        StatusCode::CONFLICT,
    )
    .await;

    // Revision after re-evaluation (C-038): a new decision supersedes; history is preserved.
    let b = b3::seed_student(&pool, &s, "Neel", None).await;
    let d = decide(&app, &admin, b.student_id, json!({"academicTermId": t1, "outcome": "probation", "creditsAttempted": 20, "creditsEarned": 10})).await;
    assert_eq!(ctx_term(&pool, b.context_id).await, 2);
    let revs = format!("{API}/progression-decisions/{}/revisions", id(&d));
    ok(
        &app,
        &admin,
        "POST",
        &revs,
        json!({"trigger": "revaluation", "reason": "Re-evaluated"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // Same outcome and evidence → recorded, no new decision.
    let same = ok(
        &app,
        &admin,
        "POST",
        &revs,
        json!({"trigger": "appeal", "reason": "Appeal heard", "outcome": "probation"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(same["revision"]["sameOutcome"], true);
    assert_eq!(same["decision"]["id"], id(&d));
    assert_eq!(same["decision"]["status"], "effective");
    let rev = ok(&app, &admin, "POST", &revs, json!({"trigger": "revaluation", "reason": "Re-evaluation published", "evidenceReference": "RV-2026-114",
        "outcome": "progressed", "creditsEarned": 20}), None, StatusCode::CREATED).await;
    assert_eq!(rev["revision"]["sameOutcome"], false);
    let nd = rev["decision"].clone();
    assert_eq!(nd["status"], "effective");
    assert_eq!(nd["supersedesDecisionId"], id(&d));
    assert_eq!(nd["resultingTermNo"], 2);
    assert_eq!(
        events(&pool, &id(&nd), "progression.decision_revised").await,
        1
    );
    let old = read(
        &app,
        &admin,
        &format!("{API}/progression-decisions/{}", id(&d)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(old["status"], "superseded");
    assert_eq!(
        old["creditsEarned"], 10.0,
        "the prior decision is not mutated"
    );
    let ledger = read(
        &app,
        &admin,
        &format!("{API}/students/{}/credit-ledger", b.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ledger["totalCredits"], 20.0, "{ledger}");
    assert!(ledger["entries"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["kind"] == "revoked"));
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", b.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "good_standing");
    assert_eq!(sum["probationCases"][0]["status"], "cleared");
    assert_eq!(
        read(&app, &admin, &revs, StatusCode::OK).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // Only an effective decision can be re-evaluated; revision rows are append-only.
    ok(
        &app,
        &admin,
        "POST",
        &revs,
        json!({"trigger": "appeal", "reason": "again", "creditsEarned": 5}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    assert!(
        sqlx::query("UPDATE student.progression_revision SET reason = 'x'")
            .execute(&pool)
            .await
            .is_err()
    );
}

// ------------------------------------------------------------------ L3-C-028

#[tokio::test]
async fn credit_ledger_is_append_only() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b09";
    let (app, pool, admin, s) = setup(T, "Ledger University").await;
    let st = b3::seed_student(&pool, &s, "Om", None).await;
    let l = format!("{API}/students/{}/credit-ledger", st.student_id);
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "earned", "credits": 12, "note": " "}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "earned", "credits": -2, "note": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "transferred_in", "credits": 4, "note": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let led = ok(&app, &admin, "POST", &l, json!({"kind": "earned", "credits": 12, "curriculumComponentId": comp(&s, "CS101"), "note": "Board sheet"}), None, StatusCode::CREATED).await;
    let e1 = led["entries"][0]["id"].as_str().unwrap().to_owned();
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "revoked", "credits": 2, "note": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "revoked", "credits": 13, "reversesEntryId": e1, "note": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "adjusted", "credits": -20, "note": "too much"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let led = ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "adjusted", "credits": -2, "note": "Moderation"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(led["totalCredits"], 10.0);
    // A reversal that would make the balance negative is refused.
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "revoked", "credits": 12, "reversesEntryId": e1, "note": "Malpractice"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    ok(&app, &admin, "POST", &l, json!({"kind": "earned", "credits": 8, "curriculumComponentId": comp(&s, "MD101"), "note": "Board sheet"}), None, StatusCode::CREATED).await;
    let led = ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "revoked", "credits": 12, "reversesEntryId": e1, "note": "Malpractice"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(led["totalCredits"], 6.0);
    ok(
        &app,
        &admin,
        "POST",
        &l,
        json!({"kind": "revoked", "credits": 1, "reversesEntryId": e1, "note": "again"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let rev = led["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "revoked")
        .unwrap();
    assert_eq!(rev["reversesEntryId"], e1);
    assert_eq!(rev["curriculumComponentId"], comp(&s, "CS101"));
    assert_eq!(
        events(&pool, &st.student_id.to_string(), "credit_ledger.updated").await,
        4
    );
    assert!(
        sqlx::query("UPDATE student.credit_ledger_entry SET credits = 1")
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(sqlx::query("DELETE FROM student.credit_ledger_entry")
        .execute(&pool)
        .await
        .is_err());
}

// ------------------------------------------------------------------ L3-C-032

#[tokio::test]
async fn credit_transfer_and_recognition() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b10";
    let (app, pool, admin, s) = setup(T, "Transfer University").await;
    let st = b3::seed_student(&pool, &s, "Pia", None).await;
    let base = format!("{API}/students/{}/credit-transfers", st.student_id);
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"sourceInstitution": "IIT Bombay", "items": []}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(&app, &admin, "POST", &base, json!({"sourceInstitution": "IIT Bombay", "items": [{"title": "Calculus", "credits": 4, "curriculumComponentId": Uuid::now_v7()}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let t = ok(&app, &admin, "POST", &base, json!({"sourceInstitution": "IIT Bombay", "sourceProgramme": "B.Tech EE",
        "items": [{"title": "Programming I", "credits": 12, "curriculumComponentId": comp(&s, "CS201")}, {"title": "MOOC: Ethics", "credits": 2}]}), None, StatusCode::CREATED).await;
    assert_eq!(t["status"], "requested");
    assert_eq!(t["creditsRequested"], 14.0);
    // Incomplete evidence: recognition is not finalised (revision required) → 422.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/credit-transfers/{}/submit", id(&t)),
        json!({}),
        ver(&t),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let t = ok(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/credit-transfers/{}", id(&t)),
        json!({"evidenceNote": "Transcript verified", "externalReference": "TR-88"}),
        ver(&t),
        StatusCode::OK,
    )
    .await;
    let t = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/credit-transfers/{}/submit", id(&t)),
        json!({}),
        ver(&t),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t["status"], "completed", "{t}");
    assert_eq!(t["creditsRecognised"], 14.0);
    assert_eq!(events(&pool, &id(&t), "credit_transfer.approved").await, 1);
    let ledger = read(
        &app,
        &admin,
        &format!("{API}/students/{}/credit-ledger", st.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ledger["totalCredits"], 14.0);
    assert!(ledger["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["kind"] == "transferred_in" && e["sourceType"] == "credit_transfer"));
    // Equivalency: CS201 is now earned.
    ok(&app, &admin, "POST", &base, json!({"sourceInstitution": "X", "items": [{"title": "Programming", "credits": 12, "curriculumComponentId": comp(&s, "CS201")}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;

    // AwaitDecision: revise → requested; reject → no ledger entry (L4-C-032-BH-005).
    let mut ast = state(&pool);
    ast.approvals = ApprovalMode::AwaitDecision;
    let tenant: TenantId = T.parse().unwrap();
    let t2 = ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"kind": "recognised", "sourceInstitution": "NPTEL", "evidenceNote": "Certificate",
        "items": [{"title": "Data Science", "credits": 4}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let tid: Uuid = id(&t2).parse().unwrap();
    use sutra_student::progression::{domain::RecognitionDecision, mobility};
    let p = mobility::submit_transfer(&ast, tenant, tid, Version(1), &user())
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "under_review");
    let p = mobility::decide_transfer(
        &ast,
        tenant,
        tid,
        Version(p.version),
        RecognitionDecision::Revise,
        Some("Attach the grade sheet".into()),
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(p.status.as_str(), "requested");
    assert!(p.revision_requested);
    let p = mobility::submit_transfer(&ast, tenant, tid, Version(p.version), &user())
        .await
        .unwrap();
    assert!(mobility::decide_transfer(
        &ast,
        tenant,
        tid,
        Version(p.version),
        RecognitionDecision::Reject,
        None,
        &user()
    )
    .await
    .is_err());
    let p = mobility::decide_transfer(
        &ast,
        tenant,
        tid,
        Version(p.version),
        RecognitionDecision::Reject,
        Some("Not equivalent".into()),
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(p.status.as_str(), "rejected");
    let ledger = read(
        &app,
        &admin,
        &format!("{API}/students/{}/credit-ledger", st.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ledger["totalCredits"], 14.0);
    assert_eq!(
        read(&app, &admin, &base, StatusCode::OK).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// ------------------------------------------------------------------ L3-C-033

/// A second published programme + curriculum in the institution (the structure exists).
async fn second_programme(app: &Router, admin: &Login, inst: Uuid) -> (Value, Value) {
    const B: &str = "/api/v1/institution";
    let p = ok(app, admin, "POST", &format!("{B}/institutions/{inst}/programmes"),
        json!({"code": "BTECH-ECE", "name": "B.Tech ECE", "level": "ug", "degreeType": "btech", "durationTerms": 4}), None, StatusCode::CREATED).await;
    let p = ok(
        app,
        admin,
        "POST",
        &format!("{B}/programmes/{}/publish", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    let mut components = vec![];
    for t in 1..=4 {
        components.push(json!({"termNo": t, "slotCode": format!("EC{t}01"), "title": "Core", "category": "major_core", "credits": 12.0, "lectureHours": 3, "tutorialHours": 1}));
        components.push(json!({"termNo": t, "slotCode": format!("MD{t}01"), "title": "Multidisciplinary", "category": "multidisciplinary", "credits": 8.0, "lectureHours": 3, "tutorialHours": 1}));
    }
    let c = ok(app, admin, "POST", &format!("{B}/programmes/{}/curricula", id(&p)), json!({"code": "ECE-2026", "title": "B.Tech ECE 2026", "initialVersion": {
        "effectiveFrom": "2026-07-01", "appliesFromCohort": "2026-27", "termStructure": "semester", "termCount": 4, "creditFramework": "nep2020",
        "minTotalCredits": 80, "minCreditsPerTerm": 16, "maxCreditsPerTerm": 28, "nepAligned": true, "nheqfLevel": 6, "multipleEntryExit": false,
        "abcEnabled": true, "exitAwards": [], "components": components}}), None, StatusCode::CREATED).await;
    let vid = c["openVersionId"].as_str().unwrap().to_owned();
    let v = read(
        app,
        admin,
        &format!("{B}/curriculum-versions/{vid}"),
        StatusCode::OK,
    )
    .await;
    let v = ok(
        app,
        admin,
        "POST",
        &format!("{B}/curriculum-versions/{vid}/publish"),
        json!({}),
        ver(&v),
        StatusCode::OK,
    )
    .await;
    (p, v)
}

#[tokio::test]
async fn programme_migration() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b11";
    let (app, pool, admin, s) = setup(T, "Migration University").await;
    let (_, _, cur) = terms(&s, today(&pool).await);
    let (ece, ece_cv) = second_programme(&app, &admin, s.institution_id).await;
    let st = b3::seed_student(&pool, &s, "Ravi", None).await;
    let base = format!("{API}/students/{}/programme-migrations", st.student_id);
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"toProgrammeId": s.programme_id, "toTermNo": 1, "reason": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"toProgrammeId": Uuid::now_v7(), "toTermNo": 1, "reason": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"toProgrammeId": id(&ece), "toTermNo": 9, "reason": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(&app, &admin, "POST", &base, json!({"toProgrammeId": id(&ece), "toTermNo": 1, "reason": "x", "creditMapping": [{"toComponentId": comp(&s, "CS101"), "credits": 12}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    // Domain B's curriculum payload carries no component ids (owner-pool lookup).
    let ec101: Uuid = sqlx::query_scalar(
        "SELECT id FROM institution.curriculum_component \
         WHERE curriculum_version_id = $1 AND slot_code = 'EC101'",
    )
    .bind(id(&ece_cv).parse::<Uuid>().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    let ec101 = ec101.to_string();
    let m = ok(&app, &admin, "POST", &base, json!({"toProgrammeId": id(&ece), "toTermNo": 1, "reason": "Branch change after year 1",
        "creditMapping": [{"fromComponentId": comp(&s, "MD101"), "toComponentId": ec101, "credits": 8, "note": "equivalent"}]}), None, StatusCode::CREATED).await;
    assert_eq!(m["status"], "requested");
    assert_eq!(
        m["toCurriculumVersionId"],
        id(&ece_cv),
        "defaults to the current published version"
    );
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"toProgrammeId": id(&ece), "toTermNo": 1, "reason": "dup"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // An open registration blocks the migration (the decision stays unresolved).
    open_window(&app, &admin, &pool, &s, &cur, None).await;
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/registrations", st.student_id),
        json!({"academicTermId": cur}),
        None,
        StatusCode::CREATED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/programme-migrations/{}/submit", id(&m)),
        json!({}),
        ver(&m),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        read(
            &app,
            &admin,
            &format!("{API}/programme-migrations/{}", id(&m)),
            StatusCode::OK
        )
        .await["status"],
        "requested"
    );
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{}/cancel", id(&r)),
        json!({"reason": "Migrating"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    let m = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/programme-migrations/{}/submit", id(&m)),
        json!({}),
        ver(&m),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m["status"], "completed", "{m}");
    assert!(m["effectiveOn"].is_string());
    assert_eq!(
        events(&pool, &id(&m), "programme_migration.completed").await,
        1
    );
    let (prog, cv): (Uuid, Uuid) = sqlx::query_as(
        "SELECT programme_id, curriculum_version_id FROM student.student_context WHERE id = $1",
    )
    .bind(st.context_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(prog.to_string(), id(&ece));
    assert_eq!(cv.to_string(), id(&ece_cv));
    // The registration options now follow the destination curriculum.
    let opts = read(
        &app,
        &admin,
        &format!("{API}/students/{}/registrations/options", st.student_id),
        StatusCode::OK,
    )
    .await;
    assert!(opts["components"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["slotCode"] == "EC101"));
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/programme-migrations/{}/withdraw", id(&m)),
        json!({}),
        ver(&m),
        StatusCode::CONFLICT,
    )
    .await;
}

// ------------------------------------------------------------------ L3-C-034 / 035

#[tokio::test]
async fn academic_break_and_rejoining() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b12";
    let (app, pool, admin, s) = setup(T, "Break University").await;
    let t = today(&pool).await;
    let (_, _, cur) = terms(&s, t);
    open_window(&app, &admin, &pool, &s, &cur, None).await;
    let st = b3::seed_student(&pool, &s, "Sana", None).await;
    let base = format!("{API}/students/{}/academic-breaks", st.student_id);
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"reason": "Medical", "startsOn": t, "expectedReturnOn": t - chrono::Days::new(1)}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // Future break: approved, not yet in effect; it cannot start early.
    let f = ok(&app, &admin, "POST", &base, json!({"reason": "Internship abroad", "startsOn": t + chrono::Days::new(30), "expectedReturnOn": t + chrono::Days::new(200)}), None, StatusCode::CREATED).await;
    assert_eq!(f["status"], "approved", "{f}");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/academic-breaks/{}/start", id(&f)),
        json!({}),
        ver(&f),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"reason": "dup", "startsOn": t, "expectedReturnOn": t}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/academic-breaks/{}/withdraw", id(&f)),
        json!({"reason": "Internship cancelled"}),
        ver(&f),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["status"], "withdrawn");

    // An open registration blocks the break from taking effect.
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/registrations", st.student_id),
        json!({"academicTermId": cur}),
        None,
        StatusCode::CREATED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"reason": "Medical", "startsOn": t, "expectedReturnOn": t + chrono::Days::new(120)}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{}/cancel", id(&r)),
        json!({"reason": "Going on break"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    let b = ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"reason": "Medical", "startsOn": t, "expectedReturnOn": t + chrono::Days::new(120)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(b["status"], "in_effect", "{b}");
    assert_eq!(b["termNoAtStart"], 1);
    assert!(b["processInstanceId"].is_string());
    assert_eq!(events(&pool, &id(&b), "academic_break.started").await, 1);
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", st.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "academic_break");
    // No registration during the break.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/registrations", st.student_id),
        json!({"academicTermId": cur}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"reason": "again", "startsOn": t, "expectedReturnOn": t}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // Rejoining: placement within the curriculum; curriculum unchanged → no mapping needed.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/academic-breaks/{}/rejoin", id(&b)),
        json!({"termNo": 7}),
        ver(&b),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let j = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/academic-breaks/{}/rejoin", id(&b)),
        json!({"termNo": 1}),
        ver(&b),
        StatusCode::OK,
    )
    .await;
    assert_eq!(j["status"], "rejoined");
    assert_eq!(j["rejoinTermNo"], 1);
    assert_eq!(j["rejoinedOn"], t.to_string());
    assert_eq!(events(&pool, &id(&j), "academic_break.rejoined").await, 1);
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", st.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "good_standing");
    // Registration eligibility is restored.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/registrations", st.student_id),
        json!({"academicTermId": cur}),
        None,
        StatusCode::CREATED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/academic-breaks/{}/rejoin", id(&j)),
        json!({}),
        ver(&j),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        read(&app, &admin, &base, StatusCode::OK).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

// ------------------------------------------------------------------ L3-C-036 / 037

struct AcceptingAbc;

#[async_trait::async_trait]
impl AbcRegistryPort for AcceptingAbc {
    async fn submit_credits(
        &self,
        _: &ExecutionScope,
        s: AbcCreditSubmission,
    ) -> Result<ExternalSyncOutcome, KernelError> {
        Ok(ExternalSyncOutcome::Accepted {
            reference: format!("ABC-{}", s.ledger_entry_ids.len()),
        })
    }
}

#[tokio::test]
async fn qualification_eligibility_and_abc_sync() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b13";
    let (app, pool, admin, s) = setup(T, "Eligibility University").await;
    let st = b3::seed_student(&pool, &s, "Tara", None).await;
    let sid = st.student_id;
    let qe = format!("{API}/students/{sid}/qualification-eligibility");
    read(&app, &admin, &qe, StatusCode::NOT_FOUND).await;
    // Qualification Pending with outstanding conditions.
    let e = ok(
        &app,
        &admin,
        "POST",
        &qe,
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(e["eligible"], false);
    assert_eq!(e["basis"], "ledger");
    assert_eq!(e["requiredCredits"], 80.0);
    assert_eq!(
        e["outstanding"].as_array().unwrap().len(),
        5,
        "credits + 4 core components: {e}"
    );
    assert_eq!(
        events(&pool, &id(&e), "qualification_eligibility.updated").await,
        1
    );
    // Earn every component (80 credits): Qualification Eligible.
    let l = format!("{API}/students/{sid}/credit-ledger");
    for c in setup_components(&s) {
        ok(&app, &admin, "POST", &l, json!({"kind": "earned", "credits": c.1, "curriculumComponentId": c.0, "note": "Result"}), None, StatusCode::CREATED).await;
    }
    let e = ok(
        &app,
        &admin,
        "POST",
        &qe,
        json!({"note": "Final audit"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(e["eligible"], true, "{e}");
    assert_eq!(e["outcomes"], json!(["degree"]));
    assert_eq!(e["totalCredits"], 80.0);
    let cur = read(&app, &admin, &qe, StatusCode::OK).await;
    assert_eq!(cur["id"], e["id"]);
    assert_eq!(
        read(&app, &admin, &format!("{qe}/history"), StatusCode::OK).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // 3C reads the newest row of the table (eligibility contract).
    let (eligible, outcomes): (bool, Vec<String>) = sqlx::query_as(
        "SELECT eligible, outcomes FROM student.qualification_eligibility WHERE student_id = $1 ORDER BY evaluated_at DESC LIMIT 1")
        .bind(sid).fetch_one(&pool).await.unwrap();
    assert!(eligible);
    assert_eq!(outcomes, vec!["degree".to_owned()]);

    // ABC sync with the no-op adapter: recorded failed / not_configured; the ledger stays authoritative.
    ok(&app, &admin, "POST", &l, json!({"kind": "adjusted", "credits": 1, "note": "Provisional", "institutionApproved": false}), None, StatusCode::CREATED).await;
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/abc-syncs"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a["status"], "failed");
    assert_eq!(a["failureReason"], "not_configured");
    assert_eq!(
        a["entryIds"].as_array().unwrap().len(),
        8,
        "provisional credit excluded"
    );
    assert_eq!(a["totalCredits"], 80.0);
    assert_eq!(events(&pool, &id(&a), "abc_sync.failed").await, 1);
    read(
        &app,
        &admin,
        &format!("{API}/abc-syncs/{}", id(&a)),
        StatusCode::OK,
    )
    .await;
    // With an accepting registry: acknowledged once; a duplicate request finds nothing new.
    let mut ast = state(&pool);
    ast.abc = Arc::new(AcceptingAbc);
    let tenant: TenantId = T.parse().unwrap();
    let ok1 =
        sutra_student::progression::eligibility::synchronise(&ast, tenant, StudentId(sid), &user())
            .await
            .unwrap();
    assert_eq!(ok1.status.as_str(), "acknowledged");
    assert_eq!(ok1.external_reference.as_deref(), Some("ABC-8"));
    assert_eq!(
        events(&pool, &ok1.id.to_string(), "abc_sync.completed").await,
        1
    );
    let dup =
        sutra_student::progression::eligibility::synchronise(&ast, tenant, StudentId(sid), &user())
            .await;
    assert!(matches!(dup, Err(KernelError::Conflict(_))), "{dup:?}");
    assert_eq!(
        read(
            &app,
            &admin,
            &format!("{API}/students/{sid}/abc-syncs"),
            StatusCode::OK
        )
        .await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}

fn setup_components(s: &b3::AcademicSetup) -> Vec<(String, f64)> {
    s.curriculum_version["components"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (id(c), c["credits"].as_f64().unwrap()))
        .collect()
}

// ------------------------------------------------------------------ AwaitDecision (WorkflowPort)

#[tokio::test]
async fn approvals_await_a_decision() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b14";
    let (app, pool, admin, s) = setup(T, "Await University").await;
    let (t1, _, cur) = terms(&s, today(&pool).await);
    open_window(&app, &admin, &pool, &s, &cur, None).await;
    let mut st = state(&pool);
    st.approvals = ApprovalMode::AwaitDecision;
    let tenant: TenantId = T.parse().unwrap();
    use sutra_student::progression::repo as prog;
    use sutra_student::registration::repo as reg;

    // Registration: submitted stays pending; reject → returned; resubmit; approve → registered.
    let a = b3::seed_student(&pool, &s, "Uma", None).await;
    let cmd = serde_json::from_value(json!({"academicTermId": cur, "lines": [{"curriculumComponentId": comp(&s, "CS101")}, {"curriculumComponentId": comp(&s, "MD101")}]})).unwrap();
    let r = reg::create_registration(&st, tenant, StudentId(a.student_id), cmd, "staff", &user())
        .await
        .unwrap();
    let r = reg::submit(&st, tenant, r.id, Version(r.version), &user())
        .await
        .unwrap();
    assert_eq!(r.status, RegistrationStatus::Submitted);
    assert!(r.process_instance_id.is_some());
    assert_eq!(
        events(&pool, &r.id.to_string(), "registration.registered").await,
        0
    );
    assert!(
        reg::decide(&st, tenant, r.id, Version(r.version), false, None, &user())
            .await
            .is_err(),
        "reason required"
    );
    let r = reg::decide(
        &st,
        tenant,
        r.id,
        Version(r.version),
        false,
        Some("Wrong elective".into()),
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(r.status, RegistrationStatus::Returned);
    let r = reg::submit(&st, tenant, r.id, Version(r.version), &user())
        .await
        .unwrap();
    // The explicit decision endpoint (HTTP) completes it.
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{}/decision", r.id),
        json!({"decision": "approve"}),
        Some(r.version),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "registered");
    assert_eq!(events(&pool, &id(&r), "registration.registered").await, 1);

    // Progression decision: pending_approval → decision → effective.
    let b = b3::seed_student(&pool, &s, "Vir", None).await;
    let cmd = serde_json::from_value(
        json!({"academicTermId": t1, "outcome": "progressed", "creditsEarned": 20}),
    )
    .unwrap();
    let d = prog::create_decision(&st, tenant, StudentId(b.student_id), cmd, &user())
        .await
        .unwrap();
    let d = prog::submit_decision(&st, tenant, d.id, Version(d.version), &user())
        .await
        .unwrap();
    assert_eq!(d.status, ProgressionDecisionStatus::PendingApproval);
    assert_eq!(
        ctx_term(&pool, b.context_id).await,
        1,
        "nothing applied yet"
    );
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-decisions/{}/decision", d.id),
        json!({"decision": "approve"}),
        Some(d.version),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "effective");
    assert_eq!(ctx_term(&pool, b.context_id).await, 2);
    // A rejected decision changes nothing.
    let c = b3::seed_student(&pool, &s, "Wasim", None).await;
    let cmd = serde_json::from_value(
        json!({"academicTermId": t1, "outcome": "detained", "creditsEarned": 2}),
    )
    .unwrap();
    let d = prog::create_decision(&st, tenant, StudentId(c.student_id), cmd, &user())
        .await
        .unwrap();
    let d = prog::submit_decision(&st, tenant, d.id, Version(d.version), &user())
        .await
        .unwrap();
    let d = prog::decide_decision(
        &st,
        tenant,
        d.id,
        Version(d.version),
        false,
        Some("Evidence incomplete".into()),
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(d.status, ProgressionDecisionStatus::Rejected);
    assert_eq!(d.outcome, ProgressionOutcome::Detained);
    let sum = read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", c.student_id),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sum["standing"], "good_standing");
    assert!(sum["detentionCases"].as_array().unwrap().is_empty());

    // Policy in review → decided; academic break requested → decided.
    let p = ok(
        &app,
        &admin,
        "POST",
        &format!(
            "{API}/institutions/{}/progression-policies",
            s.institution_id
        ),
        json!({"code": "AW", "name": "Await", "effectiveFrom": "2026-06-01", "rules": std_rules()}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p = prog::submit_policy(&st, tenant, id(&p).parse().unwrap(), Version(1), &user())
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "in_review");
    let p = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/progression-policies/{}/decision", p.id),
        json!({"decision": "reject", "reason": "Thresholds too strict"}),
        Some(p.version),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "draft");
    let t = today(&pool).await;
    let cmd = serde_json::from_value(
        json!({"reason": "Family", "startsOn": t, "expectedReturnOn": t + chrono::Days::new(60)}),
    )
    .unwrap();
    let br = sutra_student::progression::mobility::create_break(
        &st,
        tenant,
        StudentId(c.student_id),
        cmd,
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(br.status.as_str(), "requested");
    let br = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/academic-breaks/{}/decision", br.id),
        json!({"decision": "approve"}),
        Some(br.version),
        StatusCode::OK,
    )
    .await;
    assert_eq!(br["status"], "in_effect");
}

// ------------------------------------------------------------------ isolation + guards

#[tokio::test]
async fn tenant_isolation_permissions_and_terminal_contexts() {
    const T: &str = "0192f5a0-7c3e-7000-8000-0000000b3b15";
    const OTHER: &str = "0192f5a0-7c3e-7000-8000-0000000b3b16";
    let (app, pool, admin, s) = setup(T, "Isolation University").await;
    let (_, _, cur) = terms(&s, today(&pool).await);
    let w = open_window(&app, &admin, &pool, &s, &cur, None).await;
    let st = b3::seed_student(&pool, &s, "Yash", None).await;
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/registrations", st.student_id),
        json!({"academicTermId": cur}),
        None,
        StatusCode::CREATED,
    )
    .await;

    // Another tenant sees nothing (404), even with the ids.
    let (app2, _) = support::app(OTHER).await;
    let admin2 = login(&app2, "admin@sutra.dev").await;
    read(
        &app2,
        &admin2,
        &format!("{API}/registrations/{}", id(&r)),
        StatusCode::NOT_FOUND,
    )
    .await;
    read(
        &app2,
        &admin2,
        &format!("{API}/registration-windows/{}", id(&w)),
        StatusCode::NOT_FOUND,
    )
    .await;
    read(
        &app2,
        &admin2,
        &format!("{API}/students/{}/progression", st.student_id),
        StatusCode::NOT_FOUND,
    )
    .await;
    read(
        &app2,
        &admin2,
        &format!(
            "{API}/institutions/{}/registration-windows",
            s.institution_id
        ),
        StatusCode::NOT_FOUND,
    )
    .await;
    ok(
        &app2,
        &admin2,
        "POST",
        &format!("{API}/students/{}/holds", st.student_id),
        json!({"source": "academic", "reason": "x"}),
        None,
        StatusCode::NOT_FOUND,
    )
    .await;

    // Faculty never write; they read only the students they teach (Audit C3: this student sits
    // in none of their sections → 404; the credit ledger is `student.read` → 403); students
    // hold no staff permission; admins no self-service.
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    read(
        &app,
        &faculty,
        &format!("{API}/students/{}/registrations", st.student_id),
        StatusCode::NOT_FOUND,
    )
    .await;
    read(
        &app,
        &faculty,
        &format!("{API}/students/{}/credit-ledger", st.student_id),
        StatusCode::FORBIDDEN,
    )
    .await;
    ok(
        &app,
        &faculty,
        "POST",
        &format!("{API}/students/{}/progression-decisions", st.student_id),
        json!({"academicTermId": cur, "creditsEarned": 1}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    read(
        &app,
        &student,
        &format!("{API}/students/{}/progression", st.student_id),
        StatusCode::FORBIDDEN,
    )
    .await;
    read(
        &app,
        &student,
        &format!("{API}/me/registrations"),
        StatusCode::NOT_FOUND,
    )
    .await;

    // A terminal context is read-only for 3B (the guard rejects every command).
    sqlx::query("UPDATE student.student_context SET status = 'withdrawn', closed_on = current_date WHERE id = $1")
        .bind(st.context_id).execute(&pool).await.unwrap();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/registrations/{}/submit", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/holds", st.student_id),
        json!({"source": "academic", "reason": "x"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{}/credit-ledger", st.student_id),
        json!({"kind": "earned", "credits": 1, "note": "x"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // Reads still work.
    read(
        &app,
        &admin,
        &format!("{API}/registrations/{}", id(&r)),
        StatusCode::OK,
    )
    .await;
    read(
        &app,
        &admin,
        &format!("{API}/students/{}/progression", st.student_id),
        StatusCode::OK,
    )
    .await;
}
