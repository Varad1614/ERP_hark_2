//! Part 1 · Stage 3 (S3-config): per-client rate limiting of the public (unauthenticated) API.
//!
//! Runs **inside** the session layer, so it knows whether the request carries a live session:
//!
//! * `credentials` — password / second-factor endpoints (`POST /api/v1/auth/login`,
//!   `/api/v1/auth/mfa/*verify*`, step-up, WebAuthn assertion): always limited, session or not
//!   (guessing a TOTP code needs a pre-session, not a session);
//! * `auth` — the rest of `/api/v1/auth/*` reached without a session (CSRF token, OIDC start /
//!   callback, back-channel logout);
//! * `public` — any other `/api/*` request without a live session (the payment webhook, probes of
//!   protected routes). Authenticated traffic is not limited here (it is bounded by the session).
//!
//! `/health/*` is never limited (kubelet / load-balancer probes). The client is the peer address
//! (`ConnectInfo`), or — behind `trusted_proxy_hops` reverse proxies that each append to
//! `X-Forwarded-For` — the address the outermost trusted proxy saw. A token bucket per
//! (class, client) refills continuously; an exhausted bucket answers `429` problem+json with
//! `Retry-After`. The table is bounded (`max_clients`; idle full buckets are evicted first).

use crate::problem::Problem;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use sutra_kernel::auth::AuthContext;

/// Bucket size and refill of one class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limit {
    /// Sustained requests per minute.
    pub per_minute: u32,
    /// Bucket capacity (requests allowed at once).
    pub burst: u32,
}

/// Settings of [`RateLimiter`].
#[derive(Clone, Debug)]
pub struct RateLimitSettings {
    pub enabled: bool,
    pub credentials: Limit,
    pub auth: Limit,
    pub public: Limit,
    /// Reverse proxies in front of the API that append to `X-Forwarded-For` (0 = use the peer).
    pub trusted_proxy_hops: usize,
    pub max_clients: usize,
}

impl Default for RateLimitSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            credentials: Limit {
                per_minute: 30,
                burst: 10,
            },
            auth: Limit {
                per_minute: 300,
                burst: 100,
            },
            public: Limit {
                per_minute: 600,
                burst: 200,
            },
            trusted_proxy_hops: 0,
            max_clients: 100_000,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Class {
    Credentials,
    Auth,
    Public,
}

