//! HTTP API of the timetable (tag `timetable`; contract §5.2).

use super::kit::{
    self, created, expect_version, ok, DeliveryDecision, DeliveryDecisionValue, DeliveryReason,
};
use super::repo::{self, *};
use crate::shared::http::actor;
use crate::shared::AcademicState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_contracts::academic::status::TimetableStatus;
use sutra_kernel::auth::Permission;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TimetableFilter {
    pub academic_term_id: Option<Uuid>,
    pub status: Option<TimetableStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RequirementFilter {
    pub academic_term_id: Option<Uuid>,
    /// `active` | `withdrawn`.
    pub status: Option<String>,
}

/// Scheduling requirements of an institution.
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/scheduling-requirements", operation_id = "scheduling_requirement_list", tag = "timetable",
    params(("id" = String, Path, format = Uuid), RequirementFilter), security(("session_cookie" = [])),
    responses((status = 200, body = SchedulingRequirementList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requirement_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<RequirementFilter>,
) -> ApiResult<Json<SchedulingRequirementList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(SchedulingRequirementList {
        items: repo::list_requirements(
            &st,
            ctx.scope.tenant_id,
            id,
            f.academic_term_id,
            f.status.as_deref(),
        )
        .await?,
    }))
}

/// Record a section's scheduling requirement (sessions a week, length, preferred days, room).
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/scheduling-requirements", operation_id = "scheduling_requirement_create", tag = "timetable",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewSchedulingRequirement, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SchedulingRequirement, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requirement_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewSchedulingRequirement>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimetableManage)?;
    require_idempotency_key(&headers)?;
    let r = repo::create_requirement(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&r, r.version))
}

/// Withdraw a scheduling requirement.
#[utoipa::path(post, path = "/api/v1/academic/scheduling-requirements/{id}/withdraw", operation_id = "scheduling_requirement_withdraw", tag = "timetable",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requirement")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SchedulingRequirement, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requirement_withdraw(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimetableManage)?;
    let v = expect_version!(headers);
    let r = repo::withdraw_requirement(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Timetables of an institution (newest first).
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/timetables", operation_id = "timetable_list", tag = "timetable",
    params(("id" = String, Path, format = Uuid), TimetableFilter), security(("session_cookie" = [])),
    responses((status = 200, body = TimetableList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timetable_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(f): Query<TimetableFilter>,
) -> ApiResult<Json<TimetableList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(TimetableList {
        items: repo::list(
            &st,
            ctx.scope.tenant_id,
            id,
            f.academic_term_id,
            f.status.map(|s| s.as_str()),
        )
        .await?,
    }))
}

/// Start planning a timetable for an academic term.
#[utoipa::path(post, path = "/api/v1/academic/institutions/{id}/timetables", operation_id = "timetable_create", tag = "timetable",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewTimetable, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = Timetable, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timetable_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewTimetable>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimetableManage)?;
    require_idempotency_key(&headers)?;
    let t = repo::create(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&t, t.version))
}

/// A timetable version with its slots.
#[utoipa::path(get, path = "/api/v1/academic/timetables/{id}", operation_id = "timetable_get", tag = "timetable",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = Timetable, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timetable_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let t = repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&t, t.version))
}

macro_rules! tt_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $perm:expr, |$st:ident, $ctx:ident, $id:ident, $v:ident, $reason:ident| $body:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "timetable",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the timetable")),
            request_body(content = Option<DeliveryReason>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = Timetable, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State($st): State<AcademicState>, auth: Authenticated, Path($id): Path<Uuid>, headers: HeaderMap, body: Option<Json<DeliveryReason>>) -> ApiResult<Response> {
            let $ctx = auth.require($perm)?;
            let $v = expect_version!(headers);
            let $reason = kit::reason(body);
            let t: Timetable = $body.await?;
            Ok(ok(&t, t.version))
        }
    };
}

tt_cmd!(
    /// Generate a draft from the term's scheduling requirements (replaces the current slots).
    timetable_generate, "/api/v1/academic/timetables/{id}/generate", "timetable_generate", Permission::TimetableManage,
    |st, ctx, id, v, _r| repo::generate(&st, ctx, id, v, &actor(ctx)));
tt_cmd!(
    /// Validate faculty, room, section and requirement constraints; a valid draft becomes `validated`.
    timetable_validate, "/api/v1/academic/timetables/{id}/validate", "timetable_validate", Permission::TimetableManage,
    |st, ctx, id, v, _r| repo::validate(&st, ctx, id, v, &actor(ctx)));
