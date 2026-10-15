//! Part 1 · Stage 3 (S3-config): HTTP response hardening for every API response.
//!
//! The API answers JSON (and problem+json) only, so its Content-Security-Policy forbids
//! everything: a response rendered as a document by mistake can load nothing, run nothing and be
//! framed nowhere. Headers a handler set itself (e.g. a download's `Cache-Control`) are kept;
//! only missing ones are added. `Strict-Transport-Security` is sent when a max-age is configured
//! (production requires one, see `apps/api/src/config_http.rs`).

use axum::extract::{Request, State};
use axum::http::{header, HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use std::sync::Arc;

/// The API's document policy: nothing may load, run, submit or frame.
pub const API_CSP: &str =
    "default-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'";

/// Settings of [`security_headers`].
#[derive(Clone, Debug, Default)]
pub struct SecurityHeaders {
    /// `Strict-Transport-Security: max-age=<n>; includeSubDomains`; `0` = not sent.
    pub hsts_max_age_secs: u64,
}

impl SecurityHeaders {
    fn fixed(&self) -> Vec<(HeaderName, HeaderValue)> {
        let mut v = vec![
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY")),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(API_CSP),
            ),
            (
                header::REFERRER_POLICY,
                HeaderValue::from_static("no-referrer"),
            ),
            (
                HeaderName::from_static("cross-origin-resource-policy"),
                HeaderValue::from_static("same-origin"),
            ),
            (
                HeaderName::from_static("permissions-policy"),
                HeaderValue::from_static(
                    "camera=(), microphone=(), geolocation=(), payment=(), usb=()",
                ),
            ),
            // Personal data must not stay in shared or browser caches.
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ];
        if self.hsts_max_age_secs > 0 {
            if let Ok(h) = HeaderValue::from_str(&format!(
                "max-age={}; includeSubDomains",
                self.hsts_max_age_secs
            )) {
                v.push((header::STRICT_TRANSPORT_SECURITY, h));
            }
        }
        v
    }
}

/// Middleware: adds the hardening headers a response does not already carry.
pub async fn security_headers(
    State(cfg): State<Arc<SecurityHeaders>>,
    req: Request,
    next: Next,
) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    for (name, value) in cfg.fixed() {
        if !h.contains_key(&name) {
            h.insert(name, value);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn app(hsts: u64) -> Router {
        Router::new()
            .route("/json", get(|| async { "{}" }))
            .route(
                "/download",
                get(|| async {
                    (
                        [(header::CACHE_CONTROL, "private, max-age=60")],
                        "file".to_owned(),
                    )
                }),
            )
            .layer(axum::middleware::from_fn_with_state(
                Arc::new(SecurityHeaders {
                    hsts_max_age_secs: hsts,
                }),
                security_headers,
            ))
    }

    async fn headers(app: Router, path: &str) -> axum::http::HeaderMap {
        app.oneshot(Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap()
            .headers()
            .clone()
    }

    #[tokio::test]
    async fn every_response_carries_the_hardening_headers() {
        let h = headers(app(31_536_000), "/json").await;
        assert_eq!(h["x-content-type-options"], "nosniff");
        assert_eq!(h["x-frame-options"], "DENY");
        assert_eq!(h["content-security-policy"], API_CSP);
        let csp = h["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("frame-ancestors 'none'"));
        assert!(!csp.contains("unsafe-inline") && !csp.contains('*'));
        assert_eq!(h["referrer-policy"], "no-referrer");
        assert_eq!(h["cross-origin-resource-policy"], "same-origin");
        assert_eq!(h["cache-control"], "no-store");
        assert_eq!(
            h["strict-transport-security"],
            "max-age=31536000; includeSubDomains"
        );
        // Unknown routes (fallback 404s) are covered too.
        let h = headers(app(31_536_000), "/nope").await;
        assert_eq!(h["x-frame-options"], "DENY");
    }

    #[tokio::test]
    async fn handler_headers_win_and_hsts_is_optional() {
        let h = headers(app(0), "/download").await;
        assert_eq!(h["cache-control"], "private, max-age=60");
        assert!(h.get("strict-transport-security").is_none());
        assert_eq!(h["x-content-type-options"], "nosniff");
    }
}
