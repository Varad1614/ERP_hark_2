//! Service desk (L2-G01; L3-G-016, 017, 018, 041, 042, 046): service requests and complaints
//! opened through `shared::tickets::open_ticket` (office creation and `/me/tickets`), triage
//! (category / priority → SLA recalculated), assignment / reassignment, start, hold, resolution,
//! requester confirmation (`/me/tickets/{id}/confirm`) or administrative closure, reopening
//! within 7 days, duplicates merged onto a canonical ticket, manual escalation and the bulk SLA
//! check (`sla_breached`, escalation level + 1, `service_ticket.escalated`), cancellation.
//! Status only through `shared::tickets::change_ticket_status`.

use crate::infrastructure::common::{
    audit, clean, ensure_version, expected, history, invalid, limit, parse_filter, required,
    FacilityBulkFailure, FacilityBulkSummary, FacilityPersonInput, FacilityReasonBody, Transition,
};
use crate::infrastructure::domain::reopen_allowed;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::me::{my_person, require_person, PersonRef};
use crate::shared::publish;
use crate::shared::rows::{
    load_node, load_ticket, lock_ticket, ticket_from_row, ServiceTicket, TICKET_COLUMNS,
};
use crate::shared::scope::institution_scope;
use crate::shared::tickets::{change_ticket_status, open_ticket, NewTicket};
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use sutra_contracts::campus::events::facilities::{
    SERVICE_TICKET_ASSIGNED, SERVICE_TICKET_ASSIGNED_SCHEMA, SERVICE_TICKET_CLOSED,
    SERVICE_TICKET_CLOSED_SCHEMA, SERVICE_TICKET_ESCALATED, SERVICE_TICKET_ESCALATED_SCHEMA,
    SERVICE_TICKET_RESOLVED, SERVICE_TICKET_RESOLVED_SCHEMA,
};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_contracts::campus::ids::{InfrastructureNodeId, ServiceTicketId};
use sutra_contracts::campus::status::{
    PersonKind, TicketCategory, TicketPriority, TicketSource, TicketStatus,
};
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

/// A service desk ticket (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketView {
    pub id: ServiceTicketId,
    #[schema(example = "SR-000001")]
    pub number: String,
    pub institution_id: Uuid,
    pub category: TicketCategory,
    pub source: TicketSource,
    pub source_ref_id: Option<Uuid>,
    pub is_complaint: bool,
    pub node_id: Option<InfrastructureNodeId>,
    pub requester_kind: Option<PersonKind>,
    pub requester_id: Option<Uuid>,
    pub title: String,
    pub description: Option<String>,
    pub priority: TicketPriority,
    pub status: TicketStatus,
    pub status_reason: Option<String>,
    pub assignee: Option<String>,
    pub sla_due_at: DateTime<Utc>,
    pub sla_breached: bool,
    pub escalation_level: i32,
    pub resolved_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub resolution: Option<String>,
    pub canonical_ticket_id: Option<ServiceTicketId>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

impl From<&ServiceTicket> for TicketView {
    fn from(t: &ServiceTicket) -> Self {
        Self {
            id: t.id,
            number: t.number.clone(),
            institution_id: t.institution_id.as_uuid(),
            category: t.category,
            source: t.source,
            source_ref_id: t.source_ref_id,
            is_complaint: t.is_complaint,
            node_id: t.node_id,
            requester_kind: t.requester_kind,
            requester_id: t.requester_id,
            title: t.title.clone(),
            description: t.description.clone(),
            priority: t.priority,
            status: t.status,
            status_reason: t.status_reason.clone(),
            assignee: t.assignee.clone(),
            sla_due_at: t.sla_due_at,
            sla_breached: t.sla_breached,
            escalation_level: t.escalation_level,
            resolved_at: t.resolved_at,
            closed_at: t.closed_at,
            resolution: t.resolution.clone(),
            canonical_ticket_id: t.canonical_ticket_id,
            created_at: t.created_at,
            version: t.version,
        }
    }
}

/// One escalation step of a ticket.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketEscalationView {
    pub level: i32,
    /// `sla_breach` or `manual`.
    pub kind: String,
    pub reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A ticket with its escalations and duplicates.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketDetail {
    pub ticket: TicketView,
    pub escalations: Vec<TicketEscalationView>,
    /// Tickets merged onto this one.
    pub duplicates: Vec<String>,
    pub version: i64,
}