tt_cmd!(
    /// Submit a validated timetable for approval (`TIMETABLE_APPROVAL` / `TIMETABLE_REVISION`).
    timetable_submit, "/api/v1/academic/timetables/{id}/submit", "timetable_submit", Permission::TimetableManage,
    |st, ctx, id, v, _r| repo::submit(&st, ctx, id, v, &actor(ctx)));
tt_cmd!(
    /// Publish an approved timetable version (immutable from here).
    timetable_publish, "/api/v1/academic/timetables/{id}/publish", "timetable_publish", Permission::TimetablePublish,
    |st, ctx, id, v, _r| repo::publish(&st, ctx, id, v, &actor(ctx)));
tt_cmd!(
    /// Make a published (or frozen) timetable operational; a revised version replaces its base.
    timetable_activate, "/api/v1/academic/timetables/{id}/activate", "timetable_activate", Permission::TimetablePublish,
    |st, ctx, id, v, _r| repo::activate(&st, ctx, id, v, &actor(ctx)));
tt_cmd!(
    /// Freeze the operational timetable.
    timetable_freeze, "/api/v1/academic/timetables/{id}/freeze", "timetable_freeze", Permission::TimetablePublish,
    |st, ctx, id, v, r| repo::freeze(&st, ctx, id, v, r, &actor(ctx)));
tt_cmd!(
    /// Withdraw an unpublished revision draft.
    timetable_withdraw, "/api/v1/academic/timetables/{id}/withdraw", "timetable_withdraw", Permission::TimetableManage,
    |st, ctx, id, v, r| repo::withdraw(&st, ctx, id, v, r, &actor(ctx)));
tt_cmd!(
    /// Retire the operational timetable (reason required).
    timetable_retire, "/api/v1/academic/timetables/{id}/retire", "timetable_retire", Permission::TimetablePublish,
    |st, ctx, id, v, r| repo::retire(&st, ctx, id, v, r, &actor(ctx)));
tt_cmd!(
    /// Archive a retired or revised timetable (historical, never operational again).
    timetable_archive, "/api/v1/academic/timetables/{id}/archive", "timetable_archive", Permission::TimetablePublish,
    |st, ctx, id, v, _r| repo::archive(&st, ctx, id, v, &actor(ctx)));

/// Approval decision on a submitted timetable (`AwaitDecision`; reject → draft).
#[utoipa::path(post, path = "/api/v1/academic/timetables/{id}/decision", operation_id = "timetable_decision", tag = "timetable",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the timetable")),
    request_body = DeliveryDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Timetable, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timetable_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<DeliveryDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimetablePublish)?;
    let v = expect_version!(headers);
    let reason = kit::opt_text(cmd.reason.as_deref());
    let t = repo::decide(
        &st,
        ctx,
        id,
        v,
        cmd.decision == DeliveryDecisionValue::Approve,
        reason,
        &actor(ctx),
    )
    .await?;
    Ok(ok(&t, t.version))
}

/// Add a weekly slot to a planning/draft timetable.
#[utoipa::path(post, path = "/api/v1/academic/timetables/{id}/slots", operation_id = "timetable_slot_add", tag = "timetable",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the timetable")),
    request_body = NewSlot, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Timetable, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timetable_slot_add(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewSlot>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimetableManage)?;
    let v = expect_version!(headers);
    let t = repo::add_slot(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&t, t.version))
}

/// Remove a slot from a planning/draft timetable.
#[utoipa::path(delete, path = "/api/v1/academic/timetables/{id}/slots/{slotId}", operation_id = "timetable_slot_remove", tag = "timetable",
    params(("id" = String, Path, format = Uuid), ("slotId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the timetable")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Timetable, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timetable_slot_remove(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, slot_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimetableManage)?;
    let v = expect_version!(headers);
    let t = repo::remove_slot(&st, ctx, id, slot_id, v, &actor(ctx)).await?;
    Ok(ok(&t, t.version))
}

/// Request a governed revision of the operational timetable: creates the revised draft version.
#[utoipa::path(post, path = "/api/v1/academic/timetables/{id}/revisions", operation_id = "timetable_revision_request", tag = "timetable",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewRevision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = Timetable, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn timetable_revision_request(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewRevision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::TimetableManage)?;
    require_idempotency_key(&headers)?;
    let t = repo::request_revision(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&t, t.version))
}

