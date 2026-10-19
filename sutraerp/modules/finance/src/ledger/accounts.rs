//! Chart of accounts management on top of the seeded default chart (L2-F05; contract §3.3:
//! user accounts are created / deactivated here, a `system_key` account with postings is never
//! renamed or deactivated).

use super::common::{invalid, opt_text, require_text, require_version};
use super::statements::StatementGroup;
use crate::shared::coa::ensure_default_chart;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::money::fmt;
use crate::shared::publish::audit;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::FinanceState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::ids::LedgerAccountId;
use sutra_contracts::finance::status::{LedgerAccountStatus, LedgerAccountType, NormalBalance};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// A ledger account with its current balance (normal side).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LedgerAccountView {
    pub id: LedgerAccountId,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub account_type: LedgerAccountType,
    pub normal_balance: NormalBalance,
    pub parent_id: Option<Uuid>,
    /// Seeded role (`student_receivables`, `bank_main`, …); `null` for user accounts.
    pub system_key: Option<String>,
    pub is_postable: bool,
    /// Where the account sits on the financial statements (ERP_EXTENSIONS 1C); derived from the
    /// role / type unless set.
    pub statement_group: StatementGroup,
    pub currency: String,
    pub status: LedgerAccountStatus,
    #[schema(value_type = String, example = "125000.00")]
    pub debit_total: String,
    #[schema(value_type = String, example = "100000.00")]
    pub credit_total: String,
    #[schema(value_type = String, example = "25000.00")]
    pub balance: String,
    pub posting_count: i64,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LedgerAccountList {
    pub items: Vec<LedgerAccountView>,
}

const SELECT: &str = "SELECT a.id, a.institution_id, a.code, a.name, a.account_type, a.parent_id, \
    a.system_key, a.is_postable, a.currency, a.status, a.version, a.statement_group, \
    EXISTS (SELECT 1 FROM finance.bank_account b WHERE b.ledger_account_id = a.id) AS is_bank, \
    COALESCE((SELECT sum(l.debit) FROM finance.journal_line l WHERE l.ledger_account_id = a.id), 0) AS dr, \
    COALESCE((SELECT sum(l.credit) FROM finance.journal_line l WHERE l.ledger_account_id = a.id), 0) AS cr, \
    (SELECT count(*) FROM finance.journal_line l WHERE l.ledger_account_id = a.id) AS n \
    FROM finance.ledger_account a";

fn from_row(r: &PgRow) -> Result<LedgerAccountView, KernelError> {
    let ty: String = r.try_get("account_type").map_err(map_db_error)?;
    let ty = LedgerAccountType::parse(&ty)
        .ok_or_else(|| KernelError::internal_msg(format!("account type `{ty}`")))?;
    let st: String = r.try_get("status").map_err(map_db_error)?;
    let st = LedgerAccountStatus::parse(&st)
        .ok_or_else(|| KernelError::internal_msg(format!("account status `{st}`")))?;
    let dr: Decimal = r.try_get("dr").map_err(map_db_error)?;
    let cr: Decimal = r.try_get("cr").map_err(map_db_error)?;
    Ok(LedgerAccountView {
        id: LedgerAccountId(r.try_get("id").map_err(map_db_error)?),
        institution_id: r.try_get("institution_id").map_err(map_db_error)?,
        code: r.try_get("code").map_err(map_db_error)?,
        name: r.try_get("name").map_err(map_db_error)?,
        account_type: ty,
        normal_balance: ty.normal_balance(),
        parent_id: r.try_get("parent_id").map_err(map_db_error)?,
        system_key: r.try_get("system_key").map_err(map_db_error)?,
        is_postable: r.try_get("is_postable").map_err(map_db_error)?,
        statement_group: {
            let g: Option<String> = r.try_get("statement_group").map_err(map_db_error)?;
            let key: Option<String> = r.try_get("system_key").map_err(map_db_error)?;
            g.as_deref()
                .and_then(StatementGroup::parse)
                .unwrap_or_else(|| {
                    StatementGroup::default_for(
                        ty.as_str(),
                        key.as_deref(),
                        r.try_get("is_bank").unwrap_or(false),
                    )
                })
        },
        currency: r.try_get("currency").map_err(map_db_error)?,
        status: st,
        debit_total: fmt(dr),
        credit_total: fmt(cr),
        balance: fmt(super::trial::normal_balance(ty, dr, cr)),
        posting_count: r.try_get("n").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
    })
}

