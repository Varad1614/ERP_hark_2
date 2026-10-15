//! Payroll disbursement (Part 1, DEF-E-007): net pay is paid out and the payroll liabilities are
//! cleared.
//!
//! A payroll run whose liability is posted to Finance (`posted` / `closed`; Dr 5410 / Cr 2410
//! net pay / Cr 2420 deductions + employer contributions) gets at most one live **disbursement
//! batch per payout kind** (idempotent create):
//!
//! * `net_pay` — one line per finalised result (net pay > 0) with the payee's salary bank account
//!   (`people.staff_bank_account`, snapshotted on the line). The batch produces a bank transfer
//!   file in the generic NEFT bulk format of [`neft_file`] (documented in
//!   `docs/people/payroll-disbursement.md`). There is no bank API: the file is uploaded by hand
//!   and `bankSubmission` stays `BLOCKED_EXTERNAL`.
//! * `statutory_dues` — one line per deduction / employer-contribution component (PF, PT, …)
//!   with the amount to remit by challan ([`challan_file`]).
//!
//! Lifecycle: `pending_approval → approved → paid` (or `rejected`, after which a new batch may be
//! created). The batch goes through the shared approval helper
//! ([`process::PAYROLL_DISBURSEMENT_APPROVAL`]); the decision endpoint needs
//! `people.payroll.approve`. **Mark paid** (`people.payroll.approve`, bank reference / challan
//! reference required, a net-pay batch's bank file must have been generated) posts the payout
//! through the existing cross-module contract
//! `PayrollPostingPort::post_payroll_payout` — Dr 2410 / Cr bank (net pay) or Dr 2420 / Cr bank
//! (statutory dues), `source_type = payroll_disbursement`, idempotent on the batch — and moves
//! the batch to `paid`; a second mark-paid is a 409. Money is `rust_decimal` end to end.

use super::common::{
    cause, ensure_version, history, illegal, invalid, opt_reason, required_reason, Cmd,
};
use super::rows::load_run;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::fmt2;
use crate::shared::guard::require_institution_writable;
use crate::shared::numbering::{self, kind};
use crate::shared::publish::audit;
use crate::shared::time::business_today;
use crate::shared::PeopleState;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::ports::{PayrollPayoutKind, PayrollPayoutRequest};
use sutra_contracts::people::status::PayrollRunStatus;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// Aggregate name (status history, audit, business keys).
pub const DISBURSEMENT: &str = "payroll_disbursement";
/// Aggregate name of a staff salary bank account (audit).
pub const BANK_ACCOUNT: &str = "staff_bank_account";
/// Bank submission state: there is no bank API, so every batch is BLOCKED_EXTERNAL.
pub const BANK_SUBMISSION_BLOCKED: &str = "BLOCKED_EXTERNAL";

const CREATED: &str = "people.payroll_disbursement.created";
const APPROVED: &str = "people.payroll_disbursement.approved";
const REJECTED: &str = "people.payroll_disbursement.rejected";
const PAID: &str = "people.payroll_disbursement.paid";

/// Status of a disbursement batch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PayrollDisbursementStatus {
    PendingApproval,
    Approved,
    Rejected,
    Paid,
}

impl PayrollDisbursementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PendingApproval => "pending_approval",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Paid => "paid",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pending_approval" => Some(Self::PendingApproval),
            "approved" => Some(Self::Approved),
            "rejected" => Some(Self::Rejected),
            "paid" => Some(Self::Paid),
            _ => None,
        }
    }
}

/// Wire name of a payout kind (`net_pay` / `statutory_dues`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PayrollDisbursementKind {
    NetPay,
    StatutoryDues,
}

impl From<PayrollDisbursementKind> for PayrollPayoutKind {
    fn from(k: PayrollDisbursementKind) -> Self {
        match k {
            PayrollDisbursementKind::NetPay => Self::NetPay,
            PayrollDisbursementKind::StatutoryDues => Self::StatutoryDues,
        }
    }
}

impl From<PayrollPayoutKind> for PayrollDisbursementKind {
    fn from(k: PayrollPayoutKind) -> Self {
        match k {
            PayrollPayoutKind::NetPay => Self::NetPay,
            PayrollPayoutKind::StatutoryDues => Self::StatutoryDues,
        }
    }
}

// ------------------------------------------------------------------ rows

/// A disbursement batch.
#[derive(Clone, Debug)]
pub struct DisbursementRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub run_id: Uuid,
    pub run_number: String,
    pub number: String,
    pub kind: PayrollPayoutKind,
    pub status: PayrollDisbursementStatus,
    pub status_reason: Option<String>,
    pub line_count: i32,
    pub total_amount: Decimal,
    pub file_generated_at: Option<DateTime<Utc>>,
    pub payment_reference: Option<String>,
    pub paid_on: Option<NaiveDate>,
    pub paid_by: Option<String>,
    pub paid_at: Option<DateTime<Utc>>,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub created_by: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

const SELECT: &str = "SELECT d.id, d.institution_id, d.run_id, r.number AS run_number, d.number, \
    d.kind, d.status, d.status_reason, d.line_count, d.total_amount, d.file_generated_at, \
    d.payment_reference, d.paid_on, d.paid_by, d.paid_at, d.journal_entry_id, \
    d.journal_entry_number, d.process_instance_id, d.approved_at, d.created_by, d.created_at, \
    d.version FROM people.payroll_disbursement d JOIN people.payroll_run r ON r.id = d.run_id";

