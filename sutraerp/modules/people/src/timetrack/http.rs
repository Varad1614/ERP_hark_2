//! **ERP_EXTENSIONS 5C** — staff time tracking against research projects and sponsored grants
//! (DST, SERB, UGC, ICSSR, AICTE …), internal projects and committees. Maths in
//! [`super::logic`], ported from HeyPuter/everything time-tracking (MIT). Operation ids
//! `people_time_*`, tag `people-time`.
//!
//! Staff log time (timer or manual: `1h30`, `1.5`, `90m`) on the projects they belong to; an
//! entry with clock times may not overlap another of theirs (409). A week of one project is
//! submitted as a timesheet, decided by the project's PI or the staff member's reporting
//! manager (or an administrator with `people.timesheet.decide`), never by the person who
//! logged it, and is locked once submitted. Effort reports give hours, % effort and
//! person-months per staff per project per month, costed at hours × hourly rate: the staff
//! member's own effective-dated rate, else gross salary from the last finalised payroll ÷ the
//! institution's standard monthly hours, else the institution default.
#![allow(
    clippy::type_complexity,
    clippy::too_many_lines,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss
)]

use super::logic::{self as lg, Budget, RatePeriod, RateValue, Tracked};
use crate::leave::common::{audit, ensure_version, expected, invalid};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_staff;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::PeopleState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, NaiveDate, NaiveDateTime};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, HashMap, HashSet};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const KINDS: [&str; 5] = [
    "sponsored_research",
    "consultancy",
    "internal_project",
    "committee",
    "other",
];
const ROLES: [&str; 8] = [
    "pi",
    "co_pi",
    "member",
    "jrf",
    "srf",
    "research_associate",
    "project_assistant",
    "technical_staff",
];
const ROUNDINGS: [i64; 6] = [0, 5, 6, 10, 15, 30];
const MINIMUMS: [i64; 6] = [0, 6, 10, 15, 30, 60];

fn ds(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn pd(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn money(x: f64) -> Decimal {
    Decimal::from_f64(x).unwrap_or_default().round_dp(2)
}

fn get<'r, T: sqlx::Decode<'r, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>>(
    r: &'r PgRow,
    c: &str,
) -> Result<T, KernelError> {
    r.try_get(c).map_err(map_db_error)
}

fn text(s: Option<&String>, max: usize, field: &str) -> Result<Option<String>, KernelError> {
    let v = s.map(|x| x.trim().to_owned()).filter(|x| !x.is_empty());
    if v.as_ref().is_some_and(|x| x.chars().count() > max) {
        return Err(invalid(
            field,
            "too_long",
            &format!("at most {max} characters"),
        ));
    }
    Ok(v)
}

fn dec_field(raw: Option<&String>, field: &str) -> Result<Option<Decimal>, KernelError> {
    match raw.map(|s| s.trim()).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => {
            let v = crate::shared::decimal::parse_decimal(field, s)?;
            if v < Decimal::ZERO {
                return Err(invalid(field, "negative", "must not be negative"));
            }
            Ok(Some(v))
        }
    }
}

/// The first of `ps` the caller holds (checked before any data is read), else 403.
fn require_any<'a>(
    auth: &'a Authenticated,
    ps: &[Permission],
) -> Result<&'a AuthContext, KernelError> {
    for p in ps {
        if auth.0.has_permission(*p) {
            auth.0.require(*p)?;
            return Ok(&auth.0);
        }
    }
    Err(KernelError::Forbidden(format!(
        "missing permission `{}`",
        ps.first().map_or("", |p| p.as_str())
    )))
}

fn csv_response(name: &str, body: String) -> Result<Response, KernelError> {
    let disposition = HeaderValue::from_str(&format!("attachment; filename=\"{name}\""))
        .map_err(|e| KernelError::internal_msg(format!("file name header: {e}")))?;
    let mut out = (StatusCode::OK, body).into_response();
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

// ------------------------------------------------------------------ settings

#[derive(Debug, Clone)]
struct Settings {
    week_start: u32,
    rounding: i64,
    standard_monthly_hours: f64,
    default_rate: Option<RateValue>,
    timer_cap_hours: f64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            week_start: 1,
            rounding: 0,
            standard_monthly_hours: 176.0,
            default_rate: None,
            timer_cap_hours: 12.0,
        }
    }
}

async fn load_settings(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<Settings, KernelError> {
    let row: Option<(i16, i32, f64, Option<Value>, f64)> = sqlx::query_as(
        "SELECT week_start, rounding_minutes, standard_monthly_hours::float8, default_hourly_rate, \
         timer_cap_hours::float8 FROM people.time_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(
        row.map_or_else(Settings::default, |(w, r, h, d, c)| Settings {
            week_start: u32::from(w == 1),
            rounding: i64::from(r),
            standard_monthly_hours: h,
            default_rate: d.and_then(|x| serde_json::from_value(x).ok()),
            timer_cap_hours: c,
        }),
    )
}

/// Time-tracking settings of an institution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeSettings {
    /// 1 = weeks start on Monday (default), 0 = Sunday.
    pub week_start: u32,
    /// Default rounding of chargeable time, minutes, always up (0, 5, 6, 10, 15, 30).
    pub rounding_minutes: i64,
    /// Working hours in a month for salary-derived hourly rates (default 176 = 22 days × 8 h).
    pub standard_monthly_hours: f64,
    /// Fallback hourly rate in ₹ (a number or effective-dated periods); null = none.
    pub default_hourly_rate: Option<RateValue>,
    /// A running timer is trimmed to this many hours when stopped (default 12).
    pub timer_cap_hours: f64,
}

fn settings_view(s: &Settings) -> TimeSettings {
    TimeSettings {
        week_start: s.week_start,
        rounding_minutes: s.rounding,
        standard_monthly_hours: s.standard_monthly_hours,
        default_hourly_rate: s.default_rate.clone(),
        timer_cap_hours: s.timer_cap_hours,
    }
}

/// Time-tracking settings (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/time-settings", operation_id = "people_time_settings_get", tag = "people-time",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TimeSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TimeSettings>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = load_settings(&mut conn, ctx.scope.tenant_id, id).await?;
    Ok(Json(settings_view(&s)))
}

/// Save time-tracking settings (`people.time.manage`).
#[utoipa::path(put, path = "/api/v1/people/institutions/{id}/time-settings", operation_id = "people_time_settings_put", tag = "people-time",
    params(("id" = String, Path, format = Uuid)), request_body = TimeSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimeSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<TimeSettings>,
) -> ApiResult<Json<TimeSettings>> {
    let ctx = auth.require(Permission::TimeTrackingManage)?;
    if b.week_start > 1 {
        return Err(invalid("weekStart", "invalid", "1 (Monday) or 0 (Sunday)").into());
    }
    if !ROUNDINGS.contains(&b.rounding_minutes) {
        return Err(invalid("roundingMinutes", "invalid", "one of 0, 5, 6, 10, 15, 30").into());
    }
    if !(b.standard_monthly_hours >= 1.0 && b.standard_monthly_hours <= 400.0) {
        return Err(invalid("standardMonthlyHours", "invalid", "between 1 and 400 hours").into());
    }
    if !(b.timer_cap_hours >= 1.0 && b.timer_cap_hours <= 24.0) {
        return Err(invalid("timerCapHours", "invalid", "between 1 and 24 hours").into());
    }
    check_rate(b.default_hourly_rate.as_ref(), "defaultHourlyRate")?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.time_settings (tenant_id, institution_id, week_start, rounding_minutes, \
         standard_monthly_hours, default_hourly_rate, timer_cap_hours) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET week_start = EXCLUDED.week_start, \
         rounding_minutes = EXCLUDED.rounding_minutes, standard_monthly_hours = EXCLUDED.standard_monthly_hours, \
         default_hourly_rate = EXCLUDED.default_hourly_rate, timer_cap_hours = EXCLUDED.timer_cap_hours, \
         version = people.time_settings.version + 1, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.week_start as i16)
    .bind(b.rounding_minutes as i32)
    .bind(b.standard_monthly_hours)
    .bind(b.default_hourly_rate.as_ref().map(|r| json!(r)))
    .bind(b.timer_cap_hours)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.time_settings.update",
        "institution",
        id,
        json!(b),
        &actor(ctx),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(b))
}

fn check_rate(r: Option<&RateValue>, field: &str) -> Result<(), KernelError> {
    let ok = |x: f64| x.is_finite() && (0.0..=1_000_000.0).contains(&x);
    let good = match r {
        None => true,
        Some(RateValue::Plain(x)) => ok(*x),
        Some(RateValue::Periods(l)) => {
            l.len() <= 50
                && l.iter().all(|p| {
                    p.rate.is_none_or(ok) && p.from.as_deref().is_none_or(|f| pd(f).is_some())
                })
        }
    };
    if good {
        Ok(())
    } else {
        Err(invalid(
            field,
            "invalid",
            "a rate in ₹ per hour, or periods { from: date, rate }",
        ))
    }
}

// ------------------------------------------------------------------ projects

#[derive(Debug, Clone)]
struct Proj {
    id: Uuid,
    institution_id: Uuid,
    code: String,
    name: String,
    kind: String,
    funding_agency: Option<String>,
    scheme: Option<String>,
    sanction_number: Option<String>,
    sanction_date: Option<NaiveDate>,
    sanctioned_amount: Option<Decimal>,
    manpower_budget: Option<Decimal>,
    start_date: Option<NaiveDate>,
    end_date: Option<NaiveDate>,
    pi_staff_id: Option<Uuid>,
    cost_centre: Option<String>,
    contract_id: Option<Uuid>,
    budget_kind: Option<String>,
    budget_period: Option<String>,
    budget_value: Option<Decimal>,
    rounding: Option<i64>,
    minimum: Option<i64>,
    billable_default: bool,
    open_to_all: bool,
    status: String,
    notes: Option<String>,
    version: i64,
}

const PROJ_COLS: &str = "id, institution_id, code, name, kind, funding_agency, scheme, sanction_number, sanction_date, \
     sanctioned_amount, manpower_budget, start_date, end_date, pi_staff_id, cost_centre, contract_id, budget_kind, \
     budget_period, budget_value, rounding_minutes, minimum_minutes, billable_default, open_to_all, status, notes, version";

fn proj_of(r: &PgRow) -> Result<Proj, KernelError> {
    Ok(Proj {
        id: get(r, "id")?,
        institution_id: get(r, "institution_id")?,
        code: get(r, "code")?,
        name: get(r, "name")?,
        kind: get(r, "kind")?,
        funding_agency: get(r, "funding_agency")?,
        scheme: get(r, "scheme")?,
        sanction_number: get(r, "sanction_number")?,
        sanction_date: get(r, "sanction_date")?,
        sanctioned_amount: get(r, "sanctioned_amount")?,
        manpower_budget: get(r, "manpower_budget")?,
        start_date: get(r, "start_date")?,
        end_date: get(r, "end_date")?,
        pi_staff_id: get(r, "pi_staff_id")?,
        cost_centre: get(r, "cost_centre")?,
        contract_id: get(r, "contract_id")?,
        budget_kind: get(r, "budget_kind")?,
        budget_period: get(r, "budget_period")?,
        budget_value: get(r, "budget_value")?,
        rounding: get::<Option<i32>>(r, "rounding_minutes")?.map(i64::from),
        minimum: get::<Option<i32>>(r, "minimum_minutes")?.map(i64::from),
        billable_default: get(r, "billable_default")?,
        open_to_all: get(r, "open_to_all")?,
        status: get(r, "status")?,
        notes: get(r, "notes")?,
        version: get(r, "version")?,
    })
}

async fn project(conn: &mut PgConnection, t: TenantId, id: Uuid) -> Result<Proj, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {PROJ_COLS} FROM people.time_project WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "time project",
    })?;
    proj_of(&row)
}

async fn projects_of(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    status: Option<&str>,
) -> Result<Vec<Proj>, KernelError> {
    sqlx::query(&format!(
        "SELECT {PROJ_COLS} FROM people.time_project WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY status, code"
    ))
    .bind(t.as_uuid())
    .bind(inst)
    .bind(status)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(proj_of)
    .collect()
}

/// Projects a staff member may log time on: active, and open to all or they are a member / PI.
async fn loggable(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    staff: Uuid,
) -> Result<Vec<Proj>, KernelError> {
    sqlx::query(&format!(
        "SELECT {PROJ_COLS} FROM people.time_project p WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'active' AND (open_to_all OR pi_staff_id = $3 OR EXISTS (SELECT 1 FROM \
         people.time_project_member m WHERE m.project_id = p.id AND m.staff_id = $3)) ORDER BY code"
    ))
    .bind(t.as_uuid())
    .bind(inst)
    .bind(staff)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(proj_of)
    .collect()
}

/// A member of a project.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeProjectMember {
    pub staff_id: Uuid,
    pub name: String,
    pub staff_number: String,
    /// `pi | co_pi | member | jrf | srf | research_associate | project_assistant | technical_staff`.
    pub role: String,
    /// Effort committed in the sanction (% of time), if any.
    pub commitment_percent: Option<f64>,
}

