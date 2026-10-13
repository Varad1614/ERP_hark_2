//! Batch 6 foundation smoke test: migrations + the shared examination tables (append-only
//! history at trigger level), the grading-scheme API (validation, ETag / If-Match, permissions),
//! the shared lifecycle / registration / marks / results helpers (legal transitions only, one
//! live registration and one current result per candidate, governed revision of final marks),
//! the pure grading engine on stored data, the real `ExamDirectory` / `MarksReader` /
//! `ResultsPort` adapters, and the cross-domain ports wired for Batch 6 (`AcademicExamFacts`
//! over 4B, `ExamFeePort` → a published `exam_fee` charge in Finance, `FinanceClearancePort`
//! seeing it as dues).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::json;
use std::sync::Arc;
use support::b6::{self, dec, seed_actor};
use support::*;
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::exams::ids::{ExamSessionId, PaperMarkId};
use sutra_contracts::exams::status::{
    ExamRegistrationStatus, ExamSessionStatus, MarkStatus, ResultOutcome, ResultStatus,
};
use sutra_contracts::exams::StudentTermResultQuery;
use sutra_contracts::finance::ports::ExamFeeRequest;
use sutra_contracts::finance::status::{ChargeSourceType, ChargeStatus};
use sutra_contracts::finance::{FinanceEvidence, StudentFinanceQuery};
use sutra_contracts::student::ids::StudentId;
use sutra_exams::shared::grading::{aggregate, grade_line, LineInput};
use sutra_exams::shared::lifecycle::{change_session_status, StatusChange};
use sutra_exams::shared::marks::{self, MarkValues};
use sutra_exams::shared::results::{change_result_status, insert_result, NewResult, NewResultLine};
use sutra_exams::shared::rows::lock_session;
use sutra_kernel::error::KernelError;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b6f00";
const SCHEMES: &str = "/api/v1/exams";

fn bands() -> serde_json::Value {
    json!([
        {"grade": "O", "minPercent": "90", "gradePoint": "10", "isPass": true},
        {"grade": "A", "minPercent": "70", "gradePoint": "8", "isPass": true},
        {"grade": "P", "minPercent": "40", "gradePoint": "4", "isPass": true},
        {"grade": "F", "minPercent": "0", "gradePoint": "0", "isPass": false}
    ])
}

