//! Offering-level enrolment governance: quotas (L3-D-015), enrolment windows (L3-D-014) and
//! eligibility / restriction references incl. cross-campus and cross-institution permits
//! (L3-D-006/008). None of these changes the offering lifecycle or decides an individual
//! registration; the effective positions are readable by Section C (and gate 4A seating).

use super::lifecycle::{changeable, close_enrolment_in, expect_version, open_enrolment_in};
use super::support::*;
use crate::shared::http::with_etag;
use crate::shared::scope::tenant;
use crate::shared::time::business_today;
use crate::shared::{publish, AcademicState};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::PgConnection;
use sutra_contracts::academic::events::offering as ev;
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::academic::status::{CourseOfferingStatus, EnrolmentWindowStatus};
use sutra_contracts::institution::status::{CampusStatus, InstitutionStatus, PublicationStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::InstitutionId;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

// ================================================================== quotas

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OfferingQuota {
    pub id: Uuid,
    pub offering_id: Uuid,
    pub category: String,
    pub seats: i32,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub rationale: String,
    /// `effective` | `superseded` | `retired`.
    pub status: String,
    pub superseded_by: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingQuotaList {
    pub items: Vec<OfferingQuota>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewOfferingQuota {
    /// Lower-case category key (`general`, `sc`, `st`, `obc`, `ews`, `pwd`, `management`, …).
    pub category: String,
    pub seats: i32,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub rationale: String,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct StatusQuery {
    pub status: Option<String>,
}

const QUOTA_COLUMNS: &str = "id, offering_id, category, seats, effective_from, effective_to, rationale, status, superseded_by, version, created_at";

/// Seat ceiling quotas are checked against: the offering capacity, else the sum of live section capacities.
async fn seat_ceiling(
    conn: &mut PgConnection,
    o: &crate::shared::offering::OfferingRow,
) -> Result<Option<i64>, KernelError> {
    if let Some(cap) = o.capacity {
        return Ok(Some(i64::from(cap)));
    }
    let (n, unlimited, sum): (i64, i64, Option<i64>) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE capacity IS NULL), sum(capacity)::int8 FROM academic.offering_section \
         WHERE offering_id = $1 AND status IN ('planned', 'active')",
    )
    .bind(o.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(if n == 0 || unlimited > 0 { None } else { sum })
}

/// Make a quota effective (L3-D-015, `offering.manage`). A previous effective quota of the same
/// category is superseded (kept as history); the effective total never exceeds the capacity.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/quotas",
    operation_id = "offering_quota_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewOfferingQuota,
    responses(
        (status = 201, body = OfferingQuota),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Lifecycle forbids it, or quotas would exceed the capacity", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn quota_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<NewOfferingQuota>,
) -> ApiResult<(StatusCode, Json<OfferingQuota>)> {
    let ctx = auth.require(Permission::OfferingManage)?;
    require_idempotency_key(&headers)?;
    let category = cmd.category.trim().to_owned();
    if category.is_empty()
        || category.len() > 40
        || !category
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
        || !category
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    {
        return Err(invalid("category", "invalid", "lower-case letters, digits and _").into());
    }
    if cmd.seats < 0 {
        return Err(invalid("seats", "negative", "seats cannot be negative").into());
    }
    if let (Some(f), Some(t)) = (cmd.effective_from, cmd.effective_to) {
        if t < f {
            return Err(invalid(
                "effectiveTo",
                "before_from",
                "must not precede effectiveFrom",
            )
            .into());
        }
    }
    let rationale = check_text("rationale", &cmd.rationale, 1000)?;
    let mut c = begin(&st, ctx, id, None).await?;
    changeable(&c.o)?;
    let ceiling = seat_ceiling(&mut c.tx, &c.o).await?;
    let others: i64 = sqlx::query_scalar(
        "SELECT COALESCE(sum(seats), 0)::int8 FROM academic.offering_quota WHERE offering_id = $1 AND status = 'effective' AND category <> $2",
    )
    .bind(c.o.id.as_uuid())
    .bind(&category)
    .fetch_one(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    match ceiling {
        None => {
            return Err(
                conflict("quotas need a defined capacity (offering or every section)").into(),
            )
        }
        Some(cap) if others + i64::from(cmd.seats) > cap => {
            return Err(conflict(format!(
                "quotas would reserve {} seat(s) but the capacity is {cap}",
                others + i64::from(cmd.seats)
            ))
            .into())
        }
        _ => {}
    }
    let prior: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM academic.offering_quota WHERE offering_id = $1 AND category = $2 AND status = 'effective' FOR UPDATE",
    )
    .bind(c.o.id.as_uuid())
    .bind(&category)
    .fetch_optional(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    if let Some(p) = prior {
        sqlx::query("UPDATE academic.offering_quota SET status = 'superseded', version = version + 1, updated_at = now() WHERE id = $1")
            .bind(p).execute(&mut *c.tx).await.map_err(map_db_error)?;
    }
    let q = sqlx::query_as::<_, OfferingQuota>(&format!(
        "INSERT INTO academic.offering_quota (tenant_id, institution_id, offering_id, category, seats, effective_from, effective_to, rationale) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {QUOTA_COLUMNS}"
    ))
    .bind(c.o.tenant_id.as_uuid())
    .bind(c.o.institution_id.as_uuid())
    .bind(c.o.id.as_uuid())
    .bind(&category)
    .bind(cmd.seats)
    .bind(cmd.effective_from)
    .bind(cmd.effective_to)
    .bind(&rationale)
    .fetch_one(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    if let Some(p) = prior {
        sqlx::query("UPDATE academic.offering_quota SET superseded_by = $2 WHERE id = $1")
            .bind(p)
            .bind(q.id)
            .execute(&mut *c.tx)
            .await
            .map_err(map_db_error)?;
    }
    c.bump().await?;
    let data = modified(c.o.id.as_uuid(), &c.o, None, c.o.version.0, &["quotas"]);
    offering_event(
        &mut c.tx,
        &c.scope,
        &c.o,
        ev::COURSE_OFFERING_CAPACITY_CHANGED,
        ev::COURSE_OFFERING_CAPACITY_CHANGED_SCHEMA,
        &data,
        &c.actor,
    )
    .await?;
    c.audit(
        "academic.offering_quota.create",
        serde_json::json!({"category": category, "seats": cmd.seats, "supersedes": prior}),
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(q)))
}

/// Quotas of an offering (`academic.read`; `?status=effective` for the current position).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/quotas",
    operation_id = "offering_quota_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid), StatusQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = OfferingQuotaList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn quota_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<OfferingQuotaList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let items = sqlx::query_as::<_, OfferingQuota>(&format!(
        "SELECT {QUOTA_COLUMNS} FROM academic.offering_quota WHERE offering_id = $1 AND ($2::text IS NULL OR status = $2) ORDER BY category, created_at"
    ))
    .bind(o.id.as_uuid())
    .bind(q.status)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(OfferingQuotaList { items }))
}

