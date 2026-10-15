//! HTTP API of fee heads, fee structures, charges, instalments and disputes (tag
//! `finance-charges`; contract §5.2).

use super::fees::{
    self, FeeHeadView, FeeStructureView, GenerationResult, NewFeeHead, NewStructure,
};
use super::kit::{
    if_match, invalid, opt_text, parse_amount, reason, require_text, DecisionKind, ListQuery,
    ReasonBody,
};
use super::repo::{
    self, ChargeDetail, ChargeListItem, ChargeRevisionView, Command, DisputeView, Resolution,
    Revision,
};
use crate::shared::charge::load_charge;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_accounts;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sutra_contracts::finance::ids::{
    ChargeDisputeId, ChargeId, FeeHeadId, FeeStructureId, InstalmentId, StudentAccountId,
};
use sutra_contracts::finance::status::{
    ChargeDisputeStatus, ChargeSourceType, ChargeStatus, FeeCategory, FeeHeadStatus,
    FeeStructureStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ================================================================ DTOs

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeeHeadList {
    pub items: Vec<FeeHeadView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeHeadCreate {
    #[schema(example = "HOSTEL")]
    pub code: String,
    pub name: String,
    pub category: FeeCategory,
    /// Defaults to the category's income account of the default chart.
    pub income_account_id: Option<Uuid>,
    #[serde(default)]
    pub refundable: bool,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeHeadUpdate {
    pub name: Option<String>,
    pub refundable: Option<bool>,
    pub status: Option<FeeHeadStatus>,
}

/// A line of a charge or fee structure.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LineInput {
    pub fee_head_id: Uuid,
    pub description: Option<String>,
    #[schema(example = "60000.00")]
    pub amount: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FeeStructureList {
    pub items: Vec<FeeStructureView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeStructureCreate {
    #[schema(example = "BTECH-Y1")]
    pub code: String,
    pub name: String,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub programme_id: Option<Uuid>,
    pub due_date: Option<NaiveDate>,
    pub lines: Vec<LineInput>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GenerateCharges {
    pub student_ids: Vec<Uuid>,
    /// Publish each generated charge (default `true`); otherwise they stay `validated`.
    pub publish: Option<bool>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct StructureQuery {
    pub status: Option<FeeStructureStatus>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeList {
    pub items: Vec<ChargeListItem>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ChargeDetailList {
    pub items: Vec<ChargeDetail>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ChargeQuery {
    pub status: Option<ChargeStatus>,
    /// Charge number, student name / number or description.
    pub q: Option<String>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<String>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
}

/// A manual charge (business obligation outside a fee structure).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeCreate {
    pub student_id: Uuid,
    pub description: String,
    pub due_date: Option<NaiveDate>,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    /// `manual` (default) or `fine`.
    pub source_type: Option<ChargeSourceType>,
    pub lines: Vec<LineInput>,
    /// Validate and publish at once (default `false`: the charge stays a draft).
    #[serde(default)]
    pub publish: bool,
}

/// A governed revision.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeRevise {
    pub reason: String,
    pub description: Option<String>,
    pub due_date: Option<NaiveDate>,
    /// New net amount; becomes a posted credit / debit note.
    #[schema(example = "120000.00")]
    pub new_amount: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ChargeRevisionList {
    pub items: Vec<ChargeRevisionView>,
}

/// A due schedule: explicit instalments, or `count` equal monthly instalments from
/// `firstDueDate` (remainder in the last one).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalmentPlanCreate {
    pub instalments: Option<Vec<InstalmentInput>>,
    pub count: Option<u32>,
    pub first_due_date: Option<NaiveDate>,
    /// Months between instalments (default 1).
    pub interval_months: Option<u32>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalmentInput {
    pub due_date: NaiveDate,
    #[schema(example = "62500.00")]
    pub amount: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalmentUpdate {
    pub due_date: NaiveDate,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DisputeCreate {
    pub reason: String,
    /// Part of the outstanding amount in dispute (default: all of it).
    pub amount: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DisputeList {
    pub items: Vec<DisputeView>,
}

/// Decision on a dispute: `approve` upholds it, `reject` rejects it.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DisputeDecision {
    pub decision: DecisionKind,
    pub reason: Option<String>,
    /// For an upheld dispute: `none` (default), `credit` (post `creditAmount` as a credit note) or `waive` (waive the outstanding amount).
    pub resolution: Option<String>,
    #[schema(example = "5000.00")]
    pub credit_amount: Option<String>,
}

fn lines(input: &[LineInput]) -> Result<Vec<crate::shared::charge::NewChargeLine>, KernelError> {
    if input.is_empty() {
        return Err(invalid("lines", "required", "at least one line"));
    }
    input
        .iter()
        .enumerate()
        .map(|(i, l)| {
            Ok(repo::line(
                l.fee_head_id,
                opt_text(l.description.as_deref()).unwrap_or_else(|| "Fee".into()),
                parse_amount(&format!("lines[{i}].amount"), &l.amount)?,
            ))
        })
        .collect()
}

fn ok<T: Serialize>(body: &T, version: i64, status: StatusCode) -> Response {
    with_etag(status, body, Version(version))
}

/// The charge, when the caller may see it as staff (`finance.read`) or as its own party.
async fn readable_charge(
    st: &FinanceState,
    ctx: &AuthContext,
    id: ChargeId,
) -> Result<crate::shared::charge::ChargeRow, KernelError> {
    // SEC-P1-AUTH-005: no row read for a caller who can neither read finance nor self-serve.
    if !ctx.has_permission(Permission::FinanceRead) {
        ctx.require(Permission::FinanceSelfService)?;
    }
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load_charge(&mut conn, ctx.scope.tenant_id, id).await?;
    drop(conn);
    if ctx.has_permission(Permission::FinanceRead) {
        return Ok(c);
    }
    let mine = my_accounts(st, ctx).await?;
    if mine.iter().any(|a| a.id == c.student_account_id) && c.status.is_visible_to_student() {
        Ok(c)
    } else {
        Err(KernelError::NotFound { entity: "charge" })
    }
}

// ================================================================ fee heads

/// Fee heads of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/fee-heads", operation_id = "finance_fee_head_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FeeHeadList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_head_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FeeHeadList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let items = fees::list_heads(&st, ctx.scope.tenant_id, InstitutionId(id)).await?;
    Ok(Json(FeeHeadList { items }))
}

/// Create a fee head (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/fee-heads", operation_id = "finance_fee_head_create", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = FeeHeadCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FeeHeadView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_head_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FeeHeadCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution_writable(&st, &scope, InstitutionId(id)).await?;
    let h = fees::create_head(
        &st,
        &scope,
        InstitutionId(id),
        NewFeeHead {
            code: b.code,
            name: b.name,
            category: b.category,
            income_account_id: b.income_account_id,
            refundable: b.refundable,
        },
        &actor(ctx),
    )
    .await?;
    Ok(ok(&h, h.version, StatusCode::CREATED))
}

/// One fee head (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/fee-heads/{id}", operation_id = "finance_fee_head_get", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FeeHeadView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_head_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let h = fees::get_head(&st, ctx.scope.tenant_id, FeeHeadId(id)).await?;
    Ok(ok(&h, h.version, StatusCode::OK))
}

/// Rename, retire or reactivate a fee head (`charge.manage`).
#[utoipa::path(patch, path = "/api/v1/finance/fee-heads/{id}", operation_id = "finance_fee_head_update", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the fee head")),
    request_body = FeeHeadUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FeeHeadView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_head_update(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FeeHeadUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    let h = fees::update_head(
        &st,
        ctx.scope.tenant_id,
        FeeHeadId(id),
        v,
        opt_text(b.name.as_deref()),
        b.refundable,
        b.status,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&h, h.version, StatusCode::OK))
}

// ================================================================ fee structures

/// Fee structure versions of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/fee-structures", operation_id = "finance_fee_structure_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), StructureQuery), security(("session_cookie" = [])),
    responses((status = 200, body = FeeStructureList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StructureQuery>,
) -> ApiResult<Json<FeeStructureList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let items =
        fees::list_structures(&st, ctx.scope.tenant_id, InstitutionId(id), q.status).await?;
    Ok(Json(FeeStructureList { items }))
}

/// Create a draft fee structure version (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/fee-structures", operation_id = "finance_fee_structure_create", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = FeeStructureCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FeeStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FeeStructureCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution_writable(&st, &scope, InstitutionId(id)).await?;
    let name = require_text("name", Some(&b.name))?;
    let s = fees::create_structure(
        &st,
        &scope,
        InstitutionId(id),
        NewStructure {
            code: b.code,
            name,
            academic_year_id: b.academic_year_id,
            academic_term_id: b.academic_term_id,
            programme_id: b.programme_id,
            due_date: b.due_date,
            lines: lines(&b.lines)?,
        },
        &actor(ctx),
    )
    .await?;
    Ok(ok(&s, s.version, StatusCode::CREATED))
}

/// One fee structure version (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/fee-structures/{id}", operation_id = "finance_fee_structure_get", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FeeStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let s = fees::get_structure(&st, ctx.scope.tenant_id, FeeStructureId(id)).await?;
    Ok(ok(&s, s.version, StatusCode::OK))
}

/// Submit a draft version for approval (WorkflowPort `finance.fee_structure_approval`; `charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/fee-structures/{id}/submit", operation_id = "finance_fee_structure_submit", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the fee structure")),
    request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FeeStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_submit(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    _body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    let s = fees::submit_structure(&st, ctx.scope.tenant_id, FeeStructureId(id), v, &actor(ctx))
        .await?;
    Ok(ok(&s, s.version, StatusCode::OK))
}

/// Approve or return a version in review (`finance.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/finance/fee-structures/{id}/decision", operation_id = "finance_fee_structure_decide", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the fee structure")),
    request_body = super::kit::DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FeeStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_decide(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<super::kit::DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    let v = if_match!(headers);
    let s = fees::decide_structure(
        &st,
        ctx.scope.tenant_id,
        FeeStructureId(id),
        v,
        b.decision == DecisionKind::Approve,
        opt_text(b.reason.as_deref()),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&s, s.version, StatusCode::OK))
}

/// Publish an approved version (supersedes the previous published version of the code; `charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/fee-structures/{id}/publish", operation_id = "finance_fee_structure_publish", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the fee structure")),
    request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FeeStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_publish(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    _body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    let s = fees::publish_structure(&st, ctx.scope.tenant_id, FeeStructureId(id), v, &actor(ctx))
        .await?;
    Ok(ok(&s, s.version, StatusCode::OK))
}

/// Withdraw a version: a published one becomes superseded, an unpublished one retired (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/fee-structures/{id}/supersede", operation_id = "finance_fee_structure_supersede", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the fee structure")),
    request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FeeStructureView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_supersede(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    let s = fees::retire_structure(
        &st,
        ctx.scope.tenant_id,
        FeeStructureId(id),
        v,
        reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&s, s.version, StatusCode::OK))
}

/// Generate (and by default publish) one charge per student from a published structure; idempotent per student (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/fee-structures/{id}/generate-charges", operation_id = "finance_fee_structure_generate_charges", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = GenerateCharges, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GenerationResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_structure_generate_charges(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GenerateCharges>,
) -> ApiResult<Json<GenerationResult>> {
    let ctx = auth.require(Permission::ChargeManage)?;
    require_idempotency_key(&headers)?;
    let r = fees::generate_charges(
        &st,
        ctx.scope.tenant_id,
        FeeStructureId(id),
        b.student_ids,
        b.publish.unwrap_or(true),
        &actor(ctx),
    )
    .await?;
    Ok(Json(r))
}

// ================================================================ charges

/// Charges of an institution, newest first (`finance.read`; keyset-paged with `cursor`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/charges", operation_id = "finance_charge_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ChargeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ChargeList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ChargeQuery>,
) -> ApiResult<Json<ChargeList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let limit = ListQuery {
        status: None,
        limit: q.limit,
    }
    .limit();
    let (items, next_cursor) = repo::list(
        &st,
        ctx.scope.tenant_id,
        Some(InstitutionId(id)),
        None,
        q.status,
        q.q.as_deref(),
        q.cursor.as_deref(),
        limit,
    )
    .await?;
    Ok(Json(ChargeList { items, next_cursor }))
}

