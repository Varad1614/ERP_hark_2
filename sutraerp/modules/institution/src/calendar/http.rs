//! HTTP API of the academic calendar (contract doc §5; tag `calendar`).
//!
//! Every handler: `auth.require(..)` first, tenant from the session, institution scope from the
//! path / the aggregate row. Creates need `Idempotency-Key` (→ 201 + `ETag`); updates and state
//! commands need `If-Match` (428 when missing, 412 when stale). Illegal transitions → 409.

use super::domain::*;
use super::repo;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::institution_scope;
use crate::shared::InstitutionState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use sutra_contracts::institution::ids::*;
use sutra_contracts::institution::status::{
    AcademicYearStatus, CalendarAssignmentStatus, CalendarExceptionStatus, PublicationStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Returns the 428 response from `if_match` early.
macro_rules! expect_version {
    ($headers:expr) => {
        match if_match(&$headers)? {
            Ok(v) => v,
            Err(res) => return Ok(res),
        }
    };
}

fn ok<T: serde::Serialize>(body: &T, version: sutra_kernel::concurrency::Version) -> Response {
    with_etag(StatusCode::OK, body, version)
}

fn created<T: serde::Serialize>(body: &T, version: sutra_kernel::concurrency::Version) -> Response {
    with_etag(StatusCode::CREATED, body, version)
}

fn reason(body: Option<Json<StateCommand>>) -> Option<String> {
    body.and_then(|Json(b)| b.reason)
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct PublicationFilter {
    pub status: Option<PublicationStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct YearFilter {
    pub status: Option<AcademicYearStatus>,
    pub calendar_id: Option<AcademicCalendarId>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AssignmentFilter {
    pub status: Option<CalendarAssignmentStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ExceptionFilter {
    pub status: Option<CalendarExceptionStatus>,
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct ResolutionQuery {
    pub scope_kind: Option<ScopeKind>,
    pub scope_id: Option<Uuid>,
    /// Business date `YYYY-MM-DD` (default: today in the institution timezone).
    pub on: Option<NaiveDate>,
}

/// Result of a closure request: the year (closed, or `closing` while awaiting a decision) and
/// the closure evidence evaluated.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcademicYearClosure {
    pub year: AcademicYear,
    pub evidence: Vec<ClosureEvidence>,
}

// ============================================================================================
// Calendars
// ============================================================================================

/// List the academic calendars of an institution.
#[utoipa::path(get, path = "/api/v1/institution/institutions/{id}/calendars", operation_id = "calendar_list", tag = "calendar",
    params(("id" = String, Path, format = Uuid), PublicationFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn calendar_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<PublicationFilter>,
) -> ApiResult<Json<CalendarList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(CalendarList {
        items: repo::list_calendars(&st.pool, &scope, id, q.status).await?,
    }))
}

/// Establish an academic calendar (the stable identity its versions live under).
#[utoipa::path(post, path = "/api/v1/institution/institutions/{id}/calendars", operation_id = "calendar_create", tag = "calendar",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewCalendar, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = Calendar, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn calendar_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewCalendar>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let c = repo::create_calendar(&st.pool, ctx.scope.tenant_id, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// Get an academic calendar.
#[utoipa::path(get, path = "/api/v1/institution/calendars/{calendarId}", operation_id = "calendar_get", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = Calendar, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn calendar_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let c = repo::get_calendar(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(ok(&c, c.version))
}

/// Retire a calendar with historical preservation (L3-B-033). Refused while an assignment is
/// effective or an academic year is open.
#[utoipa::path(post, path = "/api/v1/institution/calendars/{calendarId}/retire", operation_id = "calendar_retire", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Calendar, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn calendar_retire(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let v = expect_version!(headers);
    let c = repo::retire_calendar(
        &st.pool,
        ctx.scope.tenant_id,
        id,
        v,
        reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&c, c.version))
}

// ============================================================================================
// Calendar versions, terms, key dates
// ============================================================================================

