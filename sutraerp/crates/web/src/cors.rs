//! Part 1 · Stage 3 (S3-config): CORS from an explicit allow-list (`SUTRA_ALLOWED_ORIGINS`).
//!
//! The SPA is served from the API's own origin (nginx / ingress proxy `/api`), so by default no
//! CORS header is ever sent and browsers keep cross-origin reads blocked. An origin named in the
//! allow-list (exact `scheme://host[:port]`, never a wildcard — the config loader refuses `*` and
//! `null`) gets a credentialed CORS answer that echoes that one origin, with `Vary: Origin`.
//! Every other origin gets no CORS header (and unsafe requests from it are still refused by
//! [`crate::auth::origin_guard`]).

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

/// Allowed cross-origin callers.
#[derive(Clone, Debug, Default)]
pub struct CorsPolicy {
    allowed: Arc<[String]>,
}

const ALLOW_METHODS: &str = "GET, POST, PUT, PATCH, DELETE";
const ALLOW_HEADERS: &str =
    "content-type, if-match, if-none-match, idempotency-key, x-csrf-token, x-request-id";
const EXPOSE_HEADERS: &str = "etag, location, retry-after, x-request-id";

impl CorsPolicy {
    pub fn new<I, P>(allowed: I) -> Self
    where
        I: IntoIterator<Item = P>,
        P: AsRef<str>,
    {
        Self {
            allowed: allowed
                .into_iter()
                .map(|o| o.as_ref().trim().trim_end_matches('/').to_ascii_lowercase())
                .filter(|o| !o.is_empty() && o != "*" && o != "null")
                .collect(),
        }
    }

    fn allows(&self, origin: &str) -> bool {
        let o = origin.trim().to_ascii_lowercase();
        self.allowed.contains(&o)
    }
}

fn decorate(res: &mut Response, origin: &HeaderValue) {
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin.clone());
    h.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSE_HEADERS),
    );
    h.append(header::VARY, HeaderValue::from_static("Origin"));
}

/// Middleware: answers preflights of allowed origins and decorates their responses.
pub async fn cors(State(policy): State<Arc<CorsPolicy>>, req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .filter(|o| o.to_str().is_ok_and(|s| policy.allows(s)))
        .cloned();
    let Some(origin) = origin else {
        let mut res = next.run(req).await;
        res.headers_mut()
            .append(header::VARY, HeaderValue::from_static("Origin"));
        return res;
    };
    let preflight = req.method() == Method::OPTIONS
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    if preflight {
        let mut res = Response::new(Body::empty());
        *res.status_mut() = StatusCode::NO_CONTENT;
        decorate(&mut res, &origin);
        let h = res.headers_mut();
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(ALLOW_METHODS),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(ALLOW_HEADERS),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        return res;
    }
    let mut res = next.run(req).await;
    decorate(&mut res, &origin);
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn app(allowed: &[&str]) -> Router {
        Router::new().route("/r", get(|| async { "ok" })).layer(
            axum::middleware::from_fn_with_state(Arc::new(CorsPolicy::new(allowed)), cors),
        )
    }

    async fn send(app: Router, req: Request) -> Response {
        app.oneshot(req).await.unwrap()
    }

    #[tokio::test]
    async fn allowed_origin_is_echoed_never_a_wildcard() {
        let res = send(
            app(&["https://erp.example.edu"]),
            Request::get("/r")
                .header("origin", "https://erp.example.edu")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        let h = res.headers();
        assert_eq!(h["access-control-allow-origin"], "https://erp.example.edu");
        assert_eq!(h["access-control-allow-credentials"], "true");
        assert!(h.get_all("vary").iter().any(|v| v == "Origin"));
    }

    #[tokio::test]
    async fn unknown_origins_get_no_cors_headers() {
        for origin in [
            "https://evil.example",
            "null",
            "https://erp.example.edu.evil.example",
        ] {
            let res = send(
                app(&["https://erp.example.edu"]),
                Request::get("/r")
                    .header("origin", origin)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await;
            assert!(
                res.headers().get("access-control-allow-origin").is_none(),
                "{origin}"
            );
        }
        // A wildcard in the list never matches anything (the config loader refuses it too).
        let res = send(
            app(&["*", "null"]),
            Request::get("/r")
                .header("origin", "https://evil.example")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(res.headers().get("access-control-allow-origin").is_none());
        let res = send(
            app(&["*", "null"]),
            Request::get("/r")
                .header("origin", "null")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert!(res.headers().get("access-control-allow-origin").is_none());
    }

    #[tokio::test]
    async fn preflight_only_for_allowed_origins() {
        let pre = |origin: &str| {
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/r")
                .header("origin", origin)
                .header("access-control-request-method", "POST")
                .body(Body::empty())
                .unwrap()
        };
        let res = send(
            app(&["https://erp.example.edu"]),
            pre("https://erp.example.edu"),
        )
        .await;
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert!(res.headers()["access-control-allow-headers"]
            .to_str()
            .unwrap()
            .contains("x-csrf-token"));
        let res = send(
            app(&["https://erp.example.edu"]),
            pre("https://evil.example"),
        )
        .await;
        assert_ne!(res.status(), StatusCode::NO_CONTENT);
        assert!(res.headers().get("access-control-allow-origin").is_none());
    }
}
