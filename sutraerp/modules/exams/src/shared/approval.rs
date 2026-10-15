//! Approval steps through the WorkflowPort (A05 — ADR-L5-0023 / ADR-L5-0042).
//!
//! Domain F owns the decision state; the workflow runtime only orchestrates. Pattern for every
//! governed decision (refund approval — the reference approval of Batch 5 —, adjustment /
//! write-off approval, manual journal approval, aid revocation, period close / reopen, fund
//! transfer, …):
//!
//! 1. Validate the command and the aggregate's current state (read, no lock).
//! 2. Call [`request_approval`] **before** opening the owner-local transaction. The business key
//!    is deterministic (`<aggregate>:<id>:v<version>`), so a retried command reuses it.
//! 3. In one transaction: re-check the version (`If-Match`), move the aggregate to its pending
//!    state (e.g. `pending_approval`, `under_review`) storing `process_instance_id`, emit the
//!    "submitted" event; and **if the outcome is [`ApprovalOutcome::Approved`]** also apply the
//!    decision in the same transaction (final state + decision event, decision actor
//!    [`AUTO_APPROVER`]) — the caller gets the final state back.
//! 4. With [`ApprovalMode::AwaitDecision`] (Batch 9, Flowable) the aggregate stays pending;
//!    a human with the decide/publish permission calls the slice's decision endpoint.
//!
//! The mode comes from `SUTRA_APPROVAL_MODE` (default [`ApprovalMode::AwaitDecision`];
//! [`ApprovalMode::AutoApprove`] is a dev/test opt-in refused in production); tests build a
//! [`ExamsState`] with [`ApprovalMode::AwaitDecision`] to cover the explicit decision endpoint.

use crate::shared::ExamsState;
use std::collections::BTreeMap;
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Actor id recorded for automatic approval decisions.
pub const AUTO_APPROVER: &str = "system:auto-approval";

/// Process definition keys (BPMN ids in the later Flowable deployment). One per decision type.
pub mod process {
    // 6A
    pub const POLICY_APPROVAL: &str = "exams.policy_approval";
    pub const SESSION_APPROVAL: &str = "exams.session_approval";
    pub const SCHEDULE_APPROVAL: &str = "exams.schedule_approval";
    pub const REGISTRATION_REVIEW: &str = "exams.registration_review";
    pub const DEADLINE_OVERRIDE: &str = "exams.deadline_override";
    pub const HALL_TICKET_APPROVAL: &str = "exams.hall_ticket_approval";
    pub const CONTINGENCY_DECISION: &str = "exams.contingency_decision";
    // 6B
    pub const MARKS_APPROVAL: &str = "exams.marks_approval";
    pub const MODERATION_APPROVAL: &str = "exams.moderation_approval";
    pub const MALPRACTICE_DECISION: &str = "exams.malpractice_decision";
    pub const MALPRACTICE_REVIEW: &str = "exams.malpractice_review";
    // 6C
    pub const RESULT_APPROVAL: &str = "exams.result_approval";
    pub const RESULT_PUBLICATION: &str = "exams.result_publication";
    pub const RESULT_WITHDRAWAL: &str = "exams.result_withdrawal";
    pub const RESULT_CORRECTION: &str = "exams.result_correction";
    pub const REVALUATION_DECISION: &str = "exams.revaluation_decision";
    pub const SESSION_CLOSURE: &str = "exams.session_closure";

    /// Every key (unit-tested unique).
    pub const ALL: &[&str] = &[
        POLICY_APPROVAL,
        SESSION_APPROVAL,
        SCHEDULE_APPROVAL,
        REGISTRATION_REVIEW,
        DEADLINE_OVERRIDE,
        HALL_TICKET_APPROVAL,
        CONTINGENCY_DECISION,
        MARKS_APPROVAL,
        MODERATION_APPROVAL,
        MALPRACTICE_DECISION,
        MALPRACTICE_REVIEW,
        RESULT_APPROVAL,
        RESULT_PUBLICATION,
        RESULT_WITHDRAWAL,
        RESULT_CORRECTION,
        REVALUATION_DECISION,
        SESSION_CLOSURE,
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
    state: &ExamsState,
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
        assert_eq!(business_key("mark_sheet", "abc", 3), "mark_sheet:abc:v3");
        let keys: std::collections::HashSet<_> = process::ALL.iter().collect();
        assert_eq!(keys.len(), process::ALL.len());
        assert!(process::ALL.iter().all(|k| k.starts_with("exams.")));
        assert_eq!(ApprovalMode::default(), ApprovalMode::AwaitDecision);
        let o = ApprovalOutcome::Pending {
            process_instance_id: "p".into(),
        };
        assert_eq!(o.process_instance_id(), "p");
    }
}