fn from_row(r: &PgRow) -> Result<DisbursementRow, sqlx::Error> {
    let kind: String = r.try_get("kind")?;
    let status: String = r.try_get("status")?;
    Ok(DisbursementRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        run_id: r.try_get("run_id")?,
        run_number: r.try_get("run_number")?,
        number: r.try_get("number")?,
        kind: PayrollPayoutKind::parse(&kind)
            .ok_or_else(|| sqlx::Error::Decode(format!("disbursement kind `{kind}`").into()))?,
        status: PayrollDisbursementStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("disbursement status `{status}`").into()))?,
        status_reason: r.try_get("status_reason")?,
        line_count: r.try_get("line_count")?,
        total_amount: r.try_get("total_amount")?,
        file_generated_at: r.try_get("file_generated_at")?,
        payment_reference: r.try_get("payment_reference")?,
        paid_on: r.try_get("paid_on")?,
        paid_by: r.try_get("paid_by")?,
        paid_at: r.try_get("paid_at")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        journal_entry_number: r.try_get("journal_entry_number")?,
        process_instance_id: r.try_get("process_instance_id")?,
        approved_at: r.try_get("approved_at")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    })
}

/// One payee (net pay) or one due (statutory) of a batch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DisbursementLine {
    pub line_no: i32,
    pub staff_id: Option<Uuid>,
    pub result_id: Option<Uuid>,
    pub staff_number: Option<String>,
    pub display_name: Option<String>,
    pub account_holder_name: Option<String>,
    pub account_number: Option<String>,
    pub ifsc: Option<String>,
    pub component_code: Option<String>,
    pub component_name: Option<String>,
    pub amount: Decimal,
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<DisbursementRow, KernelError> {
    let sql = format!(
        "{SELECT} WHERE d.id = $1 AND d.tenant_id = $2{}",
        if lock { " FOR UPDATE OF d" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "payroll disbursement",
        })?;
    from_row(&row).map_err(map_db_error)
}

/// One batch (404 when missing / foreign).
pub async fn get_disbursement(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<DisbursementRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant, id, false).await
}

async fn find_live(
    conn: &mut PgConnection,
    tenant: TenantId,
    run: Uuid,
    kind: PayrollPayoutKind,
) -> Result<Option<DisbursementRow>, KernelError> {
    let row = sqlx::query(&format!(
        "{SELECT} WHERE d.tenant_id = $1 AND d.run_id = $2 AND d.kind = $3 \
         AND d.status <> 'rejected'"
    ))
    .bind(tenant.as_uuid())
    .bind(run)
    .bind(kind.as_str())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.as_ref().map(from_row).transpose().map_err(map_db_error)
}

