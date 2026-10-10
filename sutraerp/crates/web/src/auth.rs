//! Session authentication, CSRF enforcement and the authenticated-principal extractor
//! (ADR-L5-0038 §3.4, ADR-L5-0024 D02, ADR-L5-0004).
//!
//! [`authenticate`] is a global middleware. For every request it:
//!
//! 1. reads the opaque session token from the session cookie (if any) and resolves it through
//!    the [`SessionResolver`] port (D01 session store);
//! 2. for **unsafe methods** (POST/PUT/PATCH/DELETE) carried by a live cookie session, requires
//!    the session's synchronizer token in `X-CSRF-Token` (constant-time compare) — except on the
//!    configured exempt paths (login and MFA verification, which have no session yet). Missing
//!    or wrong tokens get a `403 application/problem+json`;
//! 3. stores the resolved [`AuthContext`] in the request extensions.
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
use std::sync::Arc;
use sutra_kernel::auth::{AuthContext, Permission, SessionResolver};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// Header carrying the synchronizer CSRF token on unsafe requests.
pub const CSRF_HEADER: &str = "x-csrf-token";

/// Shared state of the [`authenticate`] middleware.
#[derive(Clone)]
pub struct SessionAuth {
    resolver: Arc<dyn SessionResolver>,
    cookie_name: Arc<str>,
    csrf_exempt: Arc<[String]>,
}

impl SessionAuth {
    /// `csrf_exempt` lists exact request paths that may be called unsafely without a CSRF token
    /// (only endpoints that *establish* a session belong here).
    pub fn new<I, P>(resolver: Arc<dyn SessionResolver>, cookie_name: &str, csrf_exempt: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: Into<String>,
    {
        Self {
            resolver,
            cookie_name: Arc::from(cookie_name),
            csrf_exempt: csrf_exempt.into_iter().map(Into::into).collect(),
        }
    }

    pub fn cookie_name(&self) -> &str {
        &self.cookie_name
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
    if is_unsafe(req.method())
        && !auth
            .csrf_exempt
            .iter()
            .any(|p| p.as_str() == req.uri().path())
    {
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
        }
    }

    fn app(c: AuthContext) -> Router {
        let auth = SessionAuth::new(Arc::new(Fixed(c)), "sid", ["/login"]);
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

    #[tokio::test]
    async fn exempt_paths_and_cookieless_requests_skip_csrf() {
        let a = || app(ctx(vec![Role::Administrator]));
        let (s, _) = call(a(), req("POST", "/login", Some(TOKEN), None)).await;
        assert_eq!(s, StatusCode::OK);
        let (s, _) = call(a(), req("POST", "/public", None, None)).await;
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
