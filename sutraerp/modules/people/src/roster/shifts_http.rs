//! **ERP_EXTENSIONS 5B** — shift swaps, open-shift claims, clock punches, timesheets, the
//! payroll export, roster reports and the staff calendar over the 3A duty roster (rules in
//! [`super::swaps`], [`super::clock`], [`super::timesheet`], [`super::reports`],
//! [`super::ics`], ported from HeyPuter/everything shifts, MIT). Operation ids
//! `people_roster_*`, tag `people-roster`.
//!
//! * A staff member asks to give a shift away, swap it for a colleague's, or have it covered
//!   (anyone may accept); or claims an open shift. The colleague agrees, then a manager
//!   (`people.staff.manage`, never the requester or the colleague) approves and the roster
//!   changes in one transaction under the roster lock. An overlap, an archived person, a shift
//!   that changed hands meanwhile or an approved timesheet refuse the approval (409); time off,
//!   availability, the weekly maximum, the working-time limits and the usual duty are warnings.
//! * Staff clock in and out of their published shifts (server time, institution timezone);
//!   HR records or corrects punches. Punches are reconciled with the 7B attendance day into a
//!   weekly or monthly timesheet (late, early leave, missed punch, overtime, night and holiday
//!   shifts, loss of pay). Staff submit it; a manager approves and locks it.
//! * The payroll export (`people.payroll.manage`) hands approved totals to 7C as variable
//!   inputs every payroll calculation reads, and as a CSV.
#![allow(clippy::type_complexity, clippy::too_many_arguments)]

use super::clock::{self as ck, Record};
use super::http::{clean, ds, fo, load, pd, range, window, RosterRange, World};
use super::ics::{build_schedule_ics, CalendarShift};
use super::reports::{build_report, previous_range, report_csv, State};
use super::rules::{self as r, Context, Shift};
use super::swaps::{self as sw, Kind};
use super::timesheet::{
    payroll_quantities, reconcile, round2, timesheet_csv, to_csv, totals, Actual, Cell, Limits,
    Line, PayEmployee, Totals,
};
use crate::leave::common::{audit, ensure_version, expected, invalid, LeaveDecisionBody};
use crate::shared::approval::ensure_independent_decider;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_staff;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::PeopleState;
use axum::extract::{Path, Query, State as AxState};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::{BTreeMap, HashMap, HashSet};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ helpers

async fn lock_roster(conn: &mut PgConnection, inst: Uuid) -> Result<(), KernelError> {
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("roster:{inst}"))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// Today and the wall-clock time (`HH:MM`) in the institution's timezone.
async fn local_now(
    st: &PeopleState,
    scope: &ExecutionScope,
    inst: InstitutionId,
) -> Result<(NaiveDate, String), KernelError> {
    let i = st
        .references
        .institution(scope, inst)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "institution",
        })?;
    let (d, t): (NaiveDate, String) = sqlx::query_as(
        "SELECT (now() AT TIME ZONE $1)::date, to_char(now() AT TIME ZONE $1, 'HH24:MI')",
    )
    .bind(&i.timezone)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok((d, t))
}

/// `(institution, date, staff, status)` of a shift of the tenant.
async fn shift_row(
    conn: &mut PgConnection,
    t: TenantId,
    sid: Uuid,
) -> Result<(Uuid, NaiveDate, Option<Uuid>, String), KernelError> {
    sqlx::query_as(
        "SELECT institution_id, shift_date, staff_id, status FROM people.duty_shift WHERE id = $1 AND tenant_id = $2",
    )
    .bind(sid)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "duty shift",
    })
}

async fn subject_of_staff(
    conn: &mut PgConnection,
    staff: Option<Uuid>,
) -> Result<Option<String>, KernelError> {
    let Some(s) = staff else { return Ok(None) };
    let v: Option<Option<String>> =
        sqlx::query_scalar("SELECT subject_ref FROM people.staff_member WHERE id = $1")
            .bind(s)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
    Ok(v.flatten())
}

/// 409 when an approved or locked timesheet of `staff` covers `date`.
async fn ensure_open_period(
    conn: &mut PgConnection,
    t: TenantId,
    staff: Uuid,
    date: NaiveDate,
) -> Result<(), KernelError> {
    let locked: Option<String> = sqlx::query_scalar(
        "SELECT status FROM people.timesheet WHERE tenant_id = $1 AND staff_id = $2 \
         AND status IN ('approved', 'locked') AND $3 BETWEEN period_from AND period_to LIMIT 1",
    )
    .bind(t.as_uuid())
    .bind(staff)
    .bind(date)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match locked {
        Some(s) => Err(KernelError::Conflict(format!(
            "the timesheet covering {date} is {s}; reopen it before changing this shift"
        ))),
        None => Ok(()),
    }
}

fn find(w: &World, sid: Uuid) -> Result<&Shift, KernelError> {
    w.shifts
        .iter()
        .find(|s| s.id == sid.to_string())
        .ok_or(KernelError::NotFound {
            entity: "duty shift",
        })
}

fn published(w: &World, sid: &str) -> bool {
    w.meta.get(sid).is_some_and(|m| m.status == "published")
}

/// Checks the reassignments of a request in order on a working copy of the roster: a block
/// (overlap, archived, gone) is 409; Everything's swap warnings (availability, weekly maximum,
/// working-time limits, attendance already recorded) plus time off and the usual duty come back.
fn assess(
    w: &World,
    moves: &[(String, Option<String>, String)],
) -> Result<Vec<String>, KernelError> {
    let mut working = w.shifts.clone();
    let mut warnings: Vec<String> = Vec::new();
    for m in moves {
        let shift = working
            .iter()
            .find(|s| s.id == m.0)
            .cloned()
            .ok_or(KernelError::NotFound {
                entity: "duty shift",
            })?;
        let ctx = Context {
            shifts: &working,
            employees: &w.employees,
            availability: &w.availability,
            attendance: &w.punched,
        };
        let check = r::evaluate_move(&shift, &shift.date, Some(&m.2), &ctx, false);
        if let Some(b) = check.blocked {
            return Err(KernelError::Conflict(b));
        }
        let ev = sw::evaluate_swap(
            &shift,
            &m.2,
            &working,
            Some(&w.availability),
            Some(&w.employees),
            Some(&w.punched),
            Some(&w.rules),
        );
        let mut found = ev.warnings;
        found.extend(
            check
                .warnings
                .into_iter()
                .filter(|x| x.contains("time off")),
        );
        if let Some(e) = w.employees.iter().find(|e| e.id == m.2) {
            found.extend(sw::duty_mismatch(&shift, e));
        }
        for f in found {
            if !warnings.contains(&f) {
                warnings.push(f);
            }
        }
        working = sw::apply_moves(&working, std::slice::from_ref(m));
    }
    Ok(warnings)
}

// ------------------------------------------------------------------ swap views

/// A swap, cover, give-away or open-shift claim.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterSwapView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub kind: Kind,
    /// `awaiting_peer | awaiting_manager | approved | declined | cancelled`.
    pub status: String,
    pub shift_id: Option<Uuid>,
    pub other_shift_id: Option<Uuid>,
    pub from_staff_id: Option<Uuid>,
    pub from_staff_name: Option<String>,
    pub to_staff_id: Option<Uuid>,
    pub to_staff_name: Option<String>,
    pub reason: Option<String>,
    /// `{ shift: {date, start, end, role}, other: {…} | null }` as asked.
    #[schema(value_type = Object)]
    pub snapshot: Value,
    pub warnings: Vec<String>,
    pub requested_at: DateTime<Utc>,
    pub peer_responded_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub version: i64,
}

/// A list of requests.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RosterSwapList {
    pub items: Vec<RosterSwapView>,
}

struct SwapRow {
    view: RosterSwapView,
    requested_by: String,
    peer_responded_by: Option<String>,
}

const SWAP_SELECT: &str = "SELECT w.id, w.institution_id, w.kind, w.status, w.shift_id, w.other_shift_id, \
     w.from_staff_id, f.display_name AS from_name, w.to_staff_id, o.display_name AS to_name, w.reason, w.snapshot, \
     w.warnings, w.created_at, w.peer_responded_at, w.decided_at, w.decision_reason, w.version, w.requested_by, \
     w.peer_responded_by FROM people.duty_shift_swap w \
     LEFT JOIN people.staff_member f ON f.id = w.from_staff_id \
     LEFT JOIN people.staff_member o ON o.id = w.to_staff_id";

#[derive(sqlx::FromRow)]
struct SwapTuple {
    id: Uuid,
    institution_id: Uuid,
    kind: String,
    status: String,
    shift_id: Option<Uuid>,
    other_shift_id: Option<Uuid>,
    from_staff_id: Option<Uuid>,
    from_name: Option<String>,
    to_staff_id: Option<Uuid>,
    to_name: Option<String>,
    reason: Option<String>,
    snapshot: Value,
    warnings: Value,
    created_at: DateTime<Utc>,
    peer_responded_at: Option<DateTime<Utc>>,
    decided_at: Option<DateTime<Utc>>,
    decision_reason: Option<String>,
    version: i64,
    requested_by: String,
    peer_responded_by: Option<String>,
}

fn swap_of(t: SwapTuple) -> SwapRow {
    SwapRow {
        view: RosterSwapView {
            id: t.id,
            institution_id: t.institution_id,
            kind: Kind::parse(&t.kind).unwrap_or(Kind::GiveAway),
            status: t.status,
            shift_id: t.shift_id,
            other_shift_id: t.other_shift_id,
            from_staff_id: t.from_staff_id,
            from_staff_name: t.from_name,
            to_staff_id: t.to_staff_id,
            to_staff_name: t.to_name,
            reason: t.reason,
            snapshot: t.snapshot,
            warnings: serde_json::from_value(t.warnings).unwrap_or_default(),
            requested_at: t.created_at,
            peer_responded_at: t.peer_responded_at,
            decided_at: t.decided_at,
            decision_reason: t.decision_reason,
            version: t.version,
        },
        requested_by: t.requested_by,
        peer_responded_by: t.peer_responded_by,
    }
}

