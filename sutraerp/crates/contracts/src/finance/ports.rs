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
//!   deterministic `sutra_finance::payments::gateway::MockPaymentGateway` (5B owns and extends it);
//!   Part 2 S4 adds the real `sutra_infra::razorpay::RazorpayGateway`.
//!
//! * Batch 8: [`CampusChargePort`] (Domain G library fines, hostel / transport fees →
//!   `sutra_finance::PgCampusCharges`) and [`ProcurementPostingPort`] (Domain J vendor invoices /
//!   payments → `sutra_finance::PgProcurementPostings`).
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
    // Batch 7 (Domain E): payroll liability postings through `PayrollPostingPort`.
    pub const PAYROLL_RUN: &str = "payroll_run";
    // Part 1 (DEF-E-007): payroll disbursements (net pay / statutory dues) through
    // `PayrollPostingPort::post_payroll_payout`.
    pub const PAYROLL_DISBURSEMENT: &str = "payroll_disbursement";
    // Batch 8 (Domain J): vendor invoices / payments through `ProcurementPostingPort`.
    pub const PROCUREMENT_INVOICE: &str = "procurement_invoice";
    pub const PROCUREMENT_PAYMENT: &str = "procurement_payment";
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

    /// Part 2 S4 (payments): verifies the checkout callback signature server-side (Razorpay:
    /// lowercase hex HMAC-SHA256 of `order_id|payment_id` keyed with the API key secret,
    /// constant-time compare). `Ok` only proves the callback came from the gateway; the payment
    /// is still completed only on a server-side [`PaymentGatewayPort::fetch_outcome`] or a
    /// verified webhook. Default: the adapter has no checkout signature (`Unauthenticated`).
    fn verify_checkout(
        &self,
        order_ref: &str,
        payment_ref: &str,
        signature: &str,
    ) -> Result<(), KernelError> {
        let _ = (order_ref, payment_ref, signature);
        Err(KernelError::Unauthenticated)
    }

    /// Part 2 S4 (UI fees): the **public** key id the browser opens the gateway's checkout with
    /// (Razorpay Checkout `key`, `rzp_test_…` / `rzp_live_…`; never the key secret). `None` when
    /// the adapter has no client-side checkout (mock, disabled).
    fn checkout_key_id(&self) -> Option<String> {
        None
    }
}

// ---- Batch 6 (Domain I) block: examination fee charges (revaluation / rechecking) ----

/// An examination fee Domain I asks Finance to raise (revaluation / rechecking fee, L2-I11).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExamFeeRequest {
    pub institution_id: InstitutionId,
    /// Domain C student (must have a live context in the institution).
    pub student_id: Uuid,
    /// Fee head code (`^[A-Z0-9][A-Z0-9_-]{1,31}$`), e.g. `EXAM-REVAL`; created on first use
    /// (category `examination`, income account `OtherIncome`).
    pub fee_code: String,
    pub description: String,
    /// Positive, ≤ 2 decimals, INR.
    pub amount: Decimal,
    pub academic_term_id: Option<Uuid>,
    pub due_date: Option<NaiveDate>,
    /// The Domain I aggregate the fee is for (e.g. the revaluation request id): **idempotency
    /// key** — a second call with the same id returns the charge already raised.
    pub source_id: Uuid,
    /// Actor recorded on the charge audit / posting.
    pub actor: Option<String>,
}

/// The published charge raised for an [`ExamFeeRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExamFeeCharge {
    pub charge_id: ChargeId,
    pub charge_number: String,
    pub amount: Decimal,
    /// `true` when the charge for `source_id` already existed (nothing new raised).
    pub already_raised: bool,
}

/// Write port into Domain F for examination fees (Batch 6). Runs its own owner-local
/// transaction in Finance: party account, fee head, charge (`source_type = exam_fee`,
/// `source_id`), validation, **publication** (Dr receivables / Cr other income through
/// `shared::posting`) and `charge.published`. Domain I stores `charge_id` and reads settlement
/// through [`ChargeDirectory::charge`] (`outstanding_amount == 0`). Errors: unknown student / no
/// live context → `Validation`; invalid amount → `Validation`.
/// Implemented by `sutra_finance::PgExamFees`.
#[async_trait::async_trait]
pub trait ExamFeePort: Send + Sync + 'static {
    async fn raise_exam_fee(
        &self,
        scope: &ExecutionScope,
        request: ExamFeeRequest,
    ) -> Result<ExamFeeCharge, KernelError>;
}