/// Raise a manual charge for a student (draft, or published at once with `publish`; `charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/charges", operation_id = "finance_charge_create", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ChargeCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ChargeCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let description = require_text("description", Some(&b.description))?;
    let source_type = b.source_type.unwrap_or(ChargeSourceType::Manual);
    if !matches!(
        source_type,
        ChargeSourceType::Manual | ChargeSourceType::Fine
    ) {
        return Err(invalid("sourceType", "invalid", "manual or fine").into());
    }
    let input = repo::ChargeInput {
        student_id: StudentId(b.student_id),
        description,
        due_date: b.due_date,
        academic_year_id: b.academic_year_id,
        academic_term_id: b.academic_term_id,
        source_type,
        source_id: None,
        lines: lines(&b.lines)?,
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = repo::create_in_tx(&st, &mut tx, &scope, institution, &input, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = if b.publish {
        repo::publish(&st, ctx.scope.tenant_id, c.id, c.version, true, &a).await?
    } else {
        repo::get_detail(&st, ctx.scope.tenant_id, c.id).await?
    };
    Ok(ok(&d, d.version, StatusCode::CREATED))
}

/// Charges of one party account (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/student-accounts/{id}/charges", operation_id = "finance_charge_list_of_account", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ChargeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ChargeList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_list_of_account(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ChargeQuery>,
) -> ApiResult<Json<ChargeList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    st.charges
        .student_account(&scope, StudentAccountId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "student account",
        })?;
    let limit = ListQuery {
        status: None,
        limit: q.limit,
    }
    .limit();
    let (items, next_cursor) = repo::list(
        &st,
        ctx.scope.tenant_id,
        None,
        Some(StudentAccountId(id)),
        q.status,
        q.q.as_deref(),
        q.cursor.as_deref(),
        limit,
    )
    .await?;
    Ok(Json(ChargeList { items, next_cursor }))
}

