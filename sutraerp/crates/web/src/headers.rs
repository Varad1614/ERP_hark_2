//! Browser-hardening headers on every API response (Part 1 S3, SEC-9D-006 / SEC-9D-012).
//!
//! The pilot ingress routes `/api` straight to the API service, so JSON responses cannot rely
//! on the web container's nginx for these headers. [`security_headers`] adds, when the handler
//! did not set them itself:
//!
//! * `X-Content-Type-Options: nosniff` — no MIME sniffing of JSON / downloads;
//! * `Cache-Control: no-store` — API data (personal records, CSRF tokens) is never cached by
//!   the browser or a shared proxy;
//! * `X-Frame-Options: DENY` — no framing of API responses;
//! * `Referrer-Policy: no-referrer`;
//! * `Cross-Origin-Resource-Policy: same-origin` — no cross-origin embedding of responses;
//! * `Strict-Transport-Security: max-age=31536000; includeSubDomains` when HSTS is on
//!   (`SUTRA_HSTS`, default on in production; browsers ignore it over plain http).

use axum::extract::{Request, State};
use axum::http::header::{
    HeaderName, CACHE_CONTROL, REFERRER_POLICY, STRICT_TRANSPORT_SECURITY, X_CONTENT_TYPE_OPTIONS,
    X_FRAME_OPTIONS,
};
use axum::http::{HeaderMap, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;

/// HSTS value (one year, sub-domains included).
pub const HSTS_VALUE: &str = "max-age=31536000; includeSubDomains";

/// State of [`security_headers`].
#[derive(Clone, Copy, Debug, Default)]
pub struct SecurityHeaders {
    /// Emit `Strict-Transport-Security`.
    pub hsts: bool,
}

fn set_if_absent(h: &mut HeaderMap, name: HeaderName, value: &'static str) {
    if !h.contains_key(&name) {
        h.insert(name, HeaderValue::from_static(value));
    }
}

/// Global middleware adding the hardening headers above to every response.
pub async fn security_headers(
    State(cfg): State<SecurityHeaders>,
    req: Request,
    next: Next,
) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    set_if_absent(h, X_CONTENT_TYPE_OPTIONS, "nosniff");
    set_if_absent(h, CACHE_CONTROL, "no-store");
    set_if_absent(h, X_FRAME_OPTIONS, "DENY");
    set_if_absent(h, REFERRER_POLICY, "no-referrer");
    set_if_absent(
        h,
        HeaderName::from_static("cross-origin-resource-policy"),
        "same-origin",
    );
    if cfg.hsts {
        set_if_absent(h, STRICT_TRANSPORT_SECURITY, HSTS_VALUE);
    }
    res
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    fn app(hsts: bool) -> Router {
        Router::new()
            .route("/plain", get(|| async { "x" }))
            .route(
                "/cached",
                get(|| async { ([(CACHE_CONTROL, "private, max-age=60")], "y").into_response() }),
            )
            .layer(axum::middleware::from_fn_with_state(
                SecurityHeaders { hsts },
                security_headers,
            ))
    }

    async fn headers(app: Router, path: &str) -> (StatusCode, HeaderMap) {
        let res = app
            .oneshot(axum::http::Request::get(path).body(Body::empty()).unwrap())
            .await
            .unwrap();
        (res.status(), res.headers().clone())
    }

    #[tokio::test]
    async fn adds_hardening_headers_without_overriding_handlers() {
        let (_, h) = headers(app(false), "/plain").await;
        assert_eq!(h[X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(h[CACHE_CONTROL], "no-store");
        assert_eq!(h[X_FRAME_OPTIONS], "DENY");
        assert_eq!(h[REFERRER_POLICY], "no-referrer");
        assert_eq!(h["cross-origin-resource-policy"], "same-origin");
        assert!(h.get(STRICT_TRANSPORT_SECURITY).is_none());
        let (_, h) = headers(app(true), "/cached").await;
        assert_eq!(h[CACHE_CONTROL], "private, max-age=60", "handler wins");
        assert_eq!(h[STRICT_TRANSPORT_SECURITY], HSTS_VALUE);
        // Unrouted requests (404) carry them too.
        let (s, h) = headers(app(true), "/nope").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
        assert_eq!(h[X_CONTENT_TYPE_OPTIONS], "nosniff");
    }
}
