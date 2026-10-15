//! **8D** — vendor invoices matched three-way against PO + receipt, posted to Finance AP through `ProcurementPostingPort`; vendor payments (F-044, F-045, F-081, F-082). Operation ids `resources_payable_*`, tag `resources-procurement`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * record a supplier invoice (or credit note) against an issued order (`VINV-` number; the
//!   supplier's invoice number is unique per supplier) → `received`;
//! * **three-way match** (F-081): per line, invoiced quantity (with what other live invoices
//!   already claim) ≤ accepted receipt quantity, unit price within [`PRICE_TOLERANCE_PERCENT`] of
//!   the order price; credit notes ≤ what was invoiced → `matched` (then
//!   `VENDOR_INVOICE_APPROVAL` → `approved`) or `mismatch` with findings (`matchFindings`), which
//!   blocks posting until a later match succeeds;
//! * post (F-044 / F-082): `state.postings.post_procurement(VendorInvoice)` — stock + asset lines
//!   → `inventory_amount` (1410), service lines → `expense_amount` (5510), Cr 2310; credit notes
//!   negative; idempotent on `(procurement_invoice, invoice, "post")`; stores the journal entry
//!   (`journalEntryId`, `entryNumber`), raises the order lines' invoiced quantity,
//!   `vendor_invoice.posted`;
//! * vendor payments (F-045): allocations to posted invoices of one supplier (≤ outstanding) →
//!   `draft` (`VPAY-`), post → `VendorPayment` (Dr 2310 / Cr bank) → `posted`
//!   (`vendor_payment.posted`); an invoice becomes `paid` once fully settled.

use crate::orders;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers;
use crate::suppliers::common::{
    audit_only, check_version, clean, emit, expected, history, illegal, institution_of, invalid,
    line_amount, optional_text, parse_money, parse_qty, required_text, slice_status,
    status_payload, ResourcesDecision, ResourcesDecisionBody, ResourcesReasonBody,
    ResourcesStatusQuery,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::finance::ports::{ProcurementPostingKind, ProcurementPostingRequest};
use sutra_contracts::resources::events::procurement as ev;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Unit price tolerance of the three-way match (percent of the order price).
pub const PRICE_TOLERANCE_PERCENT: i64 = 1;

slice_status!(
    /// Vendor invoice lifecycle (F-044 / 081).
    VendorInvoiceStatus {
        Received => "received",
        Matched => "matched",
        Mismatch => "mismatch",
        Approved => "approved",
        Posted => "posted",
        Paid => "paid",
        Cancelled => "cancelled",
    }
);

slice_status!(
    /// Vendor payment lifecycle (F-045).
    VendorPaymentStatus {
        Draft => "draft",
        Posted => "posted",
    }
);

impl VendorInvoiceStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use VendorInvoiceStatus::*;
        matches!(
            (self, to),
            (Received, Matched)
                | (Received, Mismatch)
                | (Mismatch, Matched)
                | (Mismatch, Mismatch)
                | (Matched, Approved)
                | (Matched, Cancelled)
                | (Received, Cancelled)
                | (Mismatch, Cancelled)
                | (Approved, Posted)
                | (Posted, Paid)
        )
    }
}

/// Whether `price` is within the tolerance of the order price.
pub fn price_within_tolerance(price: Decimal, order_price: Decimal) -> bool {
    let tolerance =
        crate::shared::decimal::round2(order_price * Decimal::new(PRICE_TOLERANCE_PERCENT, 2));
    (price - order_price).abs() <= tolerance
}

/// One three-way match finding.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VendorInvoiceFinding {
    pub line_no: i32,
    /// `quantity_exceeds_received | price_variance | credit_exceeds_invoiced`.
    pub code: String,
    pub message: String,
}

/// A vendor invoice / credit note (header; signed amounts, credit notes negative).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VendorInvoiceView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "VINV-000001")]
    pub number: String,
    pub supplier_id: Uuid,
    pub supplier_number: String,
    pub supplier_name: String,
    pub purchase_order_id: Uuid,
    pub purchase_order_number: String,
    pub supplier_invoice_number: String,
    pub is_credit_note: bool,
    pub invoice_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    #[schema(value_type = String, example = "2400.00")]
    pub inventory_amount: Decimal,
    #[schema(value_type = String, example = "0.00")]
    pub expense_amount: Decimal,
    #[schema(value_type = String, example = "2400.00")]
    pub total_amount: Decimal,
    #[schema(value_type = String, example = "0.00")]
    pub paid_amount: Decimal,
    /// `received | matched | mismatch | approved | posted | paid | cancelled`.
    pub status: String,
    pub status_reason: Option<String>,
    #[schema(value_type = Vec<VendorInvoiceFinding>)]
    pub match_findings: serde_json::Value,
    pub matched_at: Option<DateTime<Utc>>,
    /// Deprecated (Batch 9 §6.4): read `approvalState`; kept for existing clients.
    pub approval_pending: bool,
    /// 9E: approvalState — canonical approval state (`not_requested | pending | approved |
    /// rejected`, contract batch9 §6.4), derived from the status + `approvalPending`.
    pub approval_state: String,
    pub process_instance_id: Option<String>,
    /// Finance journal entry of the posting.
    pub journal_entry_id: Option<Uuid>,
    #[schema(example = "JE-000012")]
    pub entry_number: Option<String>,
    pub posted_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl VendorInvoiceView {
    pub fn status(&self) -> Result<VendorInvoiceStatus, KernelError> {
        VendorInvoiceStatus::parse(&self.status).ok_or_else(|| {
            KernelError::Conflict(format!("unknown vendor invoice status {}", self.status))
        })
    }
}

