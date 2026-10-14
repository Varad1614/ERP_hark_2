//! Lifecycle state and classification enums of the Batch 8 Domain G (Campus operations)
//! aggregates.
//!
//! Foundation-owned. Wire/DB form is the `snake_case` name, stored in a `text` column with a
//! `CHECK (… IN (...))` written by the owning migration. Adding a variant is a contract change
//! (`docs/batch8-contracts.md` §4.2).
//!
//! The **shared** machines live in foundation tables several slices read and gate on, so their
//! legal transitions are written here (`can_transition`) and only the owning slice moves them
//! through `sutra_campus::shared`:
//!
//! | Machine | Table | Moved by |
//! |---------|-------|----------|
//! | [`InfrastructureStatus`] | `campus.infrastructure_node` | 8A (`shared::infrastructure::change_node_status`) |
//! | [`TicketStatus`] | `campus.service_ticket` | 8A (`shared::tickets::change_ticket_status`); every slice may **open** tickets |
//!
//! Every other machine's transitions are slice-owned (`domain.rs` of the slice); an illegal
//! transition is `409 conflict`.

/// Declares a status enum with `ALL`, `as_str`, `parse` and serde/OpenAPI `snake_case` names.
macro_rules! status_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

// ================================================================ foundation: shared machines

status_enum!(
    /// Level of a node in the physical infrastructure hierarchy (L2-G10).
    InfrastructureNodeType {
        /// Physical campus (linked to a Domain B campus).
        Site => "site",
        Building => "building",
        Floor => "floor",
        /// Room / hall / lab / hostel room … (bookable when active).
        Space => "space",
        /// Grouping of spaces (wing, block, outdoor zone).
        Zone => "zone",
    }
);

impl InfrastructureNodeType {
    /// Node types that may be the parent of `self` (`None` = no parent allowed).
    pub fn allowed_parents(self) -> &'static [InfrastructureNodeType] {
        use InfrastructureNodeType::*;
        match self {
            Site => &[],
            Building => &[Site, Zone],
            Floor => &[Building],
            Space => &[Floor, Building, Zone],
            Zone => &[Site, Building, Floor],
        }
    }
}

status_enum!(
    /// Classification of a space (L3-G-286). Only `space` nodes carry one.
    SpaceType {
        Classroom => "classroom",
        Lab => "lab",
        SeminarRoom => "seminar_room",
        Auditorium => "auditorium",
        Office => "office",
        HostelRoom => "hostel_room",
        Library => "library",
        ReadingRoom => "reading_room",
        Store => "store",
        Workshop => "workshop",
        Sports => "sports",
        Other => "other",
    }
);

impl SpaceType {
    /// Spaces Domain D may timetable (`TeachingSpacePort`).
    pub fn is_teaching(self) -> bool {
        matches!(
            self,
            Self::Classroom | Self::Lab | Self::SeminarRoom | Self::Auditorium
        )
    }
}

status_enum!(
    /// Infrastructure node lifecycle (L2-G10 §lifecycle, L3-G-290 / 314 / 315 / 316).
    InfrastructureStatus {
        Planned => "planned",
        Active => "active",
        /// Temporarily unavailable (maintenance, compliance restriction).
        Suspended => "suspended",
        /// Historical: no new reservations or allocations.
        Decommissioned => "decommissioned",
        Archived => "archived",
    }
);

impl InfrastructureStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use InfrastructureStatus::*;
        matches!(
            (self, to),
            (Planned, Active)
                | (Planned, Archived)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Active, Decommissioned)
                | (Suspended, Decommissioned)
                | (Decommissioned, Archived)
        )
    }

    /// Reservations / allocations / tickets may target the node.
    pub fn is_operational(self) -> bool {
        matches!(self, Self::Active)
    }
}

status_enum!(
    /// Service desk ticket lifecycle (L2-G01 service request / complaint, L3-G-016 / 017 / 018).
    TicketStatus {
        Open => "open",
        Triaged => "triaged",
        Assigned => "assigned",
        InProgress => "in_progress",
        OnHold => "on_hold",
        Resolved => "resolved",
        /// Verified closure (requester confirmed or auto-closed).
        Closed => "closed",
        Reopened => "reopened",
        Cancelled => "cancelled",
        /// Duplicate consolidated onto `canonical_ticket_id` (L3-G-018).
        Merged => "merged",
    }
);

