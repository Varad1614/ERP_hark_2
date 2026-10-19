//! Wire form of the shared Student aggregate (contract §5.3).

use crate::shared::student::{StudentContextRow, StudentRow};
use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_contracts::student::status::{StudentContextStatus, StudentStatus};
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::InstitutionId;
use uuid::Uuid;

/// One StudentContext (the student's relationship with one institution).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StudentContext {
    pub id: StudentContextId,
    pub institution_id: InstitutionId,
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub admission_year_id: Option<Uuid>,
    pub batch_code: Option<String>,
    pub section_code: Option<String>,
    pub current_term_no: Option<i32>,
    pub enrolment_number: Option<String>,
    pub roll_number: Option<String>,
    /// `regular` | `lateral_entry` | `readmission` | `transfer_in` | `re_entry`.
    pub enrolment_type: String,
    pub enrolled_on: Option<NaiveDate>,
    pub admission_id: Option<Uuid>,
    pub predecessor_context_id: Option<StudentContextId>,
    pub status: StudentContextStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub closed_on: Option<NaiveDate>,
    #[schema(value_type = i64)]
    pub version: Version,
}

impl From<&StudentContextRow> for StudentContext {
    fn from(c: &StudentContextRow) -> Self {
        Self {
            id: c.id,
            institution_id: c.institution_id,
            programme_id: c.programme_id,
            curriculum_version_id: c.curriculum_version_id,
            campus_id: c.campus_id,
            org_unit_id: c.org_unit_id,
            admission_term_id: c.admission_term_id,
            admission_year_id: c.admission_year_id,
            batch_code: c.batch_code.clone(),
            section_code: c.section_code.clone(),
            current_term_no: c.current_term_no,
            enrolment_number: c.enrolment_number.clone(),
            roll_number: c.roll_number.clone(),
            enrolment_type: c.enrolment_type.clone(),
            enrolled_on: c.enrolled_on,
            admission_id: c.admission_id,
            predecessor_context_id: c.predecessor_context_id,
            status: c.status,
            status_reason: c.status_reason.clone(),
            status_changed_at: c.status_changed_at,
            closed_on: c.closed_on,
            version: c.version,
        }
    }
}

/// The Student (persona) with its current context. `GET /students/{id}` also fills `contexts`
/// (every context, newest first); directory items leave it empty. `version` / `ETag` is the
/// persona's version; context commands use `currentContext.version`.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Student {
    pub id: StudentId,
    pub student_number: String,
    pub display_name: String,
    pub given_name: String,
    pub middle_name: Option<String>,
    pub family_name: Option<String>,
    pub preferred_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub primary_email: Option<String>,
    pub primary_phone: Option<String>,
    /// A login is linked for self-service.
    pub subject_linked: bool,
    pub status: StudentStatus,
    pub status_changed_at: DateTime<Utc>,
    pub origin_admission_id: Option<Uuid>,
    /// Set on an archived duplicate: the surviving student.
    pub merged_into_id: Option<StudentId>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub current_context: Option<StudentContext>,
    pub contexts: Vec<StudentContext>,
}

impl Student {
    pub fn from_rows(
        s: &StudentRow,
        current: Option<&StudentContextRow>,
        all: &[StudentContextRow],
    ) -> Self {
        Self {
            id: s.id,
            student_number: s.student_number.clone(),
            display_name: s.display_name(),
            given_name: s.given_name.clone(),
            middle_name: s.middle_name.clone(),
            family_name: s.family_name.clone(),
            preferred_name: s.preferred_name.clone(),
            date_of_birth: s.date_of_birth,
            gender: s.gender.clone(),
            primary_email: s.primary_email.clone(),
            primary_phone: s.primary_phone.clone(),
            subject_linked: s.subject_ref.is_some(),
            status: s.status,
            status_changed_at: s.status_changed_at,
            origin_admission_id: s.origin_admission_id,
            merged_into_id: s.merged_into_id,
            version: s.version,
            created_at: s.created_at,
            current_context: current.map(StudentContext::from),
            contexts: all.iter().map(StudentContext::from).collect(),
        }
    }
}
