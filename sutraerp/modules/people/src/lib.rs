//! # Domain E — People & Workforce module
//!
//! Owns PostgreSQL schema `people` and the `/api/v1/people/...` route space (prototype
//! navigation: Admin `people`, Faculty `leave`). Boundary rules (ADR-L5-0003): this crate
//! depends only on `sutra-kernel`, `sutra-contracts`, `sutra-web` and `sutra-persistence`; it
//! publishes its ports in `sutra_contracts::people` and never reads another module's schema
//! (ADR-L5-0036 §3.2). Domain B data comes through `InstitutionReferencePort`; Domain D teaching
//! load through `FacultyWorkloadPort`; Domain F payroll postings through `PayrollPostingPort`.
//! Domain D reads staff facts through [`PgStaffDirectory`] (`sutra_contracts::people::StaffDirectory`).
//! Money and leave days are `rust_decimal` end to end — never floats.
//!
//! ## Batch 7 layout (see `docs/batch7-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/`, `directory.rs`, `staff_port.rs`, `staff.rs`, `staff_directory/` | foundation (contract changes only) |
//! | `workforce/`, `positions/`, `hr/`, `profile/` | 7A |
//! | `leave/`, `attendance/`, `workload/` | 7B |
//! | `payroll/`, `payslips/` | 7C |

pub mod directory;
pub mod metrics; // Batch 8 foundation: Domain H figures
pub mod shared;
pub mod staff;
pub mod staff_port;

// ---- foundation area (staff directory, read-only)
pub mod staff_directory;

// ---- slice areas (router mount points are pre-registered in `router_with_state`) ----
pub mod attendance; // 7B
pub mod hr; // 7A
pub mod leave; // 7B
pub mod payroll; // 7C
pub mod payslips; // 7C
pub mod positions; // 7A
pub mod profile; // 7A
pub mod workforce; // 7A
pub mod workload; // 7B

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::academic::ports::FacultyWorkloadPort;
use sutra_contracts::finance::ports::PayrollPostingPort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use directory::{PgLeaveReader, PgWorkforceDirectory};
pub use metrics::PgPeopleMetrics;
pub use shared::PeopleState;
pub use staff::SubjectOnlyStaff;
pub use staff_port::PgStaffDirectory;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "E",
    key: "people",
    name: "People & Workforce",
    schema: "people",
    batch: 7,
    status: ModuleStatus::Active,
};

/// Adapters the composition root hands to Domain E.
#[derive(Clone)]
pub struct PeopleDeps {
    pub pool: PgPool,
    /// Approval steps (`NoopWorkflowAdapter` in Batch 7).
    pub workflow: Arc<dyn WorkflowPort>,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain D teaching load (`sutra_academic::PgFacultyWorkload`).
    pub workload: Arc<dyn FacultyWorkloadPort>,
    /// Domain F payroll postings (`sutra_finance::PgPayrollPostings`).
    pub postings: Arc<dyn PayrollPostingPort>,
}

impl PeopleDeps {
    /// Handler state with the Batch 7 defaults (auto-approving approval steps, the Postgres
    /// workforce directory / leave reader over the foundation tables).
    pub fn into_state(self) -> PeopleState {
        PeopleState {
            directory: Arc::new(PgWorkforceDirectory::new(self.pool.clone())),
            leave: Arc::new(PgLeaveReader::new(self.pool.clone())),
            pool: self.pool,
            workflow: self.workflow,
            approvals: shared::approval::ApprovalMode::AutoApprove,
            references: self.references,
            workload: self.workload,
            postings: self.postings,
        }
    }
}

/// All Domain E routes. Every slice router is already mounted here; slices fill in their own
/// `router` functions and never edit this list.
pub fn router(deps: PeopleDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision` or port
/// doubles).
pub fn router_with_state(st: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(staff_directory::router(st.clone())) // foundation
        .merge(workforce::router(st.clone())) // 7A
        .merge(positions::router(st.clone())) // 7A
        .merge(hr::router(st.clone())) // 7A
        .merge(profile::router(st.clone())) // 7A
        .merge(leave::router(st.clone())) // 7B
        .merge(attendance::router(st.clone())) // 7B
        .merge(workload::router(st.clone())) // 7B
        .merge(payroll::router(st.clone())) // 7C
        .merge(payslips::router(st)) // 7C
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "people");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 7);
    }
}
