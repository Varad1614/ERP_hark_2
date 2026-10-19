//! **5A** — fee structures, charges, instalments, disputes, adjustments, aid, configuration (L2-F01/F04/F11, L3-F-001…006, 020…024, 062…066, 084/085).
//!
//! Owned by slice 5A: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch5-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;
event_type!(
    /// Fee structure version published (L3-F-001).
    FEE_STRUCTURE_PUBLISHED, FEE_STRUCTURE_PUBLISHED_SCHEMA, "fee_structure.published");
event_type!(
    /// Charge published to the student and recognised in the ledger (L3-F-001).
    CHARGE_PUBLISHED, CHARGE_PUBLISHED_SCHEMA, "charge.published");
event_type!(
    /// Governed revision of a published charge (L3-F-002, ModifiedV1).
    CHARGE_REVISED, CHARGE_REVISED_SCHEMA, "charge.revised");
event_type!(
    /// (L3-F-003).
    CHARGE_SUSPENDED, CHARGE_SUSPENDED_SCHEMA, "charge.suspended");
event_type!(
    /// (L3-F-003).
    CHARGE_RESUMED, CHARGE_RESUMED_SCHEMA, "charge.resumed");
event_type!(
    /// (L3-F-003).
    CHARGE_CANCELLED, CHARGE_CANCELLED_SCHEMA, "charge.cancelled");
event_type!(
    /// Dispute raised (L3-F-004).
    CHARGE_DISPUTED, CHARGE_DISPUTED_SCHEMA, "charge.disputed");
event_type!(
    /// Dispute upheld / rejected / withdrawn (L3-F-004).
    CHARGE_DISPUTE_RESOLVED, CHARGE_DISPUTE_RESOLVED_SCHEMA, "charge_dispute.resolved");
event_type!(
    /// Due schedule set (L3-F-005).
    INSTALMENT_PLAN_CREATED, INSTALMENT_PLAN_CREATED_SCHEMA, "instalment_plan.created");
event_type!(
    /// An instalment became due (L3-F-005).
    INSTALMENT_DUE, INSTALMENT_DUE_SCHEMA, "instalment.due");
event_type!(
    /// Settled charge financially closed (L3-F-006).
    CHARGE_CLOSED, CHARGE_CLOSED_SCHEMA, "charge.closed");
event_type!(
    /// (L3-F-006).
    CHARGE_ARCHIVED, CHARGE_ARCHIVED_SCHEMA, "charge.archived");
event_type!(
    /// Adjustment sent for approval through the WorkflowPort (L3-F-020…022).
    ADJUSTMENT_SUBMITTED, ADJUSTMENT_SUBMITTED_SCHEMA, "adjustment.submitted");
event_type!(
    /// Credit/debit note, write-off or waiver posted (L3-F-020…022).
    ADJUSTMENT_POSTED, ADJUSTMENT_POSTED_SCHEMA, "adjustment.posted");
event_type!(
    /// (L3-F-023).
    ADJUSTMENT_REVERSED, ADJUSTMENT_REVERSED_SCHEMA, "adjustment.reversed");
event_type!(
    /// Corrected successor recorded (L3-F-024).
    ADJUSTMENT_SUPERSEDED, ADJUSTMENT_SUPERSEDED_SCHEMA, "adjustment.superseded");
event_type!(
    /// Aid allocated from an approved Domain C decision (L3-F-062).
    AID_ALLOCATED, AID_ALLOCATED_SCHEMA, "aid_allocation.allocated");
event_type!(
    /// Disbursed / applied to charges (L3-F-063).
    AID_DISBURSED, AID_DISBURSED_SCHEMA, "aid_allocation.disbursed");
event_type!(
    /// (L3-F-064).
    AID_RENEWED, AID_RENEWED_SCHEMA, "aid_allocation.renewed");
event_type!(
    /// Revocation with recovery charge (L3-F-065).
    AID_REVOKED, AID_REVOKED_SCHEMA, "aid_allocation.revoked");
event_type!(
    /// Configuration version effective (L3-F-084).
    FINANCE_CONFIG_EFFECTIVE, FINANCE_CONFIG_EFFECTIVE_SCHEMA, "finance_config.effective");
event_type!(
    /// Financial rule effective (L3-F-085).
    FINANCE_RULE_EFFECTIVE, FINANCE_RULE_EFFECTIVE_SCHEMA, "finance_rule.effective");

/// Event types of this slice.
pub const EVENT_TYPES: &[&str] = &[
    FEE_STRUCTURE_PUBLISHED,
    CHARGE_PUBLISHED,
    CHARGE_REVISED,
    CHARGE_SUSPENDED,
    CHARGE_RESUMED,
    CHARGE_CANCELLED,
    CHARGE_DISPUTED,
    CHARGE_DISPUTE_RESOLVED,
    INSTALMENT_PLAN_CREATED,
    INSTALMENT_DUE,
    CHARGE_CLOSED,
    CHARGE_ARCHIVED,
    ADJUSTMENT_SUBMITTED,
    ADJUSTMENT_POSTED,
    ADJUSTMENT_REVERSED,
    ADJUSTMENT_SUPERSEDED,
    AID_ALLOCATED,
    AID_DISBURSED,
    AID_RENEWED,
    AID_REVOKED,
    FINANCE_CONFIG_EFFECTIVE,
    FINANCE_RULE_EFFECTIVE,
];
