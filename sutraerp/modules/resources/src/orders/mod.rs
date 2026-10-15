//! **8D** — purchase orders: from an award or a released requisition, issue, supplier acknowledgement, amendment, cancellation, closure (L3-J-043 … 047). Operation ids `resources_po_*`, tag `resources-procurement`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * create → `draft` (`PO-` number) from an **awarded** sourcing event (awarded supplier, quoted
//!   prices) or straight from a **released** requisition with an active supplier (only up to the
//!   direct sourcing limit); line kind from the item (`consumable` → `stock`, `asset` → `asset`,
//!   free text / service → `service`);
//! * issue → `PURCHASE_ORDER_APPROVAL` → `issued` (`purchase_order.issued`; the requisition
//!   becomes `ordered`); acknowledge (`acknowledged`, supplier reference);
//! * amend (quantities / prices of lines, never below what was received; next revision) → back
//!   to `draft` for re-approval and re-issue;
//! * cancel (nothing received; the requisition is released again); close once every line is
//!   fully received **and** invoiced (`purchase_order.closed`; requisition `closed`).
//!
//! Goods receipts against the order live in `receipts` (`POST /purchase-orders/{id}/receipts`).

use crate::requisitions::{self, RequisitionStatus};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, ResourcesState};
use crate::sourcing::{self, SourcingStatus, DIRECT_SOURCING_LIMIT};
use crate::suppliers::active_supplier;
use crate::suppliers::common::{
    active_location, audit_only, check_version, clean, emit, expected, history, illegal,
    institution_of, invalid, line_amount, load_item, optional_text, parse_money, parse_qty,
    reason_of, required_reason, slice_status, status_payload, ResourcesDecision,
    ResourcesDecisionBody, ResourcesReasonBody, ResourcesStatusQuery,
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
use std::str::FromStr;
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

slice_status!(
    /// Purchase order lifecycle (L2-J04).
    PurchaseOrderStatus {
        Draft => "draft",
        Issued => "issued",
        Acknowledged => "acknowledged",
        PartiallyReceived => "partially_received",
        Received => "received",
        Closed => "closed",
        Cancelled => "cancelled",
    }
);

impl PurchaseOrderStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use PurchaseOrderStatus::*;
        matches!(
            (self, to),
            (Draft, Issued)
                | (Draft, PartiallyReceived)
                | (Draft, Received)
                | (Draft, Cancelled)
                | (Issued, Acknowledged)
                | (Issued, PartiallyReceived)
                | (Issued, Received)
                | (Issued, Draft)
                | (Issued, Cancelled)
                | (Acknowledged, PartiallyReceived)
                | (Acknowledged, Received)
                | (Acknowledged, Draft)
                | (Acknowledged, Cancelled)
                | (PartiallyReceived, Received)
                | (PartiallyReceived, Draft)
                | (Received, Closed)
        )
    }

    /// Goods may be received against the order.
    pub fn receivable(self) -> bool {
        matches!(
            self,
            Self::Issued | Self::Acknowledged | Self::PartiallyReceived
        )
    }
}

/// A purchase order (header).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "PO-000001")]
    pub number: String,
    pub supplier_id: Uuid,
    pub supplier_number: String,
    pub supplier_name: String,
    pub requisition_id: Option<Uuid>,
    pub sourcing_event_id: Option<Uuid>,
    pub quotation_id: Option<Uuid>,
    pub delivery_location_id: Option<Uuid>,
    pub order_date: NaiveDate,
    pub expected_delivery: Option<NaiveDate>,
    pub revision: i32,
    #[schema(value_type = String, example = "24000.00")]
    pub total_amount: Decimal,
    pub notes: Option<String>,
    /// `draft | issued | acknowledged | partially_received | received | closed | cancelled`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Deprecated (Batch 9 §6.4): read `approvalState`; kept for existing clients.
    pub approval_pending: bool,
    /// 9E: approvalState — canonical approval state (`not_requested | pending | approved |
    /// rejected`, contract batch9 §6.4), derived from the status + `approvalPending`.
    pub approval_state: String,
    pub process_instance_id: Option<String>,
    pub issued_at: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub acknowledgement_ref: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl PurchaseOrderView {
    pub fn status(&self) -> Result<PurchaseOrderStatus, KernelError> {
        PurchaseOrderStatus::parse(&self.status).ok_or_else(|| {
            KernelError::Conflict(format!("unknown purchase order status {}", self.status))
        })
    }
}

