//! [`StaffDirectory`] — the Batch 4 read port Domain D (and through it Exams) uses for staff facts.
//!
//! Batch 4 (Academic, slice 4A) identifies faculty by their login subject
//! (`dev|meera.shah@sutra.dev`, Keycloak `sub`) and asks [`StaffDirectory`] for the staff record
//! behind it. Since the Batch 7 foundation the composition root wires
//! `sutra_people::PgStaffDirectory` (real staff record + engagement; falls back to the Batch 4
//! subject-only answer when no staff record exists). Frozen shape: additive changes only.

use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use uuid::Uuid;

/// Staff facts Domain D may rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaffRef {
    pub subject_ref: String,
    /// People-domain staff id; `None` when no staff record exists for the subject.
    pub staff_id: Option<Uuid>,
    pub display_name: Option<String>,
    /// Domain B org unit (department) of the staff member, when known.
    pub org_unit_id: Option<Uuid>,
    /// `false` for staff who left / are suspended / on long leave (assignments must not start).
    pub active: bool,
}

/// Read port over staff (tenant from `scope`; unknown → `None`).
#[async_trait::async_trait]
pub trait StaffDirectory: Send + Sync + 'static {
    async fn staff_by_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Option<StaffRef>, KernelError>;
}
