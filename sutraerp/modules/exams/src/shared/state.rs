//! Shared handler state of every Domain I router.

use crate::shared::approval::ApprovalMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::academic::ports::{AcademicExamFacts, OfferingDirectory};
use sutra_contracts::exams::ports::{ExamDirectory, MarksReader};
use sutra_contracts::finance::ports::{ChargeDirectory, ExamFeePort};
use sutra_contracts::finance::FinanceClearancePort;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::student::ports::{RegistrationReader, StudentDirectory};

/// Handler state: the owner's pool plus every port Domain I calls. Cheap to clone.
#[derive(Clone)]
pub struct ExamsState {
    pub pool: PgPool,
    /// Approval steps (ADR-L5-0023; `NoopWorkflowAdapter` in Batch 6).
    pub workflow: Arc<dyn WorkflowPort>,
    pub approvals: ApprovalMode,
    /// Domain B reference data (institution + timezone, years / terms, curriculum components).
    pub references: Arc<dyn InstitutionReferencePort>,
    /// Domain C students (candidates, live contexts, self-service resolution).
    pub students: Arc<dyn StudentDirectory>,
    /// Domain C (3B) term registrations — examination registration scope and eligibility (6A).
    pub registrations: Arc<dyn RegistrationReader>,
    /// Domain D (4A) offerings, sections, rosters and faculty — papers, candidates, marks-entry
    /// authority (`teaches`).
    pub offerings: Arc<dyn OfferingDirectory>,
    /// Domain D (4B) finalised internal assessment + attendance standing — 6B import, 6A hall
    /// ticket entry conditions.
    pub academic: Arc<dyn AcademicExamFacts>,
    /// Domain F dues (`student_dues`) — 6A hall ticket finance condition.
    pub finance: Arc<dyn FinanceClearancePort>,
    /// Domain F charges — 6C reads whether an exam fee charge is settled.
    pub charges: Arc<dyn ChargeDirectory>,
    /// Domain F exam fee charges — 6C revaluation / rechecking fees.
    pub fees: Arc<dyn ExamFeePort>,
    /// Sessions, papers, grading schemes, registrations (foundation tables).
    pub directory: Arc<dyn ExamDirectory>,
    /// Per-candidate paper marks (foundation table).
    pub marks: Arc<dyn MarksReader>,
}
