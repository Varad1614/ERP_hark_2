//! HTTP API of malpractice cases (tag `exam-integrity`; contract §5.2). Reads `exam.read`;
//! case handling `exam.marks.manage`; the explicit decision on a pending committee decision /
//! review outcome `exam.approve` (step-up).

use super::repo::{self, CaseRow, DecisionRow, NewCase};
use crate::marks::repo::invalid;
use crate::shared::grading::fmt2;
use crate::shared::http::{actor, if_match, require_exam_official, with_etag};
use crate::shared::scope::tenant;
use crate::shared::ExamsState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::str::FromStr;
use sutra_contracts::exams::ids::{
    ExamPaperId, ExamRegistrationId, ExamSessionId, MalpracticeCaseId, MalpracticeDecisionId,
};
use sutra_contracts::exams::status::{MalpracticeCaseStatus, MalpracticePenalty};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One decision / review / merge / reopen record.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MalpracticeDecisionView {
    pub id: MalpracticeDecisionId,
    /// `decision | review | merge | reopen`.
    pub kind: String,
    pub outcome: String,
    pub penalty: Option<String>,
    #[schema(value_type = Option<String>, example = "10.00")]
    pub marks_deduction: Option<String>,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<&DecisionRow> for MalpracticeDecisionView {
    fn from(d: &DecisionRow) -> Self {
        Self {
            id: d.id,
            kind: d.kind.clone(),
            outcome: d.outcome.clone(),
            penalty: d.penalty.clone(),
            marks_deduction: d.marks_deduction.map(fmt2),
            reason: d.reason.clone(),
            decided_by: d.decided_by.clone(),
            process_instance_id: d.process_instance_id.clone(),
            created_at: d.created_at,
        }
    }
}

/// A proposal awaiting its governed decision.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MalpracticeProposalView {
    /// `decision | review`.
    pub kind: String,
    pub outcome: Option<String>,
    pub penalty: Option<MalpracticePenalty>,
    #[schema(value_type = Option<String>)]
    pub marks_deduction: Option<String>,
    pub reason: Option<String>,
}

/// A malpractice case. 6C reads `sessionId, registrationId, paperId, status, penalty,
/// marksDeduction` of decided cases.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MalpracticeCaseView {
    pub id: MalpracticeCaseId,
    pub case_number: String,
    pub session_id: ExamSessionId,
    pub registration_id: ExamRegistrationId,
    pub paper_id: Option<ExamPaperId>,
    pub student_id: Uuid,
    pub allegation: String,
    pub evidence: Option<String>,
    pub status: MalpracticeCaseStatus,
    pub status_reason: Option<String>,
    pub penalty: Option<MalpracticePenalty>,
    #[schema(value_type = Option<String>, example = "10.00")]
    pub marks_deduction: Option<String>,
    pub pending: Option<MalpracticeProposalView>,
    pub merged_into_id: Option<MalpracticeCaseId>,
    pub process_instance_id: Option<String>,
    pub reported_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub decisions: Vec<MalpracticeDecisionView>,
    pub version: i64,
}

fn case_view(c: &CaseRow, decisions: &[DecisionRow]) -> MalpracticeCaseView {
    MalpracticeCaseView {
        id: c.id,
        case_number: c.case_number.clone(),
        session_id: c.session_id,
        registration_id: c.registration_id,
        paper_id: c.paper_id,
        student_id: c.student_id,
        allegation: c.allegation.clone(),
        evidence: c.evidence.clone(),
        status: c.status,
        status_reason: c.status_reason.clone(),
        penalty: c.penalty,
        marks_deduction: c.marks_deduction.map(fmt2),
        pending: c.proposal_kind.as_ref().map(|k| MalpracticeProposalView {
            kind: k.clone(),
            outcome: c.proposed_outcome.clone(),
            penalty: c.proposed_penalty,
            marks_deduction: c.proposed_deduction.map(fmt2),
            reason: c.proposed_reason.clone(),
        }),
        merged_into_id: c.merged_into_id,
        process_instance_id: c.process_instance_id.clone(),
        reported_by: c.reported_by.clone(),
        decided_at: c.decided_at,
        closed_at: c.closed_at,
        archived_at: c.archived_at,
        created_at: c.created_at,
        decisions: decisions
            .iter()
            .map(MalpracticeDecisionView::from)
            .collect(),
        version: c.version,
    }
}

