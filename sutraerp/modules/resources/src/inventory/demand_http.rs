//! **ERP_EXTENSIONS 2C** — demand, reorder suggestions and dated lots over the stock ledger
//! (rules in [`super::demand`] and [`super::lots`], ported from HeyPuter/everything, MIT).
//! Operation ids `resources_stock_demand_*`, `resources_stock_lot_*`, `resources_stock_expiry_*`
//! and `resources_item_planning_update`, tag `resources-inventory`.
//!
//! Demand is units issued per day (returns, receipts and approved write-ups are stock in; write-
//! downs are not demand; reversed entries and their reversals drop out), measured over the
//! ledger's reach inside the window. Suggestions never write themselves: a reorder level changes
//! only through `stock-demand/apply-thresholds`.
#![allow(clippy::type_complexity)]

use super::demand::{self, Movement, Order, OrderLine, Product};
use super::lots::{self, Lot};
use super::lotstore;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::actor;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::ResourcesState;
use crate::suppliers::common::{audit_only, institution_of, invalid};
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{NaiveDate, Utc};
use rust_decimal::prelude::{FromPrimitive, ToPrimitive};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

fn f(d: Decimal) -> f64 {
    d.to_f64().unwrap_or(0.0)
}

fn dec(x: f64) -> Decimal {
    Decimal::from_f64(x).unwrap_or_default().round_dp(3)
}

// ------------------------------------------------------------------ settings

/// Demand settings of an institution.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InventoryDemandSettings {
    /// Window demand is measured over (7…365 days; default 30).
    pub demand_days: i32,
    /// Days of cover on top of a supplier's lead time (default 3; an item may override).
    pub safety_days: i32,
    /// Days of demand an order should carry (0 = order back up to twice the reorder level).
    pub coverage_days: i32,
    /// "Expiring soon" horizon (default 30 days).
    pub expiry_days: i32,
}

impl Default for InventoryDemandSettings {
    fn default() -> Self {
        Self { demand_days: 30, safety_days: 3, coverage_days: 0, expiry_days: 30 }
    }
}

async fn settings_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Uuid,
) -> Result<InventoryDemandSettings, KernelError> {
    let row: Option<(i32, i32, i32, i32)> = sqlx::query_as(
        "SELECT demand_days, safety_days, coverage_days, expiry_days FROM \
         resources.inventory_demand_settings WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(row.map_or_else(InventoryDemandSettings::default, |(a, b, c, d)| InventoryDemandSettings {
        demand_days: a,
        safety_days: b,
        coverage_days: c,
        expiry_days: d,
    }))
}

/// Demand settings (`resources.read`; defaults until saved).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-demand/settings", operation_id = "resources_stock_demand_settings_get", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = InventoryDemandSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_settings(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<InventoryDemandSettings>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(settings_of(&mut conn, ctx.scope.tenant_id, id).await?))
}

/// Save demand settings (`resources.inventory.manage`; audited).
#[utoipa::path(put, path = "/api/v1/resources/institutions/{id}/stock-demand/settings", operation_id = "resources_stock_demand_settings_put", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), request_body(content = InventoryDemandSettings, content_type = "application/json"),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = InventoryDemandSettings), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn put_settings(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<InventoryDemandSettings>,
) -> ApiResult<Json<InventoryDemandSettings>> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let checks = [
        ("demandDays", b.demand_days, 7, 365),
        ("safetyDays", b.safety_days, 0, 365),
        ("coverageDays", b.coverage_days, 0, 365),
        ("expiryDays", b.expiry_days, 1, 365),
    ];
    for (field, v, lo, hi) in checks {
        if v < lo || v > hi {
            return Err(invalid(field, "out_of_range", &format!("between {lo} and {hi} days")).into());
        }
    }
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO resources.inventory_demand_settings (tenant_id, institution_id, demand_days, \
         safety_days, coverage_days, expiry_days, updated_by) VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (tenant_id, institution_id) DO UPDATE SET demand_days = EXCLUDED.demand_days, \
         safety_days = EXCLUDED.safety_days, coverage_days = EXCLUDED.coverage_days, \
         expiry_days = EXCLUDED.expiry_days, updated_by = EXCLUDED.updated_by, \
         version = resources.inventory_demand_settings.version + 1, updated_at = now()",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(b.demand_days)
    .bind(b.safety_days)
    .bind(b.coverage_days)
    .bind(b.expiry_days)
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "inventory_demand_settings",
        id,
        "resources.inventory_demand_settings.update",
        serde_json::to_value(&b).unwrap_or_default(),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(b))
}

