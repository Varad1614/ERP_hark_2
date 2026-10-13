//! Examination venues (Exams-owned records; no Campus room FK until Batch 8) and their
//! allocation to sittings with capacity control (L3-I-009).

use super::schedule::{allocations_of, load_sitting, ExamVenueAllocationView};
use crate::setup::common::{check_version, invalid, require_code, require_name};
use crate::setup::domain::overlaps;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::publish::audit;
use crate::shared::scope::institution_scope;
use crate::shared::ExamsState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// An examination venue.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamVenueView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub capacity: i32,
    pub location: Option<String>,
    /// `active | inactive`.
    pub status: String,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamVenueList {
    pub items: Vec<ExamVenueView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamVenueAllocationList {
    pub items: Vec<ExamVenueAllocationView>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamVenueCreate {
    #[schema(example = "HALL-A")]
    pub code: String,
    pub name: String,
    pub capacity: i32,
    pub location: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamVenueUpdate {
    pub name: Option<String>,
    pub capacity: Option<i32>,
    pub location: Option<String>,
    /// `active | inactive`.
    pub status: Option<String>,
}

/// Allocate a venue to a sitting.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamVenueAllocate {
    pub venue_id: Uuid,
    /// Seats used (defaults to the capacity still free at that time).
    pub seats: Option<i32>,
}

const VENUE_COLUMNS: &str = "id, institution_id, code, name, capacity, location, status, version";

pub async fn load_venue<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ExamVenueView, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ExamVenueView>(&format!(
        "SELECT {VENUE_COLUMNS} FROM exams.exam_venue WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "examination venue",
    })
}

fn check_capacity(c: i32) -> Result<(), KernelError> {
    if (1..=10_000).contains(&c) {
        Ok(())
    } else {
        Err(invalid("capacity", "out_of_range", "must be 1 … 10000"))
    }
}

/// Venues of an institution (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/institutions/{id}/exam-venues", operation_id = "exam_venue_list", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamVenueList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn venue_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamVenueList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let items = sqlx::query_as::<_, ExamVenueView>(&format!(
        "SELECT {VENUE_COLUMNS} FROM exams.exam_venue WHERE tenant_id = $1 AND institution_id = $2 \
         ORDER BY code"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(ExamVenueList { items }))
}

/// Register a venue (`exam.setup.manage`; code unique per institution).
#[utoipa::path(post, path = "/api/v1/exams/institutions/{id}/exam-venues", operation_id = "exam_venue_create", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamVenueCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamVenueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn venue_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamVenueCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = require_code("code", &b.code)?;
    let name = require_name("name", &b.name)?;
    check_capacity(b.capacity)?;
    let tenant = ctx.scope.tenant_id;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_venue (tenant_id, institution_id, code, name, capacity, location) \
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(b.capacity)
    .bind(
        b.location
            .as_deref()
            .map(str::trim)
            .filter(|l| !l.is_empty()),
    )
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_venue.create",
        "exam_venue",
        new_id.to_string(),
        json!({"code": code, "capacity": b.capacity}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = load_venue(&st.pool, tenant, new_id).await?;
    Ok(with_etag(StatusCode::CREATED, &v, Version(v.version)))
}

/// One venue (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-venues/{id}", operation_id = "exam_venue_get", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamVenueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn venue_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let v = load_venue(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(StatusCode::OK, &v, Version(v.version)))
}

/// Edit a venue (`exam.setup.manage`, `If-Match`).
#[utoipa::path(patch, path = "/api/v1/exams/exam-venues/{id}", operation_id = "exam_venue_update", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the venue")),
    request_body = ExamVenueUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamVenueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn venue_update(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamVenueUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let cur = load_venue(&st.pool, tenant, id).await?;
    let institution = InstitutionId(cur.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    check_version(expected, cur.version)?;
    let name = match b.name.as_deref() {
        Some(n) => require_name("name", n)?,
        None => cur.name.clone(),
    };
    let capacity = b.capacity.unwrap_or(cur.capacity);
    check_capacity(capacity)?;
    let status = b.status.clone().unwrap_or_else(|| cur.status.clone());
    if !matches!(status.as_str(), "active" | "inactive") {
        return Err(invalid("status", "invalid", "must be active or inactive").into());
    }
    let location = match b.location.as_deref() {
        Some(l) => Some(l.trim().to_owned()).filter(|l| !l.is_empty()),
        None => cur.location.clone(),
    };
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "UPDATE exams.exam_venue SET name = $3, capacity = $4, location = $5, status = $6, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $7",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(&name)
    .bind(capacity)
    .bind(location.as_deref())
    .bind(&status)
    .bind(cur.version)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_venue.update",
        "exam_venue",
        id.to_string(),
        json!({"capacity": capacity, "status": status}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = load_venue(&st.pool, tenant, id).await?;
    Ok(with_etag(StatusCode::OK, &v, Version(v.version)))
}

/// Venues allocated to a sitting (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sittings/{id}/venues", operation_id = "exam_sitting_venue_list", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamVenueAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamVenueAllocationList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_sitting(&st.pool, tenant, id).await?;
    Ok(Json(ExamVenueAllocationList {
        items: allocations_of(&st, tenant, &[id]).await?,
    }))
}

/// Seats of `venue` already allocated to sittings overlapping `(day, start, minutes)`
/// (other than `except`).
async fn seats_in_use(
    st: &ExamsState,
    tenant: TenantId,
    venue: Uuid,
    slot: (NaiveDate, NaiveTime, i32),
    except: Uuid,
) -> Result<i64, KernelError> {
    let rows: Vec<(i32, NaiveDate, NaiveTime, i32)> = sqlx::query_as(
        "SELECT a.seats, s.sits_on, s.starts_at, s.duration_minutes FROM exams.venue_allocation a \
         JOIN exams.exam_sitting s ON s.id = a.sitting_id \
         JOIN exams.exam_schedule sc ON sc.id = s.schedule_id \
         WHERE a.tenant_id = $1 AND a.venue_id = $2 AND a.status = 'allocated' AND a.sitting_id <> $3 \
         AND s.status = 'planned' AND s.sits_on = $4 \
         AND sc.status IN ('draft', 'under_review', 'approved', 'published', 'active')",
    )
    .bind(tenant.as_uuid())
    .bind(venue)
    .bind(except)
    .bind(slot.0)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .filter(|(_, d, t, m)| overlaps(slot, (*d, *t, *m)))
        .map(|(seats, ..)| i64::from(seats))
        .sum())
}