async fn members_of(
    conn: &mut PgConnection,
    t: TenantId,
    ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<TimeProjectMember>>, KernelError> {
    let rows: Vec<(Uuid, Uuid, String, String, String, Option<f64>)> = sqlx::query_as(
        "SELECT m.project_id, m.staff_id, s.display_name, s.staff_number, m.role, m.commitment_percent::float8 \
         FROM people.time_project_member m JOIN people.staff_member s ON s.id = m.staff_id \
         WHERE m.tenant_id = $1 AND m.project_id = ANY($2) ORDER BY m.role = 'pi' DESC, m.role = 'co_pi' DESC, s.display_name",
    )
    .bind(t.as_uuid())
    .bind(ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out: HashMap<Uuid, Vec<TimeProjectMember>> = HashMap::new();
    for (p, staff_id, name, staff_number, role, commitment_percent) in rows {
        out.entry(p).or_default().push(TimeProjectMember {
            staff_id,
            name,
            staff_number,
            role,
            commitment_percent,
        });
    }
    Ok(out)
}

/// A project's hours / ₹ budget.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeProjectBudget {
    /// `hours` | `amount` (₹ of chargeable time).
    pub kind: String,
    /// `total` | `monthly`.
    pub period: String,
    #[schema(value_type = String)]
    pub value: Decimal,
}

/// A trackable work item: a sponsored research project / grant, consultancy, internal project
/// or committee.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeProjectView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    /// `sponsored_research | consultancy | internal_project | committee | other`.
    pub kind: String,
    /// DST, SERB (ANRF), UGC, ICSSR, AICTE, DBT, CSIR, ICMR, industry…
    pub funding_agency: Option<String>,
    pub scheme: Option<String>,
    pub sanction_number: Option<String>,
    pub sanction_date: Option<NaiveDate>,
    #[schema(value_type = Option<String>)]
    pub sanctioned_amount: Option<Decimal>,
    /// The sanction's manpower (salaries) head, against which time cost is utilised.
    #[schema(value_type = Option<String>)]
    pub manpower_budget: Option<Decimal>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub pi_staff_id: Option<Uuid>,
    pub pi_name: Option<String>,
    /// Finance cost centre / fund code the project's cost is booked to (free text: Finance has
    /// no cost-centre master yet).
    pub cost_centre: Option<String>,
    /// The research MoU / sanction contract in Resources → Contracts, if linked.
    pub contract_id: Option<Uuid>,
    pub budget: Option<TimeProjectBudget>,
    pub budget_progress: Option<Budget>,
    pub rounding_minutes: Option<i64>,
    pub minimum_minutes: Option<i64>,
    pub billable_default: bool,
    pub open_to_all: bool,
    /// `active | closed | archived`.
    pub status: String,
    pub notes: Option<String>,
    pub members: Vec<TimeProjectMember>,
    /// Seconds logged on the project so far, by everyone.
    pub logged_seconds: i64,
    pub version: i64,
}

async fn project_views(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    ps: Vec<Proj>,
    today: &str,
) -> Result<Vec<TimeProjectView>, KernelError> {
    let ids: Vec<Uuid> = ps.iter().map(|p| p.id).collect();
    let mut members = members_of(conn, t, &ids).await?;
    let logged: HashMap<Uuid, i64> = sqlx::query_as::<_, (Uuid, i64)>(
        "SELECT project_id, COALESCE(sum(seconds), 0)::int8 FROM people.time_entry WHERE tenant_id = $1 \
         AND project_id = ANY($2) GROUP BY project_id",
    )
    .bind(t.as_uuid())
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .into_iter()
    .collect();
    let mut progress: HashMap<Uuid, Budget> = HashMap::new();
    let with_budget: Vec<&Proj> = ps.iter().filter(|p| p.budget_kind.is_some()).collect();
    if !with_budget.is_empty() {
        let costing = Costing::load(conn, t, inst).await?;
        for p in with_budget {
            let (kind, period) = (
                p.budget_kind.clone().unwrap_or_default(),
                p.budget_period.clone().unwrap_or_else(|| "total".into()),
            );
            let (from, to) = lg::budget_window(&period, today);
            let es = entries_where(
                conn,
                t,
                "e.project_id = $2 AND e.entry_date BETWEEN $3 AND $4",
                p.id,
                pd(&from),
                pd(&to),
            )
            .await?;
            let spent = if kind == "amount" {
                lg::round2(es.iter().map(|e| costing.price(e).2).sum())
            } else {
                es.iter().map(|e| e.seconds as f64).sum()
            };
            if let Some(b) = lg::budget_progress(
                &kind,
                &period,
                p.budget_value.and_then(|v| v.to_f64()).unwrap_or(0.0),
                spent,
            ) {
                progress.insert(p.id, b);
            }
        }
    }
    Ok(ps
        .into_iter()
        .map(|p| {
            let ms = members.remove(&p.id).unwrap_or_default();
            let pi_name = p
                .pi_staff_id
                .and_then(|pi| ms.iter().find(|m| m.staff_id == pi).map(|m| m.name.clone()));
            TimeProjectView {
                id: p.id,
                institution_id: p.institution_id,
                logged_seconds: logged.get(&p.id).copied().unwrap_or(0),
                budget_progress: progress.remove(&p.id),
                budget: match (&p.budget_kind, &p.budget_period, p.budget_value) {
                    (Some(k), Some(per), Some(v)) => Some(TimeProjectBudget {
                        kind: k.clone(),
                        period: per.clone(),
                        value: v,
                    }),
                    _ => None,
                },
                code: p.code,
                name: p.name,
                kind: p.kind,
                funding_agency: p.funding_agency,
                scheme: p.scheme,
                sanction_number: p.sanction_number,
                sanction_date: p.sanction_date,
                sanctioned_amount: p.sanctioned_amount,
                manpower_budget: p.manpower_budget,
                start_date: p.start_date,
                end_date: p.end_date,
                pi_staff_id: p.pi_staff_id,
                pi_name,
                cost_centre: p.cost_centre,
                contract_id: p.contract_id,
                rounding_minutes: p.rounding,
                minimum_minutes: p.minimum,
                billable_default: p.billable_default,
                open_to_all: p.open_to_all,
                status: p.status,
                notes: p.notes,
                members: ms,
                version: p.version,
            }
        })
        .collect())
}

/// One member in a project body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeProjectMemberInput {
    pub staff_id: Uuid,
    pub role: String,
    pub commitment_percent: Option<f64>,
}

/// Create / change a trackable project (money as decimal strings: `"2500000.00"`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeProjectInput {
    pub code: String,
    pub name: String,
    pub kind: String,
    pub funding_agency: Option<String>,
    pub scheme: Option<String>,
    pub sanction_number: Option<String>,
    pub sanction_date: Option<NaiveDate>,
    pub sanctioned_amount: Option<String>,
    pub manpower_budget: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub end_date: Option<NaiveDate>,
    pub pi_staff_id: Option<Uuid>,
    pub cost_centre: Option<String>,
    pub contract_id: Option<Uuid>,
    /// `hours` | `amount`; with `budgetPeriod` and `budgetValue`, or none of the three.
    pub budget_kind: Option<String>,
    pub budget_period: Option<String>,
    pub budget_value: Option<String>,
    pub rounding_minutes: Option<i64>,
    pub minimum_minutes: Option<i64>,
    pub billable_default: Option<bool>,
    pub open_to_all: Option<bool>,
    /// Update only: `active | closed | archived`.
    pub status: Option<String>,
    pub notes: Option<String>,
    #[serde(default)]
    pub members: Vec<TimeProjectMemberInput>,
}

struct CleanProject {
    code: String,
    name: String,
    funding_agency: Option<String>,
    scheme: Option<String>,
    sanction_number: Option<String>,
    sanctioned_amount: Option<Decimal>,
    manpower_budget: Option<Decimal>,
    cost_centre: Option<String>,
    budget_value: Option<Decimal>,
    notes: Option<String>,
    members: Vec<(Uuid, String, Option<f64>)>,
}

fn clean_project(b: &TimeProjectInput) -> Result<CleanProject, KernelError> {
    let code = b.code.trim().to_uppercase();
    let code_ok = !code.is_empty()
        && code.len() <= 40
        && code
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && code
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/_.-".contains(c));
    if !code_ok {
        return Err(invalid(
            "code",
            "invalid",
            "letters, digits and / _ . - (up to 40), e.g. SERB-CRG/2025/001",
        ));
    }
    let name = b.name.trim().to_owned();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(invalid(
            "name",
            "required",
            "a name of up to 200 characters",
        ));
    }
    if !KINDS.contains(&b.kind.as_str()) {
        return Err(invalid(
            "kind",
            "invalid",
            "sponsored_research, consultancy, internal_project, committee or other",
        ));
    }
    let funding_agency = text(b.funding_agency.as_ref(), 80, "fundingAgency")?;
    if b.kind == "sponsored_research"
        && (funding_agency.is_none() || b.start_date.is_none() || b.end_date.is_none())
    {
        return Err(invalid(
            "fundingAgency",
            "required",
            "a sponsored project needs its funding agency and sanctioned period",
        ));
    }
    if let (Some(a), Some(z)) = (b.start_date, b.end_date) {
        if z < a {
            return Err(invalid(
                "endDate",
                "before_start",
                "the project ends before it starts",
            ));
        }
    }
    let budget_value = dec_field(b.budget_value.as_ref(), "budgetValue")?;
    match (
        b.budget_kind.as_deref(),
        b.budget_period.as_deref(),
        budget_value,
    ) {
        (None, None, None) => {}
        (Some("hours" | "amount"), Some("total" | "monthly"), Some(v)) if v > Decimal::ZERO => {}
        _ => return Err(invalid(
            "budgetKind",
            "invalid",
            "a budget needs kind (hours | amount), period (total | monthly) and a positive value",
        )),
    }
    if b.rounding_minutes.is_some_and(|r| !ROUNDINGS.contains(&r)) {
        return Err(invalid(
            "roundingMinutes",
            "invalid",
            "one of 0, 5, 6, 10, 15, 30",
        ));
    }
    if b.minimum_minutes.is_some_and(|r| !MINIMUMS.contains(&r)) {
        return Err(invalid(
            "minimumMinutes",
            "invalid",
            "one of 0, 6, 10, 15, 30, 60",
        ));
    }
    let mut members: Vec<(Uuid, String, Option<f64>)> = Vec::new();
    for m in &b.members {
        if !ROLES.contains(&m.role.as_str()) {
            return Err(invalid("members", "invalid_role", "pi, co_pi, member, jrf, srf, research_associate, project_assistant or technical_staff"));
        }
        if m.commitment_percent
            .is_some_and(|c| !(0.0..=100.0).contains(&c))
        {
            return Err(invalid(
                "members",
                "invalid_commitment",
                "commitment is a percentage 0–100",
            ));
        }
        if members.iter().any(|x| x.0 == m.staff_id) {
            return Err(invalid(
                "members",
                "duplicate",
                "a staff member is listed twice",
            ));
        }
        members.push((m.staff_id, m.role.clone(), m.commitment_percent));
    }
    if let Some(pi) = b.pi_staff_id {
        match members.iter_mut().find(|m| m.0 == pi) {
            Some(m) => m.1 = "pi".into(),
            None => members.push((pi, "pi".into(), None)),
        }
    }
    Ok(CleanProject {
        code,
        name,
        funding_agency,
        scheme: text(b.scheme.as_ref(), 120, "scheme")?,
        sanction_number: text(b.sanction_number.as_ref(), 80, "sanctionNumber")?,
        sanctioned_amount: dec_field(b.sanctioned_amount.as_ref(), "sanctionedAmount")?,
        manpower_budget: dec_field(b.manpower_budget.as_ref(), "manpowerBudget")?,
        cost_centre: text(b.cost_centre.as_ref(), 40, "costCentre")?,
        budget_value,
        notes: text(b.notes.as_ref(), 2000, "notes")?,
        members,
    })
}

async fn check_staff(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    ids: &[Uuid],
) -> Result<(), KernelError> {
    let found: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM people.staff_member WHERE tenant_id = $1 AND institution_id = $2 AND id = ANY($3)",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if usize::try_from(found).unwrap_or(0) != ids.len() {
        return Err(invalid(
            "members",
            "unknown_staff",
            "every member must be a staff member of this institution",
        ));
    }
    Ok(())
}