async fn swap_row(conn: &mut PgConnection, t: TenantId, id: Uuid) -> Result<SwapRow, KernelError> {
    let row: Option<SwapTuple> = sqlx::query_as(&format!(
        "{SWAP_SELECT} WHERE w.id = $1 AND w.tenant_id = $2"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.map(swap_of).ok_or(KernelError::NotFound {
        entity: "shift swap request",
    })
}

fn swap_response(row: &SwapRow, status: StatusCode) -> Response {
    with_etag(status, &row.view, Version(row.view.version))
}

fn snap(s: &Shift) -> Value {
    json!({"date": s.date, "start": s.start, "end": s.end, "role": s.role})
}

fn uid(s: &str) -> Option<Uuid> {
    Uuid::parse_str(s).ok()
}

// ------------------------------------------------------------------ requests (self-service)

/// Ask for a shift to change hands.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterSwapRequestInput {
    /// One of my published, upcoming shifts.
    pub shift_id: Uuid,
    /// `give_away | swap | cover` (claims use `…/claim`).
    pub kind: Kind,
    /// The colleague (required for `give_away` and `swap`; optional for `cover`).
    pub to_staff_id: Option<Uuid>,
    /// Their shift I take in exchange (`swap` only).
    pub other_shift_id: Option<Uuid>,
    pub reason: Option<String>,
}

/// Ask a colleague to take, swap or cover one of my shifts (`people.self`; Idempotency-Key). An
/// overlap the move would cause is refused (409); the rest are warnings for the manager.
#[utoipa::path(post, path = "/api/v1/people/me/roster/swaps", operation_id = "people_roster_swap_request", tag = "people-roster",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RosterSwapRequestInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RosterSwapView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn swap_request(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<RosterSwapRequestInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let me = my_staff(&st, ctx).await?;
    let inst = me.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if b.kind == Kind::Claim {
        return Err(invalid("kind", "invalid", "claim an open shift through …/claim").into());
    }
    let reason = clean(&b.reason, 500, "reason")?;
    let t = ctx.scope.tenant_id;
    let mine = me.id.as_uuid().to_string();
    let today = business_today(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_roster(&mut tx, inst.as_uuid()).await?;
    let (sinst, date, _, _) = shift_row(&mut tx, t, b.shift_id).await?;
    if sinst != inst.as_uuid() {
        return Err(KernelError::NotFound {
            entity: "duty shift",
        }
        .into());
    }
    let mut lo_hi = vec![date];
    if let Some(o) = b.other_shift_id {
        let (oinst, odate, _, _) = shift_row(&mut tx, t, o).await?;
        if oinst != inst.as_uuid() {
            return Err(KernelError::NotFound {
                entity: "duty shift",
            }
            .into());
        }
        lo_hi.push(odate);
    }
    let (lo, hi) = window(
        *lo_hi.iter().min().unwrap_or(&date),
        *lo_hi.iter().max().unwrap_or(&date),
    );
    let w = load(&mut tx, t, inst.as_uuid(), lo, hi).await?;
    let shift = find(&w, b.shift_id)?.clone();
    if shift.assignee_id.as_deref() != Some(mine.as_str()) || !published(&w, &shift.id) {
        return Err(
            KernelError::Conflict("you can only offer your own published shifts".into()).into(),
        );
    }
    if date < today {
        return Err(
            KernelError::Conflict("that shift has already started or passed".into()).into(),
        );
    }
    let to = b.to_staff_id.map(|x| x.to_string());
    if matches!(b.kind, Kind::GiveAway | Kind::Swap) || to.is_some() {
        // Everything's request rules: an assigned shift, a colleague, not myself.
        sw::create_swap_request(
            Some(&shift),
            to.as_deref(),
            reason.as_deref().unwrap_or(""),
            None,
            None,
        )
        .map_err(KernelError::Conflict)?;
        if let Some(x) = &to {
            if !w.employees.iter().any(|e| &e.id == x && e.active) {
                return Err(
                    KernelError::Conflict("that colleague is not on the roster".into()).into(),
                );
            }
        }
    }
    let other = match (b.kind, b.other_shift_id) {
        (Kind::Swap, Some(o)) => {
            let os = find(&w, o)?.clone();
            if os.assignee_id != to || !published(&w, &os.id) || os.date < ds(today) {
                return Err(KernelError::Conflict(
                    "the shift offered in exchange must be the colleague's upcoming published shift".into(),
                )
                .into());
            }
            Some(os)
        }
        (Kind::Swap, None) => {
            return Err(invalid(
                "otherShiftId",
                "required",
                "a swap names the shift you take",
            )
            .into())
        }
        _ => None,
    };
    let warnings = match &to {
        Some(x) => {
            let moves = sw::moves(b.kind, &shift, other.as_ref(), Some(&mine), x)
                .map_err(KernelError::Conflict)?;
            assess(&w, &moves)?
        }
        None => Vec::new(),
    };
    let snapshot = json!({"shift": snap(&shift), "other": other.as_ref().map(snap)});
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.duty_shift_swap (tenant_id, institution_id, kind, shift_id, other_shift_id, \
         from_staff_id, to_staff_id, reason, snapshot, status, warnings, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(b.kind.as_str())
    .bind(b.shift_id)
    .bind(other.as_ref().and_then(|o| uid(&o.id)))
    .bind(me.id.as_uuid())
    .bind(b.to_staff_id)
    .bind(&reason)
    .bind(&snapshot)
    .bind(sw::initial_status(b.kind))
    .bind(json!(warnings))
    .bind(&ctx.subject)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift_swap.request",
        "duty_shift_swap",
        id,
        json!({"kind": b.kind.as_str(), "shiftId": b.shift_id, "toStaffId": b.to_staff_id,
            "otherShiftId": b.other_shift_id, "warnings": warnings}),
        &a,
    )
    .await?;
    let row = swap_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(swap_response(&row, StatusCode::CREATED))
}

/// Why I want the open shift.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct RosterClaimInput {
    pub reason: Option<String>,
}

/// Claim an open, published, upcoming shift (`people.self`; Idempotency-Key): a manager
/// approves it. An overlap with my own shifts is refused (409).
#[utoipa::path(post, path = "/api/v1/people/me/roster/shifts/{id}/claim", operation_id = "people_roster_shift_claim", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RosterClaimInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RosterSwapView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn claim(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(sid): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RosterClaimInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let me = my_staff(&st, ctx).await?;
    let inst = me.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let reason = clean(&b.reason, 500, "reason")?;
    let t = ctx.scope.tenant_id;
    let mine = me.id.as_uuid().to_string();
    let today = business_today(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_roster(&mut tx, inst.as_uuid()).await?;
    let (sinst, date, _, _) = shift_row(&mut tx, t, sid).await?;
    if sinst != inst.as_uuid() {
        return Err(KernelError::NotFound {
            entity: "duty shift",
        }
        .into());
    }
    let (lo, hi) = window(date, date);
    let w = load(&mut tx, t, inst.as_uuid(), lo, hi).await?;
    let shift = find(&w, sid)?.clone();
    if !published(&w, &shift.id) || date < today {
        return Err(
            KernelError::Conflict("only upcoming published shifts can be claimed".into()).into(),
        );
    }
    let moves = sw::moves(Kind::Claim, &shift, None, None, &mine).map_err(KernelError::Conflict)?;
    let warnings = assess(&w, &moves)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.duty_shift_swap (tenant_id, institution_id, kind, shift_id, to_staff_id, reason, \
         snapshot, status, warnings, requested_by) VALUES ($1, $2, 'claim', $3, $4, $5, $6, 'awaiting_manager', $7, $8) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(sid)
    .bind(me.id.as_uuid())
    .bind(&reason)
    .bind(json!({"shift": snap(&shift), "other": null}))
    .bind(json!(warnings))
    .bind(&ctx.subject)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift_swap.claim",
        "duty_shift_swap",
        id,
        json!({"shiftId": sid, "warnings": warnings}),
        &a,
    )
    .await?;
    let row = swap_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(swap_response(&row, StatusCode::CREATED))
}

/// A colleague's answer.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RosterSwapRespond {
    pub accept: bool,
    pub reason: Option<String>,
}

/// Agree to (or turn down) a request addressed to me, or take an open cover request
/// (`people.self`; If-Match). Agreeing sends it to a manager.
#[utoipa::path(post, path = "/api/v1/people/roster/swaps/{id}/respond", operation_id = "people_roster_swap_respond", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = RosterSwapRespond, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterSwapView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn swap_respond(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RosterSwapRespond>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let want = expected!(headers);
    let me = my_staff(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let reason = clean(&b.reason, 500, "reason")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = swap_row(&mut tx, t, id).await?;
    let v = &row.view;
    let inst = InstitutionId(v.institution_id);
    if inst != me.institution_id {
        return Err(KernelError::NotFound {
            entity: "shift swap request",
        }
        .into());
    }
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    lock_roster(&mut tx, v.institution_id).await?;
    ensure_version(want, v.version)?;
    let my_id = me.id.as_uuid();
    if v.status != sw::AWAITING_PEER {
        return Err(KernelError::Conflict(format!("this request is {}", v.status)).into());
    }
    if v.from_staff_id == Some(my_id) || v.to_staff_id.is_some_and(|x| x != my_id) {
        return Err(KernelError::Forbidden("this request is not addressed to you".into()).into());
    }
    let mut warnings = v.warnings.clone();
    let status = if b.accept {
        let sid = v
            .shift_id
            .ok_or_else(|| KernelError::Conflict("the shift no longer exists".into()))?;
        let (_, date, _, _) = shift_row(&mut tx, t, sid).await?;
        let mut d = vec![date];
        if let Some(o) = v.other_shift_id {
            d.push(shift_row(&mut tx, t, o).await?.1);
        }
        let (lo, hi) = window(
            *d.iter().min().unwrap_or(&date),
            *d.iter().max().unwrap_or(&date),
        );
        let w = load(&mut tx, t, v.institution_id, lo, hi).await?;
        let shift = find(&w, sid)?.clone();
        let other = match v.other_shift_id {
            Some(o) => Some(find(&w, o)?.clone()),
            None => None,
        };
        let moves = sw::moves(
            v.kind,
            &shift,
            other.as_ref(),
            v.from_staff_id.map(|x| x.to_string()).as_deref(),
            &my_id.to_string(),
        )
        .map_err(KernelError::Conflict)?;
        warnings = assess(&w, &moves)?;
        sw::AWAITING_MANAGER
    } else {
        sw::DECLINED
    };
    sqlx::query(
        "UPDATE people.duty_shift_swap SET status = $3, to_staff_id = COALESCE(to_staff_id, $4), \
         peer_responded_by = $5, peer_responded_at = now(), warnings = $6, \
         decision_reason = CASE WHEN $3 = 'declined' THEN $7 ELSE decision_reason END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(status)
    .bind(if b.accept { Some(my_id) } else { None })
    .bind(&ctx.subject)
    .bind(json!(warnings))
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift_swap.respond",
        "duty_shift_swap",
        id,
        json!({"accept": b.accept, "warnings": warnings}),
        &a,
    )
    .await?;
    let row = swap_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(swap_response(&row, StatusCode::OK))
}

/// Open shifts I could claim.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterOpenShifts {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub items: Vec<super::http::DutyShiftView>,
}