/// Batches of a payroll run (newest first).
pub async fn list_for_run(
    st: &PeopleState,
    tenant: TenantId,
    run: Uuid,
) -> Result<Vec<DisbursementRow>, KernelError> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE d.tenant_id = $1 AND d.run_id = $2 ORDER BY d.created_at DESC, d.id DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(run)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Batches of an institution (newest first, at most 200).
pub async fn list_for_institution(
    st: &PeopleState,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Vec<DisbursementRow>, KernelError> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE d.tenant_id = $1 AND d.institution_id = $2 \
         ORDER BY d.created_at DESC, d.id DESC LIMIT 200"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Lines of a batch in `line_no` order.
pub async fn lines_of(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<Vec<DisbursementLine>, KernelError> {
    let rows = sqlx::query(
        "SELECT line_no, staff_id, result_id, staff_number, display_name, account_holder_name, \
         account_number, ifsc, component_code, component_name, amount \
         FROM people.payroll_disbursement_line WHERE tenant_id = $1 AND disbursement_id = $2 \
         ORDER BY line_no",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(line_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

fn line_from_row(r: &PgRow) -> Result<DisbursementLine, sqlx::Error> {
    Ok(DisbursementLine {
        line_no: r.try_get("line_no")?,
        staff_id: r.try_get("staff_id")?,
        result_id: r.try_get("result_id")?,
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        account_holder_name: r.try_get("account_holder_name")?,
        account_number: r.try_get("account_number")?,
        ifsc: r.try_get("ifsc")?,
        component_code: r.try_get("component_code")?,
        component_name: r.try_get("component_name")?,
        amount: r.try_get("amount")?,
    })
}

// ------------------------------------------------------------------ planning

/// The lines a new batch of `kind` pays for `run` (finalised results only — superseded revisions
/// are excluded, so applied adjustments are included exactly once).
async fn plan(
    conn: &mut PgConnection,
    tenant: TenantId,
    run: Uuid,
    kind: PayrollPayoutKind,
) -> Result<Vec<DisbursementLine>, KernelError> {
    let mut lines = Vec::new();
    match kind {
        PayrollPayoutKind::NetPay => {
            let rows = sqlx::query(
                "SELECT r.id AS result_id, r.staff_id, r.staff_number, r.display_name, r.net_pay, \
                 b.account_holder_name, b.account_number, b.ifsc \
                 FROM people.payroll_result r LEFT JOIN people.staff_bank_account b \
                   ON b.tenant_id = r.tenant_id AND b.staff_id = r.staff_id \
                 WHERE r.tenant_id = $1 AND r.run_id = $2 AND r.status = 'finalized' \
                 ORDER BY r.staff_number, r.id",
            )
            .bind(tenant.as_uuid())
            .bind(run)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db_error)?;
            let mut missing = Vec::new();
            for r in &rows {
                let amount: Decimal = r.try_get("net_pay").map_err(map_db_error)?;
                let staff_number: String = r.try_get("staff_number").map_err(map_db_error)?;
                if amount < Decimal::ZERO {
                    return Err(KernelError::Conflict(format!(
                        "net pay of {staff_number} is negative ({}); recover it before disbursing",
                        fmt2(amount)
                    )));
                }
                if amount.is_zero() {
                    continue;
                }
                let account: Option<String> = r.try_get("account_number").map_err(map_db_error)?;
                if account.is_none() {
                    missing.push(staff_number.clone());
                }
                lines.push(DisbursementLine {
                    line_no: i32::try_from(lines.len() + 1).unwrap_or(i32::MAX),
                    staff_id: Some(r.try_get("staff_id").map_err(map_db_error)?),
                    result_id: Some(r.try_get("result_id").map_err(map_db_error)?),
                    staff_number: Some(staff_number),
                    display_name: Some(r.try_get("display_name").map_err(map_db_error)?),
                    account_holder_name: r.try_get("account_holder_name").map_err(map_db_error)?,
                    account_number: account,
                    ifsc: r.try_get("ifsc").map_err(map_db_error)?,
                    component_code: None,
                    component_name: None,
                    amount,
                });
            }
            if !missing.is_empty() {
                return Err(KernelError::Validation(vec![FieldViolation::new(
                    "bankAccount",
                    "bank_account_missing",
                    format!(
                        "no salary bank account on record for {}; add it before creating the \
                         transfer batch",
                        missing.join(", ")
                    ),
                )]));
            }
        }
        PayrollPayoutKind::StatutoryDues => {
            let rows = sqlx::query(
                "SELECT l.code, min(l.name) AS name, sum(l.amount) AS amount \
                 FROM people.payroll_result_line l \
                 JOIN people.payroll_result r ON r.id = l.result_id \
                 WHERE r.tenant_id = $1 AND r.run_id = $2 AND r.status = 'finalized' \
                   AND l.kind IN ('deduction', 'employer_contribution') \
                 GROUP BY l.code ORDER BY l.code",
            )
            .bind(tenant.as_uuid())
            .bind(run)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db_error)?;
            for r in &rows {
                let code: String = r.try_get("code").map_err(map_db_error)?;
                let amount: Decimal = r.try_get("amount").map_err(map_db_error)?;
                if amount < Decimal::ZERO {
                    return Err(KernelError::Conflict(format!(
                        "statutory dues of {code} are negative ({}); they cannot be remitted",
                        fmt2(amount)
                    )));
                }
                if amount.is_zero() {
                    continue;
                }
                lines.push(DisbursementLine {
                    line_no: i32::try_from(lines.len() + 1).unwrap_or(i32::MAX),
                    staff_id: None,
                    result_id: None,
                    staff_number: None,
                    display_name: None,
                    account_holder_name: None,
                    account_number: None,
                    ifsc: None,
                    component_code: Some(code),
                    component_name: Some(r.try_get("name").map_err(map_db_error)?),
                    amount,
                });
            }
        }
    }
    if lines.is_empty() {
        return Err(KernelError::Conflict(match kind {
            PayrollPayoutKind::NetPay => "the run has no net pay to disburse".to_owned(),
            PayrollPayoutKind::StatutoryDues => {
                "the run has no deductions or statutory dues to remit".to_owned()
            }
        }));
    }
    Ok(lines)
}

/// Result of [`create_disbursement`]: the batch and whether this call created it.
pub struct Created {
    pub row: DisbursementRow,
    pub created: bool,
}

/// Creates the batch of `kind` for a posted / closed run (idempotent: the live batch is returned
/// when one exists) and requests its approval.
pub async fn create_disbursement(
    c: &Cmd<'_>,
    run_id: Uuid,
    kind: PayrollPayoutKind,
) -> Result<Created, KernelError> {
    let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
    let run = load_run(&mut conn, c.tenant, run_id, false).await?;
    if let Some(row) = find_live(&mut conn, c.tenant, run_id, kind).await? {
        return Ok(Created {
            row,
            created: false,
        });
    }
    if !matches!(
        run.status,
        PayrollRunStatus::Posted | PayrollRunStatus::Closed
    ) || run.journal_entry_id.is_none()
    {
        return Err(KernelError::Conflict(format!(
            "payroll run {} is {}; only a run whose liability is posted to Finance (posted or \
             closed) can be disbursed",
            run.number, run.status
        )));
    }
    let lines = plan(&mut conn, c.tenant, run_id, kind).await?;
    drop(conn);
    let scope = c.scope(run.institution_id);
    require_institution_writable(c.st, &scope, run.institution_id).await?;
    let total: Decimal = lines.iter().map(|l| l.amount).sum();
    let id = Uuid::now_v7();
    let outcome = request_approval(
        c.st,
        &scope,
        process::PAYROLL_DISBURSEMENT_APPROVAL,
        business_key(DISBURSEMENT, id, 1),
        BTreeMap::from([
            ("payrollRun".to_owned(), json!(run.number)),
            ("kind".to_owned(), json!(kind.as_str())),
            ("totalAmount".to_owned(), json!(fmt2(total))),
            ("lineCount".to_owned(), json!(lines.len())),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let number = numbering::next(
        &mut tx,
        c.tenant,
        run.institution_id,
        kind::PAYROLL_DISBURSEMENT,
    )
    .await?;
    let inserted = sqlx::query(
        "INSERT INTO people.payroll_disbursement (id, tenant_id, institution_id, run_id, number, \
         kind, status, line_count, total_amount, process_instance_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, 'pending_approval', $7, $8, $9, $10) \
         ON CONFLICT (tenant_id, run_id, kind) WHERE status <> 'rejected' DO NOTHING",
    )
    .bind(id)
    .bind(c.tenant.as_uuid())
    .bind(run.institution_id.as_uuid())
    .bind(run_id)
    .bind(&number)
    .bind(kind.as_str())
    .bind(i32::try_from(lines.len()).unwrap_or(i32::MAX))
    .bind(total)
    .bind(&pid)
    .bind(c.actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if inserted.rows_affected() == 0 {
        // A concurrent create won the race: return its batch (the number is given back).
        tx.rollback().await.map_err(map_db_error)?;
        let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
        let row = find_live(&mut conn, c.tenant, run_id, kind)
            .await?
            .ok_or_else(|| KernelError::Conflict("the disbursement batch changed; retry".into()))?;
        return Ok(Created {
            row,
            created: false,
        });
    }
    for l in &lines {
        sqlx::query(
            "INSERT INTO people.payroll_disbursement_line (tenant_id, disbursement_id, line_no, \
             staff_id, result_id, staff_number, display_name, account_holder_name, \
             account_number, ifsc, component_code, component_name, amount) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(c.tenant.as_uuid())
        .bind(id)
        .bind(l.line_no)
        .bind(l.staff_id)
        .bind(l.result_id)
        .bind(&l.staff_number)
        .bind(&l.display_name)
        .bind(&l.account_holder_name)
        .bind(&l.account_number)
        .bind(&l.ifsc)
        .bind(&l.component_code)
        .bind(&l.component_name)
        .bind(l.amount)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        c.tenant,
        run.institution_id,
        DISBURSEMENT,
        id,
        None,
        PayrollDisbursementStatus::PendingApproval.as_str(),
        &cause(CREATED, None, Some(&pid)),
        c.actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let auto = c.auto();
        sqlx::query(
            "UPDATE people.payroll_disbursement SET status = 'approved', approved_at = now(), \
             status_reason = 'auto-approved', version = version + 1, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(id)
        .bind(c.tenant.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            c.tenant,
            run.institution_id,
            DISBURSEMENT,
            id,
            Some(PayrollDisbursementStatus::PendingApproval.as_str()),
            PayrollDisbursementStatus::Approved.as_str(),
            &cause(APPROVED, Some("auto-approved"), Some(&pid)),
            &auto,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.payroll_disbursement.create",
        DISBURSEMENT,
        id.to_string(),
        json!({"payrollRunId": run_id, "number": number, "kind": kind.as_str(),
               "totalAmount": fmt2(total), "lineCount": lines.len(),
               "processKey": process::PAYROLL_DISBURSEMENT_APPROVAL, "processInstanceId": pid}),
        c.actor,
    )
    .await?;
    let row = load(&mut tx, c.tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Created { row, created: true })
}

/// Decides a pending batch (`people.payroll.approve`): approve → `approved`; reject (reason
/// required) → `rejected` (a new batch may then be created for the run).
pub async fn decide_disbursement(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<&str>,
) -> Result<DisbursementRow, KernelError> {
    let reason = if approve {
        opt_reason(reason)
    } else {
        Some(required_reason(reason)?)
    };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let d = load(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, d.version)?;
    if d.status != PayrollDisbursementStatus::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "payroll disbursement {} is {}; only a pending batch can be decided",
            d.number,
            d.status.as_str()
        )));
    }
    let to = if approve {
        PayrollDisbursementStatus::Approved
    } else {
        PayrollDisbursementStatus::Rejected
    };
    sqlx::query(
        "UPDATE people.payroll_disbursement SET status = $3, status_reason = $4, \
         approved_at = CASE WHEN $3 = 'approved' THEN now() ELSE approved_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(c.tenant.as_uuid())
    .bind(to.as_str())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        c.tenant,
        d.institution_id,
        DISBURSEMENT,
        id,
        Some(d.status.as_str()),
        to.as_str(),
        &cause(
            if approve { APPROVED } else { REJECTED },
            reason.as_deref(),
            d.process_instance_id.as_deref(),
        ),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(d.institution_id),
        "people.payroll_disbursement.decide",
        DISBURSEMENT,
        id.to_string(),
        json!({"decision": if approve { "approve" } else { "reject" }, "reason": reason}),
        c.actor,
    )
    .await?;
    let out = load(&mut tx, c.tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ files

/// Characters a bank accepts in NEFT names / narrations: ASCII letters, digits, space and
/// `. & - /`; anything else becomes a space, leading non-alphanumerics are dropped (no CSV
/// formula injection) and the result is cut to `max` characters.
pub fn bank_text(raw: &str, max: usize) -> String {
    let mapped: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, ' ' | '.' | '&' | '-' | '/') {
                ch
            } else {
                ' '
            }
        })
        .collect();
    let collapsed = mapped.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_start_matches(|ch: char| !ch.is_ascii_alphanumeric());
    trimmed
        .chars()
        .take(max)
        .collect::<String>()
        .trim_end()
        .to_owned()
}

/// A component code for the remittance advice: ASCII letters, digits and `_` only (`PF_ER`).
pub fn code_text(raw: &str, max: usize) -> String {
    raw.chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .take(max)
        .collect()
}

/// `DD-MM-YYYY` (the date format of Indian bank bulk-upload files).
fn bank_date(d: NaiveDate) -> String {
    d.format("%d-%m-%Y").to_string()
}

/// The header fields every file carries.
pub struct FileHeader<'a> {
    pub batch_number: &'a str,
    pub run_number: &'a str,
    pub value_date: NaiveDate,
    /// Payroll month (`YYYY-MM`) for the narration.
    pub period: &'a str,
}

/// Generic NEFT bulk transfer file (CSV, CRLF line endings, no quoting needed — every field is
/// restricted to bank-safe characters). Records:
///
/// ```text
/// H,<batch number>,<payroll run number>,<value date DD-MM-YYYY>,<record count>,<total amount>,INR
/// D,NEFT,<beneficiary account no>,<beneficiary IFSC>,<beneficiary name ≤ 50>,<amount>,<value date>,<customer reference>,<narration ≤ 30>
/// T,<record count>,<total amount>
/// ```
///
/// Amounts have exactly two decimals; the customer reference is `<batch number>-<line no>`
/// (unique per transfer, used for the bank's return / reconciliation file); the narration is
/// `SALARY <YYYY-MM> <staff number>`.
pub fn neft_file(h: &FileHeader<'_>, lines: &[DisbursementLine]) -> String {
    let date = bank_date(h.value_date);
    let total: Decimal = lines.iter().map(|l| l.amount).sum();
    let mut out = format!(
        "H,{},{},{},{},{},INR\r\n",
        bank_text(h.batch_number, 20),
        bank_text(h.run_number, 20),
        date,
        lines.len(),
        fmt2(total)
    );
    for l in lines {
        let name = l
            .account_holder_name
            .as_deref()
            .or(l.display_name.as_deref())
            .unwrap_or_default();
        out.push_str(&format!(
            "D,NEFT,{},{},{},{},{},{}-{},{}\r\n",
            bank_text(l.account_number.as_deref().unwrap_or_default(), 18),
            bank_text(l.ifsc.as_deref().unwrap_or_default(), 11),
            bank_text(name, 50),
            fmt2(l.amount),
            date,
            bank_text(h.batch_number, 20),
            l.line_no,
            bank_text(
                &format!(
                    "SALARY {} {}",
                    h.period,
                    l.staff_number.as_deref().unwrap_or_default()
                ),
                30
            ),
        ));
    }
    out.push_str(&format!("T,{},{}\r\n", lines.len(), fmt2(total)));
    out
}

/// Statutory remittance advice (CSV, same envelope as [`neft_file`]): one `D` record per
/// component — `D,<component code>,<component name ≤ 50>,<amount>` — for preparing the challans
/// (EPF ECR, professional tax, TDS …).
pub fn challan_file(h: &FileHeader<'_>, lines: &[DisbursementLine]) -> String {
    let total: Decimal = lines.iter().map(|l| l.amount).sum();
    let mut out = format!(
        "H,{},{},{},{},{},INR\r\n",
        bank_text(h.batch_number, 20),
        bank_text(h.run_number, 20),
        bank_date(h.value_date),
        lines.len(),
        fmt2(total)
    );
    for l in lines {
        out.push_str(&format!(
            "D,{},{},{}\r\n",
            code_text(l.component_code.as_deref().unwrap_or_default(), 20),
            bank_text(l.component_name.as_deref().unwrap_or_default(), 50),
            fmt2(l.amount)
        ));
    }
    out.push_str(&format!("T,{},{}\r\n", lines.len(), fmt2(total)));
    out
}

/// A generated file.
pub struct GeneratedFile {
    pub file_name: String,
    pub content: String,
}

/// Generates the batch's file (approved or paid batches only) and stamps
/// `file_generated_at` the first time. Bank submission stays BLOCKED_EXTERNAL.
pub async fn generate_file(c: &Cmd<'_>, id: Uuid) -> Result<GeneratedFile, KernelError> {
    let d = get_disbursement(c.st, c.tenant, id).await?;
    if !matches!(
        d.status,
        PayrollDisbursementStatus::Approved | PayrollDisbursementStatus::Paid
    ) {
        return Err(KernelError::Conflict(format!(
            "payroll disbursement {} is {}; the file is available once the batch is approved",
            d.number,
            d.status.as_str()
        )));
    }
    let scope = c.scope(d.institution_id);
    let today = business_today(c.st, &scope, d.institution_id).await?;
    let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
    let run = load_run(&mut conn, c.tenant, d.run_id, false).await?;
    drop(conn);
    let lines = lines_of(c.st, c.tenant, id).await?;
    let period = run.period_start.format("%Y-%m").to_string();
    let header = FileHeader {
        batch_number: &d.number,
        run_number: &d.run_number,
        value_date: d.paid_on.unwrap_or(today),
        period: &period,
    };
    let (content, suffix) = match d.kind {
        PayrollPayoutKind::NetPay => (neft_file(&header, &lines), "neft"),
        PayrollPayoutKind::StatutoryDues => (challan_file(&header, &lines), "statutory"),
    };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "UPDATE people.payroll_disbursement SET file_generated_at = now(), updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND file_generated_at IS NULL",
    )
    .bind(id)
    .bind(c.tenant.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.payroll_disbursement.file",
        DISBURSEMENT,
        id.to_string(),
        json!({"format": suffix, "records": lines.len(), "totalAmount": fmt2(d.total_amount),
               "bankSubmission": BANK_SUBMISSION_BLOCKED}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(GeneratedFile {
        file_name: format!("{}-{suffix}.csv", d.number),
        content,
    })
}

// ------------------------------------------------------------------ settlement

/// Validates a bank transaction (UTR) / challan reference: 1 … 64 of `A-Z a-z 0-9 - / _ .`.
pub fn payment_reference(raw: &str) -> Result<String, KernelError> {
    let r = raw.trim();
    if r.is_empty() || r.chars().count() > 64 {
        return Err(invalid(
            "reference",
            "required",
            "a bank or challan reference of 1 to 64 characters is required",
        ));
    }
    if !r
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '/' | '_' | '.'))
    {
        return Err(invalid(
            "reference",
            "invalid",
            "letters, digits and - / _ . only",
        ));
    }
    Ok(r.to_owned())
}

/// Marks an approved batch paid (`people.payroll.approve`): posts the payout in Finance through
/// `PayrollPostingPort::post_payroll_payout` (Dr 2410 / Cr bank for net pay, Dr 2420 / Cr bank for
/// statutory dues; idempotent on the batch) and moves the batch to `paid`. A net-pay batch needs
/// its bank file generated first; a paid batch is never paid again (409).
pub async fn mark_paid(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    reference: &str,
    paid_on: Option<NaiveDate>,
) -> Result<DisbursementRow, KernelError> {
    let reference = payment_reference(reference)?;
    let d = get_disbursement(c.st, c.tenant, id).await?;
    ensure_version(expected, d.version)?;
    if d.status == PayrollDisbursementStatus::Paid {
        return Err(KernelError::Conflict(format!(
            "payroll disbursement {} is already paid (reference {})",
            d.number,
            d.payment_reference.as_deref().unwrap_or("—")
        )));
    }
    if d.status != PayrollDisbursementStatus::Approved {
        return Err(illegal(
            "payroll disbursement",
            &d.number,
            d.status.as_str(),
            "paid",
        ));
    }
    if d.kind == PayrollPayoutKind::NetPay && d.file_generated_at.is_none() {
        return Err(KernelError::Conflict(format!(
            "generate the bank transfer file of {} before marking it paid",
            d.number
        )));
    }
    let scope = c.scope(d.institution_id);
    require_institution_writable(c.st, &scope, d.institution_id).await?;
    let today = business_today(c.st, &scope, d.institution_id).await?;
    let paid_on = paid_on.unwrap_or(today);
    if paid_on > today {
        return Err(invalid(
            "paidOn",
            "future_date",
            "the payment date cannot be in the future",
        ));
    }
    let label = match d.kind {
        PayrollPayoutKind::NetPay => "Salary disbursement",
        PayrollPayoutKind::StatutoryDues => "Statutory dues remittance",
    };
    let receipt =
        c.st.postings
            .post_payroll_payout(
                &scope,
                PayrollPayoutRequest {
                    institution_id: d.institution_id,
                    kind: d.kind,
                    disbursement_id: d.id,
                    payroll_run_id: d.run_id,
                    posting_key: "paid".into(),
                    posting_date: paid_on,
                    description: format!("{label} {} (payroll {})", d.number, d.run_number),
                    amount: d.total_amount,
                    reference: reference.clone(),
                    actor: c.actor.id.clone(),
                },
            )
            .await?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    if locked.status != PayrollDisbursementStatus::Approved {
        return Err(KernelError::Conflict(format!(
            "payroll disbursement {} is {}; it cannot be paid again",
            locked.number,
            locked.status.as_str()
        )));
    }
    sqlx::query(
        "UPDATE people.payroll_disbursement SET status = 'paid', payment_reference = $3, \
         paid_on = $4, paid_by = $5, paid_at = now(), journal_entry_id = $6, \
         journal_entry_number = $7, status_reason = NULL, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(c.tenant.as_uuid())
    .bind(&reference)
    .bind(paid_on)
    .bind(c.actor.id.as_deref())
    .bind(receipt.journal_entry_id.as_uuid())
    .bind(&receipt.entry_number)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        c.tenant,
        d.institution_id,
        DISBURSEMENT,
        id,
        Some(PayrollDisbursementStatus::Approved.as_str()),
        PayrollDisbursementStatus::Paid.as_str(),
        &cause(
            PAID,
            Some(&reference),
            locked.process_instance_id.as_deref(),
        ),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.payroll_disbursement.mark_paid",
        DISBURSEMENT,
        id.to_string(),
        json!({"reference": reference, "paidOn": paid_on, "amount": fmt2(d.total_amount),
               "journalEntryId": receipt.journal_entry_id.as_uuid(),
               "entryNumber": receipt.entry_number, "alreadyPosted": receipt.already_posted}),
        c.actor,
    )
    .await?;
    let out = load(&mut tx, c.tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ staff bank accounts

/// A staff member's salary bank account.
#[derive(Clone, Debug)]
pub struct BankAccountRow {
    pub staff_id: Uuid,
    pub institution_id: InstitutionId,
    pub account_holder_name: String,
    pub account_number: String,
    pub ifsc: String,
    pub bank_name: Option<String>,
    pub updated_at: DateTime<Utc>,
    pub version: i64,
}

/// Input of [`put_bank_account`].
pub struct BankAccountInput {
    pub account_holder_name: String,
    pub account_number: String,
    pub ifsc: String,
    pub bank_name: Option<String>,
}

/// Validated `(holder, account number, IFSC, bank name)`.
pub fn validate_bank_account(
    i: &BankAccountInput,
) -> Result<(String, String, String, Option<String>), KernelError> {
    let mut v = Vec::new();
    let holder = i.account_holder_name.trim().to_owned();
    if holder.is_empty() || holder.chars().count() > 140 {
        v.push(FieldViolation::new(
            "accountHolderName",
            "required",
            "the account holder name is required (at most 140 characters)",
        ));
    }
    let number: String = i
        .account_number
        .chars()
        .filter(|ch| !ch.is_whitespace())
        .collect();
    if !(6..=18).contains(&number.len()) || !number.chars().all(|ch| ch.is_ascii_digit()) {
        v.push(FieldViolation::new(
            "accountNumber",
            "invalid",
            "6 to 18 digits",
        ));
    }
    let ifsc = i.ifsc.trim().to_ascii_uppercase();
    let b = ifsc.as_bytes();
    let ifsc_ok = b.len() == 11
        && b[..4].iter().all(u8::is_ascii_uppercase)
        && b[4] == b'0'
        && b[5..].iter().all(u8::is_ascii_alphanumeric);
    if !ifsc_ok {
        v.push(FieldViolation::new(
            "ifsc",
            "invalid",
            "an 11-character IFSC such as HDFC0001234",
        ));
    }
    let bank = i
        .bank_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.chars().take(140).collect::<String>());
    if v.is_empty() {
        Ok((holder, number, ifsc, bank))
    } else {
        Err(KernelError::Validation(v))
    }
}

/// `XXXXXX1234`: the account number with all but the last four digits masked.
pub fn mask_account(n: &str) -> String {
    let len = n.chars().count();
    n.chars()
        .enumerate()
        .map(|(i, ch)| if i + 4 < len { 'X' } else { ch })
        .collect()
}

/// The institution of a staff member (404 when missing / foreign).
pub async fn staff_institution(
    st: &PeopleState,
    tenant: TenantId,
    staff: Uuid,
) -> Result<InstitutionId, KernelError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT institution_id FROM people.staff_member WHERE id = $1 AND tenant_id = $2",
    )
    .bind(staff)
    .bind(tenant.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    id.map(InstitutionId).ok_or(KernelError::NotFound {
        entity: "staff member",
    })
}

/// A staff member's bank account, if any.
pub async fn get_bank_account(
    st: &PeopleState,
    tenant: TenantId,
    staff: Uuid,
) -> Result<Option<BankAccountRow>, KernelError> {
    let row = sqlx::query(
        "SELECT staff_id, institution_id, account_holder_name, account_number, ifsc, bank_name, \
         updated_at, version FROM people.staff_bank_account WHERE tenant_id = $1 AND staff_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(staff)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    row.as_ref()
        .map(bank_from_row)
        .transpose()
        .map_err(map_db_error)
}

fn bank_from_row(r: &PgRow) -> Result<BankAccountRow, sqlx::Error> {
    Ok(BankAccountRow {
        staff_id: r.try_get("staff_id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        account_holder_name: r.try_get("account_holder_name")?,
        account_number: r.try_get("account_number")?,
        ifsc: r.try_get("ifsc")?,
        bank_name: r.try_get("bank_name")?,
        updated_at: r.try_get("updated_at")?,
        version: r.try_get("version")?,
    })
}

/// Records (or replaces) a staff member's salary bank account. Batches already created keep the
/// details they snapshotted.
pub async fn put_bank_account(
    c: &Cmd<'_>,
    staff: Uuid,
    input: &BankAccountInput,
) -> Result<BankAccountRow, KernelError> {
    let (holder, number, ifsc, bank) = validate_bank_account(input)?;
    let institution = staff_institution(c.st, c.tenant, staff).await?;
    let scope = c.scope(institution);
    require_institution_writable(c.st, &scope, institution).await?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.staff_bank_account (tenant_id, institution_id, staff_id, \
         account_holder_name, account_number, ifsc, bank_name) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (tenant_id, staff_id) DO UPDATE SET \
         account_holder_name = EXCLUDED.account_holder_name, \
         account_number = EXCLUDED.account_number, ifsc = EXCLUDED.ifsc, \
         bank_name = EXCLUDED.bank_name, version = people.staff_bank_account.version + 1, \
         updated_at = now()",
    )
    .bind(c.tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(staff)
    .bind(&holder)
    .bind(&number)
    .bind(&ifsc)
    .bind(bank.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "people.staff_bank_account.put",
        BANK_ACCOUNT,
        staff.to_string(),
        json!({"accountNumber": mask_account(&number), "ifsc": ifsc}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_bank_account(c.st, c.tenant, staff)
        .await?
        .ok_or_else(|| KernelError::internal_msg("bank account vanished after upsert"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn payee(no: i32, name: &str, amount: &str) -> DisbursementLine {
        DisbursementLine {
            line_no: no,
            staff_id: Some(Uuid::nil()),
            result_id: Some(Uuid::nil()),
            staff_number: Some(format!("EMP-00000{no}")),
            display_name: Some(name.into()),
            account_holder_name: Some(name.into()),
            account_number: Some("001234567890".into()),
            ifsc: Some("HDFC0001234".into()),
            component_code: None,
            component_name: None,
            amount: d(amount),
        }
    }

    fn header() -> FileHeader<'static> {
        FileHeader {
            batch_number: "PD-000001",
            run_number: "PR-000001",
            value_date: NaiveDate::from_ymd_opt(2026, 11, 30).unwrap_or_default(),
            period: "2026-11",
        }
    }

    #[test]
    fn neft_file_has_header_details_and_a_balancing_trailer() {
        let f = neft_file(
            &header(),
            &[
                payee(1, "Meera Shah", "43800.00"),
                payee(2, "=cmd|' /C calc'!A0, Kiran", "100.5"),
            ],
        );
        let lines: Vec<&str> = f.split("\r\n").filter(|l| !l.is_empty()).collect();
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[0], "H,PD-000001,PR-000001,30-11-2026,2,43900.50,INR");
        assert_eq!(
            lines[1],
            "D,NEFT,001234567890,HDFC0001234,Meera Shah,43800.00,30-11-2026,PD-000001-1,SALARY 2026-11 EMP-000001"
        );
        // Formula / delimiter characters never reach the file.
        assert!(
            lines[2].starts_with("D,NEFT,001234567890,HDFC0001234,cmd /C calc A0 Kiran,100.50,")
        );
        assert_eq!(lines[2].split(',').count(), 9);
        assert_eq!(lines[3], "T,2,43900.50");
    }

    #[test]
    fn challan_file_lists_each_component() {
        let mut pf = payee(1, "x", "3600.00");
        pf.component_code = Some("PF".into());
        pf.component_name = Some("Provident fund".into());
        let mut er = pf.clone();
        er.line_no = 2;
        er.component_code = Some("PF_ER".into());
        er.component_name = Some("Employer PF".into());
        let f = challan_file(&header(), &[pf, er]);
        assert_eq!(
            f,
            "H,PD-000001,PR-000001,30-11-2026,2,7200.00,INR\r\nD,PF,Provident fund,3600.00\r\n\
             D,PF_ER,Employer PF,3600.00\r\nT,2,7200.00\r\n"
        );
        assert_eq!(code_text("=PF_ER,1", 20), "PF_ER1");
    }

    #[test]
    fn references_bank_accounts_and_masks() {
        assert!(payment_reference(" UTR-2026/11_01 ").is_ok());
        assert!(payment_reference("").is_err());
        assert!(payment_reference("a,b").is_err());
        assert!(payment_reference(&"9".repeat(65)).is_err());
        let ok = BankAccountInput {
            account_holder_name: "Meera Shah".into(),
            account_number: "0012 3456 7890".into(),
            ifsc: "hdfc0001234".into(),
            bank_name: None,
        };
        let (_, n, ifsc, _) = validate_bank_account(&ok).unwrap_or_default();
        assert_eq!((n.as_str(), ifsc.as_str()), ("001234567890", "HDFC0001234"));
        let bad = BankAccountInput {
            account_holder_name: " ".into(),
            account_number: "12ab".into(),
            ifsc: "HDFC1001234".into(),
            bank_name: None,
        };
        match validate_bank_account(&bad) {
            Err(KernelError::Validation(v)) => assert_eq!(v.len(), 3),
            _ => panic!("expected three violations"),
        }
        assert_eq!(mask_account("001234567890"), "XXXXXXXX7890");
        assert_eq!(bank_text("  --Ravi  (Jr.) ", 50), "Ravi Jr.");
        for s in ["pending_approval", "approved", "rejected", "paid"] {
            assert_eq!(
                PayrollDisbursementStatus::parse(s).map(PayrollDisbursementStatus::as_str),
                Some(s)
            );
        }
    }
}
