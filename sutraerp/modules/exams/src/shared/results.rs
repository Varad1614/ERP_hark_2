//! The **only** writer of `exams.term_result` / `exams.term_result_line` (contract doc §3.3) —
//! slice 6C. Lines are immutable: a recompute, an approved correction or a revaluation outcome
//! inserts a **new** result row (`revision + 1`, `supersedes_id`) and moves the previous one to
//! `superseded` in the same transaction (one current result per candidate and session).
//! Status moves go through [`change_result_status`] (the shared [`ResultStatus`] machine).
//!
//! The foundation `PgResultsPort` (Domain C) reads only `published` rows of this table.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::rows::decode;
use crate::shared::Actor;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::exams::ids::{
    ExamPaperId, ExamRegistrationId, ExamSessionId, GradingSchemeId, TermResultId,
};
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_contracts::exams::status::{AttemptKind, ResultOutcome, ResultStatus};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// One line of a result to insert.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewResultLine {
    pub paper_id: ExamPaperId,
    pub curriculum_component_id: Uuid,
    pub paper_code: String,
    pub title: String,
    pub credits: Decimal,
    pub attempt_kind: AttemptKind,
    pub internal_marks: Option<Decimal>,
    pub external_marks: Option<Decimal>,
    pub grace_marks: Decimal,
    pub total_marks: Option<Decimal>,
    pub max_marks: Decimal,
    pub percent: Option<Decimal>,
    pub grade: Option<String>,
    pub grade_point: Option<Decimal>,
    pub passed: bool,
    pub remark: Option<String>,
}

/// A result to insert.
#[derive(Clone, Debug)]
pub struct NewResult<'a> {
    pub tenant: TenantId,
    pub registration: &'a CandidateSummary,
    pub academic_term_id: Uuid,
    pub grading_scheme_id: GradingSchemeId,
    pub credits_attempted: Decimal,
    pub credits_earned: Decimal,
    pub grade_points: Decimal,
    pub sgpa: Option<Decimal>,
    pub cgpa: Option<Decimal>,
    pub outcome: ResultOutcome,
    /// `computed` normally; `withheld` when a blocking condition applies (L3-I-085).
    pub status: ResultStatus,
    pub status_reason: Option<String>,
    /// 6C result batch (plain id).
    pub result_batch_id: Option<Uuid>,
    pub lines: Vec<NewResultLine>,
}

/// A stored result line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultLineRow {
    pub line_no: i32,
    pub paper_id: ExamPaperId,
    pub curriculum_component_id: Uuid,
    pub paper_code: String,
    pub title: String,
    pub credits: Decimal,
    pub attempt_kind: AttemptKind,
    pub internal_marks: Option<Decimal>,
    pub external_marks: Option<Decimal>,
    pub grace_marks: Decimal,
    pub total_marks: Option<Decimal>,
    pub max_marks: Decimal,
    pub percent: Option<Decimal>,
    pub grade: Option<String>,
    pub grade_point: Option<Decimal>,
    pub passed: bool,
    pub remark: Option<String>,
}

/// A stored term result with its lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResultRow {
    pub id: TermResultId,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub registration_id: ExamRegistrationId,
    pub student_id: StudentId,
    pub academic_term_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub grading_scheme_id: GradingSchemeId,
    pub credits_attempted: Decimal,
    pub credits_earned: Decimal,
    pub grade_points: Decimal,
    pub sgpa: Option<Decimal>,
    pub cgpa: Option<Decimal>,
    pub outcome: ResultOutcome,
    pub status: ResultStatus,
    pub status_reason: Option<String>,
    pub revision: i32,
    pub supersedes_id: Option<TermResultId>,
    pub published_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub lines: Vec<ResultLineRow>,
}

pub const RESULT_COLUMNS: &str = "id, institution_id, session_id, registration_id, student_id, \
    academic_term_id, student_number, display_name, subject_ref, grading_scheme_id, \
    credits_attempted, credits_earned, grade_points, sgpa, cgpa, outcome, status, status_reason, \
    revision, supersedes_id, published_at, version";

