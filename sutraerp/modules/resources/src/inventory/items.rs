//! Item master (L3-J-051) and stores / locations (L3-J-053) over the foundation tables
//! `resources.item` / `resources.store_location` (8D writes them; contract §3.3), plus the
//! stock movement list over the append-only ledger (L3-J-058, read only).

use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers::common::{
    audit_only, check_version, clean, expected, history, illegal, institution_of, invalid,
    optional_text, parse_money, reason_of, required_text, valid_code, ResourcesReasonBody,
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
use std::str::FromStr;
use sutra_contracts::resources::status::{ItemKind, ItemStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Legal item moves (draft → active ↔ inactive; archiving is deferred, J-052).
pub fn item_can_transition(from: ItemStatus, to: ItemStatus) -> bool {
    matches!(
        (from, to),
        (ItemStatus::Draft, ItemStatus::Active)
            | (ItemStatus::Active, ItemStatus::Inactive)
            | (ItemStatus::Inactive, ItemStatus::Active)
    )
}

/// An item of the item master with its on hand across the institution's locations.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItemView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "PAPER-A4")]
    pub code: String,
    pub name: String,
    /// `consumable | asset | service`.
    pub kind: String,
    pub category: Option<String>,
    pub unit: String,
    #[schema(value_type = Option<String>, example = "12.000")]
    pub reorder_level: Option<Decimal>,
    #[schema(value_type = Option<String>, example = "50.000")]
    pub reorder_quantity: Option<Decimal>,
    #[schema(value_type = Option<String>, example = "250.00")]
    pub standard_cost: Option<Decimal>,
    /// `draft | active | inactive | archived`.
    pub status: String,
    #[schema(value_type = String, example = "40.000")]
    pub on_hand: Decimal,
    #[schema(value_type = String, example = "5.000")]
    pub reserved: Decimal,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InventoryItemList {
    pub items: Vec<InventoryItemView>,
}

const ITEM_SELECT: &str = "SELECT i.id, i.institution_id, i.code, i.name, i.kind, i.category, \
    i.unit, i.reorder_level, i.reorder_quantity, i.standard_cost, i.status, \
    COALESCE((SELECT sum(e.quantity) FROM resources.stock_ledger_entry e \
              WHERE e.tenant_id = i.tenant_id AND e.item_id = i.id), 0)::numeric(14,3) AS on_hand, \
    COALESCE((SELECT sum(r.quantity) FROM resources.stock_reservation r \
              WHERE r.tenant_id = i.tenant_id AND r.item_id = i.id AND r.status = 'active'), 0)::numeric(14,3) AS reserved, \
    i.version, i.created_at FROM resources.item i";

async fn load_item_view(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<InventoryItemView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF i" } else { "" };
    sqlx::query_as::<_, InventoryItemView>(&format!(
        "{ITEM_SELECT} WHERE i.id = $1 AND i.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "item" })
}

async fn item_response(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let v = load_item_view(conn, tenant, id, false).await?;
    Ok(with_etag(status, &v, Version(v.version)))
}

fn opt_qty(
    field: &str,
    raw: Option<String>,
    allow_zero: bool,
) -> Result<Option<Decimal>, KernelError> {
    let Some(raw) = clean(raw) else {
        return Ok(None);
    };
    let v = Decimal::from_str(&raw).map_err(|_| {
        invalid(
            field,
            "invalid_number",
            "must be a decimal string such as \"12.000\"",
        )
    })?;
    if v.normalize().scale() > 3 || v < Decimal::ZERO || (!allow_zero && v.is_zero()) {
        return Err(invalid(
            field,
            "invalid",
            if allow_zero {
                "≥ 0 with at most three decimals"
            } else {
                "> 0 with at most three decimals"
            },
        ));
    }
    Ok(Some(v))
}

/// Create an item.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItemCreate {
    pub code: String,
    pub name: String,
    /// `consumable | asset | service`.
    pub kind: String,
    pub category: Option<String>,
    /// Default `each`.
    pub unit: Option<String>,
    pub reorder_level: Option<String>,
    pub reorder_quantity: Option<String>,
    pub standard_cost: Option<String>,
}

/// Change an item's attributes.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InventoryItemUpdate {
    pub name: Option<String>,
    pub category: Option<String>,
    pub unit: Option<String>,
    pub reorder_level: Option<String>,
    pub reorder_quantity: Option<String>,
    pub standard_cost: Option<String>,
}

/// Register an item in the item master (`resources.inventory.manage`; Idempotency-Key):
/// `draft`; duplicate code → 409.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/items", operation_id = "resources_item_create", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = InventoryItemCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = InventoryItemView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_item(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<InventoryItemCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = b.code.trim().to_uppercase();
    if !valid_code(&code) {
        return Err(invalid("code", "invalid_code", "^[A-Z0-9][A-Z0-9_.-]{0,31}$").into());
    }
    let name = required_text("name", &b.name, 200)?;
    let kind = ItemKind::parse(b.kind.trim())
        .ok_or_else(|| invalid("kind", "invalid", "consumable, asset or service"))?;
    let unit = optional_text("unit", b.unit, 16)?.unwrap_or_else(|| "each".to_owned());
    let category = optional_text("category", b.category, 100)?;
    let reorder_level = opt_qty("reorderLevel", b.reorder_level, true)?;
    let reorder_quantity = opt_qty("reorderQuantity", b.reorder_quantity, false)?;
    let standard_cost = match clean(b.standard_cost) {
        Some(c) => Some(parse_money("standardCost", &c)?),
        None => None,
    };
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let iid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.item (tenant_id, institution_id, code, name, kind, category, unit, \
         reorder_level, reorder_quantity, standard_cost) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .bind(&name)
    .bind(kind.as_str())
    .bind(&category)
    .bind(&unit)
    .bind(reorder_level)
    .bind(reorder_quantity)
    .bind(standard_cost)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!("item code {code} is already in use")),
        other => other,
    })?;
    history(
        &mut tx,
        t,
        institution,
        "item",
        iid,
        None,
        "draft",
        "sutraerp.resources.item.created",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "item",
        iid,
        "resources.item.create",
        serde_json::json!({ "code": code, "kind": kind.as_str() }),
        &actor,
    )
    .await?;
    let res = item_response(&mut tx, t, iid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Items of an institution (`resources.read`; `?status=`), by code.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/items", operation_id = "resources_item_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = InventoryItemList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_items(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<InventoryItemList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, InventoryItemView>(&format!(
        "{ITEM_SELECT} WHERE i.tenant_id = $1 AND i.institution_id = $2 \
         AND ($3::text IS NULL OR i.status = $3) ORDER BY i.code LIMIT 1000"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(InventoryItemList { items }))
}

/// One item (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/items/{id}", operation_id = "resources_item_get", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = InventoryItemView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_item(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(item_response(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

async fn item_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(&mut conn, ctx.scope.tenant_id, "item", "item", id).await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// Change name, category, unit, reorder level / quantity, standard cost
/// (`resources.inventory.manage`; If-Match; not archived).
#[utoipa::path(patch, path = "/api/v1/resources/items/{id}", operation_id = "resources_item_update", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the item")),
    request_body = InventoryItemUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InventoryItemView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn update_item(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<InventoryItemUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    let name = match b.name {
        Some(n) => Some(required_text("name", &n, 200)?),
        None => None,
    };
    let category = optional_text("category", b.category, 100)?;
    let unit = optional_text("unit", b.unit, 16)?;
    let reorder_level = opt_qty("reorderLevel", b.reorder_level, true)?;
    let reorder_quantity = opt_qty("reorderQuantity", b.reorder_quantity, false)?;
    let standard_cost = match clean(b.standard_cost) {
        Some(c) => Some(parse_money("standardCost", &c)?),
        None => None,
    };
    let t = ctx.scope.tenant_id;
    let scope = item_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let item = load_item_view(&mut tx, t, id, true).await?;
    check_version(v, item.version)?;
    if item.status == "archived" {
        return Err(KernelError::Conflict(format!("item {} is archived", item.code)).into());
    }
    sqlx::query(
        "UPDATE resources.item SET name = COALESCE($2, name), category = COALESCE($3, category), \
         unit = COALESCE($4, unit), reorder_level = COALESCE($5, reorder_level), \
         reorder_quantity = COALESCE($6, reorder_quantity), \
         standard_cost = COALESCE($7, standard_cost), version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(&category)
    .bind(&unit)
    .bind(reorder_level)
    .bind(reorder_quantity)
    .bind(standard_cost)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "item",
        id,
        "resources.item.update",
        serde_json::json!({ "code": item.code }),
        &actor,
    )
    .await?;
    let res = item_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

#[allow(clippy::too_many_arguments)]
async fn move_item(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
    v: i64,
    to: ItemStatus,
    reason: Option<String>,
    action: &'static str,
) -> Result<Response, KernelError> {
    let t = ctx.scope.tenant_id;
    let scope = item_scope(st, ctx, id).await?;
    let actor: Actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let item = load_item_view(&mut tx, t, id, true).await?;
    check_version(v, item.version)?;
    let from = ItemStatus::parse(&item.status)
        .ok_or_else(|| KernelError::Conflict(format!("unknown item status {}", item.status)))?;
    if !item_can_transition(from, to) {
        return Err(illegal("item", &item.code, from.as_str(), to.as_str()));
    }
    sqlx::query(
        "UPDATE resources.item SET status = $2, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        InstitutionId(item.institution_id),
        "item",
        id,
        Some(from.as_str()),
        to.as_str(),
        "sutraerp.resources.item.status",
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "item",
        id,
        action,
        serde_json::json!({ "code": item.code, "reason": reason }),
        &actor,
    )
    .await?;
    let res = item_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Activate an item (`resources.inventory.manage`; If-Match): only active items are bought,
/// stocked and issued.
#[utoipa::path(post, path = "/api/v1/resources/items/{id}/activate", operation_id = "resources_item_activate", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the item")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InventoryItemView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn activate_item(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    Ok(move_item(
        &st,
        ctx,
        id,
        v,
        ItemStatus::Active,
        reason_of(body),
        "resources.item.activate",
    )
    .await?)
}

/// Deactivate an item (`resources.inventory.manage`; If-Match): no new purchases or movements.
#[utoipa::path(post, path = "/api/v1/resources/items/{id}/deactivate", operation_id = "resources_item_deactivate", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the item")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InventoryItemView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn deactivate_item(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    Ok(move_item(
        &st,
        ctx,
        id,
        v,
        ItemStatus::Inactive,
        reason_of(body),
        "resources.item.deactivate",
    )
    .await?)
}

