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

// ---- Batch 6 (Domain I) block: examination facts owned by Domain D ----

/// A **finalised** internal assessment result (4B `internal_assessment_result`, current
/// version, status `finalized` or `archived`) as Exams imports it into a paper's internal marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InternalAssessmentFact {
    pub result_id: Uuid,
    pub offering_id: CourseOfferingId,
    pub section_id: OfferingSectionId,
    pub academic_term_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub student_id: StudentId,
    pub marks_obtained: rust_decimal::Decimal,
    pub max_marks: rust_decimal::Decimal,
    /// Increments on every governed correction (L3-D-057).
    pub result_version: i32,
    pub finalized_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// The student's attendance standing in the section of an offering (4B attendance evaluation
/// under the effective policy) — an examination entry condition (hall tickets, L3-I-038).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttendanceStanding {
    pub section_id: OfferingSectionId,
    pub student_id: StudentId,
    pub attended: i32,
    pub total: i32,
    pub percent: rust_decimal::Decimal,
    pub below_threshold: bool,
    pub evaluated_at: chrono::DateTime<chrono::Utc>,
}

/// Read port over Domain D facts Exams needs (internal assessment + attendance eligibility).
/// Implemented by `sutra_academic::PgAcademicExamFacts` over the 4B tables (**real**).
#[async_trait::async_trait]
pub trait AcademicExamFacts: Send + Sync + 'static {
    /// Finalised internal assessment results of every student of the offering (any section).
    async fn internal_results(
        &self,
        scope: &ExecutionScope,
        offering_id: CourseOfferingId,
    ) -> Result<Vec<InternalAssessmentFact>, KernelError>;

    /// The student's latest attendance evaluation in their section of the offering; `None`
    /// when attendance was never evaluated (no policy / no finalised sessions).
    async fn attendance_standing(
        &self,
        scope: &ExecutionScope,
        offering_id: CourseOfferingId,
        student_id: StudentId,
    ) -> Result<Option<AttendanceStanding>, KernelError>;
}

// ---- Batch 7 (Domain E) block: faculty teaching load owned by Domain D ----

/// One approved / active faculty assignment counted in a faculty member's teaching load.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeachingLoadItem {
    pub assignment_id: FacultyAssignmentId,
    pub offering_id: CourseOfferingId,
    pub section_id: OfferingSectionId,
    pub academic_term_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub role: FacultyRole,
    pub status: FacultyAssignmentStatus,
    /// Weekly contact hours (4A governance); `None` when not recorded.
    pub weekly_hours: Option<rust_decimal::Decimal>,
}

/// The teaching load of one faculty member (login subject) in an institution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FacultyWorkload {
    pub faculty_subject_ref: String,
    pub items: Vec<TeachingLoadItem>,
    /// Σ `weekly_hours` of the items (missing hours count 0).
    pub total_weekly_hours: rust_decimal::Decimal,
    /// `max_weekly_hours` of the institution's **effective** faculty assignment policy.
    pub policy_max_weekly_hours: Option<rust_decimal::Decimal>,
}

/// Read port over Domain D faculty assignments People (7B workload plans, L3-E-030) needs.
/// Implemented by `sutra_academic::PgFacultyWorkload` over the 4A tables (**real**).
#[async_trait::async_trait]
pub trait FacultyWorkloadPort: Send + Sync + 'static {
    /// Approved / active assignments of `faculty_subject_ref` in the institution, optionally
    /// limited to one academic term.
    async fn teaching_load(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        faculty_subject_ref: &str,
        academic_term_id: Option<Uuid>,
    ) -> Result<FacultyWorkload, KernelError>;
}

// ---- Audit C3 (M-2): faculty read scope over Domain C ----

/// Which students a faculty member teaches, as Domain C (Student) needs it to scope
/// `student.read.assigned` reads to the caller's own sections. Implemented by
/// `sutra_academic::PgTaughtStudents` over `academic.faculty_assignment` (status `active`) ×
/// `academic.section_enrolment` (status `enrolled`) — **real**. Domain C never reads the
/// `academic` schema; it receives this port through `sutra_api::app::student_deps`.
#[async_trait::async_trait]
pub trait TaughtStudentsReader: Send + Sync + 'static {
    /// Every student holding an `enrolled` seat in a section the subject actively teaches
    /// (distinct, any institution of the tenant).
    async fn taught_students(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
    ) -> Result<Vec<StudentId>, KernelError>;

    /// `true` when the student holds an `enrolled` seat in a section the subject actively teaches.
    async fn teaches_student(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
        student_id: StudentId,
    ) -> Result<bool, KernelError> {
        Ok(self
            .taught_students(scope, faculty_subject_ref)
            .await?
            .contains(&student_id))
    }
}

/// [`TaughtStudentsReader`] for compositions without Domain D (teaches nobody: faculty-scoped
/// reads see no student).
#[derive(Clone, Copy, Debug, Default)]
pub struct NoTaughtStudents;

#[async_trait::async_trait]
impl TaughtStudentsReader for NoTaughtStudents {
    async fn taught_students(
        &self,
        _scope: &ExecutionScope,
        _faculty_subject_ref: &str,
    ) -> Result<Vec<StudentId>, KernelError> {
        Ok(Vec::new())
    }
}
