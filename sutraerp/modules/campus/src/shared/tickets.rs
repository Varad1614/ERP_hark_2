//! The service desk ticket write paths (contract `docs/batch8-contracts.md` §3.3):
//!
//! * [`open_ticket`] — **every** Campus slice opens tickets only through it (8A self-service and
//!   admin requests, 8B hostel / transport complaints, 8C library complaints): numbering
//!   (`SR-000001`), SLA due time from the priority (`TicketPriority::default_sla_hours`) unless
//!   the caller passes one, history, the foundation event `service_ticket.opened` and audit, all
//!   in the caller's owner-local transaction.
//! * [`change_ticket_status`] — the **only** writer of `service_ticket.status` (8A), on a row the
//!   caller locked with `shared::rows::lock_ticket`; stamps `resolved_at` / `closed_at`.

use crate::shared::lifecycle::{illegal, record_history, HistoryEntry, StatusChange};
use crate::shared::me::PersonRef;
use crate::shared::numbering::{self, kind};
use crate::shared::publish;
use crate::shared::rows::{ticket_from_row, ServiceTicket, TICKET_COLUMNS};
use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_contracts::campus::events::{
    StatusChangedV1, SERVICE_TICKET_OPENED, SERVICE_TICKET_OPENED_SCHEMA,
};
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{TicketCategory, TicketPriority, TicketSource, TicketStatus};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// A ticket to open.
#[derive(Clone, Debug)]
pub struct NewTicket<'a> {
    pub institution: InstitutionId,
    pub category: TicketCategory,
    pub source: TicketSource,
    /// The slice aggregate behind the ticket (hostel allocation, route, copy …).
    pub source_ref_id: Option<Uuid>,
    /// Complaint (`true`) or service request.
    pub is_complaint: bool,
    /// Space / building concerned (must exist in the tenant).
    pub node_id: Option<InfrastructureNodeId>,
    pub requester: Option<&'a PersonRef>,
    pub title: &'a str,
    pub description: Option<&'a str>,
    pub priority: TicketPriority,
    /// Overrides the priority's default SLA (hours, ≥ 1).
    pub sla_hours: Option<i64>,
}

/// Pure checks of [`open_ticket`] (unit-tested); returns the SLA in hours.
pub fn validate_new_ticket(t: &NewTicket<'_>) -> Result<i64, KernelError> {
    if t.title.trim().is_empty() || t.title.len() > 200 {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "title",
            "invalid",
            "1–200 characters",
        )]));
    }
    if t.description.is_some_and(|d| d.len() > 4000) {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "description",
            "too_long",
            "at most 4000 characters",
        )]));
    }
    let hours = t
        .sla_hours
        .unwrap_or_else(|| t.priority.default_sla_hours());
    if !(1..=24 * 365).contains(&hours) {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "slaHours",
            "out_of_range",
            "between 1 hour and a year",
        )]));
    }
    Ok(hours)
}