/// List the versions of a calendar (with terms and key dates).
#[utoipa::path(get, path = "/api/v1/institution/calendars/{calendarId}/versions", operation_id = "calendar_version_list", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), PublicationFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarVersionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    Query(q): Query<PublicationFilter>,
) -> ApiResult<Json<CalendarVersionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(Json(CalendarVersionList {
        items: repo::list_versions(&st.pool, ctx.scope.tenant_id, id, q.status).await?,
    }))
}

/// Create the first draft version of a calendar for an academic year.
#[utoipa::path(post, path = "/api/v1/institution/calendars/{calendarId}/versions", operation_id = "calendar_version_create", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewCalendarVersion, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    headers: HeaderMap,
    Json(cmd): Json<NewCalendarVersion>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let v = repo::create_version(&st.pool, ctx.scope.tenant_id, id, cmd, &actor(ctx)).await?;
    Ok(created(&v, v.version))
}

/// Get a calendar version.
#[utoipa::path(get, path = "/api/v1/institution/calendar-versions/{versionId}", operation_id = "calendar_version_get", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let v = repo::get_version(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(ok(&v, v.version))
}

/// Change a draft version's period or notes (published versions are immutable).
#[utoipa::path(patch, path = "/api/v1/institution/calendar-versions/{versionId}", operation_id = "calendar_version_update", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    request_body = UpdateCalendarVersion, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_update(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateCalendarVersion>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    let e = expect_version!(headers);
    let v = repo::update_version(&st.pool, ctx.scope.tenant_id, id, e, cmd, &actor(ctx)).await?;
    Ok(ok(&v, v.version))
}

/// Add a term to a draft version (`If-Match` of the version + `Idempotency-Key`).
#[utoipa::path(post, path = "/api/v1/institution/calendar-versions/{versionId}/terms", operation_id = "calendar_version_add_term", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the calendar version"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewTerm, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn term_add(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewTerm>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let e = expect_version!(headers);
    let v = repo::add_term(&st.pool, ctx.scope.tenant_id, id, e, cmd, &actor(ctx)).await?;
    Ok(created(&v, v.version))
}

/// Remove a term from a draft version.
#[utoipa::path(delete, path = "/api/v1/institution/calendar-versions/{versionId}/terms/{termId}", operation_id = "calendar_version_remove_term", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("termId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the calendar version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn term_remove(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path((id, term)): Path<(CalendarVersionId, AcademicTermId)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    let e = expect_version!(headers);
    let v = repo::remove_term(&st.pool, ctx.scope.tenant_id, id, term, e, &actor(ctx)).await?;
    Ok(ok(&v, v.version))
}

/// Add a key date (holiday, exam window, …) to a draft version.
#[utoipa::path(post, path = "/api/v1/institution/calendar-versions/{versionId}/key-dates", operation_id = "calendar_version_add_key_date", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the calendar version"), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewKeyDate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn key_date_add(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewKeyDate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let e = expect_version!(headers);
    let v = repo::add_key_date(&st.pool, ctx.scope.tenant_id, id, e, cmd, &actor(ctx)).await?;
    Ok(created(&v, v.version))
}

/// Remove a key date from a draft version.
#[utoipa::path(delete, path = "/api/v1/institution/calendar-versions/{versionId}/key-dates/{keyDateId}", operation_id = "calendar_version_remove_key_date", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("keyDateId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the calendar version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn key_date_remove(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path((id, key)): Path<(CalendarVersionId, KeyDateId)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    let e = expect_version!(headers);
    let v = repo::remove_key_date(&st.pool, ctx.scope.tenant_id, id, key, e, &actor(ctx)).await?;
    Ok(ok(&v, v.version))
}

/// Publish a draft version: validation → approval step (auto-approved in Batch 2) → published,
/// timezone snapshotted, predecessor superseded. Assignments are not migrated.
#[utoipa::path(post, path = "/api/v1/institution/calendar-versions/{versionId}/publish", operation_id = "calendar_version_publish", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_publish(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let v =
        repo::publish_version(&st, ctx.scope.tenant_id, id, e, reason(body), &actor(ctx)).await?;
    Ok(ok(&v, v.version))
}