/// One charge with lines, due schedule and publication entry (`finance.read`, or the student's own visible charge).
#[utoipa::path(get, path = "/api/v1/finance/charges/{id}", operation_id = "finance_charge_get", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    readable_charge(&st, ctx, ChargeId(id)).await?;
    let d = repo::get_detail(&st, ctx.scope.tenant_id, ChargeId(id)).await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

/// The caller's own visible charges with lines and due schedules (`finance.self`).
#[utoipa::path(get, path = "/api/v1/finance/me/charges", operation_id = "finance_charge_self_list", tag = "finance-charges",
    security(("session_cookie" = [])),
    responses((status = 200, body = ChargeDetailList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_self_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
) -> ApiResult<Json<ChargeDetailList>> {
    let ctx = auth.require(Permission::FinanceSelfService)?;
    let mut items = Vec::new();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    for a in my_accounts(&st, ctx).await? {
        let scope = institution_scope(ctx, a.institution_id);
        let today = business_today(&st, &scope, a.institution_id).await?;
        for c in st.charges.charges_of_account(&scope, a.id).await? {
            if !c.status.is_visible_to_student() {
                continue;
            }
            let row = load_charge(&mut conn, ctx.scope.tenant_id, c.id).await?;
            items.push(repo::detail(&mut conn, &row, today).await?);
        }
    }
    Ok(Json(ChargeDetailList { items }))
}

/// Validate a draft charge (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/charges/{id}/validate", operation_id = "finance_charge_validate", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the charge")),
    request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_validate(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    _body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    let d = repo::validate(&st, ctx.scope.tenant_id, ChargeId(id), v, &actor(ctx)).await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

/// Publish a charge to the student and recognise it in the ledger (Dr receivables / Cr income; `charge.manage`; a pending publication authority decision is completed by `finance.approve`).
#[utoipa::path(post, path = "/api/v1/finance/charges/{id}/publish", operation_id = "finance_charge_publish", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the charge"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_publish(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    _body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let can_decide = ctx.require(Permission::FinanceApprove).is_ok();
    let d = repo::publish(
        &st,
        ctx.scope.tenant_id,
        ChargeId(id),
        v,
        can_decide,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

/// Governed revision: description / due date, or a new net amount realised as a posted credit / debit note (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/charges/{id}/revise", operation_id = "finance_charge_revise", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the charge"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ChargeRevise, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_revise(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ChargeRevise>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let new_amount = match b.new_amount.as_deref() {
        Some(raw) => Some(parse_amount("newAmount", raw)?),
        None => None,
    };
    let d = repo::revise(
        &st,
        ctx.scope.tenant_id,
        ChargeId(id),
        v,
        Revision {
            reason: require_text("reason", Some(&b.reason))?,
            description: opt_text(b.description.as_deref()),
            due_date: b.due_date,
            new_amount,
        },
        &actor(ctx),
    )
    .await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

/// Append-only revision history of a charge (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/charges/{id}/revisions", operation_id = "finance_charge_revision_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ChargeRevisionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn charge_revision_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ChargeRevisionList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let items = repo::revisions(&st, ctx.scope.tenant_id, ChargeId(id)).await?;
    Ok(Json(ChargeRevisionList { items }))
}

macro_rules! charge_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $cmd:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "finance-charges",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the charge")),
            request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(
            State(st): State<FinanceState>,
            auth: Authenticated,
            Path(id): Path<Uuid>,
            headers: HeaderMap,
            body: Option<Json<ReasonBody>>,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::ChargeManage)?;
            let v = if_match!(headers);
            let d = repo::command(&st, ctx.scope.tenant_id, ChargeId(id), v, $cmd, reason(body), &actor(ctx)).await?;
            Ok(ok(&d, d.version, StatusCode::OK))
        }
    };
}