impl Class {
    fn name(self) -> &'static str {
        match self {
            Class::Credentials => "credentials",
            Class::Auth => "auth",
            Class::Public => "public",
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// Shared limiter state (one per process).
pub struct RateLimiter {
    settings: RateLimitSettings,
    buckets: Mutex<HashMap<(Class, String), Bucket>>,
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("settings", &self.settings)
            .finish_non_exhaustive()
    }
}

/// `true` for the password / second-factor endpoints.
fn is_credential_path(method: &Method, path: &str) -> bool {
    *method == Method::POST
        && (path == "/api/v1/auth/login"
            || path == "/api/v1/auth/mfa"
            || path == "/api/v1/auth/mfa/verify"
            || path == "/api/v1/auth/mfa/step-up"
            || path == "/api/v1/auth/mfa/webauthn/verify"
            || path == "/api/v1/auth/mfa/webauthn/assertion"
            || path == "/api/v1/auth/mfa/webauthn/step-up"
            || path == "/api/v1/auth/mfa/totp/confirm")
}

impl RateLimiter {
    pub fn new(settings: RateLimitSettings) -> Self {
        Self {
            settings,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    pub fn settings(&self) -> &RateLimitSettings {
        &self.settings
    }

    fn class(&self, method: &Method, path: &str, authenticated: bool) -> Option<Class> {
        if !path.starts_with("/api/") {
            return None; // /health/*, anything outside the API
        }
        if is_credential_path(method, path) {
            return Some(Class::Credentials);
        }
        if authenticated {
            return None;
        }
        if path.starts_with("/api/v1/auth/") {
            return Some(Class::Auth);
        }
        Some(Class::Public)
    }

    fn limit(&self, class: Class) -> Limit {
        match class {
            Class::Credentials => self.settings.credentials,
            Class::Auth => self.settings.auth,
            Class::Public => self.settings.public,
        }
    }

    /// Client key: the peer, or the address seen by the outermost trusted proxy.
    pub fn client_key(&self, headers: &HeaderMap, peer: Option<SocketAddr>) -> String {
        let hops = self.settings.trusted_proxy_hops;
        if hops > 0 {
            let chain: Vec<&str> = headers
                .get_all("x-forwarded-for")
                .iter()
                .filter_map(|v| v.to_str().ok())
                .flat_map(|v| v.split(','))
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            // Each trusted proxy appended the address it received the request from; the entry
            // `hops` from the end was written by the outermost trusted proxy.
            if chain.len() >= hops {
                return chain[chain.len() - hops].to_owned();
            }
            if let Some(first) = chain.first() {
                return (*first).to_owned();
            }
        }
        peer.map_or_else(|| "unknown".to_owned(), |p| p.ip().to_string())
    }

    /// Takes one token; `Err(seconds until the next token)` when the bucket is empty.
    fn take(&self, class: Class, client: String, now: Instant) -> Result<(), u64> {
        let limit = self.limit(class);
        let rate = f64::from(limit.per_minute.max(1)) / 60.0; // tokens per second
        let cap = f64::from(limit.burst.max(1));
        let mut map = match self.buckets.lock() {
            Ok(m) => m,
            Err(poisoned) => poisoned.into_inner(),
        };
        if map.len() >= self.settings.max_clients && !map.contains_key(&(class, client.clone())) {
            // Bounded memory: drop buckets that refilled completely (idle clients); if none, the
            // oldest half (a flood of distinct addresses must not grow the table without bound).
            let settings = &self.settings;
            map.retain(|(c, _), b| {
                let l = match c {
                    Class::Credentials => settings.credentials,
                    Class::Auth => settings.auth,
                    Class::Public => settings.public,
                };
                let r = f64::from(l.per_minute.max(1)) / 60.0;
                b.tokens + now.duration_since(b.at).as_secs_f64() * r < f64::from(l.burst.max(1))
            });
            if map.len() >= self.settings.max_clients {
                let mut ages: Vec<Instant> = map.values().map(|b| b.at).collect();
                ages.sort();
                let cut = ages[ages.len() / 2];
                map.retain(|_, b| b.at > cut);
            }
        }
        let b = map.entry((class, client)).or_insert(Bucket {
            tokens: cap,
            at: now,
        });
        b.tokens = (b.tokens + now.duration_since(b.at).as_secs_f64() * rate).min(cap);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            let wait = ((1.0 - b.tokens) / rate).ceil();
            Err(if wait.is_finite() && wait >= 1.0 {
                wait as u64
            } else {
                1
            })
        }
    }
}

/// Middleware (inside the session layer, see the module docs).
pub async fn rate_limit(
    State(limiter): State<Arc<RateLimiter>>,
    req: Request,
    next: Next,
) -> Response {
    if !limiter.settings.enabled {
        return next.run(req).await;
    }
    let authenticated = req.extensions().get::<AuthContext>().is_some();
    let Some(class) = limiter.class(req.method(), req.uri().path(), authenticated) else {
        return next.run(req).await;
    };
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let client = limiter.client_key(req.headers(), peer);
    match limiter.take(class, client.clone(), Instant::now()) {
        Ok(()) => next.run(req).await,
        Err(retry) => {
            tracing::warn!(class = class.name(), client = %client, path = %req.uri().path(), "rate limited");
            let mut res = Problem::new(
                StatusCode::TOO_MANY_REQUESTS,
                "rate-limited",
                "Too many requests",
            )
            .detail(format!(
                "too many {} requests from this client; retry in {retry} s",
                class.name()
            ))
            .into_response();
            if let Ok(v) = HeaderValue::from_str(&retry.to_string()) {
                res.headers_mut().insert(header::RETRY_AFTER, v);
            }
            res
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::{get, post};
    use axum::Router;
    use std::time::Duration;
    use tower::ServiceExt;

    fn settings() -> RateLimitSettings {
        RateLimitSettings {
            enabled: true,
            credentials: Limit {
                per_minute: 6,
                burst: 3,
            },
            auth: Limit {
                per_minute: 60,
                burst: 5,
            },
            public: Limit {
                per_minute: 60,
                burst: 4,
            },
            trusted_proxy_hops: 1,
            max_clients: 1000,
        }
    }

    fn app(s: RateLimitSettings) -> Router {
        Router::new()
            .route("/api/v1/auth/login", post(|| async { "ok" }))
            .route("/api/v1/auth/csrf", get(|| async { "ok" }))
            .route("/api/v1/finance/gateway/webhook", post(|| async { "ok" }))
            .route("/health/live", get(|| async { "ok" }))
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(RateLimiter::new(s)),
                rate_limit,
            ))
    }

    fn req(method: &str, path: &str, ip: &str) -> Request {
        Request::builder()
            .method(method)
            .uri(path)
            .header("x-forwarded-for", ip)
            .body(Body::empty())
            .unwrap()
    }

    async fn statuses(app: &Router, method: &str, path: &str, ip: &str, n: usize) -> Vec<u16> {
        let mut v = vec![];
        for _ in 0..n {
            let res = app.clone().oneshot(req(method, path, ip)).await.unwrap();
            v.push(res.status().as_u16());
        }
        v
    }

    #[tokio::test]
    async fn password_guessing_is_throttled_per_client() {
        let app = app(settings());
        let s = statuses(&app, "POST", "/api/v1/auth/login", "203.0.113.7", 5).await;
        assert_eq!(s, vec![200, 200, 200, 429, 429], "burst of 3");
        let res = app
            .clone()
            .oneshot(req("POST", "/api/v1/auth/login", "203.0.113.7"))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry: u64 = res.headers()["retry-after"]
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=10).contains(&retry), "{retry}");
        assert_eq!(
            res.headers()["content-type"],
            crate::problem::PROBLEM_CONTENT_TYPE
        );
        // Another client is unaffected.
        let s = statuses(&app, "POST", "/api/v1/auth/login", "198.51.100.9", 1).await;
        assert_eq!(s, vec![200]);
    }

