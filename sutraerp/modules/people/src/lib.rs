//! # Domain E — People & Workforce module
//!
//! **Batch 0 scaffold.** Owns PostgreSQL schema `people` and the `/api/v1/people/...` route space.
//! Implementation lands in Batch 7 (see `docs/batches.md`). Prototype navigation group:
//! `people`.
//!
//! Boundary rules (ADR-L5-0003): this crate may only depend on `sutra-kernel`,
//! `sutra-contracts`, `sutra-web` and `sutra-persistence`; it publishes its ports in
//! `sutra_contracts::people` and never reads another module's schema (ADR-L5-0036 §3.2).

pub mod staff;

pub use staff::SubjectOnlyStaff;

use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "E",
    key: "people",
    name: "People & Workforce",
    schema: "people",
    batch: 7,
    status: ModuleStatus::Scaffolded,
};

/// Module routes (empty until Batch 7).
pub fn router() -> OpenApiRouter {
    OpenApiRouter::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "people");
        assert_eq!(MODULE.schema, MODULE.key);
    }
}
