//! Laboratories (L2-G02; L3-G-050, 053, 057, 060): the lab profile of a `lab` space, readiness
//! checklist → `ready` / `not_ready`, equipment status (anything not operational → the lab is
//! `restricted`), practical sessions planned → ready → in progress → completed (a ready lab is
//! required), postponement / cancellation, and batch allocation of students with a capacity check.

use crate::infrastructure::common::{
    audit, clean, emit_status, ensure_version, expected, fmt_time, history, illegal, invalid,
    parse_date, parse_filter, parse_time, required, EventExtras, FacilityReasonBody, Transition,
};
use crate::infrastructure::domain::{
    lab_status, overlaps, EquipmentStatus, LabStatus, PracticalSessionStatus,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::require_person;
use crate::shared::rows::{decode, load_node};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::events::facilities::{
    PRACTICAL_SESSION_COMPLETED, PRACTICAL_SESSION_COMPLETED_SCHEMA,
};
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{InfrastructureStatus, PersonKind, SpaceType};
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

// ------------------------------------------------------------------ views + bodies

/// One readiness checklist item.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabChecklistItem {
    pub item: String,
    pub done: bool,
}

/// A laboratory.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub node_id: Uuid,
    pub node_code: String,
    pub name: String,
    pub capacity: i32,
    pub status: LabStatus,
    pub status_reason: Option<String>,
    pub checklist: Vec<LabChecklistItem>,
    pub readiness_notes: Option<String>,
    pub readiness_checked_at: Option<DateTime<Utc>>,
    pub version: i64,
}

/// A piece of lab equipment.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabEquipmentView {
    pub id: Uuid,
    pub lab_id: Uuid,
    pub code: String,
    pub name: String,
    pub status: EquipmentStatus,
    pub status_reason: Option<String>,
    pub version: i64,
}

/// A practical session.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PracticalSessionView {
    pub id: Uuid,
    pub lab_id: Uuid,
    pub title: String,
    pub course_code: Option<String>,
    pub session_date: NaiveDate,
    pub start_time: String,
    pub end_time: String,
    pub capacity: i32,
    /// Students allocated so far.
    pub allocated: i64,
    pub status: PracticalSessionStatus,
    pub status_reason: Option<String>,
    pub version: i64,
}

/// A lab with its equipment and sessions.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabDetail {
    pub lab: LabView,
    pub equipment: Vec<LabEquipmentView>,
    pub sessions: Vec<PracticalSessionView>,
    pub version: i64,
}

/// Labs.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LabList {
    pub items: Vec<LabView>,
}

/// Lab equipment.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LabEquipmentList {
    pub items: Vec<LabEquipmentView>,
}

/// Practical sessions.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PracticalSessionList {
    pub items: Vec<PracticalSessionView>,
}

/// A student allocated to a practical session.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabBatchAllocationView {
    pub id: Uuid,
    pub session_id: Uuid,
    pub person_kind: PersonKind,
    pub person_id: Uuid,
    pub batch_label: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Allocations.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LabBatchAllocationList {
    pub items: Vec<LabBatchAllocationView>,
}

/// Create the lab profile of a `lab` space.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabCreate {
    pub node_id: Uuid,
    /// Defaults to the space name.
    pub name: Option<String>,
    /// Defaults to the space capacity.
    pub capacity: Option<i32>,
}

/// A readiness check.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabReadinessBody {
    pub checklist: Vec<LabChecklistItem>,
    pub notes: Option<String>,
}

/// Register equipment.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabEquipmentCreate {
    #[schema(example = "MICRO-01")]
    pub code: String,
    pub name: String,
}

/// Change the status of equipment.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabEquipmentStatusBody {
    pub status: EquipmentStatus,
    pub reason: Option<String>,
}

/// Plan a practical session.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PracticalSessionCreate {
    pub title: String,
    pub course_code: Option<String>,
    #[schema(example = "2026-12-02")]
    pub session_date: String,
    #[schema(example = "14:00")]
    pub start_time: String,
    #[schema(example = "16:00")]
    pub end_time: String,
    /// Defaults to (and at most) the lab capacity.
    pub capacity: Option<i32>,
}

/// Postpone a session (optionally to a new slot).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PracticalSessionPostpone {
    pub reason: Option<String>,
    pub session_date: Option<String>,
    pub start_time: Option<String>,
    pub end_time: Option<String>,
}