// ------------------------------------------------------------------ stores and locations

/// A store (`parentId = null`) or a location inside one.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct StoreLocationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub parent_id: Option<Uuid>,
    #[schema(example = "CENTRAL")]
    pub code: String,
    pub name: String,
    /// Domain G infrastructure node the store sits in (plain uuid).
    pub campus_node_id: Option<Uuid>,
    /// `active | inactive`.
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StoreLocationList {
    pub items: Vec<StoreLocationView>,
}

const STORE_SELECT: &str = "SELECT id, institution_id, parent_id, code, name, campus_node_id, \
    status, version, created_at FROM resources.store_location";

async fn load_store(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<StoreLocationView, KernelError> {
    sqlx::query_as::<_, StoreLocationView>(&format!(
        "{STORE_SELECT} WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "store location",
    })
}

/// Create a store or a location.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreLocationCreate {
    pub code: String,
    pub name: String,
    /// Domain G infrastructure node (stores only; plain uuid).
    pub campus_node_id: Option<Uuid>,
}

#[allow(clippy::too_many_arguments)]
async fn insert_store(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    institution: InstitutionId,
    parent: Option<Uuid>,
    b: StoreLocationCreate,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let code = b.code.trim().to_uppercase();
    if !valid_code(&code) {
        return Err(invalid(
            "code",
            "invalid_code",
            "^[A-Z0-9][A-Z0-9_.-]{0,31}$",
        ));
    }
    let name = required_text("name", &b.name, 200)?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.store_location (tenant_id, institution_id, parent_id, code, name, \
         campus_node_id) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(parent)
    .bind(&code)
    .bind(&name)
    .bind(b.campus_node_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict(format!("store location code {code} is already in use"))
        }
        other => other,
    })?;
    audit_only(
        conn,
        scope,
        "store_location",
        id,
        "resources.store_location.create",
        serde_json::json!({ "code": code, "parentId": parent }),
        actor,
    )
    .await?;
    Ok(id)
}

