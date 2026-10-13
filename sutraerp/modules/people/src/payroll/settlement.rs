//! Separation final settlement (L3-E-056): for an engagement closed by 7A with a last working
//! date — pro-rata salary of the final month (unless the regular run already paid it), leave
//! encashment of every unpaid 7B `encashment` entry, recoveries — computed as a
//! `final_settlement` payroll run, approved through `FINAL_SETTLEMENT_APPROVAL`, finalised,
//! posted to Finance (`posting_key = settlement:<id>`) and numbered `FNF-…`.

use super::common::{
    cause, ensure_version, history, illegal, invalid, opt_reason, payload, required_reason, Change,
    Cmd,
};
use super::config::{ensure_period, valid_code};
use super::domain::{
    days_between, first_unpaid_day, month_bounds, settlement_can_transition, totals,
    uncovered_days, CalcLine, FinalSettlementStatus, LineSource,
};
use super::rows::*;
use super::runs::{
    calc_employee, delete_open_results, finalize_results, get_run, insert_result, insert_run,
    load_active, move_run, posted_payload, posting_request, run_payload, store_totals,
    unpaid_encashments, EmployeeCalc,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::fmt2;
use crate::shared::guard::require_institution_writable;
use crate::shared::numbering::{self, kind};
use crate::shared::publish::{audit, event};
use crate::shared::rows::load_engagement;
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use chrono::Datelike;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::people::events::payroll::{
    FINAL_SETTLEMENT_COMPLETED, FINAL_SETTLEMENT_COMPLETED_SCHEMA, PAYROLL_RUN_FINALIZED,
    PAYROLL_RUN_FINALIZED_SCHEMA, PAYROLL_RUN_POSTED, PAYROLL_RUN_POSTED_SCHEMA,
};
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::status::{
    EngagementStatus, PayComponentKind, PayrollRunKind, PayrollRunStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const FNF: &str = "final_settlement";

/// One recovery (notice-period shortfall, advance, asset) deducted from the settlement.
#[derive(Clone, Debug)]
pub struct Recovery {
    pub code: String,
    pub name: String,
    pub amount: Decimal,
}

fn recoveries_json(r: &[Recovery]) -> Value {
    Value::Array(
        r.iter()
            .map(|x| json!({"code": x.code, "name": x.name, "amount": fmt2(x.amount)}))
            .collect(),
    )
}

fn recoveries_from(v: &Value) -> Vec<Recovery> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Recovery {
                        code: x.get("code")?.as_str()?.to_owned(),
                        name: x.get("name")?.as_str()?.to_owned(),
                        amount: x.get("amount")?.as_str()?.parse().ok()?,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn label(s: &SettlementRow) -> &str {
    s.number.as_deref().unwrap_or("(unnumbered)")
}

pub async fn get_settlement(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<SettlementRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_settlement(&mut conn, tenant, id, false).await
}

pub async fn settlement_of_engagement(
    st: &PeopleState,
    tenant: TenantId,
    engagement: Uuid,
) -> Result<SettlementRow, KernelError> {
    let sql = format!(
        "SELECT {SETTLEMENT_COLUMNS} FROM people.final_settlement WHERE tenant_id = $1 \
         AND engagement_id = $2"
    );
    let row = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(engagement)
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "final settlement",
        })?;
    settlement_from_row(&row).map_err(map_db_error)
}

pub async fn list_settlements(
    st: &PeopleState,
    tenant: TenantId,
    institution: InstitutionId,
) -> Result<Vec<SettlementRow>, KernelError> {
    let sql = format!(
        "SELECT {SETTLEMENT_COLUMNS} FROM people.final_settlement WHERE tenant_id = $1 \
         AND institution_id = $2 ORDER BY created_at DESC"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.as_uuid())
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(settlement_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

fn check_recoveries(r: &[Recovery]) -> Result<(), KernelError> {
    for x in r {
        if !valid_code(&x.code) {
            return Err(invalid(
                "recoveries",
                "invalid_code",
                format!("{} is not a valid code", x.code),
            ));
        }
        if x.name.trim().is_empty() {
            return Err(invalid(
                "recoveries",
                "required",
                "each recovery needs a name",
            ));
        }
        if x.amount <= Decimal::ZERO {
            return Err(invalid(
                "recoveries",
                "not_positive",
                "recovery amounts are positive",
            ));
        }
    }
    Ok(())
}

/// Initiates the final settlement of an engagement closed with a last working date (one per
/// engagement → 409).
pub async fn create_settlement(
    c: &Cmd<'_>,
    engagement: Uuid,
    recoveries: Vec<Recovery>,
    notes: Option<&str>,
) -> Result<SettlementRow, KernelError> {
    check_recoveries(&recoveries)?;
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let e = load_engagement(&mut tx, c.tenant, StaffEngagementId(engagement))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    let scope = c.scope(e.institution_id);
    require_institution_writable(c.st, &scope, e.institution_id).await?;
    let lwd = match (e.status, e.last_working_date) {
        (EngagementStatus::Closed, Some(d)) => d,
        _ => {
            return Err(KernelError::Conflict(format!(
                "engagement {} is {}; a final settlement needs an engagement closed with a last \
                 working date",
                e.engagement_number, e.status
            )));
        }
    };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.final_settlement (tenant_id, institution_id, staff_id, engagement_id, \
         last_working_date, recoveries, notes) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(c.tenant.as_uuid())
    .bind(e.institution_id.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(engagement)
    .bind(lwd)
    .bind(recoveries_json(&recoveries))
    .bind(opt_reason(notes))
    .fetch_one(&mut *tx)
    .await
    .map_err(|err| match map_db_error(err) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("the engagement already has a final settlement".into())
        }
        other => other,
    })?;
    history(
        &mut tx,
        c.tenant,
        e.institution_id,
        FNF,
        id,
        None,
        FinalSettlementStatus::Initiated.as_str(),
        &cause("people.final_settlement.created", None, None),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.final_settlement.create",
        FNF,
        id.to_string(),
        json!({"engagementId": engagement, "lastWorkingDate": lwd,
               "recoveries": recoveries.len()}),
        c.actor,
    )
    .await?;
    let out = load_settlement(&mut tx, c.tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

async fn move_settlement(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    s: &SettlementRow,
    to: FinalSettlementStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<SettlementRow, KernelError> {
    if !settlement_can_transition(s.status, to) {
        return Err(illegal(
            "final settlement",
            label(s),
            s.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE people.final_settlement SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), \
         completed_at = CASE WHEN $2 = 'completed' THEN now() ELSE completed_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(s.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        c.tenant,
        s.institution_id,
        FNF,
        s.id,
        Some(s.status.as_str()),
        to.as_str(),
        &cause(
            "people.final_settlement.status",
            reason,
            process_instance_id,
        ),
        actor,
    )
    .await?;
    load_settlement(conn, c.tenant, s.id, false).await
}

/// Walks a locked run through `targets` in order.
async fn walk(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    run: RunRow,
    targets: &[PayrollRunStatus],
    pid: Option<&str>,
    actor: &Actor,
) -> Result<RunRow, KernelError> {
    let mut run = run;
    for t in targets {
        run = move_run(conn, c, &run, *t, None, pid, actor).await?;
    }
    Ok(run)
}

/// Calculates the settlement (L4-E-056-BH-002/003) as its `final_settlement` run and submits it
/// for approval (`FINAL_SETTLEMENT_APPROVAL`; auto-approval → `approved`). Allowed while
/// `initiated` or `rejected`.
pub async fn calculate_settlement(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
) -> Result<SettlementRow, KernelError> {
    let s = get_settlement(c.st, c.tenant, id).await?;
    ensure_version(expected, s.version)?;
    if !matches!(
        s.status,
        FinalSettlementStatus::Initiated | FinalSettlementStatus::Rejected
    ) {
        return Err(illegal(
            "final settlement",
            label(&s),
            s.status.as_str(),
            "calculated",
        ));
    }
    let scope = c.scope(s.institution_id);
    require_institution_writable(c.st, &scope, s.institution_id).await?;
    let e =
        c.st.directory
            .engagement(&scope, StaffEngagementId(s.engagement_id))
            .await?
            .ok_or(KernelError::NotFound {
                entity: "engagement",
            })?;
    let lwd = s.last_working_date;
    let (start, end) = month_bounds(lwd.year(), lwd.month())
        .ok_or_else(|| KernelError::internal_msg("invalid last working date"))?;
    let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
    let cfg = load_active(&mut conn, c.tenant, s.institution_id)
        .await?
        .ok_or_else(|| {
            KernelError::Conflict("no active payroll configuration for the institution".into())
        })?;
    // No double pay (L4-E-056): the final month's salary is paid here only for the days no
    // finalised regular result of this engagement already paid. A regular result still open
    // (calculated / validated) for the month must be finalised or recalculated first.
    let open_regular: Option<String> = sqlx::query_scalar(
        "SELECT u.number FROM people.payroll_result r \
         JOIN people.payroll_run u ON u.id = r.run_id \
         WHERE r.engagement_id = $1 AND u.kind = 'regular' \
         AND r.status IN ('calculated', 'validated') \
         AND r.period_start <= $3 AND r.period_end >= $2 LIMIT 1",
    )
    .bind(s.engagement_id)
    .bind(start)
    .bind(end)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .flatten();
    if let Some(run_no) = open_regular {
        return Err(KernelError::Conflict(format!(
            "the regular payroll run {run_no} still holds an open result for this engagement in \
             the final month; finalise or recalculate it before calculating the settlement"
        )));
    }
    let paid_rows: Vec<(chrono::NaiveDate, chrono::NaiveDate)> = sqlx::query_as(
        "SELECT r.period_start, r.period_end FROM people.payroll_result r \
         JOIN people.payroll_run u ON u.id = r.run_id \
         WHERE r.engagement_id = $1 AND r.status = 'finalized' AND u.kind = 'regular' \
         AND r.period_start <= $3 AND r.period_end >= $2",
    )
    .bind(s.engagement_id)
    .bind(start)
    .bind(end)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let window_from = start.max(e.start_date);
    let window_to = end.min(lwd);
    let unpaid_days = uncovered_days(window_from, window_to, &paid_rows);
    let first_unpaid = first_unpaid_day(window_from, window_to, &paid_rows);
    let regular_paid = unpaid_days == 0 || first_unpaid.is_none();
    let salary_from = first_unpaid.unwrap_or(start);
    let already_paid_days = if regular_paid {
        0
    } else {
        (days_between(salary_from, window_to) - unpaid_days).max(0)
    };
    let today = business_today(c.st, &scope, s.institution_id).await?;
    let inputs =
        c.st.leave
            .payroll_inputs(&scope, e.staff_id, e.start_date, lwd.max(today))
            .await?;
    let encashments = unpaid_encashments(&mut conn, &inputs.encashments).await?;
    let outcome = calc_employee(
        c,
        &mut conn,
        &cfg,
        EmployeeCalc {
            engagement: &e,
            from: salary_from,
            to: end,
            period_days: days_between(start, end),
            pay_salary: !regular_paid,
            already_paid_days,
            encashments: encashments.clone(),
        },
    )
    .await?;
    drop(conn);
    let mut nr = match outcome {
        Ok(r) => r,
        Err((code, msg)) => return Err(invalid("engagementId", &code, msg)),
    };
    for r in &recoveries_from(&s.recoveries) {
        nr.out.lines.push(
            CalcLine::new(&r.code, &r.name, PayComponentKind::Deduction, r.amount)
                .source(LineSource::Recovery, Some(id)),
        );
    }
    nr.out.totals = totals(&nr.out.lines);
    if nr.out.totals.net < Decimal::ZERO {
        return Err(KernelError::Conflict(format!(
            "the settlement nets {}; recoveries exceed what is payable",
            fmt2(nr.out.totals.net)
        )));
    }
    if let Some(obj) = nr.snapshot.as_object_mut() {
        obj.insert("finalSettlementId".into(), json!(id));
        obj.insert("regularRunPaidFinalMonth".into(), json!(regular_paid));
        obj.insert("unpaidSalaryDays".into(), json!(unpaid_days));
        obj.insert(
            "salaryPaidFrom".into(),
            json!(first_unpaid.filter(|_| !regular_paid)),
        );
    }
    let sum_of = |src: LineSource, k: PayComponentKind| -> Decimal {
        nr.out
            .lines
            .iter()
            .filter(|l| l.source == src && l.kind == k)
            .map(|l| l.amount)
            .sum()
    };
    let salary = sum_of(LineSource::Calculation, PayComponentKind::Earning);
    let encashed = sum_of(LineSource::Encashment, PayComponentKind::Earning);
    let recovered = sum_of(LineSource::Recovery, PayComponentKind::Deduction);
    let encashed_days: Decimal = encashments.iter().map(|(_, d)| *d).sum();
    let net = nr.out.totals.net;
    let approval = request_approval(
        c.st,
        &scope,
        process::FINAL_SETTLEMENT_APPROVAL,
        business_key(FNF, id, s.version),
        BTreeMap::from([("netPay".to_owned(), json!(fmt2(net)))]),
    )
    .await?;
    let pid = approval.process_instance_id().to_owned();

    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_settlement(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    let (period, _) =
        ensure_period(&mut tx, c.tenant, s.institution_id, lwd.year(), lwd.month()).await?;
    let run_id = match locked.run_id {
        Some(r) => r,
        None => {
            insert_run(
                &mut tx,
                c,
                &period,
                PayrollRunKind::FinalSettlement,
                Some(format!("Final settlement of {}", nr.staff_number)),
                Value::Array(vec![]),
            )
            .await?
        }
    };
    let run = load_run(&mut tx, c.tenant, run_id, true).await?;
    let run = walk(
        &mut tx,
        c,
        run,
        &[PayrollRunStatus::Calculating],
        None,
        c.actor,
    )
    .await?;
    delete_open_results(&mut tx, run_id).await?;
    insert_result(&mut tx, c.tenant, &run, &nr, Some(cfg.config.id)).await?;
    store_totals(&mut tx, run_id, Some(cfg.config.id), &[]).await?;
    let run = load_run(&mut tx, c.tenant, run_id, false).await?;
    let run = walk(
        &mut tx,
        c,
        run,
        &[
            PayrollRunStatus::Calculated,
            PayrollRunStatus::Validating,
            PayrollRunStatus::PendingApproval,
        ],
        Some(&pid),
        c.actor,
    )
    .await?;
    sqlx::query(
        "UPDATE people.final_settlement SET run_id = $2, salary_amount = $3, \
         encashment_amount = $4, encashment_days = $5, recovery_amount = $6, net_pay = $7 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(run_id)
    .bind(salary)
    .bind(encashed)
    .bind(encashed_days)
    .bind(recovered)
    .bind(net)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let cur = load_settlement(&mut tx, c.tenant, id, false).await?;
    let cur = move_settlement(
        &mut tx,
        c,
        &cur,
        FinalSettlementStatus::Calculated,
        None,
        None,
        c.actor,
    )
    .await?;
    let mut cur = move_settlement(
        &mut tx,
        c,
        &cur,
        FinalSettlementStatus::PendingApproval,
        None,
        Some(&pid),
        c.actor,
    )
    .await?;
    if matches!(approval, ApprovalOutcome::Approved { .. }) {
        let auto = c.auto();
        walk(
            &mut tx,
            c,
            run,
            &[PayrollRunStatus::Approved],
            Some(&pid),
            &auto,
        )
        .await?;
        cur = move_settlement(
            &mut tx,
            c,
            &cur,
            FinalSettlementStatus::Approved,
            Some("auto-approved"),
            Some(&pid),
            &auto,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.final_settlement.calculate",
        FNF,
        id.to_string(),
        json!({"runId": run_id, "salary": fmt2(salary), "encashment": fmt2(encashed),
               "recoveries": fmt2(recovered), "netPay": fmt2(net),
               "status": cur.status.as_str()}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(cur)
}

/// Decides a pending settlement (`people.payroll.approve`): approve → run + settlement
/// `approved`; reject (reason required) → run back to `calculated`, settlement `rejected`
/// (recalculate after correcting).
pub async fn decide_settlement(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<&str>,
) -> Result<SettlementRow, KernelError> {
    let reason = if approve {
        opt_reason(reason)
    } else {
        Some(required_reason(reason)?)
    };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let s = load_settlement(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, s.version)?;
    if s.status != FinalSettlementStatus::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "final settlement is {}; only a pending settlement can be decided",
            s.status
        )));
    }
    let run_id = s
        .run_id
        .ok_or_else(|| KernelError::internal_msg("pending settlement without a run"))?;
    let run = load_run(&mut tx, c.tenant, run_id, true).await?;
    let pid = s.process_instance_id.clone();
    let (run_to, to) = if approve {
        (PayrollRunStatus::Approved, FinalSettlementStatus::Approved)
    } else {
        (
            PayrollRunStatus::Calculated,
            FinalSettlementStatus::Rejected,
        )
    };
    move_run(
        &mut tx,
        c,
        &run,
        run_to,
        reason.as_deref(),
        pid.as_deref(),
        c.actor,
    )
    .await?;
    let out = move_settlement(
        &mut tx,
        c,
        &s,
        to,
        reason.as_deref(),
        pid.as_deref(),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(s.institution_id),
        "people.final_settlement.decide",
        FNF,
        id.to_string(),
        json!({"decision": if approve { "approve" } else { "reject" }, "reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Completes an approved settlement (L4-E-056-BH-005/006): posts it to Finance
/// (`settlement:<id>`), finalises its run (encashment payments recorded), numbers it `FNF-…`,
/// `final_settlement.completed`.
pub async fn complete_settlement(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
) -> Result<SettlementRow, KernelError> {
    let s = get_settlement(c.st, c.tenant, id).await?;
    ensure_version(expected, s.version)?;
    if s.status != FinalSettlementStatus::Approved {
        return Err(illegal(
            "final settlement",
            label(&s),
            s.status.as_str(),
            "completed",
        ));
    }
    let run_id = s
        .run_id
        .ok_or_else(|| KernelError::internal_msg("approved settlement without a run"))?;
    let run = get_run(c.st, c.tenant, run_id).await?;
    let scope = c.scope(s.institution_id);
    require_institution_writable(c.st, &scope, s.institution_id).await?;
    let staff =
        c.st.directory
            .staff(&scope, StaffMemberId(s.staff_id))
            .await?;
    let posting = if run.gross_earnings.is_zero()
        && run.employee_deductions.is_zero()
        && run.employer_contributions.is_zero()
    {
        None
    } else {
        let today = business_today(c.st, &scope, s.institution_id).await?;
        let req = posting_request(
            &run,
            format!("settlement:{id}"),
            today,
            format!(
                "Final settlement {} ({})",
                staff.as_ref().map_or("", |x| x.staff_number.as_str()),
                run.number
            ),
            (
                run.gross_earnings,
                run.employee_deductions,
                run.employer_contributions,
            ),
            c.actor,
        );
        let receipt = c.st.postings.post_payroll(&scope, req.clone()).await?;
        Some((req, receipt))
    };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_settlement(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    let run = load_run(&mut tx, c.tenant, run_id, true).await?;
    finalize_results(&mut tx, c.tenant, &run, Some(id)).await?;
    let finalized = move_run(
        &mut tx,
        c,
        &run,
        PayrollRunStatus::Finalized,
        None,
        None,
        c.actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        PAYROLL_RUN_FINALIZED,
        PAYROLL_RUN_FINALIZED_SCHEMA,
        run_id.to_string(),
        &run_payload(&finalized, PayrollRunStatus::Approved, None),
        c.actor,
    )
    .await?;
    if let Some((req, receipt)) = &posting {
        sqlx::query(
            "UPDATE people.payroll_run SET journal_entry_id = $2, journal_entry_number = $3 \
             WHERE id = $1",
        )
        .bind(run_id)
        .bind(receipt.journal_entry_id.as_uuid())
        .bind(&receipt.entry_number)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let reloaded = load_run(&mut tx, c.tenant, run_id, false).await?;
        move_run(
            &mut tx,
            c,
            &reloaded,
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
            run_id.to_string(),
            &posted_payload(req, receipt),
            c.actor,
        )
        .await?;
    }
    let number = numbering::next(&mut tx, c.tenant, s.institution_id, kind::SETTLEMENT).await?;
    sqlx::query(
        "UPDATE people.final_settlement SET number = $2, journal_entry_id = $3, \
         journal_entry_number = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(&number)
    .bind(posting.as_ref().map(|(_, r)| r.journal_entry_id.as_uuid()))
    .bind(posting.as_ref().map(|(_, r)| r.entry_number.clone()))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let locked = load_settlement(&mut tx, c.tenant, id, false).await?;
    let out = move_settlement(
        &mut tx,
        c,
        &locked,
        FinalSettlementStatus::Completed,
        None,
        None,
        c.actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        FINAL_SETTLEMENT_COMPLETED,
        FINAL_SETTLEMENT_COMPLETED_SCHEMA,
        id.to_string(),
        &payload(&Change {
            aggregate_id: id,
            institution: Some(s.institution_id),
            staff_id: Some(s.staff_id),
            engagement_id: Some(s.engagement_id),
            from: Some(FinalSettlementStatus::Approved.as_str()),
            to: out.status.as_str(),
            version: out.version,
            reason: Some(&number),
            process_instance_id: out.process_instance_id.as_deref(),
            staff_subject_ref: staff.as_ref().and_then(|x| x.subject_ref.as_deref()),
        }),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.final_settlement.complete",
        FNF,
        id.to_string(),
        json!({"number": number, "runId": run_id, "netPay": fmt2(out.net_pay),
               "journalEntryId": out.journal_entry_id}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recoveries_round_trip_and_validate() {
        let r = vec![Recovery {
            code: "NOTICE".into(),
            name: "Notice-period shortfall".into(),
            amount: Decimal::new(500000, 2),
        }];
        assert!(check_recoveries(&r).is_ok());
        let back = recoveries_from(&recoveries_json(&r));
        assert_eq!(back.len(), 1);
        assert_eq!(back[0].amount, Decimal::new(5000, 0));
        let bad = vec![Recovery {
            amount: Decimal::ZERO,
            ..r[0].clone()
        }];
        assert!(check_recoveries(&bad).is_err());
    }
}
