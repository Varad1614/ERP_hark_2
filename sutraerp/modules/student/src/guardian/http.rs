//! HTTP API of guardians and student contacts (tag `guardians`; contract doc §5).

use super::domain::*;
use super::repo;
use crate::completion::kit::{
    self, created, expect_version, ok, LifecycleDecision, LifecycleReason,
};
use crate::shared::http::actor;
use crate::shared::StudentState;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::Json;
use sutra_contracts::student::ids::{
    EmergencyContactId, GuardianConsentId, GuardianId, GuardianRelationshipId,
    PickupAuthorisationId, StudentId,
};
use sutra_contracts::student::status::{AssignmentStatus, GuardianRelationshipStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct GuardianQuery {
    /// Name / e-mail / phone contains.
    pub q: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct RelationshipFilter {
    pub status: Option<GuardianRelationshipStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AssignmentFilter {
    pub status: Option<AssignmentStatus>,
}

macro_rules! rel_cmd {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $call:ident) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "guardians",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the relationship")),
            request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(State(st): State<StudentState>, auth: Authenticated, Path(id): Path<GuardianRelationshipId>, headers: HeaderMap, body: Option<Json<LifecycleReason>>) -> ApiResult<Response> {
            let ctx = auth.require(Permission::GuardianManage)?;
            let v = expect_version!(headers);
            let r = repo::$call(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
            Ok(ok(&r, r.version))
        }
    };
}

// ---- guardians ----

/// Guardians registered at an institution.
#[utoipa::path(get, path = "/api/v1/student/institutions/{id}/guardians", operation_id = "guardian_list", tag = "guardians",
    params(("id" = String, Path, format = Uuid), GuardianQuery), security(("session_cookie" = [])),
    responses((status = 200, body = GuardianList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn guardian_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<GuardianQuery>,
) -> ApiResult<Json<GuardianList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(GuardianList {
        items: repo::list_guardians(&st, ctx.scope.tenant_id, id, q.q.as_deref()).await?,
    }))
}

/// Register a guardian (contact party) at an institution.
#[utoipa::path(post, path = "/api/v1/student/institutions/{id}/guardians", operation_id = "guardian_create", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewGuardian, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = Guardian, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn guardian_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewGuardian>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    require_idempotency_key(&headers)?;
    let g = repo::create_guardian(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&g, g.version))
}

/// Get a guardian.
#[utoipa::path(get, path = "/api/v1/student/guardians/{id}", operation_id = "guardian_get", tag = "guardians",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = Guardian, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn guardian_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let g = repo::get_guardian(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&g, g.version))
}

/// Update a guardian's contact details.
#[utoipa::path(patch, path = "/api/v1/student/guardians/{id}", operation_id = "guardian_update", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the guardian")),
    request_body = GuardianUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = Guardian, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn guardian_update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianId>,
    headers: HeaderMap,
    Json(cmd): Json<GuardianUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let g = repo::update_guardian(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&g, g.version))
}

/// The student relationships of a guardian.
#[utoipa::path(get, path = "/api/v1/student/guardians/{id}/relationships", operation_id = "guardian_relationships", tag = "guardians",
    params(("id" = String, Path, format = Uuid), RelationshipFilter), security(("session_cookie" = [])),
    responses((status = 200, body = GuardianRelationshipList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn guardian_relationships(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianId>,
    Query(f): Query<RelationshipFilter>,
) -> ApiResult<Json<GuardianRelationshipList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    repo::get_guardian(&st, ctx.scope.tenant_id, id).await?;
    Ok(Json(GuardianRelationshipList {
        items: repo::list_relationships(
            &st,
            ctx.scope.tenant_id,
            "guardian_id",
            id.as_uuid(),
            f.status,
        )
        .await?,
    }))
}

// ---- relationships ----

/// Guardian relationships of a student (with guardian, consents and communication authorisations).
#[utoipa::path(get, path = "/api/v1/student/students/{id}/guardian-relationships", operation_id = "guardian_relationship_list", tag = "guardians",
    params(("id" = String, Path, format = Uuid), RelationshipFilter), security(("session_cookie" = [])),
    responses((status = 200, body = GuardianRelationshipList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(f): Query<RelationshipFilter>,
) -> ApiResult<Json<GuardianRelationshipList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(GuardianRelationshipList {
        items: repo::list_relationships(
            &st,
            ctx.scope.tenant_id,
            "student_id",
            id.as_uuid(),
            f.status,
        )
        .await?,
    }))
}

