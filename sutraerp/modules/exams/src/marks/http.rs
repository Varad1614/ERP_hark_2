//! HTTP API of 6B marks (tag `exam-marks`; contract §5.2): mark sheets, internal import, marks
//! entry, submission / verification / final marks approval, withheld marks, submission
//! exceptions, evaluators and evaluation rounds, moderation, gradebook and the faculty
//! assessments view.
//!
//! Authority: staff reads need `exam.read`; marks entry / submission `exam.marks.enter` —
//! faculty **only** on papers whose offering has a section they actively teach
//! (`OfferingDirectory::teaches`, otherwise 404), the exam cell (`exam.marks.manage`) on any
//! paper; verification, import, moderation and withholding `exam.marks.manage`; decisions
//! `exam.approve`.

use super::domain::{ModerationKind, Resolution};
use super::repo::{self, invalid, Candidate, SheetRow, SheetSettings};
use crate::shared::grading::{fmt2, grade_line};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::ExamsState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::exams::ids::{
    EvaluationId, ExamPaperId, ExamRegistrationId, ExamSessionId, MarkSheetId, MarksExceptionId,
    ModerationId, PaperMarkId,
};
use sutra_contracts::exams::ports::{ExamPaperSummary, GradeBand};
use sutra_contracts::exams::status::{
    AttemptKind, EvaluationRound, EvaluationStatus, ExamPaperStatus, ExamRegistrationStatus,
    MarkStatus, ModerationStatus, RegistrationPaperStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ authority

/// 404 unless the caller may work on the paper: the exam cell (`exam.marks.manage`) on any paper,
/// faculty only on papers whose offering has a section they actively teach.
pub(crate) async fn authorize_paper(
    st: &ExamsState,
    ctx: &AuthContext,
    paper: &ExamPaperSummary,
) -> Result<(), KernelError> {
    if ctx.has_permission(Permission::MarksManage) {
        return Ok(());
    }
    let not_found = KernelError::NotFound {
        entity: "examination paper",
    };
    let Some(offering) = paper.offering_id else {
        return Err(not_found);
    };
    let scope = institution_scope(ctx, paper.institution_id);
    for s in st
        .offerings
        .sections_of_offering(&scope, CourseOfferingId(offering))
        .await?
    {
        if st.offerings.teaches(&scope, &ctx.subject, s.id).await? {
            return Ok(());
        }
    }
    Err(not_found)
}

fn parse_dec(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_number",
            "must be a decimal string such as \"72.50\"",
        )
    })
}

fn parse_opt(field: &str, raw: Option<&String>) -> Result<Option<Decimal>, KernelError> {
    raw.map(|r| parse_dec(field, r)).transpose()
}

fn expected(headers: &HeaderMap) -> Result<Result<i64, Response>, KernelError> {
    Ok(if_match(headers)?.map(|v| v.0))
}

fn decision(raw: &str, approve: &str, reject: &str) -> Result<bool, KernelError> {
    if raw == approve {
        Ok(true)
    } else if raw == reject {
        Ok(false)
    } else {
        Err(invalid(
            "decision",
            "invalid",
            &format!("must be {approve} or {reject}"),
        ))
    }
}

// ------------------------------------------------------------------ views

/// A paper's mark sheet (evaluation lifecycle).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkSheetView {
    pub id: MarkSheetId,
    pub session_id: ExamSessionId,
    pub paper_id: ExamPaperId,
    pub status: EvaluationStatus,
    pub status_reason: Option<String>,
    pub due_on: Option<NaiveDate>,
    pub second_evaluation_required: bool,
    #[schema(value_type = String, example = "10.00")]
    pub difference_threshold: String,
    pub submitted_by: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub approval_requested_at: Option<DateTime<Utc>>,
    /// A final marks approval is awaiting its decision.
    pub awaiting_decision: bool,
    pub approved_by: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub finalised_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

impl From<&SheetRow> for MarkSheetView {
    fn from(s: &SheetRow) -> Self {
        Self {
            id: s.id,
            session_id: s.session_id,
            paper_id: s.paper_id,
            status: s.status,
            status_reason: s.status_reason.clone(),
            due_on: s.due_on,
            second_evaluation_required: s.second_evaluation_required,
            difference_threshold: fmt2(s.difference_threshold),
            submitted_by: s.submitted_by.clone(),
            submitted_at: s.submitted_at,
            verified_by: s.verified_by.clone(),
            verified_at: s.verified_at,
            approval_requested_at: s.approval_requested_at,
            awaiting_decision: s.status == EvaluationStatus::Validated
                && s.approval_requested_at.is_some(),
            approved_by: s.approved_by.clone(),
            approved_at: s.approved_at,
            finalised_at: s.finalised_at,
            process_instance_id: s.process_instance_id.clone(),
            version: s.version,
        }
    }
}

fn sheet_response(status: StatusCode, s: &SheetRow) -> Response {
    with_etag(status, &MarkSheetView::from(s), Version(s.version))
}

/// Grade preview of one candidate (`shared::grading`, the engine 6C computes with).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkPreview {
    #[schema(value_type = Option<String>, example = "72.50")]
    pub percent: Option<String>,
    pub grade: Option<String>,
    #[schema(value_type = Option<String>, example = "8.00")]
    pub grade_point: Option<String>,
    pub passed: bool,
}

