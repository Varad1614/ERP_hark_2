//! **ERP_EXTENSIONS 1E** — periodic billing runs (tag `finance-charges`).
//!
//! * Billing plans (`/institutions/{id}/billing-plans`, `/billing-plans/{id}`): a fee head billed
//!   at an amount every week, month, quarter, half-year or financial year, optionally prorated
//!   by day for a part period. Students are enrolled on plans (`/billing-plans/{id}/enrolments`)
//!   at the plan's amount or their own, from / until a date, or on hold.
//! * Runs (`/institutions/{id}/billing-runs[/preview]`): for the period containing `day`, one
//!   charge per active student with plans of the frequency (a line per plan; `source_type`
//!   `billing_run`, `source_id` = the run item), validated and published (Dr receivables · Cr the
//!   fee heads' income, `charge.published`). A student is billed at most once per frequency and
//!   period while that charge stands (a cancelled charge frees the period). A student that
//!   cannot be billed (no live context, a closed period) is reported and the run goes on.
//! * Schedules (`/institutions/{id}/billing-schedules[/{frequency}]`): with a schedule enabled,
//!   [`BillingScheduleJob`] runs the frequency on day `runDay` of each period (once per period),
//!   as `system:billing-schedule`.
//!
//! The period and selection rules are `super::billing` (ported from Everything's childcare app,
//! MIT).

use super::billing::{
    period_for, plan_billing_run, scheduled_period, Draft, Enrolled, Frequency, Period, Plan,
    Proration, RunOptions, SkipReason, Student,
};
use super::kit::{invalid, parse_amount, parse_decimal};
use super::repo::{create_in_tx, line, publish_locked, validate_locked, ChargeInput};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::money::fmt;
use crate::shared::publish::audit;
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{Datelike, Duration, NaiveDate};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashMap, HashSet};
use sutra_contracts::finance::status::ChargeSourceType;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::StudentContextStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// The schedule job's actor.
pub const SCHEDULE_ACTOR: &str = "system:billing-schedule";

// ----------------------------------------------------------------------------- views

/// A billing plan.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingPlanView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub fee_head_id: Uuid,
    pub fee_head_code: String,
    #[schema(value_type = String, example = "4500.00")]
    pub amount: String,
    /// `weekly | monthly | quarterly | half_yearly | annual` (quarters, halves and years follow
    /// the institution's financial year).
    pub frequency: String,
    /// `none | daily` — how a part period is billed.
    pub proration: String,
    /// `active | retired`.
    pub status: String,
    /// Students enrolled.
    pub enrolled: i64,
    pub version: i32,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BillingPlanList {
    pub items: Vec<BillingPlanView>,
}

/// New billing plan.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingPlanCreate {
    /// `^[A-Z0-9][A-Z0-9_-]{1,31}$`, unique in the institution.
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    /// An active fee head of the institution: the plan's income.
    pub fee_head_id: Uuid,
    #[schema(example = "4500.00")]
    pub amount: String,
    pub frequency: String,
    /// Defaults to `none`.
    pub proration: Option<String>,
}

/// Billing plan changes (absent fields stay). A new amount applies to later runs.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingPlanUpdate {
    pub name: Option<String>,
    pub description: Option<String>,
    pub amount: Option<String>,
    pub proration: Option<String>,
    /// `active | retired` (a retired plan is not billed).
    pub status: Option<String>,
}

/// A student on a plan.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingEnrolmentView {
    pub id: Uuid,
    pub plan_id: Uuid,
    pub student_id: Uuid,
    pub student_name: String,
    pub student_number: Option<String>,
    /// The student's own amount (else the plan's).
    #[schema(value_type = Option<String>, example = "4000.00")]
    pub amount: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub on_hold: bool,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BillingEnrolmentList {
    pub items: Vec<BillingEnrolmentView>,
}

/// Enrols a student on the plan, or changes their enrolment.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingEnrolmentUpsert {
    pub student_id: Uuid,
    /// The student's own amount (`"0.00"` bills nothing); absent / null uses the plan's.
    pub amount: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    #[serde(default)]
    pub on_hold: bool,
}

/// A billing run (or its preview).
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingRunRequest {
    pub frequency: String,
    /// A day in the period to bill (default: today). At most 62 days ahead.
    pub day: Option<NaiveDate>,
    /// Days from the later of the period's start and the run day to the due date (0–365,
    /// default 7).
    pub due_days: Option<i64>,
    /// Weekly periods start on 0 = Sunday … 6 = Saturday (default 1, Monday).
    pub week_start: Option<u32>,
    /// Students to leave out of this run.
    #[serde(default)]
    pub exclude_student_ids: Vec<Uuid>,
}

/// One plan line billed (or to bill).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingLineView {
    pub plan_id: Uuid,
    pub description: String,
    #[schema(value_type = String, example = "4500.00")]
    pub amount: String,
    /// Days billed of the period when the line was prorated.
    pub prorated_days: Option<i64>,
    pub period_days: Option<i64>,
}

/// One student's charge in a run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingRunItemView {
    pub student_id: Uuid,
    pub student_name: String,
    pub student_number: Option<String>,
    pub due_date: NaiveDate,
    #[schema(value_type = String, example = "4500.00")]
    pub total: String,
    pub lines: Vec<BillingLineView>,
    pub charge_id: Option<Uuid>,
    pub charge_number: Option<String>,
    /// Why the student could not be billed (the run went on).
    pub error: Option<String>,
}

/// A student with plans of the frequency that the run leaves out.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingRunSkipView {
    pub student_id: Uuid,
    pub student_name: String,
    /// `inactive | already_billed | not_started | ended | on_hold | excluded | nothing_to_bill`.
    pub reason: String,
}