// ---- Batch 7 (Domain E) block: payroll liability postings ----

/// A payroll posting Domain E asks Finance to record (L2-E06 §4.2 "Payroll calculates money,
/// Finance moves and accounts money"). Amounts are **signed**: a corrective posting (adjustment,
/// reversal of an over-payment) may carry negative amounts; Finance turns each into a debit or a
/// credit on the right account. `net_pay` must equal `gross_earnings − employee_deductions`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayrollPostingRequest {
    pub institution_id: InstitutionId,
    /// The Domain E payroll run (journal `source_id`).
    pub payroll_run_id: Uuid,
    /// Distinguishes several postings of one run (`finalise`, `adjustment:<id>`,
    /// `settlement:<id>`); `(payroll_run, payroll_run_id, posting_key)` is the idempotency key.
    pub posting_key: String,
    /// Business date in the institution timezone (period guard applies).
    pub posting_date: NaiveDate,
    pub description: String,
    pub gross_earnings: Decimal,
    pub employee_deductions: Decimal,
    pub employer_contributions: Decimal,
    pub net_pay: Decimal,
    /// Actor recorded on the journal audit.
    pub actor: Option<String>,
}

impl PayrollPostingRequest {
    /// `net_pay = gross − deductions`, two decimals at most, a posting key and something to post.
    pub fn validate(&self) -> Result<(), String> {
        let amounts = [
            self.gross_earnings,
            self.employee_deductions,
            self.employer_contributions,
            self.net_pay,
        ];
        if amounts
            .iter()
            .any(|a| a.scale() > 2 && a.normalize().scale() > 2)
        {
            return Err("payroll amounts take at most two decimals".into());
        }
        if self.posting_key.trim().is_empty() {
            return Err("a posting key is required".into());
        }
        if self.gross_earnings - self.employee_deductions != self.net_pay {
            return Err(format!(
                "net pay {} must equal gross earnings {} minus deductions {}",
                self.net_pay, self.gross_earnings, self.employee_deductions
            ));
        }
        if amounts.iter().all(|a| a.is_zero()) {
            return Err("nothing to post: every amount is zero".into());
        }
        Ok(())
    }
}

/// The journal entry recorded for a [`PayrollPostingRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayrollPostingReceipt {
    pub journal_entry_id: JournalEntryId,
    pub entry_number: String,
    /// `true` when `(payroll_run_id, posting_key)` had been posted before (no new entry).
    pub already_posted: bool,
}

/// Write port into Domain F for payroll liabilities (Batch 7). Runs its own owner-local
/// transaction in Finance: seeds the payroll accounts on first use (`5410` salaries and wages,
/// `2410` salaries payable, `2420` payroll deductions and statutory dues payable — outside the
/// Batch 5 default chart), posts one balanced journal entry (`source_type = payroll_run`): Dr
/// salaries (gross + employer contributions) / Cr salaries payable (net) / Cr deductions payable
/// (deductions + employer contributions), and emits `journal_entry.posted`. Idempotent on
/// `(payroll_run_id, posting_key)`. Errors: invalid request → `Validation`; closed period →
/// `Conflict`. Implemented by `sutra_finance::PgPayrollPostings`.
///
/// Part 1 (DEF-E-007) adds [`PayrollPostingPort::post_payroll_payout`]: the settlement of a
/// payroll disbursement batch (net pay paid to staff → Dr `2410` / Cr bank; statutory dues
/// remitted → Dr `2420` / Cr bank), `source_type = payroll_disbursement`, idempotent on
/// `(disbursement_id, posting_key)`.
#[async_trait::async_trait]
pub trait PayrollPostingPort: Send + Sync + 'static {
    async fn post_payroll(
        &self,
        scope: &ExecutionScope,
        request: PayrollPostingRequest,
    ) -> Result<PayrollPostingReceipt, KernelError>;

    /// Records a payroll payout (see [`PayrollPayoutRequest`]). Errors: invalid request →
    /// `Validation`; closed period → `Conflict`.
    async fn post_payroll_payout(
        &self,
        scope: &ExecutionScope,
        request: PayrollPayoutRequest,
    ) -> Result<PayrollPostingReceipt, KernelError>;
}

