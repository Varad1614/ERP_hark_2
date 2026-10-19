//! Financial years and periods (L2-F09, L3-F-051…055): establish a year with its monthly
//! periods, open / lock / close / reopen / archive periods, close / archive the year. The
//! period guard (`shared::period::require_open`) reads these rows inside every posting.
//!
//! Close and reopen are governed decisions (`period.close`, step-up). A close is refused while a
//! manual journal dated into the period still awaits a decision, and it snapshots the trial
//! balance of the period end (append-only evidence, `trial_balance.generated`).

use super::domain::{can_transition_period, can_transition_year, monthly_periods};
use crate::ledger::common::{
    changed, emit, history, illegal, invalid, load_history, opt_text, require_text,
    require_version, HistoryView,
};
use crate::ledger::trial::compute;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::{Actor, FinanceState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::events::ledger::{
    FINANCIAL_PERIOD_CLOSED, FINANCIAL_PERIOD_CLOSED_SCHEMA, FINANCIAL_PERIOD_LOCKED,
    FINANCIAL_PERIOD_LOCKED_SCHEMA, FINANCIAL_PERIOD_OPENED, FINANCIAL_PERIOD_OPENED_SCHEMA,
    FINANCIAL_PERIOD_REOPENED, FINANCIAL_PERIOD_REOPENED_SCHEMA, FINANCIAL_YEAR_CLOSED,
    FINANCIAL_YEAR_CLOSED_SCHEMA, TRIAL_BALANCE_GENERATED, TRIAL_BALANCE_GENERATED_SCHEMA,
};
use sutra_contracts::finance::ids::{FinancialPeriodId, FinancialYearId};
use sutra_contracts::finance::status::{FinancialPeriodStatus, FinancialYearStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

// ------------------------------------------------------------------ rows and views

/// A financial period.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PeriodView {
    pub id: FinancialPeriodId,
    pub institution_id: Uuid,
    pub financial_year_id: FinancialYearId,
    pub period_no: i32,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub status: FinancialPeriodStatus,
    pub status_reason: Option<String>,
    /// `true` while `open | reopened` (the period guard).
    pub accepts_postings: bool,
    pub closed_at: Option<DateTime<Utc>>,
    pub closed_by: Option<String>,
    pub reopen_count: i32,
    pub entry_count: i64,
    pub history: Vec<HistoryView>,
    pub version: i64,
}

/// A financial year with its periods.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct YearView {
    pub id: FinancialYearId,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub status: FinancialYearStatus,
    pub status_reason: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
    pub periods: Vec<PeriodView>,
    pub history: Vec<HistoryView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct YearList {
    pub items: Vec<YearView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PeriodList {
    pub items: Vec<PeriodView>,
}

#[derive(Clone, Debug)]
struct YearRow {
    id: Uuid,
    tenant: TenantId,
    institution: InstitutionId,
    code: String,
    name: String,
    start: NaiveDate,
    end: NaiveDate,
    status: FinancialYearStatus,
    status_reason: Option<String>,
    closed_at: Option<DateTime<Utc>>,
    version: i64,
}

#[derive(Clone, Debug)]
struct PeriodRow {
    id: Uuid,
    tenant: TenantId,
    institution: InstitutionId,
    year: Uuid,
    no: i32,
    name: String,
    start: NaiveDate,
    end: NaiveDate,
    status: FinancialPeriodStatus,
    status_reason: Option<String>,
    closed_at: Option<DateTime<Utc>>,
    closed_by: Option<String>,
    reopen_count: i32,
    version: i64,
}

fn year_from_row(r: &PgRow) -> Result<YearRow, sqlx::Error> {
    let st: String = r.try_get("status")?;
    Ok(YearRow {
        id: r.try_get("id")?,
        tenant: TenantId(r.try_get("tenant_id")?),
        institution: InstitutionId(r.try_get("institution_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        start: r.try_get("start_date")?,
        end: r.try_get("end_date")?,
        status: FinancialYearStatus::parse(&st)
            .ok_or_else(|| sqlx::Error::Decode(format!("year status `{st}`").into()))?,
        status_reason: r.try_get("status_reason")?,
        closed_at: r.try_get("closed_at")?,
        version: r.try_get("version")?,
    })
}

fn period_from_row(r: &PgRow) -> Result<PeriodRow, sqlx::Error> {
    let st: String = r.try_get("status")?;
    Ok(PeriodRow {
        id: r.try_get("id")?,
        tenant: TenantId(r.try_get("tenant_id")?),
        institution: InstitutionId(r.try_get("institution_id")?),
        year: r.try_get("financial_year_id")?,
        no: r.try_get("period_no")?,
        name: r.try_get("name")?,
        start: r.try_get("start_date")?,
        end: r.try_get("end_date")?,
        status: FinancialPeriodStatus::parse(&st)
            .ok_or_else(|| sqlx::Error::Decode(format!("period status `{st}`").into()))?,
        status_reason: r.try_get("status_reason")?,
        closed_at: r.try_get("closed_at")?,
        closed_by: r.try_get("closed_by")?,
        reopen_count: r.try_get("reopen_count")?,
        version: r.try_get("version")?,
    })
}

async fn load_year(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<YearRow, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT * FROM finance.financial_year WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "financial year",
    })?;
    year_from_row(&r).map_err(map_db_error)
}

async fn load_period(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PeriodRow, KernelError> {
    let r = sqlx::query(&format!(
        "SELECT * FROM finance.financial_period WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "financial period",
    })?;
    period_from_row(&r).map_err(map_db_error)
}

async fn period_view(conn: &mut PgConnection, p: PeriodRow) -> Result<PeriodView, KernelError> {
    let entry_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE tenant_id = $1 AND financial_period_id = $2",
    )
    .bind(p.tenant.as_uuid())
    .bind(p.id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let history = load_history(conn, p.tenant, "financial_period", p.id).await?;
    Ok(PeriodView {
        id: FinancialPeriodId(p.id),
        institution_id: p.institution.as_uuid(),
        financial_year_id: FinancialYearId(p.year),
        period_no: p.no,
        name: p.name,
        start_date: p.start,
        end_date: p.end,
        status: p.status,
        status_reason: p.status_reason,
        accepts_postings: p.status.accepts_postings(),
        closed_at: p.closed_at,
        closed_by: p.closed_by,
        reopen_count: p.reopen_count,
        entry_count,
        history,
        version: p.version,
    })
}

async fn periods_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    year: Uuid,
) -> Result<Vec<PeriodView>, KernelError> {
    let rows = sqlx::query(
        "SELECT * FROM finance.financial_period WHERE tenant_id = $1 AND financial_year_id = $2 ORDER BY period_no",
    )
    .bind(tenant.as_uuid())
    .bind(year)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let p = period_from_row(r).map_err(map_db_error)?;
        out.push(period_view(conn, p).await?);
    }
    Ok(out)
}

async fn year_view(conn: &mut PgConnection, y: YearRow) -> Result<YearView, KernelError> {
    let periods = periods_of(conn, y.tenant, y.id).await?;
    let history = load_history(conn, y.tenant, "financial_year", y.id).await?;
    Ok(YearView {
        id: FinancialYearId(y.id),
        institution_id: y.institution.as_uuid(),
        code: y.code,
        name: y.name,
        start_date: y.start,
        end_date: y.end,
        status: y.status,
        status_reason: y.status_reason,
        closed_at: y.closed_at,
        periods,
        history,
        version: y.version,
    })
}

async fn respond_year(
    st: &FinanceState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let y = load_year(&mut conn, tenant, id, false).await?;
    let v = y.version;
    let view = year_view(&mut conn, y).await?;
    Ok(with_etag(status, &view, Version(v)))
}

async fn respond_period(st: &FinanceState, tenant: TenantId, id: Uuid) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_period(&mut conn, tenant, id, false).await?;
    let v = p.version;
    let view = period_view(&mut conn, p).await?;
    Ok(with_etag(StatusCode::OK, &view, Version(v)))
}

