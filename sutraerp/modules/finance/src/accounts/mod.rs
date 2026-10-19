//! **Foundation** — finance party accounts and their balances (read-only), shared by every
//! screen: Admin Finance account lookups, Student Fees summary. Tag `finance-accounts`,
//! operation ids `finance_account_*`.
//!
//! Charges, payments and the ledger are the slices'; this area only reads the foundation tables
//! through the ports (`ChargeDirectory`, `ReceivableReader`).

pub mod http;

use crate::shared::FinanceState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    http::router(state)
}
