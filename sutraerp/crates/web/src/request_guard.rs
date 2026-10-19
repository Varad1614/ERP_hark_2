//! Audit remediation A3 (H-1, D-1, D-2, D-3, M8, SEC-6, L-1): one error envelope for framework
//! rejections, body-less actions that refuse a body, and shared page bounds.
//!
//! Every module router uses Axum's own `Json`, `Path` and `Query` extractors (≈2000 handlers).
//! Their rejections are `text/plain` and echo serde's message — field names, the accepted field
//! list, enum variants, "UUID parsing failed: invalid character…" — to anyone who reaches the
//! handler, including signed-in callers who lack the route's permission (the permission check
//! runs inside the handler, after extraction). Instead of editing every handler, three
//! middlewares fix this centrally:
//!
//! * [`problem_envelope`] (outermost, inside the request-id layer) turns **every** error answer
//!   that is not already `application/problem+json` into an RFC 9457 problem with a generic,
//!   schema-free text and the correlation id. Extractor rejections keep their status (400
//!   malformed JSON / path / query, 413, 415, 422 well-formed JSON of the wrong shape); unknown
//!   methods (405) and panics (500) get the same envelope. The original framework text is
//!   logged (debug) under the correlation id — never returned. Field-level `validationErrors`
//!   remain only on business validation (`KernelError::Validation`), which handlers raise after
//!   their permission check.
//! * [`reject_unexpected_body`] — action-style `POST`/`PUT`/`PATCH`/`DELETE` routes whose OpenAPI
//!   operation has no `requestBody` (e.g. the receivables sweep
//!   `POST /finance/institutions/{id}/receivables`) used to ignore any body and answer 200. They
//!   now accept only an empty body, `{}` or `null`; anything else is `422 unexpected-request-body`.
//! * [`page_bounds`] — every authenticated `GET` with `limit` must ask for `1..=max` rows (200 by
//!   default, the route's own documented ceiling where it differs), `offset` must be a
//!   non-negative integer and `page` a positive one; `sort` / `order` / `orderBy` are refused
//!   because no list endpoint supports caller-chosen sorting (silently ignoring them hid typos and
//!   probes). Violations are `422 invalid-query-parameter` without echoing the value.

use crate::problem::{Problem, PROBLEM_CONTENT_TYPE};
use axum::body::Body;
use axum::extract::{MatchedPath, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use sutra_kernel::auth::AuthContext;

/// Generic detail of a body that could not be read into the operation's input.
pub const INVALID_BODY_DETAIL: &str = "The request body is invalid.";
/// Generic detail of a path parameter that could not be parsed.
pub const INVALID_PATH_DETAIL: &str = "The request path is invalid.";
/// Generic detail of a query string that could not be parsed.
pub const INVALID_QUERY_DETAIL: &str = "The query string is invalid.";

/// Largest framework error body inspected to classify it (Axum's texts are short).
const CLASSIFY_LIMIT: usize = 4096;

fn is_problem(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with(PROBLEM_CONTENT_TYPE))
}