/// Retire an effective quota (`offering.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/quotas/{quotaId}/retire",
    operation_id = "offering_quota_retire",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("quotaId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the quota"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<StepReason>),
    responses(
        (status = 200, body = OfferingQuota),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn quota_retire(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, quota)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    let v = expect_version!(headers);
    let reason = body::<StepReason>(&raw)?.validated(false)?;
    let mut c = begin(&st, ctx, id, None).await?;
    changeable(&c.o)?;
    let q = sqlx::query_as::<_, OfferingQuota>(&format!("SELECT {QUOTA_COLUMNS} FROM academic.offering_quota WHERE id = $1 AND offering_id = $2 FOR UPDATE"))
        .bind(quota).bind(c.o.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "quota" })?;
    check_version(Version(q.version), v)?;
    if q.status != "effective" {
        return Err(conflict(format!("the quota is {}", q.status)).into());
    }
    let q = sqlx::query_as::<_, OfferingQuota>(&format!("UPDATE academic.offering_quota SET status = 'retired', version = version + 1, updated_at = now() WHERE id = $1 RETURNING {QUOTA_COLUMNS}"))
        .bind(quota).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    c.bump().await?;
    let data = modified(c.o.id.as_uuid(), &c.o, None, c.o.version.0, &["quotas"]);
    offering_event(
        &mut c.tx,
        &c.scope,
        &c.o,
        ev::COURSE_OFFERING_CAPACITY_CHANGED,
        ev::COURSE_OFFERING_CAPACITY_CHANGED_SCHEMA,
        &data,
        &c.actor,
    )
    .await?;
    c.audit(
        "academic.offering_quota.retire",
        serde_json::json!({"quotaId": quota, "reason": reason}),
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &q, Version(q.version)))
}

