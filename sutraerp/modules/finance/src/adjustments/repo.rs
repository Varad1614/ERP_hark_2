//! Adjustment persistence and the posting recipes of contract §3.3 (credit note / concession /
//! waiver: Dr ConcessionExpense · Cr StudentReceivables; write-off: Dr BadDebtExpense · Cr
//! StudentReceivables; debit note: Dr StudentReceivables · Cr the charge's income account;
//! reversal: `shared::posting::reverse`). The charge amount moves only through
//! `shared::charge::apply_adjustment`, in the same transaction as the posting.

use super::domain::{can_transition, delta, number_kind};
use crate::charges::kit::{status_event, subject_of, StatusEvent};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::charge::{apply_adjustment, charge_lines, lock_charge, ChargeRow};
use crate::shared::lifecycle::{change_charge_status, record_history, HistoryEntry, StatusChange};
use crate::shared::posting;
use crate::shared::publish::audit;
use crate::shared::time::business_today;
use crate::shared::{Actor, FinanceState};
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::events::charges::{
    ADJUSTMENT_POSTED, ADJUSTMENT_POSTED_SCHEMA, ADJUSTMENT_REVERSED, ADJUSTMENT_REVERSED_SCHEMA,
    ADJUSTMENT_SUBMITTED, ADJUSTMENT_SUBMITTED_SCHEMA, ADJUSTMENT_SUPERSEDED,
    ADJUSTMENT_SUPERSEDED_SCHEMA,
};
use sutra_contracts::finance::ids::{AdjustmentId, ChargeId, StudentAccountId};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{
    AdjustmentKind, AdjustmentStatus, ChargeStatus, JournalEntryKind,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const COLUMNS: &str = "a.id, a.tenant_id, a.institution_id, a.adjustment_number, a.charge_id, \
    c.charge_number, a.student_account_id, a.kind, a.amount, a.currency, a.reason, a.origin, \
    a.status, a.status_reason, a.process_instance_id, a.decided_by, a.decided_at, a.posted_at, \
    a.posting_date, a.journal_entry_id, a.reversal_entry_id, a.supersedes_id, a.superseded_by_id, \
    a.created_by, a.version, a.created_at";
pub const FROM: &str =
    "FROM finance.adjustment a JOIN finance.charge c ON c.id = a.charge_id AND c.tenant_id = a.tenant_id";

/// A `finance.adjustment` row (with its charge number).
#[derive(Clone, Debug)]
pub struct AdjustmentRow {
    pub id: AdjustmentId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub adjustment_number: String,
    pub charge_id: ChargeId,
    pub charge_number: String,
    pub student_account_id: StudentAccountId,
    pub kind: AdjustmentKind,
    pub amount: Decimal,
    pub currency: String,
    pub reason: String,
    pub origin: String,
    pub status: AdjustmentStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub posted_at: Option<DateTime<Utc>>,
    pub posting_date: Option<NaiveDate>,
    pub journal_entry_id: Option<Uuid>,
    pub reversal_entry_id: Option<Uuid>,
    pub supersedes_id: Option<Uuid>,
    pub superseded_by_id: Option<Uuid>,
    pub created_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown value `{raw}`").into()))
}

pub fn from_row(r: &PgRow) -> Result<AdjustmentRow, sqlx::Error> {
    Ok(AdjustmentRow {
        id: AdjustmentId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        adjustment_number: r.try_get("adjustment_number")?,
        charge_id: ChargeId(r.try_get("charge_id")?),
        charge_number: r.try_get("charge_number")?,
        student_account_id: StudentAccountId(r.try_get("student_account_id")?),
        kind: decode(r.try_get("kind")?, AdjustmentKind::parse)?,
        amount: r.try_get("amount")?,
        currency: r.try_get("currency")?,
        reason: r.try_get("reason")?,
        origin: r.try_get("origin")?,
        status: decode(r.try_get("status")?, AdjustmentStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        posted_at: r.try_get("posted_at")?,
        posting_date: r.try_get("posting_date")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        reversal_entry_id: r.try_get("reversal_entry_id")?,
        supersedes_id: r.try_get("supersedes_id")?,
        superseded_by_id: r.try_get("superseded_by_id")?,
        created_by: r.try_get("created_by")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

async fn fetch(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AdjustmentId,
    lock: bool,
) -> Result<AdjustmentRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} {FROM} WHERE a.tenant_id = $1 AND a.id = $2{}",
        if lock { " FOR UPDATE OF a" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "adjustment",
    })?;
    from_row(&row).map_err(map_db_error)
}

pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AdjustmentId,
) -> Result<AdjustmentRow, KernelError> {
    fetch(conn, tenant, id, false).await
}

pub async fn get(
    state: &FinanceState,
    tenant: TenantId,
    id: AdjustmentId,
) -> Result<AdjustmentRow, KernelError> {
    let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant, id).await
}

