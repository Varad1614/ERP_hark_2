//! Live-database tests of the Batch 1B auth tables: `platform.pre_auth`, `platform.mfa_factor`
//! and the new `platform.session` columns / write throttling. Run with:
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra cargo test -p sutra-platform --features db-tests
//! ```
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::platform::auth::{
    MfaFactorStore, MfaLevel, NewSession, PreAuthKind, PreAuthStore, Principal, Role,
    SessionPolicy, SessionStore,
};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::{migrate, pool};
use sutra_platform::auth::mfa::{
    base32_decode, totp_code, totp_step, MfaService, PgMfaFactorStore, SecretBox,
};
use sutra_platform::auth::preauth::PgPreAuthStore;
use sutra_platform::auth::session_pg::PgSessionStore;

async fn db() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for db-tests");
    let p = pool::connect_lazy(&pool::PoolSettings {
        url,
        max_connections: 4,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(5),
        application_name: "sutra-platform-auth-db-tests".into(),
    })
    .expect("pool");
    migrate::run(&p).await.expect("migrations");
    p
}

fn principal() -> Principal {
    Principal {
        subject: format!("dev|{}", uuid::Uuid::now_v7()),
        display_name: "Dhruv Naik".into(),
        login_id: "admin@sutra.dev".into(),
        roles: vec![Role::Administrator],
        scope: ExecutionScope::tenant(TenantId::new()),
    }
}

#[tokio::test]
async fn pre_auth_is_single_use_kind_bound_and_expires() {
    let p = db().await;
    let s = PgPreAuthStore::new(p.clone());
    let h = s
        .put(
            PreAuthKind::OidcAuthorization,
            serde_json::json!({"nonce": "n"}),
            chrono::Duration::seconds(600),
        )
        .await
        .unwrap();
    // Only the digest is stored.
    let raw: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.pre_auth WHERE encode(handle_hash, 'hex') = $1",
    )
    .bind(&h)
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!(raw, 0);
    assert!(
        s.take(PreAuthKind::MfaChallenge, &h)
            .await
            .unwrap()
            .is_none(),
        "kind bound"
    );
    assert_eq!(
        s.take(PreAuthKind::OidcAuthorization, &h).await.unwrap(),
        Some(serde_json::json!({"nonce": "n"}))
    );
    assert!(
        s.take(PreAuthKind::OidcAuthorization, &h)
            .await
            .unwrap()
            .is_none(),
        "single use"
    );

    let h = s
        .put(
            PreAuthKind::MfaChallenge,
            serde_json::json!({}),
            chrono::Duration::seconds(600),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE platform.pre_auth SET created_at = now() - interval '2 hours', \
         expires_at = now() - interval '1 second' \
         WHERE handle_hash = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(&h)
    .execute(&p)
    .await
    .unwrap();
    assert!(
        s.take(PreAuthKind::MfaChallenge, &h)
            .await
            .unwrap()
            .is_none(),
        "expired"
    );
    let h2 = s
        .put(
            PreAuthKind::MfaChallenge,
            serde_json::json!({}),
            chrono::Duration::seconds(1),
        )
        .await
        .unwrap();
    sqlx::query(
        "UPDATE platform.pre_auth SET created_at = now() - interval '1 hour', \
         expires_at = now() - interval '1 minute' WHERE handle_hash = sha256(convert_to($1, 'UTF8'))",
    )
    .bind(&h2)
    .execute(&p)
    .await
    .unwrap();
    assert!(s.purge_expired().await.unwrap() >= 1);
}

