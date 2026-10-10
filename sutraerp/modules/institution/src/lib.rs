//! # Domain B — Institution module
//!
//! Batch 0 ships one **reference aggregate** (`institution.institution`) that demonstrates every
//! persistence/API convention end to end, so later batches copy a proven pattern:
//!
//! * UUIDv7 technical ids, tenant-scoped uniqueness (ADR-L5-0036)
//! * owner-local transaction: state + outbox intent + audit commit atomically (ADR-L5-0006/0007)
//! * optimistic concurrency via `version` + `ETag`/`If-Match` (ADR-L5-0006)
//! * required `Idempotency-Key` on create (ADR-L5-0034)
//! * fail-closed, **session-derived** tenant scope (ADR-L5-0004) and deny-by-default D02
//!   permissions (`institution.read` / `.register` / `.rename`) — Batch 1 slice A
//!
//! Full institution structure (campuses, departments, calendars) is Batch 2.

pub mod domain;
pub mod http;
pub mod repo;

use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "B",
    key: "institution",
    name: "Institution",
    schema: "institution",
    batch: 2,
    status: ModuleStatus::Scaffolded,
};

pub use http::router;
pub use repo::PgInstitutionDirectory;
