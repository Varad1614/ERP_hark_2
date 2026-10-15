//! # Domain G — Campus operations module
//!
//! Owns PostgreSQL schema `campus` and the `/api/v1/campus/...` route space (prototype
//! navigation: Admin `campus` (Batch 8 tabs next to the Batch 2 campus setup), Student
//! `library`). Boundary rules (ADR-L5-0003): this crate depends only on `sutra-kernel`,
//! `sutra-contracts`, `sutra-web` and `sutra-persistence`; it publishes its ports in
//! `sutra_contracts::campus` and never reads another module's schema (ADR-L5-0036 §3.2).
//! Domain B data comes through `InstitutionReferencePort`; students through `StudentDirectory`;
//! staff through `WorkforceDirectory`; Finance charges through `CampusChargePort`. Domain D reads
//! teaching spaces through [`PgTeachingSpaces`]; Domain H reads figures through
//! [`PgCampusMetrics`]. Money is `rust_decimal` end to end — never floats.
//!
//! ## Batch 8 layout (see `docs/batch8-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/`, `directory/`, `spaces.rs`, `metrics.rs` | foundation (contract changes only) |
//! | `infrastructure/`, `reservations/`, `labs/`, `maintenance/`, `servicedesk/` | 8A |
//! | `hostel/`, `transport/` | 8B |
//! | `library/` | 8C |
//! | `governance/` | 9E (Batch 9, `docs/batch9-contracts.md`) |

pub mod metrics;
pub mod search_source; // 9C
pub mod shared;
pub mod spaces;

// ---- foundation area (infrastructure tree + campus person, read-only)
pub mod directory;

// ---- slice areas (router mount points are pre-registered in `router_with_state`) ----
pub mod governance; // 9E
pub mod hostel; // 8B
pub mod infrastructure; // 8A
pub mod labs; // 8A
pub mod library; // 8C
pub mod maintenance; // 8A
pub mod reservations; // 8A
pub mod servicedesk; // 8A
pub mod transport; // 8B

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::finance::ports::{CampusChargePort, ChargeDirectory};
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::people::ports::WorkforceDirectory;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::StudentDirectory;
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use metrics::PgCampusMetrics;
pub use shared::CampusState;
pub use spaces::{NoTeachingSpaces, PgTeachingSpaces};

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "G",
    key: "campus",
    name: "Campus",
    schema: "campus",
    batch: 8,
    status: ModuleStatus::Active,
};

/// Adapters the composition root hands to Domain G.
#[derive(Clone)]
pub struct CampusDeps {
    pub pool: PgPool,
    /// Approval steps (`NoopWorkflowAdapter` in Batch 8).
    pub workflow: Arc<dyn WorkflowPort>,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students (`sutra_student::PgStudentDirectory`).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain E staff records (`sutra_people::PgWorkforceDirectory`).
    pub workforce: Arc<dyn WorkforceDirectory>,
    /// Domain F campus charges (`sutra_finance::PgCampusCharges`).
    pub charges: Arc<dyn CampusChargePort>,
    /// Domain F charge reads (`sutra_finance::PgChargeDirectory`).
    pub finance: Arc<dyn ChargeDirectory>,
}

impl CampusDeps {
    /// Handler state with the Batch 8 defaults (auto-approving approval steps).
    pub fn into_state(self) -> CampusState {
        CampusState {
            pool: self.pool,
            workflow: self.workflow,
            approvals: shared::approval::ApprovalMode::AutoApprove,
            references: self.references,
            students: self.students,
            workforce: self.workforce,
            charges: self.charges,
            finance: self.finance,
        }
    }
}

/// All Domain G routes. Every slice router is already mounted here; slices fill in their own
/// `router` functions and never edit this list.
pub fn router(deps: CampusDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision` or port
/// doubles).
pub fn router_with_state(st: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(directory::router(st.clone())) // foundation
        .merge(infrastructure::router(st.clone())) // 8A
        .merge(reservations::router(st.clone())) // 8A
        .merge(labs::router(st.clone())) // 8A
        .merge(maintenance::router(st.clone())) // 8A
        .merge(servicedesk::router(st.clone())) // 8A
        .merge(hostel::router(st.clone())) // 8B
        .merge(transport::router(st.clone())) // 8B
        .merge(governance::router(st.clone())) // 9E
        .merge(library::router(st)) // 8C
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "campus");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 8);
    }
}