/// Allocate a batch of students to a session.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LabBatchAllocationCreate {
    pub session_id: Uuid,
    pub batch_label: Option<String>,
    /// Domain C student ids.
    pub student_ids: Vec<Uuid>,
}

/// Filters of lab lists.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct LabQuery {
    pub status: Option<String>,
    /// Allocation list: one session.
    pub session_id: Option<Uuid>,
}

// ------------------------------------------------------------------ rows

const LAB_COLUMNS: &str = "l.id, l.institution_id, l.node_id, n.code AS node_code, l.name, \
    l.capacity, l.status, l.status_reason, l.checklist, l.readiness_notes, \
    l.readiness_checked_at, l.version";
const LAB_FROM: &str = "campus.lab l JOIN campus.infrastructure_node n ON n.id = l.node_id";

fn lab_from_row(r: &PgRow) -> Result<LabView, sqlx::Error> {
    let checklist: serde_json::Value = r.try_get("checklist")?;
    Ok(LabView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        node_id: r.try_get("node_id")?,
        node_code: r.try_get("node_code")?,
        name: r.try_get("name")?,
        capacity: r.try_get("capacity")?,
        status: decode(r.try_get("status")?, LabStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        checklist: serde_json::from_value(checklist).unwrap_or_default(),
        readiness_notes: r.try_get("readiness_notes")?,
        readiness_checked_at: r.try_get("readiness_checked_at")?,
        version: r.try_get("version")?,
    })
}

async fn load_lab(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LabView, KernelError> {
    let sql = format!(
        "SELECT {LAB_COLUMNS} FROM {LAB_FROM} WHERE l.id = $1 AND l.tenant_id = $2{}",
        if lock { " FOR UPDATE OF l" } else { "" }
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(lab_from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "lab" })
}

const EQ_COLUMNS: &str = "id, lab_id, code, name, status, status_reason, version";

fn eq_from_row(r: &PgRow) -> Result<LabEquipmentView, sqlx::Error> {
    Ok(LabEquipmentView {
        id: r.try_get("id")?,
        lab_id: r.try_get("lab_id")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        status: decode(r.try_get("status")?, EquipmentStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        version: r.try_get("version")?,
    })
}

async fn equipment_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    lab: Uuid,
) -> Result<Vec<LabEquipmentView>, KernelError> {
    let sql = format!(
        "SELECT {EQ_COLUMNS} FROM campus.lab_equipment WHERE tenant_id = $1 AND lab_id = $2 \
         ORDER BY code"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(lab)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(eq_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

const SESSION_COLUMNS: &str = "s.id, s.lab_id, s.institution_id, s.title, s.course_code, \
    s.session_date, s.start_time, s.end_time, s.capacity, s.status, s.status_reason, s.version, \
    (SELECT count(*) FROM campus.lab_batch_allocation a WHERE a.session_id = s.id) AS allocated";

fn session_from_row(r: &PgRow) -> Result<(PracticalSessionView, InstitutionId), sqlx::Error> {
    let start: NaiveTime = r.try_get("start_time")?;
    let end: NaiveTime = r.try_get("end_time")?;
    Ok((
        PracticalSessionView {
            id: r.try_get("id")?,
            lab_id: r.try_get("lab_id")?,
            title: r.try_get("title")?,
            course_code: r.try_get("course_code")?,
            session_date: r.try_get("session_date")?,
            start_time: fmt_time(start),
            end_time: fmt_time(end),
            capacity: r.try_get("capacity")?,
            allocated: r.try_get("allocated")?,
            status: decode(r.try_get("status")?, PracticalSessionStatus::parse)?,
            status_reason: r.try_get("status_reason")?,
            version: r.try_get("version")?,
        },
        InstitutionId(r.try_get("institution_id")?),
    ))
}

async fn sessions_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    lab: Uuid,
    status: Option<PracticalSessionStatus>,
) -> Result<Vec<PracticalSessionView>, KernelError> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM campus.practical_session s WHERE s.tenant_id = $1 \
         AND s.lab_id = $2 AND ($3::text IS NULL OR s.status = $3) \
         ORDER BY s.session_date DESC, s.start_time DESC LIMIT 200"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(lab)
        .bind(status.map(|s| s.as_str()))
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(|r| session_from_row(r).map(|x| x.0))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

async fn load_session(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<(PracticalSessionView, InstitutionId), KernelError> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM campus.practical_session s WHERE s.id = $1 \
         AND s.tenant_id = $2{}",
        if lock { " FOR UPDATE OF s" } else { "" }
    );
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(session_from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "practical session",
        })
}

async fn detail(st: &CampusState, tenant: TenantId, id: Uuid) -> Result<LabDetail, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let lab = load_lab(&mut conn, tenant, id, false).await?;
    let equipment = equipment_of(&mut conn, tenant, id).await?;
    let sessions = sessions_of(&mut conn, tenant, id, None).await?;
    Ok(LabDetail {
        version: lab.version,
        lab,
        equipment,
        sessions,
    })
}