/// One candidate's marks on a paper (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkRowView {
    pub id: PaperMarkId,
    pub paper_id: ExamPaperId,
    pub registration_id: ExamRegistrationId,
    pub registration_number: String,
    pub student_number: String,
    pub display_name: String,
    pub registration_status: ExamRegistrationStatus,
    pub paper_status: RegistrationPaperStatus,
    pub attempt_kind: AttemptKind,
    pub attempt_no: i32,
    #[schema(value_type = Option<String>, example = "34.50")]
    pub internal_marks: Option<String>,
    #[schema(value_type = Option<String>, example = "40.00")]
    pub internal_max: Option<String>,
    pub internal_source_id: Option<Uuid>,
    #[schema(value_type = Option<String>, example = "61.00")]
    pub external_marks: Option<String>,
    #[schema(value_type = String, example = "100.00")]
    pub external_max: String,
    #[schema(value_type = String, example = "0.00")]
    pub grace_marks: String,
    pub absent: bool,
    pub status: MarkStatus,
    /// `false` for withdrawn papers / non-effective registrations (shown, never entered).
    pub effective: bool,
    pub preview: Option<MarkPreview>,
    pub version: i64,
}

fn preview_of(
    paper: &ExamPaperSummary,
    c: &Candidate,
    bands: Option<&[GradeBand]>,
) -> Option<MarkPreview> {
    let bands = bands?;
    let g = grade_line(&repo::line_input(paper, &c.mark), bands);
    g.percent?;
    Some(MarkPreview {
        percent: g.percent.map(fmt2),
        grade: g.grade,
        grade_point: g.grade_point.map(fmt2),
        passed: g.passed,
    })
}

fn row_view(paper: &ExamPaperSummary, c: &Candidate, bands: Option<&[GradeBand]>) -> MarkRowView {
    let m = &c.mark;
    MarkRowView {
        id: m.id,
        paper_id: m.paper_id,
        registration_id: m.registration_id,
        registration_number: c.registration_number.clone(),
        student_number: c.student_number.clone(),
        display_name: c.display_name.clone(),
        registration_status: c.registration_status,
        paper_status: c.paper_status,
        attempt_kind: c.attempt_kind,
        attempt_no: c.attempt_no,
        internal_marks: m.internal_marks.map(fmt2),
        internal_max: m.internal_max.map(fmt2),
        internal_source_id: m.internal_source_id,
        external_marks: m.external_marks.map(fmt2),
        external_max: fmt2(m.external_max),
        grace_marks: fmt2(m.grace_marks),
        absent: m.absent,
        status: m.status,
        effective: c.effective(),
        preview: preview_of(paper, c, bands),
        version: m.version,
    }
}

/// Candidates of a paper with the sheet (if open).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkRowList {
    pub paper_id: ExamPaperId,
    pub paper_code: String,
    pub paper_status: ExamPaperStatus,
    pub sheet: Option<MarkSheetView>,
    pub items: Vec<MarkRowView>,
}

async fn marks_list(
    st: &ExamsState,
    t: sutra_kernel::scope::TenantId,
    paper: &ExamPaperSummary,
) -> Result<MarkRowList, KernelError> {
    let session = repo::load_session(st, t, paper.session_id).await?;
    let bands = repo::bands_of(st, t, &session).await?;
    let sheet = repo::sheet_of_paper(&st.pool, t, paper.id).await?;
    let rows = repo::candidates(&st.pool, t, paper.id).await?;
    Ok(MarkRowList {
        paper_id: paper.id,
        paper_code: paper.paper_code.clone(),
        paper_status: paper.status,
        sheet: sheet.as_ref().map(MarkSheetView::from),
        items: rows
            .iter()
            .map(|c| row_view(paper, c, bands.as_deref()))
            .collect(),
    })
}

/// Summary of a bulk command.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarksBulkSummary {
    pub processed: usize,
    pub skipped: usize,
    pub failed: Vec<MarksBulkFailure>,
    /// Student numbers without a finalised internal assessment result.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarksBulkFailure {
    pub registration_id: Uuid,
    pub reason: String,
}

