//! Audit C3 (M-2): [`TaughtStudentsReader`] — the students a faculty member teaches, read by
//! Domain C to scope `student.read.assigned` (own sections only). A student counts while holding
//! an `enrolled` seat (`academic.section_enrolment`) in a section where the subject holds an
//! **active** faculty assignment (`academic.faculty_assignment`).

use sqlx::PgPool;
use sutra_contracts::academic::ports::TaughtStudentsReader;
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgTaughtStudents {
    pool: PgPool,
}

impl PgTaughtStudents {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl TaughtStudentsReader for PgTaughtStudents {
    async fn taught_students(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
    ) -> Result<Vec<StudentId>, KernelError> {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT DISTINCT e.student_id FROM academic.section_enrolment e \
             JOIN academic.faculty_assignment f ON f.section_id = e.section_id \
                 AND f.tenant_id = e.tenant_id \
             WHERE e.tenant_id = $1 AND e.status = 'enrolled' \
               AND f.faculty_subject_ref = $2 AND f.status = 'active' \
             ORDER BY e.student_id",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(faculty_subject_ref)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(ids.into_iter().map(StudentId).collect())
    }

    async fn teaches_student(
        &self,
        scope: &ExecutionScope,
        faculty_subject_ref: &str,
        student_id: StudentId,
    ) -> Result<bool, KernelError> {
        sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM academic.section_enrolment e \
             JOIN academic.faculty_assignment f ON f.section_id = e.section_id \
                 AND f.tenant_id = e.tenant_id \
             WHERE e.tenant_id = $1 AND e.student_id = $2 AND e.status = 'enrolled' \
               AND f.faculty_subject_ref = $3 AND f.status = 'active')",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(faculty_subject_ref)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)
    }
}
