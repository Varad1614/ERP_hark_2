//! **5C** — Bank accounts, bank execution of payment populations, statement import → reconciliation, cash position, fund transfers, failed bank transactions (L3-F-056…061).
//!
//! Owned by slice 5C (`docs/batch5-contracts.md` §1/§2). Tag `finance-treasury`; every operation id
//! starts with the prefixes of §5.2 (enforced by `apps/api/tests/architecture.rs`).

pub mod bankfile;
pub mod domain;
pub mod reconcile;
pub mod reconcile_http;
pub mod http;

use crate::shared::FinanceState;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Routes of this area.
pub fn router(state: FinanceState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(http::bank_account_list, http::bank_account_create))
        .routes(routes!(http::bank_account_get))
        .routes(routes!(http::bank_account_decide))
        .routes(routes!(http::bank_account_suspend))
        .routes(routes!(http::bank_account_resume))
        .routes(routes!(http::bank_account_close))
        .routes(routes!(http::bank_settlement_create))
        .routes(routes!(http::bank_transaction_list))
        .routes(routes!(http::bank_transaction_return))
        .routes(routes!(http::statement_list, http::statement_import))
        .routes(routes!(http::statement_preview))
        .routes(routes!(http::statement_get))
        .routes(routes!(http::statement_reconcile))
        .routes(routes!(http::cash_position))
        // ERP_EXTENSIONS 1B
        .routes(routes!(reconcile_http::bank_recon_workspace))
        .routes(routes!(reconcile_http::bank_recon_preview))
        .routes(routes!(reconcile_http::bank_recon_list, reconcile_http::bank_recon_finish))
        .routes(routes!(reconcile_http::bank_recon_undo))
        .routes(routes!(reconcile_http::bank_recon_brs))
        .routes(routes!(reconcile_http::bank_recon_discrepancies))
        .routes(routes!(reconcile_http::statement_line_suggestions))
        .with_state(state)
}