/// Decide a version awaiting publication approval (approve → published, reject → draft).
#[utoipa::path(post, path = "/api/v1/institution/calendar-versions/{versionId}/decision", operation_id = "calendar_version_decision", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    request_body = Decision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_decision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
    headers: HeaderMap,
    Json(d): Json<Decision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let v = repo::decide_version(&st.pool, ctx.scope.tenant_id, id, e, d, &actor(ctx)).await?;
    Ok(ok(&v, v.version))
}

/// Revise a published version into a successor draft (same calendar identity, L3-B-029).
#[utoipa::path(post, path = "/api/v1/institution/calendar-versions/{versionId}/revise", operation_id = "calendar_version_revise", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the published version")),
    request_body(content = Option<StateCommand>, description = "`reason` becomes the successor's notes"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_revise(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    let e = expect_version!(headers);
    let v = repo::revise_version(
        &st.pool,
        ctx.scope.tenant_id,
        id,
        e,
        reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(created(&v, v.version))
}

/// Retire a published version (or discard a draft); history is preserved.
#[utoipa::path(post, path = "/api/v1/institution/calendar-versions/{versionId}/retire", operation_id = "calendar_version_retire", tag = "calendar",
    params(("versionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarVersion, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn version_retire(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarVersionId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let v = repo::retire_version(
        &st.pool,
        ctx.scope.tenant_id,
        id,
        e,
        reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&v, v.version))
}

// ============================================================================================
// Academic years
// ============================================================================================

/// List the academic years of an institution.
#[utoipa::path(get, path = "/api/v1/institution/institutions/{id}/academic-years", operation_id = "academic_year_list", tag = "calendar",
    params(("id" = String, Path, format = Uuid), YearFilter), security(("session_cookie" = [])),
    responses((status = 200, body = AcademicYearList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<YearFilter>,
) -> ApiResult<Json<AcademicYearList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(AcademicYearList {
        items: repo::list_years(&st.pool, &scope, id, q.status, q.calendar_id).await?,
    }))
}

/// Plan an academic year in a calendar (no overlap with the calendar's other years).
#[utoipa::path(post, path = "/api/v1/institution/institutions/{id}/academic-years", operation_id = "academic_year_create", tag = "calendar",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewAcademicYear, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AcademicYear, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewAcademicYear>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let y = repo::create_year(&st.pool, ctx.scope.tenant_id, id, cmd, &actor(ctx)).await?;
    Ok(created(&y, y.version))
}

/// Get an academic year.
#[utoipa::path(get, path = "/api/v1/institution/academic-years/{yearId}", operation_id = "academic_year_get", tag = "calendar",
    params(("yearId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AcademicYear, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicYearId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let y = repo::get_year(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(ok(&y, y.version))
}

/// Open the academic year (the open academic context). Needs a published version.
#[utoipa::path(post, path = "/api/v1/institution/academic-years/{yearId}/open", operation_id = "academic_year_open", tag = "calendar",
    params(("yearId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the year")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AcademicYear, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_open(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicYearId>,
    headers: HeaderMap,
    _body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let y = repo::open_year(&st.pool, ctx.scope.tenant_id, id, e, &actor(ctx)).await?;
    Ok(ok(&y, y.version))
}

/// Close the academic year into the next-cycle foundation (L3-B-031): closure evidence →
/// approval step → closed + planned successor year with a draft version (never opened).
#[utoipa::path(post, path = "/api/v1/institution/academic-years/{yearId}/close", operation_id = "academic_year_close", tag = "calendar",
    params(("yearId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the year")),
    request_body = CloseAcademicYear, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AcademicYearClosure, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_close(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicYearId>,
    headers: HeaderMap,
    body: Option<Json<CloseAcademicYear>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let cmd = body.map(|Json(b)| b).unwrap_or(CloseAcademicYear {
        prepare_successor: true,
        ..Default::default()
    });
    let r = repo::close_year(&st, ctx.scope.tenant_id, id, e, cmd, &actor(ctx)).await?;
    let v = r.year.version;
    Ok(ok(
        &AcademicYearClosure {
            year: r.year,
            evidence: r.evidence,
        },
        v,
    ))
}

/// Decide a closure awaiting approval (approve → closed, reject → back to open).
#[utoipa::path(post, path = "/api/v1/institution/academic-years/{yearId}/decision", operation_id = "academic_year_decision", tag = "calendar",
    params(("yearId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the year")),
    request_body = Decision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AcademicYear, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn year_decision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicYearId>,
    headers: HeaderMap,
    Json(d): Json<Decision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let y = repo::decide_closure(&st.pool, ctx.scope.tenant_id, id, e, d, &actor(ctx)).await?;
    Ok(ok(&y, y.version))
}

// ============================================================================================
// Assignments + resolution
// ============================================================================================

/// List the assignments of a calendar.
#[utoipa::path(get, path = "/api/v1/institution/calendars/{calendarId}/assignments", operation_id = "calendar_assignment_list", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), AssignmentFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarAssignmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    Query(q): Query<AssignmentFilter>,
) -> ApiResult<Json<CalendarAssignmentList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    repo::get_calendar(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(CalendarAssignmentList {
        items: repo::list_assignments(&st.pool, ctx.scope.tenant_id, Some(id), None, q.status)
            .await?,
    }))
}

