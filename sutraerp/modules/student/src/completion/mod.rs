//! **3C** — qualification completion (eligibility → audit → clearance → approval → award/conferment → record finalisation → exit), NEP MEME exit, withdrawal/transfer/discontinuation/termination exits, exit clearance, administrative closure, deceased record closure, completion deferral, conferred qualification correction/revocation (L3-C-039…050).
//!
//! Paths: `/api/v1/student/students/{id}/qualification-completions`, `/api/v1/student/qualification-completions/{id}[/…]`, `/api/v1/student/conferred-qualifications/{id}[/…]`, `/api/v1/student/students/{id}/exits`, `/api/v1/student/exits/{id}[/…]` (clearance items nested `…/exits/{id}/clearances/{clearanceId}`)`.
//! operation_id prefixes: `qualification_completion_`, `conferred_qualification_`, `exit_`, `exit_clearance_`. Tag: `completion`.
//!
//! Owned by slice 3C (see `docs/batch3-contracts.md`). Add `domain.rs`, `repo.rs`, `http.rs`
//! (or finer submodules) inside this directory only. Handlers take
//! [`StudentState`](crate::shared::StudentState); every `#[utoipa::path]` sets an explicit
//! `operation_id` with this slice's prefix and `tag = "completion"`. The first path parameter after a
//! collection segment is always named `{id}` (contract doc §5.2).

pub mod domain;
pub mod eligibility;
pub mod exit;
pub mod http;
pub mod kit;
pub mod qualification;

use crate::shared::StudentState;
use utoipa_axum::router::OpenApiRouter;

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: StudentState) -> OpenApiRouter {
    http::router(state)
}
