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

async fn app_with(
    tenant: &'static str,
    extra: &[(&'static str, &'static str)],
) -> (Router, sqlx::PgPool) {
    let url: &'static str = Box::leak(
        std::env::var("DATABASE_URL")
            .expect("DATABASE_URL must be set for db-tests")
            .into_boxed_str(),
    );
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", url),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
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
    sutra_persistence::migrate::run(&ctx.pool)
        .await
        .expect("migrations");
    let pool = ctx.pool.clone();
    (router(ctx), pool)
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
            serde_json::json!({"loginId": login_id, "password": "sutra"}).to_string(),
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
