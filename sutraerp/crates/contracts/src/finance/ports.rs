//! Typed ports of Domain F (`docs/batch5-contracts.md` §6).
//!
//! * [`ChargeDirectory`] — charges and party accounts (foundation tables) for 5B, 5C, Domain C
//!   and later domains. Implemented by `sutra_finance::PgChargeDirectory` (**real**).
//! * [`ReceivableReader`] — outstanding / overdue balances of a party (clearance, dunning,
//!   Student Fees). Implemented by `sutra_finance::PgReceivableReader` (**real**).
//! * [`PostingPort`] — the general-ledger posting service for callers **outside** a finance
//!   transaction (other domains later, background jobs). Inside Domain F, slices call the
//!   in-transaction helper `sutra_finance::shared::posting::post` directly; both share the
//!   validation of [`PostingRequest::validate`]. Implemented by `sutra_finance::PgPostingPort`.
//! * [`PaymentGatewayPort`] — the payment gateway adapter (Razorpay-shaped). Batch 5 wires the
//!   deterministic `sutra_finance::payments::gateway::MockPaymentGateway` (5B owns and extends it).
//!
//! Money is `rust_decimal::Decimal` at scale ≤ 2 with an explicit currency (INR), never floats.

use crate::finance::ids::{ChargeId, JournalEntryId, LedgerAccountId, StudentAccountId};
use crate::finance::status::{
    ChargeStatus, JournalEntryKind, NormalBalance, PartyKind, StudentAccountStatus,
};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

// ------------------------------------------------------------------ money rules

/// Scale of every stored amount (NUMERIC(18,2)).
pub const MONEY_SCALE: u32 = 2;

/// Largest amount a NUMERIC(18,2) column holds.
pub fn max_amount() -> Decimal {
    Decimal::new(9_999_999_999_999_999, 2)
}

/// `true` when `amount` is positive, at most two decimals and fits NUMERIC(18,2).
pub fn is_valid_positive_amount(amount: Decimal) -> bool {
    amount > Decimal::ZERO && amount.normalize().scale() <= MONEY_SCALE && amount <= max_amount()
}

// ------------------------------------------------------------------ charges and parties

/// A finance party account (student or pre-student applicant).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StudentAccountSummary {
    pub id: StudentAccountId,
    pub institution_id: InstitutionId,
    pub party_kind: PartyKind,
    pub student_id: Option<Uuid>,
    pub admission_id: Option<Uuid>,
    pub student_number: Option<String>,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub currency: String,
    pub status: StudentAccountStatus,
}

/// What other slices / domains may rely on about a charge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeSummary {
    pub id: ChargeId,
    pub institution_id: InstitutionId,
    pub student_account_id: StudentAccountId,
    pub student_id: Option<Uuid>,
    pub admission_id: Option<Uuid>,
    pub charge_number: String,
    pub description: String,
    pub currency: String,
    pub total_amount: Decimal,
    pub adjustment_amount: Decimal,
    pub settled_amount: Decimal,
    /// `total + adjustment − settled` (generated column).
    pub outstanding_amount: Decimal,
    pub due_date: Option<NaiveDate>,
    pub academic_term_id: Option<Uuid>,
    pub status: ChargeStatus,
    pub version: i64,
}