/// Which payroll liability a payout settles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PayrollPayoutKind {
    /// Net pay transferred to staff bank accounts: Dr `2410` salaries payable / Cr `bank_main`.
    NetPay,
    /// Statutory dues and deductions remitted (challan): Dr `2420` payroll deductions and
    /// statutory dues payable / Cr `bank_main`.
    StatutoryDues,
}

impl PayrollPayoutKind {
    /// Wire / storage name (`net_pay`, `statutory_dues`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NetPay => "net_pay",
            Self::StatutoryDues => "statutory_dues",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "net_pay" => Some(Self::NetPay),
            "statutory_dues" => Some(Self::StatutoryDues),
            _ => None,
        }
    }
}

/// A payroll payout Domain E asks Finance to record when an authorised user marks a
/// disbursement batch paid (bank reference / challan reference in hand).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayrollPayoutRequest {
    pub institution_id: InstitutionId,
    pub kind: PayrollPayoutKind,
    /// The Domain E disbursement batch (journal `source_id`).
    pub disbursement_id: Uuid,
    /// The payroll run the batch pays (recorded in the description / audit).
    pub payroll_run_id: Uuid,
    /// `paid`; `(payroll_disbursement, disbursement_id, posting_key)` is the idempotency key.
    pub posting_key: String,
    /// Payment value date (business date, period guard applies).
    pub posting_date: NaiveDate,
    pub description: String,
    /// Positive, at most two decimals.
    pub amount: Decimal,
    /// Bank transaction reference (UTR) or challan reference.
    pub reference: String,
    pub actor: Option<String>,
}

impl PayrollPayoutRequest {
    /// A positive amount with at most two decimals, a posting key and a reference.
    pub fn validate(&self) -> Result<(), String> {
        if self.amount.normalize().scale() > 2 {
            return Err("payout amounts take at most two decimals".into());
        }
        if self.amount <= Decimal::ZERO {
            return Err("a payout amount must be positive".into());
        }
        if self.posting_key.trim().is_empty() {
            return Err("a posting key is required".into());
        }
        let r = self.reference.trim();
        if r.is_empty() || r.chars().count() > 64 {
            return Err("a bank or challan reference of 1 to 64 characters is required".into());
        }
        Ok(())
    }
}

// ---- Batch 8 (Domain G) block: campus service charges (library fines, hostel, transport) ----

/// Fee category of a campus charge (the Batch 5 fee-head categories campus services use).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CampusChargeCategory {
    Library,
    Hostel,
    Transport,
    Laboratory,
    Fine,
}

impl CampusChargeCategory {
    /// Stored `finance.fee_head.category`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Hostel => "hostel",
            Self::Transport => "transport",
            Self::Laboratory => "laboratory",
            Self::Fine => "fine",
        }
    }
}

/// A charge Domain G asks Finance to raise on a student (library fine, hostel fee or damage,
/// transport pass fee). Same shape and rules as [`ExamFeeRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampusChargeRequest {
    pub institution_id: InstitutionId,
    /// Domain C student (must have a live context in the institution).
    pub student_id: Uuid,
    /// Fee head code (`^[A-Z0-9][A-Z0-9_-]{1,31}$`), e.g. `LIB-FINE`, `HOSTEL-FEE`,
    /// `TRANSPORT-PASS`; created on first use with `category` and income account `OtherIncome`.
    pub fee_code: String,
    pub category: CampusChargeCategory,
    pub description: String,
    /// Positive, ≤ 2 decimals, INR.
    pub amount: Decimal,
    pub due_date: Option<NaiveDate>,
    /// The Domain G aggregate the charge is for (fine, allocation, pass, inspection id):
    /// **idempotency key** — a second call with the same id returns the charge already raised.
    pub source_id: Uuid,
    /// Actor recorded on the charge audit / posting.
    pub actor: Option<String>,
}

/// The published charge raised for a [`CampusChargeRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CampusCharge {
    pub charge_id: ChargeId,
    pub charge_number: String,
    pub amount: Decimal,
    /// `true` when the charge for `source_id` already existed (nothing new raised).
    pub already_raised: bool,
}