charge_cmd!(
    /// Suspend a published / active charge (not demanded while suspended; reason required; `charge.manage`).
    charge_suspend, "/api/v1/finance/charges/{id}/suspend", "finance_charge_suspend", Command::Suspend);
charge_cmd!(
    /// Resume a suspended charge (`charge.manage`).
    charge_resume, "/api/v1/finance/charges/{id}/resume", "finance_charge_resume", Command::Resume);
charge_cmd!(
    /// Cancel a charge; a published one must have nothing settled and its receivable is cleared in the ledger (reason required; `charge.manage`).
    charge_cancel, "/api/v1/finance/charges/{id}/cancel", "finance_charge_cancel", Command::Cancel);
charge_cmd!(
    /// Financially close a fully settled charge (`charge.manage`).
    charge_close, "/api/v1/finance/charges/{id}/close", "finance_charge_close", Command::Close);
charge_cmd!(
    /// Archive a closed / cancelled / waived / written-off charge (immutable afterwards; `charge.manage`).
    charge_archive, "/api/v1/finance/charges/{id}/archive", "finance_charge_archive", Command::Archive);

// ================================================================ instalments

/// Due schedule of a charge (`finance.read`, or the student's own visible charge).
#[utoipa::path(get, path = "/api/v1/finance/charges/{id}/instalments", operation_id = "finance_instalment_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn instalment_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    readable_charge(&st, ctx, ChargeId(id)).await?;
    let d = repo::get_detail(&st, ctx.scope.tenant_id, ChargeId(id)).await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

