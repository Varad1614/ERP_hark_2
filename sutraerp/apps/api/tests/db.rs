//! End-to-end tests of the assembled API against a live PostgreSQL 18: durable sessions,
//! session-derived tenant scope and cross-tenant isolation. Run with:
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra cargo test -p sutra-api --features db-tests
//! ```
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use std::collections::HashMap;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use tower::ServiceExt;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-00000000a001";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-00000000b001";
const INSTITUTIONS: &str = "/api/v1/institution/institutions";

/// A fully Postgres-backed app whose dev authenticator issues sessions in `tenant`.
async fn app(tenant: &'static str) -> (Router, sqlx::PgPool) {
    app_with(tenant, &[]).await
}

/// Runtime LOGIN role the API runs as in these tests (member of `sutra_app`, owns nothing).
const RUNTIME_USER: &str = "sutra_it_runtime";
const RUNTIME_PASSWORD: &str = "sutra-it-runtime-pw";

fn database_url() -> &'static str {
    Box::leak(
        std::env::var("DATABASE_URL")
            .expect("DATABASE_URL must be set for db-tests")
            .into_boxed_str(),
    )
}

/// `url` with the credentials replaced by the runtime role's.
fn runtime_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}")
}

/// Owner pool: applies migrations (as the schema owner, ADR-L5-0008) and provisions the
/// runtime login role, exactly like `sutra-api migrate` with SUTRA_DB_RUNTIME_USER set.
async fn owner_pool() -> sqlx::PgPool {
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

/// A fully Postgres-backed app **running as the runtime role**; returns the owner pool for
/// test setup and assertions.
async fn app_with(
    tenant: &'static str,
    extra: &[(&'static str, &'static str)],
) -> (Router, sqlx::PgPool) {
    let owner = owner_pool().await;
    let url: &'static str = Box::leak(runtime_url(database_url()).into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("SUTRA_DEV_AUTH_PASSWORD", sutra_api::app::SPEC_DEV_PASSWORD),
        ("DATABASE_URL", url),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        // Background workers are driven explicitly by the tests.
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        // Shared login-CSRF key: tokens from one instance verify on another (multi-replica).
        (
            "SUTRA_CSRF_KEY",
            "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0",
        ),
    ]);
    let mut src = src;
    src.extend(extra.iter().copied());
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    (router(ctx), owner)
}

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

struct Login {
    cookie: String,
    csrf: String,
}

