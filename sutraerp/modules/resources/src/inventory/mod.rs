//! **8D** — items (item master), stores and locations, stock on hand, issues, reservations, returns, approved adjustments, reorder levels → replenishment needs (L3-J-051, 053, 057…059, 061, 064…066). Operation ids `resources_item_*`, `resources_store_*`, `resources_stock_*`, tag `resources-inventory`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * [`items`] — item master, stores / locations (foundation tables, 8D writes), the movement list;
//! * [`stock`] — issues + returns, reservations, adjustments, replenishment. Every movement goes
//!   through the foundation's single ledger write path `shared::stock::post_movement`; stock on
//!   hand is the foundation API `GET /institutions/{id}/stock-on-hand`.

pub mod demand;
pub mod items;
pub mod lots;
pub mod lotstore;
pub mod stock;

use crate::shared::ResourcesState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(items::router(state.clone()))
        .merge(stock::router(state))
}
