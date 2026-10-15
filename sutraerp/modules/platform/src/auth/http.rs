//! HTTP surface of the auth boundary (D01).
//!
//! | Endpoint | Protection |
//! |----------|------------|
//! | `GET  /api/v1/auth/csrf` | issues the pre-session double-submit token |
//! | `POST /api/v1/auth/login`, `POST /api/v1/auth/mfa/verify` | pre-session CSRF token |
//! | `GET  /api/v1/auth/session` | session cookie |
//! | `POST /api/v1/auth/logout`, `…/mfa/totp/enrolment`, `…/mfa/totp/confirm`, `…/mfa/step-up` | session cookie + session CSRF token |
//! | `GET  /api/v1/auth/oidc/start` | none (starts a flow; state/nonce/PKCE stored server-side) |
//! | `GET  /api/v1/auth/oidc/callback` | single-use `state` + browser-binding cookie |
//!
//! Unsafe requests additionally pass the global cross-site `Origin` guard.

use super::keycloak::{pkce_challenge, pkce_verifier};
use super::mfa::MfaService;
use super::CookieSettings;
use crate::PlatformDeps;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, Duration, Utc};
use std::sync::Arc;
use sutra_contracts::platform::auth::{
    AuthorizationParams, CredentialAuthenticator, Credentials, MfaLevel, MfaMethod, MfaPolicy,
    NewSession, OidcIdentityProvider, PreAuthKind, PreAuthStore, Principal, ReplayGuard,
    SessionStore, WebauthnPort,
};
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome, AuditSink};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::secret;
use sutra_web::auth::{read_cookie, CSRF_HEADER};
use sutra_web::request_id::current_correlation_id;
use sutra_web::{ApiResult, Authenticated, PreSessionCsrf, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Lifetime of a pending MFA challenge.
pub(crate) const MFA_CHALLENGE_TTL_SECS: i64 = 300;
/// Lifetime of an OIDC authorization request (state / nonce / PKCE verifier).
const OIDC_REQUEST_TTL_SECS: i64 = 600;

#[derive(Clone)]
pub struct AuthState {
    pub authenticator: Arc<dyn CredentialAuthenticator>,
    pub sessions: Arc<dyn SessionStore>,
    pub audit: Arc<dyn AuditSink>,
    pub cookie: CookieSettings,
    pub mfa_policy: Arc<dyn MfaPolicy>,
    pub mfa: Option<MfaService>,
    pub pre_auth: Arc<dyn PreAuthStore>,
    pub oidc: Option<Arc<dyn OidcIdentityProvider>>,
    pub webauthn: Arc<dyn WebauthnPort>,
    pub pre_session: PreSessionCsrf,
    /// Single-use guard for back-channel logout token ids.
    pub replay: Arc<dyn ReplayGuard>,
    /// Refreshes the user's profile row at every sign-in (best effort; `None` without a DB).
    pub sign_in_hook: Option<Arc<dyn SignInHook>>,
}

/// Called after a session was established (profile refresh, last sign-in time).
#[async_trait::async_trait]
pub trait SignInHook: Send + Sync + 'static {
    async fn signed_in(&self, principal: &Principal) -> Result<(), KernelError>;
}

// ---------------------------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------------------------

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    /// Governed login identifier (e-mail in the dev realm).
    #[schema(example = "admin@sutra.dev")]
    pub login_id: String,
    #[schema(format = Password)]
    pub password: String,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MfaVerifyRequest {
    pub challenge_id: String,
    /// Six-digit TOTP code.
    #[schema(example = "123456")]
    pub code: String,
}

#[derive(Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TotpCodeRequest {
    /// Six-digit TOTP code from the authenticator app.
    #[schema(example = "123456")]
    pub code: String,
}

