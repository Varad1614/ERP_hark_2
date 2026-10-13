//! Batch 6 integrator DB tests — cross-slice wiring after 6A → 6B → 6C merged
//! (docs/batch6-contracts.md §7, §12):
//!
//! * `absent_at_conduct_is_absent_on_the_mark_sheet` — 6A records a registered paper `absent` at
//!   conduct (L3-I-055); 6B shows the candidate as absent on the mark sheet, never stores external
//!   marks for them, and submits a still-pending absent row as absent instead of blocking the sheet.
//! * `decided_malpractice_feeds_the_result_outcome` — L3-I-095 / 112: 6C's computation reads 6B's
//!   decided malpractice cases read-only: a decided marks deduction reduces the external marks and
//!   makes the outcome `malpractice`; overturned (`none`, then closed) and merged duplicate cases are ignored;
//!   a case reopened for investigation no longer counts on recompute.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use support::b6::{self, ExamSetup};
use support::*;
use sutra_contracts::exams::ids::{ExamPaperId, ExamRegistrationId};
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_contracts::exams::status::{AttemptKind, ExamRegistrationStatus, RegistrationChannel};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_exams::shared::registration::{
    insert_registration, NewRegistration, NewRegistrationPaper,
};
use uuid::Uuid;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000b6e00";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-0000000b6e01";
const API: &str = "/api/v1/exams";

async fn call(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    (s, b)
}

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().expect("version")
}

fn row(list: &Value, registration: ExamRegistrationId) -> Value {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["registrationId"] == json!(registration))
        .expect("candidate row")
        .clone()
}

