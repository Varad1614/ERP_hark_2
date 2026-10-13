//! Row types and decoders of the 7C tables, plus the loaders the commands share.

use super::domain::{
    ComponentDef, FinalSettlementStatus, PayrollPeriodStatus, Rounding, RuleDef,
    SalaryAssignmentStatus, Slab, StatutoryKind, StructureLine,
};
use crate::shared::rows::decode;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::people::status::{
    CalculationBasis, PayComponentKind, PayrollAdjustmentStatus, PayrollConfigStatus,
    PayrollResultStatus, PayrollRunKind, PayrollRunStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

fn dec_err(e: serde_json::Error) -> sqlx::Error {
    sqlx::Error::Decode(e.to_string().into())
}

fn lock_clause(lock: bool) -> &'static str {
    if lock {
        " FOR UPDATE"
    } else {
        ""
    }
}

// ------------------------------------------------------------------ configuration

#[derive(Clone, Debug)]
pub struct ConfigRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub version_no: i32,
    pub name: String,
    pub effective_from: NaiveDate,
    pub rounding: Rounding,
    pub encashment_divisor: i32,
    pub copied_from_id: Option<Uuid>,
    pub status: PayrollConfigStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub approved_at: Option<DateTime<Utc>>,
    pub activated_at: Option<DateTime<Utc>>,
    pub superseded_at: Option<DateTime<Utc>>,
    pub version: i64,
}

pub const CONFIG_COLUMNS: &str = "id, institution_id, version_no, name, effective_from, rounding, \
    encashment_divisor, copied_from_id, status, status_reason, process_instance_id, approved_at, \
    activated_at, superseded_at, version";

pub fn config_from_row(r: &PgRow) -> Result<ConfigRow, sqlx::Error> {
    Ok(ConfigRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        version_no: r.try_get("version_no")?,
        name: r.try_get("name")?,
        effective_from: r.try_get("effective_from")?,
        rounding: decode(r.try_get("rounding")?, Rounding::parse)?,
        encashment_divisor: r.try_get("encashment_divisor")?,
        copied_from_id: r.try_get("copied_from_id")?,
        status: decode(r.try_get("status")?, PayrollConfigStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        approved_at: r.try_get("approved_at")?,
        activated_at: r.try_get("activated_at")?,
        superseded_at: r.try_get("superseded_at")?,
        version: r.try_get("version")?,
    })
}

#[derive(Clone, Debug)]
pub struct ComponentRow {
    pub id: Uuid,
    pub config_id: Uuid,
    pub def: ComponentDef,
}

pub const COMPONENT_COLUMNS: &str =
    "id, config_id, code, name, kind, basis, amount, rate, sequence";

pub fn component_from_row(r: &PgRow) -> Result<ComponentRow, sqlx::Error> {
    Ok(ComponentRow {
        id: r.try_get("id")?,
        config_id: r.try_get("config_id")?,
        def: ComponentDef {
            code: r.try_get("code")?,
            name: r.try_get("name")?,
            kind: decode(r.try_get("kind")?, PayComponentKind::parse)?,
            basis: decode(r.try_get("basis")?, CalculationBasis::parse)?,
            amount: r.try_get("amount")?,
            rate: r.try_get("rate")?,
            sequence: r.try_get("sequence")?,
        },
    })
}

#[derive(Clone, Debug)]
pub struct RuleRow {
    pub id: Uuid,
    pub config_id: Uuid,
    pub def: RuleDef,
}

pub const RULE_COLUMNS: &str = "id, config_id, kind, employee_rate, employer_rate, wage_ceiling, \
    threshold, slabs, fixed_amount, employee_component_code, employer_component_code";

