//! # Domain F — Finance module
//!
//! Owns PostgreSQL schema `finance` and the `/api/v1/finance/...` route space (prototype
//! navigation groups `finance` (admin) and `fees` (student)). Boundary rules (ADR-L5-0003): this
//! crate depends only on `sutra-kernel`, `sutra-contracts`, `sutra-web` and `sutra-persistence`;
//! it publishes its ports in `sutra_contracts::finance` and never reads another module's schema
//! (ADR-L5-0036 §3.2). Domain B data comes through `InstitutionReferencePort`; Domain C students
//! through `StudentDirectory` and approved scholarship decisions through `AidDecisionSource`.
//! Money is `rust_decimal` end to end (NUMERIC(18,2), INR) — never floats.
//!
//! ## Batch 5 layout (see `docs/batch5-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/` (except `shared/period.rs` body → 5C), `directory.rs`, `clearance.rs`, `accounts/`, `fees.rs` | foundation (contract changes only) |
//! | `charges/`, `adjustments/`, `aid/`, `config/` | 5A |
//! | `payments/` (incl. `payments/gateway.rs`), `refunds/`, `receivables/` | 5B |
//! | `ledger/`, `periods/`, `treasury/` | 5C |

pub mod clearance;
pub mod directory;
pub mod fees;
pub mod shared;

// ---- foundation area (party accounts + balances)
pub mod accounts;

// ---- slice areas (router mount points are pre-registered in `router_with_state`) ----
pub mod adjustments; // 5A
pub mod aid; // 5A
pub mod charges; // 5A
pub mod config; // 5A
pub mod ledger; // 5C
pub mod payments; // 5B
pub mod periods; // 5C
pub mod receivables; // 5B
pub mod refunds; // 5B
pub mod treasury; // 5C

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::finance::ports::PaymentGatewayPort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{AidDecisionSource, StudentDirectory};
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use clearance::PgFinanceClearance;
pub use directory::{PgChargeDirectory, PgPostingPort, PgReceivableReader};
pub use payments::gateway::MockPaymentGateway;
pub use shared::FinanceState;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "F",
    key: "finance",
    name: "Finance",
    schema: "finance",
    batch: 5,
    status: ModuleStatus::Active,
};

/// Adapters the composition root hands to Domain F.
#[derive(Clone)]
pub struct FinanceDeps {
    pub pool: PgPool,
    /// Approval steps (`NoopWorkflowAdapter` in Batch 5).
    pub workflow: Arc<dyn WorkflowPort>,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students (`sutra_student::PgStudentDirectory`).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain C scholarship decisions handed off to Financial Aid (`sutra_student::PgAidDecisionSource`).
    pub aid: Arc<dyn AidDecisionSource>,
    /// Payment gateway (`MockPaymentGateway` in Batch 5).
    pub gateway: Arc<dyn PaymentGatewayPort>,
}

impl FinanceDeps {
    /// Handler state with the Batch 5 defaults (auto-approving approval steps, the Postgres
    /// charge directory / receivable reader / posting port over the foundation tables).
    pub fn into_state(self) -> FinanceState {
        FinanceState {
            charges: Arc::new(PgChargeDirectory::new(self.pool.clone())),
            receivables: Arc::new(PgReceivableReader::new(self.pool.clone())),
            posting: Arc::new(PgPostingPort::new(self.pool.clone())),
            pool: self.pool,
            workflow: self.workflow,
            approvals: shared::approval::ApprovalMode::AutoApprove,
            references: self.references,
            students: self.students,
            aid: self.aid,
            gateway: self.gateway,
        }
    }
}

/// All Domain F routes. Every slice router is already mounted here; slices fill in their own
/// `router` functions and never edit this list.
pub fn router(deps: FinanceDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision` or port
/// doubles).
pub fn router_with_state(st: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(accounts::router(st.clone())) // foundation
        .merge(charges::router(st.clone())) // 5A
        .merge(adjustments::router(st.clone())) // 5A
        .merge(aid::router(st.clone())) // 5A
        .merge(config::router(st.clone())) // 5A
        .merge(payments::router(st.clone())) // 5B
        .merge(refunds::router(st.clone())) // 5B
        .merge(receivables::router(st.clone())) // 5B
        .merge(ledger::router(st.clone())) // 5C
        .merge(periods::router(st.clone())) // 5C
        .merge(treasury::router(st)) // 5C
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "finance");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 5);
    }
}
