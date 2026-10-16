//! Part 2 · S4-import: Domain F import targets (`docs/readiness/part2/s4-import.md`).
//!
//! * [`PgFeeHeadImport`] (`fee_heads`) — fee heads through 5A's [`create_head_in`] (code rules,
//!   default chart, income account by category or by ledger code, audit). Natural key: `code`.
//! * [`PgOpeningBalanceImport`] (`opening_balances`) — a student's outstanding balance from the
//!   legacy system as one ordinary, payable charge (`source_type = opening_balance`,
//!   `source_id` = the student's finance account, so a re-import never duplicates it) created,
//!   validated and **published through 5A's charge flow**: the posting engine writes a balanced
//!   journal entry Dr student receivables / Cr accumulated fund (`OPENING-BAL` fee head) and
//!   `charge.published` goes out. Commits need a second person's approval (the import job's
//!   separation of duties); [`ImportTargetPort::ledger_total`] reads the posted receivable
//!   debits back for the reconciliation summary.

use crate::charges::fees::{create_head_in, NewFeeHead};
use crate::charges::repo::{create_in_tx, line, publish_locked, validate_locked, ChargeInput};
use crate::shared::account::ensure_student_account;
use crate::shared::coa::{ensure_default_chart, resolve};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::ports::{AccountRef, SystemAccount};
use sutra_contracts::finance::status::{ChargeSourceType, FeeCategory};
use sutra_contracts::platform::dataimport::{
    entity, outcomes, parse_amount, parse_bool, parse_date, BatchOutcome, ColumnKind, ColumnSpec,
    EntitySpec, ImportBatch, ImportRow, ImportTargetPort, OnExisting, RowAction, RowOutcome,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::batch::{self, ItemWriter};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Fee head every opening balance is raised under (credits the accumulated fund).
pub const OPENING_BALANCE_FEE_HEAD: &str = "OPENING-BAL";

const CATEGORIES: &[&str] = &[
    "tuition",
    "admission",
    "examination",
    "hostel",
    "transport",
    "library",
    "laboratory",
    "fine",
    "deposit",
    "miscellaneous",
];

fn actor_of(batch: &ImportBatch) -> Actor {
    Actor {
        id: batch.actor.clone(),
        correlation_id: sutra_web::request_id::current_correlation_id(),
    }
}

fn violation(field: &str, code: &str, message: impl Into<String>) -> FieldViolation {
    FieldViolation::new(field, code, message.into())
}

fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![violation(field, code, message)])
}

