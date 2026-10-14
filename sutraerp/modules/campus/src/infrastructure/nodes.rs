//! Infrastructure nodes (L2-G10; L3-G-278, 281…290, 315): registration through
//! `shared::infrastructure::insert_node` (campus validated through Domain B), attributes /
//! classification / capacity changes (`infrastructure_node.modified`), capabilities, availability
//! profiles, the free / busy calendar of a space, and the state commands activate / suspend
//! (open reservations flagged) / restore / decommission (`NODE_DECOMMISSION`, `campus.approve`) /
//! archive — status only through `shared::infrastructure::change_node_status`.

use super::common::{
    audit, clean, emit_modified, ensure_version, expected, fmt_time, invalid, parse_date,
    parse_filter, parse_time, required, FacilityDecisionBody, FacilityReasonBody,
};
use super::domain::{free_windows, open_windows, Availability, ReservationStatus, WeeklyWindow};
use crate::directory::http::InfrastructureNodeView;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::parse_decimal;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::infrastructure::{change_node_status, insert_node, NewNode};
use crate::shared::lifecycle::StatusChange;
use crate::shared::publish;
use crate::shared::rows::{load_node, lock_node, node_from_row, InfrastructureNode, NODE_COLUMNS};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{Duration, NaiveDate, NaiveTime};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::campus::events::facilities::{
    INFRASTRUCTURE_NODE_ACTIVATED, INFRASTRUCTURE_NODE_ACTIVATED_SCHEMA,
    INFRASTRUCTURE_NODE_DECOMMISSIONED, INFRASTRUCTURE_NODE_DECOMMISSIONED_SCHEMA,
    INFRASTRUCTURE_NODE_MODIFIED, INFRASTRUCTURE_NODE_MODIFIED_SCHEMA,
    INFRASTRUCTURE_NODE_RESTORED, INFRASTRUCTURE_NODE_RESTORED_SCHEMA,
    INFRASTRUCTURE_NODE_SUSPENDED, INFRASTRUCTURE_NODE_SUSPENDED_SCHEMA,
};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{InfrastructureNodeType, InfrastructureStatus, SpaceType};
use sutra_contracts::institution::status::CampusStatus;
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

// ------------------------------------------------------------------ views

/// A capability of a space (projector, AC, wheelchair access, fume hood …).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceCapabilityView {
    /// `snake_case` code (`projector`, `accessible`, `lab_safety` …).
    #[schema(example = "projector")]
    pub code: String,
    pub label: String,
    pub quantity: Option<i32>,
}

/// One weekly opening window (ISO weekday 1 = Monday … 7 = Sunday, business-local `HH:MM`).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyHoursView {
    pub weekday: u32,
    #[schema(example = "08:00")]
    pub opens_at: String,
    #[schema(example = "18:00")]
    pub closes_at: String,
}

/// The availability profile of a node.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvailabilityProfileView {
    /// `false` = no profile set (open all day, every day).
    pub configured: bool,
    pub bookable: bool,
    pub weekly_hours: Vec<WeeklyHoursView>,
    pub blackout_dates: Vec<NaiveDate>,
    pub notes: Option<String>,
}

/// A decommissioning request (`NODE_DECOMMISSION`).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecommissionRequestView {
    pub id: Uuid,
    /// `pending | approved | rejected`.
    pub status: String,
    pub reason: String,
    pub process_instance_id: Option<String>,
    pub decision_reason: Option<String>,
}

/// One node with its children, capabilities, availability profile and pending decommissioning.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureNodeDetail {
    pub node: InfrastructureNodeView,
    pub children: Vec<InfrastructureNodeView>,
    pub capabilities: Vec<SpaceCapabilityView>,
    pub availability: Option<AvailabilityProfileView>,
    pub decommission: Option<DecommissionRequestView>,
    /// Requested / confirmed reservations from today on.
    pub future_reservations: i64,
    pub version: i64,
}

/// Nodes of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InfrastructureNodeList {
    pub items: Vec<InfrastructureNodeView>,
}

/// A time window `HH:MM`–`HH:MM`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceTimeWindow {
    pub start_time: String,
    pub end_time: String,
}

/// A reservation occupying a slot.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceBusySlot {
    pub reservation_id: Uuid,
    pub number: String,
    pub start_time: String,
    pub end_time: String,
    pub status: ReservationStatus,
    pub purpose: String,
}

/// One day of a space calendar.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceCalendarDay {
    pub date: NaiveDate,
    pub open: Vec<SpaceTimeWindow>,
    pub busy: Vec<SpaceBusySlot>,
    pub free: Vec<SpaceTimeWindow>,
}

/// Free / busy calendar of a space (L3-G-315).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceCalendar {
    pub node_id: InfrastructureNodeId,
    pub code: String,
    pub status: InfrastructureStatus,
    pub capacity: Option<i32>,
    pub days: Vec<SpaceCalendarDay>,
}

// ------------------------------------------------------------------ bodies

/// Register a node.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureNodeCreate {
    /// Domain B campus; defaults to the parent's campus.
    pub campus_id: Option<Uuid>,
    /// Required for everything but a site.
    pub parent_id: Option<Uuid>,
    pub node_type: InfrastructureNodeType,
    #[schema(example = "MAIN-B1-101")]
    pub code: String,
    pub name: String,
    /// Spaces only.
    pub space_type: Option<SpaceType>,
    pub capacity: Option<i32>,
    pub floor_number: Option<i32>,
    #[schema(example = "64.50")]
    pub area_sq_m: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub attributes: Option<serde_json::Value>,
    /// `planned` (default) or `active`.
    pub status: Option<InfrastructureStatus>,
    #[schema(example = "2026-11-01")]
    pub effective_from: Option<String>,
}

