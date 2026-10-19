//! HTTP API of the stock-on-hand projection (tag `resources-directory`; contract §5.2).

use crate::shared::guard::require_institution;
use crate::shared::scope::institution_scope;
use crate::shared::ResourcesState;
use axum::extract::{Path, Query, State};
use axum::Json;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::Row;
use sutra_contracts::resources::ids::{ItemId, StoreLocationId};
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// On hand of one item at one location (quantity as a decimal string, three places).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockOnHandLine {
    pub item_id: ItemId,
    pub item_code: String,
    pub item_name: String,
    pub unit: String,
    pub location_id: StoreLocationId,
    pub location_code: String,
    #[schema(value_type = String, example = "12.500")]
    pub on_hand: String,
    #[schema(value_type = Option<String>, example = "5.000")]
    pub reorder_level: Option<String>,
    /// On hand across all locations is at / below the item's reorder level.
    pub at_reorder_level: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StockOnHand {
    pub items: Vec<StockOnHandLine>,
}

/// Filters of the stock-on-hand list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct StockOnHandQuery {
    pub item_id: Option<Uuid>,
    pub location_id: Option<Uuid>,
}

/// Three-place wire string of a quantity (`"12.500"`).
pub fn fmt3(v: Decimal) -> String {
    let mut r = v.round_dp(3);
    r.rescale(3);
    r.to_string()
}

/// Stock on hand per item and location (`resources.read`); zero lines are omitted.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-on-hand", operation_id = "resources_directory_stock_on_hand", tag = "resources-directory",
    params(("id" = String, Path, format = Uuid), StockOnHandQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StockOnHand), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn stock_on_hand(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StockOnHandQuery>,
) -> ApiResult<Json<StockOnHand>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let rows = sqlx::query(
        "SELECT e.item_id, i.code AS item_code, i.name AS item_name, i.unit, e.location_id, \
         l.code AS location_code, sum(e.quantity) AS on_hand, i.reorder_level, \
         (SELECT sum(x.quantity) FROM resources.stock_ledger_entry x \
           WHERE x.tenant_id = e.tenant_id AND x.item_id = e.item_id) AS total \
         FROM resources.stock_ledger_entry e \
         JOIN resources.item i ON i.id = e.item_id \
         JOIN resources.store_location l ON l.id = e.location_id \
         WHERE e.tenant_id = $1 AND e.institution_id = $2 \
           AND ($3::uuid IS NULL OR e.item_id = $3) AND ($4::uuid IS NULL OR e.location_id = $4) \
         GROUP BY e.tenant_id, e.item_id, i.code, i.name, i.unit, e.location_id, l.code, i.reorder_level \
         HAVING sum(e.quantity) <> 0 ORDER BY i.code, l.code LIMIT 2000",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(q.item_id)
    .bind(q.location_id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let on_hand: Decimal = r.try_get("on_hand").map_err(map_db_error)?;
        let total: Option<Decimal> = r.try_get("total").map_err(map_db_error)?;
        let reorder: Option<Decimal> = r.try_get("reorder_level").map_err(map_db_error)?;
        items.push(StockOnHandLine {
            item_id: ItemId(r.try_get("item_id").map_err(map_db_error)?),
            item_code: r.try_get("item_code").map_err(map_db_error)?,
            item_name: r.try_get("item_name").map_err(map_db_error)?,
            unit: r.try_get("unit").map_err(map_db_error)?,
            location_id: StoreLocationId(r.try_get("location_id").map_err(map_db_error)?),
            location_code: r.try_get("location_code").map_err(map_db_error)?,
            on_hand: fmt3(on_hand),
            reorder_level: reorder.map(fmt3),
            at_reorder_level: reorder.is_some_and(|l| total.unwrap_or(Decimal::ZERO) <= l),
        });
    }
    Ok(Json(StockOnHand { items }))
}

pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(stock_on_hand))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantities_have_three_places() {
        assert_eq!(fmt3(Decimal::new(125, 1)), "12.500");
        assert_eq!(fmt3(Decimal::from(3)), "3.000");
    }
}
