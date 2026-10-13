//! **7B** — Staff attendance days (single, bulk, correction) through `shared::attendance::record_day`; `/me/attendance`.
//!
//! Tag `people-attendance`; operation id prefixes `people_attendance_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 7B fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.
//!
//! Rules: a day is captured only for a staff member of the institution whose engagement covers
//! the date and only up to today; `on_leave` days come from approved leave (consumption) and are
//! changed only by an explicit correction; recording the same status again is skipped.

use crate::leave::common::{
    audit, clean, emit_status, ensure_version, expected, invalid, parse_date, required, subject_of,
    EventParties, LeaveBulkFailure, LeaveBulkSummary, Transition,
};
use crate::shared::attendance::{
    attendance_from_row, record_day, AttendanceDayRow, DayRecord, ATTENDANCE_COLUMNS,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_staff;
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{Duration, NaiveDate};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use sutra_contracts::people::events::leave::{ATTENDANCE_CORRECTED, ATTENDANCE_CORRECTED_SCHEMA};
use sutra_contracts::people::ids::{AttendanceDayId, StaffMemberId};
use sutra_contracts::people::status::{AttendanceDayStatus, AttendanceSource, EngagementStatus};
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

/// One attendance day.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceDayView {
    pub id: AttendanceDayId,
    pub staff_id: StaffMemberId,
    pub work_date: NaiveDate,
    pub status: AttendanceDayStatus,
    pub source: AttendanceSource,
    pub leave_request_id: Option<Uuid>,
    pub remark: Option<String>,
    pub version: i64,
}

impl From<&AttendanceDayRow> for AttendanceDayView {
    fn from(d: &AttendanceDayRow) -> Self {
        Self {
            id: d.id,
            staff_id: d.staff_id,
            work_date: d.work_date,
            status: d.status,
            source: d.source,
            leave_request_id: d.leave_request_id,
            remark: d.remark.clone(),
            version: d.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AttendanceDayList {
    pub items: Vec<AttendanceDayView>,
}

/// One staff member on the daily grid.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceGridRow {
    pub staff_id: StaffMemberId,
    pub staff_number: String,
    pub display_name: String,
    pub engagement_status: EngagementStatus,
    /// Operationally available (engagement working, e.g. not on long leave).
    pub available: bool,
    pub day: Option<AttendanceDayView>,
}

/// The daily attendance grid of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceGrid {
    pub work_date: NaiveDate,
    pub items: Vec<AttendanceGridRow>,
}

/// `?date=YYYY-MM-DD` (default today).
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AttendanceDateQuery {
    pub date: Option<String>,
}

/// `?from=&to=` (default: the last 31 days).
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AttendanceRangeQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

/// Record one day.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceRecordCreate {
    pub staff_id: Uuid,
    /// `YYYY-MM-DD`.
    pub work_date: String,
    pub status: AttendanceDayStatus,
    pub remark: Option<String>,
}

/// One entry of a bulk capture.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceBulkEntry {
    pub staff_id: Uuid,
    pub status: AttendanceDayStatus,
    pub remark: Option<String>,
}

/// Bulk capture of one date.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceBulkCreate {
    /// `YYYY-MM-DD`.
    pub work_date: String,
    pub entries: Vec<AttendanceBulkEntry>,
}

/// Correct a recorded day (remark required, history kept).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceCorrect {
    pub status: AttendanceDayStatus,
    pub remark: Option<String>,
}

enum Capture {
    Recorded(AttendanceDayRow),
    Unchanged(AttendanceDayRow),
}

