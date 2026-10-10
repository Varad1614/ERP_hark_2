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

/// `GET /api/v1/auth/csrf` → (cookie pair, token): the pre-session double-submit token.
async fn login_csrf(app: &Router) -> (String, String) {
    let (s, h, body) = send(
        app,
        Request::get("/api/v1/auth/csrf")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let cookie = h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();
    (cookie, body["csrfToken"].as_str().unwrap().to_owned())
}

fn login_request(login_id: &str, csrf: Option<&(String, String)>) -> Request<Body> {
    let mut b = Request::post("/api/v1/auth/login").header("content-type", "application/json");
    if let Some((cookie, token)) = csrf {
        b = b.header("cookie", cookie).header("x-csrf-token", token);
    }
    b.body(Body::from(
        serde_json::json!({"loginId": login_id, "password": "sutra"}).to_string(),
    ))
    .unwrap()
}

#[tokio::test]
async fn dev_login_sets_session_cookie() {
    let app = router(AppContext::for_spec());
    let csrf = login_csrf(&app).await;
    let (s, h, body) = send(&app, login_request("meera.shah@sutra.dev", Some(&csrf))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h[header::SET_COOKIE].to_str().unwrap().contains("HttpOnly"));
    assert_eq!(body["principal"]["roles"][0], "faculty");
}

#[tokio::test]
async fn login_requires_the_pre_session_csrf_token() {
    let app = router(AppContext::for_spec());
    let (s, h, body) = send(&app, login_request("admin@sutra.dev", None)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(h[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(body["errorCode"], "csrf-token-missing");
    let (cookie, _) = login_csrf(&app).await;
    let forged = (cookie, "0".repeat(64) + ".9999999999." + &"0".repeat(64));
    let (s, _, body) = send(&app, login_request("admin@sutra.dev", Some(&forged))).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["errorCode"], "csrf-token-invalid");
    let ok = login_csrf(&app).await;
    let (s, _, _) = send(&app, login_request("admin@sutra.dev", Some(&ok))).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn cross_site_unsafe_requests_are_rejected() {
    let app = router(AppContext::for_spec());
    let csrf = login_csrf(&app).await;
    for (name, value) in [
        ("origin", "https://evil.example"),
        ("sec-fetch-site", "cross-site"),
        ("origin", "null"),
    ] {
        let mut req = login_request("admin@sutra.dev", Some(&csrf));
        req.headers_mut()
            .insert("host", "erp.sutra.test".parse().unwrap());
        req.headers_mut().insert(name, value.parse().unwrap());
        let (s, h, body) = send(&app, req).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{name}: {value}");
        assert_eq!(h[header::CONTENT_TYPE], "application/problem+json");
        assert_eq!(body["errorCode"], "cross-site-request-rejected");
        assert!(h.get(header::SET_COOKIE).is_none());
    }
    // Same-origin browser request passes.
    let mut req = login_request("admin@sutra.dev", Some(&csrf));
    req.headers_mut()
        .insert("host", "erp.sutra.test".parse().unwrap());
    req.headers_mut()
        .insert("origin", "https://erp.sutra.test".parse().unwrap());
    req.headers_mut()
        .insert("sec-fetch-site", "same-origin".parse().unwrap());
    let (s, _, _) = send(&app, req).await;
    assert_eq!(s, StatusCode::OK);
    // Safe methods are not blocked.
    let mut req = Request::get("/api/v1/platform/modules")
        .body(Body::empty())
        .unwrap();
    req.headers_mut()
        .insert("sec-fetch-site", "cross-site".parse().unwrap());
    let (s, _, _) = send(&app, req).await;
    assert_eq!(s, StatusCode::OK);
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
        "/api/v1/auth/csrf",
        "/api/v1/auth/mfa/step-up",
        "/api/v1/auth/mfa/totp/enrolment",
        "/api/v1/auth/oidc/start",
        "/api/v1/auth/oidc/callback",
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
    let csrf = login_csrf(app).await;
    let (s, h, body) = send(app, login_request(login_id, Some(&csrf))).await;
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

// ---------------------------------------------------------------------------------------------
// Batch 1 slice B: step-up for admin-sensitive permissions (dev TOTP, memory stores, no DB).
// ---------------------------------------------------------------------------------------------

fn step_up_app() -> Router {
    let src: std::collections::HashMap<&'static str, &'static str> =
        std::collections::HashMap::from([
            ("SUTRA_ENV", "test"),
            ("DATABASE_URL", "postgres://sutra:sutra@127.0.0.1:1/sutra"),
            ("SUTRA_DB_ACQUIRE_TIMEOUT_SECS", "1"),
            ("SUTRA_MFA_STEP_UP", "true"),
        ]);
    let cfg = sutra_api::config::Config::load(&src).unwrap();
    router(AppContext::from_config(&cfg).unwrap())
}

#[tokio::test]
async fn admin_sensitive_permission_needs_step_up() {
    use sutra_platform::auth::mfa::{base32_decode, totp_code, totp_step};
    let app = step_up_app();
    let admin = login(&app, "admin@sutra.dev").await;
    // Granted by role, but the session has no second factor → 403 step-up.
    let (s, h, body) = send(
        &app,
        register_req(Some(&admin), Some(&admin.csrf), Some("su-1")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(h[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(body["errorCode"], "mfa-step-up-required");
    assert!(body["detail"]
        .as_str()
        .unwrap()
        .contains("institution.register"));
    // Not admin-sensitive: rename is still allowed at aal1 (reaches the repository → 503).
    let (s, _, _) = send(&app, rename_req(&admin, Some(&admin.csrf))).await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE);
    // Faculty gets the plain 403 (step-up never grants a permission).
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let (_, _, body) = send(
        &app,
        register_req(Some(&faculty), Some(&faculty.csrf), Some("su-2")),
    )
    .await;
    assert_eq!(body["errorCode"], "forbidden");

    // Enrol TOTP (confirmation upgrades + rotates the session), then register passes the policy.
    let unsafe_req = |path: &str, body: serde_json::Value, who: &Login| {
        Request::post(path)
            .header("content-type", "application/json")
            .header("cookie", &who.cookie)
            .header("x-csrf-token", &who.csrf)
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let (s, _, enrol) = send(
        &app,
        unsafe_req(
            "/api/v1/auth/mfa/totp/enrolment",
            serde_json::json!({}),
            &admin,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let secret = base32_decode(enrol["secret"].as_str().unwrap()).unwrap();
    let now = chrono::Utc::now().timestamp();
    let code = totp_code(&secret, totp_step(now));
    let (s, h, body) = send(
        &app,
        unsafe_req(
            "/api/v1/auth/mfa/totp/confirm",
            serde_json::json!({"code": code}),
            &admin,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["mfaLevel"], "mfa");
    let elevated = Login {
        cookie: h[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned(),
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    };
    let (s, _, body) = send(
        &app,
        register_req(Some(&elevated), Some(&elevated.csrf), Some("su-3")),
    )
    .await;
    assert_eq!(s, StatusCode::SERVICE_UNAVAILABLE, "policy passed: {body}");
    // The pre-enrolment session id is gone.
    let (s, _, _) = send(&app, get_req(Some(&admin))).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}