/// Registers another seeded student of the setup's institution for `papers`.
async fn register_other(
    pool: &sqlx::PgPool,
    setup: &ExamSetup,
    name: &str,
    papers: &[ExamPaperId],
) -> CandidateSummary {
    let s = b4::seed_student(pool, &setup.finance.delivery, name, None).await;
    let mut tx = pool.begin().await.unwrap();
    let reg = insert_registration(
        &mut tx,
        &NewRegistration {
            tenant: setup.tenant(),
            institution: setup.institution(),
            session_id: setup.session_id,
            student_id: StudentId(s.student_id),
            student_context_id: StudentContextId(s.context_id),
            student_number: s.student_number.clone(),
            display_name: format!("{name} Seeded"),
            subject_ref: None,
            channel: RegistrationChannel::Administrative,
            status: ExamRegistrationStatus::Confirmed,
            papers: papers
                .iter()
                .map(|p| NewRegistrationPaper {
                    paper_id: *p,
                    attempt_kind: AttemptKind::Regular,
                    attempt_no: 1,
                })
                .collect(),
        },
        "test.seed",
        &b6::seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    reg
}

/// Marks a registered paper `absent` as 6A's conduct record does (test seeding).
async fn mark_absent(pool: &sqlx::PgPool, reg: ExamRegistrationId) {
    sqlx::query(
        "UPDATE exams.exam_registration_paper SET status = 'absent' WHERE registration_id = $1",
    )
    .bind(reg.as_uuid())
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn absent_at_conduct_is_absent_on_the_mark_sheet() {
    let (app, pool) = app(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT_A,
        "Absence University",
        Some(b4::AARAV),
        "active",
        "conducted",
    )
    .await;
    b4::seed_faculty(
        &pool,
        &setup.finance.delivery,
        &setup.offering,
        b4::MEERA,
        "active",
    )
    .await;
    let p = setup.paper_id;
    let aarav = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[p],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    let isha = register_other(&pool, &setup, "Isha", &[p]).await;
    mark_absent(&pool, aarav.registration_id).await;
    mark_absent(&pool, isha.registration_id).await;

    let open_path = format!("{API}/exam-papers/{p}/mark-sheet");
    let (s, sheet) = call(&app, &admin, "POST", &open_path, json!({}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{sheet}");
    let sheet_id = sheet["id"].as_str().unwrap().to_owned();

    // ---- both candidates show as absent before anyone touches their rows.
    let marks_path = format!("{API}/exam-papers/{p}/marks");
    let (s, list) = read(&app, &meera, &marks_path).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    for reg in [aarav.registration_id, isha.registration_id] {
        let r = row(&list, reg);
        assert_eq!(r["absent"], true, "{r}");
        assert_eq!(r["status"], "pending", "{r}");
        assert!(r["externalMarks"].is_null(), "{r}");
    }

    // ---- external marks are never stored for a candidate absent at conduct.
    let r = row(&list, aarav.registration_id);
    let (s, list) = call(
        &app,
        &meera,
        "PUT",
        &marks_path,
        json!({"rows": [{"id": r["id"], "version": ver(&r), "externalMarks": "50"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let r = row(&list, aarav.registration_id);
    assert_eq!(r["status"], "entered", "{r}");
    assert_eq!(r["absent"], true, "{r}");
    assert!(r["externalMarks"].is_null(), "{r}");
    assert_eq!(list["sheet"]["status"], "in_evaluation");

    // ---- submission does not block on the pending absent row: it is submitted as absent.
    let (s, submitted) = call(
        &app,
        &meera,
        "POST",
        &format!("{API}/mark-sheets/{sheet_id}/submit"),
        json!({}),
        Some(ver(&list["sheet"])),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{submitted}");
    assert_eq!(submitted["status"], "evaluated");
    let (_, list) = read(&app, &meera, &marks_path).await;
    for reg in [aarav.registration_id, isha.registration_id] {
        let r = row(&list, reg);
        assert_eq!(r["status"], "submitted", "{r}");
        assert_eq!(r["absent"], true, "{r}");
    }
    let stored: Vec<(bool, Option<rust_decimal::Decimal>)> =
        sqlx::query_as("SELECT absent, external_marks FROM exams.paper_mark WHERE paper_id = $1")
            .bind(p.as_uuid())
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored.len(), 2);
    assert!(stored.iter().all(|(a, e)| *a && e.is_none()), "{stored:?}");
}

/// Inserts a malpractice case row as 6B's governed decision leaves it (test seeding).
#[allow(clippy::too_many_arguments)]
async fn seed_case(
    pool: &sqlx::PgPool,
    setup: &ExamSetup,
    reg: &CandidateSummary,
    number: &str,
    status: &str,
    penalty: Option<&str>,
    deduction: Option<&str>,
    merged_into: Option<Uuid>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO exams.malpractice_case (tenant_id, institution_id, session_id, \
         registration_id, paper_id, student_id, case_number, allegation, status, penalty, \
         marks_deduction, merged_into_id, decided_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'unauthorised material', $8, $9, \
         $10::numeric, $11, now()) RETURNING id",
    )
    .bind(setup.tenant().as_uuid())
    .bind(setup.institution().as_uuid())
    .bind(setup.session_id.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(setup.paper_id.as_uuid())
    .bind(reg.student_id.as_uuid())
    .bind(number)
    .bind(status)
    .bind(penalty)
    .bind(deduction)
    .bind(merged_into)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn current(app: &axum::Router, admin: &Login, session: Uuid) -> Value {
    let (s, list) = read(
        app,
        admin,
        &format!("{API}/exam-sessions/{session}/results"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    list["items"][0].clone()
}

#[tokio::test]
async fn decided_malpractice_feeds_the_result_outcome() {
    let (app, pool) = app(TENANT_B).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT_B,
        "Integrity University",
        Some(b4::AARAV),
        "active",
        "conducted",
    )
    .await;
    let sid = setup.session_id.as_uuid();
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("30", "40")),
        Some("60"),
    )
    .await;
    let decided = seed_case(
        &pool,
        &setup,
        &reg,
        "MC-INT-1",
        "decided",
        Some("marks_deduction"),
        Some("25"),
        None,
    )
    .await;
    // Overturned on appeal (6B stores `none`) and since closed, and a duplicate merged into the
    // decided case (only one open case per candidate paper).
    seed_case(
        &pool,
        &setup,
        &reg,
        "MC-INT-2",
        "closed",
        Some("none"),
        None,
        None,
    )
    .await;
    seed_case(
        &pool,
        &setup,
        &reg,
        "MC-INT-3",
        "closed",
        Some("debarred"),
        None,
        Some(decided),
    )
    .await;

    let (s, batch) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/exam-sessions/{sid}/result-batches"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{batch}");
    let bid = batch["id"].as_str().unwrap().to_owned();
    let compute = format!("{API}/result-batches/{bid}/compute");
    let (s, run) = call(&app, &admin, "POST", &compute, json!({}), Some(ver(&batch))).await;
    assert_eq!(s, StatusCode::OK, "{run}");
    let r = current(&app, &admin, sid).await;
    assert_eq!(r["outcome"], "malpractice", "{r}");
    assert_eq!(r["lines"][0]["externalMarks"], "35.00", "60 − 25: {r}");
    assert_ne!(r["lines"][0]["remark"], Value::Null, "{r}");

    // ---- reopened for investigation (decided → under_investigation): no longer counted.
    sqlx::query("UPDATE exams.malpractice_case SET status = 'under_investigation' WHERE id = $1")
        .bind(decided)
        .execute(&pool)
        .await
        .unwrap();
    let (_, b) = read(&app, &admin, &format!("{API}/result-batches/{bid}")).await;
    let (s, run) = call(&app, &admin, "POST", &compute, json!({}), Some(ver(&b))).await;
    assert_eq!(s, StatusCode::OK, "{run}");
    let r = current(&app, &admin, sid).await;
    assert_ne!(r["outcome"], "malpractice", "{r}");
    assert_eq!(r["lines"][0]["externalMarks"], "60.00", "{r}");
}
