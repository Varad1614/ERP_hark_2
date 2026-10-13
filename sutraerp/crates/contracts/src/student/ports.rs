//! Domain C ports.
//!
//! * [`StudentDirectory`] — read port other domains (D, F, I, …) use to resolve a student and its
//!   live StudentContext. Implemented by `sutra_student::PgStudentDirectory` (foundation).
//! * [`AbcRegistryPort`] — Academic Bank of Credits (external, L3-C-037), used by 3B.
//! * [`ExternalScholarshipPort`] — external scholarship ecosystems (e.g. NSP, L3-C-069), used by 3C.
//!
//! * [`RegistrationReader`] (Batch 4) — Domain D's read of 3B registrations: 4A gates offering
//!   enrolment on the student's registered curriculum components (no cross-schema SQL).
//!   Implemented by `sutra_student::PgRegistrationReader`.
//! * [`CreditPostingPort`] (Batch 4) — Domain D (4C) posts recognised flexible-learning credits
//!   into 3B's credit ledger. Implemented by `sutra_student::PgCreditPosting`.
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

// ---- Batch 4 foundation (Domain D reads registrations and posts recognised credits) ----

/// One curriculum component on a student's registration for a term.
#[derive(Clone, Debug, PartialEq)]
pub struct RegisteredComponent {
    pub registration_line_id: Uuid,
    pub curriculum_component_id: Uuid,
    /// `RegistrationLineKind` wire name (`regular`, `backlog`, `repeat`, `audit`, `elective`).
    pub kind: String,
    /// `RegistrationLineStatus` wire name; only `selected` / `registered` lines are live.
    pub status: String,
    pub credits: f64,
}

/// A student's (non-cancelled) registration for one academic term.
#[derive(Clone, Debug, PartialEq)]
pub struct TermRegistration {
    pub registration_id: Uuid,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub institution_id: InstitutionId,
    pub academic_term_id: Uuid,
    /// `RegistrationStatus` wire name.
    pub status: String,
    pub lines: Vec<RegisteredComponent>,
}

impl TermRegistration {
    /// The registration counts for offering enrolment (`approved | registered | finalised`).
    pub fn is_effective(&self) -> bool {
        matches!(
            self.status.as_str(),
            "approved" | "registered" | "finalised"
        )
    }

    /// `true` when the component is on a live line (`selected | registered`) of an effective registration.
    pub fn covers(&self, curriculum_component_id: Uuid) -> bool {
        self.is_effective()
            && self.lines.iter().any(|l| {
                l.curriculum_component_id == curriculum_component_id
                    && matches!(l.status.as_str(), "selected" | "registered")
            })
    }
}

/// Read port over 3B registrations (tenant from `scope`; none → `None`).
#[async_trait::async_trait]
pub trait RegistrationReader: Send + Sync + 'static {
    /// The student's non-cancelled registration for `academic_term_id`, with all its lines.
    async fn term_registration(
        &self,
        scope: &ExecutionScope,
        student_id: StudentId,
        academic_term_id: Uuid,
    ) -> Result<Option<TermRegistration>, KernelError>;
}

/// Credits recognised by another domain (Domain D flexible / external learning, L3-D-039).
#[derive(Clone, Debug, PartialEq)]
pub struct CreditPostingRequest {
    pub institution_id: InstitutionId,
    pub student_id: StudentId,
    /// Credits to add (> 0, ≤ 1000).
    pub credits: f64,
    /// Curriculum component the credits count for, when mapped.
    pub curriculum_component_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    /// Origin of the posting, e.g. [`CREDIT_SOURCE_FLEXIBLE_LEARNING`].
    pub source_type: &'static str,
    /// The posting domain's aggregate id (e.g. the credit recognition id): **idempotency key**.
    pub source_id: Uuid,
    pub note: Option<String>,
    /// Actor recorded on the ledger entry (`recorded_by`).
    pub actor: Option<String>,
}

/// `source_type` of credits recognised by Domain D (4C).
pub const CREDIT_SOURCE_FLEXIBLE_LEARNING: &str = "academic_flexible_learning";

/// Result of a posting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreditPostingReceipt {
    pub ledger_entry_id: Uuid,
    /// `true` when `(source_type, source_id)` had already been posted (no second entry written).
    pub already_posted: bool,
}

/// Write port into the Domain C credit ledger (3B's `credit_ledger_entry`, kind `recognised`).
/// Runs its own owner-local transaction in Domain C (ledger entry + `credit_ledger.updated`
/// event + audit); the caller records the receipt in its own transaction afterwards.
/// Errors: unknown student / no live context in the institution → `NotFound` / `Conflict`.
#[async_trait::async_trait]
pub trait CreditPostingPort: Send + Sync + 'static {
    async fn post_credits(
        &self,
        scope: &ExecutionScope,
        request: CreditPostingRequest,
    ) -> Result<CreditPostingReceipt, KernelError>;
}