/// One ledger account of the tenant (404 otherwise).
pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<LedgerAccountView, KernelError> {
    let r = sqlx::query(&format!("{SELECT} WHERE a.tenant_id = $1 AND a.id = $2"))
        .bind(tenant.as_uuid())
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "ledger account",
        })?;
    from_row(&r)
}

/// Chart of accounts of an institution (`finance.read`; the default chart is seeded on first
/// read).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/ledger-accounts", operation_id = "finance_ledger_account_list", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LedgerAccountList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ledger_account_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LedgerAccountList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let t = tenant(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    ensure_default_chart(&mut tx, t, inst).await?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE a.tenant_id = $1 AND a.institution_id = $2 ORDER BY a.code"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    let items = rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?;
    Ok(Json(LedgerAccountList { items }))
}

/// A new user ledger account.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LedgerAccountCreate {
    /// `^[0-9A-Z][0-9A-Z._-]{0,31}$`, unique per institution.
    pub code: String,
    pub name: String,
    pub account_type: LedgerAccountType,
    pub parent_id: Option<Uuid>,
    /// Defaults to `true` (a header / grouping account is not postable).
    pub is_postable: Option<bool>,
    /// Statement placement (must fit the account type); derived when absent.
    pub statement_group: Option<StatementGroup>,
}

/// Sets an account's statement group (it must fit the account type).
async fn set_group(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    account_type: &str,
    g: StatementGroup,
) -> Result<(), KernelError> {
    if g.account_type() != account_type {
        return Err(invalid(
            "statementGroup",
            "type_mismatch",
            format!("{} is not a group for a {account_type} account", g.as_str()),
        ));
    }
    sqlx::query(
        "UPDATE finance.ledger_account SET statement_group = $3 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(g.as_str())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

fn valid_code(code: &str) -> bool {
    let mut chars = code.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_digit() || c.is_ascii_uppercase())
        && code.len() <= 32
        && chars.all(|c| c.is_ascii_digit() || c.is_ascii_uppercase() || "._-".contains(c))
}

/// Inserts a user ledger account in the caller's transaction (used by treasury for a new bank
/// account's own ledger account too).
#[allow(clippy::too_many_arguments)]
pub async fn insert_account(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    code: &str,
    name: &str,
    ty: LedgerAccountType,
    parent: Option<Uuid>,
    postable: bool,
) -> Result<Uuid, KernelError> {
    if !valid_code(code) {
        return Err(invalid(
            "code",
            "invalid",
            "1-32 characters: digits, capitals, . _ -",
        ));
    }
    if let Some(p) = parent {
        let ok: Option<String> = sqlx::query_scalar(
            "SELECT account_type FROM finance.ledger_account WHERE tenant_id = $1 AND institution_id = $2 AND id = $3",
        )
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(p)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        match ok {
            None => return Err(invalid("parentId", "not_found", "unknown parent account")),
            Some(t) if t != ty.as_str() => {
                return Err(invalid(
                    "parentId",
                    "type_mismatch",
                    "the parent account has another account type",
                ))
            }
            _ => {}
        }
    }
    let id: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, account_type, \
         normal_balance, parent_id, is_postable) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (tenant_id, institution_id, code) DO NOTHING RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(code)
    .bind(name)
    .bind(ty.as_str())
    .bind(ty.normal_balance().as_str())
    .bind(parent)
    .bind(postable)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    id.ok_or_else(|| KernelError::Conflict(format!("ledger account code {code} already exists")))
}