/// One invoiced order line, with what the order and receipts say.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VendorInvoiceLineView {
    pub line_no: i32,
    pub purchase_order_line_id: Uuid,
    pub purchase_order_line_no: i32,
    /// `stock | asset | service`.
    pub line_kind: String,
    pub description: String,
    #[schema(value_type = String, example = "10.000")]
    pub quantity: Decimal,
    #[schema(value_type = String, example = "240.00")]
    pub unit_price: Decimal,
    #[schema(value_type = String, example = "2400.00")]
    pub amount: Decimal,
    #[schema(value_type = String, example = "10.000")]
    pub ordered_quantity: Decimal,
    #[schema(value_type = String, example = "240.00")]
    pub order_unit_price: Decimal,
    #[schema(value_type = String, example = "10.000")]
    pub received_quantity: Decimal,
    #[schema(value_type = String, example = "0.000")]
    pub invoiced_quantity: Decimal,
}

/// A vendor invoice with its lines.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VendorInvoiceDetail {
    #[serde(flatten)]
    pub invoice: VendorInvoiceView,
    pub lines: Vec<VendorInvoiceLineView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VendorInvoiceList {
    pub items: Vec<VendorInvoiceView>,
}

const SELECT: &str = "SELECT v.id, v.institution_id, v.number, v.supplier_id, \
    s.number AS supplier_number, s.legal_name AS supplier_name, v.purchase_order_id, \
    o.number AS purchase_order_number, v.supplier_invoice_number, v.is_credit_note, v.invoice_date, \
    v.due_date, v.inventory_amount, v.expense_amount, v.total_amount, v.paid_amount, v.status, \
    v.status_reason, v.match_findings, v.matched_at, v.approval_pending, v.process_instance_id, \
    v.journal_entry_id, v.entry_number, v.posted_at, v.version, v.created_at, \
    CASE WHEN v.approval_pending THEN 'pending' \
         WHEN v.status IN ('approved', 'posted', 'paid') THEN 'approved' \
         ELSE 'not_requested' END AS approval_state /* 9E: approvalState */ \
    FROM resources.vendor_invoice v JOIN resources.supplier s ON s.id = v.supplier_id \
    JOIN resources.purchase_order o ON o.id = v.purchase_order_id";

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<VendorInvoiceView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF v" } else { "" };
    sqlx::query_as::<_, VendorInvoiceView>(&format!(
        "{SELECT} WHERE v.id = $1 AND v.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "vendor invoice",
    })
}

