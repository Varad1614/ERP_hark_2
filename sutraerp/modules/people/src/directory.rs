//! Foundation read adapters over the shared People tables (contract doc §6.2):
//!
//! * [`PgWorkforceDirectory`] — `WorkforceDirectory` ("7A → 7B / 7C") over
//!   `people.staff_member` / `people.staff_engagement`.
//! * [`PgLeaveReader`] — `LeaveReader` ("7B → 7C") over `people.leave_ledger_entry` and
//!   `people.attendance_day`.
//!
//! Every query filters on the scope's tenant; another tenant's rows are indistinguishable from
//! missing.

use crate::shared::leave::{balance_from_row, BALANCE_SQL};
use crate::shared::rows::{engagement_from_row, staff_from_row, ENGAGEMENT_COLUMNS, STAFF_COLUMNS};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use sutra_contracts::people::ids::{LeaveLedgerEntryId, StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::{
    EncashmentEntry, EngagementSummary, LeaveBalance, LeavePayrollInputs, LeaveReader,
    StaffSummary, WorkforceDirectory,
};
use sutra_contracts::people::status::StaffStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

/// [`WorkforceDirectory`] over the foundation tables (**real** from day one).
#[derive(Clone)]
pub struct PgWorkforceDirectory {
    pool: PgPool,
}

impl PgWorkforceDirectory {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn engagements(
        &self,
        where_sql: &str,
        binds: impl FnOnce(
            sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
        )
            -> sqlx::query::Query<'_, sqlx::Postgres, sqlx::postgres::PgArguments>,
    ) -> Result<Vec<EngagementSummary>, KernelError> {
        let sql =
            format!("SELECT {ENGAGEMENT_COLUMNS} FROM people.staff_engagement WHERE {where_sql}");
        let rows = binds(sqlx::query(&sql))
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(engagement_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }
}

#[async_trait::async_trait]
impl WorkforceDirectory for PgWorkforceDirectory {
    async fn staff(
        &self,
        scope: &ExecutionScope,
        id: StaffMemberId,
    ) -> Result<Option<StaffSummary>, KernelError> {
        let sql = format!(
            "SELECT {STAFF_COLUMNS} FROM people.staff_member WHERE id = $1 AND tenant_id = $2"
        );
        sqlx::query(&sql)
            .bind(id.as_uuid())
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?
            .as_ref()
            .map(staff_from_row)
            .transpose()
            .map_err(map_db_error)
    }

    async fn staff_by_subject(
        &self,
        scope: &ExecutionScope,
        subject_ref: &str,
    ) -> Result<Option<StaffSummary>, KernelError> {
        let sql = format!(
            "SELECT {STAFF_COLUMNS} FROM people.staff_member WHERE tenant_id = $1 \
             AND subject_ref = $2 AND status NOT IN ('merged', 'archived')"
        );
        sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(subject_ref)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?
            .as_ref()
            .map(staff_from_row)
            .transpose()
            .map_err(map_db_error)
    }

    async fn staff_of_institution(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        status: Option<StaffStatus>,
    ) -> Result<Vec<StaffSummary>, KernelError> {
        let sql = format!(
            "SELECT {STAFF_COLUMNS} FROM people.staff_member WHERE tenant_id = $1 \
             AND institution_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY staff_number"
        );
        let rows = sqlx::query(&sql)
            .bind(scope.tenant_id.as_uuid())
            .bind(institution_id.as_uuid())
            .bind(status.map(|s| s.as_str()))
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(staff_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn engagement(
        &self,
        scope: &ExecutionScope,
        id: StaffEngagementId,
    ) -> Result<Option<EngagementSummary>, KernelError> {
        let tenant = scope.tenant_id.as_uuid();
        Ok(self
            .engagements("id = $1 AND tenant_id = $2", |q| {
                q.bind(id.as_uuid()).bind(tenant)
            })
            .await?
            .into_iter()
            .next())
    }

    async fn current_engagement(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
    ) -> Result<Option<EngagementSummary>, KernelError> {
        let mut conn = self.pool.acquire().await.map_err(map_db_error)?;
        crate::shared::rows::current_engagement(&mut conn, scope.tenant_id, staff_id).await
    }

    async fn engagements_of_staff(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
    ) -> Result<Vec<EngagementSummary>, KernelError> {
        let tenant = scope.tenant_id.as_uuid();
        self.engagements(
            "staff_id = $1 AND tenant_id = $2 ORDER BY start_date DESC, created_at DESC",
            |q| q.bind(staff_id.as_uuid()).bind(tenant),
        )
        .await
    }

    async fn payable_engagements(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<Vec<EngagementSummary>, KernelError> {
        let tenant = scope.tenant_id.as_uuid();
        self.engagements(
            "tenant_id = $1 AND institution_id = $2 AND start_date <= $4 AND ( \
             (status IN ('active', 'suspended', 'deputed', 'long_leave') \
              AND (end_date IS NULL OR end_date >= $3)) \
             OR (status = 'closed' AND last_working_date BETWEEN $3 AND $4)) \
             ORDER BY engagement_number",
            |q| {
                q.bind(tenant)
                    .bind(institution_id.as_uuid())
                    .bind(from)
                    .bind(to)
            },
        )
        .await
    }

    async fn reports_of(
        &self,
        scope: &ExecutionScope,
        manager: StaffMemberId,
    ) -> Result<Vec<EngagementSummary>, KernelError> {
        let tenant = scope.tenant_id.as_uuid();
        self.engagements(
            "tenant_id = $1 AND reports_to_staff_id = $2 AND status NOT IN ('closed', 'cancelled') \
             ORDER BY engagement_number",
            |q| q.bind(tenant).bind(manager.as_uuid()),
        )
        .await
    }
}

/// [`LeaveReader`] over the leave ledger and attendance days (**real** from day one).
#[derive(Clone)]
pub struct PgLeaveReader {
    pool: PgPool,
}

impl PgLeaveReader {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl LeaveReader for PgLeaveReader {
    async fn balances(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
        leave_year: i32,
    ) -> Result<Vec<LeaveBalance>, KernelError> {
        let rows = sqlx::query(BALANCE_SQL)
            .bind(scope.tenant_id.as_uuid())
            .bind(staff_id.as_uuid())
            .bind(leave_year)
            .bind(Option::<uuid::Uuid>::None)
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(balance_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn payroll_inputs(
        &self,
        scope: &ExecutionScope,
        staff_id: StaffMemberId,
        from: NaiveDate,
        to: NaiveDate,
    ) -> Result<LeavePayrollInputs, KernelError> {
        let tenant = scope.tenant_id.as_uuid();
        // Unpaid consumption effective in the period, net of reversals of such consumption.
        let unpaid: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(-sum(e.days), 0) FROM people.leave_ledger_entry e \
             LEFT JOIN people.leave_ledger_entry r ON r.id = e.reverses_entry_id \
             WHERE e.tenant_id = $1 AND e.staff_id = $2 AND NOT e.is_paid \
             AND COALESCE(r.kind, e.kind) = 'consumption' \
             AND COALESCE(r.effective_on, e.effective_on) BETWEEN $3 AND $4",
        )
        .bind(tenant)
        .bind(staff_id.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        let absent: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(sum(CASE status WHEN 'absent' THEN 1.00 WHEN 'half_day' THEN 0.50 \
             ELSE 0 END), 0)::numeric(8,2) FROM people.attendance_day \
             WHERE tenant_id = $1 AND staff_id = $2 AND work_date BETWEEN $3 AND $4",
        )
        .bind(tenant)
        .bind(staff_id.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_one(&self.pool)
        .await
        .map_err(map_db_error)?;
        // Encashment entries effective in the period that were not reversed.
        let rows = sqlx::query(
            "SELECT e.id, e.leave_type_id, e.leave_type_code, -e.days AS days, e.effective_on \
             FROM people.leave_ledger_entry e WHERE e.tenant_id = $1 AND e.staff_id = $2 \
             AND e.kind = 'encashment' AND e.effective_on BETWEEN $3 AND $4 \
             AND NOT EXISTS (SELECT 1 FROM people.leave_ledger_entry r WHERE r.reverses_entry_id = e.id) \
             ORDER BY e.effective_on, e.created_at",
        )
        .bind(tenant)
        .bind(staff_id.as_uuid())
        .bind(from)
        .bind(to)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        let encashments = rows
            .iter()
            .map(|r| {
                Ok(EncashmentEntry {
                    entry_id: LeaveLedgerEntryId(r.try_get("id")?),
                    leave_type_id: r.try_get("leave_type_id")?,
                    leave_type_code: r.try_get("leave_type_code")?,
                    days: r.try_get("days")?,
                    effective_on: r.try_get("effective_on")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(map_db_error)?;
        Ok(LeavePayrollInputs {
            staff_id,
            from,
            to,
            unpaid_leave_days: unpaid,
            absent_days: absent,
            encashments,
        })
    }
}
