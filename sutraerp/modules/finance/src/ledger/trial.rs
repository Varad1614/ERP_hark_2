//! Trial balance (L3-F-030): cumulative turnover and balance of every ledger account with
//! postings up to a date, read straight from the append-only journal.

use crate::shared::money::fmt;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::ids::LedgerAccountId;
use sutra_contracts::finance::status::{LedgerAccountType, NormalBalance};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// One account row of the trial balance.
#[derive(Debug, Clone, Serialize, serde::Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TrialBalanceRow {
    pub ledger_account_id: LedgerAccountId,
    pub code: String,
    pub name: String,
    pub account_type: LedgerAccountType,
    /// Σ debits up to `asOf`.
    #[schema(value_type = String, example = "125000.00")]
    pub debit: String,
    /// Σ credits up to `asOf`.
    #[schema(value_type = String, example = "100000.00")]
    pub credit: String,
    /// Net balance on the account's normal side (negative = contra balance).
    #[schema(value_type = String, example = "25000.00")]
    pub balance: String,
}

/// The trial balance (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrialBalanceView {
    pub as_of: NaiveDate,
    pub period_id: Option<Uuid>,
    pub rows: Vec<TrialBalanceRow>,
    #[schema(value_type = String, example = "225000.00")]
    pub total_debit: String,
    #[schema(value_type = String, example = "225000.00")]
    pub total_credit: String,
    pub balanced: bool,
    pub currency: String,
}

/// Net balance on the normal side.
pub fn normal_balance(ty: LedgerAccountType, debit: Decimal, credit: Decimal) -> Decimal {
    match ty.normal_balance() {
        NormalBalance::Debit => debit - credit,
        NormalBalance::Credit => credit - debit,
    }
}

/// Computes the trial balance of `institution` as of `as_of` (inclusive).
pub async fn compute(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    as_of: NaiveDate,
    period_id: Option<Uuid>,
) -> Result<TrialBalanceView, KernelError> {
    let rows = sqlx::query(
        "SELECT a.id, a.code, a.name, a.account_type, COALESCE(sum(l.debit), 0) AS debit, \
         COALESCE(sum(l.credit), 0) AS credit \
         FROM finance.journal_line l \
         JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE e.tenant_id = $1 AND e.institution_id = $2 AND e.posting_date <= $3 \
         GROUP BY a.id, a.code, a.name, a.account_type ORDER BY a.code",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(as_of)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (mut td, mut tc) = (Decimal::ZERO, Decimal::ZERO);
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let ty: String = r.try_get("account_type").map_err(map_db_error)?;
        let ty = LedgerAccountType::parse(&ty)
            .ok_or_else(|| KernelError::internal_msg(format!("account type `{ty}`")))?;
        let d: Decimal = r.try_get("debit").map_err(map_db_error)?;
        let c: Decimal = r.try_get("credit").map_err(map_db_error)?;
        td += d;
        tc += c;
        out.push(TrialBalanceRow {
            ledger_account_id: LedgerAccountId(r.try_get("id").map_err(map_db_error)?),
            code: r.try_get("code").map_err(map_db_error)?,
            name: r.try_get("name").map_err(map_db_error)?,
            account_type: ty,
            debit: fmt(d),
            credit: fmt(c),
            balance: fmt(normal_balance(ty, d, c)),
        });
    }
    Ok(TrialBalanceView {
        as_of,
        period_id,
        rows: out,
        total_debit: fmt(td),
        total_credit: fmt(tc),
        balanced: td == tc,
        currency: "INR".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn balances_follow_the_normal_side() {
        let (d, c) = (Decimal::new(500, 0), Decimal::new(200, 0));
        assert_eq!(
            normal_balance(LedgerAccountType::Asset, d, c),
            Decimal::new(300, 0)
        );
        assert_eq!(
            normal_balance(LedgerAccountType::Income, d, c),
            Decimal::new(-300, 0)
        );
        assert_eq!(
            normal_balance(LedgerAccountType::Liability, c, d),
            Decimal::new(300, 0)
        );
    }
}