/// Open published shifts of my institution from today (default: the next four weeks), with the
/// checks against my own roster (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/roster/open-shifts", operation_id = "people_roster_open_shifts", tag = "people-roster",
    params(RosterRange), security(("session_cookie" = [])),
    responses((status = 200, body = RosterOpenShifts), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn open_shifts(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Query(q): Query<RosterRange>,
) -> ApiResult<Json<RosterOpenShifts>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let inst = me.institution_id;
    let today = business_today(&st, &institution_scope(ctx, inst), inst).await?;
    let from = q.from.unwrap_or(today).max(today);
    let (from, to) = range(
        &RosterRange {
            from: Some(from),
            to: Some(q.to.unwrap_or(from + Duration::days(27))),
        },
        today,
    )?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (lo, hi) = window(from, to);
    let w = load(&mut conn, ctx.scope.tenant_id, inst.as_uuid(), lo, hi).await?;
    let items = w
        .shifts
        .iter()
        .filter(|s| {
            s.assignee_id.is_none()
                && published(&w, &s.id)
                && s.date >= ds(from)
                && s.date <= ds(to)
        })
        .map(|s| super::http::shift_view(s, &w))
        .collect();
    Ok(Json(RosterOpenShifts { from, to, items }))
}

/// Withdraw my own pending request (`people.self`; If-Match).
#[utoipa::path(post, path = "/api/v1/people/roster/swaps/{id}/cancel", operation_id = "people_roster_swap_cancel", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterSwapView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn swap_cancel(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let want = expected!(headers);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = swap_row(&mut tx, t, id).await?;
    if row.requested_by != ctx.subject {
        return Err(
            KernelError::Forbidden("only the requester can withdraw a request".into()).into(),
        );
    }
    let inst = InstitutionId(row.view.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    ensure_version(want, row.view.version)?;
    if !sw::is_pending(&row.view.status) {
        return Err(KernelError::Conflict("This request has already been resolved.".into()).into());
    }
    sqlx::query(
        "UPDATE people.duty_shift_swap SET status = 'cancelled', version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift_swap.cancel",
        "duty_shift_swap",
        id,
        json!({}),
        &a,
    )
    .await?;
    let row = swap_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(swap_response(&row, StatusCode::OK))
}

/// My requests (as requester or colleague) and open cover requests I could take
/// (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/roster/swaps", operation_id = "people_roster_swap_mine", tag = "people-roster",
    security(("session_cookie" = [])),
    responses((status = 200, body = RosterSwapList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn swap_mine(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<RosterSwapList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<SwapTuple> = sqlx::query_as(&format!(
        "{SWAP_SELECT} WHERE w.tenant_id = $1 AND w.institution_id = $2 AND (w.from_staff_id = $3 \
         OR w.to_staff_id = $3 OR (w.status = 'awaiting_peer' AND w.to_staff_id IS NULL)) \
         ORDER BY w.created_at DESC LIMIT 200"
    ))
    .bind(t.as_uuid())
    .bind(me.institution_id.as_uuid())
    .bind(me.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Json(RosterSwapList {
        items: rows.into_iter().map(|x| swap_of(x).view).collect(),
    }))
}

/// Filter of the request list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct SwapQuery {
    /// `awaiting_peer | awaiting_manager | approved | declined | cancelled`; default all.
    pub status: Option<String>,
}

/// Requests of the institution, newest first (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/swaps", operation_id = "people_roster_swap_list", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), SwapQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RosterSwapList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn swap_list(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<SwapQuery>,
) -> ApiResult<Json<RosterSwapList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<SwapTuple> = sqlx::query_as(&format!(
        "{SWAP_SELECT} WHERE w.tenant_id = $1 AND w.institution_id = $2 AND ($3::text IS NULL OR w.status = $3) \
         ORDER BY w.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.status.as_deref().filter(|s| !s.is_empty()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Json(RosterSwapList {
        items: rows.into_iter().map(|x| swap_of(x).view).collect(),
    }))
}

/// Approve or decline a request agreed by the colleague (`people.staff.manage`; If-Match;
/// never the requester or the colleague). Approving re-checks the roster and reassigns the
/// shift(s) atomically; an overlap, a shift that changed hands or an approved timesheet is 409.
#[utoipa::path(post, path = "/api/v1/people/roster/swaps/{id}/decision", operation_id = "people_roster_swap_decide", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterSwapView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn swap_decide(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let want = expected!(headers);
    let t = ctx.scope.tenant_id;
    let reason = clean(&b.reason, 500, "reason")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = swap_row(&mut tx, t, id).await?;
    let v = row.view.clone();
    let inst = InstitutionId(v.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    lock_roster(&mut tx, v.institution_id).await?;
    ensure_version(want, v.version)?;
    if v.status != sw::AWAITING_MANAGER {
        return Err(KernelError::Conflict(format!(
            "this request is {}; only an agreed request can be decided",
            v.status
        ))
        .into());
    }
    let from_subject = subject_of_staff(&mut tx, v.from_staff_id).await?;
    let to_subject = subject_of_staff(&mut tx, v.to_staff_id).await?;
    ensure_independent_decider(
        &mut tx,
        t,
        "duty_shift_swap",
        id,
        &[],
        &[
            Some(row.requested_by.as_str()),
            row.peer_responded_by.as_deref(),
            from_subject.as_deref(),
            to_subject.as_deref(),
        ],
        Some(&ctx.subject),
        "shift swap",
    )
    .await?;
    let mut warnings = v.warnings.clone();
    let status = if b.approve() {
        let sid = v.shift_id.ok_or_else(|| {
            KernelError::Conflict("The shift in this request no longer exists.".into())
        })?;
        let to = v
            .to_staff_id
            .ok_or_else(|| KernelError::Conflict("nobody has agreed to take the shift".into()))?;
        let (_, date, _, _) = shift_row(&mut tx, t, sid).await?;
        let mut d = vec![date];
        if let Some(o) = v.other_shift_id {
            d.push(shift_row(&mut tx, t, o).await?.1);
        }
        let (lo, hi) = window(
            *d.iter().min().unwrap_or(&date),
            *d.iter().max().unwrap_or(&date),
        );
        let w = load(&mut tx, t, v.institution_id, lo, hi).await?;
        let shift = find(&w, sid)?.clone();
        let other = match v.other_shift_id {
            Some(o) => Some(
                find(&w, o)
                    .map_err(|_| {
                        KernelError::Conflict(
                            "The shift offered in exchange no longer exists.".into(),
                        )
                    })?
                    .clone(),
            ),
            None => None,
        };
        let moves = sw::moves(
            v.kind,
            &shift,
            other.as_ref(),
            v.from_staff_id.map(|x| x.to_string()).as_deref(),
            &to.to_string(),
        )
        .map_err(KernelError::Conflict)?;
        warnings = assess(&w, &moves)?;
        for (sid, from, to) in &moves {
            let s = find(&w, uid(sid).unwrap_or_default())?;
            let date = pd(&s.date).unwrap_or(date);
            for who in [from.as_deref(), Some(to.as_str())].into_iter().flatten() {
                ensure_open_period(&mut tx, t, uid(who).unwrap_or_default(), date).await?;
            }
        }
        for (sid, from, to) in &moves {
            let (sid, to) = (uid(sid).unwrap_or_default(), uid(to).unwrap_or_default());
            let n = sqlx::query(
                "UPDATE people.duty_shift SET staff_id = $3, version = version + 1, updated_at = now() \
                 WHERE id = $1 AND tenant_id = $2 AND staff_id IS NOT DISTINCT FROM $4",
            )
            .bind(sid)
            .bind(t.as_uuid())
            .bind(to)
            .bind(from.as_deref().and_then(uid))
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?
            .rows_affected();
            if n != 1 {
                return Err(KernelError::Conflict(
                    "The shift has been reassigned since this request was made.".into(),
                )
                .into());
            }
            // Attendance is kept per shift: it goes with the shift (Everything's rule).
            sqlx::query("UPDATE people.duty_shift_punch SET staff_id = $2, updated_at = now() WHERE shift_id = $1")
                .bind(sid)
                .bind(to)
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
        }
        sw::APPROVED
    } else {
        sw::DECLINED
    };
    sqlx::query(
        "UPDATE people.duty_shift_swap SET status = $3, decided_by = $4, decided_at = now(), decision_reason = $5, \
         warnings = $6, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(status)
    .bind(&ctx.subject)
    .bind(&reason)
    .bind(json!(warnings))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift_swap.decide",
        "duty_shift_swap",
        id,
        json!({"decision": status, "reason": reason, "warnings": warnings}),
        &a,
    )
    .await?;
    let row = swap_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(swap_response(&row, StatusCode::OK))
}

// ------------------------------------------------------------------ punches

/// A shift's clock record.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterPunchView {
    pub shift_id: Uuid,
    pub staff_id: Uuid,
    /// `present | late | absent`.
    pub status: String,
    pub clock_in: Option<String>,
    pub clock_out: Option<String>,
    pub note: Option<String>,
    /// `self | manager`.
    pub source: String,
    /// Minutes late against the scheduled start.
    pub late_minutes: i64,
    /// Worked minutes so far (both punches) net of nothing.
    pub worked_minutes: Option<i64>,
    pub version: i64,
}

async fn punch_view(
    conn: &mut PgConnection,
    sid: Uuid,
) -> Result<Option<RosterPunchView>, KernelError> {
    let row: Option<(Uuid, String, Option<String>, Option<String>, Option<String>, String, i64, String, String)> =
        sqlx::query_as(
            "SELECT p.staff_id, p.status, p.clock_in, p.clock_out, p.note, p.source, p.version, s.start_time, s.end_time \
             FROM people.duty_shift_punch p JOIN people.duty_shift s ON s.id = p.shift_id WHERE p.shift_id = $1",
        )
        .bind(sid)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(row.map(
        |(staff, status, i, o, note, source, version, start, end)| RosterPunchView {
            shift_id: sid,
            staff_id: staff,
            late_minutes: i
                .as_deref()
                .map_or(0, |x| ck::late_minutes(&start, &end, x)),
            worked_minutes: match (&i, &o) {
                (Some(a), Some(b)) => ck::worked_minutes(a, b),
                _ => None,
            },
            status,
            clock_in: i,
            clock_out: o,
            note,
            source,
            version,
        },
    ))
}

