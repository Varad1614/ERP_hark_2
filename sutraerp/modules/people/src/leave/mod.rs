//! **7B** — Leave policy versions and leave types (L3-E-044), requests → final decision (`LEAVE_APPROVAL`, the reference approval of Batch 7, L3-E-045), accrual (046), reservation / consumption (047), cancellation (048), adjustment (049), carry forward (050), encashment (051), exceptions (052), leave cases (034), long leave (006); `/me/leave-*` and manager approvals.
//!
//! Tag `people-leave`; operation id prefixes `people_leave_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 7B fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.
//!
//! | File | Content |
//! |------|---------|
//! | [`domain`] | pure rules: transitions, working-day calendar, month segments, accrual / carry-forward / encashment / over-allocation |
//! | [`common`] | bodies, `If-Match`, history + `StatusChangedV1` events, audit (shared with `attendance` / `workload`) |
//! | [`policy`] | leave policy versions + leave types |
//! | [`request`] | leave requests, decisions (HR + reporting manager), consumption, cancellation, `/me` |
//! | [`balance`] | balances, ledger, accrual + year-end runs, adjustments, encashment |
//! | [`cases`] | leave exceptions, leave cases, long leave |

pub mod balance;
pub mod cases;
pub mod common;
pub mod domain;
pub mod policy;
pub mod request;

use crate::shared::PeopleState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .merge(policy::router(state.clone()))
        .merge(request::router(state.clone()))
        .merge(balance::router(state.clone()))
        .merge(cases::router(state))
}
