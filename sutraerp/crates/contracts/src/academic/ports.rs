//! Domain D ports.
//!
//! * [`OfferingDirectory`] — the **offering read port**: slice 4A's offerings, sections, rosters
//!   and faculty assignments as 4B, 4C and later domains (Exams, Batch 6) see them. Implemented
//!   by `sutra_academic::PgOfferingDirectory` over the foundation tables (`academic.course_offering`,
//!   `academic.offering_section`, `academic.faculty_assignment`, `academic.section_enrolment`), so
//!   it is real from day one — 4B/4C never wait for 4A, and never read 4A's own tables.
//!
//! Every method is tenant-scoped through `scope`; a missing or foreign row is `None` / empty.

use super::ids::{CourseOfferingId, FacultyAssignmentId, OfferingSectionId, SectionEnrolmentId};
use super::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, FacultyRole, SectionEnrolmentStatus,
    SectionStatus,
};
use crate::student::ids::{StudentContextId, StudentId};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// Offering facts other slices / domains rely on.
#[derive(Clone, Debug, PartialEq)]
pub struct OfferingSummary {
    pub id: CourseOfferingId,
    pub institution_id: InstitutionId,
    pub campus_id: Option<Uuid>,
    pub academic_term_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub credits: f64,
    pub status: CourseOfferingStatus,
    pub version: i64,
}

/// Section facts other slices / domains rely on.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionSummary {
    pub id: OfferingSectionId,
    pub offering_id: CourseOfferingId,
    pub institution_id: InstitutionId,
    pub academic_term_id: Uuid,
    pub section_code: String,
    pub capacity: Option<i32>,
    pub status: SectionStatus,
    /// Offering code + title, denormalised for lists ("CS201 · Data Structures").
    pub offering_code: String,
    pub offering_title: String,
    pub offering_status: CourseOfferingStatus,
}

/// One seated (or formerly seated) student of a section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosterEntry {
    pub enrolment_id: SectionEnrolmentId,
    pub section_id: OfferingSectionId,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub status: SectionEnrolmentStatus,
    /// Snapshot taken at enrolment (no join into the `student` schema).
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
}

/// One faculty assignment of a section.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FacultyOnSection {
    pub assignment_id: FacultyAssignmentId,
    pub section_id: OfferingSectionId,
    pub faculty_subject_ref: String,
    pub faculty_name: Option<String>,
    pub role: FacultyRole,
    pub status: FacultyAssignmentStatus,
}

/// Read port over the shared offering tables.
#[async_trait::async_trait]
pub trait OfferingDirectory: Send + Sync + 'static {
    async fn offering(
        &self,
        scope: &ExecutionScope,
        id: CourseOfferingId,
    ) -> Result<Option<OfferingSummary>, KernelError>;

    /// Offerings of an institution in one academic term (any status).
    async fn offerings_in_term(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        academic_term_id: Uuid,
    ) -> Result<Vec<OfferingSummary>, KernelError>;

    async fn section(
        &self,
        scope: &ExecutionScope,
        id: OfferingSectionId,
    ) -> Result<Option<SectionSummary>, KernelError>;

    async fn sections_of_offering(
        &self,
        scope: &ExecutionScope,
        offering_id: CourseOfferingId,
    ) -> Result<Vec<SectionSummary>, KernelError>;

    /// Students of a section; `seated_only` keeps `enrolled` seats (attendance / coursework rosters).
    async fn roster(
        &self,
        scope: &ExecutionScope,
        section_id: OfferingSectionId,
        seated_only: bool,
    ) -> Result<Vec<RosterEntry>, KernelError>;

    /// Faculty assignments of a section (all statuses; filter on `status == Active` for teaching rights).
    async fn faculty_of_section(
        &self,
        scope: &ExecutionScope,
        section_id: OfferingSectionId,
    ) -> Result<Vec<FacultyOnSection>, KernelError>;

    /// Sections the login subject **actively** teaches ("My classes"; faculty-scoped permissions).
    async fn sections_taught_by(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
    ) -> Result<Vec<SectionSummary>, KernelError>;

    /// Sections where the student holds an `enrolled` seat (student timetable / courses / assignments).
    async fn sections_of_student(
        &self,
        scope: &ExecutionScope,
        student_id: StudentId,
    ) -> Result<Vec<SectionSummary>, KernelError>;

    /// `true` when `faculty_subject_ref` holds an **active** assignment on the section.
    async fn teaches(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
        section_id: OfferingSectionId,
    ) -> Result<bool, KernelError> {
        Ok(self
            .faculty_of_section(scope, section_id)
            .await?
            .iter()
            .any(|f| {
                f.status == FacultyAssignmentStatus::Active
                    && f.faculty_subject_ref == faculty_subject_ref
            }))
    }
}
