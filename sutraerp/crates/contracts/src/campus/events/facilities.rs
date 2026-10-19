//! **8A** — infrastructure hierarchy, space reservations, labs, maintenance (work orders, preventive plans) and the
//! service desk ticket lifecycle (L2-G01, G02, G10; L3-G-016…026, 041, 042, 046, 050…060, 278…293, 315).
//!
//! Owned by slice 8A: add events here (constant + `EVENT_TYPES` entry) and list them in
//! `docs/batch8-contracts.md` §4.3. Payload: [`super::StatusChangedV1`] unless noted.

use super::event_type;

event_type!(
    /// Infrastructure node active (L3-G-278 / 282 / 283 / 284).
    INFRASTRUCTURE_NODE_ACTIVATED, INFRASTRUCTURE_NODE_ACTIVATED_SCHEMA, "infrastructure_node.activated");
event_type!(
    /// Node suspended (L3-G-290).
    INFRASTRUCTURE_NODE_SUSPENDED, INFRASTRUCTURE_NODE_SUSPENDED_SCHEMA, "infrastructure_node.suspended");
event_type!(
    /// Node restored (L3-G-290).
    INFRASTRUCTURE_NODE_RESTORED, INFRASTRUCTURE_NODE_RESTORED_SCHEMA, "infrastructure_node.restored");
event_type!(
    /// Node decommissioned (L3-G-315).
    INFRASTRUCTURE_NODE_DECOMMISSIONED, INFRASTRUCTURE_NODE_DECOMMISSIONED_SCHEMA, "infrastructure_node.decommissioned");
event_type!(
    /// Attributes / classification / capacity changed (payload `ModifiedV1`).
    INFRASTRUCTURE_NODE_MODIFIED, INFRASTRUCTURE_NODE_MODIFIED_SCHEMA, "infrastructure_node.modified");
event_type!(
    /// Reservation confirmed (L3-G-291).
    SPACE_RESERVATION_CONFIRMED, SPACE_RESERVATION_CONFIRMED_SCHEMA, "space_reservation.confirmed");
event_type!(
    /// Reservation moved (L3-G-292).
    SPACE_RESERVATION_RESCHEDULED, SPACE_RESERVATION_RESCHEDULED_SCHEMA, "space_reservation.rescheduled");
event_type!(
    /// Reservation cancelled (L3-G-293).
    SPACE_RESERVATION_CANCELLED, SPACE_RESERVATION_CANCELLED_SCHEMA, "space_reservation.cancelled");
event_type!(
    /// Ticket assigned to a technician / team (L3-G-016 / 022).
    SERVICE_TICKET_ASSIGNED, SERVICE_TICKET_ASSIGNED_SCHEMA, "service_ticket.assigned");
event_type!(
    /// Ticket resolved, awaiting verification (L3-G-017).
    SERVICE_TICKET_RESOLVED, SERVICE_TICKET_RESOLVED_SCHEMA, "service_ticket.resolved");
event_type!(
    /// Ticket closed (L3-G-017 / 046).
    SERVICE_TICKET_CLOSED, SERVICE_TICKET_CLOSED_SCHEMA, "service_ticket.closed");
event_type!(
    /// SLA breach / escalation (L3-G-041 / 042).
    SERVICE_TICKET_ESCALATED, SERVICE_TICKET_ESCALATED_SCHEMA, "service_ticket.escalated");
event_type!(
    /// Work order completed and verified (L3-G-020 / 026).
    WORK_ORDER_COMPLETED, WORK_ORDER_COMPLETED_SCHEMA, "work_order.completed");
event_type!(
    /// Preventive plan active (L3-G-023).
    MAINTENANCE_PLAN_ACTIVATED, MAINTENANCE_PLAN_ACTIVATED_SCHEMA, "maintenance_plan.activated");
event_type!(
    /// Practical session completed (L3-G-053).
    PRACTICAL_SESSION_COMPLETED, PRACTICAL_SESSION_COMPLETED_SCHEMA, "practical_session.completed");

/// Event types of this file (keep in sync; `all_event_types` collects them).
pub const EVENT_TYPES: &[&str] = &[
    INFRASTRUCTURE_NODE_ACTIVATED,
    INFRASTRUCTURE_NODE_SUSPENDED,
    INFRASTRUCTURE_NODE_RESTORED,
    INFRASTRUCTURE_NODE_DECOMMISSIONED,
    INFRASTRUCTURE_NODE_MODIFIED,
    SPACE_RESERVATION_CONFIRMED,
    SPACE_RESERVATION_RESCHEDULED,
    SPACE_RESERVATION_CANCELLED,
    SERVICE_TICKET_ASSIGNED,
    SERVICE_TICKET_RESOLVED,
    SERVICE_TICKET_CLOSED,
    SERVICE_TICKET_ESCALATED,
    WORK_ORDER_COMPLETED,
    MAINTENANCE_PLAN_ACTIVATED,
    PRACTICAL_SESSION_COMPLETED,
];