fn finish(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

fn put(m: &mut BTreeMap<String, String>, k: &str, v: Option<String>) {
    if let Some(v) = v.filter(|v| !v.is_empty()) {
        m.insert(k.to_owned(), v);
    }
}

fn lines_of(batch: &ImportBatch) -> Vec<u32> {
    batch.rows.iter().map(|r| r.line).collect()
}

// ================================================================== fee heads

/// `fee_heads` import target.
#[derive(Clone)]
pub struct PgFeeHeadImport {
    state: FinanceState,
}

impl PgFeeHeadImport {
    pub fn new(state: FinanceState) -> Self {
        Self { state }
    }
}

struct FeeHeadWriter {
    scope: ExecutionScope,
    institution: InstitutionId,
    actor: Actor,
}

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for FeeHeadWriter {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let mut v = Vec::new();
        let code = row.get("code").unwrap_or("").to_uppercase();
        let name = row.get("name").unwrap_or("").to_owned();
        let category = row.get("category").unwrap_or("").to_lowercase();
        let category = match FeeCategory::parse(&category) {
            Some(c) => Some(c),
            None => {
                v.push(violation(
                    "category",
                    "choice",
                    format!("category must be one of {}", CATEGORIES.join(", ")),
                ));
                None
            }
        };
        let refundable = match row.get("refundable").map(|b| parse_bool("refundable", b)) {
            Some(Ok(b)) => b,
            Some(Err(e)) => {
                v.push(e);
                false
            }
            None => false,
        };
        finish(v)?;
        let Some(category) = category else {
            return Err(invalid("category", "choice", "category is required"));
        };
        let tenant = self.scope.tenant_id;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 \
             AND code = $3",
        )
        .bind(tenant.as_uuid())
        .bind(self.institution.as_uuid())
        .bind(&code)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if let Some(id) = existing {
            return Ok(RowOutcome::done(row.line, RowAction::Skip, id));
        }
        let income_account_id = match row.get("income_account_code") {
            None => None,
            Some(account_code) => {
                ensure_default_chart(&mut *conn, tenant, self.institution).await?;
                let id: Option<Uuid> = sqlx::query_scalar(
                    "SELECT id FROM finance.ledger_account WHERE tenant_id = $1 \
                     AND institution_id = $2 AND code = $3",
                )
                .bind(tenant.as_uuid())
                .bind(self.institution.as_uuid())
                .bind(account_code)
                .fetch_optional(&mut *conn)
                .await
                .map_err(map_db_error)?;
                Some(id.ok_or_else(|| {
                    invalid(
                        "income_account_code",
                        "unknown",
                        format!("ledger account {account_code} does not exist"),
                    )
                })?)
            }
        };
        let head = create_head_in(
            conn,
            &self.scope,
            self.institution,
            NewFeeHead {
                code,
                name,
                category,
                income_account_id,
                refundable,
            },
            &self.actor,
        )
        .await
        .map_err(|e| match e {
            // 5A names API members; the import reports file columns.
            KernelError::Validation(v) => KernelError::Validation(
                v.into_iter()
                    .map(|mut f| {
                        if f.field == "incomeAccountId" {
                            f.field = "income_account_code".into();
                        }
                        f
                    })
                    .collect(),
            ),
            other => other,
        })?;
        Ok(RowOutcome::done(row.line, RowAction::Create, head.id))
    }
}

fn fee_heads_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::FEE_HEADS,
        label: "Fee heads",
        owner: "finance",
        columns: vec![
            ColumnSpec::new("code", "Code", Code)
                .required()
                .example("TUITION"),
            ColumnSpec::new("name", "Name", Text)
                .required()
                .example("Tuition fee"),
            ColumnSpec::new("category", "Category", Choice)
                .required()
                .choices(CATEGORIES)
                .example("tuition"),
            ColumnSpec::new("income_account_code", "Income account code", Text)
                .example("4100")
                .help("Ledger account code; the category's default income account when empty"),
            ColumnSpec::new("refundable", "Refundable", Boolean).example("false"),
        ],
        natural_key: &["code"],
        amount_column: None,
        group_by: None,
        supports_update: false,
        requires_approval: false,
        permission: Permission::ChargeManage,
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgFeeHeadImport {
    fn spec(&self) -> EntitySpec {
        fee_heads_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        if batch.on_existing == OnExisting::Update {
            return Err(KernelError::BadRequest(
                "fee heads are not updated by an import; use on_existing = skip".into(),
            ));
        }
        let inst = batch.institution_id;
        let mut w = FeeHeadWriter {
            scope: ExecutionScope::institution(scope.tenant_id, inst),
            institution: inst,
            actor: actor_of(&batch),
        };
        let run = batch::run(&self.state.pool, &batch.rows, batch.dry_run, &mut w).await?;
        Ok(BatchOutcome {
            rows: outcomes(&lines_of(&batch), run.results)?,
            committed: run.committed,
        })
    }

    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
        let rows = sqlx::query(
            "SELECT h.code, h.name, h.category, a.code AS account_code, h.refundable \
             FROM finance.fee_head h JOIN finance.ledger_account a ON a.id = h.income_account_id \
             WHERE h.tenant_id = $1 AND h.institution_id = $2 AND h.code <> $3 ORDER BY h.code",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(OPENING_BALANCE_FEE_HEAD)
        .fetch_all(&self.state.pool)
        .await
        .map_err(map_db_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut m = BTreeMap::new();
            put(&mut m, "code", r.try_get("code").map_err(map_db_error)?);
            put(&mut m, "name", r.try_get("name").map_err(map_db_error)?);
            put(
                &mut m,
                "category",
                r.try_get("category").map_err(map_db_error)?,
            );
            put(
                &mut m,
                "income_account_code",
                r.try_get("account_code").map_err(map_db_error)?,
            );
            let refundable: bool = r.try_get("refundable").map_err(map_db_error)?;
            put(&mut m, "refundable", Some(refundable.to_string()));
            out.push(m);
        }
        Ok(out)
    }
}