/// A page of tickets.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketList {
    pub items: Vec<TicketView>,
    /// Pass as `?cursor=` for the next page.
    pub next_cursor: Option<Uuid>,
}

/// Open a ticket from the office.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketCreate {
    pub category: TicketCategory,
    pub title: String,
    pub description: Option<String>,
    /// Default `medium`.
    pub priority: Option<TicketPriority>,
    pub node_id: Option<Uuid>,
    /// Complaint (`true`) or service request (default).
    pub is_complaint: Option<bool>,
    /// The student / staff member who reported it.
    pub requester: Option<FacilityPersonInput>,
    /// Overrides the priority's default SLA.
    pub sla_hours: Option<i64>,
}

/// Open my own ticket.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketSelfCreate {
    /// Required unless `nodeId` is given (the node's institution is used then).
    pub institution_id: Option<Uuid>,
    pub category: TicketCategory,
    pub title: String,
    pub description: Option<String>,
    pub priority: Option<TicketPriority>,
    pub node_id: Option<Uuid>,
    pub is_complaint: Option<bool>,
}

/// Triage: (re)classify; the SLA is recalculated from the new priority.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketTriage {
    pub category: Option<TicketCategory>,
    pub priority: Option<TicketPriority>,
    pub reason: Option<String>,
}

/// Assign (or reassign) a technician / team.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketAssign {
    pub assignee: String,
    pub reason: Option<String>,
}

/// Resolve with the resolution text.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketResolve {
    pub resolution: String,
}

/// Merge a duplicate onto its canonical ticket.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketMerge {
    pub canonical_ticket_id: Uuid,
    pub reason: Option<String>,
}

/// Requester verification of a resolved ticket.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TicketConfirm {
    /// `true` closes the ticket; `false` reopens it (within 7 days of resolution).
    pub satisfied: bool,
    pub comment: Option<String>,
}

/// Filters of the ticket list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct TicketQuery {
    pub status: Option<String>,
    pub category: Option<String>,
    pub priority: Option<String>,
    pub breached: Option<bool>,
    pub complaint: Option<bool>,
    /// `true` = only tickets that are not closed / cancelled / merged.
    pub open: Option<bool>,
    pub limit: Option<i64>,
    pub cursor: Option<Uuid>,
}

// ------------------------------------------------------------------ helpers

fn respond(status: StatusCode, t: &ServiceTicket) -> Response {
    with_etag(status, &TicketView::from(t), Version(t.version))
}

