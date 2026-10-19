//! **6A** — Schedules / timetables (plan → approval → publication → revision → withdrawal, conflict detection — L3-I-007/008/013/014/016), venues and allocation (L3-I-009), seating plans (L3-I-015/059), invigilation and examiner duties incl. shortage and replacement (L3-I-010/011/012/060), conduct records (attendance, completion, failed entry → non-participation — L3-I-043/055/057), exceptions and contingencies (L3-I-172/174/175/185).
//!
//! Tag `exam-scheduling`; operation id prefixes `exam_schedule_`, `exam_sitting_`, `exam_venue_`, `exam_seating_`, `exam_invigilation_`, `exam_conduct_`, `exam_exception_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 6A fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.

pub mod conduct;
pub mod exception;
pub mod invigilation;
pub mod schedule;
pub mod seating;
pub mod timetable;
pub mod venue;

use crate::shared::ExamsState;
use conduct::*;
use exception::*;
use invigilation::*;
use schedule::*;
use seating::*;
use timetable::*;
use utoipa_axum::{router::OpenApiRouter, routes};
use venue::*;

/// Routes of this area.
pub fn router(state: ExamsState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(schedule_list, schedule_create))
        .routes(routes!(schedule_get))
        .routes(routes!(schedule_conflicts))
        .routes(routes!(schedule_submit))
        .routes(routes!(schedule_decision))
        .routes(routes!(schedule_publish))
        .routes(routes!(schedule_revise))
        .routes(routes!(schedule_withdraw))
        .routes(routes!(sitting_list, sitting_create))
        .routes(routes!(sitting_get, sitting_update))
        .routes(routes!(sitting_cancel))
        .routes(routes!(venue_list, venue_create))
        .routes(routes!(venue_get, venue_update))
        .routes(routes!(allocation_list, allocation_create))
        .routes(routes!(allocation_release))
        .routes(routes!(seating_for_sitting, seating_generate))
        .routes(routes!(seating_get))
        .routes(routes!(seating_publish))
        .routes(routes!(seating_revise))
        .routes(routes!(invigilation_list, invigilation_assign))
        .routes(routes!(invigilation_confirm))
        .routes(routes!(invigilation_release))
        .routes(routes!(invigilation_replace))
        .routes(routes!(my_invigilation))
        .routes(routes!(conduct_for_sitting, conduct_open))
        .routes(routes!(conduct_get))
        .routes(routes!(conduct_start))
        .routes(routes!(conduct_suspend))
        .routes(routes!(conduct_resume))
        .routes(routes!(conduct_complete))
        .routes(routes!(conduct_postpone))
        .routes(routes!(conduct_decision))
        .routes(routes!(exception_list, exception_create))
        .routes(routes!(exception_get))
        .routes(routes!(exception_classify))
        .routes(routes!(exception_review))
        .routes(routes!(exception_resolve))
        .routes(routes!(exception_close))
        .routes(routes!(my_timetable))
        .with_state(state)
}