/// Set or replace the due schedule of a charge (instalments sum to the net amount; `charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/charges/{id}/instalments", operation_id = "finance_instalment_plan_create", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = InstalmentPlanCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn instalment_plan_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<InstalmentPlanCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    require_idempotency_key(&headers)?;
    let items = match (&b.instalments, b.count, b.first_due_date) {
        (Some(list), _, _) => list
            .iter()
            .enumerate()
            .map(|(i, x)| {
                Ok((
                    x.due_date,
                    parse_amount(&format!("instalments[{i}].amount"), &x.amount)?,
                ))
            })
            .collect::<Result<Vec<_>, KernelError>>()?,
        (None, Some(count), Some(first)) => {
            if count == 0 || count > 24 {
                return Err(invalid("count", "invalid", "1–24 instalments").into());
            }
            let detail = repo::get_detail(&st, ctx.scope.tenant_id, ChargeId(id)).await?;
            let net = super::kit::parse_decimal("totalAmount", &detail.total_amount)?
                + super::kit::parse_decimal("adjustmentAmount", &detail.adjustment_amount)?;
            super::domain::monthly_dates(first, count, b.interval_months.unwrap_or(1))
                .into_iter()
                .zip(super::domain::split_equal(net, count))
                .collect()
        }
        _ => {
            return Err(invalid(
                "instalments",
                "required",
                "give instalments, or count and firstDueDate",
            )
            .into())
        }
    };
    let d = repo::set_schedule(
        &st,
        ctx.scope.tenant_id,
        ChargeId(id),
        items,
        opt_text(b.reason.as_deref()),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&d, d.version, StatusCode::CREATED))
}