/// Change attributes / classification (absent members are kept).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureNodeUpdate {
    pub name: Option<String>,
    /// Classification (spaces only).
    pub space_type: Option<SpaceType>,
    pub floor_number: Option<i32>,
    pub area_sq_m: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub attributes: Option<serde_json::Value>,
    pub effective_from: Option<String>,
}

/// Change the capacity (seats / beds / persons).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureCapacityBody {
    pub capacity: i32,
    pub reason: Option<String>,
}

/// Replace the capabilities of a space.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SpaceCapabilitiesBody {
    pub capabilities: Vec<SpaceCapabilityView>,
}

/// Replace the availability profile of a node.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvailabilityProfileBody {
    /// Default `true`.
    pub bookable: Option<bool>,
    /// Empty = open all day.
    #[serde(default)]
    pub weekly_hours: Vec<WeeklyHoursView>,
    /// `YYYY-MM-DD`.
    #[serde(default)]
    pub blackout_dates: Vec<String>,
    pub notes: Option<String>,
}

/// Filters of the node list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureNodeQuery {
    pub parent_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub node_type: Option<String>,
    pub status: Option<String>,
}

/// Window of the space calendar (default: today + 6 days; at most 31 days).
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct SpaceCalendarQuery {
    pub from: Option<String>,
    pub to: Option<String>,
}

// ------------------------------------------------------------------ reads

/// The availability profile of a node (`None` = none set: open all day).
pub async fn load_availability(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: InfrastructureNodeId,
) -> Result<Option<(Availability, Option<String>)>, KernelError> {
    let row = sqlx::query(
        "SELECT bookable, weekly_hours, blackout_dates, notes FROM campus.availability_profile \
         WHERE tenant_id = $1 AND node_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(node.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some(r) = row else {
        return Ok(None);
    };
    let weekly_raw: serde_json::Value = r.try_get("weekly_hours").map_err(map_db_error)?;
    let views: Vec<WeeklyHoursView> = serde_json::from_value(weekly_raw).unwrap_or_default();
    let mut weekly = Vec::with_capacity(views.len());
    for v in &views {
        if let (Ok(o), Ok(c)) = (
            NaiveTime::parse_from_str(&v.opens_at, "%H:%M"),
            NaiveTime::parse_from_str(&v.closes_at, "%H:%M"),
        ) {
            weekly.push(WeeklyWindow {
                weekday: v.weekday,
                opens: o,
                closes: c,
            });
        }
    }
    Ok(Some((
        Availability {
            bookable: r.try_get("bookable").map_err(map_db_error)?,
            weekly,
            blackout: r.try_get("blackout_dates").map_err(map_db_error)?,
        },
        r.try_get("notes").map_err(map_db_error)?,
    )))
}

fn availability_view(a: &Availability, notes: Option<String>) -> AvailabilityProfileView {
    AvailabilityProfileView {
        configured: true,
        bookable: a.bookable,
        weekly_hours: a
            .weekly
            .iter()
            .map(|w| WeeklyHoursView {
                weekday: w.weekday,
                opens_at: fmt_time(w.opens),
                closes_at: fmt_time(w.closes),
            })
            .collect(),
        blackout_dates: a.blackout.clone(),
        notes,
    }
}

/// Reservations of a node that still hold a slot on or after `from`.
pub async fn future_reservations(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: InfrastructureNodeId,
    from: NaiveDate,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM campus.space_reservation WHERE tenant_id = $1 AND node_id = $2 \
         AND reservation_date >= $3 AND status IN ('requested', 'confirmed', 'rescheduled')",
    )
    .bind(tenant.as_uuid())
    .bind(node.as_uuid())
    .bind(from)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn require_node(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<InfrastructureNode, KernelError> {
    load_node(conn, tenant, InfrastructureNodeId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "infrastructure node",
        })
}

async fn node_detail(
    st: &CampusState,
    scope_tenant: TenantId,
    id: Uuid,
) -> Result<InfrastructureNodeDetail, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let node = require_node(&mut conn, scope_tenant, id).await?;
    let scope = ExecutionScope::institution(scope_tenant, node.institution_id);
    let today = business_today(st, &scope, node.institution_id).await?;
    let sql = format!(
        "SELECT {NODE_COLUMNS} FROM campus.infrastructure_node WHERE tenant_id = $1 \
         AND parent_id = $2 ORDER BY code"
    );
    let rows = sqlx::query(&sql)
        .bind(scope_tenant.as_uuid())
        .bind(id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut children = Vec::with_capacity(rows.len());
    for r in &rows {
        children.push(InfrastructureNodeView::from(
            &node_from_row(r).map_err(map_db_error)?,
        ));
    }
    let caps = sqlx::query(
        "SELECT code, label, quantity FROM campus.space_capability WHERE tenant_id = $1 \
         AND node_id = $2 ORDER BY code",
    )
    .bind(scope_tenant.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut capabilities = Vec::with_capacity(caps.len());
    for r in &caps {
        capabilities.push(SpaceCapabilityView {
            code: r.try_get("code").map_err(map_db_error)?,
            label: r.try_get("label").map_err(map_db_error)?,
            quantity: r.try_get("quantity").map_err(map_db_error)?,
        });
    }
    let availability = load_availability(&mut conn, scope_tenant, node.id)
        .await?
        .map(|(a, notes)| availability_view(&a, notes));
    let decommission = sqlx::query(
        "SELECT id, status, reason, process_instance_id, decision_reason \
         FROM campus.infrastructure_decommission WHERE tenant_id = $1 AND node_id = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(scope_tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(|r| -> Result<DecommissionRequestView, sqlx::Error> {
        Ok(DecommissionRequestView {
            id: r.try_get("id")?,
            status: r.try_get("status")?,
            reason: r.try_get("reason")?,
            process_instance_id: r.try_get("process_instance_id")?,
            decision_reason: r.try_get("decision_reason")?,
        })
    })
    .transpose()
    .map_err(map_db_error)?;
    let future = future_reservations(&mut conn, scope_tenant, node.id, today).await?;
    Ok(InfrastructureNodeDetail {
        version: node.version,
        node: InfrastructureNodeView::from(&node),
        children,
        capabilities,
        availability,
        decommission,
        future_reservations: future,
    })
}

