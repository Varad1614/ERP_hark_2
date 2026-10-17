//! **8D** — goods receipts against purchase orders → inspection → accepted / partially accepted / rejected; accepted stock lines post `receipt` movements (L3-J-049, 054, 057). Operation ids `resources_receipt_*`, tag `resources-procurement`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * record a delivery against a receivable order (`GRN-` number; quantities ≤ ordered −
//!   already received / awaiting inspection, else 422 `over_receipt`) into an active store
//!   location → `received`;
//! * inspect (`inspecting`), then accept (per line accepted / rejected quantities; default all
//!   accepted) → `accepted` | `partially_accepted` | `rejected`, or reject the whole delivery;
//! * acceptance posts one `receipt` movement per accepted **stock** line through
//!   `shared::stock::post_movement` (source `goods_receipt`, key `line:<n>`, unit cost = order
//!   price), raises the order's received quantities (→ `partially_received` / `received`) and
//!   emits `goods_receipt.accepted`; accepted **asset** lines become asset candidates
//!   (`assets` registers them).

use crate::orders;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::stock::{post_movement, NewMovement};
use crate::shared::time::business_today;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers::common::{
    active_location, audit_only, check_version, clean, emit, expected, history, illegal,
    institution_of, invalid, optional_text, parse_qty, reason_of, required_reason, slice_status,
    status_payload, ResourcesReasonBody, ResourcesStatusQuery,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::str::FromStr;
use sutra_contracts::resources::events::procurement as ev;
use sutra_contracts::resources::ids::{ItemId, StoreLocationId};
use sutra_contracts::resources::status::StockMovementKind;
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
    /// Goods receipt lifecycle (L2-J05).
    GoodsReceiptStatus {
        Received => "received",
        Inspecting => "inspecting",
        Accepted => "accepted",
        PartiallyAccepted => "partially_accepted",
        Rejected => "rejected",
    }
);

impl GoodsReceiptStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use GoodsReceiptStatus::*;
        matches!(
            (self, to),
            (Received, Inspecting)
                | (Received, Accepted)
                | (Received, PartiallyAccepted)
                | (Received, Rejected)
                | (Inspecting, Accepted)
                | (Inspecting, PartiallyAccepted)
                | (Inspecting, Rejected)
        )
    }
}

/// A goods receipt (header).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct GoodsReceiptView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "GRN-000001")]
    pub number: String,
    pub purchase_order_id: Uuid,
    pub purchase_order_number: String,
    pub supplier_id: Uuid,
    pub supplier_number: String,
    pub location_id: Uuid,
    pub location_code: String,
    pub received_on: NaiveDate,
    pub delivery_note: Option<String>,
    /// `received | inspecting | accepted | partially_accepted | rejected`.
    pub status: String,
    pub status_reason: Option<String>,
    pub inspected_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl GoodsReceiptView {
    pub fn status(&self) -> Result<GoodsReceiptStatus, KernelError> {
        GoodsReceiptStatus::parse(&self.status).ok_or_else(|| {
            KernelError::Conflict(format!("unknown goods receipt status {}", self.status))
        })
    }
}

/// One received line.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct GoodsReceiptLineView {
    pub id: Uuid,
    pub line_no: i32,
    pub purchase_order_line_id: Uuid,
    pub purchase_order_line_no: i32,
    pub item_id: Option<Uuid>,
    pub item_code: Option<String>,
    /// `stock | asset | service`.
    pub line_kind: String,
    pub description: String,
    #[schema(value_type = String, example = "10.000")]
    pub received_quantity: Decimal,
    #[schema(value_type = String, example = "9.000")]
    pub accepted_quantity: Decimal,
    #[schema(value_type = String, example = "1.000")]
    pub rejected_quantity: Decimal,
    pub rejection_reason: Option<String>,
    #[schema(value_type = String, example = "240.00")]
    pub unit_cost: Decimal,
    pub ledger_entry_id: Option<Uuid>,
    /// Expiry date of the delivered lot (items that track expiry; ERP_EXTENSIONS 2C).
    pub expiry_on: Option<NaiveDate>,
    /// Assets registered from this line (asset lines).
    pub assets_registered: i64,
}

/// A goods receipt with its lines.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoodsReceiptDetail {
    #[serde(flatten)]
    pub receipt: GoodsReceiptView,
    pub lines: Vec<GoodsReceiptLineView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GoodsReceiptList {
    pub items: Vec<GoodsReceiptView>,
}