/// Read port over the foundation charge and party tables (tenant from `scope`; foreign or
/// missing → `None` / empty).
#[async_trait::async_trait]
pub trait ChargeDirectory: Send + Sync + 'static {
    async fn charge(
        &self,
        scope: &ExecutionScope,
        id: ChargeId,
    ) -> Result<Option<ChargeSummary>, KernelError>;

    /// Every charge of a party account, newest first.
    async fn charges_of_account(
        &self,
        scope: &ExecutionScope,
        account: StudentAccountId,
    ) -> Result<Vec<ChargeSummary>, KernelError>;

    /// Receivable charges (`published | active | disputed`) with an outstanding amount, in the
    /// default allocation order (due date, then charge number).
    async fn open_charges_of_account(
        &self,
        scope: &ExecutionScope,
        account: StudentAccountId,
    ) -> Result<Vec<ChargeSummary>, KernelError>;

    async fn student_account(
        &self,
        scope: &ExecutionScope,
        id: StudentAccountId,
    ) -> Result<Option<StudentAccountSummary>, KernelError>;

    /// The party account of a Domain C student in an institution.
    async fn account_of_student(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        student_id: Uuid,
    ) -> Result<Option<StudentAccountSummary>, KernelError>;

    /// The applicant party account of a Domain C admission (pre-student charges).
    async fn account_of_admission(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        admission_id: Uuid,
    ) -> Result<Option<StudentAccountSummary>, KernelError>;

    /// The party accounts linked to a login subject (Student Fees self-service), any institution.
    async fn accounts_of_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Vec<StudentAccountSummary>, KernelError>;
}

/// Balance of a party account as of a business date.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountBalance {
    pub student_account_id: StudentAccountId,
    pub currency: String,
    /// Σ total of receivable + closed charges.
    pub total_charged: Decimal,
    /// Σ adjustment (credit notes negative, debit notes positive) of those charges.
    pub total_adjusted: Decimal,
    pub total_settled: Decimal,
    /// Σ outstanding of `published | active` charges.
    pub outstanding: Decimal,
    /// Σ outstanding of `disputed` charges (not demanded until resolved).
    pub disputed_outstanding: Decimal,
    /// Part of `outstanding` whose due date is before `as_of`.
    pub overdue: Decimal,
    /// Number of receivable charges with an outstanding amount.
    pub open_charges: i64,
    /// Number of non-draft charges ever raised on the account (cancelled ones included).
    pub charges_raised: i64,
}

/// Read port over receivable balances.
#[async_trait::async_trait]
pub trait ReceivableReader: Send + Sync + 'static {
    async fn account_balance(
        &self,
        scope: &ExecutionScope,
        account: StudentAccountId,
        as_of: NaiveDate,
    ) -> Result<Option<AccountBalance>, KernelError>;
}

// ------------------------------------------------------------------ posting

/// Seeded chart-of-accounts roles every institution has (`shared::coa`, §3.2 of the contract).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SystemAccount {
    CashOnHand,
    BankMain,
    GatewayClearing,
    StudentReceivables,
    UnappliedReceipts,
    RefundsPayable,
    DepositsHeld,
    AccumulatedFund,
    FeeIncome,
    OtherIncome,
    AidExpense,
    ConcessionExpense,
    BadDebtExpense,
    BankCharges,
}

impl SystemAccount {
    pub const ALL: [SystemAccount; 14] = [
        SystemAccount::CashOnHand,
        SystemAccount::BankMain,
        SystemAccount::GatewayClearing,
        SystemAccount::StudentReceivables,
        SystemAccount::UnappliedReceipts,
        SystemAccount::RefundsPayable,
        SystemAccount::DepositsHeld,
        SystemAccount::AccumulatedFund,
        SystemAccount::FeeIncome,
        SystemAccount::OtherIncome,
        SystemAccount::AidExpense,
        SystemAccount::ConcessionExpense,
        SystemAccount::BadDebtExpense,
        SystemAccount::BankCharges,
    ];

    /// Stored `system_key`.
    pub fn key(self) -> &'static str {
        match self {
            SystemAccount::CashOnHand => "cash_on_hand",
            SystemAccount::BankMain => "bank_main",
            SystemAccount::GatewayClearing => "gateway_clearing",
            SystemAccount::StudentReceivables => "student_receivables",
            SystemAccount::UnappliedReceipts => "unapplied_receipts",
            SystemAccount::RefundsPayable => "refunds_payable",
            SystemAccount::DepositsHeld => "deposits_held",
            SystemAccount::AccumulatedFund => "accumulated_fund",
            SystemAccount::FeeIncome => "fee_income",
            SystemAccount::OtherIncome => "other_income",
            SystemAccount::AidExpense => "aid_expense",
            SystemAccount::ConcessionExpense => "concession_expense",
            SystemAccount::BadDebtExpense => "bad_debt_expense",
            SystemAccount::BankCharges => "bank_charges",
        }
    }

    pub fn parse(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.key() == key)
    }
}

