//! HTTP API of payroll disbursement (Part 1, DEF-E-007; tag `people-payroll`): disbursement
//! batches of a payroll run (create idempotently, list, detail, decision, bank / challan file,
//! mark paid) and staff salary bank accounts. See [`super::disbursement`].

use super::common::{PayrollDecision, PayrollDecisionBody};
use super::disbursement::{
    self, BankAccountInput, BankAccountRow, DisbursementLine, DisbursementRow,
    PayrollDisbursementKind, PayrollDisbursementStatus, BANK_SUBMISSION_BLOCKED,
};
use super::http::{cmd, expected};
use crate::shared::decimal::fmt2;
use crate::shared::guard::require_institution;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::PeopleState;
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ================================================================== views

/// A payroll disbursement batch (net pay transfer or statutory remittance).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollDisbursementView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub payroll_run_id: Uuid,
    #[schema(example = "PR-000001")]
    pub payroll_run_number: String,
    #[schema(example = "PD-000001")]
    pub number: String,
    pub kind: PayrollDisbursementKind,
    pub status: PayrollDisbursementStatus,
    pub status_reason: Option<String>,
    pub line_count: i32,
    #[schema(example = "43800.00")]
    pub total_amount: String,
    /// `BLOCKED_EXTERNAL`: no bank API is integrated; upload the file to the bank portal.
    #[schema(example = "BLOCKED_EXTERNAL")]
    pub bank_submission: String,
    pub file_generated_at: Option<DateTime<Utc>>,
    pub payment_reference: Option<String>,
    pub paid_on: Option<NaiveDate>,
    pub paid_by: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl From<&DisbursementRow> for PayrollDisbursementView {
    fn from(d: &DisbursementRow) -> Self {
        Self {
            id: d.id,
            institution_id: d.institution_id.as_uuid(),
            payroll_run_id: d.run_id,
            payroll_run_number: d.run_number.clone(),
            number: d.number.clone(),
            kind: d.kind.into(),
            status: d.status,
            status_reason: d.status_reason.clone(),
            line_count: d.line_count,
            total_amount: fmt2(d.total_amount),
            bank_submission: BANK_SUBMISSION_BLOCKED.to_owned(),
            file_generated_at: d.file_generated_at,
            payment_reference: d.payment_reference.clone(),
            paid_on: d.paid_on,
            paid_by: d.paid_by.clone(),
            paid_at: d.paid_at,
            journal_entry_id: d.journal_entry_id,
            journal_entry_number: d.journal_entry_number.clone(),
            process_instance_id: d.process_instance_id.clone(),
            approved_at: d.approved_at,
            created_by: d.created_by.clone(),
            created_at: d.created_at,
            version: d.version,
        }
    }
}

/// One payee or due of a batch (the account number is masked).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollDisbursementLineView {
    pub line_no: i32,
    pub staff_id: Option<Uuid>,
    pub staff_number: Option<String>,
    pub display_name: Option<String>,
    pub account_holder_name: Option<String>,
    #[schema(example = "XXXXXXXX7890")]
    pub account_number_masked: Option<String>,
    pub ifsc: Option<String>,
    pub component_code: Option<String>,
    pub component_name: Option<String>,
    #[schema(example = "43800.00")]
    pub amount: String,
}

impl From<&DisbursementLine> for PayrollDisbursementLineView {
    fn from(l: &DisbursementLine) -> Self {
        Self {
            line_no: l.line_no,
            staff_id: l.staff_id,
            staff_number: l.staff_number.clone(),
            display_name: l.display_name.clone(),
            account_holder_name: l.account_holder_name.clone(),
            account_number_masked: l.account_number.as_deref().map(disbursement::mask_account),
            ifsc: l.ifsc.clone(),
            component_code: l.component_code.clone(),
            component_name: l.component_name.clone(),
            amount: fmt2(l.amount),
        }
    }
}

/// A batch with its lines.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollDisbursementDetail {
    pub batch: PayrollDisbursementView,
    pub lines: Vec<PayrollDisbursementLineView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayrollDisbursementList {
    pub items: Vec<PayrollDisbursementView>,
}

