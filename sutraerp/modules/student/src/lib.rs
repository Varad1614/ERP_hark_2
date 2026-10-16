//! # Domain C — Student Lifecycle module
//!
//! Owns PostgreSQL schema `student` and the `/api/v1/student/...` route space (prototype
//! navigation group `students`). Boundary rules (ADR-L5-0003): this crate depends only on
//! `sutra-kernel`, `sutra-contracts`, `sutra-web` and `sutra-persistence`; it publishes its ports
//! in `sutra_contracts::student` and never reads another module's schema (ADR-L5-0036 §3.2).
//! Domain B data comes through `InstitutionReferencePort`; Finance (F) and Exams (I) facts
//! through their contract ports (stub adapters until Batches 5/6).
//!
//! ## Batch 3 layout (see `docs/batch3-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/`, `directory.rs`, `adapters.rs` | foundation (contract changes only) |
//! | `academic_ports.rs` | Batch 4 foundation (Domain D's `RegistrationReader` / `CreditPostingPort`) |
//! | `admission/`, `record/` | 3A |
//! | `registration/`, `progression/` | 3B |
//! | `completion/`, `guardian/`, `scholarship/`, `alumni/` | 3C |

pub mod academic_ports; // Batch 4 foundation: ports Domain D calls
pub mod adapters;
pub mod dataimport; // Part 2 · S4-import
pub mod directory;
pub mod files; // Part 2 · S4-docs: stored files of admission / record documents
pub mod finance_ports; // Batch 5 foundation: ports Domain F calls
pub mod identity_source; // Part 2 · S4-idp
pub mod metrics; // Batch 8 foundation: Domain H figures
pub mod search_source;
pub mod shared; // 9C

// ---- slice areas (router mount points are pre-registered in `router`) ----
pub mod admission; // 3A
pub mod alumni; // 3C
pub mod completion; // 3C
pub mod guardian; // 3C
pub mod progression; // 3B
pub mod record; // 3A
pub mod registration; // 3B
pub mod scholarship; // 3C

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::exams::ResultsPort;
use sutra_contracts::finance::FinanceClearancePort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::documents::DocumentVault;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{AbcRegistryPort, ExternalScholarshipPort};
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use academic_ports::{PgCreditPosting, PgRegistrationReader};
pub use dataimport::{PgHistoricalResultImport, PgStudentImport}; // Part 2 · S4-import
pub use directory::PgStudentDirectory;
pub use finance_ports::PgAidDecisionSource;
pub use metrics::PgStudentMetrics;
pub use shared::StudentState;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "C",
    key: "student",
    name: "Student Lifecycle",
    schema: "student",
    batch: 3,
    status: ModuleStatus::Scaffolded,
};

/// Adapters the composition root hands to Domain C.
#[derive(Clone)]
pub struct StudentDeps {
    pub pool: PgPool,
    /// Approval steps (workflow adapter selected by `SUTRA_WORKFLOW`; was `NoopWorkflowAdapter` in Batch 3).
    pub workflow: Arc<dyn WorkflowPort>,
    /// How approval steps resolve (`SUTRA_APPROVAL_MODE`, composed by the API; never hard-coded).
    pub approvals: shared::approval::ApprovalMode,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Finance facts (`sutra_finance::PgFinanceClearance` since the Batch 5 foundation).
    pub finance: Arc<dyn FinanceClearancePort>,
    /// Exams results (`sutra_exams::NoPublishedResults` until Batch 6).
    pub results: Arc<dyn ResultsPort>,
    /// Academic Bank of Credits ([`adapters::NoopAbcRegistry`]).
    pub abc: Arc<dyn AbcRegistryPort>,
    /// External scholarship ecosystems ([`adapters::NoopExternalScholarships`]).
    pub external_scholarships: Arc<dyn ExternalScholarshipPort>,
    /// Part 2 · S4-docs: platform document service (S3-compatible object storage; disabled vault
    /// when no storage is configured).
    pub documents: Arc<dyn DocumentVault>,
}

impl StudentDeps {
    /// Handler state with the Batch 3 defaults (approval steps in the configured `approvals` mode).
    pub fn into_state(self) -> StudentState {
        StudentState {
            pool: self.pool,
            workflow: self.workflow,
            approvals: self.approvals,
            references: self.references,
            finance: self.finance,
            results: self.results,
            abc: self.abc,
            external_scholarships: self.external_scholarships,
            documents: self.documents,
        }
    }
}

/// All Domain C routes. Every slice router is already mounted here; slices fill in their own
/// `router` functions and never edit this list.
pub fn router(deps: StudentDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision`).
pub fn router_with_state(st: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(admission::router(st.clone())) // 3A
        .merge(record::router(st.clone())) // 3A
        .merge(registration::router(st.clone())) // 3B
        .merge(progression::router(st.clone())) // 3B
        .merge(completion::router(st.clone())) // 3C
        .merge(guardian::router(st.clone())) // 3C
        .merge(scholarship::router(st.clone())) // 3C
        .merge(alumni::router(st.clone())) // 3C
        .merge(files::router(st)) // Part 2 · S4-docs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "student");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 3);
    }
}
