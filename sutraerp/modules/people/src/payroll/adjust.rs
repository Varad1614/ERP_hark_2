//! Payroll adjustments (L3-E-054): a corrective delta on a finalised result — requested →
//! validated → pending_approval (`PAYROLL_ADJUSTMENT_APPROVAL`) → approved → calculated → applied.
//! Applying never edits the finalised result: it supersedes it with a new revision (the old lines
//! plus `adjustment` delta lines) and posts the delta to Finance (`posting_key =
//! adjustment:<id>`, signed amounts allowed).

use super::common::{
    cause, ensure_version, history, illegal, invalid, opt_reason, payload, required_reason, Change,
    Cmd,
};
use super::domain::{
    adjustment_can_transition, run_is_final, totals, CalcLine, LineSource, LEAVE_ENCASHMENT,
};
use super::rows::*;
use super::runs::{get_result, get_run, insert_lines, posting_request};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::fmt2;
use crate::shared::guard::require_institution_writable;
use crate::shared::publish::{audit, event};
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::people::events::payroll::{
    PAYROLL_ADJUSTMENT_APPLIED, PAYROLL_ADJUSTMENT_APPLIED_SCHEMA,
};
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::status::{
    PayComponentKind, PayrollAdjustmentStatus, PayrollResultStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use uuid::Uuid;

const ADJ: &str = "payroll_adjustment";

/// One requested delta line.
#[derive(Clone, Debug)]
pub struct AdjustmentLineInput {
    pub code: String,
    /// Required when the code is not a component of the result's configuration.
    pub kind: Option<PayComponentKind>,
    pub name: Option<String>,
    /// Signed delta.
    pub amount: Decimal,
}

/// Requested lines stored on the adjustment.
pub fn stored_lines(v: &Value) -> Vec<CalcLine> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| {
                    let kind = PayComponentKind::parse(l.get("kind")?.as_str()?)?;
                    let amount: Decimal = l.get("amount")?.as_str()?.parse().ok()?;
                    Some(CalcLine::new(
                        l.get("code")?.as_str()?,
                        l.get("name")?.as_str()?,
                        kind,
                        amount,
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

pub async fn list_adjustments(
    st: &PeopleState,
    tenant: TenantId,
    result: Uuid,
) -> Result<Vec<AdjustmentRow>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_result(&mut conn, tenant, result, false).await?;
    let sql = format!(
        "SELECT {ADJUSTMENT_COLUMNS} FROM people.payroll_adjustment WHERE tenant_id = $1 \
         AND staff_id = $2 AND run_id = $3 ORDER BY created_at"
    );
    sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(r.staff_id)
        .bind(r.run_id)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(adjustment_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

pub async fn get_adjustment(
    st: &PeopleState,
    tenant: TenantId,
    id: Uuid,
) -> Result<AdjustmentRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_adjustment(&mut conn, tenant, id, false).await
}

/// Opens an adjustment case on a finalised result (L4-E-054-BH-001/002).
pub async fn create_adjustment(
    c: &Cmd<'_>,
    result: Uuid,
    reason: Option<&str>,
    lines: Vec<AdjustmentLineInput>,
) -> Result<AdjustmentRow, KernelError> {
    let reason = required_reason(reason)?;
    if lines.is_empty() {
        return Err(invalid("lines", "required", "at least one delta line"));
    }
    let mut conn = c.st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_result(&mut conn, c.tenant, result, false).await?;
    if r.status != PayrollResultStatus::Finalized {
        return Err(KernelError::Conflict(format!(
            "payroll result is {}; only the current finalised result can be adjusted",
            r.status
        )));
    }
    let run = load_run(&mut conn, c.tenant, r.run_id, false).await?;
    if !run_is_final(run.status) {
        return Err(KernelError::Conflict(format!(
            "payroll run {} is {}",
            run.number, run.status
        )));
    }
    let scope = c.scope(r.institution_id);
    require_institution_writable(c.st, &scope, r.institution_id).await?;
    let components = match r.config_id {
        Some(cfg) => components_of(&mut conn, cfg).await?,
        None => Vec::new(),
    };
    drop(conn);
    let mut calc = Vec::with_capacity(lines.len());
    for l in &lines {
        let code = l.code.trim();
        if !super::config::valid_code(code) || code == LEAVE_ENCASHMENT {
            return Err(invalid(
                "lines",
                "invalid_code",
                format!("{code} cannot be adjusted"),
            ));
        }
        if l.amount.is_zero() {
            return Err(invalid("lines", "zero", "delta amounts must not be zero"));
        }
        let comp = components.iter().find(|x| x.def.code == code);
        if let (Some(x), Some(k)) = (comp, l.kind) {
            if x.def.kind != k {
                return Err(invalid(
                    "lines",
                    "kind_mismatch",
                    format!("{code} is a configured {}", x.def.kind.as_str()),
                ));
            }
        }
        let kind = comp.map(|x| x.def.kind).or(l.kind).ok_or_else(|| {
            invalid(
                "lines",
                "kind_required",
                format!("{code} is not a configured component; give its kind"),
            )
        })?;
        let name = l
            .name
            .as_deref()
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_owned)
            .or_else(|| comp.map(|x| x.def.name.clone()))
            .unwrap_or_else(|| code.to_owned());
        calc.push(CalcLine::new(code, &name, kind, l.amount));
    }
    let t = totals(&calc);
    let stored = Value::Array(
        calc.iter()
            .map(|l| {
                json!({"code": l.code, "name": l.name, "kind": l.kind.as_str(),
                       "amount": fmt2(l.amount)})
            })
            .collect(),
    );
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.payroll_adjustment (tenant_id, institution_id, result_id, run_id, \
         staff_id, engagement_id, reason, lines, gross_delta, deduction_delta, employer_delta, \
         net_delta) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(c.tenant.as_uuid())
    .bind(r.institution_id.as_uuid())
    .bind(r.id)
    .bind(r.run_id)
    .bind(r.staff_id)
    .bind(r.engagement_id)
    .bind(&reason)
    .bind(stored)
    .bind(t.gross)
    .bind(t.deductions)
    .bind(t.employer)
    .bind(t.net)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        c.tenant,
        r.institution_id,
        ADJ,
        id,
        None,
        PayrollAdjustmentStatus::Requested.as_str(),
        &cause("people.payroll_adjustment.created", Some(&reason), None),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.payroll_adjustment.create",
        ADJ,
        id.to_string(),
        json!({"resultId": r.id, "netDelta": fmt2(t.net), "reason": reason}),
        c.actor,
    )
    .await?;
    let out = load_adjustment(&mut tx, c.tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

async fn move_adjustment(
    conn: &mut PgConnection,
    c: &Cmd<'_>,
    a: &AdjustmentRow,
    to: PayrollAdjustmentStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    if !adjustment_can_transition(a.status, to) {
        return Err(illegal(
            "payroll adjustment",
            &a.id.to_string(),
            a.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE people.payroll_adjustment SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), \
         applied_at = CASE WHEN $2 = 'applied' THEN now() ELSE applied_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(a.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        c.tenant,
        a.institution_id,
        ADJ,
        a.id,
        Some(a.status.as_str()),
        to.as_str(),
        &cause(
            "people.payroll_adjustment.status",
            reason,
            process_instance_id,
        ),
        actor,
    )
    .await?;
    load_adjustment(conn, c.tenant, a.id, false).await
}

/// Validates the adjustment against the current result (still the finalised revision, net pay
/// stays ≥ 0) and submits it for approval (`PAYROLL_ADJUSTMENT_APPROVAL`; auto-approval →
/// `approved` in the same transaction).
pub async fn validate_adjustment(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
) -> Result<AdjustmentRow, KernelError> {
    let a = get_adjustment(c.st, c.tenant, id).await?;
    ensure_version(expected, a.version)?;
    if a.status != PayrollAdjustmentStatus::Requested {
        return Err(illegal(
            "payroll adjustment",
            &id.to_string(),
            a.status.as_str(),
            "validated",
        ));
    }
    let r = get_result(c.st, c.tenant, a.result_id).await?;
    if r.status != PayrollResultStatus::Finalized {
        return Err(KernelError::Conflict(
            "the adjusted result was superseded meanwhile; raise the adjustment on the current \
             revision"
                .into(),
        ));
    }
    if r.net_pay + a.net_delta < Decimal::ZERO {
        return Err(invalid(
            "lines",
            "negative_net_pay",
            format!("net pay would become {}", fmt2(r.net_pay + a.net_delta)),
        ));
    }
    let scope = c.scope(a.institution_id);
    let outcome = request_approval(
        c.st,
        &scope,
        process::PAYROLL_ADJUSTMENT_APPROVAL,
        business_key(ADJ, id, a.version),
        BTreeMap::from([("netDelta".to_owned(), json!(fmt2(a.net_delta)))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_adjustment(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    let validated = move_adjustment(
        &mut tx,
        c,
        &locked,
        PayrollAdjustmentStatus::Validated,
        None,
        None,
        c.actor,
    )
    .await?;
    let mut out = move_adjustment(
        &mut tx,
        c,
        &validated,
        PayrollAdjustmentStatus::PendingApproval,
        None,
        Some(&pid),
        c.actor,
    )
    .await?;
    if matches!(outcome, ApprovalOutcome::Approved { .. }) {
        out = move_adjustment(
            &mut tx,
            c,
            &out,
            PayrollAdjustmentStatus::Approved,
            Some("auto-approved"),
            Some(&pid),
            &c.auto(),
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.payroll_adjustment.validate",
        ADJ,
        id.to_string(),
        json!({"status": out.status.as_str(), "processInstanceId": pid}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Decides a pending adjustment (`people.payroll.approve`); rejection needs a reason and keeps
/// the finalised result untouched.
pub async fn decide_adjustment(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<&str>,
) -> Result<AdjustmentRow, KernelError> {
    let reason = if approve {
        opt_reason(reason)
    } else {
        Some(required_reason(reason)?)
    };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let a = load_adjustment(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, a.version)?;
    if a.status != PayrollAdjustmentStatus::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "payroll adjustment is {}; only a pending adjustment can be decided",
            a.status
        )));
    }
    let to = if approve {
        PayrollAdjustmentStatus::Approved
    } else {
        PayrollAdjustmentStatus::Rejected
    };
    let pid = a.process_instance_id.clone();
    let out = move_adjustment(
        &mut tx,
        c,
        &a,
        to,
        reason.as_deref(),
        pid.as_deref(),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope(a.institution_id),
        "people.payroll_adjustment.decide",
        ADJ,
        id.to_string(),
        json!({"decision": if approve { "approve" } else { "reject" }, "reason": reason}),
        c.actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Applies an approved adjustment (L4-E-054-BH-004/005/006): posts the delta to Finance, then in
/// one transaction supersedes the finalised result with a corrected revision (old lines + the
/// `adjustment` lines), `approved → calculated → applied`, `payroll_adjustment.applied`.
pub async fn apply_adjustment(
    c: &Cmd<'_>,
    id: Uuid,
    expected: i64,
) -> Result<AdjustmentRow, KernelError> {
    let a = get_adjustment(c.st, c.tenant, id).await?;
    ensure_version(expected, a.version)?;
    if a.status != PayrollAdjustmentStatus::Approved {
        return Err(illegal(
            "payroll adjustment",
            &id.to_string(),
            a.status.as_str(),
            "applied",
        ));
    }
    let r = get_result(c.st, c.tenant, a.result_id).await?;
    if r.status != PayrollResultStatus::Finalized {
        return Err(KernelError::Conflict(
            "the adjusted result was superseded meanwhile".into(),
        ));
    }
    let run = get_run(c.st, c.tenant, a.run_id).await?;
    let scope = c.scope(a.institution_id);
    require_institution_writable(c.st, &scope, a.institution_id).await?;
    let staff =
        c.st.directory
            .staff(&scope, StaffMemberId(a.staff_id))
            .await?;
    let receipt =
        if a.gross_delta.is_zero() && a.deduction_delta.is_zero() && a.employer_delta.is_zero() {
            None
        } else {
            let today = business_today(c.st, &scope, a.institution_id).await?;
            let req = posting_request(
                &run,
                format!("adjustment:{id}"),
                today,
                format!("Payroll adjustment of {} ({})", r.staff_number, run.number),
                (a.gross_delta, a.deduction_delta, a.employer_delta),
                c.actor,
            );
            Some(c.st.postings.post_payroll(&scope, req).await?)
        };
    let mut tx = c.st.pool.begin().await.map_err(map_db_error)?;
    let locked = load_adjustment(&mut tx, c.tenant, id, true).await?;
    ensure_version(expected, locked.version)?;
    let old = load_result(&mut tx, c.tenant, a.result_id, true).await?;
    if old.status != PayrollResultStatus::Finalized {
        return Err(KernelError::Conflict(
            "the adjusted result was superseded meanwhile".into(),
        ));
    }
    move_adjustment(
        &mut tx,
        c,
        &locked,
        PayrollAdjustmentStatus::Calculated,
        None,
        None,
        c.actor,
    )
    .await?;
    sqlx::query(
        "UPDATE people.payroll_result SET status = 'superseded', version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(old.id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let revised = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO people.payroll_result (id, tenant_id, institution_id, run_id, staff_id, \
         engagement_id, staff_number, display_name, status, revision, supersedes_result_id, \
         origin_result_id, period_start, period_end, period_days, employed_days, lop_days, \
         payable_days, gross_earnings, total_deductions, employer_contributions, net_pay, \
         salary_assignment_id, structure_id, config_id, snapshot, exceptions, finalized_at) \
         SELECT $2, tenant_id, institution_id, run_id, staff_id, engagement_id, staff_number, \
         display_name, 'finalized', revision + 1, id, origin_result_id, period_start, period_end, \
         period_days, employed_days, lop_days, payable_days, gross_earnings + $3, \
         total_deductions + $4, employer_contributions + $5, net_pay + $6, salary_assignment_id, \
         structure_id, config_id, snapshot || jsonb_build_object('adjustmentId', $7::text), \
         exceptions, now() FROM people.payroll_result WHERE id = $1",
    )
    .bind(old.id)
    .bind(revised)
    .bind(a.gross_delta)
    .bind(a.deduction_delta)
    .bind(a.employer_delta)
    .bind(a.net_delta)
    .bind(id.to_string())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.payroll_result_line (tenant_id, result_id, line_no, code, name, kind, \
         amount, basis, source, source_ref) SELECT tenant_id, $2, line_no, code, name, kind, \
         amount, basis, source, source_ref FROM people.payroll_result_line WHERE result_id = $1",
    )
    .bind(old.id)
    .bind(revised)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = old.lines.iter().map(|l| l.line_no).max().unwrap_or(0) + 1;
    let deltas: Vec<CalcLine> = stored_lines(&a.lines)
        .into_iter()
        .map(|l| l.source(LineSource::Adjustment, Some(id)))
        .collect();
    insert_lines(&mut tx, c.tenant, revised, &deltas, next).await?;
    sqlx::query(
        "UPDATE people.payroll_adjustment SET revised_result_id = $2, journal_entry_id = $3, \
         journal_entry_number = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(revised)
    .bind(receipt.as_ref().map(|x| x.journal_entry_id.as_uuid()))
    .bind(receipt.as_ref().map(|x| x.entry_number.clone()))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let calculated = load_adjustment(&mut tx, c.tenant, id, false).await?;
    let pid = calculated.process_instance_id.clone();
    let out = move_adjustment(
        &mut tx,
        c,
        &calculated,
        PayrollAdjustmentStatus::Applied,
        None,
        pid.as_deref(),
        c.actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        PAYROLL_ADJUSTMENT_APPLIED,
        PAYROLL_ADJUSTMENT_APPLIED_SCHEMA,
        id.to_string(),
        &payload(&Change {
            aggregate_id: id,
            institution: Some(a.institution_id),
            staff_id: Some(a.staff_id),
            engagement_id: Some(a.engagement_id),
            from: Some(PayrollAdjustmentStatus::Calculated.as_str()),
            to: out.status.as_str(),
            version: out.version,
            reason: Some(&a.reason),
            process_instance_id: out.process_instance_id.as_deref(),
            staff_subject_ref: staff.as_ref().and_then(|s| s.subject_ref.as_deref()),
        }),
        c.actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.payroll_adjustment.apply",
        ADJ,
        id.to_string(),
        json!({"revisedResultId": revised, "supersededResultId": old.id,
               "netDelta": fmt2(a.net_delta),
               "journalEntryId": receipt.as_ref().map(|x| x.journal_entry_id.as_uuid())}),
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
    fn stored_lines_parse_signed_amounts() {
        let v = json!([
            {"code": "HRA", "name": "House rent", "kind": "earning", "amount": "-500.00"},
            {"code": "X", "kind": "bogus", "name": "x", "amount": "1"}
        ]);
        let l = stored_lines(&v);
        assert_eq!(l.len(), 1);
        assert_eq!(l[0].amount, Decimal::new(-50000, 2));
        assert_eq!(totals(&l).net, Decimal::new(-50000, 2));
    }
}
