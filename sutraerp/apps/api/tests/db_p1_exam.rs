//! Part 1 Stage 2 — Domain I (exams) regression DB tests, real Postgres (`db-tests`):
//!
//! * `release_after_approval_needs_a_release_decision` — DEF-I-003: a withheld result released
//!   after the batch is approved with changed content waits in `validated` for an explicit
//!   release decision (`result_decision.kind = release`) and is never published before it;
//!   rejection withholds it again; approval makes it publishable (by a new publication —
//!   DEF-I-010).
//! * `open_malpractice_cases_withhold_the_result` — DEF-I-004: a reported / under-investigation /
//!   hearing / appealed case withholds the computed result; a case opened after approval keeps
//!   the result out of the publication (withheld at publish time).
//! * `cgpa_and_completion_across_terms_and_supplementary` — DEF-I-005: regular term 1, term 2,
//!   then a supplementary for the failed term-1 course: hand-computed SGPA / CGPA, the
//!   supplementary replaces only that course, no credit double-counted; completion credits /
//!   CGPA agree.
//! * `pass_mark_is_compared_on_the_exact_percentage` — DEF-I-008: 39.995 % is displayed 40.00
//!   but fails a 40 % paper.
//! * `publication_waits_for_its_date_and_publishes_its_frozen_set` — DEF-I-009 (scheduled date
//!   enforced) and DEF-I-010 (a result revised after authorisation is not published by it).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use chrono::NaiveDate;
use std::sync::Arc;
use support::b6::{self, dec, ExamSetup};
use support::*;
use sutra_contracts::exams::ids::{ExamPaperId, ExamSessionId};
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_contracts::exams::status::{
    AttemptKind, ExamRegistrationStatus, RegistrationChannel, ResultBatchStatus, ResultOutcome,
    ResultStatus,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_exams::completion::repo as cr;
use sutra_exams::results::repo as rr;
use sutra_exams::shared::approval::ApprovalMode;
use sutra_exams::shared::marks::{self, MarkValues};
use sutra_exams::shared::registration::{
    insert_registration, NewRegistration, NewRegistrationPaper,
};
use sutra_exams::shared::results::ResultRow;
use sutra_exams::shared::{Actor, ExamsState};
use sutra_kernel::error::KernelError;
use uuid::Uuid;

const TENANT_REL: &str = "0192f5a0-7c3e-7000-8000-0000000e1a01";
const TENANT_MAL: &str = "0192f5a0-7c3e-7000-8000-0000000e1a02";
const TENANT_CGPA: &str = "0192f5a0-7c3e-7000-8000-0000000e1a03";
const TENANT_ROUND: &str = "0192f5a0-7c3e-7000-8000-0000000e1a04";
const TENANT_PUB: &str = "0192f5a0-7c3e-7000-8000-0000000e1a05";

fn maker() -> Actor {
    Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    }
}

fn approver() -> Actor {
    Actor {
        id: Some("dev|approver@sutra.dev".into()),
        correlation_id: None,
    }
}

/// Exams state with explicit decisions (independent of the production approval mode).
fn awaiting(pool: &sqlx::PgPool) -> ExamsState {
    let mut st = sutra_api::app::exams_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    st
}

async fn setup(tenant: &'static str, name: &str) -> (sqlx::PgPool, ExamSetup) {
    let (app, pool) = app(tenant).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &pool,
        tenant,
        name,
        Some(b4::AARAV),
        "active",
        "conducted",
    )
    .await;
    (pool, setup)
}

/// The single current result of a session.
async fn current(st: &ExamsState, setup: &ExamSetup, sid: ExamSessionId) -> ResultRow {
    let mut rows = rr::list_results(st, setup.tenant(), sid, None, false)
        .await
        .unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    rows.remove(0)
}

/// compute → validate (maker) → approve (approver) → the approved batch.
async fn approve_session(st: &ExamsState, setup: &ExamSetup, sid: ExamSessionId) {
    let t = setup.tenant();
    let b = rr::create_batch(st, t, sid, &maker()).await.unwrap();
    let (b, _) = rr::compute_batch(st, t, b.id, b.version, &maker())
        .await
        .unwrap();
    let b = rr::validate_batch(st, t, b.id, b.version, None, &maker())
        .await
        .unwrap();
    let b = rr::decide_batch(st, t, b.id, b.version, true, None, &approver())
        .await
        .unwrap();
    assert_eq!(b.status, ResultBatchStatus::Approved);
}

