//! **ERP_EXTENSIONS 1D** — late-fee runs and pre-due fee reminders (tag `finance-receivables`).
//!
//! * `POST /institutions/{id}/late-fees/preview` and `POST /institutions/{id}/late-fees`: with
//!   the effective `late_fee` rule, every published charge with something late (on a schedule,
//!   only the instalments past due) is priced as of the business date; a run charges the
//!   difference between the fee due today and the fees already raised on it as a `fine` charge
//!   (`source_id` = the late charge; Dr receivables · Cr the fee head's income), records the
//!   accrual and emits `late_fee.charged`. Re-running the same day charges nothing more; a waived
//!   fee still counts as raised, so waiving never re-accrues it. Fees never carry fees.
//! * `POST /institutions/{id}/fee-reminders`: with the effective `reminder_schedule` rule, a
//!   reminder goes out `beforeDays` before each charge's next due date (`fee_reminder.sent`),
//!   once per charge, due date and offset. After the due date 5B's dunning run follows the same
//!   rule's `afterDays`.
//!
//! The pricing rules are `super::latefee` (ported from Everything's invoicing app, MIT).

use super::latefee::{
    days_late, due_date_for, overdue_amount, schedule_rows, Instalment, LateFeePolicy,
    ReminderSchedule, RowAmount, ScheduleRow,
};
use crate::charges::repo::{create_in_tx, line, publish_locked, validate_locked, ChargeInput};
use crate::payments::common::{subject_ref, Changed};
use crate::shared::charge::{lock_charge, ChargeRow};
use crate::shared::coa::resolve;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::money::{display_inr, fmt};
use crate::shared::publish::{audit, event};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use axum::extract::{Path, State};
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::PgConnection;
use sutra_contracts::finance::events::payments::{
    FEE_REMINDER_SENT, FEE_REMINDER_SENT_SCHEMA, LATE_FEE_CHARGED, LATE_FEE_CHARGED_SCHEMA,
};
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::ports::{AccountRef, SystemAccount};
use sutra_contracts::finance::status::ChargeSourceType;
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// One charge priced by a late-fee run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LateFeeItem {
    pub charge_id: Uuid,
    pub charge_number: String,
    pub description: String,
    pub student_name: String,
    pub student_number: Option<String>,
    /// The date it is late against (the earliest open instalment, or the charge's due date).
    pub due_date: NaiveDate,
    pub days_late: i64,
    /// The late part (on a schedule only the instalments past due).
    #[schema(value_type = String, example = "25000.00")]
    pub overdue_amount: String,
    /// The fee the rule asks for as of today.
    #[schema(value_type = String, example = "500.00")]
    pub fee_to_date: String,
    #[schema(value_type = String, example = "250.00")]
    pub already_charged: String,
    /// What the run charges (fee to date − already charged).
    #[schema(value_type = String, example = "250.00")]
    pub to_charge: String,
    /// The fee charge raised by this run.
    pub fee_charge_id: Option<Uuid>,
    pub fee_charge_number: Option<String>,
}

/// A late-fee preview or run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LateFeeRun {
    pub as_of: NaiveDate,
    /// The effective `late_fee` rule version that priced the run.
    pub rule_version: i32,
    /// `true` for a preview (nothing charged).
    pub preview: bool,
    /// Charges with a fee to charge now (preview) or charged (run).
    pub charged: i64,
    #[schema(value_type = String, example = "750.00")]
    pub total: String,
    pub items: Vec<LateFeeItem>,
}

/// One reminder sent before a due date.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeReminderItem {
    pub charge_id: Uuid,
    pub charge_number: String,
    pub student_name: String,
    pub due_date: NaiveDate,
    pub days_before: i64,
    #[schema(value_type = String, example = "25000.00")]
    pub amount_due: String,
    pub message: String,
}

/// A pre-due reminder run.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FeeReminderRun {
    pub as_of: NaiveDate,
    pub rule_version: i32,
    pub sent: i64,
    pub items: Vec<FeeReminderItem>,
}