/// Adds a user account to the chart (`ledger.manage`, `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/ledger-accounts", operation_id = "finance_ledger_account_create", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = LedgerAccountCreate, security(("session_cookie" = [])),
    responses((status = 201, body = LedgerAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ledger_account_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<LedgerAccountCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LedgerManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let name = require_text("name", Some(&body.name))?;
    let code = body.code.trim().to_owned();
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    ensure_default_chart(&mut tx, t, inst).await?;
    let new_id = insert_account(
        &mut tx,
        t,
        inst,
        &code,
        &name,
        body.account_type,
        body.parent_id,
        body.is_postable.unwrap_or(true),
    )
    .await?;
    if let Some(g) = body.statement_group {
        set_group(&mut tx, t, new_id, body.account_type.as_str(), g).await?;
    }
    audit(
        &mut tx,
        &scope,
        "finance.ledger_account.create",
        "ledger_account",
        new_id.to_string(),
        json!({"code": code, "name": name, "accountType": body.account_type}),
        &a,
    )
    .await?;
    let v = load(&mut tx, t, new_id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &v, Version(v.version)))
}

/// One ledger account with its balance (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/ledger-accounts/{id}", operation_id = "finance_ledger_account_get", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LedgerAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ledger_account_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load(&mut conn, tenant(ctx), id).await?;
    Ok(with_etag(StatusCode::OK, &v, Version(v.version)))
}

/// Rename / (de)activate / change postability of an account.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LedgerAccountUpdate {
    pub name: Option<String>,
    pub status: Option<LedgerAccountStatus>,
    pub is_postable: Option<bool>,
    /// Statement placement (must fit the account type).
    pub statement_group: Option<StatementGroup>,
}

/// Updates an account (`ledger.manage`, `If-Match`). A system account with postings keeps its
/// name and stays active; an account with a non-zero balance cannot be deactivated.
#[utoipa::path(patch, path = "/api/v1/finance/ledger-accounts/{id}", operation_id = "finance_ledger_account_update", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = LedgerAccountUpdate, security(("session_cookie" = [])),
    responses((status = 200, body = LedgerAccountView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ledger_account_update(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<LedgerAccountUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LedgerManage)?;
    let v = match if_match(&headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT 1 FROM finance.ledger_account WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
        .bind(t.as_uuid())
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let cur = load(&mut tx, t, id).await?;
    require_version(v, cur.version)?;
    let inst = InstitutionId(cur.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let name = match body.name.as_deref() {
        Some(n) => Some(require_text("name", Some(n))?),
        None => None,
    };
    let renamed = name.as_deref().is_some_and(|n| n != cur.name);
    let deactivating = body.status == Some(LedgerAccountStatus::Inactive)
        && cur.status == LedgerAccountStatus::Active;
    let unpostable = body.is_postable == Some(false) && cur.is_postable;
    if cur.system_key.is_some() && cur.posting_count > 0 && (renamed || deactivating || unpostable)
    {
        return Err(KernelError::Conflict(format!(
            "system account {} has postings; it cannot be renamed or deactivated",
            cur.code
        ))
        .into());
    }
    if (deactivating || unpostable) && cur.balance != "0.00" {
        return Err(KernelError::Conflict(format!(
            "ledger account {} has a balance of {}; clear it with a journal first",
            cur.code, cur.balance
        ))
        .into());
    }
    sqlx::query(
        "UPDATE finance.ledger_account SET name = COALESCE($3, name), status = COALESCE($4, status), \
         is_postable = COALESCE($5, is_postable), version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(name.as_deref())
    .bind(body.status.map(|s| s.as_str()))
    .bind(body.is_postable)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(g) = body.statement_group {
        set_group(&mut tx, t, id, cur.account_type.as_str(), g).await?;
    }
    audit(
        &mut tx,
        &scope,
        "finance.ledger_account.update",
        "ledger_account",
        id.to_string(),
        json!({"name": opt_text(name.as_deref()), "status": body.status, "isPostable": body.is_postable,
               "statementGroup": body.statement_group.map(StatementGroup::as_str)}),
        &actor(ctx),
    )
    .await?;
    let out = load(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

#[cfg(test)]
mod tests {
    #[test]
    fn account_codes() {
        assert!(super::valid_code("1210"));
        assert!(super::valid_code("BANK-HDFC.01"));
        assert!(!super::valid_code("bank"));
        assert!(!super::valid_code("-12"));
        assert!(!super::valid_code(""));
    }
}
