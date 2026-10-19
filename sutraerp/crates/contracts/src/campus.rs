//! Domain G — Campus operations contracts (Batch 8, see `docs/batch8-contracts.md`).
//!
//! * [`ids`] — typed UUIDv7 ids of every Batch 8 Campus aggregate (foundation-owned, append-only).
//! * [`status`] — lifecycle / classification enums; the shared machines (infrastructure nodes,
//!   service tickets) carry their rules here, every other machine's transitions are slice-owned.
//! * [`events`] — released event type names + V1 payloads, one file per slice (8A / 8B / 8C).
//! * [`TeachingSpacePort`] — Batch 4 (Academic timetable, slice 4B) asks for teaching spaces
//!   (rooms, labs). Since the Batch 8 foundation the composition root wires
//!   `sutra_campus::PgTeachingSpaces` (active `space` nodes whose space type is a teaching type);
//!   an unknown id still answers `None`, so 4B keeps its free-text room label fallback.
//!
//! Domain G reaches other domains only through their contract ports:
//! `institution::reference::InstitutionReferencePort`, `student::ports::StudentDirectory`,
//! `people::ports::WorkforceDirectory`, `finance::ports::CampusChargePort`. Other modules must
//! only ever reach this domain through items declared here.

pub mod events;
pub mod ids;
pub mod status;

use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use uuid::Uuid;

/// A bookable teaching space.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeachingSpaceRef {
    pub id: Uuid,
    /// Domain B campus id.
    pub campus_id: Uuid,
    pub code: String,
    pub name: String,
    /// `classroom`, `lab`, `auditorium`, `seminar_room`, …
    pub kind: String,
    pub capacity: Option<i32>,
    pub active: bool,
}

/// Read port over Campus teaching spaces (tenant from `scope`; missing/foreign → `None`).
#[async_trait::async_trait]
pub trait TeachingSpacePort: Send + Sync + 'static {
    async fn space(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<TeachingSpaceRef>, KernelError>;

    async fn spaces_of_campus(
        &self,
        scope: &ExecutionScope,
        campus_id: Uuid,
    ) -> Result<Vec<TeachingSpaceRef>, KernelError>;
}