/// A marks submission exception.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarksExceptionView {
    pub id: MarksExceptionId,
    pub mark_sheet_id: MarkSheetId,
    pub paper_id: ExamPaperId,
    pub paper_mark_id: Option<PaperMarkId>,
    /// `late_submission | missing_marks`.
    pub kind: String,
    pub reason: String,
    /// `raised | accepted | rejected`.
    pub status: String,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl From<&repo::ExceptionRow> for MarksExceptionView {
    fn from(e: &repo::ExceptionRow) -> Self {
        Self {
            id: e.id,
            mark_sheet_id: e.mark_sheet_id,
            paper_id: e.paper_id,
            paper_mark_id: e.paper_mark_id,
            kind: e.kind.clone(),
            reason: e.reason.clone(),
            status: e.status.clone(),
            resolution: e.resolution.clone(),
            resolved_by: e.resolved_by.clone(),
            created_at: e.created_at,
            version: e.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MarksExceptionList {
    pub items: Vec<MarksExceptionView>,
}

/// An evaluator assigned to a mark sheet.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatorAssignmentView {
    pub id: Uuid,
    pub mark_sheet_id: MarkSheetId,
    pub evaluator_ref: String,
    pub evaluator_name: Option<String>,
    /// `examiner | panel_member | second_examiner | adjudicator`.
    pub role: String,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

impl From<&repo::AssignmentRow> for EvaluatorAssignmentView {
    fn from(a: &repo::AssignmentRow) -> Self {
        Self {
            id: a.id,
            mark_sheet_id: a.mark_sheet_id,
            evaluator_ref: a.evaluator_ref.clone(),
            evaluator_name: a.evaluator_name.clone(),
            role: a.role.clone(),
            status: a.status.clone(),
            created_at: a.created_at,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct EvaluatorAssignmentList {
    pub items: Vec<EvaluatorAssignmentView>,
}

/// One recorded evaluation of a script.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationView {
    pub id: EvaluationId,
    pub paper_mark_id: PaperMarkId,
    pub registration_id: ExamRegistrationId,
    pub round: EvaluationRound,
    pub evaluator_ref: String,
    #[schema(value_type = String, example = "62.00")]
    pub marks: String,
    pub remark: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<&repo::EvaluationRow> for EvaluationView {
    fn from(e: &repo::EvaluationRow) -> Self {
        Self {
            id: e.id,
            paper_mark_id: e.paper_mark_id,
            registration_id: e.registration_id,
            round: e.round,
            evaluator_ref: e.evaluator_ref.clone(),
            marks: fmt2(e.marks),
            remark: e.remark.clone(),
            created_at: e.created_at,
        }
    }
}

/// Where a script stands after its evaluations.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationResolutionView {
    pub paper_mark_id: Uuid,
    /// `none | resolved | awaiting_second | awaiting_adjudication`.
    pub state: String,
    /// Authoritative evaluated marks when resolved.
    #[schema(value_type = Option<String>, example = "63.50")]
    pub marks: Option<String>,
    #[schema(value_type = Option<String>)]
    pub first: Option<String>,
    #[schema(value_type = Option<String>)]
    pub second: Option<String>,
}

fn resolution_view(id: Uuid, r: &Resolution) -> EvaluationResolutionView {
    let (state, marks, first, second) = match r {
        Resolution::None => ("none", None, None, None),
        Resolution::Resolved(v) => ("resolved", Some(fmt2(*v)), None, None),
        Resolution::AwaitingSecond { first } => ("awaiting_second", None, Some(fmt2(*first)), None),
        Resolution::AwaitingAdjudication { first, second } => (
            "awaiting_adjudication",
            None,
            Some(fmt2(*first)),
            Some(fmt2(*second)),
        ),
    };
    EvaluationResolutionView {
        paper_mark_id: id,
        state: state.to_owned(),
        marks,
        first,
        second,
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationList {
    pub items: Vec<EvaluationView>,
    pub resolutions: Vec<EvaluationResolutionView>,
}

/// Result of recording an evaluation.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationRecorded {
    pub evaluation: EvaluationView,
    pub resolution: EvaluationResolutionView,
    pub mark: MarkRowView,
}

/// A moderation of a paper with its before / after report.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModerationView {
    pub id: ModerationId,
    pub session_id: ExamSessionId,
    pub paper_id: ExamPaperId,
    pub mark_sheet_id: MarkSheetId,
    /// `grace | scaling`.
    pub kind: String,
    #[schema(value_type = String, example = "3.00")]
    pub value: String,
    pub reason: String,
    pub status: ModerationStatus,
    pub status_reason: Option<String>,
    /// Per script: `{paperMarkId, studentNumber, before{externalMarks, graceMarks, preview?}, after{…}}`.
    pub report: serde_json::Value,
    pub applied_count: i32,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl From<&repo::ModerationRow> for ModerationView {
    fn from(m: &repo::ModerationRow) -> Self {
        Self {
            id: m.id,
            session_id: m.session_id,
            paper_id: m.paper_id,
            mark_sheet_id: m.mark_sheet_id,
            kind: m.kind.as_str().to_owned(),
            value: fmt2(m.value),
            reason: m.reason.clone(),
            status: m.status,
            status_reason: m.status_reason.clone(),
            report: m.report.clone(),
            applied_count: m.applied_count,
            process_instance_id: m.process_instance_id.clone(),
            decided_by: m.decided_by.clone(),
            created_at: m.created_at,
            version: m.version,
        }
    }
}

fn moderation_response(status: StatusCode, m: &repo::ModerationRow) -> Response {
    with_etag(status, &ModerationView::from(m), Version(m.version))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModerationList {
    pub items: Vec<ModerationView>,
}

/// Gradebook line of one candidate.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GradebookLine {
    pub paper_mark_id: PaperMarkId,
    pub registration_id: ExamRegistrationId,
    pub student_number: String,
    pub display_name: String,
    pub attempt_kind: AttemptKind,
    pub status: MarkStatus,
    pub absent: bool,
    #[schema(value_type = Option<String>)]
    pub internal_marks: Option<String>,
    #[schema(value_type = Option<String>)]
    pub external_marks: Option<String>,
    #[schema(value_type = String)]
    pub grace_marks: String,
    #[schema(value_type = Option<String>, example = "72.50")]
    pub percent: Option<String>,
    pub grade: Option<String>,
    #[schema(value_type = Option<String>, example = "8.00")]
    pub grade_point: Option<String>,
    pub passed: bool,
}

/// Gradebook of a paper (`shared::grading` with the session's scheme).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Gradebook {
    pub paper_id: ExamPaperId,
    pub paper_code: String,
    pub title: String,
    #[schema(value_type = String, example = "4.00")]
    pub credits: String,
    #[schema(value_type = String, example = "40.00")]
    pub pass_percent: String,
    #[schema(value_type = String, example = "40.00")]
    pub internal_weight: String,
    pub candidates: usize,
    pub graded: usize,
    pub passed: usize,
    pub failed: usize,
    pub absent: usize,
    #[schema(value_type = Option<String>, example = "64.25")]
    pub average_percent: Option<String>,
    /// Grade → count.
    pub distribution: Vec<GradeCount>,
    pub lines: Vec<GradebookLine>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GradeCount {
    pub grade: String,
    pub count: usize,
}

/// A paper of a section the faculty member teaches.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssessmentView {
    pub paper_id: ExamPaperId,
    pub session_id: ExamSessionId,
    pub session_code: String,
    pub session_name: String,
    pub session_status: String,
    pub paper_code: String,
    pub title: String,
    pub paper_status: ExamPaperStatus,
    pub accepts_marks: bool,
    pub sheet_id: Option<MarkSheetId>,
    pub sheet_status: Option<EvaluationStatus>,
    pub candidates: i64,
    pub entered: i64,
    pub sections: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssessmentList {
    pub items: Vec<AssessmentView>,
}

// ------------------------------------------------------------------ bodies

/// Policy settings of a mark sheet.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkSheetSettingsBody {
    /// Submission deadline (business date).
    pub due_on: Option<NaiveDate>,
    pub second_evaluation_required: Option<bool>,
    /// First / second difference above which an adjudicating evaluation is required.
    #[schema(value_type = Option<String>, example = "10.00")]
    pub difference_threshold: Option<String>,
}

impl MarkSheetSettingsBody {
    fn settings(&self) -> Result<SheetSettings, KernelError> {
        Ok(SheetSettings {
            due_on: self.due_on,
            second_evaluation_required: self.second_evaluation_required,
            difference_threshold: parse_opt(
                "differenceThreshold",
                self.difference_threshold.as_ref(),
            )?,
        })
    }
}

/// Optional reason of a marks state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct MarksReasonBody {
    pub reason: Option<String>,
}

/// `approve | reject` (exceptions: `accept | reject`) with a reason.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MarksDecisionBody {
    pub decision: String,
    pub reason: Option<String>,
}

/// One row of a bulk marks entry (decimals as strings; `If-Match` per row via `version`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarksEntryRow {
    pub id: PaperMarkId,
    pub version: i64,
    #[schema(value_type = Option<String>, example = "61.50")]
    pub external_marks: Option<String>,
    #[schema(value_type = Option<String>)]
    pub internal_marks: Option<String>,
    #[schema(value_type = Option<String>)]
    pub internal_max: Option<String>,
    pub absent: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MarksEntryBody {
    pub rows: Vec<MarksEntryRow>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarksExceptionCreate {
    /// `late_submission | missing_marks`.
    pub kind: String,
    pub paper_mark_id: Option<PaperMarkId>,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluatorAssignmentCreate {
    /// Login subject of the evaluator.
    pub evaluator_ref: String,
    pub evaluator_name: Option<String>,
    /// `examiner | panel_member | second_examiner | adjudicator`.
    pub role: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EvaluationCreate {
    pub paper_mark_id: PaperMarkId,
    pub round: EvaluationRound,
    #[schema(value_type = String, example = "62.00")]
    pub marks: String,
    /// Defaults to the caller.
    pub evaluator_ref: Option<String>,
    pub remark: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ModerationCreate {
    /// `grace | scaling`.
    pub kind: String,
    /// Grace marks, or the scaling percentage.
    #[schema(value_type = String, example = "3.00")]
    pub value: String,
    pub reason: String,
}

fn reason_of(body: Option<Json<MarksReasonBody>>) -> Option<String> {
    body.and_then(|Json(b)| b.reason)
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
}

// ------------------------------------------------------------------ mark sheets

/// A paper's mark sheet (`exam.read`; faculty: own sections).
#[utoipa::path(get, path = "/api/v1/exams/exam-papers/{id}/mark-sheet", operation_id = "exam_mark_sheet_of_paper", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_of_paper(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let s = repo::sheet_of_paper(&st.pool, t, paper.id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "mark sheet",
        })?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Opens a paper's mark sheet (`exam.marks.manage`); the paper must accept marks (`conducted`),
/// it moves to `evaluation`.
#[utoipa::path(post, path = "/api/v1/exams/exam-papers/{id}/mark-sheet", operation_id = "exam_mark_sheet_open", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MarkSheetSettingsBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_open(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MarkSheetSettingsBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    require_idempotency_key(&headers)?;
    let settings = body.map(|Json(b)| b).unwrap_or_default().settings()?;
    let s = repo::open_sheet(&st, tenant(ctx), ExamPaperId(id), settings, &actor(ctx)).await?;
    Ok(sheet_response(StatusCode::CREATED, &s))
}

/// One mark sheet (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/mark-sheets/{id}", operation_id = "exam_mark_sheet_get", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let s = repo::get_sheet(&st, t, MarkSheetId(id)).await?;
    let paper = repo::load_paper(&st, t, s.paper_id).await?;
    authorize_paper(&st, ctx, &paper).await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Changes a sheet's policy settings before submission (`exam.marks.manage`).
#[utoipa::path(patch, path = "/api/v1/exams/mark-sheets/{id}", operation_id = "exam_mark_sheet_update", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the mark sheet")),
    request_body = MarkSheetSettingsBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_update(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MarkSheetSettingsBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let s = repo::update_sheet(
        &st,
        tenant(ctx),
        MarkSheetId(id),
        Some(v),
        b.settings()?,
        &actor(ctx),
    )
    .await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Imports finalised internal assessment from Batch 4 into the candidates' internal marks
/// (`exam.marks.manage`); re-import after a 4B correction updates rows that are not final.
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/import-internal", operation_id = "exam_mark_sheet_import_internal", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarksBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_import_internal(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<MarksBulkSummary>> {
    let ctx = auth.require(Permission::MarksManage)?;
    require_idempotency_key(&headers)?;
    let s = repo::import_internal(&st, tenant(ctx), MarkSheetId(id), &actor(ctx)).await?;
    Ok(Json(MarksBulkSummary {
        processed: s.processed,
        skipped: s.skipped,
        failed: s
            .failed
            .into_iter()
            .map(|(registration_id, reason)| MarksBulkFailure {
                registration_id,
                reason,
            })
            .collect(),
        missing: s.missing,
    }))
}

/// Evaluator submission (`exam.marks.enter`; faculty: own sections): rows `entered → submitted`,
/// sheet `in_evaluation → evaluated`, `mark_sheet.submitted`.
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/submit", operation_id = "exam_mark_sheet_submit", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the mark sheet")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_submit(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksEntry)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let s = repo::get_sheet(&st, t, MarkSheetId(id)).await?;
    let paper = repo::load_paper(&st, t, s.paper_id).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let s = repo::submit_sheet(&st, t, MarkSheetId(id), Some(v), &actor(ctx)).await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Returns submitted marks to the evaluator (`exam.marks.manage`, reason required).
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/return", operation_id = "exam_mark_sheet_return", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the mark sheet")),
    request_body = MarksReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_return(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MarksReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = reason_of(body).unwrap_or_default();
    let s = repo::return_sheet(
        &st,
        tenant(ctx),
        MarkSheetId(id),
        Some(v),
        &reason,
        &actor(ctx),
    )
    .await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Exam-cell verification (`exam.marks.manage`): rows `submitted → verified`, sheet `validated`.
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/verify", operation_id = "exam_mark_sheet_verify", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the mark sheet")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_verify(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let s = repo::verify_sheet(&st, tenant(ctx), MarkSheetId(id), Some(v), &actor(ctx)).await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Requests final marks approval (`MARKS_APPROVAL`, `exam.marks.manage`). Auto-approval returns
/// the sheet `approved`; otherwise it awaits `…/decision`.
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/request-approval", operation_id = "exam_mark_sheet_request_approval", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the mark sheet")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_request_approval(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let s = repo::request_marks_approval(&st, tenant(ctx), MarkSheetId(id), Some(v), &actor(ctx))
        .await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Decision on a pending final marks approval (`exam.approve`, step-up): `approve | reject`.
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/decision", operation_id = "exam_mark_sheet_decide", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the mark sheet")),
    request_body = MarksDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MarksDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let approve = decision(&b.decision, "approve", "reject")?;
    let s = repo::decide_sheet(
        &st,
        tenant(ctx),
        MarkSheetId(id),
        Some(v),
        approve,
        b.reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Final approved marks (`exam.marks.manage`): rows `approved → finalised`; the paper becomes
/// `marks_finalised` when every effective candidate is final.
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/finalise", operation_id = "exam_mark_sheet_finalise", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the mark sheet")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkSheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sheet_finalise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let s = repo::finalise_sheet(&st, tenant(ctx), MarkSheetId(id), Some(v), &actor(ctx)).await?;
    Ok(sheet_response(StatusCode::OK, &s))
}