// ------------------------------------------------------------------ years

/// A new financial year (monthly periods are generated, all `planned`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct YearCreate {
    /// e.g. `FY2026-27`.
    pub code: String,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
}

/// Financial years of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/financial-years", operation_id = "finance_year_list", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = YearList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<YearList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(
        "SELECT * FROM finance.financial_year WHERE tenant_id = $1 AND institution_id = $2 ORDER BY start_date DESC",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let y = year_from_row(r).map_err(map_db_error)?;
        items.push(year_view(&mut conn, y).await?);
    }
    Ok(Json(YearList { items }))
}

/// Establishes a financial year and its monthly periods (`period.manage`, `Idempotency-Key`;
/// L3-F-051). Years of an institution never overlap.
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/financial-years", operation_id = "finance_year_create", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)), request_body = YearCreate, security(("session_cookie" = [])),
    responses((status = 201, body = YearView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<YearCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let name = require_text("name", Some(&body.name))?;
    let code = body.code.trim().to_uppercase();
    if code.len() < 2
        || code.len() > 32
        || !code
            .chars()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || "._-".contains(c))
    {
        return Err(invalid(
            "code",
            "invalid",
            "2-32 characters: capitals, digits, . _ -",
        )
        .into());
    }
    let periods = monthly_periods(body.start_date, body.end_date).ok_or_else(|| {
        invalid(
            "endDate",
            "invalid_range",
            "a financial year runs forward and spans at most 13 months",
        )
    })?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Serialise year creation per institution (overlap check below).
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 51))")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let overlap: Option<String> = sqlx::query_scalar(
        "SELECT code FROM finance.financial_year WHERE tenant_id = $1 AND institution_id = $2 \
         AND start_date <= $4 AND end_date >= $3 LIMIT 1",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(body.start_date)
    .bind(body.end_date)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(c) = overlap {
        return Err(KernelError::Conflict(format!("the dates overlap financial year {c}")).into());
    }
    let yid: Option<Uuid> = sqlx::query_scalar(
        "INSERT INTO finance.financial_year (tenant_id, institution_id, code, name, start_date, end_date) \
         VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (tenant_id, institution_id, code) DO NOTHING RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(body.start_date)
    .bind(body.end_date)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let yid =
        yid.ok_or_else(|| KernelError::Conflict(format!("financial year {code} already exists")))?;
    history(
        &mut tx,
        t,
        inst,
        "financial_year",
        yid,
        None,
        "draft",
        &StatusChange::default(),
        &a,
    )
    .await?;
    for (no, pname, start, end) in &periods {
        let pid: Uuid = sqlx::query_scalar(
            "INSERT INTO finance.financial_period (tenant_id, institution_id, financial_year_id, period_no, \
             name, start_date, end_date) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(yid)
        .bind(no)
        .bind(pname)
        .bind(start)
        .bind(end)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            t,
            inst,
            "financial_period",
            pid,
            None,
            "planned",
            &StatusChange::default(),
            &a,
        )
        .await?;
    }
    emit(
        &mut tx,
        &scope,
        None,
        &changed(yid, inst, None, "draft", 1, None, Some(&code), None, None),
        "finance.financial_year.create",
        "financial_year",
        json!({"code": code, "startDate": body.start_date, "endDate": body.end_date, "periods": periods.len()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond_year(&st, t, yid, StatusCode::CREATED).await
}

/// One financial year with its periods (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/financial-years/{id}", operation_id = "finance_year_get", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = YearView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond_year(&st, tenant(ctx), id, StatusCode::OK).await
}

/// Periods of one financial year (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/financial-years/{id}/periods", operation_id = "finance_period_list", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PeriodList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<PeriodList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_year(&mut conn, t, id, false).await?;
    Ok(Json(PeriodList {
        items: periods_of(&mut conn, t, id).await?,
    }))
}

/// Optional reason on a state command (required for reopen).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonBody {
    pub reason: Option<String>,
}