/// What a run did (or, as a preview, would do).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingRunView {
    /// The run (none for a preview).
    pub id: Option<Uuid>,
    pub preview: bool,
    pub frequency: String,
    pub period_key: String,
    pub period_label: String,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub run_date: NaiveDate,
    pub due_days: i64,
    /// `manual | schedule`.
    pub trigger: String,
    pub billed: i64,
    pub skipped: i64,
    pub failed: i64,
    #[schema(value_type = String, example = "90000.00")]
    pub total: String,
    pub items: Vec<BillingRunItemView>,
    pub skips: Vec<BillingRunSkipView>,
}

/// A past run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingRunSummary {
    pub id: Uuid,
    pub frequency: String,
    pub period_key: String,
    pub period_label: String,
    pub run_date: NaiveDate,
    pub trigger: String,
    pub billed: i64,
    pub skipped: i64,
    pub failed: i64,
    #[schema(value_type = String, example = "90000.00")]
    pub total: String,
    pub created_by: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BillingRunList {
    pub items: Vec<BillingRunSummary>,
}

/// A past run with its charges.
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingRunDetail {
    pub run: BillingRunSummary,
    pub items: Vec<BillingRunItemView>,
}

/// The automatic run of one frequency.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BillingScheduleView {
    pub frequency: String,
    pub enabled: bool,
    /// The day of each period it runs on (1 = the first; past a short period's end, its last).
    pub run_day: i32,
    pub due_days: i32,
    pub last_period_key: Option<String>,
    pub last_run_id: Option<Uuid>,
    /// The period the current one would bill.
    pub current_period_label: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BillingScheduleList {
    pub items: Vec<BillingScheduleView>,
}

/// Sets a frequency's automatic run.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BillingScheduleUpdate {
    pub enabled: bool,
    /// 1–31.
    pub run_day: i32,
    /// 0–365.
    pub due_days: i32,
}

// ----------------------------------------------------------------------------- helpers

fn frequency(field: &str, raw: &str) -> Result<Frequency, KernelError> {
    Frequency::parse(raw).ok_or_else(|| {
        invalid(
            field,
            "invalid_frequency",
            "must be weekly, monthly, quarterly, half_yearly or annual",
        )
    })
}

fn proration(raw: &str) -> Result<Proration, KernelError> {
    Proration::parse(raw)
        .ok_or_else(|| invalid("proration", "invalid_proration", "must be none or daily"))
}

fn valid_code(code: &str) -> bool {
    let b = code.as_bytes();
    (2..=32).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn clean_name(raw: &str) -> Result<String, KernelError> {
    let n = raw.trim();
    if n.is_empty() || n.chars().count() > 120 {
        return Err(invalid("name", "invalid_length", "must be 1–120 characters"));
    }
    Ok(n.to_string())
}

fn count(n: usize) -> i64 {
    i64::try_from(n).unwrap_or(i64::MAX)
}

/// The month the institution's financial year starts in (April when none is defined).
async fn fy_start_month(
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

const PLAN_SELECT: &str = "SELECT p.id, p.institution_id, p.code, p.name, p.description, p.fee_head_id, \
     h.code AS fee_head_code, p.amount, p.frequency, p.proration, p.status, p.version, \
     (SELECT count(*) FROM finance.billing_plan_enrolment e WHERE e.tenant_id = p.tenant_id AND e.plan_id = p.id) AS enrolled \
     FROM finance.billing_plan p JOIN finance.fee_head h ON h.id = p.fee_head_id";

fn plan_view(r: &sqlx::postgres::PgRow) -> Result<BillingPlanView, KernelError> {
    let g = |e| map_db_error(e);
    Ok(BillingPlanView {
        id: r.try_get("id").map_err(g)?,
        institution_id: r.try_get("institution_id").map_err(g)?,
        code: r.try_get("code").map_err(g)?,
        name: r.try_get("name").map_err(g)?,
        description: r.try_get("description").map_err(g)?,
        fee_head_id: r.try_get("fee_head_id").map_err(g)?,
        fee_head_code: r.try_get("fee_head_code").map_err(g)?,
        amount: fmt(r.try_get("amount").map_err(g)?),
        frequency: r.try_get("frequency").map_err(g)?,
        proration: r.try_get("proration").map_err(g)?,
        status: r.try_get("status").map_err(g)?,
        enrolled: r.try_get("enrolled").map_err(g)?,
        version: r.try_get("version").map_err(g)?,
    })
}

async fn load_plan(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<BillingPlanView, KernelError> {
    let r = sqlx::query(&format!("{PLAN_SELECT} WHERE p.tenant_id = $1 AND p.id = $2"))
        .bind(t.as_uuid())
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "billing_plan",
        })?;
    plan_view(&r)
}

/// Student name and number: the finance account's, else the student directory's.
async fn student_names(
    st: &FinanceState,
    scope: &ExecutionScope,
    t: TenantId,
    inst: InstitutionId,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, (String, Option<String>)>, KernelError> {
    let mut out: HashMap<Uuid, (String, Option<String>)> = HashMap::new();
    let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT student_id, display_name, student_number FROM finance.student_account \
         WHERE tenant_id = $1 AND institution_id = $2 AND student_id = ANY($3)",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(ids)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    for (id, name, number) in rows {
        out.insert(id, (name, number));
    }
    for id in ids {
        if out.contains_key(id) {
            continue;
        }
        let entry = match st.students.student(scope, StudentId(*id)).await? {
            Some(s) => (s.display_name, Some(s.student_number)),
            None => ("(unknown student)".to_string(), None),
        };
        out.insert(*id, entry);
    }
    Ok(out)
}

// ----------------------------------------------------------------------------- plans

/// Billing plans of an institution (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/billing-plans", operation_id = "finance_charge_billing_plan_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BillingPlanList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_plan_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BillingPlanList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let rows = sqlx::query(&format!(
        "{PLAN_SELECT} WHERE p.tenant_id = $1 AND p.institution_id = $2 ORDER BY p.status, p.code"
    ))
    .bind(tenant(ctx).as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows.iter().map(plan_view).collect::<Result<_, _>>()?;
    Ok(Json(BillingPlanList { items }))
}