// ================================================================== opening balances

/// `opening_balances` import target.
#[derive(Clone)]
pub struct PgOpeningBalanceImport {
    state: FinanceState,
}

impl PgOpeningBalanceImport {
    pub fn new(state: FinanceState) -> Self {
        Self { state }
    }
}

struct BalanceCells {
    student_number: String,
    amount: Decimal,
    as_of: Option<NaiveDate>,
    due_date: Option<NaiveDate>,
    description: String,
}

fn balance_cells(row: &ImportRow) -> Result<BalanceCells, KernelError> {
    let mut v = Vec::new();
    let number = row.get("student_number").unwrap_or("").to_owned();
    if number.is_empty() {
        v.push(violation(
            "student_number",
            "required",
            "student_number is required",
        ));
    }
    let amount = match row.get("amount").map(|a| parse_amount("amount", a)) {
        Some(Ok(a)) if a > Decimal::ZERO => a,
        Some(Ok(_)) => {
            v.push(violation(
                "amount",
                "positive",
                "amount must be greater than zero (credit balances are migrated as receipts)",
            ));
            Decimal::ZERO
        }
        Some(Err(e)) => {
            v.push(e);
            Decimal::ZERO
        }
        None => {
            v.push(violation("amount", "required", "amount is required"));
            Decimal::ZERO
        }
    };
    let mut date = |c: &str| match row.get(c).map(|d| parse_date(c, d)) {
        Some(Ok(d)) => Some(d),
        Some(Err(e)) => {
            v.push(e);
            None
        }
        None => None,
    };
    let as_of = date("as_of_date");
    let due_date = date("due_date");
    let description = row
        .get("description")
        .unwrap_or("Opening balance migrated from the legacy system")
        .to_owned();
    if description.chars().count() > 500 {
        v.push(violation(
            "description",
            "too-long",
            "description must be at most 500 characters",
        ));
    }
    finish(v)?;
    Ok(BalanceCells {
        student_number: number,
        amount,
        as_of,
        due_date,
        description,
    })
}

struct BalanceWriter<'a> {
    state: &'a FinanceState,
    scope: ExecutionScope,
    institution: InstitutionId,
    today: NaiveDate,
    actor: Actor,
    fee_head: Option<Uuid>,
}

impl BalanceWriter<'_> {
    /// The institution's `OPENING-BAL` fee head (Cr accumulated fund), created on first use.
    async fn fee_head(&mut self, conn: &mut PgConnection) -> Result<Uuid, KernelError> {
        if let Some(id) = self.fee_head {
            // Created in an earlier row of this transaction (or found): still there unless
            // that row's savepoint was rolled back, so check it again.
            let still: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM finance.fee_head WHERE id = $1")
                    .bind(id)
                    .fetch_optional(&mut *conn)
                    .await
                    .map_err(map_db_error)?;
            if still.is_some() {
                return Ok(id);
            }
        }
        let tenant = self.scope.tenant_id;
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 \
             AND code = $3",
        )
        .bind(tenant.as_uuid())
        .bind(self.institution.as_uuid())
        .bind(OPENING_BALANCE_FEE_HEAD)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let id = match existing {
            Some(id) => id,
            None => {
                ensure_default_chart(&mut *conn, tenant, self.institution).await?;
                let fund = resolve(
                    &mut *conn,
                    tenant,
                    self.institution,
                    AccountRef::System(SystemAccount::AccumulatedFund),
                )
                .await?;
                create_head_in(
                    conn,
                    &self.scope,
                    self.institution,
                    NewFeeHead {
                        code: OPENING_BALANCE_FEE_HEAD.into(),
                        name: "Opening balance (data migration)".into(),
                        category: FeeCategory::Miscellaneous,
                        income_account_id: Some(fund.id.as_uuid()),
                        refundable: false,
                    },
                    &self.actor,
                )
                .await?
                .id
            }
        };
        self.fee_head = Some(id);
        Ok(id)
    }
}