/// Create a store (`resources.inventory.manage`; Idempotency-Key; duplicate code → 409).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/stores", operation_id = "resources_store_create", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = StoreLocationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = StoreLocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_store(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StoreLocationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sid = insert_store(&mut tx, &scope, t, institution, None, b, &actor).await?;
    let out = load_store(&mut tx, t, sid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &out, Version(out.version)))
}

/// Stores and locations of an institution (`resources.read`), stores first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stores", operation_id = "resources_store_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StoreLocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_stores(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StoreLocationList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, StoreLocationView>(&format!(
        "{STORE_SELECT} WHERE tenant_id = $1 AND institution_id = $2 \
         ORDER BY parent_id NULLS FIRST, code LIMIT 1000"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(StoreLocationList { items }))
}

/// Add a location (rack / bin / room) inside a store (`resources.inventory.manage`;
/// Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/resources/stores/{id}/locations", operation_id = "resources_store_location_create", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = StoreLocationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = StoreLocationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_location(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StoreLocationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let store = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_store(&mut conn, t, id).await?
    };
    if store.parent_id.is_some() {
        return Err(KernelError::Conflict(format!(
            "{} is a location, not a store; locations sit directly in a store",
            store.code
        ))
        .into());
    }
    let institution = InstitutionId(store.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let lid = insert_store(&mut tx, &scope, t, institution, Some(id), b, &actor).await?;
    let out = load_store(&mut tx, t, lid).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &out, Version(out.version)))
}