async fn write_members(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    project_id: Uuid,
    members: &[(Uuid, String, Option<f64>)],
) -> Result<(), KernelError> {
    let ids: Vec<Uuid> = members.iter().map(|m| m.0).collect();
    check_staff(conn, t, inst, &ids).await?;
    sqlx::query("DELETE FROM people.time_project_member WHERE project_id = $1 AND tenant_id = $2")
        .bind(project_id)
        .bind(t.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    for (staff, role, c) in members {
        sqlx::query(
            "INSERT INTO people.time_project_member (project_id, staff_id, tenant_id, institution_id, role, commitment_percent) \
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(project_id)
        .bind(staff)
        .bind(t.as_uuid())
        .bind(inst)
        .bind(role)
        .bind(c)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

/// Status filter.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatusQuery {
    /// Only this status.
    pub status: Option<String>,
}

/// Projects of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimeProjectList {
    pub items: Vec<TimeProjectView>,
}

/// Trackable projects, grants and committees (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/time-projects", operation_id = "people_time_project_list", tag = "people-time",
    params(("id" = String, Path, format = Uuid), StatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = TimeProjectList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn project_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<TimeProjectList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = ds(business_today(&st, &scope, inst).await?);
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let ps = projects_of(&mut conn, t, id, q.status.as_deref()).await?;
    let items = project_views(&mut conn, t, id, ps, &today).await?;
    Ok(Json(TimeProjectList { items }))
}

async fn project_response(
    st: &PeopleState,
    ctx: &AuthContext,
    pid: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = project(&mut conn, t, pid).await?;
    let inst = InstitutionId(p.institution_id);
    let today = ds(business_today(st, &institution_scope(ctx, inst), inst).await?);
    let view = project_views(&mut conn, t, p.institution_id, vec![p], &today)
        .await?
        .pop()
        .ok_or(KernelError::NotFound {
            entity: "time project",
        })?;
    let v = view.version;
    Ok(with_etag(status, &view, Version(v)))
}

/// Create a project / grant / committee (`people.time.manage`; Idempotency-Key). The PI is
/// always a member.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/time-projects", operation_id = "people_time_project_create", tag = "people-time",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TimeProjectInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TimeProjectView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn project_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TimeProjectInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimeTrackingManage)?;
    require_idempotency_key(&headers)?;
    let c = clean_project(&b)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.time_project (tenant_id, institution_id, code, name, kind, funding_agency, scheme, \
         sanction_number, sanction_date, sanctioned_amount, manpower_budget, start_date, end_date, pi_staff_id, \
         cost_centre, contract_id, budget_kind, budget_period, budget_value, rounding_minutes, minimum_minutes, \
         billable_default, open_to_all, notes, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, \
         $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24, $25) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&c.code)
    .bind(&c.name)
    .bind(&b.kind)
    .bind(&c.funding_agency)
    .bind(&c.scheme)
    .bind(&c.sanction_number)
    .bind(b.sanction_date)
    .bind(c.sanctioned_amount)
    .bind(c.manpower_budget)
    .bind(b.start_date)
    .bind(b.end_date)
    .bind(b.pi_staff_id)
    .bind(&c.cost_centre)
    .bind(b.contract_id)
    .bind(&b.budget_kind)
    .bind(&b.budget_period)
    .bind(c.budget_value)
    .bind(b.rounding_minutes.map(|x| x as i32))
    .bind(b.minimum_minutes.map(|x| x as i32))
    .bind(b.billable_default.unwrap_or(true))
    .bind(b.open_to_all.unwrap_or(false))
    .bind(&c.notes)
    .bind(&a.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => KernelError::Conflict(format!("project code {} is already used", c.code)),
        _ => map_db_error(e),
    })?;
    write_members(&mut tx, t, id, pid, &c.members).await?;
    audit(&mut tx, &scope, "people.time_project.create", "time_project", pid,
        json!({"code": c.code, "kind": b.kind, "fundingAgency": c.funding_agency, "piStaffId": b.pi_staff_id}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    project_response(&st, ctx, pid, StatusCode::CREATED).await
}

/// A project with its members (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/time-projects/{id}", operation_id = "people_time_project_get", tag = "people-time",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TimeProjectView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn project_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(pid): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    project_response(&st, ctx, pid, StatusCode::OK).await
}

/// Change a project, its members and status (`people.time.manage`; If-Match). Time already
/// logged stays; members removed can no longer log.
#[utoipa::path(put, path = "/api/v1/people/time-projects/{id}", operation_id = "people_time_project_update", tag = "people-time",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = TimeProjectInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimeProjectView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn project_update(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(pid): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TimeProjectInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimeTrackingManage)?;
    let want = expected!(headers);
    let c = clean_project(&b)?;
    let status = b.status.clone().unwrap_or_else(|| "active".into());
    if !["active", "closed", "archived"].contains(&status.as_str()) {
        return Err(invalid("status", "invalid", "active, closed or archived").into());
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = project(&mut tx, t, pid).await?;
    let inst = InstitutionId(p.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    ensure_version(want, p.version)?;
    sqlx::query(
        "UPDATE people.time_project SET code = $3, name = $4, kind = $5, funding_agency = $6, scheme = $7, \
         sanction_number = $8, sanction_date = $9, sanctioned_amount = $10, manpower_budget = $11, start_date = $12, \
         end_date = $13, pi_staff_id = $14, cost_centre = $15, contract_id = $16, budget_kind = $17, budget_period = $18, \
         budget_value = $19, rounding_minutes = $20, minimum_minutes = $21, billable_default = $22, open_to_all = $23, \
         status = $24, notes = $25, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(pid)
    .bind(t.as_uuid())
    .bind(&c.code)
    .bind(&c.name)
    .bind(&b.kind)
    .bind(&c.funding_agency)
    .bind(&c.scheme)
    .bind(&c.sanction_number)
    .bind(b.sanction_date)
    .bind(c.sanctioned_amount)
    .bind(c.manpower_budget)
    .bind(b.start_date)
    .bind(b.end_date)
    .bind(b.pi_staff_id)
    .bind(&c.cost_centre)
    .bind(b.contract_id)
    .bind(&b.budget_kind)
    .bind(&b.budget_period)
    .bind(c.budget_value)
    .bind(b.rounding_minutes.map(|x| x as i32))
    .bind(b.minimum_minutes.map(|x| x as i32))
    .bind(b.billable_default.unwrap_or(true))
    .bind(b.open_to_all.unwrap_or(false))
    .bind(&status)
    .bind(&c.notes)
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => KernelError::Conflict(format!("project code {} is already used", c.code)),
        _ => map_db_error(e),
    })?;
    write_members(&mut tx, t, p.institution_id, pid, &c.members).await?;
    audit(&mut tx, &scope, "people.time_project.update", "time_project", pid,
        json!({"code": c.code, "status": {"from": p.status, "to": status}, "members": c.members.len()}), &actor(ctx)).await?;
    tx.commit().await.map_err(map_db_error)?;
    project_response(&st, ctx, pid, StatusCode::OK).await
}

// ------------------------------------------------------------------ entries & costing

#[derive(Debug, Clone)]
struct EntryRow {
    id: Uuid,
    staff_id: Uuid,
    staff_name: String,
    staff_number: String,
    project_id: Uuid,
    project_code: String,
    project_name: String,
    funding_agency: Option<String>,
    date: NaiveDate,
    seconds: i64,
    started_at: Option<NaiveDateTime>,
    ended_at: Option<NaiveDateTime>,
    task: String,
    notes: String,
    billable: bool,
    source: String,
    rounding: Option<i64>,
    minimum: Option<i64>,
    week_status: Option<String>,
    version: i64,
}

const ENTRY_SELECT: &str = "SELECT e.id, e.staff_id, s.display_name, s.staff_number, e.project_id, p.code, p.name, \
     p.funding_agency, e.entry_date, e.seconds, e.started_at, e.ended_at, e.task, e.notes, e.billable, e.source, \
     p.rounding_minutes, p.minimum_minutes, e.version, \
     (SELECT ts.status FROM people.time_timesheet ts WHERE ts.tenant_id = e.tenant_id AND ts.staff_id = e.staff_id \
       AND ts.project_id = e.project_id AND e.entry_date BETWEEN ts.week_start AND ts.week_start + 6) AS week_status \
     FROM people.time_entry e JOIN people.staff_member s ON s.id = e.staff_id \
     JOIN people.time_project p ON p.id = e.project_id WHERE e.tenant_id = $1";

fn entry_of(r: &PgRow) -> Result<EntryRow, KernelError> {
    Ok(EntryRow {
        id: get(r, "id")?,
        staff_id: get(r, "staff_id")?,
        staff_name: get(r, "display_name")?,
        staff_number: get(r, "staff_number")?,
        project_id: get(r, "project_id")?,
        project_code: get(r, "code")?,
        project_name: get(r, "name")?,
        funding_agency: get(r, "funding_agency")?,
        date: get(r, "entry_date")?,
        seconds: i64::from(get::<i32>(r, "seconds")?),
        started_at: get(r, "started_at")?,
        ended_at: get(r, "ended_at")?,
        task: get(r, "task")?,
        notes: get(r, "notes")?,
        billable: get(r, "billable")?,
        source: get(r, "source")?,
        rounding: get::<Option<i32>>(r, "rounding_minutes")?.map(i64::from),
        minimum: get::<Option<i32>>(r, "minimum_minutes")?.map(i64::from),
        week_status: get(r, "week_status")?,
        version: get(r, "version")?,
    })
}