/// Submission exceptions of a sheet (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/mark-sheets/{id}/exceptions", operation_id = "exam_marks_exception_list", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MarksExceptionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MarksExceptionList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let s = repo::get_sheet(&st, t, MarkSheetId(id)).await?;
    let paper = repo::load_paper(&st, t, s.paper_id).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let items = repo::exceptions_of(&st.pool, t, s.id)
        .await?
        .iter()
        .map(MarksExceptionView::from)
        .collect();
    Ok(Json(MarksExceptionList { items }))
}

/// Raises a marks submission exception (late submission / missing marks, L3-I-074;
/// `exam.marks.enter`, faculty: own sections).
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/exceptions", operation_id = "exam_marks_exception_raise", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MarksExceptionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MarksExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_raise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MarksExceptionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksEntry)?;
    require_idempotency_key(&headers)?;
    let t = tenant(ctx);
    let s = repo::get_sheet(&st, t, MarkSheetId(id)).await?;
    let paper = repo::load_paper(&st, t, s.paper_id).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let e = repo::raise_exception(
        &st,
        t,
        s.id,
        b.kind.trim(),
        b.paper_mark_id,
        &b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &MarksExceptionView::from(&e),
        Version(e.version),
    ))
}

/// Accepts or rejects a submission exception (`exam.marks.manage`; decision `accept | reject`).
#[utoipa::path(post, path = "/api/v1/exams/marks-exceptions/{id}/resolve", operation_id = "exam_marks_exception_resolve", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body = MarksDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarksExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_resolve(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MarksDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let accept = decision(&b.decision, "accept", "reject")?;
    let e = repo::resolve_exception(
        &st,
        tenant(ctx),
        MarksExceptionId(id),
        Some(v),
        accept,
        b.reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(
        StatusCode::OK,
        &MarksExceptionView::from(&e),
        Version(e.version),
    ))
}

