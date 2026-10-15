//! Approval steps through the WorkflowPort (A05 — ADR-L5-0023 / ADR-L5-0042).
//!
//! Domain E owns the decision state; the workflow runtime only orchestrates. Pattern for every
//! governed decision (leave approval — the reference approval of Batch 7 —, engagement activation,
//! payroll approval, separation, policy activation, …):
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
//! [`PeopleState`] with [`ApprovalMode::AwaitDecision`] to cover the explicit decision endpoint.

use crate::shared::PeopleState;
use std::collections::BTreeMap;
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Actor id recorded for automatic approval decisions.
pub const AUTO_APPROVER: &str = "system:auto-approval";

/// Process definition keys (BPMN ids in the later Flowable deployment). One per decision type.
pub mod process {
    // 7A
    pub const ENGAGEMENT_ACTIVATION: &str = "people.engagement_activation";
    pub const ENGAGEMENT_CHANGE: &str = "people.engagement_change";
    pub const POSITION_ASSIGNMENT: &str = "people.position_assignment";
    pub const REQUISITION_APPROVAL: &str = "people.requisition_approval";
    pub const ONBOARDING_ACCEPTANCE: &str = "people.onboarding_acceptance";
    pub const PROBATION_DECISION: &str = "people.probation_decision";
    pub const SEPARATION_APPROVAL: &str = "people.separation_approval";
    pub const CERTIFICATE_APPROVAL: &str = "people.certificate_approval";
    // 7B
    pub const LEAVE_APPROVAL: &str = "people.leave_approval";
    pub const LEAVE_POLICY_APPROVAL: &str = "people.leave_policy_approval";
    pub const LEAVE_ADJUSTMENT_APPROVAL: &str = "people.leave_adjustment_approval";
    pub const ENCASHMENT_DECISION: &str = "people.encashment_decision";
    pub const LEAVE_CASE_DECISION: &str = "people.leave_case_decision";
    pub const WORKLOAD_PLAN_APPROVAL: &str = "people.workload_plan_approval";
    // 7C
    pub const PAYROLL_CONFIG_APPROVAL: &str = "people.payroll_config_approval";
    pub const PAYROLL_APPROVAL: &str = "people.payroll_approval";
    pub const PAYROLL_ADJUSTMENT_APPROVAL: &str = "people.payroll_adjustment_approval";
    pub const OFF_CYCLE_APPROVAL: &str = "people.off_cycle_approval";
    pub const FINAL_SETTLEMENT_APPROVAL: &str = "people.final_settlement_approval";
    // Part 1 (DEF-E-007): payroll disbursement batches (net pay transfer / statutory remittance).
    pub const PAYROLL_DISBURSEMENT_APPROVAL: &str = "people.payroll_disbursement_approval";

    /// Every key (unit-tested unique).
    pub const ALL: &[&str] = &[
        ENGAGEMENT_ACTIVATION,
        ENGAGEMENT_CHANGE,
        POSITION_ASSIGNMENT,
        REQUISITION_APPROVAL,
        ONBOARDING_ACCEPTANCE,
        PROBATION_DECISION,
        SEPARATION_APPROVAL,
        CERTIFICATE_APPROVAL,
        LEAVE_APPROVAL,
        LEAVE_POLICY_APPROVAL,
        LEAVE_ADJUSTMENT_APPROVAL,
        ENCASHMENT_DECISION,
        LEAVE_CASE_DECISION,
        WORKLOAD_PLAN_APPROVAL,
        PAYROLL_CONFIG_APPROVAL,
        PAYROLL_APPROVAL,
        PAYROLL_ADJUSTMENT_APPROVAL,
        OFF_CYCLE_APPROVAL,
        FINAL_SETTLEMENT_APPROVAL,
        PAYROLL_DISBURSEMENT_APPROVAL,
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
    state: &PeopleState,
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

/// Part 1 (DEF-X-002): separation of duties on the shared decision path. Refuses (`403`) a
/// decision by an unidentified / system actor or by one of the request's makers: whoever created
/// the aggregate or moved it into one of `pending` statuses (from `people.status_history`), plus
/// `also` (e.g. the aggregate's own `requested_by` / `created_by` column). `what` names the
/// governed kind in the problem detail.
#[allow(clippy::too_many_arguments)]
pub async fn ensure_independent_decider(
    conn: &mut sqlx::PgConnection,
    tenant: sutra_kernel::scope::TenantId,
    aggregate_type: &str,
    aggregate_id: uuid::Uuid,
    pending: &[&str],
    also: &[Option<&str>],
    decider: Option<&str>,
    what: &str,
) -> Result<(), KernelError> {
    let mut makers = sutra_persistence::approval::requesters(
        conn,
        sutra_persistence::approval::HistoryTable::People,
        tenant.as_uuid(),
        aggregate_type,
        aggregate_id,
        pending,
    )
    .await?;
    makers.extend(also.iter().flatten().map(|s| (*s).to_owned()));
    sutra_kernel::approval::ensure_independent_decider(
        decider,
        makers.iter().map(String::as_str),
        what,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn business_keys_are_deterministic() {
        assert_eq!(
            business_key("leave_request", "abc", 3),
            "leave_request:abc:v3"
        );
        let keys: std::collections::HashSet<_> = process::ALL.iter().collect();
        assert_eq!(keys.len(), process::ALL.len());
        assert!(process::ALL.iter().all(|k| k.starts_with("people.")));
        assert_eq!(ApprovalMode::default(), ApprovalMode::AwaitDecision);
        let o = ApprovalOutcome::Pending {
            process_instance_id: "p".into(),
        };
        assert_eq!(o.process_instance_id(), "p");
    }
}