/// The effective version of a financial rule: `(id, version_no, parameters)`.
pub(crate) async fn effective_rule(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    key: &str,
) -> Result<Option<(Uuid, i32, Value)>, KernelError> {
    sqlx::query_as::<_, (Uuid, i32, Value)>(
        "SELECT id, version_no, parameters FROM finance.finance_rule \
         WHERE tenant_id = $1 AND institution_id = $2 AND rule_key = $3 AND status = 'effective'",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// The effective reminder schedule, when the institution has one (a bad stored value is ignored
/// rather than stopping dunning).
pub(crate) async fn reminder_schedule(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
) -> Result<Option<(i32, ReminderSchedule)>, KernelError> {
    Ok(effective_rule(conn, t, inst, "reminder_schedule")
        .await?
        .and_then(|(_, v, p)| ReminderSchedule::from_values(&p).ok().map(|s| (v, s))))
}

/// Charges that can be late: published, owing, raised for a student, and not themselves fees.
async fn candidates(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
) -> Result<Vec<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM finance.charge WHERE tenant_id = $1 AND institution_id = $2 \
         AND status IN ('published', 'active') AND outstanding_amount > 0 AND student_id IS NOT NULL \
         AND source_type <> 'fine' ORDER BY id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// The charge's instalments, filled with what it has received (earliest first).
async fn instalments(
    conn: &mut PgConnection,
    t: TenantId,
    c: &ChargeRow,
) -> Result<Vec<Instalment>, KernelError> {
    let rows: Vec<(NaiveDate, Decimal)> = sqlx::query_as(
        "SELECT i.due_date, i.amount FROM finance.instalment i \
         JOIN finance.instalment_plan p ON p.id = i.instalment_plan_id \
         WHERE i.tenant_id = $1 AND i.charge_id = $2 AND p.status = 'active' AND i.status <> 'cancelled' \
         ORDER BY i.sequence",
    )
    .bind(t.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let rows: Vec<ScheduleRow> = rows
        .into_iter()
        .map(|(due, amount)| ScheduleRow {
            due: Some(due),
            amount: RowAmount::Fixed(amount),
        })
        .collect();
    Ok(schedule_rows(
        &rows,
        c.total_amount + c.adjustment_amount,
        c.settled_amount,
    ))
}

async fn student_of(
    conn: &mut PgConnection,
    t: TenantId,
    c: &ChargeRow,
) -> Result<(String, Option<String>), KernelError> {
    sqlx::query_as(
        "SELECT display_name, student_number FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(c.student_account_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Prices one charge; `None` when nothing is late.
async fn assess(
    conn: &mut PgConnection,
    t: TenantId,
    c: &ChargeRow,
    policy: &LateFeePolicy,
    today: NaiveDate,
) -> Result<Option<(LateFeeItem, Decimal)>, KernelError> {
    let inst = instalments(conn, t, c).await?;
    let Some(due) = due_date_for(&inst, c.due_date) else {
        return Ok(None);
    };
    let late = days_late(Some(due), today);
    let overdue = overdue_amount(c.outstanding_amount, &inst, c.due_date, today);
    if late <= 0 || overdue <= Decimal::ZERO {
        return Ok(None);
    }
    let fee = policy.fee_due(overdue, late);
    // Raised fees count whatever happened to them since (paid, waived): waiving never re-accrues.
    let already: Decimal = sqlx::query_scalar(
        "SELECT coalesce(sum(total_amount), 0) FROM finance.charge WHERE tenant_id = $1 \
         AND source_type = 'fine' AND source_id = $2 AND status NOT IN ('draft', 'pending_validation', 'validated', 'cancelled')",
    )
    .bind(t.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let to_charge = (fee - already).max(Decimal::ZERO);
    let (name, number) = student_of(conn, t, c).await?;
    Ok(Some((
        LateFeeItem {
            charge_id: c.id.as_uuid(),
            charge_number: c.charge_number.clone(),
            description: c.description.clone(),
            student_name: name,
            student_number: number,
            due_date: due,
            days_late: late,
            overdue_amount: fmt(overdue),
            fee_to_date: fmt(fee),
            already_charged: fmt(already),
            to_charge: fmt(to_charge),
            fee_charge_id: None,
            fee_charge_number: None,
        },
        to_charge,
    )))
}

async fn fee_head(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
    code: &str,
) -> Result<Uuid, KernelError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM finance.fee_head WHERE tenant_id = $1 AND institution_id = $2 AND code = $3",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(code)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(id) = existing {
        return Ok(id);
    }
    let income = resolve(conn, t, inst, AccountRef::System(SystemAccount::OtherIncome)).await?;
    sqlx::query_scalar(
        "INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, income_account_id) \
         VALUES ($1, $2, $3, 'Late fee', 'fine', $4) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(code)
    .bind(income.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn late_fee_policy(
    conn: &mut PgConnection,
    t: TenantId,
    inst: InstitutionId,
) -> Result<(Uuid, i32, LateFeePolicy), KernelError> {
    let none = || {
        KernelError::Conflict(
            "no late-fee rule is effective; draft and approve a `late_fee` rule first".into(),
        )
    };
    let (id, version, params) = effective_rule(conn, t, inst, "late_fee")
        .await?
        .ok_or_else(none)?;
    let policy = LateFeePolicy::from_values(&params)
        .map_err(|m| KernelError::Conflict(format!("the effective late-fee rule is invalid: {m}")))?
        .ok_or_else(|| {
            KernelError::Conflict("the effective late-fee rule charges nothing (mode none)".into())
        })?;
    Ok((id, version, policy))
}

async fn late_fees(
    st: &FinanceState,
    auth: &Authenticated,
    id: Uuid,
    preview: bool,
) -> ApiResult<LateFeeRun> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    if preview {
        require_institution(st, &scope, inst).await?;
    } else {
        require_institution_writable(st, &scope, inst).await?;
    }
    let t = tenant(ctx);
    let a = actor(ctx);
    let today = business_today(st, &scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (rule_id, rule_version, policy) = late_fee_policy(&mut conn, t, inst).await?;
    let ids = candidates(&mut conn, t, inst).await?;
    drop(conn);
    let mut items = Vec::new();
    let (mut charged, mut total) = (0_i64, Decimal::ZERO);
    for cid in ids {
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let c = lock_charge(&mut tx, t, ChargeId(cid)).await?;
        let Some((mut item, to_charge)) = assess(&mut tx, t, &c, &policy, today).await? else {
            continue;
        };
        if to_charge > Decimal::ZERO {
            charged += 1;
            total += to_charge;
            if !preview {
                let raised = charge_fee(st, &mut tx, &scope, &c, &item, to_charge, rule_id, &policy, today, &a)
                    .await?;
                item.fee_charge_id = Some(raised.0);
                item.fee_charge_number = Some(raised.1);
            }
        }
        tx.commit().await.map_err(map_db_error)?;
        items.push(item);
    }
    Ok(LateFeeRun {
        as_of: today,
        rule_version,
        preview,
        charged,
        total: fmt(total),
        items,
    })
}

#[allow(clippy::too_many_arguments)]
async fn charge_fee(
    st: &FinanceState,
    tx: &mut PgConnection,
    scope: &ExecutionScope,
    c: &ChargeRow,
    item: &LateFeeItem,
    amount: Decimal,
    rule_id: Uuid,
    policy: &LateFeePolicy,
    today: NaiveDate,
    a: &Actor,
) -> Result<(Uuid, String), KernelError> {
    let t = c.tenant_id;
    let inst = c.institution_id;
    let student = c.student_id.ok_or_else(|| KernelError::internal_msg("late charge without a student"))?;
    let head = fee_head(tx, t, inst, policy.fee_head()).await?;
    let description = format!("Late fee on {} ({} days late)", c.charge_number, item.days_late);
    let draft = create_in_tx(
        st,
        tx,
        scope,
        inst,
        &ChargeInput {
            student_id: StudentId(student),
            description: description.clone(),
            due_date: Some(today),
            academic_year_id: c.academic_year_id,
            academic_term_id: c.academic_term_id,
            source_type: ChargeSourceType::Fine,
            source_id: Some(c.id.as_uuid()),
            lines: vec![line(head, description.clone(), amount)],
        },
        a,
    )
    .await?;
    let validated = validate_locked(tx, &draft, a).await?;
    let (fee, journal) = publish_locked(tx, &validated, today, None, a).await?;
    sqlx::query(
        "INSERT INTO finance.late_fee_accrual (tenant_id, institution_id, charge_id, fee_charge_id, rule_id, as_of, \
         days_late, overdue_amount, fee_to_date, amount, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(t.as_uuid())
    .bind(inst.as_uuid())
    .bind(c.id.as_uuid())
    .bind(fee.id.as_uuid())
    .bind(rule_id)
    .bind(today)
    .bind(i32::try_from(item.days_late).unwrap_or(i32::MAX))
    .bind(decimal(&item.overdue_amount))
    .bind(decimal(&item.fee_to_date))
    .bind(amount)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let subject = subject_ref(tx, t, c.student_account_id).await?;
    let reason = format!(
        "{} on {} ({}), {} days late",
        display_inr(amount),
        c.charge_number,
        c.description,
        item.days_late
    );
    let payload = Changed::new(fee.id.as_uuid(), inst, "published", fee.version)
        .account(c.student_account_id, subject)
        .charge(fee.id.as_uuid())
        .amount(amount, &fee.currency)
        .reason(Some(&reason))
        .journal(Some(journal.as_uuid()));
    event(tx, scope, LATE_FEE_CHARGED, LATE_FEE_CHARGED_SCHEMA, fee.id.to_string(), &payload.v, a).await?;
    audit(
        tx,
        scope,
        "finance.late_fee.charge",
        "charge",
        fee.id.to_string(),
        json!({"lateChargeId": c.id, "amount": fmt(amount), "daysLate": item.days_late,
               "overdue": item.overdue_amount, "feeToDate": item.fee_to_date, "ruleId": rule_id}),
        a,
    )
    .await?;
    Ok((fee.id.as_uuid(), fee.charge_number))
}

fn decimal(s: &str) -> Decimal {
    s.parse().unwrap_or(Decimal::ZERO)
}

/// What a late-fee run would charge today, without charging (`receivable.manage`).
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/late-fees/preview", operation_id = "finance_receivable_late_fee_preview", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LateFeeRun), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn late_fee_preview(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LateFeeRun>> {
    Ok(Json(late_fees(&st, &auth, id, true).await?))
}

/// Charges late fees by the effective `late_fee` rule (`receivable.manage`). Idempotent per day:
/// each charge is charged only the difference between the fee due today and fees already raised.
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/late-fees", operation_id = "finance_receivable_late_fee_run", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LateFeeRun), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn late_fee_run(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LateFeeRun>> {
    Ok(Json(late_fees(&st, &auth, id, false).await?))
}

/// Sends the reminders due today before due dates by the effective `reminder_schedule` rule
/// (`receivable.manage`); each charge, due date and offset is reminded once.
#[utoipa::path(post, path = "/api/v1/finance/institutions/{id}/fee-reminders", operation_id = "finance_dunning_reminder_run", tag = "finance-receivables",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FeeReminderRun), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fee_reminder_run(
    State(st): State<FinanceState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FeeReminderRun>> {
    let ctx = auth.require(Permission::ReceivableManage)?;
    let inst = InstitutionId(id);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let t = tenant(ctx);
    let a = actor(ctx);
    let today = business_today(&st, &scope, inst).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let (rule_version, schedule) = reminder_schedule(&mut conn, t, inst).await?.ok_or_else(|| {
        KernelError::Conflict(
            "no reminder schedule is effective; draft and approve a `reminder_schedule` rule first"
                .into(),
        )
    })?;
    let ids = candidates(&mut conn, t, inst).await?;
    drop(conn);
    let mut items = Vec::new();
    for cid in ids {
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let c = lock_charge(&mut tx, t, ChargeId(cid)).await?;
        let inst_rows = instalments(&mut tx, t, &c).await?;
        let Some(due) = due_date_for(&inst_rows, c.due_date) else {
            continue;
        };
        let Some(before) = schedule.upcoming_today(days_late(Some(due), today)) else {
            continue;
        };
        let amount_due = inst_rows
            .iter()
            .find(|r| r.due == Some(due) && r.open > Decimal::ZERO)
            .map_or(c.outstanding_amount, |r| r.open);
        if amount_due <= Decimal::ZERO {
            continue;
        }
        let (name, _) = student_of(&mut tx, t, &c).await?;
        let day_word = if before == 1 { "day" } else { "days" };
        let message = format!(
            "{} on {} ({}) is due on {} — {before} {day_word} from today.",
            display_inr(amount_due),
            c.charge_number,
            c.description,
            due.format("%d %b %Y")
        );
        let rid: Option<Uuid> = sqlx::query_scalar(
            "INSERT INTO finance.fee_reminder (tenant_id, institution_id, charge_id, student_account_id, due_date, \
             days_before, amount_due, reminder_date, message, sent_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
             ON CONFLICT (tenant_id, charge_id, due_date, days_before) DO NOTHING RETURNING id",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(cid)
        .bind(c.student_account_id.as_uuid())
        .bind(due)
        .bind(i32::try_from(before).unwrap_or(i32::MAX))
        .bind(amount_due)
        .bind(today)
        .bind(&message)
        .bind(a.id.as_deref())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let Some(rid) = rid else { continue };
        let subject = subject_ref(&mut tx, t, c.student_account_id).await?;
        let payload = Changed::new(rid, inst, "sent", 1)
            .account(c.student_account_id, subject)
            .charge(cid)
            .amount(amount_due, &c.currency)
            .reason(Some(&message));
        event(&mut tx, &scope, FEE_REMINDER_SENT, FEE_REMINDER_SENT_SCHEMA, rid.to_string(), &payload.v, &a).await?;
        audit(
            &mut tx,
            &scope,
            "finance.fee_reminder.send",
            "fee_reminder",
            rid.to_string(),
            json!({"chargeId": cid, "dueDate": due, "daysBefore": before, "amountDue": fmt(amount_due)}),
            &a,
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
        items.push(FeeReminderItem {
            charge_id: cid,
            charge_number: c.charge_number.clone(),
            student_name: name,
            due_date: due,
            days_before: before,
            amount_due: fmt(amount_due),
            message,
        });
    }
    Ok(Json(FeeReminderRun {
        as_of: today,
        rule_version,
        sent: i64::try_from(items.len()).unwrap_or(i64::MAX),
        items,
    }))
}
