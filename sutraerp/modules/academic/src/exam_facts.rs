//! [`AcademicExamFacts`] over the 4B tables (Batch 6 foundation, contract change to Domain D
//! recorded in `docs/batch6-contracts.md` §6.3): finalised internal assessment results and the
//! latest attendance evaluation, read-only, for Domain I (internal marks import, hall ticket
//! attendance condition). Exams never reads the `academic` schema itself.

use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId};
use sutra_contracts::academic::ports::{
    AcademicExamFacts, AttendanceStanding, InternalAssessmentFact,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgAcademicExamFacts {
    pool: PgPool,
}

impl PgAcademicExamFacts {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn fact_from_row(r: &PgRow) -> Result<InternalAssessmentFact, sqlx::Error> {
    Ok(InternalAssessmentFact {
        result_id: r.try_get("id")?,
        offering_id: CourseOfferingId(r.try_get("offering_id")?),
        section_id: OfferingSectionId(r.try_get("section_id")?),
        academic_term_id: r.try_get("academic_term_id")?,
        curriculum_component_id: r.try_get("curriculum_component_id")?,
        student_id: StudentId(r.try_get("student_id")?),
        marks_obtained: r.try_get::<rust_decimal::Decimal, _>("marks_obtained")?,
        max_marks: r.try_get::<rust_decimal::Decimal, _>("max_marks")?,
        result_version: r.try_get("result_version")?,
        finalized_at: r.try_get("finalized_at")?,
    })
}

#[async_trait::async_trait]
impl AcademicExamFacts for PgAcademicExamFacts {
    async fn internal_results(
        &self,
        scope: &ExecutionScope,
        offering_id: CourseOfferingId,
    ) -> Result<Vec<InternalAssessmentFact>, KernelError> {
        sqlx::query(
            "SELECT id, offering_id, section_id, academic_term_id, curriculum_component_id, \
             student_id, marks_obtained, max_marks, result_version, finalized_at \
             FROM academic.internal_assessment_result \
             WHERE tenant_id = $1 AND offering_id = $2 AND status IN ('finalized', 'archived') \
             ORDER BY student_number, id",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(offering_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(fact_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
    }

    async fn attendance_standing(
        &self,
        scope: &ExecutionScope,
        offering_id: CourseOfferingId,
        student_id: StudentId,
    ) -> Result<Option<AttendanceStanding>, KernelError> {
        let Some(r) = sqlx::query(
            "SELECT e.section_id, e.student_id, e.attended, e.total, e.percent, \
             e.below_threshold, e.evaluated_at \
             FROM academic.attendance_evaluation e \
             JOIN academic.offering_section s ON s.id = e.section_id \
             WHERE e.tenant_id = $1 AND s.offering_id = $2 AND e.student_id = $3 \
             ORDER BY e.evaluated_at DESC LIMIT 1",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(offering_id.as_uuid())
        .bind(student_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?
        else {
            return Ok(None);
        };
        (|| -> Result<AttendanceStanding, sqlx::Error> {
            Ok(AttendanceStanding {
                section_id: OfferingSectionId(r.try_get("section_id")?),
                student_id: StudentId(r.try_get("student_id")?),
                attended: r.try_get("attended")?,
                total: r.try_get("total")?,
                percent: r.try_get::<rust_decimal::Decimal, _>("percent")?,
                below_threshold: r.try_get("below_threshold")?,
                evaluated_at: r.try_get("evaluated_at")?,
            })
        })()
        .map(Some)
        .map_err(map_db_error)
    }
}
