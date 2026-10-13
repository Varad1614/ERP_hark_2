//! HTTP API of payslips (tag `people-payroll`, operation ids `people_payslip_*`).

use super::repo::{self, PayslipRow};
use crate::payroll::common::{PayrollBulkSummary, PayrollReasonBody};
use crate::payroll::http::{cmd, expected, reason_of};
use crate::shared::decimal::fmt2;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::me::my_staff;
use crate::shared::PeopleState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sutra_contracts::people::status::{PayComponentKind, PayslipStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One payslip line (lines of the same code are summed).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayslipLine {
    #[schema(example = "BASIC")]
    pub code: String,
    pub name: String,
    #[schema(example = "30000.00")]
    pub amount: String,
}

/// A payslip (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayslipView {
    pub id: Uuid,
    #[schema(example = "PS-000001")]
    pub number: String,
    pub payroll_run_id: Uuid,
    pub result_id: Uuid,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub earnings: Vec<PayslipLine>,
    pub deductions: Vec<PayslipLine>,
    pub employer_contributions: Vec<PayslipLine>,
    #[schema(example = "45000.00")]
    pub gross_earnings: String,
    pub total_deductions: String,
    pub net_pay: String,
    #[schema(example = "1.50")]
    pub lop_days: String,
    pub payable_days: String,
    pub revision: i32,
    pub status: PayslipStatus,
    pub status_reason: Option<String>,
    pub issued_at: DateTime<Utc>,
    pub reissued_at: Option<DateTime<Utc>>,
    pub withdrawn_at: Option<DateTime<Utc>>,
    pub version: i64,
}

fn grouped(p: &PayslipRow, kind: PayComponentKind) -> Vec<PayslipLine> {
    let mut out: Vec<(String, String, Decimal)> = Vec::new();
    if let Some(r) = &p.result {
        for l in r.lines.iter().filter(|l| l.kind == kind) {
            match out.iter_mut().find(|x| x.0 == l.code) {
                Some(x) => x.2 += l.amount,
                None => out.push((l.code.clone(), l.name.clone(), l.amount)),
            }
        }
    }
    out.into_iter()
        .map(|(code, name, amount)| PayslipLine {
            code,
            name,
            amount: fmt2(amount),
        })
        .collect()
}

impl PayslipView {
    fn of(p: &PayslipRow) -> Result<Self, KernelError> {
        let r = p
            .result
            .as_ref()
            .ok_or_else(|| KernelError::internal_msg("payslip without its result"))?;
        Ok(Self {
            id: p.id,
            number: p.number.clone(),
            payroll_run_id: p.run_id,
            result_id: p.result_id,
            staff_id: p.staff_id,
            staff_number: r.staff_number.clone(),
            display_name: r.display_name.clone(),
            period_start: r.period_start,
            period_end: r.period_end,
            earnings: grouped(p, PayComponentKind::Earning),
            deductions: grouped(p, PayComponentKind::Deduction),
            employer_contributions: grouped(p, PayComponentKind::EmployerContribution),
            gross_earnings: fmt2(r.gross_earnings),
            total_deductions: fmt2(r.total_deductions),
            net_pay: fmt2(r.net_pay),
            lop_days: fmt2(r.lop_days),
            payable_days: fmt2(r.payable_days),
            revision: r.revision,
            status: p.status,
            status_reason: p.status_reason.clone(),
            issued_at: p.issued_at,
            reissued_at: p.reissued_at,
            withdrawn_at: p.withdrawn_at,
            version: p.version,
        })
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PayslipList {
    pub items: Vec<PayslipView>,
}

fn list(rows: &[PayslipRow]) -> Result<PayslipList, KernelError> {
    Ok(PayslipList {
        items: rows
            .iter()
            .map(PayslipView::of)
            .collect::<Result<Vec<_>, _>>()?,
    })
}

fn one(p: &PayslipRow) -> ApiResult<Response> {
    Ok(with_etag(
        StatusCode::OK,
        &PayslipView::of(p)?,
        Version(p.version),
    ))
}

/// Payslips of a run (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payroll-runs/{id}/payslips", operation_id = "people_payslip_list", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayslipList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payslip_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PayslipList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let rows = repo::list_for_run(&st, ctx.scope.tenant_id, id).await?;
    Ok(Json(list(&rows)?))
}

/// Issue payslips for every finalised result of a run without one (`people.payroll.approve`;
/// bulk, `Idempotency-Key`; `{processed, skipped, failed[]}`).
#[utoipa::path(post, path = "/api/v1/people/payroll-runs/{id}/payslips/issue", operation_id = "people_payslip_issue", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayrollBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payslip_issue(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<PayrollBulkSummary>> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    require_idempotency_key(&headers)?;
    let a = actor(ctx);
    Ok(Json(repo::issue_payslips(&cmd(&st, ctx, &a), id).await?))
}

/// One payslip (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/payslips/{id}", operation_id = "people_payslip_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayslipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payslip_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    one(&repo::get_payslip(&st, ctx.scope.tenant_id, id).await?)
}

/// Reissue a payslip on the current result revision (`people.payroll.approve`).
#[utoipa::path(post, path = "/api/v1/people/payslips/{id}/reissue", operation_id = "people_payslip_reissue", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the payslip")),
    request_body = PayrollReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayslipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payslip_reissue(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    b: Option<Json<PayrollReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    one(&repo::reissue_payslip(&cmd(&st, ctx, &a), id, v, reason_of(&b)).await?)
}

/// Withdraw a payslip (`people.payroll.approve`; reason required).
#[utoipa::path(post, path = "/api/v1/people/payslips/{id}/withdraw", operation_id = "people_payslip_withdraw", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the payslip")),
    request_body = PayrollReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PayslipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payslip_withdraw(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    b: Option<Json<PayrollReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollApprove)?;
    let v = expected!(headers);
    let a = actor(ctx);
    one(&repo::withdraw_payslip(&cmd(&st, ctx, &a), id, v, reason_of(&b)).await?)
}

/// The caller's own payslips (`people.self`; 404 when the login has no staff record).
#[utoipa::path(get, path = "/api/v1/people/me/payslips", operation_id = "people_payslip_self_list", tag = "people-payroll",
    security(("session_cookie" = [])),
    responses((status = 200, body = PayslipList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_payslips(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<PayslipList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let rows = repo::list_for_staff(&st, ctx.scope.tenant_id, me.id).await?;
    Ok(Json(list(&rows)?))
}

/// One of the caller's own payslips (`people.self`; anyone else's → 404).
#[utoipa::path(get, path = "/api/v1/people/me/payslips/{id}", operation_id = "people_payslip_self_get", tag = "people-payroll",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PayslipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_payslip(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let p = repo::get_payslip(&st, ctx.scope.tenant_id, id).await?;
    if p.staff_id != me.id.as_uuid() || p.status == PayslipStatus::Withdrawn {
        return Err(KernelError::NotFound { entity: "payslip" }.into());
    }
    one(&p)
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(payslip_list))
        .routes(routes!(payslip_issue))
        .routes(routes!(payslip_get))
        .routes(routes!(payslip_reissue))
        .routes(routes!(payslip_withdraw))
        .routes(routes!(my_payslips))
        .routes(routes!(my_payslip))
        .with_state(state)
}
