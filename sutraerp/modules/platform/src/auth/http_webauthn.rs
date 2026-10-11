//! WebAuthn (passkey / security key) endpoints of the auth boundary — dev mode second factor.
//!
//! | Endpoint | Protection | Purpose |
//! |----------|------------|---------|
//! | `POST /api/v1/auth/mfa/webauthn/challenge` | pre-session CSRF + MFA challenge | passkey option of a sign-in challenge |
//! | `POST /api/v1/auth/mfa/webauthn/verify` | pre-session CSRF + MFA challenge | finish sign-in with a passkey |
//! | `POST /api/v1/auth/mfa/webauthn/registration` | session + CSRF | start registering a passkey |
//! | `POST /api/v1/auth/mfa/webauthn/registration/complete` | session + CSRF | store it; session → `mfa` |
//! | `POST /api/v1/auth/mfa/webauthn/assertion` | session + CSRF | start a step-up assertion |
//! | `POST /api/v1/auth/mfa/webauthn/step-up` | session + CSRF | finish step-up; session → `mfa` |
//! | `DELETE /api/v1/auth/mfa/webauthn/credentials/{id}` | session + CSRF + MFA session | remove a passkey |
//!
//! In Keycloak mode every ceremony answers `501` (passkeys are a Keycloak required action).

use super::http::{
    audit, browser_binding, elevate, enrolled_factors, establish, no_store, AuthState,
    MfaChallengePayload, MFA_CHALLENGE_TTL_SECS,
};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use chrono::Duration;
use sutra_contracts::platform::auth::{
    MfaLevel, NewSession, PasskeyInfo, PreAuthKind, Principal, WebauthnCeremony,
};
use sutra_kernel::audit::AuditOutcome;
use sutra_kernel::error::KernelError;
use sutra_kernel::secret;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Pre-session request naming the sign-in challenge.
#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebauthnChallengeRequest {
    pub challenge_id: String,
}