/// Schema-free problem for a framework error answer with `status` and (framework) `text`.
fn classify(status: StatusCode, text: &str) -> Problem {
    let t = text.trim_start();
    let (code, title, detail) = if t.starts_with("Failed to parse the request body as JSON")
        || t.starts_with("Failed to deserialize the JSON body")
        || (t.starts_with("Failed to buffer the request body")
            && status != StatusCode::PAYLOAD_TOO_LARGE)
        || t.starts_with("Failed to deserialize form")
    {
        (
            "invalid-request-body",
            "Invalid request body",
            INVALID_BODY_DETAIL,
        )
    } else if t.starts_with("Invalid URL") && status.is_client_error() {
        (
            "invalid-path-parameter",
            "Invalid path parameter",
            INVALID_PATH_DETAIL,
        )
    } else if t.starts_with("Failed to deserialize query string") {
        (
            "invalid-query-parameter",
            "Invalid query parameter",
            INVALID_QUERY_DETAIL,
        )
    } else {
        match status {
            StatusCode::PAYLOAD_TOO_LARGE => (
                "payload-too-large",
                "Request body too large",
                "The request body is larger than this endpoint accepts.",
            ),
            StatusCode::UNSUPPORTED_MEDIA_TYPE => (
                "unsupported-media-type",
                "Unsupported media type",
                "The request body must be sent as application/json.",
            ),
            StatusCode::METHOD_NOT_ALLOWED => (
                "method-not-allowed",
                "Method not allowed",
                "This method is not supported on this resource.",
            ),
            StatusCode::NOT_FOUND => ("route-not-found", "Route not found", "Route not found."),
            StatusCode::UNPROCESSABLE_ENTITY => (
                "invalid-request-body",
                "Invalid request body",
                INVALID_BODY_DETAIL,
            ),
            s if s.is_server_error() => (
                "internal-error",
                "Internal error",
                "The request could not be completed.",
            ),
            _ => ("bad-request", "Bad request", "The request is invalid."),
        }
    };
    let status = if status.is_client_error() || status.is_server_error() {
        status
    } else {
        StatusCode::BAD_REQUEST
    };
    Problem::new(status, code, title).detail(detail)
}

/// Global middleware: every `>= 400` answer that is not already problem+json becomes one, with a
/// generic text (see the module docs). `/health/*` keeps its own JSON shape.
pub async fn problem_envelope(req: Request, next: Next) -> Response {
    let health = req.uri().path().starts_with("/health");
    let res = next.run(req).await;
    let status = res.status();
    if health || status.as_u16() < 400 || is_problem(res.headers()) {
        return res;
    }
    let (parts, body) = res.into_parts();
    let text = match http_body_util::Limited::new(body, CLASSIFY_LIMIT)
        .collect()
        .await
    {
        Ok(c) => String::from_utf8_lossy(&c.to_bytes()).into_owned(),
        Err(_) => String::new(),
    };
    tracing::debug!(
        status = status.as_u16(),
        framework_error = %text.chars().take(512).collect::<String>(),
        "framework error answered as problem+json"
    );
    let mut out = classify(status, &text).into_response();
    // Keep protocol headers of the original answer (`Allow`, `Retry-After`, licence status…).
    for (name, value) in &parts.headers {
        if name != header::CONTENT_TYPE && name != header::CONTENT_LENGTH {
            out.headers_mut().insert(name.clone(), value.clone());
        }
    }
    out
}

// ---------------------------------------------------------------- body-less actions (D-2)

/// `(METHOD, route template)` of the mutating operations that declare no request body.
#[derive(Clone, Debug, Default)]
pub struct BodylessRoutes(Arc<HashSet<(Method, String)>>);

impl BodylessRoutes {
    /// Collects the `POST` / `PUT` / `PATCH` / `DELETE` operations of `api` without `requestBody`.
    pub fn from_openapi(api: &utoipa::openapi::OpenApi) -> Self {
        let mut set = HashSet::new();
        for (path, item) in &api.paths.paths {
            for (method, op) in [
                (Method::POST, &item.post),
                (Method::PUT, &item.put),
                (Method::PATCH, &item.patch),
                (Method::DELETE, &item.delete),
            ] {
                if op.as_ref().is_some_and(|o| o.request_body.is_none()) {
                    set.insert((method, path.clone()));
                }
            }
        }
        Self(Arc::new(set))
    }