/// Adjustments of a charge, oldest first.
pub async fn list_of_charge(
    state: &FinanceState,
    tenant: TenantId,
    charge: ChargeId,
) -> Result<Vec<AdjustmentRow>, KernelError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} {FROM} WHERE a.tenant_id = $1 AND a.charge_id = $2 ORDER BY a.created_at, a.id"
    ))
    .bind(tenant.as_uuid())
    .bind(charge.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Adjustments of an institution, newest first.
pub async fn list_of_institution(
    state: &FinanceState,
    tenant: TenantId,
    institution: InstitutionId,
    status: Option<AdjustmentStatus>,
    kind: Option<AdjustmentKind>,
    limit: i64,
) -> Result<Vec<AdjustmentRow>, KernelError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} {FROM} WHERE a.tenant_id = $1 AND a.institution_id = $2 \
         AND ($3::text IS NULL OR a.status = $3) AND ($4::text IS NULL OR a.kind = $4) \
         ORDER BY a.created_at DESC, a.id DESC LIMIT $5"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .bind(kind.map(|k| k.as_str()))
    .bind(limit)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// A new adjustment draft.
pub struct NewAdjustment<'a> {
    pub kind: AdjustmentKind,
    pub amount: Decimal,
    pub reason: &'a str,
    pub origin: &'static str,
    pub supersedes: Option<AdjustmentId>,
}

/// Inserts a `draft` adjustment of `charge` (the charge must accept adjustments and a credit
/// must fit the unsettled amount).
pub async fn insert(
    conn: &mut PgConnection,
    charge: &ChargeRow,
    n: &NewAdjustment<'_>,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    crate::shared::money::require_positive("amount", n.amount)?;
    crate::shared::charge::adjustment_after(charge, delta(n.kind, n.amount))?;
    let number = crate::shared::numbering::next(
        conn,
        charge.tenant_id,
        charge.institution_id,
        number_kind(n.kind),
    )
    .await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.adjustment (tenant_id, institution_id, adjustment_number, charge_id, \
         student_account_id, kind, amount, reason, origin, supersedes_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(charge.tenant_id.as_uuid())
    .bind(charge.institution_id.as_uuid())
    .bind(&number)
    .bind(charge.id.as_uuid())
    .bind(charge.student_account_id.as_uuid())
    .bind(n.kind.as_str())
    .bind(n.amount)
    .bind(n.reason)
    .bind(n.origin)
    .bind(n.supersedes.map(|s| s.as_uuid()))
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let scope = ExecutionScope::institution(charge.tenant_id, charge.institution_id);
    audit(
        conn,
        &scope,
        "finance.adjustment.create",
        "adjustment",
        id.to_string(),
        json!({"number": number, "chargeId": charge.id, "kind": n.kind, "amount": crate::shared::money::fmt(n.amount), "origin": n.origin}),
        actor,
    )
    .await?;
    fetch(conn, charge.tenant_id, AdjustmentId(id), true).await
}

/// Moves `a` to `to` (legal transition, optimistic version), recording history.
#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    a: &AdjustmentRow,
    to: AdjustmentStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    cause: &'static str,
    extra_sql: &str,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    if !can_transition(a.status, to) {
        return Err(KernelError::Conflict(format!(
            "adjustment {} is {}; it cannot become {to}",
            a.adjustment_number, a.status
        )));
    }
    let n = sqlx::query(&format!(
        "UPDATE finance.adjustment SET status = $3, status_reason = COALESCE($4, status_reason), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now(){extra_sql} WHERE tenant_id = $1 AND id = $2 AND version = $6"
    ))
    .bind(a.tenant_id.as_uuid())
    .bind(a.id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .bind(a.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::Conflict(
            "adjustment changed concurrently".into(),
        ));
    }
    record_history(
        conn,
        HistoryEntry {
            tenant: a.tenant_id,
            institution: a.institution_id,
            aggregate_type: "adjustment",
            aggregate_id: a.id.as_uuid(),
            from: Some(a.status.as_str()),
            to: to.as_str(),
        },
        &StatusChange {
            cause_event_type: cause,
            reason,
            process_instance_id,
        },
        actor,
    )
    .await?;
    fetch(conn, a.tenant_id, a.id, true).await
}

