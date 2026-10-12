//! Approval steps through the WorkflowPort (A05 — ADR-L5-0023 / ADR-L5-0042).
//!
//! Domain B owns the decision state; the workflow runtime only orchestrates. Pattern for every
//! governed decision (institution activation, campus activation, structure transformation,
//! curriculum / structure / school-model publication, calendar publication, exception approval):
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
//! Batch 2 runs [`ApprovalMode::AutoApprove`] with the platform `NoopWorkflowAdapter`.

use crate::shared::InstitutionState;
use std::collections::BTreeMap;
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Actor id recorded for automatic approval decisions.
pub const AUTO_APPROVER: &str = "system:auto-approval";

/// Process definition keys (BPMN ids in the later Flowable deployment). One per decision type.
pub mod process {
    // 2A
    pub const INSTITUTION_ACTIVATION: &str = "institution.activation";
    pub const ORG_RESTRUCTURE: &str = "institution.org_restructure";
    pub const ORG_TYPE_BLUEPRINT_PUBLICATION: &str = "institution.org_type_blueprint_publication";
    // 2B
    pub const CAMPUS_ACTIVATION: &str = "institution.campus_activation";
    pub const CAMPUS_DECOMMISSION: &str = "institution.campus_decommission";
    pub const CURRICULUM_PUBLICATION: &str = "institution.curriculum_publication";
    pub const ACADEMIC_STRUCTURE_PUBLICATION: &str = "institution.academic_structure_publication";
    pub const SCHOOL_MODEL_PUBLICATION: &str = "institution.school_model_publication";
    // 2C
    pub const CALENDAR_PUBLICATION: &str = "institution.calendar_publication";
    pub const CALENDAR_EXCEPTION: &str = "institution.calendar_exception";
    pub const ACADEMIC_YEAR_CLOSURE: &str = "institution.academic_year_closure";
}

/// How approval steps resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Batch 2: the process is started (no-op adapter) and the decision is approved at once.
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
    state: &InstitutionState,
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
        assert_eq!(business_key("campus", "abc", 3), "campus:abc:v3");
        assert_eq!(ApprovalMode::default(), ApprovalMode::AutoApprove);
        let o = ApprovalOutcome::Pending {
            process_instance_id: "p".into(),
        };
        assert_eq!(o.process_instance_id(), "p");
    }
}
