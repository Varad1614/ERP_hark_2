//! **5B** — payments, receipts, refunds, receivables (L2-F02/F03/F07, L3-F-007…019, 039…043).
//!
//! Owned by slice 5B: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch5-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;
event_type!(
    /// Payment started (gateway order created / counter entry) (L3-F-007).
    PAYMENT_INITIATED, PAYMENT_INITIATED_SCHEMA, "payment.initiated");
event_type!(
    /// Payment captured, allocated and receipted (L3-F-007/008).
    PAYMENT_COMPLETED, PAYMENT_COMPLETED_SCHEMA, "payment.completed");
event_type!(
    /// Failure / timeout recovered safely (L3-F-010).
    PAYMENT_FAILED, PAYMENT_FAILED_SCHEMA, "payment.failed");
event_type!(
    /// (L3-F-012).
    PAYMENT_CANCELLED, PAYMENT_CANCELLED_SCHEMA, "payment.cancelled");
event_type!(
    /// Reversal or chargeback (L3-F-011).
    PAYMENT_REVERSED, PAYMENT_REVERSED_SCHEMA, "payment.reversed");
event_type!(
    /// Receipt issued (L3-F-007).
    RECEIPT_ISSUED, RECEIPT_ISSUED_SCHEMA, "receipt.issued");
event_type!(
    /// Unidentified receipt identified and allocated / refunded (L3-F-009).
    UNIDENTIFIED_RECEIPT_RESOLVED, UNIDENTIFIED_RECEIPT_RESOLVED_SCHEMA, "unidentified_receipt.resolved");
event_type!(
    /// Bulk collection outcome (L3-F-013).
    PAYMENT_BATCH_COMPLETED, PAYMENT_BATCH_COMPLETED_SCHEMA, "payment_batch.completed");
event_type!(
    /// (L3-F-014).
    REFUND_REQUESTED, REFUND_REQUESTED_SCHEMA, "refund.requested");
event_type!(
    /// Approved / rejected (L3-F-014).
    REFUND_DECIDED, REFUND_DECIDED_SCHEMA, "refund.decided");
event_type!(
    /// (L3-F-015).
    REFUND_COMPLETED, REFUND_COMPLETED_SCHEMA, "refund.completed");
event_type!(
    /// (L3-F-016).
    REFUND_CANCELLED, REFUND_CANCELLED_SCHEMA, "refund.cancelled");
event_type!(
    /// (L3-F-017).
    REFUND_FAILED, REFUND_FAILED_SCHEMA, "refund.failed");
event_type!(
    /// (L3-F-018).
    REFUND_REVERSED, REFUND_REVERSED_SCHEMA, "refund.reversed");
event_type!(
    /// (L3-F-039).
    RECEIVABLE_RECOGNISED, RECEIVABLE_RECOGNISED_SCHEMA, "receivable.recognised");
event_type!(
    /// (L3-F-040).
    RECEIVABLE_OVERDUE, RECEIVABLE_OVERDUE_SCHEMA, "receivable.overdue");
event_type!(
    /// (L3-F-040).
    DUNNING_NOTICE_SENT, DUNNING_NOTICE_SENT_SCHEMA, "dunning.notice_sent");
event_type!(
    /// ERP_EXTENSIONS 1D: a late fee was charged on an overdue charge (the new fee charge;
    /// `chargeId` = the fee charge, `reason` names the late charge).
    LATE_FEE_CHARGED, LATE_FEE_CHARGED_SCHEMA, "late_fee.charged");
event_type!(
    /// ERP_EXTENSIONS 1D: a reminder before the due date of a charge.
    FEE_REMINDER_SENT, FEE_REMINDER_SENT_SCHEMA, "fee_reminder.sent");
event_type!(
    /// (L3-F-041).
    RECEIVABLE_SETTLED, RECEIVABLE_SETTLED_SCHEMA, "receivable.settled");
event_type!(
    /// (L3-F-042).
    RECEIVABLE_WRITTEN_OFF, RECEIVABLE_WRITTEN_OFF_SCHEMA, "receivable.written_off");
event_type!(
    /// (L3-F-043).
    RECEIVABLE_REOPENED, RECEIVABLE_REOPENED_SCHEMA, "receivable.reopened");

/// Event types of this slice.
pub const EVENT_TYPES: &[&str] = &[
    PAYMENT_INITIATED,
    PAYMENT_COMPLETED,
    PAYMENT_FAILED,
    PAYMENT_CANCELLED,
    PAYMENT_REVERSED,
    RECEIPT_ISSUED,
    UNIDENTIFIED_RECEIPT_RESOLVED,
    PAYMENT_BATCH_COMPLETED,
    REFUND_REQUESTED,
    REFUND_DECIDED,
    REFUND_COMPLETED,
    REFUND_CANCELLED,
    REFUND_FAILED,
    REFUND_REVERSED,
    RECEIVABLE_RECOGNISED,
    RECEIVABLE_OVERDUE,
    DUNNING_NOTICE_SENT,
    LATE_FEE_CHARGED,
    FEE_REMINDER_SENT,
    RECEIVABLE_SETTLED,
    RECEIVABLE_WRITTEN_OFF,
    RECEIVABLE_REOPENED,
];
