//! Lifecycle state and classification enums of the Batch 5 (Domain F) aggregates.
//!
//! Foundation-owned. Wire/DB form is the `snake_case` name, stored in a `text` column with a
//! `CHECK (… IN (...))` written by the owning migration. Adding a variant is a contract change
//! (edit `docs/batch5-contracts.md` §4.2).
//!
//! Transitions: the **shared** machine [`ChargeStatus`] carries its legal transitions here
//! (`can_transition`) because charges live in a foundation table that 5B and 5C read and gate
//! on; only **5A** moves it, through `sutra_finance::shared::lifecycle::change_charge_status`.
//! Every other machine's transitions are slice-owned business rules written as a pure,
//! unit-tested `can_transition(from, to)` in the owning slice's `domain.rs`; an illegal
//! transition is `409 conflict`.

/// Declares a status enum with `ALL`, `as_str`, `parse` and serde/OpenAPI `snake_case` names.
macro_rules! status_enum {
    ($(#[$meta:meta])* $name:ident { $($(#[$vmeta:meta])* $variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($(#[$vmeta])* $variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

// ================================================================ foundation

status_enum!(
    /// Student charge lifecycle (L2-F01 §18–19 canonical states and transitions).
    ChargeStatus {
        Draft => "draft",
        PendingValidation => "pending_validation",
        Validated => "validated",
        /// Visible to the student and recognised as a receivable (posted to the ledger).
        Published => "published",
        /// Financially active (first settlement, instalment due or explicit activation).
        Active => "active",
        Suspended => "suspended",
        Disputed => "disputed",
        Cancelled => "cancelled",
        Waived => "waived",
        WrittenOff => "written_off",
        /// Fully settled / resolved and financially closed (L3-F-006).
        Closed => "closed",
        Archived => "archived",
    }
);

impl ChargeStatus {
    /// Legal transitions (L2-F01 §19.4). `published`/`active` both may branch into the
    /// exception states; exception states return to `active` or end.
    pub fn can_transition(self, to: ChargeStatus) -> bool {
        use ChargeStatus::*;
        matches!(
            (self, to),
            (Draft, PendingValidation)
                | (Draft, Cancelled)
                | (PendingValidation, Validated)
                | (PendingValidation, Draft)
                | (Validated, Published)
                | (Validated, Draft)
                | (Validated, Cancelled)
                | (Published, Active)
                | (Published | Active, Suspended)
                | (Published | Active, Disputed)
                | (Published | Active, Cancelled)
                | (Published | Active, Waived)
                | (Published | Active, WrittenOff)
                | (Published | Active, Closed)
                | (Suspended, Active)
                | (Suspended, Cancelled)
                | (Disputed, Active)
                | (Disputed, Cancelled)
                | (Disputed, Waived)
                // A reversed settlement re-opens a closed charge (L3-F-011 / F-043); done only by
                // `shared::charge::apply_settlement`.
                | (Closed, Active)
                | (Cancelled | Waived | WrittenOff | Closed, Archived)
        )
    }

    /// Statuses whose outstanding amount is owed by the party (dues, clearance, dunning).
    pub fn is_receivable(self) -> bool {
        matches!(
            self,
            ChargeStatus::Published | ChargeStatus::Active | ChargeStatus::Disputed
        )
    }

    /// Statuses in which a payment may be allocated to the charge (5B).
    pub fn accepts_settlement(self) -> bool {
        matches!(self, ChargeStatus::Published | ChargeStatus::Active)
    }

    /// Statuses in which a posted adjustment may change the charge (5A credit/debit notes,
    /// write-off, waiver, aid).
    pub fn accepts_adjustment(self) -> bool {
        matches!(
            self,
            ChargeStatus::Published | ChargeStatus::Active | ChargeStatus::Disputed
        )
    }

    /// Visible to the student (self-service).
    pub fn is_visible_to_student(self) -> bool {
        !matches!(
            self,
            ChargeStatus::Draft | ChargeStatus::PendingValidation | ChargeStatus::Validated
        )
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, ChargeStatus::Archived)
    }
}

status_enum!(
    /// Where a charge came from.
    ChargeSourceType {
        FeeStructure => "fee_structure",
        Manual => "manual",
        /// Pre-student admission charge (L2-F01 amendment 2026-10-05).
        Admission => "admission",
        DebitNote => "debit_note",
        AidRecovery => "aid_recovery",
        Fine => "fine",
        /// Examination fee raised by Domain I through `ExamFeePort` (Batch 6).
        ExamFee => "exam_fee",
        /// Campus service charge raised by Domain G through `CampusChargePort` (Batch 8).
        CampusCharge => "campus_charge",
    }
);

status_enum!(
    /// Kind of finance party.
    PartyKind { Student => "student", Applicant => "applicant" }
);

status_enum!(
    /// Student finance account status.
    StudentAccountStatus { Active => "active", OnHold => "on_hold", Closed => "closed" }
);

status_enum!(
    /// Fee head status.
    FeeHeadStatus { Draft => "draft", Active => "active", Retired => "retired" }
);

status_enum!(
    /// Fee head category.
    FeeCategory {
        Tuition => "tuition",
        Admission => "admission",
        Examination => "examination",
        Hostel => "hostel",
        Transport => "transport",
        Library => "library",
        Laboratory => "laboratory",
        Fine => "fine",
        Deposit => "deposit",
        Miscellaneous => "miscellaneous",
    }
);

status_enum!(
    /// GL account classification.
    LedgerAccountType {
        Asset => "asset",
        Liability => "liability",
        Equity => "equity",
        Income => "income",
        Expense => "expense",
    }
);

impl LedgerAccountType {
    /// Normal balance side of the account type.
    pub fn normal_balance(self) -> NormalBalance {
        match self {
            LedgerAccountType::Asset | LedgerAccountType::Expense => NormalBalance::Debit,
            _ => NormalBalance::Credit,
        }
    }
}

status_enum!(
    /// Debit or credit side.
    NormalBalance { Debit => "debit", Credit => "credit" }
);

status_enum!(
    /// GL account status (5C deactivates; inactive accounts refuse postings).
    LedgerAccountStatus { Active => "active", Inactive => "inactive" }
);

status_enum!(
    /// Kind of journal entry.
    JournalEntryKind {
        /// Automatic posting of a business transaction (charge, adjustment, payment, refund, aid).
        Standard => "standard",
        /// Mirror entry of a reversed entry (`reverses_entry_id`).
        Reversal => "reversal",
        /// Approved manual journal (5C).
        Manual => "manual",
        Adjusting => "adjusting",
        Opening => "opening",
        Closing => "closing",
    }
);

// ================================================================ 5A (transitions slice-owned)

status_enum!(
    /// Fee structure version (L3-F-001, F-084 governance).
    FeeStructureStatus {
        Draft => "draft",
        InReview => "in_review",
        Approved => "approved",
        Published => "published",
        Superseded => "superseded",
        Retired => "retired",
    }
);

status_enum!(
    /// Instalment of a due schedule (L3-F-005).
    InstalmentStatus {
        Scheduled => "scheduled",
        Due => "due",
        Overdue => "overdue",
        Paid => "paid",
        Waived => "waived",
        Cancelled => "cancelled",
    }
);

status_enum!(
    /// Charge dispute (L3-F-004).
    ChargeDisputeStatus {
        Raised => "raised",
        UnderReview => "under_review",
        Upheld => "upheld",
        Rejected => "rejected",
        Withdrawn => "withdrawn",
    }
);

status_enum!(
    /// Kind of financial adjustment (L2-F04).
    AdjustmentKind {
        CreditNote => "credit_note",
        DebitNote => "debit_note",
        WriteOff => "write_off",
        Waiver => "waiver",
        Concession => "concession",
    }
);

status_enum!(
    /// Adjustment lifecycle (L3-F-020…024).
    AdjustmentStatus {
        Draft => "draft",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Posted => "posted",
        Rejected => "rejected",
        Reversed => "reversed",
        Superseded => "superseded",
    }
);

status_enum!(
    /// Financial aid allocation (L3-F-062…066).
    AidAllocationStatus {
        Proposed => "proposed",
        Approved => "approved",
        Allocated => "allocated",
        Disbursed => "disbursed",
        Suspended => "suspended",
        Revoked => "revoked",
        Recovered => "recovered",
        Closed => "closed",
    }
);

status_enum!(
    /// Governed configuration / rule version (L3-F-084/085 + rollback amendment 2026-09-01).
    FinancePolicyStatus {
        Draft => "draft",
        InReview => "in_review",
        Effective => "effective",
        Superseded => "superseded",
        RolledBack => "rolled_back",
        Retired => "retired",
    }
);

// ================================================================ 5B (transitions slice-owned)

status_enum!(
    /// Payment lifecycle (L2-F02, L3-F-007…013).
    PaymentStatus {
        Initiated => "initiated",
        Pending => "pending",
        Completed => "completed",
        Failed => "failed",
        Expired => "expired",
        Cancelled => "cancelled",
        Reversed => "reversed",
        ChargedBack => "charged_back",
    }
);

status_enum!(
    /// How money arrived.
    PaymentChannel {
        Gateway => "gateway",
        Cash => "cash",
        Cheque => "cheque",
        BankTransfer => "bank_transfer",
        DemandDraft => "demand_draft",
        Sponsor => "sponsor",
    }
);

status_enum!(
    /// Payment allocation to a charge.
    AllocationStatus { Allocated => "allocated", Reversed => "reversed" }
);

status_enum!(
    /// Receipt.
    ReceiptStatus { Issued => "issued", Cancelled => "cancelled" }
);

status_enum!(
    /// Unidentified / unallocated receipt (L3-F-009).
    UnidentifiedReceiptStatus {
        Unidentified => "unidentified",
        UnderInvestigation => "under_investigation",
        Identified => "identified",
        Allocated => "allocated",
        Refunded => "refunded",
    }
);

status_enum!(
    /// Bulk collection population (L3-F-013).
    PaymentBatchStatus {
        Draft => "draft",
        Validated => "validated",
        Executing => "executing",
        Completed => "completed",
        PartiallyCompleted => "partially_completed",
        Failed => "failed",
    }
);

status_enum!(
    /// Refund lifecycle (L2-F03, L3-F-014…019).
    RefundStatus {
        Requested => "requested",
        UnderReview => "under_review",
        Approved => "approved",
        Rejected => "rejected",
        Processing => "processing",
        Completed => "completed",
        Failed => "failed",
        Cancelled => "cancelled",
        Reversed => "reversed",
        Closed => "closed",
    }
);

status_enum!(
    /// Accounts receivable (L2-F07, L3-F-039…043).
    ReceivableStatus {
        Open => "open",
        PartiallySettled => "partially_settled",
        Overdue => "overdue",
        Settled => "settled",
        WrittenOff => "written_off",
        Reopened => "reopened",
    }
);

status_enum!(
    /// Dunning case (L3-F-040).
    DunningStatus {
        Open => "open",
        NoticeSent => "notice_sent",
        Escalated => "escalated",
        Resolved => "resolved",
        Cancelled => "cancelled",
    }
);

// ================================================================ 5C (transitions slice-owned)

status_enum!(
    /// Financial year (L3-F-051/053).
    FinancialYearStatus {
        Draft => "draft",
        Open => "open",
        Closing => "closing",
        Closed => "closed",
        Archived => "archived",
    }
);

status_enum!(
    /// Financial period (L3-F-051…055). Postings are accepted only while `open | reopened`.
    FinancialPeriodStatus {
        Planned => "planned",
        Open => "open",
        Locked => "locked",
        Closed => "closed",
        Reopened => "reopened",
        Archived => "archived",
    }
);

impl FinancialPeriodStatus {
    /// `true` when the period accepts new postings (the period guard, §3.5).
    pub fn accepts_postings(self) -> bool {
        matches!(
            self,
            FinancialPeriodStatus::Open | FinancialPeriodStatus::Reopened
        )
    }
}

status_enum!(
    /// Accounting batch (L3-F-027).
    AccountingBatchStatus {
        Draft => "draft",
        Validated => "validated",
        Posted => "posted",
        Rejected => "rejected",
        Failed => "failed",
    }
);

status_enum!(
    /// Manual journal request (L3-F-026/028).
    ManualJournalStatus {
        Draft => "draft",
        PendingApproval => "pending_approval",
        Approved => "approved",
        Posted => "posted",
        Rejected => "rejected",
        Reversed => "reversed",
    }
);

status_enum!(
    /// Reconciliation run (ledger L3-F-029, bank L3-F-058).
    ReconciliationStatus {
        Open => "open",
        Matched => "matched",
        Exceptions => "exceptions",
        Resolved => "resolved",
        Closed => "closed",
    }
);

status_enum!(
    /// Treasury bank account (L3-F-056).
    BankAccountStatus {
        Requested => "requested",
        Approved => "approved",
        Active => "active",
        Suspended => "suspended",
        Closed => "closed",
    }
);

status_enum!(
    /// Imported bank statement (L3-F-058).
    BankStatementStatus {
        Imported => "imported",
        Reconciling => "reconciling",
        Reconciled => "reconciled",
        Failed => "failed",
    }
);

status_enum!(
    /// Bank transaction / execution outcome (L3-F-057, F-061).
    BankTransactionStatus {
        Pending => "pending",
        Executed => "executed",
        Failed => "failed",
        Returned => "returned",
        Uncertain => "uncertain",
        Resolved => "resolved",
    }
);

status_enum!(
    /// Fund transfer (L3-F-060).
    FundTransferStatus {
        Requested => "requested",
        Approved => "approved",
        Executed => "executed",
        Settled => "settled",
        Failed => "failed",
        Cancelled => "cancelled",
    }
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_roundtrip_and_are_unique() {
        for s in ChargeStatus::ALL {
            assert_eq!(ChargeStatus::parse(s.as_str()), Some(*s));
        }
        let names: std::collections::HashSet<_> =
            ChargeStatus::ALL.iter().map(|s| s.as_str()).collect();
        assert_eq!(names.len(), ChargeStatus::ALL.len());
        assert_eq!(
            serde_json::to_value(ChargeStatus::WrittenOff).unwrap_or_default(),
            "written_off"
        );
    }

    #[test]
    fn charge_transitions_follow_l2_f01() {
        use ChargeStatus::*;
        assert!(Draft.can_transition(PendingValidation));
        assert!(Validated.can_transition(Published));
        assert!(Published.can_transition(Active));
        assert!(Active.can_transition(Closed));
        assert!(Disputed.can_transition(Active));
        assert!(Closed.can_transition(Archived));
        assert!(!Draft.can_transition(Published), "validation is mandatory");
        assert!(!Archived.can_transition(Active));
        assert!(Closed.can_transition(Active), "reversal re-opens");
        assert!(!WrittenOff.can_transition(Active));
        assert!(!Cancelled.can_transition(Active));
        for s in ChargeStatus::ALL {
            assert!(!Archived.can_transition(*s), "archived is terminal");
        }
        assert!(Published.is_receivable() && Disputed.is_receivable());
        assert!(!Suspended.is_receivable() && !Draft.is_receivable());
        assert!(Active.accepts_settlement() && !Disputed.accepts_settlement());
        assert!(!Validated.is_visible_to_student() && Published.is_visible_to_student());
    }

    #[test]
    fn account_types_and_periods() {
        assert_eq!(
            LedgerAccountType::Asset.normal_balance(),
            NormalBalance::Debit
        );
        assert_eq!(
            LedgerAccountType::Income.normal_balance(),
            NormalBalance::Credit
        );
        assert!(FinancialPeriodStatus::Reopened.accepts_postings());
        assert!(!FinancialPeriodStatus::Locked.accepts_postings());
    }
}
