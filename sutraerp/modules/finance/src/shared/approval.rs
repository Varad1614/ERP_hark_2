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
//! Batch 5 runs [`ApprovalMode::AutoApprove`] with the platform `NoopWorkflowAdapter`; tests build a
//! [`FinanceState`] with [`ApprovalMode::AwaitDecision`] to cover the explicit decision endpoint.

use crate::shared::FinanceState;
use std::collections::BTreeMap;
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Actor id recorded for automatic approval decisions.
pub const AUTO_APPROVER: &str = "system:auto-approval";

/// Process definition keys (BPMN ids in the later Flowable deployment). One per decision type.
pub mod process {
    // 5A
    pub const FEE_STRUCTURE_APPROVAL: &str = "finance.fee_structure_approval";
    pub const CHARGE_PUBLICATION: &str = "finance.charge_publication";
    pub const CHARGE_REVISION: &str = "finance.charge_revision";
    pub const CHARGE_CANCELLATION: &str = "finance.charge_cancellation";
    pub const CHARGE_DISPUTE_RESOLUTION: &str = "finance.charge_dispute_resolution";
    pub const ADJUSTMENT_APPROVAL: &str = "finance.adjustment_approval";
    pub const ADJUSTMENT_REVERSAL: &str = "finance.adjustment_reversal";
    pub const AID_ALLOCATION: &str = "finance.aid_allocation";
    pub const AID_REVOCATION: &str = "finance.aid_revocation";
    pub const FINANCE_CONFIG_CHANGE: &str = "finance.finance_config_change";
    pub const FINANCE_RULE_CHANGE: &str = "finance.finance_rule_change";
    // 5B
    pub const PAYMENT_REVERSAL: &str = "finance.payment_reversal";
    pub const PAYMENT_BATCH_APPROVAL: &str = "finance.payment_batch_approval";
    pub const REFUND_APPROVAL: &str = "finance.refund_approval";
    pub const REFUND_REVERSAL: &str = "finance.refund_reversal";
    pub const RECEIVABLE_WRITE_OFF: &str = "finance.receivable_write_off";
    pub const RECEIVABLE_REOPEN: &str = "finance.receivable_reopen";
    // 5C
    pub const MANUAL_JOURNAL_APPROVAL: &str = "finance.manual_journal_approval";
    pub const JOURNAL_REVERSAL: &str = "finance.journal_reversal";
    pub const ACCOUNTING_BATCH_APPROVAL: &str = "finance.accounting_batch_approval";
    pub const PERIOD_CLOSE: &str = "finance.period_close";
    pub const PERIOD_REOPEN: &str = "finance.period_reopen";
    pub const YEAR_CLOSE: &str = "finance.year_close";
    pub const BANK_ACCOUNT_APPROVAL: &str = "finance.bank_account_approval";
    pub const FUND_TRANSFER_APPROVAL: &str = "finance.fund_transfer_approval";

    /// Every key (unit-tested unique).
    pub const ALL: &[&str] = &[
        FEE_STRUCTURE_APPROVAL,
        CHARGE_PUBLICATION,
        CHARGE_REVISION,
        CHARGE_CANCELLATION,
        CHARGE_DISPUTE_RESOLUTION,
        ADJUSTMENT_APPROVAL,
        ADJUSTMENT_REVERSAL,
        AID_ALLOCATION,
        AID_REVOCATION,
        FINANCE_CONFIG_CHANGE,
        FINANCE_RULE_CHANGE,
        PAYMENT_REVERSAL,
        PAYMENT_BATCH_APPROVAL,
        REFUND_APPROVAL,
        REFUND_REVERSAL,
        RECEIVABLE_WRITE_OFF,
        RECEIVABLE_REOPEN,
        MANUAL_JOURNAL_APPROVAL,
        JOURNAL_REVERSAL,
        ACCOUNTING_BATCH_APPROVAL,
        PERIOD_CLOSE,
        PERIOD_REOPEN,
        YEAR_CLOSE,
        BANK_ACCOUNT_APPROVAL,
        FUND_TRANSFER_APPROVAL,
    ];
}

/// How approval steps resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Batch 5 default: the process is started (no-op adapter) and approved at once.
    #[default]
    AutoApprove,
    /// The aggregate stays pending until a decision endpoint is called.
    AwaitDecision,
}

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
    state: &FinanceState,
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
        assert_eq!(business_key("refund", "abc", 3), "refund:abc:v3");
        let keys: std::collections::HashSet<_> = process::ALL.iter().collect();
        assert_eq!(keys.len(), process::ALL.len());
        assert!(process::ALL.iter().all(|k| k.starts_with("finance.")));
        assert_eq!(ApprovalMode::default(), ApprovalMode::AutoApprove);
        let o = ApprovalOutcome::Pending {
            process_instance_id: "p".into(),
        };
        assert_eq!(o.process_instance_id(), "p");
    }
}