async fn save_punch(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    sid: Uuid,
    staff: Uuid,
    rec: &Record,
    source: &str,
    by: &str,
) -> Result<(), KernelError> {
    let nz = |v: &Option<String>| v.clone().filter(|x| !x.is_empty());
    sqlx::query(
        "INSERT INTO people.duty_shift_punch (shift_id, tenant_id, institution_id, staff_id, status, clock_in, \
         clock_out, note, source, recorded_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         ON CONFLICT (shift_id) DO UPDATE SET staff_id = EXCLUDED.staff_id, status = EXCLUDED.status, \
         clock_in = EXCLUDED.clock_in, clock_out = EXCLUDED.clock_out, note = EXCLUDED.note, \
         source = EXCLUDED.source, recorded_by = EXCLUDED.recorded_by, \
         version = people.duty_shift_punch.version + 1, updated_at = now()",
    )
    .bind(sid)
    .bind(t.as_uuid())
    .bind(inst)
    .bind(staff)
    .bind(rec.status.as_deref().unwrap_or(ck::PRESENT))
    .bind(nz(&rec.clock_in))
    .bind(nz(&rec.clock_out))
    .bind(nz(&rec.note))
    .bind(source)
    .bind(by)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

async fn current_record(conn: &mut PgConnection, sid: Uuid) -> Result<Option<Record>, KernelError> {
    let row: Option<(String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT status, clock_in, clock_out, note FROM people.duty_shift_punch WHERE shift_id = $1",
    )
    .bind(sid)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(row.map(|(s, i, o, n)| Record {
        status: Some(s),
        clock_in: i,
        clock_out: o,
        note: n,
    }))
}

/// Clock in or out.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct RosterClockInput {
    /// `in | out`.
    pub action: String,
}

/// Clock in to / out of my published shift now (`people.self`): the institution's clock, the
/// shift's own day (or the day before, for a night shift); late beyond the grace period.
#[utoipa::path(post, path = "/api/v1/people/me/roster/shifts/{id}/clock", operation_id = "people_roster_clock", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterClockInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterPunchView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn clock(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(sid): Path<Uuid>,
    Json(b): Json<RosterClockInput>,
) -> ApiResult<Json<RosterPunchView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let inst = me.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = ctx.scope.tenant_id;
    let (today, now) = local_now(&st, &scope, inst).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (sinst, date, staff, status) = shift_row(&mut tx, t, sid).await?;
    if sinst != inst.as_uuid() || staff != Some(me.id.as_uuid()) || status != "published" {
        return Err(
            KernelError::Conflict("you can only clock your own published shifts".into()).into(),
        );
    }
    if date != today && date != today - Duration::days(1) {
        return Err(KernelError::Conflict("clock in and out on the shift's own day".into()).into());
    }
    ensure_open_period(&mut tx, t, me.id.as_uuid(), date).await?;
    let (lo, hi) = window(date, date);
    let w = load(&mut tx, t, inst.as_uuid(), lo, hi).await?;
    let shift = find(&w, sid)?.clone();
    let current = current_record(&mut tx, sid).await?;
    let grace: i32 = sqlx::query_scalar(
        "SELECT late_grace_minutes FROM people.roster_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .unwrap_or(10);
    let has = |x: &Option<String>| x.as_deref().is_some_and(|v| !v.is_empty());
    let rec = match b.action.as_str() {
        "in" => {
            if current.as_ref().is_some_and(|c| has(&c.clock_in)) {
                return Err(KernelError::Conflict(
                    "you are already clocked in for this shift".into(),
                )
                .into());
            }
            ck::clock_in_record(&shift, current.as_ref(), &now, f64::from(grace))
        }
        "out" => {
            if !current.as_ref().is_some_and(|c| has(&c.clock_in)) {
                return Err(KernelError::Conflict("clock in first".into()).into());
            }
            if current.as_ref().is_some_and(|c| has(&c.clock_out)) {
                return Err(KernelError::Conflict(
                    "you have already clocked out of this shift".into(),
                )
                .into());
            }
            ck::clock_out_record(current.as_ref(), &now)
        }
        _ => return Err(invalid("action", "invalid", "in or out").into()),
    };
    save_punch(
        &mut tx,
        t,
        inst.as_uuid(),
        sid,
        me.id.as_uuid(),
        &rec,
        "self",
        &ctx.subject,
    )
    .await?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift_punch.clock",
        "duty_shift",
        sid,
        json!({"action": b.action, "at": now}),
        &a,
    )
    .await?;
    let view = punch_view(&mut tx, sid)
        .await?
        .ok_or(KernelError::NotFound { entity: "punch" })?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

/// Record or correct a shift's attendance.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterPunchInput {
    /// `present | late | absent`.
    pub status: String,
    /// `HH:MM`.
    pub clock_in: Option<String>,
    /// `HH:MM` (earlier than the clock-in = after midnight).
    pub clock_out: Option<String>,
    pub note: Option<String>,
}

/// Record or correct the attendance of an assigned shift (`people.staff.manage`). Refused (409)
/// once the holder's timesheet for the day is approved.
#[utoipa::path(put, path = "/api/v1/people/roster/shifts/{id}/attendance", operation_id = "people_roster_attendance_put", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterPunchInput, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterPunchView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_put(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(sid): Path<Uuid>,
    Json(b): Json<RosterPunchInput>,
) -> ApiResult<Json<RosterPunchView>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let t = ctx.scope.tenant_id;
    if !ck::is_valid_status(&b.status) {
        return Err(invalid("status", "invalid", "present, late or absent").into());
    }
    for (k, v) in [("clockIn", &b.clock_in), ("clockOut", &b.clock_out)] {
        if v.as_deref()
            .is_some_and(|x| !x.is_empty() && !r::is_valid_time(x))
        {
            return Err(invalid(k, "invalid", "HH:MM").into());
        }
    }
    let note = clean(&b.note, 500, "note")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (inst_id, date, staff, _) = shift_row(&mut tx, t, sid).await?;
    let inst = InstitutionId(inst_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let staff =
        staff.ok_or_else(|| KernelError::Conflict("an open shift has no attendance".into()))?;
    ensure_open_period(&mut tx, t, staff, date).await?;
    let absent = b.status == ck::ABSENT;
    let rec = Record {
        status: Some(b.status.clone()),
        clock_in: if absent { None } else { b.clock_in.clone() },
        clock_out: if absent { None } else { b.clock_out.clone() },
        note,
    };
    save_punch(
        &mut tx,
        t,
        inst_id,
        sid,
        staff,
        &rec,
        "manager",
        &ctx.subject,
    )
    .await?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.duty_shift_punch.record",
        "duty_shift",
        sid,
        json!({"status": b.status, "clockIn": rec.clock_in, "clockOut": rec.clock_out}),
        &a,
    )
    .await?;
    let view = punch_view(&mut tx, sid)
        .await?
        .ok_or(KernelError::NotFound { entity: "punch" })?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

// ------------------------------------------------------------------ timesheets

async fn limits_of(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<Limits, KernelError> {
    let row: Option<(i32, i32, Option<Decimal>, Option<Decimal>)> = sqlx::query_as(
        "SELECT late_grace_minutes, early_leave_grace_minutes, daily_overtime_hours, weekly_overtime_hours \
         FROM people.roster_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(row.map_or_else(Limits::default, |(l, e, d, w)| Limits {
        late_grace_minutes: i64::from(l),
        early_leave_grace_minutes: i64::from(e),
        daily_overtime_hours: fo(d),
        weekly_overtime_hours: fo(w),
    }))
}

/// Everything a reconciliation of `[from, to]` reads, per staff member.
struct Facts {
    shifts: Vec<Shift>,
    punches: BTreeMap<String, Record>,
    /// `(staff, date) → attendance day status`.
    days: HashMap<(String, String), String>,
    holidays: HashSet<String>,
    limits: Limits,
}