async fn respond(
    st: &ExamsState,
    t: sutra_kernel::scope::TenantId,
    c: &CaseRow,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let decisions = repo::decisions_of(st, t, c.id).await?;
    Ok(with_etag(
        status,
        &case_view(c, &decisions),
        Version(c.version),
    ))
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MalpracticeCaseList {
    pub items: Vec<MalpracticeCaseView>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
pub struct MalpracticeListQuery {
    pub status: Option<MalpracticeCaseStatus>,
}

/// Suspected malpractice report.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MalpracticeCaseCreate {
    pub registration_id: ExamRegistrationId,
    /// The paper concerned; omit for the whole registration.
    pub paper_id: Option<ExamPaperId>,
    pub allegation: String,
    pub evidence: Option<String>,
}

/// Optional reason of a malpractice state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct MalpracticeReasonBody {
    pub reason: Option<String>,
}

/// Committee decision with a penalty.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MalpracticeDecisionCreate {
    pub penalty: MalpracticePenalty,
    /// Required for `marks_deduction` (decimal string).
    #[schema(value_type = Option<String>, example = "10.00")]
    pub marks_deduction: Option<String>,
    pub reason: String,
}

/// Appeal review outcome.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MalpracticeReviewCreate {
    /// `uphold | modify | overturn`.
    pub outcome: String,
    /// New penalty when modifying.
    pub penalty: Option<MalpracticePenalty>,
    #[schema(value_type = Option<String>)]
    pub marks_deduction: Option<String>,
    pub reason: String,
}

/// `approve | reject` of the pending proposal.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct MalpracticeApprovalBody {
    pub decision: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MalpracticeMergeBody {
    /// The authoritative case of the same candidate and paper.
    pub into_case_id: MalpracticeCaseId,
    pub reason: Option<String>,
}

fn reason_of(body: Option<Json<MalpracticeReasonBody>>) -> Option<String> {
    body.and_then(|Json(b)| b.reason)
        .map(|r| r.trim().to_owned())
        .filter(|r| !r.is_empty())
}

fn parse_deduction(raw: Option<&String>) -> Result<Option<Decimal>, KernelError> {
    raw.map(|r| {
        Decimal::from_str(r.trim()).map_err(|_| {
            invalid(
                "marksDeduction",
                "invalid_number",
                "must be a decimal string such as \"10.00\"",
            )
        })
    })
    .transpose()
}

macro_rules! if_match_or_return {
    ($headers:expr) => {
        match if_match(&$headers)? {
            Ok(v) => v.0,
            Err(resp) => return Ok(resp),
        }
    };
}

// ------------------------------------------------------------------ handlers

/// Malpractice cases of a session (`exam.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/malpractice-cases", operation_id = "exam_malpractice_list", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), MalpracticeListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = MalpracticeCaseList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<MalpracticeListQuery>,
) -> ApiResult<Json<MalpracticeCaseList>> {
    let ctx = require_exam_official(&auth)?;
    let t = tenant(ctx);
    crate::marks::repo::load_session(&st, t, ExamSessionId(id)).await?;
    let mut items = Vec::new();
    for c in repo::cases_of_session(&st, t, ExamSessionId(id), q.status).await? {
        let d = repo::decisions_of(&st, t, c.id).await?;
        items.push(case_view(&c, &d));
    }
    Ok(Json(MalpracticeCaseList { items }))
}

/// Reports suspected malpractice (`exam.marks.manage`; `MAL-` number). A second open case for
/// the same candidate and paper is `409`.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/malpractice-cases", operation_id = "exam_malpractice_report", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = MalpracticeCaseCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_report(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MalpracticeCaseCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    require_idempotency_key(&headers)?;
    let t = tenant(ctx);
    let c = repo::report_case(
        &st,
        t,
        ExamSessionId(id),
        NewCase {
            registration_id: b.registration_id,
            paper_id: b.paper_id,
            allegation: b.allegation,
            evidence: b.evidence,
        },
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::CREATED).await?)
}

/// One malpractice case with its decisions (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/malpractice-cases/{id}", operation_id = "exam_malpractice_get", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = require_exam_official(&auth)?;
    let t = tenant(ctx);
    let c = repo::get_case(&st, t, MalpracticeCaseId(id)).await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

async fn simple(
    st: &ExamsState,
    auth: &Authenticated,
    id: Uuid,
    headers: &HeaderMap,
    to: MalpracticeCaseStatus,
    reason: Option<String>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let c = repo::advance(
        st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        to,
        reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(respond(st, t, &c, StatusCode::OK).await?)
}

/// Opens the investigation (`reported → under_investigation`; `exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/investigate", operation_id = "exam_malpractice_investigate", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_investigate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MalpracticeReasonBody>>,
) -> ApiResult<Response> {
    simple(
        &st,
        &auth,
        id,
        &headers,
        MalpracticeCaseStatus::UnderInvestigation,
        reason_of(body),
    )
    .await
}