/// Sign-in challenge after the passkey option was started: use the **new** `challengeId` for
/// both `…/webauthn/verify` and the TOTP `…/mfa/verify` (the previous one was consumed).
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebauthnChallengeResponse {
    pub challenge_id: String,
    /// `PublicKeyCredentialRequestOptions` (JSON form, under `publicKey`).
    #[schema(value_type = Object)]
    pub options: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebauthnVerifyRequest {
    pub challenge_id: String,
    /// `PublicKeyCredential` from `navigator.credentials.get` (binary members base64url).
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebauthnRegistrationCompleteRequest {
    pub ceremony_id: String,
    /// `PublicKeyCredential` from `navigator.credentials.create` (binary members base64url).
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
    /// Name shown in the security page (defaults to "Passkey").
    #[serde(default)]
    pub label: String,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebauthnAssertionRequest {
    pub ceremony_id: String,
    #[schema(value_type = Object)]
    pub credential: serde_json::Value,
}

fn webauthn_enabled(state: &AuthState) -> Result<(), KernelError> {
    if state.mfa.is_some() && state.webauthn.enabled() {
        Ok(())
    } else {
        Err(KernelError::NotImplemented(
            "passkeys are managed by Keycloak in this mode",
        ))
    }
}

/// Consumes the sign-in challenge after checking the browser binding.
async fn take_challenge(
    state: &AuthState,
    headers: &HeaderMap,
    challenge_id: &str,
) -> Result<MfaChallengePayload, KernelError> {
    let raw = state
        .pre_auth
        .take(PreAuthKind::MfaChallenge, challenge_id.trim())
        .await?
        .ok_or(KernelError::Unauthenticated)?;
    let payload: MfaChallengePayload =
        serde_json::from_value(raw).map_err(KernelError::internal)?;
    if !secret::ct_eq(
        browser_binding(headers).as_bytes(),
        payload.binding.as_bytes(),
    ) {
        tracing::warn!("MFA challenge presented by another browser");
        return Err(KernelError::Unauthenticated);
    }
    Ok(payload)
}

/// Start the passkey option of a sign-in challenge.
///
/// Consumes `challengeId` and returns a **new** one (carrying the WebAuthn ceremony) together
/// with the assertion options for `navigator.credentials.get`. The new id also still accepts a
/// TOTP code at `/api/v1/auth/mfa/verify`. Requires the pre-session CSRF token the challenge
/// was issued to.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/webauthn/challenge",
    tag = "auth",
    request_body = WebauthnChallengeRequest,
    security(("login_csrf" = [])),
    responses(
        (status = 200, body = WebauthnChallengeResponse),
        (status = 401, description = "Unknown/expired challenge or another browser", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "No passkey registered", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "Passkeys are managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn webauthn_challenge(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(req): Json<WebauthnChallengeRequest>,
) -> ApiResult<Response> {
    webauthn_enabled(&state)?;
    let mut payload = take_challenge(&state, &headers, &req.challenge_id).await?;
    let ceremony = state
        .webauthn
        .start_authentication(&payload.principal)
        .await?;
    payload.webauthn_ceremony = Some(ceremony.ceremony_id);
    let challenge_id = state
        .pre_auth
        .put(
            PreAuthKind::MfaChallenge,
            serde_json::to_value(&payload).map_err(KernelError::internal)?,
            Duration::seconds(MFA_CHALLENGE_TTL_SECS),
        )
        .await?;
    let mut res = Json(WebauthnChallengeResponse {
        challenge_id,
        options: ceremony.options,
    })
    .into_response();
    no_store(&mut res);
    Ok(res)
}

/// Finish a sign-in challenge with a passkey.
///
/// Single use like the TOTP path: a failed assertion burns the challenge. On success the
/// session is created with `mfaLevel = mfa` (`amr` = `pwd`, `hwk`, `mfa`).
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/webauthn/verify",
    tag = "auth",
    request_body = WebauthnVerifyRequest,
    security(("login_csrf" = [])),
    responses(
        (status = 200, body = super::http::LoginResponse),
        (status = 401, description = "Unknown challenge, wrong browser, or assertion rejected", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "Passkeys are managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn webauthn_verify(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(req): Json<WebauthnVerifyRequest>,
) -> ApiResult<Response> {
    webauthn_enabled(&state)?;
    let payload = take_challenge(&state, &headers, &req.challenge_id).await?;
    let ceremony = payload
        .webauthn_ceremony
        .as_deref()
        .ok_or(KernelError::Unauthenticated)?;
    if let Err(e) = state
        .webauthn
        .finish_authentication(&payload.principal, ceremony, req.credential)
        .await
    {
        audit(
            &state,
            &payload.principal,
            "platform.auth.mfa.verify",
            AuditOutcome::Failed,
            None,
        )
        .await;
        return Err(e.into());
    }
    let mut new = NewSession::password_and_totp(payload.principal);
    new.amr = vec!["pwd".into(), "hwk".into(), "mfa".into()];
    establish(&state, &headers, new, "platform.auth.login").await
}

/// Begin registering a passkey for the signed-in account.
///
/// Returns the creation options for `navigator.credentials.create` and a `ceremonyId`.
/// Adding a passkey to an account that already has a second factor needs an MFA-verified
/// session (`403 mfa-step-up-required`).
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/webauthn/registration",
    tag = "auth",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = WebauthnCeremony),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Step-up required", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Too many passkeys", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "Passkeys are managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn webauthn_registration(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Response> {
    webauthn_enabled(&state)?;
    let p = Principal::from(&ctx);
    let (totp, passkey) = enrolled_factors(&state, &p).await?;
    if (totp || passkey) && ctx.mfa_level < MfaLevel::Mfa {
        return Err(KernelError::StepUpRequired("mfa.webauthn.register").into());
    }
    let mut res = Json(state.webauthn.start_registration(&p).await?).into_response();
    no_store(&mut res);
    Ok(res)
}

/// Finish passkey registration; the session is rotated to `mfaLevel = mfa`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/webauthn/registration/complete",
    tag = "auth",
    request_body = WebauthnRegistrationCompleteRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = super::http::LoginResponse),
        (status = 401, description = "Unknown or foreign ceremony", body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Authenticator already registered", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Attestation could not be verified", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn webauthn_registration_complete(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Authenticated(ctx): Authenticated,
    Json(req): Json<WebauthnRegistrationCompleteRequest>,
) -> ApiResult<Response> {
    webauthn_enabled(&state)?;
    let p = Principal::from(&ctx);
    let info = state
        .webauthn
        .finish_registration(&p, &req.ceremony_id, req.credential, &req.label)
        .await?;
    audit(
        &state,
        &p,
        "platform.auth.mfa.webauthn.registered",
        AuditOutcome::Succeeded,
        Some(info.id.to_string()),
    )
    .await;
    elevate(&state, &headers, &ctx, "hwk", "platform.auth.mfa.enrolled").await
}

/// Begin a passkey assertion to step up the current session.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/webauthn/assertion",
    tag = "auth",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = WebauthnCeremony),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "No passkey registered", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "Passkeys are managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn webauthn_assertion(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Response> {
    webauthn_enabled(&state)?;
    let mut res = Json(
        state
            .webauthn
            .start_authentication(&Principal::from(&ctx))
            .await?,
    )
    .into_response();
    no_store(&mut res);
    Ok(res)
}

/// Finish a passkey step-up; the session is rotated to `mfaLevel = mfa` (absolute expiry kept).
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/webauthn/step-up",
    tag = "auth",
    request_body = WebauthnAssertionRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = super::http::LoginResponse),
        (status = 401, description = "Assertion rejected", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "Passkeys are managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn webauthn_step_up(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Authenticated(ctx): Authenticated,
    Json(req): Json<WebauthnAssertionRequest>,
) -> ApiResult<Response> {
    webauthn_enabled(&state)?;
    let p = Principal::from(&ctx);
    if let Err(e) = state
        .webauthn
        .finish_authentication(&p, &req.ceremony_id, req.credential)
        .await
    {
        audit(
            &state,
            &p,
            "platform.auth.mfa.step_up",
            AuditOutcome::Failed,
            Some(ctx.session_id.to_string()),
        )
        .await;
        return Err(e.into());
    }
    elevate(&state, &headers, &ctx, "hwk", "platform.auth.mfa.step_up").await
}