fn respond(status: StatusCode, d: &LabDetail) -> Response {
    with_etag(status, d, Version(d.version))
}

/// Recomputes and stores the lab status (history on a change); returns the new status.
async fn refresh_lab_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    lab: &LabView,
    reason: Option<&str>,
    bump: bool,
    actor: &Actor,
) -> Result<LabStatus, KernelError> {
    let node = load_node(conn, tenant, InfrastructureNodeId(lab.node_id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "infrastructure node",
        })?;
    let equipment = equipment_of(conn, tenant, lab.id).await?;
    let complete = !lab.checklist.is_empty() && lab.checklist.iter().all(|i| i.done);
    let to = if node.status != InfrastructureStatus::Active {
        LabStatus::Unavailable
    } else {
        lab_status(
            complete,
            &equipment.iter().map(|e| e.status).collect::<Vec<_>>(),
        )
    };
    if to != lab.status || bump {
        sqlx::query(
            "UPDATE campus.lab SET status = $3, status_reason = $4, version = version + 1, \
             updated_at = now() WHERE id = $1 AND tenant_id = $2",
        )
        .bind(lab.id)
        .bind(tenant.as_uuid())
        .bind(to.as_str())
        .bind(reason)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    if to != lab.status {
        history(
            conn,
            &Transition {
                tenant,
                institution: InstitutionId(lab.institution_id),
                aggregate_type: "lab",
                aggregate_id: lab.id,
                from: Some(lab.status.as_str()),
                to: to.as_str(),
                reason,
                process_instance_id: None,
                cause: "",
            },
            actor,
        )
        .await?;
    }
    Ok(to)
}

// ------------------------------------------------------------------ handlers: labs + equipment

/// Create the lab profile of a `lab` space (`campus.facility.manage`; one per space).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/labs", operation_id = "campus_lab_create", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LabCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LabDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LabCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = load_node(&mut tx, t, InfrastructureNodeId(b.node_id))
        .await?
        .filter(|n| n.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                "nodeId",
                "unknown_node",
                "no infrastructure node with this id",
            )
        })?;
    if node.space_type != Some(SpaceType::Lab) {
        return Err(invalid(
            "nodeId",
            "not_a_lab",
            "a lab profile belongs to a `lab` space",
        )
        .into());
    }
    if matches!(
        node.status,
        InfrastructureStatus::Decommissioned | InfrastructureStatus::Archived
    ) {
        return Err(
            KernelError::Conflict(format!("space {} is {}", node.code, node.status)).into(),
        );
    }
    let capacity = b.capacity.or(node.capacity).ok_or_else(|| {
        invalid(
            "capacity",
            "required",
            "the space has no capacity; give one",
        )
    })?;
    if capacity < 1 || node.capacity.is_some_and(|c| capacity > c) {
        return Err(invalid(
            "capacity",
            "out_of_range",
            "at least 1 and at most the space capacity",
        )
        .into());
    }
    let name = clean(b.name).unwrap_or_else(|| node.name.clone());
    let lid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.lab (tenant_id, institution_id, node_id, name, capacity) \
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.node_id)
    .bind(&name)
    .bind(capacity)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => {
            KernelError::Conflict(format!("space {} already has a lab profile", node.code))
        }
        _ => map_db_error(e),
    })?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "lab",
            aggregate_id: lid,
            from: None,
            to: LabStatus::NotReady.as_str(),
            reason: None,
            process_instance_id: None,
            cause: "",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.lab.create",
        "lab",
        lid,
        serde_json::json!({"nodeId": b.node_id, "capacity": capacity}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, lid).await?;
    Ok(respond(StatusCode::CREATED, &d))
}