pub fn rule_from_row(r: &PgRow) -> Result<RuleRow, sqlx::Error> {
    let slabs: serde_json::Value = r.try_get("slabs")?;
    let slabs: Vec<Slab> = serde_json::from_value(slabs).map_err(dec_err)?;
    Ok(RuleRow {
        id: r.try_get("id")?,
        config_id: r.try_get("config_id")?,
        def: RuleDef {
            kind: decode(r.try_get("kind")?, StatutoryKind::parse)?,
            employee_rate: r.try_get("employee_rate")?,
            employer_rate: r.try_get("employer_rate")?,
            wage_ceiling: r.try_get("wage_ceiling")?,
            threshold: r.try_get("threshold")?,
            slabs,
            fixed_amount: r.try_get("fixed_amount")?,
            employee_code: r.try_get("employee_component_code")?,
            employer_code: r.try_get("employer_component_code")?,
        },
    })
}

pub async fn load_config(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ConfigRow, KernelError> {
    let sql = format!(
        "SELECT {CONFIG_COLUMNS} FROM people.payroll_config WHERE id = $1 AND tenant_id = $2{}",
        lock_clause(lock)
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "payroll configuration",
        })?;
    config_from_row(&row).map_err(map_db_error)
}

/// The institution's active configuration, if any.
pub async fn active_config(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Option<ConfigRow>, KernelError> {
    let sql = format!(
        "SELECT {CONFIG_COLUMNS} FROM people.payroll_config WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'active'"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(config_from_row)
        .transpose()
        .map_err(map_db_error)
}

pub async fn components_of(
    conn: &mut PgConnection,
    config: Uuid,
) -> Result<Vec<ComponentRow>, KernelError> {
    let sql = format!(
        "SELECT {COMPONENT_COLUMNS} FROM people.pay_component WHERE config_id = $1 \
         ORDER BY sequence, code"
    );
    sqlx::query(&sql)
        .bind(config)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(component_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

pub async fn rules_of(conn: &mut PgConnection, config: Uuid) -> Result<Vec<RuleRow>, KernelError> {
    let sql = format!(
        "SELECT {RULE_COLUMNS} FROM people.statutory_rule WHERE config_id = $1 ORDER BY kind"
    );
    sqlx::query(&sql)
        .bind(config)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(rule_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

// ------------------------------------------------------------------ structures / assignments

#[derive(Clone, Debug)]
pub struct StructureRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub grade: Option<String>,
    pub status: String,
    pub lines: Vec<StructureLine>,
    pub version: i64,
}

pub const STRUCTURE_COLUMNS: &str = "id, institution_id, code, name, grade, status, version";

pub fn structure_from_row(r: &PgRow) -> Result<StructureRow, sqlx::Error> {
    Ok(StructureRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        grade: r.try_get("grade")?,
        status: r.try_get("status")?,
        lines: Vec::new(),
        version: r.try_get("version")?,
    })
}

fn structure_line_from_row(r: &PgRow) -> Result<StructureLine, sqlx::Error> {
    Ok(StructureLine {
        code: r.try_get("component_code")?,
        amount: r.try_get("amount")?,
        rate: r.try_get("rate")?,
    })
}

pub async fn structure_lines(
    conn: &mut PgConnection,
    structure: Uuid,
) -> Result<Vec<StructureLine>, KernelError> {
    sqlx::query(
        "SELECT component_code, amount, rate FROM people.salary_structure_line \
         WHERE structure_id = $1 ORDER BY component_code",
    )
    .bind(structure)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(structure_line_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

pub async fn load_structure(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<StructureRow, KernelError> {
    let sql = format!(
        "SELECT {STRUCTURE_COLUMNS} FROM people.salary_structure WHERE id = $1 AND tenant_id = $2{}",
        lock_clause(lock)
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "salary structure",
        })?;
    let mut s = structure_from_row(&row).map_err(map_db_error)?;
    s.lines = structure_lines(conn, s.id).await?;
    Ok(s)
}

#[derive(Clone, Debug)]
pub struct AssignmentRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub structure_id: Uuid,
    pub structure_code: String,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub status: SalaryAssignmentStatus,
    pub reason: Option<String>,
    pub version: i64,
}

pub const ASSIGNMENT_SELECT: &str = "SELECT a.id, a.institution_id, a.staff_id, a.engagement_id, \
    a.structure_id, s.code AS structure_code, a.effective_from, a.effective_to, a.status, a.reason, \
    a.version FROM people.salary_assignment a JOIN people.salary_structure s ON s.id = a.structure_id";

pub fn assignment_from_row(r: &PgRow) -> Result<AssignmentRow, sqlx::Error> {
    Ok(AssignmentRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        staff_id: r.try_get("staff_id")?,
        engagement_id: r.try_get("engagement_id")?,
        structure_id: r.try_get("structure_id")?,
        structure_code: r.try_get("structure_code")?,
        effective_from: r.try_get("effective_from")?,
        effective_to: r.try_get("effective_to")?,
        status: decode(r.try_get("status")?, SalaryAssignmentStatus::parse)?,
        reason: r.try_get("reason")?,
        version: r.try_get("version")?,
    })
}

/// The salary assignment that applies to an engagement in `[from, to]`: the latest one
/// overlapping the window.
pub async fn assignment_for(
    conn: &mut PgConnection,
    tenant: TenantId,
    engagement: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Option<AssignmentRow>, KernelError> {
    let sql = format!(
        "{ASSIGNMENT_SELECT} WHERE a.tenant_id = $1 AND a.engagement_id = $2 \
         AND a.effective_from <= $4 AND (a.effective_to IS NULL OR a.effective_to >= $3) \
         ORDER BY a.effective_from DESC, a.created_at DESC LIMIT 1"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(engagement)
        .bind(from)
        .bind(to)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(assignment_from_row)
        .transpose()
        .map_err(map_db_error)
}

// ------------------------------------------------------------------ periods / runs

#[derive(Clone, Debug)]
pub struct PeriodRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub year: i32,
    pub month: i32,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub status: PayrollPeriodStatus,
    pub version: i64,
}

pub const PERIOD_COLUMNS: &str =
    "id, institution_id, year, month, period_start, period_end, status, version";

pub fn period_from_row(r: &PgRow) -> Result<PeriodRow, sqlx::Error> {
    Ok(PeriodRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        year: r.try_get("year")?,
        month: r.try_get("month")?,
        period_start: r.try_get("period_start")?,
        period_end: r.try_get("period_end")?,
        status: decode(r.try_get("status")?, PayrollPeriodStatus::parse)?,
        version: r.try_get("version")?,
    })
}

pub async fn load_period(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<PeriodRow, KernelError> {
    let sql = format!(
        "SELECT {PERIOD_COLUMNS} FROM people.payroll_period WHERE id = $1 AND tenant_id = $2"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "payroll period",
        })?;
    period_from_row(&row).map_err(map_db_error)
}

#[derive(Clone, Debug)]
pub struct RunRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub period_id: Uuid,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub number: String,
    pub kind: PayrollRunKind,
    pub status: PayrollRunStatus,
    pub status_reason: Option<String>,
    pub description: Option<String>,
    pub config_id: Option<Uuid>,
    pub items: serde_json::Value,
    pub exceptions: serde_json::Value,
    pub headcount: i32,
    pub gross_earnings: Decimal,
    pub employee_deductions: Decimal,
    pub employer_contributions: Decimal,
    pub net_pay: Decimal,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub calculated_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub finalized_at: Option<DateTime<Utc>>,
    pub posted_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

pub const RUN_SELECT: &str = "SELECT r.id, r.institution_id, r.period_id, p.period_start, \
    p.period_end, r.number, r.kind, r.status, r.status_reason, r.description, r.config_id, r.items, \
    r.exceptions, r.headcount, r.gross_earnings, r.employee_deductions, r.employer_contributions, \
    r.net_pay, r.journal_entry_id, r.journal_entry_number, r.process_instance_id, r.calculated_at, \
    r.approved_at, r.finalized_at, r.posted_at, r.closed_at, r.version \
    FROM people.payroll_run r JOIN people.payroll_period p ON p.id = r.period_id";

pub fn run_from_row(r: &PgRow) -> Result<RunRow, sqlx::Error> {
    Ok(RunRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        period_id: r.try_get("period_id")?,
        period_start: r.try_get("period_start")?,
        period_end: r.try_get("period_end")?,
        number: r.try_get("number")?,
        kind: decode(r.try_get("kind")?, PayrollRunKind::parse)?,
        status: decode(r.try_get("status")?, PayrollRunStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        description: r.try_get("description")?,
        config_id: r.try_get("config_id")?,
        items: r.try_get("items")?,
        exceptions: r.try_get("exceptions")?,
        headcount: r.try_get("headcount")?,
        gross_earnings: r.try_get("gross_earnings")?,
        employee_deductions: r.try_get("employee_deductions")?,
        employer_contributions: r.try_get("employer_contributions")?,
        net_pay: r.try_get("net_pay")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        journal_entry_number: r.try_get("journal_entry_number")?,
        process_instance_id: r.try_get("process_instance_id")?,
        calculated_at: r.try_get("calculated_at")?,
        approved_at: r.try_get("approved_at")?,
        finalized_at: r.try_get("finalized_at")?,
        posted_at: r.try_get("posted_at")?,
        closed_at: r.try_get("closed_at")?,
        version: r.try_get("version")?,
    })
}

pub async fn load_run(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<RunRow, KernelError> {
    let sql = format!(
        "{RUN_SELECT} WHERE r.id = $1 AND r.tenant_id = $2{}",
        if lock { " FOR UPDATE OF r" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "payroll run",
        })?;
    run_from_row(&row).map_err(map_db_error)
}

// ------------------------------------------------------------------ results

#[derive(Clone, Debug)]
pub struct LineRow {
    pub line_no: i32,
    pub code: String,
    pub name: String,
    pub kind: PayComponentKind,
    pub amount: Decimal,
    pub basis: Option<String>,
    pub source: String,
    pub source_ref: Option<Uuid>,
}

#[derive(Clone, Debug)]
pub struct ResultRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub run_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub status: PayrollResultStatus,
    pub revision: i32,
    pub supersedes_result_id: Option<Uuid>,
    pub origin_result_id: Uuid,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    pub period_days: Decimal,
    pub employed_days: Decimal,
    pub lop_days: Decimal,
    pub payable_days: Decimal,
    pub gross_earnings: Decimal,
    pub total_deductions: Decimal,
    pub employer_contributions: Decimal,
    pub net_pay: Decimal,
    pub salary_assignment_id: Option<Uuid>,
    pub structure_id: Option<Uuid>,
    pub config_id: Option<Uuid>,
    pub snapshot: serde_json::Value,
    pub exceptions: serde_json::Value,
    pub finalized_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub lines: Vec<LineRow>,
}

pub const RESULT_COLUMNS: &str = "id, institution_id, run_id, staff_id, engagement_id, \
    staff_number, display_name, status, revision, supersedes_result_id, origin_result_id, \
    period_start, period_end, period_days, employed_days, lop_days, payable_days, gross_earnings, \
    total_deductions, employer_contributions, net_pay, salary_assignment_id, structure_id, \
    config_id, snapshot, exceptions, finalized_at, version";

pub fn result_from_row(r: &PgRow) -> Result<ResultRow, sqlx::Error> {
    Ok(ResultRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        run_id: r.try_get("run_id")?,
        staff_id: r.try_get("staff_id")?,
        engagement_id: r.try_get("engagement_id")?,
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        status: decode(r.try_get("status")?, PayrollResultStatus::parse)?,
        revision: r.try_get("revision")?,
        supersedes_result_id: r.try_get("supersedes_result_id")?,
        origin_result_id: r.try_get("origin_result_id")?,
        period_start: r.try_get("period_start")?,
        period_end: r.try_get("period_end")?,
        period_days: r.try_get("period_days")?,
        employed_days: r.try_get("employed_days")?,
        lop_days: r.try_get("lop_days")?,
        payable_days: r.try_get("payable_days")?,
        gross_earnings: r.try_get("gross_earnings")?,
        total_deductions: r.try_get("total_deductions")?,
        employer_contributions: r.try_get("employer_contributions")?,
        net_pay: r.try_get("net_pay")?,
        salary_assignment_id: r.try_get("salary_assignment_id")?,
        structure_id: r.try_get("structure_id")?,
        config_id: r.try_get("config_id")?,
        snapshot: r.try_get("snapshot")?,
        exceptions: r.try_get("exceptions")?,
        finalized_at: r.try_get("finalized_at")?,
        version: r.try_get("version")?,
        lines: Vec::new(),
    })
}

fn line_from_row(r: &PgRow) -> Result<LineRow, sqlx::Error> {
    Ok(LineRow {
        line_no: r.try_get("line_no")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        kind: decode(r.try_get("kind")?, PayComponentKind::parse)?,
        amount: r.try_get("amount")?,
        basis: r.try_get("basis")?,
        source: r.try_get("source")?,
        source_ref: r.try_get("source_ref")?,
    })
}

pub async fn lines_of(conn: &mut PgConnection, result: Uuid) -> Result<Vec<LineRow>, KernelError> {
    sqlx::query(
        "SELECT line_no, code, name, kind, amount, basis, source, source_ref \
         FROM people.payroll_result_line WHERE result_id = $1 ORDER BY line_no",
    )
    .bind(result)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(line_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

pub async fn load_result(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ResultRow, KernelError> {
    let sql = format!(
        "SELECT {RESULT_COLUMNS} FROM people.payroll_result WHERE id = $1 AND tenant_id = $2{}",
        lock_clause(lock)
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "payroll result",
        })?;
    let mut r = result_from_row(&row).map_err(map_db_error)?;
    r.lines = lines_of(conn, r.id).await?;
    Ok(r)
}

/// Current (not superseded) results of a run, by staff number, with lines.
pub async fn current_results(
    conn: &mut PgConnection,
    tenant: TenantId,
    run: Uuid,
) -> Result<Vec<ResultRow>, KernelError> {
    let sql = format!(
        "SELECT {RESULT_COLUMNS} FROM people.payroll_result WHERE run_id = $1 AND tenant_id = $2 \
         AND status <> 'superseded' ORDER BY staff_number, id"
    );
    let rows = sqlx::query(&sql)
        .bind(run)
        .bind(tenant.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut r = result_from_row(row).map_err(map_db_error)?;
        r.lines = lines_of(conn, r.id).await?;
        out.push(r);
    }
    Ok(out)
}

// ------------------------------------------------------------------ adjustments

#[derive(Clone, Debug)]
pub struct AdjustmentRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub result_id: Uuid,
    pub run_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub status: PayrollAdjustmentStatus,
    pub status_reason: Option<String>,
    pub reason: String,
    pub lines: serde_json::Value,
    pub gross_delta: Decimal,
    pub deduction_delta: Decimal,
    pub employer_delta: Decimal,
    pub net_delta: Decimal,
    pub revised_result_id: Option<Uuid>,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub applied_at: Option<DateTime<Utc>>,
    pub version: i64,
}

pub const ADJUSTMENT_COLUMNS: &str = "id, institution_id, result_id, run_id, staff_id, \
    engagement_id, status, status_reason, reason, lines, gross_delta, deduction_delta, \
    employer_delta, net_delta, revised_result_id, journal_entry_id, journal_entry_number, \
    process_instance_id, applied_at, version";

pub fn adjustment_from_row(r: &PgRow) -> Result<AdjustmentRow, sqlx::Error> {
    Ok(AdjustmentRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        result_id: r.try_get("result_id")?,
        run_id: r.try_get("run_id")?,
        staff_id: r.try_get("staff_id")?,
        engagement_id: r.try_get("engagement_id")?,
        status: decode(r.try_get("status")?, PayrollAdjustmentStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        reason: r.try_get("reason")?,
        lines: r.try_get("lines")?,
        gross_delta: r.try_get("gross_delta")?,
        deduction_delta: r.try_get("deduction_delta")?,
        employer_delta: r.try_get("employer_delta")?,
        net_delta: r.try_get("net_delta")?,
        revised_result_id: r.try_get("revised_result_id")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        journal_entry_number: r.try_get("journal_entry_number")?,
        process_instance_id: r.try_get("process_instance_id")?,
        applied_at: r.try_get("applied_at")?,
        version: r.try_get("version")?,
    })
}

pub async fn load_adjustment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<AdjustmentRow, KernelError> {
    let sql = format!(
        "SELECT {ADJUSTMENT_COLUMNS} FROM people.payroll_adjustment WHERE id = $1 AND tenant_id = $2{}",
        lock_clause(lock)
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "payroll adjustment",
        })?;
    adjustment_from_row(&row).map_err(map_db_error)
}

// ------------------------------------------------------------------ final settlements

#[derive(Clone, Debug)]
pub struct SettlementRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub number: Option<String>,
    pub status: FinalSettlementStatus,
    pub status_reason: Option<String>,
    pub last_working_date: NaiveDate,
    pub recoveries: serde_json::Value,
    pub notes: Option<String>,
    pub run_id: Option<Uuid>,
    pub salary_amount: Decimal,
    pub encashment_amount: Decimal,
    pub encashment_days: Decimal,
    pub recovery_amount: Decimal,
    pub net_pay: Decimal,
    pub journal_entry_id: Option<Uuid>,
    pub journal_entry_number: Option<String>,
    pub process_instance_id: Option<String>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: i64,
}

pub const SETTLEMENT_COLUMNS: &str =
    "id, institution_id, staff_id, engagement_id, number, status, \
    status_reason, last_working_date, recoveries, notes, run_id, salary_amount, encashment_amount, \
    encashment_days, recovery_amount, net_pay, journal_entry_id, journal_entry_number, \
    process_instance_id, completed_at, version";

pub fn settlement_from_row(r: &PgRow) -> Result<SettlementRow, sqlx::Error> {
    Ok(SettlementRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        staff_id: r.try_get("staff_id")?,
        engagement_id: r.try_get("engagement_id")?,
        number: r.try_get("number")?,
        status: decode(r.try_get("status")?, FinalSettlementStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        last_working_date: r.try_get("last_working_date")?,
        recoveries: r.try_get("recoveries")?,
        notes: r.try_get("notes")?,
        run_id: r.try_get("run_id")?,
        salary_amount: r.try_get("salary_amount")?,
        encashment_amount: r.try_get("encashment_amount")?,
        encashment_days: r.try_get("encashment_days")?,
        recovery_amount: r.try_get("recovery_amount")?,
        net_pay: r.try_get("net_pay")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        journal_entry_number: r.try_get("journal_entry_number")?,
        process_instance_id: r.try_get("process_instance_id")?,
        completed_at: r.try_get("completed_at")?,
        version: r.try_get("version")?,
    })
}

pub async fn load_settlement(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<SettlementRow, KernelError> {
    let sql = format!(
        "SELECT {SETTLEMENT_COLUMNS} FROM people.final_settlement WHERE id = $1 AND tenant_id = $2{}",
        lock_clause(lock)
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "final settlement",
        })?;
    settlement_from_row(&row).map_err(map_db_error)
}