/// Creates a billing plan (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/billing-plans", operation_id = "finance_charge_billing_plan_create", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), request_body = BillingPlanCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = BillingPlanView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_plan_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<BillingPlanCreate>,
) -> ApiResult<(StatusCode, Json<BillingPlanView>)> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let code = b.code.trim().to_string();
    if !valid_code(&code) {
        return Err(invalid("code", "invalid_code", "must match ^[A-Z0-9][A-Z0-9_-]{1,31}$").into());
    }
    let name = clean_name(&b.name)?;
    let amount = parse_amount("amount", &b.amount)?;
    let freq = frequency("frequency", &b.frequency)?;
    let pro = proration(b.proration.as_deref().unwrap_or("none"))?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let head: Option<String> = sqlx::query_scalar(
        "SELECT status FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 AND id = $3",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.fee_head_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    match head.as_deref() {
        None => return Err(invalid("feeHeadId", "not_found", "no such fee head in this institution").into()),
        Some("active") => {}
        Some(s) => {
            return Err(invalid("feeHeadId", "fee_head_not_active", &format!("the fee head is {s}")).into())
        }
    }
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM finance.billing_plan WHERE tenant_id = $1 AND institution_id = $2 AND code = $3)",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if exists {
        return Err(KernelError::Conflict(format!("billing plan {code} already exists")).into());
    }
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.billing_plan (tenant_id, institution_id, code, name, description, fee_head_id, amount, \
         frequency, proration, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(b.description.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(b.fee_head_id)
    .bind(amount)
    .bind(freq.as_str())
    .bind(pro.as_str())
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.billing_plan.create",
        "billing_plan",
        pid.to_string(),
        json!({"code": code, "amount": fmt(amount), "frequency": freq.as_str(), "proration": pro.as_str(), "feeHeadId": b.fee_head_id}),
        &a,
    )
    .await?;
    let view = load_plan(&mut tx, t, pid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Changes a billing plan (`charge.manage`).
#[utoipa::path(patch, path = "/api/v1/finance/billing-plans/{id}", operation_id = "finance_charge_billing_plan_update", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), request_body = BillingPlanUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = BillingPlanView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_plan_update(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<BillingPlanUpdate>,
) -> ApiResult<Json<BillingPlanView>> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let current = sqlx::query(
        "SELECT institution_id, name, description, amount, proration, status FROM finance.billing_plan \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "billing_plan",
    })?;
    let inst = InstitutionId(current.try_get("institution_id").map_err(map_db_error)?);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let name = match &b.name {
        Some(n) => clean_name(n)?,
        None => current.try_get("name").map_err(map_db_error)?,
    };
    let description: Option<String> = match &b.description {
        Some(d) => Some(d.trim().to_string()).filter(|s| !s.is_empty()),
        None => current.try_get("description").map_err(map_db_error)?,
    };
    let amount: Decimal = match &b.amount {
        Some(raw) => parse_amount("amount", raw)?,
        None => current.try_get("amount").map_err(map_db_error)?,
    };
    let pro: String = match &b.proration {
        Some(p) => proration(p)?.as_str().to_string(),
        None => current.try_get("proration").map_err(map_db_error)?,
    };
    let status: String = match b.status.as_deref() {
        Some(s @ ("active" | "retired")) => s.to_string(),
        Some(_) => return Err(invalid("status", "invalid_status", "must be active or retired").into()),
        None => current.try_get("status").map_err(map_db_error)?,
    };
    sqlx::query(
        "UPDATE finance.billing_plan SET name = $3, description = $4, amount = $5, proration = $6, status = $7, \
         version = version + 1, updated_at = now() WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&name)
    .bind(&description)
    .bind(amount)
    .bind(&pro)
    .bind(&status)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.billing_plan.update",
        "billing_plan",
        id.to_string(),
        json!({"name": name, "amount": fmt(amount), "proration": pro, "status": status}),
        &a,
    )
    .await?;
    let view = load_plan(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

// ----------------------------------------------------------------------------- enrolments

async fn enrolment_views(
    st: &FinanceState,
    scope: &ExecutionScope,
    t: TenantId,
    inst: InstitutionId,
    plan: Uuid,
    only: Option<Uuid>,
) -> Result<Vec<BillingEnrolmentView>, KernelError> {
    let rows: Vec<(Uuid, Uuid, Option<Decimal>, Option<NaiveDate>, Option<NaiveDate>, bool)> = sqlx::query_as(
        "SELECT id, student_id, amount, start_date, end_date, on_hold FROM finance.billing_plan_enrolment \
         WHERE tenant_id = $1 AND plan_id = $2 AND ($3::uuid IS NULL OR student_id = $3) ORDER BY created_at",
    )
    .bind(t.as_uuid())
    .bind(plan)
    .bind(only)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let ids: Vec<Uuid> = rows.iter().map(|r| r.1).collect();
    let names = student_names(st, scope, t, inst, &ids).await?;
    let mut out: Vec<BillingEnrolmentView> = rows
        .into_iter()
        .map(|(eid, sid, amount, start, end, hold)| {
            let (name, number) = names.get(&sid).cloned().unwrap_or_default();
            BillingEnrolmentView {
                id: eid,
                plan_id: plan,
                student_id: sid,
                student_name: name,
                student_number: number,
                amount: amount.map(fmt),
                start_date: start,
                end_date: end,
                on_hold: hold,
            }
        })
        .collect();
    out.sort_by(|a, b| a.student_name.cmp(&b.student_name));
    Ok(out)
}

async fn plan_institution(st: &FinanceState, t: TenantId, plan: Uuid) -> Result<InstitutionId, KernelError> {
    let inst: Option<Uuid> = sqlx::query_scalar(
        "SELECT institution_id FROM finance.billing_plan WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(plan)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    inst.map(InstitutionId).ok_or(KernelError::NotFound {
        entity: "billing_plan",
    })
}

