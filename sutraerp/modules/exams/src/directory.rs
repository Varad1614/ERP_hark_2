//! [`ExamDirectory`] and [`MarksReader`] over the foundation tables (foundation-owned). 6B and
//! 6C read sessions, papers, registrations and marks only through these ports (`ExamsState::
//! directory` / `marks`) or the read-only decoders in `shared::rows`; other domains get them
//! from the composition root.

use crate::shared::rows::{
    attach_papers, load_scheme, mark_from_row, paper_from_row, registration_from_row,
    session_from_row, MARK_COLUMNS, PAPER_COLUMNS, REGISTRATION_COLUMNS, SESSION_COLUMNS,
};
use sqlx::PgPool;
use sutra_contracts::exams::ids::{
    ExamPaperId, ExamRegistrationId, ExamSessionId, GradingSchemeId,
};
use sutra_contracts::exams::ports::{
    CandidateSummary, ExamDirectory, ExamPaperSummary, ExamSessionSummary, GradingSchemeSummary,
    MarksReader, PaperMarkSummary,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgExamDirectory {
    pool: PgPool,
}

impl PgExamDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn registrations(
        &self,
        scope: &ExecutionScope,
        filter: &str,
        a: Uuid,
        b: Option<Uuid>,
        subject: Option<&str>,
    ) -> Result<Vec<CandidateSummary>, KernelError> {
        let sql = format!(
            "SELECT {REGISTRATION_COLUMNS} FROM exams.exam_registration \
             WHERE tenant_id = $1 AND {filter} ORDER BY registration_number"
        );
        let mut q = sqlx::query(&sql).bind(scope.tenant_id.as_uuid());
        if let Some(s) = subject {
            q = q.bind(s);
        } else {
            q = q.bind(a);
            if let Some(b) = b {
                q = q.bind(b);
            }
        }
        let regs = q
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?
            .iter()
            .map(registration_from_row)
            .collect::<Result<Vec<_>, _>>()
            .map_err(map_db_error)?;
        attach_papers(&self.pool, scope.tenant_id, regs).await
    }
}

#[async_trait::async_trait]
impl ExamDirectory for PgExamDirectory {
    async fn session(
        &self,
        scope: &ExecutionScope,
        id: ExamSessionId,
    ) -> Result<Option<ExamSessionSummary>, KernelError> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM exams.exam_session WHERE id = $1 AND tenant_id = $2"
        );
        sqlx::query(&sql)
            .bind(id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?
            .as_ref()
            .map(session_from_row)
            .transpose()
            .map_err(map_db_error)
    }

    async fn sessions_of_institution(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<ExamSessionSummary>, KernelError> {
        let sql = format!(
            "SELECT {SESSION_COLUMNS} FROM exams.exam_session \
             WHERE tenant_id = $1 AND institution_id = $2 ORDER BY created_at DESC, id DESC"
        );
        sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(institution_id.as_uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?
            .iter()
            .map(session_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn grading_scheme(
        &self,
        scope: &ExecutionScope,
        id: GradingSchemeId,
    ) -> Result<Option<GradingSchemeSummary>, KernelError> {
        load_scheme(&self.pool, scope.tenant_id, id).await
    }

    async fn paper(
        &self,
        scope: &ExecutionScope,
        id: ExamPaperId,
    ) -> Result<Option<ExamPaperSummary>, KernelError> {
        let sql = format!(
            "SELECT {PAPER_COLUMNS} FROM exams.exam_paper WHERE id = $1 AND tenant_id = $2"
        );
        sqlx::query(&sql)
            .bind(id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?
            .as_ref()
            .map(paper_from_row)
            .transpose()
            .map_err(map_db_error)
    }

    async fn papers_of_session(
        &self,
        scope: &ExecutionScope,
        session_id: ExamSessionId,
    ) -> Result<Vec<ExamPaperSummary>, KernelError> {
        let sql = format!(
            "SELECT {PAPER_COLUMNS} FROM exams.exam_paper \
             WHERE tenant_id = $1 AND session_id = $2 ORDER BY paper_code"
        );
        sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(session_id.as_uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?
            .iter()
            .map(paper_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn registration(
        &self,
        scope: &ExecutionScope,
        id: ExamRegistrationId,
    ) -> Result<Option<CandidateSummary>, KernelError> {
        Ok(self
            .registrations(scope, "id = $2", id.as_uuid(), None, None)
            .await?
            .pop())
    }

    async fn registrations_of_session(
        &self,
        scope: &ExecutionScope,
        session_id: ExamSessionId,
    ) -> Result<Vec<CandidateSummary>, KernelError> {
        self.registrations(scope, "session_id = $2", session_id.as_uuid(), None, None)
            .await
    }

    async fn registration_of_student(
        &self,
        scope: &ExecutionScope,
        session_id: ExamSessionId,
        student_id: StudentId,
    ) -> Result<Option<CandidateSummary>, KernelError> {
        Ok(self
            .registrations(
                scope,
                "session_id = $2 AND student_id = $3 \
                 AND status NOT IN ('withdrawn', 'rejected', 'archived')",
                session_id.as_uuid(),
                Some(student_id.as_uuid()),
                None,
            )
            .await?
            .pop())
    }

    async fn registrations_of_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Vec<CandidateSummary>, KernelError> {
        self.registrations(
            scope,
            "subject_ref = $2",
            Uuid::nil(),
            None,
            Some(subject_ref),
        )
        .await
    }
}

#[derive(Clone)]
pub struct PgMarksReader {
    pool: PgPool,
}

impl PgMarksReader {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn marks(
        &self,
        scope: &ExecutionScope,
        filter: &str,
        id: Uuid,
    ) -> Result<Vec<PaperMarkSummary>, KernelError> {
        let cols = MARK_COLUMNS
            .split(", ")
            .map(|c| format!("m.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT {cols} FROM exams.paper_mark m \
             JOIN exams.exam_registration r ON r.id = m.registration_id \
             WHERE m.tenant_id = $1 AND {filter} ORDER BY r.registration_number, m.created_at"
        );
        sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(id)
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?
            .iter()
            .map(mark_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }
}

#[async_trait::async_trait]
impl MarksReader for PgMarksReader {
    async fn marks_of_paper(
        &self,
        scope: &ExecutionScope,
        paper_id: ExamPaperId,
    ) -> Result<Vec<PaperMarkSummary>, KernelError> {
        self.marks(scope, "m.paper_id = $2", paper_id.as_uuid())
            .await
    }

    async fn marks_of_registration(
        &self,
        scope: &ExecutionScope,
        registration_id: ExamRegistrationId,
    ) -> Result<Vec<PaperMarkSummary>, KernelError> {
        self.marks(scope, "m.registration_id = $2", registration_id.as_uuid())
            .await
    }
}