const LINE_COLUMNS: &str = "line_no, paper_id, curriculum_component_id, paper_code, title, \
    credits, attempt_kind, internal_marks, external_marks, grace_marks, total_marks, max_marks, \
    percent, grade, grade_point, passed, remark";

/// Result header decoder (`lines` empty).
pub fn result_from_row(r: &PgRow) -> Result<ResultRow, sqlx::Error> {
    Ok(ResultRow {
        id: TermResultId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        registration_id: ExamRegistrationId(r.try_get("registration_id")?),
        student_id: StudentId(r.try_get("student_id")?),
        academic_term_id: r.try_get("academic_term_id")?,
        student_number: r.try_get("student_number")?,
        display_name: r.try_get("display_name")?,
        subject_ref: r.try_get("subject_ref")?,
        grading_scheme_id: GradingSchemeId(r.try_get("grading_scheme_id")?),
        credits_attempted: r.try_get("credits_attempted")?,
        credits_earned: r.try_get("credits_earned")?,
        grade_points: r.try_get("grade_points")?,
        sgpa: r.try_get("sgpa")?,
        cgpa: r.try_get("cgpa")?,
        outcome: decode(r.try_get("outcome")?, ResultOutcome::parse)?,
        status: decode(r.try_get("status")?, ResultStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        revision: r.try_get("revision")?,
        supersedes_id: r
            .try_get::<Option<Uuid>, _>("supersedes_id")?
            .map(TermResultId),
        published_at: r.try_get("published_at")?,
        version: r.try_get("version")?,
        lines: Vec::new(),
    })
}

fn line_from_row(r: &PgRow) -> Result<ResultLineRow, sqlx::Error> {
    Ok(ResultLineRow {
        line_no: r.try_get("line_no")?,
        paper_id: ExamPaperId(r.try_get("paper_id")?),
        curriculum_component_id: r.try_get("curriculum_component_id")?,
        paper_code: r.try_get("paper_code")?,
        title: r.try_get("title")?,
        credits: r.try_get("credits")?,
        attempt_kind: decode(r.try_get("attempt_kind")?, AttemptKind::parse)?,
        internal_marks: r.try_get("internal_marks")?,
        external_marks: r.try_get("external_marks")?,
        grace_marks: r.try_get("grace_marks")?,
        total_marks: r.try_get("total_marks")?,
        max_marks: r.try_get("max_marks")?,
        percent: r.try_get("percent")?,
        grade: r.try_get("grade")?,
        grade_point: r.try_get("grade_point")?,
        passed: r.try_get("passed")?,
        remark: r.try_get("remark")?,
    })
}

/// Loads a result with its lines (404 when missing / foreign). `lock` takes `FOR UPDATE`.
pub async fn load_result(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: TermResultId,
    lock: bool,
) -> Result<ResultRow, KernelError> {
    let sql = format!(
        "SELECT {RESULT_COLUMNS} FROM exams.term_result WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "term result",
        })?;
    let mut r = result_from_row(&row).map_err(map_db_error)?;
    r.lines = sqlx::query(&format!(
        "SELECT {LINE_COLUMNS} FROM exams.term_result_line WHERE result_id = $1 ORDER BY line_no"
    ))
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(line_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)?;
    Ok(r)
}

