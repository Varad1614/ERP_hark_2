//! Batch 6 slice 6B — assessment & marks (Domain I) against PostgreSQL.
//!
//! * `marks_collection_import_entry_approval_and_gradebook` — L3/L4-I-071 (marks collection for
//!   a conducted paper, faculty authority on own sections only), I-072 (verification → final
//!   marks approval `MARKS_APPROVAL` → final approved marks, paper `marks_finalised`), internal
//!   assessment import from Batch 4 + re-import after a 4B correction, gradebook with the shared
//!   grading engine, `/me/assessments`.
//! * `evaluation_rounds_exceptions_withheld_and_moderation` — I-073 (panel evaluator
//!   assignment), I-074 (late / missing marks exceptions), I-076 (incomplete evaluation →
//!   withheld marks, release), I-080 … I-082 (policy second evaluation, difference beyond the
//!   threshold → adjudication), I-075 (grace moderation with before / after report), I-077
//!   (supplementary attempt marked through the same flow).
//! * `malpractice_report_decision_appeal_reopen_merge_close` — I-170, I-163 … I-165, I-162,
//!   I-168, I-171 (and the debarment / restoration of the registered paper).
//! * `await_decision_keeps_marks_and_malpractice_decisions_pending` — WorkflowPort
//!   `AwaitDecision` (contract §9) for `MARKS_APPROVAL`, `MALPRACTICE_DECISION` and
//!   `MALPRACTICE_REVIEW` (overturn → restored outcome, I-166 / I-167).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b6::{self, dec, ExamSetup};
use support::*;
use sutra_contracts::exams::ids::{ExamPaperId, ExamRegistrationId};
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_contracts::exams::status::{
    AttemptKind, EvaluationStatus, ExamRegistrationStatus, MalpracticeCaseStatus,
    MalpracticePenalty, MarkStatus, RegistrationChannel,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_exams::shared::approval::ApprovalMode;
use sutra_exams::shared::registration::{
    insert_registration, NewRegistration, NewRegistrationPaper,
};
use sutra_exams::shared::Actor;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b6b00";
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
    v["version"].as_i64().unwrap()
}

