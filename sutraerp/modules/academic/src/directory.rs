//! [`OfferingDirectory`] over the shared delivery tables (foundation-owned). 4B and 4C read
//! offerings, sections, rosters and faculty only through this port (`AcademicState::offerings`);
//! other domains (Exams, Batch 6) get it from the composition root.

use crate::shared::offering::{
    assignment_from_row, enrolment_from_row, offering_from_row, OfferingRow, ASSIGNMENT_COLUMNS,
    ENROLMENT_COLUMNS, OFFERING_COLUMNS,
};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId};
use sutra_contracts::academic::ports::{
    FacultyOnSection, OfferingDirectory, OfferingSummary, RosterEntry, SectionSummary,
};
use sutra_contracts::academic::status::{CourseOfferingStatus, SectionStatus};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgOfferingDirectory {
    pool: PgPool,
}

impl PgOfferingDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

/// Section columns joined with their offering (`s` = section, `o` = offering).
const SECTION_SELECT: &str = "SELECT s.id, s.offering_id, s.institution_id, o.academic_term_id, \
    s.section_code, s.capacity, s.status, o.offering_code, o.title AS offering_title, \
    o.status AS offering_status \
    FROM academic.offering_section s JOIN academic.course_offering o ON o.id = s.offering_id";

fn offering_summary(o: OfferingRow) -> OfferingSummary {
    OfferingSummary {
        id: o.id,
        institution_id: o.institution_id,
        campus_id: o.campus_id,
        academic_term_id: o.academic_term_id,
        curriculum_component_id: o.curriculum_component_id,
        offering_code: o.offering_code,
        title: o.title,
        credits: o.credits,
        status: o.status,
        version: o.version.0,
    }
}

fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown status `{raw}`").into()))
}