/// Entries matching `cond` (placeholders `$2` id, `$3`/`$4` dates).
async fn entries_where(
    conn: &mut PgConnection,
    t: TenantId,
    cond: &str,
    id: Uuid,
    from: Option<NaiveDate>,
    to: Option<NaiveDate>,
) -> Result<Vec<EntryRow>, KernelError> {
    sqlx::query(&format!(
        "{ENTRY_SELECT} AND {cond} ORDER BY e.entry_date, e.started_at NULLS LAST, e.created_at"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(entry_of)
    .collect()
}

/// Rate sources and rules for pricing entries of one institution.
struct Costing {
    settings: Settings,
    staff_rates: HashMap<Uuid, RateValue>,
    /// (period start, full-month gross) of finalised payroll results, oldest first.
    payroll: HashMap<Uuid, Vec<(NaiveDate, f64)>>,
}

impl Costing {
    async fn load(conn: &mut PgConnection, t: TenantId, inst: Uuid) -> Result<Self, KernelError> {
        let settings = load_settings(conn, t, inst).await?;
        let staff_rates = sqlx::query_as::<_, (Uuid, Value)>(
            "SELECT staff_id, rate FROM people.time_staff_rate WHERE tenant_id = $1 AND institution_id = $2",
        )
        .bind(t.as_uuid())
        .bind(inst)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .filter_map(|(s, v)| serde_json::from_value(v).ok().map(|r| (s, r)))
        .collect();
        let rows: Vec<(Uuid, NaiveDate, f64, f64, f64)> = sqlx::query_as(
            "SELECT staff_id, period_start, gross_earnings::float8, period_days::float8, payable_days::float8 \
             FROM people.payroll_result WHERE tenant_id = $1 AND institution_id = $2 AND status = 'finalized' \
             ORDER BY period_start, revision",
        )
        .bind(t.as_uuid())
        .bind(inst)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let mut payroll: HashMap<Uuid, Vec<(NaiveDate, f64)>> = HashMap::new();
        for (s, start, gross, period, payable) in rows {
            let full = if payable > 0.0 {
                gross * period / payable
            } else {
                gross
            };
            payroll.entry(s).or_default().push((start, full));
        }
        Ok(Self {
            settings,
            staff_rates,
            payroll,
        })
    }

    /// Hourly rate from the latest finalised payroll that started on or before `date` (else the
    /// earliest one).
    fn payroll_hourly(&self, staff: Uuid, date: NaiveDate) -> Option<f64> {
        let list = self.payroll.get(&staff)?;
        let gross = list
            .iter()
            .rev()
            .find(|(s, _)| *s <= date)
            .or_else(|| list.first())?
            .1;
        lg::hourly_from_monthly(gross, self.settings.standard_monthly_hours)
    }

    /// (rate, source) on `date`: staff rate → payroll → default → none.
    fn rate(&self, staff: Uuid, date: NaiveDate) -> (f64, &'static str) {
        let d = ds(date);
        let own = self.staff_rates.get(&staff);
        if let Some(r) = lg::rate_at(own, &d) {
            return (r, "staff");
        }
        let derived = self.payroll_hourly(staff, date).map(RateValue::Plain);
        let levels = lg::RateLevels {
            entry: None,
            project: own,
            client: derived.as_ref(),
            default: self.settings.default_rate.as_ref(),
        };
        let r = lg::resolve_rate(&levels, &d, &d);
        let source = if derived.is_some() {
            "payroll"
        } else if lg::rate_at(self.settings.default_rate.as_ref(), &d).is_some() {
            "default"
        } else {
            "none"
        };
        (r, source)
    }

    /// (billed seconds, rate, amount) of an entry.
    fn price(&self, e: &EntryRow) -> (i64, f64, f64) {
        let rounding = lg::resolve_rounding(
            e.rounding.map(|x| x as f64),
            Some(self.settings.rounding as f64),
        );
        let billed = lg::billed_seconds(e.billable, e.seconds, rounding, e.minimum.unwrap_or(0));
        let (rate, _) = self.rate(e.staff_id, e.date);
        (billed, rate, lg::entry_amount(e.billable, billed, rate))
    }
}

fn hm(t: Option<NaiveDateTime>) -> Option<String> {
    t.map(|x| x.format("%H:%M").to_string())
}

/// One time entry.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeEntryView {
    pub id: Uuid,
    pub staff_id: Uuid,
    pub staff_name: String,
    pub project_id: Uuid,
    pub project_code: String,
    pub project_name: String,
    pub date: NaiveDate,
    pub seconds: i64,
    /// `h:mm` (`h:mm:ss` when the seconds are not whole minutes).
    pub duration: String,
    /// `HH:MM` wall-clock start on the entry's day, when known.
    pub start: Option<String>,
    pub end: Option<String>,
    /// The end falls on the next day (a night's work).
    pub ends_next_day: bool,
    pub task: String,
    pub notes: String,
    /// Chargeable to the project (counts towards cost and utilisation).
    pub billable: bool,
    /// `manual | timer | import`.
    pub source: String,
    /// The week is submitted or approved: the entry cannot change.
    pub locked: bool,
    pub version: i64,
}

fn duration_text(s: i64) -> String {
    if s % 60 == 0 {
        lg::fmt_clock(s as f64)
    } else {
        lg::fmt_timer(s as f64)
    }
}

fn entry_view(e: &EntryRow) -> TimeEntryView {
    TimeEntryView {
        id: e.id,
        staff_id: e.staff_id,
        staff_name: e.staff_name.clone(),
        project_id: e.project_id,
        project_code: e.project_code.clone(),
        project_name: e.project_name.clone(),
        date: e.date,
        seconds: e.seconds,
        duration: duration_text(e.seconds),
        start: hm(e.started_at),
        end: hm(e.ended_at),
        ends_next_day: e.ended_at.is_some_and(|x| x.date() > e.date),
        task: e.task.clone(),
        notes: e.notes.clone(),
        billable: e.billable,
        source: e.source.clone(),
        locked: e.week_status.as_deref().is_some_and(lg::timesheet_locked),
        version: e.version,
    }
}

fn tracked(e: &EntryRow) -> Tracked {
    Tracked {
        id: e.id.to_string(),
        seconds: e.seconds,
        billable: e.billable,
        billed: 0,
        amount: 0.0,
        started_at: e.started_at.map(lg::ms_of),
        ended_at: e.ended_at.map(lg::ms_of),
    }
}

fn week_of(date: NaiveDate, week_start: u32) -> NaiveDate {
    lg::week_start_of(&ds(date), week_start)
        .and_then(|s| pd(&s))
        .unwrap_or(date)
}

async fn week_status(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    project: Uuid,
    week: NaiveDate,
) -> Result<Option<String>, KernelError> {
    sqlx::query_scalar(
        "SELECT status FROM people.time_timesheet WHERE tenant_id = $1 AND staff_id = $2 AND project_id = $3 AND week_start = $4",
    )
    .bind(t.as_uuid())
    .bind(staff)
    .bind(project)
    .bind(week)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn ensure_open_week(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    project: Uuid,
    date: NaiveDate,
    week_start: u32,
) -> Result<(), KernelError> {
    let week = week_of(date, week_start);
    if week_status(conn, t, staff, project, week)
        .await?
        .as_deref()
        .is_some_and(lg::timesheet_locked)
    {
        return Err(KernelError::Conflict(format!(
            "the timesheet for the week of {} is submitted or approved; ask your approver to return it first",
            week.format("%d %b %Y")
        )));
    }
    Ok(())
}

/// Clashing entries of the same person around `date` (409 names the first).
async fn ensure_no_overlap(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    date: NaiveDate,
    span: (Option<NaiveDateTime>, Option<NaiveDateTime>),
    except: Option<Uuid>,
) -> Result<(), KernelError> {
    let (Some(s), Some(e)) = span else {
        return Ok(());
    };
    let around = entries_where(
        conn,
        t,
        "e.staff_id = $2 AND e.entry_date BETWEEN $3 AND $4 AND e.started_at IS NOT NULL",
        staff,
        Some(date - Duration::days(1)),
        Some(date + Duration::days(1)),
    )
    .await?;
    let ts: Vec<Tracked> = around.iter().map(tracked).collect();
    let skip = except.map(|x| x.to_string());
    let hits = lg::overlapping(&ts, Some(lg::ms_of(s)), Some(lg::ms_of(e)), skip.as_deref());
    if let Some(h) = hits.first() {
        let other = around.iter().find(|x| x.id.to_string() == h.id);
        let what = other.map_or_else(String::new, |o| {
            format!(
                " ({} {}–{} on {})",
                o.date.format("%d %b"),
                hm(o.started_at).unwrap_or_default(),
                hm(o.ended_at).unwrap_or_default(),
                o.project_code
            )
        });
        return Err(KernelError::Conflict(format!(
            "this time overlaps another entry of yours{what}"
        )));
    }
    Ok(())
}

/// A manual entry. `duration` takes `1:30`, `1h30`, `1.5`, `90m`, `:45`; with `start` and `end`
/// (`HH:MM`; an end at or before the start is the next morning) the duration may be left out.
/// When all three are given the start stays and the end moves to where the duration lands.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeEntryInput {
    pub project_id: Uuid,
    pub date: NaiveDate,
    pub duration: Option<String>,
    pub start: Option<String>,
    pub end: Option<String>,
    pub task: Option<String>,
    pub notes: Option<String>,
    /// Chargeable to the project (default: the project's default).
    pub billable: Option<bool>,
}

struct CleanEntry {
    seconds: i64,
    started_at: Option<NaiveDateTime>,
    ended_at: Option<NaiveDateTime>,
    task: String,
    notes: String,
}

fn clean_entry(b: &TimeEntryInput) -> Result<CleanEntry, KernelError> {
    let d = ds(b.date);
    let start = b.start.as_deref().map(str::trim).filter(|x| !x.is_empty());
    let end = b.end.as_deref().map(str::trim).filter(|x| !x.is_empty());
    let s_ms = match start {
        Some(x) => Some(
            lg::ms_at(&d, Some(x), 0)
                .ok_or_else(|| invalid("start", "invalid_time", "a time such as 09:30"))?,
        ),
        None => None,
    };
    let mut e_ms = match end {
        Some(x) => Some(
            lg::ms_at(&d, Some(x), 0)
                .ok_or_else(|| invalid("end", "invalid_time", "a time such as 17:30"))?,
        ),
        None => None,
    };
    if let (Some(s), Some(e)) = (s_ms, e_ms) {
        if e <= s {
            e_ms = lg::ms_at(&d, end, 1);
        }
    }
    if s_ms.is_some() != e_ms.is_some() {
        return Err(invalid(
            "end",
            "incomplete",
            "give both a start and an end, or neither",
        ));
    }
    let typed = b
        .duration
        .as_deref()
        .map(str::trim)
        .filter(|x| !x.is_empty());
    let (seconds, ended) = match (typed, s_ms, e_ms) {
        (Some(t), s, e) => {
            let secs = lg::parse_duration(t).ok_or_else(|| {
                invalid(
                    "duration",
                    "invalid_duration",
                    "a duration such as 1:30, 1h30, 1.5 or 90m",
                )
            })?;
            let (n, end2) = lg::duration_patch(s, e, secs as f64);
            (n, end2)
        }
        (None, Some(s), Some(e)) => ((e - s) / 1000, Some(e)),
        _ => {
            return Err(invalid(
                "duration",
                "required",
                "a duration, or a start and an end",
            ))
        }
    };
    let problems = lg::entry_problems(seconds, &d, None, None, s_ms, ended);
    if let Some(p) = problems.first() {
        return Err(invalid("duration", "invalid", p));
    }
    Ok(CleanEntry {
        seconds,
        started_at: s_ms.and_then(lg::dt_of),
        ended_at: ended.and_then(lg::dt_of),
        task: text(b.task.as_ref(), 200, "task")?.unwrap_or_default(),
        notes: text(b.notes.as_ref(), 2000, "notes")?.unwrap_or_default(),
    })
}

/// The project as the staff member may use it (active, theirs, inside its period).
fn usable(ps: &[Proj], id: Uuid, date: NaiveDate) -> Result<&Proj, KernelError> {
    let p = ps.iter().find(|p| p.id == id).ok_or_else(|| {
        invalid(
            "projectId",
            "not_member",
            "you can log time only on active projects you belong to",
        )
    })?;
    let problems = lg::entry_problems(
        1,
        &ds(date),
        p.start_date.map(ds).as_deref(),
        p.end_date.map(ds).as_deref(),
        None,
        None,
    );
    if let Some(x) = problems.first() {
        return Err(invalid("date", "outside_project", x));
    }
    Ok(p)
}

/// A saved entry.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeEntryResult {
    pub entry: TimeEntryView,
}

async fn entry_result(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let e = entries_where(
        conn,
        t,
        "e.id = $2 AND ($3::date IS NULL OR $4::date IS NULL OR true)",
        id,
        None,
        None,
    )
    .await?
    .pop()
    .ok_or(KernelError::NotFound {
        entity: "time entry",
    })?;
    let v = e.version;
    Ok(with_etag(
        status,
        &TimeEntryResult {
            entry: entry_view(&e),
        },
        Version(v),
    ))
}

struct Me {
    staff: Uuid,
    inst: Uuid,
}

async fn me_of(st: &PeopleState, ctx: &AuthContext) -> Result<Me, KernelError> {
    let me = my_staff(st, ctx).await?;
    Ok(Me {
        staff: me.id.as_uuid(),
        inst: me.institution_id.as_uuid(),
    })
}

/// Log time (`people.self`; Idempotency-Key). 409 when the week is submitted or the clock times
/// overlap another of your entries.
#[utoipa::path(post, path = "/api/v1/people/me/time/entries", operation_id = "people_time_entry_create", tag = "people-time",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TimeEntryInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TimeEntryResult, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn entry_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<TimeEntryInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let c = clean_entry(&b)?;
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, me.staff).await?;
    let settings = load_settings(&mut tx, t, me.inst).await?;
    let ps = loggable(&mut tx, t, me.inst, me.staff).await?;
    let p = usable(&ps, b.project_id, b.date)?;
    ensure_open_week(&mut tx, t, me.staff, p.id, b.date, settings.week_start).await?;
    ensure_no_overlap(
        &mut tx,
        t,
        me.staff,
        b.date,
        (c.started_at, c.ended_at),
        None,
    )
    .await?;
    let a = actor(ctx);
    let id = insert_entry(
        &mut tx,
        t,
        me.inst,
        me.staff,
        p.id,
        b.date,
        &c,
        b.billable.unwrap_or(p.billable_default),
        "manual",
        &a.id,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.time_entry.create",
        "time_entry",
        id,
        json!({"projectId": p.id, "date": b.date, "seconds": c.seconds}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    entry_result(&mut conn, t, id, StatusCode::CREATED).await
}

async fn lock_staff(conn: &mut PgConnection, staff: Uuid) -> Result<(), KernelError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("time:{staff}"))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
async fn insert_entry(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    staff: Uuid,
    project: Uuid,
    date: NaiveDate,
    c: &CleanEntry,
    billable: bool,
    source: &str,
    by: &Option<String>,
) -> Result<Uuid, KernelError> {
    sqlx::query_scalar(
        "INSERT INTO people.time_entry (tenant_id, institution_id, staff_id, project_id, entry_date, seconds, \
         started_at, ended_at, task, notes, billable, source, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(staff)
    .bind(project)
    .bind(date)
    .bind(c.seconds as i32)
    .bind(c.started_at)
    .bind(c.ended_at)
    .bind(&c.task)
    .bind(&c.notes)
    .bind(billable)
    .bind(source)
    .bind(by)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn my_entry(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
    staff: Uuid,
) -> Result<EntryRow, KernelError> {
    entries_where(
        conn,
        t,
        "e.id = $2 AND ($3::date IS NULL OR $4::date IS NULL OR true)",
        id,
        None,
        None,
    )
    .await?
    .pop()
    .filter(|e| e.staff_id == staff)
    .ok_or(KernelError::NotFound {
        entity: "time entry",
    })
}

/// Change one of your entries (`people.self`; If-Match). Both the old and the new week must be
/// open.
#[utoipa::path(put, path = "/api/v1/people/me/time/entries/{id}", operation_id = "people_time_entry_update", tag = "people-time",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = TimeEntryInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimeEntryResult, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn entry_update(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TimeEntryInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let want = expected!(headers);
    let c = clean_entry(&b)?;
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, me.staff).await?;
    let old = my_entry(&mut tx, t, id, me.staff).await?;
    ensure_version(want, old.version)?;
    let settings = load_settings(&mut tx, t, me.inst).await?;
    ensure_open_week(
        &mut tx,
        t,
        me.staff,
        old.project_id,
        old.date,
        settings.week_start,
    )
    .await?;
    let ps = loggable(&mut tx, t, me.inst, me.staff).await?;
    let p = usable(&ps, b.project_id, b.date)?;
    ensure_open_week(&mut tx, t, me.staff, p.id, b.date, settings.week_start).await?;
    ensure_no_overlap(
        &mut tx,
        t,
        me.staff,
        b.date,
        (c.started_at, c.ended_at),
        Some(id),
    )
    .await?;
    sqlx::query(
        "UPDATE people.time_entry SET project_id = $3, entry_date = $4, seconds = $5, started_at = $6, ended_at = $7, \
         task = $8, notes = $9, billable = $10, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(p.id)
    .bind(b.date)
    .bind(c.seconds as i32)
    .bind(c.started_at)
    .bind(c.ended_at)
    .bind(&c.task)
    .bind(&c.notes)
    .bind(b.billable.unwrap_or(old.billable))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.time_entry.update",
        "time_entry",
        id,
        json!({"from": {"projectId": old.project_id, "date": old.date, "seconds": old.seconds},
            "to": {"projectId": p.id, "date": b.date, "seconds": c.seconds}}),
        &actor(ctx),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    entry_result(&mut conn, t, id, StatusCode::OK).await
}

/// Delete one of your entries in an open week (`people.self`; If-Match).
#[utoipa::path(delete, path = "/api/v1/people/me/time/entries/{id}", operation_id = "people_time_entry_delete", tag = "people-time",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn entry_delete(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let want = expected!(headers);
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, me.staff).await?;
    let old = my_entry(&mut tx, t, id, me.staff).await?;
    ensure_version(want, old.version)?;
    let settings = load_settings(&mut tx, t, me.inst).await?;
    ensure_open_week(
        &mut tx,
        t,
        me.staff,
        old.project_id,
        old.date,
        settings.week_start,
    )
    .await?;
    sqlx::query("DELETE FROM people.time_entry WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.time_entry.delete",
        "time_entry",
        id,
        json!({"projectId": old.project_id, "date": old.date, "seconds": old.seconds}),
        &actor(ctx),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ------------------------------------------------------------------ timer

async fn local_now(
    st: &PeopleState,
    ctx: &AuthContext,
    inst: Uuid,
) -> Result<NaiveDateTime, KernelError> {
    let i = InstitutionId(inst);
    let r = require_institution(st, &institution_scope(ctx, i), i).await?;
    sqlx::query_scalar("SELECT (now() AT TIME ZONE $1)")
        .bind(&r.timezone)
        .fetch_one(&st.pool)
        .await
        .map_err(map_db_error)
}

/// A running timer.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeTimerView {
    pub project_id: Uuid,
    pub project_code: String,
    pub project_name: String,
    /// Local wall-clock start (institution timezone).
    pub started_at: NaiveDateTime,
    pub elapsed_seconds: i64,
    /// `h:mm:ss`.
    pub elapsed: String,
    pub task: String,
    pub notes: String,
    pub billable: bool,
}

