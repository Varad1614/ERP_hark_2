//! Wire views of slice 5B payments and receipts (contract §5.3; amounts are decimal strings).

use super::service::PaymentRow;
use crate::shared::money::fmt;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::ids::{PaymentId, ReceiptId, StudentAccountId};
use sutra_contracts::finance::status::{
    AllocationStatus, PaymentChannel, PaymentStatus, ReceiptStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// One allocation of a payment to a charge.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AllocationView {
    pub id: Uuid,
    pub charge_id: Uuid,
    pub charge_number: String,
    #[schema(value_type = String, example = "1250.00")]
    pub amount: String,
    /// `capture` (with the receipt) or `unapplied` (allocated later).
    pub source: String,
    pub status: AllocationStatus,
    pub journal_entry_id: Option<Uuid>,
}

/// Receipt reference on a payment.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptRef {
    pub id: ReceiptId,
    pub receipt_number: String,
    pub status: ReceiptStatus,
    pub issued_at: DateTime<Utc>,
}

/// A payment (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PaymentView {
    pub id: PaymentId,
    pub payment_number: String,
    pub institution_id: Uuid,
    pub student_account_id: StudentAccountId,
    pub account_name: Option<String>,
    pub channel: PaymentChannel,
    pub payer_kind: String,
    pub payer_name: Option<String>,
    pub external_ref: Option<String>,
    #[schema(value_type = String, example = "1250.00")]
    pub amount: String,
    pub currency: String,
    pub status: PaymentStatus,
    pub status_reason: Option<String>,
    pub gateway: Option<String>,
    pub gateway_order_ref: Option<String>,
    pub gateway_payment_ref: Option<String>,
    /// Mock checkout page of a gateway payment (open it, then confirm).
    pub checkout_url: Option<String>,
    pub failure_code: Option<String>,
    pub received_on: NaiveDate,
    pub completed_at: Option<DateTime<Utc>>,
    pub allocations: Vec<AllocationView>,
    #[schema(value_type = String, example = "0.00")]
    pub unapplied_amount: String,
    pub receipt: Option<ReceiptRef>,
    pub journal_entry_id: Option<Uuid>,
    pub reversal_entry_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub unidentified_receipt_id: Option<Uuid>,
    pub batch_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

/// A page of payments.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PaymentList {
    pub items: Vec<PaymentView>,
    /// Pass as `cursor` to get the next page; absent on the last page.
    pub next_cursor: Option<Uuid>,
}

/// One receipt with its allocations (printable).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptView {
    pub id: ReceiptId,
    pub receipt_number: String,
    pub payment_id: PaymentId,
    pub payment_number: String,
    pub student_account_id: StudentAccountId,
    pub account_name: Option<String>,
    pub student_number: Option<String>,
    pub channel: PaymentChannel,
    #[schema(value_type = String, example = "1250.00")]
    pub amount: String,
    pub currency: String,
    pub status: ReceiptStatus,
    pub issued_at: DateTime<Utc>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
    pub allocations: Vec<AllocationView>,
    #[schema(value_type = String, example = "0.00")]
    pub unapplied_amount: String,
    /// "₹1,25,000.00" — amount in Indian grouping for printing.
    pub amount_display: String,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReceiptList {
    pub items: Vec<ReceiptView>,
}

/// One requested allocation line (`chargeId` + decimal string amount).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AllocationInput {
    pub charge_id: Uuid,
    #[schema(value_type = String, example = "1000.00")]
    pub amount: String,
}

