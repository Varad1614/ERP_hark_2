//! # Domain B — Institution module
//!
//! Batch 0 shipped one **reference aggregate** (`institution.institution`) that demonstrates every
//! persistence/API convention end to end, so later batches copy a proven pattern:
//!
//! * UUIDv7 technical ids, tenant-scoped uniqueness (ADR-L5-0036)
//! * owner-local transaction: state + outbox intent + audit commit atomically (ADR-L5-0006/0007)
//! * optimistic concurrency via `version` + `ETag`/`If-Match` (ADR-L5-0006)
//! * required `Idempotency-Key` on create (ADR-L5-0034)
//! * fail-closed, **session-derived** tenant scope (ADR-L5-0004) and deny-by-default D02
//!   permissions (`institution.read` / `.register` / `.rename`) — Batch 1 slice A
//! * advisory read-through cache for `GET …/institutions/{id}` keyed by session tenant, with
//!   owner-driven invalidation on register/rename (ADR-L5-0037) — Batch 1 slice C
//!
//! ## Batch 2 layout (see `docs/batch2-contracts.md`)
//!
//! Every Batch 2 aggregate lives in this crate and in the `institution` schema (the architecture
//! tests fix exactly ten domain crates, and `modules/campus` is Domain G). One directory per area,
//! each owned by exactly one slice:
//!
//! | Path | Owner |
//! |------|-------|
//! | `lib.rs`, `shared/` | foundation (contract changes only) |
//! | `domain.rs`, `http.rs`, `repo.rs`, `lifecycle/`, `org/`, `position/` | 2A |
//! | `campus/`, `curriculum/` | 2B |
//! | `calendar/`, `readiness/` | 2C |
//! | `reference.rs` (`InstitutionReferencePort` adapter for Domain C) | Batch 3 foundation |

pub mod domain;
pub mod http;
pub mod reference;
pub mod repo;
pub mod shared;

// ---- slice areas (router mount points are pre-registered in `router`) ----
pub mod calendar; // 2C
pub mod campus; // 2B
pub mod curriculum; // 2B
pub mod lifecycle; // 2A
pub mod org; // 2A
pub mod position; // 2A
pub mod readiness; // 2C

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_kernel::cache::AdvisoryCache;
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub use reference::PgInstitutionReferences;
pub use repo::PgInstitutionDirectory;
pub use shared::InstitutionState;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "B",
    key: "institution",
    name: "Institution",
    schema: "institution",
    batch: 2,
    status: ModuleStatus::Scaffolded,
};

/// Adapters the composition root hands to Domain B.
#[derive(Clone)]
pub struct InstitutionDeps {
    pub pool: PgPool,
    /// Advisory cache: pass `AdvisoryCache::disabled()` to turn it off.
    pub cache: AdvisoryCache,
    /// Approval steps (`NoopWorkflowAdapter` in Batch 2).
    pub workflow: Arc<dyn WorkflowPort>,
}

/// All Domain B routes. Every slice router is already mounted here; slices fill in their own
/// `router` functions and never edit this list.
pub fn router(deps: InstitutionDeps) -> OpenApiRouter {
    let st = InstitutionState::new(deps.pool, deps.cache, deps.workflow);
    OpenApiRouter::new()
        .merge(http::router(st.clone())) // 2A — reference aggregate root
        .merge(lifecycle::router(st.clone())) // 2A
        .merge(org::router(st.clone())) // 2A
        .merge(position::router(st.clone())) // 2A
        .merge(campus::router(st.clone())) // 2B
        .merge(curriculum::router(st.clone())) // 2B
        .merge(calendar::router(st.clone())) // 2C
        .merge(readiness::router(st)) // 2C
}