async fn adjustment_event(
    conn: &mut PgConnection,
    a: &AdjustmentRow,
    from: AdjustmentStatus,
    ev: (&'static str, &'static str),
    reason: Option<&str>,
    journal_entry_id: Option<Uuid>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let scope = ExecutionScope::institution(a.tenant_id, a.institution_id);
    let subject = subject_of(conn, a.tenant_id, a.student_account_id).await?;
    status_event(
        conn,
        &scope,
        ev,
        StatusEvent {
            aggregate_id: a.id.as_uuid(),
            student_account_id: Some(a.student_account_id.as_uuid()),
            charge_id: Some(a.charge_id.as_uuid()),
            from: Some(from.as_str().to_owned()),
            to: a.status.as_str().to_owned(),
            version: a.version,
            amount: Some(a.amount),
            reason: reason.map(str::to_owned),
            process_instance_id: a.process_instance_id.clone(),
            journal_entry_id,
            subject,
            ..StatusEvent::default()
        },
        actor,
    )
    .await
}

/// `draft → pending_approval` with the approval process id; emits `adjustment.submitted`.
pub async fn mark_pending(
    conn: &mut PgConnection,
    a: &AdjustmentRow,
    process_instance_id: &str,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    let next = transition(
        conn,
        a,
        AdjustmentStatus::PendingApproval,
        None,
        Some(process_instance_id),
        ADJUSTMENT_SUBMITTED,
        "",
        actor,
    )
    .await?;
    adjustment_event(
        conn,
        &next,
        a.status,
        (ADJUSTMENT_SUBMITTED, ADJUSTMENT_SUBMITTED_SCHEMA),
        Some(&a.reason),
        None,
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(a.tenant_id, a.institution_id);
    audit(
        conn,
        &scope,
        "finance.adjustment.submit",
        "adjustment",
        a.id.to_string(),
        json!({"processInstanceId": process_instance_id}),
        actor,
    )
    .await?;
    Ok(next)
}

/// The posting request of an adjustment.
pub async fn posting_request(
    conn: &mut PgConnection,
    a: &AdjustmentRow,
    charge: &ChargeRow,
    posting_date: NaiveDate,
) -> Result<PostingRequest, KernelError> {
    let receivable = |side_debit: bool| {
        let acc = AccountRef::System(SystemAccount::StudentReceivables);
        let l = if side_debit {
            PostingLine::debit(acc, a.amount)
        } else {
            PostingLine::credit(acc, a.amount)
        };
        l.for_charge(charge.student_account_id, Some(charge.id))
            .memo(a.adjustment_number.clone())
    };
    let lines = match a.kind {
        AdjustmentKind::DebitNote => {
            let income = charge_lines(conn, charge.tenant_id, charge.id)
                .await?
                .first()
                .map(|l| AccountRef::Id(l.income_account_id))
                .unwrap_or(AccountRef::System(SystemAccount::OtherIncome));
            vec![receivable(true), PostingLine::credit(income, a.amount)]
        }
        AdjustmentKind::WriteOff => vec![
            PostingLine::debit(AccountRef::System(SystemAccount::BadDebtExpense), a.amount),
            receivable(false),
        ],
        AdjustmentKind::CreditNote | AdjustmentKind::Concession | AdjustmentKind::Waiver => vec![
            PostingLine::debit(
                AccountRef::System(SystemAccount::ConcessionExpense),
                a.amount,
            ),
            receivable(false),
        ],
    };
    Ok(PostingRequest {
        institution_id: a.institution_id,
        posting_date,
        kind: JournalEntryKind::Adjusting,
        source_type: source::ADJUSTMENT.into(),
        source_id: a.id.as_uuid(),
        posting_key: "post".into(),
        description: format!(
            "{} {} on {}",
            a.kind.as_str().replace('_', " "),
            a.adjustment_number,
            charge.charge_number
        ),
        currency: CurrencyCode::INR,
        lines,
    })
}

/// Posts a pending / approved adjustment on its **locked** charge: `apply_adjustment` + the
/// journal entry + `posted` + `adjustment.posted`, all in the caller's transaction. A write-off
/// (or waiver of an unpaid charge) that clears the charge moves it to `written_off` / `waived`.
pub async fn post_locked(
    conn: &mut PgConnection,
    a: &AdjustmentRow,
    charge: &ChargeRow,
    posting_date: NaiveDate,
    decided_by: Option<&str>,
    actor: &Actor,
) -> Result<(AdjustmentRow, ChargeRow), KernelError> {
    if !matches!(
        a.status,
        AdjustmentStatus::PendingApproval | AdjustmentStatus::Approved
    ) {
        return Err(KernelError::Conflict(format!(
            "adjustment {} is {}; only approved adjustments are posted",
            a.adjustment_number, a.status
        )));
    }
    let mut c = apply_adjustment(conn, charge, delta(a.kind, a.amount)).await?;
    let req = posting_request(conn, a, &c, posting_date).await?;
    let receipt = posting::post(conn, a.tenant_id, &req, actor).await?;
    let je = receipt.journal_entry_id.as_uuid();
    let decider = decided_by.or(actor.id.as_deref());
    transition(
        conn,
        a,
        AdjustmentStatus::Posted,
        None,
        None,
        ADJUSTMENT_POSTED,
        ", posted_at = now(), decided_at = COALESCE(decided_at, now())",
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.adjustment SET journal_entry_id = $3, posting_date = $4, \
         decided_by = COALESCE(decided_by, $5) WHERE tenant_id = $1 AND id = $2",
    )
    .bind(a.tenant_id.as_uuid())
    .bind(a.id.as_uuid())
    .bind(je)
    .bind(posting_date)
    .bind(decider)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let next = fetch(conn, a.tenant_id, a.id, true).await?;
    // Terminal outcomes of a cleared charge (L3-F-022): written off / waived.
    if c.outstanding_amount.is_zero() {
        let to = match a.kind {
            AdjustmentKind::WriteOff => Some(ChargeStatus::WrittenOff),
            AdjustmentKind::Waiver if c.settled_amount.is_zero() => Some(ChargeStatus::Waived),
            _ => None,
        };
        if let Some(to) = to.filter(|t| c.status.can_transition(*t)) {
            c = change_charge_status(
                conn,
                &c,
                to,
                &StatusChange {
                    cause_event_type: ADJUSTMENT_POSTED,
                    reason: Some(&a.reason),
                    process_instance_id: None,
                },
                actor,
            )
            .await?;
        }
    }
    adjustment_event(
        conn,
        &next,
        a.status,
        (ADJUSTMENT_POSTED, ADJUSTMENT_POSTED_SCHEMA),
        Some(&a.reason),
        Some(je),
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(a.tenant_id, a.institution_id);
    audit(
        conn,
        &scope,
        "finance.adjustment.post",
        "adjustment",
        a.id.to_string(),
        json!({"journalEntryId": je, "entryNumber": receipt.entry_number, "decidedBy": decider,
               "chargeOutstanding": crate::shared::money::fmt(c.outstanding_amount)}),
        actor,
    )
    .await?;
    Ok((next, c))
}

/// Creates a draft adjustment of a charge (L3-F-020/021/022).
pub async fn create(
    state: &FinanceState,
    tenant: TenantId,
    charge: ChargeId,
    n: NewAdjustment<'_>,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let c = lock_charge(&mut tx, tenant, charge).await?;
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    crate::shared::guard::require_institution_writable(state, &scope, c.institution_id).await?;
    let a = insert(&mut tx, &c, &n, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(a)
}

/// Submits a draft for approval through the WorkflowPort (`ADJUSTMENT_APPROVAL`); with
/// auto-approval the adjustment is posted in the same transaction.
pub async fn submit(
    state: &FinanceState,
    tenant: TenantId,
    id: AdjustmentId,
    expected: i64,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    let current = get(state, tenant, id).await?;
    crate::charges::kit::require_version(current.version, expected)?;
    if current.status != AdjustmentStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "adjustment {} is {}; only drafts are submitted",
            current.adjustment_number, current.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, current.institution_id);
    crate::shared::guard::require_institution_writable(state, &scope, current.institution_id)
        .await?;
    let today = business_today(state, &scope, current.institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("kind".into(), json!(current.kind));
    vars.insert(
        "amount".into(),
        json!(crate::shared::money::fmt(current.amount)),
    );
    vars.insert("chargeNumber".into(), json!(current.charge_number));
    let outcome = request_approval(
        state,
        &scope,
        process::ADJUSTMENT_APPROVAL,
        business_key("adjustment", id, current.version),
        vars,
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = fetch(&mut tx, tenant, id, true).await?;
    crate::charges::kit::require_version(a.version, expected)?;
    let a = mark_pending(&mut tx, &a, outcome.process_instance_id(), actor).await?;
    let a = if let ApprovalOutcome::Approved { .. } = outcome {
        let c = lock_charge(&mut tx, tenant, a.charge_id).await?;
        post_locked(
            &mut tx,
            &a,
            &c,
            today,
            Some(crate::shared::approval::AUTO_APPROVER),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
        .0
    } else {
        a
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(a)
}

/// Explicit decision on a pending adjustment (`finance.approve`): approve → posted; reject →
/// rejected (the charge is untouched).
pub async fn decide(
    state: &FinanceState,
    tenant: TenantId,
    id: AdjustmentId,
    expected: i64,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    let current = get(state, tenant, id).await?;
    let scope = ExecutionScope::institution(tenant, current.institution_id);
    let today = business_today(state, &scope, current.institution_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = fetch(&mut tx, tenant, id, true).await?;
    crate::charges::kit::require_version(a.version, expected)?;
    if a.status != AdjustmentStatus::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "adjustment {} is {}; there is no pending decision",
            a.adjustment_number, a.status
        )));
    }
    // Part 1 (DEF-X-002): the creator / submitter of an adjustment never decides it.
    crate::shared::approval::ensure_independent_decider(
        &mut tx,
        tenant,
        "adjustment",
        id.as_uuid(),
        &["pending_approval"],
        &[a.created_by.as_deref()],
        actor.id.as_deref(),
        "fee adjustment",
    )
    .await?;
    let a = if approve {
        let c = lock_charge(&mut tx, tenant, a.charge_id).await?;
        post_locked(&mut tx, &a, &c, today, actor.id.as_deref(), actor)
            .await?
            .0
    } else {
        let r = crate::charges::kit::require_text("reason", reason)?;
        let next = transition(
            &mut tx,
            &a,
            AdjustmentStatus::Rejected,
            Some(&r),
            None,
            ADJUSTMENT_SUBMITTED,
            ", decided_at = now()",
            actor,
        )
        .await?;
        sqlx::query(
            "UPDATE finance.adjustment SET decided_by = $3 WHERE tenant_id = $1 AND id = $2",
        )
        .bind(tenant.as_uuid())
        .bind(id.as_uuid())
        .bind(actor.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        audit(
            &mut tx,
            &scope,
            "finance.adjustment.reject",
            "adjustment",
            id.to_string(),
            json!({"reason": r}),
            actor,
        )
        .await?;
        fetch(&mut tx, tenant, next.id, true).await?
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(a)
}

/// Undoes a posted adjustment on its locked charge (mirror entry + opposite charge delta).
async fn unpost(
    conn: &mut PgConnection,
    a: &AdjustmentRow,
    to: AdjustmentStatus,
    reason: &str,
    today: NaiveDate,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    if a.status != AdjustmentStatus::Posted {
        return Err(KernelError::Conflict(format!(
            "adjustment {} is {}; only posted adjustments are reversed",
            a.adjustment_number, a.status
        )));
    }
    let entry = a.journal_entry_id.ok_or_else(|| {
        KernelError::Conflict("the adjustment has no journal entry to reverse".into())
    })?;
    let c = lock_charge(conn, a.tenant_id, a.charge_id).await?;
    apply_adjustment(conn, &c, -delta(a.kind, a.amount)).await?;
    let receipt = posting::reverse(
        conn,
        a.tenant_id,
        sutra_contracts::finance::ids::JournalEntryId(entry),
        today,
        reason,
        actor,
    )
    .await?;
    let cause = if to == AdjustmentStatus::Superseded {
        ADJUSTMENT_SUPERSEDED
    } else {
        ADJUSTMENT_REVERSED
    };
    let next = transition(
        conn,
        a,
        to,
        Some(reason),
        process_instance_id,
        cause,
        ", reversed_at = now()",
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.adjustment SET reversal_entry_id = $3 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(a.tenant_id.as_uuid())
    .bind(a.id.as_uuid())
    .bind(receipt.journal_entry_id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let next = fetch(conn, a.tenant_id, next.id, true).await?;
    Ok(next)
}

/// Reverses a posted adjustment (L3-F-023). The caller holds `finance.approve` (the reversal is
/// the governed decision itself); the `ADJUSTMENT_REVERSAL` process is started through the
/// WorkflowPort for orchestration evidence and its id recorded.
pub async fn reverse(
    state: &FinanceState,
    tenant: TenantId,
    id: AdjustmentId,
    expected: i64,
    reason: &str,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    let current = get(state, tenant, id).await?;
    crate::charges::kit::require_version(current.version, expected)?;
    let scope = ExecutionScope::institution(tenant, current.institution_id);
    crate::shared::guard::require_institution_writable(state, &scope, current.institution_id)
        .await?;
    let today = business_today(state, &scope, current.institution_id).await?;
    let outcome = request_approval(
        state,
        &scope,
        process::ADJUSTMENT_REVERSAL,
        business_key("adjustment-reversal", id, current.version),
        BTreeMap::new(),
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = fetch(&mut tx, tenant, id, true).await?;
    crate::charges::kit::require_version(a.version, expected)?;
    let next = unpost(
        &mut tx,
        &a,
        AdjustmentStatus::Reversed,
        reason,
        today,
        Some(outcome.process_instance_id()),
        actor,
    )
    .await?;
    adjustment_event(
        &mut tx,
        &next,
        a.status,
        (ADJUSTMENT_REVERSED, ADJUSTMENT_REVERSED_SCHEMA),
        Some(reason),
        next.reversal_entry_id,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.adjustment.reverse",
        "adjustment",
        id.to_string(),
        json!({"reason": reason, "reversalEntryId": next.reversal_entry_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Correction of an erroneous adjustment (L3-F-024): the original is superseded (a posted one
/// is reversed in the ledger first) and a corrected successor draft references it. Returns the
/// successor, which then goes through the normal submit → approval → posting flow.
#[allow(clippy::too_many_arguments)]
pub async fn correct(
    state: &FinanceState,
    tenant: TenantId,
    id: AdjustmentId,
    expected: i64,
    kind: Option<AdjustmentKind>,
    amount: Decimal,
    reason: &str,
    actor: &Actor,
) -> Result<AdjustmentRow, KernelError> {
    let current = get(state, tenant, id).await?;
    crate::charges::kit::require_version(current.version, expected)?;
    let scope = ExecutionScope::institution(tenant, current.institution_id);
    crate::shared::guard::require_institution_writable(state, &scope, current.institution_id)
        .await?;
    let today = business_today(state, &scope, current.institution_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let a = fetch(&mut tx, tenant, id, true).await?;
    crate::charges::kit::require_version(a.version, expected)?;
    let superseded = match a.status {
        AdjustmentStatus::Posted => {
            unpost(
                &mut tx,
                &a,
                AdjustmentStatus::Superseded,
                reason,
                today,
                None,
                actor,
            )
            .await?
        }
        AdjustmentStatus::Draft | AdjustmentStatus::Rejected => {
            transition(
                &mut tx,
                &a,
                AdjustmentStatus::Superseded,
                Some(reason),
                None,
                ADJUSTMENT_SUPERSEDED,
                "",
                actor,
            )
            .await?
        }
        other => {
            return Err(KernelError::Conflict(format!(
                "adjustment {} is {other}; it cannot be corrected",
                a.adjustment_number
            )))
        }
    };
    let c = lock_charge(&mut tx, tenant, a.charge_id).await?;
    let successor = insert(
        &mut tx,
        &c,
        &NewAdjustment {
            kind: kind.unwrap_or(a.kind),
            amount,
            reason,
            origin: "correction",
            supersedes: Some(a.id),
        },
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.adjustment SET superseded_by_id = $3 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(a.id.as_uuid())
    .bind(successor.id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    adjustment_event(
        &mut tx,
        &superseded,
        a.status,
        (ADJUSTMENT_SUPERSEDED, ADJUSTMENT_SUPERSEDED_SCHEMA),
        Some(reason),
        superseded.reversal_entry_id,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "finance.adjustment.correct",
        "adjustment",
        id.to_string(),
        json!({"successorId": successor.id, "successorNumber": successor.adjustment_number, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(successor)
}