// ------------------------------------------------------------------ demand

/// Demand and reorder suggestion of one item.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockDemandRow {
    pub item_id: Uuid,
    pub item_code: String,
    pub item_name: String,
    pub unit: String,
    #[schema(value_type = String, example = "40.000")]
    pub on_hand: Decimal,
    #[schema(value_type = String, example = "0.000")]
    pub reserved: Decimal,
    /// On hand − reserved: what demand and cover are judged on.
    #[schema(value_type = String, example = "40.000")]
    pub available: Decimal,
    /// Still due on issued / acknowledged / partially received purchase orders.
    #[schema(value_type = String, example = "0.000")]
    pub on_order: Decimal,
    /// Units issued per day over the measured span (0 when unknown).
    pub per_day: f64,
    /// "≈1.4 units a day" (blank when unknown).
    pub rate_label: String,
    /// Days of stock left at that rate (null when unknown).
    pub cover_days: Option<f64>,
    pub cover_label: String,
    pub lead_time_days: Option<i32>,
    /// The safety days used (item override or the institution default).
    pub safety_days: i32,
    /// Lead time + safety days.
    pub restock_days: i64,
    #[schema(value_type = Option<String>)]
    pub pack_size: Option<Decimal>,
    #[schema(value_type = Option<String>)]
    pub reorder_level: Option<Decimal>,
    /// The reorder level demand implies (null when there is no arithmetic to show).
    #[schema(value_type = Option<String>)]
    pub suggested_reorder_level: Option<Decimal>,
    /// Runs out before a restock could land.
    pub running_out: bool,
    /// `out | low | ok` against the reorder level (no level = never low).
    pub status: String,
    /// How many to order now (0 = nothing; whole packs; less what is on order).
    #[schema(value_type = String, example = "24.000")]
    pub suggested_order_quantity: Decimal,
    pub track_expiry: bool,
}

/// Demand of an institution's stock items.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockDemandReport {
    pub window_days: i32,
    /// The ledger's reach inside the window (days).
    pub measured_days: f64,
    /// Under seven days of ledger: no rates, and no suggestions from them.
    pub enough: bool,
    pub items: Vec<StockDemandRow>,
}

/// Demand query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct StockDemandQuery {
    /// Override the window (7…365 days).
    pub days: Option<i32>,
    /// Only items that are out, low, running out, or have something to order.
    pub attention: Option<bool>,
}

type ItemRow = (Uuid, String, String, String, Option<Decimal>, Option<Decimal>, Option<i32>, Option<Decimal>, Option<i32>, bool);