const SELECT: &str = "SELECT g.id, g.institution_id, g.number, g.purchase_order_id, \
    o.number AS purchase_order_number, g.supplier_id, s.number AS supplier_number, g.location_id, \
    l.code AS location_code, g.received_on, g.delivery_note, g.status, g.status_reason, \
    g.inspected_at, g.decided_at, g.version, g.created_at FROM resources.goods_receipt g \
    JOIN resources.purchase_order o ON o.id = g.purchase_order_id \
    JOIN resources.supplier s ON s.id = g.supplier_id \
    JOIN resources.store_location l ON l.id = g.location_id";

/// A goods receipt of the tenant (404 when missing).
pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<GoodsReceiptView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF g" } else { "" };
    sqlx::query_as::<_, GoodsReceiptView>(&format!(
        "{SELECT} WHERE g.id = $1 AND g.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "goods receipt",
    })
}

/// Lines of a receipt.
pub async fn lines_of(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<GoodsReceiptLineView>, KernelError> {
    sqlx::query_as::<_, GoodsReceiptLineView>(
        "SELECT gl.id, gl.line_no, gl.purchase_order_line_id, pl.line_no AS purchase_order_line_no, \
         gl.item_id, i.code AS item_code, gl.line_kind, pl.description, gl.received_quantity, \
         gl.accepted_quantity, gl.rejected_quantity, gl.rejection_reason, gl.unit_cost, \
         gl.ledger_entry_id, gl.expiry_on, \
         (SELECT count(*) FROM resources.asset a WHERE a.goods_receipt_line_id = gl.id) AS assets_registered \
         FROM resources.goods_receipt_line gl \
         JOIN resources.purchase_order_line pl ON pl.id = gl.purchase_order_line_id \
         LEFT JOIN resources.item i ON i.id = gl.item_id \
         WHERE gl.goods_receipt_id = $1 ORDER BY gl.line_no",
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
) -> Result<GoodsReceiptDetail, KernelError> {
    let receipt = load(conn, tenant, id, false).await?;
    let lines = lines_of(conn, id).await?;
    Ok(GoodsReceiptDetail { receipt, lines })
}

async fn respond(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let d = detail(conn, tenant, id).await?;
    let v = d.receipt.version;
    Ok(with_etag(status, &d, Version(v)))
}

async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    g: &GoodsReceiptView,
    to: GoodsReceiptStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = g.status()?;
    if !from.can_transition(to) {
        return Err(illegal(
            "goods receipt",
            &g.number,
            from.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE resources.goods_receipt SET status = $2, status_reason = COALESCE($3, status_reason), \
         inspected_at = CASE WHEN $2 = 'inspecting' THEN now() ELSE inspected_at END, \
         decided_at = CASE WHEN $2 IN ('accepted', 'partially_accepted', 'rejected') THEN now() ELSE decided_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(g.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(g.institution_id),
        "goods_receipt",
        g.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        None,
        actor,
    )
    .await
}

async fn receipt_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(
        &mut conn,
        ctx.scope.tenant_id,
        "goods_receipt",
        "goods receipt",
        id,
    )
    .await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// One delivered order line.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoodsReceiptLineInput {
    /// Purchase order line number.
    pub line_no: i32,
    #[schema(example = "10")]
    pub quantity: String,
    /// Expiry date of the delivered lot, for items that track expiry (ERP_EXTENSIONS 2C).
    #[serde(default)]
    pub expiry_on: Option<NaiveDate>,
}

/// Record a delivery.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoodsReceiptCreate {
    /// Receiving store location (default: the order's delivery location).
    pub location_id: Option<Uuid>,
    /// Default: today in the institution timezone.
    pub received_on: Option<NaiveDate>,
    pub delivery_note: Option<String>,
    pub lines: Vec<GoodsReceiptLineInput>,
}

/// Order line no, order line id, item, line kind, quantity, unit cost, lot expiry.
type PlannedLine = (
    i32,
    Uuid,
    Option<Uuid>,
    String,
    Decimal,
    Decimal,
    Option<NaiveDate>,
);

/// Record a supplier delivery against an order (`resources.procurement.manage`;
/// Idempotency-Key; L3-J-049): over-receipt → 422.
#[utoipa::path(post, path = "/api/v1/resources/purchase-orders/{id}/receipts", operation_id = "resources_receipt_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = GoodsReceiptCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GoodsReceiptDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
/// Order line no, order line id, item, line kind, quantity, unit cost, lot expiry.
type PlannedLine = (
    i32,
    Uuid,
    Option<Uuid>,
    String,
    Decimal,
    Decimal,
    Option<NaiveDate>,
);

pub async fn create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<GoodsReceiptCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let institution = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        institution_of(&mut conn, t, "purchase_order", "purchase order", id).await?
    };
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let delivery_note = optional_text("deliveryNote", b.delivery_note, 500)?;
    if b.lines.is_empty() {
        return Err(invalid("lines", "required", "at least one delivered line").into());
    }
    let received_on = match b.received_on {
        Some(d) => d,
        None => business_today(&st, &scope, institution).await?,
    };
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = orders::load(&mut tx, t, id, true).await?;
    if !o.status()?.receivable() {
        return Err(KernelError::Conflict(format!(
            "purchase order {} is {}; goods are received against issued orders",
            o.number, o.status
        ))
        .into());
    }
    let location = b
        .location_id
        .or(o.delivery_location_id)
        .ok_or_else(|| invalid("locationId", "required", "the receiving store location"))?;
    active_location(&mut tx, t, institution, "locationId", location).await?;
    let po_lines = orders::lines_of(&mut tx, id).await?;
    let mut planned: Vec<PlannedLine> = Vec::new();
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
        if planned.iter().any(|p| p.1 == pl.id) {
            return Err(invalid(
                &format!("lines[{i}].lineNo"),
                "duplicate",
                "each order line once",
            )
            .into());
        }
        let quantity = parse_qty(&format!("lines[{i}].quantity"), &l.quantity)?;
        let pending: Option<Decimal> = sqlx::query_scalar(
            "SELECT sum(gl.received_quantity) FROM resources.goods_receipt_line gl \
             JOIN resources.goods_receipt g ON g.id = gl.goods_receipt_id \
             WHERE gl.purchase_order_line_id = $1 AND g.status IN ('received', 'inspecting')",
        )
        .bind(pl.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let open = pl.quantity - pl.received_quantity - pending.unwrap_or(Decimal::ZERO);
        if quantity > open {
            return Err(invalid(
                &format!("lines[{i}].quantity"),
                "over_receipt",
                &format!(
                    "line {} has {} of {} still open; {} delivered",
                    pl.line_no,
                    open.max(Decimal::ZERO),
                    pl.quantity,
                    quantity
                ),
            )
            .into());
        }
        planned.push((
            pl.line_no,
            pl.id,
            pl.item_id,
            pl.line_kind.clone(),
            quantity,
            pl.unit_price,
            l.expiry_on,
        ));
    }
    let number = next(&mut tx, t, institution, kind::GOODS_RECEIPT).await?;
    let gid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.goods_receipt (tenant_id, institution_id, number, purchase_order_id, \
         supplier_id, location_id, received_on, delivery_note) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(id)
    .bind(o.supplier_id)
    .bind(location)
    .bind(received_on)
    .bind(&delivery_note)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (n, (_po_line_no, pl_id, item, line_kind, qty, cost, expiry)) in planned.iter().enumerate()
    {
        sqlx::query(
            "INSERT INTO resources.goods_receipt_line (tenant_id, goods_receipt_id, line_no, \
             purchase_order_line_id, item_id, line_kind, received_quantity, unit_cost, expiry_on) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(t.as_uuid())
        .bind(gid)
        .bind(n as i32 + 1)
        .bind(*pl_id)
        .bind(*item)
        .bind(line_kind)
        .bind(*qty)
        .bind(*cost)
        .bind(*expiry)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    history(
        &mut tx,
        t,
        institution,
        "goods_receipt",
        gid,
        None,
        "received",
        "sutraerp.resources.goods_receipt.received",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "goods_receipt",
        gid,
        "resources.goods_receipt.create",
        serde_json::json!({ "number": number, "order": o.number, "lines": planned.len() }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, gid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Receipts of an order (`resources.read`), oldest first.
#[utoipa::path(get, path = "/api/v1/resources/purchase-orders/{id}/receipts", operation_id = "resources_receipt_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GoodsReceiptList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<GoodsReceiptList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    orders::load(&mut conn, t, id, false).await?;
    let items = sqlx::query_as::<_, GoodsReceiptView>(&format!(
        "{SELECT} WHERE g.tenant_id = $1 AND g.purchase_order_id = $2 ORDER BY g.created_at"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Json(GoodsReceiptList { items }))
}

/// Goods receipts of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/goods-receipts", operation_id = "resources_receipt_list_all", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = GoodsReceiptList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_all(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<GoodsReceiptList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, GoodsReceiptView>(&format!(
        "{SELECT} WHERE g.tenant_id = $1 AND g.institution_id = $2 \
         AND ($3::text IS NULL OR g.status = $3) ORDER BY g.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(GoodsReceiptList { items }))
}

/// One goods receipt with its lines (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/goods-receipts/{id}", operation_id = "resources_receipt_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GoodsReceiptDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_one(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Start the inspection (`resources.procurement.manage`; If-Match): `received → inspecting`.
#[utoipa::path(post, path = "/api/v1/resources/goods-receipts/{id}/inspect", operation_id = "resources_receipt_inspect", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the receipt")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GoodsReceiptDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn inspect(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = receipt_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let g = load(&mut tx, t, id, true).await?;
    check_version(v, g.version)?;
    set_status(
        &mut tx,
        t,
        &g,
        GoodsReceiptStatus::Inspecting,
        "sutraerp.resources.goods_receipt.inspecting",
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "goods_receipt",
        id,
        "resources.goods_receipt.inspect",
        serde_json::json!({ "number": g.number }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Inspection result of one receipt line.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoodsReceiptLineDecision {
    /// Receipt line number.
    pub line_no: i32,
    #[schema(example = "9")]
    pub accepted_quantity: String,
    /// Default: received − accepted.
    pub rejected_quantity: Option<String>,
    pub rejection_reason: Option<String>,
}

/// Accept a receipt (all lines in full unless listed).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GoodsReceiptAccept {
    pub lines: Option<Vec<GoodsReceiptLineDecision>>,
}

fn non_negative_qty(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_number",
            "must be a decimal string such as \"9.000\"",
        )
    })?;
    if v < Decimal::ZERO || v.normalize().scale() > 3 {
        return Err(invalid(field, "invalid", "≥ 0 with at most three decimals"));
    }
    Ok(v)
}

/// Accept the inspected delivery (`resources.procurement.manage`; If-Match; L3-J-054 / 057):
/// accepted stock lines post `receipt` movements, the order's received quantities rise;
/// `goods_receipt.accepted`.
#[utoipa::path(post, path = "/api/v1/resources/goods-receipts/{id}/accept", operation_id = "resources_receipt_accept", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the receipt")),
    request_body(content = GoodsReceiptAccept, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GoodsReceiptDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn accept(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<GoodsReceiptAccept>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let decisions = body.and_then(|b| b.0.lines);
    let t = ctx.scope.tenant_id;
    let scope = receipt_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let g = load(&mut tx, t, id, true).await?;
    check_version(v, g.version)?;
    let from = g.status()?;
    if !matches!(
        from,
        GoodsReceiptStatus::Received | GoodsReceiptStatus::Inspecting
    ) {
        return Err(illegal("goods receipt", &g.number, from.as_str(), "accepted").into());
    }
    let o = orders::load(&mut tx, t, g.purchase_order_id, true).await?;
    if !o.status()?.receivable() {
        return Err(KernelError::Conflict(format!(
            "purchase order {} is {}; its deliveries can no longer be accepted",
            o.number, o.status
        ))
        .into());
    }
    let lines = lines_of(&mut tx, id).await?;
    if let Some(ds) = &decisions {
        for (i, d) in ds.iter().enumerate() {
            if !lines.iter().any(|l| l.line_no == d.line_no) {
                return Err(invalid(
                    &format!("lines[{i}].lineNo"),
                    "unknown_line",
                    "no such receipt line",
                )
                .into());
            }
        }
    }
    let mut total_accepted = Decimal::ZERO;
    let mut total_rejected = Decimal::ZERO;
    for l in &lines {
        let d = decisions
            .as_ref()
            .and_then(|ds| ds.iter().find(|d| d.line_no == l.line_no));
        let (accepted, rejected, why) = match d {
            None => (l.received_quantity, Decimal::ZERO, None),
            Some(d) => {
                let f = |n: &str| format!("lines[{}].{n}", l.line_no);
                let a = non_negative_qty(&f("acceptedQuantity"), &d.accepted_quantity)?;
                let r = match &d.rejected_quantity {
                    Some(r) => non_negative_qty(&f("rejectedQuantity"), r)?,
                    None => (l.received_quantity - a).max(Decimal::ZERO),
                };
                if a + r > l.received_quantity {
                    return Err(invalid(
                        &f("acceptedQuantity"),
                        "exceeds_received",
                        &format!("only {} was received", l.received_quantity),
                    )
                    .into());
                }
                let why = clean(d.rejection_reason.clone());
                if r > Decimal::ZERO && why.is_none() {
                    return Err(invalid(
                        &f("rejectionReason"),
                        "required",
                        "say why goods were rejected",
                    )
                    .into());
                }
                (a, r, why)
            }
        };
        total_accepted += accepted;
        total_rejected += rejected;
        let mut entry: Option<Uuid> = None;
        if accepted > Decimal::ZERO && l.line_kind == "stock" {
            if let Some(item) = l.item_id {
                let key = format!("line:{}", l.line_no);
                let m = post_movement(
                    &mut tx,
                    NewMovement {
                        tenant: t,
                        institution: InstitutionId(g.institution_id),
                        item_id: ItemId(item),
                        location_id: StoreLocationId(g.location_id),
                        kind: StockMovementKind::Receipt,
                        quantity: accepted,
                        unit_cost: Some(l.unit_cost),
                        effective_on: g.received_on,
                        source_type: "goods_receipt",
                        source_id: id,
                        posting_key: &key,
                        reverses_entry_id: None,
                        reason: Some(&g.number),
                        expiry_on: l.expiry_on,
                    },
                    &actor,
                )
                .await?;
                entry = Some(m.entry_id.as_uuid());
            }
        }
        sqlx::query(
            "UPDATE resources.goods_receipt_line SET accepted_quantity = $2, rejected_quantity = $3, \
             rejection_reason = $4, ledger_entry_id = $5 WHERE id = $1",
        )
        .bind(l.id)
        .bind(accepted)
        .bind(rejected)
        .bind(&why)
        .bind(entry)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if accepted > Decimal::ZERO {
            sqlx::query(
                "UPDATE resources.purchase_order_line SET received_quantity = received_quantity + $2 \
                 WHERE id = $1",
            )
            .bind(l.purchase_order_line_id)
            .bind(accepted)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    let to = if total_accepted.is_zero() {
        GoodsReceiptStatus::Rejected
    } else if total_rejected.is_zero() {
        GoodsReceiptStatus::Accepted
    } else {
        GoodsReceiptStatus::PartiallyAccepted
    };
    set_status(&mut tx, t, &g, to, ev::GOODS_RECEIPT_ACCEPTED, None, &actor).await?;
    orders::refresh_fulfilment(&mut tx, t, g.purchase_order_id, &actor).await?;
    let out = load(&mut tx, t, id, false).await?;
    if to == GoodsReceiptStatus::Rejected {
        audit_only(
            &mut tx,
            &scope,
            "goods_receipt",
            id,
            "resources.goods_receipt.reject",
            serde_json::json!({ "number": g.number }),
            &actor,
        )
        .await?;
    } else {
        emit(
            &mut tx,
            &scope,
            ev::GOODS_RECEIPT_ACCEPTED,
            ev::GOODS_RECEIPT_ACCEPTED_SCHEMA,
            "goods_receipt",
            id,
            "resources.goods_receipt.accept",
            &status_payload(
                id,
                out.institution_id,
                Some(from.as_str()),
                out.status.as_str(),
                out.version,
                Some(&o.number),
                None,
                Some(&out.number),
                None,
            ),
            &actor,
        )
        .await?;
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Reject the whole delivery (`resources.procurement.manage`; If-Match; reason required):
/// nothing moves into stock.
#[utoipa::path(post, path = "/api/v1/resources/goods-receipts/{id}/reject", operation_id = "resources_receipt_reject", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the receipt")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GoodsReceiptDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reject(
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
    let scope = receipt_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let g = load(&mut tx, t, id, true).await?;
    check_version(v, g.version)?;
    set_status(
        &mut tx,
        t,
        &g,
        GoodsReceiptStatus::Rejected,
        "sutraerp.resources.goods_receipt.rejected",
        Some(&reason),
        &actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.goods_receipt_line SET rejected_quantity = received_quantity, \
         accepted_quantity = 0, rejection_reason = $2 WHERE goods_receipt_id = $1",
    )
    .bind(id)
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "goods_receipt",
        id,
        "resources.goods_receipt.reject",
        serde_json::json!({ "number": g.number, "reason": reason }),
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
        .routes(routes!(list_all))
        .routes(routes!(get_one))
        .routes(routes!(inspect))
        .routes(routes!(accept))
        .routes(routes!(reject))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_transitions() {
        use GoodsReceiptStatus::*;
        assert!(Received.can_transition(Inspecting) && Inspecting.can_transition(Accepted));
        assert!(Inspecting.can_transition(PartiallyAccepted));
        assert!(!Accepted.can_transition(Rejected));
        assert!(non_negative_qty("q", "0").is_ok() && non_negative_qty("q", "-1").is_err());
    }
}
