//! Sections of an offering (create, edit, activate, cancel) and capacity governance
//! (L3-D-005): offering-wide and per-section seat caps with an immutable history.

use super::lifecycle::{changeable, end_assignments, etag, expect_version, unique_code};
use super::support::*;
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_section_status, StatusChange};
use crate::shared::offering::{
    load_section, lock_offering, lock_section, section_from_row, OfferingRow, SectionRow,
    SECTION_COLUMNS,
};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{publish, AcademicState};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sqlx::PgConnection;
use sutra_contracts::academic::events::offering as ev;
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId};
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, SectionStatus,
};
use sutra_contracts::institution::status::CampusStatus;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewOfferingSection {
    /// `A`, `B`, `LAB-1` … (unique per offering among live sections).
    pub section_code: String,
    pub capacity: Option<i32>,
    /// Campus where the section is taught (cross-campus delivery); defaults to the offering's.
    pub campus_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOfferingSection {
    pub section_code: Option<String>,
    pub campus_id: Option<Uuid>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingSectionList {
    pub items: Vec<OfferingSection>,
}

/// Capacity change of the offering (no `sectionId`) or one section (L3-D-005).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingCapacityChange {
    pub section_id: Option<Uuid>,
    /// New seat cap; `null` = unlimited.
    pub capacity: Option<i32>,
    pub reason: Option<String>,
}

/// One entry of the capacity history.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct OfferingCapacityHistoryEntry {
    pub id: Uuid,
    pub section_id: Option<Uuid>,
    pub from_capacity: Option<i32>,
    pub to_capacity: Option<i32>,
    pub seated_at_change: i32,
    pub reason: String,
    pub actor: Option<String>,
    pub effective_on: chrono::NaiveDate,
    pub recorded_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingCapacityHistory {
    pub items: Vec<OfferingCapacityHistoryEntry>,
}

async fn check_campus(
    st: &AcademicState,
    scope: &ExecutionScope,
    o: &OfferingRow,
    campus: Option<Uuid>,
) -> Result<(), KernelError> {
    if let Some(c) = campus {
        let campus = st
            .references
            .campus(scope, c)
            .await?
            .filter(|x| x.institution_id == o.institution_id)
            .ok_or_else(|| {
                invalid(
                    "campusId",
                    "not_found",
                    "no such campus in this institution",
                )
            })?;
        if campus.status != CampusStatus::Active {
            return Err(invalid(
                "campusId",
                "not_active",
                "the campus is not active",
            ));
        }
    }
    Ok(())
}

/// Inserts a planned section of `o` (validated; 409 on a duplicate live code).
pub async fn insert_section(
    st: &AcademicState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    o: &OfferingRow,
    s: &NewOfferingSection,
) -> Result<SectionRow, KernelError> {
    let code = check_code("sectionCode", &s.section_code, 20, false)?;
    let capacity = check_capacity("capacity", s.capacity)?;
    check_campus(st, scope, o, s.campus_id).await?;
    insert_section_row(
        conn,
        o,
        &code,
        capacity,
        s.campus_id.or(o.campus_id),
        SectionStatus::Planned,
    )
    .await
}

/// Raw section insert (status as given; used by merge/split for successor sections).
pub async fn insert_section_row(
    conn: &mut PgConnection,
    o: &OfferingRow,
    code: &str,
    capacity: Option<i32>,
    campus: Option<Uuid>,
    status: SectionStatus,
) -> Result<SectionRow, KernelError> {
    let sql = format!(
        "INSERT INTO academic.offering_section (tenant_id, institution_id, offering_id, section_code, \
         campus_id, capacity, status) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {SECTION_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(o.tenant_id.as_uuid())
        .bind(o.institution_id.as_uuid())
        .bind(o.id.as_uuid())
        .bind(code)
        .bind(campus)
        .bind(capacity)
        .bind(status.as_str())
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| unique_code(e, "sectionCode", "a live section with this code exists"))?;
    section_from_row(&row).map_err(map_db_error)
}

fn section_etag(status: StatusCode, s: &OfferingSection) -> Response {
    with_etag(status, s, Version(s.version))
}

/// Add a planned section to an offering (`offering.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/sections",
    operation_id = "offering_section_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid, description = "Offering id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewOfferingSection,
    responses(
        (status = 201, body = OfferingSection, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<NewOfferingSection>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    require_idempotency_key(&headers)?;
    let mut c = begin(&st, ctx, id, None).await?;
    changeable(&c.o)?;
    if c.o.status == CourseOfferingStatus::Suspended {
        return Err(conflict("the course offering is suspended; sections cannot be added").into());
    }
    let s = insert_section(&st, &mut c.tx, &c.scope, &c.o, &cmd).await?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.offering_section.create",
        "offering_section",
        s.id.to_string(),
        serde_json::json!({"offeringId": c.o.id, "sectionCode": s.section_code}),
        &c.actor,
    )
    .await?;
    let v = section_view(&mut c.tx, &s).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(section_etag(StatusCode::CREATED, &v))
}

/// Sections of an offering (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/sections",
    operation_id = "offering_section_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid, description = "Offering id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = OfferingSectionList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Json<OfferingSectionList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = load_view(&st, tenant(ctx), id).await?;
    Ok(Json(OfferingSectionList { items: o.sections }))
}