/// One purchase order line.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderLineView {
    pub id: Uuid,
    pub line_no: i32,
    pub item_id: Option<Uuid>,
    pub item_code: Option<String>,
    /// `stock | asset | service`.
    pub line_kind: String,
    pub description: String,
    #[schema(value_type = String, example = "10.000")]
    pub quantity: Decimal,
    pub unit: String,
    #[schema(value_type = String, example = "240.00")]
    pub unit_price: Decimal,
    #[schema(value_type = String, example = "2400.00")]
    pub amount: Decimal,
    /// Accepted into stock / custody so far.
    #[schema(value_type = String, example = "4.000")]
    pub received_quantity: Decimal,
    /// On posted vendor invoices (net of credit notes).
    #[schema(value_type = String, example = "4.000")]
    pub invoiced_quantity: Decimal,
}

/// A purchase order with its lines.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderDetail {
    #[serde(flatten)]
    pub order: PurchaseOrderView,
    pub lines: Vec<PurchaseOrderLineView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PurchaseOrderList {
    pub items: Vec<PurchaseOrderView>,
}

const SELECT: &str = "SELECT o.id, o.institution_id, o.number, o.supplier_id, \
    s.number AS supplier_number, s.legal_name AS supplier_name, o.requisition_id, \
    o.sourcing_event_id, o.quotation_id, o.delivery_location_id, o.order_date, o.expected_delivery, \
    o.revision, o.total_amount, o.notes, o.status, o.status_reason, o.approval_pending, \
    o.process_instance_id, o.issued_at, o.acknowledged_at, o.acknowledgement_ref, o.closed_at, \
    o.version, o.created_at, \
    CASE WHEN o.approval_pending THEN 'pending' \
         WHEN o.status NOT IN ('draft', 'cancelled') OR o.issued_at IS NOT NULL THEN 'approved' \
         ELSE 'not_requested' END AS approval_state /* 9E: approvalState */ \
    FROM resources.purchase_order o \
    JOIN resources.supplier s ON s.id = o.supplier_id";

/// A purchase order of the tenant (404 when missing).
pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PurchaseOrderView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF o" } else { "" };
    sqlx::query_as::<_, PurchaseOrderView>(&format!(
        "{SELECT} WHERE o.id = $1 AND o.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "purchase order",
    })
}

