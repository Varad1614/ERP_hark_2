//! Live-database tests of the durable session store. Run with:
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra cargo test -p sutra-platform --features db-tests
//! ```
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;
use sutra_contracts::platform::auth::{Principal, Role, SessionPolicy, SessionStore};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::{migrate, pool};
use sutra_platform::auth::session::token_hash;
use sutra_platform::auth::session_pg::PgSessionStore;

async fn db() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for db-tests");
    let p = pool::connect_lazy(&pool::PoolSettings {
        url,
        max_connections: 4,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(5),
        application_name: "sutra-platform-db-tests".into(),
    })
    .expect("pool");
    migrate::run(&p).await.expect("migrations");
    p
}

fn store(p: &PgPool) -> PgSessionStore {
    PgSessionStore::new(p.clone(), SessionPolicy::new(1800, 28800).unwrap())
}

fn principal() -> Principal {
    Principal {
        subject: format!("dev|{}", uuid::Uuid::now_v7()),
        display_name: "Dr. Meera Shah".into(),
        login_id: "meera.shah@sutra.dev".into(),
        roles: vec![Role::Faculty, Role::Administrator],
        scope: ExecutionScope::institution(TenantId::new(), InstitutionId::new()),
    }
}

#[tokio::test]
async fn create_get_revoke_roundtrip() {
    let p = db().await;
    let s = store(&p);
    let who = principal();
    let issued = s.create(who.clone()).await.unwrap();
    assert_eq!(issued.token.len(), 64);
    assert_eq!(issued.session.principal, who);
    assert_eq!(issued.session.csrf_token.len(), 64);
    assert!(issued.session.expires_at > issued.session.created_at);

    let got = s.get(&issued.token).await.unwrap().expect("live session");
    assert_eq!(got.id, issued.session.id);
    assert_eq!(got.principal, who, "roles and scope survive the round trip");
    assert_eq!(
        got.csrf_token, issued.session.csrf_token,
        "CSRF token is stable"
    );
    assert!(got.last_seen_at >= issued.session.last_seen_at);
    assert!(got.idle_expires_at <= got.expires_at);

    assert!(s.get("unknown-token").await.unwrap().is_none());
    s.revoke(&issued.token).await.unwrap();
    assert!(s.get(&issued.token).await.unwrap().is_none());
    s.revoke(&issued.token).await.unwrap(); // idempotent
    let revoked: Option<chrono::DateTime<chrono::Utc>> =
        sqlx::query_scalar("SELECT revoked_at FROM platform.session WHERE id = $1")
            .bind(issued.session.id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(revoked.is_some(), "revocation is recorded, not deleted");
}

#[tokio::test]
async fn only_the_token_digest_is_stored() {
    let p = db().await;
    let issued = store(&p).create(principal()).await.unwrap();
    let (hash, secret): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT token_hash, csrf_secret FROM platform.session WHERE id = $1")
            .bind(issued.session.id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert_eq!(hash, token_hash(&issued.token).to_vec());
    assert_ne!(hash, issued.token.as_bytes());
    assert_eq!(secret.len(), 32);
    // Neither the token nor the CSRF token appears anywhere in the row.
    let row_text: String =
        sqlx::query_scalar("SELECT row_to_json(s)::text FROM platform.session s WHERE id = $1")
            .bind(issued.session.id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(!row_text.contains(&issued.token));
    assert!(!row_text.contains(&issued.session.csrf_token));
}

#[tokio::test]
async fn idle_expiry() {
    let p = db().await;
    let s = store(&p);
    let issued = s.create(principal()).await.unwrap();
    // 29 minutes idle: alive, and the lookup slides the window.
    sqlx::query(
        "UPDATE platform.session SET last_seen_at = now() - interval '29 minutes' WHERE id = $1",
    )
    .bind(issued.session.id)
    .execute(&p)
    .await
    .unwrap();
    assert!(s.get(&issued.token).await.unwrap().is_some());
    let seen: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM platform.session WHERE id = $1")
            .bind(issued.session.id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(
        chrono::Utc::now() - seen < chrono::Duration::minutes(1),
        "activity recorded"
    );
    // 31 minutes idle: dead, and a lookup does not revive it.
    sqlx::query(
        "UPDATE platform.session SET last_seen_at = now() - interval '31 minutes' WHERE id = $1",
    )
    .bind(issued.session.id)
    .execute(&p)
    .await
    .unwrap();
    assert!(s.get(&issued.token).await.unwrap().is_none());
    assert!(s.get(&issued.token).await.unwrap().is_none());
}

#[tokio::test]
async fn absolute_expiry() {
    let p = db().await;
    let s = store(&p);
    let issued = s.create(principal()).await.unwrap();
    sqlx::query(
        "UPDATE platform.session SET created_at = now() - interval '9 hours', \
         expires_at = now() - interval '1 hour' WHERE id = $1",
    )
    .bind(issued.session.id)
    .execute(&p)
    .await
    .unwrap();
    assert!(
        s.get(&issued.token).await.unwrap().is_none(),
        "activity cannot outlive the absolute expiry"
    );
}

#[tokio::test]
async fn unknown_stored_roles_grant_nothing() {
    let p = db().await;
    let s = store(&p);
    let issued = s.create(principal()).await.unwrap();
    sqlx::query("UPDATE platform.session SET roles = ARRAY['superuser', 'faculty'] WHERE id = $1")
        .bind(issued.session.id)
        .execute(&p)
        .await
        .unwrap();
    let got = s.get(&issued.token).await.unwrap().unwrap();
    assert_eq!(got.principal.roles, vec![Role::Faculty]);
}

#[tokio::test]
async fn distinct_sessions_have_distinct_secrets() {
    let p = db().await;
    let s = store(&p);
    let a = s.create(principal()).await.unwrap();
    let b = s.create(principal()).await.unwrap();
    assert_ne!(a.token, b.token);
    assert_ne!(a.session.csrf_token, b.session.csrf_token);
    assert_ne!(a.session.id, b.session.id);
}

#[tokio::test]
async fn purge_removes_long_dead_sessions_only() {
    let p = db().await;
    let s = store(&p);
    let live = s.create(principal()).await.unwrap();
    let dead = s.create(principal()).await.unwrap();
    sqlx::query(
        "UPDATE platform.session SET created_at = now() - interval '10 days', \
         last_seen_at = now() - interval '10 days', expires_at = now() - interval '9 days' WHERE id = $1",
    )
    .bind(dead.session.id)
    .execute(&p)
    .await
    .unwrap();
    let removed = s.purge_expired(chrono::Duration::days(1)).await.unwrap();
    assert!(removed >= 1);
    let remaining: Vec<uuid::Uuid> =
        sqlx::query_scalar("SELECT id FROM platform.session WHERE id = ANY($1)")
            .bind(vec![live.session.id, dead.session.id])
            .fetch_all(&p)
            .await
            .unwrap();
    assert_eq!(remaining, vec![live.session.id]);
}
