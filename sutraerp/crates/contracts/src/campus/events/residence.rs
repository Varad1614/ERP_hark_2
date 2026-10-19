//! **8B** — hostel (applications, allocations / residency, resident leave, room transfers, inspections, mess) and
//! transport (routes, stops, vehicles, drivers, allocations, passes, trips) (L2-G03, G04; L3-G-078…116).
//!
//! Owned by slice 8B (`docs/batch8-contracts.md` §4.3). Payload: [`super::StatusChangedV1`] unless noted.

use super::event_type;

event_type!(
    /// Hostel admission decided (L3-G-078; `toStatus` approved / rejected / waitlisted).
    HOSTEL_APPLICATION_DECIDED, HOSTEL_APPLICATION_DECIDED_SCHEMA, "hostel_application.decided");
event_type!(
    /// Residency active: bed allocated (L3-G-079).
    HOSTEL_ALLOCATION_ACTIVATED, HOSTEL_ALLOCATION_ACTIVATED_SCHEMA, "hostel_allocation.activated");
event_type!(
    /// Residency ended, bed released (L3-G-081).
    HOSTEL_ALLOCATION_CLOSED, HOSTEL_ALLOCATION_CLOSED_SCHEMA, "hostel_allocation.closed");
event_type!(
    /// Resident leave approved / rejected (L3-G-082).
    RESIDENT_LEAVE_DECIDED, RESIDENT_LEAVE_DECIDED_SCHEMA, "resident_leave.decided");
event_type!(
    /// Room transfer effective (L3-G-087).
    ROOM_TRANSFER_COMPLETED, ROOM_TRANSFER_COMPLETED_SCHEMA, "room_transfer.completed");
event_type!(
    /// Inspection completed (damage charge raised when found; L3-G-089).
    ROOM_INSPECTION_COMPLETED, ROOM_INSPECTION_COMPLETED_SCHEMA, "room_inspection.completed");
event_type!(
    /// Route (version) active (L3-G-093 / 094).
    TRANSPORT_ROUTE_ACTIVATED, TRANSPORT_ROUTE_ACTIVATED_SCHEMA, "transport_route.activated");
event_type!(
    /// Route suspended (L3-G-095).
    TRANSPORT_ROUTE_SUSPENDED, TRANSPORT_ROUTE_SUSPENDED_SCHEMA, "transport_route.suspended");
event_type!(
    /// Passenger allocated to a route + stop (L3-G-099 / 100).
    TRANSPORT_ALLOCATION_ACTIVATED, TRANSPORT_ALLOCATION_ACTIVATED_SCHEMA, "transport_allocation.activated");
event_type!(
    /// Transport pass issued / renewed / replaced (L3-G-113…115).
    TRANSPORT_PASS_ISSUED, TRANSPORT_PASS_ISSUED_SCHEMA, "transport_pass.issued");
event_type!(
    /// Pass suspended (L3-G-116).
    TRANSPORT_PASS_SUSPENDED, TRANSPORT_PASS_SUSPENDED_SCHEMA, "transport_pass.suspended");
event_type!(
    /// Trip completed (L3-G-106).
    TRIP_COMPLETED, TRIP_COMPLETED_SCHEMA, "trip.completed");
event_type!(
    /// Trip cancelled (L3-G-105).
    TRIP_CANCELLED, TRIP_CANCELLED_SCHEMA, "trip.cancelled");

/// Event types of this file (keep in sync; `all_event_types` collects them).
pub const EVENT_TYPES: &[&str] = &[
    HOSTEL_APPLICATION_DECIDED,
    HOSTEL_ALLOCATION_ACTIVATED,
    HOSTEL_ALLOCATION_CLOSED,
    RESIDENT_LEAVE_DECIDED,
    ROOM_TRANSFER_COMPLETED,
    ROOM_INSPECTION_COMPLETED,
    TRANSPORT_ROUTE_ACTIVATED,
    TRANSPORT_ROUTE_SUSPENDED,
    TRANSPORT_ALLOCATION_ACTIVATED,
    TRANSPORT_PASS_ISSUED,
    TRANSPORT_PASS_SUSPENDED,
    TRIP_COMPLETED,
    TRIP_CANCELLED,
];