/// Allocate a venue to a sitting (`exam.setup.manage`, L3-I-009): the seats must fit in the
/// venue's capacity left over by concurrent sittings (`409` otherwise).
#[utoipa::path(post, path = "/api/v1/exams/exam-sittings/{id}/venues", operation_id = "exam_sitting_venue_allocate", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamVenueAllocate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamVenueAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamVenueAllocate>,
) -> ApiResult<(StatusCode, Json<ExamVenueAllocationList>)> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let x = load_sitting(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(x.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(x.institution_id)).await?;
    if x.status != "planned" {
        return Err(KernelError::Conflict(format!("the sitting is {}", x.status)).into());
    }
    let venue = load_venue(&st.pool, tenant, b.venue_id)
        .await
        .map_err(|_| invalid("venueId", "not_found", "unknown venue"))?;
    if venue.institution_id != x.institution_id || venue.status != "active" {
        return Err(invalid(
            "venueId",
            "unusable",
            "the venue is inactive or belongs to another institution",
        )
        .into());
    }
    let used = seats_in_use(
        &st,
        tenant,
        venue.id,
        (x.sits_on, x.starts_at, x.duration_minutes),
        id,
    )
    .await?;
    let free = i64::from(venue.capacity) - used;
    let seats = i64::from(
        b.seats
            .unwrap_or_else(|| i32::try_from(free.max(0)).unwrap_or(0)),
    );
    if seats < 1 {
        return Err(KernelError::Conflict(format!(
            "venue {} has no free seats at that time ({used} of {} in use)",
            venue.code, venue.capacity
        ))
        .into());
    }
    if seats > free {
        return Err(KernelError::Conflict(format!(
            "venue {} has only {free} free seat(s) at that time (capacity {}, {used} allocated to concurrent sittings)",
            venue.code, venue.capacity
        ))
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let alloc: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.venue_allocation (tenant_id, institution_id, sitting_id, venue_id, seats) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(x.institution_id)
    .bind(id)
    .bind(venue.id)
    .bind(i32::try_from(seats).unwrap_or(i32::MAX))
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict(format!("venue {} is already allocated to this sitting", venue.code))
        }
        other => other,
    })?;
    audit(
        &mut tx,
        &scope,
        "exams.venue_allocation.create",
        "venue_allocation",
        alloc.to_string(),
        json!({"sittingId": id, "venueId": venue.id, "seats": seats}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(ExamVenueAllocationList {
            items: allocations_of(&st, tenant, &[id]).await?,
        }),
    ))
}

/// Release a venue from a sitting (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sittings/{id}/venues/{venueId}/release", operation_id = "exam_sitting_venue_release", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("venueId" = String, Path, format = Uuid)), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamVenueAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn allocation_release(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path((id, venue_id)): Path<(Uuid, Uuid)>,
) -> ApiResult<Json<ExamVenueAllocationList>> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let tenant = ctx.scope.tenant_id;
    let x = load_sitting(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(x.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(x.institution_id)).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let n = sqlx::query(
        "UPDATE exams.venue_allocation SET status = 'released', version = version + 1, \
         updated_at = now() WHERE tenant_id = $1 AND sitting_id = $2 AND venue_id = $3 \
         AND status = 'allocated'",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .bind(venue_id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n == 0 {
        return Err(KernelError::NotFound {
            entity: "venue allocation",
        }
        .into());
    }
    audit(
        &mut tx,
        &scope,
        "exams.venue_allocation.release",
        "venue_allocation",
        id.to_string(),
        json!({"venueId": venue_id}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(ExamVenueAllocationList {
        items: allocations_of(&st, tenant, &[id]).await?,
    }))
}
