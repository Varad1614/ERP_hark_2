//! The default chart of accounts (contract §3.2) and ledger account resolution.
//!
//! Every institution gets the [`DEFAULT_CHART`] the first time Finance needs it
//! ([`ensure_default_chart`], idempotent — called by `shared::posting` and by 5A before creating
//! fee heads). Seeded accounts carry a `system_key` ([`SystemAccount`]) so postings can name them
//! by role (`AccountRef::System`). 5C manages accounts afterwards (new accounts, deactivation);
//! seeded accounts are never renamed by code.

use sqlx::{PgConnection, Row};
use sutra_contracts::finance::ids::LedgerAccountId;
use sutra_contracts::finance::ports::{AccountRef, SystemAccount};
use sutra_contracts::finance::status::{LedgerAccountStatus, LedgerAccountType};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// `(role, code, name, type)` of every seeded account.
pub const DEFAULT_CHART: [(SystemAccount, &str, &str, LedgerAccountType); 14] = [
    (
        SystemAccount::CashOnHand,
        "1100",
        "Cash on hand",
        LedgerAccountType::Asset,
    ),
    (
        SystemAccount::BankMain,
        "1200",
        "Bank — main collection account",
        LedgerAccountType::Asset,
    ),
    (
        SystemAccount::GatewayClearing,
        "1250",
        "Payment gateway clearing",
        LedgerAccountType::Asset,
    ),
    (
        SystemAccount::StudentReceivables,
        "1300",
        "Student fee receivables",
        LedgerAccountType::Asset,
    ),
    (
        SystemAccount::UnappliedReceipts,
        "2100",
        "Unapplied receipts and student advances",
        LedgerAccountType::Liability,
    ),
    (
        SystemAccount::RefundsPayable,
        "2200",
        "Refunds payable",
        LedgerAccountType::Liability,
    ),
    (
        SystemAccount::DepositsHeld,
        "2300",
        "Refundable deposits held",
        LedgerAccountType::Liability,
    ),
    (
        SystemAccount::AccumulatedFund,
        "3100",
        "Accumulated fund",
        LedgerAccountType::Equity,
    ),
    (
        SystemAccount::FeeIncome,
        "4100",
        "Tuition and academic fee income",
        LedgerAccountType::Income,
    ),
    (
        SystemAccount::OtherIncome,
        "4900",
        "Other fee income",
        LedgerAccountType::Income,
    ),
    (
        SystemAccount::AidExpense,
        "5100",
        "Scholarships and financial aid",
        LedgerAccountType::Expense,
    ),
    (
        SystemAccount::ConcessionExpense,
        "5200",
        "Fee concessions and waivers",
        LedgerAccountType::Expense,
    ),
    (
        SystemAccount::BadDebtExpense,
        "5300",
        "Fees written off",
        LedgerAccountType::Expense,
    ),
    (
        SystemAccount::BankCharges,
        "5900",
        "Bank and gateway charges",
        LedgerAccountType::Expense,
    ),
];

/// A ledger account as postings need it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LedgerAccountRow {
    pub id: LedgerAccountId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub account_type: LedgerAccountType,
    pub system_key: Option<String>,
    pub is_postable: bool,
    pub currency: String,
    pub status: LedgerAccountStatus,
}

