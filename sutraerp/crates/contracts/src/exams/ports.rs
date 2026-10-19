//! Domain I read ports over the **foundation tables** (`docs/batch6-contracts.md` §6.2).
//!
//! * [`ExamDirectory`] — sessions, papers, grading schemes and candidate registrations ("6A → 6B /
//!   6C"): 6B enters marks for the candidates it lists, 6C computes results for them, the student
//!   screens resolve the caller's own registrations. Implemented by `sutra_exams::PgExamDirectory`
//!   over `exams.exam_session`, `exams.exam_paper`, `exams.grading_scheme`,
//!   `exams.exam_registration(_paper)` — real from day one, so 6B / 6C never wait for 6A's API
//!   (tests seed through `apps/api/tests/support/b6.rs`).
//! * [`MarksReader`] — per-candidate paper marks ("6B → 6C"): 6C computes only from
//!   [`MarkStatus::Finalised`] rows. Implemented by `sutra_exams::PgMarksReader` over
//!   `exams.paper_mark`.
//! * Domain C keeps reading published results through [`super::ResultsPort`]
//!   (`sutra_exams::PgResultsPort` over `exams.term_result`, real since the Batch 6 foundation).
//!
//! Every method is tenant-scoped through `scope`; a missing or foreign row is `None` / empty.
//! Marks, credits, percentages and grade points are `rust_decimal::Decimal` (NUMERIC in the
//! database) — never floats.

use super::ids::{
    ExamPaperId, ExamRegistrationId, ExamRegistrationPaperId, ExamSessionId, GradingSchemeId,
    PaperMarkId,
};
use super::status::{
    AttemptKind, ExamMode, ExamPaperStatus, ExamRegistrationStatus, ExamSessionKind,
    ExamSessionStatus, GradingSchemeStatus, MarkStatus, RegistrationChannel,
    RegistrationPaperStatus,
};
use crate::student::ids::{StudentContextId, StudentId};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// One band of a grading scheme: a percentage at or above `min_percent` earns `grade`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradeBand {
    pub grade: String,
    /// 0 … 100, two decimals.
    pub min_percent: Decimal,
    /// 0 … 10 on the ten-point scale.
    pub grade_point: Decimal,
    pub is_pass: bool,
}

/// A grading scheme with its bands ordered by `min_percent` descending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GradingSchemeSummary {
    pub id: GradingSchemeId,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub bands: Vec<GradeBand>,
    pub status: GradingSchemeStatus,
    pub version: i64,
}

/// Session facts other slices / domains rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExamSessionSummary {
    pub id: ExamSessionId,
    pub institution_id: InstitutionId,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Uuid,
    pub code: String,
    pub name: String,
    pub kind: ExamSessionKind,
    pub status: ExamSessionStatus,
    /// 6A's governing policy version (plain id).
    pub policy_id: Option<Uuid>,
    pub grading_scheme_id: Option<GradingSchemeId>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub registration_opens_on: Option<NaiveDate>,
    pub registration_closes_on: Option<NaiveDate>,
    pub version: i64,
}

/// Paper facts other slices rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExamPaperSummary {
    pub id: ExamPaperId,
    pub session_id: ExamSessionId,
    pub institution_id: InstitutionId,
    /// Domain D course offering (plain id) — internal assessment and rosters come from it.
    pub offering_id: Option<Uuid>,
    pub curriculum_component_id: Uuid,
    pub academic_term_id: Uuid,
    pub paper_code: String,
    pub title: String,
    pub credits: Decimal,
    pub mode: ExamMode,
    /// Maximum external (end-semester) marks.
    pub max_marks: Decimal,
    /// Minimum overall percentage to pass the paper (with the grade band's `is_pass`).
    pub pass_percent: Decimal,
    /// Share of the final percentage taken from internal assessment (0 … 100).
    pub internal_weight: Decimal,
    pub duration_minutes: Option<i32>,
    pub status: ExamPaperStatus,
    pub version: i64,
}

/// One registered paper of a candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidatePaper {
    pub registration_paper_id: ExamRegistrationPaperId,
    pub paper_id: ExamPaperId,
    pub attempt_kind: AttemptKind,
    pub attempt_no: i32,
    pub status: RegistrationPaperStatus,
}