async fn timer_of(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    now: NaiveDateTime,
) -> Result<Option<TimeTimerView>, KernelError> {
    let row: Option<(Uuid, String, String, NaiveDateTime, String, String, bool)> = sqlx::query_as(
        "SELECT r.project_id, p.code, p.name, r.started_at, r.task, r.notes, r.billable FROM people.time_timer r \
         JOIN people.time_project p ON p.id = r.project_id WHERE r.tenant_id = $1 AND r.staff_id = $2",
    )
    .bind(t.as_uuid())
    .bind(staff)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(row.map(
        |(project_id, project_code, project_name, started_at, task, notes, billable)| {
            let el = lg::elapsed_seconds(lg::ms_of(started_at), lg::ms_of(now));
            TimeTimerView {
                project_id,
                project_code,
                project_name,
                started_at,
                elapsed_seconds: el,
                elapsed: lg::fmt_timer(el as f64),
                task,
                notes,
                billable,
            }
        },
    ))
}

/// Start a timer.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeTimerStart {
    pub project_id: Uuid,
    pub task: Option<String>,
    pub notes: Option<String>,
    pub billable: Option<bool>,
}

/// Start your timer on a project (`people.self`); 409 while one is running.
#[utoipa::path(post, path = "/api/v1/people/me/time/timer", operation_id = "people_time_timer_start", tag = "people-time",
    request_body = TimeTimerStart, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TimeTimerView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timer_start(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Json(b): Json<TimeTimerStart>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let now = local_now(&st, ctx, me.inst).await?;
    let t = ctx.scope.tenant_id;
    let task = text(b.task.as_ref(), 200, "task")?.unwrap_or_default();
    let notes = text(b.notes.as_ref(), 2000, "notes")?.unwrap_or_default();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, me.staff).await?;
    let ps = loggable(&mut tx, t, me.inst, me.staff).await?;
    let p = usable(&ps, b.project_id, now.date())?;
    let inserted = sqlx::query(
        "INSERT INTO people.time_timer (staff_id, tenant_id, institution_id, project_id, started_at, task, notes, billable) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (staff_id) DO NOTHING",
    )
    .bind(me.staff)
    .bind(t.as_uuid())
    .bind(me.inst)
    .bind(p.id)
    .bind(now)
    .bind(&task)
    .bind(&notes)
    .bind(b.billable.unwrap_or(p.billable_default))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if inserted.rows_affected() == 0 {
        return Err(
            KernelError::Conflict("a timer is already running; stop it first".into()).into(),
        );
    }
    audit(
        &mut tx,
        &scope,
        "people.time_timer.start",
        "staff",
        me.staff,
        json!({"projectId": p.id}),
        &actor(ctx),
    )
    .await?;
    let view = timer_of(&mut tx, t, me.staff, now)
        .await?
        .ok_or(KernelError::NotFound { entity: "timer" })?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)).into_response())
}

/// Stop the timer.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeTimerStop {
    /// Trim a forgotten timer to this many hours (default: the institution's timer cap).
    pub cap_hours: Option<f64>,
}

/// Stop your timer and record an entry on the day it started (`people.self`): under a minute
/// still records a minute; a forgotten timer is trimmed to the cap.
#[utoipa::path(post, path = "/api/v1/people/me/time/timer/stop", operation_id = "people_time_timer_stop", tag = "people-time",
    request_body = TimeTimerStop, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TimeEntryResult, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timer_stop(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Json(b): Json<TimeTimerStop>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let now = local_now(&st, ctx, me.inst).await?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, me.staff).await?;
    let timer = timer_of(&mut tx, t, me.staff, now)
        .await?
        .ok_or_else(|| KernelError::Conflict("no timer is running".into()))?;
    let settings = load_settings(&mut tx, t, me.inst).await?;
    let cap_h = b
        .cap_hours
        .filter(|h| *h > 0.0 && *h <= 24.0)
        .unwrap_or(settings.timer_cap_hours);
    let seconds = lg::stopped_seconds(timer.elapsed_seconds, Some((cap_h * 3600.0) as i64));
    let date = timer.started_at.date();
    let c = CleanEntry {
        seconds,
        started_at: Some(timer.started_at),
        ended_at: Some(timer.started_at + Duration::seconds(seconds)),
        task: timer.task.clone(),
        notes: timer.notes.clone(),
    };
    ensure_open_week(
        &mut tx,
        t,
        me.staff,
        timer.project_id,
        date,
        settings.week_start,
    )
    .await?;
    ensure_no_overlap(&mut tx, t, me.staff, date, (c.started_at, c.ended_at), None).await?;
    let a = actor(ctx);
    let id = insert_entry(
        &mut tx,
        t,
        me.inst,
        me.staff,
        timer.project_id,
        date,
        &c,
        timer.billable,
        "timer",
        &a.id,
    )
    .await?;
    sqlx::query("DELETE FROM people.time_timer WHERE staff_id = $1 AND tenant_id = $2")
        .bind(me.staff)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(&mut tx, &scope, "people.time_timer.stop", "time_entry", id,
        json!({"projectId": timer.project_id, "elapsed": timer.elapsed_seconds, "seconds": seconds}), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    entry_result(&mut conn, t, id, StatusCode::CREATED).await
}

/// Discard your running timer without recording anything (`people.self`).
#[utoipa::path(delete, path = "/api/v1/people/me/time/timer", operation_id = "people_time_timer_discard", tag = "people-time",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timer_discard(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let gone = sqlx::query("DELETE FROM people.time_timer WHERE staff_id = $1 AND tenant_id = $2")
        .bind(me.staff)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if gone.rows_affected() == 0 {
        return Err(KernelError::Conflict("no timer is running".into()).into());
    }
    audit(
        &mut tx,
        &scope,
        "people.time_timer.discard",
        "staff",
        me.staff,
        json!({}),
        &actor(ctx),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

// ------------------------------------------------------------------ my week

/// A timesheet in brief.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimesheetBrief {
    pub id: Uuid,
    /// `submitted | approved | rejected | draft`.
    pub status: String,
    pub decision_note: Option<String>,
    pub version: i64,
}

/// One project row of my week.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyTimeProjectWeek {
    pub project_id: Uuid,
    pub code: String,
    pub name: String,
    pub kind: String,
    pub funding_agency: Option<String>,
    pub billable_default: bool,
    /// Seconds per day of the week, in order.
    pub day_seconds: Vec<i64>,
    pub seconds: i64,
    pub timesheet: Option<TimesheetBrief>,
}

/// My time for one week.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyTimeWeek {
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub today: NaiveDate,
    pub week_start: NaiveDate,
    pub days: Vec<NaiveDate>,
    pub settings: TimeSettings,
    pub projects: Vec<MyTimeProjectWeek>,
    pub entries: Vec<TimeEntryView>,
    pub timer: Option<TimeTimerView>,
    pub week_seconds: i64,
    /// `1h 30m`.
    pub week_total: String,
}

/// Which week.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct WeekQuery {
    /// Any day of the week (default: this week).
    pub week: Option<NaiveDate>,
}

/// My week (`people.self`): the projects I can log on with daily totals and timesheet status,
/// the week's entries and my running timer.
#[utoipa::path(get, path = "/api/v1/people/me/time", operation_id = "people_time_me_week", tag = "people-time",
    params(WeekQuery), security(("session_cookie" = [])),
    responses((status = 200, body = MyTimeWeek), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_week(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Query(q): Query<WeekQuery>,
) -> ApiResult<Json<MyTimeWeek>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = me_of(&st, ctx).await?;
    let now = local_now(&st, ctx, me.inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let settings = load_settings(&mut conn, t, me.inst).await?;
    let today = now.date();
    let week = week_of(q.week.unwrap_or(today), settings.week_start);
    let days: Vec<NaiveDate> = (0..7).map(|i| week + Duration::days(i)).collect();
    let entries = entries_where(
        &mut conn,
        t,
        "e.staff_id = $2 AND e.entry_date BETWEEN $3 AND $4",
        me.staff,
        Some(week),
        Some(week + Duration::days(6)),
    )
    .await?;
    let ps = loggable(&mut conn, t, me.inst, me.staff).await?;
    let sheets: Vec<(Uuid, Uuid, String, Option<String>, i64)> = sqlx::query_as(
        "SELECT id, project_id, status, decision_note, version FROM people.time_timesheet WHERE tenant_id = $1 AND staff_id = $2 AND week_start = $3",
    )
    .bind(t.as_uuid())
    .bind(me.staff)
    .bind(week)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut rows: Vec<MyTimeProjectWeek> = Vec::new();
    let mut ids: Vec<Uuid> = ps.iter().map(|p| p.id).collect();
    for e in &entries {
        if !ids.contains(&e.project_id) {
            ids.push(e.project_id);
        }
    }
    for pid in ids {
        let p = ps.iter().find(|p| p.id == pid);
        let mine: Vec<&EntryRow> = entries.iter().filter(|e| e.project_id == pid).collect();
        let first = mine.first();
        let day_seconds: Vec<i64> = days
            .iter()
            .map(|d| {
                mine.iter()
                    .filter(|e| e.date == *d)
                    .map(|e| e.seconds)
                    .sum()
            })
            .collect();
        rows.push(MyTimeProjectWeek {
            project_id: pid,
            code: p
                .map(|x| x.code.clone())
                .or_else(|| first.map(|e| e.project_code.clone()))
                .unwrap_or_default(),
            name: p
                .map(|x| x.name.clone())
                .or_else(|| first.map(|e| e.project_name.clone()))
                .unwrap_or_default(),
            kind: p.map(|x| x.kind.clone()).unwrap_or_else(|| "other".into()),
            funding_agency: p
                .and_then(|x| x.funding_agency.clone())
                .or_else(|| first.and_then(|e| e.funding_agency.clone())),
            billable_default: p.is_none_or(|x| x.billable_default),
            seconds: day_seconds.iter().sum(),
            day_seconds,
            timesheet: sheets.iter().find(|s| s.1 == pid).map(|s| TimesheetBrief {
                id: s.0,
                status: s.2.clone(),
                decision_note: s.3.clone(),
                version: s.4,
            }),
        });
    }
    let week_seconds: i64 = entries.iter().map(|e| e.seconds).sum();
    let timer = timer_of(&mut conn, t, me.staff, now).await?;
    Ok(Json(MyTimeWeek {
        institution_id: me.inst,
        staff_id: me.staff,
        today,
        week_start: week,
        days,
        settings: settings_view(&settings),
        projects: rows,
        entries: entries.iter().map(entry_view).collect(),
        timer,
        week_total: lg::fmt_duration(week_seconds as f64),
        week_seconds,
    }))
}

// ------------------------------------------------------------------ timesheets

/// A weekly timesheet of one staff member on one project.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimesheetView {
    pub id: Uuid,
    pub staff_id: Uuid,
    pub staff_name: String,
    pub project_id: Uuid,
    pub project_code: String,
    pub project_name: String,
    pub week_start: NaiveDate,
    pub week_end: NaiveDate,
    /// `draft | submitted | approved | rejected`.
    pub status: String,
    pub total_seconds: i64,
    /// Chargeable seconds after rounding (snapshot at submission).
    pub billed_seconds: i64,
    pub submitted_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decided_at: Option<chrono::DateTime<chrono::Utc>>,
    pub decided_by: Option<String>,
    pub decision_note: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone)]
struct Sheet {
    view: TimesheetView,
    institution_id: Uuid,
    submitted_by: Option<String>,
    pi_staff_id: Option<Uuid>,
    owner_subject: Option<String>,
    manager_staff_id: Option<Uuid>,
}

const SHEET_SELECT: &str = "SELECT ts.id, ts.institution_id, ts.staff_id, s.display_name, s.subject_ref, ts.project_id, \
     p.code, p.name, p.pi_staff_id, ts.week_start, ts.status, ts.total_seconds, ts.billed_seconds, ts.submitted_at, \
     ts.submitted_by, ts.decided_at, ts.decided_by, ts.decision_note, ts.version, \
     (SELECT g.reports_to_staff_id FROM people.staff_engagement g WHERE g.staff_id = ts.staff_id \
       AND g.status NOT IN ('closed', 'cancelled') ORDER BY g.start_date DESC LIMIT 1) AS manager \
     FROM people.time_timesheet ts JOIN people.staff_member s ON s.id = ts.staff_id \
     JOIN people.time_project p ON p.id = ts.project_id WHERE ts.tenant_id = $1";