/// A ledger account named by id or by its seeded role.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountRef {
    Id(LedgerAccountId),
    System(SystemAccount),
}

/// `source_type` values of journal entries (contract §3.3). Slices add theirs at the end of
/// their block with a `// 5X:` marker.
pub mod source {
    // 5A
    pub const CHARGE: &str = "charge";
    pub const ADJUSTMENT: &str = "adjustment";
    pub const AID_ALLOCATION: &str = "aid_allocation";
    pub const AID_DISBURSEMENT: &str = "aid_disbursement";
    // 5B
    pub const PAYMENT: &str = "payment";
    pub const PAYMENT_ALLOCATION: &str = "payment_allocation";
    pub const UNIDENTIFIED_RECEIPT: &str = "unidentified_receipt";
    pub const REFUND: &str = "refund";
    pub const RECEIVABLE_WRITE_OFF: &str = "receivable_write_off";
    // 5C
    pub const MANUAL_JOURNAL: &str = "manual_journal";
    pub const ACCOUNTING_BATCH: &str = "accounting_batch";
    pub const FUND_TRANSFER: &str = "fund_transfer";
    pub const BANK_TRANSACTION: &str = "bank_transaction";
    pub const OPENING_BALANCE: &str = "opening_balance";
    pub const PERIOD_CLOSE: &str = "period_close";
}

/// Debit or credit side of a posting line.
pub type EntrySide = NormalBalance;

/// One line of a posting request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingLine {
    pub account: AccountRef,
    pub side: EntrySide,
    /// Positive, scale ≤ 2.
    pub amount: Decimal,
    /// Receivable sub-ledger dimensions.
    pub student_account_id: Option<StudentAccountId>,
    pub charge_id: Option<ChargeId>,
    pub memo: Option<String>,
}

impl PostingLine {
    pub fn debit(account: AccountRef, amount: Decimal) -> Self {
        Self {
            account,
            side: NormalBalance::Debit,
            amount,
            student_account_id: None,
            charge_id: None,
            memo: None,
        }
    }

    pub fn credit(account: AccountRef, amount: Decimal) -> Self {
        Self {
            side: NormalBalance::Credit,
            ..Self::debit(account, amount)
        }
    }

    /// Attaches the receivable dimensions (party + charge).
    pub fn for_charge(mut self, account: StudentAccountId, charge: Option<ChargeId>) -> Self {
        self.student_account_id = Some(account);
        self.charge_id = charge;
        self
    }

    pub fn memo(mut self, memo: impl Into<String>) -> Self {
        self.memo = Some(memo.into());
        self
    }
}

/// A balanced double-entry posting of one business transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingRequest {
    pub institution_id: InstitutionId,
    /// Business date in the institution timezone; the period guard checks it.
    pub posting_date: NaiveDate,
    pub kind: JournalEntryKind,
    /// One of [`source`] (`charge`, `adjustment`, `payment`, …; lowercase snake case).
    pub source_type: String,
    pub source_id: Uuid,
    /// Distinguishes several postings of one source (`publish`, `settle:<allocation id>`, …);
    /// `(source_type, source_id, posting_key)` is the idempotency key.
    pub posting_key: String,
    pub description: String,
    pub currency: CurrencyCode,
    pub lines: Vec<PostingLine>,
}

/// Why a posting request is refused before it reaches the database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostingError {
    TooFewLines,
    InvalidAmount { line: usize },
    Unbalanced { debit: Decimal, credit: Decimal },
    EmptyKey,
}

