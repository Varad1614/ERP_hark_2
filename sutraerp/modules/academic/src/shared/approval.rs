//! Approval steps through the WorkflowPort (A05 — ADR-L5-0023 / ADR-L5-0042).
//!
//! Domain D owns the decision state; the workflow runtime only orchestrates. Pattern for every
//! governed decision (offering approval — the reference approval of Batch 4 —, faculty
//! assignment, timetable publication, attendance correction, assessment finalisation, credit
//! recognition, …):
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
//! Batch 4 runs [`ApprovalMode::AutoApprove`] with the platform `NoopWorkflowAdapter`; tests build a
//! [`AcademicState`] with [`ApprovalMode::AwaitDecision`] to cover the explicit decision endpoint.

use crate::shared::AcademicState;
use std::collections::BTreeMap;
use sutra_contracts::platform::workflow::StartProcess;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Actor id recorded for automatic approval decisions.
pub const AUTO_APPROVER: &str = "system:auto-approval";

/// Process definition keys (BPMN ids in the later Flowable deployment). One per decision type.
pub mod process {
    // 4A
    pub const COURSE_OFFERING_APPROVAL: &str = "academic.course_offering_approval";
    pub const COURSE_OFFERING_CANCELLATION: &str = "academic.course_offering_cancellation";
    pub const COURSE_OFFERING_REVISION: &str = "academic.course_offering_revision";
    pub const COURSE_OFFERING_REORGANISATION: &str = "academic.course_offering_reorganisation";
    pub const COURSE_OFFERING_UNFREEZE: &str = "academic.course_offering_unfreeze";
    pub const OFFERING_CHANGE_IMPACT: &str = "academic.offering_change_impact";
    pub const ENROLMENT_ELIGIBILITY_OVERRIDE: &str = "academic.enrolment_eligibility_override";
    pub const FACULTY_ASSIGNMENT_APPROVAL: &str = "academic.faculty_assignment_approval";
    pub const FACULTY_ASSIGNMENT_CHANGE: &str = "academic.faculty_assignment_change";
    pub const FACULTY_ASSIGNMENT_POLICY: &str = "academic.faculty_assignment_policy";
    // 4B
    pub const TIMETABLE_APPROVAL: &str = "academic.timetable_approval";
    pub const TIMETABLE_REVISION: &str = "academic.timetable_revision";
    pub const ATTENDANCE_CORRECTION: &str = "academic.attendance_correction";
    pub const ATTENDANCE_POLICY: &str = "academic.attendance_policy";
    pub const MONITORING_DECISION: &str = "academic.monitoring_decision";
    pub const MONITORING_CLOSURE: &str = "academic.monitoring_closure";
    pub const ASSESSMENT_MODERATION: &str = "academic.assessment_moderation";
    pub const ASSESSMENT_FINALIZATION: &str = "academic.assessment_finalization";
    pub const ASSESSMENT_CORRECTION: &str = "academic.assessment_correction";
    // 4C
    pub const LEARNING_PROVIDER_ONBOARDING: &str = "academic.learning_provider_onboarding";
    pub const LEARNING_OPPORTUNITY_APPROVAL: &str = "academic.learning_opportunity_approval";
    pub const FLEXIBLE_ENROLMENT_APPROVAL: &str = "academic.flexible_enrolment_approval";
    pub const CERTIFICATE_VERIFICATION: &str = "academic.certificate_verification";
    pub const CREDIT_RECOGNITION: &str = "academic.credit_recognition";
    pub const CREDIT_MAPPING_RULE: &str = "academic.credit_mapping_rule";
    pub const FLEXIBLE_LEARNING_POLICY: &str = "academic.flexible_learning_policy";
    pub const FLEXIBLE_EXCEPTION: &str = "academic.flexible_exception";

    /// Every key (unit-tested unique).
    pub const ALL: &[&str] = &[
        COURSE_OFFERING_APPROVAL,
        COURSE_OFFERING_CANCELLATION,
        COURSE_OFFERING_REVISION,
        COURSE_OFFERING_REORGANISATION,
        COURSE_OFFERING_UNFREEZE,
        OFFERING_CHANGE_IMPACT,
        ENROLMENT_ELIGIBILITY_OVERRIDE,
        FACULTY_ASSIGNMENT_APPROVAL,
        FACULTY_ASSIGNMENT_CHANGE,
        FACULTY_ASSIGNMENT_POLICY,
        TIMETABLE_APPROVAL,
        TIMETABLE_REVISION,
        ATTENDANCE_CORRECTION,
        ATTENDANCE_POLICY,
        MONITORING_DECISION,
        MONITORING_CLOSURE,
        ASSESSMENT_MODERATION,
        ASSESSMENT_FINALIZATION,
        ASSESSMENT_CORRECTION,
        LEARNING_PROVIDER_ONBOARDING,
        LEARNING_OPPORTUNITY_APPROVAL,
        FLEXIBLE_ENROLMENT_APPROVAL,
        CERTIFICATE_VERIFICATION,
        CREDIT_RECOGNITION,
        CREDIT_MAPPING_RULE,
        FLEXIBLE_LEARNING_POLICY,
        FLEXIBLE_EXCEPTION,
    ];
}

/// How approval steps resolve.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ApprovalMode {
    /// Batch 4 default: the process is started (no-op adapter) and approved at once.
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
    state: &AcademicState,
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
        assert_eq!(
            business_key("course_offering", "abc", 3),
            "course_offering:abc:v3"
        );
        let keys: std::collections::HashSet<_> = process::ALL.iter().collect();
        assert_eq!(keys.len(), process::ALL.len());
        assert!(process::ALL.iter().all(|k| k.starts_with("academic.")));
        assert_eq!(ApprovalMode::default(), ApprovalMode::AutoApprove);
        let o = ApprovalOutcome::Pending {
            process_instance_id: "p".into(),
        };
        assert_eq!(o.process_instance_id(), "p");
    }
}