// ================================================================== enrolment windows

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingEnrolmentWindow {
    pub id: Uuid,
    pub offering_id: Uuid,
    /// `regular` | `add_drop` | `late`.
    pub kind: String,
    pub opens_on: NaiveDate,
    pub closes_on: NaiveDate,
    pub status: EnrolmentWindowStatus,
    pub status_reason: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingWindowList {
    pub items: Vec<OfferingEnrolmentWindow>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewOfferingWindow {
    pub kind: Option<String>,
    pub opens_on: NaiveDate,
    pub closes_on: NaiveDate,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingWindowCommand {
    pub reason: Option<String>,
    /// New deadline (`extend` only).
    pub closes_on: Option<NaiveDate>,
}

/// Legal enrolment-window transitions (slice-owned machine).
pub fn window_can_transition(from: EnrolmentWindowStatus, to: EnrolmentWindowStatus) -> bool {
    use EnrolmentWindowStatus::*;
    matches!(
        (from, to),
        (Scheduled, Open)
            | (Scheduled, ClosedPermanently)
            | (Open, Suspended)
            | (Open, Closed)
            | (Suspended, Open)
            | (Suspended, Closed)
            | (Closed, Open)
            | (Closed, ClosedPermanently)
    )
}

type WindowRow = (
    Uuid,
    Uuid,
    String,
    NaiveDate,
    NaiveDate,
    String,
    Option<String>,
    i64,
);
const WINDOW_COLUMNS: &str =
    "id, offering_id, kind, opens_on, closes_on, status, status_reason, version";

fn window_view(r: WindowRow) -> OfferingEnrolmentWindow {
    OfferingEnrolmentWindow {
        id: r.0,
        offering_id: r.1,
        kind: r.2,
        opens_on: r.3,
        closes_on: r.4,
        status: EnrolmentWindowStatus::parse(&r.5).unwrap_or(EnrolmentWindowStatus::Scheduled),
        status_reason: r.6,
        version: r.7,
    }
}

/// Schedule an enrolment window (L3-D-014, `offering.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/windows",
    operation_id = "offering_window_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewOfferingWindow,
    responses(
        (status = 201, body = OfferingEnrolmentWindow),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Lifecycle forbids it or it overlaps a live window of the same kind", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn window_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<NewOfferingWindow>,
) -> ApiResult<(StatusCode, Json<OfferingEnrolmentWindow>)> {
    let ctx = auth.require(Permission::OfferingManage)?;
    require_idempotency_key(&headers)?;
    let kind = cmd.kind.unwrap_or_else(|| "regular".into());
    if !matches!(kind.as_str(), "regular" | "add_drop" | "late") {
        return Err(invalid("kind", "invalid", "one of regular, add_drop, late").into());
    }
    if cmd.closes_on < cmd.opens_on {
        return Err(invalid("closesOn", "before_opens", "must not precede opensOn").into());
    }
    let mut c = begin(&st, ctx, id, None).await?;
    changeable(&c.o)?;
    if matches!(
        c.o.status,
        CourseOfferingStatus::Draft
            | CourseOfferingStatus::PendingApproval
            | CourseOfferingStatus::Suspended
    ) {
        return Err(conflict(format!(
            "the course offering is {}; windows are scheduled once it is approved",
            c.o.status
        ))
        .into());
    }
    let term = st
        .references
        .academic_term(&c.scope, c.o.academic_term_id)
        .await?
        .ok_or_else(|| conflict("the offering's academic term is no longer available"))?;
    if cmd.opens_on > term.ends_on {
        return Err(invalid(
            "opensOn",
            "after_term",
            format!("the term ends on {}", term.ends_on),
        )
        .into());
    }
    let today = business_today(&st, &c.scope, c.o.institution_id).await?;
    if cmd.closes_on < today {
        return Err(invalid("closesOn", "past", "the window would already be over").into());
    }
    let overlap: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.offering_enrolment_window WHERE offering_id = $1 AND kind = $2 \
         AND status IN ('scheduled', 'open', 'suspended') AND opens_on <= $4 AND closes_on >= $3",
    )
    .bind(c.o.id.as_uuid()).bind(&kind).bind(cmd.opens_on).bind(cmd.closes_on)
    .fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    if overlap > 0 {
        return Err(conflict("the window overlaps a live window of the same kind").into());
    }
    let r: WindowRow = sqlx::query_as(&format!(
        "INSERT INTO academic.offering_enrolment_window (tenant_id, institution_id, offering_id, kind, opens_on, closes_on) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING {WINDOW_COLUMNS}"
    ))
    .bind(c.o.tenant_id.as_uuid()).bind(c.o.institution_id.as_uuid()).bind(c.o.id.as_uuid())
    .bind(&kind).bind(cmd.opens_on).bind(cmd.closes_on)
    .fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    let w = window_view(r);
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.offering_window.schedule",
        "offering_enrolment_window",
        w.id.to_string(),
        serde_json::json!({"kind": kind, "opensOn": w.opens_on, "closesOn": w.closes_on}),
        &c.actor,
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(w)))
}

