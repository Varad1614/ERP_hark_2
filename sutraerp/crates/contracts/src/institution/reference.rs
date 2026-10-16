//! **Batch 3** read port over Domain B reference data, for Domain C (and later D/F/I).
//!
//! Domain C (`sutra-student`) may not read the `institution` schema (architecture test
//! `modules_do_not_touch_foreign_schemas`). It resolves institutions, campuses, organisation
//! units, programmes, curriculum versions/components, academic years, terms and calendar key
//! dates through [`InstitutionReferencePort`], implemented by
//! `sutra_institution::PgInstitutionReferences` and wired by the composition root.
//!
//! Every method is tenant-scoped by `scope.tenant_id`; a foreign-tenant or missing id is `None`
//! (never an error), so callers turn it into their own `404`/`422`.

use chrono::NaiveDate;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

use super::status::{
    AcademicYearStatus, CampusStatus, InstitutionStatus, OrgUnitStatus, PublicationStatus,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstitutionRef {
    pub id: InstitutionId,
    pub code: String,
    pub name: String,
    pub status: InstitutionStatus,
    /// IANA business timezone (governs every business-local date of Domain C).
    pub timezone: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampusRef {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub status: CampusStatus,
    pub timezone: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgUnitRef {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub unit_type: String,
    pub status: OrgUnitStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgrammeRef {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub org_unit_id: Option<Uuid>,
    pub code: String,
    pub name: String,
    pub level: String,
    pub duration_terms: i32,
    pub campus_ids: Vec<Uuid>,
    pub status: PublicationStatus,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurriculumVersionRef {
    pub id: Uuid,
    pub curriculum_id: Uuid,
    pub programme_id: Uuid,
    pub institution_id: InstitutionId,
    pub version_no: i32,
    pub status: PublicationStatus,
    pub effective_from: NaiveDate,
    /// `semester` | `trimester` | `annual` | …
    pub term_structure: String,
    pub term_count: i32,
    /// `nep2020` | `cbcs` | `custom`
    pub credit_framework: String,
    pub min_total_credits: Option<f64>,
    pub min_credits_per_term: Option<f64>,
    pub max_credits_per_term: Option<f64>,
    pub multiple_entry_exit: bool,
    pub abc_enabled: bool,
    pub nheqf_level: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct CurriculumComponentRef {
    pub id: Uuid,
    pub curriculum_version_id: Uuid,
    pub term_no: i32,
    pub slot_code: String,
    pub title: String,
    pub category: String,
    pub credits: f64,
    pub course_code: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcademicYearRef {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub calendar_id: Uuid,
    pub label: String,
    pub status: AcademicYearStatus,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AcademicTermRef {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub calendar_version_id: Uuid,
    pub academic_year_id: Uuid,
    /// Status of the owning calendar version (`published` = authoritative).
    pub calendar_version_status: PublicationStatus,
    pub code: String,
    pub name: String,
    pub sequence: i32,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyDateRef {
    pub id: Uuid,
    pub calendar_version_id: Uuid,
    /// `registration_window` | `instruction_start` | `exam_window` | `holiday` | …
    pub kind: String,
    pub name: String,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

/// Read-only Domain B reference lookups (tenant from `scope`).
#[async_trait::async_trait]
pub trait InstitutionReferencePort: Send + Sync + 'static {
    async fn institution(
        &self,
        scope: &ExecutionScope,
        id: InstitutionId,
    ) -> Result<Option<InstitutionRef>, KernelError>;
    async fn campus(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<CampusRef>, KernelError>;
    async fn org_unit(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<OrgUnitRef>, KernelError>;
    async fn programme(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<ProgrammeRef>, KernelError>;
    /// Part 2 · S4-import: a programme of `institution` by its code (case-insensitive), so
    /// migration files can name programmes the way the legacy system did.
    async fn programme_by_code(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        code: &str,
    ) -> Result<Option<ProgrammeRef>, KernelError>;

    /// Part 2 · S4-import: a campus of `institution` by its code (case-insensitive).
    async fn campus_by_code(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        code: &str,
    ) -> Result<Option<CampusRef>, KernelError>;

    async fn curriculum_version(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<CurriculumVersionRef>, KernelError>;
    /// The **published** curriculum version of a programme in force on `on` (latest
    /// `effective_from <= on`), if any.
    async fn current_curriculum_version(
        &self,
        scope: &ExecutionScope,
        programme_id: Uuid,
        on: NaiveDate,
    ) -> Result<Option<CurriculumVersionRef>, KernelError>;
    /// Components of a curriculum version ordered by `term_no`, `sort_order`.
    async fn curriculum_components(
        &self,
        scope: &ExecutionScope,
        curriculum_version_id: Uuid,
    ) -> Result<Vec<CurriculumComponentRef>, KernelError>;
    async fn academic_year(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<AcademicYearRef>, KernelError>;
    async fn academic_term(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<AcademicTermRef>, KernelError>;
    /// Key dates of one calendar version (e.g. `registration_window`).
    async fn key_dates(
        &self,
        scope: &ExecutionScope,
        calendar_version_id: Uuid,
    ) -> Result<Vec<KeyDateRef>, KernelError>;
}
