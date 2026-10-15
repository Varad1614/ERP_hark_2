//! Approval steps through the WorkflowPort (A05 — ADR-L5-0023 / ADR-L5-0042).
//!
//! Domain C owns the decision state; the workflow runtime only orchestrates. Pattern for every
//! governed decision (admission approval — the first BPMN-style approval of the product —,
//! revocation, transfer, identity correction, progression decision, exit, conferment, …):
//!
//! 1. Validate the command and the aggregate's current state (read, no lock).
//! 2. Call [`request_approval`] **before** opening the owner-local transaction. The business key
//!    is deterministic (`<aggregate>:<id>:v<version>`), so a retried command reuses it.
//! 3. In one transaction: re-check the version (`If-Match`), move the aggregate to its pending
//!    state (e.g. `pending_approval`, `in_review`) storing `process_instance_id`, emit the
//!    "submitted" event; and **if the outcome is [`ApprovalOutcome::Approved`]** also apply the
//!    decision in the same transaction (final state + decision event, decision actor
//!    [`AUTO_APPROVER`]) — the caller gets the final state back.
//! 4. With [`ApprovalMode::AwaitDecision`] (later batches, Flowable) the aggregate stays pending;
//!    a human with the decide/publish permission calls the slice's decision endpoint.
//!
//! The mode comes from `SUTRA_APPROVAL_MODE` (default [`ApprovalMode::AwaitDecision`];
//! [`ApprovalMode::AutoApprove`] is a dev/test opt-in refused in production); tests build a
//! [`StudentState`] with [`ApprovalMode::AwaitDecision`] to cover the explicit decision endpoint.

use crate::shared::StudentState;
use std::collections::BTreeMap;
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Actor id recorded for automatic approval decisions.
pub const AUTO_APPROVER: &str = "system:auto-approval";

/// Process definition keys (BPMN ids in the later Flowable deployment). One per decision type.
pub mod process {
    // 3A
    pub const ADMISSION_APPROVAL: &str = "student.admission_approval";
    pub const ADMISSION_REVOCATION: &str = "student.admission_revocation";
    pub const STUDENT_SUSPENSION: &str = "student.student_suspension";
    pub const STUDENT_TRANSFER: &str = "student.student_transfer";
    pub const IDENTITY_CORRECTION: &str = "student.identity_correction";
    pub const DUPLICATE_RESOLUTION: &str = "student.duplicate_resolution";
    pub const STUDENT_ARCHIVAL: &str = "student.student_archival";
    // 3B
    pub const REGISTRATION_APPROVAL: &str = "student.registration_approval";
    pub const PROGRESSION_DECISION: &str = "student.progression_decision";
    pub const PROGRESSION_REVISION: &str = "student.progression_revision";
    pub const CREDIT_TRANSFER: &str = "student.credit_transfer";
    pub const PROGRAMME_MIGRATION: &str = "student.programme_migration";
    pub const ACADEMIC_BREAK: &str = "student.academic_break";
    pub const PROGRESSION_POLICY: &str = "student.progression_policy";
    // 3C
    pub const QUALIFICATION_COMPLETION: &str = "student.qualification_completion";
    pub const QUALIFICATION_REVOCATION: &str = "student.qualification_revocation";
    pub const STUDENT_EXIT: &str = "student.student_exit";
    pub const GUARDIAN_VERIFICATION: &str = "student.guardian_verification";
    pub const SCHOLARSHIP_POLICY: &str = "student.scholarship_policy";
    pub const SCHOLARSHIP_RECOMMENDATION: &str = "student.scholarship_recommendation";
    pub const ALUMNI_TRANSITION: &str = "student.alumni_transition";

    /// Every key (unit-tested unique).
    pub const ALL: &[&str] = &[
        ADMISSION_APPROVAL,
        ADMISSION_REVOCATION,
        STUDENT_SUSPENSION,
        STUDENT_TRANSFER,
        IDENTITY_CORRECTION,
        DUPLICATE_RESOLUTION,
        STUDENT_ARCHIVAL,
        REGISTRATION_APPROVAL,
        PROGRESSION_DECISION,
        PROGRESSION_REVISION,
        CREDIT_TRANSFER,
        PROGRAMME_MIGRATION,
        ACADEMIC_BREAK,
        PROGRESSION_POLICY,
        QUALIFICATION_COMPLETION,
        QUALIFICATION_REVOCATION,
        STUDENT_EXIT,
        GUARDIAN_VERIFICATION,
        SCHOLARSHIP_POLICY,
        SCHOLARSHIP_RECOMMENDATION,
        ALUMNI_TRANSITION,
    ];
}

/// How approval steps resolve: the one config-driven mode shared by every module
/// (`SUTRA_APPROVAL_MODE`; default human decision, auto-approval is a dev/test opt-in).
pub use sutra_kernel::approval::ApprovalMode;

/// Result of [`request_approval`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// Apply the decision now (auto-approval), recording `process_instance_id`.
    Approved { process_instance_id: String },
    /// Leave the aggregate pending; a decision endpoint completes it later.
    Pending { process_instance_id: String },
}

impl ApprovalOutcome {
    pub fn process_instance_id(&self) -> &str {
        match self {
            Self::Approved {
                process_instance_id,
            }
            | Self::Pending {
                process_instance_id,
            } => process_instance_id,
        }
    }
}

/// Deterministic business key `<aggregate>:<id>:v<version>`.
pub fn business_key(aggregate: &str, id: impl std::fmt::Display, version: i64) -> String {
    format!("{aggregate}:{id}:v{version}")
}

/// Starts the approval process for one decision and says whether to apply it now.
pub async fn request_approval(
    state: &StudentState,
    scope: &ExecutionScope,
    process_definition_key: &'static str,
    business_key: String,
    variables: BTreeMap<String, serde_json::Value>,
) -> Result<ApprovalOutcome, KernelError> {
    let r = state
        .workflow
        .start_process(
            scope,
            StartProcess {
                process_definition_key: process_definition_key.to_owned(),
                business_key,
                variables,
            },
        )
        .await?;
    Ok(match state.approvals {
        ApprovalMode::AutoApprove => ApprovalOutcome::Approved {
            process_instance_id: r.process_instance_id,
        },
        ApprovalMode::AwaitDecision => ApprovalOutcome::Pending {
            process_instance_id: r.process_instance_id,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_keys_are_deterministic() {
        assert_eq!(business_key("admission", "abc", 3), "admission:abc:v3");
        let keys: std::collections::HashSet<_> = process::ALL.iter().collect();
        assert_eq!(keys.len(), process::ALL.len());
        assert!(process::ALL.iter().all(|k| k.starts_with("student.")));
        assert_eq!(ApprovalMode::default(), ApprovalMode::AwaitDecision);
        let o = ApprovalOutcome::Pending {
            process_instance_id: "p".into(),
        };
        assert_eq!(o.process_instance_id(), "p");
    }
}