async fn lines_of(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<VendorInvoiceLineView>, KernelError> {
    sqlx::query_as::<_, VendorInvoiceLineView>(
        "SELECT vl.line_no, vl.purchase_order_line_id, pl.line_no AS purchase_order_line_no, \
         vl.line_kind, pl.description, vl.quantity, vl.unit_price, vl.amount, \
         pl.quantity AS ordered_quantity, pl.unit_price AS order_unit_price, pl.received_quantity, \
         pl.invoiced_quantity FROM resources.vendor_invoice_line vl \
         JOIN resources.purchase_order_line pl ON pl.id = vl.purchase_order_line_id \
         WHERE vl.vendor_invoice_id = $1 ORDER BY vl.line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn respond(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let invoice = load(conn, tenant, id, false).await?;
    let lines = lines_of(conn, id).await?;
    let v = invoice.version;
    Ok(with_etag(
        status,
        &VendorInvoiceDetail { invoice, lines },
        Version(v),
    ))
}

async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    inv: &VendorInvoiceView,
    to: VendorInvoiceStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = inv.status()?;
    if !from.can_transition(to) {
        return Err(illegal(
            "vendor invoice",
            &inv.number,
            from.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE resources.vendor_invoice SET status = $2, status_reason = $3, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(inv.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(inv.institution_id),
        "vendor_invoice",
        inv.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        inv.process_instance_id.as_deref(),
        actor,
    )
    .await
}

async fn invoice_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(
        &mut conn,
        ctx.scope.tenant_id,
        "vendor_invoice",
        "vendor invoice",
        id,
    )
    .await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// One invoiced order line.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VendorInvoiceLineInput {
    /// Purchase order line number.
    pub line_no: i32,
    #[schema(example = "10")]
    pub quantity: String,
    #[schema(example = "240.00")]
    pub unit_price: String,
}

/// Record a supplier invoice or credit note.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VendorInvoiceCreate {
    pub purchase_order_id: Uuid,
    pub supplier_invoice_number: String,
    pub invoice_date: NaiveDate,
    pub due_date: Option<NaiveDate>,
    /// A credit note: quantities / prices positive, amounts post negative.
    #[serde(default)]
    pub credit_note: bool,
    pub lines: Vec<VendorInvoiceLineInput>,
}

/// Record a supplier invoice / credit note against an order (`resources.procurement.manage`;
/// Idempotency-Key; F-044): `received`; the supplier's invoice number is unique per supplier.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/vendor-invoices", operation_id = "resources_payable_invoice_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = VendorInvoiceCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = VendorInvoiceDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_invoice(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VendorInvoiceCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let supplier_ref = required_text("supplierInvoiceNumber", &b.supplier_invoice_number, 60)?;
    if b.lines.is_empty() {
        return Err(invalid("lines", "required", "at least one invoiced line").into());
    }
    if b.due_date.is_some_and(|d| d < b.invoice_date) {
        return Err(invalid(
            "dueDate",
            "before_invoice",
            "the due date precedes the invoice date",
        )
        .into());
    }
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = match orders::load(&mut tx, t, b.purchase_order_id, false).await {
        Ok(o) if o.institution_id == id => o,
        Ok(_) | Err(KernelError::NotFound { .. }) => {
            return Err(invalid(
                "purchaseOrderId",
                "unknown_order",
                "no such purchase order in the institution",
            )
            .into())
        }
        Err(e) => return Err(e.into()),
    };
    if matches!(o.status.as_str(), "draft" | "cancelled") {
        return Err(KernelError::Conflict(format!(
            "purchase order {} is {}; invoices are recorded against issued orders",
            o.number, o.status
        ))
        .into());
    }
    let po_lines = orders::lines_of(&mut tx, o.id).await?;
    let sign = if b.credit_note {
        -Decimal::ONE
    } else {
        Decimal::ONE
    };
    let mut inventory = Decimal::ZERO;
    let mut expense = Decimal::ZERO;
    let mut planned: Vec<(Uuid, String, Decimal, Decimal, Decimal)> = Vec::new();
    for (i, l) in b.lines.iter().enumerate() {
        let pl = po_lines
            .iter()
            .find(|p| p.line_no == l.line_no)
            .ok_or_else(|| {
                invalid(
                    &format!("lines[{i}].lineNo"),
                    "unknown_line",
                    "no such order line",
                )
            })?;
        if planned.iter().any(|p| p.0 == pl.id) {
            return Err(invalid(
                &format!("lines[{i}].lineNo"),
                "duplicate",
                "each order line once",
            )
            .into());
        }
        let quantity = parse_qty(&format!("lines[{i}].quantity"), &l.quantity)?;
        let price = parse_money(&format!("lines[{i}].unitPrice"), &l.unit_price)?;
        let amount = line_amount(quantity, price);
        if pl.line_kind == "service" {
            expense += sign * amount;
        } else {
            inventory += sign * amount;
        }
        planned.push((pl.id, pl.line_kind.clone(), quantity, price, amount));
    }
    let number = next(&mut tx, t, institution, kind::VENDOR_INVOICE).await?;
    let vid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.vendor_invoice (tenant_id, institution_id, number, supplier_id, \
         purchase_order_id, supplier_invoice_number, is_credit_note, invoice_date, due_date, \
         inventory_amount, expense_amount, total_amount) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(o.supplier_id)
    .bind(o.id)
    .bind(&supplier_ref)
    .bind(b.credit_note)
    .bind(b.invoice_date)
    .bind(b.due_date)
    .bind(inventory)
    .bind(expense)
    .bind(inventory + expense)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!(
            "supplier {} invoice {supplier_ref} is already recorded",
            o.supplier_number
        )),
        other => other,
    })?;
    for (n, (pl_id, line_kind, qty, price, amount)) in planned.iter().enumerate() {
        sqlx::query(
            "INSERT INTO resources.vendor_invoice_line (tenant_id, vendor_invoice_id, line_no, \
             purchase_order_line_id, line_kind, quantity, unit_price, amount) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(t.as_uuid())
        .bind(vid)
        .bind(n as i32 + 1)
        .bind(*pl_id)
        .bind(line_kind)
        .bind(*qty)
        .bind(*price)
        .bind(*amount)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        t,
        institution,
        "vendor_invoice",
        vid,
        None,
        "received",
        "sutraerp.resources.vendor_invoice.received",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "vendor_invoice",
        vid,
        "resources.vendor_invoice.create",
        serde_json::json!({ "number": number, "order": o.number, "supplierInvoice": supplier_ref,
            "creditNote": b.credit_note, "total": (inventory + expense).to_string() }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, vid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Vendor invoices of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/vendor-invoices", operation_id = "resources_payable_invoice_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = VendorInvoiceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_invoices(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<VendorInvoiceList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, VendorInvoiceView>(&format!(
        "{SELECT} WHERE v.tenant_id = $1 AND v.institution_id = $2 \
         AND ($3::text IS NULL OR v.status = $3) ORDER BY v.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(VendorInvoiceList { items }))
}

/// One vendor invoice with its lines (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/vendor-invoices/{id}", operation_id = "resources_payable_invoice_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = VendorInvoiceDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_invoice(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Three-way match findings of an invoice (pure over the loaded figures; unit-tested).
/// `claimed` = quantity other live invoices of the same order line already claim (net).
#[allow(clippy::too_many_arguments)]
pub fn match_line(
    line_no: i32,
    credit_note: bool,
    quantity: Decimal,
    price: Decimal,
    order_price: Decimal,
    received: Decimal,
    invoiced: Decimal,
    claimed: Decimal,
) -> Vec<VendorInvoiceFinding> {
    let mut out = Vec::new();
    if credit_note {
        if quantity > invoiced {
            out.push(VendorInvoiceFinding {
                line_no,
                code: "credit_exceeds_invoiced".into(),
                message: format!("credits {quantity} but only {invoiced} was invoiced"),
            });
        }
    } else if quantity + invoiced + claimed > received {
        out.push(VendorInvoiceFinding {
            line_no,
            code: "quantity_exceeds_received".into(),
            message: format!(
                "invoices {quantity} with {} already invoiced or claimed; only {received} accepted on receipts",
                invoiced + claimed
            ),
        });
    }
    if !price_within_tolerance(price, order_price) {
        out.push(VendorInvoiceFinding {
            line_no,
            code: "price_variance".into(),
            message: format!(
                "unit price {price} differs from the order price {order_price} by more than {PRICE_TOLERANCE_PERCENT}%"
            ),
        });
    }
    out
}

async fn approve_invoice(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let inv = load(conn, tenant, id, true).await?;
    set_status(
        conn,
        tenant,
        &inv,
        VendorInvoiceStatus::Approved,
        "sutraerp.resources.vendor_invoice.approved",
        None,
        actor,
    )
    .await?;
    sqlx::query("UPDATE resources.vendor_invoice SET approval_pending = false WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// Three-way match (`resources.procurement.manage`; If-Match; F-081): `matched` →
/// `VENDOR_INVOICE_APPROVAL` (auto-approval → `approved`), or `mismatch` with `matchFindings`.
#[utoipa::path(post, path = "/api/v1/resources/vendor-invoices/{id}/match", operation_id = "resources_payable_invoice_match", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the invoice")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VendorInvoiceDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn match_invoice(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = invoice_scope(&st, ctx, id).await?;
    let (inv, findings) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let inv = load(&mut conn, t, id, false).await?;
        check_version(v, inv.version)?;
        let from = inv.status()?;
        if !matches!(
            from,
            VendorInvoiceStatus::Received | VendorInvoiceStatus::Mismatch
        ) {
            return Err(illegal("vendor invoice", &inv.number, from.as_str(), "matched").into());
        }
        let mut findings = Vec::new();
        for l in lines_of(&mut conn, id).await? {
            let claimed: Option<Decimal> = sqlx::query_scalar(
                "SELECT sum(CASE WHEN v.is_credit_note THEN -vl.quantity ELSE vl.quantity END) \
                 FROM resources.vendor_invoice_line vl \
                 JOIN resources.vendor_invoice v ON v.id = vl.vendor_invoice_id \
                 WHERE vl.purchase_order_line_id = $1 AND v.id <> $2 \
                   AND v.status IN ('matched', 'approved')",
            )
            .bind(l.purchase_order_line_id)
            .bind(id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            findings.extend(match_line(
                l.line_no,
                inv.is_credit_note,
                l.quantity,
                l.unit_price,
                l.order_unit_price,
                l.received_quantity,
                l.invoiced_quantity,
                claimed.unwrap_or(Decimal::ZERO),
            ));
        }
        (inv, findings)
    };
    let outcome = if findings.is_empty() {
        let mut vars = BTreeMap::new();
        vars.insert("invoiceNumber".to_owned(), serde_json::json!(inv.number));
        vars.insert(
            "total".to_owned(),
            serde_json::json!(inv.total_amount.to_string()),
        );
        Some(
            request_approval(
                &st,
                &scope,
                process::VENDOR_INVOICE_APPROVAL,
                business_key("vendor_invoice", id, inv.version),
                vars,
            )
            .await?,
        )
    } else {
        None
    };
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let inv = load(&mut tx, t, id, true).await?;
    check_version(v, inv.version)?;
    let json = serde_json::to_value(&findings).map_err(KernelError::internal)?;
    let to = if findings.is_empty() {
        VendorInvoiceStatus::Matched
    } else {
        VendorInvoiceStatus::Mismatch
    };
    set_status(
        &mut tx,
        t,
        &inv,
        to,
        "sutraerp.resources.vendor_invoice.matched",
        None,
        &actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.vendor_invoice SET match_findings = $2, matched_at = now(), \
         approval_pending = $3, process_instance_id = COALESCE($4, process_instance_id) WHERE id = $1",
    )
    .bind(id)
    .bind(&json)
    .bind(outcome.is_some())
    .bind(outcome.as_ref().map(|o| o.process_instance_id().to_owned()))
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "vendor_invoice",
        id,
        "resources.vendor_invoice.match",
        serde_json::json!({ "number": inv.number, "result": to.as_str(), "findings": json }),
        &actor,
    )
    .await?;
    if let Some(ApprovalOutcome::Approved { .. }) = outcome {
        approve_invoice(&mut tx, t, id, &Actor::system(actor.correlation_id.clone())).await?;
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Decide a matched invoice awaiting approval (`resources.approve`, step-up; If-Match): approve
/// → `approved`; reject → `cancelled` (reason required; the supplier re-invoices).
#[utoipa::path(post, path = "/api/v1/resources/vendor-invoices/{id}/decision", operation_id = "resources_payable_invoice_decide", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the invoice")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VendorInvoiceDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide_invoice(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ResourcesDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == ResourcesDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = invoice_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let inv = load(&mut tx, t, id, true).await?;
    check_version(v, inv.version)?;
    if !inv.approval_pending || inv.status()? != VendorInvoiceStatus::Matched {
        return Err(illegal("vendor invoice", &inv.number, &inv.status, "decided").into());
    }
    match b.decision {
        ResourcesDecision::Approve => approve_invoice(&mut tx, t, id, &actor).await?,
        ResourcesDecision::Reject => {
            set_status(
                &mut tx,
                t,
                &inv,
                VendorInvoiceStatus::Cancelled,
                "sutraerp.resources.vendor_invoice.rejected",
                reason.as_deref(),
                &actor,
            )
            .await?;
            sqlx::query(
                "UPDATE resources.vendor_invoice SET approval_pending = false WHERE id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    audit_only(
        &mut tx,
        &scope,
        "vendor_invoice",
        id,
        "resources.vendor_invoice.decide",
        serde_json::json!({ "number": inv.number, "decision": b.decision, "reason": reason }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Post an approved invoice to Finance AP (`resources.procurement.manage`; If-Match; F-044 /
/// F-082): one balanced journal entry through `ProcurementPostingPort` (idempotent); a
/// mismatched or unapproved invoice → 409; `vendor_invoice.posted`.
#[utoipa::path(post, path = "/api/v1/resources/vendor-invoices/{id}/post", operation_id = "resources_payable_invoice_post", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the invoice")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VendorInvoiceDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn post_invoice(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = invoice_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let inv = load(&mut tx, t, id, true).await?;
    check_version(v, inv.version)?;
    if inv.status()? != VendorInvoiceStatus::Approved {
        return Err(KernelError::Conflict(format!(
            "vendor invoice {} is {}; only a matched and approved invoice posts",
            inv.number, inv.status
        ))
        .into());
    }
    let receipt = st
        .postings
        .post_procurement(
            &scope,
            ProcurementPostingRequest {
                institution_id: InstitutionId(inv.institution_id),
                kind: ProcurementPostingKind::VendorInvoice,
                source_id: id,
                posting_key: "post".into(),
                posting_date: inv.invoice_date,
                description: format!(
                    "{} {} {} ({})",
                    if inv.is_credit_note {
                        "Credit note"
                    } else {
                        "Vendor invoice"
                    },
                    inv.number,
                    inv.supplier_invoice_number,
                    inv.purchase_order_number
                ),
                supplier_label: format!("{} {}", inv.supplier_number, inv.supplier_name),
                inventory_amount: inv.inventory_amount,
                expense_amount: inv.expense_amount,
                payment_amount: Decimal::ZERO,
                actor: actor.id.clone(),
            },
        )
        .await?;
    set_status(
        &mut tx,
        t,
        &inv,
        VendorInvoiceStatus::Posted,
        ev::VENDOR_INVOICE_POSTED,
        None,
        &actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.vendor_invoice SET journal_entry_id = $2, entry_number = $3, \
         posted_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(receipt.journal_entry_id.as_uuid())
    .bind(&receipt.entry_number)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE resources.purchase_order_line pl SET invoiced_quantity = pl.invoiced_quantity + \
         CASE WHEN $2 THEN -vl.quantity ELSE vl.quantity END \
         FROM resources.vendor_invoice_line vl WHERE vl.vendor_invoice_id = $1 \
         AND vl.purchase_order_line_id = pl.id",
    )
    .bind(id)
    .bind(inv.is_credit_note)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = load(&mut tx, t, id, false).await?;
    emit(
        &mut tx,
        &scope,
        ev::VENDOR_INVOICE_POSTED,
        ev::VENDOR_INVOICE_POSTED_SCHEMA,
        "vendor_invoice",
        id,
        "resources.vendor_invoice.post",
        &status_payload(
            id,
            out.institution_id,
            Some("approved"),
            "posted",
            out.version,
            Some(&format!("{} {}", receipt.entry_number, out.total_amount)),
            out.process_instance_id.as_deref(),
            Some(&out.number),
            None,
        ),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

// ================================================================== payments

/// A vendor payment.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VendorPaymentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "VPAY-000001")]
    pub number: String,
    pub supplier_id: Uuid,
    pub supplier_number: String,
    pub supplier_name: String,
    pub payment_date: NaiveDate,
    #[schema(value_type = String, example = "2400.00")]
    pub amount: Decimal,
    pub reference: Option<String>,
    /// `draft | posted`.
    pub status: String,
    pub journal_entry_id: Option<Uuid>,
    pub entry_number: Option<String>,
    pub posted_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// One invoice settled by a payment.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct VendorPaymentAllocationView {
    pub vendor_invoice_id: Uuid,
    pub invoice_number: String,
    pub supplier_invoice_number: String,
    #[schema(value_type = String, example = "2400.00")]
    pub amount: Decimal,
}

/// A vendor payment with its allocations.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VendorPaymentDetail {
    #[serde(flatten)]
    pub payment: VendorPaymentView,
    pub allocations: Vec<VendorPaymentAllocationView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct VendorPaymentList {
    pub items: Vec<VendorPaymentView>,
}

const PSELECT: &str = "SELECT p.id, p.institution_id, p.number, p.supplier_id, \
    s.number AS supplier_number, s.legal_name AS supplier_name, p.payment_date, p.amount, \
    p.reference, p.status, p.journal_entry_id, p.entry_number, p.posted_at, p.version, p.created_at \
    FROM resources.vendor_payment p JOIN resources.supplier s ON s.id = p.supplier_id";

async fn load_payment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<VendorPaymentView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF p" } else { "" };
    sqlx::query_as::<_, VendorPaymentView>(&format!(
        "{PSELECT} WHERE p.id = $1 AND p.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "vendor payment",
    })
}

async fn payment_response(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let payment = load_payment(conn, tenant, id, false).await?;
    let allocations = sqlx::query_as::<_, VendorPaymentAllocationView>(
        "SELECT a.vendor_invoice_id, v.number AS invoice_number, v.supplier_invoice_number, a.amount \
         FROM resources.vendor_payment_allocation a \
         JOIN resources.vendor_invoice v ON v.id = a.vendor_invoice_id \
         WHERE a.vendor_payment_id = $1 ORDER BY v.number",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let v = payment.version;
    Ok(with_etag(
        status,
        &VendorPaymentDetail {
            payment,
            allocations,
        },
        Version(v),
    ))
}

/// One invoice to settle.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VendorPaymentAllocationInput {
    pub vendor_invoice_id: Uuid,
    #[schema(example = "2400.00")]
    pub amount: String,
}

/// Pay a supplier.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VendorPaymentCreate {
    pub supplier_id: Uuid,
    pub payment_date: NaiveDate,
    pub reference: Option<String>,
    pub allocations: Vec<VendorPaymentAllocationInput>,
}

/// Draft a vendor payment (`resources.procurement.manage`; Idempotency-Key; F-045): each
/// allocation settles a **posted** invoice of the supplier, at most its outstanding amount
/// (net of other draft payments).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/vendor-payments", operation_id = "resources_payable_payment_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = VendorPaymentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = VendorPaymentDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_payment(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VendorPaymentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let reference = optional_text("reference", b.reference, 100)?;
    if b.allocations.is_empty() {
        return Err(invalid("allocations", "required", "settle at least one invoice").into());
    }
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let supplier = match suppliers::load(&mut tx, t, b.supplier_id, false).await {
        Ok(s) if s.institution_id == id => s,
        Ok(_) | Err(KernelError::NotFound { .. }) => {
            return Err(invalid(
                "supplierId",
                "unknown_supplier",
                "no such supplier in the institution",
            )
            .into())
        }
        Err(e) => return Err(e.into()),
    };
    let mut total = Decimal::ZERO;
    let mut planned: Vec<(Uuid, Decimal)> = Vec::new();
    for (i, a) in b.allocations.iter().enumerate() {
        let f = |n: &str| format!("allocations[{i}].{n}");
        let amount = parse_money(&f("amount"), &a.amount)?;
        if amount <= Decimal::ZERO {
            return Err(invalid(&f("amount"), "not_positive", "must be greater than zero").into());
        }
        if planned.iter().any(|p| p.0 == a.vendor_invoice_id) {
            return Err(invalid(&f("vendorInvoiceId"), "duplicate", "each invoice once").into());
        }
        let inv = match load(&mut tx, t, a.vendor_invoice_id, true).await {
            Ok(v) if v.institution_id == id => v,
            Ok(_) | Err(KernelError::NotFound { .. }) => {
                return Err(invalid(
                    &f("vendorInvoiceId"),
                    "unknown_invoice",
                    "no such vendor invoice",
                )
                .into())
            }
            Err(e) => return Err(e.into()),
        };
        if inv.supplier_id != supplier.id {
            return Err(invalid(
                &f("vendorInvoiceId"),
                "other_supplier",
                "the invoice belongs to another supplier",
            )
            .into());
        }
        if inv.status()? != VendorInvoiceStatus::Posted || inv.is_credit_note {
            return Err(KernelError::Conflict(format!(
                "vendor invoice {} is {}; payments settle posted invoices",
                inv.number, inv.status
            ))
            .into());
        }
        let drafted: Option<Decimal> = sqlx::query_scalar(
            "SELECT sum(a.amount) FROM resources.vendor_payment_allocation a \
             JOIN resources.vendor_payment p ON p.id = a.vendor_payment_id \
             WHERE a.vendor_invoice_id = $1 AND p.status = 'draft'",
        )
        .bind(inv.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let outstanding = inv.total_amount - inv.paid_amount - drafted.unwrap_or(Decimal::ZERO);
        if amount > outstanding {
            return Err(invalid(
                &f("amount"),
                "exceeds_outstanding",
                &format!("{} has {outstanding} outstanding", inv.number),
            )
            .into());
        }
        total += amount;
        planned.push((inv.id, amount));
    }
    let number = next(&mut tx, t, institution, kind::VENDOR_PAYMENT).await?;
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.vendor_payment (tenant_id, institution_id, number, supplier_id, \
         payment_date, amount, reference) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(supplier.id)
    .bind(b.payment_date)
    .bind(total)
    .bind(&reference)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (inv, amount) in &planned {
        sqlx::query(
            "INSERT INTO resources.vendor_payment_allocation (tenant_id, vendor_payment_id, \
             vendor_invoice_id, amount) VALUES ($1, $2, $3, $4)",
        )
        .bind(t.as_uuid())
        .bind(pid)
        .bind(*inv)
        .bind(*amount)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        t,
        institution,
        "vendor_payment",
        pid,
        None,
        "draft",
        "sutraerp.resources.vendor_payment.drafted",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "vendor_payment",
        pid,
        "resources.vendor_payment.create",
        serde_json::json!({ "number": number, "supplier": supplier.number, "amount": total.to_string() }),
        &actor,
    )
    .await?;
    let res = payment_response(&mut tx, t, pid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Vendor payments of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/vendor-payments", operation_id = "resources_payable_payment_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = VendorPaymentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_payments(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<VendorPaymentList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, VendorPaymentView>(&format!(
        "{PSELECT} WHERE p.tenant_id = $1 AND p.institution_id = $2 \
         AND ($3::text IS NULL OR p.status = $3) ORDER BY p.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(VendorPaymentList { items }))
}

/// One vendor payment with its allocations (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/vendor-payments/{id}", operation_id = "resources_payable_payment_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = VendorPaymentDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_payment(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(payment_response(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Post a drafted payment to Finance (`resources.procurement.manage`; If-Match; F-045): Dr
/// accounts payable / Cr the main bank account through `ProcurementPostingPort` (idempotent);
/// settles the invoices (`paid` once fully paid); `vendor_payment.posted`.
#[utoipa::path(post, path = "/api/v1/resources/vendor-payments/{id}/post", operation_id = "resources_payable_payment_post", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the payment")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = VendorPaymentDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn post_payment(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        institution_of(&mut conn, t, "vendor_payment", "vendor payment", id).await?
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_payment(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    if p.status != VendorPaymentStatus::Draft.as_str() {
        return Err(illegal("vendor payment", &p.number, &p.status, "posted").into());
    }
    let allocations: Vec<(Uuid, Decimal)> = sqlx::query_as(
        "SELECT vendor_invoice_id, amount FROM resources.vendor_payment_allocation \
         WHERE vendor_payment_id = $1 ORDER BY vendor_invoice_id",
    )
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (inv_id, amount) in &allocations {
        let inv = load(&mut tx, t, *inv_id, true).await?;
        if inv.status()? != VendorInvoiceStatus::Posted
            || *amount > inv.total_amount - inv.paid_amount
        {
            return Err(KernelError::Conflict(format!(
                "vendor invoice {} is {} with {} outstanding; the payment no longer fits",
                inv.number,
                inv.status,
                inv.total_amount - inv.paid_amount
            ))
            .into());
        }
    }
    let receipt = st
        .postings
        .post_procurement(
            &scope,
            ProcurementPostingRequest {
                institution_id: inst,
                kind: ProcurementPostingKind::VendorPayment,
                source_id: id,
                posting_key: "post".into(),
                posting_date: p.payment_date,
                description: format!(
                    "Vendor payment {} {}",
                    p.number,
                    p.reference.clone().unwrap_or_default()
                ),
                supplier_label: format!("{} {}", p.supplier_number, p.supplier_name),
                inventory_amount: Decimal::ZERO,
                expense_amount: Decimal::ZERO,
                payment_amount: p.amount,
                actor: actor.id.clone(),
            },
        )
        .await?;
    sqlx::query(
        "UPDATE resources.vendor_payment SET status = 'posted', journal_entry_id = $2, \
         entry_number = $3, posted_at = now(), version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(receipt.journal_entry_id.as_uuid())
    .bind(&receipt.entry_number)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        inst,
        "vendor_payment",
        id,
        Some("draft"),
        "posted",
        ev::VENDOR_PAYMENT_POSTED,
        None,
        None,
        &actor,
    )
    .await?;
    for (inv_id, amount) in &allocations {
        sqlx::query(
            "UPDATE resources.vendor_invoice SET paid_amount = paid_amount + $2, \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(*inv_id)
        .bind(*amount)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let inv = load(&mut tx, t, *inv_id, true).await?;
        if inv.paid_amount >= inv.total_amount {
            set_status(
                &mut tx,
                t,
                &inv,
                VendorInvoiceStatus::Paid,
                ev::VENDOR_PAYMENT_POSTED,
                Some(&p.number),
                &actor,
            )
            .await?;
        }
    }
    let out = load_payment(&mut tx, t, id, false).await?;
    emit(
        &mut tx,
        &scope,
        ev::VENDOR_PAYMENT_POSTED,
        ev::VENDOR_PAYMENT_POSTED_SCHEMA,
        "vendor_payment",
        id,
        "resources.vendor_payment.post",
        &status_payload(
            id,
            out.institution_id,
            Some("draft"),
            "posted",
            out.version,
            Some(&format!("{} {}", receipt.entry_number, out.amount)),
            None,
            Some(&out.number),
            None,
        ),
        &actor,
    )
    .await?;
    let res = payment_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Routes of this area.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_invoice, list_invoices))
        .routes(routes!(get_invoice))
        .routes(routes!(match_invoice))
        .routes(routes!(decide_invoice))
        .routes(routes!(post_invoice))
        .routes(routes!(create_payment, list_payments))
        .routes(routes!(get_payment))
        .routes(routes!(post_payment))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn three_way_match_rules() {
        assert!(price_within_tolerance(d("101.00"), d("100.00")));
        assert!(!price_within_tolerance(d("101.01"), d("100.00")));
        assert!(
            match_line(1, false, d("4"), d("100"), d("100"), d("4"), d("0"), d("0")).is_empty()
        );
        let over = match_line(1, false, d("5"), d("100"), d("100"), d("4"), d("0"), d("0"));
        assert_eq!(over[0].code, "quantity_exceeds_received");
        let claimed = match_line(1, false, d("2"), d("100"), d("100"), d("4"), d("1"), d("2"));
        assert_eq!(claimed.len(), 1);
        let price = match_line(1, false, d("1"), d("120"), d("100"), d("4"), d("0"), d("0"));
        assert_eq!(price[0].code, "price_variance");
        let credit = match_line(1, true, d("3"), d("100"), d("100"), d("4"), d("2"), d("0"));
        assert_eq!(credit[0].code, "credit_exceeds_invoiced");
        assert!(VendorInvoiceStatus::Approved.can_transition(VendorInvoiceStatus::Posted));
        assert!(!VendorInvoiceStatus::Mismatch.can_transition(VendorInvoiceStatus::Posted));
        assert_eq!(VendorPaymentStatus::Posted.as_str(), "posted");
    }
}