fn sheet_of(r: &PgRow) -> Result<Sheet, KernelError> {
    let week_start: NaiveDate = get(r, "week_start")?;
    Ok(Sheet {
        institution_id: get(r, "institution_id")?,
        submitted_by: get(r, "submitted_by")?,
        pi_staff_id: get(r, "pi_staff_id")?,
        owner_subject: get(r, "subject_ref")?,
        manager_staff_id: get(r, "manager")?,
        view: TimesheetView {
            id: get(r, "id")?,
            staff_id: get(r, "staff_id")?,
            staff_name: get(r, "display_name")?,
            project_id: get(r, "project_id")?,
            project_code: get(r, "code")?,
            project_name: get(r, "name")?,
            week_start,
            week_end: week_start + Duration::days(6),
            status: get(r, "status")?,
            total_seconds: i64::from(get::<i32>(r, "total_seconds")?),
            billed_seconds: i64::from(get::<i32>(r, "billed_seconds")?),
            submitted_at: get(r, "submitted_at")?,
            decided_at: get(r, "decided_at")?,
            decided_by: get(r, "decided_by")?,
            decision_note: get(r, "decision_note")?,
            version: get(r, "version")?,
        },
    })
}

async fn sheets_where(
    conn: &mut PgConnection,
    t: TenantId,
    cond: &str,
    a: Option<Uuid>,
    b: Option<String>,
) -> Result<Vec<Sheet>, KernelError> {
    sqlx::query(&format!(
        "{SHEET_SELECT} AND {cond} ORDER BY ts.week_start DESC, s.display_name"
    ))
    .bind(t.as_uuid())
    .bind(a)
    .bind(b)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(sheet_of)
    .collect()
}

async fn sheet(conn: &mut PgConnection, t: TenantId, id: Uuid) -> Result<Sheet, KernelError> {
    sheets_where(
        conn,
        t,
        "ts.id = $2 AND ($3::text IS NULL OR true)",
        Some(id),
        None,
    )
    .await?
    .pop()
    .ok_or(KernelError::NotFound {
        entity: "timesheet",
    })
}

/// Submit a week.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimesheetSubmit {
    pub project_id: Uuid,
    /// Any day of the week.
    pub week: NaiveDate,
}

/// Submit one project's week for approval (`people.self`): the week's entries lock until the
/// PI or your reporting manager decides. A week without time is 422; a submitted or approved
/// week is 409.
#[utoipa::path(post, path = "/api/v1/people/me/time/timesheets", operation_id = "people_time_timesheet_submit", tag = "people-time",
    request_body = TimesheetSubmit, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimesheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_submit(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Json(b): Json<TimesheetSubmit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, me.staff).await?;
    let settings = load_settings(&mut tx, t, me.inst).await?;
    let week = week_of(b.week, settings.week_start);
    let entries = entries_where(
        &mut tx,
        t,
        "e.staff_id = $2 AND e.entry_date BETWEEN $3 AND $4",
        me.staff,
        Some(week),
        Some(week + Duration::days(6)),
    )
    .await?;
    let mine: Vec<&EntryRow> = entries
        .iter()
        .filter(|e| e.project_id == b.project_id)
        .collect();
    if mine.is_empty() {
        return Err(invalid(
            "week",
            "empty",
            "there is no time on this project in that week",
        )
        .into());
    }
    let current = week_status(&mut tx, t, me.staff, b.project_id, week).await?;
    if let Some(s) = current.as_deref() {
        if !lg::timesheet_can(s, "submitted") {
            return Err(KernelError::Conflict(format!("this timesheet is already {s}")).into());
        }
    }
    let costing = Costing::load(&mut tx, t, me.inst).await?;
    let total: i64 = mine.iter().map(|e| e.seconds).sum();
    let billed: i64 = mine.iter().map(|e| costing.price(e).0).sum();
    let a = actor(ctx);
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.time_timesheet (tenant_id, institution_id, staff_id, project_id, week_start, status, \
         total_seconds, billed_seconds, submitted_at, submitted_by) VALUES ($1, $2, $3, $4, $5, 'submitted', $6, $7, now(), $8) \
         ON CONFLICT (tenant_id, staff_id, project_id, week_start) DO UPDATE SET status = 'submitted', \
         total_seconds = EXCLUDED.total_seconds, billed_seconds = EXCLUDED.billed_seconds, submitted_at = now(), \
         submitted_by = EXCLUDED.submitted_by, decided_at = NULL, decided_by = NULL, decision_note = NULL, \
         version = people.time_timesheet.version + 1, updated_at = now() RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(me.inst)
    .bind(me.staff)
    .bind(b.project_id)
    .bind(week)
    .bind(total as i32)
    .bind(billed as i32)
    .bind(&a.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.time_timesheet.submit",
        "time_timesheet",
        id,
        json!({"projectId": b.project_id, "weekStart": week, "totalSeconds": total}),
        &a,
    )
    .await?;
    let s = sheet(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = s.view.version;
    Ok(with_etag(StatusCode::OK, &s.view, Version(v)))
}

/// Take back a submitted timesheet before it is decided (`people.self`; If-Match): the week
/// opens again.
#[utoipa::path(post, path = "/api/v1/people/me/time/timesheets/{id}/withdraw", operation_id = "people_time_timesheet_withdraw", tag = "people-time",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimesheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_withdraw(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let want = expected!(headers);
    let me = me_of(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = sheet(&mut tx, t, id).await?;
    if s.view.staff_id != me.staff {
        return Err(KernelError::NotFound {
            entity: "timesheet",
        }
        .into());
    }
    let inst = InstitutionId(s.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    ensure_version(want, s.view.version)?;
    if !lg::timesheet_can(&s.view.status, "draft") {
        return Err(KernelError::Conflict(format!(
            "a {} timesheet cannot be withdrawn",
            s.view.status
        ))
        .into());
    }
    sqlx::query("UPDATE people.time_timesheet SET status = 'draft', version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.time_timesheet.withdraw",
        "time_timesheet",
        id,
        json!({}),
        &actor(ctx),
    )
    .await?;
    let s = sheet(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = s.view.version;
    Ok(with_etag(StatusCode::OK, &s.view, Version(v)))
}

/// Timesheets.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimesheetList {
    pub items: Vec<TimesheetView>,
}

/// Timesheets of an institution (`people.read`), newest week first; `?status=submitted` is the
/// approval queue.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/timesheets", operation_id = "people_time_timesheet_list", tag = "people-time",
    params(("id" = String, Path, format = Uuid), StatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = TimesheetList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<TimesheetList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = sheets_where(
        &mut conn,
        ctx.scope.tenant_id,
        "ts.institution_id = $2 AND ($3::text IS NULL OR ts.status = $3)",
        Some(id),
        q.status,
    )
    .await?
    .into_iter()
    .take(500)
    .map(|s| s.view)
    .collect();
    Ok(Json(TimesheetList { items }))
}

/// Timesheets waiting for my decision (`people.self`): projects I lead as PI and staff who
/// report to me; never my own.
#[utoipa::path(get, path = "/api/v1/people/me/time-approvals", operation_id = "people_time_me_approvals", tag = "people-time",
    security(("session_cookie" = [])),
    responses((status = 200, body = TimesheetList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_approvals(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<TimesheetList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = me_of(&st, ctx).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = sheets_where(
        &mut conn,
        ctx.scope.tenant_id,
        "ts.status = 'submitted' AND ts.staff_id <> $2 AND ($3::text IS NULL OR true)",
        Some(me.staff),
        None,
    )
    .await?
    .into_iter()
    .filter(|s| s.pi_staff_id == Some(me.staff) || s.manager_staff_id == Some(me.staff))
    .map(|s| s.view)
    .collect();
    Ok(Json(TimesheetList { items }))
}

/// A timesheet with its entries.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimesheetDetail {
    pub timesheet: TimesheetView,
    pub entries: Vec<TimeEntryView>,
    /// The caller may approve / reject it now.
    pub can_decide: bool,
}

async fn my_staff_id(st: &PeopleState, ctx: &AuthContext) -> Option<Uuid> {
    if !ctx.has_permission(Permission::PeopleSelfService) {
        return None;
    }
    me_of(st, ctx).await.ok().map(|m| m.staff)
}

fn may_decide(ctx: &AuthContext, s: &Sheet, me: Option<Uuid>) -> bool {
    s.view.status == "submitted"
        && me != Some(s.view.staff_id)
        && (ctx.has_permission(Permission::TimesheetDecide)
            || me.is_some_and(|m| s.pi_staff_id == Some(m) || s.manager_staff_id == Some(m)))
}

/// A timesheet and its entries: the owner, the project's PI, the owner's reporting manager or
/// `people.read`.
#[utoipa::path(get, path = "/api/v1/people/timesheets/{id}", operation_id = "people_time_timesheet_get", tag = "people-time",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TimesheetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = require_any(
        &auth,
        &[Permission::PeopleRead, Permission::PeopleSelfService],
    )?;
    let me = my_staff_id(&st, ctx).await;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = sheet(&mut conn, t, id).await?;
    let involved = me.is_some_and(|m| {
        m == s.view.staff_id || s.pi_staff_id == Some(m) || s.manager_staff_id == Some(m)
    });
    if !(involved || ctx.has_permission(Permission::PeopleRead)) {
        return Err(KernelError::NotFound {
            entity: "timesheet",
        }
        .into());
    }
    let entries = entries_where(
        &mut conn,
        t,
        "e.staff_id = $2 AND e.entry_date BETWEEN $3 AND $4",
        s.view.staff_id,
        Some(s.view.week_start),
        Some(s.view.week_end),
    )
    .await?
    .iter()
    .filter(|e| e.project_id == s.view.project_id)
    .map(entry_view)
    .collect();
    let can_decide = may_decide(ctx, &s, me);
    let v = s.view.version;
    Ok(with_etag(
        StatusCode::OK,
        &TimesheetDetail {
            timesheet: s.view,
            entries,
            can_decide,
        },
        Version(v),
    ))
}

/// Approve or reject.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct TimesheetDecision {
    /// `approve` | `reject`.
    pub decision: String,
    /// Required to reject.
    pub reason: Option<String>,
}

/// Decide a submitted timesheet (If-Match): the project's PI or the staff member's reporting
/// manager (`people.self`), or `people.timesheet.decide` on behalf of any approver. Never the
/// person who logged or submitted it (separation of duties, 403). Approved weeks stay locked;
/// a rejected week opens again for correction and resubmission.
#[utoipa::path(post, path = "/api/v1/people/timesheets/{id}/decision", operation_id = "people_time_timesheet_decide", tag = "people-time",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = TimesheetDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimesheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TimesheetDecision>,
) -> ApiResult<Response> {
    let ctx = require_any(
        &auth,
        &[Permission::TimesheetDecide, Permission::PeopleSelfService],
    )?;
    let want = expected!(headers);
    let approve = match b.decision.as_str() {
        "approve" => true,
        "reject" => false,
        _ => return Err(invalid("decision", "invalid", "approve or reject").into()),
    };
    let reason = text(b.reason.as_ref(), 1000, "reason")?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "say why the week is returned").into());
    }
    let me = my_staff_id(&st, ctx).await;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = sheet(&mut tx, t, id).await?;
    let inst = InstitutionId(s.institution_id);
    let scope = institution_scope(ctx, inst);
    let approver = ctx.has_permission(Permission::TimesheetDecide)
        || me.is_some_and(|m| s.pi_staff_id == Some(m) || s.manager_staff_id == Some(m));
    if !approver {
        return Err(KernelError::Forbidden("only the project's PI, the staff member's reporting manager or a timesheet approver may decide".into()).into());
    }
    let a = actor(ctx);
    sutra_kernel::approval::ensure_independent_decider(
        a.id.as_deref(),
        [s.owner_subject.as_deref(), s.submitted_by.as_deref()]
            .into_iter()
            .flatten(),
        "timesheet",
    )?;
    if me == Some(s.view.staff_id) {
        return Err(KernelError::Forbidden(
            "separation of duties: you cannot decide your own timesheet".into(),
        )
        .into());
    }
    require_institution_writable(&st, &scope, inst).await?;
    ensure_version(want, s.view.version)?;
    let to = if approve { "approved" } else { "rejected" };
    if !lg::timesheet_can(&s.view.status, to) {
        return Err(
            KernelError::Conflict(format!("a {} timesheet cannot be {to}", s.view.status)).into(),
        );
    }
    sqlx::query(
        "UPDATE people.time_timesheet SET status = $3, decided_at = now(), decided_by = $4, decision_note = $5, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(to)
    .bind(&a.id)
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(&mut tx, &scope, &format!("people.time_timesheet.{}", if approve { "approve" } else { "reject" }), "time_timesheet", id,
        json!({"staffId": s.view.staff_id, "projectId": s.view.project_id, "weekStart": s.view.week_start, "reason": reason}), &a).await?;
    let s = sheet(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = s.view.version;
    Ok(with_etag(StatusCode::OK, &s.view, Version(v)))
}

// ------------------------------------------------------------------ rates

/// Hourly cost rate of one staff member.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeStaffRate {
    pub staff_id: Uuid,
    pub name: String,
    pub staff_number: String,
    /// The staff member's own rate (₹/h; a number or effective-dated periods), if set.
    pub rate: Option<RateValue>,
    pub periods: Vec<RatePeriod>,
    /// Gross salary of the last finalised payroll ÷ standard monthly hours.
    pub payroll_hourly: Option<f64>,
    /// The rate costing uses today.
    pub effective_today: f64,
    /// `staff | payroll | default | none`.
    pub source: String,
}

/// Staff rates.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimeStaffRateList {
    pub items: Vec<TimeStaffRate>,
}

fn periods_of(v: Option<&RateValue>) -> Vec<RatePeriod> {
    match v {
        None => vec![],
        Some(RateValue::Plain(r)) => vec![RatePeriod {
            from: None,
            rate: Some(*r),
        }],
        Some(RateValue::Periods(l)) => {
            let mut l = l.clone();
            l.sort_by(|a, b| {
                a.from
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.from.as_deref().unwrap_or(""))
            });
            l
        }
    }
}

