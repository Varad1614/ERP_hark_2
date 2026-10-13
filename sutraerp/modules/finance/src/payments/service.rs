//! Payment rows and the transactional core of slice 5B: completion (allocation + posting +
//! receipt), later allocation of the unapplied part, failure / cancellation / expiry, and
//! reversal / chargeback. Every function runs inside the caller's owner-local transaction;
//! charges are settled **only** through `shared::charge::apply_settlement` and the ledger is
//! written **only** through `shared::posting::{post, reverse}`.

use super::common::{history, illegal, subject_ref, Changed};
use super::domain::{can_transition, plan_allocation, OpenCharge};
use crate::shared::charge::{apply_settlement, lock_charge};
use crate::shared::lifecycle::StatusChange;
use crate::shared::numbering::{kind, next};
use crate::shared::posting;
use crate::shared::publish::{audit, event};
use crate::shared::Actor;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::events::payments::{
    PAYMENT_CANCELLED, PAYMENT_CANCELLED_SCHEMA, PAYMENT_COMPLETED, PAYMENT_COMPLETED_SCHEMA,
    PAYMENT_FAILED, PAYMENT_FAILED_SCHEMA, PAYMENT_REVERSED, PAYMENT_REVERSED_SCHEMA,
    RECEIPT_ISSUED, RECEIPT_ISSUED_SCHEMA,
};
use sutra_contracts::finance::ids::{ChargeId, JournalEntryId, PaymentId, StudentAccountId};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{JournalEntryKind, PaymentChannel, PaymentStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const PAYMENT_COLUMNS: &str = "id, tenant_id, institution_id, student_account_id, \
    payment_number, channel, payer_kind, payer_name, external_ref, amount, currency, \
    unapplied_amount, status, status_reason, target_charge_ids, gateway, gateway_order_ref, \
    gateway_payment_ref, checkout_url, failure_code, received_on, completed_at, journal_entry_id, \
    reversal_entry_id, process_instance_id, unidentified_receipt_id, batch_id, version, created_at";

/// One `finance.payment` row.
#[derive(Clone, Debug)]
pub struct PaymentRow {
    pub id: PaymentId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub student_account_id: StudentAccountId,
    pub payment_number: String,
    pub channel: PaymentChannel,
    pub payer_kind: String,
    pub payer_name: Option<String>,
    pub external_ref: Option<String>,
    pub amount: Decimal,
    pub currency: String,
    pub unapplied_amount: Decimal,
    pub status: PaymentStatus,
    pub status_reason: Option<String>,
    pub target_charge_ids: Vec<Uuid>,
    pub gateway: Option<String>,
    pub gateway_order_ref: Option<String>,
    pub gateway_payment_ref: Option<String>,
    pub checkout_url: Option<String>,
    pub failure_code: Option<String>,
    pub received_on: NaiveDate,
    pub completed_at: Option<DateTime<Utc>>,
    pub journal_entry_id: Option<Uuid>,
    pub reversal_entry_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub unidentified_receipt_id: Option<Uuid>,
    pub batch_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

fn bad_enum(what: &str, v: &str) -> sqlx::Error {
    sqlx::Error::Decode(format!("unknown {what} `{v}`").into())
}

pub fn payment_from_row(r: &PgRow) -> Result<PaymentRow, sqlx::Error> {
    let channel: String = r.try_get("channel")?;
    let status: String = r.try_get("status")?;
    Ok(PaymentRow {
        id: PaymentId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        student_account_id: StudentAccountId(r.try_get("student_account_id")?),
        payment_number: r.try_get("payment_number")?,
        channel: PaymentChannel::parse(&channel).ok_or_else(|| bad_enum("channel", &channel))?,
        payer_kind: r.try_get("payer_kind")?,
        payer_name: r.try_get("payer_name")?,
        external_ref: r.try_get("external_ref")?,
        amount: r.try_get("amount")?,
        currency: r.try_get("currency")?,
        unapplied_amount: r.try_get("unapplied_amount")?,
        status: PaymentStatus::parse(&status).ok_or_else(|| bad_enum("status", &status))?,
        status_reason: r.try_get("status_reason")?,
        target_charge_ids: r.try_get("target_charge_ids")?,
        gateway: r.try_get("gateway")?,
        gateway_order_ref: r.try_get("gateway_order_ref")?,
        gateway_payment_ref: r.try_get("gateway_payment_ref")?,
        checkout_url: r.try_get("checkout_url")?,
        failure_code: r.try_get("failure_code")?,
        received_on: r.try_get("received_on")?,
        completed_at: r.try_get("completed_at")?,
        journal_entry_id: r.try_get("journal_entry_id")?,
        reversal_entry_id: r.try_get("reversal_entry_id")?,
        process_instance_id: r.try_get("process_instance_id")?,
        unidentified_receipt_id: r.try_get("unidentified_receipt_id")?,
        batch_id: r.try_get("batch_id")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Reads (optionally locks `FOR UPDATE`) a payment of the tenant; 404 otherwise.
pub async fn load_payment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PaymentRow, KernelError> {
    let sql = format!(
        "SELECT {PAYMENT_COLUMNS} FROM finance.payment WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "payment" })?;
    payment_from_row(&row).map_err(map_db_error)
}

/// Data for a new payment row.
pub struct NewPayment<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub account: StudentAccountId,
    pub channel: PaymentChannel,
    pub payer_kind: &'a str,
    pub payer_name: Option<&'a str>,
    pub external_ref: Option<&'a str>,
    pub amount: Decimal,
    pub status: PaymentStatus,
    pub targets: &'a [Uuid],
    pub gateway: Option<&'a str>,
    pub order_ref: Option<&'a str>,
    pub checkout_url: Option<&'a str>,
    pub idempotency_key: Option<&'a str>,
    pub received_on: NaiveDate,
    pub unidentified_receipt_id: Option<Uuid>,
    pub journal_entry_id: Option<Uuid>,
    pub unapplied: Decimal,
    pub batch_id: Option<Uuid>,
}

/// Inserts a payment (`PAY-` number) and its first history row.
pub async fn insert_payment(
    conn: &mut PgConnection,
    n: &NewPayment<'_>,
    actor: &Actor,
) -> Result<PaymentRow, KernelError> {
    let number = next(conn, n.tenant, n.institution, kind::PAYMENT).await?;
    let row = sqlx::query(&format!(
        "INSERT INTO finance.payment (tenant_id, institution_id, student_account_id, payment_number, \
         channel, payer_kind, payer_name, external_ref, amount, status, target_charge_ids, gateway, \
         gateway_order_ref, checkout_url, idempotency_key, received_on, unidentified_receipt_id, \
         journal_entry_id, unapplied_amount, batch_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21) \
         RETURNING {PAYMENT_COLUMNS}"
    ))
    .bind(n.tenant.as_uuid())
    .bind(n.institution.as_uuid())
    .bind(n.account.as_uuid())
    .bind(&number)
    .bind(n.channel.as_str())
    .bind(n.payer_kind)
    .bind(n.payer_name)
    .bind(n.external_ref)
    .bind(n.amount)
    .bind(n.status.as_str())
    .bind(n.targets)
    .bind(n.gateway)
    .bind(n.order_ref)
    .bind(n.checkout_url)
    .bind(n.idempotency_key)
    .bind(n.received_on)
    .bind(n.unidentified_receipt_id)
    .bind(n.journal_entry_id)
    .bind(n.unapplied)
    .bind(n.batch_id)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let p = payment_from_row(&row).map_err(map_db_error)?;
    history(
        conn,
        p.tenant_id,
        p.institution_id,
        "payment",
        p.id.as_uuid(),
        None,
        p.status.as_str(),
        &StatusChange {
            cause_event_type: "",
            reason: None,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(p)
}

/// Checks + applies a status move (version bump + history). Extra columns are set by the caller.
async fn move_status(
    conn: &mut PgConnection,
    p: &PaymentRow,
    to: PaymentStatus,
    reason: Option<&str>,
    cause: &'static str,
    process: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !can_transition(p.status, to) {
        return Err(illegal("payment", p.status.as_str(), to.as_str()));
    }
    let n = sqlx::query(
        "UPDATE finance.payment SET status = $3, status_reason = $4, \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $6",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(process)
    .bind(p.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::VersionConflict {
            expected: p.version,
            actual: p.version + 1,
        });
    }
    history(
        conn,
        p.tenant_id,
        p.institution_id,
        "payment",
        p.id.as_uuid(),
        Some(p.status.as_str()),
        to.as_str(),
        &StatusChange {
            cause_event_type: cause,
            reason,
            process_instance_id: process,
        },
        actor,
    )
    .await
}

/// How a completed payment's money arrived.
pub fn debit_account(channel: PaymentChannel) -> SystemAccount {
    match channel {
        PaymentChannel::Gateway => SystemAccount::GatewayClearing,
        PaymentChannel::Cash => SystemAccount::CashOnHand,
        _ => SystemAccount::BankMain,
    }
}

/// Explicit allocation line requested by the caller.
#[derive(Clone, Copy, Debug)]
pub struct AllocationLine {
    pub charge_id: ChargeId,
    pub amount: Decimal,
}

/// Settles the planned / requested lines on locked charges of `account`; returns the lines
/// actually applied. Over-allocation of a charge is a 409 (`apply_settlement`).
#[allow(clippy::too_many_arguments)]
async fn settle_lines(
    conn: &mut PgConnection,
    tenant: TenantId,
    account: StudentAccountId,
    available: Decimal,
    explicit: Option<&[AllocationLine]>,
    ordered: &[ChargeId],
    cause: &'static str,
    actor: &Actor,
) -> Result<Vec<(ChargeId, Decimal)>, KernelError> {
    let lines: Vec<(ChargeId, Decimal)> = match explicit {
        Some(req) => {
            let sum: Decimal = req.iter().map(|l| l.amount).sum();
            if sum > available {
                return Err(KernelError::Conflict(format!(
                    "allocations total {sum} but only {available} is available"
                )));
            }
            req.iter().map(|l| (l.charge_id, l.amount)).collect()
        }
        None => {
            let mut open = Vec::new();
            for id in ordered {
                let c = lock_charge(conn, tenant, *id).await?;
                if c.student_account_id == account && c.status.accepts_settlement() {
                    open.push(OpenCharge {
                        id: c.id,
                        outstanding: c.outstanding_amount,
                    });
                }
            }
            plan_allocation(&open, available).lines
        }
    };
    for (charge_id, amount) in &lines {
        let c = lock_charge(conn, tenant, *charge_id).await?;
        if c.student_account_id != account {
            return Err(KernelError::Conflict(format!(
                "charge {} belongs to another party account",
                c.charge_number
            )));
        }
        apply_settlement(conn, &c, *amount, cause, actor).await?;
    }
    Ok(lines)
}

async fn insert_allocation(
    conn: &mut PgConnection,
    p: &PaymentRow,
    charge: ChargeId,
    amount: Decimal,
    source_kind: &str,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    sqlx::query_scalar(
        "INSERT INTO finance.payment_allocation (tenant_id, institution_id, payment_id, charge_id, \
         student_account_id, amount, source, allocated_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.institution_id.as_uuid())
    .bind(p.id.as_uuid())
    .bind(charge.as_uuid())
    .bind(p.student_account_id.as_uuid())
    .bind(amount)
    .bind(source_kind)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Completes a payment (L3-F-007/008): allocates (explicit lines, else the payer's target
/// charges, else every open charge oldest due first), posts Dr bank / cash / gateway clearing
/// · Cr receivables per charge · Cr unapplied receipts for the surplus, issues the receipt and
/// emits `payment.completed` + `receipt.issued`. Idempotent: an already completed payment is
/// returned unchanged.
#[allow(clippy::too_many_arguments)]
pub async fn complete(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &PaymentRow,
    explicit: Option<&[AllocationLine]>,
    ordered: &[ChargeId],
    gateway_payment_ref: Option<&str>,
    posting_date: NaiveDate,
    actor: &Actor,
) -> Result<PaymentRow, KernelError> {
    if p.status == PaymentStatus::Completed {
        return Ok(p.clone());
    }
    if !can_transition(p.status, PaymentStatus::Completed) {
        return Err(illegal("payment", p.status.as_str(), "completed"));
    }
    let lines = settle_lines(
        conn,
        p.tenant_id,
        p.student_account_id,
        p.amount,
        explicit,
        ordered,
        PAYMENT_COMPLETED,
        actor,
    )
    .await?;
    let allocated: Decimal = lines.iter().map(|(_, a)| *a).sum();
    let unapplied = p.amount - allocated;
    let mut posting_lines =
        vec![
            PostingLine::debit(AccountRef::System(debit_account(p.channel)), p.amount)
                .for_charge(p.student_account_id, None)
                .memo(p.payment_number.clone()),
        ];
    for (charge, amount) in &lines {
        posting_lines.push(
            PostingLine::credit(
                AccountRef::System(SystemAccount::StudentReceivables),
                *amount,
            )
            .for_charge(p.student_account_id, Some(*charge)),
        );
    }
    if unapplied > Decimal::ZERO {
        posting_lines.push(
            PostingLine::credit(
                AccountRef::System(SystemAccount::UnappliedReceipts),
                unapplied,
            )
            .for_charge(p.student_account_id, None)
            .memo("unapplied receipt"),
        );
    }
    let key = if p.channel == PaymentChannel::Gateway {
        "capture"
    } else {
        "receive"
    };
    let entry = posting::post(
        conn,
        p.tenant_id,
        &PostingRequest {
            institution_id: p.institution_id,
            posting_date,
            kind: JournalEntryKind::Standard,
            source_type: source::PAYMENT.into(),
            source_id: p.id.as_uuid(),
            posting_key: key.into(),
            description: format!("Payment {} received", p.payment_number),
            currency: CurrencyCode::INR,
            lines: posting_lines,
        },
        actor,
    )
    .await?;
    for (charge, amount) in &lines {
        let id = insert_allocation(conn, p, *charge, *amount, "capture", actor).await?;
        sqlx::query("UPDATE finance.payment_allocation SET journal_entry_id = $2 WHERE id = $1")
            .bind(id)
            .bind(entry.journal_entry_id.as_uuid())
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
    }
    move_status(
        conn,
        p,
        PaymentStatus::Completed,
        None,
        PAYMENT_COMPLETED,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE finance.payment SET unapplied_amount = $3, journal_entry_id = $4, \
         gateway_payment_ref = COALESCE($5, gateway_payment_ref), completed_at = now(), \
         failure_code = NULL WHERE tenant_id = $1 AND id = $2",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .bind(unapplied)
    .bind(entry.journal_entry_id.as_uuid())
    .bind(gateway_payment_ref)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (receipt_id, receipt_number) = issue_receipt(conn, p).await?;
    let subject = subject_ref(conn, p.tenant_id, p.student_account_id).await?;
    let done = load_payment(conn, p.tenant_id, p.id.as_uuid(), false).await?;
    let payload = Changed::new(
        p.id.as_uuid(),
        p.institution_id,
        PaymentStatus::Completed.as_str(),
        done.version,
    )
    .from(p.status.as_str())
    .account(p.student_account_id, subject.clone())
    .amount(p.amount, &p.currency)
    .reason(Some(&p.payment_number))
    .journal(Some(entry.journal_entry_id.as_uuid()));
    event(
        conn,
        scope,
        PAYMENT_COMPLETED,
        PAYMENT_COMPLETED_SCHEMA,
        p.id.to_string(),
        &payload.v,
        actor,
    )
    .await?;
    let receipt_payload = Changed::new(receipt_id, p.institution_id, "issued", 1)
        .account(p.student_account_id, subject)
        .amount(p.amount, &p.currency)
        .reason(Some(&receipt_number))
        .journal(Some(entry.journal_entry_id.as_uuid()));
    event(
        conn,
        scope,
        RECEIPT_ISSUED,
        RECEIPT_ISSUED_SCHEMA,
        receipt_id.to_string(),
        &receipt_payload.v,
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        "finance.payment.complete",
        "payment",
        p.id.to_string(),
        json!({"paymentNumber": p.payment_number, "amount": crate::shared::money::fmt(p.amount),
               "allocated": crate::shared::money::fmt(allocated), "unapplied": crate::shared::money::fmt(unapplied),
               "receiptNumber": receipt_number, "journalEntryId": entry.journal_entry_id}),
        actor,
    )
    .await?;
    Ok(done)
}

/// Issues the receipt (`RCT-` number) of a completed payment.
pub async fn issue_receipt(
    conn: &mut PgConnection,
    p: &PaymentRow,
) -> Result<(Uuid, String), KernelError> {
    let receipt_number = next(conn, p.tenant_id, p.institution_id, kind::RECEIPT).await?;
    let receipt_id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.receipt (tenant_id, institution_id, payment_id, student_account_id, \
         receipt_number, amount) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.institution_id.as_uuid())
    .bind(p.id.as_uuid())
    .bind(p.student_account_id.as_uuid())
    .bind(&receipt_number)
    .bind(p.amount)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok((receipt_id, receipt_number))
}

/// Moves an open payment to `failed`, `expired` or `cancelled` (no money moved, nothing posted).
pub async fn close_unpaid(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &PaymentRow,
    to: PaymentStatus,
    reason: &str,
    failure_code: Option<&str>,
    actor: &Actor,
) -> Result<PaymentRow, KernelError> {
    let (ev, schema, action) = match to {
        PaymentStatus::Cancelled => (
            PAYMENT_CANCELLED,
            PAYMENT_CANCELLED_SCHEMA,
            "finance.payment.cancel",
        ),
        PaymentStatus::Failed => (
            PAYMENT_FAILED,
            PAYMENT_FAILED_SCHEMA,
            "finance.payment.fail",
        ),
        _ => (
            PAYMENT_FAILED,
            PAYMENT_FAILED_SCHEMA,
            "finance.payment.expire",
        ),
    };
    if !matches!(p.status, PaymentStatus::Initiated | PaymentStatus::Pending) {
        return Err(illegal("payment", p.status.as_str(), to.as_str()));
    }
    move_status(conn, p, to, Some(reason), ev, None, actor).await?;
    sqlx::query("UPDATE finance.payment SET failure_code = $3 WHERE tenant_id = $1 AND id = $2")
        .bind(p.tenant_id.as_uuid())
        .bind(p.id.as_uuid())
        .bind(failure_code)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let subject = subject_ref(conn, p.tenant_id, p.student_account_id).await?;
    let done = load_payment(conn, p.tenant_id, p.id.as_uuid(), false).await?;
    let payload = Changed::new(p.id.as_uuid(), p.institution_id, to.as_str(), done.version)
        .from(p.status.as_str())
        .account(p.student_account_id, subject)
        .amount(p.amount, &p.currency)
        .reason(Some(reason));
    event(conn, scope, ev, schema, p.id.to_string(), &payload.v, actor).await?;
    audit(
        conn,
        scope,
        action,
        "payment",
        p.id.to_string(),
        json!({"paymentNumber": p.payment_number, "reason": reason, "failureCode": failure_code}),
        actor,
    )
    .await?;
    Ok(done)
}

/// Allocates (part of) the unapplied amount of a completed payment (L3-F-008/009): Dr unapplied
/// receipts / Cr receivables per allocation (`payment_allocation` / `allocate`).
pub async fn allocate_unapplied(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &PaymentRow,
    explicit: Option<&[AllocationLine]>,
    ordered: &[ChargeId],
    posting_date: NaiveDate,
    actor: &Actor,
) -> Result<PaymentRow, KernelError> {
    if p.status != PaymentStatus::Completed {
        return Err(KernelError::Conflict(format!(
            "only a completed payment can be allocated (payment is {})",
            p.status
        )));
    }
    if p.unapplied_amount <= Decimal::ZERO {
        return Err(KernelError::Conflict(
            "the payment has no unapplied amount left".into(),
        ));
    }
    let lines = settle_lines(
        conn,
        p.tenant_id,
        p.student_account_id,
        p.unapplied_amount,
        explicit,
        ordered,
        PAYMENT_COMPLETED,
        actor,
    )
    .await?;
    if lines.is_empty() {
        return Err(KernelError::Conflict(
            "no open charge of the account accepts a settlement".into(),
        ));
    }
    let mut total = Decimal::ZERO;
    for (charge, amount) in &lines {
        let id = insert_allocation(conn, p, *charge, *amount, "unapplied", actor).await?;
        let entry = posting::post(
            conn,
            p.tenant_id,
            &PostingRequest {
                institution_id: p.institution_id,
                posting_date,
                kind: JournalEntryKind::Standard,
                source_type: source::PAYMENT_ALLOCATION.into(),
                source_id: id,
                posting_key: "allocate".into(),
                description: format!("Unapplied receipt of {} allocated", p.payment_number),
                currency: CurrencyCode::INR,
                lines: vec![
                    PostingLine::debit(
                        AccountRef::System(SystemAccount::UnappliedReceipts),
                        *amount,
                    )
                    .for_charge(p.student_account_id, None),
                    PostingLine::credit(
                        AccountRef::System(SystemAccount::StudentReceivables),
                        *amount,
                    )
                    .for_charge(p.student_account_id, Some(*charge)),
                ],
            },
            actor,
        )
        .await?;
        sqlx::query("UPDATE finance.payment_allocation SET journal_entry_id = $2 WHERE id = $1")
            .bind(id)
            .bind(entry.journal_entry_id.as_uuid())
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
        total += amount;
    }
    let n = sqlx::query(
        "UPDATE finance.payment SET unapplied_amount = unapplied_amount - $3, version = version + 1, \
         updated_at = now() WHERE tenant_id = $1 AND id = $2 AND version = $4 AND unapplied_amount >= $3",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .bind(total)
    .bind(p.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n != 1 {
        return Err(KernelError::VersionConflict {
            expected: p.version,
            actual: p.version + 1,
        });
    }
    audit(
        conn,
        scope,
        "finance.payment.allocate",
        "payment",
        p.id.to_string(),
        json!({"paymentNumber": p.payment_number, "allocated": crate::shared::money::fmt(total),
               "lines": lines.iter().map(|(c, a)| json!({"chargeId": c, "amount": crate::shared::money::fmt(*a)})).collect::<Vec<_>>()}),
        actor,
    )
    .await?;
    load_payment(conn, p.tenant_id, p.id.as_uuid(), false).await
}

/// Reverses a completed payment (L3-F-011): un-settles every live allocation, posts the mirror of
/// the receipt entry and of each later allocation entry, cancels the receipt and moves the
/// payment to `reversed` / `charged_back`. Refused while a refund holds part of its credit.
#[allow(clippy::too_many_arguments)]
pub async fn reverse(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &PaymentRow,
    to: PaymentStatus,
    reason: &str,
    process: Option<&str>,
    posting_date: NaiveDate,
    actor: &Actor,
) -> Result<PaymentRow, KernelError> {
    if !can_transition(p.status, to) {
        return Err(illegal("payment", p.status.as_str(), to.as_str()));
    }
    let live_refunds: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.refund WHERE tenant_id = $1 \
         AND status IN ('approved', 'processing', 'completed', 'failed') \
         AND (payment_id = $2 OR sources @> jsonb_build_array(jsonb_build_object('paymentId', $2::text)))",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if live_refunds > 0 {
        return Err(KernelError::Conflict(
            "a refund was paid or approved from this payment; reverse or cancel the refund first"
                .into(),
        ));
    }
    let allocations: Vec<(Uuid, Uuid, Decimal, Option<Uuid>, String)> = sqlx::query_as(
        "SELECT id, charge_id, amount, journal_entry_id, source FROM finance.payment_allocation \
         WHERE tenant_id = $1 AND payment_id = $2 AND status = 'allocated' ORDER BY created_at, id",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (id, charge, amount, entry, src) in &allocations {
        let c = lock_charge(conn, p.tenant_id, ChargeId(*charge)).await?;
        apply_settlement(conn, &c, -*amount, PAYMENT_REVERSED, actor).await?;
        sqlx::query(
            "UPDATE finance.payment_allocation SET status = 'reversed', reversed_at = now() WHERE id = $1",
        )
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if src == "unapplied" {
            if let Some(e) = entry {
                posting::reverse(
                    conn,
                    p.tenant_id,
                    JournalEntryId(*e),
                    posting_date,
                    reason,
                    actor,
                )
                .await?;
            }
        }
    }
    let mut reversal = None;
    if let Some(e) = p.journal_entry_id {
        reversal = Some(
            posting::reverse(
                conn,
                p.tenant_id,
                JournalEntryId(e),
                posting_date,
                reason,
                actor,
            )
            .await?
            .journal_entry_id
            .as_uuid(),
        );
    }
    move_status(conn, p, to, Some(reason), PAYMENT_REVERSED, process, actor).await?;
    sqlx::query(
        "UPDATE finance.payment SET unapplied_amount = 0, reversal_entry_id = $3 \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .bind(reversal)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE finance.receipt SET status = 'cancelled', cancelled_at = now(), cancel_reason = $3, \
         version = version + 1 WHERE tenant_id = $1 AND payment_id = $2 AND status = 'issued'",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let subject = subject_ref(conn, p.tenant_id, p.student_account_id).await?;
    let done = load_payment(conn, p.tenant_id, p.id.as_uuid(), false).await?;
    let payload = Changed::new(p.id.as_uuid(), p.institution_id, to.as_str(), done.version)
        .from(p.status.as_str())
        .account(p.student_account_id, subject)
        .amount(p.amount, &p.currency)
        .reason(Some(reason))
        .process(process)
        .journal(reversal);
    event(
        conn,
        scope,
        PAYMENT_REVERSED,
        PAYMENT_REVERSED_SCHEMA,
        p.id.to_string(),
        &payload.v,
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        if to == PaymentStatus::ChargedBack {
            "finance.payment.chargeback"
        } else {
            "finance.payment.reverse"
        },
        "payment",
        p.id.to_string(),
        json!({"paymentNumber": p.payment_number, "reason": reason, "reversalEntryId": reversal,
               "allocationsReversed": allocations.len()}),
        actor,
    )
    .await?;
    Ok(done)
}