pub async fn allocations(
    conn: &mut PgConnection,
    tenant: TenantId,
    payment: Uuid,
) -> Result<Vec<AllocationView>, KernelError> {
    let rows = sqlx::query(
        "SELECT a.id, a.charge_id, c.charge_number, a.amount, a.source, a.status, a.journal_entry_id \
         FROM finance.payment_allocation a JOIN finance.charge c ON c.id = a.charge_id \
         WHERE a.tenant_id = $1 AND a.payment_id = $2 ORDER BY a.created_at, a.id",
    )
    .bind(tenant.as_uuid())
    .bind(payment)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let status: String = r.try_get("status")?;
            let amount: Decimal = r.try_get("amount")?;
            Ok(AllocationView {
                id: r.try_get("id")?,
                charge_id: r.try_get("charge_id")?,
                charge_number: r.try_get("charge_number")?,
                amount: fmt(amount),
                source: r.try_get("source")?,
                status: AllocationStatus::parse(&status).unwrap_or(AllocationStatus::Allocated),
                journal_entry_id: r.try_get("journal_entry_id")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

/// Full view of one payment row.
pub async fn payment_view(
    conn: &mut PgConnection,
    p: PaymentRow,
) -> Result<PaymentView, KernelError> {
    let allocations = allocations(conn, p.tenant_id, p.id.as_uuid()).await?;
    let receipt = sqlx::query(
        "SELECT id, receipt_number, status, issued_at FROM finance.receipt WHERE tenant_id = $1 AND payment_id = $2",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(|r| -> Result<ReceiptRef, sqlx::Error> {
        let status: String = r.try_get("status")?;
        Ok(ReceiptRef {
            id: ReceiptId(r.try_get("id")?),
            receipt_number: r.try_get("receipt_number")?,
            status: ReceiptStatus::parse(&status).unwrap_or(ReceiptStatus::Issued),
            issued_at: r.try_get("issued_at")?,
        })
    })
    .transpose()
    .map_err(map_db_error)?;
    let account_name: Option<String> = sqlx::query_scalar(
        "SELECT display_name FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(p.tenant_id.as_uuid())
    .bind(p.student_account_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(PaymentView {
        id: p.id,
        payment_number: p.payment_number,
        institution_id: p.institution_id.as_uuid(),
        student_account_id: p.student_account_id,
        account_name,
        channel: p.channel,
        payer_kind: p.payer_kind,
        payer_name: p.payer_name,
        external_ref: p.external_ref,
        amount: fmt(p.amount),
        currency: p.currency,
        status: p.status,
        status_reason: p.status_reason,
        gateway: p.gateway,
        gateway_order_ref: p.gateway_order_ref,
        gateway_payment_ref: p.gateway_payment_ref,
        checkout_url: p.checkout_url,
        failure_code: p.failure_code,
        received_on: p.received_on,
        completed_at: p.completed_at,
        allocations,
        unapplied_amount: fmt(p.unapplied_amount),
        receipt,
        journal_entry_id: p.journal_entry_id,
        reversal_entry_id: p.reversal_entry_id,
        process_instance_id: p.process_instance_id,
        unidentified_receipt_id: p.unidentified_receipt_id,
        batch_id: p.batch_id,
        created_at: p.created_at,
        version: p.version,
    })
}

/// Receipt view by receipt id or payment id.
pub async fn receipt_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    receipt: Option<Uuid>,
    payment: Option<Uuid>,
) -> Result<ReceiptView, KernelError> {
    let r = sqlx::query(
        "SELECT r.id, r.receipt_number, r.payment_id, p.payment_number, r.student_account_id, \
         a.display_name, a.student_number, p.channel, r.amount, r.currency, r.status, r.issued_at, \
         r.cancelled_at, r.cancel_reason, p.unapplied_amount \
         FROM finance.receipt r JOIN finance.payment p ON p.id = r.payment_id \
         JOIN finance.student_account a ON a.id = r.student_account_id \
         WHERE r.tenant_id = $1 AND ($2::uuid IS NULL OR r.id = $2) AND ($3::uuid IS NULL OR r.payment_id = $3)",
    )
    .bind(tenant.as_uuid())
    .bind(receipt)
    .bind(payment)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "receipt" })?;
    let map = |r: &sqlx::postgres::PgRow| -> Result<(ReceiptView, Uuid), sqlx::Error> {
        let status: String = r.try_get("status")?;
        let channel: String = r.try_get("channel")?;
        let amount: Decimal = r.try_get("amount")?;
        let unapplied: Decimal = r.try_get("unapplied_amount")?;
        let payment_id: Uuid = r.try_get("payment_id")?;
        Ok((
            ReceiptView {
                id: ReceiptId(r.try_get("id")?),
                receipt_number: r.try_get("receipt_number")?,
                payment_id: PaymentId(payment_id),
                payment_number: r.try_get("payment_number")?,
                student_account_id: StudentAccountId(r.try_get("student_account_id")?),
                account_name: r.try_get("display_name")?,
                student_number: r.try_get("student_number")?,
                channel: PaymentChannel::parse(&channel).unwrap_or(PaymentChannel::Cash),
                amount: fmt(amount),
                currency: r.try_get("currency")?,
                status: ReceiptStatus::parse(&status).unwrap_or(ReceiptStatus::Issued),
                issued_at: r.try_get("issued_at")?,
                cancelled_at: r.try_get("cancelled_at")?,
                cancel_reason: r.try_get("cancel_reason")?,
                allocations: Vec::new(),
                unapplied_amount: fmt(unapplied),
                amount_display: crate::shared::money::display_inr(amount),
            },
            payment_id,
        ))
    };
    let (mut v, payment_id) = map(&r).map_err(map_db_error)?;
    v.allocations = allocations(conn, tenant, payment_id).await?;
    Ok(v)
}