/// Enrolment windows of an offering (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/windows",
    operation_id = "offering_window_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OfferingWindowList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn window_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Json<OfferingWindowList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let rows: Vec<WindowRow> = sqlx::query_as(&format!("SELECT {WINDOW_COLUMNS} FROM academic.offering_enrolment_window WHERE offering_id = $1 ORDER BY opens_on, created_at"))
        .bind(o.id.as_uuid()).fetch_all(&st.pool).await.map_err(map_db_error)?;
    Ok(Json(OfferingWindowList {
        items: rows.into_iter().map(window_view).collect(),
    }))
}

#[derive(Clone, Copy, Debug)]
enum WindowVerb {
    Open,
    Suspend,
    Resume,
    Extend,
    Close,
    ClosePermanently,
}

async fn window_command(
    st: &AcademicState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: CourseOfferingId,
    window: Uuid,
    v: Version,
    verb: WindowVerb,
    cmd: OfferingWindowCommand,
) -> Result<OfferingEnrolmentWindow, KernelError> {
    let reason = clean_reason(
        cmd.reason,
        matches!(verb, WindowVerb::Suspend | WindowVerb::ClosePermanently),
    )?;
    let mut c = begin(st, ctx, id, None).await?;
    changeable(&c.o)?;
    let r: WindowRow = sqlx::query_as(&format!("SELECT {WINDOW_COLUMNS} FROM academic.offering_enrolment_window WHERE id = $1 AND offering_id = $2 FOR UPDATE"))
        .bind(window).bind(c.o.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "enrolment window" })?;
    let w = window_view(r);
    check_version(Version(w.version), v)?;
    let today = business_today(st, &c.scope, c.o.institution_id).await?;
    let mut closes_on = w.closes_on;
    let to = match verb {
        WindowVerb::Open | WindowVerb::Resume => EnrolmentWindowStatus::Open,
        WindowVerb::Suspend => EnrolmentWindowStatus::Suspended,
        WindowVerb::Close => EnrolmentWindowStatus::Closed,
        WindowVerb::ClosePermanently => EnrolmentWindowStatus::ClosedPermanently,
        WindowVerb::Extend => {
            let new = cmd
                .closes_on
                .ok_or_else(|| invalid("closesOn", "required", "the new deadline is required"))?;
            if new <= w.closes_on {
                return Err(invalid(
                    "closesOn",
                    "not_later",
                    "an extension moves the deadline later",
                ));
            }
            if !matches!(
                w.status,
                EnrolmentWindowStatus::Scheduled
                    | EnrolmentWindowStatus::Open
                    | EnrolmentWindowStatus::Suspended
            ) {
                return Err(conflict(format!(
                    "the window is {}; it cannot be extended",
                    w.status
                )));
            }
            closes_on = new;
            w.status
        }
    };
    if !matches!(verb, WindowVerb::Extend) && !window_can_transition(w.status, to) {
        return Err(conflict(format!(
            "the window is {}; it cannot become {to}",
            w.status
        )));
    }
    if to == EnrolmentWindowStatus::Open && matches!(verb, WindowVerb::Open | WindowVerb::Resume) {
        if today > closes_on {
            return Err(conflict("the window deadline has passed; extend it first"));
        }
        if matches!(
            c.o.status,
            CourseOfferingStatus::Published | CourseOfferingStatus::EnrollmentClosed
        ) {
            open_enrolment_in(&mut c, st, reason.as_deref()).await?;
        } else if c.o.status != CourseOfferingStatus::EnrollmentOpen
            && c.o.status != CourseOfferingStatus::Active
        {
            return Err(conflict(format!(
                "the course offering is {}; windows open on published offerings",
                c.o.status
            )));
        }
    }
    let r: WindowRow = sqlx::query_as(&format!(
        "UPDATE academic.offering_enrolment_window SET status = $2, status_reason = COALESCE($3, status_reason), closes_on = $4, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {WINDOW_COLUMNS}"
    ))
    .bind(w.id).bind(to.as_str()).bind(reason.as_deref()).bind(closes_on)
    .fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    let nw = window_view(r);
    if to == EnrolmentWindowStatus::Closed && c.o.status == CourseOfferingStatus::EnrollmentOpen {
        let still_open: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.offering_enrolment_window WHERE offering_id = $1 AND status = 'open'")
            .bind(c.o.id.as_uuid()).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
        if still_open == 0 {
            close_enrolment_in(&mut c, reason.as_deref()).await?;
        }
    }
    let verb_name = format!("{verb:?}").to_lowercase();
    publish::audit(&mut c.tx, &c.scope, &format!("academic.offering_window.{verb_name}"), "offering_enrolment_window", nw.id.to_string(), serde_json::json!({"from": w.status.as_str(), "to": nw.status.as_str(), "closesOn": nw.closes_on, "reason": reason}), &c.actor).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(nw)
}