async fn demand_report(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Uuid,
    window: i32,
    s: &InventoryDemandSettings,
) -> Result<StockDemandReport, KernelError> {
    let items: Vec<ItemRow> = sqlx::query_as(
        "SELECT id, code, name, unit, reorder_level, reorder_quantity, lead_time_days, pack_size, \
         safety_days, track_expiry FROM resources.item WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'active' AND kind = 'consumable' ORDER BY code",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let on_hand: BTreeMap<Uuid, Decimal> = sqlx::query_as::<_, (Uuid, Decimal)>(
        "SELECT item_id, sum(quantity) FROM resources.stock_ledger_entry WHERE tenant_id = $1 \
         AND institution_id = $2 GROUP BY item_id",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .into_iter()
    .collect();
    let reserved: BTreeMap<Uuid, Decimal> = sqlx::query_as::<_, (Uuid, Decimal)>(
        "SELECT item_id, sum(quantity) FROM resources.stock_reservation WHERE tenant_id = $1 \
         AND institution_id = $2 AND status = 'active' GROUP BY item_id",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .into_iter()
    .collect();
    // The ledger as Everything's activity log. Reversed entries and their reversals drop out.
    let oldest: Option<NaiveDate> = sqlx::query_scalar(
        "SELECT min(effective_on) FROM resources.stock_ledger_entry WHERE tenant_id = $1 \
         AND institution_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let rows: Vec<(Uuid, String, Decimal, NaiveDate)> = sqlx::query_as(
        "SELECT e.item_id, e.kind, e.quantity, e.effective_on FROM resources.stock_ledger_entry e \
         WHERE e.tenant_id = $1 AND e.institution_id = $2 AND e.effective_on >= $3 \
         AND e.kind IN ('receipt', 'issue', 'return_to_store', 'adjustment') \
         AND NOT EXISTS (SELECT 1 FROM resources.stock_ledger_entry r WHERE r.tenant_id = e.tenant_id \
             AND r.reverses_entry_id = e.id)",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind((Utc::now() - chrono::Duration::days(i64::from(window) + 1)).date_naive())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let at = |d: NaiveDate| {
        d.and_hms_opt(12, 0, 0).map_or(0.0, |t| t.and_utc().timestamp_millis() as f64)
    };
    let now = Utc::now().timestamp_millis() as f64;
    let mut log: Vec<Movement> = rows
        .into_iter()
        .map(|(item, kind, q, d)| Movement {
            product_id: Some(item.to_string()),
            kind: if q > Decimal::ZERO { "add".into() } else { "remove".into() },
            delta: f(q),
            at: Some(at(d)),
            reason: (kind == "adjustment" && q < Decimal::ZERO).then_some("lost".to_owned()),
        })
        .collect();
    // The log's reach: the oldest entry ever, as a zero-delta marker.
    if let Some(o) = oldest {
        log.push(Movement { product_id: None, kind: "note".into(), delta: 0.0, at: Some(at(o)), reason: None });
    }
    let rates = demand::demand_rates(&log, f64::from(window), now);
    let po: Vec<(Uuid, Decimal, Decimal)> = sqlx::query_as(
        "SELECT pl.item_id, pl.quantity, pl.received_quantity FROM resources.purchase_order_line pl \
         JOIN resources.purchase_order o ON o.id = pl.purchase_order_id WHERE o.tenant_id = $1 \
         AND o.institution_id = $2 AND o.status IN ('issued', 'acknowledged', 'partially_received') \
         AND pl.line_kind = 'stock' AND pl.item_id IS NOT NULL",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let orders = vec![Order {
        status: "ordered".into(),
        lines: po
            .into_iter()
            .map(|(item, q, r)| OrderLine { product_id: Some(item.to_string()), qty: f(q), received: f(r) })
            .collect(),
    }];
    let incoming = demand::on_order_qty(&orders, &["ordered"]);
    let mut out = Vec::with_capacity(items.len());
    for (id, code, name, unit, level, reorder_qty, lead, pack, safety, track) in items {
        let oh = on_hand.get(&id).copied().unwrap_or_default();
        let res = reserved.get(&id).copied().unwrap_or_default();
        let avail = (oh - res).max(Decimal::ZERO);
        let key = id.to_string();
        let per_day = rates.rates.get(&key).copied().unwrap_or(0.0);
        let inbound = incoming.get(&key).copied().unwrap_or(0.0);
        let safety_days = safety.unwrap_or(s.safety_days);
        let lead_f = f64::from(lead.unwrap_or(0));
        let p = Product {
            id: key,
            quantity: f(avail),
            unit: Some(unit.clone()),
            pack_size: pack.map(f),
            // No reorder level in SutraERP means "never warn", not Everything's default of 5.
            low_stock: Some(level.map_or(0.0, f)),
            reorder_qty: reorder_qty.map(f),
        };
        let cover = demand::cover_days(p.quantity, per_day);
        out.push(StockDemandRow {
            item_id: id,
            item_code: code,
            item_name: name,
            unit: unit.clone(),
            on_hand: oh,
            reserved: res,
            available: avail,
            on_order: dec(inbound),
            per_day,
            rate_label: demand::rate_label(per_day, Some(&unit)),
            cover_days: cover,
            cover_label: demand::cover_label(cover),
            lead_time_days: lead,
            safety_days,
            restock_days: demand::restock_days(lead_f, f64::from(safety_days)),
            pack_size: pack,
            reorder_level: level,
            suggested_reorder_level: demand::suggested_threshold(&p, per_day, lead_f, f64::from(safety_days)).map(dec),
            running_out: demand::running_out(p.quantity, per_day, lead_f, f64::from(safety_days)),
            status: demand::stock_status(&p, 0.0).to_owned(),
            suggested_order_quantity: dec(demand::suggested_qty(
                &p,
                0.0,
                inbound,
                per_day,
                f64::from(s.coverage_days),
            )),
            track_expiry: track,
        });
    }
    Ok(StockDemandReport { window_days: window, measured_days: rates.days, enough: rates.enough, items: out })
}

fn needs_attention(r: &StockDemandRow) -> bool {
    r.status != "ok" || r.running_out
}

/// Demand, cover and reorder suggestions of every active consumable (`resources.read`):
/// most urgent first — out of stock, then fewest days of cover, unknown demand last.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-demand", operation_id = "resources_stock_demand_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), StockDemandQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StockDemandReport), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_demand(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StockDemandQuery>,
) -> ApiResult<Json<StockDemandReport>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = settings_of(&mut conn, ctx.scope.tenant_id, id).await?;
    let window = q.days.unwrap_or(s.demand_days);
    if !(7..=365).contains(&window) {
        return Err(invalid("days", "out_of_range", "between 7 and 365 days").into());
    }
    let mut r = demand_report(&mut conn, ctx.scope.tenant_id, id, window, &s).await?;
    if q.attention.unwrap_or(false) {
        r.items.retain(needs_attention);
    }
    let urgency = |x: &StockDemandRow| {
        if x.status == "out" {
            -1.0
        } else {
            x.cover_days.unwrap_or(f64::INFINITY)
        }
    };
    r.items.sort_by(|a, b| urgency(a).partial_cmp(&urgency(b)).unwrap_or(std::cmp::Ordering::Equal));
    Ok(Json(r))
}

/// Items whose reorder level should take the demand suggestion.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyThresholds {
    pub item_ids: Vec<Uuid>,
}

/// What applying suggestions changed.
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApplyThresholdsResult {
    pub updated_ids: Vec<Uuid>,
    /// Items with no suggestion (unknown demand), already at it, or not found.
    pub skipped: usize,
}

/// Set the reorder level of the chosen items to what demand suggests
/// (`resources.inventory.manage`; audited per item).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/stock-demand/apply-thresholds", operation_id = "resources_stock_demand_apply_thresholds", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), request_body(content = ApplyThresholds, content_type = "application/json"),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ApplyThresholdsResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn apply_thresholds(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ApplyThresholds>,
) -> ApiResult<Json<ApplyThresholdsResult>> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if b.item_ids.is_empty() || b.item_ids.len() > 500 {
        return Err(invalid("itemIds", "count", "between 1 and 500 items").into());
    }
    let actor = actor(ctx);
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = settings_of(&mut tx, t, id).await?;
    let report = demand_report(&mut tx, t, id, s.demand_days, &s).await?;
    let mut result = ApplyThresholdsResult::default();
    for item in &b.item_ids {
        let Some(row) = report.items.iter().find(|r| r.item_id == *item) else {
            result.skipped += 1;
            continue;
        };
        let Some(level) = row.suggested_reorder_level.filter(|l| Some(*l) != row.reorder_level) else {
            result.skipped += 1;
            continue;
        };
        sqlx::query(
            "UPDATE resources.item SET reorder_level = $3, version = version + 1, updated_at = now() \
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(item)
        .bind(t.as_uuid())
        .bind(level)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        audit_only(
            &mut tx,
            &scope,
            "item",
            *item,
            "resources.item.reorder_level.apply_suggestion",
            serde_json::json!({
                "from": row.reorder_level.map(|l| l.to_string()),
                "to": level.to_string(),
                "perDay": row.per_day,
                "restockDays": row.restock_days,
            }),
            &actor,
        )
        .await?;
        result.updated_ids.push(*item);
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(result))
}

// ------------------------------------------------------------------ item planning

/// Planning fields of an item.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemPlanning {
    /// Supplier lead time in days (0…365).
    pub lead_time_days: Option<i32>,
    /// Bought in packs of this many (> 1 rounds orders up to whole packs).
    #[schema(value_type = Option<String>, example = "12")]
    pub pack_size: Option<Decimal>,
    /// Override of the institution's safety days.
    pub safety_days: Option<i32>,
    /// Keep dated lots (expiry) for this item.
    pub track_expiry: bool,
}

