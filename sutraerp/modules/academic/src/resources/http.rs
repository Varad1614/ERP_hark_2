//! HTTP API of course resources and library links (tag `resources`; contract §5.2).

use super::repo::{self, *};
use crate::flexible::kit::{self, created, expect_version, ok, FlexibleReason};
use crate::shared::http::actor;
use crate::shared::AcademicState;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_kernel::auth::Permission;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Resources of a section (faculty teaching it, or `course_resource.manage`).
#[utoipa::path(get, path = "/api/v1/academic/sections/{id}/resources", operation_id = "course_resource_list_section", tag = "resources",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CourseResourceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn section_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CourseResourceList>> {
    let ctx = &auth.0;
    Ok(Json(CourseResourceList {
        items: repo::list_for_section(&st, ctx, id).await?,
    }))
}

/// Add a resource / library link to a section (draft unless `publish`).
#[utoipa::path(post, path = "/api/v1/academic/sections/{id}/resources", operation_id = "course_resource_create_section", tag = "resources",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewCourseResource, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CourseResource, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn section_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewCourseResource>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    require_idempotency_key(&headers)?;
    let r = repo::create_for_section(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&r, r.version))
}

/// All resources of an offering (offering-wide and per section).
#[utoipa::path(get, path = "/api/v1/academic/offerings/{id}/resources", operation_id = "course_resource_list_offering", tag = "resources",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CourseResourceList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offering_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CourseResourceList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(Json(CourseResourceList {
        items: repo::list_for_offering(&st, ctx.scope.tenant_id, id).await?,
    }))
}

/// Add an offering-wide resource (`course_resource.manage`).
#[utoipa::path(post, path = "/api/v1/academic/offerings/{id}/resources", operation_id = "course_resource_create_offering", tag = "resources",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewCourseResource, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CourseResource, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn offering_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<NewCourseResource>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CourseResourceManage)?;
    require_idempotency_key(&headers)?;
    let r = repo::create_for_offering(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&r, r.version))
}

/// A resource.
#[utoipa::path(get, path = "/api/v1/academic/resources/{id}", operation_id = "course_resource_get", tag = "resources",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CourseResource, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn resource_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let r = repo::get(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&r, r.version))
}

/// Edit a resource (title, kind, url, description).
#[utoipa::path(patch, path = "/api/v1/academic/resources/{id}", operation_id = "course_resource_update", tag = "resources",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the resource")),
    request_body = CourseResourcePatch, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CourseResource, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn resource_update(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<CourseResourcePatch>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let v = expect_version!(headers);
    let r = repo::update(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Publish a resource (visible in the students' Library).
#[utoipa::path(post, path = "/api/v1/academic/resources/{id}/publish", operation_id = "course_resource_publish", tag = "resources",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the resource")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CourseResource, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn resource_publish(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let v = expect_version!(headers);
    let r = repo::publish(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Withdraw a resource (kept for history, hidden from the Library).
#[utoipa::path(post, path = "/api/v1/academic/resources/{id}/withdraw", operation_id = "course_resource_withdraw", tag = "resources",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the resource")),
    request_body(content = Option<FlexibleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CourseResource, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn resource_withdraw(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<FlexibleReason>>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let v = expect_version!(headers);
    let r = repo::withdraw(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Faculty: the sections I teach and their resources.
#[utoipa::path(get, path = "/api/v1/academic/my/resources", operation_id = "course_resource_my", tag = "resources",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyResources), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_resources(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyResources>> {
    let ctx = auth.require(Permission::AcademicTeach)?;
    Ok(Json(repo::mine(&st, ctx).await?))
}

/// Student Library: published resources of my sections.
#[utoipa::path(get, path = "/api/v1/academic/me/library", operation_id = "resource_self_library", tag = "resources",
    security(("session_cookie" = [])),
    responses((status = 200, body = LibraryList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_library(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<LibraryList>> {
    let ctx = auth.require(Permission::AcademicSelfService)?;
    Ok(Json(LibraryList {
        items: repo::library(&st, ctx).await?,
    }))
}

/// All resource routes.
pub fn router(state: AcademicState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(section_list, section_create))
        .routes(routes!(offering_list, offering_create))
        .routes(routes!(resource_get, resource_update))
        .routes(routes!(resource_publish))
        .routes(routes!(resource_withdraw))
        .routes(routes!(my_resources))
        .routes(routes!(my_library))
        .with_state(state)
}