/// Write port into Domain F for campus service charges (Batch 8). One owner-local transaction in
/// Finance (party account, fee head, charge `source_type = campus_charge`, validation,
/// **publication** Dr receivables / Cr other income, `charge.published`). Domain G stores
/// `charge_id` and reads settlement through [`ChargeDirectory::charge`]. Errors: unknown student
/// / no live context → `Validation`; invalid amount or fee code → `Validation`.
/// Implemented by `sutra_finance::PgCampusCharges`.
#[async_trait::async_trait]
pub trait CampusChargePort: Send + Sync + 'static {
    async fn raise_campus_charge(
        &self,
        scope: &ExecutionScope,
        request: CampusChargeRequest,
    ) -> Result<CampusCharge, KernelError>;
}

// ---- Batch 8 (Domain J) block: procurement postings (accounts payable) ----

/// What a procurement posting records.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcurementPostingKind {
    /// Matched vendor invoice → liability (F-044 / F-082): Dr `1410` inventory (stock lines) +
    /// Dr `5510` procurement expense (service / non-stock lines) / Cr `2310` accounts payable.
    VendorInvoice,
    /// Vendor payment settling invoices (F-045): Dr `2310` accounts payable / Cr `bank_main`.
    VendorPayment,
}

impl ProcurementPostingKind {
    /// Journal `source_type` (see [`source`]).
    pub fn source_type(self) -> &'static str {
        match self {
            Self::VendorInvoice => source::PROCUREMENT_INVOICE,
            Self::VendorPayment => source::PROCUREMENT_PAYMENT,
        }
    }
}

/// A procurement posting Domain J asks Finance to record. Amounts are **signed** (a credit note
/// carries negative amounts and flips the sides). For a vendor invoice `inventory_amount +
/// expense_amount` is the payable; for a payment only `payment_amount` is used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcurementPostingRequest {
    pub institution_id: InstitutionId,
    pub kind: ProcurementPostingKind,
    /// The Domain J vendor invoice / payment (journal `source_id`).
    pub source_id: Uuid,
    /// `post`, `credit_note:<id>` …; `(source_type, source_id, posting_key)` is the idempotency
    /// key.
    pub posting_key: String,
    /// Business date in the institution timezone (period guard applies).
    pub posting_date: NaiveDate,
    pub description: String,
    /// Supplier label recorded in the description / audit (`SUP-000001 Acme Traders`).
    pub supplier_label: String,
    pub inventory_amount: Decimal,
    pub expense_amount: Decimal,
    pub payment_amount: Decimal,
    pub actor: Option<String>,
}

impl ProcurementPostingRequest {
    /// Two decimals at most, a posting key, something to post, and only the amounts of the kind.
    pub fn validate(&self) -> Result<(), String> {
        let amounts = [
            self.inventory_amount,
            self.expense_amount,
            self.payment_amount,
        ];
        if amounts.iter().any(|a| a.normalize().scale() > 2) {
            return Err("procurement amounts take at most two decimals".into());
        }
        if self.posting_key.trim().is_empty() {
            return Err("a posting key is required".into());
        }
        match self.kind {
            ProcurementPostingKind::VendorInvoice => {
                if !self.payment_amount.is_zero() {
                    return Err("a vendor invoice carries no payment amount".into());
                }
                if (self.inventory_amount + self.expense_amount).is_zero() {
                    return Err("nothing to post: the invoice total is zero".into());
                }
            }
            ProcurementPostingKind::VendorPayment => {
                if !self.inventory_amount.is_zero() || !self.expense_amount.is_zero() {
                    return Err("a vendor payment carries only a payment amount".into());
                }
                if self.payment_amount.is_zero() {
                    return Err("nothing to post: the payment amount is zero".into());
                }
            }
        }
        Ok(())
    }
}

/// The journal entry recorded for a [`ProcurementPostingRequest`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcurementPostingReceipt {
    pub journal_entry_id: JournalEntryId,
    pub entry_number: String,
    /// `true` when `(source, posting_key)` had been posted before (no new entry).
    pub already_posted: bool,
}

