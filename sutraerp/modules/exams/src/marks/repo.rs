//! 6B persistence + commands: mark sheets, internal import, marks entry, submission /
//! verification / final marks approval (`MARKS_APPROVAL`, the reference approval of Batch 6),
//! withheld marks, submission exceptions, evaluation rounds, moderation and the gradebook.
//!
//! Candidate values live in the foundation `exams.paper_mark` rows and change **only** through
//! `shared::marks::update`; the paper moves `conducted → evaluation → marks_finalised` only through
//! `shared::lifecycle::change_paper_status`. Every command writes its outbox event + audit entry in
//! the same owner-local transaction. Functions take an [`Actor`] so DB tests can drive them
//! directly (e.g. with `ApprovalMode::AwaitDecision`).

use super::domain::{
    moderate, moderation_can_transition, resolve, sheet_accepts_entry, sheet_can_transition,
    EvaluationMark, ModerationKind, Resolution,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::grading::{fmt2, grade_line, GradedLine, LineInput};
use crate::shared::guard::{require_institution_writable, require_operational};
use crate::shared::lifecycle::{change_paper_status, record_history, HistoryEntry, StatusChange};
use crate::shared::marks::{self as shared_marks, MarkValues};
use crate::shared::publish::{audit, event};
use crate::shared::rows::{decode, lock_paper, mark_from_row, MARK_COLUMNS};
use crate::shared::time::business_today;
use crate::shared::{Actor, ExamsState};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::exams::events::marks::{
    MARKS_APPROVED, MARKS_APPROVED_SCHEMA, MARKS_FINALISED, MARKS_FINALISED_SCHEMA,
    MARKS_MODERATED, MARKS_MODERATED_SCHEMA, MARKS_SUBMITTED, MARKS_SUBMITTED_SCHEMA,
    MARKS_VERIFIED, MARKS_VERIFIED_SCHEMA, MARKS_WITHHELD, MARKS_WITHHELD_SCHEMA,
    SECOND_EVALUATION_REQUIRED, SECOND_EVALUATION_REQUIRED_SCHEMA,
};
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_contracts::exams::ids::{
    EvaluationId, ExamPaperId, ExamRegistrationId, ExamSessionId, MarkSheetId, MarksExceptionId,
    ModerationId, PaperMarkId,
};
use sutra_contracts::exams::ports::{
    ExamPaperSummary, ExamSessionSummary, GradeBand, PaperMarkSummary,
};
use sutra_contracts::exams::status::{
    AttemptKind, EvaluationRound, EvaluationStatus, ExamPaperStatus, ExamRegistrationStatus,
    MarkStatus, ModerationStatus, RegistrationPaperStatus,
};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

// ------------------------------------------------------------------ helpers

pub(crate) fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

fn conflict(msg: impl Into<String>) -> KernelError {
    KernelError::Conflict(msg.into())
}

pub(crate) fn inst_scope(t: TenantId, inst: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(t, inst)
}

/// `StatusChangedV1` skeleton (fill the optional members with struct update syntax).
pub(crate) fn changed(
    aggregate: Uuid,
    inst: InstitutionId,
    from: Option<&str>,
    to: &str,
    version: i64,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id: aggregate,
        institution_id: inst.as_uuid(),
        session_id: None,
        paper_id: None,
        registration_id: None,
        student_id: None,
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        reason: None,
        process_instance_id: None,
        student_subject_ref: None,
    }
}

fn check_version(expected: Option<i64>, actual: i64) -> Result<(), KernelError> {
    match expected {
        Some(e) if e != actual => Err(KernelError::VersionConflict {
            expected: e,
            actual,
        }),
        _ => Ok(()),
    }
}

fn prefixed(cols: &str, alias: &str) -> String {
    cols.split(',')
        .map(|c| format!("{alias}.{}", c.trim()))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Paper (404 when missing / foreign).
pub async fn load_paper(
    st: &ExamsState,
    t: TenantId,
    id: ExamPaperId,
) -> Result<ExamPaperSummary, KernelError> {
    st.directory
        .paper(&ExecutionScope::tenant(t), id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "examination paper",
        })
}

/// Session (404 when missing / foreign).
pub async fn load_session(
    st: &ExamsState,
    t: TenantId,
    id: ExamSessionId,
) -> Result<ExamSessionSummary, KernelError> {
    st.directory
        .session(&ExecutionScope::tenant(t), id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "examination session",
        })
}

/// Grade bands of the session's scheme (`None` when the session has no scheme).
pub async fn bands_of(
    st: &ExamsState,
    t: TenantId,
    session: &ExamSessionSummary,
) -> Result<Option<Vec<GradeBand>>, KernelError> {
    let Some(id) = session.grading_scheme_id else {
        return Ok(None);
    };
    Ok(st
        .directory
        .grading_scheme(&ExecutionScope::tenant(t), id)
        .await?
        .map(|s| s.bands))
}

/// Writable institution + operational session + a paper that accepts marks.
async fn require_markable(
    st: &ExamsState,
    t: TenantId,
    paper: &ExamPaperSummary,
) -> Result<ExamSessionSummary, KernelError> {
    let session = load_session(st, t, paper.session_id).await?;
    require_institution_writable(
        st,
        &inst_scope(t, paper.institution_id),
        paper.institution_id,
    )
    .await?;
    require_operational(&session)?;
    Ok(session)
}

fn require_accepts_marks(paper: &ExamPaperSummary) -> Result<(), KernelError> {
    if paper.status.accepts_marks() {
        Ok(())
    } else {
        Err(conflict(format!(
            "paper {} is {}; marks are collected only after conduct",
            paper.paper_code, paper.status
        )))
    }
}

/// The grading input of one mark row.
pub fn line_input(paper: &ExamPaperSummary, m: &PaperMarkSummary) -> LineInput {
    LineInput {
        credits: paper.credits,
        internal_marks: m.internal_marks,
        internal_max: m.internal_max,
        external_marks: m.external_marks,
        external_max: m.external_max,
        grace_marks: m.grace_marks,
        internal_weight: paper.internal_weight,
        pass_percent: paper.pass_percent,
        absent: m.absent,
    }
}

// ------------------------------------------------------------------ mark sheet rows

/// A mark sheet (one per paper).
#[derive(Clone, Debug)]
pub struct SheetRow {
    pub id: MarkSheetId,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub paper_id: ExamPaperId,
    pub status: EvaluationStatus,
    pub status_reason: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub second_evaluation_required: bool,
    pub difference_threshold: Decimal,
    pub submitted_by: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub approval_requested_at: Option<DateTime<Utc>>,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub finalised_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

const SHEET_COLUMNS: &str = "id, institution_id, session_id, paper_id, status, status_reason, \
    due_on, second_evaluation_required, difference_threshold, submitted_by, submitted_at, \
    verified_by, verified_at, approval_requested_at, approved_by, approved_at, finalised_at, \
    process_instance_id, version";

fn sheet_from_row(r: &PgRow) -> Result<SheetRow, sqlx::Error> {
    Ok(SheetRow {
        id: MarkSheetId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        paper_id: ExamPaperId(r.try_get("paper_id")?),
        status: decode(r.try_get("status")?, EvaluationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        due_on: r.try_get("due_on")?,
        second_evaluation_required: r.try_get("second_evaluation_required")?,
        difference_threshold: r.try_get("difference_threshold")?,
        submitted_by: r.try_get("submitted_by")?,
        submitted_at: r.try_get("submitted_at")?,
        verified_by: r.try_get("verified_by")?,
        verified_at: r.try_get("verified_at")?,
        approval_requested_at: r.try_get("approval_requested_at")?,
        approved_by: r.try_get("approved_by")?,
        approved_at: r.try_get("approved_at")?,
        finalised_at: r.try_get("finalised_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

/// The sheet of a paper, if opened.
pub async fn sheet_of_paper<'c, E>(
    executor: E,
    t: TenantId,
    paper: ExamPaperId,
) -> Result<Option<SheetRow>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query(&format!(
        "SELECT {SHEET_COLUMNS} FROM exams.mark_sheet WHERE tenant_id = $1 AND paper_id = $2"
    ))
    .bind(t.as_uuid())
    .bind(paper.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .map(|r| sheet_from_row(&r))
    .transpose()
    .map_err(map_db_error)
}

/// One sheet (404).
pub async fn get_sheet(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
) -> Result<SheetRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {SHEET_COLUMNS} FROM exams.mark_sheet WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "mark sheet",
    })?;
    sheet_from_row(&row).map_err(map_db_error)
}

async fn lock_sheet(
    conn: &mut PgConnection,
    t: TenantId,
    id: MarkSheetId,
) -> Result<SheetRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {SHEET_COLUMNS} FROM exams.mark_sheet WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "mark sheet",
    })?;
    sheet_from_row(&row).map_err(map_db_error)
}

/// Which audit stamp a sheet move sets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stamp {
    None,
    Submitted,
    Verified,
    ApprovalRequested,
    ApprovalCleared,
    Approved,
    Finalised,
}

impl Stamp {
    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Submitted => "submitted",
            Self::Verified => "verified",
            Self::ApprovalRequested => "approval_requested",
            Self::ApprovalCleared => "approval_cleared",
            Self::Approved => "approved",
            Self::Finalised => "finalised",
        }
    }
}

