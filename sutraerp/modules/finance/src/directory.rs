//! Foundation adapters of the Domain F ports over the shared tables (contract §6.2):
//!
//! * [`PgChargeDirectory`] — `ChargeDirectory` (charges + party accounts);
//! * [`PgReceivableReader`] — `ReceivableReader` (party balances, outstanding / overdue);
//! * [`PgPostingPort`] — `PostingPort` (own transaction around `shared::posting`).
//!
//! 5B and 5C read charges only through these (or the read-only `shared::charge::load_*`);
//! other domains get them from the composition root.

use crate::shared::account::{account_from_row, ACCOUNT_COLUMNS};
use crate::shared::charge::{charge_from_row, CHARGE_COLUMNS};
use crate::shared::posting;
use crate::shared::Actor;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use sutra_contracts::finance::ids::{ChargeId, JournalEntryId, StudentAccountId};
use sutra_contracts::finance::ports::{
    AccountBalance, ChargeDirectory, ChargeSummary, PostingPort, PostingReceipt, PostingRequest,
    ReceivableReader, StudentAccountSummary,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::request_id::current_correlation_id;
use uuid::Uuid;

/// [`ChargeDirectory`] over `finance.charge` / `finance.student_account`.
#[derive(Clone)]
pub struct PgChargeDirectory {
    pool: PgPool,
}

impl PgChargeDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn charges(
        &self,
        scope: &ExecutionScope,
        account: StudentAccountId,
        open_only: bool,
    ) -> Result<Vec<ChargeSummary>, KernelError> {
        let sql = if open_only {
            format!(
                "SELECT {CHARGE_COLUMNS} FROM finance.charge WHERE tenant_id = $1 \
                 AND student_account_id = $2 AND status IN ('published', 'active', 'disputed') \
                 AND outstanding_amount > 0 ORDER BY due_date NULLS LAST, charge_number"
            )
        } else {
            format!(
                "SELECT {CHARGE_COLUMNS} FROM finance.charge WHERE tenant_id = $1 \
                 AND student_account_id = $2 ORDER BY created_at DESC, charge_number DESC"
            )
        };
        let rows = sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(account.as_uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(|r| charge_from_row(r).map(|c| c.summary()))
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn account_where(
        &self,
        scope: &ExecutionScope,
        filter: &str,
        institution: Option<InstitutionId>,
        key: Uuid,
    ) -> Result<Option<StudentAccountSummary>, KernelError> {
        let row = sqlx::query(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM finance.student_account WHERE tenant_id = $1 \
             AND ($2::uuid IS NULL OR institution_id = $2) AND {filter}"
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(institution.map(|i| i.as_uuid()))
        .bind(key)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.as_ref()
            .map(|r| account_from_row(r).map(|a| a.account))
            .transpose()
            .map_err(map_db_error)
    }
}

#[async_trait::async_trait]
impl ChargeDirectory for PgChargeDirectory {
    async fn charge(
        &self,
        scope: &ExecutionScope,
        id: ChargeId,
    ) -> Result<Option<ChargeSummary>, KernelError> {
        let row = sqlx::query(&format!(
            "SELECT {CHARGE_COLUMNS} FROM finance.charge WHERE tenant_id = $1 AND id = $2"
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.as_ref()
            .map(|r| charge_from_row(r).map(|c| c.summary()))
            .transpose()
            .map_err(map_db_error)
    }

    async fn charges_of_account(
        &self,
        scope: &ExecutionScope,
        account: StudentAccountId,
    ) -> Result<Vec<ChargeSummary>, KernelError> {
        self.charges(scope, account, false).await
    }

    async fn open_charges_of_account(
        &self,
        scope: &ExecutionScope,
        account: StudentAccountId,
    ) -> Result<Vec<ChargeSummary>, KernelError> {
        self.charges(scope, account, true).await
    }

    async fn student_account(
        &self,
        scope: &ExecutionScope,
        id: StudentAccountId,
    ) -> Result<Option<StudentAccountSummary>, KernelError> {
        self.account_where(scope, "id = $3", None, id.as_uuid())
            .await
    }

    async fn account_of_student(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        student_id: Uuid,
    ) -> Result<Option<StudentAccountSummary>, KernelError> {
        self.account_where(scope, "student_id = $3", Some(institution_id), student_id)
            .await
    }

    async fn account_of_admission(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        admission_id: Uuid,
    ) -> Result<Option<StudentAccountSummary>, KernelError> {
        self.account_where(
            scope,
            "admission_id = $3 AND student_id IS NULL",
            Some(institution_id),
            admission_id,
        )
        .await
    }

    async fn accounts_of_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Vec<StudentAccountSummary>, KernelError> {
        let rows = sqlx::query(&format!(
            "SELECT {ACCOUNT_COLUMNS} FROM finance.student_account WHERE tenant_id = $1 \
             AND subject_ref = $2 AND status <> 'closed' ORDER BY created_at"
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(subject_ref)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        rows.iter()
            .map(|r| account_from_row(r).map(|a| a.account))
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }
}

/// SQL aggregate of one account's balance (`$1` tenant, `$2` account, `$3` as-of date).
pub const BALANCE_SQL: &str = "SELECT a.id, a.currency, \
    COALESCE(sum(c.total_amount) FILTER (WHERE c.status NOT IN ('draft', 'pending_validation', 'validated', 'cancelled')), 0) AS total_charged, \
    COALESCE(sum(c.adjustment_amount) FILTER (WHERE c.status NOT IN ('draft', 'pending_validation', 'validated', 'cancelled')), 0) AS total_adjusted, \
    COALESCE(sum(c.settled_amount), 0) AS total_settled, \
    COALESCE(sum(c.outstanding_amount) FILTER (WHERE c.status IN ('published', 'active')), 0) AS outstanding, \
    COALESCE(sum(c.outstanding_amount) FILTER (WHERE c.status = 'disputed'), 0) AS disputed_outstanding, \
    COALESCE(sum(c.outstanding_amount) FILTER (WHERE c.status IN ('published', 'active') AND c.due_date < $3), 0) AS overdue, \
    count(c.id) FILTER (WHERE c.status IN ('published', 'active', 'disputed') AND c.outstanding_amount > 0) AS open_charges, \
    count(c.id) FILTER (WHERE c.status NOT IN ('draft', 'pending_validation', 'validated')) AS charges_raised \
    FROM finance.student_account a LEFT JOIN finance.charge c ON c.student_account_id = a.id AND c.tenant_id = a.tenant_id \
    WHERE a.tenant_id = $1 AND a.id = $2 GROUP BY a.id, a.currency";

/// Decodes a [`BALANCE_SQL`] row.
pub fn balance_from_row(r: &sqlx::postgres::PgRow) -> Result<AccountBalance, sqlx::Error> {
    Ok(AccountBalance {
        student_account_id: StudentAccountId(r.try_get("id")?),
        currency: r.try_get("currency")?,
        total_charged: r.try_get::<Decimal, _>("total_charged")?,
        total_adjusted: r.try_get("total_adjusted")?,
        total_settled: r.try_get("total_settled")?,
        outstanding: r.try_get("outstanding")?,
        disputed_outstanding: r.try_get("disputed_outstanding")?,
        overdue: r.try_get("overdue")?,
        open_charges: r.try_get("open_charges")?,
        charges_raised: r.try_get("charges_raised")?,
    })
}

/// [`ReceivableReader`] over the charge table.
#[derive(Clone)]
pub struct PgReceivableReader {
    pool: PgPool,
}

impl PgReceivableReader {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ReceivableReader for PgReceivableReader {
    async fn account_balance(
        &self,
        scope: &ExecutionScope,
        account: StudentAccountId,
        as_of: NaiveDate,
    ) -> Result<Option<AccountBalance>, KernelError> {
        let row = sqlx::query(BALANCE_SQL)
            .bind(scope.tenant_id.as_uuid())
            .bind(account.as_uuid())
            .bind(as_of)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        row.as_ref()
            .map(balance_from_row)
            .transpose()
            .map_err(map_db_error)
    }
}

/// [`PostingPort`]: one owner-local transaction around `shared::posting::{post, reverse}`.
#[derive(Clone)]
pub struct PgPostingPort {
    pool: PgPool,
}

impl PgPostingPort {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl PostingPort for PgPostingPort {
    async fn post(
        &self,
        scope: &ExecutionScope,
        request: PostingRequest,
    ) -> Result<PostingReceipt, KernelError> {
        if scope
            .institution_id
            .is_some_and(|i| i != request.institution_id)
        {
            return Err(KernelError::Scope(
                "posting outside the caller's institution".into(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let r = posting::post(
            &mut tx,
            scope.tenant_id,
            &request,
            &Actor::system(current_correlation_id()),
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(r)
    }

    async fn reverse(
        &self,
        scope: &ExecutionScope,
        entry: JournalEntryId,
        posting_date: NaiveDate,
        reason: String,
    ) -> Result<PostingReceipt, KernelError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let r = posting::reverse(
            &mut tx,
            scope.tenant_id,
            entry,
            posting_date,
            &reason,
            &Actor::system(current_correlation_id()),
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(r)
    }
}