macro_rules! window_cmd {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $verb:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "offerings",
            params(
                ("id" = String, Path, format = Uuid),
                ("windowId" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag (version) of the window"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<OfferingWindowCommand>),
            responses(
                (status = 200, body = OfferingEnrolmentWindow),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State(st): State<AcademicState>,
            auth: Authenticated,
            Path((id, window)): Path<(CourseOfferingId, Uuid)>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::OfferingDecide)?;
            let v = expect_version!(headers);
            let cmd = body::<OfferingWindowCommand>(&raw)?;
            let w = window_command(&st, ctx, id, window, v, $verb, cmd).await?;
            Ok(with_etag(StatusCode::OK, &w, Version(w.version)))
        }
    };
}

window_cmd!(
    window_open,
    "/api/v1/academic/offerings/{id}/windows/{windowId}/open",
    "offering_window_open",
    "Open a scheduled window; a published offering moves to enrollment_open (`offering.decide`).",
    WindowVerb::Open
);
window_cmd!(
    window_suspend,
    "/api/v1/academic/offerings/{id}/windows/{windowId}/suspend",
    "offering_window_suspend",
    "Suspend an open window (reason required, `offering.decide`).",
    WindowVerb::Suspend
);
window_cmd!(
    window_resume,
    "/api/v1/academic/offerings/{id}/windows/{windowId}/resume",
    "offering_window_resume",
    "Resume a suspended window (`offering.decide`).",
    WindowVerb::Resume
);
window_cmd!(
    window_extend,
    "/api/v1/academic/offerings/{id}/windows/{windowId}/extend",
    "offering_window_extend",
    "Extend a window's deadline (`closesOn` later than now, `offering.decide`).",
    WindowVerb::Extend
);
window_cmd!(
    window_close,
    "/api/v1/academic/offerings/{id}/windows/{windowId}/close",
    "offering_window_close",
    "Close a window; when no window stays open the offering closes enrolment (`offering.decide`).",
    WindowVerb::Close
);
window_cmd!(
    window_close_permanently,
    "/api/v1/academic/offerings/{id}/windows/{windowId}/close-permanently",
    "offering_window_close_permanently",
    "Close a window permanently (reason required, `offering.decide`).",
    WindowVerb::ClosePermanently
);