/// Write port into Domain F for procurement liabilities and payments (Batch 8). One owner-local
/// transaction in Finance: seeds `1410` inventory, `5510` procurement expense and `2310`
/// accounts payable on first use (outside the Batch 5 default chart), posts one balanced journal
/// entry through `shared::posting` (period-guarded, idempotent, `journal_entry.posted`).
/// Errors: invalid request → `Validation`; closed period → `Conflict`.
/// Implemented by `sutra_finance::PgProcurementPostings`.
#[async_trait::async_trait]
pub trait ProcurementPostingPort: Send + Sync + 'static {
    async fn post_procurement(
        &self,
        scope: &ExecutionScope,
        request: ProcurementPostingRequest,
    ) -> Result<ProcurementPostingReceipt, KernelError>;
}

#[cfg(test)]
mod procurement_posting_tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn req(
        kind: ProcurementPostingKind,
        inv: &str,
        exp: &str,
        pay: &str,
    ) -> ProcurementPostingRequest {
        ProcurementPostingRequest {
            institution_id: InstitutionId(Uuid::nil()),
            kind,
            source_id: Uuid::nil(),
            posting_key: "post".into(),
            posting_date: NaiveDate::default(),
            description: "Invoice".into(),
            supplier_label: "SUP-000001".into(),
            inventory_amount: d(inv),
            expense_amount: d(exp),
            payment_amount: d(pay),
            actor: None,
        }
    }

    #[test]
    fn procurement_requests_validate() {
        use ProcurementPostingKind::*;
        assert!(req(VendorInvoice, "1000.00", "250.00", "0")
            .validate()
            .is_ok());
        assert!(req(VendorInvoice, "-100.00", "0", "0").validate().is_ok());
        assert!(req(VendorInvoice, "0", "0", "0").validate().is_err());
        assert!(req(VendorInvoice, "10", "0", "5").validate().is_err());
        assert!(req(VendorPayment, "0", "0", "1250.00").validate().is_ok());
        assert!(req(VendorPayment, "1", "0", "1").validate().is_err());
        assert!(req(VendorPayment, "0", "0", "1.005").validate().is_err());
        assert_eq!(VendorInvoice.source_type(), "procurement_invoice");
        assert_eq!(CampusChargeCategory::Library.as_str(), "library");
    }
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

#[cfg(test)]
mod payroll_posting_tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn req(gross: &str, ded: &str, er: &str, net: &str) -> PayrollPostingRequest {
        PayrollPostingRequest {
            institution_id: InstitutionId(Uuid::nil()),
            payroll_run_id: Uuid::nil(),
            posting_key: "finalise".into(),
            posting_date: NaiveDate::default(),
            description: "Payroll".into(),
            gross_earnings: d(gross),
            employee_deductions: d(ded),
            employer_contributions: d(er),
            net_pay: d(net),
            actor: None,
        }
    }

    #[test]
    fn payroll_payouts_validate() {
        let ok = PayrollPayoutRequest {
            institution_id: InstitutionId(Uuid::nil()),
            kind: PayrollPayoutKind::NetPay,
            disbursement_id: Uuid::nil(),
            payroll_run_id: Uuid::nil(),
            posting_key: "paid".into(),
            posting_date: NaiveDate::default(),
            description: "Salary".into(),
            amount: d("43800.00"),
            reference: "UTR123".into(),
            actor: None,
        };
        assert!(ok.validate().is_ok());
        for (amount, reference) in [
            ("0", "UTR"),
            ("-1.00", "UTR"),
            ("1.005", "UTR"),
            ("1.00", " "),
        ] {
            let mut r = ok.clone();
            r.amount = d(amount);
            r.reference = reference.into();
            assert!(r.validate().is_err(), "{amount} {reference}");
        }
        for k in [PayrollPayoutKind::NetPay, PayrollPayoutKind::StatutoryDues] {
            assert_eq!(PayrollPayoutKind::parse(k.as_str()), Some(k));
        }
        assert_eq!(PayrollPayoutKind::parse("x"), None);
    }

    #[test]
    fn payroll_requests_validate_net_pay() {
        assert!(req("50000.00", "6200.00", "1800.00", "43800.00")
            .validate()
            .is_ok());
        assert!(req("-500.00", "0", "0", "-500.00").validate().is_ok());
        assert!(req("50000.00", "6200.00", "0", "43000.00")
            .validate()
            .is_err());
        assert!(req("0", "0", "0", "0").validate().is_err());
        assert!(req("10.005", "0", "0", "10.005").validate().is_err());
        let mut r = req("1.00", "0", "0", "1.00");
        r.posting_key = " ".into();
        assert!(r.validate().is_err());
    }
}
