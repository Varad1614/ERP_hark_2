//! [`PayrollPostingPort`] — payroll liability postings raised by Domain E (Batch 7 foundation,
//! contract change to Domain F recorded in `docs/batch7-contracts.md` §6.2).
//!
//! One owner-local transaction in Finance: the payroll accounts are seeded on first use
//! (`5410` salaries and wages, `2410` salaries payable, `2420` payroll deductions and statutory
//! dues payable; `system_key` `salary_expense` / `payroll_payable` / `payroll_deductions_payable`)
//! **outside** the Batch 5 default chart, then one balanced journal entry is posted through
//! `shared::posting` (`source_type = payroll_run`, `source_id` = the run, `posting_key` from the
//! request; period-guarded, idempotent, `journal_entry.posted`):
//!
//! | Account | Side | Amount |
//! |---------|------|--------|
//! | 5410 salaries and wages | Dr | gross earnings + employer contributions |
//! | 2410 salaries payable | Cr | net pay |
//! | 2420 deductions payable | Cr | employee deductions + employer contributions |
//!
//! Amounts are signed: a negative amount (corrective posting) moves to the opposite side; zero
//! lines are skipped.

use crate::shared::posting;
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use rust_decimal::Decimal;
use sqlx::PgConnection;
use sutra_contracts::finance::ids::LedgerAccountId;
use sutra_contracts::finance::ports::{
    source, AccountRef, PayrollPayoutKind, PayrollPayoutRequest, PayrollPostingPort,
    PayrollPostingReceipt, PayrollPostingRequest, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{JournalEntryKind, LedgerAccountType};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::request_id::current_correlation_id;
use uuid::Uuid;

/// `(system_key, code, name, type)` of the payroll accounts.
pub const PAYROLL_ACCOUNTS: [(&str, &str, &str, LedgerAccountType); 3] = [
    (
        "salary_expense",
        "5410",
        "Salaries and wages",
        LedgerAccountType::Expense,
    ),
    (
        "payroll_payable",
        "2410",
        "Salaries payable",
        LedgerAccountType::Liability,
    ),
    (
        "payroll_deductions_payable",
        "2420",
        "Payroll deductions and statutory dues payable",
        LedgerAccountType::Liability,
    ),
];

#[derive(Clone)]
pub struct PgPayrollPostings {
    state: FinanceState,
}

impl PgPayrollPostings {
    pub fn new(state: FinanceState) -> Self {
        Self { state }
    }
}

/// Seeds (idempotent) and resolves the three payroll accounts of an institution, in
/// [`PAYROLL_ACCOUNTS`] order.
pub async fn ensure_payroll_accounts(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<[LedgerAccountId; 3], KernelError> {
    let mut ids = [LedgerAccountId(Uuid::nil()); 3];
    for (i, (key, code, name, ty)) in PAYROLL_ACCOUNTS.iter().enumerate() {
        sqlx::query(
            "INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, \
             account_type, normal_balance, system_key) VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT DO NOTHING",
        )
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(*code)
        .bind(*name)
        .bind(ty.as_str())
        .bind(ty.normal_balance().as_str())
        .bind(*key)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let id: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM finance.ledger_account WHERE tenant_id = $1 AND institution_id = $2 \
             AND system_key = $3",
        )
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(*key)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        ids[i] = LedgerAccountId(id.ok_or_else(|| {
            KernelError::Conflict(format!(
                "ledger account code {code} is taken by another account; the payroll account \
                 `{key}` cannot be created"
            ))
        })?);
    }
    Ok(ids)
}

/// Signed amount → a debit (positive) or credit (positive) line on `account` for a normally
/// `debit`-side amount; `None` for zero.
fn line(
    account: LedgerAccountId,
    amount: Decimal,
    debit_when_positive: bool,
) -> Option<PostingLine> {
    if amount.is_zero() {
        return None;
    }
    let a = AccountRef::Id(account);
    let positive = amount > Decimal::ZERO;
    Some(if positive == debit_when_positive {
        PostingLine::debit(a, amount.abs())
    } else {
        PostingLine::credit(a, amount.abs())
    })
}

/// The balanced lines of a payroll posting (see the module docs).
pub fn payroll_lines(
    req: &PayrollPostingRequest,
    accounts: [LedgerAccountId; 3],
) -> Vec<PostingLine> {
    let [expense, payable, deductions] = accounts;
    [
        line(
            expense,
            req.gross_earnings + req.employer_contributions,
            true,
        ),
        line(payable, req.net_pay, false),
        line(
            deductions,
            req.employee_deductions + req.employer_contributions,
            false,
        ),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The balanced lines of a payroll payout: Dr the settled liability / Cr bank.
pub fn payout_lines(
    req: &PayrollPayoutRequest,
    accounts: [LedgerAccountId; 3],
) -> Vec<PostingLine> {
    let [_, payable, deductions] = accounts;
    let liability = match req.kind {
        PayrollPayoutKind::NetPay => payable,
        PayrollPayoutKind::StatutoryDues => deductions,
    };
    vec![
        PostingLine::debit(AccountRef::Id(liability), req.amount),
        PostingLine::credit(AccountRef::System(SystemAccount::BankMain), req.amount),
    ]
}

#[async_trait::async_trait]
impl PayrollPostingPort for PgPayrollPostings {
    async fn post_payroll_payout(
        &self,
        scope: &ExecutionScope,
        req: PayrollPayoutRequest,
    ) -> Result<PayrollPostingReceipt, KernelError> {
        req.validate().map_err(|m| {
            KernelError::Validation(vec![FieldViolation::new(
                "amount",
                "invalid_payroll_payout",
                &m,
            )])
        })?;
        if scope
            .institution_id
            .is_some_and(|i| i != req.institution_id)
        {
            return Err(KernelError::Scope(
                "posting outside the caller's institution".into(),
            ));
        }
        let st = &self.state;
        let tenant = scope.tenant_id;
        let institution = req.institution_id;
        let iscope = ExecutionScope::institution(tenant, institution);
        business_today(st, &iscope, institution).await?;
        let actor = Actor {
            id: req.actor.clone().or_else(|| Some("system:payroll".into())),
            correlation_id: current_correlation_id(),
        };
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let accounts = ensure_payroll_accounts(&mut tx, tenant, institution).await?;
        let posting = PostingRequest {
            institution_id: institution,
            posting_date: req.posting_date,
            kind: JournalEntryKind::Standard,
            source_type: source::PAYROLL_DISBURSEMENT.into(),
            source_id: req.disbursement_id,
            posting_key: req.posting_key.trim().to_owned(),
            description: format!("{} (ref {})", req.description, req.reference.trim()),
            currency: CurrencyCode::INR,
            lines: payout_lines(&req, accounts),
        };
        let r = posting::post(&mut tx, tenant, &posting, &actor).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(PayrollPostingReceipt {
            journal_entry_id: r.journal_entry_id,
            entry_number: r.entry_number,
            already_posted: r.already_posted,
        })
    }

    async fn post_payroll(
        &self,
        scope: &ExecutionScope,
        req: PayrollPostingRequest,
    ) -> Result<PayrollPostingReceipt, KernelError> {
        req.validate().map_err(|m| {
            KernelError::Validation(vec![FieldViolation::new(
                "netPay",
                "invalid_payroll_posting",
                &m,
            )])
        })?;
        if scope
            .institution_id
            .is_some_and(|i| i != req.institution_id)
        {
            return Err(KernelError::Scope(
                "posting outside the caller's institution".into(),
            ));
        }
        let st = &self.state;
        let tenant = scope.tenant_id;
        let institution = req.institution_id;
        // The institution must exist in the tenant (404 otherwise); the business date check
        // keeps the period guard meaningful for future-dated requests.
        let iscope = ExecutionScope::institution(tenant, institution);
        business_today(st, &iscope, institution).await?;
        let actor = Actor {
            id: req.actor.clone().or_else(|| Some("system:payroll".into())),
            correlation_id: current_correlation_id(),
        };
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let accounts = ensure_payroll_accounts(&mut tx, tenant, institution).await?;
        let posting = PostingRequest {
            institution_id: institution,
            posting_date: req.posting_date,
            kind: JournalEntryKind::Standard,
            source_type: source::PAYROLL_RUN.into(),
            source_id: req.payroll_run_id,
            posting_key: req.posting_key.trim().to_owned(),
            description: req.description.clone(),
            currency: CurrencyCode::INR,
            lines: payroll_lines(&req, accounts),
        };
        let r = posting::post(&mut tx, tenant, &posting, &actor).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(PayrollPostingReceipt {
            journal_entry_id: r.journal_entry_id,
            entry_number: r.entry_number,
            already_posted: r.already_posted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::str::FromStr;
    use sutra_contracts::finance::status::NormalBalance;

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

    fn accounts() -> [LedgerAccountId; 3] {
        [
            LedgerAccountId(Uuid::from_u128(1)),
            LedgerAccountId(Uuid::from_u128(2)),
            LedgerAccountId(Uuid::from_u128(3)),
        ]
    }

    fn sums(lines: &[PostingLine]) -> (Decimal, Decimal) {
        lines
            .iter()
            .fold((Decimal::ZERO, Decimal::ZERO), |(dr, cr), l| match l.side {
                NormalBalance::Debit => (dr + l.amount, cr),
                NormalBalance::Credit => (dr, cr + l.amount),
            })
    }

    #[test]
    fn payout_lines_clear_the_settled_liability_against_the_bank() {
        let mut r = PayrollPayoutRequest {
            institution_id: InstitutionId(Uuid::nil()),
            kind: PayrollPayoutKind::NetPay,
            disbursement_id: Uuid::nil(),
            payroll_run_id: Uuid::nil(),
            posting_key: "paid".into(),
            posting_date: NaiveDate::default(),
            description: "Salary".into(),
            amount: d("43800.00"),
            reference: "UTR1".into(),
            actor: None,
        };
        let l = payout_lines(&r, accounts());
        assert_eq!(sums(&l), (d("43800.00"), d("43800.00")));
        assert_eq!(
            l[0].account,
            AccountRef::Id(LedgerAccountId(Uuid::from_u128(2)))
        );
        assert_eq!(l[1].account, AccountRef::System(SystemAccount::BankMain));
        r.kind = PayrollPayoutKind::StatutoryDues;
        let l = payout_lines(&r, accounts());
        assert_eq!(
            l[0].account,
            AccountRef::Id(LedgerAccountId(Uuid::from_u128(3)))
        );
        assert!(matches!(l[0].side, NormalBalance::Debit));
        assert!(matches!(l[1].side, NormalBalance::Credit));
    }

    #[test]
    fn payroll_lines_balance_for_regular_and_corrective_postings() {
        let l = payroll_lines(
            &req("50000.00", "6200.00", "1800.00", "43800.00"),
            accounts(),
        );
        assert_eq!(l.len(), 3);
        assert_eq!(sums(&l), (d("51800.00"), d("51800.00")));
        // A recovery (negative gross) flips the sides; zero lines are skipped.
        let l = payroll_lines(&req("-500.00", "0", "0", "-500.00"), accounts());
        assert_eq!(l.len(), 2);
        assert_eq!(sums(&l), (d("500.00"), d("500.00")));
        assert!(matches!(l[0].side, NormalBalance::Credit));
    }
}
