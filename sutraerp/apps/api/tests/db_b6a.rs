//! Batch 6 slice 6A (exam setup) DB tests: policy versions, sessions + papers, schedules /
//! timetable (conflicts, approval, publication, withdrawal, revision), venues + capacity,
//! seating, invigilation (shortage, duplicates), registrations (self-service, bulk,
//! administrative, rejection, duplicates, amendment / correction, scope sync, arrangements,
//! deadline override, late registration + Finance fee, reopening, population finalisation,
//! withdrawal / reinstatement), hall tickets (dues → withheld → paid → released, reissue,
//! invalidation, finalisation), conduct (attendance → appeared / absent, suspension /
//! resumption, postponement), exception cases, `/me` views, and the `AwaitDecision` session
//! approval with its inbox projection.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{Duration, NaiveDate};
use serde_json::{json, Value};
use std::sync::Arc;
use support::b6;
use support::*;
use sutra_exams::shared::approval::ApprovalMode;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b6a00";
const X: &str = "/api/v1/exams";

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, v) = send(app, write(who, method, path, body, if_match)).await;
    (s, v)
}

async fn read(app: &Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, v) = send(app, get(who, path)).await;
    (s, v)
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

async fn outbox_count(pool: &sqlx::PgPool, subject: &str, suffix: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE subject = $1 AND event_type = $2",
    )
    .bind(subject)
    .bind(format!("sutraerp.exams.{suffix}"))
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn business_today(pool: &sqlx::PgPool, institution: Uuid) -> NaiveDate {
    sqlx::query_scalar(
        "SELECT (now() AT TIME ZONE timezone)::date FROM institution.institution WHERE id = $1",
    )
    .bind(institution)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// L3-I-001 … I-006, I-007 … I-016, I-017 … I-027, I-029, I-030 (attempt rules), I-032, I-034,
/// I-036 … I-043, I-055, I-057, I-059, I-060, I-172, I-174, I-175, I-185 end to end.
#[tokio::test]
async fn exam_setup_end_to_end() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Exam Setup University",
        Some(b4::AARAV),
        "draft",
        "draft",
    )
    .await;
    let inst = setup.institution().as_uuid();
    let delivery = &setup.finance.delivery;
    let today = business_today(&pool, inst).await;
    let d = |days: i64| (today + Duration::days(days)).to_string();

    // ================================================================ policy (I-001 … I-003)
    let policies = format!("{X}/institutions/{inst}/exam-policies");
    let (s, v) = call(
        &app,
        &admin,
        "POST",
        &policies,
        json!({"code": "bad code", "name": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (s, v) = call(
        &app,
        &admin,
        "POST",
        &policies,
        json!({"code": "EXAM-POL", "name": "Examination policy", "lateFeeAmount": "500"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "late fee needs a code: {v}"
    );
    let (s, p1) = call(&app, &admin, "POST", &policies, json!({"code": "EXAM-POL", "name": "Examination policy", "requireDuesCleared": true, "minAttendancePercent": "75", "lateFeeCode": "EXAM-LATE", "lateFeeAmount": "500"}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{p1}");
    assert_eq!(
        (
            p1["status"].as_str(),
            p1["versionNo"].as_i64(),
            p1["minAttendancePercent"].as_str()
        ),
        (Some("draft"), Some(1), Some("75.00"))
    );
    let p1_id = id(&p1);
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &policies,
        json!({"code": "STU", "name": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, p1) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{X}/exam-policies/{p1_id}"),
        json!({"maxAttempts": 4}),
        Some(1),
    )
    .await;
    assert_eq!(
        (s, p1["maxAttempts"].as_i64()),
        (StatusCode::OK, Some(4)),
        "{p1}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p1_id}/activate"),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a draft cannot be activated");
    let (s, p1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p1_id}/submit"),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(
        (s, p1["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{p1}"
    );
    let (s, p1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p1_id}/activate"),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(
        (s, p1["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{p1}"
    );
    assert_eq!(
        outbox_count(&pool, &p1_id, "exam_policy.activated").await,
        1
    );
    // I-002: the next version inherits the rules and supersedes v1 on activation.
    let (s, p2) = call(
        &app,
        &admin,
        "POST",
        &policies,
        json!({"code": "EXAM-POL", "invigilatorRatio": 30}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{p2}");
    assert_eq!(
        (
            p2["versionNo"].as_i64(),
            p2["maxAttempts"].as_i64(),
            p2["supersedesId"].as_str()
        ),
        (Some(2), Some(4), Some(p1_id.as_str()))
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &policies,
        json!({"code": "EXAM-POL"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open version per code");
    let p2_id = id(&p2);
    let (_, p2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p2_id}/submit"),
        json!({}),
        Some(ver(&p2)),
    )
    .await;
    let (s, p2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p2_id}/activate"),
        json!({}),
        Some(ver(&p2)),
    )
    .await;
    assert_eq!(
        (s, p2["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{p2}"
    );
    let (_, p1) = read(&app, &admin, &format!("{X}/exam-policies/{p1_id}")).await;
    assert_eq!(p1["status"], "superseded");
    assert_eq!(
        outbox_count(&pool, &p1_id, "exam_policy.superseded").await,
        1
    );
    // I-003: retirement needs a reason and keeps history.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p1_id}/retire"),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, p1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p1_id}/retire"),
        json!({"reason": "replaced by v2"}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(
        (s, p1["status"].as_str()),
        (StatusCode::OK, Some("retired")),
        "{p1}"
    );
    let (_, list) = read(&app, &meera, &policies).await;
    assert_eq!(
        list["items"].as_array().unwrap().len(),
        2,
        "faculty read: {list}"
    );

    // ================================================================ session + papers (I-004 … I-006)
    let sessions = format!("{X}/institutions/{inst}/exam-sessions");
    let (s, v) = call(
        &app,
        &admin,
        "POST",
        &sessions,
        json!({"academicTermId": Uuid::now_v7(), "code": "ESE-6A", "name": "End semester"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown term: {v}");
    let (s, sess) = call(
        &app,
        &admin,
        "POST",
        &sessions,
        json!({"academicTermId": setup.term_id(), "code": "ESE-6A", "name": "End semester", "gradingSchemeId": setup.scheme_id,
               "startsOn": d(40), "endsOn": d(60), "registrationOpensOn": d(-5), "registrationClosesOn": d(30)}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sess}");
    assert_eq!(
        (sess["status"].as_str(), sess["policyId"].as_str()),
        (Some("draft"), Some(p2_id.as_str())),
        "binds the active policy"
    );
    let sid = id(&sess);
    let papers = format!("{X}/exam-sessions/{sid}/papers");
    let (s, paper) = call(
        &app,
        &admin,
        "POST",
        &papers,
        json!({"offeringId": setup.offering.offering_id, "internalWeight": "40"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{paper}");
    assert_eq!(
        (
            paper["internalWeight"].as_str(),
            paper["maxMarks"].as_str(),
            paper["status"].as_str()
        ),
        (Some("40.00"), Some("100.00"), Some("draft"))
    );
    let pid = id(&paper);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &papers,
        json!({"offeringId": setup.offering.offering_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one live paper per offering");
    let md = b4::seed_offering(&pool, delivery, "MD101", "active").await;
    b4::seed_enrolment(
        &pool,
        delivery,
        &md,
        &setup.finance.student,
        Some(b4::AARAV),
    )
    .await;
    let (s, paper2) = call(
        &app,
        &admin,
        "POST",
        &papers,
        json!({"offeringId": md.offering_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{paper2}");
    let pid2 = id(&paper2);
    let sess_v = |v: &Value| ver(v);
    let (_, sess) = read(&app, &admin, &format!("{X}/exam-sessions/{sid}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/activate"),
        json!({}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "draft → active is illegal");
    let (s, sess) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/submit"),
        json!({}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(
        (s, sess["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{sess}"
    );
    assert_eq!(outbox_count(&pool, &sid, "exam_session.submitted").await, 1);
    assert_eq!(outbox_count(&pool, &sid, "exam_session.approved").await, 1);
    let (s, sess) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/schedule"),
        json!({}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(
        (s, sess["status"].as_str()),
        (StatusCode::OK, Some("scheduled")),
        "{sess}"
    );
    let (s, sess) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/activate"),
        json!({}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(
        (s, sess["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{sess}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/suspend"),
        json!({}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "suspension needs a reason"
    );
    let (_, sess) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/suspend"),
        json!({"reason": "flood"}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(sess["status"], "suspended");
    let (_, sess) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/reopen"),
        json!({}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(sess["status"], "reopened");
    // I-005 governed revision.
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{X}/exam-sessions/{sid}"),
        json!({"name": "End semester (revised)"}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "revision needs a reason"
    );
    let (s, sess) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{X}/exam-sessions/{sid}"),
        json!({"name": "End semester (revised)", "reason": "board resolution"}),
        Some(sess_v(&sess)),
    )
    .await;
    assert_eq!(
        (s, sess["revisions"].as_i64()),
        (StatusCode::OK, Some(1)),
        "{sess}"
    );
    // I-006 duplicate session → merged into the authoritative one.
    let (_, dup) = call(
        &app,
        &admin,
        "POST",
        &sessions,
        json!({"academicTermId": setup.term_id(), "code": "ESE-6A-DUP", "name": "Duplicate"}),
        None,
    )
    .await;
    let dup_id = id(&dup);
    let (s, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{dup_id}/submit"),
        json!({}),
        Some(ver(&dup)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "duplicate of a governing session: {v}"
    );
    let (s, dup) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{dup_id}/merge"),
        json!({"intoSessionId": sid, "reason": "duplicate"}),
        Some(ver(&dup)),
    )
    .await;
    assert_eq!(
        (s, dup["mergedIntoId"].as_str()),
        (StatusCode::OK, Some(sid.as_str())),
        "{dup}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{dup_id}/submit"),
        json!({}),
        Some(ver(&dup)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{dup_id}/registrations"),
        json!({"studentId": Uuid::now_v7()}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "session not operational: {v}");
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &sessions,
        json!({"academicTermId": setup.term_id(), "code": "F1", "name": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = read(&app, &aarav, &sessions).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Retiring the policy that governs an operational session is refused.
    let (_, p2) = read(&app, &admin, &format!("{X}/exam-policies/{p2_id}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-policies/{p2_id}/retire"),
        json!({"reason": "x"}),
        Some(ver(&p2)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // ================================================================ registration (I-017 … I-034)
    b4::seed_registration(
        &pool,
        delivery,
        &setup.finance.student,
        &[setup.offering.component_id],
        "registered",
    )
    .await;
    let (s, opts) = read(&app, &aarav, &format!("{X}/me/exam-registrations/options")).await;
    assert_eq!(s, StatusCode::OK, "{opts}");
    let opt = opts["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["sessionId"] == sid.as_str())
        .cloned()
        .unwrap();
    assert_eq!(opt["window"], "open");
    assert!(opt["papers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["paperId"] == pid.as_str() && p["covered"] == true));
    let (s, mine) = call(
        &app,
        &aarav,
        "POST",
        &format!("{X}/me/exam-registrations"),
        json!({"sessionId": sid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{mine}");
    assert_eq!(
        (mine["status"].as_str(), mine["channel"].as_str()),
        (Some("confirmed"), Some("self_service"))
    );
    assert_eq!(
        mine["papers"].as_array().unwrap().len(),
        1,
        "only the covered paper: {mine}"
    );
    assert!(mine["registrationNumber"]
        .as_str()
        .unwrap()
        .starts_with("EXR-"));
    let reg_a = id(&mine);
    assert_eq!(
        outbox_count(&pool, &reg_a, "exam_registration.confirmed").await,
        1
    );
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{X}/me/exam-registrations"),
        json!({"sessionId": sid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "I-032 one live registration");
    // I-018 bulk from rosters: a second enrolled student.
    let diya = b4::seed_student(&pool, delivery, "Diya", None).await;
    b4::seed_enrolment(&pool, delivery, &setup.offering, &diya, None).await;
    b4::seed_registration(
        &pool,
        delivery,
        &diya,
        &[setup.offering.component_id],
        "registered",
    )
    .await;
    let (s, bulk) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/registrations/bulk"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{bulk}");
    assert_eq!(
        (bulk["processed"].as_i64(), bulk["skipped"].as_i64()),
        (Some(1), Some(1)),
        "{bulk}"
    );
    let (_, regs) = read(
        &app,
        &admin,
        &format!("{X}/exam-sessions/{sid}/registrations?status=confirmed"),
    )
    .await;
    let items = regs["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{regs}");
    let reg_d = items
        .iter()
        .find(|r| r["studentId"] == diya.student_id.to_string())
        .map(id)
        .unwrap();
    // I-026 / I-034: administrative registration of an ineligible candidate → rejected with reasons.
    let kabir = b4::seed_student(&pool, delivery, "Kabir", None).await;
    let (s, rej) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/registrations"),
        json!({"studentId": kabir.student_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rej}");
    assert_eq!(rej["status"], "rejected");
    assert!(
        rej["statusReason"]
            .as_str()
            .unwrap()
            .contains("term registration"),
        "{rej}"
    );
    assert_eq!(
        outbox_count(&pool, &id(&rej), "exam_registration.rejected").await,
        1
    );
    // I-030 a backlog attempt without an earlier attempt is not allowed.
    let (_, rej2) = call(&app, &admin, "POST", &format!("{X}/exam-sessions/{sid}/registrations"), json!({"studentId": kabir.student_id, "papers": [{"paperId": pid, "attemptKind": "backlog"}]}), None).await;
    assert_eq!(rej2["status"], "rejected", "{rej2}");
    // I-021 / I-022 amendment and correction.
    let (_, ra) = read(&app, &admin, &format!("{X}/exam-registrations/{reg_a}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-registrations/{reg_a}/amend"),
        json!({"withdrawPapers": [pid]}),
        Some(ver(&ra)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, ra) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-registrations/{reg_a}/amend"),
        json!({"withdrawPapers": [pid], "reason": "erroneous paper"}),
        Some(ver(&ra)),
    )
    .await;
    assert_eq!(
        (s, ra["status"].as_str(), ra["papers"][0]["status"].as_str()),
        (StatusCode::OK, Some("amended"), Some("withdrawn")),
        "{ra}"
    );
    let (s, ra) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-registrations/{reg_a}/papers"),
        json!({"paperId": pid, "reason": "corrected"}),
        Some(ver(&ra)),
    )
    .await;
    assert_eq!(
        (s, ra["papers"][0]["status"].as_str()),
        (StatusCode::OK, Some("registered")),
        "{ra}"
    );
    let (s, ra) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-registrations/{reg_a}/confirm"),
        json!({}),
        Some(ver(&ra)),
    )
    .await;
    assert_eq!(
        (s, ra["status"].as_str()),
        (StatusCode::OK, Some("confirmed")),
        "{ra}"
    );
    // I-027 scope sync: nothing changed → unchanged.
    let (s, ra2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-registrations/{reg_a}/scope-sync"),
        json!({}),
        Some(ver(&ra)),
    )
    .await;
    assert_eq!((s, ver(&ra2)), (StatusCode::OK, ver(&ra)), "{ra2}");
    // I-029 special arrangement.
    let (s, ra) = call(&app, &admin, "POST", &format!("{X}/exam-registrations/{reg_a}/arrangements"), json!({"kind": "extra_time", "extraMinutes": 30, "note": "approved by the disability cell"}), None).await;
    assert_eq!(
        (s, ra["arrangements"].as_array().map(Vec::len)),
        (StatusCode::CREATED, Some(1)),
        "{ra}"
    );

    // ================================================================ schedule (I-007 … I-016)
    let (s, sch1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/schedules"),
        json!({"name": "Main timetable"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sch1}");
    let sch1_id = id(&sch1);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/schedules"),
        json!({"name": "Second"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one live schedule");
    let sittings = |sch: &str| format!("{X}/exam-schedules/{sch}/sittings");
    let (s, v) = call(
        &app,
        &admin,
        "POST",
        &sittings(&sch1_id),
        json!({"paperId": pid, "sitsOn": d(1), "startsAt": "09:30"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "outside the session window: {v}"
    );
    let (s, x1) = call(
        &app,
        &admin,
        "POST",
        &sittings(&sch1_id),
        json!({"paperId": pid, "sitsOn": d(41), "startsAt": "09:30", "durationMinutes": 180}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{x1}");
    let (_, x2) = call(
        &app,
        &admin,
        "POST",
        &sittings(&sch1_id),
        json!({"paperId": pid2, "sitsOn": d(41), "startsAt": "11:00", "durationMinutes": 120}),
        None,
    )
    .await;
    let (_, conflicts) = read(
        &app,
        &admin,
        &format!("{X}/exam-schedules/{sch1_id}/conflicts"),
    )
    .await;
    assert_eq!(
        conflicts["items"][0]["kind"], "candidate_overlap",
        "I-014: {conflicts}"
    );
    let (_, sch1) = read(&app, &admin, &format!("{X}/exam-schedules/{sch1_id}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch1_id}/submit"),
        json!({}),
        Some(ver(&sch1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "conflicts block submission");
    let (s, x2) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{X}/exam-sittings/{}", id(&x2)),
        json!({"startsAt": "14:00"}),
        Some(ver(&x2)),
    )
    .await;
    assert_eq!(
        (s, x2["startsAt"].as_str()),
        (StatusCode::OK, Some("14:00")),
        "{x2}"
    );
    let (s, sch1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch1_id}/submit"),
        json!({}),
        Some(ver(&sch1)),
    )
    .await;
    assert_eq!(
        (s, sch1["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{sch1}"
    );
    let (s, sch1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch1_id}/publish"),
        json!({}),
        Some(ver(&sch1)),
    )
    .await;
    assert_eq!(
        (s, sch1["status"].as_str()),
        (StatusCode::OK, Some("published")),
        "{sch1}"
    );
    assert_eq!(
        outbox_count(&pool, &sch1_id, "exam_schedule.published").await,
        1
    );
    let (_, p) = read(&app, &admin, &format!("{X}/exam-papers/{pid}")).await;
    assert_eq!(p["status"], "scheduled", "publication schedules the papers");
    // I-016 withdrawal → notified; a new schedule may then be planned.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch1_id}/withdraw"),
        json!({}),
        Some(ver(&sch1)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, sch1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch1_id}/withdraw"),
        json!({"reason": "venue unavailable"}),
        Some(ver(&sch1)),
    )
    .await;
    assert_eq!(
        (s, sch1["status"].as_str()),
        (StatusCode::OK, Some("cancelled")),
        "{sch1}"
    );
    assert_eq!(
        outbox_count(&pool, &sch1_id, "exam_schedule.withdrawn").await,
        1
    );
    let (_, sch2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/schedules"),
        json!({"name": "Main timetable"}),
        None,
    )
    .await;
    let sch2_id = id(&sch2);
    assert_eq!(sch2["versionNo"], 2);
    call(
        &app,
        &admin,
        "POST",
        &sittings(&sch2_id),
        json!({"paperId": pid, "sitsOn": d(42), "startsAt": "09:30"}),
        None,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &sittings(&sch2_id),
        json!({"paperId": pid2, "sitsOn": d(43), "startsAt": "09:30"}),
        None,
    )
    .await;
    let (_, sch2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch2_id}/submit"),
        json!({}),
        Some(ver(&sch2)),
    )
    .await;
    let (_, sch2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch2_id}/publish"),
        json!({}),
        Some(ver(&sch2)),
    )
    .await;
    assert_eq!(sch2["status"], "published", "{sch2}");
    // I-008 revision → authoritative updated schedule (v3, sittings copied).
    let (s, sch3) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch2_id}/revise"),
        json!({"reason": "holiday declared"}),
        Some(ver(&sch2)),
    )
    .await;
    assert_eq!(
        (s, sch3["versionNo"].as_i64(), sch3["status"].as_str()),
        (StatusCode::OK, Some(3), Some("draft")),
        "{sch3}"
    );
    assert_eq!(sch3["sittings"].as_array().unwrap().len(), 2);
    let sch3_id = id(&sch3);
    let x2_3 = sch3["sittings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["paperId"] == pid2.as_str())
        .cloned()
        .unwrap();
    call(
        &app,
        &admin,
        "PATCH",
        &format!("{X}/exam-sittings/{}", id(&x2_3)),
        json!({"sitsOn": d(44)}),
        Some(ver(&x2_3)),
    )
    .await;
    let (_, sch3) = read(&app, &admin, &format!("{X}/exam-schedules/{sch3_id}")).await;
    let (_, sch3) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch3_id}/submit"),
        json!({}),
        Some(ver(&sch3)),
    )
    .await;
    let (_, sch3) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-schedules/{sch3_id}/publish"),
        json!({}),
        Some(ver(&sch3)),
    )
    .await;
    assert_eq!(sch3["status"], "published", "{sch3}");
    let (_, sch2) = read(&app, &admin, &format!("{X}/exam-schedules/{sch2_id}")).await;
    assert_eq!(sch2["status"], "revised");
    let x1 = sch3["sittings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["paperId"] == pid.as_str())
        .cloned()
        .unwrap();
    let x1_id = id(&x1);
    let x2_id = sch3["sittings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["paperId"] == pid2.as_str())
        .map(id)
        .unwrap();
    assert_eq!(x1["sitsOn"].as_str(), Some(d(42).as_str()));
    let (s, tt) = read(&app, &aarav, &format!("{X}/me/exam-timetable")).await;
    assert_eq!(
        (s, tt["items"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(1)),
        "only the registered paper: {tt}"
    );

    // ================================================================ venues + seating (I-009, I-015, I-059)
    let venues = format!("{X}/institutions/{inst}/exam-venues");
    let (_, va) = call(
        &app,
        &admin,
        "POST",
        &venues,
        json!({"code": "HALL-A", "name": "Hall A", "capacity": 1}),
        None,
    )
    .await;
    let (_, vb) = call(
        &app,
        &admin,
        "POST",
        &venues,
        json!({"code": "HALL-B", "name": "Hall B", "capacity": 40}),
        None,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &venues,
        json!({"code": "HALL-A", "name": "Again", "capacity": 5}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, alloc) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x1_id}/venues"),
        json!({"venueId": id(&va)}),
        None,
    )
    .await;
    assert_eq!(
        (s, alloc["items"][0]["seats"].as_i64()),
        (StatusCode::CREATED, Some(1)),
        "{alloc}"
    );
    let (s, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x1_id}/seating"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "two candidates, one seat: {v}");
    call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x1_id}/venues"),
        json!({"venueId": id(&vb), "seats": 20}),
        None,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x2_id}/venues"),
        json!({"venueId": id(&vb), "seats": 41}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "beyond capacity");
    let (s, plan) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x1_id}/seating"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, plan["candidates"].as_i64()),
        (StatusCode::CREATED, Some(2)),
        "{plan}"
    );
    let seats: Vec<&str> = plan["seats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["seatNo"].as_str().unwrap())
        .collect();
    assert_eq!(seats, ["HALL-A-001", "HALL-B-001"]);
    let plan_id = id(&plan);
    let (s, plan) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/seating-plans/{plan_id}/publish"),
        json!({}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(
        (s, plan["status"].as_str()),
        (StatusCode::OK, Some("published")),
        "{plan}"
    );
    let (s, plan2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/seating-plans/{plan_id}/revise"),
        json!({"reason": "seat conflict"}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(
        (s, plan2["planNo"].as_i64(), plan2["status"].as_str()),
        (StatusCode::OK, Some(2), Some("draft")),
        "I-059: {plan2}"
    );
    call(
        &app,
        &admin,
        "POST",
        &format!("{X}/seating-plans/{}/publish", id(&plan2)),
        json!({}),
        Some(ver(&plan2)),
    )
    .await;
    let (_, tt) = read(&app, &aarav, &format!("{X}/me/exam-timetable")).await;
    assert!(tt["items"][0]["seatNo"].is_string(), "{tt}");

    // ================================================================ invigilation (I-010 … I-012, I-060)
    let (_, staff) = read(
        &app,
        &admin,
        &format!("{X}/exam-sittings/{x1_id}/invigilation"),
    )
    .await;
    assert_eq!(
        (
            staff["candidates"].as_i64(),
            staff["required"].as_i64(),
            staff["shortage"].as_i64()
        ),
        (Some(2), Some(1), Some(1)),
        "I-011: {staff}"
    );
    let (s, duty) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x1_id}/invigilation"),
        json!({"subjectRef": b4::MEERA, "name": "Dr. Meera Shah"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{duty}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x1_id}/invigilation"),
        json!({"subjectRef": b4::MEERA}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (_, staff) = read(
        &app,
        &admin,
        &format!("{X}/exam-sittings/{x1_id}/invigilation"),
    )
    .await;
    assert_eq!(staff["shortage"].as_i64(), Some(0));
    let (s, mine) = read(&app, &meera, &format!("{X}/me/invigilation")).await;
    assert_eq!(
        (s, mine["items"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(1)),
        "{mine}"
    );

    // ================================================================ hall tickets (I-037 … I-042)
    let charge = b5::seed_charge(&pool, &setup.finance, "1000", "published", None).await;
    let (s, issued) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/hall-tickets/issue"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, issued["processed"].as_i64()),
        (StatusCode::OK, Some(2)),
        "{issued}"
    );
    let (_, tickets) = read(
        &app,
        &admin,
        &format!("{X}/exam-sessions/{sid}/hall-tickets"),
    )
    .await;
    let t_a = tickets["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["registrationId"] == reg_a.as_str())
        .cloned()
        .unwrap();
    let t_d = tickets["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["registrationId"] == reg_d.as_str())
        .cloned()
        .unwrap();
    assert_eq!(t_a["status"], "withheld", "I-038 dues: {t_a}");
    assert!(t_a["withheldReason"].as_str().unwrap().contains("dues"));
    assert!(
        t_a["specialArrangements"]
            .as_str()
            .unwrap()
            .contains("extra_time"),
        "I-040: {t_a}"
    );
    assert_eq!(t_d["status"], "active");
    assert!(t_d["number"].as_str().unwrap().starts_with("HT-"));
    assert_eq!(
        outbox_count(&pool, &id(&t_a), "hall_ticket.withheld").await,
        1
    );
    let (s, again) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/hall-tickets/issue"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, again["skipped"].as_i64()),
        (StatusCode::OK, Some(2)),
        "idempotent: {again}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/hall-tickets/{}/release", id(&t_a)),
        json!({}),
        Some(ver(&t_a)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "dues still outstanding");
    b5::seed_payment(&pool, &setup.finance, charge.charge_id, "1000").await;
    let (s, t_a) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/hall-tickets/{}/release", id(&t_a)),
        json!({}),
        Some(ver(&t_a)),
    )
    .await;
    assert_eq!(
        (s, t_a["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{t_a}"
    );
    // I-041 duplicate / lost ticket → reissued, exactly one valid.
    let (s, t_d2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/hall-tickets/{}/reissue", id(&t_d)),
        json!({"reason": "duplicate printed"}),
        Some(ver(&t_d)),
    )
    .await;
    assert_eq!(
        (s, t_d2["status"].as_str(), t_d2["replacesId"].as_str()),
        (StatusCode::OK, Some("active"), Some(id(&t_d).as_str())),
        "{t_d2}"
    );
    assert_ne!(t_d2["number"], t_d["number"]);
    let current: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.hall_ticket WHERE registration_id = $1 AND status = 'active'",
    )
    .bind(Uuid::parse_str(&reg_d).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(current, 1);
    let (s, mine) = read(&app, &aarav, &format!("{X}/me/hall-tickets")).await;
    assert_eq!(
        (s, mine["items"][0]["ticket"]["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{mine}"
    );
    let (s, fin) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/hall-tickets/finalise"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, fin["processed"].as_i64()),
        (StatusCode::OK, Some(2)),
        "I-042: {fin}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/hall-tickets/issue"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // ================================================================ conduct (I-043, I-055, I-057, I-060, I-174, I-175, I-185)
    let (s, c1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x1_id}/conduct"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, c1["status"].as_str()),
        (StatusCode::CREATED, Some("scheduled")),
        "{c1}"
    );
    let c1_id = id(&c1);
    let (_, c1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/conduct-records/{c1_id}/start"),
        json!({}),
        Some(ver(&c1)),
    )
    .await;
    assert_eq!(c1["status"], "in_progress");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/conduct-records/{c1_id}/suspend"),
        json!({}),
        Some(ver(&c1)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, c1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/conduct-records/{c1_id}/suspend"),
        json!({"reason": "power failure"}),
        Some(ver(&c1)),
    )
    .await;
    assert_eq!(c1["status"], "suspended");
    assert_eq!(
        outbox_count(&pool, &c1_id, "conduct_record.suspended").await,
        1
    );
    let (_, c1) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/conduct-records/{c1_id}/resume"),
        json!({}),
        Some(ver(&c1)),
    )
    .await;
    assert_eq!(c1["status"], "in_progress");
    // I-060 mid-conduct replacement keeps the replaced duty.
    let (s, rep) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/invigilation-duties/{}/replace", id(&duty)),
        json!({"subjectRef": "dev|substitute@sutra.dev", "reason": "taken ill"}),
        Some(ver(&duty)),
    )
    .await;
    assert_eq!(
        (s, rep["replacesId"].as_str()),
        (StatusCode::OK, Some(id(&duty).as_str())),
        "{rep}"
    );
    // I-057 failed entry condition: a withheld ticket turns "present" into a recorded denial.
    let (s, t_d3) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/hall-tickets/{}/withhold", id(&t_d2)),
        json!({"reason": "identity not verified"}),
        Some(ver(&t_d2)),
    )
    .await;
    assert_eq!(
        (s, t_d3["status"].as_str()),
        (StatusCode::OK, Some("withheld")),
        "{t_d3}"
    );
    let (s, c1) = call(&app, &admin, "POST", &format!("{X}/conduct-records/{c1_id}/complete"), json!({"attendance": [{"registrationId": reg_a, "outcome": "present"}, {"registrationId": reg_d, "outcome": "present"}]}), Some(ver(&c1))).await;
    assert_eq!(
        (
            s,
            c1["status"].as_str(),
            c1["presentCount"].as_i64(),
            c1["deniedCount"].as_i64()
        ),
        (StatusCode::OK, Some("completed"), Some(1), Some(1)),
        "{c1}"
    );
    let denied = c1["attendance"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["registrationId"] == reg_d.as_str())
        .cloned()
        .unwrap();
    assert_eq!(denied["outcome"], "denied_entry");
    assert!(
        denied["reason"].as_str().unwrap().contains("withheld"),
        "{denied}"
    );
    let (s, t_d3) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/hall-tickets/{}/release", id(&t_d3)),
        json!({}),
        Some(ver(&t_d3)),
    )
    .await;
    assert_eq!(
        (s, t_d3["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{t_d3}"
    );
    assert_eq!(
        outbox_count(&pool, &c1_id, "conduct_record.completed").await,
        1
    );
    let (_, p) = read(&app, &admin, &format!("{X}/exam-papers/{pid}")).await;
    assert_eq!(p["status"], "conducted");
    let (_, ra) = read(&app, &admin, &format!("{X}/exam-registrations/{reg_a}")).await;
    assert_eq!(ra["papers"][0]["status"], "appeared");
    let (_, rd) = read(&app, &admin, &format!("{X}/exam-registrations/{reg_d}")).await;
    assert_eq!(rd["papers"][0]["status"], "absent");
    // I-185 postponement of the second sitting under an authorised contingency.
    let (_, c2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x2_id}/conduct"),
        json!({}),
        None,
    )
    .await;
    let c2_id = id(&c2);
    let (s, c2) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/conduct-records/{c2_id}/postpone"),
        json!({"newDate": d(50), "newStartsAt": "10:00", "reason": "strike"}),
        Some(ver(&c2)),
    )
    .await;
    assert_eq!(
        (s, c2["status"].as_str()),
        (StatusCode::OK, Some("postponed")),
        "{c2}"
    );
    assert_eq!(
        outbox_count(&pool, &c2_id, "conduct_record.postponed").await,
        1
    );
    let (_, x2) = read(&app, &admin, &format!("{X}/exam-sittings/{x2_id}")).await;
    assert_eq!(
        (x2["sitsOn"].as_str(), x2["startsAt"].as_str()),
        (Some(d(50).as_str()), Some("10:00")),
        "{x2}"
    );
    let (s, c2b) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sittings/{x2_id}/conduct"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "a new conduct for the rescheduled sitting: {c2b}"
    );

    // ================================================================ exceptions (I-172)
    let (s, exc) = call(&app, &admin, "POST", &format!("{X}/exam-sessions/{sid}/exceptions"), json!({"category": "conduct", "description": "question paper misprint", "sittingId": x1_id}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{exc}");
    assert!(exc["number"].as_str().unwrap().starts_with("EXC-"));
    let exc_id = id(&exc);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-exceptions/{exc_id}/close"),
        json!({}),
        Some(ver(&exc)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "raised → closed is illegal");
    let (_, exc) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-exceptions/{exc_id}/classify"),
        json!({"classification": "paper_error", "severity": "high"}),
        Some(ver(&exc)),
    )
    .await;
    let (_, exc) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-exceptions/{exc_id}/review"),
        json!({}),
        Some(ver(&exc)),
    )
    .await;
    let (_, exc) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-exceptions/{exc_id}/resolve"),
        json!({"resolution": "erratum announced"}),
        Some(ver(&exc)),
    )
    .await;
    let (s, exc) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-exceptions/{exc_id}/close"),
        json!({}),
        Some(ver(&exc)),
    )
    .await;
    assert_eq!(
        (s, exc["status"].as_str(), exc["severity"].as_str()),
        (StatusCode::OK, Some("closed"), Some("high")),
        "{exc}"
    );

    // ================================================================ late registration, override, reopening, finalisation
    let (_, sess) = read(&app, &admin, &format!("{X}/exam-sessions/{sid}")).await;
    let (s, sess) = call(&app, &admin, "PATCH", &format!("{X}/exam-sessions/{sid}"), json!({"registrationOpensOn": d(-10), "registrationClosesOn": d(-1), "reason": "window closed"}), Some(ver(&sess))).await;
    assert_eq!(s, StatusCode::OK, "{sess}");
    let isha = b4::seed_student(&pool, delivery, "Isha", None).await;
    b4::seed_registration(
        &pool,
        delivery,
        &isha,
        &[setup.offering.component_id],
        "registered",
    )
    .await;
    let (s, late) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/registrations"),
        json!({"studentId": isha.student_id}),
        None,
    )
    .await;
    assert_eq!(
        (s, late["channel"].as_str(), late["status"].as_str()),
        (StatusCode::CREATED, Some("late"), Some("confirmed")),
        "I-019: {late}"
    );
    assert_eq!(late["lateFee"]["amount"], "500.00", "{late}");
    assert!(late["lateFee"]["chargeNumber"]
        .as_str()
        .unwrap()
        .starts_with("CHG-"));
    let (s, v) = call(
        &app,
        &aarav,
        "POST",
        &format!("{X}/me/exam-registrations"),
        json!({"sessionId": sid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, ov) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/registration-overrides"),
        json!({"studentId": kabir.student_id, "validUntil": d(5), "reason": "medical"}),
        None,
    )
    .await;
    assert_eq!(
        (s, ov["status"].as_str()),
        (StatusCode::CREATED, Some("approved")),
        "I-020: {ov}"
    );
    let (s, sess) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/registration-window/reopen"),
        json!({"closesOn": d(3), "reason": "extension"}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(
        (s, sess["registrationClosesOn"].as_str()),
        (StatusCode::OK, Some(d(3).as_str())),
        "I-025: {sess}"
    );
    let (s, fin) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/registration-population/finalise"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, fin["processed"].as_i64()),
        (StatusCode::OK, Some(3)),
        "I-036: {fin}"
    );
    assert_eq!(
        outbox_count(&pool, &reg_a, "exam_registration.locked").await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-sessions/{sid}/registrations"),
        json!({"studentId": kabir.student_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "population finalised");
    // I-023 / I-039 / I-024 withdrawal invalidates the ticket; reinstatement restores the registration.
    let (_, rd) = read(&app, &admin, &format!("{X}/exam-registrations/{reg_d}")).await;
    assert_eq!(rd["status"], "locked");
    let (s, rd) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-registrations/{reg_d}/withdraw"),
        json!({"reason": "left the programme"}),
        Some(ver(&rd)),
    )
    .await;
    assert_eq!(
        (
            s,
            rd["status"].as_str(),
            rd["hallTicket"]["status"].as_str()
        ),
        (StatusCode::OK, Some("withdrawn"), Some("withdrawn")),
        "{rd}"
    );
    assert_eq!(
        outbox_count(&pool, &reg_d, "exam_registration.withdrawn").await,
        1
    );
    let (s, rd) = call(
        &app,
        &admin,
        "POST",
        &format!("{X}/exam-registrations/{reg_d}/reinstate"),
        json!({"reason": "readmitted"}),
        Some(ver(&rd)),
    )
    .await;
    assert_eq!(
        (s, rd["status"].as_str()),
        (StatusCode::OK, Some("confirmed")),
        "{rd}"
    );
    // Status history kept every step of the session.
    let history: i64 = sqlx::query_scalar("SELECT count(*) FROM exams.status_history WHERE aggregate_type = 'exam_session' AND aggregate_id = $1")
        .bind(Uuid::parse_str(&sid).unwrap())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert!(history >= 6, "{history}");
}

// ------------------------------------------------------------------ AwaitDecision

fn admin_ctx() -> AuthContext {
    AuthContext {
        session_id: Uuid::now_v7(),
        subject: "dev|admin@sutra.dev".into(),
        login_id: "admin@sutra.dev".into(),
        display_name: "Admin".into(),
        roles: vec![Role::Administrator],
        scope: ExecutionScope::tenant(TENANT.parse::<TenantId>().unwrap()),
        csrf_token: "x".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        idle_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        mfa_level: MfaLevel::Mfa,
        amr: vec!["pwd".into(), "otp".into()],
        step_up_enforced: false,
    }
}

fn awaiting_app(pool: &sqlx::PgPool) -> Router {
    let mut st = sutra_api::app::exams_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let (router, _) = sutra_exams::router_with_state(st).split_for_parts();
    let ctx = admin_ctx();
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

/// L3-I-004 with an awaited `SESSION_APPROVAL` decision: the session stays `under_review`, the
/// submission event plans an approval to-do for the submitter, the explicit decision approves
/// (or returns it to draft with a reason).
#[tokio::test]
async fn session_approval_awaits_a_decision() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Awaiting Exams College",
        None,
        "draft",
        "draft",
    )
    .await;
    let inst = setup.institution().as_uuid();
    let custom = awaiting_app(&pool);
    let injected = Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    };
    let (s, sess) = call(&custom, &injected, "POST", &format!("{X}/institutions/{inst}/exam-sessions"), json!({"academicTermId": setup.term_id(), "code": "AWAIT-1", "name": "Awaited session", "gradingSchemeId": setup.scheme_id, "kind": "special"}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{sess}");
    let sid = id(&sess);
    let (s, sess) = call(
        &custom,
        &injected,
        "POST",
        &format!("{X}/exam-sessions/{sid}/submit"),
        json!({}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(
        (s, sess["status"].as_str()),
        (StatusCode::OK, Some("under_review")),
        "{sess}"
    );
    assert_eq!(
        outbox_count(&pool, &sid, "exam_session.approved").await,
        0,
        "no decision yet"
    );
    let raw: Value = sqlx::query_scalar("SELECT envelope FROM platform.outbox WHERE subject = $1 AND event_type = 'sutraerp.exams.exam_session.submitted'")
        .bind(&sid)
        .fetch_one(&pool)
        .await
        .unwrap();
    let env: EventEnvelope = serde_json::from_value(raw).unwrap();
    let actions = sutra_platform::inbox::rules::plan(&env).unwrap().unwrap();
    assert_eq!(actions.len(), 2, "admin notification + submitter to-do");
    let (s, _) = call(
        &custom,
        &injected,
        "POST",
        &format!("{X}/exam-sessions/{sid}/decision"),
        json!({"decision": "reject"}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a return needs a reason"
    );
    let (s, sess) = call(
        &custom,
        &injected,
        "POST",
        &format!("{X}/exam-sessions/{sid}/decision"),
        json!({"decision": "approve", "reason": "ok"}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(
        (s, sess["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{sess}"
    );
    assert_eq!(outbox_count(&pool, &sid, "exam_session.approved").await, 1);
    let (s, _) = call(
        &custom,
        &injected,
        "POST",
        &format!("{X}/exam-sessions/{sid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already decided");
}