/// Labs of an institution (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/labs", operation_id = "campus_lab_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), LabQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LabList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LabQuery>,
) -> ApiResult<Json<LabList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = parse_filter("status", q.status.as_deref(), LabStatus::parse)?;
    let sql = format!(
        "SELECT {LAB_COLUMNS} FROM {LAB_FROM} WHERE l.tenant_id = $1 AND l.institution_id = $2 \
         AND ($3::text IS NULL OR l.status = $3) ORDER BY n.code"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(lab_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LabList { items }))
}

/// One lab with its equipment and sessions (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/labs/{id}", operation_id = "campus_lab_get", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LabDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    let d = detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Record a readiness check (`campus.facility.manage`): a complete checklist makes the lab
/// `ready` (or `restricted` while equipment is not operational); an incomplete one `not_ready`.
#[utoipa::path(post, path = "/api/v1/campus/labs/{id}/readiness", operation_id = "campus_lab_readiness", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the lab")),
    request_body = LabReadinessBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LabDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_readiness(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LabReadinessBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    if b.checklist.is_empty()
        || b.checklist
            .iter()
            .any(|i| i.item.trim().is_empty() || i.item.len() > 200)
    {
        return Err(invalid(
            "checklist",
            "invalid",
            "at least one item, 1–200 characters each",
        )
        .into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let notes = clean(b.notes);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let lab = load_lab(&mut tx, t, id, true).await?;
    ensure_version(v, lab.version)?;
    let scope = institution_scope(ctx, InstitutionId(lab.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(lab.institution_id)).await?;
    let checklist = serde_json::to_value(&b.checklist).map_err(KernelError::internal)?;
    sqlx::query(
        "UPDATE campus.lab SET checklist = $3, readiness_notes = $4, readiness_checked_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(&checklist)
    .bind(notes.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let fresh = load_lab(&mut tx, t, id, true).await?;
    let open: Vec<&str> = b
        .checklist
        .iter()
        .filter(|i| !i.done)
        .map(|i| i.item.as_str())
        .collect();
    let reason = if open.is_empty() {
        notes.clone()
    } else {
        Some(format!("open checklist items: {}", open.join(", ")))
    };
    let status = refresh_lab_status(&mut tx, t, &fresh, reason.as_deref(), true, &a).await?;
    audit(
        &mut tx,
        &scope,
        "campus.lab.readiness",
        "lab",
        id,
        serde_json::json!({"status": status, "openItems": open}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = detail(&st, t, id).await?;
    Ok(respond(StatusCode::OK, &d))
}

/// Register equipment in a lab (`campus.facility.manage`; operational).
#[utoipa::path(post, path = "/api/v1/campus/labs/{id}/equipment", operation_id = "campus_lab_equipment_create", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LabEquipmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LabEquipmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_equipment_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LabEquipmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let code = b.code.trim().to_uppercase();
    let code_ok = {
        let bytes = code.as_bytes();
        (1..=32).contains(&bytes.len())
            && bytes[0].is_ascii_alphanumeric()
            && bytes
                .iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
    };
    if !code_ok {
        return Err(invalid(
            "code",
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_.-]{0,31}$",
        )
        .into());
    }
    let name = required("name", Some(b.name))?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let lab = load_lab(&mut tx, t, id, true).await?;
    let scope = institution_scope(ctx, InstitutionId(lab.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(lab.institution_id)).await?;
    let sql = format!(
        "INSERT INTO campus.lab_equipment (tenant_id, institution_id, lab_id, code, name) \
         VALUES ($1, $2, $3, $4, $5) RETURNING {EQ_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(lab.institution_id)
        .bind(id)
        .bind(&code)
        .bind(&name)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                KernelError::Conflict(format!("equipment {code} already exists in this lab"))
            }
            _ => map_db_error(e),
        })?;
    let eq = eq_from_row(&row).map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.lab_equipment.create",
        "lab_equipment",
        eq.id,
        serde_json::json!({"labId": id, "code": code}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &eq, Version(eq.version)))
}

/// Equipment of a lab (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/labs/{id}/equipment", operation_id = "campus_lab_equipment_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LabEquipmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_equipment_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LabEquipmentList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_lab(&mut conn, t, id, false).await?;
    Ok(Json(LabEquipmentList {
        items: equipment_of(&mut conn, t, id).await?,
    }))
}

/// Change equipment status (`campus.facility.manage`): equipment that is not operational makes
/// a checked lab `restricted`; when all is operational again it returns to `ready`.
#[utoipa::path(post, path = "/api/v1/campus/lab-equipment/{id}/status", operation_id = "campus_lab_equipment_status", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the equipment")),
    request_body = LabEquipmentStatusBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LabEquipmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_equipment_status(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LabEquipmentStatusBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.status != EquipmentStatus::Operational && reason.is_none() {
        return Err(invalid("reason", "required", "say what is wrong with the equipment").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {EQ_COLUMNS} FROM campus.lab_equipment WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    let eq = sqlx::query(&sql)
        .bind(id)
        .bind(t.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(eq_from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "lab equipment",
        })?;
    ensure_version(v, eq.version)?;
    if eq.status == b.status {
        return Err(KernelError::Conflict(format!(
            "equipment {} is already {}",
            eq.code, eq.status
        ))
        .into());
    }
    let lab = load_lab(&mut tx, t, eq.lab_id, true).await?;
    let scope = institution_scope(ctx, InstitutionId(lab.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(lab.institution_id)).await?;
    let sql = format!(
        "UPDATE campus.lab_equipment SET status = $3, status_reason = $4, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {EQ_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(t.as_uuid())
        .bind(b.status.as_str())
        .bind(reason.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let out = eq_from_row(&row).map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution: InstitutionId(lab.institution_id),
            aggregate_type: "lab_equipment",
            aggregate_id: id,
            from: Some(eq.status.as_str()),
            to: out.status.as_str(),
            reason: reason.as_deref(),
            process_instance_id: None,
            cause: "",
        },
        &a,
    )
    .await?;
    let lab_reason = (out.status != EquipmentStatus::Operational)
        .then(|| format!("equipment {} is {}", out.code, out.status));
    let lab_after = if lab.status == LabStatus::NotReady {
        lab.status
    } else {
        refresh_lab_status(&mut tx, t, &lab, lab_reason.as_deref(), false, &a).await?
    };
    audit(
        &mut tx,
        &scope,
        "campus.lab_equipment.status",
        "lab_equipment",
        id,
        serde_json::json!({"from": eq.status, "to": out.status, "reason": reason,
            "labStatus": lab_after}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

// ------------------------------------------------------------------ handlers: sessions

struct SessionSlot {
    date: NaiveDate,
    start: NaiveTime,
    end: NaiveTime,
}

fn parse_session_slot(date: &str, start: &str, end: &str) -> Result<SessionSlot, KernelError> {
    let date = parse_date("sessionDate", date)?;
    let start = parse_time("startTime", start)?;
    let end = parse_time("endTime", end)?;
    if start >= end {
        return Err(invalid(
            "endTime",
            "invalid_window",
            "endTime must be after startTime",
        ));
    }
    Ok(SessionSlot { date, start, end })
}

/// 409 when another live session of the lab overlaps the slot.
async fn session_overlap(
    conn: &mut PgConnection,
    tenant: TenantId,
    lab: Uuid,
    slot: &SessionSlot,
    exclude: Option<Uuid>,
) -> Result<(), KernelError> {
    let others: Vec<(String, NaiveTime, NaiveTime)> = sqlx::query_as(
        "SELECT title, start_time, end_time FROM campus.practical_session WHERE tenant_id = $1 \
         AND lab_id = $2 AND session_date = $3 AND status IN ('planned', 'ready', 'in_progress') \
         AND ($4::uuid IS NULL OR id <> $4)",
    )
    .bind(tenant.as_uuid())
    .bind(lab)
    .bind(slot.date)
    .bind(exclude)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some((title, s, e)) = others
        .iter()
        .find(|(_, s, e)| overlaps(slot.start, slot.end, *s, *e))
    {
        return Err(KernelError::Conflict(format!(
            "the lab already hosts \"{title}\" {}–{}",
            fmt_time(*s),
            fmt_time(*e)
        )));
    }
    Ok(())
}

/// Plan a practical session in a lab (`campus.facility.manage`; no overlap, capacity ≤ lab).
#[utoipa::path(post, path = "/api/v1/campus/labs/{id}/sessions", operation_id = "campus_lab_session_create", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = PracticalSessionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PracticalSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_session_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PracticalSessionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let title = required("title", Some(b.title))?;
    let slot = parse_session_slot(&b.session_date, &b.start_time, &b.end_time)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_lab(&mut conn, t, id, false).await?
    };
    let inst = InstitutionId(probe.institution_id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    if slot.date < business_today(&st, &scope, inst).await? {
        return Err(invalid("sessionDate", "in_the_past", "plan sessions from today on").into());
    }
    let capacity = b.capacity.unwrap_or(probe.capacity);
    if capacity < 1 || capacity > probe.capacity {
        return Err(invalid(
            "capacity",
            "out_of_range",
            "at least 1 and at most the lab capacity",
        )
        .into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    load_lab(&mut tx, t, id, true).await?;
    session_overlap(&mut tx, t, id, &slot, None).await?;
    let sid: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.practical_session (tenant_id, institution_id, lab_id, title, \
         course_code, session_date, start_time, end_time, capacity) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(id)
    .bind(&title)
    .bind(clean(b.course_code))
    .bind(slot.date)
    .bind(slot.start)
    .bind(slot.end)
    .bind(capacity)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution: inst,
            aggregate_type: "practical_session",
            aggregate_id: sid,
            from: None,
            to: PracticalSessionStatus::Planned.as_str(),
            reason: None,
            process_instance_id: None,
            cause: "",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.practical_session.create",
        "practical_session",
        sid,
        serde_json::json!({"labId": id, "title": title, "date": slot.date}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (s, _) = load_session(&mut conn, t, sid, false).await?;
    Ok(with_etag(StatusCode::CREATED, &s, Version(s.version)))
}

/// Sessions of a lab (`campus.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/campus/labs/{id}/sessions", operation_id = "campus_lab_session_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), LabQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PracticalSessionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_session_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LabQuery>,
) -> ApiResult<Json<PracticalSessionList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let status = parse_filter("status", q.status.as_deref(), PracticalSessionStatus::parse)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_lab(&mut conn, t, id, false).await?;
    Ok(Json(PracticalSessionList {
        items: sessions_of(&mut conn, t, id, status).await?,
    }))
}

/// Moves a locked session (history; `practical_session.completed`).
async fn move_session(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    s: &PracticalSessionView,
    to: PracticalSessionStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !s.status.can_transition(to) {
        return Err(illegal(
            &format!("practical session \"{}\"", s.title),
            s.status.as_str(),
            to.as_str(),
        ));
    }
    let v: i64 = sqlx::query_scalar(
        "UPDATE campus.practical_session SET status = $3, status_reason = $4, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING version",
    )
    .bind(s.id)
    .bind(scope.tenant_id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let completed = to == PracticalSessionStatus::Completed;
    let t = Transition {
        tenant: scope.tenant_id,
        institution: scope.institution_id.unwrap_or(InstitutionId(Uuid::nil())),
        aggregate_type: "practical_session",
        aggregate_id: s.id,
        from: Some(s.status.as_str()),
        to: to.as_str(),
        reason,
        process_instance_id: None,
        cause: if completed {
            PRACTICAL_SESSION_COMPLETED
        } else {
            ""
        },
    };
    history(conn, &t, actor).await?;
    if completed {
        emit_status(
            conn,
            scope,
            (
                PRACTICAL_SESSION_COMPLETED,
                PRACTICAL_SESSION_COMPLETED_SCHEMA,
            ),
            &t,
            v,
            &EventExtras::default(),
            actor,
        )
        .await?;
    }
    Ok(())
}

/// One session command: lock, version, writable, `f`, audit, commit; returns the session.
async fn session_command(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
    to: PracticalSessionStatus,
    reason: Option<String>,
    reslot: Option<SessionSlot>,
) -> Result<PracticalSessionView, KernelError> {
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (s, inst) = load_session(&mut tx, t, id, true).await?;
    ensure_version(expected_version, s.version)?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    if matches!(
        to,
        PracticalSessionStatus::Ready | PracticalSessionStatus::InProgress
    ) {
        let lab = load_lab(&mut tx, t, s.lab_id, true).await?;
        if lab.status != LabStatus::Ready {
            return Err(KernelError::Conflict(format!(
                "lab {} is {}; a session needs a ready lab",
                lab.name, lab.status
            )));
        }
    }
    if let Some(slot) = reslot.as_ref() {
        session_overlap(&mut tx, t, s.lab_id, slot, Some(s.id)).await?;
        sqlx::query(
            "UPDATE campus.practical_session SET session_date = $3, start_time = $4, \
             end_time = $5 WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(t.as_uuid())
        .bind(slot.date)
        .bind(slot.start)
        .bind(slot.end)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    move_session(&mut tx, &scope, &s, to, reason.as_deref(), &a).await?;
    audit(
        &mut tx,
        &scope,
        &format!("campus.practical_session.{}", to.as_str()),
        "practical_session",
        id,
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(load_session(&mut conn, t, id, false).await?.0)
}

fn session_response(s: &PracticalSessionView) -> Response {
    with_etag(StatusCode::OK, s, Version(s.version))
}

/// Mark a planned / postponed session ready (`campus.facility.manage`; the lab must be ready).
#[utoipa::path(post, path = "/api/v1/campus/practical-sessions/{id}/ready", operation_id = "campus_lab_session_ready", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PracticalSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_ready(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let s = session_command(
        &st,
        ctx,
        id,
        v,
        PracticalSessionStatus::Ready,
        clean(b.reason),
        None,
    )
    .await?;
    Ok(session_response(&s))
}

/// Start a ready session (`campus.facility.manage`; the lab must still be ready).
#[utoipa::path(post, path = "/api/v1/campus/practical-sessions/{id}/start", operation_id = "campus_lab_session_start", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PracticalSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_start(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let s = session_command(
        &st,
        ctx,
        id,
        v,
        PracticalSessionStatus::InProgress,
        clean(b.reason),
        None,
    )
    .await?;
    Ok(session_response(&s))
}

/// Complete a session in progress (`campus.facility.manage`; `practical_session.completed`).
#[utoipa::path(post, path = "/api/v1/campus/practical-sessions/{id}/complete", operation_id = "campus_lab_session_complete", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PracticalSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_complete(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let s = session_command(
        &st,
        ctx,
        id,
        v,
        PracticalSessionStatus::Completed,
        clean(b.reason),
        None,
    )
    .await?;
    Ok(session_response(&s))
}

/// Postpone a session, optionally to a new slot (`campus.facility.manage`; reason required).
#[utoipa::path(post, path = "/api/v1/campus/practical-sessions/{id}/postpone", operation_id = "campus_lab_session_postpone", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = PracticalSessionPostpone, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PracticalSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_postpone(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PracticalSessionPostpone>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let reslot = match (b.session_date, b.start_time, b.end_time) {
        (Some(d), Some(s), Some(e)) => Some(parse_session_slot(&d, &s, &e)?),
        (None, None, None) => None,
        _ => {
            return Err(invalid(
                "sessionDate",
                "incomplete_slot",
                "give sessionDate, startTime and endTime together",
            )
            .into())
        }
    };
    let s = session_command(
        &st,
        ctx,
        id,
        v,
        PracticalSessionStatus::Postponed,
        Some(reason),
        reslot,
    )
    .await?;
    Ok(session_response(&s))
}

/// Cancel a session that has not started (`campus.facility.manage`; reason required).
#[utoipa::path(post, path = "/api/v1/campus/practical-sessions/{id}/cancel", operation_id = "campus_lab_session_cancel", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PracticalSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_cancel(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let s = session_command(
        &st,
        ctx,
        id,
        v,
        PracticalSessionStatus::Cancelled,
        Some(reason),
        None,
    )
    .await?;
    Ok(session_response(&s))
}

// ------------------------------------------------------------------ handlers: batch allocation

async fn allocations_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    lab: Uuid,
    session: Option<Uuid>,
) -> Result<Vec<LabBatchAllocationView>, KernelError> {
    let rows = sqlx::query(
        "SELECT a.id, a.session_id, a.person_kind, a.person_id, a.batch_label, a.created_at \
         FROM campus.lab_batch_allocation a JOIN campus.practical_session s ON s.id = a.session_id \
         WHERE a.tenant_id = $1 AND s.lab_id = $2 AND ($3::uuid IS NULL OR a.session_id = $3) \
         ORDER BY a.created_at, a.id",
    )
    .bind(tenant.as_uuid())
    .bind(lab)
    .bind(session)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| -> Result<LabBatchAllocationView, sqlx::Error> {
            Ok(LabBatchAllocationView {
                id: r.try_get("id")?,
                session_id: r.try_get("session_id")?,
                person_kind: decode(r.try_get("person_kind")?, PersonKind::parse)?,
                person_id: r.try_get("person_id")?,
                batch_label: r.try_get("batch_label")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// Allocate a batch of students to a session of this lab (`campus.facility.manage`;
/// `Idempotency-Key`): students already allocated are skipped; 409 when the session would
/// exceed its capacity; unknown students are 422.
#[utoipa::path(post, path = "/api/v1/campus/labs/{id}/batch-allocations", operation_id = "campus_lab_batch_allocate", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LabBatchAllocationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LabBatchAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_batch_allocate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LabBatchAllocationCreate>,
) -> ApiResult<(StatusCode, Json<LabBatchAllocationList>)> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    if b.student_ids.is_empty() || b.student_ids.len() > 500 {
        return Err(invalid("studentIds", "invalid", "1–500 students").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let (session, inst) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_session(&mut conn, t, b.session_id, false).await?
    };
    if session.lab_id != id {
        return Err(invalid(
            "sessionId",
            "other_lab",
            "the session belongs to another lab",
        )
        .into());
    }
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut ids = b.student_ids.clone();
    ids.sort_unstable();
    ids.dedup();
    for sid in &ids {
        require_person(&st, &scope, PersonKind::Student, *sid, "studentIds").await?;
    }
    let label = clean(b.batch_label);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (s, _) = load_session(&mut tx, t, b.session_id, true).await?;
    if !s.status.accepts_allocations() {
        return Err(KernelError::Conflict(format!(
            "practical session \"{}\" is {}; it takes no more students",
            s.title, s.status
        ))
        .into());
    }
    let already: Vec<Uuid> = sqlx::query_scalar(
        "SELECT person_id FROM campus.lab_batch_allocation WHERE tenant_id = $1 \
         AND session_id = $2 AND person_kind = 'student'",
    )
    .bind(t.as_uuid())
    .bind(b.session_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let fresh: Vec<Uuid> = ids.into_iter().filter(|x| !already.contains(x)).collect();
    let total = s.allocated + i64::try_from(fresh.len()).unwrap_or(i64::MAX);
    if total > i64::from(s.capacity) {
        return Err(KernelError::Conflict(format!(
            "practical session \"{}\" seats {}; {} allocated, {} more do not fit",
            s.title,
            s.capacity,
            s.allocated,
            fresh.len()
        ))
        .into());
    }
    for sid in &fresh {
        sqlx::query(
            "INSERT INTO campus.lab_batch_allocation (tenant_id, institution_id, session_id, \
             person_kind, person_id, batch_label, created_by) \
             VALUES ($1, $2, $3, 'student', $4, $5, $6) ON CONFLICT DO NOTHING",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(b.session_id)
        .bind(sid)
        .bind(label.as_deref())
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    sqlx::query(
        "UPDATE campus.practical_session SET version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(b.session_id)
    .bind(t.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.practical_session.allocate",
        "practical_session",
        b.session_id,
        serde_json::json!({"allocated": fresh.len(), "batchLabel": label}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = allocations_of(&mut conn, t, id, Some(b.session_id)).await?;
    Ok((StatusCode::CREATED, Json(LabBatchAllocationList { items })))
}

/// Allocations of a lab's sessions (`campus.read`; `?sessionId=`).
#[utoipa::path(get, path = "/api/v1/campus/labs/{id}/batch-allocations", operation_id = "campus_lab_batch_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), LabQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LabBatchAllocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn lab_batch_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LabQuery>,
) -> ApiResult<Json<LabBatchAllocationList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_lab(&mut conn, t, id, false).await?;
    Ok(Json(LabBatchAllocationList {
        items: allocations_of(&mut conn, t, id, q.session_id).await?,
    }))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(lab_create, lab_list))
        .routes(routes!(lab_get))
        .routes(routes!(lab_readiness))
        .routes(routes!(lab_equipment_create, lab_equipment_list))
        .routes(routes!(lab_equipment_status))
        .routes(routes!(lab_session_create, lab_session_list))
        .routes(routes!(session_ready))
        .routes(routes!(session_start))
        .routes(routes!(session_complete))
        .routes(routes!(session_postpone))
        .routes(routes!(session_cancel))
        .routes(routes!(lab_batch_allocate, lab_batch_list))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_slots() {
        assert!(parse_session_slot("2026-12-02", "14:00", "16:00").is_ok());
        assert!(parse_session_slot("2026-12-02", "16:00", "14:00").is_err());
    }
}