/// `GET /api/v1/auth/csrf` → (cookie pair, token).
async fn login_csrf(app: &Router) -> (String, String) {
    let (s, h, body) = send(
        app,
        Request::get("/api/v1/auth/csrf")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    (
        h[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned(),
        body["csrfToken"].as_str().unwrap().to_owned(),
    )
}

fn login_req(login_id: &str, csrf: &(String, String)) -> Request<Body> {
    Request::post("/api/v1/auth/login")
        .header("content-type", "application/json")
        .header("cookie", &csrf.0)
        .header("x-csrf-token", &csrf.1)
        .body(Body::from(
            serde_json::json!({"loginId": login_id, "password": sutra_api::app::SPEC_DEV_PASSWORD})
                .to_string(),
        ))
        .unwrap()
}

async fn login(app: &Router, login_id: &str) -> Login {
    let csrf = login_csrf(app).await;
    let (s, h, body) = send(app, login_req(login_id, &csrf)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    Login {
        cookie: h[header::SET_COOKIE]
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned(),
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    }
}

fn get(who: &Login, id: &str) -> Request<Body> {
    Request::get(format!("{INSTITUTIONS}/{id}"))
        .header("cookie", &who.cookie)
        .body(Body::empty())
        .unwrap()
}

fn rename(who: &Login, id: &str, version: &str) -> Request<Body> {
    Request::patch(format!("{INSTITUTIONS}/{id}"))
        .header("content-type", "application/json")
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .header("if-match", format!("\"{version}\""))
        .body(Body::from(r#"{"name":"Hijacked"}"#))
        .unwrap()
}

#[tokio::test]
async fn sessions_scope_and_cross_tenant_isolation() {
    let (a, pool) = app(TENANT_A).await;
    let (b, _) = app(TENANT_B).await;

    // Tenant A administrator registers an institution.
    let admin_a = login(&a, "admin@sutra.dev").await;
    let code = format!(
        "X-{}",
        uuid::Uuid::now_v7().simple().to_string()[20..].to_uppercase()
    );
    let (s, _, inst) = send(
        &a,
        Request::post(INSTITUTIONS)
            .header("content-type", "application/json")
            .header("cookie", &admin_a.cookie)
            .header("x-csrf-token", &admin_a.csrf)
            .header("idempotency-key", uuid::Uuid::now_v7().to_string())
            .body(Body::from(
                serde_json::json!({"code": code, "name": "Tenant A College"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inst}");
    assert_eq!(inst["tenantId"], TENANT_A, "tenant comes from the session");
    let id = inst["id"].as_str().unwrap().to_owned();

    // The session row stores only the token digest; the raw token is nowhere in the table.
    let token = admin_a.cookie.split_once('=').unwrap().1.to_owned();
    let raw_hits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.session WHERE encode(token_hash, 'hex') = $1 OR subject = $1",
    )
    .bind(&token)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(raw_hits, 0);

    // Audit actor is the authenticated subject.
    let actor: Option<String> = sqlx::query_scalar(
        "SELECT actor_id FROM platform.audit_log WHERE target_id = $1 AND action = 'institution.institution.register'",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(actor.as_deref(), Some("dev|admin@sutra.dev"));

    // Same-tenant reads by any role succeed; mutation by non-admin is forbidden.
    let student_a = login(&a, "aarav.desai@sutra.dev").await;
    let (s, _, _) = send(&a, get(&student_a, &id)).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = send(&a, rename(&student_a, &id, "1")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Tenant B administrator can neither see nor modify it, whatever headers it sends.
    let admin_b = login(&b, "admin@sutra.dev").await;
    let (s, _, body) = send(&b, get(&admin_b, &id)).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{body}");
    let mut spoof = get(&admin_b, &id);
    spoof
        .headers_mut()
        .insert("x-sutra-tenant-id", TENANT_A.parse().unwrap());
    let (s, _, _) = send(&b, spoof).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "header cannot switch tenant");
    let (s, _, _) = send(&b, rename(&admin_b, &id, "1")).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, after) = send(&a, get(&admin_a, &id)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(after["name"], "Tenant A College");

    // A tenant-A session presented to the tenant-B deployment (shared DB) still carries
    // tenant A's scope, never B's: it sees A's data only.
    let (s, _, _) = send(&b, get(&admin_a, &id)).await;
    assert_eq!(s, StatusCode::OK);

    // Durability: a fresh app instance (a restart) honours the existing session.
    let (a2, _) = app(TENANT_A).await;
    let (s, _, me) = send(
        &a2,
        Request::get("/api/v1/auth/session")
            .header("cookie", &admin_a.cookie)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(me["csrfToken"], admin_a.csrf.as_str());

    // Rename by the owner works and logout revokes durably.
    let (s, _, renamed) = send(&a2, rename(&admin_a, &id, "1")).await;
    assert_eq!(s, StatusCode::OK, "{renamed}");
    let (s, _, _) = send(
        &a2,
        Request::post("/api/v1/auth/logout")
            .header("cookie", &admin_a.cookie)
            .header("x-csrf-token", &admin_a.csrf)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    let (s, _, _) = send(&a, get(&admin_a, &id)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

fn cookie_of(h: &axum::http::HeaderMap) -> String {
    h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

fn post_json(path: &str, who: &Login, body: serde_json::Value) -> Request<Body> {
    Request::post(path)
        .header("content-type", "application/json")
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .header("idempotency-key", uuid::Uuid::now_v7().to_string())
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// Batch 1B end to end on PostgreSQL: enforced MFA, encrypted TOTP enrolment, durable
/// challenge (pre_auth), replay refusal, step-up gate on institution register.
#[tokio::test]
async fn totp_mfa_and_step_up_on_postgres() {
    use sutra_platform::auth::mfa::{base32_decode, totp_code, totp_step};
    let tenant: &'static str = Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str());
    let (a, pool) = app_with(tenant, &[("SUTRA_DEV_MFA", "true")]).await;
    let admin = login(&a, "admin@sutra.dev").await;
    let register = |who: &Login| {
        post_json(
            INSTITUTIONS,
            who,
            serde_json::json!({
                "code": format!("M-{}", uuid::Uuid::now_v7().simple().to_string()[20..].to_uppercase()),
                "name": "MFA College"
            }),
        )
    };
    let (s, _, body) = send(&a, register(&admin)).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(body["errorCode"], "mfa-step-up-required");

    // Enrol: the secret is stored only as AES-GCM ciphertext.
    let (s, _, enrol) = send(
        &a,
        post_json(
            "/api/v1/auth/mfa/totp/enrolment",
            &admin,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{enrol}");
    let secret = base32_decode(enrol["secret"].as_str().unwrap()).unwrap();
    let (status, ct): (String, Vec<u8>) = sqlx::query_as(
        "SELECT status, secret_ciphertext FROM platform.mfa_factor WHERE tenant_id = $1::uuid",
    )
    .bind(tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(status, "pending");
    assert!(!ct.windows(secret.len()).any(|w| w == secret.as_slice()));

    let code = totp_code(&secret, totp_step(chrono::Utc::now().timestamp()));
    let (s, h, body) = send(
        &a,
        post_json(
            "/api/v1/auth/mfa/totp/confirm",
            &admin,
            serde_json::json!({"code": code}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let elevated = Login {
        cookie: cookie_of(&h),
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    };
    let (s, _, body) = send(&a, register(&elevated)).await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
    let mfa_level: String = sqlx::query_scalar(
        "SELECT mfa_level FROM platform.session WHERE tenant_id = $1::uuid AND revoked_at IS NULL",
    )
    .bind(tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(mfa_level, "mfa");

    // Next sign-in: durable challenge; the enrolment code is a replay.
    let csrf = login_csrf(&a).await;
    let (_, _, body) = send(&a, login_req("admin@sutra.dev", &csrf)).await;
    assert_eq!(body["outcome"], "mfaRequired");
    let challenge = body["challengeId"].as_str().unwrap().to_owned();
    let stored: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.pre_auth WHERE kind = 'mfa' AND payload::text LIKE '%' || $1 || '%'",
    )
    .bind(tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(stored, 1);
    let verify = |challenge: &str, code: &str| {
        Request::post("/api/v1/auth/mfa/verify")
            .header("content-type", "application/json")
            .header("cookie", &csrf.0)
            .header("x-csrf-token", &csrf.1)
            .body(Body::from(
                serde_json::json!({"challengeId": challenge, "code": code}).to_string(),
            ))
            .unwrap()
    };
    let (s, _, _) = send(&a, verify(&challenge, &code)).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "same step replay");
    let left: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.pre_auth WHERE kind = 'mfa' AND payload::text LIKE '%' || $1 || '%'",
    )
    .bind(tenant)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(left, 0, "challenge consumed");

    // Simulate the next time step having not been used yet, then sign in with MFA.
    sqlx::query(
        "UPDATE platform.mfa_factor SET last_used_step = last_used_step - 2 WHERE tenant_id = $1::uuid",
    )
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    let (_, _, body) = send(&a, login_req("admin@sutra.dev", &csrf)).await;
    let challenge = body["challengeId"].as_str().unwrap().to_owned();
    let (s, h, body) = send(&a, verify(&challenge, &code)).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["mfaLevel"], "mfa");
    let mfa_admin = Login {
        cookie: cookie_of(&h),
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    };
    let (s, _, body) = send(&a, register(&mfa_admin)).await;
    assert_eq!(s, StatusCode::CREATED, "{body}");
}

const TENANT_C: &str = "0192f5a0-7c3e-7000-8000-00000000c001";

/// Batch 1C: the institution GET read-through cache (memory backend) is invalidated by the
/// owner on rename — also when the rename loses an optimistic-concurrency race — and the
/// committed outbox intents of those writes relay in order with `msg_id` = outbox id.
#[tokio::test]
async fn institution_cache_invalidation_and_outbox_relay() {
    let (a, pool) = app_with(TENANT_C, &[("SUTRA_CACHE", "memory")]).await;
    let admin = login(&a, "admin@sutra.dev").await;
    let code = format!(
        "C-{}",
        uuid::Uuid::now_v7().simple().to_string()[20..].to_uppercase()
    );
    let (s, _, inst) = send(
        &a,
        post_json(
            INSTITUTIONS,
            &admin,
            serde_json::json!({"code": code, "name": "Cache College"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inst}");
    let id = inst["id"].as_str().unwrap().to_owned();
    let uid = uuid::Uuid::parse_str(&id).unwrap();

    // First read fills the cache; a change made behind the owner's API is not seen (a hit).
    let (s, _, body) = send(&a, get(&admin, &id)).await;
    assert_eq!(
        (s, body["name"].as_str()),
        (StatusCode::OK, Some("Cache College"))
    );
    sqlx::query("UPDATE institution.institution SET name = 'Behind' WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    let (_, _, body) = send(&a, get(&admin, &id)).await;
    assert_eq!(body["name"], "Cache College", "served from the cache");

    // Rename invalidates: the next read is fresh.
    let (s, _, _) = send(&a, rename(&admin, &id, "1")).await;
    assert_eq!(s, StatusCode::OK);
    let (_, h, body) = send(&a, get(&admin, &id)).await;
    assert_eq!(body["name"], "Hijacked");
    assert_eq!(h[header::ETAG], "\"2\"");

    // A stale cached ETag: the rename fails the version check (against the row, never the
    // cache) and still invalidates, so the client's refetch sees the current version.
    sqlx::query("UPDATE institution.institution SET name = 'Moved', version = 5 WHERE id = $1")
        .bind(uid)
        .execute(&pool)
        .await
        .unwrap();
    let (_, h, _) = send(&a, get(&admin, &id)).await;
    assert_eq!(h[header::ETAG], "\"2\"", "still cached");
    let (s, _, _) = send(&a, rename(&admin, &id, "2")).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (_, h, body) = send(&a, get(&admin, &id)).await;
    assert_eq!(body["name"], "Moved");
    assert_eq!(h[header::ETAG], "\"5\"");

    // Relay this tenant's committed intents through the in-memory publisher.
    let publisher = sutra_kernel::messaging::InMemoryPublisher::new();
    let relay = sutra_persistence::outbox_relay::OutboxRelay::new(
        pool.clone(),
        std::sync::Arc::new(publisher.clone()),
        sutra_persistence::outbox_relay::RelaySettings {
            tenant: Some(uuid::Uuid::parse_str(TENANT_C).unwrap()),
            ..Default::default()
        },
    );
    while relay.run_once().await.unwrap().claimed > 0 {}
    let mine: Vec<(String, serde_json::Value)> = publisher
        .published()
        .into_iter()
        .map(|m| (m.subject, serde_json::from_slice(&m.payload).unwrap()))
        .filter(|(_, env): &(String, serde_json::Value)| env["subject"] == id)
        .collect();
    let subjects: Vec<&str> = mine.iter().map(|(s, _)| s.as_str()).collect();
    assert_eq!(
        subjects,
        vec![
            format!("sutra.{TENANT_C}.institution.institution.registered"),
            format!("sutra.{TENANT_C}.institution.institution.renamed"),
        ]
    );
    let published: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE subject = $1 AND published_at IS NOT NULL",
    )
    .bind(&id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(published, 2);
}

/// Cache and broker outages never fail requests or readiness: Valkey errors degrade to misses
/// and both dependencies report as non-critical `degraded` checks.
#[tokio::test]
async fn unreachable_valkey_and_nats_degrade_without_failing() {
    let (a, _) = app_with(
        TENANT_C,
        &[
            ("SUTRA_CACHE", "valkey"),
            ("SUTRA_VALKEY_URL", "redis://127.0.0.1:1"),
            ("SUTRA_OUTBOX_RELAY", "nats"),
            ("SUTRA_NATS_URL", "nats://127.0.0.1:1"),
        ],
    )
    .await;
    let admin = login(&a, "admin@sutra.dev").await;
    let code = format!(
        "V-{}",
        uuid::Uuid::now_v7().simple().to_string()[20..].to_uppercase()
    );
    let (s, _, inst) = send(
        &a,
        post_json(
            INSTITUTIONS,
            &admin,
            serde_json::json!({"code": code, "name": "No Cache College"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inst}");
    let id = inst["id"].as_str().unwrap();
    for _ in 0..2 {
        let (s, _, body) = send(&a, get(&admin, id)).await;
        assert_eq!(s, StatusCode::OK, "{body}");
        assert_eq!(body["name"], "No Cache College");
    }
    let (s, _, ready) = send(
        &a,
        Request::get("/health/ready").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ready}");
    assert_eq!(ready["status"], "degraded");
    let checks = ready["checks"].as_array().unwrap();
    let find = |n: &str| checks.iter().find(|c| c["name"] == n).cloned().unwrap();
    assert_eq!(find("postgres")["ok"], true);
    assert_eq!(find("valkey")["ok"], false);
    assert_eq!(find("valkey")["critical"], false);
    assert_eq!(find("nats")["ok"], false);
    assert_eq!(find("nats")["critical"], false);
}

// =============================================================================================
// Batch 1D — notifications, notices, to-dos, profile, security endpoints, runtime DB role
// =============================================================================================

use sutra_persistence::jobs::{run_once, JobOutcome};
use sutra_platform::inbox::consumer::InboxProjector;

/// A tenant of its own per test, so role-addressed notifications of parallel tests never mix.
fn fresh_tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

async fn runtime_pool() -> sqlx::PgPool {
    let url = runtime_url(database_url());
    sutra_persistence::pool::connect_lazy(&sutra_persistence::pool::PoolSettings {
        url,
        max_connections: 2,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(10),
        application_name: "sutra-api-db-tests-runtime".into(),
    })
    .expect("runtime pool")
}

/// Runs the inbox consumer once (as the runtime role), waiting for a parallel test's run.
async fn project(pool: &sqlx::PgPool) {
    let job = InboxProjector {
        interval: std::time::Duration::from_secs(1),
        lookback: std::time::Duration::from_secs(3600),
        batch_size: 1000,
        email: true,
    };
    for _ in 0..200 {
        match run_once(pool, &job).await.expect("projector") {
            JobOutcome::Ran { .. } => return,
            JobOutcome::Skipped => tokio::time::sleep(std::time::Duration::from_millis(25)).await,
        }
    }
    panic!("inbox projector lock never became free");
}

fn json_req(method: &str, path: &str, who: &Login, body: serde_json::Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .header("idempotency-key", uuid::Uuid::now_v7().to_string())
        .body(Body::from(body.to_string()))
        .unwrap()
}

fn get_as(who: &Login, path: &str) -> Request<Body> {
    Request::get(path)
        .header("cookie", &who.cookie)
        .body(Body::empty())
        .unwrap()
}

fn code() -> String {
    format!(
        "N-{}",
        uuid::Uuid::now_v7().simple().to_string()[20..].to_uppercase()
    )
}

fn titles(page: &serde_json::Value) -> Vec<String> {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn notifications_from_domain_events_are_role_addressed_and_tenant_scoped() {
    let (t1, t2) = (fresh_tenant(), fresh_tenant());
    let (a, owner) = app(t1).await;
    let (b, _) = app(t2).await;
    let rt = runtime_pool().await;
    let admin = login(&a, "admin@sutra.dev").await;
    let student = login(&a, "aarav.desai@sutra.dev").await;
    let admin_b = login(&b, "admin@sutra.dev").await;

    let (s, _, inst) = send(
        &a,
        json_req(
            "POST",
            INSTITUTIONS,
            &admin,
            serde_json::json!({"code": code(), "name": "Event College"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inst}");
    let id = inst["id"].as_str().unwrap().to_owned();
    project(&rt).await;

    let (s, _, page) = send(&a, get_as(&admin, "/api/v1/notifications")).await;
    assert_eq!(s, StatusCode::OK, "{page}");
    assert_eq!(titles(&page), ["Institution registered: Event College"]);
    assert_eq!(page["unreadCount"], 1);
    assert_eq!(page["items"][0]["link"], "/academics");
    let nid = page["items"][0]["id"].as_str().unwrap().to_owned();
    // Role addressing: the student of the same tenant does not see an administrators' notification.
    let (_, _, sp) = send(&a, get_as(&student, "/api/v1/notifications")).await;
    assert!(titles(&sp).is_empty());
    // Tenant isolation: tenant B sees nothing and cannot touch A's notification.
    let (_, _, bp) = send(&b, get_as(&admin_b, "/api/v1/notifications")).await;
    assert!(titles(&bp).is_empty());
    let (s, _, _) = send(
        &b,
        json_req(
            "POST",
            &format!("/api/v1/notifications/{nid}/read"),
            &admin_b,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Event-created to-do for the registering administrator.
    let (_, _, todos) = send(&a, get_as(&admin, "/api/v1/todos")).await;
    assert_eq!(titles(&todos), ["Complete the setup of Event College"]);
    assert_eq!(todos["items"][0]["source"], "event");

    // Mark read (idempotent), unknown id → 404.
    for _ in 0..2 {
        let (s, _, _) = send(
            &a,
            json_req(
                "POST",
                &format!("/api/v1/notifications/{nid}/read"),
                &admin,
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::NO_CONTENT);
    }
    let (s, _, _) = send(
        &a,
        json_req(
            "POST",
            &format!("/api/v1/notifications/{}/read", uuid::Uuid::now_v7()),
            &admin,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, _, c) = send(&a, get_as(&admin, "/api/v1/notifications/unread-count")).await;
    assert_eq!(c["unread"], 0);

    // Rename → second notification; reprocessing never duplicates.
    let (s, _, _) = send(&a, {
        let mut r = json_req(
            "PATCH",
            &format!("{INSTITUTIONS}/{id}"),
            &admin,
            serde_json::json!({"name": "Event University"}),
        );
        r.headers_mut().insert("if-match", "\"1\"".parse().unwrap());
        r
    })
    .await;
    assert_eq!(s, StatusCode::OK);
    project(&rt).await;
    project(&rt).await;
    let (_, _, page) = send(&a, get_as(&admin, "/api/v1/notifications?unreadOnly=true")).await;
    assert_eq!(titles(&page), ["Institution renamed to Event University"]);
    let (_, _, all) = send(&a, get_as(&admin, "/api/v1/notifications?limit=1")).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 1);
    assert!(all["nextCursor"].is_string(), "second page exists");
    let cursor = all["nextCursor"].as_str().unwrap().to_owned();
    let (_, _, p2) = send(
        &a,
        get_as(
            &admin,
            &format!("/api/v1/notifications?limit=1&cursor={cursor}"),
        ),
    )
    .await;
    assert_eq!(titles(&p2), ["Institution registered: Event College"]);
    assert!(p2["nextCursor"].is_null());
    let (s, _, m) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/notifications/read-all",
            &admin,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!((s, m["marked"].as_i64()), (StatusCode::OK, Some(1)));
    let dupes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM platform.notification WHERE tenant_id = $1::uuid")
            .bind(t1)
            .fetch_one(&owner)
            .await
            .unwrap();
    assert_eq!(dupes, 2, "idempotent consumer");
    let receipts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.event_receipt r JOIN platform.outbox o ON o.id = r.event_id \
         WHERE o.tenant_id = $1::uuid AND r.consumer = 'platform.inbox'",
    )
    .bind(t1)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(receipts, 2);
    // Unauthenticated → 401.
    let (s, _, _) = send(
        &a,
        Request::get("/api/v1/notifications")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn poison_events_are_receipted_as_failed_and_do_not_block() {
    let t = fresh_tenant();
    let (_, owner) = app(t).await;
    let rt = runtime_pool().await;
    let scope = sutra_kernel::scope::ExecutionScope::parse(Some(t), None).unwrap();
    let bad = sutra_kernel::events::EventEnvelope::new(
        &scope,
        sutra_kernel::events::NewEvent {
            source: "/test",
            event_type: "sutraerp.institution.institution.renamed",
            dataschema: "urn:test",
            subject: None,
            data: serde_json::json!({"unexpected": true}),
        },
    );
    sutra_persistence::outbox::enqueue(&owner, &bad)
        .await
        .unwrap();
    project(&rt).await;
    let outcome: String = sqlx::query_scalar(
        "SELECT outcome FROM platform.event_receipt WHERE consumer = 'platform.inbox' AND event_id = $1",
    )
    .bind(bad.id)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(outcome, "failed");
}

#[tokio::test]
async fn notices_audience_permissions_lifecycle_audit_and_outbox() {
    let (t, t2) = (fresh_tenant(), fresh_tenant());
    let (a, owner) = app(t).await;
    let (b, _) = app(t2).await;
    let rt = runtime_pool().await;
    let admin = login(&a, "admin@sutra.dev").await;
    let faculty = login(&a, "meera.shah@sutra.dev").await;
    let student = login(&a, "aarav.desai@sutra.dev").await;
    let student_b = login(&b, "aarav.desai@sutra.dev").await;
    let body = serde_json::json!({"title": "Exam timetable out", "body": "See the exam cell page.",
        "audienceRoles": ["student"], "pinned": true});

    let (s, _, p) = send(
        &a,
        json_req("POST", "/api/v1/notices", &student, body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{p}");
    assert!(p["detail"]
        .as_str()
        .unwrap_or_default()
        .contains("notice.manage"));
    let (s, h, n) = send(&a, json_req("POST", "/api/v1/notices", &admin, body)).await;
    assert_eq!(s, StatusCode::CREATED, "{n}");
    assert_eq!(h[header::ETAG], "\"1\"");
    assert_eq!(n["state"], "live");
    assert_eq!(n["authorName"], "Dhruv Naik");
    let id = n["id"].as_str().unwrap().to_owned();
    let future = (chrono::Utc::now() + chrono::Duration::hours(2)).to_rfc3339();
    let (s, _, sched) = send(&a, json_req("POST", "/api/v1/notices", &admin, serde_json::json!({
        "title": "Holiday next week", "body": "Campus closed.", "audienceRoles": ["student", "faculty"], "publishAt": future}))).await;
    assert_eq!(
        (s, sched["state"].as_str()),
        (StatusCode::CREATED, Some("scheduled"))
    );
    let (s, _, v) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/notices",
            &admin,
            serde_json::json!({
        "title": "", "body": "x", "audienceRoles": []}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");

    // Audience filtering.
    let (_, _, l) = send(&a, get_as(&student, "/api/v1/notices")).await;
    assert_eq!(
        titles(&l),
        ["Exam timetable out"],
        "scheduled notice hidden"
    );
    let (s, _, _) = send(&a, get_as(&student, &format!("/api/v1/notices/{id}"))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, _, l) = send(&a, get_as(&faculty, "/api/v1/notices")).await;
    assert!(titles(&l).is_empty(), "faculty is not in the audience");
    let (s, _, _) = send(&a, get_as(&faculty, &format!("/api/v1/notices/{id}"))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = send(&a, get_as(&student, "/api/v1/notices?view=manage")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, _, m) = send(&a, get_as(&admin, "/api/v1/notices?view=manage")).await;
    assert_eq!(m["total"], 2);
    let (_, _, l) = send(&b, get_as(&student_b, "/api/v1/notices")).await;
    assert!(titles(&l).is_empty(), "tenant isolation");
    let (s, _, _) = send(&b, get_as(&student_b, &format!("/api/v1/notices/{id}"))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // Update with optimistic concurrency.
    let patch = |etag: Option<&str>| {
        let mut r = json_req(
            "PATCH",
            &format!("/api/v1/notices/{id}"),
            &admin,
            serde_json::json!({"audienceRoles": ["student", "faculty"]}),
        );
        if let Some(e) = etag {
            r.headers_mut().insert("if-match", e.parse().unwrap());
        }
        r
    };
    assert_eq!(
        send(&a, patch(None)).await.0,
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        send(&a, patch(Some("\"7\""))).await.0,
        StatusCode::PRECONDITION_FAILED
    );
    let (s, h, u) = send(&a, patch(Some("\"1\""))).await;
    assert_eq!(
        (s, h[header::ETAG].to_str().unwrap()),
        (StatusCode::OK, "\"2\""),
        "{u}"
    );
    let (_, _, l) = send(&a, get_as(&faculty, "/api/v1/notices")).await;
    assert_eq!(titles(&l), ["Exam timetable out"]);

    // Notifications for the audience (the scheduled one stays hidden until publishAt).
    project(&rt).await;
    let (_, _, np) = send(&a, get_as(&student, "/api/v1/notifications")).await;
    assert_eq!(titles(&np), ["New notice: Exam timetable out"]);
    assert_eq!(np["items"][0]["link"], "/notices");

    // Archive: readers lose it immediately; archiving twice is idempotent.
    for _ in 0..2 {
        let (s, _, x) = send(
            &a,
            json_req(
                "POST",
                &format!("/api/v1/notices/{id}/archive"),
                &admin,
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!((s, x["state"].as_str()), (StatusCode::OK, Some("archived")));
    }
    let (s, _, _) = send(&a, get_as(&student, &format!("/api/v1/notices/{id}"))).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = send(&a, patch(Some("\"3\""))).await;
    assert_eq!(s, StatusCode::CONFLICT, "archived notices are read-only");

    // Audit evidence and publication intents, in the notice's tenant.
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM platform.audit_log WHERE tenant_id = $1::uuid AND target_type = 'notice' AND target_id = $2 ORDER BY occurred_at",
    )
    .bind(t)
    .bind(&id)
    .fetch_all(&owner)
    .await
    .unwrap();
    assert_eq!(
        actions,
        [
            "platform.notice.publish",
            "platform.notice.update",
            "platform.notice.archive"
        ]
    );
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM platform.outbox WHERE tenant_id = $1::uuid AND subject = $2 ORDER BY created_at",
    )
    .bind(t)
    .bind(&id)
    .fetch_all(&owner)
    .await
    .unwrap();
    assert_eq!(
        events,
        [
            "sutraerp.platform.notice.published",
            "sutraerp.platform.notice.updated",
            "sutraerp.platform.notice.archived"
        ]
    );
}

#[tokio::test]
async fn todos_assignment_permissions_transitions_and_isolation() {
    let (t, t2) = (fresh_tenant(), fresh_tenant());
    let (a, owner) = app(t).await;
    let (b, _) = app(t2).await;
    let rt = runtime_pool().await;
    let admin = login(&a, "admin@sutra.dev").await;
    let faculty = login(&a, "meera.shah@sutra.dev").await;
    let student = login(&a, "aarav.desai@sutra.dev").await;
    let student_b = login(&b, "aarav.desai@sutra.dev").await;

    let (s, _, own) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/todos",
            &student,
            serde_json::json!({
        "title": "Pay hostel fee", "link": "/fees", "dueAt": "2020-01-01T00:00:00Z"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{own}");
    assert_eq!(own["overdue"], true);
    assert!(own["assignedBy"].is_null());
    let (s, _, _) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/todos",
            &student,
            serde_json::json!({
        "title": "x", "assigneeLoginId": "meera.shah@sutra.dev"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students cannot assign");
    let (s, _, v) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/todos",
            &faculty,
            serde_json::json!({
        "title": "Submit lab record", "assigneeLoginId": "nobody@sutra.dev"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (s, _, v) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/todos",
            &faculty,
            serde_json::json!({
        "title": "Bad link", "link": "https://evil.example"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    let (s, _, assigned) = send(&a, json_req("POST", "/api/v1/todos", &faculty, serde_json::json!({
        "title": "Submit lab record", "detail": "Week 3", "assigneeLoginId": "AARAV.DESAI@sutra.dev"}))).await;
    assert_eq!(s, StatusCode::CREATED, "{assigned}");
    assert_eq!(assigned["assignedBy"], "dev|meera.shah@sutra.dev");
    let tid = assigned["id"].as_str().unwrap().to_owned();
    let (_, _, faculty_list) = send(&a, get_as(&faculty, "/api/v1/todos")).await;
    assert!(
        titles(&faculty_list).is_empty(),
        "assigned to the student, not the assigner"
    );
    let (_, _, sl) = send(&a, get_as(&student, "/api/v1/todos")).await;
    assert_eq!(titles(&sl), ["Submit lab record", "Pay hostel fee"]);
    assert_eq!(sl["openCount"], 2);
    project(&rt).await;
    let (_, _, np) = send(&a, get_as(&student, "/api/v1/notifications")).await;
    assert_eq!(titles(&np), ["New to-do: Submit lab record"]);

    // Only the assignee can transition it.
    let (s, _, _) = send(
        &a,
        json_req(
            "POST",
            &format!("/api/v1/todos/{tid}/complete"),
            &admin,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = send(
        &b,
        json_req(
            "POST",
            &format!("/api/v1/todos/{tid}/complete"),
            &student_b,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "tenant isolation");
    for _ in 0..2 {
        let (s, _, d) = send(
            &a,
            json_req(
                "POST",
                &format!("/api/v1/todos/{tid}/complete"),
                &student,
                serde_json::json!({}),
            ),
        )
        .await;
        assert_eq!(
            (s, d["status"].as_str()),
            (StatusCode::OK, Some("completed")),
            "idempotent"
        );
    }
    let (s, _, _) = send(
        &a,
        json_req(
            "POST",
            &format!("/api/v1/todos/{tid}/dismiss"),
            &student,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (_, _, done) = send(&a, get_as(&student, "/api/v1/todos?status=completed")).await;
    assert_eq!(titles(&done), ["Submit lab record"]);
    let (s, _, r) = send(
        &a,
        json_req(
            "POST",
            &format!("/api/v1/todos/{tid}/reopen"),
            &student,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!((s, r["status"].as_str()), (StatusCode::OK, Some("open")));
    let (s, _, d) = send(
        &a,
        json_req(
            "POST",
            &format!("/api/v1/todos/{tid}/dismiss"),
            &student,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(
        (s, d["status"].as_str()),
        (StatusCode::OK, Some("dismissed"))
    );
    let (_, _, all) = send(&a, get_as(&student, "/api/v1/todos?status=all")).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 2);
    let (_, _, bl) = send(&b, get_as(&student_b, "/api/v1/todos?status=all")).await;
    assert!(titles(&bl).is_empty());
    let assign_audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1::uuid AND action = 'platform.todo.assign' AND target_id = $2",
    )
    .bind(t)
    .bind(&tid)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(assign_audit, 1);
}

#[tokio::test]
async fn profile_update_concurrency_preferences_and_email_channel() {
    let t = fresh_tenant();
    let (a, owner) = app(t).await;
    let rt = runtime_pool().await;
    let admin = login(&a, "admin@sutra.dev").await;
    let student = login(&a, "aarav.desai@sutra.dev").await;
    let (s, h, p) = send(&a, get_as(&student, "/api/v1/profile")).await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(h[header::ETAG], "\"1\"");
    assert_eq!(p["displayName"], "Aarav Desai");
    assert_eq!(p["roles"], serde_json::json!(["student"]));
    assert!(
        p["lastSignInAt"].is_string(),
        "profile refreshed at sign-in"
    );
    assert_eq!(p["notificationPreferences"].as_array().unwrap().len(), 5);
    let patch = |etag: Option<&str>, body: serde_json::Value| {
        let mut r = json_req("PATCH", "/api/v1/profile", &student, body);
        if let Some(e) = etag {
            r.headers_mut().insert("if-match", e.parse().unwrap());
        }
        r
    };
    let good = serde_json::json!({"displayName": "Aarav D.", "locale": "hi-IN", "timeZone": "Asia/Kolkata",
        "contactEmail": "aarav@sutra.dev", "contactPhone": "+919876543210",
        "notificationPreferences": [{"category": "notice", "inApp": false, "email": true}]});
    assert_eq!(
        send(&a, patch(None, good.clone())).await.0,
        StatusCode::PRECONDITION_REQUIRED
    );
    assert_eq!(
        send(&a, patch(Some("\"5\""), good.clone())).await.0,
        StatusCode::PRECONDITION_FAILED
    );
    let (s, _, v) = send(
        &a,
        patch(Some("\"1\""), serde_json::json!({"contactEmail": "nope"})),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{v}");
    assert_eq!(v["validationErrors"][0]["field"], "contactEmail");
    let (s, h, p) = send(&a, patch(Some("\"1\""), good)).await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(h[header::ETAG], "\"2\"");
    assert_eq!(p["locale"], "hi-IN");
    let notice_pref = p["notificationPreferences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["category"] == "notice")
        .unwrap()
        .clone();
    assert_eq!(
        notice_pref,
        serde_json::json!({"category": "notice", "inApp": false, "email": true})
    );

    // Muted in the app, but e-mailed: a notice for students creates a queued e-mail delivery.
    let (s, _, _) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/notices",
            &admin,
            serde_json::json!({
        "title": "Library hours", "body": "Open till 9 pm.", "audienceRoles": ["student"]}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    project(&rt).await;
    let (_, _, np) = send(&a, get_as(&student, "/api/v1/notifications")).await;
    assert!(titles(&np).is_empty(), "notice category muted in the app");
    let (address, status): (String, String) = sqlx::query_as(
        "SELECT d.address, d.status FROM platform.notification_delivery d \
         JOIN platform.notification n ON n.id = d.notification_id WHERE n.tenant_id = $1::uuid",
    )
    .bind(t)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(
        (address.as_str(), status.as_str()),
        ("aarav@sutra.dev", "pending")
    );
    // The delivery job hands it to the mail port.
    let mailer = std::sync::Arc::new(sutra_kernel::mail::InMemoryMailSender::default());
    let job = sutra_platform::inbox::delivery::NotificationDelivery {
        interval: std::time::Duration::from_secs(1),
        mailer: mailer.clone(),
        public_url: "http://localhost:3000".into(),
        batch_size: 1000,
        max_attempts: 3,
        send_timeout: std::time::Duration::from_secs(5),
    };
    loop {
        if matches!(run_once(&rt, &job).await.unwrap(), JobOutcome::Ran { .. }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    let sent = mailer.sent();
    let mine: Vec<_> = sent
        .iter()
        .filter(|m| m.to == "aarav@sutra.dev" && m.subject.contains("Library hours"))
        .collect();
    assert_eq!(mine.len(), 1);
    assert!(mine[0].text.contains("http://localhost:3000/notices"));
    let audit: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1::uuid AND action = 'platform.profile.update'",
    )
    .bind(t)
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(audit, 1);
}

#[tokio::test]
async fn security_page_sessions_on_postgres() {
    let t = fresh_tenant();
    let (a, _) = app(t).await;
    let first = login(&a, "meera.shah@sutra.dev").await;
    let second = login(&a, "meera.shah@sutra.dev").await;
    let (_, _, list) = send(&a, get_as(&first, "/api/v1/auth/sessions")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let (s, _, r) = send(
        &a,
        json_req(
            "POST",
            "/api/v1/auth/sessions/revoke-others",
            &first,
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!((s, r["revoked"].as_i64()), (StatusCode::OK, Some(1)));
    let (s, _, _) = send(&a, get_as(&second, "/api/v1/auth/session")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (_, _, mfa) = send(&a, get_as(&first, "/api/v1/auth/mfa")).await;
    assert_eq!(mfa["managedBy"], "sutra");
    assert_eq!(mfa["webauthnAvailable"], true);
}

fn permission_denied(e: sqlx::Error) -> bool {
    matches!(&e, sqlx::Error::Database(d) if d.code().as_deref() == Some("42501"))
}

#[tokio::test]
async fn runtime_role_is_least_privilege_and_owns_nothing() {
    let owner = owner_pool().await;
    let rt = runtime_pool().await;
    let (me, sup): (String, bool) = sqlx::query_as(
        "SELECT current_user::text, rolsuper FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(&rt)
    .await
    .unwrap();
    assert_eq!((me.as_str(), sup), (RUNTIME_USER, false));
    let member: bool =
        sqlx::query_scalar("SELECT pg_has_role(current_user, 'sutra_app', 'member')")
            .fetch_one(&rt)
            .await
            .unwrap();
    assert!(member);
    let owned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_tables WHERE tableowner = current_user \
            OR tableowner IN ('sutra_app', 'sutra_platform', 'sutra_institution')",
    )
    .fetch_one(&rt)
    .await
    .unwrap();
    assert_eq!(owned, 0, "runtime roles own no tables");

    // DML that the application needs works… inside the tenant context the API's session middleware
    // gives every request (Part 1 S3, DEF-X-003: RLS is forced on tenant tables).
    let t = fresh_tenant();
    let insert = "INSERT INTO platform.notification (tenant_id, recipient_role, category, title) \
         VALUES ($1::uuid, 'student', 'system', 'grant check') RETURNING id";
    // Without a context (a code path that lost it) the write is refused: fails closed.
    let refused = sqlx::query_scalar::<_, uuid::Uuid>(insert)
        .bind(t)
        .fetch_one(&rt)
        .await
        .expect_err("no tenant context: RLS refuses the write");
    assert!(permission_denied(refused));
    let scope = sutra_kernel::scope::ExecutionScope::tenant(t.parse().unwrap());
    sutra_kernel::db_context::scoped(scope, async {
        let id: uuid::Uuid = sqlx::query_scalar(insert)
            .bind(t)
            .fetch_one(&rt)
            .await
            .unwrap();
        sqlx::query("DELETE FROM platform.notification WHERE id = $1")
            .bind(id)
            .execute(&rt)
            .await
            .unwrap();
    })
    .await;
    sqlx::query("SELECT count(*) FROM institution.institution")
        .execute(&rt)
        .await
        .unwrap();

    // …but no DDL, no ownership powers, and audit evidence is append-only.
    for sql in [
        "UPDATE platform.audit_log SET action = 'x' WHERE false",
        "DELETE FROM platform.audit_log WHERE false",
        "TRUNCATE platform.outbox",
        "CREATE TABLE platform.evil (x int)",
        "CREATE TABLE public.evil (x int)",
        "ALTER TABLE institution.institution ADD COLUMN evil int",
        "DROP TABLE platform.session",
        "CREATE SCHEMA evil",
    ] {
        let err = sqlx::query(sql).execute(&rt).await.expect_err(sql);
        assert!(permission_denied(err), "{sql} must be refused with 42501");
    }

    // Per-module roles: a module may write the shared outbox/audit tables, nothing else foreign.
    let check = |role: &'static str, table: &'static str, privilege: &'static str| {
        let owner = owner.clone();
        async move {
            sqlx::query_scalar::<_, bool>("SELECT has_table_privilege($1, $2, $3)")
                .bind(role)
                .bind(table)
                .bind(privilege)
                .fetch_one(&owner)
                .await
                .unwrap()
        }
    };
    assert!(check("sutra_institution", "platform.outbox", "INSERT").await);
    assert!(check("sutra_institution", "platform.audit_log", "INSERT").await);
    assert!(!check("sutra_institution", "platform.outbox", "UPDATE").await);
    assert!(!check("sutra_institution", "platform.session", "SELECT").await);
    assert!(!check("sutra_student", "institution.institution", "SELECT").await);
    assert!(check("sutra_platform", "platform.notice", "DELETE").await);
    assert!(!check("sutra_platform", "platform.audit_log", "UPDATE").await);
    assert!(!check("sutra_platform", "platform.audit_log", "DELETE").await);
}