/// Captures one day under the rules in the module docs (inside the caller's transaction).
#[allow(clippy::too_many_arguments)]
async fn capture(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    staff: Uuid,
    date: NaiveDate,
    status: AttendanceDayStatus,
    remark: Option<&str>,
    source: AttendanceSource,
    a: &Actor,
) -> Result<Capture, KernelError> {
    if status == AttendanceDayStatus::OnLeave {
        return Err(invalid(
            "status",
            "on_leave_from_leave",
            "on_leave days are recorded by approved leave; correct a day to change it",
        ));
    }
    let covered: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.staff_engagement e JOIN people.staff_member s \
         ON s.id = e.staff_id WHERE s.id = $1 AND s.tenant_id = $2 AND s.institution_id = $3 \
         AND e.start_date <= $4 AND (e.status IN ('active', 'suspended', 'deputed', 'long_leave') \
         OR (e.status = 'closed' AND e.last_working_date >= $4)))",
    )
    .bind(staff)
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(date)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if !covered {
        return Err(invalid(
            "staffId",
            "no_engagement",
            "the staff member has no engagement of this institution on that date",
        ));
    }
    let existing = sqlx::query(&format!(
        "SELECT {ATTENDANCE_COLUMNS} FROM people.attendance_day WHERE staff_id = $1 AND work_date = $2"
    ))
    .bind(staff)
    .bind(date)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .as_ref()
    .map(attendance_from_row)
    .transpose()
    .map_err(map_db_error)?;
    if let Some(d) = existing {
        if d.status == AttendanceDayStatus::OnLeave && d.leave_request_id.is_some() {
            return Err(KernelError::Conflict(
                "the day is on approved leave; correct it instead".into(),
            ));
        }
        if d.status == status {
            return Ok(Capture::Unchanged(d));
        }
    }
    let row = record_day(
        conn,
        &DayRecord {
            tenant,
            institution,
            staff_id: StaffMemberId(staff),
            work_date: date,
            status,
            source,
            leave_request_id: None,
            remark,
        },
        a,
    )
    .await?;
    Ok(Capture::Recorded(row))
}

fn day_response(status: StatusCode, d: &AttendanceDayRow) -> Response {
    with_etag(status, &AttendanceDayView::from(d), Version(d.version))
}

async fn days_of(
    st: &PeopleState,
    tenant: TenantId,
    staff: StaffMemberId,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<AttendanceDayList, KernelError> {
    let rows = sqlx::query(&format!(
        "SELECT {ATTENDANCE_COLUMNS} FROM people.attendance_day WHERE tenant_id = $1 \
         AND staff_id = $2 AND work_date BETWEEN $3 AND $4 ORDER BY work_date"
    ))
    .bind(tenant.as_uuid())
    .bind(staff.as_uuid())
    .bind(from)
    .bind(to)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| attendance_from_row(r).map(|d| AttendanceDayView::from(&d)))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(AttendanceDayList { items })
}

fn range(
    q: &AttendanceRangeQuery,
    today: NaiveDate,
) -> Result<(NaiveDate, NaiveDate), KernelError> {
    let to = match q.to.as_deref() {
        Some(raw) => parse_date("to", raw)?,
        None => today,
    };
    let from = match q.from.as_deref() {
        Some(raw) => parse_date("from", raw)?,
        None => to - Duration::days(30),
    };
    if from > to {
        return Err(invalid("from", "after_to", "from is after to"));
    }
    if (to - from).num_days() > 370 {
        return Err(invalid("from", "too_long", "at most 370 days per request"));
    }
    Ok((from, to))
}