/// List every calendar assignment of an institution.
#[utoipa::path(get, path = "/api/v1/institution/institutions/{id}/calendar-assignments", operation_id = "calendar_assignment_list_for_institution", tag = "calendar",
    params(("id" = String, Path, format = Uuid), AssignmentFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarAssignmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_list_institution(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<AssignmentFilter>,
) -> ApiResult<Json<CalendarAssignmentList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    crate::shared::guard::institution_status(&st.pool, &institution_scope(ctx, id), id).await?;
    Ok(Json(CalendarAssignmentList {
        items: repo::list_assignments(&st.pool, ctx.scope.tenant_id, None, Some(id), q.status)
            .await?,
    }))
}

/// Create a draft assignment of a calendar version to a governing scope.
#[utoipa::path(post, path = "/api/v1/institution/calendars/{calendarId}/assignments", operation_id = "calendar_assignment_create", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewAssignment, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CalendarAssignment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    headers: HeaderMap,
    Json(cmd): Json<NewAssignment>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let a = repo::create_assignment(&st.pool, ctx.scope.tenant_id, id, cmd, &actor(ctx)).await?;
    Ok(created(&a, a.version))
}

/// Get a calendar assignment.
#[utoipa::path(get, path = "/api/v1/institution/calendar-assignments/{assignmentId}", operation_id = "calendar_assignment_get", tag = "calendar",
    params(("assignmentId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarAssignment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarAssignmentId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let a = repo::get_assignment(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(ok(&a, a.version))
}

/// Make an assignment effective (published version, existing scope, no overlapping authority).
#[utoipa::path(post, path = "/api/v1/institution/calendar-assignments/{assignmentId}/activate", operation_id = "calendar_assignment_activate", tag = "calendar",
    params(("assignmentId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarAssignment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_activate(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarAssignmentId>,
    headers: HeaderMap,
    _body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let a = repo::activate_assignment(&st.pool, ctx.scope.tenant_id, id, e, &actor(ctx)).await?;
    Ok(ok(&a, a.version))
}

/// Retire an assignment (kept for historical resolution).
#[utoipa::path(post, path = "/api/v1/institution/calendar-assignments/{assignmentId}/retire", operation_id = "calendar_assignment_retire", tag = "calendar",
    params(("assignmentId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarAssignment, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_retire(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarAssignmentId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let e = expect_version!(headers);
    let a = repo::retire_assignment(
        &st.pool,
        ctx.scope.tenant_id,
        id,
        e,
        reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&a, a.version))
}

/// Which calendar version governs a scope on a business date (institution fallback).
#[utoipa::path(get, path = "/api/v1/institution/institutions/{id}/calendar-resolution", operation_id = "calendar_resolve", tag = "calendar",
    params(("id" = String, Path, format = Uuid), ResolutionQuery), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarResolution), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn resolve(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<ResolutionQuery>,
) -> ApiResult<Json<CalendarResolution>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(
        repo::resolve(
            &st.pool,
            &scope,
            id,
            q.scope_kind.unwrap_or(ScopeKind::Institution),
            q.scope_id,
            q.on,
        )
        .await?,
    ))
}