/// Locations of a store (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/stores/{id}/locations", operation_id = "resources_store_location_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StoreLocationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_locations(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StoreLocationList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_store(&mut conn, t, id).await?;
    let items = sqlx::query_as::<_, StoreLocationView>(&format!(
        "{STORE_SELECT} WHERE tenant_id = $1 AND parent_id = $2 ORDER BY code"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Json(StoreLocationList { items }))
}

// ------------------------------------------------------------------ stock movements (read)

/// One ledger movement.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct StockMovementView {
    pub id: Uuid,
    pub item_id: Uuid,
    pub item_code: String,
    pub location_id: Uuid,
    pub location_code: String,
    /// `receipt | issue | return_to_store | transfer_out | transfer_in | adjustment | reversal`.
    pub kind: String,
    #[schema(value_type = String, example = "-4.000")]
    pub quantity: Decimal,
    #[schema(value_type = Option<String>, example = "240.00")]
    pub unit_cost: Option<Decimal>,
    pub effective_on: NaiveDate,
    pub source_type: String,
    pub source_id: Uuid,
    pub posting_key: String,
    pub reverses_entry_id: Option<Uuid>,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockMovementList {
    pub items: Vec<StockMovementView>,
    /// Pass as `cursor` for the next (older) page.
    pub next_cursor: Option<Uuid>,
}

/// Filters of the movement list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct StockMovementQuery {
    pub item_id: Option<Uuid>,
    pub location_id: Option<Uuid>,
    /// ≤ 200, default 50.
    pub limit: Option<i64>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<Uuid>,
}

/// Stock ledger movements of an institution, newest first (`resources.read`; `?itemId=`,
/// `?locationId=`, `?limit=`, `?cursor=`).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-movements", operation_id = "resources_stock_movement_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), StockMovementQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StockMovementList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_movements(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StockMovementQuery>,
) -> ApiResult<Json<StockMovementList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let items = sqlx::query_as::<_, StockMovementView>(
        "SELECT e.id, e.item_id, i.code AS item_code, e.location_id, l.code AS location_code, e.kind, \
         e.quantity, e.unit_cost, e.effective_on, e.source_type, e.source_id, e.posting_key, \
         e.reverses_entry_id, e.reason, e.actor, e.created_at FROM resources.stock_ledger_entry e \
         JOIN resources.item i ON i.id = e.item_id \
         JOIN resources.store_location l ON l.id = e.location_id \
         WHERE e.tenant_id = $1 AND e.institution_id = $2 \
           AND ($3::uuid IS NULL OR e.item_id = $3) AND ($4::uuid IS NULL OR e.location_id = $4) \
           AND ($5::uuid IS NULL OR e.id < $5) ORDER BY e.id DESC LIMIT $6",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.item_id)
    .bind(q.location_id)
    .bind(q.cursor)
    .bind(limit)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let next_cursor = if items.len() as i64 == limit {
        items.last().map(|m| m.id)
    } else {
        None
    };
    Ok(Json(StockMovementList { items, next_cursor }))
}

/// Routes of the item master, stores and the movement list.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_item, list_items))
        .routes(routes!(get_item, update_item))
        .routes(routes!(activate_item))
        .routes(routes!(deactivate_item))
        .routes(routes!(create_store, list_stores))
        .routes(routes!(create_location, list_locations))
        .routes(routes!(list_movements))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_moves_and_quantities() {
        assert!(item_can_transition(ItemStatus::Draft, ItemStatus::Active));
        assert!(item_can_transition(
            ItemStatus::Inactive,
            ItemStatus::Active
        ));
        assert!(!item_can_transition(
            ItemStatus::Draft,
            ItemStatus::Inactive
        ));
        assert!(opt_qty("x", Some("0".into()), true).is_ok());
        assert!(opt_qty("x", Some("0".into()), false).is_err());
        assert!(opt_qty("x", Some("1.2345".into()), true).is_err());
        assert_eq!(opt_qty("x", None, true).ok(), Some(None));
    }
}
