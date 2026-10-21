//! Slice 4A DB tests (owned by 4A; see `docs/batch4-contracts.md` §11). Shared harness:
//! `support::*` and the Batch 4 helpers in `support::b4`.
//!
//! Scenarios (L3/L4 D-001 … D-019): D-001 establishment → approval (WorkflowPort, auto and
//! `AwaitDecision`) → publication → enrolment window → activation with readiness; D-002/003
//! suspension / reactivation; D-004 administrative cancellation (governed); D-005 capacity
//! governance; D-006 eligibility / restriction references; D-007 waitlist → seat; D-008
//! cross-campus / cross-institution permits; D-009 revision / replacement / rollback; D-010
//! archive; D-011 merge / split; D-012 template generation; D-013 freeze → governed unfreeze;
//! D-014 enrolment windows; D-015 quotas; D-016 change simulation → impact decision; D-017
//! faculty assignment establishment; D-018 operational change; D-019 governance configuration.
//! Plus enrolment gated on 3B registrations through `RegistrationReader`, faculty "My classes",
//! student self-service, events + audit + status history, ETag/If-Match, tenant isolation and
//! permissions.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b4::DeliverySetup;
use support::*;
use sutra_academic::shared::approval::ApprovalMode;
use sutra_academic::AcademicState;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};

const A: &str = "/api/v1/academic";

// ------------------------------------------------------------------------------- helpers

fn tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let req = if method == "GET" {
        get(who, path)
    } else {
        write(who, method, path, body, if_match)
    };
    let (s, _, b) = send(app, req).await;
    (s, b)
}