/// Opens a ticket (see the module docs). `scope` is the institution scope of the ticket.
pub async fn open_ticket(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: NewTicket<'_>,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    let hours = validate_new_ticket(&t)?;
    let tenant = scope.tenant_id;
    if let Some(node) = t.node_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM campus.infrastructure_node WHERE id = $1 AND tenant_id = $2 \
             AND institution_id = $3)",
        )
        .bind(node.as_uuid())
        .bind(tenant.as_uuid())
        .bind(t.institution.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if !exists {
            return Err(KernelError::Validation(vec![FieldViolation::new(
                "nodeId",
                "unknown_node",
                "no infrastructure node with this id in the institution",
            )]));
        }
    }
    // ERP_EXTENSIONS 2B: without an explicit SLA, the institution's policy times the ticket.
    let minutes = match t.sla_hours {
        Some(h) => h * 60,
        None => {
            crate::servicedesk::automation::resolve_minutes(conn, tenant, t.institution, t.priority)
                .await?
        }
    };
    debug_assert!(hours > 0);
    let number = numbering::next(conn, tenant, t.institution, kind::SERVICE_TICKET).await?;
    let sql = format!(
        "INSERT INTO campus.service_ticket (tenant_id, institution_id, number, category, source, \
         source_ref_id, is_complaint, node_id, requester_kind, requester_id, requester_subject, \
         title, description, priority, sla_due_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, \
         $10, $11, $12, $13, $14, now() + make_interval(mins => $15)) RETURNING {TICKET_COLUMNS}"
    );
    let minutes_i32 = i32::try_from(minutes).unwrap_or(i32::MAX);
    let row = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(t.institution.as_uuid())
        .bind(&number)
        .bind(t.category.as_str())
        .bind(t.source.as_str())
        .bind(t.source_ref_id)
        .bind(t.is_complaint)
        .bind(t.node_id.map(|n| n.as_uuid()))
        .bind(t.requester.map(|p| p.kind.as_str()))
        .bind(t.requester.map(|p| p.id))
        .bind(t.requester.and_then(|p| p.subject_ref.clone()))
        .bind(t.title.trim())
        .bind(t.description)
        .bind(t.priority.as_str())
        .bind(minutes_i32)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let ticket = ticket_from_row(&row).map_err(map_db_error)?;
    let change = StatusChange {
        cause_event_type: SERVICE_TICKET_OPENED,
        reason: None,
        process_instance_id: None,
    };
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: t.institution,
            aggregate_type: "service_ticket",
            aggregate_id: ticket.id.as_uuid(),
            from: None,
            to: ticket.status.as_str(),
        },
        &change,
        actor,
    )
    .await?;
    publish::event(
        conn,
        scope,
        SERVICE_TICKET_OPENED,
        SERVICE_TICKET_OPENED_SCHEMA,
        ticket.id.to_string(),
        &StatusChangedV1 {
            aggregate_id: ticket.id.as_uuid(),
            institution_id: t.institution.as_uuid(),
            from_status: None,
            to_status: ticket.status.as_str().to_owned(),
            version: ticket.version,
            reason: Some(ticket.title.clone()),
            process_instance_id: None,
            number: Some(ticket.number.clone()),
            subject_ref: ticket.requester_subject.clone(),
        },
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        "campus.service_ticket.open",
        "service_ticket",
        ticket.id.to_string(),
        serde_json::json!({
            "number": ticket.number,
            "category": ticket.category.as_str(),
            "source": ticket.source.as_str(),
            "priority": ticket.priority.as_str(),
            "isComplaint": ticket.is_complaint,
        }),
        actor,
    )
    .await?;
    // ERP_EXTENSIONS 2B: the institution's service-desk triggers (created).
    crate::servicedesk::automation::after_event(
        conn,
        scope,
        ticket,
        None,
        crate::servicedesk::rules::Event::Created,
        actor,
    )
    .await
}

/// Moves a locked ticket to `to` (see the module docs).
pub async fn change_ticket_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    ticket: &ServiceTicket,
    to: TicketStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<ServiceTicket, KernelError> {
    if !ticket.status.can_transition(to) {
        return Err(illegal(
            "service ticket",
            &ticket.number,
            ticket.status.as_str(),
            to.as_str(),
        ));
    }
    let sql = format!(
        "UPDATE campus.service_ticket SET status = $3, status_reason = $4, \
         status_changed_at = now(), \
         resolved_at = CASE WHEN $3 = 'resolved' THEN now() WHEN $3 = 'reopened' THEN NULL \
                            ELSE resolved_at END, \
         closed_at = CASE WHEN $3 = 'closed' THEN now() ELSE closed_at END, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $5 RETURNING {TICKET_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(ticket.id.as_uuid())
        .bind(tenant.as_uuid())
        .bind(to.as_str())
        .bind(change.reason)
        .bind(ticket.version)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("service ticket changed concurrently".into()))?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: ticket.institution_id,
            aggregate_type: "service_ticket",
            aggregate_id: ticket.id.as_uuid(),
            from: Some(ticket.status.as_str()),
            to: to.as_str(),
        },
        change,
        actor,
    )
    .await?;
    ticket_from_row(&row).map_err(map_db_error)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(title: &str, sla: Option<i64>) -> NewTicket<'_> {
        NewTicket {
            institution: InstitutionId(Uuid::nil()),
            category: TicketCategory::Facility,
            source: TicketSource::SelfService,
            source_ref_id: None,
            is_complaint: true,
            node_id: None,
            requester: None,
            title,
            description: None,
            priority: TicketPriority::High,
            sla_hours: sla,
        }
    }

    #[test]
    fn sla_defaults_to_the_priority() {
        assert_eq!(validate_new_ticket(&t("Leaking tap", None)).ok(), Some(24));
        assert_eq!(
            validate_new_ticket(&t("Leaking tap", Some(2))).ok(),
            Some(2)
        );
        assert!(validate_new_ticket(&t("  ", None)).is_err());
        assert!(validate_new_ticket(&t("x", Some(0))).is_err());
    }
}