// ================================================================== restrictions

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct EnrolmentRestriction {
    pub id: Uuid,
    pub offering_id: Uuid,
    /// `programme` | `min_term` | `cross_campus` | `cross_institution`.
    pub kind: String,
    pub ref_ids: Vec<Uuid>,
    pub min_term_no: Option<i32>,
    pub home_approval_ref: Option<String>,
    pub host_approval_ref: Option<String>,
    pub seat_limit: Option<i32>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub reason: String,
    /// `effective` | `retired`.
    pub status: String,
    pub version: i64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct EnrolmentRestrictionList {
    pub items: Vec<EnrolmentRestriction>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewEnrolmentRestriction {
    pub kind: String,
    /// Programme ids (`programme`), campus ids (`cross_campus`) or home institution ids (`cross_institution`).
    #[serde(default)]
    pub ref_ids: Vec<Uuid>,
    pub min_term_no: Option<i32>,
    /// Cross-institution: the home institution's authorisation reference.
    pub home_approval_ref: Option<String>,
    /// Cross-institution: the offering institution's authorisation reference.
    pub host_approval_ref: Option<String>,
    pub seat_limit: Option<i32>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub reason: String,
}

const RESTRICTION_COLUMNS: &str = "id, offering_id, kind, ref_ids, min_term_no, home_approval_ref, host_approval_ref, seat_limit, effective_from, effective_to, reason, status, version";

/// Record an eligibility / restriction reference or a cross-campus / cross-institution permit
/// (L3-D-006/008, `offering.enrolment.manage`). Every reference must be authoritative (Domain B).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/restrictions",
    operation_id = "offering_restriction_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewEnrolmentRestriction,
    responses(
        (status = 201, body = EnrolmentRestriction),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "A reference is not authoritative or an approval is missing", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn restriction_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<NewEnrolmentRestriction>,
) -> ApiResult<(StatusCode, Json<EnrolmentRestriction>)> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    require_idempotency_key(&headers)?;
    let reason = check_text("reason", &cmd.reason, 1000)?;
    let mut c = begin(&st, ctx, id, None).await?;
    if c.o.status.is_terminal() {
        return Err(conflict(format!(
            "the course offering is {}; its configuration is closed",
            c.o.status
        ))
        .into());
    }
    let mut ids = cmd.ref_ids.clone();
    ids.sort();
    ids.dedup();
    let (home, host) = (
        cmd.home_approval_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
        cmd.host_approval_ref
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned),
    );
    match cmd.kind.as_str() {
        "programme" => {
            if ids.is_empty() {
                return Err(invalid("refIds", "required", "list at least one programme").into());
            }
            for p in &ids {
                let ok = st
                    .references
                    .programme(&c.scope, *p)
                    .await?
                    .is_some_and(|x| {
                        x.institution_id == c.o.institution_id
                            && x.status == PublicationStatus::Published
                    });
                if !ok {
                    return Err(invalid(
                        "refIds",
                        "not_authoritative",
                        format!("{p} is not a published programme of this institution"),
                    )
                    .into());
                }
            }
        }
        "min_term" => {
            if !cmd.min_term_no.is_some_and(|n| (1..=20).contains(&n)) {
                return Err(invalid("minTermNo", "required", "1 to 20").into());
            }
        }
        "cross_campus" => {
            if ids.is_empty() {
                return Err(invalid("refIds", "required", "list at least one campus").into());
            }
            for cid in &ids {
                let ok = st
                    .references
                    .campus(&c.scope, *cid)
                    .await?
                    .is_some_and(|x| {
                        x.institution_id == c.o.institution_id && x.status == CampusStatus::Active
                    });
                if !ok {
                    return Err(invalid(
                        "refIds",
                        "not_authoritative",
                        format!("{cid} is not an active campus of this institution"),
                    )
                    .into());
                }
            }
        }
        "cross_institution" => {
            if ids.is_empty() {
                return Err(
                    invalid("refIds", "required", "list at least one home institution").into(),
                );
            }
            if home.is_none() || host.is_none() {
                return Err(invalid(
                    "homeApprovalRef",
                    "required",
                    "cross-institution enrolment needs both institutions' authorisation",
                )
                .into());
            }
            for i in &ids {
                if *i == c.o.institution_id.as_uuid() {
                    return Err(invalid(
                        "refIds",
                        "same_institution",
                        "use cross_campus inside one institution",
                    )
                    .into());
                }
                let ok = st
                    .references
                    .institution(&c.scope, InstitutionId(*i))
                    .await?
                    .is_some_and(|x| x.status == InstitutionStatus::Active);
                if !ok {
                    return Err(invalid(
                        "refIds",
                        "not_authoritative",
                        format!("{i} is not an active institution of this tenant"),
                    )
                    .into());
                }
            }
        }
        _ => {
            return Err(invalid(
                "kind",
                "invalid",
                "one of programme, min_term, cross_campus, cross_institution",
            )
            .into())
        }
    }
    if let (Some(f), Some(t)) = (cmd.effective_from, cmd.effective_to) {
        if t < f {
            return Err(invalid(
                "effectiveTo",
                "before_from",
                "must not precede effectiveFrom",
            )
            .into());
        }
    }
    if cmd.seat_limit.is_some_and(|s| s < 0) {
        return Err(invalid("seatLimit", "negative", "cannot be negative").into());
    }
    let r = sqlx::query_as::<_, EnrolmentRestriction>(&format!(
        "INSERT INTO academic.enrolment_restriction (tenant_id, institution_id, offering_id, kind, ref_ids, min_term_no, \
         home_approval_ref, host_approval_ref, seat_limit, effective_from, effective_to, reason) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING {RESTRICTION_COLUMNS}"
    ))
    .bind(c.o.tenant_id.as_uuid()).bind(c.o.institution_id.as_uuid()).bind(c.o.id.as_uuid())
    .bind(&cmd.kind).bind(&ids).bind(cmd.min_term_no).bind(&home).bind(&host).bind(cmd.seat_limit)
    .bind(cmd.effective_from).bind(cmd.effective_to).bind(&reason)
    .fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.enrolment_restriction.create",
        "enrolment_restriction",
        r.id.to_string(),
        serde_json::json!({"kind": r.kind, "refIds": r.ref_ids}),
        &c.actor,
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(r)))
}

