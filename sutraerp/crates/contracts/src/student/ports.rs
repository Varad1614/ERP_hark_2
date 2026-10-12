//! Domain C ports.
//!
//! * [`StudentDirectory`] — read port other domains (D, F, I, …) use to resolve a student and its
//!   live StudentContext. Implemented by `sutra_student::PgStudentDirectory` (foundation).
//! * [`AbcRegistryPort`] — Academic Bank of Credits (external, L3-C-037), used by 3B.
//! * [`ExternalScholarshipPort`] — external scholarship ecosystems (e.g. NSP, L3-C-069), used by 3C.
//!
//! The two external ports have **no-op adapters** in Batch 3 (`sutra_student::adapters`), which
//! answer [`ExternalSyncOutcome::NotConfigured`]; real adapters live in `sutra-infra` later.

use super::ids::{StudentContextId, StudentId};
use super::status::{StudentContextStatus, StudentStatus};
use chrono::NaiveDate;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// Minimal persona facts other domains may rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentSummary {
    pub id: StudentId,
    pub student_number: String,
    pub display_name: String,
    pub status: StudentStatus,
    /// Login subject linked for self-service (`dev|aarav.desai@sutra.dev`, Keycloak `sub`).
    pub subject_ref: Option<String>,
}

/// Minimal StudentContext facts other domains may rely on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentContextSummary {
    pub id: StudentContextId,
    pub student_id: StudentId,
    pub institution_id: InstitutionId,
    pub status: StudentContextStatus,
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub current_term_no: Option<i32>,
}

/// Read port over the Student aggregate (tenant from `scope`; foreign/missing → `None`).
#[async_trait::async_trait]
pub trait StudentDirectory: Send + Sync + 'static {
    async fn student(
        &self,
        scope: &ExecutionScope,
        id: StudentId,
    ) -> Result<Option<StudentSummary>, KernelError>;

    /// The student's live (`pending_activation | active | suspended`) context in `institution`.
    async fn live_context(
        &self,
        scope: &ExecutionScope,
        student_id: StudentId,
        institution_id: InstitutionId,
    ) -> Result<Option<StudentContextSummary>, KernelError>;

    /// The student linked to a login subject (self-service), if any.
    async fn by_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Option<StudentSummary>, KernelError>;
}

/// Result of a call to an external registry.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ExternalSyncOutcome {
    Accepted {
        reference: String,
    },
    Rejected {
        reason: String,
    },
    /// No adapter is configured for this deployment (the Batch 3 default).
    NotConfigured,
}

/// One credit-ledger submission to the Academic Bank of Credits.
#[derive(Clone, Debug, PartialEq)]
pub struct AbcCreditSubmission {
    pub institution_id: InstitutionId,
    pub student_id: StudentId,
    /// APAAR / ABC id of the student, when recorded on the student profile.
    pub abc_account_id: Option<String>,
    pub ledger_entry_ids: Vec<Uuid>,
    pub total_credits: f64,
}

#[async_trait::async_trait]
pub trait AbcRegistryPort: Send + Sync + 'static {
    async fn submit_credits(
        &self,
        scope: &ExecutionScope,
        submission: AbcCreditSubmission,
    ) -> Result<ExternalSyncOutcome, KernelError>;
}

/// One synchronisation request to an external scholarship ecosystem.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExternalScholarshipSync {
    pub institution_id: InstitutionId,
    pub scholarship_id: Uuid,
    /// External scheme code (e.g. an NSP scheme id).
    pub external_scheme: String,
    pub student_ids: Vec<StudentId>,
    pub as_of: NaiveDate,
}

#[async_trait::async_trait]
pub trait ExternalScholarshipPort: Send + Sync + 'static {
    async fn synchronise(
        &self,
        scope: &ExecutionScope,
        request: ExternalScholarshipSync,
    ) -> Result<ExternalSyncOutcome, KernelError>;
}