/// create → authorise (maker) → decide (approver): the authorised publication.
async fn authorised_publication(
    st: &ExamsState,
    setup: &ExamSetup,
    sid: ExamSessionId,
    scheduled_on: Option<NaiveDate>,
) -> rr::PublicationRow {
    let t = setup.tenant();
    let p = rr::create_publication(st, t, sid, scheduled_on, &maker())
        .await
        .unwrap();
    let p = rr::authorise_publication(st, t, p.id, p.version, &maker())
        .await
        .unwrap();
    let p = rr::decide_publication(st, t, p.id, p.version, true, None, &approver())
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "authorised");
    p
}

/// Approves and publishes a whole session; returns the published result.
async fn publish_session(st: &ExamsState, setup: &ExamSetup, sid: ExamSessionId) -> ResultRow {
    approve_session(st, setup, sid).await;
    let p = authorised_publication(st, setup, sid, None).await;
    let (_, sum) = rr::publish_publication(st, setup.tenant(), p.id, p.version, &maker())
        .await
        .unwrap();
    assert_eq!(sum.published + sum.revised, 1, "{sum:?}");
    let r = current(st, setup, sid).await;
    assert_eq!(r.status, ResultStatus::Published);
    r
}

/// A session of `kind` in academic term `term` (status `active`).
async fn session(pool: &sqlx::PgPool, setup: &ExamSetup, term: Uuid, kind: &str) -> ExamSessionId {
    let sid = b6::seed_session(pool, &setup.finance, setup.scheme_id, "active").await;
    sqlx::query("UPDATE exams.exam_session SET academic_term_id = $2, kind = $3 WHERE id = $1")
        .bind(sid.as_uuid())
        .bind(term)
        .bind(kind)
        .execute(pool)
        .await
        .unwrap();
    sid
}

/// A theory paper (max 100, pass 40 %, no internal weight) examining `component`.
async fn paper(
    pool: &sqlx::PgPool,
    setup: &ExamSetup,
    sid: ExamSessionId,
    term: Uuid,
    component: Uuid,
    code: &str,
    credits: &str,
) -> ExamPaperId {
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_paper (tenant_id, institution_id, session_id, offering_id, \
         curriculum_component_id, academic_term_id, paper_code, title, credits, mode, max_marks, \
         pass_percent, internal_weight, duration_minutes, status) \
         VALUES ($1, $2, $3, NULL, $4, $5, $6, $6, $7, 'theory', 100, 40, 0, 180, 'conducted') \
         RETURNING id",
    )
    .bind(setup.tenant().as_uuid())
    .bind(setup.institution().as_uuid())
    .bind(sid.as_uuid())
    .bind(component)
    .bind(term)
    .bind(code)
    .bind(dec(credits))
    .fetch_one(pool)
    .await
    .unwrap();
    ExamPaperId(id)
}