/// Evaluators of a sheet (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/mark-sheets/{id}/assignments", operation_id = "exam_mark_sheet_assignment_list", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = EvaluatorAssignmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EvaluatorAssignmentList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let s = repo::get_sheet(&st, t, MarkSheetId(id)).await?;
    let paper = repo::load_paper(&st, t, s.paper_id).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let items = repo::assignments_of(&st, t, s.id)
        .await?
        .iter()
        .map(EvaluatorAssignmentView::from)
        .collect();
    Ok(Json(EvaluatorAssignmentList { items }))
}

/// Assigns an evaluator (examiner / panel member / second examiner / adjudicator;
/// `exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/mark-sheets/{id}/assignments", operation_id = "exam_mark_sheet_assign", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = EvaluatorAssignmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = EvaluatorAssignmentView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<EvaluatorAssignmentCreate>,
) -> ApiResult<(StatusCode, Json<EvaluatorAssignmentView>)> {
    let ctx = auth.require(Permission::MarksManage)?;
    require_idempotency_key(&headers)?;
    let a = repo::assign_evaluator(
        &st,
        tenant(ctx),
        MarkSheetId(id),
        &b.evaluator_ref,
        b.evaluator_name.as_deref(),
        b.role.trim(),
        &actor(ctx),
    )
    .await?;
    Ok((StatusCode::CREATED, Json(EvaluatorAssignmentView::from(&a))))
}