    pub fn contains(&self, method: &Method, route: &str) -> bool {
        self.0.contains(&(method.clone(), route.to_owned()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Largest "empty" body accepted by a body-less action (`{}` / `null` with whitespace).
const EMPTY_BODY_LIMIT: usize = 64;

fn unexpected_body() -> Response {
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "unexpected-request-body",
        "Unexpected request body",
    )
    .detail("This action does not take a request body; send none (or {}).")
    .into_response()
}

/// Route middleware: a body-less action refuses any body other than empty, `{}` or `null`.
pub async fn reject_unexpected_body(
    State(routes): State<BodylessRoutes>,
    req: Request,
    next: Next,
) -> Response {
    let applies = req
        .extensions()
        .get::<MatchedPath>()
        .is_some_and(|p| routes.contains(req.method(), p.as_str()));
    if !applies {
        return next.run(req).await;
    }
    let declared_empty = req
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.trim() == "0");
    if declared_empty {
        return next.run(req).await;
    }
    let (parts, body) = req.into_parts();
    let bytes = match http_body_util::Limited::new(body, EMPTY_BODY_LIMIT)
        .collect()
        .await
    {
        Ok(c) => c.to_bytes(),
        Err(_) => return unexpected_body(),
    };
    let trimmed = String::from_utf8_lossy(&bytes);
    let compact: String = trimmed.chars().filter(|c| !c.is_whitespace()).collect();
    if !(compact.is_empty() || compact == "{}" || compact == "null") {
        return unexpected_body();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

// ---------------------------------------------------------------- page bounds (L-1)

/// Default ceiling of `limit` on list endpoints.
pub const DEFAULT_MAX_LIMIT: i64 = 200;

/// Page-size ceilings: [`DEFAULT_MAX_LIMIT`] unless a route documents its own.
#[derive(Clone, Debug)]
pub struct PageBounds {
    default_max: i64,
    per_route: Arc<BTreeMap<String, i64>>,
}

impl Default for PageBounds {
    fn default() -> Self {
        Self::new(DEFAULT_MAX_LIMIT)
    }
}

impl PageBounds {
    pub fn new(default_max: i64) -> Self {
        Self {
            default_max,
            per_route: Arc::new(BTreeMap::new()),
        }
    }

    /// A route (template, e.g. `/api/v1/platform/imports/{id}/rows`) with its own ceiling.
    #[must_use]
    pub fn route(mut self, template: &str, max: i64) -> Self {
        Arc::make_mut(&mut self.per_route).insert(template.to_owned(), max);
        self
    }

    pub fn max_for(&self, route: Option<&str>) -> i64 {
        route
            .and_then(|r| self.per_route.get(r).copied())
            .unwrap_or(self.default_max)
    }

    /// `Err(message)` when the query string breaks the bounds (message names the parameter only).
    pub fn check(&self, route: Option<&str>, query: &str) -> Result<(), String> {
        let max = self.max_for(route);
        for pair in query.split('&').filter(|p| !p.is_empty()) {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            match key {
                "limit" => match value.parse::<i64>() {
                    Ok(n) if (1..=max).contains(&n) => {}
                    _ => return Err(format!("limit must be an integer from 1 to {max}")),
                },
                "offset" => match value.parse::<i64>() {
                    Ok(n) if n >= 0 => {}
                    _ => return Err("offset must be a non-negative integer".into()),
                },
                "page" => match value.parse::<i64>() {
                    Ok(n) if n >= 1 => {}
                    _ => return Err("page must be a positive integer".into()),
                },
                "sort" | "order" | "orderBy" | "order_by" | "sortBy" => {
                    return Err(format!("{key} is not supported by this endpoint"))
                }
                _ => {}
            }
        }
        Ok(())
    }
}

/// Route middleware: bounds of `limit` / `offset` / `page` and refusal of `sort` on signed-in
/// `GET`s (anonymous requests reach the handler, which answers 401 first).
pub async fn page_bounds(State(bounds): State<PageBounds>, req: Request, next: Next) -> Response {
    if req.method() != Method::GET || req.extensions().get::<AuthContext>().is_none() {
        return next.run(req).await;
    }
    let Some(query) = req.uri().query() else {
        return next.run(req).await;
    };
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str);
    if let Err(msg) = bounds.check(route, query) {
        let mut p = Problem::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid-query-parameter",
            "Invalid query parameter",
        )
        .detail(msg.clone());
        let field = msg.split_whitespace().next().unwrap_or("query").to_owned();
        p.validation_errors = vec![sutra_kernel::error::FieldViolation::new(
            field,
            "out-of-range",
            msg,
        )];
        return p.into_response();
    }
    next.run(req).await
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use tower::ServiceExt;

    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields, rename_all = "camelCase")]
    #[allow(dead_code)]
    struct Suspend {
        context_id: uuid::Uuid,
        reason: String,
        category: Category,
    }

