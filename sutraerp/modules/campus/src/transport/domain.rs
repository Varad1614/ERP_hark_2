//! 8B transport state machines (contract §4.2; L2-G04). Illegal moves are `409`.

use crate::hostel::common::machine;
use chrono::NaiveDate;

machine!(
    /// Transport route (L3-G-093 … 095).
    RouteStatus, "transport route" {
        Draft => "draft",
        Active => "active",
        Suspended => "suspended",
        Retired => "retired",
    }
    moves [
        Draft -> Active,
        Active -> Suspended,
        Suspended -> Active,
        Draft -> Retired,
        Active -> Retired,
        Suspended -> Retired,
    ]
);

machine!(
    /// Effective-dated version of a route (L3-G-094); a version is never rewritten once active.
    RouteVersionStatus, "route version" {
        Draft => "draft",
        Active => "active",
        Superseded => "superseded",
    }
    moves [Draft -> Active, Active -> Superseded]
);

machine!(
    /// Pickup / drop stop (L3-G-096 / 097).
    StopStatus, "transport stop" {
        Active => "active",
        Suspended => "suspended",
        Retired => "retired",
    }
    moves [Active -> Suspended, Suspended -> Active, Active -> Retired, Suspended -> Retired]
);

machine!(
    /// Vehicle (L3-G-108 / 109).
    VehicleStatus, "vehicle" {
        Available => "available",
        Assigned => "assigned",
        UnderMaintenance => "under_maintenance",
        BrokenDown => "broken_down",
        Retired => "retired",
    }
    moves [
        Available -> Assigned,
        Assigned -> Available,
        Available -> UnderMaintenance,
        UnderMaintenance -> Available,
        Available -> BrokenDown,
        Assigned -> BrokenDown,
        BrokenDown -> UnderMaintenance,
        BrokenDown -> Available,
        Available -> Retired,
        UnderMaintenance -> Retired,
        BrokenDown -> Retired,
    ]
);

machine!(
    /// Driver assignment to a route (L3-G-102).
    DriverAssignmentStatus, "driver assignment" {
        Active => "active",
        Ended => "ended",
    }
    moves [Active -> Ended]
);

machine!(
    /// Passenger allocation (L3-G-099 … 101).
    TransportAllocationStatus, "transport allocation" {
        Requested => "requested",
        Active => "active",
        Changed => "changed",
        Withdrawn => "withdrawn",
    }
    moves [Requested -> Active, Active -> Changed, Requested -> Withdrawn, Active -> Withdrawn]
);

machine!(
    /// Transport pass (L3-G-113 … 116).
    PassStatus, "transport pass" {
        Issued => "issued",
        Active => "active",
        Expired => "expired",
        Suspended => "suspended",
        Replaced => "replaced",
        Cancelled => "cancelled",
    }
    moves [
        Issued -> Active,
        Issued -> Suspended,
        Active -> Suspended,
        Suspended -> Active,
        Issued -> Expired,
        Active -> Expired,
        Issued -> Replaced,
        Active -> Replaced,
        Suspended -> Replaced,
        Issued -> Cancelled,
        Active -> Cancelled,
        Suspended -> Cancelled,
    ]
);

machine!(
    /// Trip (L3-G-104 … 106).
    TripStatus, "trip" {
        Planned => "planned",
        Ready => "ready",
        InProgress => "in_progress",
        Completed => "completed",
        Cancelled => "cancelled",
    }
    moves [
        Planned -> Ready,
        Ready -> InProgress,
        InProgress -> Completed,
        Planned -> Cancelled,
        Ready -> Cancelled,
    ]
);

/// Status of a newly issued pass: `active` once its validity has started, else `issued`.
pub fn pass_status_on_issue(valid_from: NaiveDate, today: NaiveDate) -> PassStatus {
    if valid_from <= today {
        PassStatus::Active
    } else {
        PassStatus::Issued
    }
}

/// A readiness checklist is complete when it has at least one item and every item is `true`.
pub fn checklist_complete(checklist: &serde_json::Value) -> bool {
    checklist
        .as_object()
        .is_some_and(|m| !m.is_empty() && m.values().all(|v| v.as_bool() == Some(true)))
}

/// `true` when a route / vehicle with `capacity` seats can take `wanted` passengers.
pub fn seats_available(capacity: i32, used: i64, wanted: i64) -> bool {
    used + wanted <= i64::from(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_machines() {
        assert!(RouteStatus::Suspended.can_transition(RouteStatus::Active));
        assert!(!RouteStatus::Retired.can_transition(RouteStatus::Active));
        assert!(RouteVersionStatus::Active.can_transition(RouteVersionStatus::Superseded));
        assert!(!RouteVersionStatus::Superseded.can_transition(RouteVersionStatus::Active));
        assert!(VehicleStatus::Assigned.can_transition(VehicleStatus::BrokenDown));
        assert!(!VehicleStatus::Assigned.can_transition(VehicleStatus::Retired));
        assert!(PassStatus::Suspended.can_transition(PassStatus::Active));
        assert!(!PassStatus::Replaced.can_transition(PassStatus::Active));
        assert!(TripStatus::Ready.can_transition(TripStatus::InProgress));
        assert!(!TripStatus::Completed.can_transition(TripStatus::Cancelled));
        assert!(
            TransportAllocationStatus::Active.can_transition(TransportAllocationStatus::Changed)
        );
        for s in TripStatus::ALL {
            assert_eq!(TripStatus::parse(s.as_str()), Some(*s));
        }
        assert_eq!(TripStatus::InProgress.as_str(), "in_progress");
    }

    #[test]
    fn pass_checklist_and_capacity_rules() {
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default();
        assert_eq!(
            pass_status_on_issue(d("2026-11-01"), d("2026-11-02")),
            PassStatus::Active
        );
        assert_eq!(
            pass_status_on_issue(d("2026-12-01"), d("2026-11-02")),
            PassStatus::Issued
        );
        assert!(checklist_complete(
            &serde_json::json!({"brakes": true, "tyres": true})
        ));
        assert!(!checklist_complete(
            &serde_json::json!({"brakes": true, "tyres": false})
        ));
        assert!(!checklist_complete(&serde_json::json!({})));
        assert!(seats_available(40, 39, 1));
        assert!(!seats_available(40, 40, 1));
    }
}