async fn facts(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    from: NaiveDate,
    to: NaiveDate,
    staff: Option<Uuid>,
) -> Result<Facts, KernelError> {
    let rows: Vec<(Uuid, NaiveDate, String, String, i32, String, Option<Uuid>)> = sqlx::query_as(
        "SELECT id, shift_date, start_time, end_time, break_minutes, role, staff_id FROM people.duty_shift \
         WHERE tenant_id = $1 AND institution_id = $2 AND shift_date BETWEEN $3 AND $4 AND status = 'published' \
         AND staff_id IS NOT NULL AND ($5::uuid IS NULL OR staff_id = $5) ORDER BY shift_date, start_time, id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(from)
    .bind(to)
    .bind(staff)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let shifts: Vec<Shift> = rows
        .into_iter()
        .map(|(id, d, s, e, b, role, who)| Shift {
            id: id.to_string(),
            date: ds(d),
            start: s,
            end: e,
            break_minutes: Some(f64::from(b)),
            role,
            assignee_id: who.map(|x| x.to_string()),
        })
        .collect();
    let ids: Vec<Uuid> = shifts.iter().filter_map(|s| uid(&s.id)).collect();
    let punches: Vec<(Uuid, String, Option<String>, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT shift_id, status, clock_in, clock_out, note FROM people.duty_shift_punch WHERE shift_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let days: Vec<(Uuid, NaiveDate, String)> = sqlx::query_as(
        "SELECT staff_id, work_date, status FROM people.attendance_day WHERE tenant_id = $1 AND institution_id = $2 \
         AND work_date BETWEEN $3 AND $4 AND ($5::uuid IS NULL OR staff_id = $5)",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(from)
    .bind(to)
    .bind(staff)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let holidays: Option<Vec<NaiveDate>> = sqlx::query_scalar(
        "SELECT holidays FROM people.leave_policy WHERE tenant_id = $1 AND institution_id = $2 AND status = 'active'",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Facts {
        shifts,
        punches: punches
            .into_iter()
            .map(|(id, s, i, o, n)| {
                (
                    id.to_string(),
                    Record {
                        status: Some(s),
                        clock_in: i,
                        clock_out: o,
                        note: n,
                    },
                )
            })
            .collect(),
        days: days
            .into_iter()
            .map(|(s, d, st)| ((s.to_string(), ds(d)), st))
            .collect(),
        holidays: holidays.unwrap_or_default().into_iter().map(ds).collect(),
        limits: limits_of(conn, t, inst).await?,
    })
}

impl Facts {
    fn of(&self, staff: &str, today: NaiveDate) -> (Vec<Line>, Totals) {
        let mine: Vec<&Shift> = self
            .shifts
            .iter()
            .filter(|s| s.assignee_id.as_deref() == Some(staff))
            .collect();
        let actuals: Vec<Actual<'_>> = mine
            .iter()
            .map(|s| Actual {
                shift: s,
                punch: self.punches.get(&s.id),
                day: self
                    .days
                    .get(&(staff.to_owned(), s.date.clone()))
                    .map(String::as_str),
                holiday: self.holidays.contains(&s.date),
            })
            .collect();
        let lines = reconcile(&actuals, &self.limits, &ds(today));
        let days: HashMap<String, String> = self
            .days
            .iter()
            .filter(|((s, _), _)| s == staff)
            .map(|((_, d), v)| (d.clone(), v.clone()))
            .collect();
        let t = totals(&lines, &self.limits, &days);
        (lines, t)
    }
}

/// `weekly` (Monday to Sunday) or `monthly` (a calendar month), or 422.
fn period_kind(from: NaiveDate, to: NaiveDate) -> Result<&'static str, KernelError> {
    if from.weekday() == chrono::Weekday::Mon && to == from + Duration::days(6) {
        return Ok("weekly");
    }
    let next = if from.month() == 12 {
        NaiveDate::from_ymd_opt(from.year() + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(from.year(), from.month() + 1, 1)
    };
    if from.day() == 1 && next.is_some_and(|n| to == n - Duration::days(1)) {
        return Ok("monthly");
    }
    Err(invalid(
        "to",
        "invalid_period",
        "a timesheet covers a week (Monday to Sunday) or a calendar month",
    ))
}

/// A timesheet.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterTimesheetView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub staff_name: String,
    pub staff_number: String,
    /// `weekly | monthly`.
    pub period_kind: String,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    /// `submitted | returned | approved | locked`.
    pub status: String,
    pub totals: Totals,
    pub lines: Vec<Line>,
    pub note: Option<String>,
    pub submitted_at: DateTime<Utc>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub locked_at: Option<DateTime<Utc>>,
    /// When the payroll export handed it to payroll.
    pub exported_at: Option<DateTime<Utc>>,
    pub version: i64,
}

/// Timesheets.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RosterTimesheetList {
    pub items: Vec<RosterTimesheetView>,
}

const TS_SELECT: &str = "SELECT t.id, t.institution_id, t.staff_id, s.display_name AS staff_name, s.staff_number, \
     t.period_kind, t.period_from, t.period_to, t.status, t.lines, t.note, t.submitted_at, t.decided_at, \
     t.decision_reason, t.locked_at, t.exported_at, t.version, t.submitted_by, s.subject_ref AS staff_subject \
     FROM people.timesheet t JOIN people.staff_member s ON s.id = t.staff_id";

#[derive(sqlx::FromRow)]
struct TsTuple {
    id: Uuid,
    institution_id: Uuid,
    staff_id: Uuid,
    staff_name: String,
    staff_number: String,
    period_kind: String,
    period_from: NaiveDate,
    period_to: NaiveDate,
    status: String,
    lines: Value,
    note: Option<String>,
    submitted_at: DateTime<Utc>,
    decided_at: Option<DateTime<Utc>>,
    decision_reason: Option<String>,
    locked_at: Option<DateTime<Utc>>,
    exported_at: Option<DateTime<Utc>>,
    version: i64,
    submitted_by: String,
    staff_subject: Option<String>,
}

struct TsRow {
    view: RosterTimesheetView,
    submitted_by: String,
    staff_subject: Option<String>,
}

fn ts_of(x: TsTuple) -> TsRow {
    let lines = x.lines.get("lines").cloned().unwrap_or(Value::Null);
    let totals = x.lines.get("totals").cloned().unwrap_or(Value::Null);
    TsRow {
        view: RosterTimesheetView {
            id: x.id,
            institution_id: x.institution_id,
            staff_id: x.staff_id,
            staff_name: x.staff_name,
            staff_number: x.staff_number,
            period_kind: x.period_kind,
            period_from: x.period_from,
            period_to: x.period_to,
            status: x.status,
            totals: serde_json::from_value(totals).unwrap_or_default(),
            lines: serde_json::from_value(lines).unwrap_or_default(),
            note: x.note,
            submitted_at: x.submitted_at,
            decided_at: x.decided_at,
            decision_reason: x.decision_reason,
            locked_at: x.locked_at,
            exported_at: x.exported_at,
            version: x.version,
        },
        submitted_by: x.submitted_by,
        staff_subject: x.staff_subject,
    }
}

async fn ts_row(conn: &mut PgConnection, t: TenantId, id: Uuid) -> Result<TsRow, KernelError> {
    let row: Option<TsTuple> =
        sqlx::query_as(&format!("{TS_SELECT} WHERE t.id = $1 AND t.tenant_id = $2"))
            .bind(id)
            .bind(t.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
    row.map(ts_of).ok_or(KernelError::NotFound {
        entity: "timesheet",
    })
}

async fn store_snapshot(
    conn: &mut PgConnection,
    id: Uuid,
    lines: &[Line],
    t: &Totals,
) -> Result<(), KernelError> {
    let d = |x: f64| Decimal::from_f64(x).unwrap_or_default().round_dp(1);
    sqlx::query(
        "UPDATE people.timesheet SET scheduled_minutes = $2, worked_minutes = $3, overtime_minutes = $4, \
         late_minutes = $5, early_leave_minutes = $6, missed_punches = $7, absences = $8, lop_days = $9, \
         night_shifts = $10, holiday_shifts = $11, lines = $12 WHERE id = $1",
    )
    .bind(id)
    .bind(d(t.scheduled_minutes))
    .bind(d(t.worked_minutes))
    .bind(d(t.overtime_minutes))
    .bind(i32::try_from(t.late_minutes).unwrap_or(i32::MAX))
    .bind(i32::try_from(t.early_leave_minutes).unwrap_or(i32::MAX))
    .bind(i32::try_from(t.missed_punches).unwrap_or(i32::MAX))
    .bind(i32::try_from(t.absences).unwrap_or(i32::MAX))
    .bind(Decimal::from_f64(t.lop_days).unwrap_or_default().round_dp(2))
    .bind(i32::try_from(t.night_shifts).unwrap_or(i32::MAX))
    .bind(i32::try_from(t.holiday_shifts).unwrap_or(i32::MAX))
    .bind(json!({"lines": lines, "totals": t}))
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// My timesheet for a period, reconciled now, and the saved one.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterMyTimesheet {
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub lines: Vec<Line>,
    pub totals: Totals,
    pub saved: Option<RosterTimesheetView>,
}

/// My timesheet (`people.self`): published shifts I held in the period (default this week)
/// reconciled with my punches and attendance days; the saved timesheet when I submitted one.
#[utoipa::path(get, path = "/api/v1/people/me/roster/timesheet", operation_id = "people_roster_timesheet_me", tag = "people-roster",
    params(RosterRange), security(("session_cookie" = [])),
    responses((status = 200, body = RosterMyTimesheet), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_me(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Query(q): Query<RosterRange>,
) -> ApiResult<Json<RosterMyTimesheet>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let inst = me.institution_id;
    let scope = institution_scope(ctx, inst);
    let today = business_today(&st, &scope, inst).await?;
    let (from, to) = range(&q, today)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = facts(
        &mut conn,
        t,
        inst.as_uuid(),
        from,
        to,
        Some(me.id.as_uuid()),
    )
    .await?;
    let (lines, totals) = f.of(&me.id.as_uuid().to_string(), today);
    let saved_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM people.timesheet WHERE tenant_id = $1 AND staff_id = $2 AND period_from = $3 AND period_to = $4",
    )
    .bind(t.as_uuid())
    .bind(me.id.as_uuid())
    .bind(from)
    .bind(to)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let saved = match saved_id {
        Some(id) => Some(ts_row(&mut conn, t, id).await?.view),
        None => None,
    };
    Ok(Json(RosterMyTimesheet {
        from,
        to,
        lines,
        totals,
        saved,
    }))
}

/// Submit a timesheet.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterTimesheetSubmit {
    /// A Monday (weekly) or the 1st (monthly).
    pub from: NaiveDate,
    /// The Sunday after `from`, or the month's last day.
    pub to: NaiveDate,
    pub note: Option<String>,
}

/// Submit my weekly or monthly timesheet (`people.self`; Idempotency-Key): the reconciliation
/// now is saved for a manager. A returned timesheet can be submitted again; an approved one
/// cannot (409).
#[utoipa::path(post, path = "/api/v1/people/me/roster/timesheets", operation_id = "people_roster_timesheet_submit", tag = "people-roster",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = RosterTimesheetSubmit, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RosterTimesheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_submit(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<RosterTimesheetSubmit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let me = my_staff(&st, ctx).await?;
    let inst = me.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let kind = period_kind(b.from, b.to)?;
    let note = clean(&b.note, 500, "note")?;
    let today = business_today(&st, &scope, inst).await?;
    if b.from > today {
        return Err(invalid("from", "future", "the period has not started").into());
    }
    let t = ctx.scope.tenant_id;
    let staff = me.id.as_uuid();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!("timesheet:{staff}"))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let existing: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, status FROM people.timesheet WHERE tenant_id = $1 AND staff_id = $2 AND period_from = $3 AND period_to = $4",
    )
    .bind(t.as_uuid())
    .bind(staff)
    .bind(b.from)
    .bind(b.to)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let overlapping: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM people.timesheet WHERE tenant_id = $1 AND staff_id = $2 AND period_from <= $4 \
         AND period_to >= $3 AND NOT (period_from = $3 AND period_to = $4) LIMIT 1",
    )
    .bind(t.as_uuid())
    .bind(staff)
    .bind(b.from)
    .bind(b.to)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if overlapping.is_some() {
        return Err(KernelError::Conflict(
            "another timesheet already covers part of this period".into(),
        )
        .into());
    }
    let (id, status) = match existing {
        Some((_, s)) if s == "approved" || s == "locked" => {
            return Err(KernelError::Conflict(format!("this timesheet is already {s}")).into())
        }
        Some((id, _)) => {
            sqlx::query(
                "UPDATE people.timesheet SET status = 'submitted', note = $2, submitted_by = $3, submitted_at = now(), \
                 decided_by = NULL, decided_at = NULL, version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&note)
            .bind(&ctx.subject)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            (id, StatusCode::OK)
        }
        None => {
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO people.timesheet (tenant_id, institution_id, staff_id, period_kind, period_from, period_to, \
                 status, note, submitted_by) VALUES ($1, $2, $3, $4, $5, $6, 'submitted', $7, $8) RETURNING id",
            )
            .bind(t.as_uuid())
            .bind(inst.as_uuid())
            .bind(staff)
            .bind(kind)
            .bind(b.from)
            .bind(b.to)
            .bind(&note)
            .bind(&ctx.subject)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            (id, StatusCode::CREATED)
        }
    };
    let f = facts(&mut tx, t, inst.as_uuid(), b.from, b.to, Some(staff)).await?;
    let (lines, totals) = f.of(&staff.to_string(), today);
    store_snapshot(&mut tx, id, &lines, &totals).await?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.timesheet.submit",
        "timesheet",
        id,
        json!({"from": b.from, "to": b.to, "totals": totals}),
        &a,
    )
    .await?;
    let row = ts_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(status, &row.view, Version(row.view.version)))
}

/// Timesheet filter.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TimesheetQuery {
    /// Periods starting on or after (default: 62 days ago).
    pub from: Option<NaiveDate>,
    /// Periods starting on or before (default: today).
    pub to: Option<NaiveDate>,
    /// `submitted | returned | approved | locked`.
    pub status: Option<String>,
}