/// Remove one of the caller's passkeys (needs an MFA-verified session). Removing the last
/// second factor of an account whose role requires MFA is refused (`409`).
#[utoipa::path(
    delete,
    path = "/api/v1/auth/mfa/webauthn/credentials/{id}",
    tag = "auth",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 204, description = "Removed"),
        (status = 403, description = "Step-up required", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Last factor of an account that requires MFA", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn webauthn_remove(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    webauthn_enabled(&state)?;
    if ctx.mfa_level < MfaLevel::Mfa {
        return Err(KernelError::StepUpRequired("mfa.webauthn.remove").into());
    }
    let p = Principal::from(&ctx);
    let creds: Vec<PasskeyInfo> = state.webauthn.credentials(&p).await?;
    if !creds.iter().any(|c| c.id == id) {
        return Err(KernelError::NotFound { entity: "passkey" }.into());
    }
    let (totp, _) = enrolled_factors(&state, &p).await?;
    if state.mfa_policy.requires_mfa(&p) && !totp && creds.len() == 1 {
        return Err(KernelError::Conflict(
            "your role requires a second factor: add another one before removing this passkey"
                .into(),
        )
        .into());
    }
    if !state.webauthn.remove(&p, id).await? {
        return Err(KernelError::NotFound { entity: "passkey" }.into());
    }
    audit(
        &state,
        &p,
        "platform.auth.mfa.webauthn.removed",
        AuditOutcome::Succeeded,
        Some(id.to_string()),
    )
    .await;
    Ok(StatusCode::NO_CONTENT.into_response())
}

pub fn routes() -> OpenApiRouter<AuthState> {
    OpenApiRouter::new()
        .routes(routes!(webauthn_challenge))
        .routes(routes!(webauthn_verify))
        .routes(routes!(webauthn_registration))
        .routes(routes!(webauthn_registration_complete))
        .routes(routes!(webauthn_assertion))
        .routes(routes!(webauthn_step_up))
        .routes(routes!(webauthn_remove))
}