fn detail_response(status: StatusCode, d: &InfrastructureNodeDetail) -> Response {
    with_etag(status, d, Version(d.version))
}

/// Bumps the node version after an 8A attribute change (409 on a concurrent change).
async fn bump_node(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: &InfrastructureNode,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "UPDATE campus.infrastructure_node SET version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $3 RETURNING version",
    )
    .bind(node.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(node.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("infrastructure node changed concurrently".into()))
}

fn editable(node: &InfrastructureNode) -> Result<(), KernelError> {
    if matches!(
        node.status,
        InfrastructureStatus::Decommissioned | InfrastructureStatus::Archived
    ) {
        return Err(KernelError::Conflict(format!(
            "infrastructure node {} is {}; it can no longer change",
            node.code, node.status
        )));
    }
    Ok(())
}

/// Moves a locked node and emits the matching event (if any).
#[allow(clippy::too_many_arguments)]
async fn move_node(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    node: &InfrastructureNode,
    to: InfrastructureStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    actor: &Actor,
) -> Result<InfrastructureNode, KernelError> {
    let moved = change_node_status(
        conn,
        scope.tenant_id,
        node,
        to,
        &StatusChange {
            cause_event_type: event.map(|e| e.0).unwrap_or(""),
            reason,
            process_instance_id: pid,
        },
        actor,
    )
    .await?;
    if let Some((ty, schema)) = event {
        publish::event(
            conn,
            scope,
            ty,
            schema,
            node.id.to_string(),
            &StatusChangedV1 {
                aggregate_id: node.id.as_uuid(),
                institution_id: node.institution_id.as_uuid(),
                from_status: Some(node.status.as_str().to_owned()),
                to_status: to.as_str().to_owned(),
                version: moved.version,
                reason: reason.map(str::to_owned),
                process_instance_id: pid.map(str::to_owned),
                number: Some(node.code.clone()),
                subject_ref: None,
            },
            actor,
        )
        .await?;
    }
    Ok(moved)
}

// ------------------------------------------------------------------ handlers: create / read / edit