/// Set an item's planning fields (`resources.inventory.manage`; audited). Switching expiry
/// tracking on starts undated lots for the stock already on hand.
#[utoipa::path(put, path = "/api/v1/resources/items/{id}/planning", operation_id = "resources_item_planning_update", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), request_body(content = ItemPlanning, content_type = "application/json"),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ItemPlanning), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn put_planning(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(b): Json<ItemPlanning>,
) -> ApiResult<Json<ItemPlanning>> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let t = ctx.scope.tenant_id;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let institution = institution_of(&mut tx, t, "item", "item", id).await?;
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if b.lead_time_days.is_some_and(|d| !(0..=365).contains(&d)) {
        return Err(invalid("leadTimeDays", "out_of_range", "between 0 and 365 days").into());
    }
    if b.safety_days.is_some_and(|d| !(0..=365).contains(&d)) {
        return Err(invalid("safetyDays", "out_of_range", "between 0 and 365 days").into());
    }
    if b.pack_size.is_some_and(|p| p <= Decimal::ZERO || p.normalize().scale() > 3) {
        return Err(invalid("packSize", "invalid", "> 0 with at most three decimals").into());
    }
    sqlx::query(
        "UPDATE resources.item SET lead_time_days = $3, pack_size = $4, safety_days = $5, \
         track_expiry = $6, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(t.as_uuid())
    .bind(b.lead_time_days)
    .bind(b.pack_size)
    .bind(b.safety_days)
    .bind(b.track_expiry)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if b.track_expiry {
        lotstore::sync_item(&mut tx, t, institution, id).await?;
    } else {
        sqlx::query("DELETE FROM resources.stock_lot WHERE tenant_id = $1 AND item_id = $2")
            .bind(t.as_uuid())
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
    }
    let actor = actor(ctx);
    audit_only(
        &mut tx,
        &scope,
        "item",
        id,
        "resources.item.planning.update",
        serde_json::to_value(&b).unwrap_or_default(),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(b))
}

