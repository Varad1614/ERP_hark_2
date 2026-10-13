//! **7C** — Payslips from finalised payroll results (issue, reissue, withdraw) and
//! `/me/payslips`.
//!
//! Tag `people-payroll`; operation id prefix `people_payslip_` (incl. `people_payslip_self_`,
//! contract §5.2). Self-service resolves the staff record from the session subject
//! (`shared::me::my_staff`), never from a client-supplied id.

pub mod http;
pub mod repo;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    http::router(state)
}