/// Restrictions of an offering (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/restrictions",
    operation_id = "offering_restriction_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid), StatusQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = EnrolmentRestrictionList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn restriction_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<EnrolmentRestrictionList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let items = sqlx::query_as::<_, EnrolmentRestriction>(&format!("SELECT {RESTRICTION_COLUMNS} FROM academic.enrolment_restriction WHERE offering_id = $1 AND ($2::text IS NULL OR status = $2) ORDER BY created_at"))
        .bind(o.id.as_uuid()).bind(q.status).fetch_all(&st.pool).await.map_err(map_db_error)?;
    Ok(Json(EnrolmentRestrictionList { items }))
}

/// Retire a restriction (`offering.enrolment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/restrictions/{restrictionId}/retire",
    operation_id = "offering_restriction_retire",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("restrictionId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the restriction"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = EnrolmentRestriction),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn restriction_retire(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, rid)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    let v = expect_version!(headers);
    let mut c = begin(&st, ctx, id, None).await?;
    if c.o.status.is_terminal() {
        return Err(conflict(format!(
            "the course offering is {}; its configuration is closed",
            c.o.status
        ))
        .into());
    }
    let r = sqlx::query_as::<_, EnrolmentRestriction>(&format!("SELECT {RESTRICTION_COLUMNS} FROM academic.enrolment_restriction WHERE id = $1 AND offering_id = $2 FOR UPDATE"))
        .bind(rid).bind(c.o.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "restriction" })?;
    check_version(Version(r.version), v)?;
    if r.status != "effective" {
        return Err(conflict("the restriction is already retired").into());
    }
    let r = sqlx::query_as::<_, EnrolmentRestriction>(&format!("UPDATE academic.enrolment_restriction SET status = 'retired', version = version + 1, updated_at = now() WHERE id = $1 RETURNING {RESTRICTION_COLUMNS}"))
        .bind(rid).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.enrolment_restriction.retire",
        "enrolment_restriction",
        r.id.to_string(),
        serde_json::json!({}),
        &c.actor,
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_machine() {
        use EnrolmentWindowStatus::*;
        assert!(window_can_transition(Scheduled, Open));
        assert!(window_can_transition(Open, Suspended));
        assert!(window_can_transition(Suspended, Open));
        assert!(window_can_transition(Closed, ClosedPermanently));
        assert!(!window_can_transition(ClosedPermanently, Open));
        assert!(!window_can_transition(Scheduled, Suspended));
    }
}
