//! Balances and the ledger view, periodic accrual (L3-E-046), year-end carry forward + expiry
//! (L3-E-050), governed balance adjustments (L3-E-049, `LEAVE_ADJUSTMENT_APPROVAL`) and encashment
//! eligibility decisions (L3-E-051, `ENCASHMENT_DECISION`). Every balance change is a ledger entry
//! through `shared::leave::post_entry` (append-only, idempotent on
//! `(source_type, source_id, posting_key)`); balances are projections.

use super::common::{
    audit, clean, emit_status, ensure_version, expected, history, illegal, invalid, required,
    subject_of, EventParties, LeaveDecisionBody, Transition,
};
use super::domain::{
    adjustment_can_transition, encashment_eligibility, monthly_accrual, year_end_split,
    AccrualFrequency,
};
use super::policy::{load_type, require_active_policy, types_of, LeaveTypeRow};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::{fmt2, parse_decimal};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::leave::{balance, post_entry, NewLeaveEntry};
use crate::shared::me::my_staff;
use crate::shared::rows::{current_engagement, lock_staff};
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::leave::{
    LEAVE_ACCRUED, LEAVE_ACCRUED_SCHEMA, LEAVE_BALANCE_ADJUSTED, LEAVE_BALANCE_ADJUSTED_SCHEMA,
    LEAVE_CARRIED_FORWARD, LEAVE_CARRIED_FORWARD_SCHEMA, LEAVE_ENCASHMENT_DECIDED,
    LEAVE_ENCASHMENT_DECIDED_SCHEMA,
};
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::LeaveBalance;
use sutra_contracts::people::status::{
    EncashmentStatus, LeaveAdjustmentStatus, LeaveEntryKind, StaffCategory,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ balances + ledger

/// Leave balance of one leave type and year (contract §5.3; days as strings).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveBalanceView {
    pub leave_type_id: Uuid,
    #[schema(example = "CL")]
    pub leave_type_code: String,
    pub leave_year: i32,
    #[schema(example = "12.00")]
    pub entitled: String,
    #[schema(example = "2.00")]
    pub reserved: String,
    #[schema(example = "3.50")]
    pub consumed: String,
    #[schema(example = "6.50")]
    pub available: String,
}

impl From<&LeaveBalance> for LeaveBalanceView {
    fn from(b: &LeaveBalance) -> Self {
        Self {
            leave_type_id: b.leave_type_id,
            leave_type_code: b.leave_type_code.clone(),
            leave_year: b.leave_year,
            entitled: fmt2(b.entitled),
            reserved: fmt2(b.reserved),
            consumed: fmt2(b.consumed),
            available: fmt2(b.available),
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveBalanceList {
    pub items: Vec<LeaveBalanceView>,
}

/// One ledger entry (read-only view of the append-only ledger).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveLedgerEntryView {
    pub id: Uuid,
    pub leave_type_id: Uuid,
    pub leave_type_code: String,
    pub is_paid: bool,
    pub kind: LeaveEntryKind,
    #[schema(example = "-2.50")]
    pub days: String,
    pub effective_on: NaiveDate,
    pub leave_year: i32,
    pub source_type: String,
    pub source_id: Uuid,
    pub posting_key: String,
    pub reverses_entry_id: Option<Uuid>,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveLedgerList {
    pub items: Vec<LeaveLedgerEntryView>,
}

/// `?leaveYear=` (default: the current business year).
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct LeaveYearQuery {
    pub leave_year: Option<i32>,
}

async fn staff_in_tenant(
    st: &PeopleState,
    scope: &ExecutionScope,
    id: Uuid,
) -> Result<sutra_contracts::people::ports::StaffSummary, KernelError> {
    st.directory
        .staff(scope, StaffMemberId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })
}

async fn year_or_today(
    st: &PeopleState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    y: Option<i32>,
) -> Result<i32, KernelError> {
    match y {
        Some(y) if (2000..=2200).contains(&y) => Ok(y),
        Some(_) => Err(invalid("leaveYear", "out_of_range", "2000 … 2200")),
        None => Ok(business_today(st, scope, institution).await?.year()),
    }
}

/// Leave balances of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/leave-balances", operation_id = "people_leave_balance_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), LeaveYearQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveBalanceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn balance_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LeaveYearQuery>,
) -> ApiResult<Json<LeaveBalanceList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let s = staff_in_tenant(&st, &tenant_scope(ctx), id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    let year = year_or_today(&st, &scope, s.institution_id, q.leave_year).await?;
    let b = st.leave.balances(&scope, s.id, year).await?;
    Ok(Json(LeaveBalanceList {
        items: b.iter().map(LeaveBalanceView::from).collect(),
    }))
}

/// My leave balances (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/leave-balances", operation_id = "people_leave_self_balances", tag = "people-leave",
    params(LeaveYearQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveBalanceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_balance_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Query(q): Query<LeaveYearQuery>,
) -> ApiResult<Json<LeaveBalanceList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let scope = institution_scope(ctx, me.institution_id);
    let year = year_or_today(&st, &scope, me.institution_id, q.leave_year).await?;
    let b = st.leave.balances(&scope, me.id, year).await?;
    Ok(Json(LeaveBalanceList {
        items: b.iter().map(LeaveBalanceView::from).collect(),
    }))
}

