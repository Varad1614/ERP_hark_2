//! [`StudentDirectory`] over the shared Student tables (foundation-owned). Other domains get it
//! from the composition root; they never query the `student` schema.

use crate::shared::student::{
    context_from_row, student_from_row, StudentContextRow, StudentRow, CONTEXT_COLUMNS,
    STUDENT_COLUMNS,
};
use sqlx::PgPool;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::{StudentContextSummary, StudentDirectory, StudentSummary};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

#[derive(Clone)]
pub struct PgStudentDirectory {
    pool: PgPool,
}

impl PgStudentDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn summary(s: StudentRow) -> StudentSummary {
    StudentSummary {
        display_name: s.display_name(),
        id: s.id,
        student_number: s.student_number,
        status: s.status,
        subject_ref: s.subject_ref,
    }
}

fn context_summary(c: StudentContextRow) -> StudentContextSummary {
    StudentContextSummary {
        id: c.id,
        student_id: c.student_id,
        institution_id: c.institution_id,
        status: c.status,
        programme_id: c.programme_id,
        curriculum_version_id: c.curriculum_version_id,
        campus_id: c.campus_id,
        current_term_no: c.current_term_no,
    }
}

#[async_trait::async_trait]
impl StudentDirectory for PgStudentDirectory {
    async fn student(
        &self,
        scope: &ExecutionScope,
        id: StudentId,
    ) -> Result<Option<StudentSummary>, KernelError> {
        let sql = format!(
            "SELECT {STUDENT_COLUMNS} FROM student.student WHERE id = $1 AND tenant_id = $2"
        );
        let row = sqlx::query(&sql)
            .bind(id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        row.as_ref()
            .map(student_from_row)
            .transpose()
            .map_err(map_db_error)
            .map(|o| o.map(summary))
    }

    async fn live_context(
        &self,
        scope: &ExecutionScope,
        student_id: StudentId,
        institution_id: InstitutionId,
    ) -> Result<Option<StudentContextSummary>, KernelError> {
        let sql = format!(
            "SELECT {CONTEXT_COLUMNS} FROM student.student_context \
             WHERE student_id = $1 AND tenant_id = $2 AND institution_id = $3 \
               AND status IN ('pending_activation', 'active', 'suspended')"
        );
        let row = sqlx::query(&sql)
            .bind(student_id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .bind(institution_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        row.as_ref()
            .map(context_from_row)
            .transpose()
            .map_err(map_db_error)
            .map(|o| o.map(context_summary))
    }

    async fn by_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Option<StudentSummary>, KernelError> {
        let sql = format!(
            "SELECT {STUDENT_COLUMNS} FROM student.student \
             WHERE subject_ref = $1 AND tenant_id = $2 AND status <> 'archived'"
        );
        let row = sqlx::query(&sql)
            .bind(subject_ref)
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        row.as_ref()
            .map(student_from_row)
            .transpose()
            .map_err(map_db_error)
            .map(|o| o.map(summary))
    }
}
