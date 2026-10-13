//! Shared harness for the Batch 2 / 3 / 4 / 5 / 6 DB test files (`db_b2*.rs` … `db_b6*.rs`). Foundation-owned: slices add
//! their own helpers in their own test file, not here. Same setup as `tests/db.rs`: migrations
//! as the schema owner, the API running as the runtime login role.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use std::collections::HashMap;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use tower::ServiceExt;

/// Integration-owned cross-slice readiness helpers (2B + 2C setup through the API).
pub mod b2;

/// Batch 3 foundation helpers: Domain B setup for Domain C tests + direct student seeding so
/// 3B/3C can test before 3A's admission flow is merged.
pub mod b3;

/// Batch 4 foundation helpers: Domain B/C setup for Domain D tests + direct offering / section /
/// faculty / enrolment / registration seeding so 4B/4C can test before 4A's API is merged.
pub mod b4;

/// Batch 5 foundation helpers: an institution with a student, the default chart of accounts and a
/// fee head, plus direct charge / payment seeding so 5B/5C can test before 5A's / 5B's APIs merge.
pub mod b5;

/// Batch 6 foundation helpers: an institution with an enrolled student, a grading scheme, an
/// examination session and paper, plus direct registration / final-marks / internal-assessment
/// seeding so 6B/6C can test before 6A's / 6B's APIs merge.
pub mod b6;

pub const INSTITUTIONS: &str = "/api/v1/institution/institutions";
pub const RUNTIME_USER: &str = "sutra_it_runtime";
pub const RUNTIME_PASSWORD: &str = "sutra-it-runtime-pw";

pub fn database_url() -> &'static str {
    Box::leak(
        std::env::var("DATABASE_URL")
            .expect("DATABASE_URL must be set for db-tests")
            .into_boxed_str(),
    )
}

fn runtime_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}")
}

/// Owner pool: applies migrations and provisions the runtime login role.
pub async fn owner_pool() -> sqlx::PgPool {
    let pool = sutra_persistence::pool::connect_lazy(&sutra_persistence::pool::PoolSettings {
        url: database_url().to_owned(),
        max_connections: 4,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(10),
        application_name: "sutra-api-db-tests-owner".into(),
    })
    .expect("owner pool");
    sutra_persistence::migrate::run(&pool)
        .await
        .expect("migrations");
    sutra_persistence::migrate::provision_runtime_login(&pool, RUNTIME_USER, RUNTIME_PASSWORD)
        .await
        .expect("runtime role");
    pool
}

/// Postgres-backed app (runtime role) whose dev authenticator issues sessions in `tenant`;
/// returns the owner pool for setup and assertions.
pub async fn app(tenant: &'static str) -> (Router, sqlx::PgPool) {
    let owner = owner_pool().await;
    let url: &'static str = Box::leak(runtime_url(database_url()).into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", url),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
    ]);
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    (router(ctx), owner)
}

pub async fn send(
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

pub struct Login {
    pub cookie: String,
    pub csrf: String,
}

fn cookie_pair(h: &axum::http::HeaderMap) -> String {
    h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

/// Dev sign-in (`admin@sutra.dev` = administrator, `aarav.desai@sutra.dev` = student, …).
pub async fn login(app: &Router, login_id: &str) -> Login {
    let (s, h, body) = send(
        app,
        Request::get("/api/v1/auth/csrf")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (cookie, token) = (
        cookie_pair(&h),
        body["csrfToken"].as_str().unwrap().to_owned(),
    );
    let (s, h, body) = send(
        app,
        Request::post("/api/v1/auth/login")
            .header("content-type", "application/json")
            .header("cookie", &cookie)
            .header("x-csrf-token", &token)
            .body(Body::from(
                serde_json::json!({"loginId": login_id, "password": "sutra"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    Login {
        cookie: cookie_pair(&h),
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    }
}

/// GET with the session cookie.
pub fn get(who: &Login, path: &str) -> Request<Body> {
    Request::get(path)
        .header("cookie", &who.cookie)
        .body(Body::empty())
        .unwrap()
}

/// Unsafe request with cookie + CSRF + JSON body; `if_match` / idempotency key optional.
pub fn write(
    who: &Login,
    method: &str,
    path: &str,
    body: serde_json::Value,
    if_match: Option<i64>,
) -> Request<Body> {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .header("idempotency-key", uuid::Uuid::now_v7().to_string());
    if let Some(v) = if_match {
        b = b.header("if-match", format!("\"{v}\""));
    }
    b.body(Body::from(body.to_string())).unwrap()
}

/// Fresh unique institution code.
pub fn code(prefix: &str) -> String {
    format!(
        "{prefix}-{}",
        uuid::Uuid::now_v7().simple().to_string()[20..].to_uppercase()
    )
}

/// Registers an institution as `who` and returns its id.
pub async fn register_institution(app: &Router, who: &Login, name: &str) -> String {
    let (s, _, inst) = send(
        app,
        write(
            who,
            "POST",
            INSTITUTIONS,
            serde_json::json!({"code": code("B2"), "name": name}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inst}");
    inst["id"].as_str().unwrap().to_owned()
}