/// Registers another seeded student of the setup's institution for `papers`.
async fn register_other(
    pool: &sqlx::PgPool,
    setup: &ExamSetup,
    name: &str,
    papers: &[(ExamPaperId, AttemptKind)],
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
                .map(|(p, k)| NewRegistrationPaper {
                    paper_id: *p,
                    attempt_kind: *k,
                    attempt_no: if *k == AttemptKind::Regular { 1 } else { 2 },
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

fn row(list: &Value, registration: ExamRegistrationId) -> Value {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["registrationId"] == json!(registration))
        .expect("candidate row")
        .clone()
}

async fn paper_status(pool: &sqlx::PgPool, paper: ExamPaperId) -> String {
    sqlx::query_scalar("SELECT status FROM exams.exam_paper WHERE id = $1")
        .bind(paper.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn reg_paper_status(pool: &sqlx::PgPool, reg: ExamRegistrationId) -> String {
    sqlx::query_scalar(
        "SELECT status FROM exams.exam_registration_paper WHERE registration_id = $1",
    )
    .bind(reg.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn outbox_count(pool: &sqlx::PgPool, event_type: &str, subject: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(event_type)
    .bind(subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn eval(mark: &str, round: &str, marks: &str, who: &str) -> Value {
    json!({"paperMarkId": mark, "round": round, "marks": marks, "evaluatorRef": who})
}

#[tokio::test]
async fn marks_collection_import_entry_approval_and_gradebook() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Marks University",
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
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    let internal_id = b6::seed_internal_result(&pool, &setup, "34.50", "40").await;
    let p = setup.paper_id;

    // A paper of another offering of the same course (Meera does not teach it).
    let other_offering = b4::seed_offering(&pool, &setup.finance.delivery, "CS101", "active").await;
    let other_paper = b6::seed_paper(
        &pool,
        &setup.finance,
        setup.session_id,
        &other_offering,
        "4",
        "40",
        "conducted",
    )
    .await;

    // ---- authority: faculty see their own sections' papers only; students never.
    let (s, list) = read(&app, &meera, &format!("{API}/exam-papers/{p}/marks")).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(row(&list, reg.registration_id)["status"], "pending");
    assert!(list["sheet"].is_null());
    let (s, _) = read(
        &app,
        &meera,
        &format!("{API}/exam-papers/{other_paper}/marks"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "not your section → 404");
    let (s, _) = read(&app, &aarav, &format!("{API}/exam-papers/{p}/marks")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, mine) = read(&app, &meera, &format!("{API}/me/assessments")).await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    let items = mine["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["paperId"] == json!(p)));
    assert!(!items.iter().any(|i| i["paperId"] == json!(other_paper)));
    let (s, _) = read(
        &app,
        &meera,
        &format!("{API}/me/assessments/{other_paper}/marks"),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // ---- open the mark sheet (paper conducted → evaluation); a second open is a conflict.
    let open_path = format!("{API}/exam-papers/{p}/mark-sheet");
    let (s, sheet) = call(&app, &admin, "POST", &open_path, json!({}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{sheet}");
    assert_eq!(sheet["status"], "planned");
    assert_eq!(paper_status(&pool, p).await, "evaluation");
    let sheet_id = sheet["id"].as_str().unwrap().to_owned();
    let (s, _) = call(&app, &admin, "POST", &open_path, json!({}), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let import = format!("{API}/mark-sheets/{sheet_id}/import-internal");
    let (s, _) = call(&app, &meera, "POST", &import, json!({}), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "import is exam-cell work");

    // ---- internal assessment import (Batch 4), then a 4B correction → re-import.
    let (s, sum) = call(&app, &admin, "POST", &import, json!({}), None).await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(
        (sum["processed"].as_u64(), sum["skipped"].as_u64()),
        (Some(1), Some(0))
    );
    let (_, again) = call(&app, &admin, "POST", &import, json!({}), None).await;
    assert_eq!(again["skipped"].as_u64(), Some(1), "unchanged → skipped");
    sqlx::query(
        "UPDATE academic.internal_assessment_result SET marks_obtained = 35.00, \
         result_version = result_version + 1 WHERE id = $1",
    )
    .bind(internal_id)
    .execute(&pool)
    .await
    .unwrap();
    let (_, again) = call(&app, &admin, "POST", &import, json!({}), None).await;
    assert_eq!(again["processed"].as_u64(), Some(1), "{again}");
    let (_, list) = read(&app, &meera, &format!("{API}/exam-papers/{p}/marks")).await;
    let r = row(&list, reg.registration_id);
    assert_eq!(r["internalMarks"], "35.00");
    assert_eq!(r["internalMax"], "40.00");
    assert_eq!(r["internalSourceId"], json!(internal_id));

    // ---- marks entry by the faculty member: validation, stale version, preview.
    let marks_path = format!("{API}/exam-papers/{p}/marks");
    let mark_id = r["id"].as_str().unwrap().to_owned();
    let v = ver(&r);
    let (s, bad) = call(
        &app,
        &meera,
        "PUT",
        &marks_path,
        json!({"rows": [{"id": mark_id, "version": v, "externalMarks": "101"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    let (s, _) = call(
        &app,
        &meera,
        "PUT",
        &marks_path,
        json!({"rows": [{"id": mark_id, "version": v + 7, "externalMarks": "61"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, list) = call(
        &app,
        &meera,
        "PUT",
        &marks_path,
        json!({"rows": [{"id": mark_id, "version": v, "externalMarks": "61"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let r = row(&list, reg.registration_id);
    assert_eq!(r["status"], "entered");
    assert_eq!(r["externalMarks"], "61.00");
    // 35/40 × 40 + 61/100 × 60 = 35 + 36.6 = 71.60 → A (8)
    assert_eq!(r["preview"]["percent"], "71.60");
    assert_eq!(r["preview"]["grade"], "A");
    assert_eq!(r["preview"]["gradePoint"], "8.00");
    assert_eq!(list["sheet"]["status"], "in_evaluation");

    // ---- submit (faculty) → verify (exam cell) → approval (auto) → finalise.
    let cmd = |verb: &str| format!("{API}/mark-sheets/{sheet_id}/{verb}");
    let (s, sheet) = call(
        &app,
        &meera,
        "POST",
        &cmd("submit"),
        json!({}),
        Some(ver(&list["sheet"])),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    assert_eq!(sheet["status"], "evaluated");
    assert_eq!(
        outbox_count(&pool, "sutraerp.exams.mark_sheet.submitted", &sheet_id).await,
        1
    );
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &cmd("verify"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "verification is exam-cell work");
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("verify"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    assert_eq!(sheet["status"], "validated");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &cmd("finalise"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "finalise needs approved marks");
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("request-approval"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    assert_eq!(sheet["status"], "approved");
    assert_eq!(sheet["approvedBy"], "system:auto-approval");
    assert!(sheet["processInstanceId"].is_string());
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("finalise"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    assert_eq!(sheet["status"], "finalised");
    assert_eq!(paper_status(&pool, p).await, "marks_finalised");
    for t in [
        "sutraerp.exams.mark_sheet.verified",
        "sutraerp.exams.mark_sheet.approved",
        "sutraerp.exams.mark_sheet.finalised",
    ] {
        assert_eq!(outbox_count(&pool, t, &sheet_id).await, 1, "{t}");
    }
    let marks = sutra_api::app::exams_deps_with(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
        // Part 1: explicit dev/test auto-approval opt-in (the subject here is not approval).
        sutra_kernel::approval::ApprovalMode::AutoApprove,
    )
    .into_state()
    .marks
    .marks_of_registration(&setup.scope(), reg.registration_id)
    .await
    .unwrap();
    assert_eq!(
        marks[0].status,
        MarkStatus::Finalised,
        "6C reads final marks"
    );
    let (s, _) = call(
        &app,
        &meera,
        "PUT",
        &marks_path,
        json!({"rows": [{"id": mark_id, "version": marks[0].version, "externalMarks": "70"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "final marks are not edited");

    // ---- gradebook (shared grading engine) + history.
    let (s, gb) = read(&app, &meera, &format!("{API}/exam-papers/{p}/gradebook")).await;
    assert_eq!(s, StatusCode::OK, "{gb}");
    assert_eq!(gb["candidates"].as_u64(), Some(1));
    assert_eq!(gb["passed"].as_u64(), Some(1));
    assert_eq!(gb["lines"][0]["grade"], "A");
    assert_eq!(gb["lines"][0]["percent"], "71.60");
    assert_eq!(gb["averagePercent"], "71.60");
    assert!(gb["distribution"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["grade"] == "A" && d["count"] == 1));
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.status_history WHERE aggregate_type = 'mark_sheet' \
         AND aggregate_id = $1::uuid",
    )
    .bind(&sheet_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(history >= 6, "planned … finalised recorded ({history})");
}

#[tokio::test]
async fn evaluation_rounds_exceptions_withheld_and_moderation() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Evaluation University",
        None,
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
    let a = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[p],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    // I-077: a supplementary attempt is marked through the same flow.
    let b = register_other(&pool, &setup, "Riya", &[(p, AttemptKind::Supplementary)]).await;

    // ---- policy: second evaluation required, threshold 10, deadline already passed.
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/exam-papers/{p}/mark-sheet"),
        json!({"secondEvaluationRequired": true, "differenceThreshold": "10", "dueOn": "2020-01-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sheet}");
    let sheet_id = sheet["id"].as_str().unwrap().to_owned();
    let cmd = |verb: &str| format!("{API}/mark-sheets/{sheet_id}/{verb}");
    // I-073: panel evaluators.
    for (who, role) in [
        ("dev|examiner1@sutra.dev", "panel_member"),
        ("dev|examiner2@sutra.dev", "second_examiner"),
        ("dev|examiner3@sutra.dev", "adjudicator"),
    ] {
        let (s, x) = call(
            &app,
            &admin,
            "POST",
            &cmd("assignments"),
            json!({"evaluatorRef": who, "role": role}),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{x}");
    }
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &cmd("assignments"),
        json!({"evaluatorRef": "dev|examiner1@sutra.dev", "role": "panel_member"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "one live assignment per evaluator and role"
    );
    let sheet_path = format!("{API}/mark-sheets/{sheet_id}");
    let (_, sheet) = read(&app, &admin, &sheet_path).await;
    assert_eq!(sheet["status"], "assigned");

    let marks_path = format!("{API}/exam-papers/{p}/marks");
    let (_, list) = read(&app, &admin, &marks_path).await;
    let ra = row(&list, a.registration_id);
    let rb = row(&list, b.registration_id);
    assert_eq!(rb["attemptKind"], "supplementary");
    let (ma, mb) = (
        ra["id"].as_str().unwrap().to_owned(),
        rb["id"].as_str().unwrap().to_owned(),
    );

    // ---- I-080 … I-082: first 40, second 62 (difference 22 > 10) → adjudication 55.
    let path = format!("{API}/exam-papers/{p}/evaluations");
    let (s, e) = call(
        &app,
        &meera,
        "POST",
        &path,
        eval(&ma, "first", "40", "dev|examiner1@sutra.dev"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(e["resolution"]["state"], "awaiting_second");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &path,
        eval(&ma, "adjudication", "50", "dev|examiner3@sutra.dev"),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "no adjudication before a differing second evaluation"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &path,
        eval(&ma, "second", "62", "dev|examiner1@sutra.dev"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the second evaluator must differ");
    let (s, e) = call(
        &app,
        &admin,
        "POST",
        &path,
        eval(&ma, "second", "62", "dev|examiner2@sutra.dev"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(e["resolution"]["state"], "awaiting_adjudication");
    assert_eq!(
        e["mark"]["status"], "pending",
        "unresolved → nothing written"
    );
    assert_eq!(
        outbox_count(&pool, "sutraerp.exams.evaluation.second_required", &ma).await,
        1
    );
    let (s, e) = call(
        &app,
        &admin,
        "POST",
        &path,
        eval(&ma, "adjudication", "55", "dev|examiner3@sutra.dev"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(e["resolution"]["state"], "resolved");
    assert_eq!(e["resolution"]["marks"], "55.00");
    assert_eq!(e["mark"]["externalMarks"], "55.00");
    assert_eq!(e["mark"]["status"], "entered");

    // ---- I-074: late submission and missing marks need accepted exceptions.
    let (_, sheet) = read(&app, &admin, &sheet_path).await;
    let (s, err) = call(
        &app,
        &meera,
        "POST",
        &cmd("submit"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{err}");
    for body in [
        json!({"kind": "late_submission", "reason": "Evaluator was on medical leave"}),
        json!({"kind": "missing_marks", "paperMarkId": mb, "reason": "Answer script under re-verification"}),
    ] {
        let (s, x) = call(&app, &meera, "POST", &cmd("exceptions"), body, None).await;
        assert_eq!(s, StatusCode::CREATED, "{x}");
        let (s, x) = call(
            &app,
            &admin,
            "POST",
            &format!(
                "{API}/marks-exceptions/{}/resolve",
                x["id"].as_str().unwrap()
            ),
            json!({"decision": "accept", "reason": "approved by the controller"}),
            Some(ver(&x)),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{x}");
        assert_eq!(x["status"], "accepted");
    }
    let (s, sheet) = call(
        &app,
        &meera,
        "POST",
        &cmd("submit"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    // I-076: the candidate without marks is withheld (and notified through the outbox).
    let (_, list) = read(&app, &admin, &marks_path).await;
    assert_eq!(row(&list, b.registration_id)["status"], "withheld");
    assert_eq!(row(&list, a.registration_id)["status"], "submitted");
    assert_eq!(
        outbox_count(&pool, "sutraerp.exams.paper_mark.withheld", &mb).await,
        1
    );

    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("verify"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");

    // ---- I-075: grace moderation (policy bound 10 % of the maximum).
    let moderation = format!("{API}/exam-papers/{p}/moderation");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &moderation,
        json!({"kind": "grace", "value": "15", "reason": "Too generous"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, m) = call(
        &app,
        &admin,
        "POST",
        &moderation,
        json!({"kind": "grace", "value": "3", "reason": "Question 4 was ambiguous"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{m}");
    assert_eq!(m["status"], "requested");
    assert_eq!(
        m["report"].as_array().unwrap().len(),
        1,
        "only verified scripts"
    );
    let mid = m["id"].as_str().unwrap().to_owned();
    let (_, sh) = read(&app, &admin, &sheet_path).await;
    assert_eq!(sh["status"], "moderation");
    let (s, m) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/moderations/{mid}/validate"),
        json!({}),
        Some(ver(&m)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{m}");
    assert_eq!(
        m["status"], "approved",
        "auto-approved (MODERATION_APPROVAL)"
    );
    let (s, m) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/moderations/{mid}/apply"),
        json!({}),
        Some(ver(&m)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{m}");
    assert_eq!(m["status"], "applied");
    assert_eq!(m["appliedCount"].as_i64(), Some(1));
    assert_eq!(m["report"][0]["after"]["graceMarks"], "3.00");
    assert_eq!(
        outbox_count(&pool, "sutraerp.exams.moderation.applied", &mid).await,
        1
    );
    let (_, list) = read(&app, &admin, &marks_path).await;
    let ra = row(&list, a.registration_id);
    assert_eq!(ra["status"], "moderated");
    assert_eq!(ra["graceMarks"], "3.00");
    let sheet = list["sheet"].clone();
    assert_eq!(sheet["status"], "validated");

    // ---- approval + finalisation: the withheld candidate keeps the paper in evaluation.
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("request-approval"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("finalise"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    assert_eq!(paper_status(&pool, p).await, "evaluation");

    // ---- release the withheld marks: the sheet reopens; marks, second evaluation, approval.
    let (_, list) = read(&app, &admin, &marks_path).await;
    let rb = row(&list, b.registration_id);
    let (s, rel) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/paper-marks/{mb}/release"),
        json!({"reason": "Script re-verified"}),
        Some(ver(&rb)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rel}");
    assert_eq!(rel["status"], "entered");
    let (s, list) = call(
        &app,
        &meera,
        "PUT",
        &marks_path,
        json!({"rows": [{"id": mb, "version": ver(&rel), "externalMarks": "45"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let sheet = list["sheet"].clone();
    assert_eq!(sheet["status"], "in_evaluation");
    let (s, sheet) = call(
        &app,
        &meera,
        "POST",
        &cmd("submit"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &cmd("verify"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "policy second evaluation outstanding"
    );
    let (s, e) = call(
        &app,
        &admin,
        "POST",
        &path,
        eval(&mb, "second", "47", "dev|examiner2@sutra.dev"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{e}");
    assert_eq!(
        e["resolution"]["marks"], "46.00",
        "mean of 45 and 47 (within the threshold)"
    );
    assert_eq!(e["mark"]["status"], "submitted");
    let (_, sheet) = read(&app, &admin, &sheet_path).await;
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("verify"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    let (_, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("request-approval"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    let (s, sheet) = call(
        &app,
        &admin,
        "POST",
        &cmd("finalise"),
        json!({}),
        Some(ver(&sheet)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sheet}");
    assert_eq!(paper_status(&pool, p).await, "marks_finalised");
    let (_, evals) = read(&app, &admin, &path).await;
    assert_eq!(evals["items"].as_array().unwrap().len(), 5, "{evals}");
    assert!(evals["resolutions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["state"] == "resolved"));
}

#[tokio::test]
async fn malpractice_report_decision_appeal_reopen_merge_close() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Integrity University",
        None,
        "active",
        "conducted",
    )
    .await;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    let session = setup.session_id;
    let p = setup.paper_id;
    let cases = format!("{API}/exam-sessions/{session}/malpractice-cases");
    let case = |id: &str, verb: &str| format!("{API}/malpractice-cases/{id}/{verb}");

    // ---- I-170: report → investigation → hearing → committee decision (debarred).
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &cases,
        json!({"registrationId": reg.registration_id, "allegation": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "case handling is exam-cell work");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &cases,
        json!({"registrationId": reg.registration_id, "paperId": p,
               "allegation": "Unauthorised notes found during the examination",
               "evidence": "Invigilator report IR-17"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    assert!(c["caseNumber"].as_str().unwrap().starts_with("MAL-"));
    assert_eq!(c["status"], "reported");
    let id = c["id"].as_str().unwrap().to_owned();
    // I-168: a duplicate report is refused while the case is open.
    let (s, dup) = call(
        &app,
        &admin,
        "POST",
        &cases,
        json!({"registrationId": reg.registration_id, "paperId": p, "allegation": "Same incident"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{dup}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "decision"),
        json!({"penalty": "debarred", "reason": "too early"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a decision follows investigation");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "investigate"),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "hearing"),
        json!({"reason": "Hearing on 2 Dec"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "hearing");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "decision"),
        json!({"penalty": "marks_deduction", "reason": "no amount"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "decision"),
        json!({"penalty": "debarred", "reason": "Committee found the use of unauthorised material"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "decided");
    assert_eq!(c["penalty"], "debarred");
    assert_eq!(
        reg_paper_status(&pool, reg.registration_id).await,
        "debarred"
    );
    assert_eq!(
        outbox_count(&pool, "sutraerp.exams.malpractice_case.decided", &id).await,
        1
    );

    // ---- I-163 / I-165: appeal → review modifies the penalty to a warning (paper restored).
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "appeal"),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an appeal states its grounds"
    );
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "appeal"),
        json!({"reason": "Notes were unrelated to the paper"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "appealed");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "review"),
        json!({"outcome": "modify", "penalty": "warning", "reason": "Material was not relevant; a warning suffices"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "modified");
    assert_eq!(c["penalty"], "warning");
    assert_eq!(
        reg_paper_status(&pool, reg.registration_id).await,
        "registered",
        "debarment lifted"
    );
    assert_eq!(
        outbox_count(&pool, "sutraerp.exams.malpractice_case.reviewed", &id).await,
        1
    );

    // ---- I-162: formal reopening → new decision (marks deduction) → close → archive (I-171).
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "reopen"),
        json!({"reason": "New evidence from CCTV review authorised by the controller"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "under_investigation");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "decision"),
        json!({"penalty": "marks_deduction", "marksDeduction": "5", "reason": "Partial use established"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["penalty"], "marks_deduction");
    assert_eq!(c["marksDeduction"], "5.00");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "close"),
        json!({"reason": "Penalty applied"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "closed");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &case(&id, "archive"),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "archived");
    let kinds: Vec<&str> = c["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["decision", "review", "reopen", "decision"]);
    let rewrite =
        sqlx::query("UPDATE exams.malpractice_decision SET reason = 'x' WHERE case_id = $1::uuid")
            .bind(&id)
            .execute(&pool)
            .await;
    assert!(rewrite.is_err(), "decisions are append-only");

    // ---- I-168: an undecided duplicate is merged into the authoritative whole-registration case.
    let (s, whole) = call(
        &app,
        &admin,
        "POST",
        &cases,
        json!({"registrationId": reg.registration_id, "allegation": "Impersonation suspected"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{whole}");
    let (s, part) = call(
        &app,
        &admin,
        "POST",
        &cases,
        json!({"registrationId": reg.registration_id, "paperId": p, "allegation": "Impersonation in this paper"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{part}");
    let (s, merged) = call(
        &app,
        &admin,
        "POST",
        &case(part["id"].as_str().unwrap(), "merge"),
        json!({"intoCaseId": whole["id"], "reason": "same incident"}),
        Some(ver(&part)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{merged}");
    assert_eq!(merged["status"], "closed");
    assert_eq!(merged["mergedIntoId"], whole["id"]);
    // Part 1 S3 (DEF-I-006): malpractice cases are confidential to the examination office;
    // faculty holding exam.read alone are refused.
    let (s, body) = read(&app, &meera, &format!("{cases}?status=reported")).await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "faculty must not list malpractice cases: {body}"
    );
    let (s, list) = read(&app, &admin, &format!("{cases}?status=reported")).await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let open = list["items"].as_array().unwrap();
    assert_eq!(open.len(), 1, "one authoritative open case");
    assert_eq!(open[0]["id"], whole["id"]);
}

#[tokio::test]
async fn await_decision_keeps_marks_and_malpractice_decisions_pending() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Approval University",
        None,
        "active",
        "conducted",
    )
    .await;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    let mut st = sutra_api::app::exams_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let _router = sutra_exams::router_with_state(st.clone());
    let t = setup.tenant();
    let maker = Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };
    let approver = Actor {
        id: Some("dev|controller@sutra.dev".into()),
        correlation_id: None,
    };
    use sutra_exams::integrity::repo as mal;
    use sutra_exams::marks::repo as mk;

    // ---- MARKS_APPROVAL stays pending until the explicit decision.
    let sheet = mk::open_sheet(&st, t, setup.paper_id, mk::SheetSettings::default(), &maker)
        .await
        .unwrap();
    let rows = mk::candidates(&pool, t, setup.paper_id).await.unwrap();
    assert_eq!(rows.len(), 1);
    mk::enter_marks(
        &st,
        t,
        setup.paper_id,
        vec![mk::EntryRow {
            id: rows[0].mark.id,
            version: rows[0].mark.version,
            external_marks: Some(dec("72")),
            internal_marks: Some(dec("30")),
            internal_max: Some(dec("40")),
            absent: None,
        }],
        &maker,
    )
    .await
    .unwrap();
    let s = mk::get_sheet(&st, t, sheet.id).await.unwrap();
    let s = mk::submit_sheet(&st, t, sheet.id, Some(s.version), &maker)
        .await
        .unwrap();
    let s = mk::verify_sheet(&st, t, sheet.id, Some(s.version), &maker)
        .await
        .unwrap();
    let s = mk::request_marks_approval(&st, t, sheet.id, Some(s.version), &maker)
        .await
        .unwrap();
    assert_eq!(
        s.status,
        EvaluationStatus::Validated,
        "pending, not approved"
    );
    assert!(s.approval_requested_at.is_some() && s.process_instance_id.is_some());
    let again = mk::request_marks_approval(&st, t, sheet.id, Some(s.version), &maker).await;
    assert!(again.is_err(), "one pending request");
    let m = mk::candidate(&pool, t, rows[0].mark.id).await.unwrap();
    assert_eq!(m.mark.status, MarkStatus::Verified);
    let rejected =
        mk::decide_sheet(&st, t, sheet.id, Some(s.version), false, None, &approver).await;
    assert!(rejected.is_err(), "a rejection needs a reason");
    let s = mk::decide_sheet(
        &st,
        t,
        sheet.id,
        Some(s.version),
        false,
        Some("Recheck question 3 totals"),
        &approver,
    )
    .await
    .unwrap();
    assert_eq!(s.status, EvaluationStatus::Validated);
    assert!(s.approval_requested_at.is_none());
    let s = mk::request_marks_approval(&st, t, sheet.id, Some(s.version), &maker)
        .await
        .unwrap();
    let s = mk::decide_sheet(&st, t, sheet.id, Some(s.version), true, None, &approver)
        .await
        .unwrap();
    assert_eq!(s.status, EvaluationStatus::Approved);
    assert_eq!(s.approved_by.as_deref(), Some("dev|controller@sutra.dev"));
    let m = mk::candidate(&pool, t, rows[0].mark.id).await.unwrap();
    assert_eq!(m.mark.status, MarkStatus::Approved);

    // ---- MALPRACTICE_DECISION / MALPRACTICE_REVIEW pending → decided → overturned (I-166/167).
    let c = mal::report_case(
        &st,
        t,
        setup.session_id,
        mal::NewCase {
            registration_id: reg.registration_id,
            paper_id: Some(setup.paper_id),
            allegation: "Mobile phone in the hall".into(),
            evidence: None,
        },
        &maker,
    )
    .await
    .unwrap();
    let c = mal::advance(
        &st,
        t,
        c.id,
        Some(c.version),
        MalpracticeCaseStatus::UnderInvestigation,
        None,
        &maker,
    )
    .await
    .unwrap();
    let c = mal::propose_decision(
        &st,
        t,
        c.id,
        Some(c.version),
        MalpracticePenalty::PaperCancelled,
        None,
        "Phone switched on during the examination",
        &maker,
    )
    .await
    .unwrap();
    assert_eq!(
        c.status,
        MalpracticeCaseStatus::UnderInvestigation,
        "pending"
    );
    assert_eq!(c.proposal_kind.as_deref(), Some("decision"));
    assert_eq!(
        reg_paper_status(&pool, reg.registration_id).await,
        "registered",
        "nothing applied yet"
    );
    let c = mal::approve_proposal(&st, t, c.id, Some(c.version), true, None, &approver)
        .await
        .unwrap();
    assert_eq!(c.status, MalpracticeCaseStatus::Decided);
    assert_eq!(c.penalty, Some(MalpracticePenalty::PaperCancelled));
    assert_eq!(
        reg_paper_status(&pool, reg.registration_id).await,
        "debarred"
    );
    let c = mal::appeal(
        &st,
        t,
        c.id,
        Some(c.version),
        "Phone was switched off",
        &maker,
    )
    .await
    .unwrap();
    let c = mal::propose_review(
        &st,
        t,
        c.id,
        Some(c.version),
        "overturn",
        None,
        None,
        "Phone was proven switched off and outside reach",
        &maker,
    )
    .await
    .unwrap();
    assert_eq!(c.status, MalpracticeCaseStatus::UnderReview);
    assert_eq!(
        reg_paper_status(&pool, reg.registration_id).await,
        "debarred",
        "still pending"
    );
    let c = mal::approve_proposal(&st, t, c.id, Some(c.version), true, None, &approver)
        .await
        .unwrap();
    assert_eq!(c.status, MalpracticeCaseStatus::Overturned);
    assert_eq!(c.penalty, Some(MalpracticePenalty::None));
    assert_eq!(
        reg_paper_status(&pool, reg.registration_id).await,
        "registered",
        "examination outcome restored"
    );
}