/// Ledger entries of a staff member in a leave year (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/leave-ledger", operation_id = "people_leave_ledger_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), LeaveYearQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveLedgerList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ledger_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LeaveYearQuery>,
) -> ApiResult<Json<LeaveLedgerList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let s = staff_in_tenant(&st, &tenant_scope(ctx), id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    let year = year_or_today(&st, &scope, s.institution_id, q.leave_year).await?;
    let rows = sqlx::query(
        "SELECT id, leave_type_id, leave_type_code, is_paid, kind, days, effective_on, leave_year, \
         source_type, source_id, posting_key, reverses_entry_id, reason, created_at \
         FROM people.leave_ledger_entry WHERE tenant_id = $1 AND staff_id = $2 AND leave_year = $3 \
         ORDER BY effective_on, created_at",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(year)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| -> Result<LeaveLedgerEntryView, sqlx::Error> {
            Ok(LeaveLedgerEntryView {
                id: r.try_get("id")?,
                leave_type_id: r.try_get("leave_type_id")?,
                leave_type_code: r.try_get("leave_type_code")?,
                is_paid: r.try_get("is_paid")?,
                kind: crate::shared::rows::decode(r.try_get("kind")?, LeaveEntryKind::parse)?,
                days: fmt2(r.try_get("days")?),
                effective_on: r.try_get("effective_on")?,
                leave_year: r.try_get("leave_year")?,
                source_type: r.try_get("source_type")?,
                source_id: r.try_get("source_id")?,
                posting_key: r.try_get("posting_key")?,
                reverses_entry_id: r.try_get("reverses_entry_id")?,
                reason: r.try_get("reason")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveLedgerList { items }))
}

// ------------------------------------------------------------------ eligible population

/// A staff member in an accrual / year-end population.
struct Member {
    staff_id: StaffMemberId,
    engagement_id: StaffEngagementId,
    category: StaffCategory,
    start_date: NaiveDate,
}

/// Staff with a payable engagement overlapping `[from, to]` (live engagements started by `to`,
/// or closed with a last working day inside the period).
async fn population(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Vec<Member>, KernelError> {
    let rows = sqlx::query(
        "SELECT e.staff_id, e.id, e.staff_category, e.start_date FROM people.staff_engagement e \
         JOIN people.staff_member s ON s.id = e.staff_id \
         WHERE e.tenant_id = $1 AND e.institution_id = $2 AND s.status NOT IN ('merged', 'archived') \
         AND e.start_date <= $4 AND ( \
           (e.status IN ('active', 'suspended', 'deputed', 'long_leave') \
             AND (e.last_working_date IS NULL OR e.last_working_date >= $3)) \
           OR (e.status = 'closed' AND e.last_working_date BETWEEN $3 AND $4)) \
         ORDER BY e.engagement_number",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| -> Result<Member, sqlx::Error> {
            Ok(Member {
                staff_id: StaffMemberId(r.try_get("staff_id")?),
                engagement_id: StaffEngagementId(r.try_get("id")?),
                category: crate::shared::rows::decode(
                    r.try_get("staff_category")?,
                    StaffCategory::parse,
                )?,
                start_date: r.try_get("start_date")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

// ------------------------------------------------------------------ accrual runs (E-046)

/// An accrual run (idempotent per institution and period).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveAccrualRunView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub policy_id: Uuid,
    /// `YYYY` (opening entitlements) or `YYYY-MM` (monthly accrual).
    pub period: String,
    pub leave_year: i32,
    pub status: String,
    pub staff_count: i32,
    /// Entries posted by this execution.
    pub processed: i64,
    /// Entries already posted by an earlier execution (idempotency).
    pub skipped: i64,
    pub runs: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveAccrualRunList {
    pub items: Vec<LeaveAccrualRunView>,
}

/// Run an accrual for a period.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveAccrualCreate {
    pub leave_year: i32,
    /// 1 … 12: monthly accrual of `monthly` types; absent: opening entitlement of `yearly` types.
    pub month: Option<u32>,
}

fn accrual_run_from_row(
    r: &PgRow,
    processed: i64,
    skipped: i64,
) -> Result<LeaveAccrualRunView, sqlx::Error> {
    Ok(LeaveAccrualRunView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        policy_id: r.try_get("policy_id")?,
        period: r.try_get("period")?,
        leave_year: r.try_get("leave_year")?,
        status: r.try_get("status")?,
        staff_count: r.try_get("staff_count")?,
        processed,
        skipped,
        runs: r.try_get("runs")?,
        created_at: r.try_get("created_at")?,
    })
}

const ACCRUAL_RUN_COLUMNS: &str =
    "id, institution_id, policy_id, period, leave_year, status, staff_count, entries_posted, \
     entries_skipped, runs, created_at";

/// Run the periodic accrual (`people.leave.manage`; Idempotency-Key): monthly types accrue
/// `annual / 12` for `YYYY-MM`, yearly types get their opening entitlement for `YYYY`. Re-running
/// a period posts only what is missing (`accrual:<staff>:<period>:<code>` keys).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/leave-accruals", operation_id = "people_leave_accrual_run", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveAccrualCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveAccrualRunView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn accrual_run(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveAccrualCreate>,
) -> ApiResult<Json<LeaveAccrualRunView>> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if !(2000..=2200).contains(&b.leave_year) {
        return Err(invalid("leaveYear", "out_of_range", "2000 … 2200").into());
    }
    let (from, to, period, frequency) = match b.month {
        Some(m) => {
            let from = NaiveDate::from_ymd_opt(b.leave_year, m, 1)
                .ok_or_else(|| invalid("month", "out_of_range", "1 … 12"))?;
            let next = if m == 12 {
                NaiveDate::from_ymd_opt(b.leave_year + 1, 1, 1)
            } else {
                NaiveDate::from_ymd_opt(b.leave_year, m + 1, 1)
            }
            .and_then(|d| d.pred_opt())
            .ok_or_else(|| invalid("month", "out_of_range", "1 … 12"))?;
            (
                from,
                next,
                format!("{}-{m:02}", b.leave_year),
                AccrualFrequency::Monthly,
            )
        }
        None => (
            NaiveDate::from_ymd_opt(b.leave_year, 1, 1)
                .ok_or_else(|| invalid("leaveYear", "out_of_range", "2000 … 2200"))?,
            NaiveDate::from_ymd_opt(b.leave_year, 12, 31)
                .ok_or_else(|| invalid("leaveYear", "out_of_range", "2000 … 2200"))?,
            b.leave_year.to_string(),
            AccrualFrequency::Yearly,
        ),
    };
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let policy = require_active_policy(&mut tx, t, institution).await?;
    let types: Vec<LeaveTypeRow> = types_of(&mut tx, t, policy.id)
        .await?
        .into_iter()
        .filter(|x| x.accrual_frequency == frequency && x.annual_entitlement > Decimal::ZERO)
        .collect();
    let run_id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.leave_accrual_run (tenant_id, institution_id, policy_id, period, \
         leave_year, created_by) VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (tenant_id, institution_id, period) DO UPDATE SET runs = \
         people.leave_accrual_run.runs + 1, updated_at = now() RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(policy.id)
    .bind(&period)
    .bind(b.leave_year)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let members = population(&mut tx, t, institution, from, to).await?;
    let (mut processed, mut skipped) = (0i64, 0i64);
    for m in &members {
        for lt in types.iter().filter(|x| x.applies_to(m.category)) {
            let (kind, days, key) = match frequency {
                AccrualFrequency::Monthly => (
                    LeaveEntryKind::Accrual,
                    monthly_accrual(lt.annual_entitlement),
                    format!("accrual:{}:{period}:{}", m.staff_id, lt.code),
                ),
                _ => (
                    LeaveEntryKind::Opening,
                    lt.annual_entitlement,
                    format!("opening:{}:{period}:{}", m.staff_id, lt.code),
                ),
            };
            if days <= Decimal::ZERO {
                continue;
            }
            let receipt = post_entry(
                &mut tx,
                &NewLeaveEntry {
                    tenant: t,
                    institution,
                    staff_id: m.staff_id,
                    engagement_id: Some(m.engagement_id),
                    leave_type_id: lt.id,
                    leave_type_code: &lt.code,
                    is_paid: lt.is_paid,
                    kind,
                    days,
                    effective_on: from.max(m.start_date).min(to),
                    leave_year: b.leave_year,
                    source_type: "leave_accrual_run",
                    source_id: run_id,
                    posting_key: &key,
                    reason: Some(if kind == LeaveEntryKind::Accrual {
                        "monthly accrual"
                    } else {
                        "opening entitlement"
                    }),
                },
                &a,
            )
            .await?;
            if receipt.already_posted {
                skipped += 1;
            } else {
                processed += 1;
            }
        }
    }
    let row = sqlx::query(&format!(
        "UPDATE people.leave_accrual_run SET staff_count = $2, entries_posted = entries_posted + $3, \
         entries_skipped = entries_skipped + $4, version = version + 1 WHERE id = $1 \
         RETURNING {ACCRUAL_RUN_COLUMNS}"
    ))
    .bind(run_id)
    .bind(i32::try_from(members.len()).unwrap_or(i32::MAX))
    .bind(i32::try_from(processed).unwrap_or(i32::MAX))
    .bind(i32::try_from(skipped).unwrap_or(i32::MAX))
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution,
        aggregate_type: "leave_accrual_run",
        aggregate_id: run_id,
        from: None,
        to: "completed",
        reason: Some(&period),
        process_instance_id: None,
        cause: LEAVE_ACCRUED,
    };
    history(&mut tx, &tr, &a).await?;
    emit_status(
        &mut tx,
        &scope,
        LEAVE_ACCRUED,
        LEAVE_ACCRUED_SCHEMA,
        &tr,
        1,
        &EventParties::default(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_accrual_run.run",
        "leave_accrual_run",
        run_id,
        serde_json::json!({"period": period, "posted": processed, "skipped": skipped,
            "staff": members.len()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(
        accrual_run_from_row(&row, processed, skipped).map_err(map_db_error)?,
    ))
}

/// Accrual runs of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/leave-accruals", operation_id = "people_leave_accrual_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveAccrualRunList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn accrual_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaveAccrualRunList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let rows = sqlx::query(&format!(
        "SELECT {ACCRUAL_RUN_COLUMNS} FROM people.leave_accrual_run WHERE tenant_id = $1 \
         AND institution_id = $2 ORDER BY period DESC LIMIT 200"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| {
            let p: i32 = r.try_get("entries_posted")?;
            let s: i32 = r.try_get("entries_skipped")?;
            accrual_run_from_row(r, i64::from(p), i64::from(s))
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveAccrualRunList { items }))
}

// ------------------------------------------------------------------ year end (E-050)

/// A year-end run (carry forward up to each type's cap, expire the rest).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveYearEndRunView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub policy_id: Uuid,
    /// The leave year closed.
    pub leave_year: i32,
    pub status: String,
    pub staff_count: i32,
    #[schema(example = "15.00")]
    pub carried_days: String,
    #[schema(example = "4.50")]
    pub expired_days: String,
    pub runs: i32,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveYearEndRunList {
    pub items: Vec<LeaveYearEndRunView>,
}

/// Close a leave year.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveYearEndCreate {
    pub leave_year: i32,
}

const YEAR_END_COLUMNS: &str = "id, institution_id, policy_id, leave_year, status, staff_count, \
    carried_days, expired_days, runs, created_at";

fn year_end_from_row(r: &PgRow) -> Result<LeaveYearEndRunView, sqlx::Error> {
    Ok(LeaveYearEndRunView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        policy_id: r.try_get("policy_id")?,
        leave_year: r.try_get("leave_year")?,
        status: r.try_get("status")?,
        staff_count: r.try_get("staff_count")?,
        carried_days: fmt2(r.try_get("carried_days")?),
        expired_days: fmt2(r.try_get("expired_days")?),
        runs: r.try_get("runs")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Year-end carry forward (`people.leave.manage`; Idempotency-Key): for every staff member and
/// leave type with an unused balance in `leaveYear`, carry forward up to the active policy's cap
/// into the next year (`carry_forward`), move the carried days out of the closed year
/// (`adjustment`, `carry_out`) and expire the rest (`expiry`). Re-running posts only what is
/// missing.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/leave-year-end", operation_id = "people_leave_year_end_run", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveYearEndCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveYearEndRunView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_end_run(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveYearEndCreate>,
) -> ApiResult<Json<LeaveYearEndRunView>> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let today = business_today(&st, &scope, institution).await?;
    if !(2000..=2199).contains(&b.leave_year) || b.leave_year > today.year() {
        return Err(invalid(
            "leaveYear",
            "out_of_range",
            "a past or the current leave year",
        )
        .into());
    }
    let year_end = NaiveDate::from_ymd_opt(b.leave_year, 12, 31)
        .ok_or_else(|| invalid("leaveYear", "out_of_range", "2000 … 2199"))?;
    let next_start = NaiveDate::from_ymd_opt(b.leave_year + 1, 1, 1)
        .ok_or_else(|| invalid("leaveYear", "out_of_range", "2000 … 2199"))?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let policy = require_active_policy(&mut tx, t, institution).await?;
    let types = types_of(&mut tx, t, policy.id).await?;
    let run_id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.leave_year_end_run (tenant_id, institution_id, policy_id, leave_year, \
         created_by) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, institution_id, leave_year) \
         DO UPDATE SET runs = people.leave_year_end_run.runs + 1, updated_at = now() RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(policy.id)
    .bind(b.leave_year)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    // Every (staff, leave type) with entries in the closed year.
    let pairs: Vec<(Uuid, Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT staff_id, leave_type_id, (array_agg(engagement_id ORDER BY created_at DESC))[1] \
         FROM people.leave_ledger_entry WHERE tenant_id = $1 AND institution_id = $2 \
         AND leave_year = $3 GROUP BY staff_id, leave_type_id ORDER BY staff_id, leave_type_id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.leave_year)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (mut carried, mut expired) = (Decimal::ZERO, Decimal::ZERO);
    let mut staff_seen = std::collections::BTreeSet::new();
    for (staff, type_id, engagement) in pairs {
        let bal = balance(&mut tx, t, StaffMemberId(staff), type_id, b.leave_year).await?;
        // The type's carry-forward rule from the active policy (same id, else same code).
        let rule = types
            .iter()
            .find(|x| x.id == type_id)
            .or_else(|| types.iter().find(|x| x.code == bal.leave_type_code));
        let Some(rule) = rule.filter(|r| r.requires_balance) else {
            continue;
        };
        let (carry, expire) = year_end_split(bal.available, rule.carry_forward_cap);
        if carry.is_zero() && expire.is_zero() {
            continue;
        }
        staff_seen.insert(staff);
        let base =
            |kind: LeaveEntryKind,
             days: Decimal,
             on: NaiveDate,
             year: i32,
             key: &str,
             reason: &'static str,
             lt_id: Uuid| { (kind, days, on, year, key.to_owned(), reason, lt_id) };
        let mut postings = Vec::new();
        if expire > Decimal::ZERO {
            postings.push(base(
                LeaveEntryKind::Expiry,
                -expire,
                year_end,
                b.leave_year,
                &format!("expire:{staff}:{}", bal.leave_type_code),
                "unused leave expired at year end",
                type_id,
            ));
        }
        if carry > Decimal::ZERO {
            postings.push(base(
                LeaveEntryKind::Adjustment,
                -carry,
                year_end,
                b.leave_year,
                &format!("carry_out:{staff}:{}", bal.leave_type_code),
                "carried forward to the next leave year",
                type_id,
            ));
            postings.push(base(
                LeaveEntryKind::CarryForward,
                carry,
                next_start,
                b.leave_year + 1,
                &format!("carry:{staff}:{}", bal.leave_type_code),
                "carried forward from the previous leave year",
                rule.id,
            ));
        }
        for (kind, days, on, year, key, reason, lt_id) in postings {
            let r = post_entry(
                &mut tx,
                &NewLeaveEntry {
                    tenant: t,
                    institution,
                    staff_id: StaffMemberId(staff),
                    engagement_id: engagement.map(StaffEngagementId),
                    leave_type_id: lt_id,
                    leave_type_code: &bal.leave_type_code,
                    is_paid: rule.is_paid,
                    kind,
                    days,
                    effective_on: on,
                    leave_year: year,
                    source_type: "leave_year_end_run",
                    source_id: run_id,
                    posting_key: &key,
                    reason: Some(reason),
                },
                &a,
            )
            .await?;
            if !r.already_posted {
                match kind {
                    LeaveEntryKind::Expiry => expired += -days,
                    LeaveEntryKind::CarryForward => carried += days,
                    _ => {}
                }
            }
        }
    }
    let row = sqlx::query(&format!(
        "UPDATE people.leave_year_end_run SET staff_count = GREATEST(staff_count, $2), \
         carried_days = carried_days + $3, expired_days = expired_days + $4, \
         version = version + 1 WHERE id = $1 RETURNING {YEAR_END_COLUMNS}"
    ))
    .bind(run_id)
    .bind(i32::try_from(staff_seen.len()).unwrap_or(i32::MAX))
    .bind(carried)
    .bind(expired)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let reason = format!(
        "{}: carried {} expired {}",
        b.leave_year,
        fmt2(carried),
        fmt2(expired)
    );
    let tr = Transition {
        tenant: t,
        institution,
        aggregate_type: "leave_year_end_run",
        aggregate_id: run_id,
        from: None,
        to: "completed",
        reason: Some(&reason),
        process_instance_id: None,
        cause: LEAVE_CARRIED_FORWARD,
    };
    history(&mut tx, &tr, &a).await?;
    emit_status(
        &mut tx,
        &scope,
        LEAVE_CARRIED_FORWARD,
        LEAVE_CARRIED_FORWARD_SCHEMA,
        &tr,
        1,
        &EventParties::default(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_year_end_run.run",
        "leave_year_end_run",
        run_id,
        serde_json::json!({"leaveYear": b.leave_year, "carried": fmt2(carried),
            "expired": fmt2(expired)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(year_end_from_row(&row).map_err(map_db_error)?))
}

/// Year-end runs of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/leave-year-end", operation_id = "people_leave_year_end_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveYearEndRunList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_end_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaveYearEndRunList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let rows = sqlx::query(&format!(
        "SELECT {YEAR_END_COLUMNS} FROM people.leave_year_end_run WHERE tenant_id = $1 \
         AND institution_id = $2 ORDER BY leave_year DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(year_end_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveYearEndRunList { items }))
}

// ------------------------------------------------------------------ adjustments (E-049)

/// A governed balance adjustment.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveAdjustmentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub leave_type_id: Uuid,
    pub leave_type_code: String,
    pub leave_year: i32,
    /// Signed days (`"2.00"`, `"-1.50"`).
    #[schema(example = "2.00")]
    pub days: String,
    pub reason: String,
    pub status: LeaveAdjustmentStatus,
    pub status_reason: Option<String>,
    pub ledger_entry_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveAdjustmentList {
    pub items: Vec<LeaveAdjustmentView>,
}

/// Request a balance adjustment.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveAdjustmentCreate {
    pub leave_type_id: Uuid,
    pub leave_year: i32,
    /// Signed non-zero days, two decimals at most.
    #[schema(example = "2.00")]
    pub days: String,
    pub reason: Option<String>,
}

struct AdjustmentRow {
    view: LeaveAdjustmentView,
    institution: InstitutionId,
    staff: StaffMemberId,
    engagement: Option<StaffEngagementId>,
    days: Decimal,
    is_paid: bool,
}

const ADJ_COLUMNS: &str = "a.id, a.institution_id, a.staff_id, a.engagement_id, a.leave_type_id, \
    a.leave_type_code, a.leave_year, a.days, a.reason, a.status, a.status_reason, \
    a.ledger_entry_id, a.process_instance_id, a.created_at, a.version, t.is_paid";

fn adjustment_from_row(r: &PgRow) -> Result<AdjustmentRow, sqlx::Error> {
    let days: Decimal = r.try_get("days")?;
    let institution: Uuid = r.try_get("institution_id")?;
    let staff: Uuid = r.try_get("staff_id")?;
    Ok(AdjustmentRow {
        view: LeaveAdjustmentView {
            id: r.try_get("id")?,
            institution_id: institution,
            staff_id: staff,
            leave_type_id: r.try_get("leave_type_id")?,
            leave_type_code: r.try_get("leave_type_code")?,
            leave_year: r.try_get("leave_year")?,
            days: fmt2(days),
            reason: r.try_get("reason")?,
            status: crate::shared::rows::decode(
                r.try_get("status")?,
                LeaveAdjustmentStatus::parse,
            )?,
            status_reason: r.try_get("status_reason")?,
            ledger_entry_id: r.try_get("ledger_entry_id")?,
            process_instance_id: r.try_get("process_instance_id")?,
            created_at: r.try_get("created_at")?,
            version: r.try_get("version")?,
        },
        institution: InstitutionId(institution),
        staff: StaffMemberId(staff),
        engagement: r
            .try_get::<Option<Uuid>, _>("engagement_id")?
            .map(StaffEngagementId),
        days,
        is_paid: r.try_get("is_paid")?,
    })
}

async fn load_adjustment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<AdjustmentRow, KernelError> {
    let sql = format!(
        "SELECT {ADJ_COLUMNS} FROM people.leave_adjustment a JOIN people.leave_type t \
         ON t.id = a.leave_type_id WHERE a.id = $1 AND a.tenant_id = $2{}",
        if lock { " FOR UPDATE OF a" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "leave adjustment",
        })?;
    adjustment_from_row(&r).map_err(map_db_error)
}

/// Balance rule of an adjustment: a balance-tracked type never goes below zero.
async fn check_adjustment(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: StaffMemberId,
    lt: &LeaveTypeRow,
    year: i32,
    days: Decimal,
) -> Result<(), KernelError> {
    if days < Decimal::ZERO && lt.requires_balance {
        let b = balance(conn, tenant, staff, lt.id, year).await?;
        if b.available + days < Decimal::ZERO {
            return Err(KernelError::Conflict(format!(
                "the adjustment would make the {} balance negative ({} available)",
                lt.code,
                fmt2(b.available)
            )));
        }
    }
    Ok(())
}

/// Applies an approved adjustment: `adjustment` ledger entry, status `applied`
/// (`leave_adjustment.applied`).
async fn apply_adjustment(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    adj: &AdjustmentRow,
    from: &str,
    reason: Option<&str>,
    a: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    let lt = load_type(conn, t, adj.view.leave_type_id).await?;
    check_adjustment(conn, t, adj.staff, &lt, adj.view.leave_year, adj.days).await?;
    let effective_on = NaiveDate::from_ymd_opt(adj.view.leave_year, 1, 1)
        .ok_or_else(|| invalid("leaveYear", "out_of_range", "2000 … 2200"))?
        .max(business_today_in_year(conn, adj.view.leave_year).await?);
    let receipt = post_entry(
        conn,
        &NewLeaveEntry {
            tenant: t,
            institution: adj.institution,
            staff_id: adj.staff,
            engagement_id: adj.engagement,
            leave_type_id: adj.view.leave_type_id,
            leave_type_code: &adj.view.leave_type_code,
            is_paid: adj.is_paid,
            kind: LeaveEntryKind::Adjustment,
            days: adj.days,
            effective_on,
            leave_year: adj.view.leave_year,
            source_type: "leave_adjustment",
            source_id: adj.view.id,
            posting_key: "adjust",
            reason: Some(&adj.view.reason),
        },
        a,
    )
    .await?;
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.leave_adjustment SET status = 'applied', ledger_entry_id = $2, \
         status_reason = COALESCE($3, status_reason), decided_by = $4, version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(adj.view.id)
    .bind(receipt.entry_id.as_uuid())
    .bind(reason)
    .bind(a.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let staff_subject = subject_of(conn, t, Some(adj.staff)).await?;
    for (f, to) in [(from, "approved"), ("approved", "applied")] {
        history(
            conn,
            &Transition {
                tenant: t,
                institution: adj.institution,
                aggregate_type: "leave_adjustment",
                aggregate_id: adj.view.id,
                from: Some(f),
                to,
                reason,
                process_instance_id: adj.view.process_instance_id.as_deref(),
                cause: LEAVE_BALANCE_ADJUSTED,
            },
            a,
        )
        .await?;
    }
    emit_status(
        conn,
        scope,
        LEAVE_BALANCE_ADJUSTED,
        LEAVE_BALANCE_ADJUSTED_SCHEMA,
        &Transition {
            tenant: t,
            institution: adj.institution,
            aggregate_type: "leave_adjustment",
            aggregate_id: adj.view.id,
            from: Some(from),
            to: "applied",
            reason: Some(
                format!(
                    "{} {} days ({})",
                    adj.view.leave_type_code,
                    fmt2(adj.days),
                    adj.view.reason
                )
                .as_str(),
            ),
            process_instance_id: adj.view.process_instance_id.as_deref(),
            cause: LEAVE_BALANCE_ADJUSTED,
        },
        version,
        &EventParties {
            staff_id: Some(adj.staff.as_uuid()),
            engagement_id: adj.engagement.map(|e| e.as_uuid()),
            staff_subject_ref: staff_subject,
            manager_subject_ref: None,
        },
        a,
    )
    .await
}

/// The effective date of a posting into `year`: today when today is inside that year, else the
/// year's last day (past years) or first day (future years).
async fn business_today_in_year(
    conn: &mut PgConnection,
    year: i32,
) -> Result<NaiveDate, KernelError> {
    let today: NaiveDate = sqlx::query_scalar("SELECT current_date")
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let first = NaiveDate::from_ymd_opt(year, 1, 1).unwrap_or(today);
    let last = NaiveDate::from_ymd_opt(year, 12, 31).unwrap_or(today);
    Ok(today.clamp(first, last))
}

/// Request a governed balance adjustment (`people.leave.manage`; `LEAVE_ADJUSTMENT_APPROVAL`,
/// auto-approval applies it at once).
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/leave-adjustments", operation_id = "people_leave_adjustment_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveAdjustmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeaveAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveAdjustmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let s = staff_in_tenant(&st, &tenant_scope(ctx), id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    let days = parse_decimal("days", &b.days)?;
    if days.is_zero() {
        return Err(invalid(
            "days",
            "zero",
            "an adjustment moves a non-zero number of days",
        )
        .into());
    }
    if !(2000..=2200).contains(&b.leave_year) {
        return Err(invalid("leaveYear", "out_of_range", "2000 … 2200").into());
    }
    let reason = required("reason", b.reason)?;
    let lt = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let lt = load_type(&mut conn, t, b.leave_type_id)
            .await
            .map_err(|_| invalid("leaveTypeId", "unknown", "unknown leave type"))?;
        if lt.institution_id != s.institution_id {
            return Err(invalid("leaveTypeId", "unknown", "unknown leave type").into());
        }
        check_adjustment(&mut conn, t, s.id, &lt, b.leave_year, days).await?;
        lt
    };
    let adj_id = Uuid::now_v7();
    let outcome = request_approval(
        &st,
        &scope,
        process::LEAVE_ADJUSTMENT_APPROVAL,
        business_key("leave_adjustment", adj_id, 1),
        BTreeMap::from([
            ("staffId".to_owned(), serde_json::json!(s.id)),
            ("days".to_owned(), serde_json::json!(fmt2(days))),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, t, s.id).await?;
    let engagement = current_engagement(&mut tx, t, s.id).await?.map(|e| e.id);
    sqlx::query(
        "INSERT INTO people.leave_adjustment (id, tenant_id, institution_id, staff_id, \
         engagement_id, leave_type_id, leave_type_code, leave_year, days, reason, status, \
         process_instance_id, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, \
         'pending_approval', $11, $12)",
    )
    .bind(adj_id)
    .bind(t.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(s.id.as_uuid())
    .bind(engagement.map(|e| e.as_uuid()))
    .bind(lt.id)
    .bind(&lt.code)
    .bind(b.leave_year)
    .bind(days)
    .bind(&reason)
    .bind(&pid)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (from, to) in [(None, "requested"), (Some("requested"), "pending_approval")] {
        history(
            &mut tx,
            &Transition {
                tenant: t,
                institution: s.institution_id,
                aggregate_type: "leave_adjustment",
                aggregate_id: adj_id,
                from,
                to,
                reason: Some(&reason),
                process_instance_id: Some(&pid),
                cause: "sutraerp.people.leave_adjustment.requested",
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.leave_adjustment.request",
        "leave_adjustment",
        adj_id,
        serde_json::json!({"days": fmt2(days), "leaveTypeCode": lt.code, "reason": reason}),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let adj = load_adjustment(&mut tx, t, adj_id, true).await?;
        apply_adjustment(
            &mut tx,
            &scope,
            &adj,
            "pending_approval",
            Some("auto-approved"),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let adj = load_adjustment(&mut conn, t, adj_id, false).await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &adj.view,
        Version(adj.view.version),
    ))
}

/// Adjustments of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/leave-adjustments", operation_id = "people_leave_adjustment_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveAdjustmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaveAdjustmentList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    staff_in_tenant(&st, &tenant_scope(ctx), id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {ADJ_COLUMNS} FROM people.leave_adjustment a JOIN people.leave_type t \
         ON t.id = a.leave_type_id WHERE a.tenant_id = $1 AND a.staff_id = $2 \
         ORDER BY a.created_at DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| adjustment_from_row(r).map(|x| x.view))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveAdjustmentList { items }))
}

/// Decide a pending adjustment (`people.approve`, step-up): approve applies it.
#[utoipa::path(post, path = "/api/v1/people/leave-adjustments/{id}/decision", operation_id = "people_leave_adjustment_decide", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment")),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn adjustment_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "leave adjustment").await?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason.clone());
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let adj = load_adjustment(&mut tx, t, id, true).await?;
    ensure_version(v, adj.view.version)?;
    let scope = institution_scope(ctx, adj.institution);
    let to = if b.approve() {
        LeaveAdjustmentStatus::Approved
    } else {
        LeaveAdjustmentStatus::Rejected
    };
    if !adjustment_can_transition(adj.view.status, to) {
        return Err(illegal(
            "the leave adjustment",
            adj.view.status.as_str(),
            to.as_str(),
        )
        .into());
    }
    if b.approve() {
        apply_adjustment(
            &mut tx,
            &scope,
            &adj,
            "pending_approval",
            reason.as_deref(),
            &a,
        )
        .await?;
    } else {
        let reason = reason
            .clone()
            .ok_or_else(|| invalid("reason", "required", "a rejection needs a reason"))?;
        sqlx::query(
            "UPDATE people.leave_adjustment SET status = 'rejected', status_reason = $2, \
             decided_by = $3, version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(&reason)
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            &Transition {
                tenant: t,
                institution: adj.institution,
                aggregate_type: "leave_adjustment",
                aggregate_id: id,
                from: Some("pending_approval"),
                to: "rejected",
                reason: Some(&reason),
                process_instance_id: adj.view.process_instance_id.as_deref(),
                cause: "sutraerp.people.leave_adjustment.rejected",
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.leave_adjustment.decide",
        "leave_adjustment",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let adj = load_adjustment(&mut conn, t, id, false).await?;
    Ok(with_etag(
        StatusCode::OK,
        &adj.view,
        Version(adj.view.version),
    ))
}

// ------------------------------------------------------------------ encashment (E-051)

/// An encashment eligibility decision (7C pays the `encashment` ledger entry).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveEncashmentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub leave_type_id: Uuid,
    pub leave_type_code: String,
    pub leave_year: i32,
    #[schema(example = "5.00")]
    pub days: String,
    /// Balance when eligibility was evaluated.
    #[schema(example = "9.50")]
    pub available_days: String,
    #[schema(example = "10.00")]
    pub encashment_cap: String,
    pub reason: Option<String>,
    pub status: EncashmentStatus,
    pub status_reason: Option<String>,
    /// The `encashment` ledger entry (negative days) once approved.
    pub ledger_entry_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveEncashmentList {
    pub items: Vec<LeaveEncashmentView>,
}

/// Request encashment of leave days.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveEncashmentCreate {
    pub leave_type_id: Uuid,
    pub leave_year: i32,
    #[schema(example = "5.00")]
    pub days: String,
    pub reason: Option<String>,
}

struct EncashmentRow {
    view: LeaveEncashmentView,
    institution: InstitutionId,
    staff: StaffMemberId,
    engagement: Option<StaffEngagementId>,
    days: Decimal,
}

const ENC_COLUMNS: &str = "id, institution_id, staff_id, engagement_id, leave_type_id, \
    leave_type_code, leave_year, days, available_days, encashment_cap, reason, status, \
    status_reason, ledger_entry_id, process_instance_id, created_at, version";

fn encashment_from_row(r: &PgRow) -> Result<EncashmentRow, sqlx::Error> {
    let days: Decimal = r.try_get("days")?;
    let institution: Uuid = r.try_get("institution_id")?;
    let staff: Uuid = r.try_get("staff_id")?;
    Ok(EncashmentRow {
        view: LeaveEncashmentView {
            id: r.try_get("id")?,
            institution_id: institution,
            staff_id: staff,
            leave_type_id: r.try_get("leave_type_id")?,
            leave_type_code: r.try_get("leave_type_code")?,
            leave_year: r.try_get("leave_year")?,
            days: fmt2(days),
            available_days: fmt2(r.try_get("available_days")?),
            encashment_cap: fmt2(r.try_get("encashment_cap")?),
            reason: r.try_get("reason")?,
            status: crate::shared::rows::decode(r.try_get("status")?, EncashmentStatus::parse)?,
            status_reason: r.try_get("status_reason")?,
            ledger_entry_id: r.try_get("ledger_entry_id")?,
            process_instance_id: r.try_get("process_instance_id")?,
            created_at: r.try_get("created_at")?,
            version: r.try_get("version")?,
        },
        institution: InstitutionId(institution),
        staff: StaffMemberId(staff),
        engagement: r
            .try_get::<Option<Uuid>, _>("engagement_id")?
            .map(StaffEngagementId),
        days,
    })
}

async fn load_encashment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<EncashmentRow, KernelError> {
    let sql = format!(
        "SELECT {ENC_COLUMNS} FROM people.leave_encashment WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "leave encashment",
        })?;
    encashment_from_row(&r).map_err(map_db_error)
}

/// Final encashment decision (row locked, status `eligible`): approve → `encashment` entry
/// (negative days, `encash`) after a balance re-check; reject → `rejected`. Emits
/// `leave_encashment.decided`.
async fn decide_encashment(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    enc: &EncashmentRow,
    approve: bool,
    reason: Option<&str>,
    a: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    let to = if approve {
        EncashmentStatus::Approved
    } else {
        EncashmentStatus::Rejected
    };
    if !super::domain::encashment_can_transition(enc.view.status, to) {
        return Err(illegal(
            "the leave encashment",
            enc.view.status.as_str(),
            to.as_str(),
        ));
    }
    let mut entry_id = None;
    if approve {
        let lt = load_type(conn, t, enc.view.leave_type_id).await?;
        let b = balance(conn, t, enc.staff, lt.id, enc.view.leave_year).await?;
        encashment_eligibility(lt.is_paid, lt.encashment_cap, b.available, enc.days).map_err(
            |code| KernelError::Conflict(format!("the encashment is no longer eligible ({code})")),
        )?;
        let on = business_today_in_year(conn, enc.view.leave_year).await?;
        let r = post_entry(
            conn,
            &NewLeaveEntry {
                tenant: t,
                institution: enc.institution,
                staff_id: enc.staff,
                engagement_id: enc.engagement,
                leave_type_id: lt.id,
                leave_type_code: &lt.code,
                is_paid: lt.is_paid,
                kind: LeaveEntryKind::Encashment,
                days: -enc.days,
                effective_on: on,
                leave_year: enc.view.leave_year,
                source_type: "leave_encashment",
                source_id: enc.view.id,
                posting_key: "encash",
                reason: Some("leave encashment approved"),
            },
            a,
        )
        .await?;
        entry_id = Some(r.entry_id.as_uuid());
    }
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.leave_encashment SET status = $2, status_reason = $3, ledger_entry_id = $4, \
         decided_by = $5, version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(enc.view.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(entry_id)
    .bind(a.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution: enc.institution,
        aggregate_type: "leave_encashment",
        aggregate_id: enc.view.id,
        from: Some(enc.view.status.as_str()),
        to: to.as_str(),
        reason,
        process_instance_id: enc.view.process_instance_id.as_deref(),
        cause: LEAVE_ENCASHMENT_DECIDED,
    };
    history(conn, &tr, a).await?;
    let subject = subject_of(conn, t, Some(enc.staff)).await?;
    emit_status(
        conn,
        scope,
        LEAVE_ENCASHMENT_DECIDED,
        LEAVE_ENCASHMENT_DECIDED_SCHEMA,
        &tr,
        version,
        &EventParties {
            staff_id: Some(enc.staff.as_uuid()),
            engagement_id: enc.engagement.map(|e| e.as_uuid()),
            staff_subject_ref: subject,
            manager_subject_ref: None,
        },
        a,
    )
    .await
}

/// Request encashment (`people.leave.manage`): eligibility is evaluated at once (paid type with an
/// encashment cap, days ≤ cap and ≤ balance) → `not_eligible` (decided) or `eligible` →
/// `ENCASHMENT_DECISION` (auto-approval: approved with its `encashment` entry).
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/leave-encashments", operation_id = "people_leave_encashment_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveEncashmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeaveEncashmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn encashment_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveEncashmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let s = staff_in_tenant(&st, &tenant_scope(ctx), id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    let days = parse_decimal("days", &b.days)?;
    if days <= Decimal::ZERO {
        return Err(invalid("days", "not_positive", "encash a positive number of days").into());
    }
    if !(2000..=2200).contains(&b.leave_year) {
        return Err(invalid("leaveYear", "out_of_range", "2000 … 2200").into());
    }
    let reason = clean(b.reason);
    let (lt, available) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let lt = load_type(&mut conn, t, b.leave_type_id)
            .await
            .map_err(|_| invalid("leaveTypeId", "unknown", "unknown leave type"))?;
        if lt.institution_id != s.institution_id {
            return Err(invalid("leaveTypeId", "unknown", "unknown leave type").into());
        }
        let bal = balance(&mut conn, t, s.id, lt.id, b.leave_year).await?;
        (lt, bal.available)
    };
    let eligibility = encashment_eligibility(lt.is_paid, lt.encashment_cap, available, days);
    let enc_id = Uuid::now_v7();
    let outcome = if eligibility.is_ok() {
        Some(
            request_approval(
                &st,
                &scope,
                process::ENCASHMENT_DECISION,
                business_key("leave_encashment", enc_id, 1),
                BTreeMap::from([
                    ("staffId".to_owned(), serde_json::json!(s.id)),
                    ("days".to_owned(), serde_json::json!(fmt2(days))),
                ]),
            )
            .await?,
        )
    } else {
        None
    };
    let pid = outcome.as_ref().map(|o| o.process_instance_id().to_owned());
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, t, s.id).await?;
    let engagement = current_engagement(&mut tx, t, s.id).await?.map(|e| e.id);
    let (status, status_reason) = match eligibility {
        Ok(()) => (EncashmentStatus::Eligible, None),
        Err(code) => (EncashmentStatus::NotEligible, Some(code.to_owned())),
    };
    sqlx::query(
        "INSERT INTO people.leave_encashment (id, tenant_id, institution_id, staff_id, \
         engagement_id, leave_type_id, leave_type_code, leave_year, days, available_days, \
         encashment_cap, reason, status, status_reason, process_instance_id, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)",
    )
    .bind(enc_id)
    .bind(t.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(s.id.as_uuid())
    .bind(engagement.map(|e| e.as_uuid()))
    .bind(lt.id)
    .bind(&lt.code)
    .bind(b.leave_year)
    .bind(days)
    .bind(available)
    .bind(lt.encashment_cap)
    .bind(reason.as_deref())
    .bind(status.as_str())
    .bind(status_reason.as_deref())
    .bind(pid.as_deref())
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution: s.institution_id,
        aggregate_type: "leave_encashment",
        aggregate_id: enc_id,
        from: Some("requested"),
        to: status.as_str(),
        reason: status_reason.as_deref(),
        process_instance_id: pid.as_deref(),
        cause: LEAVE_ENCASHMENT_DECIDED,
    };
    history(
        &mut tx,
        &Transition {
            from: None,
            to: "requested",
            cause: "sutraerp.people.leave_encashment.requested",
            ..copy_transition(&tr)
        },
        &a,
    )
    .await?;
    history(&mut tx, &tr, &a).await?;
    if status == EncashmentStatus::NotEligible {
        emit_status(
            &mut tx,
            &scope,
            LEAVE_ENCASHMENT_DECIDED,
            LEAVE_ENCASHMENT_DECIDED_SCHEMA,
            &tr,
            1,
            &EventParties {
                staff_id: Some(s.id.as_uuid()),
                engagement_id: engagement.map(|e| e.as_uuid()),
                staff_subject_ref: s.subject_ref.clone(),
                manager_subject_ref: None,
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.leave_encashment.request",
        "leave_encashment",
        enc_id,
        serde_json::json!({"days": fmt2(days), "available": fmt2(available),
            "status": status, "statusReason": status_reason}),
        &a,
    )
    .await?;
    if let Some(ApprovalOutcome::Approved { .. }) = outcome {
        let enc = load_encashment(&mut tx, t, enc_id, true).await?;
        decide_encashment(
            &mut tx,
            &scope,
            &enc,
            true,
            Some("auto-approved"),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let enc = load_encashment(&mut conn, t, enc_id, false).await?;
    Ok(with_etag(
        StatusCode::CREATED,
        &enc.view,
        Version(enc.view.version),
    ))
}

fn copy_transition<'a>(t: &Transition<'a>) -> Transition<'a> {
    Transition {
        tenant: t.tenant,
        institution: t.institution,
        aggregate_type: t.aggregate_type,
        aggregate_id: t.aggregate_id,
        from: t.from,
        to: t.to,
        reason: t.reason,
        process_instance_id: t.process_instance_id,
        cause: t.cause,
    }
}

/// Encashment decisions of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/leave-encashments", operation_id = "people_leave_encashment_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveEncashmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn encashment_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaveEncashmentList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    staff_in_tenant(&st, &tenant_scope(ctx), id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {ENC_COLUMNS} FROM people.leave_encashment WHERE tenant_id = $1 AND staff_id = $2 \
         ORDER BY created_at DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| encashment_from_row(r).map(|x| x.view))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveEncashmentList { items }))
}

/// Decide an eligible encashment (`people.approve`, step-up).
#[utoipa::path(post, path = "/api/v1/people/leave-encashments/{id}/decision", operation_id = "people_leave_encashment_decide", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the encashment")),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveEncashmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn encashment_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "leave encashment").await?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason.clone());
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let enc = load_encashment(&mut tx, t, id, true).await?;
    ensure_version(v, enc.view.version)?;
    let scope = institution_scope(ctx, enc.institution);
    decide_encashment(&mut tx, &scope, &enc, b.approve(), reason.as_deref(), &a).await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_encashment.decide",
        "leave_encashment",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let enc = load_encashment(&mut conn, t, id, false).await?;
    Ok(with_etag(
        StatusCode::OK,
        &enc.view,
        Version(enc.view.version),
    ))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(balance_list))
        .routes(routes!(my_balance_list))
        .routes(routes!(ledger_list))
        .routes(routes!(accrual_run, accrual_list))
        .routes(routes!(year_end_run, year_end_list))
        .routes(routes!(adjustment_create, adjustment_list))
        .routes(routes!(adjustment_decide))
        .routes(routes!(encashment_create, encashment_list))
        .routes(routes!(encashment_decide))
        .with_state(state)
}
