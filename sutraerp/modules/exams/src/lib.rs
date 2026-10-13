//! # Domain I — Examinations module
//!
//! Owns PostgreSQL schema `exams` and the `/api/v1/exams/...` route space (prototype navigation:
//! Admin `examinations`, Faculty `assessments`, Student `results`). Boundary rules
//! (ADR-L5-0003): this crate depends only on `sutra-kernel`, `sutra-contracts`, `sutra-web` and
//! `sutra-persistence`; it publishes its ports in `sutra_contracts::exams` and never reads
//! another module's schema (ADR-L5-0036 §3.2). Domain B data comes through
//! `InstitutionReferencePort`; Domain C students / term registrations through `StudentDirectory`
//! / `RegistrationReader`; Domain D offerings, rosters, faculty, finalised internal assessment
//! and attendance standing through `OfferingDirectory` / `AcademicExamFacts`; Domain F dues and
//! exam fee charges through `FinanceClearancePort`, `ChargeDirectory` and `ExamFeePort`.
//! Marks and grade points are `rust_decimal` end to end — never floats.
//!
//! ## Batch 6 layout (see `docs/batch6-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/`, `directory.rs`, `results_port.rs`, `grading/` | foundation (contract changes only) |
//! | `setup/`, `scheduling/`, `registration/` | 6A |
//! | `marks/`, `integrity/` | 6B |
//! | `results/`, `revaluation/`, `completion/` | 6C |

pub mod directory;
pub mod results_port;
pub mod shared;

// ---- foundation area (grading schemes)
pub mod grading;

// ---- slice areas (router mount points are pre-registered in `router_with_state`) ----
pub mod completion; // 6C
pub mod integrity; // 6B
pub mod marks; // 6B
pub mod registration; // 6A
pub mod results; // 6C
pub mod revaluation; // 6C
pub mod scheduling; // 6A
pub mod setup; // 6A

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::academic::ports::{AcademicExamFacts, OfferingDirectory};
use sutra_contracts::finance::ports::{ChargeDirectory, ExamFeePort};
use sutra_contracts::finance::FinanceClearancePort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{RegistrationReader, StudentDirectory};
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use directory::{PgExamDirectory, PgMarksReader};
pub use results_port::{NoPublishedResults, PgResultsPort};
pub use shared::ExamsState;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "I",
    key: "exams",
    name: "Examinations",
    schema: "exams",
    batch: 6,
    status: ModuleStatus::Active,
};

/// Adapters the composition root hands to Domain I.
#[derive(Clone)]
pub struct ExamsDeps {
    pub pool: PgPool,
    /// Approval steps (`NoopWorkflowAdapter` in Batch 6).
    pub workflow: Arc<dyn WorkflowPort>,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students (`sutra_student::PgStudentDirectory`).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain C term registrations (`sutra_student::PgRegistrationReader`).
    pub registrations: Arc<dyn RegistrationReader>,
    /// Domain D offerings / rosters / faculty (`sutra_academic::PgOfferingDirectory`).
    pub offerings: Arc<dyn OfferingDirectory>,
    /// Domain D internal assessment + attendance (`sutra_academic::PgAcademicExamFacts`).
    pub academic: Arc<dyn AcademicExamFacts>,
    /// Domain F dues (`sutra_finance::PgFinanceClearance`).
    pub finance: Arc<dyn FinanceClearancePort>,
    /// Domain F charges (`sutra_finance::PgChargeDirectory`).
    pub charges: Arc<dyn ChargeDirectory>,
    /// Domain F exam fee charges (`sutra_finance::PgExamFees`).
    pub fees: Arc<dyn ExamFeePort>,
}

impl ExamsDeps {
    /// Handler state with the Batch 6 defaults (auto-approving approval steps, the Postgres
    /// exam directory / marks reader over the foundation tables).
    pub fn into_state(self) -> ExamsState {
        ExamsState {
            directory: Arc::new(PgExamDirectory::new(self.pool.clone())),
            marks: Arc::new(PgMarksReader::new(self.pool.clone())),
            pool: self.pool,
            workflow: self.workflow,
            approvals: shared::approval::ApprovalMode::AutoApprove,
            references: self.references,
            students: self.students,
            registrations: self.registrations,
            offerings: self.offerings,
            academic: self.academic,
            finance: self.finance,
            charges: self.charges,
            fees: self.fees,
        }
    }
}

/// All Domain I routes. Every slice router is already mounted here; slices fill in their own
/// `router` functions and never edit this list.
pub fn router(deps: ExamsDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision` or port
/// doubles).
pub fn router_with_state(st: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(grading::router(st.clone())) // foundation
        .merge(setup::router(st.clone())) // 6A
        .merge(scheduling::router(st.clone())) // 6A
        .merge(registration::router(st.clone())) // 6A
        .merge(marks::router(st.clone())) // 6B
        .merge(integrity::router(st.clone())) // 6B
        .merge(results::router(st.clone())) // 6C
        .merge(revaluation::router(st.clone())) // 6C
        .merge(completion::router(st)) // 6C
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "exams");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 6);
    }
}