// ------------------------------------------------------------------ marks rows

/// Candidates' marks on a paper with grade previews (`exam.read`; faculty: own sections).
#[utoipa::path(get, path = "/api/v1/exams/exam-papers/{id}/marks", operation_id = "exam_marks_list", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MarkRowList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn marks_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MarkRowList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    Ok(Json(marks_list(&st, t, &paper).await?))
}

/// Bulk marks entry (`exam.marks.enter`; faculty only on papers of sections they teach, else
/// 404): each row carries its `version` (stale → 412); values through `marks::update(→ entered)`.
#[utoipa::path(put, path = "/api/v1/exams/exam-papers/{id}/marks", operation_id = "exam_marks_enter", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), request_body = MarksEntryBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkRowList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn marks_put(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<MarksEntryBody>,
) -> ApiResult<Json<MarkRowList>> {
    let ctx = auth.require(Permission::MarksEntry)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let mut rows = Vec::with_capacity(b.rows.len());
    for (i, r) in b.rows.iter().enumerate() {
        rows.push(repo::EntryRow {
            id: r.id,
            version: r.version,
            external_marks: parse_opt(
                &format!("rows[{i}].externalMarks"),
                r.external_marks.as_ref(),
            )?,
            internal_marks: parse_opt(
                &format!("rows[{i}].internalMarks"),
                r.internal_marks.as_ref(),
            )?,
            internal_max: parse_opt(&format!("rows[{i}].internalMax"), r.internal_max.as_ref())?,
            absent: r.absent,
        });
    }
    repo::enter_marks(&st, t, paper.id, rows, &actor(ctx)).await?;
    let paper = repo::load_paper(&st, t, paper.id).await?;
    Ok(Json(marks_list(&st, t, &paper).await?))
}

async fn mark_response(
    st: &ExamsState,
    t: sutra_kernel::scope::TenantId,
    c: &Candidate,
) -> Result<Response, KernelError> {
    let paper = repo::load_paper(st, t, c.mark.paper_id).await?;
    let session = repo::load_session(st, t, paper.session_id).await?;
    let bands = repo::bands_of(st, t, &session).await?;
    Ok(with_etag(
        StatusCode::OK,
        &row_view(&paper, c, bands.as_deref()),
        Version(c.mark.version),
    ))
}

/// Withholds a candidate's marks (incomplete evaluation, L3-I-076; `exam.marks.manage`, reason
/// required); `paper_mark.withheld` notifies the student.
#[utoipa::path(post, path = "/api/v1/exams/paper-marks/{id}/withhold", operation_id = "exam_marks_withhold", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the marks row")),
    request_body = MarksReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkRowView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mark_withhold(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MarksReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let reason = reason_of(body).unwrap_or_default();
    let c = repo::withhold_mark(&st, t, PaperMarkId(id), Some(v), &reason, &actor(ctx)).await?;
    Ok(mark_response(&st, t, &c).await?)
}

/// Releases withheld marks back to entry (`exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/paper-marks/{id}/release", operation_id = "exam_marks_release", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the marks row")),
    request_body = MarksReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MarkRowView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn mark_release(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MarksReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let reason = reason_of(body);
    let c = repo::release_mark(
        &st,
        t,
        PaperMarkId(id),
        Some(v),
        reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(mark_response(&st, t, &c).await?)
}

// ------------------------------------------------------------------ evaluations

/// Recorded evaluations of a paper with each script's resolution (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-papers/{id}/evaluations", operation_id = "exam_evaluation_list", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = EvaluationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn evaluation_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EvaluationList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let sheet = repo::sheet_of_paper(&st.pool, t, paper.id).await?;
    let items = repo::evaluations_of(&st.pool, t, paper.id)
        .await?
        .iter()
        .map(EvaluationView::from)
        .collect();
    let resolutions = repo::resolutions(&st, t, sheet.as_ref(), paper.id)
        .await?
        .iter()
        .map(|(k, r)| resolution_view(*k, r))
        .collect();
    Ok(Json(EvaluationList { items, resolutions }))
}