/// One section with its faculty and seat count (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/sections/{id}",
    operation_id = "offering_section_get",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = OfferingSection, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<OfferingSectionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = load_section(&mut *conn, tenant(ctx), id).await?;
    Ok(section_etag(
        StatusCode::OK,
        &section_view(&mut conn, &s).await?,
    ))
}

/// Section command context: offering + section locked (offering first).
async fn begin_section(
    st: &AcademicState,
    ctx: &AuthContext,
    id: OfferingSectionId,
    expected: Option<Version>,
) -> Result<(Cmd, SectionRow), KernelError> {
    let s = load_section(&st.pool, tenant(ctx), id).await?;
    let mut c = begin(st, ctx, s.offering_id, None).await?;
    let s = lock_section(&mut c.tx, c.o.tenant_id, id).await?;
    if let Some(v) = expected {
        check_version(s.version, v)?;
    }
    Ok((c, s))
}

/// Edit a section's code (planned only) or campus (`offering.manage`).
#[utoipa::path(
    patch,
    path = "/api/v1/academic/sections/{id}",
    operation_id = "offering_section_update",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the section"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateOfferingSection,
    responses(
        (status = 200, body = OfferingSection, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<OfferingSectionId>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateOfferingSection>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    let v = expect_version!(headers);
    let (mut c, s) = begin_section(&st, ctx, id, Some(v)).await?;
    changeable(&c.o)?;
    if !matches!(s.status, SectionStatus::Planned | SectionStatus::Active) {
        return Err(conflict(format!("the section is {}", s.status)).into());
    }
    let code = match &cmd.section_code {
        Some(x) if s.status != SectionStatus::Planned && x != &s.section_code => {
            return Err(conflict("the code of an active section cannot change").into())
        }
        Some(x) => check_code("sectionCode", x, 20, false)?,
        None => s.section_code.clone(),
    };
    check_campus(&st, &c.scope, &c.o, cmd.campus_id).await?;
    let sql = format!(
        "UPDATE academic.offering_section SET section_code = $2, campus_id = COALESCE($3, campus_id), \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {SECTION_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(s.id.as_uuid())
        .bind(&code)
        .bind(cmd.campus_id)
        .fetch_one(&mut *c.tx)
        .await
        .map_err(|e| unique_code(e, "sectionCode", "a live section with this code exists"))?;
    let s = section_from_row(&row).map_err(map_db_error)?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.offering_section.update",
        "offering_section",
        s.id.to_string(),
        serde_json::json!({"sectionCode": code}),
        &c.actor,
    )
    .await?;
    let view = section_view(&mut c.tx, &s).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(section_etag(StatusCode::OK, &view))
}

async fn section_transition(
    st: &AcademicState,
    ctx: &AuthContext,
    id: OfferingSectionId,
    v: Version,
    to: SectionStatus,
    reason: Option<String>,
) -> Result<OfferingSection, KernelError> {
    let (mut c, s) = begin_section(st, ctx, id, Some(v)).await?;
    changeable(&c.o)?;
    match to {
        SectionStatus::Active => {
            if !matches!(
                c.o.status,
                CourseOfferingStatus::Published
                    | CourseOfferingStatus::EnrollmentOpen
                    | CourseOfferingStatus::EnrollmentClosed
                    | CourseOfferingStatus::Active
            ) {
                return Err(conflict(format!(
                    "the course offering is {}; sections activate once it is published",
                    c.o.status
                )));
            }
        }
        SectionStatus::Cancelled => {
            if seated_in_section(&mut *c.tx, s.id).await? > 0 {
                return Err(conflict(
                    "students are seated in the section; transfer or withdraw them first",
                ));
            }
            let (scope, o, a) = (c.scope, c.o.clone(), c.actor.clone());
            end_assignments(
                &mut c.tx,
                &scope,
                &o,
                Some(s.id),
                FacultyAssignmentStatus::Revoked,
                ev::COURSE_OFFERING_CAPACITY_CHANGED,
                reason.as_deref(),
                &a,
            )
            .await?;
        }
        _ => {}
    }
    let s = change_section_status(
        &mut c.tx,
        &s,
        to,
        &StatusChange {
            cause_event_type: ev::COURSE_OFFERING_CAPACITY_CHANGED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &c.actor,
    )
    .await?;
    let action = if to == SectionStatus::Active {
        "activate"
    } else {
        "cancel"
    };
    publish::audit(
        &mut c.tx,
        &c.scope,
        &format!("academic.offering_section.{action}"),
        "offering_section",
        s.id.to_string(),
        serde_json::json!({"reason": reason}),
        &c.actor,
    )
    .await?;
    let view = section_view(&mut c.tx, &s).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(view)
}

macro_rules! section_command {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $to:expr, $reason_required:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "offerings",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the section"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<StepReason>),
            responses(
                (status = 200, body = OfferingSection, headers(("ETag" = String))),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State(st): State<AcademicState>,
            auth: Authenticated,
            Path(id): Path<OfferingSectionId>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::OfferingManage)?;
            let v = expect_version!(headers);
            let reason = body::<StepReason>(&raw)?.validated($reason_required)?;
            let s = section_transition(&st, ctx, id, v, $to, reason).await?;
            Ok(section_etag(StatusCode::OK, &s))
        }
    };
}

