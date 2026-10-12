//! # Domain F — Finance module
//!
//! **Batch 0 scaffold.** Owns PostgreSQL schema `finance` and the `/api/v1/finance/...` route space.
//! Implementation lands in Batch 5 (see `docs/batches.md`). Prototype navigation group:
//! `finance`.
//!
//! Boundary rules (ADR-L5-0003): this crate may only depend on `sutra-kernel`,
//! `sutra-contracts`, `sutra-web` and `sutra-persistence`; it publishes its ports in
//! `sutra_contracts::finance` and never reads another module's schema (ADR-L5-0036 §3.2).

use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub mod fees;

/// Batch 3 stub adapter consumed by Domain C (see `sutra_contracts::finance`).
pub mod clearance;
pub use clearance::StubFinanceClearance;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "F",
    key: "finance",
    name: "Finance",
    schema: "finance",
    batch: 5,
    status: ModuleStatus::Scaffolded,
};

/// Module routes (empty until Batch 5).
pub fn router() -> OpenApiRouter {
    OpenApiRouter::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "finance");
        assert_eq!(MODULE.schema, MODULE.key);
    }
}