    #[derive(serde::Deserialize)]
    #[serde(rename_all = "snake_case")]
    #[allow(dead_code)]
    enum Category {
        Academic,
        Discipline,
    }

    #[derive(serde::Deserialize)]
    #[allow(dead_code)]
    struct Q {
        as_of: chrono::NaiveDate,
    }

    async fn create(Json(_): Json<Suspend>) -> StatusCode {
        StatusCode::CREATED
    }

    fn app() -> Router {
        Router::new()
            .route("/things", post(create))
            .route(
                "/things/{id}",
                get(|axum::extract::Path(_): axum::extract::Path<uuid::Uuid>| async { "ok" }),
            )
            .route(
                "/report",
                get(|axum::extract::Query(_): axum::extract::Query<Q>| async { "ok" }),
            )
            .layer(axum::middleware::from_fn(problem_envelope))
            .layer(axum::middleware::from_fn(crate::request_id::request_id))
    }

    async fn send(req: Request) -> (StatusCode, HeaderMap, String) {
        let res = app().oneshot(req).await.unwrap();
        let s = res.status();
        let h = res.headers().clone();
        let b = res.into_body().collect().await.unwrap().to_bytes();
        (s, h, String::from_utf8_lossy(&b).into_owned())
    }

    fn json_post(body: &str) -> Request {
        Request::post("/things")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    fn assert_generic(s: StatusCode, h: &HeaderMap, b: &str, status: StatusCode) {
        assert_eq!(s, status, "{b}");
        assert_eq!(h[header::CONTENT_TYPE], PROBLEM_CONTENT_TYPE, "{b}");
        let v: serde_json::Value = serde_json::from_str(b).unwrap();
        assert!(v["correlationId"].is_string(), "{b}");
        assert!(v.get("validationErrors").is_none(), "{b}");
        for leak in [
            "contextId",
            "context_id",
            "reason",
            "category",
            "academic",
            "discipline",
            "as_of",
            "UUID",
            "expected",
            "unknown field",
            "missing field",
            "line",
            "Failed",
            "serde",
        ] {
            assert!(!b.contains(leak), "{leak} leaked: {b}");
        }
    }

    #[tokio::test]
    async fn malformed_json_is_a_generic_400_problem() {
        let (s, h, b) = send(json_post("{\"contextId\":")).await;
        assert_generic(s, &h, &b, StatusCode::BAD_REQUEST);
        assert!(b.contains("invalid-request-body"), "{b}");
        assert!(b.contains(INVALID_BODY_DETAIL), "{b}");
    }

    #[tokio::test]
    async fn wrong_shape_unknown_field_enum_and_duplicate_keys_do_not_leak() {
        for body in [
            "{}",
            "{\"bogus\":1}",
            "{\"contextId\":\"0192f5a0-7c3e-7000-8000-0000000000aa\",\"reason\":\"x\",\"category\":\"nope\"}",
            "{\"contextId\":\"0192f5a0-7c3e-7000-8000-0000000000aa\",\"reason\":\"x\",\"reason\":\"y\",\"category\":\"academic\"}",
            "[]",
        ] {
            let (s, h, b) = send(json_post(body)).await;
            assert_generic(s, &h, &b, StatusCode::UNPROCESSABLE_ENTITY);
        }
    }

    #[tokio::test]
    async fn wrong_content_type_is_415_problem() {
        let req = Request::post("/things")
            .header(header::CONTENT_TYPE, "text/plain")
            .body(Body::from("{}"))
            .unwrap();
        let (s, h, b) = send(req).await;
        assert_generic(s, &h, &b, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn bad_uuid_and_bad_query_are_generic_400_problems() {
        let (s, h, b) = send(
            Request::get("/things/not-a-uuid")
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_generic(s, &h, &b, StatusCode::BAD_REQUEST);
        assert!(b.contains("invalid-path-parameter"), "{b}");
        for uri in ["/report", "/report?as_of=yesterday"] {
            let (s, h, b) = send(Request::get(uri).body(Body::empty()).unwrap()).await;
            assert_generic(s, &h, &b, StatusCode::BAD_REQUEST);
            assert!(b.contains("invalid-query-parameter"), "{b}");
        }
    }

    #[tokio::test]
    async fn unknown_method_keeps_allow_header() {
        let (s, h, b) = send(Request::delete("/things").body(Body::empty()).unwrap()).await;
        assert_eq!(s, StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(h[header::CONTENT_TYPE], PROBLEM_CONTENT_TYPE, "{b}");
        assert!(h.contains_key(header::ALLOW));
    }

    #[tokio::test]
    async fn existing_problems_pass_untouched() {
        let app = Router::new()
            .route(
                "/p",
                get(|| async {
                    Problem::new(StatusCode::CONFLICT, "conflict", "Conflict").detail("kept")
                }),
            )
            .layer(axum::middleware::from_fn(problem_envelope));
        let res = app
            .oneshot(Request::get("/p").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let b = res.into_body().collect().await.unwrap().to_bytes();
        assert!(String::from_utf8_lossy(&b).contains("kept"));
    }

    fn bodyless_app() -> Router {
        let mut api = utoipa::openapi::OpenApi::default();
        api.paths.paths.insert(
            "/sweep".into(),
            utoipa::openapi::path::PathItem::new(
                utoipa::openapi::path::HttpMethod::Post,
                utoipa::openapi::path::Operation::new(),
            ),
        );
        let routes = BodylessRoutes::from_openapi(&api);
        assert_eq!(routes.len(), 1);
        Router::new()
            .route("/sweep", post(|| async { "swept" }))
            .layer(axum::middleware::from_fn_with_state(
                routes,
                reject_unexpected_body,
            ))
    }

    #[tokio::test]
    async fn bodyless_action_accepts_only_empty_bodies() {
        for (body, ok) in [
            ("", true),
            ("{}", true),
            (" { } ", true),
            ("null", true),
            ("{\"amount\":-999999}", false),
            ("[]", false),
            ("x", false),
        ] {
            let res = bodyless_app()
                .oneshot(
                    Request::post("/sweep")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(body))
                        .unwrap(),
                )
                .await
                .unwrap();
            let expected = if ok {
                StatusCode::OK
            } else {
                StatusCode::UNPROCESSABLE_ENTITY
            };
            assert_eq!(res.status(), expected, "{body:?}");
        }
    }

    #[test]
    fn page_bounds_rules() {
        let b = PageBounds::default().route("/rows", 5000);
        assert!(b.check(None, "limit=1").is_ok());
        assert!(b.check(None, "limit=200&cursor=abc").is_ok());
        assert!(b.check(None, "status=open").is_ok());
        for bad in [
            "limit=0",
            "limit=-1",
            "limit=201",
            "limit=100000",
            "limit=abc",
            "limit=",
            "offset=-1",
            "page=0",
            "page=-5",
            "sort=secretField",
            "sort=%3BDROP",
            "orderBy=name",
            "limit=10&limit=999",
        ] {
            assert!(b.check(None, bad).is_err(), "{bad}");
        }
        assert!(b.check(Some("/rows"), "limit=5000").is_ok());
        assert!(b.check(Some("/rows"), "limit=5001").is_err());
        assert!(b.check(None, "offset=0&page=1").is_ok());
    }
}
