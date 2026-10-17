//! **ERP_EXTENSIONS 3A** — duty rosters over staff (rules in [`super::rules`], ported from
//! HeyPuter/everything shifts, MIT). Operation ids `people_roster_*`, tag `people-roster`.
//!
//! Shifts are planned per institution in a role, open until a staff member holds them. An
//! overlap on one person is refused (409); time off (approved leave), weekly availability,
//! weekly hour limits and the institution's working-time limits come back as warnings. Fill
//! proposes staff for every open shift in a range; publish makes draft shifts visible to staff.
#![allow(clippy::type_complexity)]

use super::rules::{
    self as r, Availability, Candidate, Context, DayWindow, Employee, FillRow, Gap, Requirement,
    Rules, Shift, TimeOff, Violation,
};
use crate::leave::common::{audit, ensure_version, expected, invalid};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_staff;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::PeopleState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{Duration, NaiveDate};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::collections::{BTreeMap, HashSet};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

fn ds(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn pd(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

fn fo(d: Option<Decimal>) -> Option<f64> {
    d.and_then(|x| x.to_f64())
}

fn dec(x: Option<f64>) -> Option<Decimal> {
    x.and_then(Decimal::from_f64).map(|d| d.round_dp(1))
}

// ------------------------------------------------------------------ loading

struct World {
    shifts: Vec<Shift>,
    employees: Vec<Employee>,
    availability: BTreeMap<String, Availability>,
    rules: Rules,
    requirements: Vec<Requirement>,
    meta: BTreeMap<String, ShiftMeta>,
}

#[derive(Clone)]
struct ShiftMeta {
    location: Option<String>,
    note: Option<String>,
    status: String,
    version: i64,
}

async fn load_rules(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<Rules, KernelError> {
    let row: Option<(Option<Decimal>, Option<Decimal>, Option<i32>)> = sqlx::query_as(
        "SELECT min_rest_hours, max_daily_hours, max_consecutive_days FROM people.roster_settings \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(row.map_or_else(Rules::default, |(a, b, c)| Rules {
        min_rest_hours: fo(a),
        max_daily_hours: fo(b),
        max_consecutive_days: c.map(f64::from),
    }))
}

async fn load_requirements(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<Vec<Requirement>, KernelError> {
    let rows: Vec<(Uuid, String, Vec<i32>, i32, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT id, role, days, head_count, start_time, end_time FROM people.roster_requirement \
         WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(|(id, role, days, count, start, end)| Requirement {
            id: id.to_string(),
            role,
            days: days
                .into_iter()
                .filter_map(|d| u32::try_from(d).ok())
                .collect(),
            count: i64::from(count),
            start,
            end,
        })
        .collect())
}

/// Shifts, staff, availability (weekly windows + approved leave) and settings around a range.
async fn load(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<World, KernelError> {
    let rows: Vec<(Uuid, NaiveDate, String, String, i32, String, Option<String>, Option<String>, Option<Uuid>, String, i64)> =
        sqlx::query_as(
            "SELECT id, shift_date, start_time, end_time, break_minutes, role, location, note, staff_id, \
             status, version FROM people.duty_shift WHERE tenant_id = $1 AND institution_id = $2 \
             AND shift_date BETWEEN $3 AND $4 ORDER BY shift_date, start_time, id",
        )
        .bind(t.as_uuid())
        .bind(inst)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut shifts = Vec::new();
    let mut meta = BTreeMap::new();
    for (id, date, start, end, brk, role, location, note, staff, status, version) in rows {
        shifts.push(Shift {
            id: id.to_string(),
            date: ds(date),
            start,
            end,
            break_minutes: Some(f64::from(brk)),
            role,
            assignee_id: staff.map(|s| s.to_string()),
        });
        meta.insert(
            id.to_string(),
            ShiftMeta {
                location,
                note,
                status,
                version,
            },
        );
    }
    let staff: Vec<(Uuid, String, String, Option<String>, Option<Decimal>, Option<Decimal>, Option<serde_json::Value>, Option<bool>)> =
        sqlx::query_as(
            "SELECT s.id, s.display_name, s.status, p.role, p.target_hours, p.max_hours, p.days, p.rosterable \
             FROM people.staff_member s LEFT JOIN people.roster_staff_profile p ON p.staff_id = s.id \
             WHERE s.tenant_id = $1 AND s.institution_id = $2 AND s.status IN ('active', 'suspended') \
             ORDER BY s.display_name, s.id",
        )
        .bind(t.as_uuid())
        .bind(inst)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut employees = Vec::new();
    let mut availability: BTreeMap<String, Availability> = BTreeMap::new();
    for (id, name, status, role, target, max, days, rosterable) in staff {
        employees.push(Employee {
            id: id.to_string(),
            name,
            role: role.unwrap_or_default(),
            active: status == "active" && rosterable.unwrap_or(true),
            max_hours: fo(max),
            target_hours: fo(target),
        });
        if let Some(d) = days.and_then(|v| serde_json::from_value::<Vec<Option<DayWindow>>>(v).ok())
        {
            availability.entry(id.to_string()).or_default().days = Some(d);
        }
    }
    let leave: Vec<(Uuid, NaiveDate, NaiveDate, String)> = sqlx::query_as(
        "SELECT staff_id, start_date, end_date, leave_type_code FROM people.leave_request \
         WHERE tenant_id = $1 AND institution_id = $2 AND status IN ('approved', 'reserved', 'consumed') \
         AND end_date >= $3 AND start_date <= $4 ORDER BY start_date",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(from - Duration::days(1))
    .bind(to + Duration::days(1))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (staff, a, b, code) in leave {
        availability
            .entry(staff.to_string())
            .or_default()
            .exceptions
            .push(TimeOff {
                from: ds(a),
                to: ds(b),
                note: Some(format!("{code} leave")),
            });
    }
    Ok(World {
        shifts,
        employees,
        availability,
        rules: load_rules(conn, t, inst).await?,
        requirements: load_requirements(conn, t, inst).await?,
        meta,
    })
}

/// The window loaded to judge a date range: whole weeks, plus two days either side for
/// overnight spill and rest gaps.
fn window(from: NaiveDate, to: NaiveDate) -> (NaiveDate, NaiveDate) {
    let start = pd(&r::start_of_week(&ds(from))).unwrap_or(from);
    let end_week = pd(&r::start_of_week(&ds(to))).unwrap_or(to) + Duration::days(6);
    (
        start - Duration::days(2),
        end_week.max(to) + Duration::days(2),
    )
}

// ------------------------------------------------------------------ views

/// A duty shift with its checks.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DutyShiftView {
    pub id: Uuid,
    pub date: String,
    pub start: String,
    pub end: String,
    pub break_minutes: f64,
    /// Paid minutes.
    pub net_minutes: f64,
    pub overnight: bool,
    pub role: String,
    pub location: Option<String>,
    pub note: Option<String>,
    pub staff_id: Option<Uuid>,
    pub staff_name: Option<String>,
    /// `draft | published`.
    pub status: String,
    /// Other shifts of the same person that overlap (should be none).
    pub conflicts: Vec<Uuid>,
    /// `available | partial | unavailable` against the holder's week and time off.
    pub availability: String,
    pub rule_warnings: Vec<Violation>,
    pub version: i64,
}

fn shift_view(s: &Shift, w: &World) -> DutyShiftView {
    let m = w.meta.get(&s.id).cloned().unwrap_or(ShiftMeta {
        location: None,
        note: None,
        status: "draft".into(),
        version: 1,
    });
    let who = s.assignee_id.as_deref();
    let emp = who.and_then(|a| w.employees.iter().find(|e| e.id == a));
    let uid = |x: &str| Uuid::parse_str(x).ok();
    DutyShiftView {
        id: uid(&s.id).unwrap_or_default(),
        date: s.date.clone(),
        start: s.start.clone(),
        end: s.end.clone(),
        break_minutes: r::break_minutes(s),
        net_minutes: r::net_minutes(s),
        overnight: r::is_overnight(&s.start, &s.end),
        role: s.role.clone(),
        location: m.location,
        note: m.note,
        staff_id: who.and_then(uid),
        staff_name: emp.map(|e| e.name.clone()),
        status: m.status,
        conflicts: r::find_conflicts(s, &w.shifts, who)
            .iter()
            .filter_map(|c| uid(&c.id))
            .collect(),
        availability: r::availability_status(s, who.and_then(|a| w.availability.get(a))).into(),
        rule_warnings: match emp {
            Some(e) => r::rule_violations(s, &e.id, &w.shifts, &w.rules, &e.name),
            None => Vec::new(),
        },
        version: m.version,
    }
}

/// A staff member's week on the roster.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterStaffRow {
    pub staff_id: Uuid,
    pub name: String,
    pub role: String,
    pub rosterable: bool,
    pub target_hours: Option<f64>,
    pub max_hours: Option<f64>,
    /// Paid minutes in the week starting `weekStart`.
    pub week_minutes: f64,
    /// `over | under | ok`.
    pub hours_status: String,
    /// One per broken working-time limit in the range.
    pub rule_warnings: Vec<Violation>,
}

/// The roster for a range.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterBoard {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// Monday of the week the staff hours are counted for.
    pub week_start: NaiveDate,
    pub open_shifts: usize,
    pub shifts: Vec<DutyShiftView>,
    pub gaps: Vec<Gap>,
    pub staff: Vec<RosterStaffRow>,
}

/// A date range (inclusive).
#[derive(Debug, Clone, Default, Deserialize, IntoParams, ToSchema)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RosterRange {
    /// Default: Monday of this week.
    pub from: Option<NaiveDate>,
    /// Default: six days after `from`; at most 62 days.
    pub to: Option<NaiveDate>,
}