/// A candidate's registration in a session with its papers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateSummary {
    pub registration_id: ExamRegistrationId,
    pub session_id: ExamSessionId,
    pub institution_id: InstitutionId,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub registration_number: String,
    /// Snapshots taken at registration (no join into the `student` schema).
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub channel: RegistrationChannel,
    pub status: ExamRegistrationStatus,
    pub papers: Vec<CandidatePaper>,
    pub version: i64,
}

impl CandidateSummary {
    /// The candidate's live registration of `paper` (not withdrawn).
    pub fn paper(&self, paper: ExamPaperId) -> Option<&CandidatePaper> {
        self.papers
            .iter()
            .find(|p| p.paper_id == paper && p.status != RegistrationPaperStatus::Withdrawn)
    }
}

/// Read port over the shared session / paper / registration tables ("6A → 6B / 6C").
#[async_trait::async_trait]
pub trait ExamDirectory: Send + Sync + 'static {
    async fn session(
        &self,
        scope: &ExecutionScope,
        id: ExamSessionId,
    ) -> Result<Option<ExamSessionSummary>, KernelError>;

    /// Sessions of an institution, newest first.
    async fn sessions_of_institution(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<ExamSessionSummary>, KernelError>;

    async fn grading_scheme(
        &self,
        scope: &ExecutionScope,
        id: GradingSchemeId,
    ) -> Result<Option<GradingSchemeSummary>, KernelError>;

    async fn paper(
        &self,
        scope: &ExecutionScope,
        id: ExamPaperId,
    ) -> Result<Option<ExamPaperSummary>, KernelError>;

    /// Papers of a session ordered by paper code.
    async fn papers_of_session(
        &self,
        scope: &ExecutionScope,
        session_id: ExamSessionId,
    ) -> Result<Vec<ExamPaperSummary>, KernelError>;

    async fn registration(
        &self,
        scope: &ExecutionScope,
        id: ExamRegistrationId,
    ) -> Result<Option<CandidateSummary>, KernelError>;

    /// Every registration of a session (any status) ordered by registration number.
    async fn registrations_of_session(
        &self,
        scope: &ExecutionScope,
        session_id: ExamSessionId,
    ) -> Result<Vec<CandidateSummary>, KernelError>;

    /// The student's live registration in the session (`is_live`), if any.
    async fn registration_of_student(
        &self,
        scope: &ExecutionScope,
        session_id: ExamSessionId,
        student_id: StudentId,
    ) -> Result<Option<CandidateSummary>, KernelError>;

    /// Registrations whose snapshotted login subject is `subject_ref` (student self-service).
    async fn registrations_of_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Vec<CandidateSummary>, KernelError>;
}

/// The candidate's marks on one registered paper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaperMarkSummary {
    pub id: PaperMarkId,
    pub session_id: ExamSessionId,
    pub paper_id: ExamPaperId,
    pub registration_id: ExamRegistrationId,
    pub registration_paper_id: ExamRegistrationPaperId,
    pub student_id: StudentId,
    /// Internal assessment marks (imported from Domain D or entered) and their maximum.
    pub internal_marks: Option<Decimal>,
    pub internal_max: Option<Decimal>,
    /// External (end-semester) marks; `None` until entered or when absent.
    pub external_marks: Option<Decimal>,
    pub external_max: Decimal,
    /// Grace / moderation adjustment added on top (policy-governed).
    pub grace_marks: Decimal,
    pub absent: bool,
    pub status: MarkStatus,
    /// Domain D internal assessment result the internal marks were imported from.
    pub internal_source_id: Option<Uuid>,
    pub version: i64,
}

/// Read port over `exams.paper_mark` ("6B → 6C").
#[async_trait::async_trait]
pub trait MarksReader: Send + Sync + 'static {
    /// Every candidate's marks on a paper (ordered by registration number).
    async fn marks_of_paper(
        &self,
        scope: &ExecutionScope,
        paper_id: ExamPaperId,
    ) -> Result<Vec<PaperMarkSummary>, KernelError>;

    /// A candidate's marks on every registered paper.
    async fn marks_of_registration(
        &self,
        scope: &ExecutionScope,
        registration_id: ExamRegistrationId,
    ) -> Result<Vec<PaperMarkSummary>, KernelError>;
}