fn rate_item(
    c: &Costing,
    staff: Uuid,
    name: String,
    staff_number: String,
    today: NaiveDate,
) -> TimeStaffRate {
    let own = c.staff_rates.get(&staff);
    let (eff, source) = c.rate(staff, today);
    TimeStaffRate {
        staff_id: staff,
        name,
        staff_number,
        rate: own.cloned(),
        periods: periods_of(own),
        payroll_hourly: c.payroll_hourly(staff, today),
        effective_today: eff,
        source: source.into(),
    }
}

/// Hourly cost rates of active staff (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/time-rates", operation_id = "people_time_rate_list", tag = "people-time",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TimeStaffRateList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rate_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<TimeStaffRateList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = Costing::load(&mut conn, t, id).await?;
    let staff: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, display_name, staff_number FROM people.staff_member WHERE tenant_id = $1 AND institution_id = $2 \
         AND status IN ('active', 'suspended') ORDER BY display_name, id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let items = staff
        .into_iter()
        .map(|(s, n, no)| rate_item(&c, s, n, no, today))
        .collect();
    Ok(Json(TimeStaffRateList { items }))
}

/// Change a staff member's own hourly rate.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeRateInput {
    /// ₹ per hour; null clears (from a date: stops this level from that day).
    pub rate: Option<f64>,
    /// Start a new period on this date; null edits the period in effect today.
    pub from: Option<NaiveDate>,
}

/// Set a staff member's hourly rate (`people.time.manage`): with a date it starts a new period
/// so earlier work keeps its rate (Everything's effective-dated `withRateFrom`).
#[utoipa::path(put, path = "/api/v1/people/staff/{id}/time-rate", operation_id = "people_time_rate_put", tag = "people-time",
    params(("id" = String, Path, format = Uuid)), request_body = TimeRateInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimeStaffRate), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn rate_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(sid): Path<Uuid>,
    Json(b): Json<TimeRateInput>,
) -> ApiResult<Json<TimeStaffRate>> {
    let ctx = auth.require(Permission::TimeTrackingManage)?;
    if b.rate
        .is_some_and(|r| !(r.is_finite() && (0.0..=1_000_000.0).contains(&r)))
    {
        return Err(invalid("rate", "invalid", "a rate in ₹ per hour").into());
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row: Option<(Uuid, String, String)> = sqlx::query_as(
        "SELECT institution_id, display_name, staff_number FROM people.staff_member WHERE id = $1 AND tenant_id = $2",
    )
    .bind(sid)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (inst_id, name, number) = row.ok_or(KernelError::NotFound {
        entity: "staff member",
    })?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let current: Option<RateValue> = sqlx::query_scalar::<_, Value>(
        "SELECT rate FROM people.time_staff_rate WHERE staff_id = $1 AND tenant_id = $2 FOR UPDATE",
    )
    .bind(sid)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .and_then(|v| serde_json::from_value(v).ok());
    let from = b.from.map(ds);
    let next = lg::with_rate_from(current.as_ref(), b.rate, from.as_deref(), &ds(today));
    match &next {
        None => {
            sqlx::query(
                "DELETE FROM people.time_staff_rate WHERE staff_id = $1 AND tenant_id = $2",
            )
            .bind(sid)
            .bind(t.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        Some(v) => {
            sqlx::query(
                "INSERT INTO people.time_staff_rate (staff_id, tenant_id, institution_id, rate) VALUES ($1, $2, $3, $4) \
                 ON CONFLICT (staff_id) DO UPDATE SET rate = EXCLUDED.rate, version = people.time_staff_rate.version + 1, updated_at = now()",
            )
            .bind(sid)
            .bind(t.as_uuid())
            .bind(inst_id)
            .bind(json!(v))
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    audit(
        &mut tx,
        &scope,
        "people.time_rate.update",
        "staff",
        sid,
        json!({"from": current, "to": next, "edit": {"rate": b.rate, "from": b.from}}),
        &actor(ctx),
    )
    .await?;
    let c = Costing::load(&mut tx, t, inst_id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(rate_item(&c, sid, name, number, today)))
}

// ------------------------------------------------------------------ effort report

/// Effort report range (`YYYY-MM` months, inclusive).
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct EffortQuery {
    /// First month `YYYY-MM` (default: this month).
    pub from: Option<String>,
    /// Last month `YYYY-MM` (default: `from`).
    pub to: Option<String>,
    /// Only this project.
    pub project_id: Option<Uuid>,
    /// Count only time in approved timesheets.
    pub approved_only: Option<bool>,
}

/// Hours and effort of one staff member on one project in one month.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffortRow {
    pub staff_id: Uuid,
    pub staff_name: String,
    pub staff_number: String,
    pub project_id: Uuid,
    pub project_code: String,
    pub project_name: String,
    /// `YYYY-MM`.
    pub month: String,
    pub seconds: i64,
    pub hours: f64,
    /// Chargeable seconds after rounding.
    pub billed_seconds: i64,
    /// Share of the staff member's tracked time that month, %.
    pub percent: f64,
    /// Share × 1 month (the utilisation certificate's person-months).
    pub person_months: f64,
    /// ₹ = chargeable hours × hourly rate.
    #[schema(value_type = String)]
    pub cost: Decimal,
}

/// Totals of one staff member on one project over the range.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffortStaffTotal {
    pub staff_id: Uuid,
    pub staff_name: String,
    pub project_id: Uuid,
    pub project_code: String,
    pub hours: f64,
    pub person_months: f64,
    #[schema(value_type = String)]
    pub cost: Decimal,
}

/// Utilisation of one project over the range.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffortProjectSummary {
    pub project_id: Uuid,
    pub code: String,
    pub name: String,
    pub funding_agency: Option<String>,
    pub sanction_number: Option<String>,
    #[schema(value_type = Option<String>)]
    pub sanctioned_amount: Option<Decimal>,
    #[schema(value_type = Option<String>)]
    pub manpower_budget: Option<Decimal>,
    pub hours: f64,
    pub person_months: f64,
    #[schema(value_type = String)]
    pub cost: Decimal,
    /// Cost ÷ manpower budget, %.
    pub utilisation_percent: Option<f64>,
    pub staff_count: i64,
}

/// Effort report.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffortReport {
    pub from: String,
    pub to: String,
    pub approved_only: bool,
    pub standard_monthly_hours: f64,
    pub rows: Vec<EffortRow>,
    pub staff_totals: Vec<EffortStaffTotal>,
    pub projects: Vec<EffortProjectSummary>,
}

fn month_range(
    q: &EffortQuery,
    today: NaiveDate,
) -> Result<(String, String, NaiveDate, NaiveDate), KernelError> {
    let ok = |m: &str| m.len() == 7 && pd(&format!("{m}-01")).is_some();
    let from = q
        .from
        .clone()
        .unwrap_or_else(|| lg::month_key_of(&ds(today)));
    let to = q.to.clone().unwrap_or_else(|| from.clone());
    if !ok(&from) || !ok(&to) || to < from {
        return Err(invalid("from", "invalid", "months YYYY-MM with from ≤ to"));
    }
    if lg::month_keys_between(&format!("{from}-01"), &format!("{to}-01")).len() > 60 {
        return Err(invalid("to", "too_long", "at most 60 months"));
    }
    let start = pd(&format!("{from}-01")).ok_or_else(|| invalid("from", "invalid", "YYYY-MM"))?;
    let end = lg::month_days(&to)
        .pop()
        .and_then(|d| pd(&d))
        .ok_or_else(|| invalid("to", "invalid", "YYYY-MM"))?;
    Ok((from, to, start, end))
}

async fn effort(
    st: &PeopleState,
    ctx: &AuthContext,
    inst_id: Uuid,
    q: &EffortQuery,
) -> Result<EffortReport, KernelError> {
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution(st, &scope, inst).await?;
    let today = business_today(st, &scope, inst).await?;
    let (from, to, start, end) = month_range(q, today)?;
    let approved_only = q.approved_only.unwrap_or(false);
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let costing = Costing::load(&mut conn, t, inst_id).await?;
    let all = entries_where(
        &mut conn,
        t,
        "e.institution_id = $2 AND e.entry_date BETWEEN $3 AND $4",
        inst_id,
        Some(start),
        Some(end),
    )
    .await?;
    let counted = |e: &&EntryRow| !approved_only || e.week_status.as_deref() == Some("approved");
    // Month totals per staff over every project (the denominator of % effort).
    let mut month_total: HashMap<(Uuid, String), i64> = HashMap::new();
    for e in all.iter().filter(counted) {
        *month_total
            .entry((e.staff_id, lg::month_key_of(&ds(e.date))))
            .or_default() += e.seconds;
    }
    let mut cells: BTreeMap<(String, String, String, Uuid, Uuid), (i64, i64, f64, &EntryRow)> =
        BTreeMap::new();
    for e in all
        .iter()
        .filter(counted)
        .filter(|e| q.project_id.is_none_or(|p| p == e.project_id))
    {
        let month = lg::month_key_of(&ds(e.date));
        let (billed, _, amount) = costing.price(e);
        let c = cells
            .entry((
                e.project_code.clone(),
                e.staff_name.clone(),
                month,
                e.staff_id,
                e.project_id,
            ))
            .or_insert((0, 0, 0.0, e));
        c.0 += e.seconds;
        c.1 += billed;
        c.2 = lg::round2(c.2 + amount);
    }
    let mut rows = Vec::new();
    let mut staff_totals: BTreeMap<(String, String, Uuid, Uuid), (f64, f64, f64, String)> =
        BTreeMap::new();
    for ((code, name, month, staff, project), (secs, billed, amount, e)) in &cells {
        let total = month_total
            .get(&(*staff, month.clone()))
            .copied()
            .unwrap_or(*secs);
        let ef = lg::effort(*secs, total);
        let tt = staff_totals
            .entry((code.clone(), name.clone(), *staff, *project))
            .or_insert((0.0, 0.0, 0.0, e.staff_name.clone()));
        tt.0 += *secs as f64;
        tt.1 += ef.person_months;
        tt.2 = lg::round2(tt.2 + amount);
        rows.push(EffortRow {
            staff_id: *staff,
            staff_name: e.staff_name.clone(),
            staff_number: e.staff_number.clone(),
            project_id: *project,
            project_code: code.clone(),
            project_name: e.project_name.clone(),
            month: month.clone(),
            seconds: *secs,
            hours: lg::to_hours(*secs as f64, 2),
            billed_seconds: *billed,
            percent: ef.percent,
            person_months: ef.person_months,
            cost: money(*amount),
        });
    }
    let staff_totals: Vec<EffortStaffTotal> = staff_totals
        .into_iter()
        .map(
            |((code, _, staff, project), (secs, pm, amount, name))| EffortStaffTotal {
                staff_id: staff,
                staff_name: name,
                project_id: project,
                project_code: code,
                hours: lg::to_hours(secs, 2),
                person_months: lg::js_round(pm * 1000.0) / 1000.0,
                cost: money(amount),
            },
        )
        .collect();
    let ps = projects_of(&mut conn, t, inst_id, None).await?;
    let mut projects = Vec::new();
    for p in ps.iter().filter(|p| q.project_id.is_none_or(|x| x == p.id)) {
        let mine: Vec<&EffortStaffTotal> = staff_totals
            .iter()
            .filter(|s| s.project_id == p.id)
            .collect();
        if mine.is_empty() && q.project_id.is_none() {
            continue;
        }
        let cost: Decimal = mine.iter().map(|s| s.cost).sum();
        let hours = lg::round2(mine.iter().map(|s| s.hours).sum());
        let pm = lg::js_round(mine.iter().map(|s| s.person_months).sum::<f64>() * 1000.0) / 1000.0;
        let staff: HashSet<Uuid> = mine.iter().map(|s| s.staff_id).collect();
        projects.push(EffortProjectSummary {
            project_id: p.id,
            code: p.code.clone(),
            name: p.name.clone(),
            funding_agency: p.funding_agency.clone(),
            sanction_number: p.sanction_number.clone(),
            sanctioned_amount: p.sanctioned_amount,
            manpower_budget: p.manpower_budget,
            hours,
            person_months: pm,
            cost,
            utilisation_percent: p
                .manpower_budget
                .filter(|b| *b > Decimal::ZERO)
                .and_then(|b| {
                    (cost * Decimal::from(100) / b)
                        .round_dp_with_strategy(
                            2,
                            rust_decimal::RoundingStrategy::MidpointAwayFromZero,
                        )
                        .to_f64()
                }),
            staff_count: staff.len() as i64,
        });
    }
    Ok(EffortReport {
        from,
        to,
        approved_only,
        standard_monthly_hours: costing.settings.standard_monthly_hours,
        rows,
        staff_totals,
        projects,
    })
}

/// Effort report (`people.read`): hours, % effort and person-months per staff per project per
/// month, costed at chargeable hours × hourly rate, with project utilisation against the
/// manpower budget — the figures a DST / SERB / UGC utilisation certificate asks for.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/effort-report", operation_id = "people_time_effort_report", tag = "people-time",
    params(("id" = String, Path, format = Uuid), EffortQuery), security(("session_cookie" = [])),
    responses((status = 200, body = EffortReport), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn effort_report(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<EffortQuery>,
) -> ApiResult<Json<EffortReport>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    Ok(Json(effort(&st, ctx, id, &q).await?))
}

/// The effort report as CSV (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/effort-report/csv", operation_id = "people_time_effort_report_csv", tag = "people-time",
    params(("id" = String, Path, format = Uuid), EffortQuery), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn effort_report_csv(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<EffortQuery>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let r = effort(&st, ctx, id, &q).await?;
    let mut lines = vec![lg::to_csv_line(&[
        "Month",
        "Staff no.",
        "Staff",
        "Project code",
        "Project",
        "Hours",
        "Effort %",
        "Person-months",
        "Cost (INR)",
    ])];
    for x in &r.rows {
        lines.push(lg::to_csv_line(&[
            x.month.clone(),
            x.staff_number.clone(),
            x.staff_name.clone(),
            x.project_code.clone(),
            x.project_name.clone(),
            x.hours.to_string(),
            x.percent.to_string(),
            x.person_months.to_string(),
            x.cost.to_string(),
        ]));
    }
    Ok(csv_response(
        &format!("effort-{}-{}.csv", r.from, r.to),
        lines.join("\r\n"),
    )?)
}

// ------------------------------------------------------------------ CSV export / import

/// Export range.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ExportQuery {
    /// First day (default: 30 days ago).
    pub from: Option<NaiveDate>,
    /// Last day (default: today).
    pub to: Option<NaiveDate>,
    pub project_id: Option<Uuid>,
}