/// Inserts a result with its lines. When the candidate already has a current result in the
/// session, it is moved to `superseded` first (must be a legal transition, else `409`) and the
/// new row carries `revision + 1` and `supersedes_id`.
pub async fn insert_result(
    conn: &mut PgConnection,
    r: &NewResult<'_>,
    cause_event_type: &'static str,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    let reg = r.registration;
    let current: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.term_result WHERE tenant_id = $1 AND session_id = $2 \
         AND student_id = $3 AND status NOT IN ('superseded', 'archived') FOR UPDATE",
    )
    .bind(r.tenant.as_uuid())
    .bind(reg.session_id.as_uuid())
    .bind(reg.student_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (revision, supersedes) = match current {
        Some(id) => {
            let old = load_result(conn, r.tenant, TermResultId(id), true).await?;
            let old = change_result_status(
                conn,
                r.tenant,
                &old,
                ResultStatus::Superseded,
                &StatusChange {
                    cause_event_type,
                    reason: Some("superseded by a new revision"),
                    process_instance_id: None,
                },
                actor,
            )
            .await?;
            (old.revision + 1, Some(old.id))
        }
        None => (1, None),
    };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.term_result (tenant_id, institution_id, session_id, registration_id, \
         student_id, student_context_id, academic_term_id, student_number, display_name, \
         subject_ref, grading_scheme_id, credits_attempted, credits_earned, grade_points, sgpa, \
         cgpa, outcome, status, status_reason, revision, supersedes_id, result_batch_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, \
         $19, $20, $21, $22) RETURNING id",
    )
    .bind(r.tenant.as_uuid())
    .bind(reg.institution_id.as_uuid())
    .bind(reg.session_id.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(reg.student_id.as_uuid())
    .bind(reg.student_context_id.as_uuid())
    .bind(r.academic_term_id)
    .bind(&reg.student_number)
    .bind(&reg.display_name)
    .bind(reg.subject_ref.as_deref())
    .bind(r.grading_scheme_id.as_uuid())
    .bind(r.credits_attempted)
    .bind(r.credits_earned)
    .bind(r.grade_points)
    .bind(r.sgpa)
    .bind(r.cgpa)
    .bind(r.outcome.as_str())
    .bind(r.status.as_str())
    .bind(r.status_reason.as_deref())
    .bind(revision)
    .bind(supersedes.map(|s| s.as_uuid()))
    .bind(r.result_batch_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (i, l) in r.lines.iter().enumerate() {
        sqlx::query(
            "INSERT INTO exams.term_result_line (tenant_id, result_id, line_no, paper_id, \
             curriculum_component_id, paper_code, title, credits, attempt_kind, internal_marks, \
             external_marks, grace_marks, total_marks, max_marks, percent, grade, grade_point, \
             passed, remark) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, \
             $15, $16, $17, $18, $19)",
        )
        .bind(r.tenant.as_uuid())
        .bind(id)
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(l.paper_id.as_uuid())
        .bind(l.curriculum_component_id)
        .bind(&l.paper_code)
        .bind(&l.title)
        .bind(l.credits)
        .bind(l.attempt_kind.as_str())
        .bind(l.internal_marks)
        .bind(l.external_marks)
        .bind(l.grace_marks)
        .bind(l.total_marks)
        .bind(l.max_marks)
        .bind(l.percent)
        .bind(l.grade.as_deref())
        .bind(l.grade_point)
        .bind(l.passed)
        .bind(l.remark.as_deref())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    record_history(
        conn,
        HistoryEntry {
            tenant: r.tenant,
            institution: reg.institution_id,
            aggregate_type: "term_result",
            aggregate_id: id,
            from: None,
            to: r.status.as_str(),
        },
        &StatusChange {
            cause_event_type,
            reason: r.status_reason.as_deref(),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    load_result(conn, r.tenant, TermResultId(id), false).await
}

/// Moves a **locked** result to `to` (legal transitions of [`ResultStatus`]); stamps
/// `published_at` on publication.
pub async fn change_result_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &ResultRow,
    to: ResultStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<ResultRow, KernelError> {
    if !r.status.can_transition(to) {
        return Err(KernelError::Conflict(format!(
            "result of {} is {}; it cannot become {to}",
            r.student_number, r.status
        )));
    }
    let updated: Option<Uuid> = sqlx::query_scalar(
        "UPDATE exams.term_result SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), \
         published_at = CASE WHEN $3 = 'published' THEN now() ELSE published_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING id",
    )
    .bind(r.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(change.reason)
    .bind(change.process_instance_id)
    .bind(r.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if updated.is_none() {
        return Err(KernelError::Conflict("result changed concurrently".into()));
    }
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: r.institution_id,
            aggregate_type: "term_result",
            aggregate_id: r.id.as_uuid(),
            from: Some(r.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    load_result(conn, tenant, r.id, false).await
}
