//! Identity provisioning admin API (Part 2 · S4-idp). Always the **session** tenant (RLS).
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET  /api/v1/platform/identities` · `/{id}` | `platform.identity.read` |
//! | `POST /api/v1/platform/identities/provision` (student / staff) | `platform.identity.manage` |
//! | `POST /api/v1/platform/identities/admins` (+ privileged role request) | `platform.identity.manage` |
//! | `POST /api/v1/platform/identities/{id}/required-actions` | `platform.identity.manage` |
//! | `POST /api/v1/platform/identities/{id}/retry` | `platform.identity.manage` |
//! | `POST /api/v1/platform/identities/{id}/role-requests` | `platform.identity.manage` |
//! | `POST /api/v1/platform/identities/{id}/suspend` · `/reactivate` | `platform.identity.lifecycle` (step-up) |
//! | `GET  /api/v1/platform/identity-role-requests` | `platform.identity.read` |
//! | `POST /api/v1/platform/identity-role-requests/{id}/decision` | `platform.identity.role.approve` (step-up, never the requester) |
//! | `GET  /api/v1/platform/identity-role-mappings` | `platform.identity.read` |
//! | `POST /api/v1/platform/identity-role-mappings` · `DELETE …/{id}` | `platform.identity.role.approve` (step-up) |
//!
//! Separation of duties: nobody acts on their **own** identity (suspend, re-enable, required
//! actions, role requests, decisions); a role request that grants a privileged role (administrator
//! or an approver role) stays `pending` until **another** holder of `platform.identity.role.approve`
//! decides it (`ensure_independent_decider`, plus a database CHECK). Every change is audited in
//! the same transaction and re-queues the identity for the sync worker.

use super::{apply_facts, load, FactsOutcome, LinkRow, LINK_COLUMNS};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool, Row};
use std::sync::Arc;
use sutra_contracts::platform::identity::{
    AccountState, ErpSubjectType, IdentityFacts, IdentitySubjectSource, RequiredAction,
};
use sutra_kernel::approval::ensure_independent_decider;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::{AuthContext, Permission, Role};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_persistence::{audit, map_db_error};
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Handler state.
#[derive(Clone)]
pub struct IdentityState {
    pub pool: PgPool,
    pub sources: Vec<Arc<dyn IdentitySubjectSource>>,
    /// `Some(kind)` when provisioning is on (`SUTRA_IDP_PROVISIONING`), `None` when off.
    pub idp: Option<&'static str>,
    pub send_actions_email: bool,
}

impl IdentityState {
    /// Provisioning switched off: reads work, changes answer `503`.
    pub fn disabled(pool: PgPool) -> Self {
        Self {
            pool,
            sources: vec![],
            idp: None,
            send_actions_email: false,
        }
    }

    fn idp(&self) -> Result<&'static str, KernelError> {
        self.idp.ok_or_else(|| {
            KernelError::Unavailable(
                "identity provisioning is not configured (SUTRA_IDP_PROVISIONING=off)".into(),
            )
        })
    }

    fn source(&self, t: ErpSubjectType) -> Option<&Arc<dyn IdentitySubjectSource>> {
        self.sources.iter().find(|s| s.subject_type() == t)
    }
}

/// Mounted next to the other platform routers by the composition root.
pub fn router(state: IdentityState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list_identities))
        .routes(routes!(get_identity))
        .routes(routes!(provision))
        .routes(routes!(provision_admin))
        .routes(routes!(required_actions))
        .routes(routes!(retry))
        .routes(routes!(suspend))
        .routes(routes!(reactivate))
        .routes(routes!(request_roles))
        .routes(routes!(list_role_requests))
        .routes(routes!(role_request_decision))
        .routes(routes!(list_mappings, create_mapping))
        .routes(routes!(delete_mapping))
        .with_state(state)
}

// ---------------------------------------------------------------------------------------------
// Views and commands
// ---------------------------------------------------------------------------------------------

/// One identity (link between an ERP subject and its IdP account).
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IdentityView {
    pub id: Uuid,
    /// `student` | `staff` | `admin`.
    pub subject_type: String,
    pub subject_id: Uuid,
    pub institution_id: Option<Uuid>,
    /// `keycloak` | `memory`.
    pub idp: String,
    /// IdP user id (the OIDC `sub`), once provisioned.
    pub idp_user_id: Option<String>,
    pub username: String,
    pub email: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    /// State the ERP record calls for: `active` | `suspended` | `disabled`.
    pub erp_state: String,
    pub admin_hold: bool,
    pub hold_reason: Option<String>,
    /// State the IdP account is driven to.
    pub desired_state: String,
    /// Effective SutraERP roles (record roles + granted roles).
    pub roles: Vec<String>,
    pub base_roles: Vec<String>,
    pub granted_roles: Vec<String>,
    /// Keycloak required actions still to be triggered.
    pub pending_actions: Vec<String>,
    /// `pending` | `synced` | `failed` | `conflict`.
    pub sync_status: String,
    pub attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
    pub last_error: Option<String>,
    pub synced_at: Option<DateTime<Utc>>,
    pub applied_state: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<LinkRow> for IdentityView {
    fn from(l: LinkRow) -> Self {
        let roles = l.roles().iter().map(|r| r.as_str().to_owned()).collect();
        Self {
            id: l.id,
            subject_type: l.subject_type,
            subject_id: l.subject_id,
            institution_id: l.institution_id,
            idp: l.idp,
            idp_user_id: l.idp_user_id,
            username: l.username,
            email: l.email,
            given_name: l.given_name,
            family_name: l.family_name,
            erp_state: l.erp_state,
            admin_hold: l.admin_hold,
            hold_reason: l.hold_reason,
            desired_state: l.desired_state,
            roles,
            base_roles: l.base_roles,
            granted_roles: l.granted_roles,
            pending_actions: l.pending_actions,
            sync_status: l.sync_status,
            attempts: l.attempts,
            next_attempt_at: l.next_attempt_at,
            last_error: l.last_error,
            synced_at: l.synced_at,
            applied_state: l.applied_state,
            version: l.version,
            created_at: l.created_at,
            updated_at: l.updated_at,
        }
    }
}

/// Page of identities.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IdentityPage {
    pub items: Vec<IdentityView>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct IdentityQuery {
    /// `student` | `staff` | `admin`.
    pub subject_type: Option<String>,
    /// `pending` | `synced` | `failed` | `conflict`.
    pub sync_status: Option<String>,
    /// 1–500 (default 100), newest first.
    pub limit: Option<u32>,
}

/// Provision the identity of a student or staff member.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionRequest {
    /// `student` | `staff`.
    pub subject_type: String,
    pub subject_id: Uuid,
}

/// Create a platform administrator identity. The roles are requested, not granted: privileged
/// roles wait for a second administrator's decision.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminIdentityRequest {
    pub email: String,
    pub given_name: String,
    pub family_name: Option<String>,
    /// Pin the identity to one institution (default: tenant-wide).
    pub institution_id: Option<Uuid>,
    /// Requested SutraERP roles (e.g. `administrator`, `registrar`).
    pub roles: Vec<String>,
    pub reason: String,
}

/// Created administrator identity + its role request.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AdminIdentityCreated {
    pub identity: IdentityView,
    pub role_request: RoleRequestView,
}

/// Trigger required actions (activation, password recovery, MFA enrolment).
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequiredActionsRequest {
    pub actions: Vec<RequiredAction>,
    /// Also e-mail the user an "execute actions" link (needs SMTP in the realm).
    #[serde(default)]
    pub send_email: bool,
}

/// Reason of an administrative suspension / re-enabling.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HoldRequest {
    pub reason: String,
}

/// Grant and / or revoke SutraERP roles of an identity.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleChangeRequest {
    #[serde(default)]
    pub grant: Vec<String>,
    #[serde(default)]
    pub revoke: Vec<String>,
    pub reason: String,
}

/// One role request.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleRequestView {
    pub id: Uuid,
    pub identity_id: Uuid,
    pub grant_roles: Vec<String>,
    pub revoke_roles: Vec<String>,
    /// A privileged role is granted: another administrator must decide.
    pub privileged: bool,
    pub reason: String,
    /// `pending` | `approved` | `rejected` | `applied` (non-privileged, no decision needed).
    pub status: String,
    pub requested_by: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleRequestPage {
    pub items: Vec<RoleRequestView>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RoleRequestQuery {
    /// `pending` | `approved` | `rejected` | `applied`.
    pub status: Option<String>,
}

/// Decision on a pending role request.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleDecision {
    /// `approve` | `reject`.
    pub decision: String,
    pub reason: String,
}

/// Extra IdP target of an ERP role.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleMappingView {
    pub id: Uuid,
    pub institution_id: Option<Uuid>,
    pub erp_role: String,
    /// `realm_role` | `client_role` | `group`.
    pub target_kind: String,
    pub target: String,
    pub client_id: Option<String>,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RoleMappingPage {
    pub items: Vec<RoleMappingView>,
}

/// New ERP-role → IdP mapping.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewRoleMapping {
    pub institution_id: Option<Uuid>,
    pub erp_role: String,
    /// `realm_role` | `client_role` | `group`.
    pub target_kind: String,
    /// Realm role, client role or group path (`/lms/teachers`).
    pub target: String,
    /// Required for `client_role`.
    pub client_id: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn invalid(field: &str, code: &str, msg: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, msg)])
}

fn reason(r: &str, field: &str) -> Result<String, KernelError> {
    let t = r.trim();
    if t.is_empty() || t.len() > 1000 {
        return Err(invalid(
            field,
            "length",
            "a reason of 1–1000 characters is required",
        ));
    }
    Ok(t.to_owned())
}

fn roles_of(names: &[String], field: &str) -> Result<Vec<Role>, KernelError> {
    let mut v = vec![];
    for n in names {
        let r = Role::parse(n.trim())
            .ok_or_else(|| invalid(field, "unknown", &format!("unknown role {n:?}")))?;
        if !v.contains(&r) {
            v.push(r);
        }
    }
    Ok(v)
}

/// Whether `link` is the caller's own identity.
fn is_self(ctx: &AuthContext, link: &LinkRow) -> bool {
    link.idp_user_id.as_deref() == Some(ctx.subject.as_str())
        || link
            .email
            .as_deref()
            .is_some_and(|e| e.eq_ignore_ascii_case(&ctx.login_id))
        || link.username.eq_ignore_ascii_case(&ctx.login_id)
}

fn not_self(ctx: &AuthContext, link: &LinkRow, what: &str) -> Result<(), KernelError> {
    if is_self(ctx, link) {
        return Err(KernelError::Forbidden(format!(
            "separation of duties: you cannot {what} your own identity"
        )));
    }
    Ok(())
}

fn user_audit(
    ctx: &AuthContext,
    action: &str,
    target_id: Uuid,
    details: serde_json::Value,
) -> AuditEntry {
    AuditEntry::new(ctx.scope, action, AuditOutcome::Succeeded)
        .actor(ActorKind::User, Some(ctx.subject.clone()))
        .target("platform.identity_link", target_id.to_string())
        .details(details)
}

async fn found(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<LinkRow, KernelError> {
    load(conn, ctx.scope.tenant_id.as_uuid(), id, true)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "identity" })
}

/// Re-queues a link for the sync worker.
async fn requeue(conn: &mut PgConnection, id: Uuid) -> Result<LinkRow, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE platform.identity_link SET sync_status = 'pending', attempts = 0, \
           next_attempt_at = now(), last_error = NULL, version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING {LINK_COLUMNS}"
    ))
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    LinkRow::from_row(&row).map_err(map_db_error)
}

const REQUEST_COLUMNS: &str =
    "id, link_id, grant_roles, revoke_roles, privileged, reason, status, \
    requested_by, decided_by, decided_at, decision_reason, created_at";

fn request_view(r: &sqlx::postgres::PgRow) -> Result<RoleRequestView, sqlx::Error> {
    Ok(RoleRequestView {
        id: r.try_get("id")?,
        identity_id: r.try_get("link_id")?,
        grant_roles: r.try_get("grant_roles")?,
        revoke_roles: r.try_get("revoke_roles")?,
        privileged: r.try_get("privileged")?,
        reason: r.try_get("reason")?,
        status: r.try_get("status")?,
        requested_by: r.try_get("requested_by")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        decision_reason: r.try_get("decision_reason")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Applies granted / revoked roles to the link (privileged grants also trigger TOTP enrolment)
/// and re-queues it.
async fn apply_roles(
    conn: &mut PgConnection,
    link: &LinkRow,
    grant: &[String],
    revoke: &[String],
) -> Result<LinkRow, KernelError> {
    let mut granted: Vec<String> = link
        .granted_roles
        .iter()
        .filter(|r| !revoke.contains(r))
        .cloned()
        .collect();
    for g in grant {
        if !granted.contains(g) {
            granted.push(g.clone());
        }
    }
    let base: Vec<String> = link
        .base_roles
        .iter()
        .filter(|r| !revoke.contains(r))
        .cloned()
        .collect();
    let mut actions = link.pending_actions.clone();
    if grant
        .iter()
        .filter_map(|g| Role::parse(g))
        .any(|r| r.is_privileged())
        && !actions
            .iter()
            .any(|a| a == RequiredAction::ConfigureTotp.keycloak())
    {
        actions.push(RequiredAction::ConfigureTotp.keycloak().to_owned());
    }
    let row = sqlx::query(&format!(
        "UPDATE platform.identity_link SET granted_roles = $2, base_roles = $3, \
           pending_actions = $4, sync_status = 'pending', attempts = 0, next_attempt_at = now(), \
           last_error = NULL, version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING {LINK_COLUMNS}"
    ))
    .bind(link.id)
    .bind(&granted)
    .bind(&base)
    .bind(&actions)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    LinkRow::from_row(&row).map_err(map_db_error)
}

/// Records a role request; non-privileged ones are applied at once (`applied`).
async fn create_role_request(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    link: &LinkRow,
    grant: &[Role],
    revoke: &[Role],
    why: &str,
) -> Result<(RoleRequestView, LinkRow), KernelError> {
    if grant.is_empty() && revoke.is_empty() {
        return Err(invalid(
            "grant",
            "empty",
            "grant or revoke at least one role",
        ));
    }
    if grant.iter().any(|g| revoke.contains(g)) {
        return Err(invalid(
            "revoke",
            "overlap",
            "a role cannot be granted and revoked at once",
        ));
    }
    let privileged = grant.iter().any(Role::is_privileged);
    let g: Vec<String> = grant.iter().map(|r| r.as_str().to_owned()).collect();
    let r: Vec<String> = revoke.iter().map(|r| r.as_str().to_owned()).collect();
    let row = sqlx::query(&format!(
        "INSERT INTO platform.identity_role_request \
           (tenant_id, institution_id, link_id, grant_roles, revoke_roles, privileged, reason, \
            status, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {REQUEST_COLUMNS}"
    ))
    .bind(link.tenant_id)
    .bind(link.institution_id)
    .bind(link.id)
    .bind(&g)
    .bind(&r)
    .bind(privileged)
    .bind(why)
    .bind(if privileged { "pending" } else { "applied" })
    .bind(&ctx.subject)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("this identity already has a pending role request".into())
        }
        other => other,
    })?;
    let view = request_view(&row).map_err(map_db_error)?;
    let link = if privileged {
        link.clone()
    } else {
        apply_roles(conn, link, &g, &r).await?
    };
    audit::insert(
        &mut *conn,
        &user_audit(
            ctx,
            if privileged {
                "platform.identity.role_request.created"
            } else {
                "platform.identity.roles.changed"
            },
            link.id,
            serde_json::json!({
                "requestId": view.id, "grant": g, "revoke": r, "privileged": privileged,
                "reason": why,
            }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    Ok((view, link))
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

/// Identities of the session tenant (newest first).
#[utoipa::path(
    get,
    path = "/api/v1/platform/identities",
    operation_id = "platform_identity_list",
    tag = "platform-identity",
    params(IdentityQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = IdentityPage),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `platform.identity.read`", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_identities(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Query(q): Query<IdentityQuery>,
) -> ApiResult<Json<IdentityPage>> {
    let ctx = auth.require(Permission::IdentityRead)?;
    let limit = i64::from(q.limit.unwrap_or(100).clamp(1, 500));
    let rows = sqlx::query(&format!(
        "SELECT {LINK_COLUMNS} FROM platform.identity_link WHERE tenant_id = $1 \
           AND ($2::text IS NULL OR subject_type = $2) AND ($3::text IS NULL OR sync_status = $3) \
         ORDER BY created_at DESC, id DESC LIMIT $4"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(q.subject_type.as_deref())
    .bind(q.sync_status.as_deref())
    .bind(limit)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| LinkRow::from_row(r).map(IdentityView::from))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(IdentityPage { items }))
}

/// One identity.
#[utoipa::path(
    get,
    path = "/api/v1/platform/identities/{id}",
    operation_id = "platform_identity_get",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = IdentityView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_identity(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<IdentityView>> {
    let ctx = auth.require(Permission::IdentityRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let link = load(&mut conn, ctx.scope.tenant_id.as_uuid(), id, false)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "identity" })?;
    Ok(Json(link.into()))
}

/// Provisions (or re-queues) the identity of a student or staff member from the owning record:
/// lifecycle state, institution, e-mail and base role come from the record. Existing identities
/// are matched first (one identity per subject; username / e-mail unique per IdP), so calling
/// it twice never creates a second account.
#[utoipa::path(
    post,
    path = "/api/v1/platform/identities/provision",
    operation_id = "platform_identity_provision",
    tag = "platform-identity",
    request_body = ProvisionRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, description = "Identity created and queued", body = IdentityView),
        (status = 200, description = "Identity already existed (re-queued when its facts changed)", body = IdentityView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "No such student / staff member in the session tenant", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Duplicate identity (username / e-mail already used)", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Provisioning is off", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn provision(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Json(cmd): Json<ProvisionRequest>,
) -> ApiResult<(StatusCode, Json<IdentityView>)> {
    let ctx = auth.require(Permission::IdentityManage)?;
    let idp = st.idp()?;
    let t = ErpSubjectType::parse(cmd.subject_type.trim())
        .filter(|t| *t != ErpSubjectType::Admin)
        .ok_or_else(|| {
            invalid(
                "subjectType",
                "enum",
                "subjectType must be student or staff",
            )
        })?;
    let src = st
        .source(t)
        .ok_or_else(|| KernelError::Unavailable(format!("no {} directory wired", t.as_str())))?;
    let tenant = ctx.scope.tenant_id.as_uuid();
    let facts = src
        .facts(tenant, cmd.subject_id)
        .await?
        .ok_or(KernelError::NotFound { entity: "subject" })?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let out = apply_facts(
        &mut tx,
        &facts,
        idp,
        true,
        st.send_actions_email,
        Some(&ctx.subject),
    )
    .await
    .map_err(KernelError::from)?;
    let (status, link) = match out {
        FactsOutcome::Created(l) => (StatusCode::CREATED, l),
        FactsOutcome::Updated(l) => (StatusCode::OK, l),
        FactsOutcome::Unchanged(Some(l)) => {
            // Explicit request: retry a failed / conflicting sync too.
            if l.sync_status == "synced" {
                (StatusCode::OK, l)
            } else {
                (StatusCode::OK, requeue(&mut tx, l.id).await?)
            }
        }
        FactsOutcome::Unchanged(None) => {
            return Err(KernelError::internal_msg("identity not created").into())
        }
    };
    audit::insert(
        &mut *tx,
        &user_audit(
            ctx,
            "platform.identity.provision",
            link.id,
            serde_json::json!({
                "subjectType": link.subject_type, "subjectId": link.subject_id,
                "created": status == StatusCode::CREATED, "erpState": link.erp_state,
            }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((status, Json(link.into())))
}

/// Creates a platform administrator identity (no ERP record) and a role request for its roles:
/// privileged roles wait for another administrator's decision; until then the identity can sign
/// in to nothing (no role → `403` at sign-in).
#[utoipa::path(
    post,
    path = "/api/v1/platform/identities/admins",
    operation_id = "platform_identity_admin_create",
    tag = "platform-identity",
    request_body = AdminIdentityRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = AdminIdentityCreated),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Duplicate identity", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn provision_admin(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Json(cmd): Json<AdminIdentityRequest>,
) -> ApiResult<(StatusCode, Json<AdminIdentityCreated>)> {
    let ctx = auth.require(Permission::IdentityManage)?;
    let idp = st.idp()?;
    let email = cmd.email.trim().to_ascii_lowercase();
    if !(3..=320).contains(&email.len()) || !email.contains('@') {
        return Err(invalid("email", "format", "a valid e-mail address is required").into());
    }
    if cmd.given_name.trim().is_empty() {
        return Err(invalid("givenName", "required", "givenName is required").into());
    }
    let roles = roles_of(&cmd.roles, "roles")?;
    if roles.is_empty() {
        return Err(invalid("roles", "empty", "request at least one role").into());
    }
    let why = reason(&cmd.reason, "reason")?;
    if email.eq_ignore_ascii_case(&ctx.login_id) {
        return Err(KernelError::Forbidden(
            "separation of duties: you cannot create an identity for yourself".into(),
        )
        .into());
    }
    let tenant = ctx.scope.tenant_id.as_uuid();
    let facts = IdentityFacts {
        subject_type: ErpSubjectType::Admin,
        subject_id: Uuid::now_v7(),
        tenant_id: tenant,
        institution_id: cmd.institution_id.or(ctx.scope.institution_id.map(|i| i.0)),
        number: email.clone(),
        email: Some(email),
        given_name: cmd.given_name.trim().to_owned(),
        family_name: cmd
            .family_name
            .map(|f| f.trim().to_owned())
            .filter(|f| !f.is_empty()),
        state: AccountState::Active,
        base_roles: vec![],
        subject_ref: None,
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let link = match apply_facts(
        &mut tx,
        &facts,
        idp,
        true,
        st.send_actions_email,
        Some(&ctx.subject),
    )
    .await
    .map_err(KernelError::from)?
    {
        FactsOutcome::Created(l) => l,
        _ => return Err(KernelError::Conflict("identity already exists".into()).into()),
    };
    // Administrators always enrol TOTP (privileged roles need MFA, SEC-P1-AUTH-001).
    let link = if roles.iter().any(Role::is_privileged) {
        let row = sqlx::query(&format!(
            "UPDATE platform.identity_link SET pending_actions = \
               array_append(pending_actions, 'CONFIGURE_TOTP') WHERE id = $1 RETURNING {LINK_COLUMNS}"
        ))
        .bind(link.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        LinkRow::from_row(&row).map_err(map_db_error)?
    } else {
        link
    };
    let (request, link) = create_role_request(&mut tx, ctx, &link, &roles, &[], &why).await?;
    audit::insert(
        &mut *tx,
        &user_audit(
            ctx,
            "platform.identity.admin.created",
            link.id,
            serde_json::json!({ "username": link.username, "requestId": request.id }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(AdminIdentityCreated {
            identity: link.into(),
            role_request: request,
        }),
    ))
}

/// Triggers required actions: `verifyEmail` + `updatePassword` (activation), `updatePassword`
/// (password recovery), `configureTotp` (MFA enrolment / re-enrolment). Applied by the next sync.
#[utoipa::path(
    post,
    path = "/api/v1/platform/identities/{id}/required-actions",
    operation_id = "platform_identity_required_actions",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    request_body = RequiredActionsRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = IdentityView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing permission, or your own identity", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn required_actions(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<RequiredActionsRequest>,
) -> ApiResult<Json<IdentityView>> {
    let ctx = auth.require(Permission::IdentityManage)?;
    st.idp()?;
    if cmd.actions.is_empty() {
        return Err(invalid("actions", "empty", "name at least one action").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let link = found(&mut tx, ctx, id).await?;
    not_self(ctx, &link, "trigger required actions on")?;
    if cmd.send_email && link.email.is_none() {
        return Err(invalid(
            "sendEmail",
            "no-email",
            "the identity has no e-mail address",
        )
        .into());
    }
    let mut actions = link.pending_actions.clone();
    for a in &cmd.actions {
        if !actions.iter().any(|x| x == a.keycloak()) {
            actions.push(a.keycloak().to_owned());
        }
    }
    sqlx::query(
        "UPDATE platform.identity_link SET pending_actions = $2, \
           send_actions_email = send_actions_email OR $3 WHERE id = $1",
    )
    .bind(link.id)
    .bind(&actions)
    .bind(cmd.send_email)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let link = requeue(&mut tx, link.id).await?;
    audit::insert(
        &mut *tx,
        &user_audit(
            ctx,
            "platform.identity.required_actions.requested",
            link.id,
            serde_json::json!({
                "actions": cmd.actions.iter().map(RequiredAction::keycloak).collect::<Vec<_>>(),
                "sendEmail": cmd.send_email,
            }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(link.into()))
}

/// Re-queues a `failed` / `conflict` identity after the cause was fixed.
#[utoipa::path(
    post,
    path = "/api/v1/platform/identities/{id}/retry",
    operation_id = "platform_identity_retry",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = IdentityView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn retry(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<IdentityView>> {
    let ctx = auth.require(Permission::IdentityManage)?;
    st.idp()?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let link = found(&mut tx, ctx, id).await?;
    let link = requeue(&mut tx, link.id).await?;
    audit::insert(
        &mut *tx,
        &user_audit(
            ctx,
            "platform.identity.retry",
            link.id,
            serde_json::json!({}),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(link.into()))
}

async fn set_hold(
    st: &IdentityState,
    auth: &Authenticated,
    id: Uuid,
    hold: bool,
    why: &str,
) -> ApiResult<Json<IdentityView>> {
    let ctx = auth.require(Permission::IdentityLifecycle)?;
    st.idp()?;
    let why = reason(why, "reason")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let link = found(&mut tx, ctx, id).await?;
    not_self(ctx, &link, if hold { "suspend" } else { "re-enable" })?;
    if link.admin_hold == hold {
        return Err(KernelError::Conflict(if hold {
            "the identity is already suspended by an administrator".into()
        } else {
            "the identity has no administrative suspension".into()
        })
        .into());
    }
    let erp = AccountState::parse(&link.erp_state).unwrap_or(AccountState::Disabled);
    let desired = super::desired_state(erp, hold);
    sqlx::query(
        "UPDATE platform.identity_link SET admin_hold = $2, hold_reason = $3, desired_state = $4 \
         WHERE id = $1",
    )
    .bind(link.id)
    .bind(hold)
    .bind(hold.then_some(why.as_str()))
    .bind(desired.as_str())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let link = requeue(&mut tx, link.id).await?;
    audit::insert(
        &mut *tx,
        &user_audit(
            ctx,
            if hold {
                "platform.identity.suspend"
            } else {
                "platform.identity.reactivate"
            },
            link.id,
            serde_json::json!({ "reason": why, "desiredState": desired.as_str(), "erpState": link.erp_state }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(link.into()))
}

/// Administrative suspension: the account is disabled and every session (IdP and SutraERP) is
/// ended at the next sync, whatever the ERP record says.
#[utoipa::path(
    post,
    path = "/api/v1/platform/identities/{id}/suspend",
    operation_id = "platform_identity_suspend",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    request_body = HoldRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = IdentityView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing permission / step-up, or your own identity", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn suspend(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<HoldRequest>,
) -> ApiResult<Json<IdentityView>> {
    set_hold(&st, &auth, id, true, &cmd.reason).await
}

/// Lifts an administrative suspension. The account is re-enabled only when the ERP record is
/// active too (a suspended student stays suspended).
#[utoipa::path(
    post,
    path = "/api/v1/platform/identities/{id}/reactivate",
    operation_id = "platform_identity_reactivate",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    request_body = HoldRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = IdentityView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn reactivate(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<HoldRequest>,
) -> ApiResult<Json<IdentityView>> {
    set_hold(&st, &auth, id, false, &cmd.reason).await
}

/// Requests role changes. Revocations and non-privileged grants apply at once; a grant of a
/// privileged role (administrator, approver roles) waits for another administrator's decision.
#[utoipa::path(
    post,
    path = "/api/v1/platform/identities/{id}/role-requests",
    operation_id = "platform_identity_role_request_create",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    request_body = RoleChangeRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = RoleRequestView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing permission, or your own identity", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "A request is already pending", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn request_roles(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<RoleChangeRequest>,
) -> ApiResult<(StatusCode, Json<RoleRequestView>)> {
    let ctx = auth.require(Permission::IdentityManage)?;
    st.idp()?;
    let grant = roles_of(&cmd.grant, "grant")?;
    let revoke = roles_of(&cmd.revoke, "revoke")?;
    let why = reason(&cmd.reason, "reason")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let link = found(&mut tx, ctx, id).await?;
    not_self(ctx, &link, "change the roles of")?;
    let (view, _) = create_role_request(&mut tx, ctx, &link, &grant, &revoke, &why).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Role requests of the session tenant (newest first).
#[utoipa::path(
    get,
    path = "/api/v1/platform/identity-role-requests",
    operation_id = "platform_identity_role_request_list",
    tag = "platform-identity",
    params(RoleRequestQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RoleRequestPage),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_role_requests(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Query(q): Query<RoleRequestQuery>,
) -> ApiResult<Json<RoleRequestPage>> {
    let ctx = auth.require(Permission::IdentityRead)?;
    let rows = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM platform.identity_role_request WHERE tenant_id = $1 \
           AND ($2::text IS NULL OR status = $2) ORDER BY created_at DESC, id DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(q.status.as_deref())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(request_view)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(RoleRequestPage { items }))
}

/// Decides a pending privileged role request. The decider must hold
/// `platform.identity.role.approve`, must not be the requester (separation of duties) and must
/// not be the identity concerned. Approval applies the roles (and triggers TOTP enrolment) at the
/// next sync.
#[utoipa::path(
    post,
    path = "/api/v1/platform/identity-role-requests/{id}/decision",
    operation_id = "platform_identity_role_request_decision",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    request_body = RoleDecision,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = RoleRequestView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing permission / step-up, the requester, or your own identity", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Not pending", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn role_request_decision(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<RoleDecision>,
) -> ApiResult<Json<RoleRequestView>> {
    let ctx = auth.require(Permission::IdentityRoleApprove)?;
    st.idp()?;
    let approve = match cmd.decision.trim() {
        "approve" => true,
        "reject" => false,
        _ => return Err(invalid("decision", "enum", "decision must be approve or reject").into()),
    };
    let why = reason(&cmd.reason, "reason")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM platform.identity_role_request \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "role request",
    })?;
    let req = request_view(&row).map_err(map_db_error)?;
    if req.status != "pending" {
        return Err(KernelError::Conflict(format!("the role request is {}", req.status)).into());
    }
    ensure_independent_decider(
        Some(ctx.subject.as_str()),
        [req.requested_by.as_str()],
        "identity role",
    )?;
    let link = found(&mut tx, ctx, req.identity_id).await?;
    not_self(ctx, &link, "decide role requests for")?;
    let row = sqlx::query(&format!(
        "UPDATE platform.identity_role_request SET status = $2, decided_by = $3, \
           decided_at = now(), decision_reason = $4 WHERE id = $1 RETURNING {REQUEST_COLUMNS}"
    ))
    .bind(id)
    .bind(if approve { "approved" } else { "rejected" })
    .bind(&ctx.subject)
    .bind(&why)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let view = request_view(&row).map_err(map_db_error)?;
    if approve {
        apply_roles(&mut tx, &link, &view.grant_roles, &view.revoke_roles).await?;
    }
    audit::insert(
        &mut *tx,
        &user_audit(
            ctx,
            if approve {
                "platform.identity.role_request.approved"
            } else {
                "platform.identity.role_request.rejected"
            },
            link.id,
            serde_json::json!({
                "requestId": view.id, "grant": view.grant_roles, "revoke": view.revoke_roles,
                "requestedBy": view.requested_by, "reason": why,
            }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}

fn mapping_view(r: &sqlx::postgres::PgRow) -> Result<RoleMappingView, sqlx::Error> {
    Ok(RoleMappingView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        erp_role: r.try_get("erp_role")?,
        target_kind: r.try_get("target_kind")?,
        target: r.try_get("target")?,
        client_id: r.try_get("client_id")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
    })
}

const MAPPING_COLUMNS: &str =
    "id, institution_id, erp_role, target_kind, target, client_id, created_by, created_at";

/// ERP-role → IdP mappings of the session tenant.
#[utoipa::path(
    get,
    path = "/api/v1/platform/identity-role-mappings",
    operation_id = "platform_identity_role_mapping_list",
    tag = "platform-identity",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RoleMappingPage),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_mappings(
    State(st): State<IdentityState>,
    auth: Authenticated,
) -> ApiResult<Json<RoleMappingPage>> {
    let ctx = auth.require(Permission::IdentityRead)?;
    let rows = sqlx::query(&format!(
        "SELECT {MAPPING_COLUMNS} FROM platform.identity_role_mapping WHERE tenant_id = $1 \
           AND retired_at IS NULL ORDER BY erp_role, target_kind, target"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(mapping_view)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(RoleMappingPage { items }))
}

/// Re-queues every identity of the tenant (a mapping change applies to all of them).
async fn requeue_tenant(conn: &mut PgConnection, tenant: Uuid) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE platform.identity_link SET sync_status = 'pending', attempts = 0, \
           next_attempt_at = now(), last_error = NULL, updated_at = now() \
         WHERE tenant_id = $1 AND idp_user_id IS NOT NULL AND sync_status <> 'conflict'",
    )
    .bind(tenant)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Adds an IdP target (realm role, client role or group) to an ERP role, optionally for one
/// institution. SutraERP's own role names and tenant groups are refused as targets (they are
/// managed 1:1 and carry the tenant scope).
#[utoipa::path(
    post,
    path = "/api/v1/platform/identity-role-mappings",
    operation_id = "platform_identity_role_mapping_create",
    tag = "platform-identity",
    request_body = NewRoleMapping,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = RoleMappingView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Mapping exists", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_mapping(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Json(cmd): Json<NewRoleMapping>,
) -> ApiResult<(StatusCode, Json<RoleMappingView>)> {
    let ctx = auth.require(Permission::IdentityRoleApprove)?;
    let role = Role::parse(cmd.erp_role.trim())
        .ok_or_else(|| invalid("erpRole", "unknown", "unknown SutraERP role"))?;
    let kind = cmd.target_kind.trim();
    let target = cmd.target.trim().to_owned();
    if target.is_empty() || target.len() > 255 {
        return Err(invalid("target", "length", "target must be 1–255 characters").into());
    }
    match kind {
        "realm_role" if Role::parse(&target).is_some() => {
            return Err(invalid(
                "target",
                "reserved",
                "SutraERP roles are mapped 1:1 and cannot be a mapping target",
            )
            .into())
        }
        "group" if !target.starts_with('/') || target.starts_with("/sutra-tenant-") => {
            return Err(invalid(
                "target",
                "group",
                "a group target is a full path and never a SutraERP tenant group",
            )
            .into())
        }
        "client_role" if cmd.client_id.as_deref().is_none_or(|c| c.trim().is_empty()) => {
            return Err(invalid(
                "clientId",
                "required",
                "clientId is required for client_role",
            )
            .into())
        }
        "realm_role" | "group" | "client_role" => {}
        _ => return Err(invalid("targetKind", "enum", "realm_role, client_role or group").into()),
    }
    let client = if kind == "client_role" {
        cmd.client_id.as_deref().map(|c| c.trim().to_owned())
    } else {
        None
    };
    let tenant = ctx.scope.tenant_id.as_uuid();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "INSERT INTO platform.identity_role_mapping \
           (tenant_id, institution_id, erp_role, target_kind, target, client_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {MAPPING_COLUMNS}"
    ))
    .bind(tenant)
    .bind(cmd.institution_id)
    .bind(role.as_str())
    .bind(kind)
    .bind(&target)
    .bind(&client)
    .bind(&ctx.subject)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let view = mapping_view(&row).map_err(map_db_error)?;
    requeue_tenant(&mut tx, tenant).await?;
    audit::insert(
        &mut *tx,
        &AuditEntry::new(
            ctx.scope,
            "platform.identity.mapping.created",
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::User, Some(ctx.subject.clone()))
        .target("platform.identity_role_mapping", view.id.to_string())
        .details(serde_json::json!({
            "erpRole": view.erp_role, "targetKind": view.target_kind, "target": view.target,
            "clientId": view.client_id, "institutionId": view.institution_id,
        })),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Retires a mapping (the target is removed from every identity at the next sync).
#[utoipa::path(
    delete,
    path = "/api/v1/platform/identity-role-mappings/{id}",
    operation_id = "platform_identity_role_mapping_delete",
    tag = "platform-identity",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 204, description = "Deleted"),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn delete_mapping(
    State(st): State<IdentityState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<StatusCode> {
    let ctx = auth.require(Permission::IdentityRoleApprove)?;
    let tenant = ctx.scope.tenant_id.as_uuid();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "UPDATE platform.identity_role_mapping SET retired_at = now(), retired_by = $3 \
         WHERE tenant_id = $1 AND id = $2 AND retired_at IS NULL RETURNING {MAPPING_COLUMNS}"
    ))
    .bind(tenant)
    .bind(id)
    .bind(&ctx.subject)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "role mapping",
    })?;
    let view = mapping_view(&row).map_err(map_db_error)?;
    // The retired target stays managed, so the re-sync takes it away from every identity.
    requeue_tenant(&mut tx, tenant).await?;
    audit::insert(
        &mut *tx,
        &AuditEntry::new(
            ctx.scope,
            "platform.identity.mapping.deleted",
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::User, Some(ctx.subject.clone()))
        .target("platform.identity_role_mapping", view.id.to_string())
        .details(serde_json::json!({
            "erpRole": view.erp_role, "targetKind": view.target_kind, "target": view.target,
        })),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roles_and_reasons_are_validated() {
        assert!(
            roles_of(&["registrar".into(), "registrar".into()], "r").is_ok_and(|v| v.len() == 1)
        );
        assert!(roles_of(&["root".into()], "r").is_err());
        assert!(reason("  ", "reason").is_err());
        assert_eq!(reason(" ok ", "reason").ok().as_deref(), Some("ok"));
    }
}
