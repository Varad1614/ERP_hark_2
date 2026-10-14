//! Typed UUIDv7 ids of the Batch 8 Domain H (Institutional services & intelligence) aggregates.
//!
//! Foundation-owned and **append-only** (`docs/batch8-contracts.md` §4.1). Batch 8 implements
//! H07 dashboards and H08 regulatory reporting (slice 8C) only.

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- 8C: dashboards (H07) ----
typed_id!(
    /// Governed dashboard (L3-H-048).
    DashboardId
);
typed_id!(
    /// Published version of a dashboard (L3-H-052).
    DashboardVersionId
);
typed_id!(
    /// Widget definition bound to an operational metric (L3-H-054).
    WidgetDefinitionId
);
typed_id!(
    /// Availability of a dashboard to a role / org context (L3-H-050).
    DashboardAssignmentId
);

// ---- 8C: regulatory reporting (H08) ----
typed_id!(
    /// Regulatory reporting definition (AISHE, NIRF, NAAC data …) (L3-H-055).
    ReportingFrameworkId
);
typed_id!(
    /// Reporting cycle with its collection plan (L3-H-056).
    ReportingCycleId
);
typed_id!(
    /// Validated data snapshot of a cycle (L3-H-057).
    ReportSnapshotId
);
typed_id!(
    /// Approved submission package (L3-H-058 / 059).
    SubmissionPackageId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_uuid_v7_and_transparent() {
        let id = DashboardId::new();
        assert_eq!(id.as_uuid().get_version_num(), 7);
        let json = serde_json::to_string(&id).unwrap_or_default();
        assert_eq!(json, format!("\"{id}\""));
    }
}