/// Records one evaluation of a script (first — one per panel member —, second, adjudication;
/// `exam.marks.enter`, faculty: own sections). The resolved value is written to the marks row.
#[utoipa::path(post, path = "/api/v1/exams/exam-papers/{id}/evaluations", operation_id = "exam_evaluation_record", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = EvaluationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = EvaluationRecorded), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn evaluation_record(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<EvaluationCreate>,
) -> ApiResult<(StatusCode, Json<EvaluationRecorded>)> {
    let ctx = auth.require(Permission::MarksEntry)?;
    require_idempotency_key(&headers)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let marks = parse_dec("marks", &b.marks)?;
    let evaluator = b
        .evaluator_ref
        .clone()
        .filter(|e| !e.trim().is_empty())
        .unwrap_or_else(|| ctx.subject.clone());
    let out = repo::record_evaluation(
        &st,
        t,
        paper.id,
        b.paper_mark_id,
        b.round,
        marks,
        &evaluator,
        b.remark.as_deref(),
        &actor(ctx),
    )
    .await?;
    let paper = repo::load_paper(&st, t, paper.id).await?;
    let session = repo::load_session(&st, t, paper.session_id).await?;
    let bands = repo::bands_of(&st, t, &session).await?;
    Ok((
        StatusCode::CREATED,
        Json(EvaluationRecorded {
            evaluation: EvaluationView::from(&out.evaluation),
            resolution: resolution_view(out.mark.mark.id.as_uuid(), &out.resolution),
            mark: row_view(&paper, &out.mark, bands.as_deref()),
        }),
    ))
}

// ------------------------------------------------------------------ moderation

/// Moderations of a paper (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-papers/{id}/moderation", operation_id = "exam_moderation_list", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ModerationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ModerationList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let items = repo::moderations_of(&st, t, paper.id)
        .await?
        .iter()
        .map(ModerationView::from)
        .collect();
    Ok(Json(ModerationList { items }))
}

/// Requests a moderation of verified marks under policy (L3-I-075; `exam.marks.manage`):
/// `grace` (≤ 10 % of the maximum) or `scaling` (50 … 120 %), with a dry-run report.
#[utoipa::path(post, path = "/api/v1/exams/exam-papers/{id}/moderation", operation_id = "exam_moderation_request", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ModerationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ModerationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_request(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ModerationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    require_idempotency_key(&headers)?;
    let kind = ModerationKind::parse(b.kind.trim())
        .ok_or_else(|| invalid("kind", "invalid", "must be grace or scaling"))?;
    let value = parse_dec("value", &b.value)?;
    let m = repo::request_moderation(
        &st,
        tenant(ctx),
        ExamPaperId(id),
        kind,
        value,
        &b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(moderation_response(StatusCode::CREATED, &m))
}

/// Validates a moderation and requests its approval (`MODERATION_APPROVAL`; `exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/moderations/{id}/validate", operation_id = "exam_moderation_validate", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the moderation")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ModerationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_validate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let m =
        repo::validate_moderation(&st, tenant(ctx), ModerationId(id), Some(v), &actor(ctx)).await?;
    Ok(moderation_response(StatusCode::OK, &m))
}

/// Decision on a validated moderation (`exam.approve`, step-up): `approve | reject`.
#[utoipa::path(post, path = "/api/v1/exams/moderations/{id}/decision", operation_id = "exam_moderation_decide", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the moderation")),
    request_body = MarksDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ModerationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MarksDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let approve = decision(&b.decision, "approve", "reject")?;
    let m = repo::decide_moderation(
        &st,
        tenant(ctx),
        ModerationId(id),
        Some(v),
        approve,
        b.reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(moderation_response(StatusCode::OK, &m))
}

/// Applies an approved moderation (`exam.marks.manage`): verified scripts `→ moderated`.
#[utoipa::path(post, path = "/api/v1/exams/moderations/{id}/apply", operation_id = "exam_moderation_apply", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the moderation")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ModerationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_apply(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let m =
        repo::apply_moderation(&st, tenant(ctx), ModerationId(id), Some(v), &actor(ctx)).await?;
    Ok(moderation_response(StatusCode::OK, &m))
}

