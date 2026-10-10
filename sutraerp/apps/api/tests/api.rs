//! Router-level tests of the assembled application. No database required.

#![allow(clippy::unwrap_used)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use sutra_api::app::{router, AppContext};
use sutra_api::config::DEV_TENANT_ID;
use tower::ServiceExt;

async fn call(req: Request<Body>) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    send(&router(AppContext::for_spec()), req).await
}

/// Sends one request to a shared app (sessions live in the app's in-memory store).
async fn send(
    app: &Router,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, serde_json::Value) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

#[tokio::test]
async fn liveness_and_request_id() {
    let (s, h, body) = call(
        Request::get("/health/live")
            .header("x-request-id", "test-1")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["x-request-id"], "test-1");
    assert_eq!(body["status"], "ok");
}

#[tokio::test]
async fn readiness_fails_without_database() {
    let (s, _, body) = call(Request::get("/health/ready").body(Body::empty()).unwrap()).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(body["checks"][0]["name"], "postgres");
}

#[tokio::test]
async fn unknown_route_is_problem_json() {
    let (s, h, body) = call(
        Request::get("/api/v1/nope")
            .header("x-request-id", "rid-9")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(h[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(body["correlationId"], "rid-9");
}

#[tokio::test]
async fn dev_login_sets_session_cookie() {
    let req = Request::post("/api/v1/auth/login")
        .header("content-type", "application/json")
        .body(Body::from(
            r#"{"loginId":"meera.shah@sutra.dev","password":"sutra"}"#,
        ))
        .unwrap();
    let (s, h, body) = call(req).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h[header::SET_COOKIE].to_str().unwrap().contains("HttpOnly"));
    assert_eq!(body["principal"]["roles"][0], "faculty");
}

#[tokio::test]
async fn catalog_and_modules() {
    let (s, _, body) = call(
        Request::get("/api/v1/platform/workspaces?role=faculty")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["total"], 50);
    let (_, _, modules) = call(
        Request::get("/api/v1/platform/modules")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(modules.as_array().map(Vec::len), Some(10));
}

#[tokio::test]
async fn openapi_is_served_and_matches_committed_spec() {
    let (s, _, body) = call(
        Request::get("/api/openapi.json")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["openapi"], "3.1.0");
    for p in [
        "/api/v1/auth/login",
        "/api/v1/platform/workspaces",
        "/health/ready",
    ] {
        assert!(body["paths"].get(p).is_some(), "missing {p}");
    }
    let committed = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../openapi/openapi.json"
    ))
    .unwrap();
    let generated = sutra_api::openapi::spec_json().unwrap();
    assert!(
        committed == generated,
        "openapi/openapi.json is stale: run `make openapi`"
    );
}

// ---------------------------------------------------------------------------------------------
// Batch 1 slice A: sessions, CSRF, D02 policy and session-derived scope (memory stores, no DB).
// ---------------------------------------------------------------------------------------------

const INSTITUTIONS: &str = "/api/v1/institution/institutions";
const SOME_ID: &str = "0192f5a0-7c3e-7000-8000-0000000000aa";
const OTHER_TENANT: &str = "0192f5a0-7c3e-7000-8000-00000000ffff";

struct Login {
    cookie: String,
    csrf: String,
}

async fn login(app: &Router, login_id: &str) -> Login {
    let (s, h, body) = send(
        app,
        Request::post("/api/v1/auth/login")
            .header("content-type", "application/json")
            .body(Body::from(
                serde_json::json!({"loginId": login_id, "password": "sutra"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let cookie = h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    Login {
        cookie,
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    }
}

fn register_req(who: Option<&Login>, csrf: Option<&str>, key: Option<&str>) -> Request<Body> {
    let mut b = Request::post(INSTITUTIONS).header("content-type", "application/json");
    if let Some(l) = who {
        b = b.header("cookie", &l.cookie);
    }
    if let Some(t) = csrf {
        b = b.header("x-csrf-token", t);
    }
    if let Some(k) = key {
        b = b.header("idempotency-key", k);
    }
    b.body(Body::from(r#"{"code":"SIT","name":"Sutra"}"#))
        .unwrap()
}

fn rename_req(who: &Login, csrf: Option<&str>) -> Request<Body> {
    let mut b = Request::patch(format!("{INSTITUTIONS}/{SOME_ID}"))
        .header("content-type", "application/json")
        .header("if-match", "\"1\"")
        .header("cookie", &who.cookie);
    if let Some(t) = csrf {
        b = b.header("x-csrf-token", t);
    }
    b.body(Body::from(r#"{"name":"Renamed"}"#)).unwrap()
}

fn get_req(who: Option<&Login>) -> Request<Body> {
    let mut b = Request::get(format!("{INSTITUTIONS}/{SOME_ID}"));
    if let Some(l) = who {
        b = b.header("cookie", &l.cookie);
    }
    b.body(Body::empty()).unwrap()
}

#[tokio::test]
async fn protected_routes_are_401_without_session() {
    let app = router(AppContext::for_spec());
    // Client-supplied scope headers no longer grant anything.
    let (s, h, body) = send(
        &app,
        Request::get(format!("{INSTITUTIONS}/{SOME_ID}"))
            .header("x-sutra-tenant-id", DEV_TENANT_ID)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(h[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(body["errorCode"], "unauthenticated");
    let (s, _, _) = send(&app, register_req(None, None, Some("k"))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _, _) = send(&app, get_req(None)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // A forged/stale cookie is no session either.
    let (s, _, _) = send(
        &app,
        Request::get(format!("{INSTITUTIONS}/{SOME_ID}"))
            .header("cookie", "__Host-sutra_session=deadbeef")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_endpoint_returns_the_login_csrf_token() {
    let app = router(AppContext::for_spec());
    let admin = login(&app, "admin@sutra.dev").await;
    let (s, _, body) = send(
        &app,
        Request::get("/api/v1/auth/session")
            .header("cookie", &admin.cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(body["csrfToken"], admin.csrf.as_str());
    assert_eq!(body["principal"]["scope"]["tenantId"], DEV_TENANT_ID);
}

#[tokio::test]
async fn unsafe_requests_without_csrf_are_403() {
    let app = router(AppContext::for_spec());
    let admin = login(&app, "admin@sutra.dev").await;
    let (s, h, body) = send(&app, register_req(Some(&admin), None, Some("k1"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(h[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(body["errorCode"], "csrf-token-missing");
    let (s, _, body) = send(&app, register_req(Some(&admin), Some("forged"), Some("k1"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["errorCode"], "csrf-token-invalid");
    let (s, _, body) = send(&app, rename_req(&admin, None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["errorCode"], "csrf-token-missing");
    // Another session's token is not accepted either.
    let other = login(&app, "meera.shah@sutra.dev").await;
    let (s, _, body) = send(
        &app,
        register_req(Some(&admin), Some(&other.csrf), Some("k1")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["errorCode"], "csrf-token-invalid");
}

#[tokio::test]
async fn csrf_ok_passes_to_the_handler() {
    let app = router(AppContext::for_spec());
    let admin = login(&app, "admin@sutra.dev").await;
    // CSRF + policy pass; the handler's own precondition (Idempotency-Key) answers.
    let (s, _, body) = send(&app, register_req(Some(&admin), Some(&admin.csrf), None)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(body["detail"].as_str().unwrap().contains("Idempotency-Key"));
    // Fully valid request reaches the repository (no database here → 503, not 401/403).
    let (s, _, body) = send(
        &app,
        register_req(Some(&admin), Some(&admin.csrf), Some("k-ok")),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}

#[tokio::test]
async fn non_admin_roles_are_forbidden_to_mutate_but_may_read() {
    let app = router(AppContext::for_spec());
    for persona in ["meera.shah@sutra.dev", "aarav.desai@sutra.dev"] {
        let who = login(&app, persona).await;
        let (s, _, body) = send(&app, register_req(Some(&who), Some(&who.csrf), Some("k"))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{persona}");
        assert_eq!(body["errorCode"], "forbidden");
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("institution.register"));
        let (s, _, body) = send(&app, rename_req(&who, Some(&who.csrf))).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{persona}");
        assert!(body["detail"]
            .as_str()
            .unwrap()
            .contains("institution.rename"));
        // Read is granted to every authenticated role (no database here → 503, not 401/403).
        let (s, _, _) = send(&app, get_req(Some(&who))).await;
        assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "{persona}");
    }
}

#[tokio::test]
async fn cross_tenant_scope_headers_are_denied() {
    let app = router(AppContext::for_spec());
    let admin = login(&app, "admin@sutra.dev").await;
    let (s, _, body) = send(
        &app,
        Request::get(format!("{INSTITUTIONS}/{SOME_ID}"))
            .header("cookie", &admin.cookie)
            .header("x-sutra-tenant-id", OTHER_TENANT)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["errorCode"], "forbidden");
    let mut req = register_req(Some(&admin), Some(&admin.csrf), Some("k-x"));
    req.headers_mut()
        .insert("x-sutra-tenant-id", OTHER_TENANT.parse().unwrap());
    let (s, _, _) = send(&app, req).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn logout_requires_csrf_and_ends_access() {
    let app = router(AppContext::for_spec());
    let admin = login(&app, "admin@sutra.dev").await;
    let logout = |csrf: Option<&str>| {
        let mut b = Request::post("/api/v1/auth/logout").header("cookie", &admin.cookie);
        if let Some(t) = csrf {
            b = b.header("x-csrf-token", t);
        }
        b.body(Body::empty()).unwrap()
    };
    let (s, _, body) = send(&app, logout(None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["errorCode"], "csrf-token-missing");
    let (s, _, _) = send(&app, logout(Some(&admin.csrf))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = send(&app, get_req(Some(&admin))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