// ============================================================================================
// Exceptions
// ============================================================================================

/// List the exceptions (temporary deviations) of a calendar.
#[utoipa::path(get, path = "/api/v1/institution/calendars/{calendarId}/exceptions", operation_id = "calendar_exception_list", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), ExceptionFilter), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarExceptionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    Query(q): Query<ExceptionFilter>,
) -> ApiResult<Json<CalendarExceptionList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(Json(CalendarExceptionList {
        items: repo::list_exceptions(&st.pool, ctx.scope.tenant_id, id, q.status).await?,
    }))
}

/// Request a temporary deviation; the approval step runs at once (auto-approved in Batch 2).
#[utoipa::path(post, path = "/api/v1/institution/calendars/{calendarId}/exceptions", operation_id = "calendar_exception_create", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewException, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CalendarException, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    headers: HeaderMap,
    Json(cmd): Json<NewException>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let e = repo::create_exception(&st, ctx.scope.tenant_id, id, cmd, &actor(ctx)).await?;
    Ok(created(&e, e.version))
}

/// Get a calendar exception.
#[utoipa::path(get, path = "/api/v1/institution/calendar-exceptions/{exceptionId}", operation_id = "calendar_exception_get", tag = "calendar",
    params(("exceptionId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarException, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarExceptionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let e = repo::get_exception(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(ok(&e, e.version))
}

/// Decide a requested exception (approve / reject).
#[utoipa::path(post, path = "/api/v1/institution/calendar-exceptions/{exceptionId}/decision", operation_id = "calendar_exception_decision", tag = "calendar",
    params(("exceptionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body = Decision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarException, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_decision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarExceptionId>,
    headers: HeaderMap,
    Json(d): Json<Decision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let v = expect_version!(headers);
    let e = repo::decide_exception(&st.pool, ctx.scope.tenant_id, id, v, d, &actor(ctx)).await?;
    Ok(ok(&e, e.version))
}

async fn exception_to(
    st: &InstitutionState,
    auth: &Authenticated,
    p: Permission,
    id: CalendarExceptionId,
    headers: &HeaderMap,
    body: Option<Json<StateCommand>>,
    to: CalendarExceptionStatus,
) -> ApiResult<Response> {
    let ctx = auth.require(p)?;
    let v = expect_version!(headers);
    let e = repo::transition_exception(
        &st.pool,
        ctx.scope.tenant_id,
        id,
        v,
        to,
        reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&e, e.version))
}

/// Apply an approved exception (only within its period, institution timezone).
#[utoipa::path(post, path = "/api/v1/institution/calendar-exceptions/{exceptionId}/apply", operation_id = "calendar_exception_apply", tag = "calendar",
    params(("exceptionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarException, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_apply(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarExceptionId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    exception_to(
        &st,
        &auth,
        Permission::CalendarPublish,
        id,
        &headers,
        body,
        CalendarExceptionStatus::InEffect,
    )
    .await
}

/// Complete (expire) an exception; its governing effect ends, the record stays.
#[utoipa::path(post, path = "/api/v1/institution/calendar-exceptions/{exceptionId}/complete", operation_id = "calendar_exception_complete", tag = "calendar",
    params(("exceptionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarException, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_complete(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarExceptionId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    exception_to(
        &st,
        &auth,
        Permission::CalendarPublish,
        id,
        &headers,
        body,
        CalendarExceptionStatus::Completed,
    )
    .await
}

/// Withdraw a requested or approved (not yet applied) exception.
#[utoipa::path(post, path = "/api/v1/institution/calendar-exceptions/{exceptionId}/withdraw", operation_id = "calendar_exception_withdraw", tag = "calendar",
    params(("exceptionId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body(content = Option<StateCommand>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarException, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_withdraw(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarExceptionId>,
    headers: HeaderMap,
    body: Option<Json<StateCommand>>,
) -> ApiResult<Response> {
    exception_to(
        &st,
        &auth,
        Permission::CalendarManage,
        id,
        &headers,
        body,
        CalendarExceptionStatus::Withdrawn,
    )
    .await
}

// ============================================================================================
// Simulations
// ============================================================================================

/// List the change simulations of a calendar (newest first).
#[utoipa::path(get, path = "/api/v1/institution/calendars/{calendarId}/simulations", operation_id = "calendar_simulation_list", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarSimulationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn simulation_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
) -> ApiResult<Json<CalendarSimulationList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(Json(CalendarSimulationList {
        items: repo::list_simulations(&st.pool, ctx.scope.tenant_id, id).await?,
    }))
}