/// Propose a guardian relationship (L3-C-051).
#[utoipa::path(post, path = "/api/v1/student/students/{id}/guardian-relationships", operation_id = "guardian_relationship_propose", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewGuardianRelationship, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_propose(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<NewGuardianRelationship>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    require_idempotency_key(&headers)?;
    let r = repo::propose(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&r, r.version))
}

/// Get a guardian relationship.
#[utoipa::path(get, path = "/api/v1/student/guardian-relationships/{id}", operation_id = "guardian_relationship_get", tag = "guardians",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let r = repo::get_relationship(&st, ctx.scope.tenant_id, id).await?;
    Ok(ok(&r, r.version))
}

/// Governed modification of a relationship (type, primary flag, notes; L3-C-052).
#[utoipa::path(patch, path = "/api/v1/student/guardian-relationships/{id}", operation_id = "guardian_relationship_modify", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the relationship")),
    request_body = GuardianRelationshipUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_modify(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
    headers: HeaderMap,
    Json(cmd): Json<GuardianRelationshipUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let r = repo::modify(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Submit the relationship for verification (WorkflowPort `GUARDIAN_VERIFICATION`).
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/submit-verification", operation_id = "guardian_relationship_submit_verification", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the relationship")),
    request_body = SubmitVerification, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_submit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
    headers: HeaderMap,
    Json(cmd): Json<SubmitVerification>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let r = repo::submit_verification(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Verification decision (`AwaitDecision`; reject → proposed).
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/verification-decision", operation_id = "guardian_relationship_verification_decision", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the relationship")),
    request_body = LifecycleDecision, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
    headers: HeaderMap,
    Json(cmd): Json<LifecycleDecision>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "guardian verification").await?;
    let v = expect_version!(headers);
    let r = repo::verification_decision(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Activate a verified relationship.
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/activate", operation_id = "guardian_relationship_activate", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the relationship")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_activate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let r = repo::activate(&st, ctx, id, v, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

rel_cmd!(
    /// Suspend an active relationship (reason required; responsibilities are suspended).
    relationship_suspend, "/api/v1/student/guardian-relationships/{id}/suspend", "guardian_relationship_suspend", suspend);
rel_cmd!(
    /// Reinstate a suspended relationship (L3-C-060).
    relationship_reinstate, "/api/v1/student/guardian-relationships/{id}/reinstate", "guardian_relationship_reinstate", reinstate);
rel_cmd!(
    /// Terminate a relationship (reason required; responsibilities revoked, consents withdrawn).
    relationship_terminate, "/api/v1/student/guardian-relationships/{id}/terminate", "guardian_relationship_terminate", terminate);

/// Designate or change legal custody (L3-C-053).
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/custody", operation_id = "guardian_relationship_custody", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the relationship")),
    request_body = CustodyChange, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GuardianRelationship, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_custody(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
    headers: HeaderMap,
    Json(cmd): Json<CustodyChange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let r = repo::change_custody(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&r, r.version))
}

/// Custody designation history of a relationship.
#[utoipa::path(get, path = "/api/v1/student/guardian-relationships/{id}/custody", operation_id = "guardian_relationship_custody_history", tag = "guardians",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CustodyHistory), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn relationship_custody_history(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
) -> ApiResult<Json<CustodyHistory>> {
    let ctx = auth.require(Permission::StudentRead)?;
    repo::get_relationship(&st, ctx.scope.tenant_id, id).await?;
    Ok(Json(CustodyHistory {
        items: repo::custody_history(&st, ctx.scope.tenant_id, id).await?,
    }))
}