impl TicketStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use TicketStatus::*;
        matches!(
            (self, to),
            (Open, Triaged)
                | (Open, Assigned)
                | (Triaged, Assigned)
                | (Assigned, InProgress)
                | (Assigned, OnHold)
                | (InProgress, OnHold)
                | (OnHold, InProgress)
                | (OnHold, Assigned)
                | (InProgress, Resolved)
                | (Assigned, Resolved)
                | (Resolved, Closed)
                | (Resolved, Reopened)
                | (Reopened, Assigned)
                | (Reopened, InProgress)
                | (Open | Triaged | Assigned | Reopened, Cancelled)
                | (Open | Triaged | Assigned | OnHold | Reopened, Merged)
        )
    }

    /// Not closed / cancelled / merged.
    pub fn is_open(self) -> bool {
        !matches!(self, Self::Closed | Self::Cancelled | Self::Merged)
    }
}

status_enum!(
    /// What a ticket is about (routing + SLA defaults).
    TicketCategory {
        Facility => "facility",
        Maintenance => "maintenance",
        Hostel => "hostel",
        Transport => "transport",
        Library => "library",
        Laboratory => "laboratory",
        It => "it",
        Other => "other",
    }
);

status_enum!(
    /// Ticket priority; drives the default SLA.
    TicketPriority {
        Low => "low",
        Medium => "medium",
        High => "high",
        Critical => "critical",
    }
);

impl TicketPriority {
    /// Default resolution SLA in hours (L3-G-041; a policy may override).
    pub fn default_sla_hours(self) -> i64 {
        match self {
            Self::Critical => 4,
            Self::High => 24,
            Self::Medium => 72,
            Self::Low => 168,
        }
    }
}

status_enum!(
    /// Which part of Campus opened a ticket.
    TicketSource {
        /// Student / staff self-service (`campus.self`).
        SelfService => "self_service",
        /// Facilities office on behalf of someone.
        Admin => "admin",
        Hostel => "hostel",
        Transport => "transport",
        Library => "library",
        Laboratory => "laboratory",
        /// Generated from a preventive maintenance plan.
        Preventive => "preventive",
    }
);

status_enum!(
    /// Kind of person a campus record belongs to (resident, passenger, borrower, requester).
    PersonKind {
        /// Domain C student (`StudentDirectory`).
        Student => "student",
        /// Domain E staff member (`WorkforceDirectory`).
        Staff => "staff",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_roundtrip() {
        for s in InfrastructureStatus::ALL {
            assert_eq!(InfrastructureStatus::parse(s.as_str()), Some(*s));
        }
        for s in TicketStatus::ALL {
            assert_eq!(TicketStatus::parse(s.as_str()), Some(*s));
        }
        assert_eq!(SpaceType::parse("hostel_room"), Some(SpaceType::HostelRoom));
        assert_eq!(PersonKind::parse("staff"), Some(PersonKind::Staff));
    }

    #[test]
    fn shared_machines() {
        use InfrastructureStatus as I;
        assert!(I::Planned.can_transition(I::Active));
        assert!(I::Active.can_transition(I::Suspended) && I::Suspended.can_transition(I::Active));
        assert!(!I::Decommissioned.can_transition(I::Active));
        assert!(I::Active.is_operational() && !I::Suspended.is_operational());
        use TicketStatus as T;
        assert!(T::Open.can_transition(T::Assigned));
        assert!(T::Resolved.can_transition(T::Reopened));
        assert!(!T::Closed.can_transition(T::Open));
        assert!(T::Open.can_transition(T::Merged) && !T::Merged.is_open());
        assert_eq!(TicketPriority::Critical.default_sla_hours(), 4);
        assert!(SpaceType::Lab.is_teaching() && !SpaceType::HostelRoom.is_teaching());
        assert!(InfrastructureNodeType::Floor
            .allowed_parents()
            .contains(&InfrastructureNodeType::Building));
        assert!(InfrastructureNodeType::Site.allowed_parents().is_empty());
    }
}
