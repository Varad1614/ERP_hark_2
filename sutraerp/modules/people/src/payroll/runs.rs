//! Payroll runs (L3-E-053 regular, E-055 retro, E-057 off-cycle): creation, calculation
//! (inputs only from `WorkforceDirectory::payable_engagements` + `LeaveReader::payroll_inputs`),
//! validation, approval (`PAYROLL_APPROVAL` / `OFF_CYCLE_APPROVAL`), finalisation (results
//! immutable, encashment payments recorded), Finance posting through `PayrollPostingPort`
//! (`posting_key = finalise`) and close; failures → `failed` → `reprocessing`.

use super::common::{
    cause, ensure_version, history, illegal, invalid, opt_reason, payload, required_reason, Change,
    Cmd,
};
use super::domain::{
    calculate, days_between, employed_days, retro_delta, round_with, run_can_transition, totals,
    year_month, CalcInput, CalcLine, CalcOutput, ComponentDef, Encashment, LineSource, PriorLine,
    RuleDef, TimesheetPay,
};
use super::rows::*;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::fmt2;
use crate::shared::guard::require_institution_writable;
use crate::shared::numbering::{self, kind};
use crate::shared::publish::{audit, event};
use crate::shared::rows::{decode, load_engagement};
use crate::shared::{Actor, PeopleState};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, BTreeSet};
use sutra_contracts::finance::ports::{PayrollPostingReceipt, PayrollPostingRequest};
use sutra_contracts::people::events::payroll::{
    PAYROLL_RUN_APPROVED, PAYROLL_RUN_APPROVED_SCHEMA, PAYROLL_RUN_CALCULATED,
    PAYROLL_RUN_CALCULATED_SCHEMA, PAYROLL_RUN_FAILED, PAYROLL_RUN_FAILED_SCHEMA,
    PAYROLL_RUN_FINALIZED, PAYROLL_RUN_FINALIZED_SCHEMA, PAYROLL_RUN_POSTED,
    PAYROLL_RUN_POSTED_SCHEMA, PAYROLL_RUN_SUBMITTED, PAYROLL_RUN_SUBMITTED_SCHEMA,
};
use sutra_contracts::people::events::PayrollPostedV1;
use sutra_contracts::people::ids::StaffEngagementId;
use sutra_contracts::people::ports::{EncashmentEntry, EngagementSummary};
use sutra_contracts::people::status::{PayComponentKind, PayrollRunKind, PayrollRunStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub(crate) const RUN: &str = "payroll_run";

/// One off-cycle item (bonus, correction) of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OffCycleItem {
    pub engagement_id: Uuid,
    pub component_code: String,
    pub amount: Decimal,
    pub note: Option<String>,
}

/// Input of a new run.
pub struct NewRun {
    pub kind: PayrollRunKind,
    pub description: Option<String>,
    pub items: Vec<OffCycleItem>,
}

fn items_json(items: &[OffCycleItem]) -> Value {
    Value::Array(
        items
            .iter()
            .map(|i| {
                json!({"engagementId": i.engagement_id, "componentCode": i.component_code,
                       "amount": fmt2(i.amount), "note": i.note})
            })
            .collect(),
    )
}

fn items_from_json(v: &Value) -> Vec<OffCycleItem> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|i| {
                    Some(OffCycleItem {
                        engagement_id: i.get("engagementId")?.as_str()?.parse().ok()?,
                        component_code: i.get("componentCode")?.as_str()?.to_owned(),
                        amount: i.get("amount")?.as_str()?.parse().ok()?,
                        note: i.get("note").and_then(Value::as_str).map(str::to_owned),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn get_run(st: &PeopleState, tenant: TenantId, id: Uuid) -> Result<RunRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_run(&mut conn, tenant, id, false).await
}

pub async fn list_runs(
    st: &PeopleState,
    tenant: TenantId,
    period: Uuid,
) -> Result<Vec<RunRow>, KernelError> {
    let sql = format!("{RUN_SELECT} WHERE r.tenant_id = $1 AND r.period_id = $2 ORDER BY r.number");
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(period)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(run_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// Creates a run of a period in `created` (L4-E-053-BH-001). One regular run per period
/// (409); a retro run needs a finalised regular run of the period; an off-cycle run needs items
/// for payable engagements; final-settlement runs are created by their settlement only.
pub async fn create_run(c: &Cmd<'_>, period: Uuid, n: NewRun) -> Result<RunRow, KernelError> {
    let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_period(&mut conn, c.tenant, period).await?;
    let scope = c.scope(p.institution_id);
    require_institution_writable(c.st, &scope, p.institution_id).await?;
    if n.kind != PayrollRunKind::OffCycle && !n.items.is_empty() {
        return Err(invalid(
            "items",
            "not_applicable",
            "only off-cycle runs take items",
        ));
    }
    match n.kind {
        PayrollRunKind::FinalSettlement => {
            return Err(invalid(
                "kind",
                "not_allowed",
                "final-settlement runs are created by POST /engagements/{id}/final-settlement",
            ));
        }
        PayrollRunKind::Retro => {
            let base: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM people.payroll_run WHERE period_id = $1 \
                 AND kind = 'regular' AND status IN ('finalized', 'posted', 'closed'))",
            )
            .bind(period)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            if !base {
                return Err(KernelError::Conflict(
                    "a retro run needs a finalised regular run of the period".into(),
                ));
            }
        }
        PayrollRunKind::OffCycle => {
            if n.items.is_empty() {
                return Err(invalid(
                    "items",
                    "required",
                    "an off-cycle run pays at least one item",
                ));
            }
            for it in &n.items {
                if it.amount.is_zero() {
                    return Err(invalid("items", "zero", "item amounts must not be zero"));
                }
                if !super::config::valid_code(&it.component_code) {
                    return Err(invalid("items", "invalid_code", "unknown component code"));
                }
                let e = load_engagement(&mut conn, c.tenant, StaffEngagementId(it.engagement_id))
                    .await?
                    .filter(|e| e.institution_id == p.institution_id)
                    .ok_or_else(|| invalid("items", "not_found", "unknown engagement"))?;
                if !e.status.is_payable() {
                    return Err(invalid(
                        "items",
                        "not_payable",
                        format!("engagement {} is {}", e.engagement_number, e.status),
                    ));
                }
            }
        }
        PayrollRunKind::Regular => {}
    }
    drop(conn);
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let id = insert_run(
        &mut tx,
        c,
        &p,
        n.kind,
        opt_reason(n.description.as_deref()),
        items_json(&n.items),
    )
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) if n.kind == PayrollRunKind::Regular => {
            KernelError::Conflict("the period already has a regular payroll run".into())
        }
        other => other,
    })?;
    let run = load_run(&mut tx, c.tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(run)
}