fn range(q: &RosterRange, today: NaiveDate) -> Result<(NaiveDate, NaiveDate), KernelError> {
    let from = q
        .from
        .unwrap_or_else(|| pd(&r::start_of_week(&ds(today))).unwrap_or(today));
    let to = q.to.unwrap_or(from + Duration::days(6));
    if to < from {
        return Err(invalid("to", "before_from", "to must not be before from"));
    }
    if (to - from).num_days() > 61 {
        return Err(invalid("to", "too_long", "at most 62 days at a time"));
    }
    Ok((from, to))
}

fn dates(from: NaiveDate, to: NaiveDate) -> Vec<String> {
    (0..=(to - from).num_days())
        .map(|i| ds(from + Duration::days(i)))
        .collect()
}

/// The roster (`people.read`): shifts in the range with their checks, coverage gaps against
/// the requirements, and each rosterable staff member's hours for the week of `from`.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster", operation_id = "people_roster_board", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), RosterRange), security(("session_cookie" = [])),
    responses((status = 200, body = RosterBoard), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn board(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RosterRange>,
) -> ApiResult<Json<RosterBoard>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let (from, to) = range(&q, today)?;
    let (lo, hi) = window(from, to);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let w = load(&mut conn, ctx.scope.tenant_id, id, lo, hi).await?;
    let ds_list = dates(from, to);
    let in_range: HashSet<&str> = ds_list.iter().map(String::as_str).collect();
    let shifts: Vec<DutyShiftView> = w
        .shifts
        .iter()
        .filter(|s| in_range.contains(s.date.as_str()))
        .map(|s| shift_view(s, &w))
        .collect();
    let open_shifts = shifts.iter().filter(|s| s.staff_id.is_none()).count();
    let week = r::start_of_week(&ds(from));
    let staff = w
        .employees
        .iter()
        .map(|e| {
            let mins = r::weekly_minutes(&w.shifts, &e.id, &week);
            RosterStaffRow {
                staff_id: Uuid::parse_str(&e.id).unwrap_or_default(),
                name: e.name.clone(),
                role: e.role.clone(),
                rosterable: e.active,
                target_hours: e.target_hours,
                max_hours: e.max_hours,
                week_minutes: mins,
                hours_status: r::hours_status(mins, e).into(),
                rule_warnings: r::schedule_rule_violations(
                    &e.id, &w.shifts, &ds_list, &w.rules, &e.name,
                ),
            }
        })
        .collect();
    Ok(Json(RosterBoard {
        from,
        to,
        week_start: pd(&week).unwrap_or(from),
        open_shifts,
        gaps: r::coverage_gaps(&w.shifts, &w.requirements, &ds_list),
        shifts,
        staff,
    }))
}