/// Everything's export header, Indianised: staff and project columns, ₹ rate and amount.
pub const CSV_HEADER: [&str; 15] = [
    "Date",
    "Start",
    "End",
    "Staff no.",
    "Staff",
    "Project code",
    "Project",
    "Funding agency",
    "Task",
    "Notes",
    "Duration",
    "Hours",
    "Chargeable",
    "Rate (INR/h)",
    "Amount (INR)",
];

fn entries_csv(es: &[EntryRow], c: &Costing) -> String {
    let mut lines = vec![lg::to_csv_line(&CSV_HEADER)];
    for e in es {
        let (_, rate, amount) = c.price(e);
        lines.push(lg::to_csv_line(&[
            ds(e.date),
            hm(e.started_at).unwrap_or_default(),
            hm(e.ended_at).unwrap_or_default(),
            e.staff_number.clone(),
            e.staff_name.clone(),
            e.project_code.clone(),
            e.project_name.clone(),
            e.funding_agency.clone().unwrap_or_default(),
            e.task.clone(),
            e.notes.clone(),
            duration_text(e.seconds),
            lg::to_hours(e.seconds as f64, 2).to_string(),
            (if e.billable { "yes" } else { "no" }).to_owned(),
            if e.billable {
                rate.to_string()
            } else {
                String::new()
            },
            if e.billable {
                format!("{amount:.2}")
            } else {
                String::new()
            },
        ]));
    }
    lines.join("\r\n")
}

fn export_range(q: &ExportQuery, today: NaiveDate) -> Result<(NaiveDate, NaiveDate), KernelError> {
    let to = q.to.unwrap_or(today);
    let from = q.from.unwrap_or(to - Duration::days(30));
    if to < from || (to - from).num_days() > 731 {
        return Err(invalid(
            "from",
            "invalid",
            "a range of at most two years with from ≤ to",
        ));
    }
    Ok((from, to))
}

/// Time entries of the institution as CSV (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/time-entries/csv", operation_id = "people_time_entries_csv", tag = "people-time",
    params(("id" = String, Path, format = Uuid), ExportQuery), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn entries_csv_admin(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let (from, to) = export_range(&q, business_today(&st, &scope, inst).await?)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = Costing::load(&mut conn, t, id).await?;
    let es: Vec<EntryRow> = entries_where(
        &mut conn,
        t,
        "e.institution_id = $2 AND e.entry_date BETWEEN $3 AND $4",
        id,
        Some(from),
        Some(to),
    )
    .await?
    .into_iter()
    .filter(|e| q.project_id.is_none_or(|p| p == e.project_id))
    .collect();
    Ok(csv_response(
        &format!("time-{from}-{to}.csv"),
        entries_csv(&es, &c),
    )?)
}

/// My time entries as CSV (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/time/csv", operation_id = "people_time_me_csv", tag = "people-time",
    params(ExportQuery), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_csv(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Query(q): Query<ExportQuery>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let (from, to) = export_range(
        &q,
        business_today(&st, &institution_scope(ctx, inst), inst).await?,
    )?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = Costing::load(&mut conn, t, me.inst).await?;
    let es: Vec<EntryRow> = entries_where(
        &mut conn,
        t,
        "e.staff_id = $2 AND e.entry_date BETWEEN $3 AND $4",
        me.staff,
        Some(from),
        Some(to),
    )
    .await?
    .into_iter()
    .filter(|e| q.project_id.is_none_or(|p| p == e.project_id))
    .collect();
    Ok(csv_response(
        &format!("my-time-{from}-{to}.csv"),
        entries_csv(&es, &c),
    )?)
}

/// A CSV export of another tracker (Toggl, Clockify, Harvest or any CSV with a mapping).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeImportInput {
    /// The file's text.
    pub csv: String,
    /// Column per field (`date`, `duration`, `project`, `task`, `notes`, `billable`, `start`,
    /// `end`; -1 = absent). Default: detected from the header row.
    pub mapping: Option<BTreeMap<String, i64>>,
    /// Preview only (default true): nothing is written.
    pub dry_run: Option<bool>,
}

/// A row the import skipped.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TimeImportProblem {
    pub line: i64,
    pub reason: String,
}

/// Import outcome.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimeImportResult {
    /// `Toggl | Clockify | Harvest | CSV`.
    pub source: String,
    pub mapping: BTreeMap<String, i64>,
    pub headers: Vec<String>,
    pub rows: i64,
    /// Rows that would be (dry run) or were added.
    pub added: i64,
    /// Identical to time already logged (same day, project, task, duration, notes).
    pub skipped: i64,
    pub invalid: Vec<TimeImportProblem>,
    pub dry_run: bool,
    pub seconds: i64,
}

/// Import my time from another tracker's CSV (`people.self`). Projects match my loggable
/// projects by code or name (case-insensitive); unknown projects, dates outside the project,
/// submitted weeks and overlapping times are reported per line, never created; re-importing
/// the same file adds nothing.
#[utoipa::path(post, path = "/api/v1/people/me/time/import", operation_id = "people_time_import", tag = "people-time",
    request_body = TimeImportInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TimeImportResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn import(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Json(b): Json<TimeImportInput>,
) -> ApiResult<Json<TimeImportResult>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    if b.csv.len() > 2_000_000 {
        return Err(invalid("csv", "too_large", "at most 2 MB").into());
    }
    let rows = lg::parse_csv(&b.csv);
    let Some((head, data)) = rows.split_first() else {
        return Err(invalid("csv", "empty", "the file has no header row").into());
    };
    if data.len() > 5000 {
        return Err(invalid("csv", "too_many_rows", "at most 5000 rows").into());
    }
    let (source, detected) = lg::detect_mapping(head);
    let mapping = b.mapping.clone().unwrap_or(detected);
    for key in ["date", "duration", "project"] {
        if mapping.get(key).copied().unwrap_or(-1) < 0 {
            return Err(invalid(
                "mapping",
                "missing_column",
                &format!("say which column holds the {key}"),
            )
            .into());
        }
    }
    let dry_run = b.dry_run.unwrap_or(true);
    let (items, bad) = lg::rows_to_items(data, &mapping);
    let mut invalid_rows: Vec<TimeImportProblem> = bad
        .into_iter()
        .map(|x| TimeImportProblem {
            line: x.line,
            reason: x.reason,
        })
        .collect();
    let me = me_of(&st, ctx).await?;
    let inst = InstitutionId(me.inst);
    let scope = institution_scope(ctx, inst);
    if !dry_run {
        require_institution_writable(&st, &scope, inst).await?;
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, me.staff).await?;
    let settings = load_settings(&mut tx, t, me.inst).await?;
    let ps = loggable(&mut tx, t, me.inst, me.staff).await?;
    let lines: Vec<i64> = {
        // Lines of the valid items, in order (rows_to_items keeps file order).
        let bad_lines: HashSet<i64> = invalid_rows.iter().map(|x| x.line).collect();
        (0..data.len() as i64)
            .map(|i| i + 2)
            .filter(|l| !bad_lines.contains(l))
            .collect()
    };
    let (lo, hi) = items
        .iter()
        .fold((None::<NaiveDate>, None::<NaiveDate>), |(lo, hi), it| {
            let d = pd(&it.date);
            (lo.min(d).or(lo).or(d), hi.max(d))
        });
    let existing = match (lo, hi) {
        (Some(a), Some(z)) => {
            entries_where(
                &mut tx,
                t,
                "e.staff_id = $2 AND e.entry_date BETWEEN $3 AND $4",
                me.staff,
                Some(a - Duration::days(1)),
                Some(z + Duration::days(1)),
            )
            .await?
        }
        _ => vec![],
    };
    let mut seen: HashSet<String> = existing
        .iter()
        .map(|e| {
            lg::import_signature(
                &ds(e.date),
                &e.project_id.to_string(),
                &e.task,
                e.seconds,
                &e.notes,
            )
        })
        .collect();
    let mut spans: Vec<Tracked> = existing.iter().map(tracked).collect();
    let mut sheets: HashMap<(Uuid, NaiveDate), bool> = HashMap::new();
    let (mut added, mut skipped, mut seconds) = (0i64, 0i64, 0i64);
    let a = actor(ctx);
    for (it, line) in items.iter().zip(lines) {
        let wanted = it.project.trim().to_lowercase();
        let Some(p) = ps
            .iter()
            .find(|p| p.code.to_lowercase() == wanted || p.name.to_lowercase() == wanted)
        else {
            invalid_rows.push(TimeImportProblem {
                line,
                reason: format!("unknown project {}", it.project),
            });
            continue;
        };
        let Some(date) = pd(&it.date) else {
            invalid_rows.push(TimeImportProblem {
                line,
                reason: "unreadable date".into(),
            });
            continue;
        };
        if let Some(x) = lg::entry_problems(
            it.seconds,
            &it.date,
            p.start_date.map(ds).as_deref(),
            p.end_date.map(ds).as_deref(),
            it.started_at,
            it.ended_at,
        )
        .first()
        {
            invalid_rows.push(TimeImportProblem {
                line,
                reason: x.clone(),
            });
            continue;
        }
        let week = week_of(date, settings.week_start);
        let locked = match sheets.get(&(p.id, week)) {
            Some(l) => *l,
            None => {
                let l = week_status(&mut tx, t, me.staff, p.id, week)
                    .await?
                    .as_deref()
                    .is_some_and(lg::timesheet_locked);
                sheets.insert((p.id, week), l);
                l
            }
        };
        if locked {
            invalid_rows.push(TimeImportProblem {
                line,
                reason: "that week's timesheet is submitted".into(),
            });
            continue;
        }
        let sig =
            lg::import_signature(&it.date, &p.id.to_string(), &it.task, it.seconds, &it.notes);
        if seen.contains(&sig) {
            skipped += 1;
            continue;
        }
        if !lg::overlapping(&spans, it.started_at, it.ended_at, None).is_empty() {
            invalid_rows.push(TimeImportProblem {
                line,
                reason: "overlaps time already logged".into(),
            });
            continue;
        }
        seen.insert(sig);
        spans.push(Tracked {
            id: format!("import:{line}"),
            seconds: it.seconds,
            billable: it.billable,
            billed: 0,
            amount: 0.0,
            started_at: it.started_at,
            ended_at: it.ended_at,
        });
        added += 1;
        seconds += it.seconds;
        if !dry_run {
            let c = CleanEntry {
                seconds: it.seconds,
                started_at: it.started_at.and_then(lg::dt_of),
                ended_at: it.ended_at.and_then(lg::dt_of),
                task: it.task.chars().take(200).collect(),
                notes: it.notes.chars().take(2000).collect(),
            };
            insert_entry(
                &mut tx,
                t,
                me.inst,
                me.staff,
                p.id,
                date,
                &c,
                it.billable,
                "import",
                &a.id,
            )
            .await?;
        }
    }
    invalid_rows.sort_by_key(|x| x.line);
    if !dry_run && added > 0 {
        audit(&mut tx, &scope, "people.time_entry.import", "staff", me.staff,
            json!({"source": source, "added": added, "skipped": skipped, "invalid": invalid_rows.len()}), &a).await?;
        tx.commit().await.map_err(map_db_error)?;
    }
    Ok(Json(TimeImportResult {
        source,
        mapping,
        headers: head.clone(),
        rows: data.len() as i64,
        added,
        skipped,
        invalid: invalid_rows,
        dry_run,
        seconds,
    }))
}

/// Routes of time tracking.
pub fn router(st: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(settings_get, settings_put))
        .routes(routes!(project_list, project_create))
        .routes(routes!(project_get, project_update))
        .routes(routes!(me_week))
        .routes(routes!(entry_create))
        .routes(routes!(entry_update, entry_delete))
        .routes(routes!(timer_start, timer_discard))
        .routes(routes!(timer_stop))
        .routes(routes!(timesheet_submit))
        .routes(routes!(timesheet_withdraw))
        .routes(routes!(timesheet_list))
        .routes(routes!(me_approvals))
        .routes(routes!(timesheet_get))
        .routes(routes!(timesheet_decide))
        .routes(routes!(rate_list))
        .routes(routes!(rate_put))
        .routes(routes!(effort_report))
        .routes(routes!(effort_report_csv))
        .routes(routes!(entries_csv_admin))
        .routes(routes!(me_csv))
        .routes(routes!(import))
        .with_state(st)
}
