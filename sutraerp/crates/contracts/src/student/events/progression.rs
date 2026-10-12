//! **3B** — progression, credits, probation, detention, credit transfer, migration, break, eligibility, ABC, policy (L2-C04 + amendment 2026-08-22, L3-C-027…038, C-075).
//!
//! Owned by slice 3B: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// ProgressionEvaluated ([`ProgressionDecisionV1`]).
    PROGRESSION_EVALUATED, PROGRESSION_EVALUATED_SCHEMA, "progression.evaluated");
event_type!(
    /// StudentProgressed ([`ProgressionDecisionV1`]).
    PROGRESSION_STUDENT_PROGRESSED, PROGRESSION_STUDENT_PROGRESSED_SCHEMA, "progression.student_progressed");
event_type!(
    /// ConditionalPromotionGranted ([`ProgressionDecisionV1`]).
    PROGRESSION_CONDITIONAL_PROMOTION_GRANTED, PROGRESSION_CONDITIONAL_PROMOTION_GRANTED_SCHEMA, "progression.conditional_promotion_granted");
event_type!(
    /// AcademicProbationAssigned ([`ProgressionDecisionV1`]).
    PROGRESSION_ACADEMIC_PROBATION_ASSIGNED, PROGRESSION_ACADEMIC_PROBATION_ASSIGNED_SCHEMA, "progression.academic_probation_assigned");
event_type!(
    /// AcademicDetentionApplied ([`ProgressionDecisionV1`]).
    PROGRESSION_ACADEMIC_DETENTION_APPLIED, PROGRESSION_ACADEMIC_DETENTION_APPLIED_SCHEMA, "progression.academic_detention_applied");
event_type!(
    /// Decision superseded by a revision ([`ProgressionDecisionV1`], L3-C-038).
    PROGRESSION_DECISION_REVISED, PROGRESSION_DECISION_REVISED_SCHEMA, "progression.decision_revised");
event_type!(
    /// (StatusChangedV1, L3-C-029).
    PROGRESSION_CONDITION_FULFILLED, PROGRESSION_CONDITION_FULFILLED_SCHEMA, "progression_condition.fulfilled");
event_type!(
    /// (StatusChangedV1).
    PROGRESSION_CONDITION_FAILED, PROGRESSION_CONDITION_FAILED_SCHEMA, "progression_condition.failed");
event_type!(
    /// Probation cleared or escalated (StatusChangedV1, L3-C-030).
    PROBATION_RESOLVED, PROBATION_RESOLVED_SCHEMA, "probation.resolved");
event_type!(
    /// Ledger entries appended ([`CreditLedgerUpdatedV1`], L3-C-028).
    CREDIT_LEDGER_UPDATED, CREDIT_LEDGER_UPDATED_SCHEMA, "credit_ledger.updated");
event_type!(
    /// Credits recognised (StatusChangedV1, L3-C-032).
    CREDIT_TRANSFER_APPROVED, CREDIT_TRANSFER_APPROVED_SCHEMA, "credit_transfer.approved");
event_type!(
    /// ([`ProgrammeMigratedV1`], L3-C-033).
    PROGRAMME_MIGRATION_COMPLETED, PROGRAMME_MIGRATION_COMPLETED_SCHEMA, "programme_migration.completed");
event_type!(
    /// (StatusChangedV1, L3-C-034).
    ACADEMIC_BREAK_STARTED, ACADEMIC_BREAK_STARTED_SCHEMA, "academic_break.started");
event_type!(
    /// (StatusChangedV1, L3-C-035).
    ACADEMIC_BREAK_REJOINED, ACADEMIC_BREAK_REJOINED_SCHEMA, "academic_break.rejoined");
event_type!(
    /// QualificationEligibilityUpdated ([`QualificationEligibilityV1`], L3-C-036). Consumed by 3C.
    QUALIFICATION_ELIGIBILITY_UPDATED, QUALIFICATION_ELIGIBILITY_UPDATED_SCHEMA, "qualification_eligibility.updated");
event_type!(
    /// (StatusChangedV1, L3-C-037).
    ABC_SYNC_COMPLETED, ABC_SYNC_COMPLETED_SCHEMA, "abc_sync.completed");
event_type!(
    /// (StatusChangedV1).
    ABC_SYNC_FAILED, ABC_SYNC_FAILED_SCHEMA, "abc_sync.failed");
event_type!(
    /// (StatusChangedV1, L3-C-075).
    PROGRESSION_POLICY_EFFECTIVE, PROGRESSION_POLICY_EFFECTIVE_SCHEMA, "progression_policy.effective");
event_type!(
    /// (StatusChangedV1).
    PROGRESSION_POLICY_RETIRED, PROGRESSION_POLICY_RETIRED_SCHEMA, "progression_policy.retired");

pub const EVENT_TYPES: &[&str] = &[
    PROGRESSION_EVALUATED,
    PROGRESSION_STUDENT_PROGRESSED,
    PROGRESSION_CONDITIONAL_PROMOTION_GRANTED,
    PROGRESSION_ACADEMIC_PROBATION_ASSIGNED,
    PROGRESSION_ACADEMIC_DETENTION_APPLIED,
    PROGRESSION_DECISION_REVISED,
    PROGRESSION_CONDITION_FULFILLED,
    PROGRESSION_CONDITION_FAILED,
    PROBATION_RESOLVED,
    CREDIT_LEDGER_UPDATED,
    CREDIT_TRANSFER_APPROVED,
    PROGRAMME_MIGRATION_COMPLETED,
    ACADEMIC_BREAK_STARTED,
    ACADEMIC_BREAK_REJOINED,
    QUALIFICATION_ELIGIBILITY_UPDATED,
    ABC_SYNC_COMPLETED,
    ABC_SYNC_FAILED,
    PROGRESSION_POLICY_EFFECTIVE,
    PROGRESSION_POLICY_RETIRED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgressionDecisionV1 {
    pub decision_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Option<Uuid>,
    /// `ProgressionOutcome` wire name.
    pub outcome: String,
    pub policy_id: Option<Uuid>,
    pub supersedes_decision_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub student_subject_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreditLedgerUpdatedV1 {
    pub student_id: Uuid,
    pub institution_id: Uuid,
    pub entry_ids: Vec<Uuid>,
    /// Ledger balance after the append (earned + transferred + recognised − revoked/expired).
    pub total_credits: f64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgrammeMigratedV1 {
    pub migration_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub from_programme_id: Uuid,
    pub to_programme_id: Uuid,
    pub to_curriculum_version_id: Option<Uuid>,
    pub effective_on: NaiveDate,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QualificationEligibilityV1 {
    pub eligibility_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub eligible: bool,
    /// `QualificationOutcome` wire names the student currently qualifies for (MEME aware).
    pub outcomes: Vec<String>,
    pub total_credits: f64,
}
