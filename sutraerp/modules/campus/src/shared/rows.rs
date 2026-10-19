//! Row decoders and locks of the foundation tables (`campus.infrastructure_node`,
//! `campus.service_ticket`). Used by the foundation API and by the slices' own queries, so every
//! slice decodes the shared rows the same way.

use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::ids::{InfrastructureNodeId, ServiceTicketId};
use sutra_contracts::campus::status::{
    InfrastructureNodeType, InfrastructureStatus, PersonKind, SpaceType, TicketCategory,
    TicketPriority, TicketSource, TicketStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Strict decode of a stored status / classification name.
pub fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown value `{raw}`").into()))
}

/// Optional variant of [`decode`].
pub fn decode_opt<T>(
    raw: Option<String>,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, sqlx::Error> {
    raw.map(|r| decode(r, parse)).transpose()
}

// ------------------------------------------------------------------ infrastructure nodes

/// One node of the physical infrastructure hierarchy (site → building → floor → space / zone).
#[derive(Clone, Debug, PartialEq)]
pub struct InfrastructureNode {
    pub id: InfrastructureNodeId,
    pub institution_id: InstitutionId,
    /// Domain B campus.
    pub campus_id: Uuid,
    pub parent_id: Option<InfrastructureNodeId>,
    pub node_type: InfrastructureNodeType,
    pub code: String,
    pub name: String,
    /// Only `space` nodes carry one.
    pub space_type: Option<SpaceType>,
    pub capacity: Option<i32>,
    pub floor_number: Option<i32>,
    pub area_sq_m: Option<Decimal>,
    pub attributes: serde_json::Value,
    pub status: InfrastructureStatus,
    pub status_reason: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

pub const NODE_COLUMNS: &str = "id, institution_id, campus_id, parent_id, node_type, code, name, \
    space_type, capacity, floor_number, area_sq_m, attributes, status, status_reason, \
    effective_from, process_instance_id, version";

pub fn node_from_row(r: &PgRow) -> Result<InfrastructureNode, sqlx::Error> {
    Ok(InfrastructureNode {
        id: InfrastructureNodeId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        campus_id: r.try_get("campus_id")?,
        parent_id: r
            .try_get::<Option<Uuid>, _>("parent_id")?
            .map(InfrastructureNodeId),
        node_type: decode(r.try_get("node_type")?, InfrastructureNodeType::parse)?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        space_type: decode_opt(r.try_get("space_type")?, SpaceType::parse)?,
        capacity: r.try_get("capacity")?,
        floor_number: r.try_get("floor_number")?,
        area_sq_m: r.try_get("area_sq_m")?,
        attributes: r.try_get("attributes")?,
        status: decode(r.try_get("status")?, InfrastructureStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        effective_from: r.try_get("effective_from")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

/// Reads a node (`None` when missing / foreign).
pub async fn load_node(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: InfrastructureNodeId,
) -> Result<Option<InfrastructureNode>, KernelError> {
    let sql = format!(
        "SELECT {NODE_COLUMNS} FROM campus.infrastructure_node WHERE id = $1 AND tenant_id = $2"
    );
    sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(node_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// Locks a node `FOR UPDATE` inside the caller's transaction (404 when missing / foreign).
pub async fn lock_node(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: InfrastructureNodeId,
) -> Result<InfrastructureNode, KernelError> {
    let sql = format!(
        "SELECT {NODE_COLUMNS} FROM campus.infrastructure_node WHERE id = $1 AND tenant_id = $2 \
         FOR UPDATE"
    );
    sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(node_from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "infrastructure node",
        })
}

// ------------------------------------------------------------------ service tickets

/// A service desk ticket (service request or complaint).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceTicket {
    pub id: ServiceTicketId,
    pub institution_id: InstitutionId,
    pub number: String,
    pub category: TicketCategory,
    pub source: TicketSource,
    pub source_ref_id: Option<Uuid>,
    pub is_complaint: bool,
    pub node_id: Option<InfrastructureNodeId>,
    pub requester_kind: Option<PersonKind>,
    pub requester_id: Option<Uuid>,
    pub requester_subject: Option<String>,
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

pub const TICKET_COLUMNS: &str = "id, institution_id, number, category, source, source_ref_id, \
    is_complaint, node_id, requester_kind, requester_id, requester_subject, title, description, \
    priority, status, status_reason, assignee, sla_due_at, sla_breached, escalation_level, \
    resolved_at, closed_at, resolution, canonical_ticket_id, created_at, version";

pub fn ticket_from_row(r: &PgRow) -> Result<ServiceTicket, sqlx::Error> {
    Ok(ServiceTicket {
        id: ServiceTicketId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        number: r.try_get("number")?,
        category: decode(r.try_get("category")?, TicketCategory::parse)?,
        source: decode(r.try_get("source")?, TicketSource::parse)?,
        source_ref_id: r.try_get("source_ref_id")?,
        is_complaint: r.try_get("is_complaint")?,
        node_id: r
            .try_get::<Option<Uuid>, _>("node_id")?
            .map(InfrastructureNodeId),
        requester_kind: decode_opt(r.try_get("requester_kind")?, PersonKind::parse)?,
        requester_id: r.try_get("requester_id")?,
        requester_subject: r.try_get("requester_subject")?,
        title: r.try_get("title")?,
        description: r.try_get("description")?,
        priority: decode(r.try_get("priority")?, TicketPriority::parse)?,
        status: decode(r.try_get("status")?, TicketStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        assignee: r.try_get("assignee")?,
        sla_due_at: r.try_get("sla_due_at")?,
        sla_breached: r.try_get("sla_breached")?,
        escalation_level: r.try_get("escalation_level")?,
        resolved_at: r.try_get("resolved_at")?,
        closed_at: r.try_get("closed_at")?,
        resolution: r.try_get("resolution")?,
        canonical_ticket_id: r
            .try_get::<Option<Uuid>, _>("canonical_ticket_id")?
            .map(ServiceTicketId),
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    })
}

/// Reads a ticket (`None` when missing / foreign).
pub async fn load_ticket(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ServiceTicketId,
) -> Result<Option<ServiceTicket>, KernelError> {
    let sql = format!(
        "SELECT {TICKET_COLUMNS} FROM campus.service_ticket WHERE id = $1 AND tenant_id = $2"
    );
    sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(ticket_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// Locks a ticket `FOR UPDATE` inside the caller's transaction (404 when missing / foreign).
pub async fn lock_ticket(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ServiceTicketId,
) -> Result<ServiceTicket, KernelError> {
    let sql = format!(
        "SELECT {TICKET_COLUMNS} FROM campus.service_ticket WHERE id = $1 AND tenant_id = $2 \
         FOR UPDATE"
    );
    sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(ticket_from_row)
        .transpose()
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "service ticket",
        })
}