/// Moves a locked ticket through the shared status writer and emits the 8A event (if any).
pub(crate) async fn move_ticket(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ticket: &ServiceTicket,
    to: TicketStatus,
    reason: Option<&str>,
    event: Option<(&'static str, &'static str)>,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    let moved = change_ticket_status(
        conn,
        scope.tenant_id,
        ticket,
        to,
        &StatusChange {
            cause_event_type: event.map(|e| e.0).unwrap_or(""),
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    if let Some(e) = event {
        emit_ticket(conn, scope, &moved, Some(ticket.status), reason, e, actor).await?;
    }
    Ok(moved)
}

async fn emit_ticket(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: &ServiceTicket,
    from: Option<TicketStatus>,
    reason: Option<&str>,
    event: (&'static str, &'static str),
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::event(
        conn,
        scope,
        event.0,
        event.1,
        t.id.to_string(),
        &StatusChangedV1 {
            aggregate_id: t.id.as_uuid(),
            institution_id: t.institution_id.as_uuid(),
            from_status: from.map(|s| s.as_str().to_owned()),
            to_status: t.status.as_str().to_owned(),
            version: t.version,
            reason: reason.map(str::to_owned).or_else(|| Some(t.title.clone())),
            process_instance_id: None,
            number: Some(t.number.clone()),
            subject_ref: t.requester_subject.clone(),
        },
        actor,
    )
    .await
}

/// Sets the assignee of a locked ticket and moves it to `assigned` (or, when it is already
/// assigned / in progress, records a reassignment with a version bump). Used by the ticket API
/// and by corrective work orders.
pub(crate) async fn assign_ticket(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ticket: &ServiceTicket,
    assignee: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    let tenant = scope.tenant_id;
    if matches!(
        ticket.status,
        TicketStatus::Assigned | TicketStatus::InProgress
    ) {
        let sql = format!(
            "UPDATE campus.service_ticket SET assignee = $3, version = version + 1, \
             updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $4 \
             RETURNING {TICKET_COLUMNS}"
        );
        let row = sqlx::query(&sql)
            .bind(ticket.id.as_uuid())
            .bind(tenant.as_uuid())
            .bind(assignee)
            .bind(ticket.version)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?
            .ok_or_else(|| KernelError::Conflict("service ticket changed concurrently".into()))?;
        let t = ticket_from_row(&row).map_err(map_db_error)?;
        history(
            conn,
            &Transition {
                tenant,
                institution: t.institution_id,
                aggregate_type: "service_ticket",
                aggregate_id: t.id.as_uuid(),
                from: Some(ticket.status.as_str()),
                to: t.status.as_str(),
                reason: Some(reason.unwrap_or("reassigned")),
                process_instance_id: None,
                cause: SERVICE_TICKET_ASSIGNED,
            },
            actor,
        )
        .await?;
        emit_ticket(
            conn,
            scope,
            &t,
            Some(ticket.status),
            reason,
            (SERVICE_TICKET_ASSIGNED, SERVICE_TICKET_ASSIGNED_SCHEMA),
            actor,
        )
        .await?;
        return Ok(t);
    }
    if !ticket.status.can_transition(TicketStatus::Assigned) {
        return Err(KernelError::Conflict(format!(
            "service ticket {} is {}; it cannot be assigned",
            ticket.number, ticket.status
        )));
    }
    sqlx::query("UPDATE campus.service_ticket SET assignee = $3 WHERE id = $1 AND tenant_id = $2")
        .bind(ticket.id.as_uuid())
        .bind(tenant.as_uuid())
        .bind(assignee)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    move_ticket(
        conn,
        scope,
        ticket,
        TicketStatus::Assigned,
        reason,
        Some((SERVICE_TICKET_ASSIGNED, SERVICE_TICKET_ASSIGNED_SCHEMA)),
        actor,
    )
    .await
}

/// Resolves a locked ticket with `resolution` (ticket API and work order verification).
pub(crate) async fn resolve_ticket(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ticket: &ServiceTicket,
    resolution: &str,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    if !ticket.status.can_transition(TicketStatus::Resolved) {
        return Err(KernelError::Conflict(format!(
            "service ticket {} is {}; it cannot be resolved",
            ticket.number, ticket.status
        )));
    }
    sqlx::query(
        "UPDATE campus.service_ticket SET resolution = $3 WHERE id = $1 AND tenant_id = $2",
    )
    .bind(ticket.id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(resolution)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    move_ticket(
        conn,
        scope,
        ticket,
        TicketStatus::Resolved,
        Some(resolution),
        Some((SERVICE_TICKET_RESOLVED, SERVICE_TICKET_RESOLVED_SCHEMA)),
        actor,
    )
    .await
}

/// Escalates a locked ticket one level (`sla_breach` or `manual`).
async fn escalate(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ticket: &ServiceTicket,
    kind: &str,
    reason: &str,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    let sql = format!(
        "UPDATE campus.service_ticket SET escalation_level = escalation_level + 1, \
         sla_breached = sla_breached OR $3, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $4 RETURNING {TICKET_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(ticket.id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(kind == "sla_breach")
        .bind(ticket.version)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("service ticket changed concurrently".into()))?;
    let t = ticket_from_row(&row).map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO campus.ticket_escalation (tenant_id, institution_id, ticket_id, level, kind, \
         reason, actor) VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(t.institution_id.as_uuid())
    .bind(t.id.as_uuid())
    .bind(t.escalation_level)
    .bind(kind)
    .bind(reason)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    emit_ticket(
        conn,
        scope,
        &t,
        Some(ticket.status),
        Some(reason),
        (SERVICE_TICKET_ESCALATED, SERVICE_TICKET_ESCALATED_SCHEMA),
        actor,
    )
    .await?;
    Ok(t)
}

fn is_mine(t: &ServiceTicket, me: &PersonRef) -> bool {
    t.requester_kind == Some(me.kind) && t.requester_id == Some(me.id)
}

async fn probe(st: &CampusState, tenant: TenantId, id: Uuid) -> Result<ServiceTicket, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_ticket(&mut conn, tenant, ServiceTicketId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "service ticket",
        })
}

async fn detail(
    st: &CampusState,
    tenant: TenantId,
    t: &ServiceTicket,
) -> Result<TicketDetail, KernelError> {
    let rows: Vec<(i32, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT level, kind, reason, created_at FROM campus.ticket_escalation \
         WHERE tenant_id = $1 AND ticket_id = $2 ORDER BY level",
    )
    .bind(tenant.as_uuid())
    .bind(t.id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let duplicates: Vec<String> = sqlx::query_scalar(
        "SELECT number FROM campus.service_ticket WHERE tenant_id = $1 \
         AND canonical_ticket_id = $2 ORDER BY number",
    )
    .bind(tenant.as_uuid())
    .bind(t.id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(TicketDetail {
        ticket: TicketView::from(t),
        escalations: rows
            .into_iter()
            .map(|(level, kind, reason, created_at)| TicketEscalationView {
                level,
                kind,
                reason,
                created_at,
            })
            .collect(),
        duplicates,
        version: t.version,
    })
}

/// Shared shape of the simple state commands: lock, check version, run `f`, audit, commit.
async fn command<F>(
    st: &CampusState,
    ctx: &AuthContext,
    id: Uuid,
    expected_version: i64,
    action: &str,
    details: serde_json::Value,
    f: F,
) -> Result<ServiceTicket, KernelError>
where
    F: for<'c> FnOnce(
        &'c mut PgConnection,
        &'c ExecutionScope,
        &'c ServiceTicket,
        &'c Actor,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<ServiceTicket, KernelError>> + Send + 'c>,
    >,
{
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let p = probe(st, t, id).await?;
    let scope = institution_scope(ctx, p.institution_id);
    require_institution_writable(st, &scope, p.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ticket = lock_ticket(&mut tx, t, ServiceTicketId(id)).await?;
    ensure_version(expected_version, ticket.version)?;
    let out = f(&mut tx, &scope, &ticket, &a).await?;
    // ERP_EXTENSIONS 2B: the institution's service-desk triggers (updated).
    let out = super::automation::after_event(
        &mut tx,
        &scope,
        out,
        Some(&ticket),
        super::rules::Event::Updated,
        &a,
    )
    .await?;
    audit(&mut tx, &scope, action, "service_ticket", id, details, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ handlers: open / read

/// Open a ticket from the office (`campus.facility.manage`; source `admin`).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/tickets", operation_id = "campus_ticket_create", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TicketCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TicketCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let requester = match b.requester {
        None => None,
        Some(p) => Some(require_person(&st, &scope, p.kind, p.id, "requester").await?),
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ticket = open_ticket(
        &mut tx,
        &scope,
        NewTicket {
            institution,
            category: b.category,
            source: TicketSource::Admin,
            source_ref_id: None,
            is_complaint: b.is_complaint.unwrap_or(false),
            node_id: b.node_id.map(InfrastructureNodeId),
            requester: requester.as_ref(),
            title: &b.title,
            description: b.description.as_deref(),
            priority: b.priority.unwrap_or(TicketPriority::Medium),
            sla_hours: b.sla_hours,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::CREATED, &ticket))
}

async fn list(
    st: &CampusState,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    requester: Option<&PersonRef>,
    q: &TicketQuery,
) -> Result<TicketList, KernelError> {
    let status = parse_filter("status", q.status.as_deref(), TicketStatus::parse)?;
    let category = parse_filter("category", q.category.as_deref(), TicketCategory::parse)?;
    let priority = parse_filter("priority", q.priority.as_deref(), TicketPriority::parse)?;
    let lim = limit(q.limit);
    let sql = format!(
        "SELECT {TICKET_COLUMNS} FROM campus.service_ticket WHERE tenant_id = $1 \
         AND ($2::uuid IS NULL OR institution_id = $2) \
         AND ($3::text IS NULL OR requester_kind = $3) AND ($4::uuid IS NULL OR requester_id = $4) \
         AND ($5::text IS NULL OR status = $5) AND ($6::text IS NULL OR category = $6) \
         AND ($7::text IS NULL OR priority = $7) AND ($8::bool IS NULL OR sla_breached = $8) \
         AND ($9::bool IS NULL OR is_complaint = $9) \
         AND ($10::bool IS NULL OR (status NOT IN ('closed', 'cancelled', 'merged')) = $10) \
         AND ($11::uuid IS NULL OR id < $11) ORDER BY id DESC LIMIT $12"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.map(|i| i.as_uuid()))
        .bind(requester.map(|p| p.kind.as_str()))
        .bind(requester.map(|p| p.id))
        .bind(status.map(|s| s.as_str()))
        .bind(category.map(|s| s.as_str()))
        .bind(priority.map(|s| s.as_str()))
        .bind(q.breached)
        .bind(q.complaint)
        .bind(q.open)
        .bind(q.cursor)
        .bind(lim)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(TicketView::from(&ticket_from_row(r).map_err(map_db_error)?));
    }
    let next_cursor = if i64::try_from(items.len()).unwrap_or(0) == lim {
        items.last().map(|t| t.id.as_uuid())
    } else {
        None
    };
    Ok(TicketList { items, next_cursor })
}

/// The ticket queue of an institution (`campus.read`; newest first, cursor-paged).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/tickets", operation_id = "campus_ticket_list", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), TicketQuery), security(("session_cookie" = [])),
    responses((status = 200, body = TicketList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<TicketQuery>,
) -> ApiResult<Json<TicketList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    Ok(Json(
        list(&st, ctx.scope.tenant_id, Some(institution), None, &q).await?,
    ))
}

/// One ticket with escalations and merged duplicates (`campus.read`, or its requester).
#[utoipa::path(get, path = "/api/v1/campus/tickets/{id}", operation_id = "campus_ticket_get", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = TicketDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let t = ctx.scope.tenant_id;
    let ticket = probe(&st, t, id).await?;
    if !ctx.has_permission(Permission::CampusRead) {
        let me = my_person(&st, ctx).await?;
        if !is_mine(&ticket, &me) {
            return Err(KernelError::NotFound {
                entity: "service ticket",
            }
            .into());
        }
    }
    let d = detail(&st, t, &ticket).await?;
    Ok(with_etag(StatusCode::OK, &d, Version(d.version)))
}

/// Open my own service request or complaint (`campus.self`; source `self_service`).
#[utoipa::path(post, path = "/api/v1/campus/me/tickets", operation_id = "campus_ticket_self_create", tag = "campus-facilities",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = TicketSelfCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_ticket_create(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<TicketSelfCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let me = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let institution = match b.node_id {
        Some(n) => {
            let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
            load_node(&mut conn, t, InfrastructureNodeId(n))
                .await?
                .ok_or_else(|| {
                    invalid(
                        "nodeId",
                        "unknown_node",
                        "no infrastructure node with this id",
                    )
                })?
                .institution_id
        }
        None => InstitutionId(b.institution_id.ok_or_else(|| {
            invalid(
                "institutionId",
                "required",
                "name the institution or the space concerned",
            )
        })?),
    };
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ticket = open_ticket(
        &mut tx,
        &scope,
        NewTicket {
            institution,
            category: b.category,
            source: TicketSource::SelfService,
            source_ref_id: None,
            is_complaint: b.is_complaint.unwrap_or(false),
            node_id: b.node_id.map(InfrastructureNodeId),
            requester: Some(&me),
            title: &b.title,
            description: b.description.as_deref(),
            priority: b.priority.unwrap_or(TicketPriority::Medium),
            sla_hours: None,
        },
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::CREATED, &ticket))
}