#[async_trait::async_trait]
impl ItemWriter<ImportRow> for BalanceWriter<'_> {
    type Output = RowOutcome;

    async fn write(
        &mut self,
        conn: &mut PgConnection,
        row: &ImportRow,
    ) -> Result<RowOutcome, KernelError> {
        let c = balance_cells(row)?;
        let student = self
            .state
            .students
            .by_number(&self.scope, &c.student_number)
            .await?
            .ok_or_else(|| {
                invalid(
                    "student_number",
                    "unknown",
                    format!(
                        "student {} does not exist; import students first",
                        c.student_number
                    ),
                )
            })?;
        if self
            .state
            .students
            .live_context(&self.scope, student.id, self.institution)
            .await?
            .is_none()
        {
            return Err(invalid(
                "student_number",
                "no_live_context",
                "the student has no live record in this institution",
            ));
        }
        let tenant = self.scope.tenant_id;
        let account =
            ensure_student_account(conn, tenant, self.institution, &student, &self.actor).await?;
        let existing = sqlx::query(
            "SELECT id, total_amount FROM finance.charge WHERE tenant_id = $1 \
             AND source_type = 'opening_balance' AND source_id = $2",
        )
        .bind(tenant.as_uuid())
        .bind(account.id().as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if let Some(r) = existing {
            let id: Uuid = r.try_get("id").map_err(map_db_error)?;
            return Ok(RowOutcome::done(row.line, RowAction::Skip, id));
        }
        let fee_head = self.fee_head(conn).await?;
        let draft = create_in_tx(
            self.state,
            conn,
            &self.scope,
            self.institution,
            &ChargeInput {
                student_id: student.id,
                description: c.description.clone(),
                due_date: c.due_date,
                academic_year_id: None,
                academic_term_id: None,
                source_type: ChargeSourceType::OpeningBalance,
                source_id: Some(account.id().as_uuid()),
                lines: vec![line(fee_head, c.description.clone(), c.amount)],
            },
            &self.actor,
        )
        .await?;
        let validated = validate_locked(conn, &draft, &self.actor).await?;
        let (published, _journal) = publish_locked(
            conn,
            &validated,
            c.as_of.unwrap_or(self.today),
            None,
            &self.actor,
        )
        .await?;
        Ok(RowOutcome {
            line: row.line,
            action: RowAction::Create,
            record_id: Some(published.id.as_uuid()),
            amount: Some(published.total_amount),
            errors: vec![],
        })
    }
}

fn balances_spec() -> EntitySpec {
    use ColumnKind::*;
    EntitySpec {
        entity: entity::OPENING_BALANCES,
        label: "Opening fee balances",
        owner: "finance",
        columns: vec![
            ColumnSpec::new("student_number", "Student number", Text)
                .required()
                .example("S2023/0001"),
            ColumnSpec::new("amount", "Outstanding amount", Decimal)
                .required()
                .example("12500.00")
                .help("Amount the student still owes (greater than zero)"),
            ColumnSpec::new("as_of_date", "Balance date", Date)
                .example("2026-03-31")
                .help(
                    "Posting date of the opening entry (today when empty; its period must be open)",
                ),
            ColumnSpec::new("due_date", "Due date", Date).example("2026-04-30"),
            ColumnSpec::new("description", "Description", Text)
                .example("Balance carried forward from the legacy system"),
        ],
        natural_key: &["student_number"],
        amount_column: Some("amount"),
        group_by: None,
        supports_update: false,
        requires_approval: true,
        permission: Permission::ChargeManage,
    }
}

#[async_trait::async_trait]
impl ImportTargetPort for PgOpeningBalanceImport {
    fn spec(&self) -> EntitySpec {
        balances_spec()
    }