/// Schedules the committee hearing (`under_investigation → hearing`; `exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/hearing", operation_id = "exam_malpractice_hearing", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_hearing(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MalpracticeReasonBody>>,
) -> ApiResult<Response> {
    simple(
        &st,
        &auth,
        id,
        &headers,
        MalpracticeCaseStatus::Hearing,
        reason_of(body),
    )
    .await
}

/// Committee decision with a penalty (`MALPRACTICE_DECISION`; `exam.marks.manage`). A
/// debarring penalty sets the registered paper(s) `debarred`.
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/decision", operation_id = "exam_malpractice_decide", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeDecisionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_decide(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MalpracticeDecisionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    // Part 2 · S6-pilot (DEF-P2-PILOT-001): this is the MAKER step (the committee decision is
    // proposed and requests MALPRACTICE_DECISION); the separation-of-duties check belongs on the
    // governed decision, `case_approval`. Checking here refused a re-proposal by the same exam
    // official after a rejection and left the real decision unchecked.
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let c = repo::propose_decision(
        &st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        b.penalty,
        parse_deduction(b.marks_deduction.as_ref())?,
        &b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

/// Appeal against a decision (`decided → appealed`; `exam.marks.manage`, reason required).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/appeal", operation_id = "exam_malpractice_appeal", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_appeal(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MalpracticeReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let c = repo::appeal(
        &st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        &reason_of(body).unwrap_or_default(),
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

/// Appeal review outcome (`MALPRACTICE_REVIEW`; `exam.marks.manage`): `uphold`, `modify`
/// (new penalty) or `overturn` (papers restored).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/review", operation_id = "exam_malpractice_review", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeReviewCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_review(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MalpracticeReviewCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let c = repo::propose_review(
        &st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        b.outcome.trim(),
        b.penalty,
        parse_deduction(b.marks_deduction.as_ref())?,
        &b.reason,
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

/// Decision on the pending committee decision / review outcome (`exam.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/approval", operation_id = "exam_malpractice_approval", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeApprovalBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_approval(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MalpracticeApprovalBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    // Part 2 · S6-pilot (DEF-P2-PILOT-001, found by code review while designing UAT case
    // UAT-EXM-01; the dry run verifies the fix): the exam official who
    // proposed the committee decision / review outcome never approves it (maker ≠ checker,
    // DEF-X-002); a system or unidentified actor never decides it either.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "malpractice decision").await?;
    let v = if_match_or_return!(headers);
    let approve = match b.decision.as_str() {
        "approve" => true,
        "reject" => false,
        _ => {
            return Err(invalid("decision", "invalid", "must be approve or reject").into());
        }
    };
    let t = tenant(ctx);
    let c = repo::approve_proposal(
        &st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        approve,
        b.reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

/// Formal reopening after an authorised review (`exam.marks.manage`, reason required).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/reopen", operation_id = "exam_malpractice_reopen", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_reopen(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MalpracticeReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let c = repo::reopen(
        &st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        &reason_of(body).unwrap_or_default(),
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

/// Merges an undecided duplicate into the authoritative case (`exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/merge", operation_id = "exam_malpractice_merge", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duplicate case")),
    request_body = MalpracticeMergeBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_merge(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<MalpracticeMergeBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let c = repo::merge(
        &st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        b.into_case_id,
        b.reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

/// Closes a decided case (`exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/close", operation_id = "exam_malpractice_close", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = MalpracticeReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_close(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<MalpracticeReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let reason = reason_of(body);
    let c = repo::close(
        &st,
        t,
        MalpracticeCaseId(id),
        Some(v),
        reason.as_deref(),
        &actor(ctx),
    )
    .await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

/// Archives a closed case (`exam.marks.manage`).
#[utoipa::path(post, path = "/api/v1/exams/malpractice-cases/{id}/archive", operation_id = "exam_malpractice_archive", tag = "exam-integrity",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = MalpracticeCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_archive(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::MarksManage)?;
    let v = if_match_or_return!(headers);
    let t = tenant(ctx);
    let c = repo::archive(&st, t, MalpracticeCaseId(id), Some(v), &actor(ctx)).await?;
    Ok(respond(&st, t, &c, StatusCode::OK).await?)
}

pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(case_list, case_report))
        .routes(routes!(case_get))
        .routes(routes!(case_investigate))
        .routes(routes!(case_hearing))
        .routes(routes!(case_decide))
        .routes(routes!(case_appeal))
        .routes(routes!(case_review))
        .routes(routes!(case_approval))
        .routes(routes!(case_reopen))
        .routes(routes!(case_merge))
        .routes(routes!(case_close))
        .routes(routes!(case_archive))
        .with_state(state)
}
