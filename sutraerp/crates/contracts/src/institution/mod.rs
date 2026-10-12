//! Domain B — Institution contracts.
//!
//! Batch 2 contract surface (see `docs/batch2-contracts.md`):
//!
//! * [`ids`] — typed UUIDv7 ids of every Batch 2 aggregate (foundation-owned, append-only).
//! * [`status`] — lifecycle state enums (foundation-owned; transitions are slice-owned).
//! * [`events`] — released event type names + V1 payloads, one file per slice.

use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// Read port other modules use to confirm an institution exists inside the caller's tenant.
#[async_trait::async_trait]
pub trait InstitutionDirectory: Send + Sync + 'static {
    async fn exists(&self, scope: &ExecutionScope, id: InstitutionId) -> Result<bool, KernelError>;
}

pub mod events;
pub mod ids;
pub mod status;
