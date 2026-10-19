//! Student timetable (`/me/exam-timetable`): the official sittings of the papers on the
//! caller's own effective registrations, with their seat when a seating plan is published.

use super::schedule::{official_schedule, sittings_of};
use crate::shared::me::my_registrations;
use crate::shared::ExamsState;
use axum::extract::State;
use axum::Json;
use chrono::NaiveDate;
use serde::Serialize;
use sutra_contracts::exams::status::RegistrationPaperStatus;
use sutra_kernel::auth::Permission;
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// One entry of the caller's examination timetable.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamTimetableEntry {
    pub session_id: Uuid,
    pub registration_id: Uuid,
    pub sitting_id: Uuid,
    pub paper_id: Uuid,
    pub paper_code: String,
    pub title: String,
    pub sits_on: NaiveDate,
    #[schema(example = "09:30")]
    pub starts_at: String,
    pub duration_minutes: i32,
    /// `planned | postponed | completed`.
    pub status: String,
    pub postponed_from: Option<NaiveDate>,
    pub venue_code: Option<String>,
    pub seat_no: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamTimetable {
    pub items: Vec<ExamTimetableEntry>,
}

/// The caller's examination timetable (`exam.self`): sittings of the published timetable for
/// the papers on their effective registrations.
#[utoipa::path(get, path = "/api/v1/exams/me/exam-timetable", operation_id = "exam_schedule_self_timetable", tag = "exam-scheduling",
    security(("session_cookie" = [])),
    responses((status = 200, body = ExamTimetable), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_timetable(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<ExamTimetable>> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    let regs = my_registrations(&st, ctx).await?;
    let tenant = ctx.scope.tenant_id;
    let mut items = Vec::new();
    for r in regs.iter().filter(|r| r.status.is_effective()) {
        let Some(schedule) = official_schedule(&st, tenant, r.session_id.as_uuid()).await? else {
            continue;
        };
        if schedule.status == "revised" {
            continue;
        }
        let sittings = sittings_of(&st.pool, tenant, schedule.id).await?;
        for p in r.papers.iter().filter(|p| {
            !matches!(
                p.status,
                RegistrationPaperStatus::Withdrawn | RegistrationPaperStatus::Debarred
            )
        }) {
            let Some(x) = sittings
                .iter()
                .find(|x| x.paper_id == p.paper_id.as_uuid() && x.status != "cancelled")
            else {
                continue;
            };
            let seat: Option<(String, String)> = sqlx::query_as(
                "SELECT v.code, a.seat_no FROM exams.seat_assignment a \
                 JOIN exams.seating_plan sp ON sp.id = a.plan_id \
                 JOIN exams.exam_venue v ON v.id = a.venue_id \
                 WHERE a.tenant_id = $1 AND sp.sitting_id = $2 AND sp.status = 'published' \
                 AND a.registration_id = $3",
            )
            .bind(tenant.as_uuid())
            .bind(x.id)
            .bind(r.registration_id.as_uuid())
            .fetch_optional(&st.pool)
            .await
            .map_err(map_db_error)?;
            items.push(ExamTimetableEntry {
                session_id: r.session_id.as_uuid(),
                registration_id: r.registration_id.as_uuid(),
                sitting_id: x.id,
                paper_id: x.paper_id,
                paper_code: x.paper_code.clone(),
                title: x.paper_title.clone(),
                sits_on: x.sits_on,
                starts_at: x.starts_at.format("%H:%M").to_string(),
                duration_minutes: x.duration_minutes,
                status: x.status.clone(),
                postponed_from: x.postponed_from,
                venue_code: seat.as_ref().map(|s| s.0.clone()),
                seat_no: seat.map(|s| s.1),
            });
        }
    }
    items.sort_by(|a, b| (a.sits_on, &a.starts_at).cmp(&(b.sits_on, &b.starts_at)));
    Ok(Json(ExamTimetable { items }))
}