/// Simulate a proposed change against a point-in-time baseline and return the impact report.
/// Changes no calendar state and publishes no event.
#[utoipa::path(post, path = "/api/v1/institution/calendars/{calendarId}/simulations", operation_id = "calendar_simulation_create", tag = "calendar",
    params(("calendarId" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewSimulation, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CalendarSimulation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn simulation_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<AcademicCalendarId>,
    headers: HeaderMap,
    Json(cmd): Json<NewSimulation>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarManage)?;
    require_idempotency_key(&headers)?;
    let s = repo::create_simulation(&st.pool, ctx.scope.tenant_id, id, cmd, &actor(ctx)).await?;
    Ok(created(&s, s.version))
}

/// Get a calendar simulation.
#[utoipa::path(get, path = "/api/v1/institution/calendar-simulations/{simulationId}", operation_id = "calendar_simulation_get", tag = "calendar",
    params(("simulationId" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CalendarSimulation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn simulation_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarSimulationId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let s = repo::get_simulation(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(ok(&s, s.version))
}

/// Record the impact decision (accept / discard). Accepting applies nothing.
#[utoipa::path(post, path = "/api/v1/institution/calendar-simulations/{simulationId}/decision", operation_id = "calendar_simulation_decision", tag = "calendar",
    params(("simulationId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the simulation")),
    request_body = SimulationDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CalendarSimulation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn simulation_decision(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<CalendarSimulationId>,
    headers: HeaderMap,
    Json(d): Json<SimulationDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CalendarPublish)?;
    let v = expect_version!(headers);
    let s = repo::decide_simulation(&st.pool, ctx.scope.tenant_id, id, v, d, &actor(ctx)).await?;
    Ok(ok(&s, s.version))
}

/// All calendar routes.
pub fn router(state: InstitutionState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(calendar_list, calendar_create))
        .routes(routes!(calendar_get))
        .routes(routes!(calendar_retire))
        .routes(routes!(version_list, version_create))
        .routes(routes!(version_get, version_update))
        .routes(routes!(term_add))
        .routes(routes!(term_remove))
        .routes(routes!(key_date_add))
        .routes(routes!(key_date_remove))
        .routes(routes!(version_publish))
        .routes(routes!(version_decision))
        .routes(routes!(version_revise))
        .routes(routes!(version_retire))
        .routes(routes!(year_list, year_create))
        .routes(routes!(year_get))
        .routes(routes!(year_open))
        .routes(routes!(year_close))
        .routes(routes!(year_decision))
        .routes(routes!(assignment_list, assignment_create))
        .routes(routes!(assignment_list_institution))
        .routes(routes!(assignment_get))
        .routes(routes!(assignment_activate))
        .routes(routes!(assignment_retire))
        .routes(routes!(resolve))
        .routes(routes!(exception_list, exception_create))
        .routes(routes!(exception_get))
        .routes(routes!(exception_decision))
        .routes(routes!(exception_apply))
        .routes(routes!(exception_complete))
        .routes(routes!(exception_withdraw))
        .routes(routes!(simulation_list, simulation_create))
        .routes(routes!(simulation_get))
        .routes(routes!(simulation_decision))
        .with_state(state)
}
