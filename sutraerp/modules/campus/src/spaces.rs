//! Batch 4 stub of [`TeachingSpacePort`] (Campus facilities land in Batch 8): no teaching space
//! is known, so the Academic timetable (4B) keeps a free-text room label on its slots.

use sutra_contracts::campus::{TeachingSpacePort, TeachingSpaceRef};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Default)]
pub struct NoTeachingSpaces;

#[async_trait::async_trait]
impl TeachingSpacePort for NoTeachingSpaces {
    async fn space(
        &self,
        _scope: &ExecutionScope,
        _id: Uuid,
    ) -> Result<Option<TeachingSpaceRef>, KernelError> {
        Ok(None)
    }

    async fn spaces_of_campus(
        &self,
        _scope: &ExecutionScope,
        _campus_id: Uuid,
    ) -> Result<Vec<TeachingSpaceRef>, KernelError> {
        Ok(Vec::new())
    }
}
