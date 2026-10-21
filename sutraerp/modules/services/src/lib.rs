//! # Domain H — Institutional services & intelligence module
//!
//! Owns PostgreSQL schema `services` and the `/api/v1/services/...` route space (prototype
//! navigation: Admin `reports`). Batch 8 implements H07 enterprise dashboards and H08 regulatory
//! reporting (slice 8C) as **read-only projections**: every figure of another domain comes
//! through `sutra_contracts::services::ports::OperationalMetricsPort` (`ServicesState::metrics`),
//! never cross-schema SQL. Boundary rules (ADR-L5-0003): this crate depends only on
//! `sutra-kernel`, `sutra-contracts`, `sutra-web` and `sutra-persistence`.
//!
//! ## Batch 8 layout (see `docs/batch8-contracts.md`)
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/`, `directory/` | foundation (contract changes only) |
//! | `dashboards/`, `regulatory/` | 8C |

pub mod shared;

// ---- foundation area (metrics catalogue, read-only)
pub mod directory;

// ---- slice areas (router mount points are pre-registered in `router_with_state`) ----
pub mod accreditation; // 9E
pub mod dashboards; // 8C
pub mod regulatory; // 8C

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::services::ports::OperationalMetricsPort;
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use shared::ServicesState;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "H",
    key: "services",
    name: "Services",
    schema: "services",
    batch: 8,
    status: ModuleStatus::Active,
};

/// Adapters the composition root hands to Domain H.
#[derive(Clone)]
pub struct ServicesDeps {
    pub pool: PgPool,
    /// Approval steps (workflow adapter selected by `SUTRA_WORKFLOW`; was `NoopWorkflowAdapter` in Batch 8).
    pub workflow: Arc<dyn WorkflowPort>,
    /// How approval steps resolve (`SUTRA_APPROVAL_MODE`, composed by the API; never hard-coded).
    pub approvals: shared::approval::ApprovalMode,
    /// Domain B reference data (`sutra_institution::PgInstitutionReferences`).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// One metrics adapter per domain (`PgStudentMetrics`, `PgPeopleMetrics`,
    /// `PgFinanceMetrics`, `PgCampusMetrics`, `PgResourceMetrics`).
    pub metrics: Vec<Arc<dyn OperationalMetricsPort>>,
}

impl ServicesDeps {
    /// Handler state with the Batch 8 defaults (approval steps in the configured `approvals` mode).
    pub fn into_state(self) -> ServicesState {
        ServicesState {
            pool: self.pool,
            workflow: self.workflow,
            approvals: self.approvals,
            references: self.references,
            metrics: self.metrics.into(),
        }
    }
}

/// All Domain H routes. Every slice router is already mounted here.
pub fn router(deps: ServicesDeps) -> OpenApiRouter {
    router_with_state(deps.into_state())
}

/// Same as [`router`] with an explicit state (tests use `ApprovalMode::AwaitDecision` or port
/// doubles).
pub fn router_with_state(st: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(directory::router(st.clone())) // foundation
        .merge(dashboards::router(st.clone())) // 8C
        .merge(accreditation::router(st.clone())) // 9E
        .merge(regulatory::router(st)) // 8C
        // C1/C2 CI follow-up: the maker of every governed request is recorded even when this
        // router is mounted without the session middleware (see `sutra_web::auth::approval_scope`).
        .layer(axum::middleware::from_fn(sutra_web::auth::approval_scope))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "services");
        assert_eq!(MODULE.schema, MODULE.key);
        assert_eq!(MODULE.batch, 8);
    }
}