#[tokio::test]
async fn exams_foundation_tables_helpers_ports_and_grading_api() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Exams University",
        Some(b4::AARAV),
        "scheduled",
        "conducted",
    )
    .await;
    let (t, scope) = (setup.tenant(), setup.scope());
    let inst = setup.institution().as_uuid();

    // ---- migrations: every foundation table exists.
    for table in [
        "grading_scheme",
        "grade_band",
        "exam_session",
        "exam_paper",
        "exam_registration",
        "exam_registration_paper",
        "paper_mark",
        "paper_mark_history",
        "term_result",
        "term_result_line",
        "number_sequence",
        "status_history",
    ] {
        let found: Option<String> =
            sqlx::query_scalar(&format!("SELECT to_regclass('exams.{table}')::text"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(found.is_some(), "exams.{table} missing");
    }

    // ---- grading-scheme API: validation, create, ETag, list, retire, permissions.
    let base = format!("{SCHEMES}/institutions/{inst}/grading-schemes");
    let (s, _, body) = send(
        &app,
        write(
            &admin,
            "POST",
            &base,
            json!({"code": "BAD", "name": "No floor", "bands": [
                {"grade": "A", "minPercent": "50", "gradePoint": "8", "isPass": true},
                {"grade": "F", "minPercent": "10", "gradePoint": "0", "isPass": false}]}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (s, h, scheme) = send(
        &app,
        write(
            &admin,
            "POST",
            &base,
            json!({"code": "CBCS-10", "name": "CBCS ten-point", "bands": bands()}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{scheme}");
    assert_eq!(h["etag"].to_str().unwrap(), "\"1\"");
    assert_eq!(scheme["bands"][0]["grade"], "O");
    assert_eq!(scheme["bands"][0]["gradePoint"], "10.00");
    assert_eq!(scheme["bands"][3]["minPercent"], "0.00");
    let scheme_id = scheme["id"].as_str().unwrap().to_owned();
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &base,
            json!({"code": "CBCS-10", "name": "Duplicate", "bands": bands()}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "codes are unique per institution");
    let (s, _, _) = send(
        &app,
        write(
            &aarav,
            "POST",
            &base,
            json!({"code": "STU", "name": "x", "bands": bands()}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, list) = send(&app, get(&meera, &base)).await;
    assert_eq!(s, StatusCode::OK, "faculty hold exam.read");
    assert_eq!(list["items"].as_array().unwrap().len(), 2, "{list}");
    let (s, _, _) = send(&app, get(&aarav, &base)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students have no staff read");
    let retire = format!("{SCHEMES}/grading-schemes/{scheme_id}/retire");
    let (s, _, _) = send(&app, write(&admin, "POST", &retire, json!({}), None)).await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _, _) = send(&app, write(&admin, "POST", &retire, json!({}), Some(9))).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, h, retired) = send(
        &app,
        write(
            &admin,
            "POST",
            &retire,
            json!({"reason": "replaced"}),
            Some(1),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{retired}");
    assert_eq!(
        (retired["status"].as_str(), h["etag"].to_str().unwrap()),
        (Some("retired"), "\"2\"")
    );
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE subject = $1 AND event_type LIKE 'sutraerp.exams.grading_scheme.%'",
    )
    .bind(&scheme_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 2, "created + retired");

    // ---- session lifecycle: only legal transitions (scheduled → active; never draft → active).
    let mut tx = pool.begin().await.unwrap();
    let s0 = lock_session(&mut tx, t, setup.session_id).await.unwrap();
    assert_eq!(s0.status, ExamSessionStatus::Scheduled);
    let change = StatusChange {
        cause_event_type: "test",
        reason: Some("exam window opens"),
        process_instance_id: None,
    };
    let err = change_session_status(
        &mut tx,
        t,
        &s0,
        ExamSessionStatus::Closed,
        &change,
        &seed_actor(),
    )
    .await
    .unwrap_err();
    assert!(matches!(err, KernelError::Conflict(_)), "{err:?}");
    let active = change_session_status(
        &mut tx,
        t,
        &s0,
        ExamSessionStatus::Active,
        &change,
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (active.status, active.version),
        (ExamSessionStatus::Active, s0.version + 1)
    );

    // ---- registration: one live registration per candidate, mark rows created, directory views.
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    assert!(
        reg.registration_number.starts_with("EXR-"),
        "{}",
        reg.registration_number
    );
    assert_eq!(reg.papers.len(), 1);
    let mut tx = pool.begin().await.unwrap();
    let dup = sutra_exams::shared::registration::insert_registration(
        &mut tx,
        &sutra_exams::shared::registration::NewRegistration {
            tenant: t,
            institution: setup.institution(),
            session_id: setup.session_id,
            student_id: reg.student_id,
            student_context_id: reg.student_context_id,
            student_number: reg.student_number.clone(),
            display_name: reg.display_name.clone(),
            subject_ref: None,
            channel: sutra_contracts::exams::status::RegistrationChannel::SelfService,
            status: ExamRegistrationStatus::Draft,
            papers: vec![],
        },
        "test",
        &seed_actor(),
    )
    .await;
    assert!(
        matches!(dup, Err(KernelError::Conflict(_))),
        "L3-I-032: {dup:?}"
    );
    drop(tx);
    let state = sutra_api::app::exams_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    let mine = state
        .directory
        .registrations_of_subject(&scope, b4::AARAV)
        .await
        .unwrap();
    assert_eq!(mine.len(), 1);
    assert_eq!(mine[0].papers[0].paper_id, setup.paper_id);
    assert!(state
        .directory
        .registration_of_student(&scope, setup.session_id, StudentId(setup.student_id()))
        .await
        .unwrap()
        .is_some());
    assert_eq!(
        state
            .directory
            .papers_of_session(&scope, setup.session_id)
            .await
            .unwrap()
            .len(),
        1
    );
    let session = state
        .directory
        .session(&scope, setup.session_id)
        .await
        .unwrap()
        .unwrap();
    assert!(session.status.is_operational());
    let scheme = state
        .directory
        .grading_scheme(&scope, setup.scheme_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(scheme.bands.len(), 8);
    assert!(state
        .directory
        .session(&scope, ExamSessionId(uuid::Uuid::now_v7()))
        .await
        .unwrap()
        .is_none());

    // ---- marks: the shared machine end to end, history, illegal moves, governed revision.
    let pending = state
        .marks
        .marks_of_registration(&scope, reg.registration_id)
        .await
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        (pending[0].status, pending[0].external_max),
        (MarkStatus::Pending, dec("100"))
    );
    let fin = b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("30", "40")),
        Some("60"),
    )
    .await;
    assert_eq!(fin.status, MarkStatus::Finalised);
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.paper_mark_history WHERE paper_mark_id = $1",
    )
    .bind(fin.id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        history, 5,
        "entered, submitted, verified, approved, finalised"
    );
    let rewrite =
        sqlx::query("UPDATE exams.paper_mark_history SET reason = 'x' WHERE paper_mark_id = $1")
            .bind(fin.id.as_uuid())
            .execute(&pool)
            .await;
    assert!(rewrite.is_err(), "mark history is append-only");
    let mut tx = pool.begin().await.unwrap();
    let m = marks::lock_mark(&mut tx, t, PaperMarkId(fin.id.as_uuid()))
        .await
        .unwrap();
    let err = marks::update(
        &mut tx,
        t,
        &m,
        None,
        MarkStatus::Entered,
        "entry",
        None,
        &seed_actor(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, KernelError::Conflict(_)),
        "final marks change only via revise_final"
    );
    let mut revised_values = MarkValues::of(&m);
    revised_values.external_marks = Some(dec("68"));
    let revised = marks::revise_final(
        &mut tx,
        t,
        &m,
        revised_values,
        "revaluation",
        "revaluation outcome",
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        (revised.status, revised.external_marks),
        (MarkStatus::Finalised, Some(dec("68")))
    );

    // ---- grading on stored data + results: computed → validated → approved → published.
    let paper = state
        .directory
        .paper(&scope, setup.paper_id)
        .await
        .unwrap()
        .unwrap();
    let input = LineInput {
        credits: paper.credits,
        internal_marks: revised.internal_marks,
        internal_max: revised.internal_max,
        external_marks: revised.external_marks,
        external_max: revised.external_max,
        grace_marks: revised.grace_marks,
        internal_weight: paper.internal_weight,
        pass_percent: paper.pass_percent,
        absent: revised.absent,
    };
    let graded = grade_line(&input, &scheme.bands);
    // 30/40 × 40 + 68/100 × 60 = 30 + 40.8 = 70.80 → A (8)
    assert_eq!(graded.percent, Some(dec("70.80")));
    assert_eq!((graded.grade.as_deref(), graded.passed), (Some("A"), true));
    let agg = aggregate(&[(paper.credits, graded.clone())]);
    assert_eq!(agg.sgpa, Some(dec("8.00")));
    let line = NewResultLine {
        paper_id: paper.id,
        curriculum_component_id: paper.curriculum_component_id,
        paper_code: paper.paper_code.clone(),
        title: paper.title.clone(),
        credits: paper.credits,
        attempt_kind: reg.papers[0].attempt_kind,
        internal_marks: revised.internal_marks,
        external_marks: revised.external_marks,
        grace_marks: revised.grace_marks,
        total_marks: None,
        max_marks: revised.external_max,
        percent: graded.percent,
        grade: graded.grade.clone(),
        grade_point: graded.grade_point,
        passed: graded.passed,
        remark: None,
    };
    let new_result = |status: ResultStatus| NewResult {
        tenant: t,
        registration: &reg,
        academic_term_id: setup.term_id(),
        grading_scheme_id: setup.scheme_id,
        credits_attempted: agg.credits_attempted,
        credits_earned: agg.credits_earned,
        grade_points: agg.grade_points,
        sgpa: agg.sgpa,
        cgpa: agg.sgpa,
        outcome: ResultOutcome::Pass,
        status,
        status_reason: None,
        result_batch_id: None,
        lines: vec![line.clone()],
    };
    let results = sutra_api::app::student_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .results;
    let query = StudentTermResultQuery {
        institution_id: setup.institution(),
        student_id: setup.student_id(),
        academic_term_id: setup.term_id(),
    };
    let mut tx = pool.begin().await.unwrap();
    let r1 = insert_result(
        &mut tx,
        &new_result(ResultStatus::Computed),
        "test",
        &seed_actor(),
    )
    .await
    .unwrap();
    let err = change_result_status(
        &mut tx,
        t,
        &r1,
        ResultStatus::Published,
        &change,
        &seed_actor(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(err, KernelError::Conflict(_)),
        "publication needs approval"
    );
    let r1 = change_result_status(
        &mut tx,
        t,
        &r1,
        ResultStatus::Validated,
        &change,
        &seed_actor(),
    )
    .await
    .unwrap();
    let r1 = change_result_status(
        &mut tx,
        t,
        &r1,
        ResultStatus::Approved,
        &change,
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(
        results
            .term_result(&scope, query.clone())
            .await
            .unwrap()
            .is_none(),
        "Domain C sees nothing before publication"
    );
    let mut tx = pool.begin().await.unwrap();
    let r1 = change_result_status(
        &mut tx,
        t,
        &r1,
        ResultStatus::Published,
        &change,
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(r1.published_at.is_some());
    assert_eq!(r1.lines.len(), 1);
    let summary = results
        .term_result(&scope, query.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (summary.sgpa, summary.credits_earned, summary.revision),
        (Some(8.0), 4.0, 1)
    );
    assert!(summary.failed_component_ids.is_empty());
    // A revision supersedes the current result (one current result per candidate and session).
    let mut tx = pool.begin().await.unwrap();
    let r2 = insert_result(
        &mut tx,
        &new_result(ResultStatus::Computed),
        "test",
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!((r2.revision, r2.supersedes_id), (2, Some(r1.id)));
    let old_status: String =
        sqlx::query_scalar("SELECT status FROM exams.term_result WHERE id = $1")
            .bind(r1.id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(old_status, "superseded");
    let lines_rewrite =
        sqlx::query("UPDATE exams.term_result_line SET grade = 'O' WHERE result_id = $1")
            .bind(r2.id.as_uuid())
            .execute(&pool)
            .await;
    assert!(lines_rewrite.is_err(), "result lines are immutable");

    // ---- Domain D facts (AcademicExamFacts over 4B tables).
    let ia = b6::seed_internal_result(&pool, &setup, "34.50", "40").await;
    let facts = state
        .academic
        .internal_results(&scope, CourseOfferingId(setup.offering.offering_id))
        .await
        .unwrap();
    assert_eq!(facts.len(), 1);
    assert_eq!(
        (
            facts[0].result_id,
            facts[0].marks_obtained,
            facts[0].max_marks
        ),
        (ia, dec("34.50"), dec("40"))
    );
    assert!(state
        .academic
        .attendance_standing(
            &scope,
            CourseOfferingId(setup.offering.offering_id),
            StudentId(setup.student_id())
        )
        .await
        .unwrap()
        .is_none());

    // ---- Finance: exam fee → published `exam_fee` charge (idempotent) → dues for clearance.
    let dues = StudentFinanceQuery {
        institution_id: setup.institution(),
        student_id: setup.student_id(),
    };
    assert_eq!(
        state
            .finance
            .student_dues(&scope, dues.clone())
            .await
            .unwrap(),
        FinanceEvidence::NotApplicable
    );
    let source = uuid::Uuid::now_v7();
    let fee = |amount: &str| ExamFeeRequest {
        institution_id: setup.institution(),
        student_id: setup.student_id(),
        fee_code: "EXAM-REVAL".into(),
        description: "Revaluation fee — CS101".into(),
        amount: dec(amount),
        academic_term_id: Some(setup.term_id()),
        due_date: None,
        source_id: source,
        actor: Some("test:b6".into()),
    };
    let charge = state
        .fees
        .raise_exam_fee(&scope, fee("500.00"))
        .await
        .unwrap();
    assert!(!charge.already_raised);
    assert!(charge.charge_number.starts_with("CHG-"));
    let again = state
        .fees
        .raise_exam_fee(&scope, fee("500.00"))
        .await
        .unwrap();
    assert_eq!(
        (again.charge_id, again.already_raised),
        (charge.charge_id, true)
    );
    let c = state
        .charges
        .charge(&scope, charge.charge_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (c.status, c.outstanding_amount),
        (ChargeStatus::Published, dec("500.00"))
    );
    let src: String = sqlx::query_scalar("SELECT source_type FROM finance.charge WHERE id = $1")
        .bind(charge.charge_id.as_uuid())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(src, ChargeSourceType::ExamFee.as_str());
    let posted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE source_type = 'charge' AND source_id = $1",
    )
    .bind(charge.charge_id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(posted, 1, "publication posted to the ledger");
    assert!(matches!(
        state.finance.student_dues(&scope, dues).await.unwrap(),
        FinanceEvidence::Outstanding { .. }
    ));
    let bad = state
        .fees
        .raise_exam_fee(
            &scope,
            ExamFeeRequest {
                source_id: uuid::Uuid::now_v7(),
                ..fee("-1")
            },
        )
        .await;
    assert!(matches!(bad, Err(KernelError::Validation(_))), "{bad:?}");
}