// ------------------------------------------------------------------ shifts

/// Plan or change a shift.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DutyShiftInput {
    pub date: NaiveDate,
    /// `HH:MM`.
    #[schema(example = "22:00")]
    pub start: String,
    /// `HH:MM`; before `start` runs past midnight.
    #[schema(example = "06:00")]
    pub end: String,
    #[serde(default)]
    pub break_minutes: Option<i32>,
    #[schema(example = "Hostel night warden")]
    pub role: String,
    pub location: Option<String>,
    pub note: Option<String>,
    /// The staff member; null leaves it open.
    pub staff_id: Option<Uuid>,
}

/// A saved shift and the warnings to show.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DutyShiftResult {
    pub shift: DutyShiftView,
    pub warnings: Vec<String>,
}

fn check_input(b: &DutyShiftInput) -> Result<Shift, KernelError> {
    let s = Shift {
        id: String::new(),
        date: ds(b.date),
        start: b.start.clone(),
        end: b.end.clone(),
        break_minutes: b.break_minutes.map(f64::from),
        role: b.role.trim().to_owned(),
        assignee_id: b.staff_id.map(|x| x.to_string()),
    };
    let errors = r::validate_shift(&s);
    if let Some(first) = errors.first() {
        return Err(invalid("shift", "invalid", first));
    }
    if s.role.chars().count() > 80 {
        return Err(invalid("role", "too_long", "at most 80 characters"));
    }
    Ok(s)
}