/// Moves a locked sheet (legal transition, or the same status for approval stamps).
#[allow(clippy::too_many_arguments)]
async fn set_sheet(
    conn: &mut PgConnection,
    t: TenantId,
    s: &SheetRow,
    to: EvaluationStatus,
    stamp: Stamp,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    if to != s.status && !sheet_can_transition(s.status, to) {
        return Err(conflict(format!(
            "the mark sheet is {}; it cannot become {to}",
            s.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.mark_sheet SET status = $3, status_reason = $4, \
         submitted_by = CASE WHEN $5::text = 'submitted' THEN $6 ELSE submitted_by END, \
         submitted_at = CASE WHEN $5::text = 'submitted' THEN now() ELSE submitted_at END, \
         verified_by = CASE WHEN $5::text = 'verified' THEN $6 ELSE verified_by END, \
         verified_at = CASE WHEN $5::text = 'verified' THEN now() ELSE verified_at END, \
         approval_requested_at = CASE WHEN $5::text = 'approval_requested' THEN now() \
             WHEN $5::text IN ('approved', 'approval_cleared') THEN NULL ELSE approval_requested_at END, \
         approved_by = CASE WHEN $5::text = 'approved' THEN $6 ELSE approved_by END, \
         approved_at = CASE WHEN $5::text = 'approved' THEN now() ELSE approved_at END, \
         finalised_at = CASE WHEN $5::text = 'finalised' THEN now() ELSE finalised_at END, \
         process_instance_id = COALESCE($7, process_instance_id), \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $8 RETURNING {SHEET_COLUMNS}"
    ))
    .bind(s.id.as_uuid())
    .bind(t.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(stamp.as_str())
    .bind(actor.id.as_deref())
    .bind(process_instance_id)
    .bind(s.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| conflict("the mark sheet changed concurrently"))?;
    if to != s.status {
        record_history(
            conn,
            HistoryEntry {
                tenant: t,
                institution: s.institution_id,
                aggregate_type: "mark_sheet",
                aggregate_id: s.id.as_uuid(),
                from: Some(s.status.as_str()),
                to: to.as_str(),
            },
            &StatusChange {
                cause_event_type: "",
                reason,
                process_instance_id,
            },
            actor,
        )
        .await?;
    }
    sheet_from_row(&row).map_err(map_db_error)
}

/// Settings of a mark sheet (policy for evaluation rounds, submission deadline).
#[derive(Clone, Debug, Default)]
pub struct SheetSettings {
    pub due_on: Option<NaiveDate>,
    pub second_evaluation_required: Option<bool>,
    pub difference_threshold: Option<Decimal>,
}

fn validate_settings(s: &SheetSettings) -> Result<(), KernelError> {
    if let Some(th) = s.difference_threshold {
        if th < Decimal::ZERO || th.normalize().scale() > 2 {
            return Err(invalid(
                "differenceThreshold",
                "out_of_range",
                "must be zero or positive, with at most two decimals",
            ));
        }
    }
    Ok(())
}

/// Opens the paper's sheet in the caller's transaction when missing (the paper moves
/// `conducted → evaluation`); returns the (possibly existing) sheet.
async fn ensure_sheet(
    conn: &mut PgConnection,
    t: TenantId,
    paper: &ExamPaperSummary,
    settings: &SheetSettings,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let locked = lock_paper(conn, t, paper.id).await?;
    if let Some(s) = sheet_of_paper(&mut *conn, t, paper.id).await? {
        return Ok(s);
    }
    if locked.status == ExamPaperStatus::Conducted {
        change_paper_status(
            conn,
            t,
            &locked,
            ExamPaperStatus::Evaluation,
            &StatusChange {
                cause_event_type: "",
                reason: Some("marks collection opened"),
                process_instance_id: None,
            },
            actor,
        )
        .await?;
    }
    let row = sqlx::query(&format!(
        "INSERT INTO exams.mark_sheet (tenant_id, institution_id, session_id, paper_id, due_on, \
         second_evaluation_required, difference_threshold, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {SHEET_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(paper.institution_id.as_uuid())
    .bind(paper.session_id.as_uuid())
    .bind(paper.id.as_uuid())
    .bind(settings.due_on)
    .bind(settings.second_evaluation_required.unwrap_or(false))
    .bind(settings.difference_threshold.unwrap_or(Decimal::TEN))
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let sheet = sheet_from_row(&row).map_err(map_db_error)?;
    record_history(
        conn,
        HistoryEntry {
            tenant: t,
            institution: sheet.institution_id,
            aggregate_type: "mark_sheet",
            aggregate_id: sheet.id.as_uuid(),
            from: None,
            to: sheet.status.as_str(),
        },
        &StatusChange {
            cause_event_type: "",
            reason: Some("opened"),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(sheet)
}

/// Opens a paper's mark sheet (409 when already open or the paper does not accept marks).
pub async fn open_sheet(
    st: &ExamsState,
    t: TenantId,
    paper_id: ExamPaperId,
    settings: SheetSettings,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    validate_settings(&settings)?;
    let paper = load_paper(st, t, paper_id).await?;
    require_markable(st, t, &paper).await?;
    require_accepts_marks(&paper)?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    if sheet_of_paper(&mut *tx, t, paper_id).await?.is_some() {
        return Err(conflict("the paper's mark sheet is already open"));
    }
    let sheet = ensure_sheet(&mut tx, t, &paper, &settings, actor).await?;
    audit(
        &mut tx,
        &inst_scope(t, paper.institution_id),
        "exams.mark_sheet.open",
        "mark_sheet",
        sheet.id.to_string(),
        json!({"paperId": paper.id, "secondEvaluationRequired": sheet.second_evaluation_required}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(sheet)
}

/// Changes the sheet's policy settings (before submission).
pub async fn update_sheet(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
    expected: Option<i64>,
    settings: SheetSettings,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    validate_settings(&settings)?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_sheet(&mut tx, t, id).await?;
    check_version(expected, s.version)?;
    if !sheet_accepts_entry(s.status) {
        return Err(conflict(format!(
            "the mark sheet is {}; its settings are fixed",
            s.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.mark_sheet SET due_on = COALESCE($3, due_on), \
         second_evaluation_required = COALESCE($4, second_evaluation_required), \
         difference_threshold = COALESCE($5, difference_threshold), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {SHEET_COLUMNS}"
    ))
    .bind(id.as_uuid())
    .bind(t.as_uuid())
    .bind(settings.due_on)
    .bind(settings.second_evaluation_required)
    .bind(settings.difference_threshold)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let sheet = sheet_from_row(&row).map_err(map_db_error)?;
    audit(
        &mut tx,
        &inst_scope(t, s.institution_id),
        "exams.mark_sheet.update",
        "mark_sheet",
        id.to_string(),
        json!({"dueOn": sheet.due_on, "secondEvaluationRequired": sheet.second_evaluation_required,
               "differenceThreshold": fmt2(sheet.difference_threshold)}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(sheet)
}

// ------------------------------------------------------------------ candidates (mark rows)

/// One candidate's mark row with the registration facts the screens need.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub mark: PaperMarkSummary,
    pub registration_number: String,
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub registration_status: ExamRegistrationStatus,
    pub paper_status: RegistrationPaperStatus,
    pub attempt_kind: AttemptKind,
    pub attempt_no: i32,
}

impl Candidate {
    /// An effective registration whose paper is still registered (not withdrawn).
    pub fn effective(&self) -> bool {
        self.registration_status.is_effective()
            && self.paper_status != RegistrationPaperStatus::Withdrawn
    }
}

fn candidate_from_row(r: &PgRow) -> Result<Candidate, sqlx::Error> {
    let paper_status = decode(r.try_get("paper_status")?, RegistrationPaperStatus::parse)?;
    let mut mark = mark_from_row(r)?;
    // Integrator: a candidate 6A recorded `absent` at conduct (registered paper status) shows as
    // absent on mark sheets and in the gradebook even before anyone ticks the mark row.
    if paper_status == RegistrationPaperStatus::Absent {
        mark.absent = true;
        mark.external_marks = None;
    }
    Ok(Candidate {
        mark,
        registration_number: r.try_get("registration_number")?,
        student_number: r.try_get("student_number")?,
        display_name: r.try_get("display_name")?,
        subject_ref: r.try_get("subject_ref")?,
        registration_status: decode(
            r.try_get("registration_status")?,
            ExamRegistrationStatus::parse,
        )?,
        paper_status,
        attempt_kind: decode(r.try_get("attempt_kind")?, AttemptKind::parse)?,
        attempt_no: r.try_get("attempt_no")?,
    })
}

fn candidate_sql(filter: &str) -> String {
    format!(
        "SELECT {}, r.registration_number, r.student_number, r.display_name, r.subject_ref, \
         r.status AS registration_status, rp.status AS paper_status, rp.attempt_kind, rp.attempt_no \
         FROM exams.paper_mark m \
         JOIN exams.exam_registration r ON r.id = m.registration_id \
         JOIN exams.exam_registration_paper rp ON rp.id = m.registration_paper_id \
         WHERE m.tenant_id = $1 AND {filter} ORDER BY r.registration_number, m.id",
        prefixed(MARK_COLUMNS, "m")
    )
}

/// Every candidate row of a paper (any registration status), ordered by registration number.
pub async fn candidates<'c, E>(
    executor: E,
    t: TenantId,
    paper: ExamPaperId,
) -> Result<Vec<Candidate>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query(&candidate_sql("m.paper_id = $2"))
        .bind(t.as_uuid())
        .bind(paper.as_uuid())
        .fetch_all(executor)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(candidate_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// One candidate row by mark id (404).
pub async fn candidate<'c, E>(
    executor: E,
    t: TenantId,
    mark: PaperMarkId,
) -> Result<Candidate, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let row = sqlx::query(&candidate_sql("m.id = $2"))
        .bind(t.as_uuid())
        .bind(mark.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "marks" })?;
    candidate_from_row(&row).map_err(map_db_error)
}

/// Marks a candidate row through the shared machine and returns the updated row.
#[allow(clippy::too_many_arguments)]
async fn step(
    conn: &mut PgConnection,
    t: TenantId,
    m: &PaperMarkSummary,
    values: Option<MarkValues>,
    to: MarkStatus,
    cause: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<PaperMarkSummary, KernelError> {
    let locked = shared_marks::lock_mark(conn, t, m.id).await?;
    shared_marks::update(conn, t, &locked, values, to, cause, reason, actor).await
}

// ------------------------------------------------------------------ marks entry

/// One row of a bulk marks entry.
#[derive(Clone, Debug)]
pub struct EntryRow {
    pub id: PaperMarkId,
    pub version: i64,
    pub external_marks: Option<Decimal>,
    pub internal_marks: Option<Decimal>,
    pub internal_max: Option<Decimal>,
    pub absent: Option<bool>,
}

/// Enters marks (`→ entered`) for candidates of a paper; the sheet opens on first entry.
pub async fn enter_marks(
    st: &ExamsState,
    t: TenantId,
    paper_id: ExamPaperId,
    rows: Vec<EntryRow>,
    actor: &Actor,
) -> Result<Vec<Candidate>, KernelError> {
    if rows.is_empty() {
        return Err(invalid("rows", "required", "at least one row is required"));
    }
    let paper = load_paper(st, t, paper_id).await?;
    require_markable(st, t, &paper).await?;
    require_accepts_marks(&paper)?;
    let current: HashMap<Uuid, Candidate> = candidates(&st.pool, t, paper_id)
        .await?
        .into_iter()
        .map(|c| (c.mark.id.as_uuid(), c))
        .collect();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut sheet = ensure_sheet(&mut tx, t, &paper, &SheetSettings::default(), actor).await?;
    if !sheet_accepts_entry(sheet.status) {
        return Err(conflict(format!(
            "the mark sheet is {}; marks can no longer be entered",
            sheet.status
        )));
    }
    if sheet.status != EvaluationStatus::InEvaluation {
        sheet = set_sheet(
            &mut tx,
            t,
            &sheet,
            EvaluationStatus::InEvaluation,
            Stamp::None,
            None,
            None,
            actor,
        )
        .await?;
    }
    for (i, row) in rows.iter().enumerate() {
        let c = current
            .get(&row.id.as_uuid())
            .ok_or(KernelError::NotFound { entity: "marks" })?;
        if !c.effective() {
            return Err(conflict(format!(
                "candidate {} is not effectively registered for this paper",
                c.student_number
            )));
        }
        let m = shared_marks::lock_mark(&mut tx, t, row.id).await?;
        check_version(Some(row.version), m.version)?;
        if !m.status.is_editable() {
            return Err(conflict(format!(
                "marks of {} are {}; they can no longer be edited",
                c.student_number, m.status
            )));
        }
        let mut v = MarkValues::of(&m);
        // Absent at conduct (6A) stays absent: no external marks for a script that was not written.
        v.absent =
            c.paper_status == RegistrationPaperStatus::Absent || row.absent.unwrap_or(m.absent);
        v.external_marks = if v.absent {
            None
        } else {
            row.external_marks.or(m.external_marks)
        };
        if let Some(im) = row.internal_marks {
            v.internal_marks = Some(im);
            v.internal_max = row.internal_max.or(m.internal_max);
            if v.internal_marks != m.internal_marks || v.internal_max != m.internal_max {
                v.internal_source_id = None;
            }
        }
        shared_marks::update(
            &mut tx,
            t,
            &m,
            Some(v),
            MarkStatus::Entered,
            "entry",
            None,
            actor,
        )
        .await
        .map_err(|e| match e {
            KernelError::Validation(vs) => KernelError::Validation(
                vs.into_iter()
                    .map(|f| {
                        FieldViolation::new(format!("rows[{i}].{}", f.field), &f.code, &f.message)
                    })
                    .collect(),
            ),
            other => other,
        })?;
    }
    audit(
        &mut tx,
        &inst_scope(t, paper.institution_id),
        "exams.paper_mark.enter",
        "exam_paper",
        paper.id.to_string(),
        json!({"rows": rows.len(), "markSheetId": sheet.id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    candidates(&st.pool, t, paper_id).await
}

// ------------------------------------------------------------------ internal import

/// Summary of a bulk command.
#[derive(Clone, Debug, Default)]
pub struct BulkSummary {
    pub processed: usize,
    pub skipped: usize,
    pub failed: Vec<(Uuid, String)>,
    /// Student numbers without a finalised internal assessment result (import only).
    pub missing: Vec<String>,
}

/// Imports finalised internal assessment (Batch 4) into the candidates' internal marks; a
/// re-import after a 4B correction updates rows that are not final yet.
pub async fn import_internal(
    st: &ExamsState,
    t: TenantId,
    sheet_id: MarkSheetId,
    actor: &Actor,
) -> Result<BulkSummary, KernelError> {
    let sheet = get_sheet(st, t, sheet_id).await?;
    let paper = load_paper(st, t, sheet.paper_id).await?;
    require_markable(st, t, &paper).await?;
    if matches!(
        sheet.status,
        EvaluationStatus::Closed | EvaluationStatus::Archived
    ) {
        return Err(conflict(format!("the mark sheet is {}", sheet.status)));
    }
    let Some(offering) = paper.offering_id else {
        return Err(invalid(
            "offeringId",
            "required",
            "the paper is not linked to a course offering; there is no internal assessment to import",
        ));
    };
    let scope = inst_scope(t, paper.institution_id);
    let facts = st
        .academic
        .internal_results(&scope, CourseOfferingId(offering))
        .await?;
    let mut latest: HashMap<Uuid, &sutra_contracts::academic::ports::InternalAssessmentFact> =
        HashMap::new();
    for f in &facts {
        let k = f.student_id.as_uuid();
        if latest
            .get(&k)
            .is_none_or(|cur| cur.result_version < f.result_version)
        {
            latest.insert(k, f);
        }
    }
    let rows = candidates(&st.pool, t, paper.id).await?;
    let mut summary = BulkSummary::default();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    for c in rows.iter().filter(|c| c.effective()) {
        if c.mark.status.is_final() {
            summary.skipped += 1;
            continue;
        }
        let Some(f) = latest.get(&c.mark.student_id.as_uuid()) else {
            summary.missing.push(c.student_number.clone());
            continue;
        };
        let m = shared_marks::lock_mark(&mut tx, t, c.mark.id).await?;
        if m.internal_marks == Some(f.marks_obtained)
            && m.internal_max == Some(f.max_marks)
            && m.internal_source_id == Some(f.result_id)
        {
            summary.skipped += 1;
            continue;
        }
        let mut v = MarkValues::of(&m);
        v.internal_marks = Some(f.marks_obtained);
        v.internal_max = Some(f.max_marks);
        v.internal_source_id = Some(f.result_id);
        let reason = format!("internal assessment result v{}", f.result_version);
        match shared_marks::update(
            &mut tx,
            t,
            &m,
            Some(v),
            m.status,
            "import",
            Some(reason.as_str()),
            actor,
        )
        .await
        {
            Ok(_) => summary.processed += 1,
            Err(KernelError::Validation(vs)) => summary.failed.push((
                c.mark.registration_id.as_uuid(),
                vs.first()
                    .map(|v| v.message.clone())
                    .unwrap_or_else(|| "invalid internal marks".into()),
            )),
            Err(e) => return Err(e),
        }
    }
    audit(
        &mut tx,
        &scope,
        "exams.mark_sheet.import_internal",
        "mark_sheet",
        sheet.id.to_string(),
        json!({"processed": summary.processed, "skipped": summary.skipped,
               "failed": summary.failed.len(), "missing": summary.missing}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(summary)
}

// ------------------------------------------------------------------ submission exceptions

/// A marks submission exception (late / missing marks, L3-I-074).
#[derive(Clone, Debug)]
pub struct ExceptionRow {
    pub id: MarksExceptionId,
    pub mark_sheet_id: MarkSheetId,
    pub paper_id: ExamPaperId,
    pub paper_mark_id: Option<PaperMarkId>,
    pub kind: String,
    pub reason: String,
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const EXCEPTION_COLUMNS: &str = "id, mark_sheet_id, paper_id, paper_mark_id, kind, reason, \
    status, resolution, resolved_by, version, created_at";

fn exception_from_row(r: &PgRow) -> Result<ExceptionRow, sqlx::Error> {
    Ok(ExceptionRow {
        id: MarksExceptionId(r.try_get("id")?),
        mark_sheet_id: MarkSheetId(r.try_get("mark_sheet_id")?),
        paper_id: ExamPaperId(r.try_get("paper_id")?),
        paper_mark_id: r
            .try_get::<Option<Uuid>, _>("paper_mark_id")?
            .map(PaperMarkId),
        kind: r.try_get("kind")?,
        reason: r.try_get("reason")?,
        status: r.try_get("status")?,
        resolution: r.try_get("resolution")?,
        resolved_by: r.try_get("resolved_by")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Exceptions of a sheet, newest first.
pub async fn exceptions_of<'c, E>(
    executor: E,
    t: TenantId,
    sheet: MarkSheetId,
) -> Result<Vec<ExceptionRow>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query(&format!(
        "SELECT {EXCEPTION_COLUMNS} FROM exams.marks_exception WHERE tenant_id = $1 \
         AND mark_sheet_id = $2 ORDER BY created_at DESC, id DESC"
    ))
    .bind(t.as_uuid())
    .bind(sheet.as_uuid())
    .fetch_all(executor)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(exception_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

/// Raises a submission exception (`late_submission` for the sheet, `missing_marks` for one row).
pub async fn raise_exception(
    st: &ExamsState,
    t: TenantId,
    sheet_id: MarkSheetId,
    kind: &str,
    paper_mark: Option<PaperMarkId>,
    reason: &str,
    actor: &Actor,
) -> Result<ExceptionRow, KernelError> {
    if !matches!(kind, "late_submission" | "missing_marks") {
        return Err(invalid(
            "kind",
            "invalid",
            "must be late_submission or missing_marks",
        ));
    }
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(invalid("reason", "required", "an exception needs a reason"));
    }
    let sheet = get_sheet(st, t, sheet_id).await?;
    if kind == "missing_marks" {
        let Some(mid) = paper_mark else {
            return Err(invalid(
                "paperMarkId",
                "required",
                "a missing-marks exception names the candidate's marks row",
            ));
        };
        let c = candidate(&st.pool, t, mid).await?;
        if c.mark.paper_id != sheet.paper_id {
            return Err(KernelError::NotFound { entity: "marks" });
        }
    }
    let open: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.marks_exception WHERE tenant_id = $1 AND mark_sheet_id = $2 \
         AND kind = $3 AND paper_mark_id IS NOT DISTINCT FROM $4 AND status = 'raised'",
    )
    .bind(t.as_uuid())
    .bind(sheet_id.as_uuid())
    .bind(kind)
    .bind(paper_mark.map(|m| m.as_uuid()))
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    if open.is_some() {
        return Err(conflict("an identical exception is already open"));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "INSERT INTO exams.marks_exception (tenant_id, institution_id, session_id, paper_id, \
         mark_sheet_id, paper_mark_id, kind, reason, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {EXCEPTION_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(sheet.institution_id.as_uuid())
    .bind(sheet.session_id.as_uuid())
    .bind(sheet.paper_id.as_uuid())
    .bind(sheet_id.as_uuid())
    .bind(paper_mark.map(|m| m.as_uuid()))
    .bind(kind)
    .bind(reason)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let exc = exception_from_row(&row).map_err(map_db_error)?;
    audit(
        &mut tx,
        &inst_scope(t, sheet.institution_id),
        "exams.marks_exception.raise",
        "marks_exception",
        exc.id.to_string(),
        json!({"kind": kind, "markSheetId": sheet_id, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(exc)
}

/// Accepts or rejects a raised exception.
pub async fn resolve_exception(
    st: &ExamsState,
    t: TenantId,
    id: MarksExceptionId,
    expected: Option<i64>,
    accept: bool,
    resolution: Option<&str>,
    actor: &Actor,
) -> Result<ExceptionRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = sqlx::query(&format!(
        "SELECT {EXCEPTION_COLUMNS}, institution_id FROM exams.marks_exception \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "marks exception",
    })?;
    let inst = InstitutionId(cur.try_get("institution_id").map_err(map_db_error)?);
    let exc = exception_from_row(&cur).map_err(map_db_error)?;
    check_version(expected, exc.version)?;
    if exc.status != "raised" {
        return Err(conflict(format!("the exception is already {}", exc.status)));
    }
    let to = if accept { "accepted" } else { "rejected" };
    let row = sqlx::query(&format!(
        "UPDATE exams.marks_exception SET status = $3, resolution = $4, resolved_by = $5, \
         resolved_at = now(), version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 RETURNING {EXCEPTION_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .bind(to)
    .bind(resolution)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let updated = exception_from_row(&row).map_err(map_db_error)?;
    record_history(
        &mut tx,
        HistoryEntry {
            tenant: t,
            institution: inst,
            aggregate_type: "marks_exception",
            aggregate_id: id.as_uuid(),
            from: Some("raised"),
            to,
        },
        &StatusChange {
            cause_event_type: "",
            reason: resolution,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &inst_scope(t, inst),
        "exams.marks_exception.resolve",
        "marks_exception",
        id.to_string(),
        json!({"decision": to, "resolution": resolution}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

// ------------------------------------------------------------------ submit / return / verify

async fn withhold_in_tx(
    conn: &mut PgConnection,
    t: TenantId,
    c: &Candidate,
    reason: &str,
    actor: &Actor,
) -> Result<PaperMarkSummary, KernelError> {
    let m = step(
        conn,
        t,
        &c.mark,
        None,
        MarkStatus::Withheld,
        "withhold",
        Some(reason),
        actor,
    )
    .await?;
    let inst = institution_of_mark(conn, t, m.id).await?;
    event(
        conn,
        &inst_scope(t, inst),
        MARKS_WITHHELD,
        MARKS_WITHHELD_SCHEMA,
        m.id.to_string(),
        &StatusChangedV1 {
            session_id: Some(m.session_id.as_uuid()),
            paper_id: Some(m.paper_id.as_uuid()),
            registration_id: Some(m.registration_id.as_uuid()),
            student_id: Some(m.student_id.as_uuid()),
            reason: Some(reason.to_owned()),
            student_subject_ref: c.subject_ref.clone(),
            ..changed(
                m.id.as_uuid(),
                inst,
                Some(c.mark.status.as_str()),
                "withheld",
                m.version,
            )
        },
        actor,
    )
    .await?;
    Ok(m)
}

async fn institution_of_mark(
    conn: &mut PgConnection,
    t: TenantId,
    id: PaperMarkId,
) -> Result<InstitutionId, KernelError> {
    let inst: Uuid = sqlx::query_scalar(
        "SELECT institution_id FROM exams.paper_mark WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(InstitutionId(inst))
}

fn sheet_event(s: &SheetRow, from: EvaluationStatus) -> StatusChangedV1 {
    StatusChangedV1 {
        session_id: Some(s.session_id.as_uuid()),
        paper_id: Some(s.paper_id.as_uuid()),
        reason: s.status_reason.clone(),
        process_instance_id: s.process_instance_id.clone(),
        ..changed(
            s.id.as_uuid(),
            s.institution_id,
            Some(from.as_str()),
            s.status.as_str(),
            s.version,
        )
    }
}

/// Evaluator submission (`in_evaluation → evaluated`, rows `entered → submitted`, L3-I-071).
/// Pending candidates block it unless an accepted missing-marks exception withholds them
/// (L3-I-074); a submission after the deadline needs an accepted late-submission exception.
pub async fn submit_sheet(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
    expected: Option<i64>,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let sheet = get_sheet(st, t, id).await?;
    let paper = load_paper(st, t, sheet.paper_id).await?;
    require_markable(st, t, &paper).await?;
    let scope = inst_scope(t, sheet.institution_id);
    let today = business_today(st, &scope, sheet.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_sheet(&mut tx, t, id).await?;
    check_version(expected, s.version)?;
    if s.status != EvaluationStatus::InEvaluation {
        return Err(conflict(format!(
            "the mark sheet is {}; only a sheet in evaluation is submitted",
            s.status
        )));
    }
    let excs = exceptions_of(&mut *tx, t, id).await?;
    if s.due_on.is_some_and(|d| today > d)
        && !excs
            .iter()
            .any(|e| e.kind == "late_submission" && e.status == "accepted")
    {
        return Err(conflict(
            "the submission deadline has passed; a late-submission exception must be accepted first",
        ));
    }
    let rows = candidates(&mut *tx, t, s.paper_id).await?;
    let mut missing = Vec::new();
    let mut moved = 0usize;
    for c in rows.iter().filter(|c| c.effective()) {
        match c.mark.status {
            // Absent at conduct (6A registered paper status): recorded as absent, no script.
            MarkStatus::Pending if c.paper_status == RegistrationPaperStatus::Absent => {
                let v = MarkValues::of(&c.mark);
                let entered = step(
                    &mut tx,
                    t,
                    &c.mark,
                    Some(v),
                    MarkStatus::Entered,
                    "entry",
                    Some("absent at the examination"),
                    actor,
                )
                .await?;
                step(
                    &mut tx,
                    t,
                    &entered,
                    None,
                    MarkStatus::Submitted,
                    "submit",
                    None,
                    actor,
                )
                .await?;
                moved += 1;
            }
            MarkStatus::Pending => {
                let excused = excs.iter().find(|e| {
                    e.kind == "missing_marks"
                        && e.status == "accepted"
                        && e.paper_mark_id == Some(c.mark.id)
                });
                match excused {
                    Some(e) => {
                        withhold_in_tx(&mut tx, t, c, &e.reason, actor).await?;
                    }
                    None => missing.push(c.student_number.clone()),
                }
            }
            MarkStatus::Entered => {
                step(
                    &mut tx,
                    t,
                    &c.mark,
                    None,
                    MarkStatus::Submitted,
                    "submit",
                    None,
                    actor,
                )
                .await?;
                moved += 1;
            }
            _ => {}
        }
    }
    if !missing.is_empty() {
        return Err(conflict(format!(
            "{} candidate(s) have no marks: {}",
            missing.len(),
            missing.join(", ")
        )));
    }
    let updated = set_sheet(
        &mut tx,
        t,
        &s,
        EvaluationStatus::Evaluated,
        Stamp::Submitted,
        None,
        None,
        actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        MARKS_SUBMITTED,
        MARKS_SUBMITTED_SCHEMA,
        id.to_string(),
        &sheet_event(&updated, s.status),
        actor,
    )
    .await?;
    if updated.second_evaluation_required {
        event(
            &mut tx,
            &scope,
            SECOND_EVALUATION_REQUIRED,
            SECOND_EVALUATION_REQUIRED_SCHEMA,
            id.to_string(),
            &StatusChangedV1 {
                reason: Some("policy requires a second evaluation of every script".into()),
                ..sheet_event(&updated, s.status)
            },
            actor,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.mark_sheet.submit",
        "mark_sheet",
        id.to_string(),
        json!({"submitted": moved, "paperId": s.paper_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Returns a submitted sheet to the evaluator (`evaluated → in_evaluation`, rows back to `entered`).
pub async fn return_sheet(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
    expected: Option<i64>,
    reason: &str,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(invalid(
            "reason",
            "required",
            "returning marks needs a reason",
        ));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_sheet(&mut tx, t, id).await?;
    check_version(expected, s.version)?;
    if s.status != EvaluationStatus::Evaluated {
        return Err(conflict(format!(
            "the mark sheet is {}; only submitted marks are returned",
            s.status
        )));
    }
    for c in candidates(&mut *tx, t, s.paper_id)
        .await?
        .iter()
        .filter(|c| c.mark.status == MarkStatus::Submitted)
    {
        step(
            &mut tx,
            t,
            &c.mark,
            None,
            MarkStatus::Entered,
            "return",
            Some(reason),
            actor,
        )
        .await?;
    }
    let updated = set_sheet(
        &mut tx,
        t,
        &s,
        EvaluationStatus::InEvaluation,
        Stamp::None,
        Some(reason),
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &inst_scope(t, s.institution_id),
        "exams.mark_sheet.return",
        "mark_sheet",
        id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Exam-cell verification (`evaluated → validated`, rows `submitted → verified`, L3-I-072).
/// When policy requires a second evaluation, every gradable script must be resolved first.
pub async fn verify_sheet(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
    expected: Option<i64>,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let sheet = get_sheet(st, t, id).await?;
    let paper = load_paper(st, t, sheet.paper_id).await?;
    require_markable(st, t, &paper).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_sheet(&mut tx, t, id).await?;
    check_version(expected, s.version)?;
    if s.status != EvaluationStatus::Evaluated {
        return Err(conflict(format!(
            "the mark sheet is {}; only submitted marks are verified",
            s.status
        )));
    }
    let rows = candidates(&mut *tx, t, s.paper_id).await?;
    if s.second_evaluation_required {
        let evals = evaluations_by_mark(&mut tx, t, s.paper_id).await?;
        let unresolved: Vec<String> = rows
            .iter()
            .filter(|c| c.effective() && c.mark.status == MarkStatus::Submitted && !c.mark.absent)
            .filter(|c| {
                let marks = evals.get(&c.mark.id.as_uuid()).cloned().unwrap_or_default();
                !matches!(
                    resolve(&marks, true, s.difference_threshold),
                    Resolution::Resolved(_)
                )
            })
            .map(|c| c.student_number.clone())
            .collect();
        if !unresolved.is_empty() {
            return Err(conflict(format!(
                "policy requires a second (or adjudicating) evaluation; unresolved: {}",
                unresolved.join(", ")
            )));
        }
    }
    let mut moved = 0usize;
    for c in rows
        .iter()
        .filter(|c| c.effective() && c.mark.status == MarkStatus::Submitted)
    {
        step(
            &mut tx,
            t,
            &c.mark,
            None,
            MarkStatus::Verified,
            "verify",
            None,
            actor,
        )
        .await?;
        moved += 1;
    }
    let updated = set_sheet(
        &mut tx,
        t,
        &s,
        EvaluationStatus::Validated,
        Stamp::Verified,
        None,
        None,
        actor,
    )
    .await?;
    let scope = inst_scope(t, s.institution_id);
    event(
        &mut tx,
        &scope,
        MARKS_VERIFIED,
        MARKS_VERIFIED_SCHEMA,
        id.to_string(),
        &sheet_event(&updated, s.status),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.mark_sheet.verify",
        "mark_sheet",
        id.to_string(),
        json!({"verified": moved}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

// ------------------------------------------------------------------ final marks approval

async fn apply_marks_approval(
    conn: &mut PgConnection,
    t: TenantId,
    s: &SheetRow,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let mut approved = 0usize;
    for c in candidates(&mut *conn, t, s.paper_id)
        .await?
        .iter()
        .filter(|c| {
            c.effective() && matches!(c.mark.status, MarkStatus::Verified | MarkStatus::Moderated)
        })
    {
        step(
            conn,
            t,
            &c.mark,
            None,
            MarkStatus::Approved,
            "approve",
            reason,
            actor,
        )
        .await?;
        approved += 1;
    }
    let updated = set_sheet(
        conn,
        t,
        s,
        EvaluationStatus::Approved,
        Stamp::Approved,
        reason,
        None,
        actor,
    )
    .await?;
    let scope = inst_scope(t, s.institution_id);
    event(
        conn,
        &scope,
        MARKS_APPROVED,
        MARKS_APPROVED_SCHEMA,
        s.id.to_string(),
        &sheet_event(&updated, s.status),
        actor,
    )
    .await?;
    audit(
        conn,
        &scope,
        "exams.mark_sheet.approve",
        "mark_sheet",
        s.id.to_string(),
        json!({"approved": approved, "processInstanceId": updated.process_instance_id}),
        actor,
    )
    .await?;
    Ok(updated)
}

/// Requests final marks approval (`MARKS_APPROVAL`). Auto-approval applies the decision in the
/// same transaction (rows → `approved`, sheet → `approved`); otherwise the sheet stays
/// `validated` with the process instance until `decide_sheet`.
pub async fn request_marks_approval(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
    expected: Option<i64>,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let s = get_sheet(st, t, id).await?;
    check_version(expected, s.version)?;
    if s.status != EvaluationStatus::Validated {
        return Err(conflict(format!(
            "the mark sheet is {}; only verified marks go to approval",
            s.status
        )));
    }
    if s.approval_requested_at.is_some() {
        return Err(conflict("final marks approval is already requested"));
    }
    let paper = load_paper(st, t, s.paper_id).await?;
    require_markable(st, t, &paper).await?;
    let scope = inst_scope(t, s.institution_id);
    let mut vars = BTreeMap::new();
    vars.insert("paperId".to_owned(), json!(s.paper_id));
    vars.insert("paperCode".to_owned(), json!(paper.paper_code));
    let outcome = request_approval(
        st,
        &scope,
        process::MARKS_APPROVAL,
        business_key("mark_sheet", id, s.version),
        vars,
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_sheet(&mut tx, t, id).await?;
    check_version(Some(s.version), locked.version)?;
    let pending = set_sheet(
        &mut tx,
        t,
        &locked,
        EvaluationStatus::Validated,
        Stamp::ApprovalRequested,
        None,
        Some(outcome.process_instance_id()),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.mark_sheet.request_approval",
        "mark_sheet",
        id.to_string(),
        json!({"processInstanceId": outcome.process_instance_id()}),
        actor,
    )
    .await?;
    let result = match outcome {
        ApprovalOutcome::Approved { .. } => {
            apply_marks_approval(
                &mut tx,
                t,
                &pending,
                Some("auto-approved"),
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => pending,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(result)
}

/// Explicit decision on a pending final marks approval (`exam.approve`): approve → rows and
/// sheet `approved`; reject → the request is cleared (the sheet stays `validated`).
pub async fn decide_sheet(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
    expected: Option<i64>,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_sheet(&mut tx, t, id).await?;
    check_version(expected, s.version)?;
    if s.status != EvaluationStatus::Validated || s.approval_requested_at.is_none() {
        return Err(conflict("no final marks approval is awaiting a decision"));
    }
    let updated = if approve {
        apply_marks_approval(&mut tx, t, &s, reason, actor).await?
    } else {
        let r = reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or_else(|| invalid("reason", "required", "a rejection needs a reason"))?;
        let u = set_sheet(
            &mut tx,
            t,
            &s,
            EvaluationStatus::Validated,
            Stamp::ApprovalCleared,
            Some(r),
            None,
            actor,
        )
        .await?;
        audit(
            &mut tx,
            &inst_scope(t, s.institution_id),
            "exams.mark_sheet.reject",
            "mark_sheet",
            id.to_string(),
            json!({"reason": r}),
            actor,
        )
        .await?;
        u
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Final approved marks (`approved → finalised`): rows `approved → finalised`; the paper becomes
/// `marks_finalised` once every effective candidate is final (withheld candidates keep it in
/// `evaluation`).
pub async fn finalise_sheet(
    st: &ExamsState,
    t: TenantId,
    id: MarkSheetId,
    expected: Option<i64>,
    actor: &Actor,
) -> Result<SheetRow, KernelError> {
    let sheet = get_sheet(st, t, id).await?;
    let paper = load_paper(st, t, sheet.paper_id).await?;
    require_markable(st, t, &paper).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_sheet(&mut tx, t, id).await?;
    check_version(expected, s.version)?;
    if s.status != EvaluationStatus::Approved {
        return Err(conflict(format!(
            "the mark sheet is {}; only approved marks are finalised",
            s.status
        )));
    }
    let mut finalised = 0usize;
    for c in candidates(&mut *tx, t, s.paper_id)
        .await?
        .iter()
        .filter(|c| c.effective() && c.mark.status == MarkStatus::Approved)
    {
        step(
            &mut tx,
            t,
            &c.mark,
            None,
            MarkStatus::Finalised,
            "finalise",
            None,
            actor,
        )
        .await?;
        finalised += 1;
    }
    let updated = set_sheet(
        &mut tx,
        t,
        &s,
        EvaluationStatus::Finalised,
        Stamp::Finalised,
        None,
        None,
        actor,
    )
    .await?;
    let all_final = candidates(&mut *tx, t, s.paper_id)
        .await?
        .iter()
        .filter(|c| c.effective())
        .all(|c| c.mark.status.is_final());
    let locked = lock_paper(&mut tx, t, s.paper_id).await?;
    if all_final && locked.status == ExamPaperStatus::Evaluation {
        change_paper_status(
            &mut tx,
            t,
            &locked,
            ExamPaperStatus::MarksFinalised,
            &StatusChange {
                cause_event_type: MARKS_FINALISED,
                reason: Some("final approved marks established for every candidate"),
                process_instance_id: None,
            },
            actor,
        )
        .await?;
    }
    let scope = inst_scope(t, s.institution_id);
    event(
        &mut tx,
        &scope,
        MARKS_FINALISED,
        MARKS_FINALISED_SCHEMA,
        id.to_string(),
        &sheet_event(&updated, s.status),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.mark_sheet.finalise",
        "mark_sheet",
        id.to_string(),
        json!({"finalised": finalised, "paperMarksFinalised": all_final}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

// ------------------------------------------------------------------ withheld marks

/// Withholds one candidate's marks (incomplete evaluation, L3-I-076); reason required.
pub async fn withhold_mark(
    st: &ExamsState,
    t: TenantId,
    id: PaperMarkId,
    expected: Option<i64>,
    reason: &str,
    actor: &Actor,
) -> Result<Candidate, KernelError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(invalid(
            "reason",
            "required",
            "withholding marks needs a reason",
        ));
    }
    let c = candidate(&st.pool, t, id).await?;
    let paper = load_paper(st, t, c.mark.paper_id).await?;
    require_markable(st, t, &paper).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = shared_marks::lock_mark(&mut tx, t, id).await?;
    check_version(expected, m.version)?;
    let c = Candidate { mark: m, ..c };
    withhold_in_tx(&mut tx, t, &c, reason, actor).await?;
    audit(
        &mut tx,
        &inst_scope(t, paper.institution_id),
        "exams.paper_mark.withhold",
        "paper_mark",
        id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    candidate(&st.pool, t, id).await
}

/// Releases withheld marks back to entry (`withheld → entered`); a finalised sheet reopens
/// (`finalised → in_evaluation`, paper `marks_finalised → evaluation`).
pub async fn release_mark(
    st: &ExamsState,
    t: TenantId,
    id: PaperMarkId,
    expected: Option<i64>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<Candidate, KernelError> {
    let c = candidate(&st.pool, t, id).await?;
    let paper = load_paper(st, t, c.mark.paper_id).await?;
    require_markable(st, t, &paper).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = shared_marks::lock_mark(&mut tx, t, id).await?;
    check_version(expected, m.version)?;
    if m.status != MarkStatus::Withheld {
        return Err(conflict(format!(
            "marks are {}; nothing to release",
            m.status
        )));
    }
    let sheet = sheet_of_paper(&mut *tx, t, m.paper_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "mark sheet",
        })?;
    let sheet = lock_sheet(&mut tx, t, sheet.id).await?;
    match sheet.status {
        EvaluationStatus::Planned | EvaluationStatus::Assigned | EvaluationStatus::InEvaluation => {
        }
        EvaluationStatus::Finalised => {
            set_sheet(
                &mut tx,
                t,
                &sheet,
                EvaluationStatus::InEvaluation,
                Stamp::None,
                Some("withheld marks released"),
                None,
                actor,
            )
            .await?;
            let locked = lock_paper(&mut tx, t, m.paper_id).await?;
            if locked.status == ExamPaperStatus::MarksFinalised {
                change_paper_status(
                    &mut tx,
                    t,
                    &locked,
                    ExamPaperStatus::Evaluation,
                    &StatusChange {
                        cause_event_type: "",
                        reason: Some("withheld marks released"),
                        process_instance_id: None,
                    },
                    actor,
                )
                .await?;
            }
        }
        other => {
            return Err(conflict(format!(
                "the mark sheet is {other}; finish or return it before releasing withheld marks"
            )))
        }
    }
    shared_marks::update(
        &mut tx,
        t,
        &m,
        None,
        MarkStatus::Entered,
        "release",
        reason,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &inst_scope(t, paper.institution_id),
        "exams.paper_mark.release",
        "paper_mark",
        id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    candidate(&st.pool, t, id).await
}

// ------------------------------------------------------------------ evaluators + evaluation rounds

/// An evaluator assigned to a sheet.
#[derive(Clone, Debug)]
pub struct AssignmentRow {
    pub id: Uuid,
    pub mark_sheet_id: MarkSheetId,
    pub evaluator_ref: String,
    pub evaluator_name: Option<String>,
    pub role: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

fn assignment_from_row(r: &PgRow) -> Result<AssignmentRow, sqlx::Error> {
    Ok(AssignmentRow {
        id: r.try_get("id")?,
        mark_sheet_id: MarkSheetId(r.try_get("mark_sheet_id")?),
        evaluator_ref: r.try_get("evaluator_ref")?,
        evaluator_name: r.try_get("evaluator_name")?,
        role: r.try_get("role")?,
        status: r.try_get("status")?,
        created_at: r.try_get("created_at")?,
    })
}

const ASSIGNMENT_COLUMNS: &str =
    "id, mark_sheet_id, evaluator_ref, evaluator_name, role, status, created_at";

/// Evaluators of a sheet.
pub async fn assignments_of(
    st: &ExamsState,
    t: TenantId,
    sheet: MarkSheetId,
) -> Result<Vec<AssignmentRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM exams.evaluation_assignment WHERE tenant_id = $1 \
         AND mark_sheet_id = $2 ORDER BY created_at, id"
    ))
    .bind(t.as_uuid())
    .bind(sheet.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(assignment_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

/// Assigns an evaluator (examiner, panel member, second examiner, adjudicator; L3-I-073).
pub async fn assign_evaluator(
    st: &ExamsState,
    t: TenantId,
    sheet_id: MarkSheetId,
    evaluator_ref: &str,
    evaluator_name: Option<&str>,
    role: &str,
    actor: &Actor,
) -> Result<AssignmentRow, KernelError> {
    if !matches!(
        role,
        "examiner" | "panel_member" | "second_examiner" | "adjudicator"
    ) {
        return Err(invalid(
            "role",
            "invalid",
            "must be examiner, panel_member, second_examiner or adjudicator",
        ));
    }
    let evaluator_ref = evaluator_ref.trim();
    if evaluator_ref.is_empty() || evaluator_ref.chars().count() > 200 {
        return Err(invalid(
            "evaluatorRef",
            "invalid_length",
            "must be 1 … 200 characters",
        ));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_sheet(&mut tx, t, sheet_id).await?;
    if matches!(
        s.status,
        EvaluationStatus::Finalised | EvaluationStatus::Closed | EvaluationStatus::Archived
    ) {
        return Err(conflict(format!("the mark sheet is {}", s.status)));
    }
    let row = sqlx::query(&format!(
        "INSERT INTO exams.evaluation_assignment (tenant_id, institution_id, mark_sheet_id, \
         evaluator_ref, evaluator_name, role, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         RETURNING {ASSIGNMENT_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(sheet_id.as_uuid())
    .bind(evaluator_ref)
    .bind(evaluator_name)
    .bind(role)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = assignment_from_row(&row).map_err(map_db_error)?;
    if s.status == EvaluationStatus::Planned {
        set_sheet(
            &mut tx,
            t,
            &s,
            EvaluationStatus::Assigned,
            Stamp::None,
            None,
            None,
            actor,
        )
        .await?;
    }
    audit(
        &mut tx,
        &inst_scope(t, s.institution_id),
        "exams.mark_sheet.assign_evaluator",
        "mark_sheet",
        sheet_id.to_string(),
        json!({"evaluatorRef": evaluator_ref, "role": role}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(a)
}

/// One recorded evaluation.
#[derive(Clone, Debug)]
pub struct EvaluationRow {
    pub id: EvaluationId,
    pub paper_mark_id: PaperMarkId,
    pub registration_id: ExamRegistrationId,
    pub round: EvaluationRound,
    pub evaluator_ref: String,
    pub marks: Decimal,
    pub remark: Option<String>,
    pub created_at: DateTime<Utc>,
}

const EVALUATION_COLUMNS: &str =
    "id, paper_mark_id, registration_id, round, evaluator_ref, marks, remark, created_at";

fn evaluation_from_row(r: &PgRow) -> Result<EvaluationRow, sqlx::Error> {
    Ok(EvaluationRow {
        id: EvaluationId(r.try_get("id")?),
        paper_mark_id: PaperMarkId(r.try_get("paper_mark_id")?),
        registration_id: ExamRegistrationId(r.try_get("registration_id")?),
        round: decode(r.try_get("round")?, EvaluationRound::parse)?,
        evaluator_ref: r.try_get("evaluator_ref")?,
        marks: r.try_get("marks")?,
        remark: r.try_get("remark")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Recorded evaluations of a paper.
pub async fn evaluations_of<'c, E>(
    executor: E,
    t: TenantId,
    paper: ExamPaperId,
) -> Result<Vec<EvaluationRow>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query(&format!(
        "SELECT {EVALUATION_COLUMNS} FROM exams.evaluation WHERE tenant_id = $1 AND paper_id = $2 \
         AND status = 'recorded' ORDER BY created_at, id"
    ))
    .bind(t.as_uuid())
    .bind(paper.as_uuid())
    .fetch_all(executor)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(evaluation_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

async fn evaluations_by_mark(
    conn: &mut PgConnection,
    t: TenantId,
    paper: ExamPaperId,
) -> Result<HashMap<Uuid, Vec<EvaluationMark>>, KernelError> {
    let mut by: HashMap<Uuid, Vec<EvaluationMark>> = HashMap::new();
    for e in evaluations_of(&mut *conn, t, paper).await? {
        by.entry(e.paper_mark_id.as_uuid())
            .or_default()
            .push(EvaluationMark {
                round: e.round,
                evaluator: e.evaluator_ref,
                marks: e.marks,
            });
    }
    Ok(by)
}

/// Result of recording an evaluation.
#[derive(Clone, Debug)]
pub struct EvaluationOutcome {
    pub evaluation: EvaluationRow,
    pub resolution: Resolution,
    pub mark: Candidate,
}

#[allow(clippy::too_many_arguments)]
async fn insert_evaluation(
    conn: &mut PgConnection,
    t: TenantId,
    sheet: &SheetRow,
    m: &PaperMarkSummary,
    round: EvaluationRound,
    evaluator: &str,
    marks: Decimal,
    remark: Option<&str>,
) -> Result<EvaluationRow, KernelError> {
    let row = sqlx::query(&format!(
        "INSERT INTO exams.evaluation (tenant_id, institution_id, session_id, paper_id, \
         mark_sheet_id, paper_mark_id, registration_id, round, evaluator_ref, marks, remark) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {EVALUATION_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(sheet.institution_id.as_uuid())
    .bind(sheet.session_id.as_uuid())
    .bind(sheet.paper_id.as_uuid())
    .bind(sheet.id.as_uuid())
    .bind(m.id.as_uuid())
    .bind(m.registration_id.as_uuid())
    .bind(round.as_str())
    .bind(evaluator)
    .bind(marks)
    .bind(remark)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    evaluation_from_row(&row).map_err(map_db_error)
}

/// Records one evaluation of a candidate's script (first — one per panel member —, second,
/// adjudication; L3-I-073, 080…082) and writes the resolved value into the marks row
/// (`marks::update(→ entered)`; a submitted row is re-submitted with the new value).
#[allow(clippy::too_many_arguments)]
pub async fn record_evaluation(
    st: &ExamsState,
    t: TenantId,
    paper_id: ExamPaperId,
    mark_id: PaperMarkId,
    round: EvaluationRound,
    marks: Decimal,
    evaluator_ref: &str,
    remark: Option<&str>,
    actor: &Actor,
) -> Result<EvaluationOutcome, KernelError> {
    let evaluator = evaluator_ref.trim();
    if evaluator.is_empty() {
        return Err(invalid(
            "evaluatorRef",
            "required",
            "the evaluator is required",
        ));
    }
    let paper = load_paper(st, t, paper_id).await?;
    require_markable(st, t, &paper).await?;
    require_accepts_marks(&paper)?;
    let c = candidate(&st.pool, t, mark_id).await?;
    if c.mark.paper_id != paper_id {
        return Err(KernelError::NotFound { entity: "marks" });
    }
    if !c.effective() {
        return Err(conflict(format!(
            "candidate {} is not effectively registered for this paper",
            c.student_number
        )));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sheet = ensure_sheet(&mut tx, t, &paper, &SheetSettings::default(), actor).await?;
    let sheet = lock_sheet(&mut tx, t, sheet.id).await?;
    if !(sheet_accepts_entry(sheet.status) || sheet.status == EvaluationStatus::Evaluated) {
        return Err(conflict(format!(
            "the mark sheet is {}; evaluations are recorded before verification",
            sheet.status
        )));
    }
    let m = shared_marks::lock_mark(&mut tx, t, mark_id).await?;
    if !matches!(
        m.status,
        MarkStatus::Pending | MarkStatus::Entered | MarkStatus::Submitted
    ) {
        return Err(conflict(format!(
            "marks are {}; no further evaluation is recorded",
            m.status
        )));
    }
    if m.absent {
        return Err(conflict(
            "the candidate was absent; there is no script to evaluate",
        ));
    }
    if marks < Decimal::ZERO || marks > m.external_max || marks.normalize().scale() > 2 {
        return Err(invalid(
            "marks",
            "out_of_range",
            "must be between 0 and the paper's maximum, with at most two decimals",
        ));
    }
    let mut existing = evaluations_by_mark(&mut tx, t, paper_id)
        .await?
        .remove(&mark_id.as_uuid())
        .unwrap_or_default();
    match round {
        EvaluationRound::First => {
            if existing.iter().any(|e| e.round != EvaluationRound::First) {
                return Err(conflict(
                    "the script already has a second evaluation; first-round evaluations are closed",
                ));
            }
        }
        EvaluationRound::Second => {
            if existing.iter().all(|e| e.round != EvaluationRound::First) {
                let Some(first) = m.external_marks else {
                    return Err(conflict("record the first evaluation before the second"));
                };
                let who = "marks-entry".to_owned();
                let e = insert_evaluation(
                    &mut tx,
                    t,
                    &sheet,
                    &m,
                    EvaluationRound::First,
                    &who,
                    first,
                    Some("first evaluation from marks entry"),
                )
                .await?;
                existing.push(EvaluationMark {
                    round: e.round,
                    evaluator: e.evaluator_ref,
                    marks: e.marks,
                });
            }
            if existing
                .iter()
                .any(|e| e.round == EvaluationRound::First && e.evaluator == evaluator)
            {
                return Err(conflict(
                    "the second evaluation must be made by a different evaluator",
                ));
            }
        }
        EvaluationRound::Adjudication => {
            if !matches!(
                resolve(&existing, true, sheet.difference_threshold),
                Resolution::AwaitingAdjudication { .. }
            ) {
                return Err(conflict(
                    "no adjudicating evaluation is required for this script",
                ));
            }
        }
    }
    let evaluation =
        insert_evaluation(&mut tx, t, &sheet, &m, round, evaluator, marks, remark).await?;
    existing.push(EvaluationMark {
        round,
        evaluator: evaluator.to_owned(),
        marks,
    });
    let resolution = resolve(
        &existing,
        sheet.second_evaluation_required,
        sheet.difference_threshold,
    );
    let scope = inst_scope(t, sheet.institution_id);
    match &resolution {
        Resolution::Resolved(v) if m.external_marks != Some(*v) => {
            let mut values = MarkValues::of(&m);
            values.external_marks = Some(*v);
            values.absent = false;
            let reason = format!("{round} evaluation");
            if m.status == MarkStatus::Submitted {
                let e = shared_marks::update(
                    &mut tx,
                    t,
                    &m,
                    Some(values),
                    MarkStatus::Entered,
                    "evaluation",
                    Some(reason.as_str()),
                    actor,
                )
                .await?;
                shared_marks::update(
                    &mut tx,
                    t,
                    &e,
                    None,
                    MarkStatus::Submitted,
                    "evaluation",
                    Some(reason.as_str()),
                    actor,
                )
                .await?;
            } else {
                shared_marks::update(
                    &mut tx,
                    t,
                    &m,
                    Some(values),
                    MarkStatus::Entered,
                    "evaluation",
                    Some(reason.as_str()),
                    actor,
                )
                .await?;
            }
        }
        Resolution::AwaitingAdjudication { first, second } => {
            event(
                &mut tx,
                &scope,
                SECOND_EVALUATION_REQUIRED,
                SECOND_EVALUATION_REQUIRED_SCHEMA,
                m.id.to_string(),
                &StatusChangedV1 {
                    session_id: Some(m.session_id.as_uuid()),
                    paper_id: Some(m.paper_id.as_uuid()),
                    registration_id: Some(m.registration_id.as_uuid()),
                    student_id: Some(m.student_id.as_uuid()),
                    reason: Some(format!(
                        "first {} and second {} differ by more than {}; adjudication required",
                        fmt2(*first),
                        fmt2(*second),
                        fmt2(sheet.difference_threshold)
                    )),
                    ..changed(
                        m.id.as_uuid(),
                        sheet.institution_id,
                        None,
                        "adjudication",
                        m.version,
                    )
                },
                actor,
            )
            .await?;
        }
        _ => {}
    }
    if sheet.status != EvaluationStatus::InEvaluation && sheet_accepts_entry(sheet.status) {
        set_sheet(
            &mut tx,
            t,
            &sheet,
            EvaluationStatus::InEvaluation,
            Stamp::None,
            None,
            None,
            actor,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.evaluation.record",
        "evaluation",
        evaluation.id.to_string(),
        json!({"paperMarkId": mark_id, "round": round.as_str(), "marks": fmt2(marks),
               "evaluatorRef": evaluator}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mark = candidate(&st.pool, t, mark_id).await?;
    Ok(EvaluationOutcome {
        evaluation,
        resolution,
        mark,
    })
}

/// Resolution of every evaluated script of a paper.
pub async fn resolutions(
    st: &ExamsState,
    t: TenantId,
    sheet: Option<&SheetRow>,
    paper: ExamPaperId,
) -> Result<Vec<(Uuid, Resolution)>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let by = evaluations_by_mark(&mut conn, t, paper).await?;
    let (req, th) = sheet.map_or((false, Decimal::TEN), |s| {
        (s.second_evaluation_required, s.difference_threshold)
    });
    let mut out: Vec<(Uuid, Resolution)> = by
        .into_iter()
        .map(|(k, v)| (k, resolve(&v, req, th)))
        .collect();
    out.sort_by_key(|(k, _)| *k);
    Ok(out)
}

// ------------------------------------------------------------------ moderation

/// A moderation of a paper.
#[derive(Clone, Debug)]
pub struct ModerationRow {
    pub id: ModerationId,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub paper_id: ExamPaperId,
    pub mark_sheet_id: MarkSheetId,
    pub kind: ModerationKind,
    pub value: Decimal,
    pub reason: String,
    pub status: ModerationStatus,
    pub status_reason: Option<String>,
    pub report: serde_json::Value,
    pub applied_count: i32,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const MODERATION_COLUMNS: &str = "id, institution_id, session_id, paper_id, mark_sheet_id, kind, \
    value, reason, status, status_reason, report, applied_count, process_instance_id, decided_by, \
    version, created_at";

fn moderation_from_row(r: &PgRow) -> Result<ModerationRow, sqlx::Error> {
    Ok(ModerationRow {
        id: ModerationId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        paper_id: ExamPaperId(r.try_get("paper_id")?),
        mark_sheet_id: MarkSheetId(r.try_get("mark_sheet_id")?),
        kind: decode(r.try_get("kind")?, ModerationKind::parse)?,
        value: r.try_get("value")?,
        reason: r.try_get("reason")?,
        status: decode(r.try_get("status")?, ModerationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        report: r.try_get("report")?,
        applied_count: r.try_get("applied_count")?,
        process_instance_id: r.try_get("process_instance_id")?,
        decided_by: r.try_get("decided_by")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Moderations of a paper, newest first.
pub async fn moderations_of(
    st: &ExamsState,
    t: TenantId,
    paper: ExamPaperId,
) -> Result<Vec<ModerationRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {MODERATION_COLUMNS} FROM exams.moderation WHERE tenant_id = $1 AND paper_id = $2 \
         ORDER BY created_at DESC, id DESC"
    ))
    .bind(t.as_uuid())
    .bind(paper.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(moderation_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

async fn lock_moderation(
    conn: &mut PgConnection,
    t: TenantId,
    id: ModerationId,
) -> Result<ModerationRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {MODERATION_COLUMNS} FROM exams.moderation WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "moderation",
    })?;
    moderation_from_row(&row).map_err(map_db_error)
}

/// One moderation (404).
pub async fn get_moderation(
    st: &ExamsState,
    t: TenantId,
    id: ModerationId,
) -> Result<ModerationRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "SELECT {MODERATION_COLUMNS} FROM exams.moderation WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "moderation",
    })?;
    moderation_from_row(&row).map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn set_moderation(
    conn: &mut PgConnection,
    t: TenantId,
    m: &ModerationRow,
    to: ModerationStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    decided: bool,
    actor: &Actor,
) -> Result<ModerationRow, KernelError> {
    if !moderation_can_transition(m.status, to) {
        return Err(conflict(format!(
            "the moderation is {}; it cannot become {to}",
            m.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.moderation SET status = $3, status_reason = COALESCE($4, status_reason), \
         process_instance_id = COALESCE($5, process_instance_id), \
         decided_by = CASE WHEN $6 THEN $7 ELSE decided_by END, \
         decided_at = CASE WHEN $6 THEN now() ELSE decided_at END, \
         applied_at = CASE WHEN $3 = 'applied' THEN now() ELSE applied_at END, \
         version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND version = $8 RETURNING {MODERATION_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(m.id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .bind(decided)
    .bind(actor.id.as_deref())
    .bind(m.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| conflict("the moderation changed concurrently"))?;
    record_history(
        conn,
        HistoryEntry {
            tenant: t,
            institution: m.institution_id,
            aggregate_type: "moderation",
            aggregate_id: m.id.as_uuid(),
            from: Some(m.status.as_str()),
            to: to.as_str(),
        },
        &StatusChange {
            cause_event_type: "",
            reason,
            process_instance_id,
        },
        actor,
    )
    .await?;
    moderation_from_row(&row).map_err(map_db_error)
}

fn graded_json(g: &GradedLine) -> serde_json::Value {
    json!({
        "percent": g.percent.map(fmt2),
        "grade": g.grade,
        "gradePoint": g.grade_point.map(fmt2),
        "passed": g.passed,
    })
}

/// Before / after lines of a moderation over the paper's verified scripts.
fn moderation_report(
    paper: &ExamPaperSummary,
    rows: &[Candidate],
    kind: ModerationKind,
    value: Decimal,
    bands: Option<&[GradeBand]>,
) -> (serde_json::Value, Vec<(PaperMarkId, MarkValues)>) {
    let mut lines = Vec::new();
    let mut changes = Vec::new();
    for c in rows
        .iter()
        .filter(|c| c.effective() && c.mark.status == MarkStatus::Verified && !c.mark.absent)
    {
        let (ext, grace) = moderate(
            kind,
            value,
            c.mark.external_marks,
            c.mark.grace_marks,
            c.mark.external_max,
        );
        let mut after = c.mark.clone();
        after.external_marks = ext;
        after.grace_marks = grace;
        let mut line = json!({
            "paperMarkId": c.mark.id,
            "studentNumber": c.student_number,
            "before": {"externalMarks": c.mark.external_marks.map(fmt2), "graceMarks": fmt2(c.mark.grace_marks)},
            "after": {"externalMarks": ext.map(fmt2), "graceMarks": fmt2(grace)},
        });
        if let Some(b) = bands {
            line["before"]["preview"] = graded_json(&grade_line(&line_input(paper, &c.mark), b));
            line["after"]["preview"] = graded_json(&grade_line(&line_input(paper, &after), b));
        }
        lines.push(line);
        let mut v = MarkValues::of(&c.mark);
        v.external_marks = ext;
        v.grace_marks = grace;
        changes.push((c.mark.id, v));
    }
    (serde_json::Value::Array(lines), changes)
}

/// Requests a moderation of a verified paper (L3-I-075): bounds by policy (422), a dry-run
/// before / after report; the sheet goes `validated → moderation`.
pub async fn request_moderation(
    st: &ExamsState,
    t: TenantId,
    paper_id: ExamPaperId,
    kind: ModerationKind,
    value: Decimal,
    reason: &str,
    actor: &Actor,
) -> Result<ModerationRow, KernelError> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(invalid("reason", "required", "a moderation needs a reason"));
    }
    let paper = load_paper(st, t, paper_id).await?;
    let session = require_markable(st, t, &paper).await?;
    if let Some(msg) = moderation_out_of_bounds_for(kind, value, paper.max_marks) {
        return Err(invalid("value", "out_of_range", msg));
    }
    let bands = bands_of(st, t, &session).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sheet = sheet_of_paper(&mut *tx, t, paper_id)
        .await?
        .ok_or_else(|| conflict("the paper has no mark sheet yet"))?;
    let sheet = lock_sheet(&mut tx, t, sheet.id).await?;
    if sheet.status != EvaluationStatus::Validated || sheet.approval_requested_at.is_some() {
        return Err(conflict(format!(
            "the mark sheet is {}; moderation applies to verified marks before approval",
            sheet.status
        )));
    }
    let rows = candidates(&mut *tx, t, paper_id).await?;
    let (report, changes) = moderation_report(&paper, &rows, kind, value, bands.as_deref());
    if changes.is_empty() {
        return Err(conflict("no verified script to moderate"));
    }
    let row = sqlx::query(&format!(
        "INSERT INTO exams.moderation (tenant_id, institution_id, session_id, paper_id, \
         mark_sheet_id, kind, value, reason, report, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING {MODERATION_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(paper.institution_id.as_uuid())
    .bind(paper.session_id.as_uuid())
    .bind(paper_id.as_uuid())
    .bind(sheet.id.as_uuid())
    .bind(kind.as_str())
    .bind(value)
    .bind(reason)
    .bind(&report)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let m = moderation_from_row(&row).map_err(map_db_error)?;
    set_sheet(
        &mut tx,
        t,
        &sheet,
        EvaluationStatus::Moderation,
        Stamp::None,
        Some(reason),
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &inst_scope(t, paper.institution_id),
        "exams.moderation.request",
        "moderation",
        m.id.to_string(),
        json!({"kind": kind.as_str(), "value": fmt2(value), "scripts": changes.len()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(m)
}

fn moderation_out_of_bounds_for(
    kind: ModerationKind,
    value: Decimal,
    max: Decimal,
) -> Option<&'static str> {
    super::domain::moderation_out_of_bounds(kind, value, max)
}

async fn release_sheet_from_moderation(
    conn: &mut PgConnection,
    t: TenantId,
    sheet: MarkSheetId,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let s = lock_sheet(conn, t, sheet).await?;
    if s.status == EvaluationStatus::Moderation {
        set_sheet(
            conn,
            t,
            &s,
            EvaluationStatus::Validated,
            Stamp::None,
            reason,
            None,
            actor,
        )
        .await?;
    }
    Ok(())
}

/// Validates a requested moderation and requests its approval (`MODERATION_APPROVAL`);
/// auto-approval moves it to `approved` at once.
pub async fn validate_moderation(
    st: &ExamsState,
    t: TenantId,
    id: ModerationId,
    expected: Option<i64>,
    actor: &Actor,
) -> Result<ModerationRow, KernelError> {
    let m = get_moderation(st, t, id).await?;
    check_version(expected, m.version)?;
    if m.status != ModerationStatus::Requested {
        return Err(conflict(format!(
            "the moderation is {}; only a requested moderation is validated",
            m.status
        )));
    }
    let scope = inst_scope(t, m.institution_id);
    let mut vars = BTreeMap::new();
    vars.insert("paperId".to_owned(), json!(m.paper_id));
    vars.insert("kind".to_owned(), json!(m.kind.as_str()));
    vars.insert("value".to_owned(), json!(fmt2(m.value)));
    let outcome = request_approval(
        st,
        &scope,
        process::MODERATION_APPROVAL,
        business_key("moderation", id, m.version),
        vars,
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_moderation(&mut tx, t, id).await?;
    check_version(Some(m.version), locked.version)?;
    let mut updated = set_moderation(
        &mut tx,
        t,
        &locked,
        ModerationStatus::Validated,
        None,
        Some(outcome.process_instance_id()),
        false,
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        updated = set_moderation(
            &mut tx,
            t,
            &updated,
            ModerationStatus::Approved,
            Some("auto-approved"),
            None,
            true,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.moderation.validate",
        "moderation",
        id.to_string(),
        json!({"processInstanceId": outcome.process_instance_id(), "status": updated.status.as_str()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Explicit decision on a validated moderation (`exam.approve`).
pub async fn decide_moderation(
    st: &ExamsState,
    t: TenantId,
    id: ModerationId,
    expected: Option<i64>,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ModerationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = lock_moderation(&mut tx, t, id).await?;
    check_version(expected, m.version)?;
    if m.status != ModerationStatus::Validated {
        return Err(conflict(format!(
            "the moderation is {}; no decision is awaited",
            m.status
        )));
    }
    let to = if approve {
        ModerationStatus::Approved
    } else {
        ModerationStatus::Rejected
    };
    let updated = set_moderation(&mut tx, t, &m, to, reason, None, true, actor).await?;
    if !approve {
        release_sheet_from_moderation(&mut tx, t, m.mark_sheet_id, reason, actor).await?;
    }
    audit(
        &mut tx,
        &inst_scope(t, m.institution_id),
        "exams.moderation.decide",
        "moderation",
        id.to_string(),
        json!({"decision": to.as_str(), "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Applies an approved moderation: verified scripts `→ moderated` with the new values
/// (`marks::update`), the final before / after report is stored, the sheet returns to
/// `validated`; `moderation.applied`.
pub async fn apply_moderation(
    st: &ExamsState,
    t: TenantId,
    id: ModerationId,
    expected: Option<i64>,
    actor: &Actor,
) -> Result<ModerationRow, KernelError> {
    let m = get_moderation(st, t, id).await?;
    let paper = load_paper(st, t, m.paper_id).await?;
    let session = require_markable(st, t, &paper).await?;
    let bands = bands_of(st, t, &session).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_moderation(&mut tx, t, id).await?;
    check_version(expected, locked.version)?;
    if locked.status != ModerationStatus::Approved {
        return Err(conflict(format!(
            "the moderation is {}; only an approved moderation is applied",
            locked.status
        )));
    }
    let rows = candidates(&mut *tx, t, m.paper_id).await?;
    let (report, changes) =
        moderation_report(&paper, &rows, locked.kind, locked.value, bands.as_deref());
    for (mid, values) in &changes {
        let mk = shared_marks::lock_mark(&mut tx, t, *mid).await?;
        shared_marks::update(
            &mut tx,
            t,
            &mk,
            Some(values.clone()),
            MarkStatus::Moderated,
            "moderation",
            Some(locked.reason.as_str()),
            actor,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE exams.moderation SET report = $3, applied_count = $4 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .bind(&report)
    .bind(i32::try_from(changes.len()).unwrap_or(i32::MAX))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let reloaded = lock_moderation(&mut tx, t, id).await?;
    let updated = set_moderation(
        &mut tx,
        t,
        &reloaded,
        ModerationStatus::Applied,
        None,
        None,
        false,
        actor,
    )
    .await?;
    release_sheet_from_moderation(
        &mut tx,
        t,
        m.mark_sheet_id,
        Some("moderation applied"),
        actor,
    )
    .await?;
    let scope = inst_scope(t, m.institution_id);
    event(
        &mut tx,
        &scope,
        MARKS_MODERATED,
        MARKS_MODERATED_SCHEMA,
        id.to_string(),
        &StatusChangedV1 {
            session_id: Some(m.session_id.as_uuid()),
            paper_id: Some(m.paper_id.as_uuid()),
            reason: Some(format!(
                "{} {} applied to {} script(s)",
                m.kind.as_str(),
                fmt2(m.value),
                changes.len()
            )),
            process_instance_id: updated.process_instance_id.clone(),
            ..changed(
                id.as_uuid(),
                m.institution_id,
                Some("approved"),
                "applied",
                updated.version,
            )
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.moderation.apply",
        "moderation",
        id.to_string(),
        json!({"applied": changes.len()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Withdraws an open moderation (the sheet returns to `validated`).
pub async fn withdraw_moderation(
    st: &ExamsState,
    t: TenantId,
    id: ModerationId,
    expected: Option<i64>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ModerationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = lock_moderation(&mut tx, t, id).await?;
    check_version(expected, m.version)?;
    let updated = set_moderation(
        &mut tx,
        t,
        &m,
        ModerationStatus::Withdrawn,
        reason,
        None,
        false,
        actor,
    )
    .await?;
    release_sheet_from_moderation(&mut tx, t, m.mark_sheet_id, reason, actor).await?;
    audit(
        &mut tx,
        &inst_scope(t, m.institution_id),
        "exams.moderation.withdraw",
        "moderation",
        id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

// ------------------------------------------------------------------ faculty assessments

/// A paper of a section the faculty member teaches.
#[derive(Clone, Debug)]
pub struct AssessmentRow {
    pub paper: ExamPaperSummary,
    pub session_code: String,
    pub session_name: String,
    pub session_status: String,
    pub sheet_id: Option<MarkSheetId>,
    pub sheet_status: Option<EvaluationStatus>,
    pub candidates: i64,
    pub entered: i64,
    pub sections: Vec<String>,
}

/// Papers whose offering has a section `subject` actively teaches (`/me/assessments`).
pub async fn assessments_of(
    st: &ExamsState,
    t: TenantId,
    subject: &str,
) -> Result<Vec<AssessmentRow>, KernelError> {
    let scope = ExecutionScope::tenant(t);
    let sections = st.offerings.sections_taught_by(&scope, subject).await?;
    let mut by_offering: BTreeMap<Uuid, Vec<String>> = BTreeMap::new();
    for s in &sections {
        by_offering
            .entry(s.offering_id.as_uuid())
            .or_default()
            .push(s.section_code.clone());
    }
    if by_offering.is_empty() {
        return Ok(Vec::new());
    }
    let offerings: Vec<Uuid> = by_offering.keys().copied().collect();
    paper_rows(st, t, Some(offerings), None, &by_offering).await
}

/// Papers of an institution's sessions with their sheet status (Admin → Examinations → Marks).
pub async fn papers_of_institution(
    st: &ExamsState,
    t: TenantId,
    institution: InstitutionId,
) -> Result<Vec<AssessmentRow>, KernelError> {
    paper_rows(st, t, None, Some(institution.as_uuid()), &BTreeMap::new()).await
}

async fn paper_rows(
    st: &ExamsState,
    t: TenantId,
    offerings: Option<Vec<Uuid>>,
    institution: Option<Uuid>,
    by_offering: &BTreeMap<Uuid, Vec<String>>,
) -> Result<Vec<AssessmentRow>, KernelError> {
    let rows = sqlx::query(
        "SELECT p.id, p.offering_id, s.code AS session_code, s.name AS session_name, \
         s.status AS session_status, ms.id AS sheet_id, ms.status AS sheet_status, \
         (SELECT count(*) FROM exams.paper_mark m JOIN exams.exam_registration r ON r.id = m.registration_id \
            JOIN exams.exam_registration_paper rp ON rp.id = m.registration_paper_id \
            WHERE m.paper_id = p.id AND rp.status <> 'withdrawn' \
            AND r.status IN ('approved', 'confirmed', 'locked', 'amended', 'completed')) AS candidates, \
         (SELECT count(*) FROM exams.paper_mark m JOIN exams.exam_registration r ON r.id = m.registration_id \
            JOIN exams.exam_registration_paper rp ON rp.id = m.registration_paper_id \
            WHERE m.paper_id = p.id AND rp.status <> 'withdrawn' AND m.status <> 'pending' \
            AND r.status IN ('approved', 'confirmed', 'locked', 'amended', 'completed')) AS entered \
         FROM exams.exam_paper p JOIN exams.exam_session s ON s.id = p.session_id \
         LEFT JOIN exams.mark_sheet ms ON ms.paper_id = p.id AND ms.tenant_id = p.tenant_id \
         WHERE p.tenant_id = $1 AND ($2::uuid[] IS NULL OR p.offering_id = ANY($2)) \
         AND ($3::uuid IS NULL OR p.institution_id = $3) AND p.status <> 'cancelled' \
         ORDER BY s.code, p.paper_code LIMIT 500",
    )
    .bind(t.as_uuid())
    .bind(offerings)
    .bind(institution)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let id: Uuid = r.try_get("id").map_err(map_db_error)?;
        let offering: Option<Uuid> = r.try_get("offering_id").map_err(map_db_error)?;
        let paper = load_paper(st, t, ExamPaperId(id)).await?;
        let sheet_status: Option<String> = r.try_get("sheet_status").map_err(map_db_error)?;
        out.push(AssessmentRow {
            paper,
            session_code: r.try_get("session_code").map_err(map_db_error)?,
            session_name: r.try_get("session_name").map_err(map_db_error)?,
            session_status: r.try_get("session_status").map_err(map_db_error)?,
            sheet_id: r
                .try_get::<Option<Uuid>, _>("sheet_id")
                .map_err(map_db_error)?
                .map(MarkSheetId),
            sheet_status: sheet_status.as_deref().and_then(EvaluationStatus::parse),
            candidates: r.try_get("candidates").map_err(map_db_error)?,
            entered: r.try_get("entered").map_err(map_db_error)?,
            sections: offering
                .and_then(|o| by_offering.get(&o).cloned())
                .unwrap_or_default(),
        });
    }
    Ok(out)
}
