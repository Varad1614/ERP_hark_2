//! Lifecycle state enums of the Batch 2 aggregates.
//!
//! Foundation-owned. Wire/DB form is the `snake_case` name ([`as_str`](InstitutionStatus::as_str)),
//! stored in a `text` column with a `CHECK (status IN (...))` constraint written by the owning
//! slice's migration. Allowed *transitions* are business rules and live in the owning slice's
//! domain code, not here. Adding a variant is a contract change (edit `docs/batch2-contracts.md`).

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

// ---- 2A ----
status_enum!(
    /// L2-B01 §6 institution lifecycle (+ activation-authority amendment 2026-08-24).
    InstitutionStatus {
        /// Registered; configuration incomplete.
        Draft => "draft",
        /// Mandatory configuration complete; ready to submit.
        Configured => "configured",
        /// Activation request awaiting the Institution Activation Authority.
        PendingApproval => "pending_approval",
        /// Returned for correction; editable, resubmits via Configured.
        Returned => "returned",
        /// Activation denied for this request cycle (no direct path to Active).
        Rejected => "rejected",
        Active => "active",
        Suspended => "suspended",
        /// Terminal, read-only.
        Archived => "archived",
    }
);
status_enum!(
    /// Institutional policy revision state.
    PolicyStatus {
        Draft => "draft",
        /// In force from its `effectiveFrom` business date.
        Effective => "effective",
        Superseded => "superseded",
    }
);
status_enum!(
    /// Organisation unit lifecycle (L2-B02).
    OrgUnitStatus {
        /// Registered, not yet operational.
        Draft => "draft",
        Active => "active",
        Suspended => "suspended",
        Decommissioned => "decommissioned",
        /// Terminal historical record.
        Archived => "archived",
    }
);
status_enum!(
    /// Effective-dated position definition lifecycle.
    PositionStatus {
        Draft => "draft",
        Active => "active",
        Retired => "retired",
        Archived => "archived",
    }
);

// ---- 2B ----
status_enum!(
    /// Institutional campus lifecycle (L2-B03).
    CampusStatus {
        /// Registered, not yet operational.
        Registered => "registered",
        /// Activation decision requested.
        PendingActivation => "pending_activation",
        Active => "active",
        Suspended => "suspended",
        Decommissioned => "decommissioned",
        /// Terminal historical record.
        Archived => "archived",
    }
);

// ---- shared by 2A (blueprints, governance), 2B (structure, programme, curriculum version,
// school model) and 2C (calendar version) ----
status_enum!(
    /// Governed publication lifecycle of versioned definitions. A Published version is
    /// **immutable**: a change creates a successor Draft that supersedes it on publication.
    PublicationStatus {
        Draft => "draft",
        /// Submitted to the approval step (WorkflowPort); auto-approved in Batch 2.
        InReview => "in_review",
        Published => "published",
        /// Replaced by a published successor version.
        Superseded => "superseded",
        /// Withdrawn from new use; history preserved.
        Retired => "retired",
    }
);

// ---- 2C ----
status_enum!(
    /// Academic year (open academic context) lifecycle (L2-B05).
    AcademicYearStatus {
        Planned => "planned",
        /// The open academic context downstream domains attach to.
        Open => "open",
        /// Closure in progress (carry-forward checks running).
        Closing => "closing",
        Closed => "closed",
    }
);
status_enum!(
    /// Calendar → governing-scope assignment lifecycle.
    CalendarAssignmentStatus {
        Draft => "draft",
        Effective => "effective",
        Retired => "retired",
    }
);
status_enum!(
    /// Calendar exception (temporary deviation) lifecycle.
    CalendarExceptionStatus {
        Requested => "requested",
        Approved => "approved",
        Rejected => "rejected",
        InEffect => "in_effect",
        Completed => "completed",
        Withdrawn => "withdrawn",
    }
);
status_enum!(
    /// Calendar change simulation lifecycle.
    SimulationStatus {
        Draft => "draft",
        Evaluated => "evaluated",
        Accepted => "accepted",
        Discarded => "discarded",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_roundtrip_and_match_serde() {
        for s in InstitutionStatus::ALL {
            assert_eq!(InstitutionStatus::parse(s.as_str()), Some(*s));
            assert_eq!(
                serde_json::to_value(s).unwrap_or_default(),
                serde_json::Value::String(s.as_str().into())
            );
        }
        for s in PublicationStatus::ALL {
            assert_eq!(PublicationStatus::parse(s.as_str()), Some(*s));
        }
        assert_eq!(CampusStatus::parse("bogus"), None);
        assert_eq!(
            serde_json::to_value(CalendarExceptionStatus::InEffect).unwrap_or_default(),
            "in_effect"
        );
    }
}