/// Checks putting `shift` (with its new times) on `staff` on `date`: a block becomes 409; the
/// rest are warnings, plus the working-time limits.
fn judge(original: &Shift, updated: &Shift, w: &World) -> Result<Vec<String>, KernelError> {
    let attendance = HashSet::new();
    let ctx = Context {
        shifts: &w.shifts,
        employees: &w.employees,
        availability: &w.availability,
        attendance: &attendance,
    };
    let to = updated.assignee_id.as_deref();
    let base = Shift {
        date: original.date.clone(),
        assignee_id: original.assignee_id.clone(),
        ..updated.clone()
    };
    let check = r::evaluate_move(&base, &updated.date, to, &ctx, false);
    if let Some(b) = check.blocked {
        return Err(KernelError::Conflict(b));
    }
    let mut warnings = check.warnings;
    if let Some(who) = to {
        if !check.changed {
            if let Some(e) = w.employees.iter().find(|e| e.id == who) {
                if !r::find_conflicts(updated, &w.shifts, Some(who)).is_empty() {
                    return Err(KernelError::Conflict(format!(
                        "{} already has an overlapping shift then.",
                        e.name
                    )));
                }
            }
        }
        if let Some(e) = w.employees.iter().find(|e| e.id == who) {
            warnings.extend(
                r::rule_violations(updated, who, &w.shifts, &w.rules, &e.name)
                    .into_iter()
                    .map(|v| v.message),
            );
        }
    }
    Ok(warnings)
}

async fn shift_result(
    st: &PeopleState,
    t: TenantId,
    inst: Uuid,
    sid: Uuid,
    date: NaiveDate,
    warnings: Vec<String>,
    status: StatusCode,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (lo, hi) = window(date, date);
    let w = load(&mut conn, t, inst, lo, hi).await?;
    let s = w
        .shifts
        .iter()
        .find(|s| s.id == sid.to_string())
        .ok_or(KernelError::NotFound {
            entity: "duty shift",
        })?;
    let view = shift_view(s, &w);
    let v = view.version;
    Ok(with_etag(
        status,
        &DutyShiftResult {
            shift: view,
            warnings,
        },
        Version(v),
    ))
}

fn clean(s: &Option<String>, max: usize, field: &str) -> Result<Option<String>, KernelError> {
    let v = s
        .as_deref()
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(str::to_owned);
    if v.as_ref().is_some_and(|x| x.chars().count() > max) {
        return Err(invalid(
            field,
            "too_long",
            &format!("at most {max} characters"),
        ));
    }
    Ok(v)
}

/// Plan a shift (`staff.manage`; Idempotency-Key). An overlap on the staff member is 409.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/roster/shifts", operation_id = "people_roster_shift_create", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = DutyShiftInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = DutyShiftResult, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn shift_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DutyShiftInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let shift = check_input(&b)?;
    let location = clean(&b.location, 200, "location")?;
    let note = clean(&b.note, 500, "note")?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("roster:{id}"))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let (lo, hi) = window(b.date, b.date);
    let w = load(&mut tx, t, id, lo, hi).await?;
    let original = Shift {
        assignee_id: None,
        ..shift.clone()
    };
    let warnings = judge(&original, &shift, &w)?;
    let sid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.duty_shift (tenant_id, institution_id, shift_date, start_time, end_time, \
         break_minutes, role, location, note, staff_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.date)
    .bind(&shift.start)
    .bind(&shift.end)
    .bind(b.break_minutes.unwrap_or(0))
    .bind(&shift.role)
    .bind(&location)
    .bind(&note)
    .bind(b.staff_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(&mut tx, &scope, "people.duty_shift.create", "duty_shift", sid,
        serde_json::json!({"date": shift.date, "start": shift.start, "end": shift.end, "role": shift.role,
            "staffId": b.staff_id, "warnings": warnings}), &a)
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    shift_result(&st, t, id, sid, b.date, warnings, StatusCode::CREATED).await
}

async fn shift_institution(
    conn: &mut PgConnection,
    t: TenantId,
    sid: Uuid,
) -> Result<(Uuid, NaiveDate, i64, String), KernelError> {
    sqlx::query_as(
        "SELECT institution_id, shift_date, version, status FROM people.duty_shift WHERE id = $1 AND tenant_id = $2",
    )
    .bind(sid)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "duty shift" })
}