/// Grant a guardian consent (active relationship).
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/consents", operation_id = "guardian_consent_grant", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewGuardianConsent, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GuardianConsent, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn consent_grant(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
    headers: HeaderMap,
    Json(cmd): Json<NewGuardianConsent>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    require_idempotency_key(&headers)?;
    let c = repo::grant_consent(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// Withdraw a consent (the grant stays in history).
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/consents/{consentId}/withdraw", operation_id = "guardian_consent_withdraw", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("consentId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the consent")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GuardianConsent, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn consent_withdraw(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, consent_id)): Path<(GuardianRelationshipId, GuardianConsentId)>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let c =
        repo::withdraw_consent(&st, ctx, id, consent_id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Authorise guardian communication (channels + topics; data-sharing consent needed beyond emergency).
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/communications", operation_id = "guardian_communication_authorise", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewGuardianCommunication, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = GuardianCommunication, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn communication_authorise(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<GuardianRelationshipId>,
    headers: HeaderMap,
    Json(cmd): Json<NewGuardianCommunication>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    require_idempotency_key(&headers)?;
    let c = repo::authorise_communication(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// Revoke a communication authorisation.
#[utoipa::path(post, path = "/api/v1/student/guardian-relationships/{id}/communications/{communicationId}/revoke", operation_id = "guardian_communication_revoke", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("communicationId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the authorisation")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GuardianCommunication, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn communication_revoke(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, communication_id)): Path<(GuardianRelationshipId, Uuid)>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let c = repo::revoke_communication(
        &st,
        ctx,
        id,
        communication_id,
        v,
        kit::reason(body),
        &actor(ctx),
    )
    .await?;
    Ok(ok(&c, c.version))
}

// ---- emergency contacts ----

/// Emergency contacts of a student.
#[utoipa::path(get, path = "/api/v1/student/students/{id}/emergency-contacts", operation_id = "emergency_contact_list", tag = "guardians",
    params(("id" = String, Path, format = Uuid), AssignmentFilter), security(("session_cookie" = [])),
    responses((status = 200, body = EmergencyContactList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn emergency_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(f): Query<AssignmentFilter>,
) -> ApiResult<Json<EmergencyContactList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    let mut items: Vec<EmergencyContact> = repo::list_for_student(
        &st,
        ctx.scope.tenant_id,
        repo::TABLE_EMERGENCY,
        id,
        f.status,
    )
    .await?;
    items.sort_by_key(|c| (c.status != AssignmentStatus::Active, c.priority));
    Ok(Json(EmergencyContactList { items }))
}

/// Assign an emergency contact.
#[utoipa::path(post, path = "/api/v1/student/students/{id}/emergency-contacts", operation_id = "emergency_contact_assign", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewEmergencyContact, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = EmergencyContact, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn emergency_assign(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<NewEmergencyContact>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    require_idempotency_key(&headers)?;
    let c = repo::add_emergency_contact(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&c, c.version))
}

/// Get an emergency contact.
#[utoipa::path(get, path = "/api/v1/student/emergency-contacts/{id}", operation_id = "emergency_contact_get", tag = "guardians",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = EmergencyContact, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn emergency_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<EmergencyContactId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let c: EmergencyContact = repo::get_row(
        &st,
        ctx.scope.tenant_id,
        repo::TABLE_EMERGENCY,
        "emergency contact",
        id.as_uuid(),
    )
    .await?;
    Ok(ok(&c, c.version))
}

/// Update an emergency contact's phone numbers / priority.
#[utoipa::path(patch, path = "/api/v1/student/emergency-contacts/{id}", operation_id = "emergency_contact_update", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the contact")),
    request_body = EmergencyContactUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EmergencyContact, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn emergency_update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<EmergencyContactId>,
    headers: HeaderMap,
    Json(cmd): Json<EmergencyContactUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let c = repo::update_emergency_contact(&st, ctx, id, v, cmd, &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

/// Remove an emergency contact.
#[utoipa::path(post, path = "/api/v1/student/emergency-contacts/{id}/remove", operation_id = "emergency_contact_remove", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the contact")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = EmergencyContact, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn emergency_remove(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<EmergencyContactId>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let c = repo::remove_emergency_contact(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&c, c.version))
}

// ---- financial responsibility ----

/// Financial responsibilities of a student.
#[utoipa::path(get, path = "/api/v1/student/students/{id}/financial-responsibilities", operation_id = "financial_responsibility_list", tag = "guardians",
    params(("id" = String, Path, format = Uuid), AssignmentFilter), security(("session_cookie" = [])),
    responses((status = 200, body = FinancialResponsibilityList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn financial_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(f): Query<AssignmentFilter>,
) -> ApiResult<Json<FinancialResponsibilityList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(FinancialResponsibilityList {
        items: repo::list_for_student(
            &st,
            ctx.scope.tenant_id,
            repo::TABLE_FINANCIAL,
            id,
            f.status,
        )
        .await?,
    }))
}

/// Assign financial responsibility to a guardian (shares ≤ 100 % per type).
#[utoipa::path(post, path = "/api/v1/student/students/{id}/financial-responsibilities", operation_id = "financial_responsibility_assign", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewFinancialResponsibility, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FinancialResponsibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn financial_assign(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<NewFinancialResponsibility>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    require_idempotency_key(&headers)?;
    let f = repo::assign_financial_responsibility(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&f, f.version))
}