// ------------------------------------------------------------------ lots and expiry

/// One dated lot at a location.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockLotView {
    pub location_id: Uuid,
    pub location_code: String,
    /// Null = date unknown.
    pub expiry_on: Option<NaiveDate>,
    #[schema(value_type = String, example = "12.000")]
    pub quantity: Decimal,
}

/// An item's lots, first-expired-first-out, with how it stands against the calendar.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockLotList {
    pub item_id: Uuid,
    pub track_expiry: bool,
    pub horizon_days: i32,
    pub status: lots::ExpiryStatus,
    pub items: Vec<StockLotView>,
}

/// An item's dated lots (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/items/{id}/lots", operation_id = "resources_stock_lot_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StockLotList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_lots(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StockLotList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let institution = institution_of(&mut conn, t, "item", "item", id).await?;
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let today = business_today(&st, &scope, institution).await?;
    let s = settings_of(&mut conn, t, institution.as_uuid()).await?;
    let track: bool = sqlx::query_scalar("SELECT track_expiry FROM resources.item WHERE id = $1 AND tenant_id = $2")
        .bind(id)
        .bind(t.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let rows: Vec<(Uuid, String, Option<NaiveDate>, Decimal)> = sqlx::query_as(
        "SELECT s.location_id, l.code, s.expiry_on, s.quantity FROM resources.stock_lot s \
         JOIN resources.store_location l ON l.id = s.location_id WHERE s.tenant_id = $1 \
         AND s.item_id = $2 ORDER BY s.expiry_on NULLS LAST, l.code",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let merged: Vec<Lot> = rows
        .iter()
        .map(|(_, _, d, q)| Lot { date: d.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(), qty: f(*q) })
        .collect();
    let status = lots::expiry_status(&merged, today, i64::from(s.expiry_days));
    Ok(Json(StockLotList {
        item_id: id,
        track_expiry: track,
        horizon_days: s.expiry_days,
        status,
        items: rows
            .into_iter()
            .map(|(location_id, location_code, expiry_on, quantity)| StockLotView {
                location_id,
                location_code,
                expiry_on,
                quantity,
            })
            .collect(),
    }))
}

/// An item with stock expired or expiring.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockExpiryRow {
    pub item_id: Uuid,
    pub item_code: String,
    pub item_name: String,
    pub unit: String,
    #[schema(value_type = String, example = "4.000")]
    pub expired: Decimal,
    #[schema(value_type = String, example = "6.000")]
    pub soon: Decimal,
    pub soonest: Option<NaiveDate>,
    /// (expired + soon) × standard cost, ₹.
    #[schema(value_type = String, example = "480.00")]
    pub value: Decimal,
}

