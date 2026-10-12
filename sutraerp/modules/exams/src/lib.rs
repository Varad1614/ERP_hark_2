//! # Domain I — Examinations module
//!
//! **Batch 0 scaffold.** Owns PostgreSQL schema `exams` and the `/api/v1/exams/...` route space.
//! Implementation lands in Batch 6 (see `docs/batches.md`). Prototype navigation group:
//! `examination`.
//!
//! Boundary rules (ADR-L5-0003): this crate may only depend on `sutra-kernel`,
//! `sutra-contracts`, `sutra-web` and `sutra-persistence`; it publishes its ports in
//! `sutra_contracts::exams` and never reads another module's schema (ADR-L5-0036 §3.2).

use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

/// Batch 3 stub adapter consumed by Domain C (see `sutra_contracts::exams`).
pub mod results;
pub use results::NoPublishedResults;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "I",
    key: "exams",
    name: "Examinations",
    schema: "exams",
    batch: 6,
    status: ModuleStatus::Scaffolded,
};

/// Module routes (empty until Batch 6).
pub fn router() -> OpenApiRouter {
    OpenApiRouter::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_matches_crate() {
        assert_eq!(MODULE.key, "exams");
        assert_eq!(MODULE.schema, MODULE.key);
    }
}