/// Change, move or (re)assign a shift (`staff.manage`; If-Match). An overlap is 409.
#[utoipa::path(put, path = "/api/v1/people/roster/shifts/{id}", operation_id = "people_roster_shift_update", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = DutyShiftInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DutyShiftResult, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn shift_update(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(sid): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DutyShiftInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let want = expected!(headers);
    let t = ctx.scope.tenant_id;
    let shift = check_input(&b)?;
    let location = clean(&b.location, 200, "location")?;
    let note = clean(&b.note, 500, "note")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (inst_id, old_date, version, _) = shift_institution(&mut tx, t, sid).await?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("roster:{inst_id}"))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    ensure_version(want, version)?;
    let (lo, hi) = window(old_date.min(b.date), old_date.max(b.date));
    let w = load(&mut tx, t, inst_id, lo, hi).await?;
    let original = w
        .shifts
        .iter()
        .find(|s| s.id == sid.to_string())
        .cloned()
        .ok_or(KernelError::NotFound {
            entity: "duty shift",
        })?;
    let updated = Shift {
        id: original.id.clone(),
        ..shift
    };
    let warnings = judge(&original, &updated, &w)?;
    sqlx::query(
        "UPDATE people.duty_shift SET shift_date = $3, start_time = $4, end_time = $5, break_minutes = $6, \
         role = $7, location = $8, note = $9, staff_id = $10, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(sid)
    .bind(t.as_uuid())
    .bind(b.date)
    .bind(&updated.start)
    .bind(&updated.end)
    .bind(b.break_minutes.unwrap_or(0))
    .bind(&updated.role)
    .bind(&location)
    .bind(&note)
    .bind(b.staff_id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(&mut tx, &scope, "people.duty_shift.update", "duty_shift", sid,
        serde_json::json!({"from": {"date": original.date, "staffId": original.assignee_id},
            "to": {"date": updated.date, "start": updated.start, "end": updated.end, "staffId": b.staff_id},
            "warnings": warnings}), &a)
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    shift_result(&st, t, inst_id, sid, b.date, warnings, StatusCode::OK).await
}

/// Remove a shift (`staff.manage`; If-Match).
#[utoipa::path(delete, path = "/api/v1/people/roster/shifts/{id}", operation_id = "people_roster_shift_delete", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 204), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn shift_delete(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(sid): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let want = expected!(headers);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (inst_id, date, version, _) = shift_institution(&mut tx, t, sid).await?;
    ensure_version(want, version)?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    sqlx::query("DELETE FROM people.duty_shift WHERE id = $1 AND tenant_id = $2")
        .bind(sid)
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift.delete",
        "duty_shift",
        sid,
        serde_json::json!({"date": date}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Ranked staff for a shift.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterSuggestions {
    pub shift_id: Uuid,
    pub items: Vec<Candidate>,
}

/// How many suggestions.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LimitQuery {
    /// 1–20, default 5.
    pub limit: Option<usize>,
}

/// Who could take a shift (`people.read`): blockers excluded (time off, overlap, unavailable,
/// past the weekly max); the rest ranked clean of the limits first, then fully available,
/// then furthest below target, then role match.
#[utoipa::path(get, path = "/api/v1/people/roster/shifts/{id}/suggestions", operation_id = "people_roster_shift_suggestions", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), LimitQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RosterSuggestions), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn suggestions(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(sid): Path<Uuid>,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<RosterSuggestions>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (inst_id, date, _, _) = shift_institution(&mut conn, t, sid).await?;
    let inst = InstitutionId(inst_id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let (lo, hi) = window(date, date);
    let w = load(&mut conn, t, inst_id, lo, hi).await?;
    let s = w
        .shifts
        .iter()
        .find(|s| s.id == sid.to_string())
        .ok_or(KernelError::NotFound {
            entity: "duty shift",
        })?;
    let limit = q.limit.unwrap_or(5).clamp(1, 20);
    let items = r::suggest_assignees(
        s,
        &w.employees,
        &w.shifts,
        &w.availability,
        Some(&w.rules),
        limit,
    );
    Ok(Json(RosterSuggestions {
        shift_id: sid,
        items,
    }))
}

// ------------------------------------------------------------------ fill + publish

/// Fill open shifts in a range.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterFillBody {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// Pin a shift to a staff member, or to null to leave it open.
    #[serde(default)]
    pub choices: BTreeMap<String, Option<Uuid>>,
    /// Write the proposals (default false: preview).
    #[serde(default)]
    pub apply: bool,
}

