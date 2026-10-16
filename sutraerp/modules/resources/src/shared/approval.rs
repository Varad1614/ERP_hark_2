//! Approval steps through the WorkflowPort (A05 — ADR-L5-0023 / ADR-L5-0042).
//!
//! Domain J owns the decision state; the workflow runtime only orchestrates. Pattern for every
//! governed decision :
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
//! [`ResourcesState`] with [`ApprovalMode::AwaitDecision`] to cover the explicit decision endpoint.

use crate::shared::ResourcesState;
use std::collections::BTreeMap;
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Actor id recorded for automatic approval decisions.
pub const AUTO_APPROVER: &str = "system:auto-approval";

/// Process definition keys (BPMN ids in the later Flowable deployment). One per decision type.
pub mod process {
    // 8D
    pub const SUPPLIER_APPROVAL: &str = "resources.supplier_approval";
    pub const REQUISITION_APPROVAL: &str = "resources.requisition_approval";
    pub const AWARD_APPROVAL: &str = "resources.award_approval";
    pub const PURCHASE_ORDER_APPROVAL: &str = "resources.purchase_order_approval";
    pub const STOCK_ADJUSTMENT_APPROVAL: &str = "resources.stock_adjustment_approval";
    pub const VENDOR_INVOICE_APPROVAL: &str = "resources.vendor_invoice_approval";
    pub const ASSET_RETIREMENT: &str = "resources.asset_retirement";
    // Part 1 (DEF-J-003): a vendor payment posts only after an independent approval.
    pub const VENDOR_PAYMENT_APPROVAL: &str = "resources.vendor_payment_approval";
    // Part 1 · Stage 3 (DEF-J-004): an approved supplier's bank details change only after an
    // independent approval.
    pub const SUPPLIER_BANK_CHANGE_APPROVAL: &str = "resources.supplier_bank_change_approval";

    /// Every key (unit-tested unique).
    pub const ALL: &[&str] = &[
        SUPPLIER_APPROVAL,
        REQUISITION_APPROVAL,
        AWARD_APPROVAL,
        PURCHASE_ORDER_APPROVAL,
        STOCK_ADJUSTMENT_APPROVAL,
        VENDOR_INVOICE_APPROVAL,
        ASSET_RETIREMENT,
        VENDOR_PAYMENT_APPROVAL,
        SUPPLIER_BANK_CHANGE_APPROVAL,
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
    state: &ResourcesState,
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
    // Part 1 · S3-appr: who asked for this decision (separation of duties on its decision path).
    sutra_persistence::approval::record_request(
        &state.pool,
        scope.tenant_id.as_uuid(),
        &r.process_definition_key,
        &r.business_key,
        &r.process_instance_id,
        sutra_kernel::approval::current_requester().as_deref(),
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
/// the aggregate or moved it into one of `pending` statuses (from `resources.status_history`), plus
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
        sutra_persistence::approval::HistoryTable::Resources,
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
        assert!(process::ALL.iter().all(|k| k.starts_with("resources.")));
        assert_eq!(ApprovalMode::default(), ApprovalMode::AwaitDecision);
        let o = ApprovalOutcome::Pending {
            process_instance_id: "p".into(),
        };
        assert_eq!(o.process_instance_id(), "p");
    }
}
