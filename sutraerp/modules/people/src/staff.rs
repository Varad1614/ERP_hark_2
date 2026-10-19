//! Batch 4 stub of [`StaffDirectory`] (kept for tests / tools; the composition root wires
//! [`crate::PgStaffDirectory`] since Batch 7): any non-empty login
//! subject is accepted as an active staff member with no further facts, so Academic (4A) can
//! assign faculty by login subject until then.

use sutra_contracts::people::{StaffDirectory, StaffRef};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

#[derive(Clone, Copy, Debug, Default)]
pub struct SubjectOnlyStaff;

#[async_trait::async_trait]
impl StaffDirectory for SubjectOnlyStaff {
    async fn staff_by_subject(
        &self,
        _scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Option<StaffRef>, KernelError> {
        let s = subject_ref.trim();
        Ok((!s.is_empty()).then(|| StaffRef {
            subject_ref: s.to_owned(),
            staff_id: None,
            display_name: None,
            org_unit_id: None,
            active: true,
        }))
    }
}