    #[tokio::test]
    async fn public_endpoints_are_limited_and_health_is_not() {
        let app = app(settings());
        let s = statuses(
            &app,
            "POST",
            "/api/v1/finance/gateway/webhook",
            "192.0.2.1",
            6,
        )
        .await;
        assert_eq!(&s[..4], &[200, 200, 200, 200]);
        assert_eq!(&s[4..], &[429, 429]);
        let s = statuses(&app, "GET", "/api/v1/auth/csrf", "192.0.2.1", 5).await;
        assert!(s.iter().all(|c| *c == 200), "separate class: {s:?}");
        let s = statuses(&app, "GET", "/health/live", "192.0.2.1", 50).await;
        assert!(s.iter().all(|c| *c == 200));
    }

    #[tokio::test]
    async fn disabled_limiter_passes_everything() {
        let mut s = settings();
        s.enabled = false;
        let app = app(s);
        let s = statuses(&app, "POST", "/api/v1/auth/login", "203.0.113.7", 20).await;
        assert!(s.iter().all(|c| *c == 200));
    }

    #[test]
    fn client_key_trusts_only_the_configured_proxies() {
        let mut h = HeaderMap::new();
        h.insert(
            "x-forwarded-for",
            HeaderValue::from_static("6.6.6.6, 203.0.113.7, 10.0.0.2"),
        );
        let peer = Some(SocketAddr::from(([10, 0, 0, 3], 4000)));
        let mut s = settings();
        s.trusted_proxy_hops = 0;
        assert_eq!(RateLimiter::new(s.clone()).client_key(&h, peer), "10.0.0.3");
        s.trusted_proxy_hops = 1;
        assert_eq!(RateLimiter::new(s.clone()).client_key(&h, peer), "10.0.0.2");
        s.trusted_proxy_hops = 2;
        // A spoofed left-most entry (6.6.6.6) is ignored.
        assert_eq!(
            RateLimiter::new(s.clone()).client_key(&h, peer),
            "203.0.113.7"
        );
        assert_eq!(
            RateLimiter::new(s).client_key(&HeaderMap::new(), None),
            "unknown"
        );
    }

    #[test]
    fn buckets_refill_and_the_table_stays_bounded() {
        let mut s = settings();
        s.max_clients = 8;
        let l = RateLimiter::new(s);
        let t0 = Instant::now();
        for _ in 0..3 {
            assert!(l.take(Class::Credentials, "a".into(), t0).is_ok());
        }
        assert!(l.take(Class::Credentials, "a".into(), t0).is_err());
        // 6/min = one token every 10 s.
        assert!(l
            .take(Class::Credentials, "a".into(), t0 + Duration::from_secs(10))
            .is_ok());
        for i in 0..100 {
            let _ = l.take(Class::Public, format!("c{i}"), t0 + Duration::from_secs(11));
        }
        assert!(l.buckets.lock().unwrap().len() <= 8);
    }
}
