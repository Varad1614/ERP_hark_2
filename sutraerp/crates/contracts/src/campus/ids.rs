//! Typed UUIDv7 ids of the Batch 8 Domain G (Campus operations) aggregates (ADR-L5-0036 §3.3).
//!
//! Foundation-owned and **append-only**: a slice that needs a new id adds it at the end of its
//! own block with a `// 8X:` marker; existing ids are never renamed (`docs/batch8-contracts.md`
//! §4.1).

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- foundation: shared Campus aggregates ----
typed_id!(
    /// Node of the physical infrastructure hierarchy: site → building → floor → space / zone (L2-G10).
    InfrastructureNodeId
);
typed_id!(
    /// Service desk ticket: service request or complaint from any campus service (L2-G01 / G03 / G04 / G05).
    ServiceTicketId
);

// ---- 8A: infrastructure, rooms, labs, maintenance, service desk ----
typed_id!(
    /// Capability of a space (projector, fume hood, wheelchair access …) (L3-G-287).
    SpaceCapabilityId
);
typed_id!(
    /// Availability profile of a space: bookable hours / days (L3-G-289).
    AvailabilityProfileId
);
typed_id!(
    /// Reservation (booking) of a space for a time window (L3-G-291…293).
    SpaceReservationId
);
typed_id!(
    /// Maintenance work order raised from a ticket or a preventive plan (L3-G-020).
    WorkOrderId
);
typed_id!(
    /// Preventive maintenance plan (L3-G-023).
    MaintenancePlanId
);
typed_id!(
    /// Laboratory profile of a lab space (L2-G02).
    LabId
);
typed_id!(
    /// Equipment of a laboratory (L3-G-060).
    LabEquipmentId
);
typed_id!(
    /// Practical session scheduled in a laboratory (L3-G-053).
    PracticalSessionId
);
typed_id!(
    /// Allocation of a student batch to a laboratory (L3-G-057).
    LabBatchAllocationId
);

// ---- 8B: hostel and transport ----
typed_id!(
    /// Hostel residency policy version (fees, curfew, eligibility) (L3-G-092).
    HostelPolicyId
);
typed_id!(
    /// Bed of a hostel room (a `hostel_room` space) (L2-G03).
    HostelBedId
);
typed_id!(
    /// Hostel admission application (L3-G-078).
    HostelApplicationId
);
typed_id!(
    /// Residency: allocation of a bed to a resident (L3-G-079 / 081).
    HostelAllocationId
);
typed_id!(
    /// Resident leave / out-pass (L3-G-082).
    ResidentLeaveId
);
typed_id!(
    /// Room transfer request (L3-G-087).
    RoomTransferId
);
typed_id!(
    /// Hostel room inspection (L3-G-089).
    RoomInspectionId
);
typed_id!(
    /// Mess (dining hall) (L3-G-090).
    MessId
);
typed_id!(
    /// Mess allocation of a resident (L3-G-090 / 091).
    MessAllocationId
);
typed_id!(
    /// Transport route (L3-G-093).
    TransportRouteId
);
typed_id!(
    /// Effective-dated version of a route with its ordered stops (L3-G-094).
    RouteVersionId
);
typed_id!(
    /// Pickup / drop stop (L3-G-096).
    TransportStopId
);
typed_id!(
    /// Transport vehicle (L3-G-108).
    VehicleId
);
typed_id!(
    /// Driver assigned to a route / trip (L3-G-102).
    DriverAssignmentId
);
typed_id!(
    /// Allocation of a passenger to a route + stop (L3-G-099).
    TransportAllocationId
);
typed_id!(
    /// Transport pass (L3-G-113).
    TransportPassId
);
typed_id!(
    /// Planned / executed trip of a route (L3-G-104 / 106).
    TripId
);

// ---- 8C: library ----
typed_id!(
    /// Library borrowing policy version (limits, loan days, fine rates) (L3-G-153).
    LibraryPolicyId
);
typed_id!(
    /// Library membership of a student or staff member (L3-G-124).
    LibraryMembershipId
);
typed_id!(
    /// Bibliographic catalogue record (L3-G-127).
    CatalogueRecordId
);
typed_id!(
    /// Physical copy (accession) of a catalogue record (L3-G-129).
    LibraryCopyId
);
typed_id!(
    /// Borrowing (issue → return) of a copy (L3-G-131).
    LibraryLoanId
);
typed_id!(
    /// Reservation / hold on a catalogue record (L3-G-134).
    LibraryReservationId
);
typed_id!(
    /// Overdue / lost-copy fine (L3-G-137 / 139).
    LibraryFineId
);
typed_id!(
    /// Library clearance request (L3-G-149).
    LibraryClearanceId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_uuid_v7_and_transparent() {
        let id = InfrastructureNodeId::new();
        assert_eq!(id.as_uuid().get_version_num(), 7);
        let json = serde_json::to_string(&id).unwrap_or_default();
        assert_eq!(json, format!("\"{id}\""));
    }
}