/// Move the due date of one unpaid instalment (`charge.manage`).
#[utoipa::path(patch, path = "/api/v1/finance/charges/{id}/instalments/{instalmentId}", operation_id = "finance_instalment_update", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("instalmentId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag (version) of the instalment")),
    request_body = InstalmentUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ChargeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn instalment_update(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path((id, instalment_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(b): Json<InstalmentUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let v = if_match!(headers);
    let d = repo::reschedule(
        &st,
        ctx.scope.tenant_id,
        ChargeId(id),
        InstalmentId(instalment_id),
        v,
        b.due_date,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

// ================================================================ disputes

/// Disputes of a charge (`finance.read`, or the student's own charge).
#[utoipa::path(get, path = "/api/v1/finance/charges/{id}/disputes", operation_id = "finance_dispute_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DisputeList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dispute_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DisputeList>> {
    let ctx = &auth.0;
    readable_charge(&st, ctx, ChargeId(id)).await?;
    let items = repo::disputes_of_charge(&st, ctx.scope.tenant_id, ChargeId(id)).await?;
    Ok(Json(DisputeList { items }))
}

/// Raise a dispute on a published / active charge (`charge.manage`, or the student on their own charge with `finance.self`).
#[utoipa::path(post, path = "/api/v1/finance/charges/{id}/disputes", operation_id = "finance_dispute_create", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = DisputeCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = DisputeView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dispute_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DisputeCreate>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    require_idempotency_key(&headers)?;
    let by_student = !ctx.has_permission(Permission::ChargeManage);
    if by_student {
        ctx.require(Permission::FinanceSelfService)?;
        readable_charge(&st, ctx, ChargeId(id)).await?;
    }
    let reason = require_text("reason", Some(&b.reason))?;
    let amount = match b.amount.as_deref() {
        Some(raw) => Some(parse_amount("amount", raw)?),
        None => None,
    };
    let d = repo::raise_dispute(
        &st,
        ctx.scope.tenant_id,
        ChargeId(id),
        &reason,
        amount,
        by_student,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&d, d.version, StatusCode::CREATED))
}

/// One dispute (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/disputes/{id}", operation_id = "finance_dispute_get", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DisputeView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dispute_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let d = repo::get_dispute(&st, ctx.scope.tenant_id, ChargeDisputeId(id)).await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

/// Uphold (optionally crediting or waiving) or reject a dispute (`finance.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/finance/disputes/{id}/decision", operation_id = "finance_dispute_decide", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the dispute"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = DisputeDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DisputeView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dispute_decide(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DisputeDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceApprove)?;
    let v = if_match!(headers);
    require_idempotency_key(&headers)?;
    let note = opt_text(b.reason.as_deref());
    let (to, resolution) = match b.decision {
        DecisionKind::Reject => {
            require_text("reason", note.as_deref())?;
            (ChargeDisputeStatus::Rejected, Resolution::None)
        }
        DecisionKind::Approve => {
            let r = match b.resolution.as_deref().unwrap_or("none") {
                "none" => Resolution::None,
                "waive" => Resolution::Waive,
                "credit" => Resolution::Credit(parse_amount(
                    "creditAmount",
                    b.credit_amount.as_deref().unwrap_or_default(),
                )?),
                _ => return Err(invalid("resolution", "invalid", "none, credit or waive").into()),
            };
            (ChargeDisputeStatus::Upheld, r)
        }
    };
    let d = repo::resolve_dispute(
        &st,
        ctx.scope.tenant_id,
        ChargeDisputeId(id),
        v,
        to,
        resolution,
        note,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

/// Withdraw an open dispute (`charge.manage`, or the student who owns the charge).
#[utoipa::path(post, path = "/api/v1/finance/disputes/{id}/withdraw", operation_id = "finance_dispute_withdraw", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the dispute")),
    request_body(content = Option<ReasonBody>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DisputeView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn dispute_withdraw(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let v = if_match!(headers);
    // SEC-P1-AUTH-005: permission before the dispute row is read.
    if !ctx.has_permission(Permission::ChargeManage) {
        ctx.require(Permission::FinanceSelfService)?;
    }
    let current = repo::get_dispute(&st, ctx.scope.tenant_id, ChargeDisputeId(id)).await?;
    if !ctx.has_permission(Permission::ChargeManage) {
        ctx.require(Permission::FinanceSelfService)?;
        readable_charge(&st, ctx, ChargeId(current.charge_id)).await?;
    }
    let d = repo::resolve_dispute(
        &st,
        ctx.scope.tenant_id,
        ChargeDisputeId(id),
        v,
        ChargeDisputeStatus::Withdrawn,
        Resolution::None,
        reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&d, d.version, StatusCode::OK))
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(fee_head_list, fee_head_create))
        .routes(routes!(fee_head_get, fee_head_update))
        .routes(routes!(fee_structure_list, fee_structure_create))
        .routes(routes!(fee_structure_get))
        .routes(routes!(fee_structure_submit))
        .routes(routes!(fee_structure_decide))
        .routes(routes!(fee_structure_publish))
        .routes(routes!(fee_structure_supersede))
        .routes(routes!(fee_structure_generate_charges))
        .routes(routes!(charge_list, charge_create))
        .routes(routes!(charge_list_of_account))
        .routes(routes!(charge_get))
        .routes(routes!(charge_self_list))
        .routes(routes!(charge_validate))
        .routes(routes!(charge_publish))
        .routes(routes!(charge_revise))
        .routes(routes!(charge_revision_list))
        .routes(routes!(charge_suspend))
        .routes(routes!(charge_resume))
        .routes(routes!(charge_cancel))
        .routes(routes!(charge_close))
        .routes(routes!(charge_archive))
        .routes(routes!(instalment_list, instalment_plan_create))
        .routes(routes!(instalment_update))
        .routes(routes!(dispute_list, dispute_create))
        .routes(routes!(dispute_get))
        .routes(routes!(dispute_decide))
        .routes(routes!(dispute_withdraw))
        .with_state(state)
}