/// A fill plan.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterFillPlan {
    pub rows: Vec<FillRow>,
    pub total: usize,
    pub filling: usize,
    pub skipped: usize,
    pub nobody: usize,
    /// Shifts assigned (apply only).
    pub applied: usize,
}

/// Propose staff for every open shift in a range (`staff.manage`): each proposal counts against
/// the next, so nobody is double-booked or pushed past their weekly max in one pass. With
/// `apply`, the proposals are written.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/roster/fill", operation_id = "people_roster_fill", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterFillBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterFillPlan), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fill(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<RosterFillBody>,
) -> ApiResult<Json<RosterFillPlan>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let (from, to) = range(
        &RosterRange {
            from: Some(b.from),
            to: Some(b.to),
        },
        b.from,
    )?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("roster:{id}"))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let (lo, hi) = window(from, to);
    let w = load(&mut tx, t, id, lo, hi).await?;
    let choices: BTreeMap<String, Option<String>> = b
        .choices
        .iter()
        .map(|(k, v)| (k.clone(), v.map(|x| x.to_string())))
        .collect();
    let rows = r::plan_fill(
        &w.shifts,
        &w.employees,
        &w.availability,
        Some(&w.rules),
        &dates(from, to),
        4,
        &choices,
    );
    let mut applied = 0;
    if b.apply {
        for row in rows.iter().filter(|r| r.proposal.is_some()) {
            let (Ok(sid), Some(Ok(staff))) = (
                Uuid::parse_str(&row.shift_id),
                row.proposal.as_deref().map(Uuid::parse_str),
            ) else {
                continue;
            };
            sqlx::query(
                "UPDATE people.duty_shift SET staff_id = $3, version = version + 1, updated_at = now() \
                 WHERE id = $1 AND tenant_id = $2 AND staff_id IS NULL",
            )
            .bind(sid)
            .bind(t.as_uuid())
            .bind(staff)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            applied += 1;
        }
        let a = actor(ctx);
        audit(
            &mut tx,
            &scope,
            "people.duty_roster.fill",
            "institution",
            id,
            serde_json::json!({"from": from, "to": to, "applied": applied}),
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(RosterFillPlan {
        total: rows.len(),
        filling: rows.iter().filter(|r| r.proposal.is_some()).count(),
        skipped: rows.iter().filter(|r| r.skipped).count(),
        nobody: rows
            .iter()
            .filter(|r| r.proposal.is_none() && !r.skipped)
            .count(),
        applied,
        rows,
    }))
}

/// How many shifts were published.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RosterPublished {
    pub published: u64,
}

/// Publish the draft shifts in a range (`staff.manage`): staff see published shifts only.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/roster/publish", operation_id = "people_roster_publish", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterRange, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterPublished), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn publish(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<RosterRange>,
) -> ApiResult<Json<RosterPublished>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let (from, to) = range(&b, today)?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let n = sqlx::query(
        "UPDATE people.duty_shift SET status = 'published', version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND institution_id = $2 AND shift_date BETWEEN $3 AND $4 AND status = 'draft'",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(from)
    .bind(to)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_roster.publish",
        "institution",
        id,
        serde_json::json!({"from": from, "to": to, "published": n}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(RosterPublished { published: n }))
}

// ------------------------------------------------------------------ settings + profiles

/// A coverage requirement.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterRequirementInput {
    #[schema(example = "Security")]
    pub role: String,
    /// Weekdays, 0 = Sunday.
    pub days: Vec<u32>,
    pub count: i64,
    /// Optional window (`HH:MM`); the whole day without one.
    pub start: Option<String>,
    pub end: Option<String>,
}

/// Roster settings: working-time limits and coverage requirements.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterSettings {
    pub rules: Rules,
    pub requirements: Vec<RosterRequirementInput>,
}

/// Roster settings (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/settings", operation_id = "people_roster_settings_get", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RosterSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RosterSettings>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let t = ctx.scope.tenant_id;
    Ok(Json(settings_of(&mut conn, t, id).await?))
}

async fn settings_of(
    conn: &mut PgConnection,
    t: TenantId,
    id: Uuid,
) -> Result<RosterSettings, KernelError> {
    let rules = load_rules(conn, t, id).await?;
    let requirements = load_requirements(conn, t, id)
        .await?
        .into_iter()
        .map(|q| RosterRequirementInput {
            role: q.role,
            days: q.days,
            count: q.count,
            start: q.start,
            end: q.end,
        })
        .collect();
    Ok(RosterSettings {
        rules,
        requirements,
    })
}