async fn set_year_status(
    conn: &mut PgConnection,
    y: &YearRow,
    to: FinancialYearStatus,
    reason: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !can_transition_year(y.status, to) {
        return Err(illegal("financial year", y.status.as_str(), to.as_str()));
    }
    sqlx::query(
        "UPDATE finance.financial_year SET status = $2, status_reason = $3, version = version + 1, updated_at = now(), \
         closed_at = CASE WHEN $2 = 'closed' THEN now() ELSE closed_at END, \
         closed_by = CASE WHEN $2 = 'closed' THEN $4 ELSE closed_by END WHERE id = $1",
    )
    .bind(y.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        y.tenant,
        y.institution,
        "financial_year",
        y.id,
        Some(y.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: cause,
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await
}

async fn year_command(
    st: &FinanceState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    id: Uuid,
    to: FinancialYearStatus,
    reason: Option<String>,
) -> ApiResult<Response> {
    let v = match if_match(headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let y = load_year(&mut tx, t, id, true).await?;
    require_version(v, y.version)?;
    let scope = institution_scope(ctx, y.institution);
    require_institution_writable(st, &scope, y.institution).await?;
    if to == FinancialYearStatus::Closed {
        let open: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM finance.financial_period WHERE financial_year_id = $1 \
             AND status NOT IN ('closed', 'archived') ORDER BY period_no",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if !open.is_empty() {
            return Err(KernelError::Conflict(format!(
                "close every period first; still open: {}",
                open.join(", ")
            ))
            .into());
        }
    }
    if to == FinancialYearStatus::Archived {
        sqlx::query(
            "SELECT 1 FROM finance.financial_period WHERE financial_year_id = $1 FOR UPDATE",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let rows = sqlx::query(
            "SELECT * FROM finance.financial_period WHERE financial_year_id = $1 AND status = 'closed'",
        )
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        for r in &rows {
            let p = period_from_row(r).map_err(map_db_error)?;
            set_period_status(
                &mut tx,
                &p,
                FinancialPeriodStatus::Archived,
                Some("year archived"),
                "sutraerp.finance.financial_year.archived",
                &a,
            )
            .await?;
        }
    }
    let (ev, cause) = match to {
        FinancialYearStatus::Closed => (
            Some((FINANCIAL_YEAR_CLOSED, FINANCIAL_YEAR_CLOSED_SCHEMA)),
            FINANCIAL_YEAR_CLOSED,
        ),
        FinancialYearStatus::Archived => (None, "sutraerp.finance.financial_year.archived"),
        _ => (None, "sutraerp.finance.financial_year.opened"),
    };
    set_year_status(&mut tx, &y, to, reason.as_deref(), cause, &a).await?;
    emit(
        &mut tx,
        &scope,
        ev,
        &changed(
            id,
            y.institution,
            Some(y.status.as_str()),
            to.as_str(),
            y.version + 1,
            None,
            reason.as_deref().or(Some(&y.code)),
            None,
            None,
        ),
        &format!(
            "finance.financial_year.{}",
            match to {
                FinancialYearStatus::Closed => "close",
                FinancialYearStatus::Archived => "archive",
                _ => "open",
            }
        ),
        "financial_year",
        json!({"code": y.code, "from": y.status, "to": to, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond_year(st, t, id, StatusCode::OK).await
}

/// Opens a draft year (`period.manage`, `If-Match`). Its periods are opened one by one.
#[utoipa::path(post, path = "/api/v1/finance/financial-years/{id}/open", operation_id = "finance_year_open", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = YearView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_open(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodManage)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    year_command(&st, ctx, &headers, id, FinancialYearStatus::Open, reason).await
}

/// Closes a year whose periods are all closed (`period.close` + step-up, `If-Match`; L3-F-053).
#[utoipa::path(post, path = "/api/v1/finance/financial-years/{id}/close", operation_id = "finance_year_close", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = YearView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_close(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodClose)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    year_command(&st, ctx, &headers, id, FinancialYearStatus::Closed, reason).await
}

/// Archives a closed year and its periods (`period.close` + step-up, `If-Match`; L3-F-055).
#[utoipa::path(post, path = "/api/v1/finance/financial-years/{id}/archive", operation_id = "finance_year_archive", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = YearView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_archive(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodClose)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    year_command(
        &st,
        ctx,
        &headers,
        id,
        FinancialYearStatus::Archived,
        reason,
    )
    .await
}

// ------------------------------------------------------------------ periods

async fn set_period_status(
    conn: &mut PgConnection,
    p: &PeriodRow,
    to: FinancialPeriodStatus,
    reason: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !can_transition_period(p.status, to) {
        return Err(illegal("financial period", p.status.as_str(), to.as_str()));
    }
    sqlx::query(
        "UPDATE finance.financial_period SET status = $2, status_reason = $3, version = version + 1, updated_at = now(), \
         closed_at = CASE WHEN $2 = 'closed' THEN now() ELSE closed_at END, \
         closed_by = CASE WHEN $2 = 'closed' THEN $4 ELSE closed_by END, \
         reopen_count = reopen_count + CASE WHEN $2 = 'reopened' THEN 1 ELSE 0 END WHERE id = $1",
    )
    .bind(p.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        p.tenant,
        p.institution,
        "financial_period",
        p.id,
        Some(p.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: cause,
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await
}

async fn period_command(
    st: &FinanceState,
    ctx: &AuthContext,
    headers: &HeaderMap,
    id: Uuid,
    to: FinancialPeriodStatus,
    reason: Option<String>,
) -> ApiResult<Response> {
    let v = match if_match(headers)? {
        Ok(v) => v,
        Err(r) => return Ok(r),
    };
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Exclusive lock: waits for in-flight postings (they hold FOR SHARE on this row).
    let p = load_period(&mut tx, t, id, true).await?;
    require_version(v, p.version)?;
    let scope = institution_scope(ctx, p.institution);
    require_institution_writable(st, &scope, p.institution).await?;
    let year = load_year(&mut tx, t, p.year, false).await?;
    if matches!(
        to,
        FinancialPeriodStatus::Open | FinancialPeriodStatus::Reopened
    ) && year.status != FinancialYearStatus::Open
    {
        return Err(KernelError::Conflict(format!(
            "financial year {} is {}; its periods cannot accept postings",
            year.code, year.status
        ))
        .into());
    }
    let mut snapshot = None;
    if to == FinancialPeriodStatus::Closed {
        let pending: Vec<String> = sqlx::query_scalar(
            "SELECT journal_number FROM finance.manual_journal WHERE tenant_id = $1 AND institution_id = $2 \
             AND status IN ('pending_approval', 'approved') AND posting_date BETWEEN $3 AND $4 ORDER BY journal_number",
        )
        .bind(t.as_uuid())
        .bind(p.institution.as_uuid())
        .bind(p.start)
        .bind(p.end)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if !pending.is_empty() {
            return Err(KernelError::Conflict(format!(
                "manual journals dated into {} still await a decision: {}",
                p.name,
                pending.join(", ")
            ))
            .into());
        }
        let tb = compute(&mut tx, t, p.institution, p.end, Some(p.id)).await?;
        let total: rust_decimal::Decimal = tb
            .total_debit
            .parse()
            .map_err(|_| KernelError::internal_msg("trial balance total"))?;
        let total_credit: rust_decimal::Decimal = tb
            .total_credit
            .parse()
            .map_err(|_| KernelError::internal_msg("trial balance total"))?;
        if !tb.balanced {
            return Err(KernelError::Conflict(format!(
                "the trial balance at {} does not balance ({} / {})",
                p.end, tb.total_debit, tb.total_credit
            ))
            .into());
        }
        let tb_id: Uuid = sqlx::query_scalar(
            "INSERT INTO finance.trial_balance (tenant_id, institution_id, financial_period_id, as_of, \
             total_debit, total_credit, balanced, rows, generated_by) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        )
        .bind(t.as_uuid())
        .bind(p.institution.as_uuid())
        .bind(p.id)
        .bind(p.end)
        .bind(total)
        .bind(total_credit)
        .bind(tb.balanced)
        .bind(serde_json::to_value(&tb.rows).map_err(KernelError::internal)?)
        .bind(a.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        emit(
            &mut tx,
            &scope,
            Some((TRIAL_BALANCE_GENERATED, TRIAL_BALANCE_GENERATED_SCHEMA)),
            &changed(
                tb_id,
                p.institution,
                None,
                "generated",
                1,
                Some(total),
                Some(&p.name),
                None,
                None,
            ),
            "finance.trial_balance.generate",
            "trial_balance",
            json!({"periodId": p.id, "asOf": p.end, "totalDebit": tb.total_debit, "totalCredit": tb.total_credit}),
            &a,
        )
        .await?;
        snapshot = Some(tb_id);
    }
    let (ev, verb) = match to {
        FinancialPeriodStatus::Open => (
            Some((FINANCIAL_PERIOD_OPENED, FINANCIAL_PERIOD_OPENED_SCHEMA)),
            "open",
        ),
        FinancialPeriodStatus::Locked => (
            Some((FINANCIAL_PERIOD_LOCKED, FINANCIAL_PERIOD_LOCKED_SCHEMA)),
            "lock",
        ),
        FinancialPeriodStatus::Closed => (
            Some((FINANCIAL_PERIOD_CLOSED, FINANCIAL_PERIOD_CLOSED_SCHEMA)),
            "close",
        ),
        FinancialPeriodStatus::Reopened => (
            Some((FINANCIAL_PERIOD_REOPENED, FINANCIAL_PERIOD_REOPENED_SCHEMA)),
            "reopen",
        ),
        _ => (None, "archive"),
    };
    let cause = ev.map_or("sutraerp.finance.financial_period.archived", |e| e.0);
    set_period_status(&mut tx, &p, to, reason.as_deref(), cause, &a).await?;
    emit(
        &mut tx,
        &scope,
        ev,
        &changed(
            id,
            p.institution,
            Some(p.status.as_str()),
            to.as_str(),
            p.version + 1,
            None,
            reason.as_deref().or(Some(&p.name)),
            None,
            None,
        ),
        &format!("finance.financial_period.{verb}"),
        "financial_period",
        json!({"name": p.name, "from": p.status, "to": to, "reason": reason, "trialBalanceId": snapshot}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond_period(st, t, id).await
}

/// One financial period with its history (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/financial-periods/{id}", operation_id = "finance_period_get", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PeriodView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FinanceRead)?;
    respond_period(&st, tenant(ctx), id).await
}

/// Opens a planned (or unlocks a locked) period (`period.manage`, `If-Match`; L3-F-051).
#[utoipa::path(post, path = "/api/v1/finance/financial-periods/{id}/open", operation_id = "finance_period_open", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = PeriodView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_open(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodManage)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    period_command(&st, ctx, &headers, id, FinancialPeriodStatus::Open, reason).await
}

/// Locks a period: no new postings, close pending (`period.manage`, `If-Match`; L3-F-052).
#[utoipa::path(post, path = "/api/v1/finance/financial-periods/{id}/lock", operation_id = "finance_period_lock", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = PeriodView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_lock(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodManage)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    period_command(
        &st,
        ctx,
        &headers,
        id,
        FinancialPeriodStatus::Locked,
        reason,
    )
    .await
}

/// Controlled accounting close (`period.close` + step-up, `If-Match`; L3-F-031/052): refused
/// while manual journals of the period await a decision; snapshots the trial balance.
#[utoipa::path(post, path = "/api/v1/finance/financial-periods/{id}/close", operation_id = "finance_period_close", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = PeriodView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_close(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodClose)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    period_command(
        &st,
        ctx,
        &headers,
        id,
        FinancialPeriodStatus::Closed,
        reason,
    )
    .await
}

/// Reopens a closed period for corrections (`period.close` + step-up, `If-Match`, reason
/// required; L3-F-054). The year must still be open.
#[utoipa::path(post, path = "/api/v1/finance/financial-periods/{id}/reopen", operation_id = "finance_period_reopen", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = PeriodView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_reopen(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodClose)?;
    let reason = require_text("reason", body.as_ref().and_then(|b| b.0.reason.as_deref()))?;
    period_command(
        &st,
        ctx,
        &headers,
        id,
        FinancialPeriodStatus::Reopened,
        Some(reason),
    )
    .await
}

/// Archives a closed period (`period.close` + step-up, `If-Match`; L3-F-055).
#[utoipa::path(post, path = "/api/v1/finance/financial-periods/{id}/archive", operation_id = "finance_period_archive", tag = "finance-periods",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), request_body = ReasonBody, security(("session_cookie" = [])),
    responses((status = 200, body = PeriodView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn period_archive(
    State(st): State<FinanceState>,
    auth: Authenticated,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeriodClose)?;
    let reason = body.and_then(|b| opt_text(b.0.reason.as_deref()));
    period_command(
        &st,
        ctx,
        &headers,
        id,
        FinancialPeriodStatus::Archived,
        reason,
    )
    .await
}