/// Timesheets of the institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/timesheets", operation_id = "people_roster_timesheet_list", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), TimesheetQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RosterTimesheetList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_list(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<TimesheetQuery>,
) -> ApiResult<Json<RosterTimesheetList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<TsTuple> = sqlx::query_as(&format!(
        "{TS_SELECT} WHERE t.tenant_id = $1 AND t.institution_id = $2 AND t.period_from BETWEEN $3 AND $4 \
         AND ($5::text IS NULL OR t.status = $5) ORDER BY t.period_from DESC, s.display_name LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.from.unwrap_or(today - Duration::days(62)))
    .bind(q.to.unwrap_or(today))
    .bind(q.status.as_deref().filter(|s| !s.is_empty()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Json(RosterTimesheetList {
        items: rows.into_iter().map(|x| ts_of(x).view).collect(),
    }))
}

/// One timesheet (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/roster/timesheets/{id}", operation_id = "people_roster_timesheet_get", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RosterTimesheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_get(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = ts_row(&mut conn, ctx.scope.tenant_id, id).await?;
    let inst = InstitutionId(row.view.institution_id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    Ok(with_etag(
        StatusCode::OK,
        &row.view,
        Version(row.view.version),
    ))
}

/// Approve or return a submitted timesheet (`people.staff.manage`; If-Match; never the staff
/// member or whoever submitted it). Approving re-reconciles and freezes the totals.
#[utoipa::path(post, path = "/api/v1/people/roster/timesheets/{id}/decision", operation_id = "people_roster_timesheet_decide", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterTimesheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_decide(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let want = expected!(headers);
    let t = ctx.scope.tenant_id;
    let reason = clean(&b.reason, 500, "reason")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = ts_row(&mut tx, t, id).await?;
    let v = row.view.clone();
    let inst = InstitutionId(v.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    ensure_version(want, v.version)?;
    if v.status != "submitted" {
        return Err(KernelError::Conflict(format!("this timesheet is {}", v.status)).into());
    }
    ensure_independent_decider(
        &mut tx,
        t,
        "timesheet",
        id,
        &[],
        &[
            Some(row.submitted_by.as_str()),
            row.staff_subject.as_deref(),
        ],
        Some(&ctx.subject),
        "timesheet",
    )
    .await?;
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "say what to fix").into());
    }
    let status = if b.approve() { "approved" } else { "returned" };
    if b.approve() {
        let today = business_today(&st, &scope, inst).await?;
        let f = facts(
            &mut tx,
            t,
            v.institution_id,
            v.period_from,
            v.period_to,
            Some(v.staff_id),
        )
        .await?;
        let (lines, totals) = f.of(&v.staff_id.to_string(), today);
        store_snapshot(&mut tx, id, &lines, &totals).await?;
    }
    sqlx::query(
        "UPDATE people.timesheet SET status = $2, decided_by = $3, decided_at = now(), decision_reason = $4, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(status)
    .bind(&ctx.subject)
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.timesheet.decide",
        "timesheet",
        id,
        json!({"decision": status, "reason": reason}),
        &a,
    )
    .await?;
    let row = ts_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(
        StatusCode::OK,
        &row.view,
        Version(row.view.version),
    ))
}

/// Lock an approved timesheet (`people.staff.manage`; If-Match): punches and swaps of its
/// days are frozen.
#[utoipa::path(post, path = "/api/v1/people/roster/timesheets/{id}/lock", operation_id = "people_roster_timesheet_lock", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterTimesheetView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_lock(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let want = expected!(headers);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = ts_row(&mut tx, t, id).await?;
    let inst = InstitutionId(row.view.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    ensure_version(want, row.view.version)?;
    if row.view.status != "approved" {
        return Err(KernelError::Conflict(format!(
            "this timesheet is {}; only an approved timesheet can be locked",
            row.view.status
        ))
        .into());
    }
    sqlx::query(
        "UPDATE people.timesheet SET status = 'locked', locked_by = $2, locked_at = now(), version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&ctx.subject)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.timesheet.lock",
        "timesheet",
        id,
        json!({}),
        &a,
    )
    .await?;
    let row = ts_row(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(
        StatusCode::OK,
        &row.view,
        Version(row.view.version),
    ))
}

// ------------------------------------------------------------------ payroll export

/// Periods to export.
#[derive(Debug, Clone, Deserialize, IntoParams, ToSchema)]
#[into_params(parameter_in = Query)]
pub struct RosterExportRange {
    /// Timesheets whose period starts on or after.
    pub from: NaiveDate,
    /// … and on or before.
    pub to: NaiveDate,
}

/// One timesheet handed to payroll.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterExportItem {
    pub timesheet_id: Uuid,
    pub staff_id: Uuid,
    pub staff_name: String,
    pub period_from: NaiveDate,
    pub period_to: NaiveDate,
    /// `overtime_hours | lop_days | night_shifts | holiday_shifts` → quantity (non-zero only).
    pub quantities: BTreeMap<String, f64>,
}

/// What the export did.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterPayrollExport {
    /// Timesheets handed to payroll now.
    pub exported: usize,
    /// Approved timesheets this export locked.
    pub locked: usize,
    /// Payroll input rows written.
    pub inputs: usize,
    /// Timesheets in the range still awaiting approval (not exported).
    pub pending: usize,
    pub items: Vec<RosterExportItem>,
}

fn check_export_range(q: &RosterExportRange) -> Result<(), KernelError> {
    if q.to < q.from {
        return Err(invalid("to", "before_from", "to must not be before from"));
    }
    if (q.to - q.from).num_days() > 92 {
        return Err(invalid("to", "too_long", "at most 93 days at a time"));
    }
    Ok(())
}

/// Hand approved timesheets to payroll (`people.payroll.manage`): approved ones are locked, and
/// their overtime hours, extra loss-of-pay days, night and holiday shifts become 7C payroll
/// inputs that every payroll calculation of the month the period starts in reads. Each
/// timesheet is exported once.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/roster/payroll-export", operation_id = "people_roster_payroll_export", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterExportRange, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterPayrollExport), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payroll_export(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<RosterExportRange>,
) -> ApiResult<Json<RosterPayrollExport>> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    check_export_range(&b)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let rows: Vec<TsTuple> = sqlx::query_as(&format!(
        "{TS_SELECT} WHERE t.tenant_id = $1 AND t.institution_id = $2 AND t.period_from BETWEEN $3 AND $4 \
         AND t.status IN ('approved', 'locked') AND t.exported_at IS NULL ORDER BY t.period_from, s.display_name FOR UPDATE OF t"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.from)
    .bind(b.to)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM people.timesheet WHERE tenant_id = $1 AND institution_id = $2 \
         AND period_from BETWEEN $3 AND $4 AND status IN ('submitted', 'returned')",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.from)
    .bind(b.to)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (mut locked, mut inputs) = (0, 0);
    let mut items = Vec::new();
    for x in rows {
        let row = ts_of(x);
        let v = row.view;
        if v.status == "approved" {
            sqlx::query(
                "UPDATE people.timesheet SET status = 'locked', locked_by = $2, locked_at = now(), \
                 version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(v.id)
            .bind(&ctx.subject)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            locked += 1;
        }
        let q = payroll_quantities(&v.totals);
        for (code, qty) in &q {
            inputs += sqlx::query(
                "INSERT INTO people.payroll_variable_input (tenant_id, institution_id, staff_id, work_from, work_to, \
                 code, quantity, source_id, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
                 ON CONFLICT (source_id, code) DO NOTHING",
            )
            .bind(t.as_uuid())
            .bind(id)
            .bind(v.staff_id)
            .bind(v.period_from)
            .bind(v.period_to)
            .bind(*code)
            .bind(Decimal::from_f64(*qty).unwrap_or_default().round_dp(2))
            .bind(v.id)
            .bind(&ctx.subject)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?
            .rows_affected() as usize;
        }
        sqlx::query("UPDATE people.timesheet SET exported_at = now() WHERE id = $1")
            .bind(v.id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        items.push(RosterExportItem {
            timesheet_id: v.id,
            staff_id: v.staff_id,
            staff_name: v.staff_name,
            period_from: v.period_from,
            period_to: v.period_to,
            quantities: q.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        });
    }
    let a = actor(ctx);
    audit(&mut tx, &scope, "people.timesheet.payroll_export", "institution", id,
        json!({"from": b.from, "to": b.to, "exported": items.len(), "locked": locked, "inputs": inputs}), &a)
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(RosterPayrollExport {
        exported: items.len(),
        locked,
        inputs,
        pending: usize::try_from(pending).unwrap_or(0),
        items,
    }))
}

fn csv_response(text: String, name: &str) -> Response {
    let mut out = (StatusCode::OK, text).into_response();
    out.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/csv; charset=utf-8"),
    );
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
        out.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    out.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    out
}

/// Approved and locked timesheet totals as CSV for payroll (`people.payroll.manage`): one row
/// per timesheet, formula-safe.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/payroll-export.csv", operation_id = "people_roster_payroll_export_csv", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), RosterExportRange), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn payroll_export_csv(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RosterExportRange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PayrollManage)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    check_export_range(&q)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<TsTuple> = sqlx::query_as(&format!(
        "{TS_SELECT} WHERE t.tenant_id = $1 AND t.institution_id = $2 AND t.period_from BETWEEN $3 AND $4 \
         AND t.status IN ('approved', 'locked') ORDER BY t.period_from, s.display_name"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.from)
    .bind(q.to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let h = |m: f64| round2(m / 60.0);
    let table: Vec<Vec<Cell>> = rows
        .into_iter()
        .map(|x| {
            let v = ts_of(x).view;
            let tt = &v.totals;
            vec![
                Cell::from(v.staff_number),
                Cell::from(v.staff_name),
                Cell::from(ds(v.period_from)),
                Cell::from(ds(v.period_to)),
                Cell::from(v.status),
                Cell::from(h(tt.scheduled_minutes)),
                Cell::from(h(tt.worked_minutes)),
                Cell::from(h(tt.overtime_minutes)),
                Cell::from(tt.lop_days),
                Cell::from(tt.night_shifts),
                Cell::from(tt.holiday_shifts),
                Cell::from(tt.late_minutes),
                Cell::from(tt.early_leave_minutes),
                Cell::from(tt.missed_punches),
                Cell::from(
                    v.exported_at
                        .map_or(String::new(), |d| d.format("%Y-%m-%d").to_string()),
                ),
            ]
        })
        .collect();
    let text = to_csv(
        &[
            "Staff number",
            "Name",
            "Period from",
            "Period to",
            "Status",
            "Scheduled hours",
            "Worked hours",
            "Overtime hours",
            "LOP days",
            "Night shifts",
            "Holiday shifts",
            "Late minutes",
            "Early leave minutes",
            "Missed punches",
            "Sent to payroll",
        ],
        &table,
    );
    Ok(csv_response(
        format!("{text}\r\n"),
        &format!("payroll-inputs-{}-{}.csv", q.from, q.to),
    ))
}

