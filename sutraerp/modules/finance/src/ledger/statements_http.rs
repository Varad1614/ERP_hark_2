//! **ERP_EXTENSIONS 1C** — financial statements over the journal (tag `finance-ledger`,
//! `finance.read`): income & expenditure, balance sheet, cash flow and general ledger.
//!
//! The statements are read straight from the append-only journal (`journal_line` ⋈
//! `journal_entry`); every account's placement is its `statement_group` (derived from the seeded
//! role / account type unless set on the account). The fiscal year is the institution's
//! financial year containing the date (India: April – March when none is defined); prior years'
//! surplus is shown inside the accumulated fund. The rules are `super::statements`.

use super::statements::{
    balance_sheet, cash_flow, fiscal_year_start, general_ledger, income_expenditure, Account,
    Books, Columns, Compare, EntryRef, Posting, SheetColumns, Statement, StatementGroup,
};
use crate::shared::guard::require_institution;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::FinanceState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::{PgConnection, Row};
use std::collections::HashMap;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::IntoParams;
use uuid::Uuid;

/// The longest period a statement covers (performance budget).
const MAX_DAYS: i64 = 3_700;

fn invalid(field: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, "invalid", message)])
}

/// The month the institution's financial year starts in (its financial year containing `on`;
/// April when none is defined).
async fn fiscal_start_month(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    on: NaiveDate,
) -> Result<u32, KernelError> {
    let start: Option<NaiveDate> = sqlx::query_scalar(
        "SELECT start_date FROM finance.financial_year WHERE tenant_id = $1 AND institution_id = $2 \
         AND start_date <= $3 ORDER BY (end_date >= $3) DESC, start_date DESC LIMIT 1",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(on)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(start.map_or(4, |d| d.month()))
}

/// Loads the chart and the postings up to `to`. Postings before `detail_from` are summed per
/// account and day; from `detail_from` on they keep their journal entry (general ledger).
async fn load_books(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    to: NaiveDate,
    detail_from: Option<NaiveDate>,
    fiscal_on: NaiveDate,
) -> Result<Books, KernelError> {
    let rows = sqlx::query(
        "SELECT a.id, a.code, a.name, a.account_type, a.parent_id, a.system_key, a.statement_group, \
         EXISTS (SELECT 1 FROM finance.bank_account b WHERE b.ledger_account_id = a.id) AS is_bank \
         FROM finance.ledger_account a WHERE a.tenant_id = $1 AND a.institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut accounts = Vec::with_capacity(rows.len());
    let mut fund = None;
    for r in &rows {
        let id: Uuid = r.try_get("id").map_err(map_db_error)?;
        let ty: String = r.try_get("account_type").map_err(map_db_error)?;
        let key: Option<String> = r.try_get("system_key").map_err(map_db_error)?;
        let group: Option<String> = r.try_get("statement_group").map_err(map_db_error)?;
        let is_bank: bool = r.try_get("is_bank").map_err(map_db_error)?;
        if key.as_deref() == Some("accumulated_fund") {
            fund = Some(id);
        }
        accounts.push(Account {
            id,
            code: r.try_get("code").map_err(map_db_error)?,
            name: r.try_get("name").map_err(map_db_error)?,
            group: group
                .as_deref()
                .and_then(StatementGroup::parse)
                .unwrap_or_else(|| StatementGroup::default_for(&ty, key.as_deref(), is_bank)),
            parent: r.try_get("parent_id").map_err(map_db_error)?,
        });
    }
    let mut postings = Vec::new();
    let mut entries = vec![EntryRef::default()];
    let summary_to = detail_from.map_or(to, |d| (d - Duration::days(1)).min(to));
    let daily = sqlx::query(
        "SELECT e.posting_date, l.ledger_account_id, sum(l.debit - l.credit) AS amount \
         FROM finance.journal_line l JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
         WHERE e.tenant_id = $1 AND e.institution_id = $2 AND e.posting_date <= $3 \
         GROUP BY e.posting_date, l.ledger_account_id ORDER BY e.posting_date",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(summary_to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for r in &daily {
        let amount: Decimal = r.try_get("amount").map_err(map_db_error)?;
        if !amount.is_zero() {
            postings.push(Posting {
                date: r.try_get("posting_date").map_err(map_db_error)?,
                account: r.try_get("ledger_account_id").map_err(map_db_error)?,
                amount,
                entry: 0,
                memo: String::new(),
            });
        }
    }
    if let Some(from) = detail_from.filter(|f| *f <= to) {
        let lines = sqlx::query(
            "SELECT e.id, e.entry_number, e.description, e.source_type, e.posting_date, l.ledger_account_id, \
             l.debit - l.credit AS amount, coalesce(l.memo, '') AS memo \
             FROM finance.journal_line l JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
             WHERE e.tenant_id = $1 AND e.institution_id = $2 AND e.posting_date BETWEEN $3 AND $4 \
             ORDER BY e.posting_date, e.created_at, e.entry_number, l.line_no",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let mut index: HashMap<Uuid, usize> = HashMap::new();
        for r in &lines {
            let eid: Uuid = r.try_get("id").map_err(map_db_error)?;
            let e = match index.get(&eid) {
                Some(i) => *i,
                None => {
                    entries.push(EntryRef {
                        id: eid,
                        number: r.try_get("entry_number").map_err(map_db_error)?,
                        description: r.try_get("description").map_err(map_db_error)?,
                        source_type: r.try_get("source_type").map_err(map_db_error)?,
                    });
                    index.insert(eid, entries.len() - 1);
                    entries.len() - 1
                }
            };
            postings.push(Posting {
                date: r.try_get("posting_date").map_err(map_db_error)?,
                account: r.try_get("ledger_account_id").map_err(map_db_error)?,
                amount: r.try_get("amount").map_err(map_db_error)?,
                entry: e,
                memo: r.try_get("memo").map_err(map_db_error)?,
            });
        }
    }
    let month = fiscal_start_month(conn, t, inst, fiscal_on).await?;
    Ok(Books::new(accounts, postings, entries, month, fund))
}

/// Resolves `[from, to]`: defaults are the fiscal year to date.
async fn period(
    st: &FinanceState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: TenantId,
    inst: InstitutionId,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<(NaiveDate, NaiveDate), KernelError> {
    let to = match to {
        Some(d) => d,
        None => business_today(st, scope, inst).await?,
    };
    let from = match from {
        Some(d) => d,
        None => fiscal_year_start(to, fiscal_start_month(conn, t, inst, to).await?),
    };
    if from > to {
        return Err(invalid("from", "must be on or before `to`"));
    }
    if (to - from).num_days() > MAX_DAYS {
        return Err(invalid("from", "a statement covers at most ten years"));
    }
    Ok((from, to))
}

/// Period of a statement.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct PeriodQuery {
    /// First day (default: start of the fiscal year of `to`).
    pub from: Option<NaiveDate>,
    /// Last day (default: today).
    pub to: Option<NaiveDate>,
    /// `total` (default), `month`, `quarter` or `year` (fiscal years).
    #[param(value_type = Option<String>)]
    pub columns: Option<Columns>,
    /// `previous_period` or `previous_year`: two columns with ₹ and % change.
    #[param(value_type = Option<String>)]
    pub compare: Option<Compare>,
}

/// Income & expenditure account (profit and loss) for a period (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/financial-statements/income-expenditure", operation_id = "finance_ledger_income_expenditure", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), PeriodQuery), security(("session_cookie" = [])),
    responses((status = 200, body = Statement), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn income_expenditure_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<PeriodQuery>,
) -> ApiResult<Json<Statement>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (from, to) = period(&st, &mut conn, &scope, t, inst, q.from, q.to).await?;
    let books = load_books(&mut conn, t, inst, to, None, to).await?;
    Ok(Json(income_expenditure(
        &books,
        from,
        to,
        q.columns.unwrap_or(Columns::Total),
        q.compare,
    )))
}

/// As-of date of a balance sheet.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct SheetQuery {
    /// Default: today.
    pub as_of: Option<NaiveDate>,
    /// `total` (default), `month` or `quarter` (six period ends).
    #[param(value_type = Option<String>)]
    pub columns: Option<SheetColumns>,
    /// Add the same date a year earlier.
    pub compare_previous_year: Option<bool>,
}

/// Balance sheet as of a date (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/financial-statements/balance-sheet", operation_id = "finance_ledger_balance_sheet", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), SheetQuery), security(("session_cookie" = [])),
    responses((status = 200, body = Statement), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn balance_sheet_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<SheetQuery>,
) -> ApiResult<Json<Statement>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let as_of = match q.as_of {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let books = load_books(&mut conn, t, inst, as_of, None, as_of).await?;
    Ok(Json(balance_sheet(
        &books,
        as_of,
        q.columns.unwrap_or(SheetColumns::Total),
        q.compare_previous_year.unwrap_or(false),
    )))
}

/// Period of a cash-flow statement or general ledger.
#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct RangeQuery {
    pub from: Option<NaiveDate>,
    pub to: Option<NaiveDate>,
    /// General ledger only: one account (default: every account with activity).
    pub ledger_account_id: Option<Uuid>,
}

/// Cash flow statement (indirect method) for a period (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/financial-statements/cash-flow", operation_id = "finance_ledger_cash_flow", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), RangeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = Statement), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cash_flow_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Json<Statement>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (from, to) = period(&st, &mut conn, &scope, t, inst, q.from, q.to).await?;
    let books = load_books(&mut conn, t, inst, to, None, to).await?;
    Ok(Json(cash_flow(&books, from, to)))
}

/// General ledger for a period: opening balance, each journal entry with a running balance, and
/// the period total per account (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/financial-statements/general-ledger", operation_id = "finance_ledger_general_ledger", tag = "finance-ledger",
    params(("id" = String, Path, format = Uuid), RangeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = Statement), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn general_ledger_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Json<Statement>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (from, to) = period(&st, &mut conn, &scope, t, inst, q.from, q.to).await?;
    let books = load_books(&mut conn, t, inst, to, Some(from), from).await?;
    let only = q.ledger_account_id.map(|a| vec![a]);
    Ok(Json(general_ledger(&books, from, to, only.as_deref())))
}