fn section_summary(r: &PgRow) -> Result<SectionSummary, sqlx::Error> {
    Ok(SectionSummary {
        id: OfferingSectionId(r.try_get("id")?),
        offering_id: CourseOfferingId(r.try_get("offering_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        academic_term_id: r.try_get("academic_term_id")?,
        section_code: r.try_get("section_code")?,
        capacity: r.try_get("capacity")?,
        status: decode(r.try_get("status")?, SectionStatus::parse)?,
        offering_code: r.try_get("offering_code")?,
        offering_title: r.try_get("offering_title")?,
        offering_status: decode(r.try_get("offering_status")?, CourseOfferingStatus::parse)?,
    })
}

impl PgOfferingDirectory {
    async fn sections(
        &self,
        filter: &str,
        a: Uuid,
        b: Uuid,
        c: Option<&str>,
    ) -> Result<Vec<SectionSummary>, KernelError> {
        let sql =
            format!("{SECTION_SELECT} WHERE {filter} ORDER BY o.offering_code, s.section_code");
        let mut q = sqlx::query(&sql).bind(a).bind(b);
        if let Some(c) = c {
            q = q.bind(c);
        }
        let rows = q.fetch_all(&self.pool).await.map_err(map_db_error)?;
        rows.iter()
            .map(section_summary)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }
}

#[async_trait::async_trait]
impl OfferingDirectory for PgOfferingDirectory {
    async fn offering(
        &self,
        scope: &ExecutionScope,
        id: CourseOfferingId,
    ) -> Result<Option<OfferingSummary>, KernelError> {
        let sql = format!(
            "SELECT {OFFERING_COLUMNS} FROM academic.course_offering WHERE id = $1 AND tenant_id = $2"
        );
        let row = sqlx::query(&sql)
            .bind(id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        row.as_ref()
            .map(offering_from_row)
            .transpose()
            .map_err(map_db_error)
            .map(|o| o.map(offering_summary))
    }

    async fn offerings_in_term(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        academic_term_id: Uuid,
    ) -> Result<Vec<OfferingSummary>, KernelError> {
        let sql = format!(
            "SELECT {OFFERING_COLUMNS} FROM academic.course_offering \
             WHERE tenant_id = $1 AND institution_id = $2 AND academic_term_id = $3 ORDER BY offering_code"
        );
        let rows = sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(institution_id.as_uuid())
            .bind(academic_term_id)
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(|r| offering_from_row(r).map(offering_summary))
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn section(
        &self,
        scope: &ExecutionScope,
        id: OfferingSectionId,
    ) -> Result<Option<SectionSummary>, KernelError> {
        Ok(self
            .sections(
                "s.id = $1 AND s.tenant_id = $2",
                id.as_uuid(),
                scope.tenant_id.as_uuid(),
                None,
            )
            .await?
            .into_iter()
            .next())
    }

    async fn sections_of_offering(
        &self,
        scope: &ExecutionScope,
        offering_id: CourseOfferingId,
    ) -> Result<Vec<SectionSummary>, KernelError> {
        self.sections(
            "s.offering_id = $1 AND s.tenant_id = $2",
            offering_id.as_uuid(),
            scope.tenant_id.as_uuid(),
            None,
        )
        .await
    }

    async fn roster(
        &self,
        scope: &ExecutionScope,
        section_id: OfferingSectionId,
        seated_only: bool,
    ) -> Result<Vec<RosterEntry>, KernelError> {
        let sql = format!(
            "SELECT {ENROLMENT_COLUMNS} FROM academic.section_enrolment \
             WHERE section_id = $1 AND tenant_id = $2 AND (NOT $3 OR status = 'enrolled') \
             ORDER BY student_number, enrolled_at"
        );
        let rows = sqlx::query(&sql)
            .bind(section_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(seated_only)
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(|r| {
                enrolment_from_row(r).map(|e| RosterEntry {
                    enrolment_id: e.id,
                    section_id: e.section_id,
                    student_id: e.student_id,
                    student_context_id: e.student_context_id,
                    status: e.status,
                    student_number: e.student_number,
                    display_name: e.display_name,
                    subject_ref: e.subject_ref,
                })
            })
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn faculty_of_section(
        &self,
        scope: &ExecutionScope,
        section_id: OfferingSectionId,
    ) -> Result<Vec<FacultyOnSection>, KernelError> {
        let sql = format!(
            "SELECT {ASSIGNMENT_COLUMNS} FROM academic.faculty_assignment \
             WHERE section_id = $1 AND tenant_id = $2 ORDER BY created_at"
        );
        let rows = sqlx::query(&sql)
            .bind(section_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(|r| {
                assignment_from_row(r).map(|a| FacultyOnSection {
                    assignment_id: a.id,
                    section_id: a.section_id,
                    faculty_subject_ref: a.faculty_subject_ref,
                    faculty_name: a.faculty_name,
                    role: a.role,
                    status: a.status,
                })
            })
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn sections_taught_by(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
    ) -> Result<Vec<SectionSummary>, KernelError> {
        self.sections(
            "s.tenant_id = $2 AND s.id IN (SELECT f.section_id FROM academic.faculty_assignment f \
             WHERE f.tenant_id = $2 AND f.faculty_subject_ref = $3 AND f.status = 'active') AND $1 = $1",
            Uuid::nil(),
            scope.tenant_id.as_uuid(),
            Some(faculty_subject_ref),
        )
        .await
    }

    async fn sections_of_student(
        &self,
        scope: &ExecutionScope,
        student_id: StudentId,
    ) -> Result<Vec<SectionSummary>, KernelError> {
        self.sections(
            "s.tenant_id = $2 AND s.id IN (SELECT e.section_id FROM academic.section_enrolment e \
             WHERE e.tenant_id = $2 AND e.student_id = $1 AND e.status = 'enrolled')",
            student_id.as_uuid(),
            scope.tenant_id.as_uuid(),
            None,
        )
        .await
    }

    async fn teaches(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
        section_id: OfferingSectionId,
    ) -> Result<bool, KernelError> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM academic.faculty_assignment WHERE tenant_id = $1 \
             AND section_id = $2 AND faculty_subject_ref = $3 AND status = 'active')",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(section_id.as_uuid())
        .bind(faculty_subject_ref)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)
    }
}
