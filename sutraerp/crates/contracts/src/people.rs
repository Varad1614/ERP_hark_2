//! Domain E — People & Workforce contracts.
//!
//! Staff records land in Batch 7. Batch 4 (Academic, slice 4A) identifies faculty by their login
//! subject (`dev|meera.shah@sutra.dev`, Keycloak `sub`) and asks [`StaffDirectory`] for the staff
//! record behind it; until Batch 7 the composition root wires `sutra_people::SubjectOnlyStaff`,
//! which accepts any non-empty subject and knows nothing else (`staff_id = None`). Other modules
//! must only ever reach this domain through items declared here.

use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use uuid::Uuid;

/// Staff facts Domain D may rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StaffRef {
    pub subject_ref: String,
    /// People-domain staff id (Batch 7); `None` until then.
    pub staff_id: Option<Uuid>,
    pub display_name: Option<String>,
    /// Domain B org unit (department) of the staff member, when known.
    pub org_unit_id: Option<Uuid>,
    /// `false` for staff who left / are on long leave (assignments must not start).
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