section_command!(
    activate,
    "/api/v1/academic/sections/{id}/activate",
    "offering_section_activate",
    "Activate a planned section of a published offering (`offering.manage`).",
    SectionStatus::Active,
    false
);
section_command!(
    cancel,
    "/api/v1/academic/sections/{id}/cancel",
    "offering_section_cancel",
    "Cancel a section without seated students (reason required, `offering.manage`).",
    SectionStatus::Cancelled,
    true
);

// ================================================================== capacity

/// Change the offering or a section capacity (L3-D-005, `offering.manage`). Never below the
/// seated count, never below the effective quotas; lifecycle unchanged; history kept.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/capacity",
    operation_id = "offering_capacity_change",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the offering"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = OfferingCapacityChange,
    responses(
        (status = 200, body = Offering, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Lifecycle forbids it, or the reduction would unseat students / break quotas", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn change_capacity(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<OfferingCapacityChange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    let v = expect_version!(headers);
    let o = apply_capacity(&st, ctx, id, v, cmd).await?;
    Ok(etag(StatusCode::OK, &o))
}

pub async fn apply_capacity(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    cmd: OfferingCapacityChange,
) -> Result<Offering, KernelError> {
    let reason = clean_reason(cmd.reason, true)?.unwrap_or_default();
    let capacity = check_capacity("capacity", cmd.capacity)?;
    let mut c = begin(st, ctx, id, Some(v)).await?;
    changeable(&c.o)?;
    let today = business_today(st, &c.scope, c.o.institution_id).await?;
    let (from, seated, section) = match cmd.section_id {
        Some(sid) => {
            let s = lock_section(&mut c.tx, c.o.tenant_id, OfferingSectionId(sid)).await?;
            if s.offering_id != c.o.id {
                return Err(KernelError::NotFound { entity: "section" });
            }
            if !matches!(s.status, SectionStatus::Planned | SectionStatus::Active) {
                return Err(conflict(format!("the section is {}", s.status)));
            }
            (
                s.capacity,
                seated_in_section(&mut *c.tx, s.id).await?,
                Some(s),
            )
        }
        None => (
            c.o.capacity,
            seated_in_offering(&mut *c.tx, c.o.id).await?,
            None,
        ),
    };
    if let Some(cap) = capacity {
        if i64::from(cap) < seated {
            return Err(conflict(format!(
                "{seated} student(s) are seated; the capacity cannot drop to {cap}"
            )));
        }
        if section.is_none() {
            let quotas: i64 = sqlx::query_scalar(
                "SELECT COALESCE(sum(seats), 0)::int8 FROM academic.offering_quota WHERE offering_id = $1 AND status = 'effective'",
            )
            .bind(c.o.id.as_uuid())
            .fetch_one(&mut *c.tx)
            .await
            .map_err(map_db_error)?;
            if i64::from(cap) < quotas {
                return Err(conflict(format!(
                    "effective quotas reserve {quotas} seat(s); the capacity cannot drop to {cap}"
                )));
            }
        }
    }
    match &section {
        Some(s) => {
            sqlx::query("UPDATE academic.offering_section SET capacity = $2, version = version + 1, updated_at = now() WHERE id = $1")
                .bind(s.id.as_uuid()).bind(capacity).execute(&mut *c.tx).await.map_err(map_db_error)?;
            c.bump().await?;
        }
        None => {
            sqlx::query("UPDATE academic.course_offering SET capacity = $2, version = version + 1, updated_at = now() WHERE id = $1")
                .bind(c.o.id.as_uuid()).bind(capacity).execute(&mut *c.tx).await.map_err(map_db_error)?;
            c.o = lock_offering(&mut c.tx, c.o.tenant_id, c.o.id).await?;
        }
    }
    sqlx::query(
        "INSERT INTO academic.offering_capacity_change (tenant_id, institution_id, offering_id, section_id, \
         from_capacity, to_capacity, seated_at_change, reason, actor, effective_on) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(c.o.tenant_id.as_uuid())
    .bind(c.o.institution_id.as_uuid())
    .bind(c.o.id.as_uuid())
    .bind(section.as_ref().map(|s| s.id.as_uuid()))
    .bind(from)
    .bind(capacity)
    .bind(i32::try_from(seated).unwrap_or(i32::MAX))
    .bind(&reason)
    .bind(c.actor.id.as_deref())
    .bind(today)
    .execute(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    let field = if section.is_some() {
        "sections.capacity"
    } else {
        "capacity"
    };
    let data = modified(
        c.o.id.as_uuid(),
        &c.o,
        section.as_ref().map(|s| s.id.as_uuid()),
        c.o.version.0,
        &[field],
    );
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
    c.audit("academic.course_offering.change_capacity", serde_json::json!({"from": from, "to": capacity, "sectionId": cmd.section_id, "reason": reason})).await?;
    c.finish().await
}

/// Capacity history of an offering, newest first (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/capacity",
    operation_id = "offering_capacity_history",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = OfferingCapacityHistory),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn capacity_history(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Json<OfferingCapacityHistory>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let items = sqlx::query_as::<_, OfferingCapacityHistoryEntry>(
        "SELECT id, section_id, from_capacity, to_capacity, seated_at_change, reason, actor, effective_on, recorded_at \
         FROM academic.offering_capacity_change WHERE offering_id = $1 ORDER BY recorded_at DESC",
    )
    .bind(o.id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(OfferingCapacityHistory { items }))
}

/// Unused helper guard (scope helpers are used by sibling modules through this one).
#[allow(dead_code)]
fn _scope_helpers(ctx: &AuthContext, o: &OfferingRow) -> ExecutionScope {
    let _ = actor(ctx);
    let _ = require_institution_writable;
    institution_scope(ctx, o.institution_id)
}