/// Everything's timesheet export of the roster (`people.read`): one row per assigned shift with
/// its punches, then a per-person summary.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/timesheet.csv", operation_id = "people_roster_timesheet_csv", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), RosterRange), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timesheet_csv_get(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<RosterRange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let (from, to) = range(&q, today)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = facts(&mut conn, t, id, from, to, None).await?;
    let people = pay_people(&mut conn, t, id).await?;
    let text = timesheet_csv(&people, &f.shifts, &f.punches, &ds(from), &ds(to));
    Ok(csv_response(text, &format!("timesheet-{from}-{to}.csv")))
}

async fn pay_people(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<Vec<PayEmployee>, KernelError> {
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT id, display_name, status FROM people.staff_member WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(|(id, name, status)| PayEmployee {
            id: id.to_string(),
            name,
            active: status == "active",
            hourly_rate: None,
        })
        .collect())
}

// ------------------------------------------------------------------ reports

/// Hours by department.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterDepartmentHours {
    pub department: String,
    pub staff: usize,
    pub shifts: i64,
    pub scheduled_minutes: f64,
    pub worked_minutes: f64,
    pub overtime_minutes: f64,
}

/// One person's overtime and punctuality.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterOvertimeRow {
    pub staff_id: Uuid,
    pub name: String,
    pub department: String,
    pub worked_minutes: f64,
    pub overtime_minutes: f64,
    pub late_minutes: i64,
    pub early_leave_minutes: i64,
    pub missed_punches: i64,
    pub night_shifts: i64,
    pub holiday_shifts: i64,
}

/// Swap requests made in the period.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterSwapCounts {
    pub total: i64,
    pub pending: i64,
    pub approved: i64,
    pub declined: i64,
    pub cancelled: i64,
    /// `give_away | swap | cover | claim` → requests.
    pub by_kind: BTreeMap<String, i64>,
}

/// A roster report.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterReport {
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// Everything's report: `people`, `roles`, `attendance`, `coverage`, `open`, `totals`.
    #[schema(value_type = Object)]
    pub report: Value,
    /// The same totals for the period of equal length before.
    #[schema(value_type = Object)]
    pub previous: Value,
    pub departments: Vec<RosterDepartmentHours>,
    pub overtime: Vec<RosterOvertimeRow>,
    pub swaps: RosterSwapCounts,
}

