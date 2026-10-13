//! HTTP API of finance party accounts (tag `finance-accounts`; contract §5.2).

use crate::shared::guard::require_institution;
use crate::shared::http::with_etag;
use crate::shared::me::my_accounts;
use crate::shared::money::fmt;
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::Json;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sutra_contracts::finance::ids::StudentAccountId;
use sutra_contracts::finance::ports::{AccountBalance, ChargeSummary, StudentAccountSummary};
use sutra_contracts::finance::status::{ChargeStatus, PartyKind, StudentAccountStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Balance of a party account (amounts are decimal strings, INR).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BalanceView {
    #[schema(example = "125000.00")]
    pub total_charged: String,
    pub total_adjusted: String,
    pub total_settled: String,
    /// Owed now (`published | active` charges).
    pub outstanding: String,
    /// Under dispute (not demanded until resolved).
    pub disputed_outstanding: String,
    /// Part of `outstanding` past its due date (institution business date).
    pub overdue: String,
    pub open_charges: i64,
    pub charges_raised: i64,
    pub currency: String,
}

impl From<&AccountBalance> for BalanceView {
    fn from(b: &AccountBalance) -> Self {
        Self {
            total_charged: fmt(b.total_charged),
            total_adjusted: fmt(b.total_adjusted),
            total_settled: fmt(b.total_settled),
            outstanding: fmt(b.outstanding),
            disputed_outstanding: fmt(b.disputed_outstanding),
            overdue: fmt(b.overdue),
            open_charges: b.open_charges,
            charges_raised: b.charges_raised,
            currency: b.currency.clone(),
        }
    }
}

/// A finance party account with its balance.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StudentAccountView {
    pub id: StudentAccountId,
    pub institution_id: Uuid,
    pub party_kind: PartyKind,
    pub student_id: Option<Uuid>,
    pub admission_id: Option<Uuid>,
    pub student_number: Option<String>,
    pub display_name: String,
    pub currency: String,
    pub status: StudentAccountStatus,
    pub balance: BalanceView,
}

/// One charge as account views show it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChargeView {
    pub id: Uuid,
    pub charge_number: String,
    pub description: String,
    pub currency: String,
    pub total_amount: String,
    pub adjustment_amount: String,
    pub settled_amount: String,
    pub outstanding_amount: String,
    pub due_date: Option<chrono::NaiveDate>,
    pub academic_term_id: Option<Uuid>,
    pub status: ChargeStatus,
    pub version: i64,
}

impl From<&ChargeSummary> for ChargeView {
    fn from(c: &ChargeSummary) -> Self {
        Self {
            id: c.id.as_uuid(),
            charge_number: c.charge_number.clone(),
            description: c.description.clone(),
            currency: c.currency.clone(),
            total_amount: fmt(c.total_amount),
            adjustment_amount: fmt(c.adjustment_amount),
            settled_amount: fmt(c.settled_amount),
            outstanding_amount: fmt(c.outstanding_amount),
            due_date: c.due_date,
            academic_term_id: c.academic_term_id,
            status: c.status,
            version: c.version,
        }
    }
}

/// An account with its charges.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StudentAccountDetail {
    pub account: StudentAccountView,
    pub charges: Vec<ChargeView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StudentAccountList {
    pub items: Vec<StudentAccountView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StudentAccountDetailList {
    pub items: Vec<StudentAccountDetail>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct AccountListQuery {
    /// Case-insensitive match on name or student number.
    pub q: Option<String>,
    /// Only accounts with an outstanding amount.
    pub with_dues: Option<bool>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
}

async fn view(
    st: &FinanceState,
    scope: &ExecutionScope,
    a: StudentAccountSummary,
) -> Result<StudentAccountView, KernelError> {
    let today = business_today(st, scope, a.institution_id).await?;
    let b = st
        .receivables
        .account_balance(scope, a.id, today)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "student account",
        })?;
    Ok(StudentAccountView {
        id: a.id,
        institution_id: a.institution_id.as_uuid(),
        party_kind: a.party_kind,
        student_id: a.student_id,
        admission_id: a.admission_id,
        student_number: a.student_number,
        display_name: a.display_name,
        currency: a.currency,
        status: a.status,
        balance: BalanceView::from(&b),
    })
}

async fn detail(
    st: &FinanceState,
    scope: &ExecutionScope,
    a: StudentAccountSummary,
    student_view: bool,
) -> Result<StudentAccountDetail, KernelError> {
    let charges = st
        .charges
        .charges_of_account(scope, a.id)
        .await?
        .iter()
        .filter(|c| !student_view || c.status.is_visible_to_student())
        .map(ChargeView::from)
        .collect();
    Ok(StudentAccountDetail {
        account: view(st, scope, a).await?,
        charges,
    })
}

/// Party accounts of an institution with their balances (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/student-accounts", operation_id = "finance_account_list", tag = "finance-accounts",
    params(("id" = String, Path, format = Uuid), AccountListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StudentAccountList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn account_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AccountListQuery>,
) -> ApiResult<Json<StudentAccountList>> {
    let ctx: &AuthContext = auth.require(Permission::FinanceRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let pattern =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| format!("%{}%", s.replace('%', "\\%").replace('_', "\\_")));
    let ids: Vec<Uuid> = sqlx::query(
        "SELECT a.id FROM finance.student_account a WHERE a.tenant_id = $1 AND a.institution_id = $2 \
         AND ($3::text IS NULL OR a.display_name ILIKE $3 OR a.student_number ILIKE $3) \
         AND (NOT $4 OR EXISTS (SELECT 1 FROM finance.charge c WHERE c.student_account_id = a.id \
              AND c.status IN ('published', 'active', 'disputed') AND c.outstanding_amount > 0)) \
         ORDER BY a.display_name, a.id LIMIT $5",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(pattern)
    .bind(q.with_dues.unwrap_or(false))
    .bind(limit)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|r| r.try_get("id"))
    .collect::<Result<_, _>>()
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(ids.len());
    for a in ids {
        if let Some(acc) = st
            .charges
            .student_account(&scope, StudentAccountId(a))
            .await?
        {
            items.push(view(&st, &scope, acc).await?);
        }
    }
    Ok(Json(StudentAccountList { items }))
}

/// One party account with its balance and charges (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/student-accounts/{id}", operation_id = "finance_account_get", tag = "finance-accounts",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StudentAccountDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn account_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let scope = tenant_scope(ctx);
    let a = st
        .charges
        .student_account(&scope, StudentAccountId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "student account",
        })?;
    let scope = institution_scope(ctx, a.institution_id);
    let version: i64 = sqlx::query_scalar(
        "SELECT version FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    let d = detail(&st, &scope, a, false).await?;
    Ok(with_etag(StatusCode::OK, &d, Version(version)))
}

/// The caller's own fee accounts with balances and visible charges (`finance.self`; empty when
/// no charge was ever raised).
#[utoipa::path(get, path = "/api/v1/finance/me/accounts", operation_id = "finance_account_self_list", tag = "finance-accounts",
    security(("session_cookie" = [])),
    responses((status = 200, body = StudentAccountDetailList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_account_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
) -> ApiResult<Json<StudentAccountDetailList>> {
    let ctx = &auth.0;
    let mut items = Vec::new();
    for a in my_accounts(&st, ctx).await? {
        let scope = institution_scope(ctx, a.institution_id);
        items.push(detail(&st, &scope, a, true).await?);
    }
    Ok(Json(StudentAccountDetailList { items }))
}

pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(account_list))
        .routes(routes!(account_get))
        .routes(routes!(my_account_list))
        .with_state(state)
}
