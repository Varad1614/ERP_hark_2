//! Persistence + use cases of slice 6C results: computation batches (L3-I-083 … 100),
//! verification / approval (`RESULT_APPROVAL`), per-result governance (withhold / release,
//! recompute, approved correction `RESULT_CORRECTION`, withdrawal `RESULT_WITHDRAWAL` /
//! restoration), publications (`RESULT_PUBLICATION`; publish, delay / resume, cancel, failure
//! recovery, republication — L3-I-101 … 117) and grade cards.
//!
//! `term_result(_line)` are written only through `shared::results`; final marks only through
//! `shared::marks::revise_final`; session status never here.

use super::common::{ensure_version, history, status_payload};
use super::domain::{
    batch_approved, batch_can_transition, batch_recomputable, compute_candidate,
    publication_can_transition, same_content, CandidateComputation, DecidedPenalty,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::grading::{cumulative, fmt2, AttemptLine};
use crate::shared::guard::{require_institution_writable, require_operational};
use crate::shared::lifecycle::StatusChange;
use crate::shared::marks::{lock_mark, revise_final, MarkValues};
use crate::shared::numbering::{self, kind};
use crate::shared::publish::{audit, event};
use crate::shared::results::{
    change_result_status, insert_result, load_result, NewResult, NewResultLine, ResultRow,
};
use crate::shared::rows::{
    decode, lock_registration, mark_from_row, paper_from_row, MARK_COLUMNS, PAPER_COLUMNS,
};
use crate::shared::{Actor, ExamsState};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::exams::events::results::{
    GRADE_CARD_ISSUED, GRADE_CARD_ISSUED_SCHEMA, PUBLICATION_DELAYED, PUBLICATION_DELAYED_SCHEMA,
    RESULT_APPROVED, RESULT_APPROVED_SCHEMA, RESULT_COMPUTED, RESULT_COMPUTED_SCHEMA,
    RESULT_PUBLISHED, RESULT_PUBLISHED_SCHEMA, RESULT_REVISED, RESULT_REVISED_SCHEMA,
    RESULT_WITHDRAWN, RESULT_WITHDRAWN_SCHEMA, RESULT_WITHHELD, RESULT_WITHHELD_SCHEMA,
};
use sutra_contracts::exams::events::{ResultPublishedV1, StatusChangedV1};
use sutra_contracts::exams::ids::{ExamPaperId, ExamSessionId, TermResultId};
use sutra_contracts::exams::ports::{
    CandidateSummary, ExamPaperSummary, ExamSessionSummary, GradeBand, PaperMarkSummary,
};
use sutra_contracts::exams::status::{
    GradeCardStatus, MalpracticePenalty, PublicationStatus, ResultBatchStatus, ResultStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

// =================================================================== rows

/// A result computation batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub status: ResultBatchStatus,
    pub status_reason: Option<String>,
    pub run_count: i32,
    pub candidates: i32,
    pub computed_count: i32,
    pub withheld_count: i32,
    pub unchanged_count: i32,
    pub computed_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

const BATCH_COLUMNS: &str = "id, institution_id, session_id, status, status_reason, run_count, \
    candidates, computed_count, withheld_count, unchanged_count, computed_at, \
    process_instance_id, version";

fn batch_from_row(r: &PgRow) -> Result<BatchRow, sqlx::Error> {
    Ok(BatchRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        status: decode(r.try_get("status")?, ResultBatchStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        run_count: r.try_get("run_count")?,
        candidates: r.try_get("candidates")?,
        computed_count: r.try_get("computed_count")?,
        withheld_count: r.try_get("withheld_count")?,
        unchanged_count: r.try_get("unchanged_count")?,
        computed_at: r.try_get("computed_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

/// A publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub status: PublicationStatus,
    pub status_reason: Option<String>,
    pub scheduled_on: Option<NaiveDate>,
    pub delayed_until: Option<NaiveDate>,
    pub authorised_at: Option<DateTime<Utc>>,
    pub published_at: Option<DateTime<Utc>>,
    pub attempts: i32,
    pub published_count: i32,
    pub skipped_count: i32,
    pub last_error: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

const PUBLICATION_COLUMNS: &str = "id, institution_id, session_id, status, status_reason, \
    scheduled_on, delayed_until, authorised_at, published_at, attempts, published_count, \
    skipped_count, last_error, process_instance_id, version";

fn publication_from_row(r: &PgRow) -> Result<PublicationRow, sqlx::Error> {
    Ok(PublicationRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        status: decode(r.try_get("status")?, PublicationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        scheduled_on: r.try_get("scheduled_on")?,
        delayed_until: r.try_get("delayed_until")?,
        authorised_at: r.try_get("authorised_at")?,
        published_at: r.try_get("published_at")?,
        attempts: r.try_get("attempts")?,
        published_count: r.try_get("published_count")?,
        skipped_count: r.try_get("skipped_count")?,
        last_error: r.try_get("last_error")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

/// A grade card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradeCardRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub registration_id: Uuid,
    pub result_id: TermResultId,
    pub student_id: Uuid,
    pub subject_ref: Option<String>,
    pub card_number: String,
    pub status: GradeCardStatus,
    pub status_reason: Option<String>,
    pub supersedes_id: Option<Uuid>,
    pub issued_at: DateTime<Utc>,
    pub version: i64,
}

const CARD_COLUMNS: &str = "id, institution_id, session_id, registration_id, result_id, \
    student_id, subject_ref, card_number, status, status_reason, supersedes_id, issued_at, version";

fn card_from_row(r: &PgRow) -> Result<GradeCardRow, sqlx::Error> {
    Ok(GradeCardRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        registration_id: r.try_get("registration_id")?,
        result_id: TermResultId(r.try_get("result_id")?),
        student_id: r.try_get("student_id")?,
        subject_ref: r.try_get("subject_ref")?,
        card_number: r.try_get("card_number")?,
        status: decode(r.try_get("status")?, GradeCardStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        supersedes_id: r.try_get("supersedes_id")?,
        issued_at: r.try_get("issued_at")?,
        version: r.try_get("version")?,
    })
}

/// A governed decision on one result (correction / withdrawal).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultDecisionRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub result_id: TermResultId,
    pub kind: String,
    pub paper_id: Option<Uuid>,
    pub internal_marks: Option<Decimal>,
    pub external_marks: Option<Decimal>,
    pub grace_marks: Option<Decimal>,
    pub reason: String,
    pub status: String,
    pub decision_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

const DECISION_COLUMNS: &str = "id, institution_id, session_id, result_id, kind, paper_id, \
    internal_marks, external_marks, grace_marks, reason, status, decision_reason, \
    process_instance_id, version";

fn decision_from_row(r: &PgRow) -> Result<ResultDecisionRow, sqlx::Error> {
    Ok(ResultDecisionRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        result_id: TermResultId(r.try_get("result_id")?),
        kind: r.try_get("kind")?,
        paper_id: r.try_get("paper_id")?,
        internal_marks: r.try_get("internal_marks")?,
        external_marks: r.try_get("external_marks")?,
        grace_marks: r.try_get("grace_marks")?,
        reason: r.try_get("reason")?,
        status: r.try_get("status")?,
        decision_reason: r.try_get("decision_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

// =================================================================== shared reads

fn not_found(entity: &'static str) -> KernelError {
    KernelError::NotFound { entity }
}

/// The session (404) of a tenant.
pub async fn session_of(
    st: &ExamsState,
    tenant: TenantId,
    id: ExamSessionId,
) -> Result<ExamSessionSummary, KernelError> {
    st.directory
        .session(&ExecutionScope::tenant(tenant), id)
        .await?
        .ok_or(not_found("examination session"))
}

/// Grade bands of the session's scheme (409 when the session has none).
pub async fn bands_of(
    st: &ExamsState,
    scope: &ExecutionScope,
    session: &ExamSessionSummary,
) -> Result<Vec<GradeBand>, KernelError> {
    let id = session.grading_scheme_id.ok_or_else(|| {
        KernelError::Conflict(format!(
            "examination session {} has no grading scheme",
            session.code
        ))
    })?;
    Ok(st
        .directory
        .grading_scheme(scope, id)
        .await?
        .ok_or_else(|| KernelError::Conflict("the session's grading scheme is missing".into()))?
        .bands)
}

/// Papers of a session read inside the caller's transaction.
pub async fn papers_in_tx(
    conn: &mut PgConnection,
    tenant: TenantId,
    session: ExamSessionId,
) -> Result<HashMap<ExamPaperId, ExamPaperSummary>, KernelError> {
    let rows = sqlx::query(&format!(
        "SELECT {PAPER_COLUMNS} FROM exams.exam_paper WHERE tenant_id = $1 AND session_id = $2"
    ))
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut m = HashMap::new();
    for r in &rows {
        let p = paper_from_row(r).map_err(map_db_error)?;
        m.insert(p.id, p);
    }
    Ok(m)
}

/// A registration's mark rows read inside the caller's transaction (sees revised marks).
pub async fn marks_in_tx(
    conn: &mut PgConnection,
    tenant: TenantId,
    registration: Uuid,
) -> Result<Vec<PaperMarkSummary>, KernelError> {
    sqlx::query(&format!(
        "SELECT {MARK_COLUMNS} FROM exams.paper_mark WHERE tenant_id = $1 AND registration_id = $2"
    ))
    .bind(tenant.as_uuid())
    .bind(registration)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(mark_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// Decided malpractice penalties of a registration: a read-only read of 6B's
/// `exams.malpractice_case` (contract §7). A case counts while it carries an effective decision
/// (`decided | upheld | modified`) and also after it is closed / archived with that decision in
/// place; an overturned finding stores the penalty `none`, a case reopened for investigation is
/// not counted until it is decided again, and duplicates merged into another case are skipped.
pub async fn decided_penalties(
    conn: &mut PgConnection,
    tenant: TenantId,
    registration: Uuid,
) -> Result<Vec<DecidedPenalty>, KernelError> {
    let rows: Vec<(Option<Uuid>, String, Option<Decimal>)> = sqlx::query_as(
        "SELECT paper_id, penalty, marks_deduction FROM exams.malpractice_case \
         WHERE tenant_id = $1 AND registration_id = $2 AND penalty IS NOT NULL \
         AND merged_into_id IS NULL \
         AND status IN ('decided', 'upheld', 'modified', 'closed', 'archived') \
         ORDER BY created_at",
    )
    .bind(tenant.as_uuid())
    .bind(registration)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.into_iter()
        .map(|(paper, penalty, deduction)| {
            let penalty = MalpracticePenalty::parse(&penalty).ok_or_else(|| {
                KernelError::internal_msg(format!("unknown malpractice penalty `{penalty}`"))
            })?;
            Ok(DecidedPenalty {
                paper_id: paper.map(ExamPaperId),
                penalty,
                marks_deduction: deduction,
            })
        })
        .collect()
}

/// Open malpractice cases of a registration that must withhold its result (contract §1 6C
/// "pending malpractice", DEF-I-004): a case still being established (`reported |
/// under_investigation | hearing`) or whose decision is under appeal (`appealed |
/// under_review`). Duplicates merged into another case are skipped. Returns the case numbers.
pub async fn open_malpractice_cases(
    conn: &mut PgConnection,
    tenant: TenantId,
    registration: Uuid,
) -> Result<Vec<String>, KernelError> {
    sqlx::query_scalar(
        "SELECT case_number FROM exams.malpractice_case \
         WHERE tenant_id = $1 AND registration_id = $2 AND merged_into_id IS NULL \
         AND status IN ('reported', 'under_investigation', 'hearing', 'appealed', 'under_review') \
         ORDER BY created_at",
    )
    .bind(tenant.as_uuid())
    .bind(registration)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// `(first published, component, credits, grade point, passed)` of one published line.
type AttemptRow = (Option<DateTime<Utc>>, Uuid, Decimal, Option<Decimal>, bool);

/// Every graded line of the student's current **published** results in sessions other than
/// `except_session`, oldest attempt first, each with the time the student's result in that
/// session was first published (sessions are ordered by it) — the input of [`cumulative`].
pub async fn published_attempts(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    student: StudentId,
    except_session: Option<ExamSessionId>,
) -> Result<Vec<(Option<DateTime<Utc>>, AttemptLine)>, KernelError> {
    let rows: Vec<AttemptRow> = sqlx::query_as(
        "SELECT fp.first_published, l.curriculum_component_id, l.credits, l.grade_point, \
             l.passed FROM exams.term_result r \
             JOIN exams.term_result_line l ON l.result_id = r.id \
             CROSS JOIN LATERAL ( \
                 SELECT min(f.published_at) AS first_published FROM exams.term_result f \
                 WHERE f.tenant_id = r.tenant_id AND f.session_id = r.session_id \
                 AND f.student_id = r.student_id AND f.published_at IS NOT NULL) fp \
             WHERE r.tenant_id = $1 AND r.student_id = $2 AND r.status = 'published' \
             AND ($3::uuid IS NULL OR r.session_id <> $3) \
             AND ($4::uuid IS NULL OR r.institution_id = $4) \
             ORDER BY fp.first_published NULLS LAST, r.session_id, l.line_no",
    )
    .bind(tenant.as_uuid())
    .bind(student.as_uuid())
    .bind(except_session.map(|s| s.as_uuid()))
    .bind(institution.map(|i| i.as_uuid()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(|(at, component, credits, grade_point, passed)| {
            (
                at,
                AttemptLine {
                    component,
                    credits,
                    grade_point,
                    passed,
                },
            )
        })
        .collect())
}

/// CGPA under the grade-replacement policy ([`cumulative`], DEF-I-005): every published
/// attempt of the student in *other* sessions plus this session's computed lines, in attempt
/// order. A session not yet published for the student is the newest attempt; one already
/// published (a post-publication correction / revaluation) keeps its original place, so a
/// correction of the regular session never overtakes a later supplementary. A supplementary
/// result replaces only the course(s) it re-examined; no credit is counted twice.
pub async fn cgpa_with(
    conn: &mut PgConnection,
    tenant: TenantId,
    student: StudentId,
    session: ExamSessionId,
    this_session: &[NewResultLine],
) -> Result<Option<Decimal>, KernelError> {
    let others = published_attempts(conn, tenant, None, student, Some(session)).await?;
    let first_published: Option<DateTime<Utc>> = sqlx::query_scalar(
        "SELECT min(published_at) FROM exams.term_result WHERE tenant_id = $1 \
         AND session_id = $2 AND student_id = $3 AND published_at IS NOT NULL",
    )
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .bind(student.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let split = match first_published {
        None => others.len(),
        Some(at) => others.partition_point(|(t, _)| t.is_some_and(|t| t <= at)),
    };
    let mut attempts: Vec<AttemptLine> = Vec::with_capacity(others.len() + this_session.len());
    attempts.extend(others[..split].iter().map(|(_, l)| l.clone()));
    attempts.extend(this_session.iter().map(|l| AttemptLine {
        component: l.curriculum_component_id,
        credits: l.credits,
        grade_point: l.grade_point,
        passed: l.passed,
    }));
    attempts.extend(others[split..].iter().map(|(_, l)| l.clone()));
    Ok(cumulative(&attempts).cgpa)
}

/// The current result of a candidate in a session (locked).
pub async fn current_result(
    conn: &mut PgConnection,
    tenant: TenantId,
    session: ExamSessionId,
    student: StudentId,
) -> Result<Option<ResultRow>, KernelError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.term_result WHERE tenant_id = $1 AND session_id = $2 \
         AND student_id = $3 AND status NOT IN ('superseded', 'archived') FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .bind(student.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match id {
        Some(id) => Ok(Some(
            load_result(conn, tenant, TermResultId(id), true).await?,
        )),
        None => Ok(None),
    }
}

/// Computation inputs read for one candidate.
pub struct Computed {
    pub computation: CandidateComputation,
    pub cgpa: Option<Decimal>,
}

/// Computes one candidate inside the caller's transaction.
pub async fn compute_in_tx(
    conn: &mut PgConnection,
    tenant: TenantId,
    session: &ExamSessionSummary,
    papers: &HashMap<ExamPaperId, ExamPaperSummary>,
    bands: &[GradeBand],
    reg: &CandidateSummary,
) -> Result<Computed, KernelError> {
    let marks = marks_in_tx(conn, tenant, reg.registration_id.as_uuid()).await?;
    let penalties = decided_penalties(conn, tenant, reg.registration_id.as_uuid()).await?;
    let open_cases = open_malpractice_cases(conn, tenant, reg.registration_id.as_uuid()).await?;
    let computation = compute_candidate(reg, papers, &marks, bands, &penalties, &open_cases);
    let cgpa = if computation.withheld.is_empty() {
        cgpa_with(conn, tenant, reg.student_id, session.id, &computation.lines).await?
    } else {
        None
    };
    Ok(Computed { computation, cgpa })
}

fn result_payload(r: &ResultRow) -> ResultPublishedV1 {
    ResultPublishedV1 {
        result_id: r.id.as_uuid(),
        institution_id: r.institution_id.as_uuid(),
        session_id: r.session_id.as_uuid(),
        student_id: r.student_id.as_uuid(),
        academic_term_id: r.academic_term_id,
        sgpa: r.sgpa.map(fmt2),
        cgpa: r.cgpa.map(fmt2),
        credits_attempted: fmt2(r.credits_attempted),
        credits_earned: fmt2(r.credits_earned),
        outcome: r.outcome.as_str().to_owned(),
        revision: r.revision,
        supersedes_result_id: r.supersedes_id.map(|s| s.as_uuid()),
        student_subject_ref: r.subject_ref.clone(),
    }
}

fn result_status_payload(
    r: &ResultRow,
    from: Option<ResultStatus>,
    reason: Option<String>,
    pid: Option<String>,
) -> StatusChangedV1 {
    status_payload(
        r.id.as_uuid(),
        r.institution_id,
        Some(r.session_id.as_uuid()),
        Some(r.registration_id.as_uuid()),
        Some(r.student_id.as_uuid()),
        from.map(|f| f.as_str()),
        r.status.as_str(),
        r.version,
        reason,
        pid,
        r.subject_ref.clone(),
    )
}

fn new_result<'a>(
    tenant: TenantId,
    reg: &'a CandidateSummary,
    session: &ExamSessionSummary,
    c: &Computed,
    batch: Option<Uuid>,
) -> Result<NewResult<'a>, KernelError> {
    let scheme = session.grading_scheme_id.ok_or_else(|| {
        KernelError::Conflict(format!(
            "examination session {} has no grading scheme",
            session.code
        ))
    })?;
    let withheld = c.computation.withheld_reason();
    Ok(NewResult {
        tenant,
        registration: reg,
        academic_term_id: session.academic_term_id,
        grading_scheme_id: scheme,
        credits_attempted: c.computation.aggregate.credits_attempted,
        credits_earned: c.computation.aggregate.credits_earned,
        grade_points: c.computation.aggregate.grade_points,
        sgpa: c.computation.aggregate.sgpa,
        cgpa: c.cgpa,
        outcome: c.computation.outcome,
        status: if withheld.is_some() {
            ResultStatus::Withheld
        } else {
            ResultStatus::Computed
        },
        status_reason: withheld,
        result_batch_id: batch,
        lines: c.computation.lines.clone(),
    })
}

/// What a computation did to one candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoreOutcome {
    Computed(TermResultId),
    Withheld(TermResultId),
    Unchanged(TermResultId),
    /// Approved / published results change only through governed steps.
    Skipped(TermResultId),
}

/// Computes and stores one candidate (batch run): idempotent (an unchanged result keeps its
/// revision), approved / published results are left to the governed paths.
#[allow(clippy::too_many_arguments)]
async fn compute_and_store(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    session: &ExamSessionSummary,
    papers: &HashMap<ExamPaperId, ExamPaperSummary>,
    bands: &[GradeBand],
    reg: &CandidateSummary,
    batch: Uuid,
    actor: &Actor,
) -> Result<StoreOutcome, KernelError> {
    let current = current_result(conn, tenant, session.id, reg.student_id).await?;
    if let Some(cur) = &current {
        if matches!(
            cur.status,
            ResultStatus::Approved | ResultStatus::Published | ResultStatus::Withdrawn
        ) {
            return Ok(StoreOutcome::Skipped(cur.id));
        }
    }
    let c = compute_in_tx(conn, tenant, session, papers, bands, reg).await?;
    let withheld = !c.computation.withheld.is_empty();
    if let Some(cur) = &current {
        let same_state = (cur.status == ResultStatus::Withheld) == withheld
            && cur.status != ResultStatus::Pending;
        if same_state
            && same_content(cur, &c.computation, c.cgpa)
            && cur.status_reason
                == c.computation
                    .withheld_reason()
                    .or(cur.status_reason.clone())
        {
            return Ok(StoreOutcome::Unchanged(cur.id));
        }
    }
    let nr = new_result(tenant, reg, session, &c, Some(batch))?;
    let r = insert_result(conn, &nr, RESULT_COMPUTED, actor).await?;
    if withheld {
        event(
            conn,
            scope,
            RESULT_WITHHELD,
            RESULT_WITHHELD_SCHEMA,
            r.id.to_string(),
            &result_status_payload(&r, None, r.status_reason.clone(), None),
            actor,
        )
        .await?;
        Ok(StoreOutcome::Withheld(r.id))
    } else {
        Ok(StoreOutcome::Computed(r.id))
    }
}

/// Walks a freshly inserted `computed` revision to `target` (governed revision after approval).
async fn walk_to(
    conn: &mut PgConnection,
    tenant: TenantId,
    mut r: ResultRow,
    target: ResultStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let path: &[ResultStatus] = match target {
        ResultStatus::Validated => &[ResultStatus::Validated],
        ResultStatus::Approved | ResultStatus::Withdrawn => {
            &[ResultStatus::Validated, ResultStatus::Approved]
        }
        ResultStatus::Published => &[
            ResultStatus::Validated,
            ResultStatus::Approved,
            ResultStatus::Published,
        ],
        _ => &[],
    };
    for to in path {
        r = change_result_status(conn, tenant, &r, *to, change, actor).await?;
    }
    Ok(r)
}

/// Replaces `old` by a recomputed revision at the same level (pre-publication replacement,
/// post-publication change → `term_result.revised`, revaluation outcome; L3-I-088/100/107/111/118).
/// Returns `old` unchanged when the recomputation gives the same result.
#[allow(clippy::too_many_arguments)]
pub async fn revise_result(
    st: &ExamsState,
    conn: &mut PgConnection,
    tenant: TenantId,
    old: &ResultRow,
    cause: &'static str,
    reason: &str,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let scope = ExecutionScope::institution(tenant, old.institution_id);
    let session = session_of(st, tenant, old.session_id).await?;
    let bands = bands_of(st, &scope, &session).await?;
    let papers = papers_in_tx(conn, tenant, session.id).await?;
    let reg = lock_registration(conn, tenant, old.registration_id).await?;
    let c = compute_in_tx(conn, tenant, &session, &papers, &bands, &reg).await?;
    if let Some(w) = c.computation.withheld_reason() {
        return Err(KernelError::Conflict(format!(
            "the result cannot be recomputed: {w}"
        )));
    }
    if same_content(old, &c.computation, c.cgpa) {
        return Ok(old.clone());
    }
    let nr = new_result(tenant, &reg, &session, &c, None)?;
    let fresh = insert_result(conn, &nr, cause, actor).await?;
    let change = StatusChange {
        cause_event_type: cause,
        reason: Some(reason),
        process_instance_id,
    };
    let r = walk_to(conn, tenant, fresh, old.status, &change, actor).await?;
    if old.status == ResultStatus::Published {
        event(
            conn,
            &scope,
            RESULT_REVISED,
            RESULT_REVISED_SCHEMA,
            r.id.to_string(),
            &result_payload(&r),
            actor,
        )
        .await?;
        withdraw_cards_of(
            conn,
            tenant,
            old.registration_id.as_uuid(),
            "result revised — reissue the grade card",
            actor,
        )
        .await?;
    }
    audit(
        conn,
        &scope,
        "exams.term_result.revise",
        "term_result",
        r.id.to_string(),
        json!({"supersedes": old.id, "revision": r.revision, "cause": cause, "reason": reason}),
        actor,
    )
    .await?;
    Ok(r)
}

// =================================================================== batches

async fn load_batch_any<'c, E>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<BatchRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let row = sqlx::query(&format!(
        "SELECT {BATCH_COLUMNS} FROM exams.result_batch WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(ex)
    .await
    .map_err(map_db_error)?
    .ok_or(not_found("result batch"))?;
    batch_from_row(&row).map_err(map_db_error)
}

/// One batch (404).
pub async fn get_batch(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
) -> Result<BatchRow, KernelError> {
    load_batch_any(&st.pool, tenant, id, false).await
}

/// Batches of a session, newest first.
pub async fn list_batches(
    st: &ExamsState,
    tenant: TenantId,
    session: ExamSessionId,
) -> Result<Vec<BatchRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {BATCH_COLUMNS} FROM exams.result_batch WHERE tenant_id = $1 AND session_id = $2 \
         ORDER BY created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(batch_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// The latest non-archived batch of a session.
async fn latest_batch(
    conn: &mut PgConnection,
    tenant: TenantId,
    session: ExamSessionId,
) -> Result<Option<BatchRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {BATCH_COLUMNS} FROM exams.result_batch WHERE tenant_id = $1 AND session_id = $2 \
         AND status <> 'archived' ORDER BY created_at DESC LIMIT 1"
    ))
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(|r| batch_from_row(&r))
    .transpose()
    .map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn set_batch_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    b: &BatchRow,
    to: ResultBatchStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<BatchRow, KernelError> {
    if !batch_can_transition(b.status, to) {
        return Err(KernelError::Conflict(format!(
            "result batch is {}; it cannot become {to}",
            b.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.result_batch SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $6 \
         RETURNING {BATCH_COLUMNS}"
    ))
    .bind(b.id)
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .bind(b.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("result batch changed concurrently".into()))?;
    history(
        conn,
        tenant,
        b.institution_id,
        "result_batch",
        b.id,
        Some(b.status.as_str()),
        to.as_str(),
        cause,
        reason,
        pid,
        actor,
    )
    .await?;
    batch_from_row(&row).map_err(map_db_error)
}

/// Opens the computation batch of a session (one live batch per session → 409, L3-I-093).
pub async fn create_batch(
    st: &ExamsState,
    tenant: TenantId,
    session_id: ExamSessionId,
    actor: &Actor,
) -> Result<BatchRow, KernelError> {
    let session = session_of(st, tenant, session_id).await?;
    let scope = ExecutionScope::institution(tenant, session.institution_id);
    require_institution_writable(st, &scope, session.institution_id).await?;
    require_operational(&session)?;
    bands_of(st, &scope, &session).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let live: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.result_batch WHERE tenant_id = $1 AND session_id = $2 \
         AND status NOT IN ('closed', 'archived')",
    )
    .bind(tenant.as_uuid())
    .bind(session_id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(id) = live {
        return Err(KernelError::Conflict(format!(
            "the session already has a live result batch ({id})"
        )));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.result_batch (tenant_id, institution_id, session_id, created_by) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(session.institution_id.as_uuid())
    .bind(session_id.as_uuid())
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        tenant,
        session.institution_id,
        "result_batch",
        id,
        None,
        "pending",
        RESULT_COMPUTED,
        None,
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.result_batch.create",
        "result_batch",
        id.to_string(),
        json!({"sessionId": session_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_batch(st, tenant, id).await
}

/// Summary of a computation run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ComputeSummary {
    pub processed: i32,
    pub computed: i32,
    pub withheld: i32,
    pub unchanged: i32,
    pub skipped: i32,
}

/// Computes every effective candidate of the batch's session (L3-I-083/085/086/087/093).
/// Re-runs are idempotent; allowed while the batch is `pending | computed | validated`.
pub async fn compute_batch(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<(BatchRow, ComputeSummary), KernelError> {
    let b = get_batch(st, tenant, id).await?;
    ensure_version(expected, b.version)?;
    let session = session_of(st, tenant, b.session_id).await?;
    let scope = ExecutionScope::institution(tenant, session.institution_id);
    require_institution_writable(st, &scope, session.institution_id).await?;
    require_operational(&session)?;
    if !batch_recomputable(b.status) {
        return Err(KernelError::Conflict(format!(
            "result batch is {}; reopen it before recomputing",
            b.status
        )));
    }
    let bands = bands_of(st, &scope, &session).await?;
    let regs: Vec<CandidateSummary> = st
        .directory
        .registrations_of_session(&scope, session.id)
        .await?
        .into_iter()
        .filter(|r| r.status.is_effective())
        .collect();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let b = load_batch_any(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, b.version)?;
    if b.status == ResultBatchStatus::Validated {
        // Recompute before approval sends validated results back to `computed` (L3-I-087).
        let change = StatusChange {
            cause_event_type: RESULT_COMPUTED,
            reason: Some("recompute before approval"),
            process_instance_id: None,
        };
        for r in session_results(&mut tx, tenant, session.id, ResultStatus::Validated).await? {
            change_result_status(&mut tx, tenant, &r, ResultStatus::Computed, &change, actor)
                .await?;
        }
    }
    let papers = papers_in_tx(&mut tx, tenant, session.id).await?;
    let mut sum = ComputeSummary::default();
    for reg in &regs {
        sum.processed += 1;
        match compute_and_store(
            &mut tx, tenant, &scope, &session, &papers, &bands, reg, b.id, actor,
        )
        .await?
        {
            StoreOutcome::Computed(_) => sum.computed += 1,
            StoreOutcome::Withheld(_) => sum.withheld += 1,
            StoreOutcome::Unchanged(_) => sum.unchanged += 1,
            StoreOutcome::Skipped(_) => sum.skipped += 1,
        }
    }
    // Counts of the session's current results after the run.
    let (computed_now, withheld_now): (i64, i64) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE status IN ('computed', 'validated')), \
         count(*) FILTER (WHERE status = 'withheld') FROM exams.term_result \
         WHERE tenant_id = $1 AND session_id = $2 AND status NOT IN ('superseded', 'archived')",
    )
    .bind(tenant.as_uuid())
    .bind(session.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let reason = format!(
        "{} candidates: {} computed, {} withheld, {} unchanged",
        sum.processed, sum.computed, sum.withheld, sum.unchanged
    );
    let b2 = set_batch_status(
        &mut tx,
        tenant,
        &b,
        ResultBatchStatus::Computed,
        Some(&reason),
        None,
        RESULT_COMPUTED,
        actor,
    )
    .await?;
    let row = sqlx::query(&format!(
        "UPDATE exams.result_batch SET run_count = run_count + 1, candidates = $3, \
         computed_count = $4, withheld_count = $5, unchanged_count = $6, computed_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING {BATCH_COLUMNS}"
    ))
    .bind(b.id)
    .bind(tenant.as_uuid())
    .bind(sum.processed)
    .bind(i32::try_from(computed_now).unwrap_or(i32::MAX))
    .bind(i32::try_from(withheld_now).unwrap_or(i32::MAX))
    .bind(sum.unchanged)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let b2 = BatchRow {
        version: b2.version,
        ..batch_from_row(&row).map_err(map_db_error)?
    };
    event(
        &mut tx,
        &scope,
        RESULT_COMPUTED,
        RESULT_COMPUTED_SCHEMA,
        b.id.to_string(),
        &status_payload(
            b.id,
            session.institution_id,
            Some(session.id.as_uuid()),
            None,
            None,
            Some(b.status.as_str()),
            "computed",
            b2.version,
            Some(reason.clone()),
            None,
            None,
        ),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.result_batch.compute",
        "result_batch",
        b.id.to_string(),
        json!({"processed": sum.processed, "computed": sum.computed, "withheld": sum.withheld,
               "unchanged": sum.unchanged, "skipped": sum.skipped}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((b2, sum))
}

async fn session_results(
    conn: &mut PgConnection,
    tenant: TenantId,
    session: ExamSessionId,
    status: ResultStatus,
) -> Result<Vec<ResultRow>, KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.term_result WHERE tenant_id = $1 AND session_id = $2 \
         AND status = $3 ORDER BY student_number FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .bind(status.as_str())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut v = Vec::with_capacity(ids.len());
    for id in ids {
        v.push(load_result(conn, tenant, TermResultId(id), true).await?);
    }
    Ok(v)
}

async fn apply_batch_approval(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    b: &BatchRow,
    pid: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<BatchRow, KernelError> {
    let change = StatusChange {
        cause_event_type: RESULT_APPROVED,
        reason,
        process_instance_id: Some(pid),
    };
    for r in session_results(conn, tenant, b.session_id, ResultStatus::Validated).await? {
        change_result_status(conn, tenant, &r, ResultStatus::Approved, &change, actor).await?;
    }
    let b2 = set_batch_status(
        conn,
        tenant,
        b,
        ResultBatchStatus::Approved,
        reason,
        Some(pid),
        RESULT_APPROVED,
        actor,
    )
    .await?;
    event(
        conn,
        scope,
        RESULT_APPROVED,
        RESULT_APPROVED_SCHEMA,
        b.id.to_string(),
        &status_payload(
            b.id,
            b.institution_id,
            Some(b.session_id.as_uuid()),
            None,
            None,
            Some(b.status.as_str()),
            "approved",
            b2.version,
            reason.map(str::to_owned),
            Some(pid.to_owned()),
            None,
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        "exams.result_batch.approve",
        "result_batch",
        b.id.to_string(),
        json!({"processInstanceId": pid, "reason": reason}),
        actor,
    )
    .await?;
    Ok(b2)
}

/// Verifies the computed results (→ `validated`, verification record) and requests result
/// approval (`RESULT_APPROVAL`); auto-approval applies it at once.
pub async fn validate_batch(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    notes: Option<String>,
    actor: &Actor,
) -> Result<BatchRow, KernelError> {
    let b = get_batch(st, tenant, id).await?;
    ensure_version(expected, b.version)?;
    let scope = ExecutionScope::institution(tenant, b.institution_id);
    require_institution_writable(st, &scope, b.institution_id).await?;
    if b.status != ResultBatchStatus::Computed {
        return Err(KernelError::Conflict(format!(
            "result batch is {}; compute it before validation",
            b.status
        )));
    }
    let outcome = request_approval(
        st,
        &scope,
        process::RESULT_APPROVAL,
        business_key("result_batch", b.id, b.version),
        BTreeMap::from([("sessionId".to_owned(), json!(b.session_id))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let b = load_batch_any(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, b.version)?;
    let computed = session_results(&mut tx, tenant, b.session_id, ResultStatus::Computed).await?;
    let withheld: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.term_result WHERE tenant_id = $1 AND session_id = $2 \
         AND status = 'withheld'",
    )
    .bind(tenant.as_uuid())
    .bind(b.session_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if computed.is_empty() {
        return Err(KernelError::Conflict(
            "no computed results to validate".into(),
        ));
    }
    let change = StatusChange {
        cause_event_type: RESULT_APPROVED,
        reason: Some("verified"),
        process_instance_id: Some(&pid),
    };
    let mut discrepancies = Vec::new();
    for r in &computed {
        // Verification: every line of a non-withheld result is graded or carries a remark.
        if r.lines
            .iter()
            .any(|l| l.grade.is_none() && l.remark.is_none())
        {
            discrepancies.push(json!({"resultId": r.id, "studentNumber": r.student_number}));
        }
        change_result_status(&mut tx, tenant, r, ResultStatus::Validated, &change, actor).await?;
    }
    if !discrepancies.is_empty() {
        return Err(KernelError::Conflict(format!(
            "{} result(s) have ungraded lines without a remark",
            discrepancies.len()
        )));
    }
    sqlx::query(
        "INSERT INTO exams.result_verification (tenant_id, institution_id, result_batch_id, \
         verified_count, withheld_count, discrepancies, notes, verified_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(tenant.as_uuid())
    .bind(b.institution_id.as_uuid())
    .bind(b.id)
    .bind(i32::try_from(computed.len()).unwrap_or(i32::MAX))
    .bind(i32::try_from(withheld).unwrap_or(i32::MAX))
    .bind(json!(discrepancies))
    .bind(notes.as_deref())
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut b2 = set_batch_status(
        &mut tx,
        tenant,
        &b,
        ResultBatchStatus::Validated,
        notes.as_deref(),
        Some(&pid),
        RESULT_APPROVED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.result_batch.validate",
        "result_batch",
        b.id.to_string(),
        json!({"verified": computed.len(), "withheld": withheld, "processInstanceId": pid}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        b2 = apply_batch_approval(
            &mut tx,
            tenant,
            &scope,
            &b2,
            &pid,
            Some("auto-approved"),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(b2)
}

/// Explicit approval decision on a validated batch (`exam.result.publish`).
pub async fn decide_batch(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<BatchRow, KernelError> {
    let b = get_batch(st, tenant, id).await?;
    ensure_version(expected, b.version)?;
    let scope = ExecutionScope::institution(tenant, b.institution_id);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let b = load_batch_any(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, b.version)?;
    let Some(pid) = b
        .process_instance_id
        .clone()
        .filter(|_| b.status == ResultBatchStatus::Validated)
    else {
        return Err(KernelError::Conflict(format!(
            "result batch is {}; no approval decision is pending",
            b.status
        )));
    };
    let b2 = if approve {
        apply_batch_approval(&mut tx, tenant, &scope, &b, &pid, reason.as_deref(), actor).await?
    } else {
        let change = StatusChange {
            cause_event_type: RESULT_APPROVED,
            reason: reason.as_deref(),
            process_instance_id: Some(&pid),
        };
        for r in session_results(&mut tx, tenant, b.session_id, ResultStatus::Validated).await? {
            change_result_status(&mut tx, tenant, &r, ResultStatus::Computed, &change, actor)
                .await?;
        }
        let b2 = set_batch_status(
            &mut tx,
            tenant,
            &b,
            ResultBatchStatus::Computed,
            reason.as_deref(),
            None,
            RESULT_APPROVED,
            actor,
        )
        .await?;
        audit(
            &mut tx,
            &scope,
            "exams.result_batch.reject",
            "result_batch",
            b.id.to_string(),
            json!({"reason": reason}),
            actor,
        )
        .await?;
        b2
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(b2)
}

/// Approved batch → finalised → ready for publication.
pub async fn finalise_batch(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<BatchRow, KernelError> {
    let b = get_batch(st, tenant, id).await?;
    ensure_version(expected, b.version)?;
    let scope = ExecutionScope::institution(tenant, b.institution_id);
    require_institution_writable(st, &scope, b.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let b = load_batch_any(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, b.version)?;
    let f = set_batch_status(
        &mut tx,
        tenant,
        &b,
        ResultBatchStatus::Finalised,
        None,
        None,
        RESULT_APPROVED,
        actor,
    )
    .await?;
    let r = set_batch_status(
        &mut tx,
        tenant,
        &f,
        ResultBatchStatus::ReadyForPublication,
        None,
        None,
        RESULT_APPROVED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.result_batch.finalise",
        "result_batch",
        b.id.to_string(),
        json!({}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Governed reopening of an approved batch before publication (L3-I-099): approved results get
/// a fresh working revision (`computed`) and the batch returns to `computed`.
pub async fn reopen_batch(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    reason: &str,
    actor: &Actor,
) -> Result<BatchRow, KernelError> {
    let b = get_batch(st, tenant, id).await?;
    ensure_version(expected, b.version)?;
    let session = session_of(st, tenant, b.session_id).await?;
    let scope = ExecutionScope::institution(tenant, b.institution_id);
    require_institution_writable(st, &scope, b.institution_id).await?;
    require_operational(&session)?;
    if !matches!(
        b.status,
        ResultBatchStatus::Validated
            | ResultBatchStatus::Approved
            | ResultBatchStatus::Finalised
            | ResultBatchStatus::ReadyForPublication
    ) {
        return Err(KernelError::Conflict(format!(
            "result batch is {}; only a validated or approved batch is reopened",
            b.status
        )));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let b = load_batch_any(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, b.version)?;
    let change = StatusChange {
        cause_event_type: RESULT_COMPUTED,
        reason: Some(reason),
        process_instance_id: None,
    };
    for r in session_results(&mut tx, tenant, b.session_id, ResultStatus::Validated).await? {
        change_result_status(&mut tx, tenant, &r, ResultStatus::Computed, &change, actor).await?;
    }
    for r in session_results(&mut tx, tenant, b.session_id, ResultStatus::Approved).await? {
        // A fresh working revision with the same content (approved results never step back).
        let reg = lock_registration(&mut tx, tenant, r.registration_id).await?;
        let nr = NewResult {
            tenant,
            registration: &reg,
            academic_term_id: r.academic_term_id,
            grading_scheme_id: r.grading_scheme_id,
            credits_attempted: r.credits_attempted,
            credits_earned: r.credits_earned,
            grade_points: r.grade_points,
            sgpa: r.sgpa,
            cgpa: r.cgpa,
            outcome: r.outcome,
            status: ResultStatus::Computed,
            status_reason: Some(reason.to_owned()),
            result_batch_id: Some(b.id),
            lines: r
                .lines
                .iter()
                .map(|l| crate::shared::results::NewResultLine {
                    paper_id: l.paper_id,
                    curriculum_component_id: l.curriculum_component_id,
                    paper_code: l.paper_code.clone(),
                    title: l.title.clone(),
                    credits: l.credits,
                    attempt_kind: l.attempt_kind,
                    internal_marks: l.internal_marks,
                    external_marks: l.external_marks,
                    grace_marks: l.grace_marks,
                    total_marks: l.total_marks,
                    max_marks: l.max_marks,
                    percent: l.percent,
                    grade: l.grade.clone(),
                    grade_point: l.grade_point,
                    passed: l.passed,
                    remark: l.remark.clone(),
                })
                .collect(),
        };
        insert_result(&mut tx, &nr, RESULT_COMPUTED, actor).await?;
    }
    let b2 = set_batch_status(
        &mut tx,
        tenant,
        &b,
        ResultBatchStatus::Computed,
        Some(reason),
        None,
        RESULT_COMPUTED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.result_batch.reopen",
        "result_batch",
        b.id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(b2)
}

// =================================================================== results

/// One result with lines (404).
pub async fn get_result(
    st: &ExamsState,
    tenant: TenantId,
    id: TermResultId,
) -> Result<ResultRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_result(&mut conn, tenant, id, false).await
}

/// Current results of a session (optionally one status).
pub async fn list_results(
    st: &ExamsState,
    tenant: TenantId,
    session: ExamSessionId,
    status: Option<ResultStatus>,
    include_superseded: bool,
) -> Result<Vec<ResultRow>, KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.term_result WHERE tenant_id = $1 AND session_id = $2 \
         AND ($3::text IS NULL OR status = $3) \
         AND ($4 OR status NOT IN ('superseded', 'archived')) \
         ORDER BY student_number, revision LIMIT 500",
    )
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .bind(include_superseded)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let mut v = Vec::with_capacity(ids.len());
    for id in ids {
        v.push(load_result(&mut conn, tenant, TermResultId(id), false).await?);
    }
    Ok(v)
}

/// Withholds an individual result before publication (L3-I-097/102).
pub async fn withhold_result(
    st: &ExamsState,
    tenant: TenantId,
    id: TermResultId,
    expected: i64,
    reason: &str,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_result(&mut tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    require_institution_writable(st, &scope, r.institution_id).await?;
    if !matches!(
        r.status,
        ResultStatus::Computed | ResultStatus::Validated | ResultStatus::Approved
    ) {
        return Err(KernelError::Conflict(format!(
            "result is {}; only an unpublished result is withheld",
            r.status
        )));
    }
    let from = r.status;
    let r2 = change_result_status(
        &mut tx,
        tenant,
        &r,
        ResultStatus::Withheld,
        &StatusChange {
            cause_event_type: RESULT_WITHHELD,
            reason: Some(reason),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        RESULT_WITHHELD,
        RESULT_WITHHELD_SCHEMA,
        r.id.to_string(),
        &result_status_payload(&r2, Some(from), Some(reason.to_owned()), None),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.term_result.withhold",
        "term_result",
        r.id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r2)
}

/// `true` when the withheld result `r` had been `approved` right before it was withheld (its
/// content then carries an institutional approval already).
async fn approved_before_withhold(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &ResultRow,
) -> Result<bool, KernelError> {
    let from: Option<Option<String>> = sqlx::query_scalar(
        "SELECT from_status FROM exams.status_history WHERE tenant_id = $1 \
         AND aggregate_type = 'term_result' AND aggregate_id = $2 AND to_status = 'withheld' \
         ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(r.id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(from.flatten().as_deref() == Some("approved"))
}

/// Whether releasing the withheld result `r` with computation `c` needs a release approval
/// (DEF-I-003): after institutional approval of the batch, only a result that returns
/// **unchanged** to the approval it already had goes straight back to `approved`; any other
/// release (changed content, or content never approved) waits in `validated` for an explicit
/// `RESULT_APPROVAL` decision.
async fn release_needs_approval(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &ResultRow,
    c: &Computed,
    approved_level: bool,
) -> Result<bool, KernelError> {
    if !approved_level || r.status != ResultStatus::Withheld || !c.computation.withheld.is_empty() {
        return Ok(false);
    }
    let unchanged = same_content(r, &c.computation, c.cgpa);
    Ok(!(unchanged && approved_before_withhold(conn, tenant, r).await?))
}

/// Recomputes one result before approval (L3-I-086/087) or releases a withheld one once its
/// blocking condition is resolved (L3-I-098): a changed computation is a new revision. Before
/// the batch is approved a released result rejoins it as `computed`; after approval it returns
/// to `approved` only when unchanged from the approval it had, otherwise it waits in
/// `validated` for a release decision (`RESULT_APPROVAL`, result decision kind `release`) and is
/// never published before that decision (DEF-I-003).
pub async fn recompute_result(
    st: &ExamsState,
    tenant: TenantId,
    id: TermResultId,
    expected: i64,
    release: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let pre = get_result(st, tenant, id).await?;
    ensure_version(expected, pre.version)?;
    let scope = ExecutionScope::institution(tenant, pre.institution_id);
    require_institution_writable(st, &scope, pre.institution_id).await?;
    let session = session_of(st, tenant, pre.session_id).await?;
    let bands = bands_of(st, &scope, &session).await?;
    let reason_text = reason.unwrap_or(if release {
        "blocking condition resolved"
    } else {
        "recomputed"
    });
    // The approval process starts before the owner transaction (shared::approval pattern), so
    // a read-only probe decides whether this release needs one.
    let approval = if release {
        let mut probe = st.pool.begin().await.map_err(map_db_error)?;
        let papers = papers_in_tx(&mut probe, tenant, session.id).await?;
        let reg = lock_registration(&mut probe, tenant, pre.registration_id).await?;
        let c = compute_in_tx(&mut probe, tenant, &session, &papers, &bands, &reg).await?;
        let batch = latest_batch(&mut probe, tenant, session.id).await?;
        let approved_level = batch.as_ref().is_some_and(|b| batch_approved(b.status));
        let needs = release_needs_approval(&mut probe, tenant, &pre, &c, approved_level).await?;
        probe.rollback().await.map_err(map_db_error)?;
        if needs {
            Some(
                request_approval(
                    st,
                    &scope,
                    process::RESULT_APPROVAL,
                    business_key("term_result_release", pre.id, pre.version),
                    BTreeMap::from([("reason".to_owned(), json!(reason_text))]),
                )
                .await?,
            )
        } else {
            None
        }
    } else {
        None
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_result(&mut tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    let allowed = if release {
        r.status == ResultStatus::Withheld
    } else {
        matches!(
            r.status,
            ResultStatus::Pending
                | ResultStatus::Computed
                | ResultStatus::Validated
                | ResultStatus::Withheld
        )
    };
    if !allowed {
        return Err(KernelError::Conflict(format!(
            "result is {}; {}",
            r.status,
            if release {
                "only a withheld result is released"
            } else {
                "approved results change only through a governed correction"
            }
        )));
    }
    let papers = papers_in_tx(&mut tx, tenant, session.id).await?;
    let reg = lock_registration(&mut tx, tenant, r.registration_id).await?;
    let c = compute_in_tx(&mut tx, tenant, &session, &papers, &bands, &reg).await?;
    if release {
        if let Some(w) = c.computation.withheld_reason() {
            return Err(KernelError::Conflict(format!(
                "the blocking condition is not resolved: {w}"
            )));
        }
    }
    let batch = latest_batch(&mut tx, tenant, session.id).await?;
    let approved_level = batch.as_ref().is_some_and(|b| batch_approved(b.status));
    let needs_approval =
        release && release_needs_approval(&mut tx, tenant, &r, &c, approved_level).await?;
    if needs_approval != approval.is_some() {
        return Err(KernelError::Conflict(
            "the result changed concurrently; retry the release".into(),
        ));
    }
    let decision_id = Uuid::now_v7();
    let pending_reason =
        format!("released — awaiting release approval (result decision {decision_id})");
    let target = if needs_approval {
        ResultStatus::Validated
    } else if release && approved_level {
        ResultStatus::Approved
    } else {
        ResultStatus::Computed
    };
    let change = StatusChange {
        cause_event_type: RESULT_COMPUTED,
        reason: Some(if needs_approval {
            pending_reason.as_str()
        } else {
            reason_text
        }),
        process_instance_id: approval.as_ref().map(|a| a.process_instance_id()),
    };
    let withheld_now = !c.computation.withheld.is_empty();
    let mut out = if same_content(&r, &c.computation, c.cgpa) && !withheld_now {
        if r.status == ResultStatus::Withheld {
            if target == ResultStatus::Validated {
                let back = change_result_status(
                    &mut tx,
                    tenant,
                    &r,
                    ResultStatus::Computed,
                    &change,
                    actor,
                )
                .await?;
                change_result_status(&mut tx, tenant, &back, target, &change, actor).await?
            } else {
                change_result_status(&mut tx, tenant, &r, target, &change, actor).await?
            }
        } else {
            r.clone()
        }
    } else {
        let nr = new_result(tenant, &reg, &session, &c, batch.as_ref().map(|b| b.id))?;
        let fresh = insert_result(&mut tx, &nr, RESULT_COMPUTED, actor).await?;
        if fresh.status == ResultStatus::Withheld {
            event(
                &mut tx,
                &scope,
                RESULT_WITHHELD,
                RESULT_WITHHELD_SCHEMA,
                fresh.id.to_string(),
                &result_status_payload(&fresh, None, fresh.status_reason.clone(), None),
                actor,
            )
            .await?;
            fresh
        } else {
            walk_to(&mut tx, tenant, fresh, target, &change, actor).await?
        }
    };
    if let Some(outcome) = &approval {
        let pid = outcome.process_instance_id().to_owned();
        sqlx::query(
            "INSERT INTO exams.result_decision (id, tenant_id, institution_id, session_id, \
             result_id, kind, reason, process_instance_id, requested_by) \
             VALUES ($1, $2, $3, $4, $5, 'release', $6, $7, $8)",
        )
        .bind(decision_id)
        .bind(tenant.as_uuid())
        .bind(out.institution_id.as_uuid())
        .bind(out.session_id.as_uuid())
        .bind(out.id.as_uuid())
        .bind(reason_text)
        .bind(&pid)
        .bind(actor.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                KernelError::Conflict("a release of this result is already pending".into())
            }
            other => other,
        })?;
        audit(
            &mut tx,
            &scope,
            "exams.term_result.request_release",
            "term_result",
            out.id.to_string(),
            json!({"decisionId": decision_id, "reason": reason_text, "processInstanceId": pid}),
            actor,
        )
        .await?;
        if let ApprovalOutcome::Approved { .. } = outcome {
            let d = load_decision(&mut tx, tenant, decision_id).await?;
            let system = Actor::system(actor.correlation_id.clone());
            out = finish_decision(
                st,
                &mut tx,
                tenant,
                &d,
                true,
                Some("auto-approved"),
                &system,
            )
            .await?;
        }
    }
    audit(
        &mut tx,
        &scope,
        if release {
            "exams.term_result.release"
        } else {
            "exams.term_result.recompute"
        },
        "term_result",
        r.id.to_string(),
        json!({"result": out.id, "revision": out.revision, "status": out.status.as_str()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// A requested correction of final marks (L3-I-088/100/117).
#[derive(Clone, Debug)]
pub struct CorrectionRequest {
    pub paper_id: ExamPaperId,
    pub internal_marks: Option<Decimal>,
    pub external_marks: Option<Decimal>,
    pub grace_marks: Option<Decimal>,
    pub reason: String,
}

async fn load_decision(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<ResultDecisionRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {DECISION_COLUMNS} FROM exams.result_decision WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(not_found("result decision"))?;
    decision_from_row(&row).map_err(map_db_error)
}

/// One governed result decision (404).
pub async fn get_decision(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ResultDecisionRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {DECISION_COLUMNS} FROM exams.result_decision WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(not_found("result decision"))?;
    decision_from_row(&row).map_err(map_db_error)
}

async fn apply_correction(
    st: &ExamsState,
    conn: &mut PgConnection,
    tenant: TenantId,
    d: &ResultDecisionRow,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let r = load_result(conn, tenant, d.result_id, true).await?;
    if !r.status.is_current() || r.status == ResultStatus::Withheld {
        return Err(KernelError::Conflict(format!(
            "result is {}; it cannot be corrected",
            r.status
        )));
    }
    let paper = d
        .paper_id
        .ok_or_else(|| KernelError::Conflict("correction without a paper".into()))?;
    let mark_id: Uuid = sqlx::query_scalar(
        "SELECT id FROM exams.paper_mark WHERE tenant_id = $1 AND registration_id = $2 AND paper_id = $3",
    )
    .bind(tenant.as_uuid())
    .bind(r.registration_id.as_uuid())
    .bind(paper)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(not_found("marks"))?;
    let m = lock_mark(
        conn,
        tenant,
        sutra_contracts::exams::ids::PaperMarkId(mark_id),
    )
    .await?;
    let mut v = MarkValues::of(&m);
    if let Some(i) = d.internal_marks {
        v.internal_marks = Some(i);
    }
    if let Some(e) = d.external_marks {
        v.external_marks = Some(e);
        v.absent = false;
    }
    if let Some(g) = d.grace_marks {
        v.grace_marks = g;
    }
    revise_final(conn, tenant, &m, v, "correction", &d.reason, actor).await?;
    revise_result(
        st,
        conn,
        tenant,
        &r,
        RESULT_REVISED,
        &d.reason,
        d.process_instance_id.as_deref(),
        actor,
    )
    .await
}

async fn apply_withdrawal(
    conn: &mut PgConnection,
    tenant: TenantId,
    d: &ResultDecisionRow,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let r = load_result(conn, tenant, d.result_id, true).await?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    let r2 = change_result_status(
        conn,
        tenant,
        &r,
        ResultStatus::Withdrawn,
        &StatusChange {
            cause_event_type: RESULT_WITHDRAWN,
            reason: Some(&d.reason),
            process_instance_id: d.process_instance_id.as_deref(),
        },
        actor,
    )
    .await?;
    event(
        conn,
        &scope,
        RESULT_WITHDRAWN,
        RESULT_WITHDRAWN_SCHEMA,
        r.id.to_string(),
        &result_status_payload(
            &r2,
            Some(r.status),
            Some(d.reason.clone()),
            d.process_instance_id.clone(),
        ),
        actor,
    )
    .await?;
    withdraw_cards_of(
        conn,
        tenant,
        r.registration_id.as_uuid(),
        "result withdrawn",
        actor,
    )
    .await?;
    Ok(r2)
}

/// Settles a release decision (DEF-I-003): approval moves the released result `validated →
/// approved` (publishable); rejection withholds it again.
async fn settle_release(
    conn: &mut PgConnection,
    tenant: TenantId,
    d: &ResultDecisionRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let r = load_result(conn, tenant, d.result_id, true).await?;
    if r.status != ResultStatus::Validated {
        return Err(KernelError::Conflict(format!(
            "result is {}; the release can no longer be decided",
            r.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    let rejected = format!("release rejected: {}", reason.unwrap_or("no reason given"));
    let (to, cause, why) = if approve {
        (ResultStatus::Approved, RESULT_APPROVED, d.reason.as_str())
    } else {
        (ResultStatus::Withheld, RESULT_WITHHELD, rejected.as_str())
    };
    let r2 = change_result_status(
        conn,
        tenant,
        &r,
        to,
        &StatusChange {
            cause_event_type: cause,
            reason: Some(why),
            process_instance_id: d.process_instance_id.as_deref(),
        },
        actor,
    )
    .await?;
    if !approve {
        event(
            conn,
            &scope,
            RESULT_WITHHELD,
            RESULT_WITHHELD_SCHEMA,
            r.id.to_string(),
            &result_status_payload(
                &r2,
                Some(r.status),
                Some(why.to_owned()),
                d.process_instance_id.clone(),
            ),
            actor,
        )
        .await?;
    }
    Ok(r2)
}

/// Requests a governed decision on a result: an approved marks correction (`RESULT_CORRECTION`)
/// or a withdrawal of a published result (`RESULT_WITHDRAWAL`). Auto-approval applies it now.
pub async fn request_decision(
    st: &ExamsState,
    tenant: TenantId,
    id: TermResultId,
    expected: i64,
    correction: Option<CorrectionRequest>,
    withdrawal_reason: Option<String>,
    actor: &Actor,
) -> Result<(ResultDecisionRow, ResultRow), KernelError> {
    let r = get_result(st, tenant, id).await?;
    ensure_version(expected, r.version)?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    require_institution_writable(st, &scope, r.institution_id).await?;
    let (kind, key, reason) = match (&correction, &withdrawal_reason) {
        (Some(c), _) => {
            if r.lines.iter().all(|l| l.paper_id != c.paper_id) {
                return Err(super::common::invalid(
                    "paperId",
                    "not_in_result",
                    "the paper is not part of this result",
                ));
            }
            if !matches!(
                r.status,
                ResultStatus::Computed
                    | ResultStatus::Validated
                    | ResultStatus::Approved
                    | ResultStatus::Published
            ) {
                return Err(KernelError::Conflict(format!(
                    "result is {}; it cannot be corrected",
                    r.status
                )));
            }
            ("correction", process::RESULT_CORRECTION, c.reason.clone())
        }
        (None, Some(reason)) => {
            if r.status != ResultStatus::Published {
                return Err(KernelError::Conflict(format!(
                    "result is {}; only a published result is withdrawn",
                    r.status
                )));
            }
            ("withdrawal", process::RESULT_WITHDRAWAL, reason.clone())
        }
        (None, None) => {
            return Err(super::common::invalid(
                "reason",
                "required",
                "a reason is required",
            ))
        }
    };
    if reason.trim().is_empty() {
        return Err(super::common::invalid(
            "reason",
            "required",
            "a reason is required",
        ));
    }
    let outcome = request_approval(
        st,
        &scope,
        key,
        business_key(&format!("term_result_{kind}"), r.id, r.version),
        BTreeMap::from([("reason".to_owned(), json!(reason))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_result(&mut tx, tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    let did: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.result_decision (tenant_id, institution_id, session_id, result_id, \
         kind, paper_id, internal_marks, external_marks, grace_marks, reason, \
         process_instance_id, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(r.institution_id.as_uuid())
    .bind(r.session_id.as_uuid())
    .bind(r.id.as_uuid())
    .bind(kind)
    .bind(correction.as_ref().map(|c| c.paper_id.as_uuid()))
    .bind(correction.as_ref().and_then(|c| c.internal_marks))
    .bind(correction.as_ref().and_then(|c| c.external_marks))
    .bind(correction.as_ref().and_then(|c| c.grace_marks))
    .bind(&reason)
    .bind(&pid)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict(format!("a {kind} of this result is already pending"))
        }
        other => other,
    })?;
    audit(
        &mut tx,
        &scope,
        &format!("exams.term_result.request_{kind}"),
        "term_result",
        r.id.to_string(),
        json!({"decisionId": did, "reason": reason, "processInstanceId": pid}),
        actor,
    )
    .await?;
    let mut result = locked;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let d = load_decision(&mut tx, tenant, did).await?;
        let system = Actor::system(actor.correlation_id.clone());
        result = finish_decision(
            st,
            &mut tx,
            tenant,
            &d,
            true,
            Some("auto-approved"),
            &system,
        )
        .await?;
    }
    let d = load_decision(&mut tx, tenant, did).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((d, result))
}

#[allow(clippy::too_many_arguments)]
async fn finish_decision(
    st: &ExamsState,
    conn: &mut PgConnection,
    tenant: TenantId,
    d: &ResultDecisionRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let result = match (approve, d.kind.as_str()) {
        (true, "correction") => apply_correction(st, conn, tenant, d, actor).await?,
        (true, "release") => settle_release(conn, tenant, d, true, reason, actor).await?,
        (true, _) => apply_withdrawal(conn, tenant, d, actor).await?,
        (false, "release") => settle_release(conn, tenant, d, false, reason, actor).await?,
        (false, _) => load_result(conn, tenant, d.result_id, false).await?,
    };
    sqlx::query(
        "UPDATE exams.result_decision SET status = $3, decision_reason = $4, decided_by = $5, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(d.id)
    .bind(tenant.as_uuid())
    .bind(if approve { "approved" } else { "rejected" })
    .bind(reason)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        d.institution_id,
        "result_decision",
        d.id,
        Some("pending"),
        if approve { "approved" } else { "rejected" },
        RESULT_REVISED,
        reason,
        d.process_instance_id.as_deref(),
        actor,
    )
    .await?;
    Ok(result)
}

/// Explicit decision on a pending correction / withdrawal (`exam.result.publish`).
pub async fn decide_decision(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<(ResultDecisionRow, ResultRow), KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let d = load_decision(&mut tx, tenant, id).await?;
    ensure_version(expected, d.version)?;
    if d.status != "pending" {
        return Err(KernelError::Conflict(format!(
            "the {} is already {}",
            d.kind, d.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, d.institution_id);
    require_institution_writable(st, &scope, d.institution_id).await?;
    let r = finish_decision(st, &mut tx, tenant, &d, approve, reason.as_deref(), actor).await?;
    audit(
        &mut tx,
        &scope,
        &format!("exams.term_result.decide_{}", d.kind),
        "result_decision",
        d.id.to_string(),
        json!({"approve": approve, "reason": reason}),
        actor,
    )
    .await?;
    let d = load_decision(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((d, r))
}

/// Restores a withdrawn result (L3-I-110).
pub async fn restore_result(
    st: &ExamsState,
    tenant: TenantId,
    id: TermResultId,
    expected: i64,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_result(&mut tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    require_institution_writable(st, &scope, r.institution_id).await?;
    if r.status != ResultStatus::Withdrawn {
        return Err(KernelError::Conflict(format!(
            "result is {}; only a withdrawn result is restored",
            r.status
        )));
    }
    let r2 = change_result_status(
        &mut tx,
        tenant,
        &r,
        ResultStatus::Published,
        &StatusChange {
            cause_event_type: RESULT_PUBLISHED,
            reason: reason.or(Some("restored")),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        RESULT_PUBLISHED,
        RESULT_PUBLISHED_SCHEMA,
        r.id.to_string(),
        &result_payload(&r2),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.term_result.restore",
        "term_result",
        r.id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r2)
}

// =================================================================== publications

async fn load_publication<'c, E>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PublicationRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let row = sqlx::query(&format!(
        "SELECT {PUBLICATION_COLUMNS} FROM exams.publication WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(ex)
    .await
    .map_err(map_db_error)?
    .ok_or(not_found("publication"))?;
    publication_from_row(&row).map_err(map_db_error)
}

/// One publication (404).
pub async fn get_publication(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
) -> Result<PublicationRow, KernelError> {
    load_publication(&st.pool, tenant, id, false).await
}

/// Publications of a session, newest first.
pub async fn list_publications(
    st: &ExamsState,
    tenant: TenantId,
    session: ExamSessionId,
) -> Result<Vec<PublicationRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {PUBLICATION_COLUMNS} FROM exams.publication WHERE tenant_id = $1 AND session_id = $2 \
         ORDER BY created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(publication_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn set_publication_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &PublicationRow,
    to: PublicationStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<PublicationRow, KernelError> {
    if !publication_can_transition(p.status, to) {
        return Err(KernelError::Conflict(format!(
            "publication is {}; it cannot become {to}",
            p.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.publication SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), \
         authorised_at = CASE WHEN $3 = 'authorised' THEN now() ELSE authorised_at END, \
         published_at = CASE WHEN $3 = 'published' THEN now() ELSE published_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {PUBLICATION_COLUMNS}"
    ))
    .bind(p.id)
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .bind(p.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("publication changed concurrently".into()))?;
    history(
        conn,
        tenant,
        p.institution_id,
        "publication",
        p.id,
        Some(p.status.as_str()),
        to.as_str(),
        cause,
        reason,
        pid,
        actor,
    )
    .await?;
    publication_from_row(&row).map_err(map_db_error)
}

/// Plans a publication of the session's approved results (needs an approved batch).
pub async fn create_publication(
    st: &ExamsState,
    tenant: TenantId,
    session_id: ExamSessionId,
    scheduled_on: Option<NaiveDate>,
    actor: &Actor,
) -> Result<PublicationRow, KernelError> {
    let session = session_of(st, tenant, session_id).await?;
    let scope = ExecutionScope::institution(tenant, session.institution_id);
    require_institution_writable(st, &scope, session.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let batch = latest_batch(&mut tx, tenant, session_id).await?;
    if !batch.as_ref().is_some_and(|b| batch_approved(b.status)) {
        return Err(KernelError::Conflict(
            "results must be approved before a publication is planned".into(),
        ));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.publication (tenant_id, institution_id, session_id, status, \
         scheduled_on, created_by) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(session.institution_id.as_uuid())
    .bind(session_id.as_uuid())
    .bind(if scheduled_on.is_some() {
        "scheduled"
    } else {
        "planned"
    })
    .bind(scheduled_on)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("the session already has an open publication".into())
        }
        other => other,
    })?;
    history(
        &mut tx,
        tenant,
        session.institution_id,
        "publication",
        id,
        None,
        if scheduled_on.is_some() {
            "scheduled"
        } else {
            "planned"
        },
        RESULT_PUBLISHED,
        None,
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.publication.create",
        "publication",
        id.to_string(),
        json!({"sessionId": session_id, "scheduledOn": scheduled_on}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_publication(st, tenant, id).await
}

/// Freezes the result set a publication may publish (DEF-I-010): the session's results that
/// are `approved` at the moment the publication is authorised. Idempotent.
async fn freeze_publication_set(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &PublicationRow,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO exams.publication_snapshot (tenant_id, publication_id, result_id, \
         registration_id) SELECT $1, $2, id, registration_id FROM exams.term_result \
         WHERE tenant_id = $1 AND session_id = $3 AND status = 'approved' \
         ON CONFLICT (publication_id, result_id) DO NOTHING",
    )
    .bind(tenant.as_uuid())
    .bind(p.id)
    .bind(p.session_id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Requests publication authorisation (`RESULT_PUBLICATION`); auto-approval authorises now.
pub async fn authorise_publication(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<PublicationRow, KernelError> {
    let p = get_publication(st, tenant, id).await?;
    ensure_version(expected, p.version)?;
    let scope = ExecutionScope::institution(tenant, p.institution_id);
    require_institution_writable(st, &scope, p.institution_id).await?;
    if !matches!(
        p.status,
        PublicationStatus::Planned | PublicationStatus::Scheduled
    ) {
        return Err(KernelError::Conflict(format!(
            "publication is {}; it cannot be authorised",
            p.status
        )));
    }
    let outcome = request_approval(
        st,
        &scope,
        process::RESULT_PUBLICATION,
        business_key("publication", p.id, p.version),
        BTreeMap::from([("sessionId".to_owned(), json!(p.session_id))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_publication(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, p.version)?;
    let p2 = match outcome {
        ApprovalOutcome::Approved { .. } => {
            freeze_publication_set(&mut tx, tenant, &p).await?;
            set_publication_status(
                &mut tx,
                tenant,
                &p,
                PublicationStatus::Authorised,
                Some("auto-approved"),
                Some(&pid),
                RESULT_PUBLISHED,
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => {
            let row = sqlx::query(&format!(
                "UPDATE exams.publication SET process_instance_id = $3, version = version + 1, \
                 updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {PUBLICATION_COLUMNS}"
            ))
            .bind(p.id)
            .bind(tenant.as_uuid())
            .bind(&pid)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            publication_from_row(&row).map_err(map_db_error)?
        }
    };
    audit(
        &mut tx,
        &scope,
        "exams.publication.authorise",
        "publication",
        p.id.to_string(),
        json!({"processInstanceId": pid}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p2)
}

/// Explicit authorisation decision of a publication (`exam.result.publish`).
pub async fn decide_publication(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<PublicationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_publication(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, p.version)?;
    let scope = ExecutionScope::institution(tenant, p.institution_id);
    let pending = matches!(
        p.status,
        PublicationStatus::Planned | PublicationStatus::Scheduled
    ) && p.process_instance_id.is_some();
    if !pending {
        return Err(KernelError::Conflict(format!(
            "publication is {}; no authorisation decision is pending",
            p.status
        )));
    }
    let p2 = if approve {
        freeze_publication_set(&mut tx, tenant, &p).await?;
        set_publication_status(
            &mut tx,
            tenant,
            &p,
            PublicationStatus::Authorised,
            reason.as_deref(),
            None,
            RESULT_PUBLISHED,
            actor,
        )
        .await?
    } else {
        let row = sqlx::query(&format!(
            "UPDATE exams.publication SET process_instance_id = NULL, status_reason = $3, \
             version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 \
             RETURNING {PUBLICATION_COLUMNS}"
        ))
        .bind(p.id)
        .bind(tenant.as_uuid())
        .bind(reason.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        publication_from_row(&row).map_err(map_db_error)?
    };
    audit(
        &mut tx,
        &scope,
        "exams.publication.decide",
        "publication",
        p.id.to_string(),
        json!({"approve": approve, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p2)
}

/// Summary of a publication run.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PublishSummary {
    pub published: i32,
    pub revised: i32,
    pub skipped: i32,
}

async fn publish_in_tx(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &PublicationRow,
    actor: &Actor,
) -> Result<(PublicationRow, PublishSummary), KernelError> {
    let scope = ExecutionScope::institution(tenant, p.institution_id);
    // DEF-I-009: never before the scheduled date or while a delay is in force.
    let today: NaiveDate = sqlx::query_scalar("SELECT current_date")
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    if let Some(on) = p.scheduled_on.filter(|d| today < *d) {
        return Err(KernelError::Conflict(format!(
            "publication is scheduled for {on}; it cannot be published before then"
        )));
    }
    if let Some(until) = p.delayed_until.filter(|d| today < *d) {
        return Err(KernelError::Conflict(format!(
            "publication is delayed until {until}; it cannot be published before then"
        )));
    }
    // DEF-I-010: only the result set frozen at authorisation is published.
    let frozen: Vec<Uuid> = sqlx::query_scalar(
        "SELECT result_id FROM exams.publication_snapshot WHERE tenant_id = $1 \
         AND publication_id = $2 ORDER BY created_at, id",
    )
    .bind(tenant.as_uuid())
    .bind(p.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if frozen.is_empty() {
        return Err(KernelError::Conflict(
            "no approved results to publish".into(),
        ));
    }
    let mut sum = PublishSummary::default();
    let change = StatusChange {
        cause_event_type: RESULT_PUBLISHED,
        reason: None,
        process_instance_id: p.process_instance_id.as_deref(),
    };
    let mut approved = Vec::new();
    for id in frozen {
        let r = load_result(conn, tenant, TermResultId(id), true).await?;
        let skip = if r.status != ResultStatus::Approved {
            Some(format!(
                "result is {} since authorisation; not published",
                r.status
            ))
        } else {
            // DEF-I-004: a malpractice case opened after approval withholds the result.
            let open = open_malpractice_cases(conn, tenant, r.registration_id.as_uuid()).await?;
            if open.is_empty() {
                None
            } else {
                let why = format!("malpractice case {} pending", open.join(", "));
                let w = change_result_status(
                    conn,
                    tenant,
                    &r,
                    ResultStatus::Withheld,
                    &StatusChange {
                        cause_event_type: RESULT_WITHHELD,
                        reason: Some(&why),
                        process_instance_id: None,
                    },
                    actor,
                )
                .await?;
                event(
                    conn,
                    &scope,
                    RESULT_WITHHELD,
                    RESULT_WITHHELD_SCHEMA,
                    w.id.to_string(),
                    &result_status_payload(&w, Some(r.status), Some(why.clone()), None),
                    actor,
                )
                .await?;
                Some(why)
            }
        };
        match skip {
            None => approved.push(r),
            Some(detail) => {
                sum.skipped += 1;
                sqlx::query(
                    "INSERT INTO exams.publication_item (tenant_id, publication_id, result_id, \
                     registration_id, outcome, detail) VALUES ($1, $2, $3, $4, 'skipped', $5) \
                     ON CONFLICT (publication_id, result_id) DO NOTHING",
                )
                .bind(tenant.as_uuid())
                .bind(p.id)
                .bind(r.id.as_uuid())
                .bind(r.registration_id.as_uuid())
                .bind(detail)
                .execute(&mut *conn)
                .await
                .map_err(map_db_error)?;
            }
        }
    }
    for r in &approved {
        let already: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM exams.term_result WHERE tenant_id = $1 \
             AND session_id = $2 AND student_id = $3 AND id <> $4 AND published_at IS NOT NULL)",
        )
        .bind(tenant.as_uuid())
        .bind(r.session_id.as_uuid())
        .bind(r.student_id.as_uuid())
        .bind(r.id.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let pr =
            change_result_status(conn, tenant, r, ResultStatus::Published, &change, actor).await?;
        let (etype, schema, outcome) = if already {
            sum.revised += 1;
            (RESULT_REVISED, RESULT_REVISED_SCHEMA, "revised")
        } else {
            sum.published += 1;
            (RESULT_PUBLISHED, RESULT_PUBLISHED_SCHEMA, "published")
        };
        event(
            conn,
            &scope,
            etype,
            schema,
            pr.id.to_string(),
            &result_payload(&pr),
            actor,
        )
        .await?;
        sqlx::query(
            "INSERT INTO exams.publication_item (tenant_id, publication_id, result_id, \
             registration_id, outcome) VALUES ($1, $2, $3, $4, $5) \
             ON CONFLICT (publication_id, result_id) DO NOTHING",
        )
        .bind(tenant.as_uuid())
        .bind(p.id)
        .bind(pr.id.as_uuid())
        .bind(pr.registration_id.as_uuid())
        .bind(outcome)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    for r in session_results(conn, tenant, p.session_id, ResultStatus::Withheld).await? {
        let inserted = sqlx::query(
            "INSERT INTO exams.publication_item (tenant_id, publication_id, result_id, \
             registration_id, outcome, detail) VALUES ($1, $2, $3, $4, 'skipped', $5) \
             ON CONFLICT (publication_id, result_id) DO NOTHING",
        )
        .bind(tenant.as_uuid())
        .bind(p.id)
        .bind(r.id.as_uuid())
        .bind(r.registration_id.as_uuid())
        .bind(r.status_reason.as_deref().unwrap_or("withheld"))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if inserted.rows_affected() > 0 {
            sum.skipped += 1;
        }
    }
    let p2 = set_publication_status(
        conn,
        tenant,
        p,
        PublicationStatus::Published,
        None,
        None,
        RESULT_PUBLISHED,
        actor,
    )
    .await?;
    let row = sqlx::query(&format!(
        "UPDATE exams.publication SET attempts = attempts + 1, published_count = $3, \
         skipped_count = $4, last_error = NULL WHERE id = $1 AND tenant_id = $2 \
         RETURNING {PUBLICATION_COLUMNS}"
    ))
    .bind(p.id)
    .bind(tenant.as_uuid())
    .bind(sum.published + sum.revised)
    .bind(sum.skipped)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let p3 = PublicationRow {
        version: p2.version,
        ..publication_from_row(&row).map_err(map_db_error)?
    };
    // Batches ready for publication are closed once their results are out.
    let ready: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.result_batch WHERE tenant_id = $1 AND session_id = $2 \
         AND status = 'ready_for_publication'",
    )
    .bind(tenant.as_uuid())
    .bind(p.session_id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for b in ready {
        let b = load_batch_any(&mut *conn, tenant, b, true).await?;
        set_batch_status(
            conn,
            tenant,
            &b,
            ResultBatchStatus::Closed,
            Some("published"),
            None,
            RESULT_PUBLISHED,
            actor,
        )
        .await?;
    }
    audit(
        conn,
        &scope,
        "exams.publication.publish",
        "publication",
        p.id.to_string(),
        json!({"published": sum.published, "revised": sum.revised, "skipped": sum.skipped}),
        actor,
    )
    .await?;
    Ok((p3, sum))
}

/// Publishes the result set frozen when the publication was authorised (DEF-I-010; results
/// still `approved`, none with an open malpractice case — DEF-I-004), never before its
/// scheduled date / delay (DEF-I-009) (`authorised | ready | failed` → `published`).
/// A failure rolls the run back and leaves the publication `failed` with the error, so
/// [`retry`](publish_publication) recovers safely (L3-I-116); re-runs never republish.
pub async fn publish_publication(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<(PublicationRow, PublishSummary), KernelError> {
    let p = get_publication(st, tenant, id).await?;
    ensure_version(expected, p.version)?;
    let scope = ExecutionScope::institution(tenant, p.institution_id);
    require_institution_writable(st, &scope, p.institution_id).await?;
    if !matches!(
        p.status,
        PublicationStatus::Authorised | PublicationStatus::Ready | PublicationStatus::Failed
    ) {
        return Err(KernelError::Conflict(format!(
            "publication is {}; it must be authorised before publishing",
            p.status
        )));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_publication(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    match publish_in_tx(&mut tx, tenant, &locked, actor).await {
        Ok(out) => {
            tx.commit().await.map_err(map_db_error)?;
            Ok(out)
        }
        Err(err) => {
            drop(tx);
            if matches!(err, KernelError::Internal(_) | KernelError::Unavailable(_))
                && locked.status != PublicationStatus::Failed
            {
                let mut tx = st.pool.begin().await.map_err(map_db_error)?;
                let p = load_publication(&mut *tx, tenant, id, true).await?;
                let failed = set_publication_status(
                    &mut tx,
                    tenant,
                    &p,
                    PublicationStatus::Failed,
                    Some("publication failed"),
                    None,
                    RESULT_PUBLISHED,
                    actor,
                )
                .await?;
                sqlx::query(
                    "UPDATE exams.publication SET attempts = attempts + 1, last_error = $3 \
                     WHERE id = $1 AND tenant_id = $2",
                )
                .bind(failed.id)
                .bind(tenant.as_uuid())
                .bind(err.to_string())
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
                tx.commit().await.map_err(map_db_error)?;
            }
            Err(err)
        }
    }
}

/// Delays a pending publication (L3-I-104).
pub async fn delay_publication(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    reason: &str,
    until: Option<NaiveDate>,
    actor: &Actor,
) -> Result<PublicationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_publication(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, p.version)?;
    let scope = ExecutionScope::institution(tenant, p.institution_id);
    require_institution_writable(st, &scope, p.institution_id).await?;
    let p2 = set_publication_status(
        &mut tx,
        tenant,
        &p,
        PublicationStatus::Delayed,
        Some(reason),
        None,
        PUBLICATION_DELAYED,
        actor,
    )
    .await?;
    sqlx::query("UPDATE exams.publication SET delayed_until = $3 WHERE id = $1 AND tenant_id = $2")
        .bind(p.id)
        .bind(tenant.as_uuid())
        .bind(until)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    event(
        &mut tx,
        &scope,
        PUBLICATION_DELAYED,
        PUBLICATION_DELAYED_SCHEMA,
        p.id.to_string(),
        &status_payload(
            p.id,
            p.institution_id,
            Some(p.session_id.as_uuid()),
            None,
            None,
            Some(p.status.as_str()),
            "delayed",
            p2.version,
            Some(reason.to_owned()),
            None,
            None,
        ),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.publication.delay",
        "publication",
        p.id.to_string(),
        json!({"reason": reason, "until": until}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_publication(st, tenant, id).await
}

/// Resolves a delay: back to `authorised` when it had been authorised, else `planned`.
pub async fn resume_publication(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<PublicationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_publication(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, p.version)?;
    let scope = ExecutionScope::institution(tenant, p.institution_id);
    require_institution_writable(st, &scope, p.institution_id).await?;
    let to = if p.authorised_at.is_some() {
        PublicationStatus::Authorised
    } else {
        PublicationStatus::Planned
    };
    let p2 = set_publication_status(
        &mut tx,
        tenant,
        &p,
        to,
        Some("delay resolved"),
        None,
        PUBLICATION_DELAYED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.publication.resume",
        "publication",
        p.id.to_string(),
        json!({"to": to.as_str()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p2)
}

/// Cancels a pending publication (L3-I-105); results stay approved.
pub async fn cancel_publication(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    reason: &str,
    actor: &Actor,
) -> Result<PublicationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_publication(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, p.version)?;
    let scope = ExecutionScope::institution(tenant, p.institution_id);
    require_institution_writable(st, &scope, p.institution_id).await?;
    let p2 = set_publication_status(
        &mut tx,
        tenant,
        &p,
        PublicationStatus::Cancelled,
        Some(reason),
        None,
        RESULT_PUBLISHED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.publication.cancel",
        "publication",
        p.id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p2)
}

// =================================================================== grade cards

async fn withdraw_cards_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    registration: Uuid,
    reason: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let rows = sqlx::query(&format!(
        "UPDATE exams.grade_card SET status = 'withdrawn', status_reason = $3, \
         version = version + 1, updated_at = now() WHERE tenant_id = $1 AND registration_id = $2 \
         AND status <> 'withdrawn' RETURNING {CARD_COLUMNS}"
    ))
    .bind(tenant.as_uuid())
    .bind(registration)
    .bind(reason)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for r in &rows {
        let c = card_from_row(r).map_err(map_db_error)?;
        history(
            conn,
            tenant,
            c.institution_id,
            "grade_card",
            c.id,
            None,
            "withdrawn",
            GRADE_CARD_ISSUED,
            Some(reason),
            None,
            actor,
        )
        .await?;
    }
    Ok(())
}

async fn load_card<'c, E>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<GradeCardRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let row = sqlx::query(&format!(
        "SELECT {CARD_COLUMNS} FROM exams.grade_card WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(ex)
    .await
    .map_err(map_db_error)?
    .ok_or(not_found("grade card"))?;
    card_from_row(&row).map_err(map_db_error)
}

/// One grade card (404).
pub async fn get_card(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
) -> Result<GradeCardRow, KernelError> {
    load_card(&st.pool, tenant, id, false).await
}

/// Grade cards of a set of registrations, newest first.
pub async fn cards_of_registrations(
    st: &ExamsState,
    tenant: TenantId,
    registrations: &[Uuid],
) -> Result<Vec<GradeCardRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {CARD_COLUMNS} FROM exams.grade_card WHERE tenant_id = $1 \
         AND registration_id = ANY($2) ORDER BY issued_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(registrations)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(card_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

async fn insert_card(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &ResultRow,
    status: GradeCardStatus,
    supersedes: Option<Uuid>,
    actor: &Actor,
) -> Result<GradeCardRow, KernelError> {
    let number = numbering::next(conn, tenant, r.institution_id, kind::GRADE_CARD).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.grade_card (tenant_id, institution_id, session_id, registration_id, \
         result_id, student_id, subject_ref, card_number, status, supersedes_id, issued_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(r.institution_id.as_uuid())
    .bind(r.session_id.as_uuid())
    .bind(r.registration_id.as_uuid())
    .bind(r.id.as_uuid())
    .bind(r.student_id.as_uuid())
    .bind(r.subject_ref.as_deref())
    .bind(&number)
    .bind(status.as_str())
    .bind(supersedes)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("the candidate already holds a valid grade card".into())
        }
        other => other,
    })?;
    history(
        conn,
        tenant,
        r.institution_id,
        "grade_card",
        id,
        None,
        status.as_str(),
        GRADE_CARD_ISSUED,
        None,
        None,
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    event(
        conn,
        &scope,
        GRADE_CARD_ISSUED,
        GRADE_CARD_ISSUED_SCHEMA,
        id.to_string(),
        &status_payload(
            id,
            r.institution_id,
            Some(r.session_id.as_uuid()),
            Some(r.registration_id.as_uuid()),
            Some(r.student_id.as_uuid()),
            None,
            status.as_str(),
            1,
            Some(number.clone()),
            None,
            r.subject_ref.clone(),
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        &scope,
        "exams.grade_card.issue",
        "grade_card",
        id.to_string(),
        json!({"number": number, "resultId": r.id, "supersedes": supersedes}),
        actor,
    )
    .await?;
    load_card(&mut *conn, tenant, id, false).await
}

/// Issues the grade card of a published result (a valid card of an older revision is replaced
/// by a `reissued` one; the same result twice → 409).
pub async fn issue_card(
    st: &ExamsState,
    tenant: TenantId,
    result: TermResultId,
    actor: &Actor,
) -> Result<GradeCardRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_result(&mut tx, tenant, result, true).await?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    require_institution_writable(st, &scope, r.institution_id).await?;
    if r.status != ResultStatus::Published {
        return Err(KernelError::Conflict(format!(
            "result is {}; grade cards are issued from published results",
            r.status
        )));
    }
    let valid = sqlx::query(&format!(
        "SELECT {CARD_COLUMNS} FROM exams.grade_card WHERE tenant_id = $1 AND registration_id = $2 \
         AND status <> 'withdrawn' FOR UPDATE"
    ))
    .bind(tenant.as_uuid())
    .bind(r.registration_id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .map(|row| card_from_row(&row))
    .transpose()
    .map_err(map_db_error)?;
    let card = match valid {
        Some(c) if c.result_id == r.id => {
            return Err(KernelError::Conflict(format!(
                "grade card {} is already issued for this result",
                c.card_number
            )))
        }
        Some(c) => {
            withdraw_cards_of(
                &mut tx,
                tenant,
                r.registration_id.as_uuid(),
                "replaced by a reissued card",
                actor,
            )
            .await?;
            insert_card(
                &mut tx,
                tenant,
                &r,
                GradeCardStatus::Reissued,
                Some(c.id),
                actor,
            )
            .await?
        }
        None => insert_card(&mut tx, tenant, &r, GradeCardStatus::Issued, None, actor).await?,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(card)
}

/// Reissues a grade card from the candidate's current published result.
pub async fn reissue_card(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<GradeCardRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_card(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, c.version)?;
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    require_institution_writable(st, &scope, c.institution_id).await?;
    let current = current_result(&mut tx, tenant, c.session_id, StudentId(c.student_id))
        .await?
        .filter(|r| r.status == ResultStatus::Published)
        .ok_or_else(|| {
            KernelError::Conflict("the candidate has no published result to reissue from".into())
        })?;
    withdraw_cards_of(
        &mut tx,
        tenant,
        c.registration_id,
        reason.unwrap_or("reissued"),
        actor,
    )
    .await?;
    let card = insert_card(
        &mut tx,
        tenant,
        &current,
        GradeCardStatus::Reissued,
        Some(c.id),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(card)
}

/// Withdraws a grade card.
pub async fn withdraw_card(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    reason: &str,
    actor: &Actor,
) -> Result<GradeCardRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_card(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, c.version)?;
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    require_institution_writable(st, &scope, c.institution_id).await?;
    if c.status == GradeCardStatus::Withdrawn {
        return Err(KernelError::Conflict(
            "the grade card is already withdrawn".into(),
        ));
    }
    withdraw_cards_of(&mut tx, tenant, c.registration_id, reason, actor).await?;
    audit(
        &mut tx,
        &scope,
        "exams.grade_card.withdraw",
        "grade_card",
        c.id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    let c2 = load_card(&mut *tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c2)
}