async fn departments(
    st: &PeopleState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: TenantId,
    inst: Uuid,
) -> Result<HashMap<String, String>, KernelError> {
    let rows: Vec<(Uuid, Option<Uuid>)> = sqlx::query_as(
        "SELECT DISTINCT ON (staff_id) staff_id, org_unit_id FROM people.staff_engagement \
         WHERE tenant_id = $1 AND institution_id = $2 ORDER BY staff_id, (status = 'active') DESC, start_date DESC",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut names: HashMap<Uuid, String> = HashMap::new();
    let mut out = HashMap::new();
    for (staff, unit) in rows {
        let Some(u) = unit else { continue };
        let n = match names.get(&u) {
            Some(n) => n.clone(),
            None => {
                let n = st
                    .references
                    .org_unit(scope, u)
                    .await?
                    .map_or_else(|| "Unknown department".to_owned(), |o| o.name);
                names.insert(u, n.clone());
                n
            }
        };
        out.insert(staff.to_string(), n);
    }
    Ok(out)
}

async fn report_state(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<State, KernelError> {
    let (lo, hi) = window(from, to);
    let w = load(conn, t, inst, lo, hi).await?;
    let ids: Vec<Uuid> = w.shifts.iter().filter_map(|s| uid(&s.id)).collect();
    let punches: Vec<(Uuid, String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT shift_id, status, clock_in, clock_out FROM people.duty_shift_punch WHERE shift_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let employees = pay_people(conn, t, inst).await?;
    Ok(State {
        employees,
        shifts: w.shifts,
        attendance: punches
            .into_iter()
            .map(|(id, s, i, o)| {
                (
                    id.to_string(),
                    Record {
                        status: Some(s),
                        clock_in: i,
                        clock_out: o,
                        note: None,
                    },
                )
            })
            .collect(),
        coverage: w.requirements,
        overtime_threshold_hours: None,
        overtime_multiplier: None,
    })
}

/// A report range and section.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ReportQuery {
    /// Default: Monday of this week.
    pub from: Option<NaiveDate>,
    /// Default: six days after `from`; at most 62 days.
    pub to: Option<NaiveDate>,
    /// CSV only: `hours | cost | roles | attendance | coverage | departments`.
    pub section: Option<String>,
}

async fn build(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
    q: &ReportQuery,
) -> Result<(RosterReport, super::reports::Report), KernelError> {
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution(st, &scope, inst).await?;
    let today = business_today(st, &scope, inst).await?;
    let (from, to) = range(
        &RosterRange {
            from: q.from,
            to: q.to,
        },
        today,
    )?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let state = report_state(&mut conn, t, id, from, to).await?;
    let report = build_report(&state, &ds(from), &ds(to));
    let (pf, pt) = previous_range(&ds(from), &ds(to));
    let (pfd, ptd) = (pd(&pf).unwrap_or(from), pd(&pt).unwrap_or(to));
    let prev_state = report_state(&mut conn, t, id, pfd, ptd).await?;
    let previous = build_report(&prev_state, &pf, &pt);
    let depts = departments(st, &mut conn, &scope, t, id).await?;
    let f = facts(&mut conn, t, id, from, to, None).await?;
    let names: HashMap<String, String> = state
        .employees
        .iter()
        .map(|e| (e.id.clone(), e.name.clone()))
        .collect();
    let mut staff: Vec<String> = f
        .shifts
        .iter()
        .filter_map(|s| s.assignee_id.clone())
        .collect();
    staff.sort();
    staff.dedup();
    let mut overtime = Vec::new();
    let mut by_dept: BTreeMap<String, RosterDepartmentHours> = BTreeMap::new();
    for s in &staff {
        let (_, tt) = f.of(s, today);
        let dept = depts
            .get(s)
            .cloned()
            .unwrap_or_else(|| "No department".to_owned());
        let e = by_dept
            .entry(dept.clone())
            .or_insert_with(|| RosterDepartmentHours {
                department: dept.clone(),
                staff: 0,
                shifts: 0,
                scheduled_minutes: 0.0,
                worked_minutes: 0.0,
                overtime_minutes: 0.0,
            });
        e.staff += 1;
        e.shifts += tt.shifts;
        e.scheduled_minutes += tt.scheduled_minutes;
        e.worked_minutes += tt.worked_minutes;
        e.overtime_minutes += tt.overtime_minutes;
        overtime.push(RosterOvertimeRow {
            staff_id: uid(s).unwrap_or_default(),
            name: names
                .get(s)
                .cloned()
                .unwrap_or_else(|| "Former employee".to_owned()),
            department: dept,
            worked_minutes: tt.worked_minutes,
            overtime_minutes: tt.overtime_minutes,
            late_minutes: tt.late_minutes,
            early_leave_minutes: tt.early_leave_minutes,
            missed_punches: tt.missed_punches,
            night_shifts: tt.night_shifts,
            holiday_shifts: tt.holiday_shifts,
        });
    }
    overtime.sort_by(|a, b| {
        b.overtime_minutes
            .partial_cmp(&a.overtime_minutes)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    let counts: Vec<(String, String, i64)> = sqlx::query_as(
        "SELECT kind, status, count(*) FROM people.duty_shift_swap w WHERE tenant_id = $1 AND institution_id = $2 \
         AND (created_at AT TIME ZONE 'UTC')::date BETWEEN $3 AND $4 GROUP BY kind, status",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut swaps = RosterSwapCounts::default();
    for (kind, status, n) in counts {
        swaps.total += n;
        *swaps.by_kind.entry(kind).or_default() += n;
        match status.as_str() {
            "approved" => swaps.approved += n,
            "declined" => swaps.declined += n,
            "cancelled" => swaps.cancelled += n,
            _ => swaps.pending += n,
        }
    }
    Ok((
        RosterReport {
            from,
            to,
            report: serde_json::to_value(&report).unwrap_or_default(),
            previous: serde_json::to_value(&previous.totals).unwrap_or_default(),
            departments: by_dept.into_values().collect(),
            overtime,
            swaps,
        },
        report,
    ))
}

/// Roster report (`people.read`): Everything's people / roles / attendance / coverage / open
/// shifts with the previous period's totals, plus hours by department, overtime and punctuality
/// per person (from the reconciliation), and swap request counts.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/reports", operation_id = "people_roster_report", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ReportQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RosterReport), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn report(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Json<RosterReport>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    Ok(Json(build(&st, ctx, id, &q).await?.0))
}

/// One report section as CSV (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/reports.csv", operation_id = "people_roster_report_csv", tag = "people-roster",
    params(("id" = String, Path, format = Uuid), ReportQuery), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/csv"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn report_csv_get(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ReportQuery>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let section = q.section.clone().unwrap_or_else(|| "hours".to_owned());
    let (rep, full) = build(&st, ctx, id, &q).await?;
    let text = if section == "departments" {
        let h = |m: f64| round2(m / 60.0);
        let rows: Vec<Vec<Cell>> = rep
            .departments
            .iter()
            .map(|d| {
                vec![
                    Cell::from(d.department.clone()),
                    Cell::from(d.staff as f64),
                    Cell::from(d.shifts),
                    Cell::from(h(d.scheduled_minutes)),
                    Cell::from(h(d.worked_minutes)),
                    Cell::from(h(d.overtime_minutes)),
                ]
            })
            .collect();
        format!(
            "{}\r\n",
            to_csv(
                &[
                    "Department",
                    "Staff",
                    "Shifts",
                    "Scheduled hours",
                    "Worked hours",
                    "Overtime hours"
                ],
                &rows
            )
        )
    } else {
        report_csv(&full, &section).map_err(|e| invalid("section", "unknown", &e))?
    };
    Ok(csv_response(
        text,
        &format!("roster-{section}-{}-{}.csv", rep.from, rep.to),
    ))
}

// ------------------------------------------------------------------ calendar

/// My published duties as an iCalendar file (`people.self`; default: the last week and the
/// next eight). An authenticated download — there is no token feed.
#[utoipa::path(get, path = "/api/v1/people/me/roster.ics", operation_id = "people_roster_me_ics", tag = "people-roster",
    params(RosterRange), security(("session_cookie" = [])),
    responses((status = 200, body = String, content_type = "text/calendar"), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_ics(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Query(q): Query<RosterRange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let inst = me.institution_id;
    let today = business_today(&st, &institution_scope(ctx, inst), inst).await?;
    let from = q.from.unwrap_or(today - Duration::days(7));
    let (from, to) = range(
        &RosterRange {
            from: Some(from),
            to: Some(q.to.unwrap_or(from + Duration::days(61))),
        },
        today,
    )?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (lo, hi) = window(from, to);
    let w = load(&mut conn, t, inst.as_uuid(), lo, hi).await?;
    let mine = me.id.as_uuid().to_string();
    let shifts: Vec<CalendarShift> = w
        .shifts
        .iter()
        .filter(|s| s.assignee_id.as_deref() == Some(mine.as_str()) && published(&w, &s.id))
        .map(|s| {
            let m = w.meta.get(&s.id);
            let note = [
                m.and_then(|m| m.location.clone())
                    .map(|l| format!("Where: {l}")),
                m.and_then(|m| m.note.clone()),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join("\n");
            CalendarShift {
                shift: s.clone(),
                note: (!note.is_empty()).then_some(note),
            }
        })
        .collect();
    let mut names = HashMap::new();
    names.insert(mine.clone(), me.display_name.clone());
    let stamp = Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string();
    let (text, _) = build_schedule_ics(
        &names,
        &shifts,
        &ds(from),
        &ds(to),
        Some(&mine),
        &stamp,
        "My duty roster",
    );
    let mut out = (StatusCode::OK, text).into_response();
    out.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    out.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"my-duty-roster.ics\""),
    );
    out.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(out)
}

// ------------------------------------------------------------------ settings

/// Timesheet and pay settings of the roster (Indian defaults: Factories Act 1948 s.51/54/59).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RosterTimesheetSettings {
    /// Minutes after the start that still count as on time.
    pub late_grace_minutes: i64,
    /// Minutes before the end that do not count as leaving early.
    pub early_leave_grace_minutes: i64,
    /// Paid hours a day before overtime (default 9; null = none).
    pub daily_overtime_hours: Option<f64>,
    /// Paid hours a week before overtime (default 48; null = none).
    pub weekly_overtime_hours: Option<f64>,
    /// Overtime pays this times the ordinary hourly rate (default 2).
    pub overtime_multiplier: f64,
    /// A flat ₹ per overtime hour instead (null = from BASIC + DA).
    pub overtime_rate_per_hour: Option<f64>,
    /// Hours of an ordinary day (ordinary hourly rate = (BASIC + DA) / days / hours).
    pub standard_hours_per_day: f64,
    /// ₹ per night shift (one running past midnight).
    pub night_shift_allowance: Option<f64>,
    /// ₹ per shift worked on a holiday.
    pub holiday_shift_allowance: Option<f64>,
}

impl Default for RosterTimesheetSettings {
    fn default() -> Self {
        Self {
            late_grace_minutes: 10,
            early_leave_grace_minutes: 10,
            daily_overtime_hours: Some(9.0),
            weekly_overtime_hours: Some(48.0),
            overtime_multiplier: 2.0,
            overtime_rate_per_hour: None,
            standard_hours_per_day: 8.0,
            night_shift_allowance: None,
            holiday_shift_allowance: None,
        }
    }
}

async fn ts_settings(
    conn: &mut PgConnection,
    t: TenantId,
    inst: Uuid,
) -> Result<RosterTimesheetSettings, KernelError> {
    let row: Option<(i32, i32, Option<Decimal>, Option<Decimal>, Decimal, Option<Decimal>, Decimal, Option<Decimal>, Option<Decimal>)> =
        sqlx::query_as(
            "SELECT late_grace_minutes, early_leave_grace_minutes, daily_overtime_hours, weekly_overtime_hours, \
             overtime_multiplier, overtime_rate_per_hour, standard_hours_per_day, night_shift_allowance, \
             holiday_shift_allowance FROM people.roster_settings WHERE tenant_id = $1 AND institution_id = $2",
        )
        .bind(t.as_uuid())
        .bind(inst)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(row.map_or_else(RosterTimesheetSettings::default, |x| {
        RosterTimesheetSettings {
            late_grace_minutes: i64::from(x.0),
            early_leave_grace_minutes: i64::from(x.1),
            daily_overtime_hours: fo(x.2),
            weekly_overtime_hours: fo(x.3),
            overtime_multiplier: x.4.to_f64().unwrap_or(2.0),
            overtime_rate_per_hour: fo(x.5),
            standard_hours_per_day: x.6.to_f64().unwrap_or(8.0),
            night_shift_allowance: fo(x.7),
            holiday_shift_allowance: fo(x.8),
        }
    }))
}

/// Timesheet and pay settings (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/roster/timesheet-settings", operation_id = "people_roster_timesheet_settings_get", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RosterTimesheetSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ts_settings_get(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<RosterTimesheetSettings>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let inst = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, inst), inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(ts_settings(&mut conn, ctx.scope.tenant_id, id).await?))
}

/// Save timesheet and pay settings (`people.staff.manage`).
#[utoipa::path(put, path = "/api/v1/people/institutions/{id}/roster/timesheet-settings", operation_id = "people_roster_timesheet_settings_put", tag = "people-roster",
    params(("id" = String, Path, format = Uuid)), request_body = RosterTimesheetSettings, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RosterTimesheetSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ts_settings_put(
    AxState(st): AxState<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<RosterTimesheetSettings>,
) -> ApiResult<Json<RosterTimesheetSettings>> {
    let ctx = auth.require(Permission::StaffManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let grace = |v: i64| (0..=240).contains(&v);
    if !grace(b.late_grace_minutes) {
        return Err(invalid("lateGraceMinutes", "out_of_range", "0 to 240").into());
    }
    if !grace(b.early_leave_grace_minutes) {
        return Err(invalid("earlyLeaveGraceMinutes", "out_of_range", "0 to 240").into());
    }
    if b.daily_overtime_hours.is_some_and(|x| x <= 0.0 || x > 24.0) {
        return Err(invalid(
            "dailyOvertimeHours",
            "out_of_range",
            "more than 0 and at most 24",
        )
        .into());
    }
    if b.weekly_overtime_hours
        .is_some_and(|x| x <= 0.0 || x > 168.0)
    {
        return Err(invalid(
            "weeklyOvertimeHours",
            "out_of_range",
            "more than 0 and at most 168",
        )
        .into());
    }
    if !(1.0..=5.0).contains(&b.overtime_multiplier) {
        return Err(invalid("overtimeMultiplier", "out_of_range", "1 to 5").into());
    }
    if !(b.standard_hours_per_day > 0.0 && b.standard_hours_per_day <= 24.0) {
        return Err(invalid(
            "standardHoursPerDay",
            "out_of_range",
            "more than 0 and at most 24",
        )
        .into());
    }
    for (k, v) in [
        ("overtimeRatePerHour", b.overtime_rate_per_hour),
        ("nightShiftAllowance", b.night_shift_allowance),
        ("holidayShiftAllowance", b.holiday_shift_allowance),
    ] {
        if v.is_some_and(|x| !(0.0..=10_000_000.0).contains(&x)) {
            return Err(invalid(k, "out_of_range", "0 or more (₹)").into());
        }
    }
    let money = |x: Option<f64>| x.and_then(Decimal::from_f64).map(|d| d.round_dp(2));
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.roster_settings (tenant_id, institution_id, late_grace_minutes, early_leave_grace_minutes, \
         daily_overtime_hours, weekly_overtime_hours, overtime_multiplier, overtime_rate_per_hour, standard_hours_per_day, \
         night_shift_allowance, holiday_shift_allowance) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET late_grace_minutes = EXCLUDED.late_grace_minutes, \
         early_leave_grace_minutes = EXCLUDED.early_leave_grace_minutes, daily_overtime_hours = EXCLUDED.daily_overtime_hours, \
         weekly_overtime_hours = EXCLUDED.weekly_overtime_hours, overtime_multiplier = EXCLUDED.overtime_multiplier, \
         overtime_rate_per_hour = EXCLUDED.overtime_rate_per_hour, standard_hours_per_day = EXCLUDED.standard_hours_per_day, \
         night_shift_allowance = EXCLUDED.night_shift_allowance, holiday_shift_allowance = EXCLUDED.holiday_shift_allowance, \
         version = people.roster_settings.version + 1, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(i32::try_from(b.late_grace_minutes).unwrap_or(0))
    .bind(i32::try_from(b.early_leave_grace_minutes).unwrap_or(0))
    .bind(super::http::dec(b.daily_overtime_hours))
    .bind(super::http::dec(b.weekly_overtime_hours))
    .bind(Decimal::from_f64(b.overtime_multiplier).unwrap_or(Decimal::TWO).round_dp(2))
    .bind(money(b.overtime_rate_per_hour))
    .bind(Decimal::from_f64(b.standard_hours_per_day).unwrap_or(Decimal::from(8)).round_dp(1))
    .bind(money(b.night_shift_allowance))
    .bind(money(b.holiday_shift_allowance))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let a = actor(ctx);
    audit(
        &mut tx,
        &scope,
        "people.roster_settings.timesheet",
        "institution",
        id,
        serde_json::to_value(&b).unwrap_or_default(),
        &a,
    )
    .await?;
    let out = ts_settings(&mut tx, t, id).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(out))
}

/// Routes of 5B.
pub fn router(st: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(swap_request, swap_mine))
        .routes(routes!(claim))
        .routes(routes!(open_shifts))
        .routes(routes!(swap_respond))
        .routes(routes!(swap_cancel))
        .routes(routes!(swap_list))
        .routes(routes!(swap_decide))
        .routes(routes!(clock))
        .routes(routes!(attendance_put))
        .routes(routes!(timesheet_me))
        .routes(routes!(timesheet_submit))
        .routes(routes!(timesheet_list))
        .routes(routes!(timesheet_get))
        .routes(routes!(timesheet_decide))
        .routes(routes!(timesheet_lock))
        .routes(routes!(payroll_export))
        .routes(routes!(payroll_export_csv))
        .routes(routes!(timesheet_csv_get))
        .routes(routes!(report))
        .routes(routes!(report_csv_get))
        .routes(routes!(me_ics))
        .routes(routes!(ts_settings_get, ts_settings_put))
        .with_state(st)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn periods_are_weeks_or_months() {
        let d = |s: &str| pd(s).unwrap();
        assert_eq!(
            period_kind(d("2026-10-12"), d("2026-10-18")).unwrap(),
            "weekly"
        );
        assert_eq!(
            period_kind(d("2026-12-01"), d("2026-12-31")).unwrap(),
            "monthly"
        );
        assert_eq!(
            period_kind(d("2027-02-01"), d("2027-02-28")).unwrap(),
            "monthly"
        );
        assert!(period_kind(d("2026-10-13"), d("2026-10-19")).is_err());
        assert!(check_export_range(&RosterExportRange {
            from: d("2026-10-01"),
            to: d("2026-09-01")
        })
        .is_err());
    }
}