/// Create body (`POST /payroll-runs/{id}/disbursements`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PayrollDisbursementCreate {
    pub kind: PayrollDisbursementKind,
}

/// Mark-paid body: the bank transaction reference (UTR) or the challan reference.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollDisbursementMarkPaid {
    #[schema(example = "UTR2026113000123")]
    pub reference: String,
    /// Payment value date (default: today in the institution's timezone; never in the future).
    pub paid_on: Option<NaiveDate>,
}

/// A staff member's salary bank account (the account number is masked).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollBankAccountView {
    pub staff_id: Uuid,
    pub institution_id: Uuid,
    pub account_holder_name: String,
    #[schema(example = "XXXXXXXX7890")]
    pub account_number_masked: String,
    #[schema(example = "HDFC0001234")]
    pub ifsc: String,
    pub bank_name: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

impl From<&BankAccountRow> for PayrollBankAccountView {
    fn from(b: &BankAccountRow) -> Self {
        Self {
            staff_id: b.staff_id,
            institution_id: b.institution_id.as_uuid(),
            account_holder_name: b.account_holder_name.clone(),
            account_number_masked: disbursement::mask_account(&b.account_number),
            ifsc: b.ifsc.clone(),
            bank_name: b.bank_name.clone(),
            updated_at: b.updated_at,
            version: b.version,
        }
    }
}

/// Bank account body (`PUT /staff/{id}/bank-account`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollBankAccountPut {
    pub account_holder_name: String,
    #[schema(example = "001234567890")]
    pub account_number: String,
    #[schema(example = "HDFC0001234")]
    pub ifsc: String,
    pub bank_name: Option<String>,
}

fn res(status: StatusCode, d: &DisbursementRow) -> Response {
    with_etag(
        status,
        &PayrollDisbursementView::from(d),
        Version(d.version),
    )
}

// ================================================================== handlers

/// Create the disbursement batch of a posted payroll run (`people.payroll.manage`). Idempotent:
/// 201 with the new batch, or 200 with the live batch of that run and kind. Requests approval
/// (`people.payroll_disbursement_approval`).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/disbursements", operation_id = "people_payroll_disbursement_create", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = PayrollDisbursementCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PayrollDisbursementView, headers(("ETag" = String))), (status = 200, body = PayrollDisbursementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn disbursement_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollDisbursementCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    let out = disbursement::create_disbursement(&cmd(&st, ctx, &a), id, b.kind.into()).await?;
    let status = if out.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok(res(status, &out.row))
}

/// Disbursement batches of a payroll run (`people.payroll.manage`).
#[utoipa::path(get, path = "/api/v1/people/payroll-runs/{id}/disbursements", operation_id = "people_payroll_disbursement_list_for_run", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollDisbursementList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn disbursement_list_for_run(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PayrollDisbursementList>> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let items = disbursement::list_for_run(&st, ctx.scope.tenant_id, id)
        .await?
        .iter()
        .map(PayrollDisbursementView::from)
        .collect();
    Ok(Json(PayrollDisbursementList { items }))
}

/// Disbursement batches of an institution, newest first (`people.payroll.manage`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/payroll-disbursements", operation_id = "people_payroll_disbursement_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollDisbursementList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn disbursement_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PayrollDisbursementList>> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let items = disbursement::list_for_institution(&st, ctx.scope.tenant_id, inst)
        .await?
        .iter()
        .map(PayrollDisbursementView::from)
        .collect();
    Ok(Json(PayrollDisbursementList { items }))
}

/// One disbursement batch with its lines (`people.payroll.manage`).
#[utoipa::path(get, path = "/api/v1/people/payroll-disbursements/{id}", operation_id = "people_payroll_disbursement_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollDisbursementDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn disbursement_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let t = ctx.scope.tenant_id;
    let d = disbursement::get_disbursement(&st, t, id).await?;
    let lines = disbursement::lines_of(&st, t, id).await?;
    Ok(with_etag(
        StatusCode::OK,
        &PayrollDisbursementDetail {
            batch: PayrollDisbursementView::from(&d),
            lines: lines
                .iter()
                .map(PayrollDisbursementLineView::from)
                .collect(),
        },
        Version(d.version),
    ))
}