/// The signed-in student's weekly timetable (operational timetables of their seated sections).
#[utoipa::path(get, path = "/api/v1/academic/me/timetable", operation_id = "timetable_self_student", tag = "timetable",
    security(("session_cookie" = [])),
    responses((status = 200, body = WeeklyTimetable), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_timetable(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<WeeklyTimetable>> {
    let (_me, sections) = crate::shared::me::my_sections(&st, &auth.0).await?;
    weekly(&st, &auth.0, sections).await
}

/// The signed-in faculty member's teaching timetable (sections with an active assignment).
#[utoipa::path(get, path = "/api/v1/academic/my/timetable", operation_id = "timetable_self_faculty", tag = "timetable",
    security(("session_cookie" = [])),
    responses((status = 200, body = WeeklyTimetable), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_timetable(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<WeeklyTimetable>> {
    let sections = crate::shared::faculty::my_sections(&st, &auth.0).await?;
    weekly(&st, &auth.0, sections).await
}

async fn weekly(
    st: &AcademicState,
    ctx: &sutra_kernel::auth::AuthContext,
    sections: Vec<sutra_contracts::academic::ports::SectionSummary>,
) -> ApiResult<Json<WeeklyTimetable>> {
    let ids: Vec<Uuid> = sections.iter().map(|s| s.id.as_uuid()).collect();
    let items = repo::operational_slots(st, ctx.scope.tenant_id, &ids).await?;
    let today = match sections.first() {
        Some(s) => {
            let scope = crate::shared::scope::institution_scope(ctx, s.institution_id);
            crate::shared::time::business_today(st, &scope, s.institution_id).await?
        }
        None => chrono::Utc::now().date_naive(),
    };
    Ok(Json(WeeklyTimetable {
        items,
        week_of: repo::monday_of(today),
    }))
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TermQuery {
    pub academic_term_id: Uuid,
}

/// A teachable section of a term (offering read port), for schedulers and delivery screens.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeachingSection {
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub academic_term_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub section_code: String,
    pub status: sutra_contracts::academic::status::SectionStatus,
    pub offering_status: sutra_contracts::academic::status::CourseOfferingStatus,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct TeachingSectionList {
    pub items: Vec<TeachingSection>,
}

/// Sections of the offerings of a term (read through the offering read port).
#[utoipa::path(get, path = "/api/v1/academic/institutions/{id}/teaching-sections", operation_id = "timetable_section_list", tag = "timetable",
    params(("id" = String, Path, format = Uuid), TermQuery), security(("session_cookie" = [])),
    responses((status = 200, body = TeachingSectionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn section_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<TermQuery>,
) -> ApiResult<Json<TeachingSectionList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let scope = crate::shared::scope::tenant_scope(ctx);
    let mut items = Vec::new();
    for o in st
        .offerings
        .offerings_in_term(
            &scope,
            sutra_kernel::scope::InstitutionId(id),
            q.academic_term_id,
        )
        .await?
    {
        for s in st.offerings.sections_of_offering(&scope, o.id).await? {
            items.push(TeachingSection {
                section_id: s.id.as_uuid(),
                offering_id: o.id.as_uuid(),
                academic_term_id: o.academic_term_id,
                offering_code: o.offering_code.clone(),
                title: o.title.clone(),
                section_code: s.section_code,
                status: s.status,
                offering_status: o.status,
            });
        }
    }
    Ok(Json(TeachingSectionList { items }))
}

/// All timetable routes.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(requirement_list, requirement_create))
        .routes(routes!(requirement_withdraw))
        .routes(routes!(timetable_list, timetable_create))
        .routes(routes!(timetable_get))
        .routes(routes!(timetable_generate))
        .routes(routes!(timetable_validate))
        .routes(routes!(timetable_submit))
        .routes(routes!(timetable_decision))
        .routes(routes!(timetable_publish))
        .routes(routes!(timetable_activate))
        .routes(routes!(timetable_freeze))
        .routes(routes!(timetable_withdraw))
        .routes(routes!(timetable_retire))
        .routes(routes!(timetable_archive))
        .routes(routes!(timetable_slot_add))
        .routes(routes!(timetable_slot_remove))
        .routes(routes!(timetable_revision_request))
        .routes(routes!(section_list))
        .routes(routes!(me_timetable))
        .routes(routes!(my_timetable))
        .with_state(state)
}
