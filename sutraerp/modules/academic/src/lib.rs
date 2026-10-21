//! # Domain D — Academic delivery module
//!
//! Owns PostgreSQL schema `academic` and the `/api/v1/academic/...` route space (prototype
//! navigation groups `academics`, `classes`, `timetable`, `attendance`, `courses`,
//! `assignments`, `resources`). Boundary rules (ADR-L5-0003): this crate depends only on
//! `sutra-kernel`, `sutra-contracts`, `sutra-web` and `sutra-persistence`; it publishes its ports
//! in `sutra_contracts::academic` and never reads another module's schema (ADR-L5-0036 §3.2).
//! Domain B data comes through `InstitutionReferencePort`; Domain C students, registrations and
//! the credit ledger through `StudentDirectory`, `RegistrationReader` and `CreditPostingPort`;
//! rooms (Campus, Batch 8) and staff (People, Batch 7) through stub ports.
//!
//! ## Batch 4 layout (see `docs/batch4-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/`, `directory.rs` | foundation (contract changes only) |
//! | `offering/`, `faculty/` | 4A |
//! | `timetable/`, `attendance/`, `monitoring/`, `assessment/` | 4B |
//! | `flexible/`, `recognition/`, `resources/` | 4C |

pub mod directory;
/// Batch 6 foundation: Domain D facts for Exams (`AcademicExamFacts`).
pub mod exam_facts;
pub mod faculty_workload;
pub mod shared;
pub mod taught_students; // Audit C3

// ---- slice areas (router mount points are pre-registered in `router_with_state`) ----
pub mod assessment; // 4B
pub mod attendance; // 4B
pub mod faculty; // 4A
pub mod flexible; // 4C
pub mod monitoring; // 4B
pub mod offering; // 4A
pub mod recognition; // 4C
pub mod resources; // 4C
pub mod timetable; // 4B

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::campus::TeachingSpacePort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::people::StaffDirectory;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{CreditPostingPort, RegistrationReader, StudentDirectory};
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use directory::PgOfferingDirectory;
pub use exam_facts::PgAcademicExamFacts;
pub use faculty_workload::PgFacultyWorkload;
pub use shared::AcademicState;
pub use taught_students::PgTaughtStudents;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "D",
    key: "academic",
    name: "Academic",
    schema: "academic",
    batch: 4,
    status: ModuleStatus::Scaffolded,
};

/// Adapters the composition root hands to Domain D.
#[derive(Clone)]
pub struct AcademicDeps {
    pub pool: PgPool,
    /// Approval steps (workflow adapter selected by `SUTRA_WORKFLOW`; was `NoopWorkflowAdapter` in Batch 4).
    pub workflow: Arc<dyn WorkflowPort>,
    /// How approval steps resolve (`SUTRA_APPROVAL_MODE`, composed by the API; never hard-coded).
    pub approvals: shared::approval::ApprovalMode,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students (`sutra_student::PgStudentDirectory`).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain C registrations (`sutra_student::PgRegistrationReader`).
    pub registrations: Arc<dyn RegistrationReader>,
    /// Domain C credit ledger (`sutra_student::PgCreditPosting`).
    pub credits: Arc<dyn CreditPostingPort>,
    /// Campus teaching spaces (`sutra_campus::NoTeachingSpaces` until Batch 8).
    pub spaces: Arc<dyn TeachingSpacePort>,
    /// Staff behind faculty login subjects (`sutra_people::SubjectOnlyStaff` until Batch 7).
    pub staff: Arc<dyn StaffDirectory>,
}

impl AcademicDeps {
    /// Handler state with the Batch 4 defaults (approval steps in the configured `approvals` mode, the Postgres
    /// offering directory over the foundation tables).
    pub fn into_state(self) -> AcademicState {
        AcademicState {
            offerings: Arc::new(PgOfferingDirectory::new(self.pool.clone())),
            pool: self.pool,
            workflow: self.workflow,
            approvals: self.approvals,
            references: self.references,
            students: self.students,
            registrations: self.registrations,
            credits: self.credits,
            spaces: self.spaces,
            staff: self.staff,
        }
    }
}

/// All Domain D routes. Every slice router is already mounted here; slices fill in their own
/// `router` functions and never edit this list.
pub fn router(deps: AcademicDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision` or port
/// doubles).
pub fn router_with_state(st: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(offering::router(st.clone())) // 4A
        .merge(faculty::router(st.clone())) // 4A
        .merge(timetable::router(st.clone())) // 4B
        .merge(attendance::router(st.clone())) // 4B
        .merge(monitoring::router(st.clone())) // 4B
        .merge(assessment::router(st.clone())) // 4B
        .merge(flexible::router(st.clone())) // 4C
        .merge(recognition::router(st.clone())) // 4C
        .merge(resources::router(st)) // 4C
        // C1/C2 CI follow-up: the maker of every governed request is recorded even when this
        // router is mounted without the session middleware (see `sutra_web::auth::approval_scope`).
        .layer(axum::middleware::from_fn(sutra_web::auth::approval_scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "academic");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 4);
    }
}
