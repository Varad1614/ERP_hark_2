//! Session authentication, CSRF enforcement and the authenticated-principal extractor
//! (ADR-L5-0038 §3.4, ADR-L5-0024 D02, ADR-L5-0004).
//!
//! [`authenticate`] is a global middleware. For every request it:
//!
//! 1. reads the opaque session token from the session cookie (if any) and resolves it through
//!    the [`SessionResolver`] port (D01 session store);
//! 2. on the configured **pre-session paths** (login, MFA verification — they establish a
//!    session) requires a signed double-submit [`PreSessionCsrf`] token: the `X-CSRF-Token`
//!    header must equal the `__Host-sutra_login_csrf` cookie issued by `GET /api/v1/auth/csrf`,
//!    and the token's HMAC and expiry must verify (closes the login-CSRF gap);
//! 3. for every other **unsafe method** (POST/PUT/PATCH/DELETE) carried by a live cookie
//!    session, requires the session's synchronizer token in `X-CSRF-Token` (constant-time
//!    compare). Missing or wrong tokens get a `403 application/problem+json`;
//! 4. stores the resolved [`AuthContext`] in the request extensions.
//!
//! [`origin_guard`] (separate middleware) rejects unsafe requests that a browser marks as
//! cross-site (`Sec-Fetch-Site: cross-site`) or whose `Origin` is neither the request's own
//! host nor an allow-listed origin.
//!
//! Handlers then take [`Authenticated`] (401 without a live session) and call
//! [`Authenticated::require`] with a typed [`Permission`] (403 when the deny-by-default policy
//! does not grant it). The execution scope of a protected operation is **always** the session
//! scope; scope headers sent by a client can never select or widen it, and a header that
//! contradicts the session is rejected with 403.

use crate::problem::ApiError;
use crate::scope::{INSTITUTION_HEADER, TENANT_HEADER};
use crate::Problem;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{header, request::Parts, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use std::sync::Arc;
use sutra_kernel::auth::{AuthContext, Permission, SessionResolver};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_kernel::secret;

/// Header carrying the synchronizer CSRF token on unsafe requests.
pub const CSRF_HEADER: &str = "x-csrf-token";

const LOGIN_CSRF_CONTEXT: &[u8] = b"sutraerp.login-csrf.v1|";

/// Pre-session (login) CSRF protection: a **signed double-submit** token.
///
/// `GET /api/v1/auth/csrf` issues `<nonce>.<expiry>.<mac>` where
/// `mac = HMAC-SHA256(key, "sutraerp.login-csrf.v1|" ‖ nonce ‖ "." ‖ expiry)`, both as an
/// `HttpOnly; SameSite=Strict` (`__Host-` when secure) cookie and in the response body. Requests
/// to pre-session paths must echo it in `X-CSRF-Token`; the header must equal the cookie
/// (constant time), the MAC must verify under the server key and the expiry must not have
/// passed. An attacker can neither read the cookie nor mint a valid token, and a cookie planted
/// from a sibling sub-domain fails the MAC.
#[derive(Clone)]
pub struct PreSessionCsrf {
    key: Arc<[u8; 32]>,
    secure: bool,
    ttl_secs: i64,
}

impl PreSessionCsrf {
    pub fn new(key: [u8; 32], secure: bool, ttl_secs: i64) -> Self {
        Self {
            key: Arc::new(key),
            secure,
            ttl_secs: ttl_secs.max(1),
        }
    }

    pub fn ttl_secs(&self) -> i64 {
        self.ttl_secs
    }

    pub fn cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-sutra_login_csrf"
        } else {
            "sutra_login_csrf"
        }
    }

    fn mac(&self, nonce: &str, expiry: i64) -> Result<String, KernelError> {
        let msg = format!("{nonce}.{expiry}");
        let mut m = Vec::with_capacity(LOGIN_CSRF_CONTEXT.len() + msg.len());
        m.extend_from_slice(LOGIN_CSRF_CONTEXT);
        m.extend_from_slice(msg.as_bytes());
        Ok(secret::hex(&secret::hmac_sha256(self.key.as_slice(), &m)?))
    }

    /// Mints a fresh token valid until `now + ttl`.
    pub fn issue(&self, now: DateTime<Utc>) -> Result<String, KernelError> {
        let nonce = secret::random_token()?;
        let expiry = now.timestamp() + self.ttl_secs;
        Ok(format!("{nonce}.{expiry}.{}", self.mac(&nonce, expiry)?))
    }

    /// `true` when `token` carries a valid MAC and has not expired at `now`.
    pub fn verify(&self, token: &str, now: DateTime<Utc>) -> bool {
        let mut parts = token.split('.');
        let (Some(nonce), Some(expiry), Some(mac), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return false;
        };
        let Ok(expiry) = expiry.parse::<i64>() else {
            return false;
        };
        if nonce.len() != 64 || now.timestamp() >= expiry {
            return false;
        }
        match self.mac(nonce, expiry) {
            Ok(expected) => secret::ct_eq(expected.as_bytes(), mac.as_bytes()),
            Err(_) => false,
        }
    }

    pub fn set_cookie(&self, token: &str) -> String {
        let mut c = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
            self.cookie_name(),
            token,
            self.ttl_secs
        );
        if self.secure {
            c.push_str("; Secure");
        }
        c
    }

    /// Validates the double-submit pair on a request. `Err` carries the problem response.
    pub fn check(&self, headers: &HeaderMap, now: DateTime<Utc>) -> Result<(), Box<Response>> {
        let Some(header) = headers.get(CSRF_HEADER) else {
            return Err(Box::new(csrf_problem(
                "csrf-token-missing",
                "sign-in requests must carry the X-CSRF-Token issued by GET /api/v1/auth/csrf",
            )));
        };
        let header = header.to_str().map(str::trim).unwrap_or_default();
        let cookie = read_cookie(headers, self.cookie_name()).unwrap_or_default();
        if cookie.is_empty()
            || !secret::ct_eq(header.as_bytes(), cookie.as_bytes())
            || !self.verify(header, now)
        {
            tracing::warn!("pre-session CSRF token rejected");
            return Err(Box::new(csrf_problem(
                "csrf-token-invalid",
                "the sign-in CSRF token is missing from the cookie, does not match it, is forged or expired; fetch a new one from GET /api/v1/auth/csrf",
            )));
        }
        Ok(())
    }
}