    async fn apply(
        &self,
        scope: &ExecutionScope,
        batch: ImportBatch,
    ) -> Result<BatchOutcome, KernelError> {
        if batch.on_existing == OnExisting::Update {
            return Err(KernelError::BadRequest(
                "opening balances are posted once; correct them with an adjustment".into(),
            ));
        }
        let inst = batch.institution_id;
        let scope = ExecutionScope::institution(scope.tenant_id, inst);
        let today = business_today(&self.state, &scope, inst).await?;
        let mut w = BalanceWriter {
            state: &self.state,
            scope,
            institution: inst,
            today,
            actor: actor_of(&batch),
            fee_head: None,
        };
        let run = batch::run(&self.state.pool, &batch.rows, batch.dry_run, &mut w).await?;
        Ok(BatchOutcome {
            rows: outcomes(&lines_of(&batch), run.results)?,
            committed: run.committed,
        })
    }

    async fn export(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
    ) -> Result<Vec<BTreeMap<String, String>>, KernelError> {
        let rows = sqlx::query(
            "SELECT a.student_number, c.total_amount, j.posting_date, c.due_date, c.description \
             FROM finance.charge c \
             JOIN finance.student_account a ON a.id = c.student_account_id \
             LEFT JOIN LATERAL (SELECT e.posting_date FROM finance.journal_entry e \
               WHERE e.tenant_id = c.tenant_id AND e.source_type = 'charge' \
               AND e.source_id = c.id ORDER BY e.created_at LIMIT 1) j ON true \
             WHERE c.tenant_id = $1 AND c.institution_id = $2 \
             AND c.source_type = 'opening_balance' ORDER BY a.student_number",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_all(&self.state.pool)
        .await
        .map_err(map_db_error)?;
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let mut m = BTreeMap::new();
            put(
                &mut m,
                "student_number",
                r.try_get("student_number").map_err(map_db_error)?,
            );
            let amount: Decimal = r.try_get("total_amount").map_err(map_db_error)?;
            put(&mut m, "amount", Some(format!("{amount:.2}")));
            let posted: Option<NaiveDate> = r.try_get("posting_date").map_err(map_db_error)?;
            put(&mut m, "as_of_date", posted.map(|d| d.to_string()));
            let due: Option<NaiveDate> = r.try_get("due_date").map_err(map_db_error)?;
            put(&mut m, "due_date", due.map(|d| d.to_string()));
            put(
                &mut m,
                "description",
                r.try_get("description").map_err(map_db_error)?,
            );
            out.push(m);
        }
        Ok(out)
    }

    /// Σ receivable debits the ledger holds for the given opening-balance charges (the journal
    /// lines of their publication entries).
    async fn ledger_total(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        record_ids: &[Uuid],
    ) -> Result<Option<Decimal>, KernelError> {
        let total: Decimal = sqlx::query_scalar(
            "SELECT COALESCE(sum(l.debit), 0) FROM finance.journal_line l \
             JOIN finance.journal_entry e ON e.id = l.journal_entry_id \
             JOIN finance.charge c ON c.id = l.charge_id \
             WHERE l.tenant_id = $1 AND e.institution_id = $2 AND l.charge_id = ANY($3) \
             AND c.source_type = 'opening_balance' AND l.debit > 0",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(record_ids)
        .fetch_one(&self.state.pool)
        .await
        .map_err(map_db_error)?;
        Ok(Some(total))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(cells: &[(&str, &str)]) -> ImportRow {
        ImportRow {
            line: 2,
            values: cells
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                .collect(),
        }
    }

    #[test]
    fn opening_balances_are_positive_two_decimal_amounts() {
        let ok = balance_cells(&row(&[("student_number", "S1"), ("amount", "1,250.50")])).unwrap();
        assert_eq!(ok.amount, "1250.50".parse::<Decimal>().unwrap());
        assert!(ok.description.contains("legacy"));
        for bad in ["0", "-5", "12.345", "abc"] {
            assert!(
                balance_cells(&row(&[("student_number", "S1"), ("amount", bad)])).is_err(),
                "{bad}"
            );
        }
        assert!(balance_cells(&row(&[("amount", "10")])).is_err());
        let spec = balances_spec();
        assert!(spec.requires_approval);
        assert_eq!(spec.amount_column, Some("amount"));
        assert!(!fee_heads_spec().requires_approval);
        assert_eq!(CATEGORIES.len(), FeeCategory::ALL.len());
        for c in CATEGORIES {
            assert!(FeeCategory::parse(c).is_some(), "{c}");
        }
    }
}