#[tokio::test]
async fn mfa_factor_store_encrypts_activates_and_refuses_replay() {
    let p = db().await;
    let store: Arc<dyn MfaFactorStore> = Arc::new(PgMfaFactorStore::new(p.clone()));
    let svc = MfaService::new(
        store.clone(),
        SecretBox::new(&[9u8; 32]).unwrap(),
        "SutraERP".into(),
    );
    let who = principal();
    assert!(!svc.has_totp(&who).await.unwrap());
    let e = svc.begin_totp_enrolment(&who).await.unwrap();
    let secret = base32_decode(&e.secret_b32).unwrap();
    let pending = store
        .pending_totp(&who.scope, &who.subject)
        .await
        .unwrap()
        .unwrap();
    assert!(!pending.active);
    assert!(!pending
        .secret_ciphertext
        .windows(secret.len())
        .any(|w| w == secret.as_slice()));
    // Re-starting enrolment replaces the pending row (unique per status).
    let e = svc.begin_totp_enrolment(&who).await.unwrap();
    let secret = base32_decode(&e.secret_b32).unwrap();
    let rows: i64 =
        sqlx::query_scalar("SELECT count(*) FROM platform.mfa_factor WHERE subject = $1")
            .bind(&who.subject)
            .fetch_one(&p)
            .await
            .unwrap();
    assert_eq!(rows, 1);

    let now = chrono::Utc::now().timestamp();
    let code = totp_code(&secret, totp_step(now));
    svc.confirm_totp_enrolment(&who, &code).await.unwrap();
    let active = store.totp(&who.scope, &who.subject).await.unwrap().unwrap();
    assert!(active.active);
    let step = active.last_used_step.unwrap();
    // Replay within the same step (and of older steps) is refused atomically.
    assert!(!store.consume_totp_step(active.id, step).await.unwrap());
    assert!(!store.consume_totp_step(active.id, step - 1).await.unwrap());
    assert!(svc.verify_totp(&who, &code).await.is_err());
    assert!(store.consume_totp_step(active.id, step + 1).await.unwrap());
    // Another tenant with the same subject sees no factor (and could not decrypt it).
    let mut other = who.clone();
    other.scope = ExecutionScope::tenant(TenantId::new());
    assert!(!svc.has_totp(&other).await.unwrap());
    let (confirmed, status): (Option<chrono::DateTime<chrono::Utc>>, String) =
        sqlx::query_as("SELECT confirmed_at, status FROM platform.mfa_factor WHERE id = $1")
            .bind(active.id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(confirmed.is_some());
    assert_eq!(status, "active");
}

#[tokio::test]
async fn session_mfa_columns_not_after_and_throttled_touch() {
    let p = db().await;
    let s = PgSessionStore::new(
        p.clone(),
        SessionPolicy::new(1800, 28800)
            .and_then(|x| x.with_touch_interval(60))
            .unwrap(),
    );
    let mut new = NewSession::password_and_totp(principal());
    new.idp_sid = Some("kc-sid-42".into());
    let cap = chrono::Utc::now() + chrono::Duration::minutes(10);
    new.not_after = Some(cap);
    let issued = s.create(new).await.unwrap();
    assert_eq!(issued.session.mfa_level, MfaLevel::Mfa);
    assert!(issued.session.amr.contains(&"otp".to_owned()));
    assert_eq!(issued.session.idp_sid.as_deref(), Some("kc-sid-42"));
    assert!((issued.session.expires_at - cap).num_milliseconds().abs() < 5);
    let got = s.get(&issued.token).await.unwrap().unwrap();
    assert_eq!(got.mfa_level, MfaLevel::Mfa);
    assert_eq!(got.idp_sid.as_deref(), Some("kc-sid-42"));

    // Fresh (< 60 s): a lookup does not write last_seen_at.
    sqlx::query(
        "UPDATE platform.session SET last_seen_at = now() - interval '30 seconds' WHERE id = $1",
    )
    .bind(issued.session.id)
    .execute(&p)
    .await
    .unwrap();
    let before: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM platform.session WHERE id = $1")
            .bind(issued.session.id)
            .fetch_one(&p)
            .await
            .unwrap();
    let got = s.get(&issued.token).await.unwrap().unwrap();
    assert_eq!(got.last_seen_at, before, "returned value is the stored one");
    let after: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM platform.session WHERE id = $1")
            .bind(issued.session.id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert_eq!(before, after, "no write within the touch interval");
    // Stale (> 60 s): written.
    sqlx::query(
        "UPDATE platform.session SET last_seen_at = now() - interval '90 seconds' WHERE id = $1",
    )
    .bind(issued.session.id)
    .execute(&p)
    .await
    .unwrap();
    let got = s.get(&issued.token).await.unwrap().unwrap();
    assert!(chrono::Utc::now() - got.last_seen_at < chrono::Duration::seconds(5));
    // Revoked sessions are not touched or returned.
    s.revoke(&issued.token).await.unwrap();
    sqlx::query(
        "UPDATE platform.session SET last_seen_at = now() - interval '90 seconds' WHERE id = $1",
    )
    .bind(issued.session.id)
    .execute(&p)
    .await
    .unwrap();
    assert!(s.get(&issued.token).await.unwrap().is_none());
    let untouched: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT last_seen_at FROM platform.session WHERE id = $1")
            .bind(issued.session.id)
            .fetch_one(&p)
            .await
            .unwrap();
    assert!(chrono::Utc::now() - untouched > chrono::Duration::seconds(60));

    // A not_after cap in the past cannot mint a session.
    let mut past = NewSession::password(principal());
    past.not_after = Some(chrono::Utc::now() - chrono::Duration::seconds(5));
    assert!(s.create(past).await.is_err());
}