/// Students on a plan (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/billing-plans/{id}/enrolments", operation_id = "finance_charge_billing_enrolment_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BillingEnrolmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_enrolment_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BillingEnrolmentList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let inst = plan_institution(&st, t, id).await?;
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let items = enrolment_views(&st, &scope, t, inst, id, None).await?;
    Ok(Json(BillingEnrolmentList { items }))
}

/// Enrols a student on a plan or changes their enrolment (`charge.manage`). The student must
/// have a live context in the plan's institution.
#[utoipa::path(put, path = "/api/v1/finance/billing-plans/{id}/enrolments", operation_id = "finance_charge_billing_enrolment_upsert", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), request_body = BillingEnrolmentUpsert, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = BillingEnrolmentView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_enrolment_upsert(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<BillingEnrolmentUpsert>,
) -> ApiResult<Json<BillingEnrolmentView>> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let inst = plan_institution(&st, t, id).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let amount = match b.amount.as_deref() {
        Some(raw) => {
            let d = parse_decimal("amount", raw)?;
            if d < Decimal::ZERO || d.scale() > 2 {
                return Err(invalid("amount", "invalid_amount", "must be zero or more, to the paisa").into());
            }
            Some(d)
        }
        None => None,
    };
    if let (Some(s), Some(e)) = (b.start_date, b.end_date) {
        if e < s {
            return Err(invalid("endDate", "before_start", "must be on or after startDate").into());
        }
    }
    if st.students.student(&scope, StudentId(b.student_id)).await?.is_none() {
        return Err(invalid("studentId", "not_found", "no such student in this tenant").into());
    }
    if st
        .students
        .live_context(&scope, StudentId(b.student_id), inst)
        .await?
        .is_none()
    {
        return Err(invalid(
            "studentId",
            "no_live_context",
            "the student has no live context in this institution",
        )
        .into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let eid: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.billing_plan_enrolment (tenant_id, institution_id, plan_id, student_id, amount, start_date, \
         end_date, on_hold, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (tenant_id, plan_id, student_id) DO UPDATE SET amount = EXCLUDED.amount, \
         start_date = EXCLUDED.start_date, end_date = EXCLUDED.end_date, on_hold = EXCLUDED.on_hold, updated_at = now() \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(id)
    .bind(b.student_id)
    .bind(amount)
    .bind(b.start_date)
    .bind(b.end_date)
    .bind(b.on_hold)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.billing_enrolment.upsert",
        "billing_plan_enrolment",
        eid.to_string(),
        json!({"planId": id, "studentId": b.student_id, "amount": amount.map(fmt), "startDate": b.start_date,
               "endDate": b.end_date, "onHold": b.on_hold}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let view = enrolment_views(&st, &scope, t, inst, id, Some(b.student_id))
        .await?
        .pop()
        .ok_or_else(|| KernelError::internal_msg("enrolment vanished"))?;
    Ok(Json(view))
}

/// Takes a student off a plan (`charge.manage`); charges already raised stay.
#[utoipa::path(delete, path = "/api/v1/finance/billing-plans/{id}/enrolments/{studentId}", operation_id = "finance_charge_billing_enrolment_remove", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("studentId" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_enrolment_remove(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path((id, student)): Path<(Uuid, Uuid)>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let inst = plan_institution(&st, t, id).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let eid: Option<Uuid> = sqlx::query_scalar(
        "DELETE FROM finance.billing_plan_enrolment WHERE tenant_id = $1 AND plan_id = $2 AND student_id = $3 RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(student)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let eid = eid.ok_or(KernelError::NotFound {
        entity: "billing_plan_enrolment",
    })?;
    audit(
        &mut tx,
        &scope,
        "finance.billing_enrolment.remove",
        "billing_plan_enrolment",
        eid.to_string(),
        json!({"planId": id, "studentId": student}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

// ----------------------------------------------------------------------------- runs

/// Everything a run needs to decide, loaded for one frequency.
struct Roster {
    plans: Vec<Plan>,
    fee_heads: HashMap<Uuid, Uuid>,
    students: Vec<Student>,
}

async fn roster(
    st: &FinanceState,
    scope: &ExecutionScope,
    t: TenantId,
    inst: InstitutionId,
    freq: Frequency,
) -> Result<Roster, KernelError> {
    let plan_rows: Vec<(Uuid, String, Decimal, String, Uuid)> = sqlx::query_as(
        "SELECT id, name, amount, proration, fee_head_id FROM finance.billing_plan \
         WHERE tenant_id = $1 AND institution_id = $2 AND frequency = $3 AND status = 'active' ORDER BY code",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(freq.as_str())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut fee_heads = HashMap::new();
    let plans: Vec<Plan> = plan_rows
        .into_iter()
        .map(|(id, name, amount, pro, head)| {
            fee_heads.insert(id, head);
            Plan {
                id,
                name,
                amount,
                frequency: freq,
                proration: Proration::parse(&pro).unwrap_or(Proration::None),
            }
        })
        .collect();
    let ids: Vec<Uuid> = plans.iter().map(|p| p.id).collect();
    let rows: Vec<(Uuid, Uuid, Option<Decimal>, Option<NaiveDate>, Option<NaiveDate>, bool)> = sqlx::query_as(
        "SELECT student_id, plan_id, amount, start_date, end_date, on_hold FROM finance.billing_plan_enrolment \
         WHERE tenant_id = $1 AND plan_id = ANY($2) ORDER BY student_id, plan_id",
    )
    .bind(t.as_uuid())
    .bind(&ids)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut by_student: BTreeMap<Uuid, Vec<Enrolled>> = BTreeMap::new();
    for (sid, pid, amount, start, end, hold) in rows {
        by_student.entry(sid).or_default().push(Enrolled {
            plan_id: pid,
            amount,
            start,
            end,
            on_hold: hold,
        });
    }
    let mut students = Vec::with_capacity(by_student.len());
    for (sid, plans) in by_student {
        let active = st
            .students
            .live_context(scope, StudentId(sid), inst)
            .await?
            .is_some_and(|c| c.status == StudentContextStatus::Active);
        students.push(Student {
            id: sid,
            active,
            start: None,
            plans,
        });
    }
    Ok(Roster {
        plans,
        fee_heads,
        students,
    })
}

/// Students billed for the period by a charge that still stands.
async fn billed(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    freq: Frequency,
    key: &str,
) -> Result<HashSet<Uuid>, KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT i.student_id FROM finance.billing_run_item i LEFT JOIN finance.charge c ON c.id = i.charge_id \
         WHERE i.tenant_id = $1 AND i.institution_id = $2 AND i.frequency = $3 AND i.period_key = $4 \
         AND i.status = 'billed' AND (c.id IS NULL OR c.status <> 'cancelled')",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(freq.as_str())
    .bind(key)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(ids.into_iter().collect())
}

fn line_views(d: &Draft) -> Vec<BillingLineView> {
    d.lines
        .iter()
        .map(|l| BillingLineView {
            plan_id: l.plan_id,
            description: l.description.clone(),
            amount: fmt(l.amount),
            prorated_days: l.prorated.map(|p| p.0),
            period_days: l.prorated.map(|p| p.1),
        })
        .collect()
}

/// A run's parameters, checked.
pub struct RunInput {
    pub frequency: Frequency,
    pub day: Option<NaiveDate>,
    pub due_days: i64,
    pub week_start: u32,
    pub excluded: HashSet<Uuid>,
    /// `manual | schedule`.
    pub trigger: &'static str,
}

impl RunInput {
    fn from_request(b: &BillingRunRequest) -> Result<RunInput, KernelError> {
        let due_days = b.due_days.unwrap_or(7);
        if !(0..=365).contains(&due_days) {
            return Err(invalid("dueDays", "out_of_range", "must be 0–365"));
        }
        let week_start = b.week_start.unwrap_or(1);
        if week_start > 6 {
            return Err(invalid("weekStart", "out_of_range", "must be 0 (Sunday) to 6 (Saturday)"));
        }
        Ok(RunInput {
            frequency: frequency("frequency", &b.frequency)?,
            day: b.day,
            due_days,
            week_start,
            excluded: b.exclude_student_ids.iter().copied().collect(),
            trigger: "manual",
        })
    }
}

/// Plans (and, unless `preview`, performs) a billing run.
pub async fn execute(
    st: &FinanceState,
    scope: &ExecutionScope,
    inst: InstitutionId,
    input: &RunInput,
    preview: bool,
    a: &Actor,
) -> Result<BillingRunView, KernelError> {
    let t = scope.tenant_id;
    let today = business_today(st, scope, inst).await?;
    let day = input.day.unwrap_or(today);
    if day > today + Duration::days(62) {
        return Err(invalid("day", "too_far_ahead", "may be at most 62 days ahead"));
    }
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let fy = fy_start_month(&mut conn, t, inst, day).await?;
    let period = period_for(input.frequency, day, input.week_start, fy);
    let done = billed(&mut conn, t, inst, input.frequency, &period.key).await?;
    drop(conn);
    let r = roster(st, scope, t, inst, input.frequency).await?;
    let opts = RunOptions {
        frequency: input.frequency,
        day,
        due_days: input.due_days,
        week_start: input.week_start,
        fy_start_month: fy,
    };
    let plan = plan_billing_run(&r.students, &r.plans, &done, &input.excluded, &opts);
    let mut ids: Vec<Uuid> = plan.drafts.iter().map(|d| d.student_id).collect();
    ids.extend(plan.skipped.iter().map(|s| s.student_id));
    let names = student_names(st, scope, t, inst, &ids).await?;
    let name = |id: &Uuid| names.get(id).cloned().unwrap_or_default();
    let mut skips: Vec<BillingRunSkipView> = plan
        .skipped
        .iter()
        .map(|s| BillingRunSkipView {
            student_id: s.student_id,
            student_name: name(&s.student_id).0,
            reason: s.reason.as_str().to_string(),
        })
        .collect();
    let run_id = if preview {
        None
    } else {
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO finance.billing_run (tenant_id, institution_id, frequency, period_key, period_start, period_end, \
             period_label, run_date, due_days, trigger, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(input.frequency.as_str())
        .bind(&period.key)
        .bind(period.start)
        .bind(period.end)
        .bind(&period.label)
        .bind(today)
        .bind(i32::try_from(input.due_days).unwrap_or(7))
        .bind(input.trigger)
        .bind(a.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Some(id)
    };
    let mut items = Vec::new();
    let (mut billed_n, mut failed, mut total) = (0_i64, 0_i64, Decimal::ZERO);
    for d in &plan.drafts {
        let (student_name, student_number) = name(&d.student_id);
        let amount = d.total();
        if amount <= Decimal::ZERO {
            skips.push(BillingRunSkipView {
                student_id: d.student_id,
                student_name,
                reason: "nothing_to_bill".into(),
            });
            continue;
        }
        let mut item = BillingRunItemView {
            student_id: d.student_id,
            student_name,
            student_number,
            due_date: d.due_date,
            total: fmt(amount),
            lines: line_views(d),
            charge_id: None,
            charge_number: None,
            error: None,
        };
        if let Some(run) = run_id {
            match bill_one(st, scope, inst, run, &period, d, &r.fee_heads, today, a).await {
                Ok(Some((cid, number))) => {
                    item.charge_id = Some(cid);
                    item.charge_number = Some(number);
                }
                Ok(None) => {
                    skips.push(BillingRunSkipView {
                        student_id: d.student_id,
                        student_name: item.student_name.clone(),
                        reason: SkipReason::AlreadyBilled.as_str().into(),
                    });
                    continue;
                }
                Err(e) => {
                    failed += 1;
                    item.error = Some(match e {
                        KernelError::Validation(v) => v
                            .iter()
                            .map(|f| f.message.clone())
                            .collect::<Vec<_>>()
                            .join("; "),
                        KernelError::Conflict(m) => m,
                        other => {
                            tracing::warn!(error = %other, student = %d.student_id, "billing run: student not billed");
                            "could not be billed".into()
                        }
                    });
                    items.push(item);
                    continue;
                }
            }
        }
        billed_n += 1;
        total += amount;
        items.push(item);
    }
    let view = BillingRunView {
        id: run_id,
        preview,
        frequency: input.frequency.as_str().into(),
        period_key: period.key.clone(),
        period_label: period.label.clone(),
        period_start: period.start,
        period_end: period.end,
        run_date: today,
        due_days: input.due_days,
        trigger: input.trigger.into(),
        billed: billed_n,
        skipped: count(skips.len()),
        failed,
        total: fmt(total),
        items,
        skips,
    };
    if let Some(run) = run_id {
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        sqlx::query(
            "UPDATE finance.billing_run SET billed_count = $3, skipped_count = $4, failed_count = $5, total = $6 \
             WHERE tenant_id = $1 AND id = $2",
        )
        .bind(t.as_uuid())
        .bind(run)
        .bind(i32::try_from(view.billed).unwrap_or(i32::MAX))
        .bind(i32::try_from(view.skipped).unwrap_or(i32::MAX))
        .bind(i32::try_from(view.failed).unwrap_or(i32::MAX))
        .bind(total)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if input.trigger == "schedule" {
            sqlx::query(
                "UPDATE finance.billing_schedule SET last_period_key = $4, last_run_id = $5 \
                 WHERE tenant_id = $1 AND institution_id = $2 AND frequency = $3",
            )
            .bind(t.as_uuid())
            .bind(inst.as_uuid())
            .bind(input.frequency.as_str())
            .bind(&period.key)
            .bind(run)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        audit(
            &mut tx,
            scope,
            "finance.billing_run.run",
            "billing_run",
            run.to_string(),
            json!({"frequency": view.frequency, "periodKey": view.period_key, "billed": view.billed,
                   "skipped": view.skipped, "failed": view.failed, "total": view.total, "trigger": view.trigger}),
            a,
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
    }
    Ok(view)
}

/// Bills one student in their own transaction; `None` when the period turned out billed.
#[allow(clippy::too_many_arguments)]
async fn bill_one(
    st: &FinanceState,
    scope: &ExecutionScope,
    inst: InstitutionId,
    run: Uuid,
    period: &Period,
    d: &Draft,
    fee_heads: &HashMap<Uuid, Uuid>,
    today: NaiveDate,
    a: &Actor,
) -> Result<Option<(Uuid, String)>, KernelError> {
    let t = scope.tenant_id;
    let freq = period.frequency.as_str();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 57))")
        .bind(format!("{t}:{inst}:{}:{freq}:{}", d.student_id, period.key))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    // A cancelled charge frees its period.
    sqlx::query(
        "UPDATE finance.billing_run_item i SET status = 'void' FROM finance.charge c \
         WHERE c.id = i.charge_id AND c.status = 'cancelled' AND i.tenant_id = $1 AND i.institution_id = $2 \
         AND i.student_id = $3 AND i.frequency = $4 AND i.period_key = $5 AND i.status = 'billed'",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(d.student_id)
    .bind(freq)
    .bind(&period.key)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM finance.billing_run_item WHERE tenant_id = $1 AND institution_id = $2 \
         AND student_id = $3 AND frequency = $4 AND period_key = $5 AND status = 'billed')",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(d.student_id)
    .bind(freq)
    .bind(&period.key)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if taken {
        return Ok(None);
    }
    let lines_json = serde_json::to_value(line_views(d)).map_err(KernelError::internal)?;
    let amount = d.total();
    let item: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.billing_run_item (tenant_id, institution_id, run_id, student_id, frequency, period_key, \
         amount, lines) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(run)
    .bind(d.student_id)
    .bind(freq)
    .bind(&period.key)
    .bind(amount)
    .bind(&lines_json)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut lines = Vec::new();
    for l in d.lines.iter().filter(|l| l.amount > Decimal::ZERO) {
        let head = fee_heads
            .get(&l.plan_id)
            .copied()
            .ok_or_else(|| KernelError::internal_msg("billing plan without a fee head"))?;
        lines.push(line(head, l.description.clone(), l.amount));
    }
    let description = format!("Fees for {}", period.label);
    let draft = create_in_tx(
        st,
        &mut tx,
        scope,
        inst,
        &ChargeInput {
            student_id: StudentId(d.student_id),
            description,
            due_date: Some(d.due_date),
            academic_year_id: None,
            academic_term_id: None,
            source_type: ChargeSourceType::BillingRun,
            source_id: Some(item),
            lines,
        },
        a,
    )
    .await?;
    let validated = validate_locked(&mut tx, &draft, a).await?;
    let (published, _) = publish_locked(&mut tx, &validated, today, None, a).await?;
    sqlx::query("UPDATE finance.billing_run_item SET charge_id = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(t.as_uuid())
        .bind(item)
        .bind(published.id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Some((published.id.as_uuid(), published.charge_number)))
}

async fn run(
    st: &FinanceState,
    auth: &Authenticated,
    id: Uuid,
    b: &BillingRunRequest,
    preview: bool,
) -> ApiResult<BillingRunView> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    if preview {
        require_institution(st, &scope, inst).await?;
    } else {
        require_institution_writable(st, &scope, inst).await?;
    }
    let input = RunInput::from_request(b)?;
    Ok(execute(st, &scope, inst, &input, preview, &actor(ctx)).await?)
}

/// What a billing run would charge, without charging (`charge.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/billing-runs/preview", operation_id = "finance_charge_billing_run_preview", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), request_body = BillingRunRequest, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = BillingRunView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_run_preview(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<BillingRunRequest>,
) -> ApiResult<Json<BillingRunView>> {
    Ok(Json(run(&st, &auth, id, &b, true).await?))
}

/// Runs billing for a period (`charge.manage`): one published charge per student not yet billed
/// for it. Safe to repeat.
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/billing-runs", operation_id = "finance_charge_billing_run_create", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), request_body = BillingRunRequest, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = BillingRunView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_run_create(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<BillingRunRequest>,
) -> ApiResult<Json<BillingRunView>> {
    Ok(Json(run(&st, &auth, id, &b, false).await?))
}

const RUN_SELECT: &str = "SELECT id, frequency, period_key, period_label, run_date, trigger, billed_count, \
     skipped_count, failed_count, total, created_by, created_at FROM finance.billing_run";

fn run_summary(r: &sqlx::postgres::PgRow) -> Result<BillingRunSummary, KernelError> {
    let g = |e| map_db_error(e);
    Ok(BillingRunSummary {
        id: r.try_get("id").map_err(g)?,
        frequency: r.try_get("frequency").map_err(g)?,
        period_key: r.try_get("period_key").map_err(g)?,
        period_label: r.try_get("period_label").map_err(g)?,
        run_date: r.try_get("run_date").map_err(g)?,
        trigger: r.try_get("trigger").map_err(g)?,
        billed: i64::from(r.try_get::<i32, _>("billed_count").map_err(g)?),
        skipped: i64::from(r.try_get::<i32, _>("skipped_count").map_err(g)?),
        failed: i64::from(r.try_get::<i32, _>("failed_count").map_err(g)?),
        total: fmt(r.try_get("total").map_err(g)?),
        created_by: r.try_get("created_by").map_err(g)?,
        created_at: r.try_get("created_at").map_err(g)?,
    })
}

/// Recent billing runs, newest first (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/billing-runs", operation_id = "finance_charge_billing_run_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BillingRunList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_run_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BillingRunList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let rows = sqlx::query(&format!(
        "{RUN_SELECT} WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at DESC LIMIT 100"
    ))
    .bind(tenant(ctx).as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows.iter().map(run_summary).collect::<Result<_, _>>()?;
    Ok(Json(BillingRunList { items }))
}

/// One billing run with the charges it raised (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/billing-runs/{id}", operation_id = "finance_charge_billing_run_get", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BillingRunDetail), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_run_get(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BillingRunDetail>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let t = tenant(ctx);
    let row = sqlx::query(
        "SELECT id, institution_id, frequency, period_key, period_label, run_date, trigger, billed_count, \
         skipped_count, failed_count, total, created_by, created_at FROM finance.billing_run WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "billing_run",
    })?;
    let inst = InstitutionId(row.try_get("institution_id").map_err(map_db_error)?);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let summary = run_summary(&row)?;
    let rows = sqlx::query(
        "SELECT i.student_id, i.amount, i.lines, i.charge_id, c.charge_number, c.due_date FROM finance.billing_run_item i \
         LEFT JOIN finance.charge c ON c.id = i.charge_id WHERE i.tenant_id = $1 AND i.run_id = $2 ORDER BY i.created_at",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let ids: Vec<Uuid> = rows
        .iter()
        .filter_map(|r| r.try_get::<Uuid, _>("student_id").ok())
        .collect();
    let names = student_names(&st, &scope, t, inst, &ids).await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        let sid: Uuid = r.try_get("student_id").map_err(map_db_error)?;
        let (student_name, student_number) = names.get(&sid).cloned().unwrap_or_default();
        let lines: Vec<serde_json::Value> = serde_json::from_value(r.try_get("lines").map_err(map_db_error)?)
            .unwrap_or_default();
        let lines = lines
            .into_iter()
            .filter_map(|l| {
                Some(BillingLineView {
                    plan_id: l.get("planId")?.as_str()?.parse().ok()?,
                    description: l.get("description")?.as_str()?.to_string(),
                    amount: l.get("amount")?.as_str()?.to_string(),
                    prorated_days: l.get("proratedDays").and_then(serde_json::Value::as_i64),
                    period_days: l.get("periodDays").and_then(serde_json::Value::as_i64),
                })
            })
            .collect();
        items.push(BillingRunItemView {
            student_id: sid,
            student_name,
            student_number,
            due_date: r
                .try_get::<Option<NaiveDate>, _>("due_date")
                .map_err(map_db_error)?
                .unwrap_or(summary.run_date),
            total: fmt(r.try_get("amount").map_err(map_db_error)?),
            lines,
            charge_id: r.try_get("charge_id").map_err(map_db_error)?,
            charge_number: r.try_get("charge_number").map_err(map_db_error)?,
            error: None,
        });
    }
    Ok(Json(BillingRunDetail { run: summary, items }))
}