impl std::fmt::Display for PostingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PostingError::TooFewLines => f.write_str("a journal entry needs at least two lines"),
            PostingError::InvalidAmount { line } => write!(
                f,
                "line {line}: amounts must be positive with at most two decimals"
            ),
            PostingError::Unbalanced { debit, credit } => {
                write!(f, "unbalanced entry: debit {debit} ≠ credit {credit}")
            }
            PostingError::EmptyKey => f.write_str("source type and posting key are required"),
        }
    }
}

impl From<PostingError> for KernelError {
    fn from(e: PostingError) -> Self {
        KernelError::Conflict(e.to_string())
    }
}

impl PostingRequest {
    /// Pure double-entry checks: ≥ 2 lines, every amount positive at scale ≤ 2, Σ debit = Σ
    /// credit. Returns the entry total.
    pub fn validate(&self) -> Result<Decimal, PostingError> {
        if self.source_type.trim().is_empty() || self.posting_key.trim().is_empty() {
            return Err(PostingError::EmptyKey);
        }
        if self.lines.len() < 2 {
            return Err(PostingError::TooFewLines);
        }
        let (mut debit, mut credit) = (Decimal::ZERO, Decimal::ZERO);
        for (i, l) in self.lines.iter().enumerate() {
            if !is_valid_positive_amount(l.amount) {
                return Err(PostingError::InvalidAmount { line: i + 1 });
            }
            match l.side {
                NormalBalance::Debit => debit += l.amount,
                NormalBalance::Credit => credit += l.amount,
            }
        }
        if debit != credit {
            return Err(PostingError::Unbalanced { debit, credit });
        }
        Ok(debit)
    }
}

/// Result of a posting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostingReceipt {
    pub journal_entry_id: JournalEntryId,
    pub entry_number: String,
    pub financial_period_id: Option<Uuid>,
    /// `true` when `(source_type, source_id, posting_key)` had already been posted (no new entry).
    pub already_posted: bool,
}

/// General-ledger posting for callers outside a finance transaction (opens its own owner-local
/// transaction, emits `journal_entry.posted`, audits).
#[async_trait::async_trait]
pub trait PostingPort: Send + Sync + 'static {
    async fn post(
        &self,
        scope: &ExecutionScope,
        request: PostingRequest,
    ) -> Result<PostingReceipt, KernelError>;

    /// Posts the mirror entry of `entry` on `posting_date` (idempotent: a second call returns
    /// the existing reversal with `already_posted`).
    async fn reverse(
        &self,
        scope: &ExecutionScope,
        entry: JournalEntryId,
        posting_date: NaiveDate,
        reason: String,
    ) -> Result<PostingReceipt, KernelError>;
}

// ------------------------------------------------------------------ payment gateway

/// Create a gateway order for an amount owed (5B `payment_initiate`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayOrderRequest {
    pub institution_id: InstitutionId,
    pub amount: Decimal,
    pub currency: CurrencyCode,
    /// Our payment id (Razorpay `receipt`).
    pub receipt_ref: String,
    /// Retry-safe key: the same key returns the same order.
    pub idempotency_key: String,
}

/// Gateway-side order status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayOrderStatus {
    Created,
    Paid,
    Failed,
    Expired,
}

/// A gateway order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayOrder {
    /// Adapter name (`mock`, later `razorpay`).
    pub gateway: &'static str,
    pub order_ref: String,
    pub amount: Decimal,
    pub currency: CurrencyCode,
    pub status: GatewayOrderStatus,
    /// Where the payer completes the payment (the dev mock: a local simulate URL).
    pub checkout_url: Option<String>,
}

/// What the gateway says about the payment of an order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GatewayPaymentOutcome {
    Captured {
        payment_ref: String,
        amount: Decimal,
    },
    Failed {
        code: String,
        reason: String,
    },
    /// Not known yet (timeout / in flight): recover by polling, never by guessing (L3-F-010).
    Pending,
}

