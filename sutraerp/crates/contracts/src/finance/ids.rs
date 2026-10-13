//! Typed UUIDv7 ids of the Batch 5 (Domain F) aggregates (ADR-L5-0036 §3.3).
//!
//! Foundation-owned and **append-only**: a slice that needs a new id adds it at the end of its
//! own block with a `// 5X:` marker; existing ids are never renamed (see
//! `docs/batch5-contracts.md` §4.1).

use sutra_kernel::typed_id;

pub use sutra_kernel::scope::{InstitutionId, TenantId};

// ---- foundation: shared finance aggregates (every slice references these) ----
typed_id!(
    /// Ledger (GL) account of an institution's chart of accounts (L2-F05).
    LedgerAccountId
);
typed_id!(
    /// Fee head: a named fee component with its income account (tuition, hostel, …).
    FeeHeadId
);
typed_id!(
    /// Finance party account of a student or pre-student applicant (receivable sub-ledger).
    StudentAccountId
);
typed_id!(
    /// Student charge — the canonical receivable obligation (L2-F01).
    ChargeId
);
typed_id!(
    /// One fee-head line of a charge.
    ChargeLineId
);
typed_id!(
    /// Posted, balanced, append-only journal entry of the general ledger (L2-F05).
    JournalEntryId
);
typed_id!(
    /// One debit or credit line of a journal entry.
    JournalLineId
);

// ---- 5A: fee structures, charges, instalments, disputes, adjustments, aid, configuration ----
typed_id!(
    /// Versioned fee structure (business obligation template, L3-F-001).
    FeeStructureId
);
typed_id!(
    /// One fee-head amount of a fee structure.
    FeeStructureLineId
);
typed_id!(
    /// Governed revision of a published charge (L3-F-002).
    ChargeRevisionId
);
typed_id!(
    /// Instalment / due schedule of a charge (L3-F-005).
    InstalmentPlanId
);
typed_id!(
    /// One instalment of an instalment plan.
    InstalmentId
);
typed_id!(
    /// Dispute raised against a charge (L3-F-004).
    ChargeDisputeId
);
typed_id!(
    /// Credit note, debit note, write-off or waiver adjustment (L3-F-020…024).
    AdjustmentId
);
typed_id!(
    /// Financial aid allocation from an approved Domain C scholarship decision (L3-F-062).
    AidAllocationId
);
typed_id!(
    /// Aid disbursement or charge adjustment of an allocation (L3-F-063).
    AidDisbursementId
);
typed_id!(
    /// Revocation / recovery of aid (L3-F-065).
    AidRecoveryId
);
typed_id!(
    /// Aid programme funding vs utilisation reconciliation (L3-F-066).
    AidProgrammeReconciliationId
);
typed_id!(
    /// Governed finance configuration version (L3-F-084).
    FinanceConfigVersionId
);
typed_id!(
    /// Governed financial rule / policy version (L3-F-085).
    FinanceRuleId
);

// ---- 5B: payments, receipts, refunds, receivables ----
typed_id!(
    /// A payment attempt / received payment (L3-F-007…013).
    PaymentId
);
typed_id!(
    /// Allocation of a payment amount to one charge (L3-F-008).
    PaymentAllocationId
);
typed_id!(
    /// Receipt issued for a completed payment.
    ReceiptId
);
typed_id!(
    /// Money received without an identified payer or obligation (L3-F-009).
    UnidentifiedReceiptId
);
typed_id!(
    /// Bulk collection population (L3-F-013).
    PaymentBatchId
);
typed_id!(
    /// Payment gateway order of a payment (mock / Razorpay-shaped).
    GatewayOrderId
);
typed_id!(
    /// Refund request through completion (L3-F-014…019).
    RefundId
);
typed_id!(
    /// Accounts receivable recognised from a published charge (L3-F-039).
    ReceivableId
);
typed_id!(
    /// Dunning case of an overdue receivable (L3-F-040).
    DunningCaseId
);
typed_id!(
    /// Write-off decision of an uncollectible receivable (L3-F-042).
    ReceivableWriteOffId
);

// ---- 5C: ledger, periods, treasury ----
typed_id!(
    /// Financial year (L3-F-051).
    FinancialYearId
);
typed_id!(
    /// Financial (accounting) period of a year (L3-F-051…055).
    FinancialPeriodId
);
typed_id!(
    /// Accounting batch of source postings (L3-F-027).
    AccountingBatchId
);
typed_id!(
    /// Manual journal request (L3-F-026).
    ManualJournalId
);
typed_id!(
    /// Source-to-ledger reconciliation run (L3-F-029).
    LedgerReconciliationId
);
typed_id!(
    /// Trial balance snapshot of a period (L3-F-030).
    TrialBalanceId
);
typed_id!(
    /// Treasury bank account (L3-F-056).
    BankAccountId
);
typed_id!(
    /// Bank execution of a payment population (L3-F-057).
    BankExecutionBatchId
);
typed_id!(
    /// Imported bank statement (L3-F-058).
    BankStatementId
);
typed_id!(
    /// One line of a bank statement.
    BankStatementLineId
);
typed_id!(
    /// Bank reconciliation of a statement (L3-F-058).
    BankReconciliationId
);
typed_id!(
    /// Inter-account fund transfer (L3-F-060).
    FundTransferId
);
typed_id!(
    /// Failed / returned / uncertain bank transaction case (L3-F-061).
    BankTransactionId
);