// ----------------------------------------------------------------------------- schedules

async fn schedule_views(
    st: &FinanceState,
    scope: &ExecutionScope,
    t: TenantId,
    inst: InstitutionId,
) -> Result<Vec<BillingScheduleView>, KernelError> {
    let today = business_today(st, scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let fy = fy_start_month(&mut conn, t, inst, today).await?;
    let rows: Vec<(String, bool, i32, i32, Option<String>, Option<Uuid>)> = sqlx::query_as(
        "SELECT frequency, enabled, run_day, due_days, last_period_key, last_run_id FROM finance.billing_schedule \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Frequency::ALL
        .into_iter()
        .map(|f| {
            let current = period_for(f, today, 1, fy).label;
            match rows.iter().find(|r| r.0 == f.as_str()) {
                Some((_, enabled, run_day, due_days, last, last_run)) => BillingScheduleView {
                    frequency: f.as_str().into(),
                    enabled: *enabled,
                    run_day: *run_day,
                    due_days: *due_days,
                    last_period_key: last.clone(),
                    last_run_id: *last_run,
                    current_period_label: current,
                },
                None => BillingScheduleView {
                    frequency: f.as_str().into(),
                    enabled: false,
                    run_day: 1,
                    due_days: 7,
                    last_period_key: None,
                    last_run_id: None,
                    current_period_label: current,
                },
            }
        })
        .collect())
}

/// The automatic run of each frequency (`finance.read`).
#[utoipa::path(get, path = "/api/v1/finance/institutions/{id}/billing-schedules", operation_id = "finance_charge_billing_schedule_list", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = BillingScheduleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_schedule_list(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<BillingScheduleList>> {
    let ctx = auth.require(Permission::FinanceRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let items = schedule_views(&st, &scope, tenant(ctx), inst).await?;
    Ok(Json(BillingScheduleList { items }))
}

/// Turns a frequency's automatic run on or off (`charge.manage`).
#[utoipa::path(put, path = "/api/v1/finance/institutions/{id}/billing-schedules/{frequency}", operation_id = "finance_charge_billing_schedule_set", tag = "finance-charges",
    params(("id" = String, Path, format = Uuid), ("frequency" = String, Path)), request_body = BillingScheduleUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = BillingScheduleView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn billing_schedule_set(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path((id, freq)): Path<(Uuid, String)>,
    Json(b): Json<BillingScheduleUpdate>,
) -> ApiResult<Json<BillingScheduleView>> {
    let ctx = auth.require(Permission::ChargeManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let f = frequency("frequency", &freq)?;
    if !(1..=31).contains(&b.run_day) {
        return Err(invalid("runDay", "out_of_range", "must be 1–31").into());
    }
    if !(0..=365).contains(&b.due_days) {
        return Err(invalid("dueDays", "out_of_range", "must be 0–365").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sid: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.billing_schedule (tenant_id, institution_id, frequency, enabled, run_day, due_days, updated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) ON CONFLICT (tenant_id, institution_id, frequency) DO UPDATE SET \
         enabled = EXCLUDED.enabled, run_day = EXCLUDED.run_day, due_days = EXCLUDED.due_days, \
         updated_by = EXCLUDED.updated_by, updated_at = now() RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(f.as_str())
    .bind(b.enabled)
    .bind(b.run_day)
    .bind(b.due_days)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "finance.billing_schedule.set",
        "billing_schedule",
        sid.to_string(),
        json!({"frequency": f.as_str(), "enabled": b.enabled, "runDay": b.run_day, "dueDays": b.due_days}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let view = schedule_views(&st, &scope, t, inst)
        .await?
        .into_iter()
        .find(|v| v.frequency == f.as_str())
        .ok_or_else(|| KernelError::internal_msg("schedule vanished"))?;
    Ok(Json(view))
}

// ----------------------------------------------------------------------------- schedule job

/// Runs the enabled billing schedules that are due (one replica at a time, under the sweeper
/// lock). Each institution's run happens in its own tenant context as `system:billing-schedule`.
pub struct BillingScheduleJob {
    pub state: FinanceState,
    pub interval: std::time::Duration,
}

impl BillingScheduleJob {
    async fn run_due(&self, rows: Vec<(Uuid, Uuid, String, i32, i32, Option<String>)>) -> u64 {
        let mut ran = 0;
        for (tenant_id, inst_id, freq, run_day, due_days, last) in rows {
            let Some(f) = Frequency::parse(&freq) else { continue };
            let scope = ExecutionScope::institution(TenantId(tenant_id), InstitutionId(inst_id));
            let st = self.state.clone();
            let outcome = sutra_kernel::db_context::scoped(scope, async move {
                let today = business_today(&st, &scope, InstitutionId(inst_id)).await?;
                let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
                let fy = fy_start_month(&mut conn, TenantId(tenant_id), InstitutionId(inst_id), today).await?;
                drop(conn);
                let run_day = u32::try_from(run_day).unwrap_or(1);
                if scheduled_period(f, today, run_day, last.as_deref(), 1, fy).is_none() {
                    return Ok::<bool, KernelError>(false);
                }
                let input = RunInput {
                    frequency: f,
                    day: Some(today),
                    due_days: i64::from(due_days),
                    week_start: 1,
                    excluded: HashSet::new(),
                    trigger: "schedule",
                };
                let a = Actor {
                    id: Some(SCHEDULE_ACTOR.to_string()),
                    correlation_id: None,
                };
                execute(&st, &scope, InstitutionId(inst_id), &input, false, &a).await?;
                Ok(true)
            })
            .await;
            match outcome {
                Ok(true) => ran += 1,
                Ok(false) => {}
                Err(e) => tracing::warn!(error = %e, tenant = %tenant_id, institution = %inst_id, frequency = %freq,
                    "scheduled billing run failed"),
            }
        }
        ran
    }
}

#[async_trait::async_trait]
impl sutra_persistence::jobs::SweepJob for BillingScheduleJob {
    fn name(&self) -> &'static str {
        "finance_billing_schedule"
    }

    fn interval(&self) -> std::time::Duration {
        self.interval
    }

    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let rows: Vec<(Uuid, Uuid, String, i32, i32, Option<String>)> = sqlx::query_as(
            "SELECT tenant_id, institution_id, frequency, run_day, due_days, last_period_key \
             FROM finance.billing_schedule WHERE enabled ORDER BY tenant_id, institution_id, frequency",
        )
        .fetch_all(&mut *conn)
        .await?;
        Ok(self.run_due(rows).await)
    }
}