/// Withdraws an open moderation (`exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/moderations/{id}/withdraw", operation_id = "exam_moderation_withdraw", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the moderation")),
    request_body = MarksReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ModerationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn moderation_withdraw(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MarksReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = match expected(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let reason = reason_of(body);
    let m = repo::withdraw_moderation(
        &st,
        tenant(ctx),
        ModerationId(id),
        Some(v),
        reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(moderation_response(StatusCode::OK, &m))
}

// ------------------------------------------------------------------ gradebook

/// Gradebook of a paper (`exam.read`; faculty: own sections): every effective candidate graded
/// with `shared::grading` and the session's scheme — the engine 6C computes results with.
#[utoipa::path(get, path = "/api/v1/exams/exam-papers/{id}/gradebook", operation_id = "exam_gradebook_get", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = Gradebook), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn gradebook_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<Gradebook>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    let session = repo::load_session(&st, t, paper.session_id).await?;
    let bands = repo::bands_of(&st, t, &session).await?.ok_or_else(|| {
        KernelError::Conflict(format!(
            "examination session {} has no grading scheme",
            session.code
        ))
    })?;
    let rows = repo::candidates(&st.pool, t, paper.id).await?;
    let mut lines = Vec::new();
    let mut dist: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
    let (mut graded, mut passed, mut absent) = (0usize, 0usize, 0usize);
    let mut sum = Decimal::ZERO;
    for c in rows.iter().filter(|c| c.effective()) {
        let g = grade_line(&repo::line_input(&paper, &c.mark), &bands);
        if c.mark.absent {
            absent += 1;
        }
        if let Some(p) = g.percent {
            graded += 1;
            sum += p;
            if g.passed {
                passed += 1;
            }
            if let Some(gr) = &g.grade {
                *dist.entry(gr.clone()).or_default() += 1;
            }
        }
        lines.push(GradebookLine {
            paper_mark_id: c.mark.id,
            registration_id: c.mark.registration_id,
            student_number: c.student_number.clone(),
            display_name: c.display_name.clone(),
            attempt_kind: c.attempt_kind,
            status: c.mark.status,
            absent: c.mark.absent,
            internal_marks: c.mark.internal_marks.map(fmt2),
            external_marks: c.mark.external_marks.map(fmt2),
            grace_marks: fmt2(c.mark.grace_marks),
            percent: g.percent.map(fmt2),
            grade: g.grade.clone(),
            grade_point: g.grade_point.map(fmt2),
            passed: g.passed,
        });
    }
    let average = (graded > 0).then(|| fmt2(sum / Decimal::from(graded as i64)));
    let mut distribution: Vec<GradeCount> = bands
        .iter()
        .map(|b| GradeCount {
            grade: b.grade.clone(),
            count: dist.get(&b.grade).copied().unwrap_or(0),
        })
        .collect();
    distribution.dedup_by(|a, b| a.grade == b.grade);
    Ok(Json(Gradebook {
        paper_id: paper.id,
        paper_code: paper.paper_code.clone(),
        title: paper.title.clone(),
        credits: fmt2(paper.credits),
        pass_percent: fmt2(paper.pass_percent),
        internal_weight: fmt2(paper.internal_weight),
        candidates: lines.len(),
        graded,
        passed,
        failed: graded - passed,
        absent,
        average_percent: average,
        distribution,
        lines,
    }))
}

// ------------------------------------------------------------------ faculty assessments

/// Papers of the sections the caller teaches with their sheet status (Faculty Assessments;
/// `exam.marks.enter`).
#[utoipa::path(get, path = "/api/v1/exams/me/assessments", operation_id = "exam_marks_self_assessments", tag = "exam-marks",
    security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_assessments(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<AssessmentList>> {
    let ctx = auth.require(Permission::MarksEntry)?;
    let rows = repo::assessments_of(&st, tenant(ctx), &ctx.subject).await?;
    Ok(Json(assessment_list(rows)))
}

/// Papers of an institution's examination sessions with their mark sheet status (Admin →
/// Examinations → Marks; `exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/institutions/{id}/mark-sheets", operation_id = "exam_mark_sheet_list", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssessmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn institution_sheets(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AssessmentList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let inst = sutra_kernel::scope::InstitutionId(id);
    crate::shared::guard::require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let rows = repo::papers_of_institution(&st, tenant(ctx), inst).await?;
    Ok(Json(assessment_list(rows)))
}

fn assessment_list(rows: Vec<repo::AssessmentRow>) -> AssessmentList {
    let items = rows
        .into_iter()
        .map(|a| AssessmentView {
            paper_id: a.paper.id,
            session_id: a.paper.session_id,
            session_code: a.session_code,
            session_name: a.session_name,
            session_status: a.session_status,
            paper_code: a.paper.paper_code.clone(),
            title: a.paper.title.clone(),
            paper_status: a.paper.status,
            accepts_marks: a.paper.status.accepts_marks(),
            sheet_id: a.sheet_id,
            sheet_status: a.sheet_status,
            candidates: a.candidates,
            entered: a.entered,
            sections: a.sections,
        })
        .collect();
    AssessmentList { items }
}

/// Marks of one of the caller's papers (`exam.marks.enter`; 404 unless they teach a section of
/// its offering).
#[utoipa::path(get, path = "/api/v1/exams/me/assessments/{id}/marks", operation_id = "exam_marks_self_list", tag = "exam-marks",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MarkRowList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_assessment_marks(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<MarkRowList>> {
    let ctx = auth.require(Permission::MarksEntry)?;
    let t = tenant(ctx);
    let paper = repo::load_paper(&st, t, ExamPaperId(id)).await?;
    authorize_paper(&st, ctx, &paper).await?;
    Ok(Json(marks_list(&st, t, &paper).await?))
}

pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(sheet_of_paper, sheet_open))
        .routes(routes!(sheet_get, sheet_update))
        .routes(routes!(sheet_import_internal))
        .routes(routes!(sheet_submit))
        .routes(routes!(sheet_return))
        .routes(routes!(sheet_verify))
        .routes(routes!(sheet_request_approval))
        .routes(routes!(sheet_decide))
        .routes(routes!(sheet_finalise))
        .routes(routes!(exception_list, exception_raise))
        .routes(routes!(exception_resolve))
        .routes(routes!(assignment_list, assignment_create))
        .routes(routes!(marks_get, marks_put))
        .routes(routes!(mark_withhold))
        .routes(routes!(mark_release))
        .routes(routes!(evaluation_list, evaluation_record))
        .routes(routes!(moderation_list, moderation_request))
        .routes(routes!(moderation_validate))
        .routes(routes!(moderation_decide))
        .routes(routes!(moderation_apply))
        .routes(routes!(moderation_withdraw))
        .routes(routes!(gradebook_get))
        .routes(routes!(my_assessments))
        .routes(routes!(institution_sheets))
        .routes(routes!(my_assessment_marks))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_and_decimals_parse() {
        assert!(matches!(decision("approve", "approve", "reject"), Ok(true)));
        assert!(matches!(decision("reject", "approve", "reject"), Ok(false)));
        assert!(decision("maybe", "approve", "reject").is_err());
        assert!(parse_dec("marks", " 61.50 ").is_ok());
        assert!(parse_dec("marks", "sixty").is_err());
        let r = resolution_view(Uuid::nil(), &Resolution::Resolved(Decimal::new(6350, 2)));
        assert_eq!(r.state, "resolved");
        assert_eq!(r.marks.as_deref(), Some("63.50"));
    }
}