/// Get a financial responsibility.
#[utoipa::path(get, path = "/api/v1/student/financial-responsibilities/{id}", operation_id = "financial_responsibility_get", tag = "guardians",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FinancialResponsibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn financial_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let f: FinancialResponsibility = repo::get_row(
        &st,
        ctx.scope.tenant_id,
        repo::TABLE_FINANCIAL,
        "financial responsibility",
        id,
    )
    .await?;
    Ok(ok(&f, f.version))
}

/// End a financial responsibility.
#[utoipa::path(post, path = "/api/v1/student/financial-responsibilities/{id}/end", operation_id = "financial_responsibility_end", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the responsibility")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FinancialResponsibility, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn financial_end(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let f =
        repo::end_financial_responsibility(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&f, f.version))
}

// ---- pickup ----

/// Pickup authorisations of a student.
#[utoipa::path(get, path = "/api/v1/student/students/{id}/pickup-authorisations", operation_id = "pickup_authorisation_list", tag = "guardians",
    params(("id" = String, Path, format = Uuid), AssignmentFilter), security(("session_cookie" = [])),
    responses((status = 200, body = PickupAuthorisationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pickup_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(f): Query<AssignmentFilter>,
) -> ApiResult<Json<PickupAuthorisationList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    crate::shared::student::load_student(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(Json(PickupAuthorisationList {
        items: repo::list_for_student(&st, ctx.scope.tenant_id, repo::TABLE_PICKUP, id, f.status)
            .await?,
    }))
}

/// Grant a pickup authorisation.
#[utoipa::path(post, path = "/api/v1/student/students/{id}/pickup-authorisations", operation_id = "pickup_authorisation_grant", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = NewPickupAuthorisation, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PickupAuthorisation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pickup_grant(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<NewPickupAuthorisation>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    require_idempotency_key(&headers)?;
    let p = repo::grant_pickup(&st, ctx, id, cmd, &actor(ctx)).await?;
    Ok(created(&p, p.version))
}

/// Get a pickup authorisation.
#[utoipa::path(get, path = "/api/v1/student/pickup-authorisations/{id}", operation_id = "pickup_authorisation_get", tag = "guardians",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PickupAuthorisation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pickup_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<PickupAuthorisationId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let p: PickupAuthorisation = repo::get_row(
        &st,
        ctx.scope.tenant_id,
        repo::TABLE_PICKUP,
        "pickup authorisation",
        id.as_uuid(),
    )
    .await?;
    Ok(ok(&p, p.version))
}

/// Revoke a pickup authorisation.
#[utoipa::path(post, path = "/api/v1/student/pickup-authorisations/{id}/revoke", operation_id = "pickup_authorisation_revoke", tag = "guardians",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the authorisation")),
    request_body(content = Option<LifecycleReason>), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PickupAuthorisation, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn pickup_revoke(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<PickupAuthorisationId>,
    headers: HeaderMap,
    body: Option<Json<LifecycleReason>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::GuardianManage)?;
    let v = expect_version!(headers);
    let p = repo::revoke_pickup(&st, ctx, id, v, kit::reason(body), &actor(ctx)).await?;
    Ok(ok(&p, p.version))
}

/// All guardian routes.
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(guardian_list, guardian_create))
        .routes(routes!(guardian_get, guardian_update))
        .routes(routes!(guardian_relationships))
        .routes(routes!(relationship_list, relationship_propose))
        .routes(routes!(relationship_get, relationship_modify))
        .routes(routes!(relationship_submit))
        .routes(routes!(relationship_decision))
        .routes(routes!(relationship_activate))
        .routes(routes!(relationship_suspend))
        .routes(routes!(relationship_reinstate))
        .routes(routes!(relationship_terminate))
        .routes(routes!(relationship_custody, relationship_custody_history))
        .routes(routes!(consent_grant))
        .routes(routes!(consent_withdraw))
        .routes(routes!(communication_authorise))
        .routes(routes!(communication_revoke))
        .routes(routes!(emergency_list, emergency_assign))
        .routes(routes!(emergency_get, emergency_update))
        .routes(routes!(emergency_remove))
        .routes(routes!(financial_list, financial_assign))
        .routes(routes!(financial_get))
        .routes(routes!(financial_end))
        .routes(routes!(pickup_list, pickup_grant))
        .routes(routes!(pickup_get))
        .routes(routes!(pickup_revoke))
        .with_state(state)
}