/// Inserts a run in `created` (numbering + history + audit) in the caller's transaction.
pub(crate) async fn insert_run(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    p: &PeriodRow,
    run_kind: PayrollRunKind,
    description: Option<String>,
    items: Value,
) -> Result<Uuid, KernelError> {
    let number = numbering::next(conn, c.tenant, p.institution_id, kind::PAYROLL_RUN).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.payroll_run (tenant_id, institution_id, period_id, number, kind, \
         description, items) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(c.tenant.as_uuid())
    .bind(p.institution_id.as_uuid())
    .bind(p.id)
    .bind(&number)
    .bind(run_kind.as_str())
    .bind(description.as_deref())
    .bind(items)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        c.tenant,
        p.institution_id,
        RUN,
        id,
        None,
        PayrollRunStatus::Created.as_str(),
        &cause("people.payroll_run.created", None, None),
        c.actor,
    )
    .await?;
    audit(
        conn,
        &c.scope(p.institution_id),
        "people.payroll_run.create",
        RUN,
        id.to_string(),
        json!({"number": number, "kind": run_kind.as_str(), "periodId": p.id}),
        c.actor,
    )
    .await?;
    Ok(id)
}

/// Moves a locked run (legal transitions only) and records history; returns the reloaded run.
pub(crate) async fn move_run(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    run: &RunRow,
    to: PayrollRunStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<RunRow, KernelError> {
    if !run_can_transition(run.status, to) {
        return Err(illegal(
            "payroll run",
            &run.number,
            run.status.as_str(),
            to.as_str(),
        ));
    }
    let done = sqlx::query(
        "UPDATE people.payroll_run SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), \
         calculated_at = CASE WHEN $2 = 'calculated' THEN now() ELSE calculated_at END, \
         approved_at = CASE WHEN $2 = 'approved' THEN now() ELSE approved_at END, \
         finalized_at = CASE WHEN $2 = 'finalized' THEN now() ELSE finalized_at END, \
         posted_at = CASE WHEN $2 = 'posted' THEN now() ELSE posted_at END, \
         closed_at = CASE WHEN $2 = 'closed' THEN now() ELSE closed_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND version = $5",
    )
    .bind(run.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .bind(run.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if done.rows_affected() != 1 {
        return Err(KernelError::Conflict(
            "payroll run changed concurrently".into(),
        ));
    }
    history(
        conn,
        c.tenant,
        run.institution_id,
        RUN,
        run.id,
        Some(run.status.as_str()),
        to.as_str(),
        &cause("people.payroll_run.status", reason, process_instance_id),
        actor,
    )
    .await?;
    load_run(conn, c.tenant, run.id, false).await
}

/// `StatusChangedV1` of a run that moved from `from`.
pub(crate) fn run_payload(
    run: &RunRow,
    from: PayrollRunStatus,
    reason: Option<&str>,
) -> sutra_contracts::people::events::StatusChangedV1 {
    payload(&Change {
        aggregate_id: run.id,
        institution: Some(run.institution_id),
        from: Some(from.as_str()),
        to: run.status.as_str(),
        version: run.version,
        reason,
        process_instance_id: run.process_instance_id.as_deref(),
        ..Change::default()
    })
}

/// Emits a lifecycle event of a run.
async fn run_event(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    run: &RunRow,
    from: PayrollRunStatus,
    types: (&'static str, &'static str),
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    event(
        conn,
        &c.scope(run.institution_id),
        types.0,
        types.1,
        run.id.to_string(),
        &run_payload(run, from, reason),
        actor,
    )
    .await
}

fn ensure_not_settlement(run: &RunRow) -> Result<(), KernelError> {
    if run.kind == PayrollRunKind::FinalSettlement {
        Err(KernelError::Conflict(format!(
            "payroll run {} belongs to a final settlement; use its settlement commands",
            run.number
        )))
    } else {
        Ok(())
    }
}

// ------------------------------------------------------------------ calculation

/// One employee result computed before the write transaction.
#[derive(Clone, Debug)]
pub(crate) struct NewResult {
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub period_days: i64,
    pub employed_days: i64,
    pub lop_days: Decimal,
    pub out: CalcOutput,
    pub assignment_id: Option<Uuid>,
    pub structure_id: Option<Uuid>,
    pub snapshot: Value,
}

/// The active configuration of a run's institution with its components and rules.
pub(crate) struct ActiveConfig {
    pub config: ConfigRow,
    pub components: Vec<ComponentDef>,
    pub rules: Vec<RuleDef>,
}

pub(crate) async fn load_active(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Option<ActiveConfig>, KernelError> {
    let Some(config) = active_config(conn, tenant, institution).await? else {
        return Ok(None);
    };
    let components = components_of(conn, config.id)
        .await?
        .into_iter()
        .map(|c| c.def)
        .collect();
    let rules = rules_of(conn, config.id)
        .await?
        .into_iter()
        .map(|r| r.def)
        .collect();
    Ok(Some(ActiveConfig {
        config,
        components,
        rules,
    }))
}

fn exception(e: &EngagementSummary, code: &str, message: &str) -> Value {
    json!({"engagementId": e.id.as_uuid(), "staffId": e.staff_id.as_uuid(),
           "staffNumber": e.engagement_number, "code": code, "message": message})
}

fn config_snapshot(cfg: &ActiveConfig) -> Value {
    let components: Vec<Value> = cfg
        .components
        .iter()
        .map(|c| {
            json!({"code": c.code, "kind": c.kind.as_str(), "basis": c.basis.as_str(),
                   "amount": c.amount.map(fmt2), "rate": c.rate.map(super::common::fmt4)})
        })
        .collect();
    let rules: Vec<Value> = cfg
        .rules
        .iter()
        .map(|r| {
            json!({"kind": r.kind.as_str(),
                   "employeeRate": r.employee_rate.map(super::common::fmt4),
                   "employerRate": r.employer_rate.map(super::common::fmt4),
                   "wageCeiling": r.wage_ceiling.map(fmt2), "threshold": r.threshold.map(fmt2),
                   "slabs": r.slabs, "fixedAmount": r.fixed_amount.map(fmt2),
                   "employeeCode": r.employee_code, "employerCode": r.employer_code})
        })
        .collect();
    json!({
        "configId": cfg.config.id,
        "versionNo": cfg.config.version_no,
        "rounding": cfg.config.rounding.as_str(),
        "encashmentDivisor": cfg.config.encashment_divisor,
        "components": components,
        "rules": rules,
    })
}

/// Unpaid encashment entries among `entries` (not yet in `people.encashment_payment`).
pub(crate) async fn unpaid_encashments(
    conn: &mut PgConnection,
    entries: &[EncashmentEntry],
) -> Result<Vec<Encashment>, KernelError> {
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = entries.iter().map(|e| e.entry_id.as_uuid()).collect();
    let paid: Vec<Uuid> = sqlx::query_scalar(
        "SELECT ledger_entry_id FROM people.encashment_payment WHERE ledger_entry_id = ANY($1)",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(entries
        .iter()
        .filter(|e| !paid.contains(&e.entry_id.as_uuid()))
        .map(|e| (e.entry_id.as_uuid(), e.days.abs()))
        .collect())
}

/// What one employee calculation needs besides the configuration.
pub(crate) struct EmployeeCalc<'a> {
    pub engagement: &'a EngagementSummary,
    /// Period the salary is paid for (the run period / the final month).
    pub from: NaiveDate,
    pub to: NaiveDate,
    /// Days of the full period (proration denominator).
    pub period_days: i64,
    pub pay_salary: bool,
    /// Days inside `[from, to]` an earlier finalised run already paid (final settlement after
    /// a gap; 0 for every regular / retro / off-cycle calculation).
    pub already_paid_days: i64,
    pub encashments: Vec<Encashment>,
}

/// Outcome of one employee calculation: a result, or a business exception `(code, message)`.
pub(crate) type EmployeeOutcome = Result<NewResult, (String, String)>;

/// Calculates one employee: salary assignment + structure, LOP from `LeaveReader`, the engine.
pub(crate) async fn calc_employee(
    c: &Cmd<'_>,
    conn: &mut PgConnection,
    cfg: &ActiveConfig,
    ec: EmployeeCalc<'_>,
) -> Result<EmployeeOutcome, KernelError> {
    let e = ec.engagement;
    let scope = c.scope(e.institution_id);
    let Some(staff) = c.st.directory.staff(&scope, e.staff_id).await? else {
        return Ok(Err((
            "missing_staff_record".into(),
            "the engagement has no staff record".into(),
        )));
    };
    let eng_end = e.last_working_date.or(e.end_date);
    let window_from = ec.from.max(e.start_date);
    let window_to = eng_end.map_or(ec.to, |x| x.min(ec.to)).max(window_from);
    let Some(a) = assignment_for(conn, c.tenant, e.id.as_uuid(), window_from, window_to).await?
    else {
        return Ok(Err((
            "missing_salary_assignment".into(),
            format!(
                "{} has no salary assignment for the period",
                staff.staff_number
            ),
        )));
    };
    let s = load_structure(conn, c.tenant, a.structure_id, false).await?;
    let employed = if ec.pay_salary {
        (employed_days(ec.from, ec.to, e.start_date, eng_end) - ec.already_paid_days).max(0)
    } else {
        0
    };
    let inputs =
        c.st.leave
            .payroll_inputs(&scope, e.staff_id, window_from, window_to)
            .await?;
    // ERP_EXTENSIONS 5B: approved timesheets of the window (overtime, extra LOP, shifts).
    let timesheet = if ec.pay_salary {
        timesheet_pay(
            conn,
            c.tenant,
            e.institution_id.as_uuid(),
            e.staff_id.as_uuid(),
            window_from,
            window_to,
        )
        .await?
    } else {
        None
    };
    let timesheet_lop = timesheet.as_ref().map_or(Decimal::ZERO, |(_, l)| *l);
    let lop = if ec.pay_salary {
        inputs.lop_days() + timesheet_lop
    } else {
        Decimal::ZERO
    };
    let timesheet = timesheet.map(|(t, _)| t);
    let timesheet_snapshot = timesheet.as_ref().map(|t| {
        json!({"overtimeHours": fmt2(t.overtime_hours), "nightShifts": fmt2(t.night_shifts),
               "holidayShifts": fmt2(t.holiday_shifts), "lopDays": fmt2(timesheet_lop)})
    });
    let out = calculate(&CalcInput {
        components: &cfg.components,
        rules: &cfg.rules,
        structure: &s.lines,
        rounding: cfg.config.rounding,
        encashment_divisor: cfg.config.encashment_divisor,
        period_days: ec.period_days,
        employed_days: employed,
        lop_days: lop,
        encashments: &ec.encashments,
        timesheet,
    });
    let structure_lines: Vec<Value> = s
        .lines
        .iter()
        .map(|l| {
            json!({"code": l.code, "amount": l.amount.map(fmt2),
                   "rate": l.rate.map(super::common::fmt4)})
        })
        .collect();
    let encashed: Vec<Value> = ec
        .encashments
        .iter()
        .map(|(id, d)| json!({"entryId": id, "days": fmt2(*d)}))
        .collect();
    let snapshot = json!({
        "config": config_snapshot(cfg),
        "assignmentId": a.id,
        "structure": {"id": s.id, "code": s.code, "lines": structure_lines},
        "inputs": {
            "from": window_from,
            "to": window_to,
            "unpaidLeaveDays": fmt2(inputs.unpaid_leave_days),
            "absentDays": fmt2(inputs.absent_days),
            "lopDays": fmt2(lop),
            "encashments": encashed,
            "timesheet": timesheet_snapshot,
        },
        "periodDays": ec.period_days,
        "employedDays": employed,
    });
    Ok(Ok(NewResult {
        staff_id: e.staff_id.as_uuid(),
        engagement_id: e.id.as_uuid(),
        staff_number: staff.staff_number.clone(),
        display_name: staff.display_name.clone(),
        period_days: ec.period_days,
        employed_days: employed,
        lop_days: lop,
        out,
        assignment_id: Some(a.id),
        structure_id: Some(s.id),
        snapshot,
    }))
}

/// ERP_EXTENSIONS 5B: the approved-timesheet quantities whose work started inside
/// `[from, to]` (`people.payroll_variable_input`) with the institution's rates
/// (`people.roster_settings`), and the extra loss-of-pay days; `None` when there are none.
async fn timesheet_pay(
    conn: &mut PgConnection,
    tenant: sutra_kernel::scope::TenantId,
    institution: Uuid,
    staff: Uuid,
    from: NaiveDate,
    to: NaiveDate,
) -> Result<Option<(TimesheetPay, Decimal)>, KernelError> {
    let rows: Vec<(String, Decimal, i64, Option<Uuid>)> = sqlx::query_as(
        "SELECT code, sum(quantity), count(DISTINCT source_id), min(source_id::text)::uuid \
         FROM people.payroll_variable_input WHERE tenant_id = $1 AND staff_id = $2 \
         AND status = 'active' AND work_from BETWEEN $3 AND $4 GROUP BY code",
    )
    .bind(tenant.as_uuid())
    .bind(staff)
    .bind(from)
    .bind(to)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if rows.is_empty() {
        return Ok(None);
    }
    let q = |code: &str| {
        rows.iter()
            .find(|r| r.0 == code)
            .map_or(Decimal::ZERO, |r| r.1)
    };
    let single = rows.iter().all(|r| r.2 == 1)
        && rows
            .iter()
            .filter_map(|r| r.3)
            .collect::<BTreeSet<_>>()
            .len()
            == 1;
    // (rate per hour, multiplier, hours a day, night allowance, holiday allowance)
    type Rates = (
        Option<Decimal>,
        Decimal,
        Decimal,
        Option<Decimal>,
        Option<Decimal>,
    );
    let settings: Option<Rates> = sqlx::query_as(
        "SELECT overtime_rate_per_hour, overtime_multiplier, standard_hours_per_day, \
         night_shift_allowance, holiday_shift_allowance FROM people.roster_settings \
         WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (rate, mult, hours, night, holiday) =
        settings.unwrap_or((None, Decimal::from(2), Decimal::from(8), None, None));
    Ok(Some((
        TimesheetPay {
            overtime_hours: q("overtime_hours"),
            night_shifts: q("night_shifts"),
            holiday_shifts: q("holiday_shifts"),
            overtime_rate: rate,
            overtime_multiplier: mult,
            hours_per_day: hours,
            night_allowance: night,
            holiday_allowance: holiday,
            source_ref: if single { rows[0].3 } else { None },
        },
        q("lop_days"),
    )))
}

/// Lines finalised results of the period already paid an engagement (regular + retro runs,
/// calculation and retro lines only — adjustments are corrections of their own).
async fn prior_lines(
    conn: &mut PgConnection,
    period: Uuid,
    engagement: Uuid,
) -> Result<Vec<PriorLine>, KernelError> {
    let rows = sqlx::query(
        "SELECT l.code, l.name, l.kind, l.amount FROM people.payroll_result_line l \
         JOIN people.payroll_result r ON r.id = l.result_id \
         JOIN people.payroll_run u ON u.id = r.run_id \
         WHERE u.period_id = $1 AND r.engagement_id = $2 AND r.status = 'finalized' \
         AND u.kind IN ('regular', 'retro') AND l.source IN ('calculation', 'retro')",
    )
    .bind(period)
    .bind(engagement)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let kind: String = r.try_get("kind").map_err(map_db_error)?;
        out.push((
            r.try_get::<String, _>("code").map_err(map_db_error)?,
            r.try_get::<String, _>("name").map_err(map_db_error)?,
            decode(kind, PayComponentKind::parse).map_err(map_db_error)?,
            r.try_get::<Decimal, _>("amount").map_err(map_db_error)?,
        ));
    }
    Ok(out)
}

/// Computes every result of a regular / retro / off-cycle run (no writes).
async fn prepare(
    c: &Cmd<'_>,
    run: &RunRow,
    cfg: &ActiveConfig,
) -> Result<(Vec<NewResult>, Vec<Value>), KernelError> {
    let scope = c.scope(run.institution_id);
    let period_days = days_between(run.period_start, run.period_end);
    let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
    let mut results = Vec::new();
    let mut exceptions = Vec::new();
    match run.kind {
        PayrollRunKind::Regular => {
            let population = c
                .st
                .directory
                .payable_engagements(&scope, run.institution_id, run.period_start, run.period_end)
                .await?;
            // Engagements whose final settlement pays (or already paid) this period are not paid
            // twice: any current settlement result, finalised or still awaiting approval.
            let settled: Vec<Uuid> = sqlx::query_scalar(
                "SELECT r.engagement_id FROM people.payroll_result r \
                 JOIN people.payroll_run u ON u.id = r.run_id WHERE u.period_id = $1 \
                 AND u.kind = 'final_settlement' AND r.status <> 'superseded'",
            )
            .bind(run.period_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db_error)?;
            for e in population
                .iter()
                .filter(|e| !settled.contains(&e.id.as_uuid()))
            {
                let inputs =
                    c.st.leave
                        .payroll_inputs(&scope, e.staff_id, run.period_start, run.period_end)
                        .await?;
                let encashments = unpaid_encashments(&mut conn, &inputs.encashments).await?;
                let outcome = calc_employee(
                    c,
                    &mut conn,
                    cfg,
                    EmployeeCalc {
                        engagement: e,
                        from: run.period_start,
                        to: run.period_end,
                        period_days,
                        pay_salary: true,
                        already_paid_days: 0,
                        encashments,
                    },
                )
                .await?;
                match outcome {
                    Ok(r) => results.push(r),
                    Err((code, msg)) => exceptions.push(exception(e, &code, &msg)),
                }
            }
        }
        PayrollRunKind::Retro => {
            let engagements: Vec<Uuid> = sqlx::query_scalar(
                "SELECT DISTINCT r.engagement_id FROM people.payroll_result r \
                 JOIN people.payroll_run u ON u.id = r.run_id WHERE u.period_id = $1 \
                 AND u.kind IN ('regular', 'retro') AND r.status = 'finalized'",
            )
            .bind(run.period_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db_error)?;
            for id in engagements {
                let Some(e) =
                    c.st.directory
                        .engagement(&scope, StaffEngagementId(id))
                        .await?
                else {
                    continue;
                };
                let outcome = calc_employee(
                    c,
                    &mut conn,
                    cfg,
                    EmployeeCalc {
                        engagement: &e,
                        from: run.period_start,
                        to: run.period_end,
                        period_days,
                        pay_salary: true,
                        already_paid_days: 0,
                        encashments: Vec::new(),
                    },
                )
                .await?;
                let mut r = match outcome {
                    Ok(r) => r,
                    Err((code, msg)) => {
                        exceptions.push(exception(&e, &code, &msg));
                        continue;
                    }
                };
                let recalculated: Vec<CalcLine> = r
                    .out
                    .lines
                    .iter()
                    .filter(|l| l.source == LineSource::Calculation)
                    .cloned()
                    .collect();
                let prior = prior_lines(&mut conn, run.period_id, id).await?;
                let delta = retro_delta(&recalculated, &prior);
                if delta.is_empty() {
                    continue;
                }
                let t = totals(&delta);
                r.out = CalcOutput {
                    lines: delta,
                    payable_days: r.out.payable_days,
                    totals: t,
                    exceptions: Vec::new(),
                };
                results.push(r);
            }
        }
        PayrollRunKind::OffCycle => {
            let mut by_engagement: BTreeMap<Uuid, Vec<OffCycleItem>> = BTreeMap::new();
            for it in items_from_json(&run.items) {
                by_engagement.entry(it.engagement_id).or_default().push(it);
            }
            for (id, items) in by_engagement {
                let Some(e) =
                    c.st.directory
                        .engagement(&scope, StaffEngagementId(id))
                        .await?
                else {
                    continue;
                };
                let Some(staff) = c.st.directory.staff(&scope, e.staff_id).await? else {
                    continue;
                };
                let mut lines = Vec::new();
                let mut bad = false;
                for it in &items {
                    match cfg.components.iter().find(|x| x.code == it.component_code) {
                        Some(comp) => lines.push(
                            CalcLine::new(
                                &comp.code,
                                &comp.name,
                                comp.kind,
                                round_with(cfg.config.rounding, it.amount),
                            )
                            .source(LineSource::OffCycle, None),
                        ),
                        None => {
                            exceptions.push(exception(
                                &e,
                                "unknown_component",
                                &format!(
                                    "component {} is not in the active configuration",
                                    it.component_code
                                ),
                            ));
                            bad = true;
                        }
                    }
                }
                if bad {
                    continue;
                }
                let t = totals(&lines);
                let mut exc = Vec::new();
                if t.net < Decimal::ZERO {
                    exc.push("negative_net_pay".to_owned());
                }
                let employed = employed_days(
                    run.period_start,
                    run.period_end,
                    e.start_date,
                    e.last_working_date.or(e.end_date),
                );
                results.push(NewResult {
                    staff_id: e.staff_id.as_uuid(),
                    engagement_id: id,
                    staff_number: staff.staff_number.clone(),
                    display_name: staff.display_name.clone(),
                    period_days,
                    employed_days: employed,
                    lop_days: Decimal::ZERO,
                    out: CalcOutput {
                        lines,
                        payable_days: Decimal::from(employed),
                        totals: t,
                        exceptions: exc,
                    },
                    assignment_id: None,
                    structure_id: None,
                    snapshot: json!({"config": config_snapshot(cfg), "items": items_json(&items)}),
                });
            }
        }
        PayrollRunKind::FinalSettlement => {}
    }
    for r in &results {
        for x in &r.out.exceptions {
            exceptions.push(
                json!({"engagementId": r.engagement_id, "staffId": r.staff_id,
                "staffNumber": r.staff_number, "code": x, "message": x}),
            );
        }
    }
    Ok((results, exceptions))
}

/// Inserts one result (+ lines) of a run; returns its id.
pub(crate) async fn insert_result(
    conn: &mut PgConnection,
    tenant: TenantId,
    run: &RunRow,
    r: &NewResult,
    config_id: Option<Uuid>,
) -> Result<Uuid, KernelError> {
    let id = Uuid::now_v7();
    let t = r.out.totals;
    sqlx::query(
        "INSERT INTO people.payroll_result (id, tenant_id, institution_id, run_id, staff_id, \
         engagement_id, staff_number, display_name, origin_result_id, period_start, period_end, \
         period_days, employed_days, lop_days, payable_days, gross_earnings, total_deductions, \
         employer_contributions, net_pay, salary_assignment_id, structure_id, config_id, \
         snapshot, exceptions) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $1, $9, $10, $11, $12, \
         $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23)",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(run.institution_id.as_uuid())
    .bind(run.id)
    .bind(r.staff_id)
    .bind(r.engagement_id)
    .bind(&r.staff_number)
    .bind(&r.display_name)
    .bind(run.period_start)
    .bind(run.period_end)
    .bind(Decimal::from(r.period_days))
    .bind(Decimal::from(r.employed_days))
    .bind(r.lop_days)
    .bind(r.out.payable_days)
    .bind(t.gross)
    .bind(t.deductions)
    .bind(t.employer)
    .bind(t.net)
    .bind(r.assignment_id)
    .bind(r.structure_id)
    .bind(config_id)
    .bind(&r.snapshot)
    .bind(json!(r.out.exceptions))
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    insert_lines(conn, tenant, id, &r.out.lines, 1).await?;
    Ok(id)
}

/// Inserts lines numbered from `first`.
pub(crate) async fn insert_lines(
    conn: &mut PgConnection,
    tenant: TenantId,
    result: Uuid,
    lines: &[CalcLine],
    first: i32,
) -> Result<(), KernelError> {
    for (n, l) in (first..).zip(lines) {
        sqlx::query(
            "INSERT INTO people.payroll_result_line (tenant_id, result_id, line_no, code, name, \
             kind, amount, basis, source, source_ref) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(tenant.as_uuid())
        .bind(result)
        .bind(n)
        .bind(&l.code)
        .bind(&l.name)
        .bind(l.kind.as_str())
        .bind(l.amount)
        .bind(l.basis.as_deref())
        .bind(l.source.as_str())
        .bind(l.source_ref)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

/// Deletes the not-yet-final results of a run (recalculation).
pub(crate) async fn delete_open_results(
    conn: &mut PgConnection,
    run: Uuid,
) -> Result<(), KernelError> {
    sqlx::query(
        "DELETE FROM people.payroll_result WHERE run_id = $1 \
         AND status IN ('calculated', 'validated')",
    )
    .bind(run)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Stores run totals / exceptions / headcount from its current results.
pub(crate) async fn store_totals(
    conn: &mut PgConnection,
    run: Uuid,
    config: Option<Uuid>,
    exceptions: &[Value],
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE people.payroll_run r SET headcount = s.n, gross_earnings = s.g, \
         employee_deductions = s.d, employer_contributions = s.e, net_pay = s.p, \
         config_id = COALESCE($2, r.config_id), exceptions = $3 \
         FROM (SELECT count(*)::int AS n, COALESCE(sum(gross_earnings), 0) AS g, \
               COALESCE(sum(total_deductions), 0) AS d, \
               COALESCE(sum(employer_contributions), 0) AS e, \
               COALESCE(sum(net_pay), 0) AS p FROM people.payroll_result \
               WHERE run_id = $1 AND status <> 'superseded') s WHERE r.id = $1",
    )
    .bind(run)
    .bind(config)
    .bind(Value::Array(exceptions.to_vec()))
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Calculates a run (L4-E-053-BH-002/003): `created | calculated | validating | reprocessing →
/// calculating → calculated` with fresh per-employee results, or `→ failed` when no active
/// configuration applies (`payroll_run.failed`).
pub async fn calculate_run(c: &Cmd<'_>, id: Uuid, expected: i64) -> Result<RunRow, KernelError> {
    let run = get_run(c.st, c.tenant, id).await?;
    ensure_version(expected, run.version)?;
    ensure_not_settlement(&run)?;
    if !run_can_transition(run.status, PayrollRunStatus::Calculating) {
        return Err(illegal(
            "payroll run",
            &run.number,
            run.status.as_str(),
            "calculating",
        ));
    }
    let scope = c.scope(run.institution_id);
    require_institution_writable(c.st, &scope, run.institution_id).await?;
    let cfg = {
        let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
        load_active(&mut conn, c.tenant, run.institution_id).await?
    };
    let prepared = match &cfg {
        Some(cfg) => Some(prepare(c, &run, cfg).await?),
        None => None,
    };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    let calculating = move_run(
        &mut tx,
        c,
        &locked,
        PayrollRunStatus::Calculating,
        None,
        None,
        c.actor,
    )
    .await?;
    let out = match (cfg, prepared) {
        (Some(cfg), Some((results, exceptions))) => {
            delete_open_results(&mut tx, id).await?;
            for r in &results {
                insert_result(&mut tx, c.tenant, &calculating, r, Some(cfg.config.id)).await?;
            }
            store_totals(&mut tx, id, Some(cfg.config.id), &exceptions).await?;
            let reloaded = load_run(&mut tx, c.tenant, id, false).await?;
            let done = move_run(
                &mut tx,
                c,
                &reloaded,
                PayrollRunStatus::Calculated,
                None,
                None,
                c.actor,
            )
            .await?;
            run_event(
                &mut tx,
                c,
                &done,
                PayrollRunStatus::Calculating,
                (PAYROLL_RUN_CALCULATED, PAYROLL_RUN_CALCULATED_SCHEMA),
                None,
                c.actor,
            )
            .await?;
            audit(
                &mut tx,
                &scope,
                "people.payroll_run.calculate",
                RUN,
                id.to_string(),
                json!({"headcount": done.headcount, "grossEarnings": fmt2(done.gross_earnings),
                       "netPay": fmt2(done.net_pay), "exceptions": exceptions.len(),
                       "configId": cfg.config.id, "configVersionNo": cfg.config.version_no}),
                c.actor,
            )
            .await?;
            done
        }
        _ => {
            let reason = "no active payroll configuration for the institution";
            let failed = move_run(
                &mut tx,
                c,
                &calculating,
                PayrollRunStatus::Failed,
                Some(reason),
                None,
                c.actor,
            )
            .await?;
            run_event(
                &mut tx,
                c,
                &failed,
                PayrollRunStatus::Calculating,
                (PAYROLL_RUN_FAILED, PAYROLL_RUN_FAILED_SCHEMA),
                Some(reason),
                c.actor,
            )
            .await?;
            audit(
                &mut tx,
                &scope,
                "people.payroll_run.calculate",
                RUN,
                id.to_string(),
                json!({"failed": reason}),
                c.actor,
            )
            .await?;
            failed
        }
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Validates a calculated run (L4-E-053-BH-004): exceptions (missing salary assignment,
/// negative net pay, an empty run) move it to `failed`; otherwise results become `validated`
/// and the run waits in `validating` for submission.
pub async fn validate_run(c: &Cmd<'_>, id: Uuid, expected: i64) -> Result<RunRow, KernelError> {
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let run = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, run.version)?;
    ensure_not_settlement(&run)?;
    if run.status != PayrollRunStatus::Calculated {
        return Err(illegal(
            "payroll run",
            &run.number,
            run.status.as_str(),
            "validating",
        ));
    }
    let validating = move_run(
        &mut tx,
        c,
        &run,
        PayrollRunStatus::Validating,
        None,
        None,
        c.actor,
    )
    .await?;
    let mut findings: Vec<String> = validating
        .exceptions
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| {
                    format!(
                        "{}: {}",
                        x.get("staffNumber").and_then(Value::as_str).unwrap_or("-"),
                        x.get("code").and_then(Value::as_str).unwrap_or("exception")
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    if validating.headcount == 0 {
        findings.push("the run has no results".into());
    }
    let out = if findings.is_empty() {
        sqlx::query(
            "UPDATE people.payroll_result SET status = 'validated', version = version + 1, \
             updated_at = now() WHERE run_id = $1 AND status = 'calculated'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        validating
    } else {
        let reason: String = format!("validation failed: {}", findings.join("; "))
            .chars()
            .take(1000)
            .collect();
        let failed = move_run(
            &mut tx,
            c,
            &validating,
            PayrollRunStatus::Failed,
            Some(&reason),
            None,
            c.actor,
        )
        .await?;
        run_event(
            &mut tx,
            c,
            &failed,
            PayrollRunStatus::Validating,
            (PAYROLL_RUN_FAILED, PAYROLL_RUN_FAILED_SCHEMA),
            Some(&reason),
            c.actor,
        )
        .await?;
        failed
    };
    audit(
        &mut tx,
        &c.scope(out.institution_id),
        "people.payroll_run.validate",
        RUN,
        id.to_string(),
        json!({"status": out.status.as_str(), "findings": findings}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Submits a validated run for approval (L4-E-053-BH-005): `PAYROLL_APPROVAL`
/// (`OFF_CYCLE_APPROVAL` for off-cycle runs). Pending → `payroll_run.submitted`; auto-approval →
/// `approved` + `payroll_run.approved` in the same transaction.
pub async fn submit_run(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    reason: Option<&str>,
) -> Result<RunRow, KernelError> {
    let run = get_run(c.st, c.tenant, id).await?;
    ensure_version(expected, run.version)?;
    ensure_not_settlement(&run)?;
    if run.status != PayrollRunStatus::Validating {
        return Err(illegal(
            "payroll run",
            &run.number,
            run.status.as_str(),
            "pending_approval",
        ));
    }
    let scope = c.scope(run.institution_id);
    require_institution_writable(c.st, &scope, run.institution_id).await?;
    let key = if run.kind == PayrollRunKind::OffCycle {
        process::OFF_CYCLE_APPROVAL
    } else {
        process::PAYROLL_APPROVAL
    };
    let outcome = request_approval(
        c.st,
        &scope,
        key,
        business_key(RUN, id, run.version),
        BTreeMap::from([
            ("number".to_owned(), json!(run.number)),
            ("kind".to_owned(), json!(run.kind.as_str())),
            ("netPay".to_owned(), json!(fmt2(run.net_pay))),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let reason = opt_reason(reason);
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    let pending = move_run(
        &mut tx,
        c,
        &locked,
        PayrollRunStatus::PendingApproval,
        reason.as_deref(),
        Some(&pid),
        c.actor,
    )
    .await?;
    let out = match outcome {
        ApprovalOutcome::Pending { .. } => {
            run_event(
                &mut tx,
                c,
                &pending,
                PayrollRunStatus::Validating,
                (PAYROLL_RUN_SUBMITTED, PAYROLL_RUN_SUBMITTED_SCHEMA),
                Some(&pending.number),
                c.actor,
            )
            .await?;
            pending
        }
        ApprovalOutcome::Approved { .. } => {
            let auto = c.auto();
            let approved = move_run(
                &mut tx,
                c,
                &pending,
                PayrollRunStatus::Approved,
                Some("auto-approved"),
                Some(&pid),
                &auto,
            )
            .await?;
            run_event(
                &mut tx,
                c,
                &approved,
                PayrollRunStatus::PendingApproval,
                (PAYROLL_RUN_APPROVED, PAYROLL_RUN_APPROVED_SCHEMA),
                Some("auto-approved"),
                &auto,
            )
            .await?;
            approved
        }
    };
    audit(
        &mut tx,
        &scope,
        "people.payroll_run.submit",
        RUN,
        id.to_string(),
        json!({"status": out.status.as_str(), "processKey": key, "processInstanceId": pid}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Decides a pending run (`people.payroll.approve`): approve → `approved`; reject (reason
/// required) → back to `calculated` for correction (L4-E-053 §6).
pub async fn decide_run(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<&str>,
) -> Result<RunRow, KernelError> {
    let reason = if approve {
        opt_reason(reason)
    } else {
        Some(required_reason(reason)?)
    };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let run = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, run.version)?;
    ensure_not_settlement(&run)?;
    if run.status != PayrollRunStatus::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "payroll run {} is {}; only a pending run can be decided",
            run.number, run.status
        )));
    }
    // Part 1 (DEF-X-002): the preparer / submitter of a payroll run never decides it.
    crate::shared::approval::ensure_independent_decider(
        &mut tx,
        c.tenant,
        RUN,
        id,
        &["pending_approval"],
        &[],
        c.actor.id.as_deref(),
        "payroll run",
    )
    .await?;
    let pid = run.process_instance_id.clone();
    let out = if approve {
        let approved = move_run(
            &mut tx,
            c,
            &run,
            PayrollRunStatus::Approved,
            reason.as_deref(),
            pid.as_deref(),
            c.actor,
        )
        .await?;
        run_event(
            &mut tx,
            c,
            &approved,
            PayrollRunStatus::PendingApproval,
            (PAYROLL_RUN_APPROVED, PAYROLL_RUN_APPROVED_SCHEMA),
            reason.as_deref(),
            c.actor,
        )
        .await?;
        approved
    } else {
        sqlx::query(
            "UPDATE people.payroll_result SET status = 'calculated', version = version + 1, \
             updated_at = now() WHERE run_id = $1 AND status = 'validated'",
        )
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        move_run(
            &mut tx,
            c,
            &run,
            PayrollRunStatus::Calculated,
            reason.as_deref(),
            pid.as_deref(),
            c.actor,
        )
        .await?
    };
    audit(
        &mut tx,
        &c.scope(out.institution_id),
        "people.payroll_run.decide",
        RUN,
        id.to_string(),
        json!({"decision": if approve { "approve" } else { "reject" }, "reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Days of encashment entry `entry` recorded in a result snapshot (`inputs.encashments`).
pub(crate) fn encashed_days(snapshot: &Value, entry: Uuid) -> Option<Decimal> {
    let id = entry.to_string();
    snapshot
        .get("inputs")?
        .get("encashments")?
        .as_array()?
        .iter()
        .find(|e| e.get("entryId").and_then(Value::as_str) == Some(id.as_str()))?
        .get("days")?
        .as_str()?
        .parse()
        .ok()
}

/// Finalises the current results of a run (immutable from here on) and records the encashment
/// ledger entries they pay (each entry once — 409 when paid elsewhere meanwhile).
pub(crate) async fn finalize_results(
    conn: &mut PgConnection,
    tenant: TenantId,
    run: &RunRow,
    settlement: Option<Uuid>,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE people.payroll_result SET status = 'finalized', finalized_at = now(), \
         version = version + 1, updated_at = now() WHERE run_id = $1 \
         AND status IN ('calculated', 'validated')",
    )
    .bind(run.id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let rows = sqlx::query(
        "SELECT l.source_ref, l.amount, r.id AS result_id, r.staff_id, r.snapshot \
         FROM people.payroll_result_line l JOIN people.payroll_result r ON r.id = l.result_id \
         WHERE r.run_id = $1 AND r.status = 'finalized' AND l.source = 'encashment' \
         AND l.source_ref IS NOT NULL",
    )
    .bind(run.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut seen = BTreeSet::new();
    for r in &rows {
        let entry: Uuid = r.try_get("source_ref").map_err(map_db_error)?;
        if !seen.insert(entry) {
            continue;
        }
        let snapshot: Value = r.try_get("snapshot").map_err(map_db_error)?;
        let days = encashed_days(&snapshot, entry).ok_or_else(|| {
            KernelError::internal_msg(format!("encashment {entry} is missing from the snapshot"))
        })?;
        let done = sqlx::query(
            "INSERT INTO people.encashment_payment (tenant_id, institution_id, ledger_entry_id, \
             staff_id, run_id, result_id, settlement_id, days, amount) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) ON CONFLICT (ledger_entry_id) DO NOTHING",
        )
        .bind(tenant.as_uuid())
        .bind(run.institution_id.as_uuid())
        .bind(entry)
        .bind(r.try_get::<Uuid, _>("staff_id").map_err(map_db_error)?)
        .bind(run.id)
        .bind(r.try_get::<Uuid, _>("result_id").map_err(map_db_error)?)
        .bind(settlement)
        .bind(days)
        .bind(r.try_get::<Decimal, _>("amount").map_err(map_db_error)?)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if done.rows_affected() == 0 {
            return Err(KernelError::Conflict(format!(
                "leave encashment entry {entry} was paid by another payroll; recalculate the run"
            )));
        }
    }
    Ok(())
}

/// Finalises an approved run (L4-E-053-BH-006): results immutable, `payroll_run.finalized`.
pub async fn finalise_run(c: &Cmd<'_>, id: Uuid, expected: i64) -> Result<RunRow, KernelError> {
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let run = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, run.version)?;
    ensure_not_settlement(&run)?;
    if run.status != PayrollRunStatus::Approved {
        return Err(illegal(
            "payroll run",
            &run.number,
            run.status.as_str(),
            "finalized",
        ));
    }
    finalize_results(&mut tx, c.tenant, &run, None).await?;
    let done = move_run(
        &mut tx,
        c,
        &run,
        PayrollRunStatus::Finalized,
        None,
        None,
        c.actor,
    )
    .await?;
    run_event(
        &mut tx,
        c,
        &done,
        PayrollRunStatus::Approved,
        (PAYROLL_RUN_FINALIZED, PAYROLL_RUN_FINALIZED_SCHEMA),
        None,
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(done.institution_id),
        "people.payroll_run.finalise",
        RUN,
        id.to_string(),
        json!({"headcount": done.headcount, "netPay": fmt2(done.net_pay)}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(done)
}

/// Signed amounts of a posting: `(gross, employee deductions, employer contributions)`.
pub(crate) type PostingAmounts = (Decimal, Decimal, Decimal);

/// The Finance request of a posting (`net = gross − deductions`).
pub(crate) fn posting_request(
    run: &RunRow,
    key: String,
    posting_date: NaiveDate,
    description: String,
    t: PostingAmounts,
    actor: &Actor,
) -> PayrollPostingRequest {
    PayrollPostingRequest {
        institution_id: run.institution_id,
        payroll_run_id: run.id,
        posting_key: key,
        posting_date,
        description,
        gross_earnings: t.0,
        employee_deductions: t.1,
        employer_contributions: t.2,
        net_pay: t.0 - t.1,
        actor: actor.id.clone(),
    }
}

/// `PayrollPostedV1` of a receipt.
pub(crate) fn posted_payload(
    req: &PayrollPostingRequest,
    receipt: &PayrollPostingReceipt,
) -> PayrollPostedV1 {
    PayrollPostedV1 {
        payroll_run_id: req.payroll_run_id,
        institution_id: req.institution_id.as_uuid(),
        journal_entry_id: receipt.journal_entry_id.as_uuid(),
        entry_number: receipt.entry_number.clone(),
        gross_earnings: fmt2(req.gross_earnings),
        employee_deductions: fmt2(req.employee_deductions),
        employer_contributions: fmt2(req.employer_contributions),
        net_pay: fmt2(req.net_pay),
        posting_key: req.posting_key.clone(),
    }
}

/// Posts a finalised run's liability to Finance through `PayrollPostingPort` (`posting_key =
/// finalise`, idempotent): stores the journal entry, `→ posted`, `payroll_run.posted`.
pub async fn post_run(c: &Cmd<'_>, id: Uuid, expected: i64) -> Result<RunRow, KernelError> {
    let run = get_run(c.st, c.tenant, id).await?;
    ensure_version(expected, run.version)?;
    ensure_not_settlement(&run)?;
    if run.status != PayrollRunStatus::Finalized {
        return Err(illegal(
            "payroll run",
            &run.number,
            run.status.as_str(),
            "posted",
        ));
    }
    let scope = c.scope(run.institution_id);
    let req = posting_request(
        &run,
        "finalise".into(),
        run.period_end,
        format!(
            "Payroll {} ({}) {}",
            run.number,
            run.kind.as_str().replace('_', "-"),
            year_month(run.period_start)
        ),
        (
            run.gross_earnings,
            run.employee_deductions,
            run.employer_contributions,
        ),
        c.actor,
    );
    let receipt = c.st.postings.post_payroll(&scope, req.clone()).await?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    sqlx::query(
        "UPDATE people.payroll_run SET journal_entry_id = $2, journal_entry_number = $3 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(receipt.journal_entry_id.as_uuid())
    .bind(&receipt.entry_number)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let locked = load_run(&mut tx, c.tenant, id, false).await?;
    let done = move_run(
        &mut tx,
        c,
        &locked,
        PayrollRunStatus::Posted,
        None,
        None,
        c.actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        PAYROLL_RUN_POSTED,
        PAYROLL_RUN_POSTED_SCHEMA,
        id.to_string(),
        &posted_payload(&req, &receipt),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.payroll_run.post",
        RUN,
        id.to_string(),
        json!({"journalEntryId": receipt.journal_entry_id.as_uuid(),
               "entryNumber": receipt.entry_number, "alreadyPosted": receipt.already_posted}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(done)
}

/// Closes a posted run.
pub async fn close_run(c: &Cmd<'_>, id: Uuid, expected: i64) -> Result<RunRow, KernelError> {
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let run = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, run.version)?;
    let done = move_run(
        &mut tx,
        c,
        &run,
        PayrollRunStatus::Closed,
        None,
        None,
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(done.institution_id),
        "people.payroll_run.close",
        RUN,
        id.to_string(),
        json!({}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(done)
}

/// Reopens a failed run for recalculation (`failed → reprocessing`).
pub async fn reprocess_run(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    reason: Option<&str>,
) -> Result<RunRow, KernelError> {
    let reason = opt_reason(reason);
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let run = load_run(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, run.version)?;
    ensure_not_settlement(&run)?;
    let done = move_run(
        &mut tx,
        c,
        &run,
        PayrollRunStatus::Reprocessing,
        reason.as_deref(),
        None,
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(done.institution_id),
        "people.payroll_run.reprocess",
        RUN,
        id.to_string(),
        json!({"reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(done)
}

// ------------------------------------------------------------------ results (reads)

/// Results of a run by staff number (`limit` ≤ 200, `cursor` = staff number to continue after).
pub async fn list_results(
    st: &PeopleState,
    tenant: TenantId,
    run: Uuid,
    include_superseded: bool,
    limit: i64,
    cursor: Option<&str>,
) -> Result<(Vec<ResultRow>, Option<String>), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_run(&mut conn, tenant, run, false).await?;
    let sql = format!(
        "SELECT {RESULT_COLUMNS} FROM people.payroll_result WHERE run_id = $1 AND tenant_id = $2 \
         AND ($3 OR status <> 'superseded') AND ($4::text IS NULL OR staff_number > $4) \
         ORDER BY staff_number, revision LIMIT $5"
    );
    let rows = sqlx::query(&sql)
        .bind(run)
        .bind(tenant.as_uuid())
        .bind(include_superseded)
        .bind(cursor)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut r = result_from_row(row).map_err(map_db_error)?;
        r.lines = lines_of(&mut conn, r.id).await?;
        out.push(r);
    }
    let cap = usize::try_from(limit).unwrap_or(50);
    let next = if out.len() > cap {
        out.truncate(cap);
        out.last().map(|r| r.staff_number.clone())
    } else {
        None
    };
    Ok((out, next))
}

pub async fn get_result(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ResultRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_result(&mut conn, tenant, id, false).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_cycle_items_round_trip_through_json() {
        let items = vec![OffCycleItem {
            engagement_id: Uuid::nil(),
            component_code: "BONUS".into(),
            amount: Decimal::new(500000, 2),
            note: Some("festival".into()),
        }];
        let v = items_json(&items);
        assert_eq!(v[0]["amount"], "5000.00");
        assert_eq!(items_from_json(&v), items);
        assert!(items_from_json(&json!({})).is_empty());
        let e = Uuid::now_v7();
        let snap = json!({"inputs": {"encashments": [{"entryId": e, "days": "3.00"}]}});
        assert_eq!(encashed_days(&snap, e), Some(Decimal::new(300, 2)));
        assert_eq!(encashed_days(&snap, Uuid::nil()), None);
    }
}