/// A verified webhook notification.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayWebhook {
    /// Gateway event id: the idempotency key of webhook processing.
    pub event_id: String,
    /// `payment.captured`, `payment.failed`, `refund.processed`, …
    pub event: String,
    pub order_ref: String,
    pub outcome: GatewayPaymentOutcome,
}

/// Refund through the gateway.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayRefundRequest {
    pub payment_ref: String,
    pub amount: Decimal,
    pub currency: CurrencyCode,
    pub idempotency_key: String,
}

/// Gateway-side refund status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayRefundStatus {
    Processed,
    Pending,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GatewayRefund {
    pub refund_ref: String,
    pub status: GatewayRefundStatus,
}

/// Payment gateway adapter (Razorpay-shaped). Every call is idempotent on its key.
#[async_trait::async_trait]
pub trait PaymentGatewayPort: Send + Sync + 'static {
    /// Adapter name stored with payments (`mock`).
    fn name(&self) -> &'static str;

    async fn create_order(
        &self,
        scope: &ExecutionScope,
        request: GatewayOrderRequest,
    ) -> Result<GatewayOrder, KernelError>;

    /// Polls the payment outcome of an order (failure / timeout recovery).
    async fn fetch_outcome(
        &self,
        scope: &ExecutionScope,
        order_ref: &str,
    ) -> Result<GatewayPaymentOutcome, KernelError>;

    /// Verifies the webhook signature over the raw body and parses it (`Err` = 401/400).
    fn verify_webhook(&self, body: &[u8], signature: &str) -> Result<GatewayWebhook, KernelError>;

    async fn refund(
        &self,
        scope: &ExecutionScope,
        request: GatewayRefundRequest,
    ) -> Result<GatewayRefund, KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn req(lines: Vec<PostingLine>) -> PostingRequest {
        PostingRequest {
            institution_id: InstitutionId(Uuid::nil()),
            posting_date: NaiveDate::default(),
            kind: JournalEntryKind::Standard,
            source_type: source::CHARGE.into(),
            source_id: Uuid::nil(),
            posting_key: "publish".into(),
            description: "Tuition".into(),
            currency: CurrencyCode::INR,
            lines,
        }
    }

    #[test]
    fn balanced_requests_validate() {
        let r = req(vec![
            PostingLine::debit(
                AccountRef::System(SystemAccount::StudentReceivables),
                d("1500.50"),
            ),
            PostingLine::credit(AccountRef::System(SystemAccount::FeeIncome), d("1000.00")),
            PostingLine::credit(AccountRef::System(SystemAccount::OtherIncome), d("500.50")),
        ]);
        assert_eq!(r.validate(), Ok(d("1500.50")));
    }

    #[test]
    fn unbalanced_short_or_bad_amounts_are_refused() {
        let ar = AccountRef::System(SystemAccount::StudentReceivables);
        let inc = AccountRef::System(SystemAccount::FeeIncome);
        assert!(matches!(
            req(vec![
                PostingLine::debit(ar, d("10")),
                PostingLine::credit(inc, d("9.99"))
            ])
            .validate(),
            Err(PostingError::Unbalanced { .. })
        ));
        assert_eq!(
            req(vec![PostingLine::debit(ar, d("10"))]).validate(),
            Err(PostingError::TooFewLines)
        );
        assert_eq!(
            req(vec![
                PostingLine::debit(ar, d("10.001")),
                PostingLine::credit(inc, d("10.001"))
            ])
            .validate(),
            Err(PostingError::InvalidAmount { line: 1 })
        );
        assert_eq!(
            req(vec![
                PostingLine::debit(ar, d("0")),
                PostingLine::credit(inc, d("0"))
            ])
            .validate(),
            Err(PostingError::InvalidAmount { line: 1 })
        );
        assert!(
            is_valid_positive_amount(d("10.100")),
            "trailing zeros are fine"
        );
    }

    #[test]
    fn system_accounts_roundtrip() {
        for a in SystemAccount::ALL {
            assert_eq!(SystemAccount::parse(a.key()), Some(a));
        }
    }
}