/// The daily attendance grid (`people.read`): every staff member whose engagement covers the
/// date, with the recorded day when there is one.
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/attendance", operation_id = "people_attendance_grid", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), AttendanceDateQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AttendanceGrid), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_grid(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AttendanceDateQuery>,
) -> ApiResult<Json<AttendanceGrid>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let date = match q.date.as_deref() {
        Some(raw) => parse_date("date", raw)?,
        None => business_today(&st, &scope, institution).await?,
    };
    let rows = sqlx::query(
        "SELECT s.id AS staff_id, s.staff_number, s.display_name, e.status AS engagement_status, \
         d.id, d.work_date, d.status, d.source, d.leave_request_id, d.remark, d.version \
         FROM people.staff_engagement e JOIN people.staff_member s ON s.id = e.staff_id \
         LEFT JOIN people.attendance_day d ON d.staff_id = s.id AND d.work_date = $3 \
         WHERE e.tenant_id = $1 AND e.institution_id = $2 AND e.start_date <= $3 \
         AND s.status NOT IN ('merged', 'archived') \
         AND (e.status IN ('active', 'suspended', 'deputed', 'long_leave') \
              OR (e.status = 'closed' AND e.last_working_date >= $3)) \
         ORDER BY s.staff_number",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(date)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| -> Result<AttendanceGridRow, sqlx::Error> {
            let es = crate::shared::rows::decode(
                r.try_get("engagement_status")?,
                EngagementStatus::parse,
            )?;
            let staff_id = StaffMemberId(r.try_get("staff_id")?);
            let day = match r.try_get::<Option<Uuid>, _>("id")? {
                None => None,
                Some(day_id) => Some(AttendanceDayView {
                    id: AttendanceDayId(day_id),
                    staff_id,
                    work_date: r.try_get("work_date")?,
                    status: crate::shared::rows::decode(
                        r.try_get("status")?,
                        AttendanceDayStatus::parse,
                    )?,
                    source: crate::shared::rows::decode(
                        r.try_get("source")?,
                        AttendanceSource::parse,
                    )?,
                    leave_request_id: r.try_get("leave_request_id")?,
                    remark: r.try_get("remark")?,
                    version: r.try_get("version")?,
                }),
            };
            Ok(AttendanceGridRow {
                staff_id,
                staff_number: r.try_get("staff_number")?,
                display_name: r.try_get("display_name")?,
                engagement_status: es,
                available: es.is_working(),
                day,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(AttendanceGrid {
        work_date: date,
        items,
    }))
}

/// Record one attendance day (`people.leave.manage`).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/attendance", operation_id = "people_attendance_record", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AttendanceRecordCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AttendanceDayView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_record(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AttendanceRecordCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let date = parse_date("workDate", &b.work_date)?;
    if date > business_today(&st, &scope, institution).await? {
        return Err(invalid("workDate", "future", "attendance is recorded up to today").into());
    }
    let a = actor(ctx);
    let remark = clean(b.remark);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = capture(
        &mut tx,
        ctx.scope.tenant_id,
        institution,
        b.staff_id,
        date,
        b.status,
        remark.as_deref(),
        AttendanceSource::Manual,
        &a,
    )
    .await?;
    let (row, status) = match c {
        Capture::Recorded(r) => (r, StatusCode::CREATED),
        Capture::Unchanged(r) => (r, StatusCode::OK),
    };
    audit(
        &mut tx,
        &scope,
        "people.attendance_day.record",
        "attendance_day",
        row.id.as_uuid(),
        serde_json::json!({"staffId": b.staff_id, "workDate": date, "status": b.status}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(day_response(status, &row))
}

/// Bulk capture of one date (`people.leave.manage`; Idempotency-Key): `{processed, skipped,
/// failed[]}` — unchanged days are skipped, refused rows are listed with their reason.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/attendance/bulk", operation_id = "people_attendance_bulk", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = AttendanceBulkCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_bulk(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AttendanceBulkCreate>,
) -> ApiResult<Json<LeaveBulkSummary>> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let date = parse_date("workDate", &b.work_date)?;
    if date > business_today(&st, &scope, institution).await? {
        return Err(invalid("workDate", "future", "attendance is recorded up to today").into());
    }
    if b.entries.is_empty() || b.entries.len() > 2000 {
        return Err(invalid("entries", "size", "1 … 2000 entries").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut summary = LeaveBulkSummary::default();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    for e in &b.entries {
        let remark = clean(e.remark.clone());
        // Each row in its own savepoint so one refusal does not undo the others.
        sqlx::query("SAVEPOINT attendance_row")
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        match capture(
            &mut tx,
            t,
            institution,
            e.staff_id,
            date,
            e.status,
            remark.as_deref(),
            AttendanceSource::Bulk,
            &a,
        )
        .await
        {
            Ok(Capture::Recorded(_)) => summary.processed += 1,
            Ok(Capture::Unchanged(_)) => summary.skipped += 1,
            Err(err) => {
                sqlx::query("ROLLBACK TO SAVEPOINT attendance_row")
                    .execute(&mut *tx)
                    .await
                    .map_err(map_db_error)?;
                summary.failed.push(LeaveBulkFailure {
                    id: e.staff_id,
                    reason: match err {
                        KernelError::Validation(v) => v
                            .first()
                            .map(|f| f.message.clone())
                            .unwrap_or_else(|| "invalid".into()),
                        other => other.to_string(),
                    },
                });
            }
        }
    }
    audit(
        &mut tx,
        &scope,
        "people.attendance_day.bulk",
        "attendance_day",
        id,
        serde_json::json!({"workDate": date, "processed": summary.processed,
            "skipped": summary.skipped, "failed": summary.failed.len()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(summary))
}

/// Attendance days of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/attendance", operation_id = "people_attendance_staff_list", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), AttendanceRangeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AttendanceDayList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn staff_attendance(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<AttendanceRangeQuery>,
) -> ApiResult<Json<AttendanceDayList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let s = st
        .directory
        .staff(&tenant_scope(ctx), StaffMemberId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let today = business_today(
        &st,
        &institution_scope(ctx, s.institution_id),
        s.institution_id,
    )
    .await?;
    let (from, to) = range(&q, today)?;
    Ok(Json(
        days_of(&st, ctx.scope.tenant_id, s.id, from, to).await?,
    ))
}

/// My attendance days (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/attendance", operation_id = "people_attendance_self_list", tag = "people-attendance",
    params(AttendanceRangeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AttendanceDayList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_attendance(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Query(q): Query<AttendanceRangeQuery>,
) -> ApiResult<Json<AttendanceDayList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    let today = business_today(
        &st,
        &institution_scope(ctx, me.institution_id),
        me.institution_id,
    )
    .await?;
    let (from, to) = range(&q, today)?;
    Ok(Json(
        days_of(&st, ctx.scope.tenant_id, me.id, from, to).await?,
    ))
}

/// Correct a recorded day (`people.leave.manage`; If-Match; remark required): any status, history
/// kept (`attendance_day.corrected`).
#[utoipa::path(post, path = "/api/v1/people/attendance-days/{id}/correct", operation_id = "people_attendance_correct", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the day")),
    request_body = AttendanceCorrect, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AttendanceDayView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn attendance_correct(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AttendanceCorrect>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    let remark = required("remark", b.remark)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = sqlx::query(&format!(
        "SELECT {ATTENDANCE_COLUMNS}, institution_id FROM people.attendance_day \
         WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "attendance day",
    })?;
    let day = attendance_from_row(&r).map_err(map_db_error)?;
    let institution = InstitutionId(r.try_get("institution_id").map_err(map_db_error)?);
    ensure_version(v, day.version)?;
    let scope = institution_scope(ctx, institution);
    let leave_request = if b.status == AttendanceDayStatus::OnLeave {
        day.leave_request_id
    } else {
        None
    };
    let row = record_day(
        &mut tx,
        &DayRecord {
            tenant: t,
            institution,
            staff_id: day.staff_id,
            work_date: day.work_date,
            status: b.status,
            source: AttendanceSource::Correction,
            leave_request_id: leave_request,
            remark: Some(&remark),
        },
        &a,
    )
    .await?;
    let subject = subject_of(&mut tx, t, Some(day.staff_id)).await?;
    emit_status(
        &mut tx,
        &scope,
        ATTENDANCE_CORRECTED,
        ATTENDANCE_CORRECTED_SCHEMA,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "attendance_day",
            aggregate_id: day.id.as_uuid(),
            from: Some(day.status.as_str()),
            to: row.status.as_str(),
            reason: Some(&remark),
            process_instance_id: None,
            cause: ATTENDANCE_CORRECTED,
        },
        row.version,
        &EventParties {
            staff_id: Some(day.staff_id.as_uuid()),
            engagement_id: None,
            staff_subject_ref: subject,
            manager_subject_ref: None,
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.attendance_day.correct",
        "attendance_day",
        day.id.as_uuid(),
        serde_json::json!({"from": day.status, "to": row.status, "remark": remark}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(day_response(StatusCode::OK, &row))
}

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(attendance_grid, attendance_record))
        .routes(routes!(attendance_bulk))
        .routes(routes!(staff_attendance))
        .routes(routes!(my_attendance))
        .routes(routes!(attendance_correct))
        .with_state(state)
}
