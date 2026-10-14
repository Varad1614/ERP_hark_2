//! 8B hostel state machines (contract §4.2; L2-G03). Illegal moves are `409`.

use super::common::machine;

machine!(
    /// Hostel residency policy version (L3-G-092).
    HostelPolicyStatus, "hostel policy" {
        Draft => "draft",
        Approved => "approved",
        Active => "active",
        Superseded => "superseded",
    }
    moves [Draft -> Approved, Approved -> Draft, Approved -> Active, Active -> Superseded]
);

machine!(
    /// Hostel admission application (L3-G-078 / 080).
    HostelApplicationStatus, "hostel application" {
        Submitted => "submitted",
        UnderReview => "under_review",
        Approved => "approved",
        Waitlisted => "waitlisted",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
        Allocated => "allocated",
    }
    moves [
        Submitted -> UnderReview,
        UnderReview -> Approved,
        UnderReview -> Rejected,
        Approved -> Allocated,
        Approved -> Waitlisted,
        Waitlisted -> Approved,
        Waitlisted -> Allocated,
        Submitted -> Withdrawn,
        UnderReview -> Withdrawn,
        Approved -> Withdrawn,
        Waitlisted -> Withdrawn,
    ]
);

machine!(
    /// Residency = bed allocation (L3-G-079 / 081).
    HostelAllocationStatus, "hostel allocation" {
        Active => "active",
        OnLeave => "on_leave",
        TransferPending => "transfer_pending",
        Vacating => "vacating",
        Closed => "closed",
    }
    moves [
        Active -> OnLeave,
        OnLeave -> Active,
        Active -> TransferPending,
        TransferPending -> Active,
        Active -> Vacating,
        Vacating -> Closed,
    ]
);

machine!(
    /// Resident leave / out-pass (L3-G-082).
    ResidentLeaveStatus, "resident leave" {
        Requested => "requested",
        Approved => "approved",
        Rejected => "rejected",
        Out => "out",
        Returned => "returned",
        Overdue => "overdue",
    }
    moves [
        Requested -> Approved,
        Requested -> Rejected,
        Approved -> Out,
        Out -> Returned,
        Out -> Overdue,
        Overdue -> Returned,
    ]
);

machine!(
    /// Room transfer (L3-G-087).
    RoomTransferStatus, "room transfer" {
        Requested => "requested",
        Approved => "approved",
        Rejected => "rejected",
        Completed => "completed",
    }
    moves [Requested -> Approved, Requested -> Rejected, Approved -> Completed]
);

machine!(
    /// Room inspection (L3-G-089).
    InspectionStatus, "room inspection" {
        Scheduled => "scheduled",
        Completed => "completed",
        DamageFound => "damage_found",
        Charged => "charged",
        Closed => "closed",
    }
    moves [
        Scheduled -> Completed,
        Scheduled -> DamageFound,
        DamageFound -> Charged,
        Completed -> Closed,
        DamageFound -> Closed,
        Charged -> Closed,
    ]
);

machine!(
    /// Bed of a hostel room.
    BedStatus, "hostel bed" {
        Available => "available",
        Occupied => "occupied",
        OutOfService => "out_of_service",
    }
    moves [
        Available -> Occupied,
        Occupied -> Available,
        Available -> OutOfService,
        OutOfService -> Available,
    ]
);

machine!(
    /// Mess allocation (L3-G-090 / 091).
    MessAllocationStatus, "mess allocation" {
        Active => "active",
        Changed => "changed",
        Ended => "ended",
    }
    moves [Active -> Changed, Active -> Ended]
);

/// Application states that still hold a place in the queue.
pub const OPEN_APPLICATION: &[HostelApplicationStatus] = &[
    HostelApplicationStatus::Submitted,
    HostelApplicationStatus::UnderReview,
    HostelApplicationStatus::Approved,
    HostelApplicationStatus::Waitlisted,
];

/// `true` when a returning resident is late (returned after the last leave day).
pub fn returned_late(to_date: chrono::NaiveDate, today: chrono::NaiveDate) -> bool {
    today > to_date
}

/// `true` when a room with `capacity` beds can take one more bed.
pub fn bed_fits(existing: i64, capacity: Option<i32>) -> bool {
    capacity.is_some_and(|c| existing < i64::from(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    #[test]
    fn hostel_machines() {
        use HostelApplicationStatus as A;
        assert!(A::UnderReview.can_transition(A::Approved));
        assert!(A::Waitlisted.can_transition(A::Allocated));
        assert!(!A::Rejected.can_transition(A::Approved));
        assert!(!A::Allocated.can_transition(A::Withdrawn));
        use HostelAllocationStatus as R;
        assert!(R::Vacating.can_transition(R::Closed));
        assert!(!R::Active.can_transition(R::Closed));
        assert!(HostelPolicyStatus::Approved.can_transition(HostelPolicyStatus::Active));
        assert!(!HostelPolicyStatus::Draft.can_transition(HostelPolicyStatus::Active));
        assert!(InspectionStatus::DamageFound.can_transition(InspectionStatus::Charged));
        assert!(ResidentLeaveStatus::Overdue.can_transition(ResidentLeaveStatus::Returned));
        assert_eq!(A::parse("under_review"), Some(A::UnderReview));
        assert_eq!(OPEN_APPLICATION.len(), 4);
        for s in A::ALL {
            assert_eq!(A::parse(s.as_str()), Some(*s));
        }
    }

    #[test]
    fn late_return_and_bed_capacity() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default();
        assert!(returned_late(d("2026-11-10"), d("2026-11-11")));
        assert!(!returned_late(d("2026-11-10"), d("2026-11-10")));
        assert!(bed_fits(1, Some(2)));
        assert!(!bed_fits(2, Some(2)));
        assert!(!bed_fits(0, None));
    }
}