/// Save roster settings (`staff.manage`): the limits, and the requirements replaced whole.
#[utoipa::path(put, path = "/api/v1/people/institutions/{id}/roster/settings", operation_id = "people_roster_settings_put", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn settings_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<RosterSettings>,
) -> ApiResult<Json<RosterSettings>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let ok = |v: Option<f64>, max: f64| v.is_none_or(|x| x > 0.0 && x <= max);
    if !ok(b.rules.min_rest_hours, 24.0) {
        return Err(invalid(
            "rules.minRestHours",
            "out_of_range",
            "more than 0 and at most 24",
        )
        .into());
    }
    if !ok(b.rules.max_daily_hours, 24.0) {
        return Err(invalid(
            "rules.maxDailyHours",
            "out_of_range",
            "more than 0 and at most 24",
        )
        .into());
    }
    if !b
        .rules
        .max_consecutive_days
        .is_none_or(|x| (1.0..=60.0).contains(&x) && x.fract() == 0.0)
    {
        return Err(invalid(
            "rules.maxConsecutiveDays",
            "out_of_range",
            "a whole number from 1 to 60",
        )
        .into());
    }
    if b.requirements.len() > 50 {
        return Err(invalid("requirements", "too_many", "at most 50").into());
    }
    for (i, q) in b.requirements.iter().enumerate() {
        let f = |k: &str| format!("requirements[{i}].{k}");
        if q.role.trim().is_empty() || q.role.trim().chars().count() > 80 {
            return Err(invalid(&f("role"), "required", "1–80 characters").into());
        }
        if q.days.is_empty() || q.days.iter().any(|d| *d > 6) {
            return Err(invalid(&f("days"), "invalid", "weekdays 0 (Sunday) to 6").into());
        }
        if !(1..=100).contains(&q.count) {
            return Err(invalid(&f("count"), "out_of_range", "1 to 100").into());
        }
        for (k, v) in [("start", &q.start), ("end", &q.end)] {
            if v.as_deref().is_some_and(|x| !r::is_valid_time(x)) {
                return Err(invalid(&f(k), "invalid", "HH:MM").into());
            }
        }
    }
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.roster_settings (tenant_id, institution_id, min_rest_hours, max_daily_hours, \
         max_consecutive_days) VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, institution_id) DO UPDATE \
         SET min_rest_hours = EXCLUDED.min_rest_hours, max_daily_hours = EXCLUDED.max_daily_hours, \
         max_consecutive_days = EXCLUDED.max_consecutive_days, version = people.roster_settings.version + 1, \
         updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(dec(b.rules.min_rest_hours))
    .bind(dec(b.rules.max_daily_hours))
    .bind(b.rules.max_consecutive_days.map(|x| x as i32))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "DELETE FROM people.roster_requirement WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for q in &b.requirements {
        let mut days: Vec<i32> = q.days.iter().map(|d| *d as i32).collect();
        days.sort_unstable();
        days.dedup();
        sqlx::query(
            "INSERT INTO people.roster_requirement (tenant_id, institution_id, role, days, head_count, \
             start_time, end_time) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(q.role.trim())
        .bind(&days)
        .bind(q.count as i32)
        .bind(q.start.as_deref().filter(|x| !x.is_empty()))
        .bind(q.end.as_deref().filter(|x| !x.is_empty()))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.roster_settings.update",
        "institution",
        id,
        serde_json::to_value(&b).unwrap_or_default(),
        &a,
    )
    .await?;
    let out = settings_of(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

/// A staff member's roster profile.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterProfile {
    /// Their usual duty (matched against shift roles when suggesting).
    pub role: Option<String>,
    pub target_hours: Option<f64>,
    pub max_hours: Option<f64>,
    /// Seven windows, Sunday first; null = available any time.
    pub days: Option<Vec<Option<DayWindow>>>,
    /// False keeps them off the roster.
    #[serde(default = "yes")]
    pub rosterable: bool,
}

fn yes() -> bool {
    true
}

