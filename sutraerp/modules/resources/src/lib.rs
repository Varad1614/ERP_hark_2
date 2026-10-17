//! # Domain J — Enterprise resources module
//!
//! Owns PostgreSQL schema `resources` and the `/api/v1/resources/...` route space (prototype
//! navigation: Admin `campus` tabs procurement / inventory / assets). Boundary rules
//! (ADR-L5-0003): this crate depends only on `sutra-kernel`, `sutra-contracts`, `sutra-web` and
//! `sutra-persistence`; it never reads another module's schema. Domain B data comes through
//! `InstitutionReferencePort`; staff (custodians, requesters) through `WorkforceDirectory`;
//! Finance accounts payable through `ProcurementPostingPort` (vendor invoices and payments).
//! Domain H reads figures through [`PgResourceMetrics`]. Money and quantities are
//! `rust_decimal` end to end — never floats.
//!
//! ## Batch 8 layout (see `docs/batch8-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/`, `directory/`, `metrics.rs` | foundation (contract changes only) |
//! | `suppliers/`, `requisitions/`, `sourcing/`, `orders/`, `receipts/`, `inventory/`, `assets/`, `payables/` | 8D |

pub mod metrics;
pub mod shared;

// ---- foundation area (stock on hand, read-only)
pub mod directory;

// ---- slice areas (8D; router mount points are pre-registered in `router_with_state`) ----
pub mod assets;
pub mod contracts;
pub mod inventory;
pub mod orders;
pub mod payables;
pub mod receipts;
pub mod requisitions;
pub mod sourcing;
pub mod suppliers;

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::finance::ports::ProcurementPostingPort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::people::ports::WorkforceDirectory;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use metrics::PgResourceMetrics;
pub use shared::ResourcesState;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "J",
    key: "resources",
    name: "Resources",
    schema: "resources",
    batch: 8,
    status: ModuleStatus::Active,
};

/// Adapters the composition root hands to Domain J.
#[derive(Clone)]
pub struct ResourcesDeps {
    pub pool: PgPool,
    /// Approval steps (workflow adapter selected by `SUTRA_WORKFLOW`; was `NoopWorkflowAdapter` in Batch 8).
    pub workflow: Arc<dyn WorkflowPort>,
    /// How approval steps resolve (`SUTRA_APPROVAL_MODE`, composed by the API; never hard-coded).
    pub approvals: shared::approval::ApprovalMode,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain E staff records (`sutra_people::PgWorkforceDirectory`).
    pub workforce: Arc<dyn WorkforceDirectory>,
    /// Domain F procurement postings (`sutra_finance::PgProcurementPostings`).
    pub postings: Arc<dyn ProcurementPostingPort>,
}

impl ResourcesDeps {
    /// Handler state with the Batch 8 defaults (approval steps in the configured `approvals` mode).
    pub fn into_state(self) -> ResourcesState {
        ResourcesState {
            pool: self.pool,
            workflow: self.workflow,
            approvals: self.approvals,
            references: self.references,
            workforce: self.workforce,
            postings: self.postings,
        }
    }
}

/// All Domain J routes. Every slice router is already mounted here.
pub fn router(deps: ResourcesDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision` or port
/// doubles).
pub fn router_with_state(st: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(directory::router(st.clone())) // foundation
        .merge(suppliers::router(st.clone())) // 8D
        .merge(requisitions::router(st.clone())) // 8D
        .merge(sourcing::router(st.clone())) // 8D
        .merge(orders::router(st.clone())) // 8D
        .merge(receipts::router(st.clone())) // 8D
        .merge(inventory::router(st.clone())) // 8D
        .merge(assets::router(st.clone())) // 8D
        .merge(contracts::router(st.clone())) // ERP_EXTENSIONS 3C
        .merge(payables::router(st)) // 8D
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "resources");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 8);
    }
}
