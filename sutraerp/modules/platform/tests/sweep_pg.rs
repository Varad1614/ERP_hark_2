//! Live-database tests of the platform sweeper jobs (sessions, pre-auth records). Run with:
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra cargo test -p sutra-platform --features db-tests
//! ```
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;
use std::time::Duration;
use sutra_persistence::jobs::{self, JobOutcome, SweepJob};
use sutra_persistence::{migrate, pool};
use sutra_platform::sweep::{PreAuthPurge, SessionPurge};
use uuid::Uuid;

async fn db() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for db-tests");
    let p = pool::connect_lazy(&pool::PoolSettings {
        url,
        max_connections: 4,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        application_name: "sutra-platform-sweep-db-tests".into(),
    })
    .expect("pool");
    migrate::run(&p).await.expect("migrations");
    p
}

/// Runs the job, waiting out a concurrent holder of its lock.
async fn run(p: &PgPool, job: &dyn SweepJob) -> u64 {
    for _ in 0..100 {
        match jobs::run_once(p, job).await.unwrap() {
            JobOutcome::Ran { affected } => return affected,
            JobOutcome::Skipped => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    panic!("job never acquired its lock");
}

fn random32() -> Vec<u8> {
    sutra_kernel::secret::random_bytes().unwrap().to_vec()
}

/// Inserts a session with timestamps relative to now (seconds; negative = past).
async fn session(
    p: &PgPool,
    tenant: Uuid,
    created: i64,
    last_seen: i64,
    expires: i64,
    revoked: Option<i64>,
) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO platform.session (token_hash, csrf_secret, subject, login_id, display_name, tenant_id, \
           created_at, last_seen_at, expires_at, revoked_at) \
         VALUES ($1, $2, 'dev|sweep', 'sweep@sutra.dev', 'Sweep', $3, \
           now() + make_interval(secs => $4), now() + make_interval(secs => $5), \
           now() + make_interval(secs => $6), now() + make_interval(secs => $7)) RETURNING id",
    )
    .bind(random32())
    .bind(random32())
    .bind(tenant)
    .bind(created as f64)
    .bind(last_seen as f64)
    .bind(expires as f64)
    .bind(revoked.map(|r| r as f64))
    .fetch_one(p)
    .await
    .unwrap()
}

#[tokio::test]
async fn session_purge_removes_only_sessions_dead_past_the_grace_period() {
    let p = db().await;
    let tenant = Uuid::now_v7();
    const H: i64 = 3600;
    const D: i64 = 24 * H;
    // live: recent activity, expires later
    let live = session(&p, tenant, -H, -60, 7 * H, None).await;
    // expired 2 days ago → purged (grace 1 day)
    let expired_old = session(&p, tenant, -3 * D, -3 * D + 60, -2 * D, None).await;
    // expired 1 hour ago → kept (inside grace)
    let expired_recent = session(&p, tenant, -9 * H, -2 * H, -H, None).await;
    // revoked 2 days ago (otherwise valid) → purged
    let revoked_old = session(&p, tenant, -3 * D, -2 * D, 30 * D, Some(-2 * D)).await;
    // revoked just now → kept
    let revoked_recent = session(&p, tenant, -H, -60, 7 * H, Some(-10)).await;
    // idle for 3 days (idle 30 min + grace 1 day exceeded) though absolute expiry is far → purged
    let idle_old = session(&p, tenant, -4 * D, -3 * D, 30 * D, None).await;
    let job = SessionPurge {
        interval: Duration::from_secs(60),
        grace: Duration::from_secs(D as u64),
        idle_timeout: Duration::from_secs(1800),
        batch_limit: 100_000,
    };
    assert!(run(&p, &job).await >= 3);
    let mut left: Vec<Uuid> =
        sqlx::query_scalar("SELECT id FROM platform.session WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_all(&p)
            .await
            .unwrap();
    left.sort();
    let mut expected = vec![live, expired_recent, revoked_recent];
    expected.sort();
    assert_eq!(left, expected);
    for gone in [expired_old, revoked_old, idle_old] {
        assert!(!left.contains(&gone));
    }
}

#[tokio::test]
async fn session_purge_respects_the_batch_limit() {
    let p = db().await;
    let tenant = Uuid::now_v7();
    for _ in 0..3 {
        session(&p, tenant, -10 * 86400, -10 * 86400, -9 * 86400, None).await;
    }
    let job = SessionPurge {
        interval: Duration::from_secs(60),
        grace: Duration::ZERO,
        idle_timeout: Duration::from_secs(1800),
        batch_limit: 1,
    };
    assert_eq!(run(&p, &job).await, 1, "one row per run at batch_limit = 1");
}

#[tokio::test]
async fn pre_auth_purge_removes_expired_records_only() {
    let p = db().await;
    let insert = |created: i64, expires: i64| {
        let p = p.clone();
        async move {
            let h = random32();
            sqlx::query(
                "INSERT INTO platform.pre_auth (handle_hash, kind, payload, created_at, expires_at) \
                 VALUES ($1, 'oidc', '{}'::jsonb, now() + make_interval(secs => $2), now() + make_interval(secs => $3))",
            )
            .bind(&h)
            .bind(created as f64)
            .bind(expires as f64)
            .execute(&p)
            .await
            .unwrap();
            h
        }
    };
    let expired = insert(-1200, -600).await;
    let in_grace = insert(-700, -30).await;
    let live = insert(-10, 600).await;
    let job = PreAuthPurge {
        interval: Duration::from_secs(60),
        grace: Duration::from_secs(300),
        batch_limit: 100_000,
    };
    assert!(run(&p, &job).await >= 1);
    let exists = |h: Vec<u8>| {
        let p = p.clone();
        async move {
            sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS (SELECT 1 FROM platform.pre_auth WHERE handle_hash = $1)",
            )
            .bind(h)
            .fetch_one(&p)
            .await
            .unwrap()
        }
    };
    assert!(!exists(expired).await);
    assert!(exists(in_grace).await);
    assert!(exists(live).await);
}