/// Lines of an order, in line order.
pub async fn lines_of(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<PurchaseOrderLineView>, KernelError> {
    sqlx::query_as::<_, PurchaseOrderLineView>(
        "SELECT l.id, l.line_no, l.item_id, i.code AS item_code, l.line_kind, l.description, \
         l.quantity, l.unit, l.unit_price, l.amount, l.received_quantity, l.invoiced_quantity \
         FROM resources.purchase_order_line l LEFT JOIN resources.item i ON i.id = l.item_id \
         WHERE l.purchase_order_id = $1 ORDER BY l.line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn detail(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<PurchaseOrderDetail, KernelError> {
    let order = load(conn, tenant, id, false).await?;
    let lines = lines_of(conn, id).await?;
    Ok(PurchaseOrderDetail { order, lines })
}

async fn respond(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let d = detail(conn, tenant, id).await?;
    let v = d.order.version;
    Ok(with_etag(status, &d, Version(v)))
}

/// Moves an order (row locked by the caller): legal-transition check, update, history.
pub async fn change_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    o: &PurchaseOrderView,
    to: PurchaseOrderStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = o.status()?;
    if from == to {
        return Ok(());
    }
    if !from.can_transition(to) {
        return Err(illegal(
            "purchase order",
            &o.number,
            from.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE resources.purchase_order SET status = $2, status_reason = COALESCE($3, status_reason), \
         issued_at = CASE WHEN $2 = 'issued' THEN now() ELSE issued_at END, \
         closed_at = CASE WHEN $2 = 'closed' THEN now() ELSE closed_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(o.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(o.institution_id),
        "purchase_order",
        o.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        o.process_instance_id.as_deref(),
        actor,
    )
    .await
}

/// Status an issued order should have from its received quantities.
fn fulfilment(lines: &[PurchaseOrderLineView]) -> PurchaseOrderStatus {
    let any = lines.iter().any(|l| l.received_quantity > Decimal::ZERO);
    let all = !lines.is_empty() && lines.iter().all(|l| l.received_quantity >= l.quantity);
    if all {
        PurchaseOrderStatus::Received
    } else if any {
        PurchaseOrderStatus::PartiallyReceived
    } else {
        PurchaseOrderStatus::Issued
    }
}

/// After accepted receipts: moves a receivable order to `partially_received` / `received`.
pub async fn refresh_fulfilment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let o = load(conn, tenant, id, true).await?;
    if !o.status()?.receivable() {
        return Ok(());
    }
    let lines = lines_of(conn, id).await?;
    let to = fulfilment(&lines);
    if to != PurchaseOrderStatus::Issued {
        change_status(
            conn,
            tenant,
            &o,
            to,
            "sutraerp.resources.purchase_order.received",
            None,
            actor,
        )
        .await?;
    }
    Ok(())
}

async fn issue_now(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    id: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let o = load(conn, tenant, id, true).await?;
    active_supplier(
        conn,
        tenant,
        InstitutionId(o.institution_id),
        "supplierId",
        o.supplier_id,
    )
    .await?;
    let lines = lines_of(conn, id).await?;
    let to = fulfilment(&lines);
    change_status(conn, tenant, &o, to, ev::PURCHASE_ORDER_ISSUED, None, actor).await?;
    sqlx::query(
        "UPDATE resources.purchase_order SET approval_pending = false, status_reason = NULL, \
         issued_at = COALESCE(issued_at, now()) WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(rid) = o.requisition_id {
        let r = requisitions::load(conn, tenant, rid, true).await?;
        if r.status()? == RequisitionStatus::Released {
            requisitions::change_status(
                conn,
                tenant,
                &r,
                RequisitionStatus::Ordered,
                ev::PURCHASE_ORDER_ISSUED,
                Some(&o.number),
                actor,
            )
            .await?;
        }
    }
    let out = load(conn, tenant, id, false).await?;
    emit(
        conn,
        scope,
        ev::PURCHASE_ORDER_ISSUED,
        ev::PURCHASE_ORDER_ISSUED_SCHEMA,
        "purchase_order",
        id,
        "resources.purchase_order.issue",
        &status_payload(
            id,
            out.institution_id,
            Some("draft"),
            out.status.as_str(),
            out.version,
            Some(&format!("{} {}", out.supplier_number, out.total_amount)),
            out.process_instance_id.as_deref(),
            Some(&out.number),
            None,
        ),
        actor,
    )
    .await
}

async fn order_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(
        &mut conn,
        ctx.scope.tenant_id,
        "purchase_order",
        "purchase order",
        id,
    )
    .await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// Price of one requisition line on a direct order.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderPriceInput {
    pub line_no: i32,
    #[schema(example = "240.00")]
    pub unit_price: String,
}

/// Create a purchase order from an awarded sourcing event **or** a released requisition.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderCreate {
    pub sourcing_event_id: Option<Uuid>,
    pub requisition_id: Option<Uuid>,
    /// Required for an order straight from a requisition.
    pub supplier_id: Option<Uuid>,
    /// Prices of a direct order (default: the requisition's estimates).
    pub prices: Option<Vec<PurchaseOrderPriceInput>>,
    pub delivery_location_id: Option<Uuid>,
    pub expected_delivery: Option<NaiveDate>,
    pub notes: Option<String>,
}

/// Draft a purchase order (`resources.procurement.manage`; Idempotency-Key; L3-J-043).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/purchase-orders", operation_id = "resources_po_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = PurchaseOrderCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
#[allow(clippy::type_complexity)]
pub async fn create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PurchaseOrderCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let notes = optional_text("notes", b.notes, 2000)?;
    let today = business_today(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    if let Some(loc) = b.delivery_location_id {
        active_location(&mut tx, t, institution, "deliveryLocationId", loc).await?;
    }
    // (requisition, supplier, sourcing event, quotation, price per requisition line)
    // `direct`: no competitive award behind the order (Part 1 DEF-J-005 caps its total).
    let (req_id, supplier_id, event_id, quotation_id, prices, direct): (
        Uuid,
        Uuid,
        Option<Uuid>,
        Option<Uuid>,
        Vec<(i32, Decimal)>,
        bool,
    ) = match (b.sourcing_event_id, b.requisition_id) {
        (Some(eid), _) => {
            let e = match sourcing::load(&mut tx, t, eid, true).await {
                Ok(e) if e.institution_id == id => e,
                Ok(_) | Err(KernelError::NotFound { .. }) => {
                    return Err(invalid(
                        "sourcingEventId",
                        "unknown_sourcing_event",
                        "no such sourcing event in the institution",
                    )
                    .into())
                }
                Err(e) => return Err(e.into()),
            };
            if e.status()? != SourcingStatus::Awarded {
                return Err(KernelError::Conflict(format!(
                    "sourcing event {} is {}; order from an awarded event",
                    e.number, e.status
                ))
                .into());
            }
            let (Some(q), Some(s)) = (e.awarded_quotation_id, e.awarded_supplier_id) else {
                return Err(KernelError::Conflict(format!(
                    "sourcing event {} has no award",
                    e.number
                ))
                .into());
            };
            let prices: Vec<(i32, Decimal)> = sourcing::quotation_lines(&mut tx, q)
                .await?
                .into_iter()
                .map(|l| (l.line_no, l.unit_price))
                .collect();
            let direct = e.method == "direct";
            (e.requisition_id, s, Some(eid), Some(q), prices, direct)
        }
        (None, Some(rid)) => {
            let Some(sid) = b.supplier_id else {
                return Err(invalid(
                    "supplierId",
                    "required",
                    "a direct order names its supplier",
                )
                .into());
            };
            let r = match requisitions::load(&mut tx, t, rid, false).await {
                Ok(r) if r.institution_id == id => r,
                Ok(_) | Err(KernelError::NotFound { .. }) => {
                    return Err(invalid(
                        "requisitionId",
                        "unknown_requisition",
                        "no such requisition in the institution",
                    )
                    .into())
                }
                Err(e) => return Err(e.into()),
            };
            let limit = Decimal::from_str(DIRECT_SOURCING_LIMIT).unwrap_or(Decimal::ZERO);
            if r.estimated_total > limit {
                return Err(KernelError::Conflict(format!(
                    "purchase requisition {} is estimated at {}; above {DIRECT_SOURCING_LIMIT} it must be sourced (RFQ) first",
                    r.number, r.estimated_total
                ))
                .into());
            }
            let demand = requisitions::lines_of(&mut tx, rid).await?;
            let mut prices = Vec::with_capacity(demand.len());
            for d in &demand {
                let given = b
                    .prices
                    .as_ref()
                    .and_then(|p| p.iter().find(|x| x.line_no == d.line_no));
                let price = match given {
                    Some(p) => {
                        parse_money(&format!("prices[{}].unitPrice", d.line_no), &p.unit_price)?
                    }
                    None => d.estimated_unit_cost,
                };
                prices.push((d.line_no, price));
            }
            (rid, sid, None, None, prices, true)
        }
        (None, None) => {
            return Err(invalid(
                "sourcingEventId",
                "required",
                "order from an awarded sourcing event or a released requisition",
            )
            .into())
        }
    };
    let r = requisitions::load(&mut tx, t, req_id, true).await?;
    if r.status()? != RequisitionStatus::Released {
        return Err(KernelError::Conflict(format!(
            "purchase requisition {} is {}; only released demand is ordered",
            r.number, r.status
        ))
        .into());
    }
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resources.purchase_order WHERE requisition_id = $1 \
         AND status <> 'cancelled'",
    )
    .bind(req_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if live > 0 {
        return Err(KernelError::Conflict(format!(
            "purchase requisition {} already has a purchase order",
            r.number
        ))
        .into());
    }
    let supplier = active_supplier(&mut tx, t, institution, "supplierId", supplier_id).await?;
    let demand = requisitions::lines_of(&mut tx, req_id).await?;
    let number = next(&mut tx, t, institution, kind::PURCHASE_ORDER).await?;
    let po: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.purchase_order (tenant_id, institution_id, number, supplier_id, \
         requisition_id, sourcing_event_id, quotation_id, delivery_location_id, order_date, \
         expected_delivery, notes) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(supplier.id)
    .bind(req_id)
    .bind(event_id)
    .bind(quotation_id)
    .bind(b.delivery_location_id)
    .bind(today)
    .bind(b.expected_delivery)
    .bind(&notes)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut total = Decimal::ZERO;
    for d in &demand {
        let price = prices
            .iter()
            .find(|p| p.0 == d.line_no)
            .map(|p| p.1)
            .unwrap_or(d.estimated_unit_cost);
        let line_kind = match d.item_id {
            Some(item) => load_item(&mut tx, t, item)
                .await?
                .map(|i| i.line_kind())
                .unwrap_or("service"),
            None => "service",
        };
        let amount = line_amount(d.quantity, price);
        total += amount;
        sqlx::query(
            "INSERT INTO resources.purchase_order_line (tenant_id, purchase_order_id, line_no, \
             item_id, line_kind, description, quantity, unit, unit_price, amount) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(t.as_uuid())
        .bind(po)
        .bind(d.line_no)
        .bind(d.item_id)
        .bind(line_kind)
        .bind(&d.description)
        .bind(d.quantity)
        .bind(&d.unit)
        .bind(price)
        .bind(amount)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    if direct {
        // Part 1 (DEF-J-005): the limit binds the ordered value (caller-supplied prices), not
        // only the requisition's estimate.
        sourcing::ensure_within_direct_limit(&format!("direct purchase order {number}"), total)?;
    }
    sqlx::query("UPDATE resources.purchase_order SET total_amount = $2 WHERE id = $1")
        .bind(po)
        .bind(total)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "purchase_order",
        po,
        None,
        "draft",
        "sutraerp.resources.purchase_order.created",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "purchase_order",
        po,
        "resources.purchase_order.create",
        serde_json::json!({ "number": number, "supplier": supplier.number, "requisition": r.number }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, po, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Purchase orders of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/purchase-orders", operation_id = "resources_po_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PurchaseOrderList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<PurchaseOrderList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, PurchaseOrderView>(&format!(
        "{SELECT} WHERE o.tenant_id = $1 AND o.institution_id = $2 \
         AND ($3::text IS NULL OR o.status = $3) ORDER BY o.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(PurchaseOrderList { items }))
}

/// One purchase order with its lines (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/purchase-orders/{id}", operation_id = "resources_po_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_one(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Issue a draft order to its supplier (`resources.procurement.manage`; If-Match;
/// `PURCHASE_ORDER_APPROVAL`; `purchase_order.issued`).
#[utoipa::path(post, path = "/api/v1/resources/purchase-orders/{id}/issue", operation_id = "resources_po_issue", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the order")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn issue(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = order_scope(&st, ctx, id).await?;
    let o = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let o = load(&mut conn, t, id, false).await?;
        active_supplier(
            &mut conn,
            t,
            InstitutionId(o.institution_id),
            "supplierId",
            o.supplier_id,
        )
        .await?;
        o
    };
    check_version(v, o.version)?;
    if o.status()? != PurchaseOrderStatus::Draft || o.approval_pending {
        return Err(illegal("purchase order", &o.number, &o.status, "issued").into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("orderNumber".to_owned(), serde_json::json!(o.number));
    vars.insert(
        "total".to_owned(),
        serde_json::json!(o.total_amount.to_string()),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::PURCHASE_ORDER_APPROVAL,
        business_key("purchase_order", id, o.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    sqlx::query(
        "UPDATE resources.purchase_order SET approval_pending = true, process_instance_id = $2, \
         approval_requested_by = $3, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(outcome.process_instance_id())
    .bind(actor.id.as_deref()) // Part 1: DEF-X-002
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "purchase_order",
        id,
        "resources.purchase_order.submit",
        serde_json::json!({ "number": o.number, "processInstanceId": outcome.process_instance_id() }),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        issue_now(
            &mut tx,
            &scope,
            t,
            id,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Decide a pending order approval (`resources.approve`, step-up; If-Match): approve → issued;
/// reject → stays `draft` with the reason.
#[utoipa::path(post, path = "/api/v1/resources/purchase-orders/{id}/decision", operation_id = "resources_po_decide", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the order")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
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
    let scope = order_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    if !o.approval_pending {
        return Err(illegal("purchase order", &o.number, &o.status, "decided").into());
    }
    // Part 1 (DEF-X-002): whoever created the order or asked for its approval never decides it.
    let requested_by: Option<String> = sqlx::query_scalar(
        "SELECT approval_requested_by FROM resources.purchase_order WHERE id = $1",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    crate::shared::approval::ensure_independent_decider(
        &mut tx,
        t,
        "purchase_order",
        id,
        &[],
        &[requested_by.as_deref()],
        actor.id.as_deref(),
        "purchase order",
    )
    .await?;
    match b.decision {
        ResourcesDecision::Approve => issue_now(&mut tx, &scope, t, id, &actor).await?,
        ResourcesDecision::Reject => {
            sqlx::query(
                "UPDATE resources.purchase_order SET approval_pending = false, status_reason = $2, \
                 version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&reason)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            audit_only(
                &mut tx,
                &scope,
                "purchase_order",
                id,
                "resources.purchase_order.reject",
                serde_json::json!({ "number": o.number, "reason": reason }),
                &actor,
            )
            .await?;
        }
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Supplier acknowledgement of an issued order.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderAcknowledge {
    /// Supplier's order confirmation reference.
    pub reference: String,
    pub expected_delivery: Option<NaiveDate>,
}

/// Record the supplier's acknowledgement (`resources.procurement.manage`; If-Match; L3-J-044).
#[utoipa::path(post, path = "/api/v1/resources/purchase-orders/{id}/acknowledge", operation_id = "resources_po_acknowledge", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the order")),
    request_body = PurchaseOrderAcknowledge, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn acknowledge(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PurchaseOrderAcknowledge>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let reference = clean(Some(b.reference)).ok_or_else(|| {
        invalid(
            "reference",
            "required",
            "the supplier's confirmation reference",
        )
    })?;
    let t = ctx.scope.tenant_id;
    let scope = order_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    if o.status()? != PurchaseOrderStatus::Issued {
        return Err(illegal("purchase order", &o.number, &o.status, "acknowledged").into());
    }
    change_status(
        &mut tx,
        t,
        &o,
        PurchaseOrderStatus::Acknowledged,
        "sutraerp.resources.purchase_order.acknowledged",
        None,
        &actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.purchase_order SET acknowledged_at = now(), acknowledgement_ref = $2, \
         expected_delivery = COALESCE($3, expected_delivery) WHERE id = $1",
    )
    .bind(id)
    .bind(&reference)
    .bind(b.expected_delivery)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "purchase_order",
        id,
        "resources.purchase_order.acknowledge",
        serde_json::json!({ "number": o.number, "reference": reference }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// One changed line of an amendment.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderLineChange {
    pub line_no: i32,
    pub quantity: Option<String>,
    pub unit_price: Option<String>,
}

/// Amend an issued order.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseOrderAmend {
    pub lines: Vec<PurchaseOrderLineChange>,
    pub expected_delivery: Option<NaiveDate>,
    pub reason: Option<String>,
}

/// Amend quantities / prices of an issued order (`resources.procurement.manage`; If-Match;
/// reason required; L3-J-045): never below the received quantity; next revision; back to
/// `draft` for re-approval and re-issue.
#[utoipa::path(post, path = "/api/v1/resources/purchase-orders/{id}/amend", operation_id = "resources_po_amend", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the order")),
    request_body = PurchaseOrderAmend, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn amend(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PurchaseOrderAmend>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    if b.lines.is_empty() && b.expected_delivery.is_none() {
        return Err(invalid("lines", "required", "nothing to amend").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = order_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    let from = o.status()?;
    if !from.can_transition(PurchaseOrderStatus::Draft) || o.approval_pending {
        return Err(illegal("purchase order", &o.number, from.as_str(), "amended").into());
    }
    let lines = lines_of(&mut tx, id).await?;
    for (i, c) in b.lines.iter().enumerate() {
        let l = lines
            .iter()
            .find(|l| l.line_no == c.line_no)
            .ok_or_else(|| {
                invalid(
                    &format!("lines[{i}].lineNo"),
                    "unknown_line",
                    "no such order line",
                )
            })?;
        let quantity = match &c.quantity {
            Some(q) => parse_qty(&format!("lines[{i}].quantity"), q)?,
            None => l.quantity,
        };
        if quantity < l.received_quantity {
            return Err(invalid(
                &format!("lines[{i}].quantity"),
                "below_received",
                &format!("{} already received", l.received_quantity),
            )
            .into());
        }
        let price = match &c.unit_price {
            Some(p) => parse_money(&format!("lines[{i}].unitPrice"), p)?,
            None => l.unit_price,
        };
        sqlx::query(
            "UPDATE resources.purchase_order_line SET quantity = $2, unit_price = $3, amount = $4 \
             WHERE id = $1",
        )
        .bind(l.id)
        .bind(quantity)
        .bind(price)
        .bind(line_amount(quantity, price))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let new_total: Decimal = sqlx::query_scalar(
        "UPDATE resources.purchase_order SET revision = revision + 1, \
         expected_delivery = COALESCE($2, expected_delivery), \
         total_amount = (SELECT COALESCE(sum(amount), 0) FROM resources.purchase_order_line \
                         WHERE purchase_order_id = $1) WHERE id = $1 RETURNING total_amount",
    )
    .bind(id)
    .bind(b.expected_delivery)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    // Part 1 (DEF-J-005): an amendment never lifts a direct order above the direct-purchase
    // limit (that needs the competitive route); any amendment goes back to `draft`, so the
    // changed order is issued again only through `PURCHASE_ORDER_APPROVAL` (re-approval).
    let direct = match o.sourcing_event_id {
        None => true,
        Some(eid) => sourcing::load(&mut tx, t, eid, false).await?.method == "direct",
    };
    if direct && new_total > o.total_amount {
        sourcing::ensure_within_direct_limit(
            &format!("amended direct purchase order {}", o.number),
            new_total,
        )?;
    }
    change_status(
        &mut tx,
        t,
        &o,
        PurchaseOrderStatus::Draft,
        "sutraerp.resources.purchase_order.amended",
        Some(&reason),
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "purchase_order",
        id,
        "resources.purchase_order.amend",
        serde_json::json!({ "number": o.number, "revision": o.revision + 1, "reason": reason,
            "previousTotal": o.total_amount.to_string(), "total": new_total.to_string() }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Cancel an order with nothing received (`resources.procurement.manage`; If-Match; reason
/// required; L3-J-046): the requisition is released again.
#[utoipa::path(post, path = "/api/v1/resources/purchase-orders/{id}/cancel", operation_id = "resources_po_cancel", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the order")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cancel(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let reason = required_reason(reason_of(body))?;
    let t = ctx.scope.tenant_id;
    let scope = order_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resources.goods_receipt WHERE purchase_order_id = $1 \
         AND status <> 'rejected'",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if receipts > 0 {
        return Err(KernelError::Conflict(format!(
            "purchase order {} has goods receipts; it can no longer be cancelled",
            o.number
        ))
        .into());
    }
    change_status(
        &mut tx,
        t,
        &o,
        PurchaseOrderStatus::Cancelled,
        "sutraerp.resources.purchase_order.cancelled",
        Some(&reason),
        &actor,
    )
    .await?;
    sqlx::query("UPDATE resources.purchase_order SET approval_pending = false WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if let Some(rid) = o.requisition_id {
        let r = requisitions::load(&mut tx, t, rid, true).await?;
        if r.status()? == RequisitionStatus::Ordered {
            requisitions::change_status(
                &mut tx,
                t,
                &r,
                RequisitionStatus::Released,
                "sutraerp.resources.purchase_order.cancelled",
                Some(&reason),
                &actor,
            )
            .await?;
        }
    }
    audit_only(
        &mut tx,
        &scope,
        "purchase_order",
        id,
        "resources.purchase_order.cancel",
        serde_json::json!({ "number": o.number, "reason": reason }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Close a fulfilled order (`resources.procurement.manage`; If-Match; L3-J-047): every line
/// received and invoiced; `purchase_order.closed`; the requisition is closed.
#[utoipa::path(post, path = "/api/v1/resources/purchase-orders/{id}/close", operation_id = "resources_po_close", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the order")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseOrderDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn close(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = order_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = load(&mut tx, t, id, true).await?;
    check_version(v, o.version)?;
    if o.status()? != PurchaseOrderStatus::Received {
        return Err(KernelError::Conflict(format!(
            "purchase order {} is {}; only a fully received order closes",
            o.number, o.status
        ))
        .into());
    }
    let lines = lines_of(&mut tx, id).await?;
    if let Some(l) = lines
        .iter()
        .find(|l| l.invoiced_quantity < l.received_quantity)
    {
        return Err(KernelError::Conflict(format!(
            "purchase order {} line {} awaits invoicing ({} of {} invoiced)",
            o.number, l.line_no, l.invoiced_quantity, l.received_quantity
        ))
        .into());
    }
    change_status(
        &mut tx,
        t,
        &o,
        PurchaseOrderStatus::Closed,
        ev::PURCHASE_ORDER_CLOSED,
        None,
        &actor,
    )
    .await?;
    if let Some(rid) = o.requisition_id {
        let r = requisitions::load(&mut tx, t, rid, true).await?;
        if r.status()? == RequisitionStatus::Ordered {
            requisitions::change_status(
                &mut tx,
                t,
                &r,
                RequisitionStatus::Closed,
                ev::PURCHASE_ORDER_CLOSED,
                Some(&o.number),
                &actor,
            )
            .await?;
        }
    }
    let out = load(&mut tx, t, id, false).await?;
    emit(
        &mut tx,
        &scope,
        ev::PURCHASE_ORDER_CLOSED,
        ev::PURCHASE_ORDER_CLOSED_SCHEMA,
        "purchase_order",
        id,
        "resources.purchase_order.close",
        &status_payload(
            id,
            out.institution_id,
            Some("received"),
            "closed",
            out.version,
            None,
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

/// Routes of this area.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get_one))
        .routes(routes!(issue))
        .routes(routes!(decide))
        .routes(routes!(acknowledge))
        .routes(routes!(amend))
        .routes(routes!(cancel))
        .routes(routes!(close))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn order_transitions() {
        use PurchaseOrderStatus::*;
        assert!(Draft.can_transition(Issued) && Issued.can_transition(Acknowledged));
        assert!(Acknowledged.can_transition(PartiallyReceived));
        assert!(PartiallyReceived.can_transition(Received) && Received.can_transition(Closed));
        assert!(!PartiallyReceived.can_transition(Cancelled));
        assert!(Issued.can_transition(Draft), "amendment");
        assert!(Issued.receivable() && !Draft.receivable());
    }
}