/// My tickets (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/tickets", operation_id = "campus_ticket_self_list", tag = "campus-facilities",
    params(TicketQuery), security(("session_cookie" = [])),
    responses((status = 200, body = TicketList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_ticket_list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Query(q): Query<TicketQuery>,
) -> ApiResult<Json<TicketList>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let me = my_person(&st, ctx).await?;
    Ok(Json(
        list(&st, ctx.scope.tenant_id, None, Some(&me), &q).await?,
    ))
}

/// Verify my resolved ticket (`campus.self`, requester only): satisfied → closed; not
/// satisfied → reopened (comment required; within 7 days of resolution).
#[utoipa::path(post, path = "/api/v1/campus/me/tickets/{id}/confirm", operation_id = "campus_ticket_self_confirm", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = TicketConfirm, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_ticket_confirm(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TicketConfirm>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let v = expected!(headers);
    let me = my_person(&st, ctx).await?;
    let p = probe(&st, ctx.scope.tenant_id, id).await?;
    if !is_mine(&p, &me) {
        return Err(KernelError::NotFound {
            entity: "service ticket",
        }
        .into());
    }
    let comment = clean(b.comment);
    if !b.satisfied && comment.is_none() {
        return Err(invalid("comment", "required", "say what is still wrong").into());
    }
    let satisfied = b.satisfied;
    let c2 = comment.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.confirm",
        serde_json::json!({"satisfied": satisfied, "comment": comment}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                if satisfied {
                    move_ticket(
                        conn,
                        scope,
                        ticket,
                        TicketStatus::Closed,
                        Some(c2.as_deref().unwrap_or("confirmed by the requester")),
                        Some((SERVICE_TICKET_CLOSED, SERVICE_TICKET_CLOSED_SCHEMA)),
                        a,
                    )
                    .await
                } else {
                    if ticket.status == TicketStatus::Resolved
                        && !reopen_allowed(ticket.resolved_at, Utc::now())
                    {
                        return Err(KernelError::Conflict(
                            "the resolution is older than 7 days; open a new ticket".into(),
                        ));
                    }
                    move_ticket(
                        conn,
                        scope,
                        ticket,
                        TicketStatus::Reopened,
                        c2.as_deref(),
                        None,
                        a,
                    )
                    .await
                }
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

// ------------------------------------------------------------------ handlers: office commands

/// Triage an open ticket (`campus.facility.manage`): category / priority, SLA recalculated from
/// the creation time.
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/triage", operation_id = "campus_ticket_triage", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = TicketTriage, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_triage(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TicketTriage>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let (category, priority) = (b.category, b.priority);
    let r2 = reason.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.triage",
        serde_json::json!({"category": category, "priority": priority, "reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                if !ticket.status.can_transition(TicketStatus::Triaged) {
                    return Err(KernelError::Conflict(format!(
                        "service ticket {} is {}; only an open ticket is triaged",
                        ticket.number, ticket.status
                    )));
                }
                let p = priority.unwrap_or(ticket.priority);
                let c = category.unwrap_or(ticket.category);
                // ERP_EXTENSIONS 2B: the SLA policy's resolution target when one is set.
                let minutes = super::automation::resolve_minutes(
                    conn,
                    scope.tenant_id,
                    ticket.institution_id,
                    p,
                )
                .await?;
                let hours = i32::try_from(minutes).unwrap_or(i32::MAX);
                sqlx::query(
                    "UPDATE campus.service_ticket SET category = $3, priority = $4, \
                     sla_due_at = created_at + make_interval(mins => $5), \
                     sla_breached = sla_breached AND (created_at + make_interval(mins => $5)) < now() \
                     WHERE id = $1 AND tenant_id = $2",
                )
                .bind(ticket.id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(c.as_str())
                .bind(p.as_str())
                .bind(hours)
                .execute(&mut *conn)
                .await
                .map_err(map_db_error)?;
                move_ticket(conn, scope, ticket, TicketStatus::Triaged, r2.as_deref(), None, a).await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Assign or reassign a technician / team (`campus.facility.manage`; `service_ticket.assigned`).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/assign", operation_id = "campus_ticket_assign", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = TicketAssign, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_assign(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TicketAssign>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let assignee = required("assignee", Some(b.assignee))?;
    if assignee.len() > 200 {
        return Err(invalid("assignee", "too_long", "at most 200 characters").into());
    }
    let reason = clean(b.reason);
    let (as2, r2) = (assignee.clone(), reason.clone());
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.assign",
        serde_json::json!({"assignee": assignee, "reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(
                async move { assign_ticket(conn, scope, ticket, &as2, r2.as_deref(), a).await },
            )
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Start work on an assigned / on-hold / reopened ticket (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/start", operation_id = "campus_ticket_start", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_start(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let r2 = reason.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.start",
        serde_json::json!({"reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                if ticket.assignee.is_none() {
                    return Err(KernelError::Conflict(format!(
                        "service ticket {} has no assignee; assign it first",
                        ticket.number
                    )));
                }
                move_ticket(
                    conn,
                    scope,
                    ticket,
                    TicketStatus::InProgress,
                    r2.as_deref(),
                    None,
                    a,
                )
                .await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Put a ticket on hold (`campus.facility.manage`; reason required; the SLA check skips it).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/hold", operation_id = "campus_ticket_hold", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_hold(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let r2 = reason.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.hold",
        serde_json::json!({"reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                move_ticket(
                    conn,
                    scope,
                    ticket,
                    TicketStatus::OnHold,
                    Some(&r2),
                    None,
                    a,
                )
                .await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Resolve with the resolution text (`campus.facility.manage`; `service_ticket.resolved` →
/// the requester verifies).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/resolve", operation_id = "campus_ticket_resolve", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = TicketResolve, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_resolve(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TicketResolve>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let resolution = required("resolution", Some(b.resolution))?;
    let r2 = resolution.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.resolve",
        serde_json::json!({"resolution": resolution}),
        move |conn, scope, ticket, a| {
            Box::pin(async move { resolve_ticket(conn, scope, ticket, &r2, a).await })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Close a resolved ticket administratively (`campus.facility.manage`; `service_ticket.closed`).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/close", operation_id = "campus_ticket_close", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_close(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason).unwrap_or_else(|| "closed by the service desk".to_owned());
    let r2 = reason.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.close",
        serde_json::json!({"reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                move_ticket(
                    conn,
                    scope,
                    ticket,
                    TicketStatus::Closed,
                    Some(&r2),
                    Some((SERVICE_TICKET_CLOSED, SERVICE_TICKET_CLOSED_SCHEMA)),
                    a,
                )
                .await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Reopen a resolved ticket within 7 days of its resolution (`campus.facility.manage`).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/reopen", operation_id = "campus_ticket_reopen", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_reopen(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let r2 = reason.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.reopen",
        serde_json::json!({"reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                if ticket.status == TicketStatus::Resolved
                    && !reopen_allowed(ticket.resolved_at, Utc::now())
                {
                    return Err(KernelError::Conflict(
                        "the resolution is older than 7 days; open a new ticket".into(),
                    ));
                }
                move_ticket(
                    conn,
                    scope,
                    ticket,
                    TicketStatus::Reopened,
                    Some(&r2),
                    None,
                    a,
                )
                .await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Cancel a ticket (`campus.facility.manage`; reason required).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/cancel", operation_id = "campus_ticket_cancel", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_cancel(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let r2 = reason.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.cancel",
        serde_json::json!({"reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                move_ticket(
                    conn,
                    scope,
                    ticket,
                    TicketStatus::Cancelled,
                    Some(&r2),
                    None,
                    a,
                )
                .await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Merge a duplicate onto its canonical ticket (`campus.facility.manage`): the canonical ticket
/// must be another open, unmerged ticket of the same institution.
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/merge", operation_id = "campus_ticket_merge", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duplicate")),
    request_body = TicketMerge, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_merge(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<TicketMerge>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    if b.canonical_ticket_id == id {
        return Err(invalid(
            "canonicalTicketId",
            "self_merge",
            "a ticket cannot be merged onto itself",
        )
        .into());
    }
    let canonical = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_ticket(
            &mut conn,
            ctx.scope.tenant_id,
            ServiceTicketId(b.canonical_ticket_id),
        )
        .await?
        .ok_or_else(|| {
            invalid(
                "canonicalTicketId",
                "unknown_ticket",
                "no service ticket with this id",
            )
        })?
    };
    if !canonical.status.is_open() {
        return Err(KernelError::Conflict(format!(
            "canonical ticket {} is {}; merge onto an open ticket",
            canonical.number, canonical.status
        ))
        .into());
    }
    let reason = clean(b.reason);
    let (cid, cnum, cinst) = (
        canonical.id,
        canonical.number.clone(),
        canonical.institution_id,
    );
    let r2 = reason
        .clone()
        .unwrap_or_else(|| format!("duplicate of {cnum}"));
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.merge",
        serde_json::json!({"canonicalTicketId": cid, "canonicalNumber": cnum, "reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                if ticket.institution_id != cinst {
                    return Err(invalid(
                        "canonicalTicketId",
                        "other_institution",
                        "the canonical ticket belongs to another institution",
                    ));
                }
                sqlx::query(
                    "UPDATE campus.service_ticket SET canonical_ticket_id = $3 \
                     WHERE id = $1 AND tenant_id = $2",
                )
                .bind(ticket.id.as_uuid())
                .bind(scope.tenant_id.as_uuid())
                .bind(cid.as_uuid())
                .execute(&mut *conn)
                .await
                .map_err(map_db_error)?;
                move_ticket(
                    conn,
                    scope,
                    ticket,
                    TicketStatus::Merged,
                    Some(&r2),
                    None,
                    a,
                )
                .await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// Escalate an open ticket one level by hand (`campus.facility.manage`; reason required;
/// `service_ticket.escalated`).
#[utoipa::path(post, path = "/api/v1/campus/tickets/{id}/escalate", operation_id = "campus_ticket_escalate", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the ticket")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = TicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_escalate(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacilityReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacilityManage)?;
    let v = expected!(headers);
    let reason = required("reason", b.reason)?;
    let r2 = reason.clone();
    let out = command(
        &st,
        ctx,
        id,
        v,
        "campus.service_ticket.escalate",
        serde_json::json!({"reason": reason}),
        move |conn, scope, ticket, a| {
            Box::pin(async move {
                if !ticket.status.is_open() || ticket.status == TicketStatus::Resolved {
                    return Err(KernelError::Conflict(format!(
                        "service ticket {} is {}; only unresolved tickets escalate",
                        ticket.number, ticket.status
                    )));
                }
                escalate(conn, scope, ticket, "manual", &r2, a).await
            })
        },
    )
    .await?;
    Ok(respond(StatusCode::OK, &out))
}

/// SLA check (`campus.facility.manage`; bulk, `Idempotency-Key`): every unresolved ticket past
/// its SLA that is not yet flagged gets `slaBreached`, escalation level + 1 and
/// `service_ticket.escalated`. Tickets on hold are skipped (the SLA is paused).
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/tickets/sla-check", operation_id = "campus_ticket_sla_check", tag = "campus-facilities",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = FacilityReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FacilityBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn ticket_sla_check(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<FacilityReasonBody>,
) -> ApiResult<Json<FacilityBulkSummary>> {
    let ctx = auth.require(Permission::FacilityManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let system = Actor::system(actor(ctx).correlation_id);
    // ERP_EXTENSIONS 2B: with an SLA policy, a missed first response breaches too.
    let first: Vec<Option<i32>> = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let policy = super::automation::load_policy(&mut conn, t, institution).await?;
        ["low", "medium", "high", "critical"]
            .iter()
            .map(|p| {
                policy
                    .as_ref()
                    .filter(|x| x.enabled)
                    .and_then(|x| x.targets.get(*p))
                    .map(|x| x.first)
                    .filter(|m| *m > 0)
                    .and_then(|m| i32::try_from(m).ok())
            })
            .collect()
    };
    let due: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM campus.service_ticket WHERE tenant_id = $1 AND institution_id = $2 \
         AND status IN ('open', 'triaged', 'assigned', 'in_progress', 'reopened') \
         AND NOT sla_breached AND (sla_due_at < now() OR (first_response_at IS NULL \
           AND status IN ('open', 'reopened') AND created_at + make_interval(mins => CASE priority \
             WHEN 'low' THEN $3::int WHEN 'medium' THEN $4::int WHEN 'high' THEN $5::int \
             ELSE $6::int END) < now())) ORDER BY sla_due_at LIMIT 1000",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(first[0])
    .bind(first[1])
    .bind(first[2])
    .bind(first[3])
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let held: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.service_ticket WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'on_hold' AND NOT sla_breached AND sla_due_at < now()",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut summary = FacilityBulkSummary {
        skipped: held,
        ..FacilityBulkSummary::default()
    };
    for tid in due {
        let res: Result<(), KernelError> = async {
            let mut tx = st.pool.begin().await.map_err(map_db_error)?;
            let ticket = lock_ticket(&mut tx, t, ServiceTicketId(tid)).await?;
            if ticket.sla_breached || ticket.status == TicketStatus::OnHold {
                return Ok(());
            }
            let e = escalate(
                &mut tx,
                &scope,
                &ticket,
                "sla_breach",
                "SLA breached",
                &system,
            )
            .await?;
            audit(
                &mut tx,
                &scope,
                "campus.service_ticket.sla_breach",
                "service_ticket",
                tid,
                serde_json::json!({"number": e.number, "level": e.escalation_level}),
                &system,
            )
            .await?;
            tx.commit().await.map_err(map_db_error)?;
            Ok(())
        }
        .await;
        match res {
            Ok(()) => summary.processed += 1,
            Err(e) => summary.failed.push(FacilityBulkFailure {
                id: tid,
                reason: e.to_string(),
            }),
        }
    }
    Ok(Json(summary))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(ticket_create, ticket_list))
        .routes(routes!(ticket_get))
        .routes(routes!(my_ticket_create, my_ticket_list))
        .routes(routes!(my_ticket_confirm))
        .routes(routes!(ticket_triage))
        .routes(routes!(ticket_assign))
        .routes(routes!(ticket_start))
        .routes(routes!(ticket_hold))
        .routes(routes!(ticket_resolve))
        .routes(routes!(ticket_close))
        .routes(routes!(ticket_reopen))
        .routes(routes!(ticket_cancel))
        .routes(routes!(ticket_merge))
        .routes(routes!(ticket_escalate))
        .routes(routes!(ticket_sla_check))
        .with_state(state)
}