/// Register a node under its parent (`campus.facility.manage`): the campus is validated through
/// Domain B, the hierarchy rule and unique code through `insert_node`; starts planned or active.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/infrastructure-nodes", operation_id = "campus_infra_node_create", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = InfrastructureNodeCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<InfrastructureNodeCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let area = b
        .area_sq_m
        .as_deref()
        .map(|r| parse_decimal("areaSqM", r))
        .transpose()?;
    if area.is_some_and(|v| v < Decimal::ZERO) {
        return Err(invalid("areaSqM", "negative", "area cannot be negative").into());
    }
    let effective_from = b
        .effective_from
        .as_deref()
        .map(|r| parse_date("effectiveFrom", r))
        .transpose()?;
    let status = b.status.unwrap_or(InfrastructureStatus::Planned);
    let attributes = b.attributes.unwrap_or_else(|| serde_json::json!({}));
    if !attributes.is_object() {
        return Err(invalid("attributes", "invalid", "must be a JSON object").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let parent = match b.parent_id {
        None => None,
        Some(pid) => Some(
            load_node(&mut tx, t, InfrastructureNodeId(pid))
                .await?
                .ok_or_else(|| {
                    invalid(
                        "parentId",
                        "unknown_parent",
                        "no infrastructure node with this id",
                    )
                })?,
        ),
    };
    let campus_id = match (b.campus_id, parent.as_ref()) {
        (Some(c), _) => c,
        (None, Some(p)) => p.campus_id,
        (None, None) => {
            return Err(invalid("campusId", "required", "a site needs its campus").into())
        }
    };
    let campus = st
        .references
        .campus(&scope, campus_id)
        .await?
        .filter(|c| c.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                "campusId",
                "unknown_campus",
                "no campus of this institution",
            )
        })?;
    if matches!(
        campus.status,
        CampusStatus::Decommissioned | CampusStatus::Archived
    ) {
        return Err(invalid(
            "campusId",
            "campus_closed",
            "the campus is decommissioned or archived",
        )
        .into());
    }
    let name = b.name.trim().to_owned();
    let code = b.code.trim().to_owned();
    let node = insert_node(
        &mut tx,
        NewNode {
            tenant: t,
            institution,
            campus_id,
            parent: parent.as_ref(),
            node_type: b.node_type,
            code: &code,
            name: &name,
            space_type: b.space_type,
            capacity: b.capacity,
            floor_number: b.floor_number,
            area_sq_m: area,
            attributes,
            status,
            effective_from,
        },
        &a,
    )
    .await?;
    if node.status == InfrastructureStatus::Active {
        publish::event(
            &mut tx,
            &scope,
            INFRASTRUCTURE_NODE_ACTIVATED,
            INFRASTRUCTURE_NODE_ACTIVATED_SCHEMA,
            node.id.to_string(),
            &StatusChangedV1 {
                aggregate_id: node.id.as_uuid(),
                institution_id: id,
                from_status: None,
                to_status: node.status.as_str().to_owned(),
                version: node.version,
                reason: None,
                process_instance_id: None,
                number: Some(node.code.clone()),
                subject_ref: None,
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.create",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"code": node.code, "nodeType": node.node_type.as_str(),
            "status": node.status.as_str()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, node.id.as_uuid()).await?;
    Ok(detail_response(StatusCode::CREATED, &d))
}

/// Nodes of an institution, filtered (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/infrastructure-nodes", operation_id = "campus_infra_node_list", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), InfrastructureNodeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = InfrastructureNodeList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<InfrastructureNodeQuery>,
) -> ApiResult<Json<InfrastructureNodeList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let node_type = parse_filter(
        "nodeType",
        q.node_type.as_deref(),
        InfrastructureNodeType::parse,
    )?;
    let status = parse_filter("status", q.status.as_deref(), InfrastructureStatus::parse)?;
    let sql = format!(
        "SELECT {NODE_COLUMNS} FROM campus.infrastructure_node WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::uuid IS NULL OR parent_id = $3) \
         AND ($4::uuid IS NULL OR campus_id = $4) AND ($5::text IS NULL OR node_type = $5) \
         AND ($6::text IS NULL OR status = $6) ORDER BY code LIMIT 2000"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(q.parent_id)
        .bind(q.campus_id)
        .bind(node_type.map(|x| x.as_str()))
        .bind(status.map(|x| x.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(InfrastructureNodeView::from(
            &node_from_row(r).map_err(map_db_error)?,
        ));
    }
    Ok(Json(InfrastructureNodeList { items }))
}

/// One node with children, capabilities, availability and pending decommissioning (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/infrastructure-nodes/{id}", operation_id = "campus_infra_node_get", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    let d = node_detail(&st, ctx.scope.tenant_id, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Change name / classification / floor / area / attributes (`campus.facility.manage`; 409 once
/// decommissioned or archived; `infrastructure_node.modified`).
#[utoipa::path(put, path = "/api/v1/campus/infrastructure-nodes/{id}", operation_id = "campus_infra_node_update", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = InfrastructureNodeUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_update(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<InfrastructureNodeUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let area = b
        .area_sq_m
        .as_deref()
        .map(|r| parse_decimal("areaSqM", r))
        .transpose()?;
    if area.is_some_and(|x| x < Decimal::ZERO) {
        return Err(invalid("areaSqM", "negative", "area cannot be negative").into());
    }
    let effective_from = b
        .effective_from
        .as_deref()
        .map(|r| parse_date("effectiveFrom", r))
        .transpose()?;
    let name = clean(b.name.clone());
    if b.name.is_some() && name.as_ref().is_none_or(|n| n.len() > 200) {
        return Err(invalid("name", "invalid", "1–200 characters").into());
    }
    if b.attributes.as_ref().is_some_and(|x| !x.is_object()) {
        return Err(invalid("attributes", "invalid", "must be a JSON object").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let scope = institution_scope(ctx, node.institution_id);
    require_institution_writable(&st, &scope, node.institution_id).await?;
    editable(&node)?;
    if b.space_type.is_some() && node.node_type != InfrastructureNodeType::Space {
        return Err(invalid(
            "spaceType",
            "space_type_mismatch",
            "only spaces carry a space type",
        )
        .into());
    }
    let mut changed = Vec::new();
    if name.is_some() {
        changed.push("name".to_owned());
    }
    if b.space_type.is_some() {
        changed.push("spaceType".to_owned());
    }
    if b.floor_number.is_some() {
        changed.push("floorNumber".to_owned());
    }
    if area.is_some() {
        changed.push("areaSqM".to_owned());
    }
    if b.attributes.is_some() {
        changed.push("attributes".to_owned());
    }
    if effective_from.is_some() {
        changed.push("effectiveFrom".to_owned());
    }
    if changed.is_empty() {
        return Err(invalid("body", "nothing_to_change", "no member to change").into());
    }
    let new_version: i64 = sqlx::query_scalar(
        "UPDATE campus.infrastructure_node SET name = COALESCE($3, name), \
         space_type = COALESCE($4, space_type), floor_number = COALESCE($5, floor_number), \
         area_sq_m = COALESCE($6, area_sq_m), attributes = COALESCE($7, attributes), \
         effective_from = COALESCE($8, effective_from), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $9 RETURNING version",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(name.as_deref())
    .bind(b.space_type.map(|s| s.as_str()))
    .bind(b.floor_number)
    .bind(area)
    .bind(b.attributes.clone())
    .bind(effective_from)
    .bind(node.version)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("infrastructure node changed concurrently".into()))?;
    emit_modified(
        &mut tx,
        &scope,
        (
            INFRASTRUCTURE_NODE_MODIFIED,
            INFRASTRUCTURE_NODE_MODIFIED_SCHEMA,
        ),
        node.id.as_uuid(),
        new_version,
        changed.clone(),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.update",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"changedFields": changed}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Change the capacity (`campus.facility.manage`): refused (409) below the attendees of a
/// requested / confirmed reservation from today on.
#[utoipa::path(put, path = "/api/v1/campus/infrastructure-nodes/{id}/capacity", operation_id = "campus_infra_node_capacity", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = InfrastructureCapacityBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_capacity(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<InfrastructureCapacityBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    if b.capacity < 0 {
        return Err(invalid("capacity", "negative", "capacity cannot be negative").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        require_node(&mut conn, t, id).await?
    };
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    let today = business_today(&st, &scope, probe.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    editable(&node)?;
    let biggest: Option<i32> = sqlx::query_scalar(
        "SELECT max(attendees) FROM campus.space_reservation WHERE tenant_id = $1 AND node_id = $2 \
         AND reservation_date >= $3 AND status IN ('requested', 'confirmed', 'rescheduled')",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(today)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(m) = biggest.filter(|m| *m > b.capacity) {
        return Err(KernelError::Conflict(format!(
            "a reservation from today on expects {m} attendees; capacity cannot drop to {}",
            b.capacity
        ))
        .into());
    }
    // Integration (8A × 8B): a hostel room cannot drop below the beds it keeps in service
    // (available or occupied; take a bed out of service first).
    let beds_in_service: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.hostel_bed WHERE tenant_id = $1 AND room_node_id = $2 \
         AND status <> 'out_of_service'",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if beds_in_service > i64::from(b.capacity) {
        return Err(KernelError::Conflict(format!(
            "the room keeps {beds_in_service} beds in service; capacity cannot drop to {}",
            b.capacity
        ))
        .into());
    }
    let new_version: i64 = sqlx::query_scalar(
        "UPDATE campus.infrastructure_node SET capacity = $3, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $4 RETURNING version",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(b.capacity)
    .bind(node.version)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("infrastructure node changed concurrently".into()))?;
    emit_modified(
        &mut tx,
        &scope,
        (
            INFRASTRUCTURE_NODE_MODIFIED,
            INFRASTRUCTURE_NODE_MODIFIED_SCHEMA,
        ),
        node.id.as_uuid(),
        new_version,
        vec!["capacity".to_owned()],
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.capacity",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"from": node.capacity, "to": b.capacity, "reason": clean(b.reason)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Replace the capabilities of a space (`campus.facility.manage`; spaces only).
#[utoipa::path(put, path = "/api/v1/campus/infrastructure-nodes/{id}/capabilities", operation_id = "campus_infra_node_capabilities", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = SpaceCapabilitiesBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_capabilities(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SpaceCapabilitiesBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut seen = std::collections::HashSet::new();
    for c in &b.capabilities {
        let ok = {
            let bytes = c.code.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 48
                && bytes[0].is_ascii_lowercase()
                && bytes
                    .iter()
                    .all(|x| x.is_ascii_lowercase() || x.is_ascii_digit() || *x == b'_')
        };
        if !ok {
            return Err(invalid(
                "capabilities",
                "invalid_code",
                "codes match ^[a-z][a-z0-9_]{0,47}$",
            )
            .into());
        }
        if !seen.insert(c.code.clone()) {
            return Err(invalid("capabilities", "duplicate", "a code appears twice").into());
        }
        if c.label.trim().is_empty() || c.label.len() > 120 || c.quantity.is_some_and(|q| q < 0) {
            return Err(invalid(
                "capabilities",
                "invalid",
                "labels 1–120 characters, quantities ≥ 0",
            )
            .into());
        }
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let scope = institution_scope(ctx, node.institution_id);
    require_institution_writable(&st, &scope, node.institution_id).await?;
    editable(&node)?;
    if node.node_type != InfrastructureNodeType::Space {
        return Err(invalid(
            "capabilities",
            "not_a_space",
            "capabilities belong to spaces",
        )
        .into());
    }
    sqlx::query("DELETE FROM campus.space_capability WHERE tenant_id = $1 AND node_id = $2")
        .bind(t.as_uuid())
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    for c in &b.capabilities {
        sqlx::query(
            "INSERT INTO campus.space_capability (tenant_id, institution_id, node_id, code, \
             label, quantity) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(t.as_uuid())
        .bind(node.institution_id.as_uuid())
        .bind(id)
        .bind(&c.code)
        .bind(c.label.trim())
        .bind(c.quantity)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let new_version = bump_node(&mut tx, t, &node).await?;
    emit_modified(
        &mut tx,
        &scope,
        (
            INFRASTRUCTURE_NODE_MODIFIED,
            INFRASTRUCTURE_NODE_MODIFIED_SCHEMA,
        ),
        node.id.as_uuid(),
        new_version,
        vec!["capabilities".to_owned()],
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.capabilities",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"codes": seen.into_iter().collect::<Vec<_>>()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// The availability profile of a node (`campus.read`; `configured = false`: none set, open all day).
#[utoipa::path(get, path = "/api/v1/campus/infrastructure-nodes/{id}/availability", operation_id = "campus_infra_node_availability_get", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AvailabilityProfileView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_availability_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AvailabilityProfileView>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let node = require_node(&mut conn, t, id).await?;
    Ok(Json(
        load_availability(&mut conn, t, node.id)
            .await?
            .map(|(a, n)| availability_view(&a, n))
            .unwrap_or(AvailabilityProfileView {
                configured: false,
                bookable: true,
                weekly_hours: Vec::new(),
                blackout_dates: Vec::new(),
                notes: None,
            }),
    ))
}

/// Replace the availability profile (weekly open hours + blackout dates; `campus.facility.manage`).
#[utoipa::path(put, path = "/api/v1/campus/infrastructure-nodes/{id}/availability", operation_id = "campus_infra_node_availability_set", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = AvailabilityProfileBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_availability_set(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AvailabilityProfileBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut weekly = Vec::with_capacity(b.weekly_hours.len());
    for w in &b.weekly_hours {
        if !(1..=7).contains(&w.weekday) {
            return Err(invalid(
                "weeklyHours",
                "invalid_weekday",
                "ISO weekdays 1 (Monday) … 7 (Sunday)",
            )
            .into());
        }
        let o = parse_time("weeklyHours", &w.opens_at)?;
        let c = parse_time("weeklyHours", &w.closes_at)?;
        if o >= c {
            return Err(invalid(
                "weeklyHours",
                "invalid_window",
                "opensAt must be before closesAt",
            )
            .into());
        }
        weekly.push(WeeklyHoursView {
            weekday: w.weekday,
            opens_at: fmt_time(o),
            closes_at: fmt_time(c),
        });
    }
    let mut blackout = Vec::with_capacity(b.blackout_dates.len());
    for d in &b.blackout_dates {
        blackout.push(parse_date("blackoutDates", d)?);
    }
    blackout.sort_unstable();
    blackout.dedup();
    let weekly_json = serde_json::to_value(&weekly).map_err(KernelError::internal)?;
    let notes = clean(b.notes);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let scope = institution_scope(ctx, node.institution_id);
    require_institution_writable(&st, &scope, node.institution_id).await?;
    editable(&node)?;
    sqlx::query(
        "INSERT INTO campus.availability_profile (tenant_id, institution_id, node_id, bookable, \
         weekly_hours, blackout_dates, notes) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (tenant_id, node_id) DO UPDATE SET bookable = EXCLUDED.bookable, \
         weekly_hours = EXCLUDED.weekly_hours, blackout_dates = EXCLUDED.blackout_dates, \
         notes = EXCLUDED.notes, updated_at = now()",
    )
    .bind(t.as_uuid())
    .bind(node.institution_id.as_uuid())
    .bind(id)
    .bind(b.bookable.unwrap_or(true))
    .bind(&weekly_json)
    .bind(&blackout)
    .bind(notes.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let new_version = bump_node(&mut tx, t, &node).await?;
    emit_modified(
        &mut tx,
        &scope,
        (
            INFRASTRUCTURE_NODE_MODIFIED,
            INFRASTRUCTURE_NODE_MODIFIED_SCHEMA,
        ),
        node.id.as_uuid(),
        new_version,
        vec!["availability".to_owned()],
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.availability",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"windows": weekly.len(), "blackoutDates": blackout.len()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Free / busy calendar of a space from its availability profile and reservations
/// (`campus.read`; at most 31 days).
#[utoipa::path(get, path = "/api/v1/campus/spaces/{id}/availability", operation_id = "campus_space_calendar", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), SpaceCalendarQuery), security(("session_cookie" = [])),
    responses((status = 200, body = SpaceCalendar), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn space_calendar(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<SpaceCalendarQuery>,
) -> ApiResult<Json<SpaceCalendar>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let node = require_node(&mut conn, t, id).await?;
    if node.node_type != InfrastructureNodeType::Space {
        return Err(invalid("id", "not_a_space", "only spaces have a booking calendar").into());
    }
    let scope = institution_scope(ctx, node.institution_id);
    let from = match q.from.as_deref() {
        Some(r) => parse_date("from", r)?,
        None => business_today(&st, &scope, node.institution_id).await?,
    };
    let to = match q.to.as_deref() {
        Some(r) => parse_date("to", r)?,
        None => from + Duration::days(6),
    };
    if to < from || (to - from).num_days() > 30 {
        return Err(invalid("to", "invalid_window", "from ≤ to, at most 31 days").into());
    }
    let profile = load_availability(&mut conn, t, node.id).await?.map(|x| x.0);
    let rows = sqlx::query(
        "SELECT id, number, reservation_date, start_time, end_time, status, purpose \
         FROM campus.space_reservation WHERE tenant_id = $1 AND node_id = $2 \
         AND reservation_date BETWEEN $3 AND $4 \
         AND status IN ('requested', 'confirmed', 'rescheduled') ORDER BY reservation_date, start_time",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut slots: BTreeMap<NaiveDate, Vec<SpaceBusySlot>> = BTreeMap::new();
    let mut ranges: BTreeMap<NaiveDate, Vec<(NaiveTime, NaiveTime)>> = BTreeMap::new();
    for r in &rows {
        let date: NaiveDate = r.try_get("reservation_date").map_err(map_db_error)?;
        let s: NaiveTime = r.try_get("start_time").map_err(map_db_error)?;
        let e: NaiveTime = r.try_get("end_time").map_err(map_db_error)?;
        let status_raw: String = r.try_get("status").map_err(map_db_error)?;
        let status = ReservationStatus::parse(&status_raw)
            .ok_or_else(|| KernelError::internal_msg(format!("unknown status {status_raw}")))?;
        ranges.entry(date).or_default().push((s, e));
        slots.entry(date).or_default().push(SpaceBusySlot {
            reservation_id: r.try_get("id").map_err(map_db_error)?,
            number: r.try_get("number").map_err(map_db_error)?,
            start_time: fmt_time(s),
            end_time: fmt_time(e),
            status,
            purpose: r.try_get("purpose").map_err(map_db_error)?,
        });
    }
    let win = |(s, e): &(NaiveTime, NaiveTime)| SpaceTimeWindow {
        start_time: fmt_time(*s),
        end_time: fmt_time(*e),
    };
    let mut days = Vec::new();
    let mut d = from;
    while d <= to {
        let open = open_windows(profile.as_ref(), d);
        let busy = ranges.get(&d).cloned().unwrap_or_default();
        let free = free_windows(&open, &busy);
        days.push(SpaceCalendarDay {
            date: d,
            open: open.iter().map(win).collect(),
            busy: slots.remove(&d).unwrap_or_default(),
            free: free.iter().map(win).collect(),
        });
        d += Duration::days(1);
    }
    Ok(Json(SpaceCalendar {
        node_id: node.id,
        code: node.code,
        status: node.status,
        capacity: node.capacity,
        days,
    }))
}

// ------------------------------------------------------------------ state commands

/// Activate a planned node (`campus.facility.manage`; its parent must be active).
#[utoipa::path(post, path = "/api/v1/campus/infrastructure-nodes/{id}/activate", operation_id = "campus_infra_node_activate", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_activate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let scope = institution_scope(ctx, node.institution_id);
    require_institution_writable(&st, &scope, node.institution_id).await?;
    if node.status != InfrastructureStatus::Planned {
        return Err(KernelError::Conflict(format!(
            "infrastructure node {} is {}; only a planned node is activated (restore a suspended one)",
            node.code, node.status
        ))
        .into());
    }
    if let Some(pid) = node.parent_id {
        let parent = load_node(&mut tx, t, pid).await?;
        if parent.is_some_and(|p| p.status != InfrastructureStatus::Active) {
            return Err(KernelError::Conflict(
                "the parent node is not active; activate it first".into(),
            )
            .into());
        }
    }
    move_node(
        &mut tx,
        &scope,
        &node,
        InfrastructureStatus::Active,
        reason.as_deref(),
        None,
        Some((
            INFRASTRUCTURE_NODE_ACTIVATED,
            INFRASTRUCTURE_NODE_ACTIVATED_SCHEMA,
        )),
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.activate",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Suspend an active node (`campus.facility.manage`; reason required): requested / confirmed
/// reservations from today on are flagged.
#[utoipa::path(post, path = "/api/v1/campus/infrastructure-nodes/{id}/suspend", operation_id = "campus_infra_node_suspend", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_suspend(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        require_node(&mut conn, t, id).await?
    };
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    let today = business_today(&st, &scope, probe.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    move_node(
        &mut tx,
        &scope,
        &node,
        InfrastructureStatus::Suspended,
        Some(&reason),
        None,
        Some((
            INFRASTRUCTURE_NODE_SUSPENDED,
            INFRASTRUCTURE_NODE_SUSPENDED_SCHEMA,
        )),
        &a,
    )
    .await?;
    let flagged = sqlx::query(
        "UPDATE campus.space_reservation SET flagged = true, flag_reason = $4, updated_at = now() \
         WHERE tenant_id = $1 AND node_id = $2 AND reservation_date >= $3 \
         AND status IN ('requested', 'confirmed', 'rescheduled')",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(today)
    .bind(format!("space suspended: {reason}"))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.suspend",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"reason": reason, "flaggedReservations": flagged}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Restore a suspended node (`campus.facility.manage`): the flags it raised are cleared.
#[utoipa::path(post, path = "/api/v1/campus/infrastructure-nodes/{id}/restore", operation_id = "campus_infra_node_restore", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_restore(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let scope = institution_scope(ctx, node.institution_id);
    require_institution_writable(&st, &scope, node.institution_id).await?;
    if node.status != InfrastructureStatus::Suspended {
        return Err(KernelError::Conflict(format!(
            "infrastructure node {} is {}; only a suspended node is restored",
            node.code, node.status
        ))
        .into());
    }
    move_node(
        &mut tx,
        &scope,
        &node,
        InfrastructureStatus::Active,
        reason.as_deref(),
        None,
        Some((
            INFRASTRUCTURE_NODE_RESTORED,
            INFRASTRUCTURE_NODE_RESTORED_SCHEMA,
        )),
        &a,
    )
    .await?;
    sqlx::query(
        "UPDATE campus.space_reservation SET flagged = false, flag_reason = NULL, \
         updated_at = now() WHERE tenant_id = $1 AND node_id = $2 AND flagged",
    )
    .bind(t.as_uuid())
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.restore",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// 409 while live children or future reservations block decommissioning.
async fn decommission_blockers(
    conn: &mut PgConnection,
    tenant: TenantId,
    node: &InfrastructureNode,
    today: NaiveDate,
) -> Result<(), KernelError> {
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.infrastructure_node WHERE tenant_id = $1 AND parent_id = $2 \
         AND status IN ('planned', 'active', 'suspended')",
    )
    .bind(tenant.as_uuid())
    .bind(node.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if live > 0 {
        return Err(KernelError::Conflict(format!(
            "infrastructure node {} still has {live} live child node(s); decommission them first",
            node.code
        )));
    }
    let future = future_reservations(conn, tenant, node.id, today).await?;
    if future > 0 {
        return Err(KernelError::Conflict(format!(
            "infrastructure node {} has {future} reservation(s) from today on; cancel or move them first",
            node.code
        )));
    }
    Ok(())
}

/// Request decommissioning (`campus.approve`, step-up; reason required; `NODE_DECOMMISSION`):
/// refused (409) while children are live or reservations from today on exist. Auto-approval
/// decommissions at once; otherwise the request waits for the decision endpoint.
#[utoipa::path(post, path = "/api/v1/campus/infrastructure-nodes/{id}/decommission", operation_id = "campus_infra_node_decommission", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_decommission(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        require_node(&mut conn, t, id).await?
    };
    ensure_version(v, probe.version)?;
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    if !probe
        .status
        .can_transition(InfrastructureStatus::Decommissioned)
    {
        return Err(KernelError::Conflict(format!(
            "infrastructure node {} is {}; it cannot become decommissioned",
            probe.code, probe.status
        ))
        .into());
    }
    let today = business_today(&st, &scope, probe.institution_id).await?;
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        decommission_blockers(&mut conn, t, &probe, today).await?;
    }
    let outcome = request_approval(
        &st,
        &scope,
        process::NODE_DECOMMISSION,
        business_key("infrastructure_node", id, probe.version),
        BTreeMap::from([
            ("nodeId".to_owned(), serde_json::json!(id)),
            ("code".to_owned(), serde_json::json!(probe.code)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM campus.infrastructure_decommission WHERE tenant_id = $1 \
         AND node_id = $2 AND status = 'pending')",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if pending {
        return Err(KernelError::Conflict(
            "a decommissioning request is already waiting for a decision".into(),
        )
        .into());
    }
    let approved = matches!(outcome, ApprovalOutcome::Approved { .. });
    let system = Actor::system(a.correlation_id.clone());
    sqlx::query(
        "INSERT INTO campus.infrastructure_decommission (tenant_id, institution_id, node_id, \
         status, reason, process_instance_id, requested_by, decided_by, decided_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, CASE WHEN $4 = 'approved' THEN now() END)",
    )
    .bind(t.as_uuid())
    .bind(node.institution_id.as_uuid())
    .bind(id)
    .bind(if approved { "approved" } else { "pending" })
    .bind(&reason)
    .bind(&pid)
    .bind(a.id.as_deref())
    .bind(if approved { system.id.as_deref() } else { None })
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if approved {
        move_node(
            &mut tx,
            &scope,
            &node,
            InfrastructureStatus::Decommissioned,
            Some(&reason),
            Some(&pid),
            Some((
                INFRASTRUCTURE_NODE_DECOMMISSIONED,
                INFRASTRUCTURE_NODE_DECOMMISSIONED_SCHEMA,
            )),
            &system,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.decommission",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"reason": reason, "processInstanceId": pid, "approved": approved}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Decide a pending decommissioning request (`campus.approve`, step-up): approve
/// decommissions the node (blockers re-checked), reject keeps it.
#[utoipa::path(post, path = "/api/v1/campus/infrastructure-nodes/{id}/decommission-decision", operation_id = "campus_infra_node_decommission_decide", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = FacilityDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_decommission_decide(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason.clone());
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        require_node(&mut conn, t, id).await?
    };
    let scope = institution_scope(ctx, probe.institution_id);
    require_institution_writable(&st, &scope, probe.institution_id).await?;
    let today = business_today(&st, &scope, probe.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let req: Option<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT id, reason, process_instance_id FROM campus.infrastructure_decommission \
         WHERE tenant_id = $1 AND node_id = $2 AND status = 'pending' FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let Some((req_id, req_reason, pid)) = req else {
        return Err(KernelError::Conflict(
            "no decommissioning request is waiting for a decision".into(),
        )
        .into());
    };
    if b.approve() {
        decommission_blockers(&mut tx, t, &node, today).await?;
        move_node(
            &mut tx,
            &scope,
            &node,
            InfrastructureStatus::Decommissioned,
            Some(reason.as_deref().unwrap_or(&req_reason)),
            pid.as_deref(),
            Some((
                INFRASTRUCTURE_NODE_DECOMMISSIONED,
                INFRASTRUCTURE_NODE_DECOMMISSIONED_SCHEMA,
            )),
            &a,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE campus.infrastructure_decommission SET status = $2, decided_by = $3, \
         decision_reason = $4, decided_at = now() WHERE id = $1",
    )
    .bind(req_id)
    .bind(if b.approve() { "approved" } else { "rejected" })
    .bind(a.id.as_deref())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.decommission_decide",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

/// Archive a planned or decommissioned node (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/infrastructure-nodes/{id}/archive", operation_id = "campus_infra_node_archive", tag = "campus-infrastructure",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the node")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InfrastructureNodeDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn node_archive(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = lock_node(&mut tx, t, InfrastructureNodeId(id)).await?;
    ensure_version(v, node.version)?;
    let scope = institution_scope(ctx, node.institution_id);
    require_institution_writable(&st, &scope, node.institution_id).await?;
    move_node(
        &mut tx,
        &scope,
        &node,
        InfrastructureStatus::Archived,
        reason.as_deref(),
        None,
        None,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "campus.infrastructure_node.archive",
        "infrastructure_node",
        node.id.as_uuid(),
        serde_json::json!({"reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = node_detail(&st, t, id).await?;
    Ok(detail_response(StatusCode::OK, &d))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(node_create, node_list))
        .routes(routes!(node_get, node_update))
        .routes(routes!(node_capacity))
        .routes(routes!(node_capabilities))
        .routes(routes!(node_availability_get, node_availability_set))
        .routes(routes!(space_calendar))
        .routes(routes!(node_activate))
        .routes(routes!(node_suspend))
        .routes(routes!(node_restore))
        .routes(routes!(node_decommission))
        .routes(routes!(node_decommission_decide))
        .routes(routes!(node_archive))
        .with_state(state)
}