#[derive(Debug, Clone, Copy, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum LoginOutcome {
    Authenticated,
    MfaRequired,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LoginResponse {
    pub outcome: LoginOutcome,
    /// Present when `outcome = authenticated`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub principal: Option<Principal>,
    /// Present when `outcome = mfaRequired`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub challenge_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mfa_methods: Vec<MfaMethod>,
    /// Present when `outcome = authenticated`: the session's synchronizer CSRF token. Send it as
    /// `X-CSRF-Token` on every POST/PUT/PATCH/DELETE made with the session cookie.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub csrf_token: Option<String>,
    /// Present when `outcome = authenticated`: absolute session expiry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    /// Present when `outcome = authenticated`: assurance of the new session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mfa_level: Option<MfaLevel>,
    /// `true` when the account must enrol a second factor (`/api/v1/auth/mfa/totp/enrolment`);
    /// until then step-up-protected permissions are refused.
    pub mfa_enrollment_required: bool,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub principal: Principal,
    /// Absolute expiry (re-authentication required afterwards).
    pub expires_at: DateTime<Utc>,
    /// Expiry if the session stays idle from now on (sliding window).
    pub idle_expires_at: DateTime<Utc>,
    /// Synchronizer CSRF token bound to this session; send as `X-CSRF-Token` on unsafe requests.
    pub csrf_token: String,
    /// Assurance of the session: `mfa` once a second factor was verified.
    pub mfa_level: MfaLevel,
    /// Authentication methods references (`pwd`, `otp`, …).
    pub amr: Vec<String>,
    /// `true` when the account must enrol a second factor.
    pub mfa_enrollment_required: bool,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CsrfTokenResponse {
    /// Pre-session CSRF token; echo it as `X-CSRF-Token` on login and MFA verification. The
    /// same value is set in the `__Host-sutra_login_csrf` cookie (double submit).
    pub csrf_token: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TotpEnrolmentResponse {
    /// `otpauth://totp/…` key URI (render as a QR code).
    pub otpauth_uri: String,
    /// Base32 shared secret for manual entry.
    pub secret: String,
    pub digits: u32,
    pub period: i64,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LogoutResponse {
    /// Keycloak RP-initiated logout URL; navigate the browser there to end the SSO session.
    pub end_session_url: String,
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct OidcStartQuery {
    /// SPA path to land on after sign-in (relative, same origin). Defaults to `/`.
    pub return_to: Option<String>,
    /// Request a fresh multi-factor authentication (step-up).
    pub step_up: Option<bool>,
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct OidcCallbackQuery {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct MfaChallengePayload {
    pub(crate) principal: Principal,
    /// `SHA-256(pre-session CSRF token)` the challenge was issued to (browser binding).
    pub(crate) binding: String,
    /// WebAuthn authentication ceremony started for this challenge (passkey option).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) webauthn_ceremony: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct OidcRequestPayload {
    nonce: String,
    verifier: String,
    /// `SHA-256(browser-binding cookie)`.
    binding: String,
    return_to: String,
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

pub(crate) fn no_store(res: &mut Response) {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

pub(crate) fn add_cookie(res: &mut Response, cookie: &str) -> Result<(), KernelError> {
    res.headers_mut().append(
        header::SET_COOKIE,
        HeaderValue::from_str(cookie).map_err(KernelError::internal)?,
    );
    Ok(())
}

pub(crate) async fn audit(
    state: &AuthState,
    principal: &Principal,
    action: &'static str,
    outcome: AuditOutcome,
    target: Option<String>,
) {
    let mut e = AuditEntry::new(principal.scope, action, outcome)
        .actor(ActorKind::User, Some(principal.subject.clone()))
        .correlation(current_correlation_id());
    if let Some(t) = target {
        e = e.target("session", t);
    }
    let _ = state.audit.record(e).await;
}

/// Second factors the subject has enrolled with SutraERP (dev mode): `(totp, passkey)`.
pub(crate) async fn enrolled_factors(
    state: &AuthState,
    p: &Principal,
) -> Result<(bool, bool), KernelError> {
    let Some(mfa) = &state.mfa else {
        return Ok((false, false));
    };
    let totp = mfa.has_totp(p).await?;
    let passkey = state.webauthn.enabled() && !state.webauthn.credentials(p).await?.is_empty();
    Ok((totp, passkey))
}

/// Whether `p` must (still) enrol a second factor with SutraERP.
pub(crate) async fn enrollment_required(
    state: &AuthState,
    p: &Principal,
) -> Result<bool, KernelError> {
    if state.mfa.is_none() || !state.mfa_policy.requires_mfa(p) {
        return Ok(false);
    }
    let (totp, passkey) = enrolled_factors(state, p).await?;
    Ok(!totp && !passkey)
}

/// The pre-session CSRF token of this request, hashed: the browser binding of MFA challenges.
pub(crate) fn browser_binding(headers: &HeaderMap) -> String {
    let presented = headers
        .get(CSRF_HEADER)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .trim();
    secret::hex(&secret::sha256(presented.as_bytes()))
}

/// Mints a session (always a new token; any presented session is revoked — fixation defence),
/// sets the cookie and builds the authenticated response body.
async fn issue_session(
    state: &AuthState,
    headers: &HeaderMap,
    new: NewSession,
    action: &'static str,
) -> ApiResult<(Response, LoginResponse)> {
    if let Some(old) = read_cookie(headers, state.cookie.name()) {
        state.sessions.revoke(&old).await?;
    }
    let principal = new.principal.clone();
    let issued = state.sessions.create(new).await?;
    if let Some(hook) = &state.sign_in_hook {
        if let Err(e) = hook.signed_in(&principal).await {
            tracing::warn!(error = %e, "sign-in hook failed (profile refresh)");
        }
    }
    audit(
        state,
        &principal,
        action,
        AuditOutcome::Succeeded,
        Some(issued.session.id.to_string()),
    )
    .await;
    let body = LoginResponse {
        outcome: LoginOutcome::Authenticated,
        mfa_enrollment_required: enrollment_required(state, &principal).await?,
        principal: Some(principal),
        challenge_id: None,
        mfa_methods: vec![],
        csrf_token: Some(issued.session.csrf_token.clone()),
        expires_at: Some(issued.session.expires_at),
        mfa_level: Some(issued.session.mfa_level),
    };
    let mut res = StatusCode::OK.into_response();
    add_cookie(&mut res, &state.cookie.set_cookie(&issued.token))?;
    no_store(&mut res);
    Ok((res, body))
}

pub(crate) async fn establish(
    state: &AuthState,
    headers: &HeaderMap,
    new: NewSession,
    action: &'static str,
) -> ApiResult<Response> {
    let (res, body) = issue_session(state, headers, new, action).await?;
    let (parts, _) = res.into_parts();
    let mut out = Json(body).into_response();
    out.headers_mut().extend(parts.headers);
    Ok(out)
}

/// Rotates the caller's session into an MFA-verified one (new token + CSRF token, same
/// principal, original absolute expiry kept). `method` is the `amr` of the second factor
/// (`otp` for TOTP, `hwk` for WebAuthn).
pub(crate) async fn elevate(
    state: &AuthState,
    headers: &HeaderMap,
    ctx: &AuthContext,
    method: &'static str,
    action: &'static str,
) -> ApiResult<Response> {
    let mut new = NewSession::password_and_totp(Principal::from(ctx));
    let mut amr = ctx.amr.clone();
    for m in [method, "mfa"] {
        if !amr.iter().any(|a| a == m) {
            amr.push(m.into());
        }
    }
    new.amr = amr;
    new.not_after = Some(ctx.expires_at);
    establish(state, headers, new, action).await
}

pub(crate) fn mfa_service(state: &AuthState) -> Result<&MfaService, KernelError> {
    state.mfa.as_ref().ok_or(KernelError::NotImplemented(
        "second factors are managed by Keycloak in this mode",
    ))
}

pub(crate) fn require_code(code: &str) -> Result<(), KernelError> {
    let c = code.trim();
    if c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit()) {
        Ok(())
    } else {
        Err(KernelError::Validation(vec![FieldViolation::new(
            "code",
            "format",
            "a six-digit code is required",
        )]))
    }
}

/// Only same-origin absolute paths are accepted as post-login targets (no open redirect).
fn safe_return_to(r: Option<&str>) -> String {
    match r {
        Some(p)
            if p.starts_with('/')
                && !p.starts_with("//")
                && !p.contains('\\')
                && !p.chars().any(char::is_control) =>
        {
            p.to_owned()
        }
        _ => "/".to_owned(),
    }
}

fn redirect(to: &str) -> Response {
    let mut res = StatusCode::SEE_OTHER.into_response();
    if let Ok(v) = HeaderValue::from_str(to) {
        res.headers_mut().insert(header::LOCATION, v);
    }
    no_store(&mut res);
    res
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

/// Issue a pre-session (login) CSRF token.
///
/// Sets the `__Host-sutra_login_csrf` cookie and returns the same signed value; `POST /login`
/// and `POST /mfa/verify` must echo it in `X-CSRF-Token` (signed double submit).
#[utoipa::path(
    get,
    path = "/api/v1/auth/csrf",
    tag = "auth",
    responses((status = 200, body = CsrfTokenResponse))
)]
pub async fn csrf(State(state): State<AuthState>) -> ApiResult<Response> {
    let now = Utc::now();
    let token = state.pre_session.issue(now)?;
    let mut res = Json(CsrfTokenResponse {
        csrf_token: token.clone(),
        expires_at: now + Duration::seconds(state.pre_session.ttl_secs()),
    })
    .into_response();
    add_cookie(&mut res, &state.pre_session.set_cookie(&token))?;
    no_store(&mut res);
    Ok(res)
}

/// Authenticate with login id + password (dev mode).
///
/// Requires the pre-session CSRF token (`GET /api/v1/auth/csrf`) in `X-CSRF-Token`. On success
/// sets an opaque `HttpOnly; Secure; SameSite=Strict` session cookie and returns the session's
/// CSRF token. Principals that need MFA and have an enrolled TOTP factor get `mfaRequired` and
/// must call `/api/v1/auth/mfa/verify`; those without a factor get a single-factor session with
/// `mfaEnrollmentRequired = true`. Any presented session is revoked and replaced (fixation
/// defence). In Keycloak mode this endpoint answers `501` (no ROPC; use OIDC).
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    security(("login_csrf" = [])),
    responses(
        (status = 200, description = "Authenticated or MFA required", body = LoginResponse),
        (status = 401, description = "Invalid credentials (generic)", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Login CSRF token missing/invalid or cross-site request", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Validation failed", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "Password login not offered (Keycloak mode)", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn login(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(req): Json<LoginRequest>,
) -> ApiResult<Response> {
    let mut violations = vec![];
    if req.login_id.trim().is_empty() {
        violations.push(FieldViolation::new(
            "loginId",
            "required",
            "login id is required",
        ));
    }
    if req.password.is_empty() {
        violations.push(FieldViolation::new(
            "password",
            "required",
            "password is required",
        ));
    }
    if !violations.is_empty() {
        return Err(KernelError::Validation(violations).into());
    }
    let login_hint = req.login_id.clone();
    let principal = match state
        .authenticator
        .authenticate(Credentials {
            login_id: req.login_id,
            password: req.password,
        })
        .await
    {
        Ok(p) => p,
        Err(e) => {
            // Attributable internal evidence without leaking which factor failed.
            tracing::warn!(login = %login_hint, code = e.code(), "login rejected");
            return Err(e.into());
        }
    };
    // Principals the policy requires MFA for are challenged for any enrolled second factor
    // (TOTP and/or passkey); without one they get a single-factor session flagged for enrolment.
    if state.mfa.is_some() && state.mfa_policy.requires_mfa(&principal) {
        let (totp, passkey) = enrolled_factors(&state, &principal).await?;
        if totp || passkey {
            let mut mfa_methods = vec![];
            if passkey {
                mfa_methods.push(MfaMethod::Webauthn);
            }
            if totp {
                mfa_methods.push(MfaMethod::Totp);
            }
            let payload = MfaChallengePayload {
                principal,
                binding: browser_binding(&headers),
                webauthn_ceremony: None,
            };
            let challenge_id = state
                .pre_auth
                .put(
                    PreAuthKind::MfaChallenge,
                    serde_json::to_value(&payload).map_err(KernelError::internal)?,
                    Duration::seconds(MFA_CHALLENGE_TTL_SECS),
                )
                .await?;
            let mut res = Json(LoginResponse {
                outcome: LoginOutcome::MfaRequired,
                principal: None,
                challenge_id: Some(challenge_id),
                mfa_methods,
                csrf_token: None,
                expires_at: None,
                mfa_level: None,
                mfa_enrollment_required: false,
            })
            .into_response();
            no_store(&mut res);
            return Ok(res);
        }
    }
    establish(
        &state,
        &headers,
        NewSession::password(principal),
        "platform.auth.login",
    )
    .await
}

/// Complete a TOTP challenge issued by `login`.
///
/// Requires the same pre-session CSRF token the challenge was issued to. A challenge is single
/// use (a wrong code burns it) and expires after 5 minutes; each TOTP time step is accepted once.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/verify",
    tag = "auth",
    request_body = MfaVerifyRequest,
    security(("login_csrf" = [])),
    responses(
        (status = 200, body = LoginResponse),
        (status = 401, description = "Unknown/expired challenge, wrong or replayed code", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Login CSRF token missing/invalid", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn verify_mfa(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(req): Json<MfaVerifyRequest>,
) -> ApiResult<Response> {
    let mfa = mfa_service(&state)?;
    let payload = state
        .pre_auth
        .take(PreAuthKind::MfaChallenge, req.challenge_id.trim())
        .await?
        .ok_or(KernelError::Unauthenticated)?;
    let payload: MfaChallengePayload =
        serde_json::from_value(payload).map_err(KernelError::internal)?;
    let binding = browser_binding(&headers);
    if !secret::ct_eq(binding.as_bytes(), payload.binding.as_bytes()) {
        tracing::warn!("MFA challenge presented by another browser");
        return Err(KernelError::Unauthenticated.into());
    }
    if let Err(e) = mfa.verify_totp(&payload.principal, &req.code).await {
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
    establish(
        &state,
        &headers,
        NewSession::password_and_totp(payload.principal),
        "platform.auth.login",
    )
    .await
}

/// Current session: principal, expiries, assurance and the session's CSRF token.
///
/// Counts as activity (slides the idle window; the write is throttled).
#[utoipa::path(
    get,
    path = "/api/v1/auth/session",
    tag = "auth",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = SessionInfo),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn session(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Response> {
    let principal = Principal::from(&ctx);
    let mut res = Json(SessionInfo {
        mfa_enrollment_required: enrollment_required(&state, &principal).await?,
        principal,
        expires_at: ctx.expires_at,
        idle_expires_at: ctx.idle_expires_at,
        csrf_token: ctx.csrf_token,
        mfa_level: ctx.mfa_level,
        amr: ctx.amr,
    })
    .into_response();
    no_store(&mut res);
    Ok(res)
}

/// End the current session (idempotent).
///
/// With a live session the request must carry `X-CSRF-Token` (prevents forced sign-out). For a
/// Keycloak session the response is `200` with the IdP's RP-initiated logout URL.
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "auth",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, description = "Signed out; continue at the IdP to end the SSO session", body = LogoutResponse),
        (status = 204, description = "Signed out"),
        (status = 403, description = "CSRF token missing or invalid", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn logout(
    State(state): State<AuthState>,
    ctx: Option<Extension<AuthContext>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let mut end_session = None;
    if let Some(token) = read_cookie(&headers, state.cookie.name()) {
        let federated = match (&state.oidc, ctx.is_some()) {
            (Some(_), true) => state
                .sessions
                .get(&token)
                .await?
                .and_then(|s| s.idp_sid)
                .is_some(),
            _ => false,
        };
        state.sessions.revoke(&token).await?;
        if federated {
            if let Some(oidc) = &state.oidc {
                // The local session is gone either way; an unreachable IdP only loses the
                // single-sign-out hop.
                end_session = oidc.end_session_url(None).await.unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "end-session URL unavailable");
                    None
                });
            }
        }
    }
    if let Some(Extension(ctx)) = &ctx {
        audit(
            &state,
            &Principal::from(ctx),
            "platform.auth.logout",
            AuditOutcome::Succeeded,
            Some(ctx.session_id.to_string()),
        )
        .await;
    }
    let mut res = match end_session {
        Some(url) => Json(LogoutResponse {
            end_session_url: url,
        })
        .into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    };
    add_cookie(&mut res, &state.cookie.clear_cookie())?;
    Ok(res)
}

/// Start TOTP enrolment for the signed-in account (dev mode).
///
/// Returns a fresh secret as an `otpauth://` URI; nothing changes until `…/totp/confirm`
/// accepts a code from it. Replacing an already active factor requires an MFA-verified session.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/totp/enrolment",
    tag = "auth",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = TotpEnrolmentResponse),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Replacing a factor needs step-up", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "MFA is managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn totp_enrolment(
    State(state): State<AuthState>,
    Authenticated(ctx): Authenticated,
) -> ApiResult<Response> {
    let mfa = mfa_service(&state)?;
    let p = Principal::from(&ctx);
    if mfa.has_totp(&p).await? && ctx.mfa_level < MfaLevel::Mfa {
        return Err(KernelError::StepUpRequired("mfa.totp.replace").into());
    }
    let e = mfa.begin_totp_enrolment(&p).await?;
    let mut res = Json(TotpEnrolmentResponse {
        otpauth_uri: e.otpauth_uri,
        secret: e.secret_b32,
        digits: super::mfa::TOTP_DIGITS,
        period: super::mfa::TOTP_PERIOD,
    })
    .into_response();
    no_store(&mut res);
    Ok(res)
}

/// Confirm TOTP enrolment with the first code; activates the factor and upgrades the session.
///
/// The session is rotated (new cookie and CSRF token) to `mfaLevel = mfa`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/totp/confirm",
    tag = "auth",
    request_body = TotpCodeRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = LoginResponse),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "No enrolment in progress", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Wrong code", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn totp_confirm(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Authenticated(ctx): Authenticated,
    Json(req): Json<TotpCodeRequest>,
) -> ApiResult<Response> {
    let mfa = mfa_service(&state)?;
    require_code(&req.code)?;
    mfa.confirm_totp_enrolment(&Principal::from(&ctx), &req.code)
        .await?;
    elevate(&state, &headers, &ctx, "otp", "platform.auth.mfa.enrolled").await
}

/// Step up the current session with a TOTP code (needed for admin-sensitive permissions).
///
/// The session is rotated (new cookie and CSRF token) to `mfaLevel = mfa`; its absolute expiry
/// is unchanged. In Keycloak mode use `GET /api/v1/auth/oidc/start?stepUp=true` instead.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/step-up",
    tag = "auth",
    request_body = TotpCodeRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = LoginResponse),
        (status = 401, description = "No session, wrong or replayed code", body = Problem, content_type = "application/problem+json"),
        (status = 501, description = "MFA is managed by Keycloak", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn step_up(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Authenticated(ctx): Authenticated,
    Json(req): Json<TotpCodeRequest>,
) -> ApiResult<Response> {
    let mfa = mfa_service(&state)?;
    require_code(&req.code)?;
    let p = Principal::from(&ctx);
    if let Err(e) = mfa.verify_totp(&p, &req.code).await {
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
    elevate(&state, &headers, &ctx, "otp", "platform.auth.mfa.step_up").await
}

/// Start an OIDC sign-in at Keycloak (Authorization Code + PKCE, Keycloak mode).
///
/// Stores `state`, `nonce` and the PKCE verifier server-side for 10 minutes, binds the request
/// to this browser with a `SameSite=Lax` cookie and redirects (`303`) to Keycloak.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oidc/start",
    tag = "auth",
    params(OidcStartQuery),
    responses(
        (status = 303, description = "Redirect to the Keycloak authorization endpoint"),
        (status = 501, description = "OIDC not configured (dev mode)", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Identity provider unreachable", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn oidc_start(
    State(state): State<AuthState>,
    Query(q): Query<OidcStartQuery>,
) -> ApiResult<Response> {
    let oidc = state.oidc.as_ref().ok_or(KernelError::NotImplemented(
        "OIDC sign-in is only available with SUTRA_AUTH_MODE=keycloak",
    ))?;
    let nonce = secret::random_token()?;
    let verifier = pkce_verifier()?;
    let binding = secret::random_token()?;
    let payload = OidcRequestPayload {
        nonce: nonce.clone(),
        verifier: verifier.clone(),
        binding: secret::hex(&secret::sha256(binding.as_bytes())),
        return_to: safe_return_to(q.return_to.as_deref()),
    };
    let state_handle = state
        .pre_auth
        .put(
            PreAuthKind::OidcAuthorization,
            serde_json::to_value(&payload).map_err(KernelError::internal)?,
            Duration::seconds(OIDC_REQUEST_TTL_SECS),
        )
        .await?;
    let url = oidc
        .authorization_url(&AuthorizationParams {
            state: state_handle,
            nonce,
            code_challenge: pkce_challenge(&verifier),
            step_up: q.step_up.unwrap_or(false),
        })
        .await?;
    let mut res = redirect(&url);
    add_cookie(
        &mut res,
        &state
            .cookie
            .set_oidc_cookie(&binding, OIDC_REQUEST_TTL_SECS),
    )?;
    Ok(res)
}

/// Failure redirect of the OIDC callback: the SPA sign-in page with a stable error code.
fn callback_failure(state: &AuthState, code: &str) -> Response {
    let mut res = redirect(&format!("/sign-in?authError={code}"));
    let _ = add_cookie(&mut res, &state.cookie.clear_oidc_cookie());
    res
}

/// OIDC redirect endpoint (Keycloak → SutraERP).
///
/// Consumes the single-use `state`, checks the browser-binding cookie, exchanges the code with
/// the PKCE verifier, validates the ID token (JWKS signature, `iss`, `aud`, `exp`, `nonce`),
/// maps roles and tenant claims, creates the session and redirects (`303`) into the SPA. Any
/// failure redirects to `/sign-in?authError=<code>` without a session.
#[utoipa::path(
    get,
    path = "/api/v1/auth/oidc/callback",
    tag = "auth",
    params(OidcCallbackQuery),
    responses(
        (status = 303, description = "Session created → SPA, or failure → /sign-in?authError=…"),
    )
)]
pub async fn oidc_callback(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Query(q): Query<OidcCallbackQuery>,
) -> Response {
    let Some(oidc) = state.oidc.clone() else {
        return callback_failure(&state, "oidc-disabled");
    };
    let Some(handle) = q.state.as_deref().filter(|s| !s.is_empty()) else {
        return callback_failure(&state, "state-invalid");
    };
    // Consume the state first so it can never be replayed, whatever happens next.
    let payload = match state
        .pre_auth
        .take(PreAuthKind::OidcAuthorization, handle)
        .await
    {
        Ok(Some(p)) => p,
        Ok(None) => return callback_failure(&state, "state-invalid"),
        Err(e) => {
            tracing::error!(error = %e, "pre-auth lookup failed");
            return callback_failure(&state, "unavailable");
        }
    };
    let Ok(payload) = serde_json::from_value::<OidcRequestPayload>(payload) else {
        return callback_failure(&state, "state-invalid");
    };
    let cookie = read_cookie(&headers, state.cookie.oidc_cookie_name()).unwrap_or_default();
    let binding = secret::hex(&secret::sha256(cookie.as_bytes()));
    if cookie.is_empty() || !secret::ct_eq(binding.as_bytes(), payload.binding.as_bytes()) {
        tracing::warn!("OIDC callback from a browser that did not start the flow");
        return callback_failure(&state, "browser-mismatch");
    }
    if let Some(err) = q.error.as_deref() {
        tracing::warn!(error = err, "identity provider returned an error");
        let code = if err == "access_denied" {
            "access-denied"
        } else {
            "idp-error"
        };
        return callback_failure(&state, code);
    }
    let Some(code) = q.code.as_deref().filter(|c| !c.is_empty()) else {
        return callback_failure(&state, "code-missing");
    };
    let identity = match oidc.exchange(code, &payload.verifier, &payload.nonce).await {
        Ok(i) => i,
        Err(KernelError::Forbidden(why)) => {
            tracing::warn!(reason = %why, "federated identity not admitted");
            return callback_failure(&state, "access-denied");
        }
        Err(KernelError::Unavailable(why)) => {
            tracing::error!(reason = %why, "identity provider unavailable");
            return callback_failure(&state, "unavailable");
        }
        Err(_) => return callback_failure(&state, "token-invalid"),
    };
    let new = NewSession {
        principal: identity.principal,
        mfa_level: identity.mfa_level,
        amr: identity.amr,
        idp_sid: identity.idp_sid,
        not_after: None,
    };
    match issue_session(&state, &headers, new, "platform.auth.oidc.login").await {
        Ok((mut res, _)) => {
            *res.status_mut() = StatusCode::SEE_OTHER;
            if let Ok(v) = HeaderValue::from_str(&payload.return_to) {
                res.headers_mut().insert(header::LOCATION, v);
            }
            let _ = add_cookie(&mut res, &state.cookie.clear_oidc_cookie());
            res
        }
        Err(e) => {
            tracing::error!(error = ?e.0, "session creation failed");
            callback_failure(&state, "unavailable")
        }
    }
}

pub fn router(deps: PlatformDeps) -> OpenApiRouter {
    let state = AuthState {
        authenticator: deps.authenticator,
        sessions: deps.sessions,
        audit: deps.audit,
        cookie: deps.cookie,
        mfa_policy: deps.mfa,
        mfa: deps.mfa_service,
        pre_auth: deps.pre_auth,
        oidc: deps.oidc,
        webauthn: deps.webauthn,
        pre_session: deps.pre_session,
        replay: deps.replay,
        sign_in_hook: deps.sign_in_hook,
    };
    OpenApiRouter::new()
        .routes(routes!(csrf))
        .routes(routes!(login))
        .routes(routes!(verify_mfa))
        .routes(routes!(session))
        .routes(routes!(logout))
        .routes(routes!(totp_enrolment))
        .routes(routes!(totp_confirm))
        .routes(routes!(step_up))
        .routes(routes!(oidc_start))
        .routes(routes!(oidc_callback))
        .merge(super::http_security::routes())
        .merge(super::http_webauthn::routes())
        .with_state(state)
        // Part 1 S3 (DEF-X-003): sign-in before a session exists runs in the system database
        // context; requests with a session stay scoped to its tenant.
        .layer(axum::middleware::from_fn(sutra_web::auth::pre_auth_system))
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