/// Set a staff member's roster profile (`staff.manage`).
#[utoipa::path(put, path = "/api/v1/people/staff/{id}/roster-profile", operation_id = "people_roster_profile_put", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterProfile, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterProfile), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn profile_put(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(staff): Path<Uuid>,
    Json(b): Json<RosterProfile>,
) -> ApiResult<Json<RosterProfile>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let inst_id: Uuid = sqlx::query_scalar(
        "SELECT institution_id FROM people.staff_member WHERE id = $1 AND tenant_id = $2",
    )
    .bind(staff)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "staff member",
    })?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if b.target_hours.is_some_and(|x| !(0.0..=168.0).contains(&x)) {
        return Err(invalid("targetHours", "out_of_range", "0 to 168").into());
    }
    if b.max_hours.is_some_and(|x| x <= 0.0 || x > 168.0) {
        return Err(invalid("maxHours", "out_of_range", "more than 0 and at most 168").into());
    }
    if let Some(days) = &b.days {
        if days.len() != 7 {
            return Err(invalid("days", "invalid", "seven windows, Sunday first").into());
        }
        for (i, d) in days.iter().enumerate() {
            if let Some(w) = d.as_ref().filter(|w| !w.off) {
                if !r::is_valid_time(&w.start) || !r::is_valid_time(&w.end) {
                    return Err(invalid(
                        &format!("days[{i}]"),
                        "invalid",
                        "start and end as HH:MM",
                    )
                    .into());
                }
            }
        }
    }
    let role = clean(&b.role, 80, "role")?;
    sqlx::query(
        "INSERT INTO people.roster_staff_profile (staff_id, tenant_id, institution_id, role, target_hours, \
         max_hours, days, rosterable) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) ON CONFLICT (staff_id) DO UPDATE \
         SET role = EXCLUDED.role, target_hours = EXCLUDED.target_hours, max_hours = EXCLUDED.max_hours, \
         days = EXCLUDED.days, rosterable = EXCLUDED.rosterable, version = people.roster_staff_profile.version + 1, \
         updated_at = now()",
    )
    .bind(staff)
    .bind(t.as_uuid())
    .bind(inst_id)
    .bind(&role)
    .bind(dec(b.target_hours))
    .bind(dec(b.max_hours))
    .bind(b.days.as_ref().map(|d| serde_json::to_value(d).unwrap_or_default()))
    .bind(b.rosterable)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.roster_profile.update",
        "staff_member",
        staff,
        serde_json::to_value(&b).unwrap_or_default(),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(RosterProfile { role, ..b }))
}

// ------------------------------------------------------------------ self-service

/// The caller's published duties.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyRoster {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub items: Vec<DutyShiftView>,
    pub minutes: f64,
}

/// My duties (`people.self_service`): published shifts I hold in the range (default this week).
#[utoipa::path(get, path = "/api/v1/people/me/roster", operation_id = "people_roster_me", tag = "people-roster",
    params(RosterRange), security(("session_cookie" = [])),
    responses((status = 200, body = MyRoster), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Query(q): Query<RosterRange>,
) -> ApiResult<Json<MyRoster>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = me.institution_id;
    let inst_id = inst.as_uuid();
    let today = business_today(&st, &institution_scope(ctx, inst), inst).await?;
    let (from, to) = range(&q, today)?;
    let (lo, hi) = window(from, to);
    let w = load(&mut conn, t, inst_id, lo, hi).await?;
    let mine = me.id.as_uuid().to_string();
    let items: Vec<DutyShiftView> = w
        .shifts
        .iter()
        .filter(|s| {
            s.assignee_id.as_deref() == Some(mine.as_str())
                && s.date >= ds(from)
                && s.date <= ds(to)
                && w.meta.get(&s.id).is_some_and(|m| m.status == "published")
        })
        .map(|s| shift_view(s, &w))
        .collect();
    let minutes = items.iter().map(|s| s.net_minutes).sum();
    Ok(Json(MyRoster {
        from,
        to,
        items,
        minutes,
    }))
}

/// Routes of the duty roster.
pub fn router(st: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(board))
        .routes(routes!(shift_create))
        .routes(routes!(shift_update, shift_delete))
        .routes(routes!(suggestions))
        .routes(routes!(fill))
        .routes(routes!(publish))
        .routes(routes!(settings_get, settings_put))
        .routes(routes!(profile_put))
        .routes(routes!(me))
        .with_state(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_and_windows() {
        let d = |s: &str| pd(s).unwrap();
        let (f, t) = range(&RosterRange::default(), d("2026-10-17")).unwrap();
        assert_eq!(
            (ds(f), ds(t)),
            ("2026-10-12".to_owned(), "2026-10-18".to_owned())
        );
        assert!(range(
            &RosterRange {
                from: Some(d("2026-10-01")),
                to: Some(d("2026-12-31"))
            },
            f
        )
        .is_err());
        let (lo, hi) = window(d("2026-10-14"), d("2026-10-14"));
        assert_eq!(
            (ds(lo), ds(hi)),
            ("2026-10-10".to_owned(), "2026-10-20".to_owned())
        );
        assert_eq!(dates(d("2026-10-12"), d("2026-10-14")).len(), 3);
    }
}
