//! Self-service security endpoints (profile → Security page) and the OIDC back-channel logout.
//!
//! | Endpoint | Protection |
//! |----------|------------|
//! | `GET /api/v1/auth/mfa` | session |
//! | `DELETE /api/v1/auth/mfa/totp` | session + CSRF + MFA-verified session |
//! | `GET /api/v1/auth/sessions` | session |
//! | `POST /api/v1/auth/sessions/revoke-others` | session + CSRF |
//! | `DELETE /api/v1/auth/sessions/{id}` | session + CSRF |
//! | `POST /api/v1/auth/oidc/backchannel-logout` | signed logout token (Keycloak → API, no cookie) |
//! | `POST /api/v1/auth/lockouts/unlock` | administrator (`platform.identity.lifecycle`, step-up): clear an account's sign-in lockouts (audit H-3) |

use super::http::{audit, enrolled_factors, mfa_service, no_store, AuthState};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::{DateTime, Utc};
use sutra_contracts::platform::auth::{MfaLevel, PasskeyInfo, Principal};
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_web::request_id::current_correlation_id;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Who manages the account's second factors.
#[derive(Debug, Clone, Copy, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum FactorAuthority {
    /// SutraERP (dev auth mode): TOTP + passkeys on the Security page.
    Sutra,
    /// Keycloak (OIDC mode): factors are managed in the Keycloak account console.
    Keycloak,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MfaStatus {
    pub managed_by: FactorAuthority,
    /// An authenticator app (TOTP) is active.
    pub totp_enrolled: bool,
    /// Registered passkeys / security keys.
    pub passkeys: Vec<PasskeyInfo>,
    /// Dev-mode WebAuthn is available in this deployment.
    pub webauthn_available: bool,
    /// The account's role requires a second factor.
    pub mfa_required: bool,
    /// Assurance of the current session.
    pub session_mfa_level: MfaLevel,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub idle_expires_at: DateTime<Utc>,
    pub mfa_level: MfaLevel,
    pub amr: Vec<String>,
    /// Signed in through single sign-on (Keycloak).
    pub federated: bool,
    /// The session making this request.
    pub current: bool,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionList {
    pub items: Vec<SessionView>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RevokedCount {
    pub revoked: u64,
}

/// Second-factor status of the signed-in account.
#[utoipa::path(
    get,
    path = "/api/v1/auth/mfa",
    tag = "auth",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = MfaStatus),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn mfa_status(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Response> {
    let p = Principal::from(&ctx);
    let (totp, _) = enrolled_factors(&state, &p).await?;
    let passkeys = if state.mfa.is_some() {
        state.webauthn.credentials(&p).await?
    } else {
        vec![]
    };
    let mut res = Json(MfaStatus {
        managed_by: if state.mfa.is_some() {
            FactorAuthority::Sutra
        } else {
            FactorAuthority::Keycloak
        },
        totp_enrolled: totp,
        passkeys,
        webauthn_available: state.mfa.is_some() && state.webauthn.enabled(),
        mfa_required: state.mfa_policy.requires_mfa(&p),
        session_mfa_level: ctx.mfa_level,
    })
    .into_response();
    no_store(&mut res);
    Ok(res)
}

/// Turn off the authenticator app (needs an MFA-verified session).
///
/// Refused (`409`) when it is the last second factor of an account whose role requires MFA.
#[utoipa::path(
    delete,
    path = "/api/v1/auth/mfa/totp",
    tag = "auth",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 204, description = "Authenticator removed"),
        (status = 403, description = "Step-up required", body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "No authenticator enrolled", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Last factor of an account that requires MFA", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "MFA is managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn totp_disable(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Response> {
    let mfa = mfa_service(&state)?;
    if ctx.mfa_level < MfaLevel::Mfa {
        return Err(KernelError::StepUpRequired("mfa.totp.disable").into());
    }
    let p = Principal::from(&ctx);
    let (totp, passkey) = enrolled_factors(&state, &p).await?;
    if !totp {
        return Err(KernelError::NotFound {
            entity: "authenticator",
        }
        .into());
    }
    if state.mfa_policy.requires_mfa(&p) && !passkey {
        return Err(KernelError::Conflict(
            "your role requires a second factor: add a passkey before removing the authenticator"
                .into(),
        )
        .into());
    }
    mfa.disable_totp(&p).await?;
    audit(
        &state,
        &p,
        "platform.auth.mfa.totp.disabled",
        AuditOutcome::Succeeded,
        Some(ctx.session_id.to_string()),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Active sessions of the signed-in account (newest first; the current one is flagged).
#[utoipa::path(
    get,
    path = "/api/v1/auth/sessions",
    tag = "auth",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = SessionList),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn sessions(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Response> {
    let items = state
        .sessions
        .list_for_subject(&ctx.scope, &ctx.subject)
        .await?
        .into_iter()
        .map(|s| SessionView {
            current: s.id == ctx.session_id,
            id: s.id,
            created_at: s.created_at,
            last_seen_at: s.last_seen_at,
            expires_at: s.expires_at,
            idle_expires_at: s.idle_expires_at,
            mfa_level: s.mfa_level,
            amr: s.amr,
            federated: s.federated,
        })
        .collect();
    let mut res = Json(SessionList { items }).into_response();
    no_store(&mut res);
    Ok(res)
}

/// Sign out every other session of this account ("sign out other devices").
#[utoipa::path(
    post,
    path = "/api/v1/auth/sessions/revoke-others",
    tag = "auth",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = RevokedCount),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "CSRF failure", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revoke_other_sessions(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Json<RevokedCount>> {
    let revoked = state
        .sessions
        .revoke_others(&ctx.scope, &ctx.subject, ctx.session_id)
        .await?;
    audit(
        &state,
        &Principal::from(&ctx),
        "platform.auth.sessions.revoke_others",
        AuditOutcome::Succeeded,
        Some(ctx.session_id.to_string()),
    )
    .await;
    Ok(Json(RevokedCount { revoked }))
}

/// Sign out one other session of this account. The current session ends with `POST /logout`.
#[utoipa::path(
    delete,
    path = "/api/v1/auth/sessions/{id}",
    tag = "auth",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 204, description = "Session revoked"),
        (status = 404, description = "Not a live session of this account", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "That is the current session (use logout)", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn revoke_session(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    if id == ctx.session_id {
        return Err(
            KernelError::Conflict("this is the current session: sign out instead".into()).into(),
        );
    }
    if !state
        .sessions
        .revoke_id(&ctx.scope, &ctx.subject, id)
        .await?
    {
        return Err(KernelError::NotFound { entity: "session" }.into());
    }
    audit(
        &state,
        &Principal::from(&ctx),
        "platform.auth.session.revoke",
        AuditOutcome::Succeeded,
        Some(id.to_string()),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Namespace of logout-token ids in the replay guard.
const LOGOUT_JTI_NAMESPACE: &str = "oidc.backchannel-logout.jti";

/// OIDC Back-Channel Logout 1.0 receiver (Keycloak → SutraERP, server to server).
///
/// Body: `application/x-www-form-urlencoded` with `logout_token`. The token is validated
/// (JWKS signature, `iss`, `aud`, `iat`/`exp`, back-channel-logout `events`, `sid`/`sub`, no
/// `nonce`), its `jti` must not have been seen before, and then every SutraERP session bound
/// to the IdP session `sid` (or, without `sid`, every federated session of `sub`) is revoked.
/// Answers `200` (also when no session matched) or `400`; never sets cookies.
#[utoipa::path(
    post,
    path = "/api/v1/auth/oidc/backchannel-logout",
    tag = "auth",
    request_body(content = String, content_type = "application/x-www-form-urlencoded", description = "logout_token=<JWT>"),
    responses(
        (status = 200, description = "Logout token accepted; matching sessions revoked"),
        (status = 400, description = "Missing, invalid or replayed logout token", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "OIDC not configured (dev mode)", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Identity provider keys unavailable", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn backchannel_logout(
    State(state): State<AuthState>,
    body: Bytes,
) -> ApiResult<Response> {
    let oidc = state.oidc.as_ref().ok_or(KernelError::NotImplemented(
        "back-channel logout is only available with SUTRA_AUTH_MODE=keycloak",
    ))?;
    let token = url::form_urlencoded::parse(&body)
        .find(|(k, _)| k == "logout_token")
        .map(|(_, v)| v.into_owned())
        .filter(|t| !t.is_empty())
        .ok_or_else(|| KernelError::BadRequest("logout_token is required".into()))?;
    let claims = oidc.validate_logout_token(&token).await?;
    let ttl = (claims.replayable_until - Utc::now()).max(chrono::Duration::seconds(60));
    if !state
        .replay
        .first_use(LOGOUT_JTI_NAMESPACE, &claims.jti, ttl)
        .await?
    {
        tracing::warn!(jti = %claims.jti, "replayed logout token refused");
        return Err(KernelError::BadRequest("logout token was already used".into()).into());
    }
    let revoked = state
        .sessions
        .revoke_federated(claims.sid.as_deref(), claims.sub.as_deref())
        .await?;
    tracing::info!(
        sid = ?claims.sid,
        sub = ?claims.sub,
        revoked = revoked.len(),
        "back-channel logout"
    );
    for r in &revoked {
        let e = AuditEntry::new(
            r.scope,
            "platform.auth.backchannel_logout",
            AuditOutcome::Succeeded,
        )
        .actor(ActorKind::System, Some("keycloak".into()))
        .target("session", r.id.to_string())
        .correlation(current_correlation_id())
        .details(serde_json::json!({ "subject": r.subject, "sid": claims.sid }));
        let _ = state.audit.record(e).await;
    }
    let mut res = StatusCode::OK.into_response();
    no_store(&mut res);
    Ok(res)
}

/// Body of `POST /api/v1/auth/lockouts/unlock`.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AuthLockoutUnlockRequest {
    /// Login id whose sign-in lockouts (from every client address) are cleared.
    pub login_id: String,
    /// Why the account is being unlocked (recorded in the audit trail).
    pub reason: String,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AuthLockoutUnlockResult {
    /// Client-address lockouts / failure counters cleared on this API replica.
    pub cleared: usize,
}

/// Clear the sign-in lockouts of an account (audit H-3).
///
/// Failed sign-ins lock a login id only for the client address they came from; an administrator
/// holding `platform.identity.lifecycle` (step-up) clears every address's lockout of that login id
/// here, e.g. after a lockout reported by its owner. The counter is per API replica: call once per
/// replica or wait for the lockout to expire. Audited as `platform.auth.lockout.unlock`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/lockouts/unlock",
    tag = "auth",
    request_body = AuthLockoutUnlockRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, description = "Lockouts cleared", body = AuthLockoutUnlockResult),
        (status = 401, description = "No session", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing permission or step-up", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Validation failed", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn unlock_login(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
    Json(req): Json<AuthLockoutUnlockRequest>,
) -> ApiResult<Json<AuthLockoutUnlockResult>> {
    // Permission first: nothing is read or changed before the caller is authorised.
    ctx.require(Permission::IdentityLifecycle)?;
    let login_id = req.login_id.trim();
    let reason = req.reason.trim();
    let mut violations = vec![];
    if login_id.is_empty() || login_id.chars().count() > 254 {
        violations.push(FieldViolation::new(
            "loginId",
            "length",
            "login id is required (at most 254 characters)",
        ));
    }
    if reason.is_empty() || reason.chars().count() > 500 {
        violations.push(FieldViolation::new(
            "reason",
            "length",
            "reason is required (at most 500 characters)",
        ));
    }
    if !violations.is_empty() {
        return Err(KernelError::Validation(violations).into());
    }
    let cleared = state.throttle.unlock_login(login_id);
    tracing::info!(login = %login_id, cleared, "sign-in lockout cleared by administrator");
    let e = AuditEntry::new(
        ctx.scope,
        "platform.auth.lockout.unlock",
        AuditOutcome::Succeeded,
    )
    .actor(ActorKind::User, Some(ctx.subject.clone()))
    .target("login", login_id.to_lowercase())
    .correlation(current_correlation_id())
    .details(serde_json::json!({ "cleared": cleared, "reason": reason }));
    let _ = state.audit.record(e).await;
    Ok(Json(AuthLockoutUnlockResult { cleared }))
}

pub fn routes() -> OpenApiRouter<AuthState> {
    OpenApiRouter::new()
        .routes(routes!(mfa_status))
        .routes(routes!(totp_disable))
        .routes(routes!(sessions))
        .routes(routes!(revoke_other_sessions))
        .routes(routes!(revoke_session))
        .routes(routes!(backchannel_logout))
        .routes(routes!(unlock_login))
}
