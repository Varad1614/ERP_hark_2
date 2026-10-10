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
    ]);
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
