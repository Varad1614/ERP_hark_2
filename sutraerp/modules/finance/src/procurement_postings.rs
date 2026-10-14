//! [`ProcurementPostingPort`] — accounts payable postings raised by Domain J (Batch 8
//! foundation, contract change to Domain F recorded in `docs/batch8-contracts.md` §6.2; Batch 5
//! deferrals F-044 vendor invoice → payable, F-045 vendor settlement, F-082 vendor liability).
//!
//! One owner-local transaction in Finance: the procurement accounts are seeded on first use
//! (`1410` inventory and stores, `5510` procurement expense, `2310` accounts payable — suppliers;
//! `system_key` `inventory` / `procurement_expense` / `accounts_payable`) **outside** the Batch 5
//! default chart, then one balanced journal entry is posted through `shared::posting`
//! (`source_type = procurement_invoice | procurement_payment`, `source_id` = the Domain J
//! invoice / payment, `posting_key` from the request; period-guarded, idempotent,
//! `journal_entry.posted`):
//!
//! | Kind | Lines |
//! |------|-------|
//! | vendor invoice | Dr 1410 inventory (stock lines) + Dr 5510 expense (other lines) / Cr 2310 payable (total) |
//! | vendor payment | Dr 2310 payable / Cr bank (`bank_main`) |
//!
//! Amounts are signed: a credit note (negative amounts) moves each line to the opposite side;
//! zero lines are skipped.

use crate::shared::posting;
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use rust_decimal::Decimal;
use sqlx::PgConnection;
use sutra_contracts::finance::ids::LedgerAccountId;
use sutra_contracts::finance::ports::{
    AccountRef, PostingLine, PostingRequest, ProcurementPostingKind, ProcurementPostingPort,
    ProcurementPostingReceipt, ProcurementPostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{JournalEntryKind, LedgerAccountType};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::request_id::current_correlation_id;
use uuid::Uuid;

/// `(system_key, code, name, type)` of the procurement accounts.
pub const PROCUREMENT_ACCOUNTS: [(&str, &str, &str, LedgerAccountType); 3] = [
    (
        "inventory",
        "1410",
        "Inventory and stores",
        LedgerAccountType::Asset,
    ),
    (
        "procurement_expense",
        "5510",
        "Procurement expense (non-stock purchases)",
        LedgerAccountType::Expense,
    ),
    (
        "accounts_payable",
        "2310",
        "Accounts payable - suppliers",
        LedgerAccountType::Liability,
    ),
];

#[derive(Clone)]
pub struct PgProcurementPostings {
    state: FinanceState,
}

impl PgProcurementPostings {
    pub fn new(state: FinanceState) -> Self {
        Self { state }
    }
}

/// Seeds (idempotent) and resolves the three procurement accounts of an institution, in
/// [`PROCUREMENT_ACCOUNTS`] order.
pub async fn ensure_procurement_accounts(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<[LedgerAccountId; 3], KernelError> {
    let mut ids = [LedgerAccountId(Uuid::nil()); 3];
    for (i, (key, code, name, ty)) in PROCUREMENT_ACCOUNTS.iter().enumerate() {
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
                "ledger account code {code} is taken by another account; the procurement \
                 account `{key}` cannot be created"
            ))
        })?);
    }
    Ok(ids)
}

/// Signed amount → a debit (positive) or credit line on `account`; `None` for zero.
fn line(account: AccountRef, amount: Decimal, debit_when_positive: bool) -> Option<PostingLine> {
    if amount.is_zero() {
        return None;
    }
    let positive = amount > Decimal::ZERO;
    Some(if positive == debit_when_positive {
        PostingLine::debit(account, amount.abs())
    } else {
        PostingLine::credit(account, amount.abs())
    })
}

/// The balanced lines of a procurement posting (see the module docs).
pub fn procurement_lines(
    req: &ProcurementPostingRequest,
    accounts: [LedgerAccountId; 3],
) -> Vec<PostingLine> {
    let [inventory, expense, payable] = accounts.map(AccountRef::Id);
    match req.kind {
        ProcurementPostingKind::VendorInvoice => [
            line(inventory, req.inventory_amount, true),
            line(expense, req.expense_amount, true),
            line(payable, req.inventory_amount + req.expense_amount, false),
        ]
        .into_iter()
        .flatten()
        .collect(),
        ProcurementPostingKind::VendorPayment => [
            line(payable, req.payment_amount, true),
            line(
                AccountRef::System(SystemAccount::BankMain),
                req.payment_amount,
                false,
            ),
        ]
        .into_iter()
        .flatten()
        .collect(),
    }
}

#[async_trait::async_trait]
impl ProcurementPostingPort for PgProcurementPostings {
    async fn post_procurement(
        &self,
        scope: &ExecutionScope,
        req: ProcurementPostingRequest,
    ) -> Result<ProcurementPostingReceipt, KernelError> {
        req.validate().map_err(|m| {
            KernelError::Validation(vec![FieldViolation::new(
                "amount",
                "invalid_procurement_posting",
                m,
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
            id: req
                .actor
                .clone()
                .or_else(|| Some("system:procurement".into())),
            correlation_id: current_correlation_id(),
        };
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let accounts = ensure_procurement_accounts(&mut tx, tenant, institution).await?;
        let description = if req.supplier_label.trim().is_empty() {
            req.description.clone()
        } else {
            format!("{} ({})", req.description, req.supplier_label.trim())
        };
        let posting = PostingRequest {
            institution_id: institution,
            posting_date: req.posting_date,
            kind: JournalEntryKind::Standard,
            source_type: req.kind.source_type().into(),
            source_id: req.source_id,
            posting_key: req.posting_key.trim().to_owned(),
            description,
            currency: CurrencyCode::INR,
            lines: procurement_lines(&req, accounts),
        };
        let r = posting::post(&mut tx, tenant, &posting, &actor).await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(ProcurementPostingReceipt {
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
    fn invoice_payment_and_credit_note_lines_balance() {
        use ProcurementPostingKind::*;
        let l = procurement_lines(&req(VendorInvoice, "1000.00", "250.00", "0"), accounts());
        assert_eq!(l.len(), 3);
        assert_eq!(sums(&l), (d("1250.00"), d("1250.00")));
        let l = procurement_lines(&req(VendorInvoice, "0", "300.00", "0"), accounts());
        assert_eq!(l.len(), 2);
        let l = procurement_lines(&req(VendorInvoice, "-100.00", "0", "0"), accounts());
        assert_eq!(sums(&l), (d("100.00"), d("100.00")));
        assert!(matches!(l[0].side, NormalBalance::Credit));
        let l = procurement_lines(&req(VendorPayment, "0", "0", "1250.00"), accounts());
        assert_eq!(l.len(), 2);
        assert_eq!(sums(&l), (d("1250.00"), d("1250.00")));
        assert!(matches!(l[0].side, NormalBalance::Debit));
    }
}
