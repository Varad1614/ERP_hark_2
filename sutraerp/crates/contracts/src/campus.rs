//! Domain G — Campus contracts.
//!
//! Campus facilities land in Batch 8. Batch 4 (Academic timetable, slice 4B) asks for teaching
//! spaces (rooms, labs) through [`TeachingSpacePort`]; until Batch 8 the composition root wires
//! `sutra_campus::NoTeachingSpaces`, which knows no space — 4B then stores the free-text room
//! label on the slot and leaves `teaching_space_id` empty. Other modules must only ever reach
//! this domain through items declared here.

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