/// Inserts the missing [`DEFAULT_CHART`] accounts of an institution (idempotent). An existing
/// account wins on either unique key: a code already in use **or** a system role already held by
/// an account under another code (a chart imported or seeded with its own numbering) is left
/// untouched instead of failing the caller with a unique violation (409).
pub async fn ensure_default_chart(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<(), KernelError> {
    for (role, code, name, ty) in DEFAULT_CHART {
        sqlx::query(
            "INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, \
             account_type, normal_balance, system_key) VALUES ($1, $2, $3, $4, $5, $6, $7) \
             ON CONFLICT DO NOTHING",
        )
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .bind(code)
        .bind(name)
        .bind(ty.as_str())
        .bind(ty.normal_balance().as_str())
        .bind(role.key())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

const COLUMNS: &str = "id, tenant_id, institution_id, code, name, account_type, system_key, \
    is_postable, currency, status";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<LedgerAccountRow, sqlx::Error> {
    let ty: String = r.try_get("account_type")?;
    let st: String = r.try_get("status")?;
    Ok(LedgerAccountRow {
        id: LedgerAccountId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        account_type: LedgerAccountType::parse(&ty)
            .ok_or_else(|| sqlx::Error::Decode(format!("account type `{ty}`").into()))?,
        system_key: r.try_get("system_key")?,
        is_postable: r.try_get("is_postable")?,
        currency: r.try_get("currency")?,
        status: LedgerAccountStatus::parse(&st)
            .ok_or_else(|| sqlx::Error::Decode(format!("account status `{st}`").into()))?,
    })
}

/// Resolves an account of `institution` (seeding the default chart when a system account is
/// missing). 422-style conflict when the account is foreign, inactive or not postable.
pub async fn resolve(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    account: AccountRef,
) -> Result<LedgerAccountRow, KernelError> {
    let row = match account {
        AccountRef::Id(id) => fetch(conn, tenant, institution, Some(id.as_uuid()), None).await?,
        AccountRef::System(role) => {
            match fetch(conn, tenant, institution, None, Some(role.key())).await? {
                Some(r) => Some(r),
                None => {
                    ensure_default_chart(conn, tenant, institution).await?;
                    fetch(conn, tenant, institution, None, Some(role.key())).await?
                }
            }
        }
    };
    let row = row.ok_or(KernelError::NotFound {
        entity: "ledger account",
    })?;
    if row.status != LedgerAccountStatus::Active || !row.is_postable {
        return Err(KernelError::Conflict(format!(
            "ledger account {} {} does not accept postings",
            row.code, row.name
        )));
    }
    Ok(row)
}

async fn fetch(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    id: Option<Uuid>,
    key: Option<&str>,
) -> Result<Option<LedgerAccountRow>, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM finance.ledger_account WHERE tenant_id = $1 AND institution_id = $2 \
         AND (($3::uuid IS NOT NULL AND id = $3) OR ($4::text IS NOT NULL AND system_key = $4))"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(id)
    .bind(key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.as_ref().map(from_row).transpose().map_err(map_db_error)
}

/// `(system_key, code, name)` of the GST output-tax liability accounts (ERP_EXTENSIONS 5D),
/// seeded on first use by [`ensure_gst_accounts`] **outside** the Batch 5 default chart (the same
/// way as the payroll accounts), in [`crate::gst::rules::Component`] order.
pub const GST_ACCOUNTS: [(&str, &str, &str); 3] = [
    ("output_cgst", "2510", "Output CGST payable"),
    ("output_sgst", "2520", "Output SGST / UTGST payable"),
    ("output_igst", "2530", "Output IGST payable"),
];

/// Seeds the GST output-tax accounts of an institution (idempotent) and returns their ids in
/// [`GST_ACCOUNTS`] order. An account already holding the role (a chart with its own numbering)
/// is kept.
pub async fn ensure_gst_accounts(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<[LedgerAccountId; 3], KernelError> {
    ensure_default_chart(conn, tenant, institution).await?;
    let mut ids = [LedgerAccountId(Uuid::nil()); 3];
    for (i, (key, code, name)) in GST_ACCOUNTS.iter().enumerate() {
        let ty = LedgerAccountType::Liability;
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
        let row = fetch(conn, tenant, institution, None, Some(key))
            .await?
            .ok_or(KernelError::Conflict(format!(
                "ledger account code {code} is taken; free it or give an account the `{key}` role"
            )))?;
        ids[i] = row.id;
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_chart_covers_every_system_account_once() {
        for role in SystemAccount::ALL {
            assert_eq!(
                DEFAULT_CHART.iter().filter(|(r, ..)| *r == role).count(),
                1,
                "{role:?}"
            );
        }
        let codes: std::collections::HashSet<_> =
            DEFAULT_CHART.iter().map(|(_, c, ..)| c).collect();
        assert_eq!(codes.len(), DEFAULT_CHART.len());
    }
}
