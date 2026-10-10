//! HTTP surface of the auth boundary.

use super::CookieSettings;
use crate::PlatformDeps;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use std::sync::Arc;
use sutra_contracts::platform::auth::{
    AuthOutcome, CredentialAuthenticator, Credentials, MfaMethod, Principal, SessionStore,
};
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome, AuditSink};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_web::auth::read_cookie;
use sutra_web::request_id::current_correlation_id;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

#[derive(Clone)]
pub struct AuthState {
    pub authenticator: Arc<dyn CredentialAuthenticator>,
    pub sessions: Arc<dyn SessionStore>,
    pub audit: Arc<dyn AuditSink>,
    pub cookie: CookieSettings,
}

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
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    pub principal: Principal,
    /// Absolute expiry (re-authentication required afterwards).
    pub expires_at: chrono::DateTime<chrono::Utc>,
    /// Expiry if the session stays idle from now on (sliding window).
    pub idle_expires_at: chrono::DateTime<chrono::Utc>,
    /// Synchronizer CSRF token bound to this session; send as `X-CSRF-Token` on unsafe requests.
    pub csrf_token: String,
}

async fn establish(
    state: &AuthState,
    headers: &HeaderMap,
    principal: Principal,
) -> ApiResult<Response> {
    // Session fixation defence: always mint a new id and revoke any presented one.
    if let Some(old) = read_cookie(headers, state.cookie.name()) {
        state.sessions.revoke(&old).await?;
    }
    let issued = state.sessions.create(principal.clone()).await?;
    let _ = state
        .audit
        .record(
            AuditEntry::new(
                principal.scope,
                "platform.auth.login",
                AuditOutcome::Succeeded,
            )
            .actor(ActorKind::User, Some(principal.subject.clone()))
            .target("session", issued.session.id.to_string())
            .correlation(current_correlation_id()),
        )
        .await;
    let body = LoginResponse {
        outcome: LoginOutcome::Authenticated,
        principal: Some(principal),
        challenge_id: None,
        mfa_methods: vec![],
        csrf_token: Some(issued.session.csrf_token.clone()),
        expires_at: Some(issued.session.expires_at),
    };
    let mut res = Json(body).into_response();
    let cookie = HeaderValue::from_str(&state.cookie.set_cookie(&issued.token))
        .map_err(KernelError::internal)?;
    res.headers_mut().insert(header::SET_COOKIE, cookie);
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

/// Authenticate with login id + password.
///
/// Delegates to the configured credential authority (Keycloak in production, dev stub in Batch 0)
/// and, on success, sets an opaque `HttpOnly; Secure; SameSite=Strict` session cookie and returns
/// the session's CSRF token. Privileged principals may receive `mfaRequired` instead and must
/// call `/api/v1/auth/mfa/verify`. Exempt from the CSRF check (no session exists yet); any
/// presented session is revoked and replaced (fixation defence).
#[utoipa::path(
    post,
    path = "/api/v1/auth/login",
    tag = "auth",
    request_body = LoginRequest,
    responses(
        (status = 200, description = "Authenticated or MFA required", body = LoginResponse),
        (status = 401, description = "Invalid credentials (generic)", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Validation failed", body = Problem, content_type = "application/problem+json"),
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
    let outcome = state
        .authenticator
        .authenticate(Credentials {
            login_id: req.login_id,
            password: req.password,
        })
        .await;
    match outcome {
        Ok(AuthOutcome::Authenticated(principal)) => establish(&state, &headers, principal).await,
        Ok(AuthOutcome::MfaRequired {
            challenge_id,
            methods,
            ..
        }) => Ok(Json(LoginResponse {
            outcome: LoginOutcome::MfaRequired,
            principal: None,
            challenge_id: Some(challenge_id),
            mfa_methods: methods,
            csrf_token: None,
            expires_at: None,
        })
        .into_response()),
        Err(e) => {
            // Attributable internal evidence without leaking which factor failed.
            tracing::warn!(login = %login_hint, code = e.code(), "login rejected");
            Err(e.into())
        }
    }
}

/// Complete an MFA challenge issued by `login`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/mfa/verify",
    tag = "auth",
    request_body = MfaVerifyRequest,
    responses(
        (status = 200, body = LoginResponse),
        (status = 401, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn verify_mfa(
    State(state): State<AuthState>,
    headers: HeaderMap,
    Json(req): Json<MfaVerifyRequest>,
) -> ApiResult<Response> {
    let principal = state
        .authenticator
        .verify_mfa(&req.challenge_id, &req.code)
        .await?;
    establish(&state, &headers, principal).await
}

/// Current session: principal, expiries and the session's CSRF token.
///
/// Counts as activity (slides the idle window).
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
pub async fn session(Authenticated(ctx): Authenticated) -> Response {
    let mut res = Json(SessionInfo {
        principal: Principal::from(&ctx),
        expires_at: ctx.expires_at,
        idle_expires_at: ctx.idle_expires_at,
        csrf_token: ctx.csrf_token,
    })
    .into_response();
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// End the current session (idempotent).
///
/// With a live session the request must carry `X-CSRF-Token` (prevents forced sign-out).
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "auth",
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 204, description = "Signed out"),
        (status = 403, description = "CSRF token missing or invalid", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn logout(
    State(state): State<AuthState>,
    ctx: Option<Extension<AuthContext>>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    if let Some(token) = read_cookie(&headers, state.cookie.name()) {
        state.sessions.revoke(&token).await?;
    }
    if let Some(Extension(ctx)) = ctx {
        let _ = state
            .audit
            .record(
                AuditEntry::new(ctx.scope, "platform.auth.logout", AuditOutcome::Succeeded)
                    .actor(ActorKind::User, Some(ctx.subject.clone()))
                    .target("session", ctx.session_id.to_string())
                    .correlation(current_correlation_id()),
            )
            .await;
    }
    let mut res = StatusCode::NO_CONTENT.into_response();
    let cookie =
        HeaderValue::from_str(&state.cookie.clear_cookie()).map_err(KernelError::internal)?;
    res.headers_mut().insert(header::SET_COOKIE, cookie);
    Ok(res)
}

pub fn router(deps: PlatformDeps) -> OpenApiRouter {
    let state = AuthState {
        authenticator: deps.authenticator,
        sessions: deps.sessions,
        audit: deps.audit,
        cookie: deps.cookie,
    };
    OpenApiRouter::new()
        .routes(routes!(login))
        .routes(routes!(verify_mfa))
        .routes(routes!(session))
        .routes(routes!(logout))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::dev::{DevAuthenticator, PrivilegedRoleMfaPolicy, DEV_MFA_CODE};
    use crate::auth::session::InMemorySessionStore;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use sutra_contracts::platform::auth::SessionPolicy;
    use sutra_kernel::audit::TracingAuditSink;
    use sutra_kernel::scope::{ExecutionScope, TenantId};
    use sutra_web::auth::CSRF_HEADER;
    use tower::ServiceExt;

    fn app(enforce_mfa: bool) -> axum::Router {
        let sessions: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new(
            SessionPolicy::new(1800, 3600).unwrap(),
        ));
        let cookie = CookieSettings {
            secure: true,
            max_age_secs: 3600,
        };
        let auth = crate::auth::session_auth(sessions.clone(), &cookie);
        let deps = PlatformDeps {
            authenticator: Arc::new(DevAuthenticator::new(
                "sutra".into(),
                ExecutionScope::tenant(TenantId::new()),
                Box::new(PrivilegedRoleMfaPolicy {
                    enforce: enforce_mfa,
                }),
            )),
            sessions,
            mfa: Arc::new(PrivilegedRoleMfaPolicy {
                enforce: enforce_mfa,
            }),
            audit: Arc::new(TracingAuditSink),
            cookie,
            modules: vec![],
        };
        axum::Router::from(router(deps)).layer(axum::middleware::from_fn_with_state(
            auth,
            sutra_web::auth::authenticate,
        ))
    }

    fn post(path: &str, body: serde_json::Value, cookie: Option<&str>) -> Request<Body> {
        post_csrf(path, body, cookie, None)
    }

    fn post_csrf(
        path: &str,
        body: serde_json::Value,
        cookie: Option<&str>,
        csrf: Option<&str>,
    ) -> Request<Body> {
        let mut b = Request::post(path).header("content-type", "application/json");
        if let Some(c) = cookie {
            b = b.header("cookie", c);
        }
        if let Some(t) = csrf {
            b = b.header(CSRF_HEADER, t);
        }
        b.body(Body::from(body.to_string())).unwrap()
    }

    fn get_session(cookie: &str) -> Request<Body> {
        Request::get("/api/v1/auth/session")
            .header("cookie", cookie)
            .body(Body::empty())
            .unwrap()
    }

    async fn json(res: Response) -> serde_json::Value {
        serde_json::from_slice(&res.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    fn cookie_pair(res: &Response) -> String {
        res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn login(app: &axum::Router, login_id: &str) -> (String, String) {
        let res = app
            .clone()
            .oneshot(post(
                "/api/v1/auth/login",
                serde_json::json!({"loginId": login_id, "password": "sutra"}),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let cookie = cookie_pair(&res);
        let csrf = json(res).await["csrfToken"].as_str().unwrap().to_owned();
        (cookie, csrf)
    }

    #[tokio::test]
    async fn login_session_logout_flow() {
        let app = app(false);
        let res = app
            .clone()
            .oneshot(post(
                "/api/v1/auth/login",
                serde_json::json!({"loginId": "aarav.desai@sutra.dev", "password": "sutra"}),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let set = res.headers()[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(
            set.contains("HttpOnly") && set.contains("Secure") && set.contains("SameSite=Strict")
        );
        let cookie = cookie_pair(&res);
        let body = json(res).await;
        assert_eq!(body["outcome"], "authenticated");
        assert_eq!(body["principal"]["roles"][0], "student");
        assert!(!body.to_string().contains("password"));
        let csrf = body["csrfToken"].as_str().unwrap().to_owned();
        assert_eq!(csrf.len(), 64);
        assert!(
            !set.contains(&csrf),
            "the CSRF token is not the session token"
        );

        let me = app.clone().oneshot(get_session(&cookie)).await.unwrap();
        assert_eq!(me.status(), StatusCode::OK);
        assert_eq!(me.headers()[header::CACHE_CONTROL], "no-store");
        let me = json(me).await;
        assert_eq!(me["principal"]["displayName"], "Aarav Desai");
        assert_eq!(
            me["csrfToken"],
            csrf.as_str(),
            "same token from login and session"
        );
        assert!(me["idleExpiresAt"].is_string() && me["expiresAt"].is_string());

        // Logout with a live session is an unsafe cookie request: CSRF required.
        let forced = app
            .clone()
            .oneshot(post(
                "/api/v1/auth/logout",
                serde_json::json!({}),
                Some(&cookie),
            ))
            .await
            .unwrap();
        assert_eq!(forced.status(), StatusCode::FORBIDDEN);
        assert_eq!(json(forced).await["errorCode"], "csrf-token-missing");
        assert_eq!(
            app.clone()
                .oneshot(get_session(&cookie))
                .await
                .unwrap()
                .status(),
            StatusCode::OK,
            "a forged logout must not end the session"
        );

        let out = app
            .clone()
            .oneshot(post_csrf(
                "/api/v1/auth/logout",
                serde_json::json!({}),
                Some(&cookie),
                Some(&csrf),
            ))
            .await
            .unwrap();
        assert_eq!(out.status(), StatusCode::NO_CONTENT);
        let me = app.oneshot(get_session(&cookie)).await.unwrap();
        assert_eq!(me.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            me.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
    }

    #[tokio::test]
    async fn logout_without_live_session_is_idempotent() {
        let res = app(false)
            .oneshot(post(
                "/api/v1/auth/logout",
                serde_json::json!({}),
                Some("__Host-sutra_session=dead"),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn bad_credentials_are_generic_problem() {
        let res = app(false)
            .oneshot(post(
                "/api/v1/auth/login",
                serde_json::json!({"loginId": "admin@sutra.dev", "password": "nope"}),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            res.headers()[header::CONTENT_TYPE],
            "application/problem+json"
        );
        assert_eq!(json(res).await["errorCode"], "unauthenticated");
    }

    #[tokio::test]
    async fn empty_fields_are_validation_errors() {
        let res = app(false)
            .oneshot(post(
                "/api/v1/auth/login",
                serde_json::json!({"loginId": "", "password": ""}),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            json(res).await["validationErrors"].as_array().map(Vec::len),
            Some(2)
        );
    }

    #[tokio::test]
    async fn mfa_hook_for_privileged_role() {
        let app = app(true);
        let res = app
            .clone()
            .oneshot(post(
                "/api/v1/auth/login",
                serde_json::json!({"loginId": "admin@sutra.dev", "password": "sutra"}),
                None,
            ))
            .await
            .unwrap();
        assert!(
            res.headers().get(header::SET_COOKIE).is_none(),
            "no session before MFA"
        );
        let body = json(res).await;
        assert_eq!(body["outcome"], "mfaRequired");
        assert!(body.get("csrfToken").is_none(), "no CSRF token before MFA");
        let challenge = body["challengeId"].as_str().unwrap().to_owned();
        let res = app
            .oneshot(post(
                "/api/v1/auth/mfa/verify",
                serde_json::json!({"challengeId": challenge, "code": DEV_MFA_CODE}),
                None,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(res.headers().get(header::SET_COOKIE).is_some());
        assert!(json(res).await["csrfToken"].is_string());
    }

    #[tokio::test]
    async fn login_rotates_presented_session_without_csrf() {
        let app = app(false);
        let (first, first_csrf) = login(&app, "meera.shah@sutra.dev").await;
        // Login is CSRF-exempt even when a live session cookie is presented.
        let second_res = app
            .clone()
            .oneshot(post(
                "/api/v1/auth/login",
                serde_json::json!({"loginId": "meera.shah@sutra.dev", "password": "sutra"}),
                Some(&first),
            ))
            .await
            .unwrap();
        assert_eq!(second_res.status(), StatusCode::OK);
        let second = cookie_pair(&second_res);
        assert_ne!(first, second);
        assert_ne!(
            json(second_res).await["csrfToken"].as_str().unwrap(),
            first_csrf,
            "a new session gets a new CSRF token"
        );
        let old = app.oneshot(get_session(&first)).await.unwrap();
        assert_eq!(old.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn csrf_token_of_another_session_is_rejected() {
        let app = app(false);
        let (cookie_a, _) = login(&app, "meera.shah@sutra.dev").await;
        let (_, csrf_b) = login(&app, "aarav.desai@sutra.dev").await;
        let res = app
            .oneshot(post_csrf(
                "/api/v1/auth/logout",
                serde_json::json!({}),
                Some(&cookie_a),
                Some(&csrf_b),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        assert_eq!(json(res).await["errorCode"], "csrf-token-invalid");
    }
}