async fn ok(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> Value {
    let (s, b) = call(app, who, method, path, body, if_match).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

async fn status_of(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> StatusCode {
    call(app, who, method, path, body, if_match).await.0
}

fn id(b: &Value) -> String {
    b["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {b}"))
        .to_owned()
}

fn ver(b: &Value) -> i64 {
    b["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no version in {b}"))
}

fn ev(suffix: &str) -> String {
    format!("sutraerp.academic.{suffix}")
}

async fn outbox_types(pool: &sqlx::PgPool, subject: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT event_type FROM platform.outbox WHERE subject = $1 ORDER BY id")
        .bind(subject)
        .fetch_all(pool)
        .await
        .unwrap()
}

async fn audit_actions(pool: &sqlx::PgPool, target: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT action FROM platform.audit_log WHERE target_id = $1 ORDER BY occurred_at, id",
    )
    .bind(target)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn history(pool: &sqlx::PgPool, aggregate: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT to_status FROM academic.status_history WHERE aggregate_id = $1::uuid ORDER BY recorded_at, id")
        .bind(aggregate)
        .fetch_all(pool)
        .await
        .unwrap()
}

/// Delivery setup whose offerings live in calendar term 2 (ends 2027-04-30), so creation stays
/// valid regardless of the run date in this academic year; registrations are seeded for it.
async fn setup(
    app: &Router,
    admin: &Login,
    pool: &sqlx::PgPool,
    tenant: &str,
    name: &str,
) -> DeliverySetup {
    let mut s = b4::academic_setup(app, admin, pool, tenant, name).await;
    s.term_id = s.base.term_ids[1];
    s
}

fn today() -> chrono::NaiveDate {
    (chrono::Utc::now() + chrono::Duration::minutes(330)).date_naive()
}

async fn new_offering(
    app: &Router,
    admin: &Login,
    s: &DeliverySetup,
    slot: &str,
    code: &str,
    sections: Value,
) -> Value {
    ok(app, admin, "POST", &format!("{A}/institutions/{}/offerings", s.base.institution_id),
        json!({"academicTermId": s.term_id, "curriculumVersionId": s.base.curriculum_version_id,
               "curriculumComponentId": s.component(slot).id, "offeringCode": code, "sections": sections}),
        None, StatusCode::CREATED).await
}

async fn reload(app: &Router, who: &Login, off: &Value) -> Value {
    ok(
        app,
        who,
        "GET",
        &format!("{A}/offerings/{}", id(off)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await
}

async fn step(app: &Router, who: &Login, off: &Value, verb: &str, body: Value) -> Value {
    ok(
        app,
        who,
        "POST",
        &format!("{A}/offerings/{}/{verb}", id(off)),
        body,
        Some(ver(off)),
        StatusCode::OK,
    )
    .await
}

/// `subject` teaches the section (draft → approved → active) and returns the assignment.
async fn assign(
    app: &Router,
    admin: &Login,
    section: &str,
    subject: &str,
    role: &str,
    hours: Option<f64>,
) -> Value {
    let a = ok(app, admin, "POST", &format!("{A}/sections/{section}/faculty-assignments"),
        json!({"facultySubjectRef": subject, "facultyName": "Dr. Meera Shah", "role": role, "weeklyHours": hours}),
        None, StatusCode::CREATED).await;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{A}/faculty-assignments/{}/submit", id(&a)),
        json!({}),
        Some(ver(&a)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "approved", "{a}");
    ok(
        app,
        admin,
        "POST",
        &format!("{A}/faculty-assignments/{}/activate", id(&a)),
        json!({}),
        Some(ver(&a)),
        StatusCode::OK,
    )
    .await
}

/// Draft → approved → published → faculty → enrollment_open (returns the open offering).
async fn to_open(app: &Router, admin: &Login, off: &Value) -> Value {
    let o = step(app, admin, off, "submit", json!({})).await;
    let o = step(app, admin, &o, "publish", json!({})).await;
    for s in o["sections"].as_array().unwrap() {
        assign(
            app,
            admin,
            s["id"].as_str().unwrap(),
            b4::MEERA,
            "primary_instructor",
            None,
        )
        .await;
    }
    let o = reload(app, admin, &o).await;
    step(app, admin, &o, "open-enrolment", json!({})).await
}

async fn to_active(app: &Router, admin: &Login, off: &Value) -> Value {
    let o = to_open(app, admin, off).await;
    let o = step(app, admin, &o, "close-enrolment", json!({})).await;
    step(app, admin, &o, "activate", json!({})).await
}

/// Academic router with a hand-built state and an injected session (no cookie / CSRF layer).
fn custom_app(st: AcademicState, tenant: &str, subject: &str, role: Role) -> Router {
    let (router, _) = sutra_academic::router_with_state(st).split_for_parts();
    let ctx = AuthContext {
        session_id: uuid::Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![role],
        scope: ExecutionScope::tenant(tenant.parse::<TenantId>().unwrap()),
        csrf_token: "x".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        idle_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        mfa_level: MfaLevel::Mfa,
        amr: vec!["pwd".into(), "otp".into()],
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

fn state(pool: &sqlx::PgPool, mode: ApprovalMode) -> AcademicState {
    let mut st = sutra_api::app::academic_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = mode;
    st
}

fn injected() -> Login {
    Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    }
}

// --------------------------------------------------------------------------------- tests

/// L3-D-001 (auto-approval) end to end with D-005 capacity, D-007 waitlist, D-014 windows,
/// D-015 quotas, D-002/003 suspension / reactivation, D-013 freeze → unfreeze, D-010 archive;
/// enrolment gated on 3B registrations; self-service; My classes; events, audit, history;
/// tenant isolation and permissions.
#[tokio::test]
async fn offering_lifecycle_enrolment_waitlist_and_archive() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let s = setup(&app, &admin, &pool, t, "Offering University").await;
    let inst = s.base.institution_id;
    let cs = s.component("CS101").id;

    // ---- D-001 establishment: references validated, draft with planned sections
    let bad = call(
        &app,
        &admin,
        "POST",
        &format!("{A}/institutions/{inst}/offerings"),
        json!({"academicTermId": s.term_id, "curriculumVersionId": s.base.curriculum_version_id,
               "curriculumComponentId": uuid::Uuid::now_v7(), "offeringCode": "X1"}),
        None,
    )
    .await;
    assert_eq!(bad.0, StatusCode::UNPROCESSABLE_ENTITY, "{}", bad.1);
    let off = new_offering(
        &app,
        &admin,
        &s,
        "CS101",
        "CS101-T2",
        json!([{"sectionCode": "A", "capacity": 2}]),
    )
    .await;
    assert_eq!(off["status"], "draft");
    assert_eq!(off["credits"], 12.0);
    assert_eq!(off["sections"][0]["status"], "planned");
    let dup = call(
        &app,
        &admin,
        "POST",
        &format!("{A}/institutions/{inst}/offerings"),
        json!({"academicTermId": s.term_id, "curriculumVersionId": s.base.curriculum_version_id,
               "slotCode": "CS101", "offeringCode": "cs101-t2"}),
        None,
    )
    .await;
    assert_eq!(
        dup.0,
        StatusCode::CONFLICT,
        "duplicate live code: {}",
        dup.1
    );
    let oid = id(&off);
    let section = off["sections"][0]["id"].as_str().unwrap().to_owned();

    // Draft is not publication; publish refused; If-Match enforced.
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/publish"),
            json!({}),
            Some(ver(&off))
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/submit"),
            json!({}),
            Some(99)
        )
        .await,
        StatusCode::PRECONDITION_FAILED
    );
    let (s428, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/submit"),
            json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(s428, StatusCode::PRECONDITION_REQUIRED);
    let off = ok(
        &app,
        &admin,
        "PATCH",
        &format!("{A}/offerings/{oid}"),
        json!({"title": "Programming I"}),
        Some(ver(&off)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(off["title"], "Programming I");

    // ---- approval through the WorkflowPort (auto)
    let off = step(&app, &admin, &off, "submit", json!({})).await;
    assert_eq!(off["status"], "approved");
    assert!(off["processInstanceId"].as_str().is_some());
    assert_eq!(
        status_of(
            &app,
            &admin,
            "PATCH",
            &format!("{A}/offerings/{oid}"),
            json!({"title": "x"}),
            Some(ver(&off))
        )
        .await,
        StatusCode::CONFLICT
    );

    // ---- D-014 window + D-015 quotas (need a capacity)
    let w = ok(&app, &admin, "POST", &format!("{A}/offerings/{oid}/windows"),
        json!({"kind": "regular", "opensOn": today(), "closesOn": today() + chrono::Duration::days(20)}), None, StatusCode::CREATED).await;
    assert_eq!(w["status"], "scheduled");
    assert_eq!(status_of(&app, &admin, "POST", &format!("{A}/offerings/{oid}/windows"),
        json!({"kind": "regular", "opensOn": today(), "closesOn": today() + chrono::Duration::days(5)}), None).await, StatusCode::CONFLICT, "overlap");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/quotas"),
        json!({"category": "general", "seats": 1, "rationale": "Open merit"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/quotas"),
            json!({"category": "sc", "seats": 2, "rationale": "x"}),
            None
        )
        .await,
        StatusCode::CONFLICT,
        "quotas above capacity"
    );
    let q2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/quotas"),
        json!({"category": "general", "seats": 2, "rationale": "Revised"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let quotas = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{oid}/quotas"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        quotas["items"].as_array().unwrap().len(),
        2,
        "prior quota kept as superseded history"
    );
    assert!(quotas["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|q| q["status"] == "superseded" && q["supersededBy"] == q2["id"]));

    // ---- publication, faculty, enrolment opening
    let off = reload(&app, &admin, &off).await;
    let off = step(&app, &admin, &off, "publish", json!({})).await;
    assert_eq!(off["status"], "published");
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/activate"),
            json!({}),
            Some(ver(&off))
        )
        .await,
        StatusCode::CONFLICT
    );
    let fa = assign(
        &app,
        &admin,
        &section,
        b4::MEERA,
        "primary_instructor",
        Some(4.0),
    )
    .await;
    assert_eq!(fa["status"], "active");
    let off = reload(&app, &admin, &off).await;
    let off = step(&app, &admin, &off, "open-enrolment", json!({})).await;
    assert_eq!(off["status"], "enrollment_open");
    assert_eq!(off["sections"][0]["status"], "active");
    let windows = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{oid}/windows"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(windows["items"][0]["status"], "open");

    // ---- students + 3B registrations (RegistrationReader gate)
    let aarav = b4::seed_student(&pool, &s, "Aarav", Some(b4::AARAV)).await;
    let bina = b4::seed_student(&pool, &s, "Bina", None).await;
    let chirag = b4::seed_student(&pool, &s, "Chirag", None).await;
    b4::seed_registration(&pool, &s, &aarav, &[cs], "registered").await;
    b4::seed_registration(&pool, &s, &bina, &[cs], "approved").await;

    let student = login(&app, "aarav.desai@sutra.dev").await;
    let avail = ok(
        &app,
        &student,
        "GET",
        &format!("{A}/me/courses/available"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    let mine = avail["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["offeringId"] == off["id"])
        .expect("offering available");
    assert_eq!(mine["covered"], true);
    let seat = ok(
        &app,
        &student,
        "POST",
        &format!("{A}/me/enrolments"),
        json!({"sectionId": section}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(seat["origin"], "registration");
    assert!(seat["registrationLineId"].as_str().is_some());
    assert_eq!(
        status_of(
            &app,
            &student,
            "POST",
            &format!("{A}/me/enrolments"),
            json!({"sectionId": section}),
            None
        )
        .await,
        StatusCode::CONFLICT,
        "one seat per offering"
    );
    let courses = ok(
        &app,
        &student,
        "GET",
        &format!("{A}/me/courses"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(courses["items"][0]["offeringCode"], "CS101-T2");
    assert_eq!(
        courses["items"][0]["faculty"][0]["facultySubjectRef"],
        b4::MEERA
    );
    assert_eq!(
        status_of(
            &app,
            &student,
            "GET",
            &format!("{A}/institutions/{inst}/offerings"),
            json!({}),
            None
        )
        .await,
        StatusCode::FORBIDDEN
    );

    // Unregistered student: refused unless an override with a reason is recorded.
    let path = format!("{A}/sections/{section}/enrolments");
    let refused = call(
        &app,
        &admin,
        "POST",
        &path,
        json!({"studentId": chirag.student_id}),
        None,
    )
    .await;
    assert_eq!(refused.0, StatusCode::CONFLICT, "{}", refused.1);
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &path,
            json!({"studentId": chirag.student_id, "overrideEligibility": true}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let over = ok(&app, &admin, "POST", &path, json!({"studentId": chirag.student_id, "overrideEligibility": true, "reason": "Dean approved"}), None, StatusCode::CREATED).await;
    assert_eq!(over["origin"], "override");

    // ---- D-005 capacity: full section, reduction below seats refused
    let full = call(
        &app,
        &admin,
        "POST",
        &path,
        json!({"studentId": bina.student_id}),
        None,
    )
    .await;
    assert_eq!(full.0, StatusCode::CONFLICT, "section full: {}", full.1);
    let off = reload(&app, &admin, &off).await;
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/capacity"),
            json!({"sectionId": section, "capacity": 1, "reason": "room change"}),
            Some(ver(&off))
        )
        .await,
        StatusCode::CONFLICT
    );
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/capacity"),
            json!({"sectionId": section, "capacity": -1, "reason": "x"}),
            Some(ver(&off))
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );

    // ---- D-007 waitlist → seat
    let wl = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/waitlist"),
        json!({"studentId": bina.student_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(wl["position"], 1);
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/waitlist/promote"),
            json!({}),
            None
        )
        .await,
        StatusCode::CONFLICT,
        "no seat yet"
    );
    let dropped = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/enrolments/{}/drop", id(&over)),
        json!({}),
        Some(ver(&over)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(dropped["status"], "dropped");
    let offered = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/waitlist/promote"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(offered["status"], "offered");
    assert_eq!(offered["offeredSectionId"].as_str(), Some(section.as_str()));
    assert!(outbox_types(&pool, &id(&offered))
        .await
        .contains(&ev("offering_waitlist.seat_offered")));
    // The offered seat is held: nobody else takes it.
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &path,
            json!({"studentId": chirag.student_id, "overrideEligibility": true, "reason": "again"}),
            None
        )
        .await,
        StatusCode::CONFLICT
    );
    let taken = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/waitlist/{}/accept", id(&offered)),
        json!({}),
        Some(ver(&offered)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(taken["origin"], "waitlist");
    let wlist = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{oid}/waitlist"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(wlist["items"][0]["status"], "accepted");
    let cur = reload(&app, &admin, &off).await;
    let off = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/capacity"),
        json!({"sectionId": section, "capacity": 3, "reason": "bigger room"}),
        Some(ver(&cur)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(off["sections"][0]["capacity"], 3);
    let hist = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{oid}/capacity"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(hist["items"][0]["fromCapacity"], 2);
    assert_eq!(hist["items"][0]["toCapacity"], 3);
    assert!(outbox_types(&pool, &oid)
        .await
        .contains(&ev("course_offering.capacity_changed")));

    // Roster read (academic.read).
    let roster = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/sections/{section}/enrolments?status=enrolled"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(roster["items"].as_array().unwrap().len(), 2);

    // ---- close enrolment, then drop is refused (withdraw instead); activation with readiness
    let off = step(&app, &admin, &off, "close-enrolment", json!({})).await;
    assert_eq!(off["status"], "enrollment_closed");
    assert_eq!(
        status_of(
            &app,
            &student,
            "POST",
            &format!("{A}/me/enrolments/{}/drop", id(&seat)),
            json!({}),
            Some(ver(&seat))
        )
        .await,
        StatusCode::CONFLICT
    );
    let readiness = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{oid}/readiness"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(readiness["ready"], true, "{readiness}");
    let off = step(&app, &admin, &off, "activate", json!({})).await;
    assert_eq!(off["status"], "active");

    // ---- My classes / My students (faculty-scoped)
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let classes = ok(
        &app,
        &meera,
        "GET",
        &format!("{A}/my/classes"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        classes["items"][0]["sectionId"].as_str(),
        Some(section.as_str())
    );
    assert_eq!(classes["items"][0]["seatedCount"], 2);
    assert_eq!(classes["items"][0]["role"], "primary_instructor");
    let my_students = ok(
        &app,
        &meera,
        "GET",
        &format!("{A}/my/students"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(my_students["items"].as_array().unwrap().len(), 2);
    assert_eq!(status_of(&app, &meera, "POST", &format!("{A}/institutions/{inst}/offerings"),
        json!({"academicTermId": s.term_id, "curriculumVersionId": s.base.curriculum_version_id, "curriculumComponentId": cs, "offeringCode": "NO"}), None).await, StatusCode::FORBIDDEN);
    assert_eq!(
        status_of(
            &app,
            &student,
            "GET",
            &format!("{A}/my/classes"),
            json!({}),
            None
        )
        .await,
        StatusCode::FORBIDDEN
    );

    // ---- D-002 / D-003 suspension → reactivation
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/suspend"),
            json!({}),
            Some(ver(&off))
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let off = step(
        &app,
        &admin,
        &off,
        "suspend",
        json!({"reason": "Instructor on leave"}),
    )
    .await;
    assert_eq!(off["status"], "suspended");
    assert_eq!(off["seatsTaken"], 2, "seats untouched by suspension");
    let off = step(
        &app,
        &admin,
        &off,
        "reactivate",
        json!({"reason": "Instructor back"}),
    )
    .await;
    assert_eq!(off["status"], "active");

    // ---- D-013 freeze → governed unfreeze (auto-approved)
    let off = step(&app, &admin, &off, "freeze", json!({"reason": "Audit"})).await;
    assert_eq!(off["status"], "frozen");
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/capacity"),
            json!({"capacity": 10, "reason": "x"}),
            Some(ver(&off))
        )
        .await,
        StatusCode::CONFLICT,
        "frozen offerings refuse changes"
    );
    let out = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/unfreeze"),
        json!({"reason": "Audit closed"}),
        Some(ver(&off)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(out["offering"]["status"], "active", "{out}");
    assert_eq!(out["request"]["status"], "applied");

    // ---- complete → archive (D-010) and immutability
    let off = out["offering"].clone();
    let off = step(&app, &admin, &off, "complete", json!({})).await;
    assert_eq!(off["status"], "completed");
    assert_eq!(off["seatsTaken"], 0, "seats completed");
    let fa = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/faculty-assignments/{}", id(&fa)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(fa["status"], "completed");
    let off = step(&app, &admin, &off, "archive", json!({})).await;
    assert_eq!(off["status"], "archived");
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/sections"),
            json!({"sectionCode": "B"}),
            None
        )
        .await,
        StatusCode::CONFLICT
    );

    // ---- evidence: events, audit, status history
    let types = outbox_types(&pool, &oid).await;
    for e in [
        "course_offering.submitted",
        "course_offering.approved",
        "course_offering.published",
        "course_offering.enrollment_opened",
        "course_offering.enrollment_closed",
        "course_offering.activated",
        "course_offering.suspended",
        "course_offering.reactivated",
        "course_offering.frozen",
        "course_offering.unfrozen",
        "course_offering.completed",
        "course_offering.archived",
    ] {
        assert!(types.contains(&ev(e)), "missing {e} in {types:?}");
    }
    let actions = audit_actions(&pool, &oid).await;
    assert!(
        actions.contains(&"academic.course_offering.create".to_owned()),
        "{actions:?}"
    );
    assert!(
        actions.contains(&"academic.course_offering.approve".to_owned()),
        "{actions:?}"
    );
    let h = history(&pool, &oid).await;
    assert_eq!(
        &h[..3],
        &["pending_approval", "approved", "published"],
        "{h:?}"
    );
    assert!(outbox_types(&pool, &id(&seat))
        .await
        .contains(&ev("section_enrolment.enrolled")));
    assert!(outbox_types(&pool, &id(&fa))
        .await
        .contains(&ev("faculty_assignment.activated")));
    let auto: i64 = sqlx::query_scalar("SELECT count(*) FROM platform.audit_log WHERE target_id = $1 AND actor_id = 'system:auto-approval'")
        .bind(&oid).fetch_one(&pool).await.unwrap();
    assert!(auto >= 1, "auto-approval recorded as the system actor");

    // ---- tenant isolation
    let other = custom_app(
        state(&pool, ApprovalMode::AutoApprove),
        tenant(),
        "dev|admin@sutra.dev",
        Role::Administrator,
    );
    assert_eq!(
        status_of(
            &other,
            &injected(),
            "GET",
            &format!("{A}/offerings/{oid}"),
            json!({}),
            None
        )
        .await,
        StatusCode::NOT_FOUND
    );
    let list = ok(
        &other,
        &injected(),
        "GET",
        &format!("{A}/institutions/{inst}/offerings"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(list["items"].as_array().unwrap().is_empty());
}

/// D-001 with `ApprovalMode::AwaitDecision` (reject → draft, approve), D-004 governed
/// cancellation awaiting a decision, D-009 revision → rollback → replacement, D-017 assignment
/// awaiting review, plus auto cancellation of a draft.
#[tokio::test]
async fn approval_await_decision_cancellation_and_revisions() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let s = setup(&app, &admin, &pool, t, "Governance University").await;
    let waiting = custom_app(
        state(&pool, ApprovalMode::AwaitDecision),
        t,
        "dev|admin@sutra.dev",
        Role::Administrator,
    );
    // C1/C2 CI follow-up: submissions are recorded as the injected administrator's, so an
    // independent administrator decides them (separation of duties forbids self-decision).
    let checker = custom_app(
        state(&pool, ApprovalMode::AwaitDecision),
        t,
        "dev|checker@sutra.dev",
        Role::Administrator,
    );
    let who = injected();

    // ---- AwaitDecision: pending_approval, reject (reason required) → draft, approve → approved
    let off = new_offering(
        &waiting,
        &who,
        &s,
        "CS101",
        "AW-1",
        json!([{"sectionCode": "A", "capacity": 30}]),
    )
    .await;
    let off = step(&waiting, &who, &off, "submit", json!({})).await;
    assert_eq!(off["status"], "pending_approval");
    let pid = off["processInstanceId"].as_str().unwrap().to_owned();
    assert_eq!(
        status_of(
            &checker,
            &who,
            "POST",
            &format!("{A}/offerings/{}/decision", id(&off)),
            json!({"decision": "reject"}),
            Some(ver(&off))
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let off = ok(
        &checker,
        &who,
        "POST",
        &format!("{A}/offerings/{}/decision", id(&off)),
        json!({"decision": "reject", "reason": "Missing syllabus"}),
        Some(ver(&off)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(off["status"], "draft");
    assert_eq!(off["statusReason"], "Missing syllabus");
    let off = step(&waiting, &who, &off, "submit", json!({})).await;
    let off = ok(
        &checker,
        &who,
        "POST",
        &format!("{A}/offerings/{}/decision", id(&off)),
        json!({"decision": "approve"}),
        Some(ver(&off)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(off["status"], "approved");
    assert_ne!(
        off["processInstanceId"].as_str(),
        Some(pid.as_str()),
        "new process per submission"
    );
    let actions = audit_actions(&pool, &id(&off)).await;
    assert!(
        actions.contains(&"academic.course_offering.reject".to_owned())
            && actions.contains(&"academic.course_offering.approve".to_owned()),
        "{actions:?}"
    );
    // The pending submission tells the inbox rule a human decision is awaited.
    let sub: Value = sqlx::query_scalar("SELECT envelope->'data' FROM platform.outbox WHERE subject = $1 AND event_type = $2 ORDER BY id LIMIT 1")
        .bind(id(&off)).bind(ev("course_offering.submitted")).fetch_one(&pool).await.unwrap();
    assert_eq!(sub["reason"], "awaiting decision");

    // ---- D-017 assignment under review → decision
    let section = off["sections"][0]["id"].as_str().unwrap().to_owned();
    let fa = ok(
        &waiting,
        &who,
        "POST",
        &format!("{A}/sections/{section}/faculty-assignments"),
        json!({"facultySubjectRef": b4::MEERA, "role": "primary_instructor"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let fa = ok(
        &waiting,
        &who,
        "POST",
        &format!("{A}/faculty-assignments/{}/submit", id(&fa)),
        json!({}),
        Some(ver(&fa)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(fa["status"], "under_review");
    let fa = ok(
        &checker,
        &who,
        "POST",
        &format!("{A}/faculty-assignments/{}/decision", id(&fa)),
        json!({"decision": "approve"}),
        Some(ver(&fa)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(fa["status"], "approved");

    // ---- D-004 cancellation awaiting a decision; the offering is untouched until approved
    let out = ok(
        &waiting,
        &who,
        "POST",
        &format!("{A}/offerings/{}/cancel", id(&off)),
        json!({"reason": "Low demand"}),
        Some(ver(&off)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(out["offering"]["status"], "approved");
    assert_eq!(out["request"]["status"], "requested");
    assert_eq!(out["request"]["kind"], "cancellation");
    let req = out["request"].clone();
    assert_eq!(
        status_of(
            &waiting,
            &who,
            "POST",
            &format!("{A}/offerings/{}/cancel", id(&off)),
            json!({"reason": "again"}),
            Some(ver(&out["offering"]))
        )
        .await,
        StatusCode::CONFLICT,
        "one open request per kind"
    );
    let req = ok(
        &checker,
        &who,
        "POST",
        &format!("{A}/offerings/{}/revisions/{}/decision", id(&off), id(&req)),
        json!({"decision": "approve"}),
        Some(ver(&req)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(req["status"], "applied");
    let off = reload(&app, &admin, &off).await;
    assert_eq!(off["status"], "cancelled");
    assert_eq!(off["sections"][0]["status"], "cancelled");
    let fa = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/faculty-assignments/{}", id(&fa)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        fa["status"], "withdrawn",
        "live assignments end with the offering"
    );
    assert!(outbox_types(&pool, &id(&off))
        .await
        .contains(&ev("course_offering.cancelled")));
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{}/open-enrolment", id(&off)),
            json!({}),
            Some(ver(&off))
        )
        .await,
        StatusCode::CONFLICT
    );
    let off = step(&app, &admin, &off, "archive", json!({})).await;
    assert_eq!(off["status"], "archived");

    // Auto mode: a draft is cancelled at once.
    let d = new_offering(&app, &admin, &s, "MD101", "AUTO-C", json!([])).await;
    let out = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{}/cancel", id(&d)),
        json!({"reason": "Duplicate"}),
        Some(ver(&d)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(out["offering"]["status"], "cancelled");
    // Active offerings are outside the administrative cancellation path.
    let act = to_active(
        &app,
        &admin,
        &new_offering(
            &app,
            &admin,
            &s,
            "MD101",
            "ACT-1",
            json!([{"sectionCode": "A", "capacity": 5}]),
        )
        .await,
    )
    .await;
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{}/cancel", id(&act)),
            json!({"reason": "x"}),
            Some(ver(&act))
        )
        .await,
        StatusCode::CONFLICT
    );

    // ---- D-009 revision (auto), second revision, rollback to the first, replacement
    let r = ok(&app, &admin, "POST", &format!("{A}/offerings/{}/revisions", id(&act)),
        json!({"kind": "revision", "proposed": {"title": "Medicine Basics", "deliveryMode": "hybrid"}, "reason": "Rename"}), None, StatusCode::CREATED).await;
    assert_eq!(r["status"], "applied", "{r}");
    let act = reload(&app, &admin, &act).await;
    assert_eq!(act["title"], "Medicine Basics");
    assert_eq!(act["deliveryMode"], "hybrid");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{}/revisions", id(&act)),
        json!({"kind": "revision", "proposed": {"title": "Medicine II"}, "reason": "Rename again"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{}/revisions", id(&act)),
            json!({"kind": "rollback", "targetRevisionId": uuid::Uuid::now_v7(), "reason": "bad"}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let rb = ok(&app, &admin, "POST", &format!("{A}/offerings/{}/revisions", id(&act)),
        json!({"kind": "rollback", "targetRevisionId": r["id"], "reason": "Back to the approved title"}), None, StatusCode::CREATED).await;
    assert_eq!(rb["status"], "applied");
    let act = reload(&app, &admin, &act).await;
    assert_eq!(act["title"], "Medicine Basics");
    let revs = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{}/revisions", id(&act)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        revs["items"].as_array().unwrap().len(),
        3,
        "history preserved"
    );
    assert!(outbox_types(&pool, &id(&act))
        .await
        .contains(&ev("course_offering.revised")));
    // Revision awaiting a decision, then rejected: nothing changes.
    let pending = ok(
        &waiting,
        &who,
        "POST",
        &format!("{A}/offerings/{}/revisions", id(&act)),
        json!({"kind": "revision", "proposed": {"capacity": 99}, "reason": "Expand"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(pending["status"], "requested");
    let rej = ok(
        &checker,
        &who,
        "POST",
        &format!(
            "{A}/offerings/{}/revisions/{}/decision",
            id(&act),
            id(&pending)
        ),
        json!({"decision": "reject", "reason": "Not now"}),
        Some(ver(&pending)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rej["status"], "rejected");
    assert_eq!(reload(&app, &admin, &act).await["capacity"], Value::Null);
    // Replacement before enrolment.
    let rep_off = new_offering(
        &app,
        &admin,
        &s,
        "CS101",
        "REP-1",
        json!([{"sectionCode": "A", "capacity": 5}]),
    )
    .await;
    let rep = ok(&app, &admin, "POST", &format!("{A}/offerings/{}/revisions", id(&rep_off)),
        json!({"kind": "replacement", "proposed": {"curriculumComponentId": s.component("MD101").id}, "reason": "Wrong course"}), None, StatusCode::CREATED).await;
    assert_eq!(rep["status"], "applied", "{rep}");
    let rep_off = reload(&app, &admin, &rep_off).await;
    assert_eq!(rep_off["credits"], 8.0);
    assert_eq!(
        rep_off["curriculumComponentId"].as_str().unwrap(),
        s.component("MD101").id.to_string()
    );
}

/// D-006 restrictions, D-008 cross-campus / cross-institution permits, D-012 template
/// generation (partial + atomic), D-016 simulation → decision, D-011 merge and split.
#[tokio::test]
async fn restrictions_templates_simulation_merge_split() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let s = setup(&app, &admin, &pool, t, "Structure University").await;
    let inst = s.base.institution_id;
    let cs = s.component("CS101").id;

    // ---- D-006 restriction references must be authoritative
    let off = to_open(
        &app,
        &admin,
        &new_offering(
            &app,
            &admin,
            &s,
            "CS101",
            "R-1",
            json!([{"sectionCode": "A", "capacity": 10}, {"sectionCode": "B", "capacity": 10}]),
        )
        .await,
    )
    .await;
    let oid = id(&off);
    let rpath = format!("{A}/offerings/{oid}/restrictions");
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &rpath,
            json!({"kind": "programme", "refIds": [uuid::Uuid::now_v7()], "reason": "x"}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    ok(&app, &admin, "POST", &rpath, json!({"kind": "programme", "refIds": [s.base.programme_id], "reason": "Programme students only"}), None, StatusCode::CREATED).await;
    let min = ok(
        &app,
        &admin,
        "POST",
        &rpath,
        json!({"kind": "min_term", "minTermNo": 3, "reason": "Senior course"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &rpath,
            json!({"kind": "cross_institution", "refIds": [uuid::Uuid::now_v7()], "reason": "x"}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "both approvals required"
    );
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &rpath,
            json!({"kind": "cross_campus", "refIds": [uuid::Uuid::now_v7()], "reason": "x"}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let st1 = b4::seed_student(&pool, &s, "Dev", None).await;
    b4::seed_registration(&pool, &s, &st1, &[cs], "registered").await;
    let section_a = off["sections"][0]["id"].as_str().unwrap().to_owned();
    let section_b = off["sections"][1]["id"].as_str().unwrap().to_owned();
    let blocked = call(
        &app,
        &admin,
        "POST",
        &format!("{A}/sections/{section_a}/enrolments"),
        json!({"studentId": st1.student_id}),
        None,
    )
    .await;
    assert_eq!(
        blocked.0,
        StatusCode::CONFLICT,
        "term-1 student vs min term 3: {}",
        blocked.1
    );
    ok(
        &app,
        &admin,
        "POST",
        &format!("{rpath}/{}/retire", id(&min)),
        json!({}),
        Some(ver(&min)),
        StatusCode::OK,
    )
    .await;
    let seat1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/sections/{section_a}/enrolments"),
        json!({"studentId": st1.student_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let list = ok(
        &app,
        &admin,
        "GET",
        &format!("{rpath}?status=effective"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    // Transfer between sections of the offering.
    let moved = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/enrolments/{}/transfer", id(&seat1)),
        json!({"toSectionId": section_b, "reason": "Timetable clash"}),
        Some(ver(&seat1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(moved["sectionId"].as_str(), Some(section_b.as_str()));
    assert_eq!(moved["transferredFromId"], seat1["id"]);

    // ---- D-012 templates: approval gate, atomic run, partial generation with a duplicate
    let tpl = ok(&app, &admin, "POST", &format!("{A}/institutions/{inst}/offering-templates"),
        json!({"name": "Standard theory", "sectionCodes": ["A", "B"], "sectionCapacity": 40, "capacity": 80, "codeSuffix": "GEN"}), None, StatusCode::CREATED).await;
    let gen = json!({"academicTermId": s.term_id, "curriculumVersionId": s.base.curriculum_version_id, "termNo": 1});
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offering-templates/{}/generate", id(&tpl)),
            gen.clone(),
            None
        )
        .await,
        StatusCode::CONFLICT,
        "draft template"
    );
    let tpl = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offering-templates/{}/activate", id(&tpl)),
        json!({}),
        Some(ver(&tpl)),
        StatusCode::OK,
    )
    .await;
    let mut atomic = gen.clone();
    atomic["atomic"] = json!(true);
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offering-templates/{}/generate", id(&tpl)),
            atomic,
            None
        )
        .await,
        StatusCode::CONFLICT,
        "CS101 already live → atomic run fails"
    );
    let res = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offering-templates/{}/generate", id(&tpl)),
        gen,
        None,
        StatusCode::CREATED,
    )
    .await;
    let items = res["items"].as_array().unwrap();
    assert!(items.len() >= 2, "{res}");
    let failed = items
        .iter()
        .find(|i| i["curriculumComponentId"].as_str().unwrap() == cs.to_string())
        .unwrap();
    assert_eq!(failed["outcome"], "failed");
    let made = items.iter().find(|i| i["outcome"] == "generated").unwrap();
    let generated = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{}", made["offeringId"].as_str().unwrap()),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        generated["status"], "draft",
        "generated offerings are never auto-published"
    );
    assert_eq!(generated["sections"].as_array().unwrap().len(), 2);
    let prov: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.offering_template_generation WHERE template_id = $1::uuid AND run_id = $2::uuid")
        .bind(id(&tpl)).bind(res["runId"].as_str().unwrap()).fetch_one(&pool).await.unwrap();
    assert_eq!(prov as usize, items.len(), "provenance per component");

    // ---- D-016 simulation: read-only report + decision that does not execute
    let before = reload(&app, &admin, &off).await;
    let sim = ok(&app, &admin, "POST", &format!("{A}/offerings/{oid}/simulations"), json!({"changeKind": "capacity", "proposedCapacity": 0, "criteria": "No student loses a seat"}), None, StatusCode::CREATED).await;
    assert_eq!(sim["report"]["feasible"], false);
    assert_eq!(sim["report"]["seated"], 1);
    assert_eq!(sim["report"]["studentsAffected"], 1);
    let dec = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{oid}/simulations/{}/decision", id(&sim)),
        json!({"decision": "reject", "reason": "Would unseat a student"}),
        Some(ver(&sim)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(dec["decision"], "reject");
    assert_eq!(dec["status"], "rejected");
    let after = reload(&app, &admin, &off).await;
    assert_eq!(
        after["version"], before["version"],
        "simulation changed nothing"
    );
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{oid}/simulations/{}/decision", id(&sim)),
            json!({"decision": "proceed"}),
            Some(ver(&dec))
        )
        .await,
        StatusCode::CONFLICT
    );

    // ---- D-011 merge: two active offerings of MD101; seats + faculty move
    let md = s.component("MD101").id;
    let m1 = to_active(
        &app,
        &admin,
        &new_offering(
            &app,
            &admin,
            &s,
            "MD101",
            "M-1",
            json!([{"sectionCode": "A", "capacity": 10}]),
        )
        .await,
    )
    .await;
    let m2 = to_open(
        &app,
        &admin,
        &new_offering(
            &app,
            &admin,
            &s,
            "MD101",
            "M-2",
            json!([{"sectionCode": "A", "capacity": 10}]),
        )
        .await,
    )
    .await;
    let st2 = b4::seed_student(&pool, &s, "Esha", None).await;
    b4::seed_registration(&pool, &s, &st2, &[md], "registered").await;
    let m2a = m2["sections"][0]["id"].as_str().unwrap().to_owned();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/sections/{m2a}/enrolments"),
        json!({"studentId": st2.student_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let m2 = step(
        &app,
        &admin,
        &reload(&app, &admin, &m2).await,
        "close-enrolment",
        json!({}),
    )
    .await;
    let m2 = step(&app, &admin, &m2, "activate", json!({})).await;
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{}/merge", id(&m1)),
            json!({"sourceOfferingIds": [oid], "reason": "x"}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
        "different course"
    );
    let mg = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/offerings/{}/merge", id(&m1)),
        json!({"sourceOfferingIds": [id(&m2)], "reason": "Low numbers"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(mg["status"], "applied", "{mg}");
    let m1 = reload(&app, &admin, &m1).await;
    let m2r = reload(&app, &admin, &m2).await;
    assert_eq!(m2r["status"], "merged");
    assert_eq!(m2r["mergedIntoId"], m1["id"]);
    assert_eq!(m2r["sections"][0]["status"], "merged");
    assert_eq!(m1["sections"].as_array().unwrap().len(), 2);
    assert_eq!(m1["seatsTaken"], 1);
    assert_eq!(m1["capacity"], Value::Null);
    let moved_faculty = m1["sections"].as_array().unwrap().iter().all(|s| {
        s["faculty"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["status"] == "active")
    });
    assert!(moved_faculty, "faculty follow the merged section: {m1}");
    assert!(outbox_types(&pool, &id(&m1))
        .await
        .contains(&ev("course_offering.reorganised")));

    // ---- D-011 split: the merged offering into two successors
    let secs: Vec<String> = m1["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/offerings/{}/split", id(&m1)),
            json!({"parts": [{"offeringCode": "S-1", "sectionIds": [secs[0]]}], "reason": "x"}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let sp = ok(&app, &admin, "POST", &format!("{A}/offerings/{}/split", id(&m1)),
        json!({"parts": [{"offeringCode": "S-1", "sectionIds": [secs[0]]}, {"offeringCode": "S-2", "title": "Medicine (evening)", "sectionIds": [secs[1]]}], "reason": "Two cohorts"}),
        None, StatusCode::CREATED).await;
    assert_eq!(sp["status"], "applied", "{sp}");
    let children = sp["resultOfferingIds"].as_array().unwrap();
    assert_eq!(children.len(), 2);
    let c2 = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{}", children[1].as_str().unwrap()),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(c2["status"], "active");
    assert_eq!(c2["splitFromId"], m1["id"]);
    assert_eq!(c2["title"], "Medicine (evening)");
    assert_eq!(reload(&app, &admin, &m1).await["status"], "split");
    let reorgs = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/offerings/{}/reorganisations", id(&m1)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(reorgs["items"].as_array().unwrap().len(), 2);
}

/// D-017 establishment with policy checks, D-018 operational change (auto + AwaitDecision
/// rejection, reassignment), D-019 governance configuration (review gate, supersession).
#[tokio::test]
async fn faculty_assignment_changes_and_policy() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let s = setup(&app, &admin, &pool, t, "Faculty University").await;
    let inst = s.base.institution_id;
    let ppath = format!("{A}/institutions/{inst}/faculty-assignment-policies");

    // ---- D-019 policy version: draft → in_review → review gate → effective → superseded
    assert_eq!(status_of(&app, &admin, "POST", &ppath, json!({"maxWeeklyHours": 6, "allowedRoles": ["tutor"], "requirePrimaryInstructor": true, "rationale": "x"}), None).await,
        StatusCode::UNPROCESSABLE_ENTITY, "incompatible configuration");
    let p1 = ok(&app, &admin, "POST", &ppath, json!({"name": "Workload v1", "maxWeeklyHours": 6, "allowedRoles": ["primary_instructor", "tutor"], "rationale": "Workload norms"}), None, StatusCode::CREATED).await;
    assert_eq!(p1["status"], "draft");
    assert_eq!(p1["versionNo"], 1);
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/faculty-assignment-policies/{}/review", id(&p1)),
            json!({"outcome": "compatible"}),
            Some(ver(&p1))
        )
        .await,
        StatusCode::CONFLICT,
        "review needs in_review"
    );
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignment-policies/{}/submit", id(&p1)),
        json!({}),
        Some(ver(&p1)),
        StatusCode::OK,
    )
    .await;
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignment-policies/{}/review", id(&p1)),
        json!({"outcome": "incompatible", "notes": "Conflicts with leave rules"}),
        Some(ver(&p1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "draft");
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignment-policies/{}/submit", id(&p1)),
        json!({}),
        Some(ver(&p1)),
        StatusCode::OK,
    )
    .await;
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignment-policies/{}/review", id(&p1)),
        json!({"outcome": "compatible"}),
        Some(ver(&p1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        p1["status"], "effective",
        "auto-approved after a compatible review: {p1}"
    );
    assert!(outbox_types(&pool, &id(&p1))
        .await
        .contains(&ev("faculty_assignment_policy.effective")));

    // ---- D-017 under the policy: disallowed role, workload ceiling
    let off = new_offering(
        &app,
        &admin,
        &s,
        "CS101",
        "F-1",
        json!([{"sectionCode": "A", "capacity": 10}, {"sectionCode": "B", "capacity": 10}]),
    )
    .await;
    let off = step(&app, &admin, &off, "submit", json!({})).await;
    let off = step(&app, &admin, &off, "publish", json!({})).await;
    let a_sec = off["sections"][0]["id"].as_str().unwrap().to_owned();
    let b_sec = off["sections"][1]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/sections/{a_sec}/faculty-assignments"),
            json!({"facultySubjectRef": b4::MEERA, "role": "lab_instructor"}),
            None
        )
        .await,
        StatusCode::CONFLICT,
        "role not allowed"
    );
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/sections/{a_sec}/faculty-assignments"),
            json!({"facultySubjectRef": " ", "role": "tutor"}),
            None
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let a1 = assign(
        &app,
        &admin,
        &a_sec,
        b4::MEERA,
        "primary_instructor",
        Some(4.0),
    )
    .await;
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &format!("{A}/sections/{a_sec}/faculty-assignments"),
            json!({"facultySubjectRef": b4::MEERA, "role": "primary_instructor"}),
            None
        )
        .await,
        StatusCode::CONFLICT,
        "one live assignment per role"
    );
    let a2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/sections/{b_sec}/faculty-assignments"),
        json!({"facultySubjectRef": b4::MEERA, "role": "primary_instructor", "weeklyHours": 4}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let over = call(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignments/{}/submit", id(&a2)),
        json!({}),
        Some(ver(&a2)),
    )
    .await;
    assert_eq!(
        over.0,
        StatusCode::CONFLICT,
        "8 h > 6 h ceiling: {}",
        over.1
    );
    let a2 = ok(
        &app,
        &admin,
        "PATCH",
        &format!("{A}/faculty-assignments/{}", id(&a2)),
        json!({"weeklyHours": 2}),
        Some(ver(&a2)),
        StatusCode::OK,
    )
    .await;
    let a2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignments/{}/submit", id(&a2)),
        json!({}),
        Some(ver(&a2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a2["status"], "approved");
    let meera = login(&app, "meera.shah@sutra.dev").await;
    assert_eq!(
        status_of(
            &app,
            &meera,
            "POST",
            &format!("{A}/sections/{a_sec}/faculty-assignments"),
            json!({"facultySubjectRef": "dev|x"}),
            None
        )
        .await,
        StatusCode::FORBIDDEN
    );
    let classes = ok(
        &app,
        &meera,
        "GET",
        &format!("{A}/my/classes"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        classes["items"].as_array().unwrap().len(),
        1,
        "approved (not active) does not grant teaching"
    );

    // ---- D-018 operational changes (auto): hours over the ceiling refused, hours, reassignment
    let cpath = format!("{A}/faculty-assignments/{}/changes", id(&a1));
    assert_eq!(
        status_of(
            &app,
            &admin,
            "POST",
            &cpath,
            json!({"changeKind": "hours", "weeklyHours": 5, "reason": "More labs"}),
            None
        )
        .await,
        StatusCode::CONFLICT
    );
    let ch = ok(
        &app,
        &admin,
        "POST",
        &cpath,
        json!({"changeKind": "hours", "weeklyHours": 3, "reason": "Fewer labs"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(ch["status"], "applied");
    let a1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/faculty-assignments/{}", id(&a1)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["weeklyHours"], 3.0);
    assert!(outbox_types(&pool, &id(&a1))
        .await
        .contains(&ev("faculty_assignment.changed")));

    // AwaitDecision: validated, then rejected → assignment unchanged.
    let waiting = custom_app(
        state(&pool, ApprovalMode::AwaitDecision),
        t,
        "dev|admin@sutra.dev",
        Role::Administrator,
    );
    // C1/C2 CI follow-up: submissions are recorded as the injected administrator's, so an
    // independent administrator decides them (separation of duties forbids self-decision).
    let checker = custom_app(
        state(&pool, ApprovalMode::AwaitDecision),
        t,
        "dev|checker@sutra.dev",
        Role::Administrator,
    );
    let pending = ok(
        &waiting,
        &injected(),
        "POST",
        &cpath,
        json!({"changeKind": "role", "role": "tutor", "reason": "Swap roles"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(pending["status"], "validated");
    let rej = ok(
        &checker,
        &injected(),
        "POST",
        &format!("{cpath}/{}/decision", id(&pending)),
        json!({"decision": "reject", "reason": "Keep primary"}),
        Some(ver(&pending)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rej["status"], "rejected");
    let a1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/faculty-assignments/{}", id(&a1)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["role"], "primary_instructor");

    // Reassignment: the original is revoked, a new active assignment replaces it.
    let re = ok(&app, &admin, "POST", &cpath, json!({"changeKind": "reassignment", "facultySubjectRef": "dev|ravi.kumar@sutra.dev", "facultyName": "Dr. Ravi Kumar", "reason": "Sabbatical"}), None, StatusCode::CREATED).await;
    assert_eq!(re["status"], "applied", "{re}");
    let a1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/faculty-assignments/{}", id(&a1)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["status"], "revoked");
    let newa = ok(
        &app,
        &admin,
        "GET",
        &format!(
            "{A}/faculty-assignments/{}",
            re["replacementAssignmentId"].as_str().unwrap()
        ),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(newa["status"], "active");
    assert_eq!(newa["facultySubjectRef"], "dev|ravi.kumar@sutra.dev");
    assert!(outbox_types(&pool, &id(&a1))
        .await
        .contains(&ev("faculty_assignment.ended")));
    let changes = ok(&app, &admin, "GET", &cpath, json!({}), None, StatusCode::OK).await;
    assert_eq!(changes["items"].as_array().unwrap().len(), 3);
    let h = history(&pool, &id(&a1)).await;
    assert_eq!(h, vec!["submitted", "approved", "active", "revoked"]);

    // ---- D-019 second version supersedes the first (kept)
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &ppath,
        json!({"name": "Workload v2", "maxWeeklyHours": 10, "rationale": "New norms"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(p2["versionNo"], 2);
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignment-policies/{}/submit", id(&p2)),
        json!({}),
        Some(ver(&p2)),
        StatusCode::OK,
    )
    .await;
    let p2 = ok(
        &waiting,
        &injected(),
        "POST",
        &format!("{A}/faculty-assignment-policies/{}/review", id(&p2)),
        json!({"outcome": "compatible"}),
        Some(ver(&p2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        p2["status"], "in_review",
        "awaiting the governance decision"
    );
    let p2 = ok(
        &checker,
        &injected(),
        "POST",
        &format!("{A}/faculty-assignment-policies/{}/decision", id(&p2)),
        json!({"decision": "approve"}),
        Some(ver(&p2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p2["status"], "effective");
    let p1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{A}/faculty-assignment-policies/{}", id(&p1)),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "superseded");
    assert_eq!(p1["supersededBy"], p2["id"]);
    // Now 2 + 4 hours fit: the second assignment activates.
    let a2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{A}/faculty-assignments/{}/activate", id(&a2)),
        json!({}),
        Some(ver(&a2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a2["status"], "active");
}