/// Expiry report.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockExpiryReport {
    pub as_of: NaiveDate,
    pub horizon_days: i32,
    pub items: Vec<StockExpiryRow>,
}

/// Expiry query.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct StockExpiryQuery {
    /// Horizon override (1…365 days).
    pub days: Option<i32>,
}

/// Items that track expiry with anything expired or expiring within the horizon
/// (`resources.read`): anything expired first, then by value at standard cost.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-expiry", operation_id = "resources_stock_expiry_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), StockExpiryQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StockExpiryReport), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_expiry(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StockExpiryQuery>,
) -> ApiResult<Json<StockExpiryReport>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = settings_of(&mut conn, t, id).await?;
    let horizon = q.days.unwrap_or(s.expiry_days);
    if !(1..=365).contains(&horizon) {
        return Err(invalid("days", "out_of_range", "between 1 and 365 days").into());
    }
    let today = business_today(&st, &scope, institution).await?;
    let items: Vec<(Uuid, String, String, String, Option<Decimal>, String)> = sqlx::query_as(
        "SELECT id, code, name, unit, standard_cost, status FROM resources.item WHERE tenant_id = $1 \
         AND institution_id = $2 AND track_expiry",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let lot_rows: Vec<(Uuid, Option<NaiveDate>, Decimal)> = sqlx::query_as(
        "SELECT item_id, expiry_on, quantity FROM resources.stock_lot WHERE tenant_id = $1 \
         AND institution_id = $2",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let tracked: Vec<lots::Tracked> = items
        .iter()
        .map(|(iid, _, _, _, cost, status)| lots::Tracked {
            id: iid.to_string(),
            cost: cost.map_or(0.0, f),
            archived: status == "archived",
            track_expiry: true,
            lots: lot_rows
                .iter()
                .filter(|(i, _, _)| i == iid)
                .map(|(_, d, q)| Lot { date: d.map(|d| d.format("%Y-%m-%d").to_string()).unwrap_or_default(), qty: f(*q) })
                .collect(),
        })
        .collect();
    let rows = lots::expiring_products(&tracked, today, i64::from(horizon))
        .into_iter()
        .filter_map(|e| {
            let (iid, code, name, unit, _, _) = items.iter().find(|x| x.0.to_string() == e.id)?;
            Some(StockExpiryRow {
                item_id: *iid,
                item_code: code.clone(),
                item_name: name.clone(),
                unit: unit.clone(),
                expired: dec(e.status.expired),
                soon: dec(e.status.soon),
                soonest: NaiveDate::parse_from_str(&e.status.soonest, "%Y-%m-%d").ok(),
                value: Decimal::from_f64(e.value).unwrap_or_default().round_dp(2),
            })
        })
        .collect();
    Ok(Json(StockExpiryReport { as_of: today, horizon_days: horizon, items: rows }))
}

/// Routes of demand, planning, lots and expiry.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list_demand))
        .routes(routes!(get_settings, put_settings))
        .routes(routes!(apply_thresholds))
        .routes(routes!(put_planning))
        .routes(routes!(list_lots))
        .routes(routes!(list_expiry))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings() {
        let s = InventoryDemandSettings::default();
        assert_eq!((s.demand_days, s.safety_days, s.coverage_days, s.expiry_days), (30, 3, 0, 30));
    }
}