/// Registers the setup's student for `papers` with the given attempt kind.
async fn register(
    pool: &sqlx::PgPool,
    setup: &ExamSetup,
    sid: ExamSessionId,
    papers: &[ExamPaperId],
    attempt: AttemptKind,
    attempt_no: i32,
) -> CandidateSummary {
    let mut tx = pool.begin().await.unwrap();
    let reg = insert_registration(
        &mut tx,
        &NewRegistration {
            tenant: setup.tenant(),
            institution: setup.institution(),
            session_id: sid,
            student_id: StudentId(setup.student_id()),
            student_context_id: StudentContextId(setup.finance.student.context_id),
            student_number: setup.finance.student.student_number.clone(),
            display_name: "Aarav Seeded".into(),
            subject_ref: setup.subject_ref.clone(),
            channel: RegistrationChannel::Administrative,
            status: ExamRegistrationStatus::Confirmed,
            papers: papers
                .iter()
                .map(|p| NewRegistrationPaper {
                    paper_id: *p,
                    attempt_kind: attempt,
                    attempt_no,
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

fn rp(
    reg: &CandidateSummary,
    p: ExamPaperId,
) -> sutra_contracts::exams::ids::ExamRegistrationPaperId {
    reg.papers
        .iter()
        .find(|x| x.paper_id == p)
        .unwrap()
        .registration_paper_id
}

async fn seed_case(
    pool: &sqlx::PgPool,
    setup: &ExamSetup,
    reg: &CandidateSummary,
    number: &str,
    status: &str,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO exams.malpractice_case (tenant_id, institution_id, session_id, \
         registration_id, paper_id, student_id, case_number, allegation, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'unauthorised material', $8) RETURNING id",
    )
    .bind(setup.tenant().as_uuid())
    .bind(setup.institution().as_uuid())
    .bind(reg.session_id.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(reg.papers[0].paper_id.as_uuid())
    .bind(reg.student_id.as_uuid())
    .bind(number)
    .bind(status)
    .fetch_one(pool)
    .await
    .unwrap()
}

// ===================================================================== DEF-I-003

#[tokio::test]
async fn release_after_approval_needs_a_release_decision() {
    let (pool, setup) = setup(TENANT_REL, "Release University").await;
    let st = awaiting(&pool);
    let t = setup.tenant();
    let sid = setup.session_id;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        sid,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    let mark = b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("30", "40")),
        Some("60"),
    )
    .await;
    approve_session(&st, &setup, sid).await;
    let r = current(&st, &setup, sid).await;
    assert_eq!((r.status, r.revision), (ResultStatus::Approved, 1));
    assert_eq!(r.lines[0].percent, Some(dec("66.00")));

    // Withheld after approval; the final marks change while it is withheld.
    let w = rr::withhold_result(&st, t, r.id, r.version, "identity check pending", &maker())
        .await
        .unwrap();
    assert_eq!(w.status, ResultStatus::Withheld);
    {
        let mut tx = pool.begin().await.unwrap();
        let m = marks::lock_mark(&mut tx, t, mark.id).await.unwrap();
        let mut v = MarkValues::of(&m);
        v.external_marks = Some(dec("70"));
        marks::revise_final(&mut tx, t, &m, v, "correction", "re-totalled", &maker())
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    // Release: the changed recomputation is NOT approved; it waits for a release decision.
    let rel = rr::recompute_result(
        &st,
        t,
        w.id,
        w.version,
        true,
        Some("identity verified"),
        &maker(),
    )
    .await
    .unwrap();
    assert_eq!(
        (rel.status, rel.revision),
        (ResultStatus::Validated, 2),
        "{rel:?}"
    );
    // 30 + 70/100 × 60 = 72.
    assert_eq!(rel.lines[0].percent, Some(dec("72.00")));
    assert!(
        rel.status_reason
            .as_deref()
            .is_some_and(|s| s.contains("awaiting release approval")),
        "{rel:?}"
    );
    let pending = |pool: sqlx::PgPool, result: Uuid| async move {
        sqlx::query_as::<_, (Uuid, String, i64)>(
            "SELECT id, status, version FROM exams.result_decision WHERE result_id = $1 \
             AND kind = 'release' ORDER BY created_at DESC LIMIT 1",
        )
        .bind(result)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    let (did, dstatus, dver) = pending(pool.clone(), rel.id.as_uuid()).await;
    assert_eq!(dstatus, "pending");

    // A publication authorised now does not publish it (nothing approved in its frozen set).
    let p = authorised_publication(&st, &setup, sid, None).await;
    let early = rr::publish_publication(&st, t, p.id, p.version, &maker()).await;
    assert!(matches!(early, Err(KernelError::Conflict(_))), "{early:?}");
    let still = current(&st, &setup, sid).await;
    assert_eq!(still.status, ResultStatus::Validated);
    assert!(still.published_at.is_none());
    let p = rr::get_publication(&st, t, p.id).await.unwrap();
    rr::cancel_publication(&st, t, p.id, p.version, "release pending", &maker())
        .await
        .unwrap();

    // Rejected release → withheld again.
    let (d, back) = rr::decide_decision(
        &st,
        t,
        did,
        dver,
        false,
        Some("not verified".into()),
        &approver(),
    )
    .await
    .unwrap();
    assert_eq!(d.status, "rejected");
    assert_eq!(back.status, ResultStatus::Withheld);

    // Released again and approved → approved; a new publication publishes it.
    let rel = rr::recompute_result(&st, t, back.id, back.version, true, None, &maker())
        .await
        .unwrap();
    assert_eq!(rel.status, ResultStatus::Validated);
    let (did, _, dver) = pending(pool.clone(), rel.id.as_uuid()).await;
    let (d, ok) = rr::decide_decision(&st, t, did, dver, true, None, &approver())
        .await
        .unwrap();
    assert_eq!(d.status, "approved");
    assert_eq!(ok.status, ResultStatus::Approved);
    let p = authorised_publication(&st, &setup, sid, None).await;
    let (_, sum) = rr::publish_publication(&st, t, p.id, p.version, &maker())
        .await
        .unwrap();
    assert_eq!(sum.published, 1);
    let r = current(&st, &setup, sid).await;
    assert_eq!(r.status, ResultStatus::Published);
    assert_eq!(r.lines[0].percent, Some(dec("72.00")));
}

// ===================================================================== DEF-I-004

#[tokio::test]
async fn open_malpractice_cases_withhold_the_result() {
    let (pool, setup) = setup(TENANT_MAL, "Integrity Pending University").await;
    let st = awaiting(&pool);
    let t = setup.tenant();
    let sid = setup.session_id;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        sid,
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
    let case = seed_case(&pool, &setup, &reg, "MC-PEND-1", "reported").await;
    let b = rr::create_batch(&st, t, sid, &maker()).await.unwrap();
    let (mut b, sum) = rr::compute_batch(&st, t, b.id, b.version, &maker())
        .await
        .unwrap();
    assert_eq!(sum.withheld, 1, "{sum:?}");
    let r = current(&st, &setup, sid).await;
    assert_eq!(
        (r.status, r.outcome),
        (ResultStatus::Withheld, ResultOutcome::Withheld)
    );
    assert!(
        r.status_reason
            .as_deref()
            .is_some_and(|s| s.contains("malpractice case MC-PEND-1 pending")),
        "{r:?}"
    );
    // Still open while investigated, heard or appealed.
    for status in ["under_investigation", "hearing", "appealed", "under_review"] {
        sqlx::query("UPDATE exams.malpractice_case SET status = $2 WHERE id = $1")
            .bind(case)
            .bind(status)
            .execute(&pool)
            .await
            .unwrap();
        let (nb, _) = rr::compute_batch(&st, t, b.id, b.version, &maker())
            .await
            .unwrap();
        b = nb;
        let r = current(&st, &setup, sid).await;
        assert_eq!(r.status, ResultStatus::Withheld, "{status}: {r:?}");
    }
    let cur = current(&st, &setup, sid).await;
    let release = rr::recompute_result(&st, t, cur.id, cur.version, true, None, &maker()).await;
    assert!(
        matches!(release, Err(KernelError::Conflict(_))),
        "still blocked: {release:?}"
    );

    // Closed with no penalty → computed, approved.
    sqlx::query(
        "UPDATE exams.malpractice_case SET status = 'closed', penalty = 'none' WHERE id = $1",
    )
    .bind(case)
    .execute(&pool)
    .await
    .unwrap();
    let (b, sum) = rr::compute_batch(&st, t, b.id, b.version, &maker())
        .await
        .unwrap();
    assert_eq!(sum.computed, 1, "{sum:?}");
    let b = rr::validate_batch(&st, t, b.id, b.version, None, &maker())
        .await
        .unwrap();
    rr::decide_batch(&st, t, b.id, b.version, true, None, &approver())
        .await
        .unwrap();
    assert_eq!(
        current(&st, &setup, sid).await.status,
        ResultStatus::Approved
    );

    // A new case reported after approval keeps the result out of the publication.
    let p = authorised_publication(&st, &setup, sid, None).await;
    seed_case(&pool, &setup, &reg, "MC-PEND-2", "reported").await;
    let (_, sum) = rr::publish_publication(&st, t, p.id, p.version, &maker())
        .await
        .unwrap();
    assert_eq!((sum.published, sum.skipped), (0, 1), "{sum:?}");
    let r = current(&st, &setup, sid).await;
    assert_eq!(r.status, ResultStatus::Withheld);
    assert!(r.published_at.is_none());
    assert!(
        r.status_reason
            .as_deref()
            .is_some_and(|s| s.contains("MC-PEND-2")),
        "{r:?}"
    );
}

// ===================================================================== DEF-I-005

#[tokio::test]
async fn cgpa_and_completion_across_terms_and_supplementary() {
    let (pool, setup) = setup(TENANT_CGPA, "Cumulative University").await;
    let st = awaiting(&pool);
    let term1 = setup.term_id();
    let term2 = Uuid::now_v7();
    let (ca, cb, cc) = (Uuid::now_v7(), Uuid::now_v7(), Uuid::now_v7());

    // Term 1 regular: A 4 cr 92 % → O (10); B 3 cr 35 % → F (0).
    let s1 = session(&pool, &setup, term1, "regular").await;
    let pa = paper(&pool, &setup, s1, term1, ca, "PA-101", "4").await;
    let pb = paper(&pool, &setup, s1, term1, cb, "PB-101", "3").await;
    let reg1 = register(&pool, &setup, s1, &[pa, pb], AttemptKind::Regular, 1).await;
    b6::seed_final_marks(&pool, &setup, rp(&reg1, pa), None, Some("92")).await;
    b6::seed_final_marks(&pool, &setup, rp(&reg1, pb), None, Some("35")).await;
    let r1 = publish_session(&st, &setup, s1).await;
    // SGPA = 40 / 7 = 5.714… → 5.71; CGPA the same.
    assert_eq!(r1.sgpa, Some(dec("5.71")));
    assert_eq!(r1.cgpa, Some(dec("5.71")));
    assert_eq!(r1.credits_earned, dec("4.00"));
    assert_eq!(r1.outcome, ResultOutcome::Fail);

    // Term 2 regular: C 4 cr 75 % → A (8).
    let s2 = session(&pool, &setup, term2, "regular").await;
    let pc = paper(&pool, &setup, s2, term2, cc, "PC-201", "4").await;
    let reg2 = register(&pool, &setup, s2, &[pc], AttemptKind::Regular, 1).await;
    b6::seed_final_marks(&pool, &setup, rp(&reg2, pc), None, Some("75")).await;
    let r2 = publish_session(&st, &setup, s2).await;
    // SGPA 8.00; CGPA = (40 + 0 + 32) / (4 + 3 + 4) = 72 / 11 = 6.545… → 6.55.
    assert_eq!(r2.sgpa, Some(dec("8.00")));
    assert_eq!(r2.cgpa, Some(dec("6.55")));

    // Supplementary (term 1) for B only: 62 % → B+ (7).
    let s3 = session(&pool, &setup, term1, "supplementary").await;
    let pb2 = paper(&pool, &setup, s3, term1, cb, "PB-101S", "3").await;
    let reg3 = register(&pool, &setup, s3, &[pb2], AttemptKind::Supplementary, 2).await;
    b6::seed_final_marks(&pool, &setup, rp(&reg3, pb2), None, Some("62")).await;
    let r3 = publish_session(&st, &setup, s3).await;
    // SGPA of the supplementary result = 7.00. CGPA: B's F is replaced by B+, A and C keep
    // their grades, every course counted once: (40 + 21 + 32) / 11 = 93 / 11 = 8.4545… → 8.45.
    // (Replacing the whole term-1 result would give (21 + 32) / 7 = 7.57; adding the
    // supplementary on top would give (72 + 21) / 14 = 6.64.)
    assert_eq!(r3.sgpa, Some(dec("7.00")));
    assert_eq!(r3.cgpa, Some(dec("8.45")));
    assert_eq!(r3.credits_attempted, dec("3.00"));

    // Completion: the same cumulative standing (all three courses passed, 11 credits).
    let ce = cr::evaluate_completion(
        &st,
        setup.tenant(),
        setup.institution(),
        setup.student_id(),
        &maker(),
    )
    .await
    .unwrap();
    assert_eq!(ce.credits_earned, dec("11.00"), "{ce:?}");
    assert_eq!(ce.cgpa, Some(dec("8.45")), "{ce:?}");
    assert_eq!(ce.evidence["terms"].as_array().unwrap().len(), 3, "{ce:?}");
}

// ===================================================================== DEF-I-008

#[tokio::test]
async fn pass_mark_is_compared_on_the_exact_percentage() {
    let (pool, setup) = setup(TENANT_ROUND, "Rounding University").await;
    let st = awaiting(&pool);
    let t = setup.tenant();
    let sid = setup.session_id;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        sid,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    // Setup paper: 40 % internal weight. 19.99/80 × 40 + 50/100 × 60 = 9.995 + 30 = 39.995 %.
    b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("19.99", "80")),
        Some("50"),
    )
    .await;
    let b = rr::create_batch(&st, t, sid, &maker()).await.unwrap();
    rr::compute_batch(&st, t, b.id, b.version, &maker())
        .await
        .unwrap();
    let r = current(&st, &setup, sid).await;
    let l = &r.lines[0];
    assert_eq!(l.percent, Some(dec("40.00")), "displayed rounded: {r:?}");
    assert_eq!(l.grade.as_deref(), Some("F"), "{r:?}");
    assert!(!l.passed, "39.995 % must not pass a 40 % paper: {r:?}");
    assert_eq!(r.outcome, ResultOutcome::Fail);
    assert_eq!(r.credits_earned, dec("0.00"));
}

// ===================================================================== DEF-I-009 / 010

#[tokio::test]
async fn publication_waits_for_its_date_and_publishes_its_frozen_set() {
    let (pool, setup) = setup(TENANT_PUB, "Schedule University").await;
    let st = awaiting(&pool);
    let t = setup.tenant();
    let sid = setup.session_id;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        sid,
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
    approve_session(&st, &setup, sid).await;

    // DEF-I-009: scheduled in the future → refused, even when authorised.
    let future = NaiveDate::from_ymd_opt(2099, 1, 1).unwrap();
    let p = authorised_publication(&st, &setup, sid, Some(future)).await;
    let early = rr::publish_publication(&st, t, p.id, p.version, &maker()).await;
    match early {
        Err(KernelError::Conflict(m)) => assert!(m.contains("scheduled"), "{m}"),
        other => panic!("expected a scheduling conflict, got {other:?}"),
    }
    assert_eq!(
        current(&st, &setup, sid).await.status,
        ResultStatus::Approved
    );

    // DEF-I-010: a correction approved after authorisation creates a new approved revision; the
    // publication publishes only its frozen set (the superseded revision → skipped).
    let r = current(&st, &setup, sid).await;
    let (d, r2) = rr::request_decision(
        &st,
        t,
        r.id,
        r.version,
        Some(rr::CorrectionRequest {
            paper_id: setup.paper_id,
            internal_marks: None,
            external_marks: Some(dec("70")),
            grace_marks: None,
            reason: "totalling error".into(),
        }),
        None,
        &maker(),
    )
    .await
    .unwrap();
    assert_eq!(r2.id, r.id, "pending: unchanged");
    let (_, r2) = rr::decide_decision(&st, t, d.id, d.version, true, None, &approver())
        .await
        .unwrap();
    assert_eq!((r2.status, r2.revision), (ResultStatus::Approved, 2));

    // The scheduled date arrives.
    sqlx::query("UPDATE exams.publication SET scheduled_on = current_date WHERE id = $1")
        .bind(p.id)
        .execute(&pool)
        .await
        .unwrap();
    let p = rr::get_publication(&st, t, p.id).await.unwrap();
    let (p, sum) = rr::publish_publication(&st, t, p.id, p.version, &maker())
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "published");
    assert_eq!(
        (sum.published, sum.revised, sum.skipped),
        (0, 0, 1),
        "{sum:?}"
    );
    let now = current(&st, &setup, sid).await;
    assert_eq!(
        (now.status, now.revision),
        (ResultStatus::Approved, 2),
        "not in the frozen set"
    );

    // A new publication (frozen now) publishes the revision.
    let p2 = authorised_publication(&st, &setup, sid, None).await;
    let (_, sum) = rr::publish_publication(&st, t, p2.id, p2.version, &maker())
        .await
        .unwrap();
    assert_eq!(sum.published, 1, "{sum:?}");
    assert_eq!(
        current(&st, &setup, sid).await.status,
        ResultStatus::Published
    );
}