/// Shared state of the [`authenticate`] middleware.
#[derive(Clone)]
pub struct SessionAuth {
    resolver: Arc<dyn SessionResolver>,
    cookie_name: Arc<str>,
    pre_session_paths: Arc<[String]>,
    pre_session: PreSessionCsrf,
}

impl SessionAuth {
    /// `pre_session_paths` lists exact request paths that *establish* a session (login, MFA
    /// verification): they are protected by the [`PreSessionCsrf`] double-submit token instead
    /// of the session's synchronizer token.
    pub fn new<I, P>(
        resolver: Arc<dyn SessionResolver>,
        cookie_name: &str,
        pre_session_paths: I,
        pre_session: PreSessionCsrf,
    ) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        Self {
            resolver,
            cookie_name: Arc::from(cookie_name),
            pre_session_paths: pre_session_paths.into_iter().map(Into::into).collect(),
            pre_session,
        }
    }

    pub fn cookie_name(&self) -> &str {
        &self.cookie_name
    }

    pub fn pre_session(&self) -> &PreSessionCsrf {
        &self.pre_session
    }
}

/// Value of the named cookie, if present.
pub fn read_cookie(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, v)| *k == name && !v.is_empty())
        .map(|(_, v)| v.to_owned())
}

/// Methods that change state and therefore need CSRF protection.
pub fn is_unsafe(m: &Method) -> bool {
    matches!(
        *m,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

fn csrf_problem(code: &str, detail: &str) -> Response {
    Problem::new(StatusCode::FORBIDDEN, code, "CSRF check failed")
        .detail(detail)
        .into_response()
}

/// Global authentication + CSRF middleware. Must wrap every router whose handlers use
/// [`Authenticated`] and must sit *outside* the idempotency middleware (which keys records by
/// the authenticated owner).
pub async fn authenticate(
    State(auth): State<SessionAuth>,
    mut req: Request,
    next: Next,
) -> Response {
    let pre_session_path = auth
        .pre_session_paths
        .iter()
        .any(|p| p.as_str() == req.uri().path());
    let unsafe_method = is_unsafe(req.method());
    if unsafe_method && pre_session_path {
        if let Err(problem) = auth.pre_session.check(req.headers(), Utc::now()) {
            return *problem;
        }
    }
    let Some(token) = read_cookie(req.headers(), &auth.cookie_name) else {
        return next.run(req).await;
    };
    let ctx = match auth.resolver.resolve(&token).await {
        Ok(ctx) => ctx,
        Err(e) => return ApiError(e).into_response(),
    };
    // A dead/unknown cookie carries no authority: the request proceeds anonymously and any
    // protected handler answers 401.
    let Some(ctx) = ctx else {
        return next.run(req).await;
    };
    if unsafe_method && !pre_session_path {
        match req.headers().get(CSRF_HEADER).map(|v| v.to_str()) {
            None => {
                return csrf_problem(
                    "csrf-token-missing",
                    "unsafe requests must carry the session's X-CSRF-Token header",
                )
            }
            Some(Ok(v)) if ctx.csrf_matches(v.trim()) => {}
            Some(_) => {
                tracing::warn!(session = %ctx.session_id, "CSRF token mismatch");
                return csrf_problem(
                    "csrf-token-invalid",
                    "the X-CSRF-Token header does not match the session",
                );
            }
        }
    }
    req.extensions_mut().insert(ctx);
    next.run(req).await
}

/// Allow-list for [`origin_guard`]: origins (`scheme://host[:port]`) that may send unsafe
/// requests in addition to the API's own host.
#[derive(Clone, Default)]
pub struct OriginPolicy {
    allowed: Arc<[String]>,
}

impl OriginPolicy {
    pub fn new<I, P>(allowed: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        Self {
            allowed: allowed
                .into_iter()
                .map(|o| o.as_ref().trim().trim_end_matches('/').to_ascii_lowercase())
                .filter(|o| !o.is_empty())
                .collect(),
        }
    }

    /// `Ok` when an unsafe request with these headers may proceed.
    pub fn check(&self, headers: &HeaderMap) -> Result<(), &'static str> {
        if let Some(site) = headers.get("sec-fetch-site") {
            if site.as_bytes().eq_ignore_ascii_case(b"cross-site") {
                return Err("the browser marked this request as cross-site (Sec-Fetch-Site)");
            }
        }
        let Some(origin) = headers.get(header::ORIGIN) else {
            // Not a browser cross-origin request (curl, server-to-server, same-origin GET→POST
            // in old browsers): session cookies are SameSite=Strict and CSRF tokens still apply.
            return Ok(());
        };
        let origin = origin
            .to_str()
            .map(|o| o.trim().trim_end_matches('/').to_ascii_lowercase())
            .unwrap_or_default();
        if origin.is_empty() || origin == "null" {
            return Err("opaque or unreadable Origin");
        }
        if self.allowed.contains(&origin) {
            return Ok(());
        }
        let authority = origin.split_once("://").map(|(_, a)| a).unwrap_or_default();
        let host = headers
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(|h| h.trim().to_ascii_lowercase());
        match host {
            Some(h) if !authority.is_empty() && h == authority => Ok(()),
            _ => Err("the request Origin is not this site and not allow-listed"),
        }
    }
}

/// Global middleware: refuses cross-site unsafe requests (`403 cross-site-request-rejected`).
/// Safe methods pass untouched (the OIDC callback is a cross-site top-level GET by design and
/// is protected by its `state`).
pub async fn origin_guard(
    State(policy): State<OriginPolicy>,
    req: Request,
    next: Next,
) -> Response {
    if is_unsafe(req.method()) {
        if let Err(why) = policy.check(req.headers()) {
            tracing::warn!(reason = why, path = %req.uri().path(), "cross-site request rejected");
            return Problem::new(
                StatusCode::FORBIDDEN,
                "cross-site-request-rejected",
                "Cross-site request rejected",
            )
            .detail(why)
            .into_response();
        }
    }
    next.run(req).await
}

/// Rejects scope headers that contradict the session scope. Headers never *set* scope.
fn check_scope_headers(headers: &HeaderMap, scope: &ExecutionScope) -> Result<(), KernelError> {
    let header_uuid = |name: &str| {
        headers.get(name).map(|v| {
            v.to_str()
                .ok()
                .and_then(|s| uuid::Uuid::parse_str(s.trim()).ok())
        })
    };
    let outside = || KernelError::Forbidden("requested scope is outside the session scope".into());
    if let Some(t) = header_uuid(TENANT_HEADER) {
        if t != Some(scope.tenant_id.as_uuid()) {
            return Err(outside());
        }
    }
    if let (Some(i), Some(own)) = (header_uuid(INSTITUTION_HEADER), scope.institution_id) {
        if i != Some(own.as_uuid()) {
            return Err(outside());
        }
    }
    Ok(())
}

/// Extractor: the authenticated caller (401 problem without a live session).
#[derive(Debug, Clone)]
pub struct Authenticated(pub AuthContext);

impl Authenticated {
    /// Deny-by-default permission check (403 problem when not granted).
    pub fn require(&self, permission: Permission) -> Result<&AuthContext, ApiError> {
        self.0.require(permission)?;
        Ok(&self.0)
    }

    /// The session-bound execution scope.
    pub fn scope(&self) -> ExecutionScope {
        self.0.scope
    }
}

impl<S: Send + Sync> FromRequestParts<S> for Authenticated {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let ctx = parts
            .extensions
            .get::<AuthContext>()
            .cloned()
            .ok_or(KernelError::Unauthenticated)?;
        check_scope_headers(&parts.headers, &ctx.scope)?;
        Ok(Authenticated(ctx))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get, post};
    use axum::Router;
    use chrono::Utc;
    use http_body_util::BodyExt;
    use sutra_kernel::auth::Role;
    use sutra_kernel::scope::{InstitutionId, TenantId};
    use tower::ServiceExt;

    const TOKEN: &str = "good-token";
    const CSRF: &str = "c5rf-0000000000000000000000000000000000000000000000000000000000";

    struct Fixed(AuthContext);

    #[async_trait::async_trait]
    impl SessionResolver for Fixed {
        async fn resolve(&self, token: &str) -> Result<Option<AuthContext>, KernelError> {
            Ok((token == TOKEN).then(|| self.0.clone()))
        }
    }

    fn ctx(roles: Vec<Role>) -> AuthContext {
        AuthContext {
            session_id: uuid::Uuid::now_v7(),
            subject: "dev|a".into(),
            login_id: "a".into(),
            display_name: "A".into(),
            roles,
            scope: ExecutionScope::institution(TenantId::new(), InstitutionId::new()),
            csrf_token: CSRF.into(),
            expires_at: Utc::now(),
            idle_expires_at: Utc::now(),
            mfa_level: sutra_kernel::auth::MfaLevel::None,
            amr: vec![],
            step_up_enforced: false,
        }
    }

    const KEY: [u8; 32] = [7u8; 32];

    fn pre() -> PreSessionCsrf {
        PreSessionCsrf::new(KEY, true, 600)
    }

    fn app(c: AuthContext) -> Router {
        let auth = SessionAuth::new(Arc::new(Fixed(c)), "sid", ["/login"], pre());
        Router::new()
            .route(
                "/me",
                get(|a: Authenticated| async move { a.0.subject.clone() }),
            )
            .route(
                "/admin",
                post(|a: Authenticated| async move {
                    a.require(Permission::InstitutionRegister)
                        .map(|_| "ok".to_owned())
                }),
            )
            .route("/login", post(|| async { "login" }))
            .route("/public", post(|| async { "public" }))
            .layer(axum::middleware::from_fn_with_state(auth, authenticate))
    }

    fn req(method: &str, path: &str, cookie: Option<&str>, csrf: Option<&str>) -> Request {
        let mut b = Request::builder().method(method).uri(path);
        if let Some(c) = cookie {
            b = b.header("cookie", format!("other=1; sid={c}"));
        }
        if let Some(t) = csrf {
            b = b.header(CSRF_HEADER, t);
        }
        b.body(Body::empty()).unwrap()
    }

    async fn call(app: Router, r: Request) -> (StatusCode, serde_json::Value) {
        let res = app.oneshot(r).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::String(
                String::from_utf8_lossy(&bytes).into(),
            )),
        )
    }

    #[test]
    fn cookie_parsing() {
        let mut h = HeaderMap::new();
        h.insert(header::COOKIE, "a=1; sid=xyz; b=2".parse().unwrap());
        assert_eq!(read_cookie(&h, "sid").as_deref(), Some("xyz"));
        assert_eq!(read_cookie(&h, "zzz"), None);
        h.insert(header::COOKIE, "sid=".parse().unwrap());
        assert_eq!(read_cookie(&h, "sid"), None, "empty cookie is no cookie");
    }

    #[tokio::test]
    async fn no_session_is_401_problem() {
        let (s, v) = call(app(ctx(vec![Role::Student])), req("GET", "/me", None, None)).await;
        assert_eq!(s, StatusCode::UNAUTHORIZED);
        assert_eq!(v["errorCode"], "unauthenticated");
        let (s, _) = call(
            app(ctx(vec![Role::Student])),
            req("GET", "/me", Some("stale"), None),
        )
        .await;
        assert_eq!(s, StatusCode::UNAUTHORIZED, "unknown token");
    }

    #[tokio::test]
    async fn safe_methods_need_no_csrf() {
        let (s, v) = call(
            app(ctx(vec![Role::Student])),
            req("GET", "/me", Some(TOKEN), None),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(v, "dev|a");
    }

    #[tokio::test]
    async fn unsafe_methods_require_matching_csrf() {
        let a = || app(ctx(vec![Role::Administrator]));
        let (s, v) = call(a(), req("POST", "/admin", Some(TOKEN), None)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["errorCode"], "csrf-token-missing");
        let (s, v) = call(a(), req("POST", "/admin", Some(TOKEN), Some("wrong"))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["errorCode"], "csrf-token-invalid");
        let (s, _) = call(a(), req("POST", "/admin", Some(TOKEN), Some(CSRF))).await;
        assert_eq!(s, StatusCode::OK);
        // Applies to every unsafe route with a live cookie session, not only protected ones.
        let (s, _) = call(a(), req("POST", "/public", Some(TOKEN), None)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }

    fn login_req(cookie: Option<&str>, header: Option<&str>) -> Request {
        let mut b = Request::builder().method("POST").uri("/login");
        if let Some(c) = cookie {
            b = b.header(
                "cookie",
                format!("sid={TOKEN}; __Host-sutra_login_csrf={c}"),
            );
        }
        if let Some(h) = header {
            b = b.header(CSRF_HEADER, h);
        }
        b.body(Body::empty()).unwrap()
    }

    #[tokio::test]
    async fn pre_session_paths_need_the_login_csrf_token() {
        let a = || app(ctx(vec![Role::Administrator]));
        let good = pre().issue(Utc::now()).unwrap();
        // missing entirely
        let (s, v) = call(a(), req("POST", "/login", None, None)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["errorCode"], "csrf-token-missing");
        // header without cookie
        let (s, v) = call(a(), login_req(None, Some(&good))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["errorCode"], "csrf-token-invalid");
        // cookie/header mismatch
        let other = pre().issue(Utc::now()).unwrap();
        let (s, v) = call(a(), login_req(Some(&good), Some(&other))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["errorCode"], "csrf-token-invalid");
        // attacker-planted pair signed with another key
        let forged = PreSessionCsrf::new([9u8; 32], true, 600)
            .issue(Utc::now())
            .unwrap();
        let (s, _) = call(a(), login_req(Some(&forged), Some(&forged))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        // the session's synchronizer token does not open pre-session paths
        let (s, _) = call(a(), req("POST", "/login", Some(TOKEN), Some(CSRF))).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        // valid double submit (a live session cookie may also be present: login rotates it)
        let (s, v) = call(a(), login_req(Some(&good), Some(&good))).await;
        assert_eq!(s, StatusCode::OK, "{v}");
        // cookieless unsafe requests to other paths are not CSRF-checked (no ambient authority)
        let (s, _) = call(a(), req("POST", "/public", None, None)).await;
        assert_eq!(s, StatusCode::OK);
    }

    #[test]
    fn login_csrf_token_signature_and_expiry() {
        let p = pre();
        let now = Utc::now();
        let t = p.issue(now).unwrap();
        assert!(p.verify(&t, now));
        assert!(
            !p.verify(&t, now + chrono::Duration::seconds(600)),
            "expired"
        );
        let mut tampered = t.clone();
        tampered.replace_range(0..1, if t.starts_with('a') { "b" } else { "a" });
        assert!(!p.verify(&tampered, now));
        // extending the expiry invalidates the MAC
        let parts: Vec<&str> = t.split('.').collect();
        let longer = format!("{}.{}.{}", parts[0], now.timestamp() + 99_999, parts[2]);
        assert!(!p.verify(&longer, now));
        assert!(!p.verify("garbage", now));
        assert!(!p.verify("a.b.c.d", now));
        let c = p.set_cookie(&t);
        for attr in [
            "__Host-sutra_login_csrf=",
            "HttpOnly",
            "Secure",
            "SameSite=Strict",
            "Path=/",
        ] {
            assert!(c.contains(attr), "{attr} in {c}");
        }
        assert_eq!(
            PreSessionCsrf::new(KEY, false, 60).cookie_name(),
            "sutra_login_csrf"
        );
    }

    fn origin_app(policy: OriginPolicy) -> Router {
        Router::new()
            .route("/x", post(|| async { "ok" }).get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(policy, origin_guard))
    }

    async fn origin_call(
        headers: &[(&str, &str)],
        method: &str,
    ) -> (StatusCode, serde_json::Value) {
        let mut b = Request::builder().method(method).uri("/x");
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        call(
            origin_app(OriginPolicy::new(["https://app.sutra.example"])),
            b.body(Body::empty()).unwrap(),
        )
        .await
    }

    #[tokio::test]
    async fn cross_site_unsafe_requests_are_rejected() {
        let host = ("host", "erp.sutra.example");
        for bad in [
            vec![host, ("origin", "https://evil.example")],
            vec![host, ("origin", "null")],
            vec![host, ("sec-fetch-site", "cross-site")],
            vec![
                host,
                ("origin", "https://erp.sutra.example"),
                ("sec-fetch-site", "cross-site"),
            ],
        ] {
            let (s, v) = origin_call(&bad, "POST").await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{bad:?}");
            assert_eq!(v["errorCode"], "cross-site-request-rejected");
        }
        for ok in [
            vec![host],
            vec![host, ("origin", "https://erp.sutra.example")],
            vec![host, ("origin", "https://app.sutra.example/")],
            vec![host, ("sec-fetch-site", "same-origin")],
            vec![host, ("sec-fetch-site", "same-site")],
            vec![host, ("sec-fetch-site", "none")],
        ] {
            let (s, _) = origin_call(&ok, "POST").await;
            assert_eq!(s, StatusCode::OK, "{ok:?}");
        }
        // safe methods are never blocked (OIDC callback is a cross-site top-level GET)
        let (s, _) = origin_call(
            &[
                host,
                ("origin", "https://evil.example"),
                ("sec-fetch-site", "cross-site"),
            ],
            "GET",
        )
        .await;
        assert_eq!(s, StatusCode::OK);
    }

    #[tokio::test]
    async fn permission_denied_is_403_problem() {
        let (s, v) = call(
            app(ctx(vec![Role::Faculty])),
            req("POST", "/admin", Some(TOKEN), Some(CSRF)),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["errorCode"], "forbidden");
    }

    #[tokio::test]
    async fn contradicting_scope_headers_are_rejected() {
        let c = ctx(vec![Role::Student]);
        let own_tenant = c.scope.tenant_id.to_string();
        let own_inst = c.scope.institution_id.map(|i| i.to_string()).unwrap();
        let send = |t: Option<String>, i: Option<String>| {
            let mut r = req("GET", "/me", Some(TOKEN), None);
            if let Some(t) = t {
                r.headers_mut().insert(TENANT_HEADER, t.parse().unwrap());
            }
            if let Some(i) = i {
                r.headers_mut()
                    .insert(INSTITUTION_HEADER, i.parse().unwrap());
            }
            r
        };
        let (s, _) = call(
            app(c.clone()),
            send(Some(own_tenant.clone()), Some(own_inst)),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "matching headers are tolerated");
        let (s, v) = call(
            app(c.clone()),
            send(Some(TenantId::new().to_string()), None),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        assert_eq!(v["errorCode"], "forbidden");
        let (s, _) = call(
            app(c.clone()),
            send(Some(own_tenant), Some(InstitutionId::new().to_string())),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = call(app(c), send(Some("garbage".into()), None)).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
}