/// Decide a pending disbursement batch (`people.payroll.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/people/payroll-disbursements/{id}/decision", operation_id = "people_payroll_disbursement_decide", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the batch")),
    request_body = PayrollDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollDisbursementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn disbursement_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let d = disbursement::decide_disbursement(
        &cmd(&st, ctx, &a),
        id,
        v,
        b.decision == PayrollDecision::Approve,
        b.reason.as_deref(),
    )
    .await?;
    Ok(res(StatusCode::OK, &d))
}

/// Download the batch's file (`people.payroll.manage`; approved or paid batches): the generic
/// NEFT bulk transfer CSV for net pay, the remittance advice CSV for statutory dues. Bank
/// submission is BLOCKED_EXTERNAL (no bank API): upload the file to the bank portal.
#[utoipa::path(get, path = "/api/v1/people/payroll-disbursements/{id}/file", operation_id = "people_payroll_disbursement_file", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn disbursement_file(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let a = actor(ctx);
    let f = disbursement::generate_file(&cmd(&st, ctx, &a), id).await?;
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{}\"", f.file_name))
        .map_err(|e| KernelError::internal_msg(format!("file name header: {e}")))?;
    let mut out = (StatusCode::OK, f.content).into_response();
    out.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    out.headers_mut()
        .insert(header::CONTENT_DISPOSITION, disposition);
    out.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(out)
}

/// Mark an approved batch paid with the bank / challan reference (`people.payroll.approve`,
/// step-up): posts Dr salaries payable (or statutory dues payable) / Cr bank in Finance. A paid
/// batch cannot be paid again (409).
#[utoipa::path(post, path = "/api/v1/people/payroll-disbursements/{id}/mark-paid", operation_id = "people_payroll_disbursement_mark_paid", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the batch")),
    request_body = PayrollDisbursementMarkPaid, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollDisbursementView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn disbursement_mark_paid(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PayrollDisbursementMarkPaid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    let d = disbursement::mark_paid(&cmd(&st, ctx, &a), id, v, &b.reference, b.paid_on).await?;
    Ok(res(StatusCode::OK, &d))
}

/// A staff member's salary bank account (`people.payroll.manage`; 404 when none is recorded).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/bank-account", operation_id = "people_pay_bank_account_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayrollBankAccountView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PayrollBankAccountView>> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let t = ctx.scope.tenant_id;
    let inst = disbursement::staff_institution(&st, t, id).await?;
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let b = disbursement::get_bank_account(&st, t, id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "bank account",
        })?;
    Ok(Json(PayrollBankAccountView::from(&b)))
}

/// Record or replace a staff member's salary bank account (`people.payroll.manage`). Batches
/// already created keep the details they snapshotted.
#[utoipa::path(put, path = "/api/v1/people/staff/{id}/bank-account", operation_id = "people_pay_bank_account_put", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)),
    request_body = PayrollBankAccountPut, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollBankAccountView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn bank_account_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<PayrollBankAccountPut>,
) -> ApiResult<Json<PayrollBankAccountView>> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let inst = disbursement::staff_institution(&st, ctx.scope.tenant_id, id).await?;
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let a = actor(ctx);
    let row = disbursement::put_bank_account(
        &cmd(&st, ctx, &a),
        id,
        &BankAccountInput {
            account_holder_name: b.account_holder_name,
            account_number: b.account_number,
            ifsc: b.ifsc,
            bank_name: b.bank_name,
        },
    )
    .await?;
    Ok(Json(PayrollBankAccountView::from(&row)))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(disbursement_create, disbursement_list_for_run))
        .routes(routes!(disbursement_list))
        .routes(routes!(disbursement_get))
        .routes(routes!(disbursement_decide))
        .routes(routes!(disbursement_file))
        .routes(routes!(disbursement_mark_paid))
        .routes(routes!(bank_account_get, bank_account_put))
        .with_state(state)
}
