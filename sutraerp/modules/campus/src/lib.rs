//! # Domain G — Campus module
//!
//! **Batch 0 scaffold.** Owns PostgreSQL schema `campus` and the `/api/v1/campus/...` route space.
//! Implementation lands in Batch 8 (see `docs/batches.md`). Prototype navigation group:
//! `campus`.
//!
//! Boundary rules (ADR-L5-0003): this crate may only depend on `sutra-kernel`,
//! `sutra-contracts`, `sutra-web` and `sutra-persistence`; it publishes its ports in
//! `sutra_contracts::campus` and never reads another module's schema (ADR-L5-0036 §3.2).

pub mod spaces;

pub use spaces::NoTeachingSpaces;

use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "G",
    key: "campus",
    name: "Campus",
    schema: "campus",
    batch: 8,
    status: ModuleStatus::Scaffolded,
};

/// Module routes (empty until Batch 8).
pub fn router() -> OpenApiRouter {
    OpenApiRouter::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "campus");
        assert_eq!(MODULE.schema, MODULE.key);
    }
}
