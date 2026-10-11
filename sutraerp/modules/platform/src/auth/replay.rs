//! Single-use identifier guard ([`ReplayGuard`]) — used for the `jti` of OIDC back-channel
//! logout tokens so a captured token cannot be replayed to end sessions again.
//!
//! Only `SHA-256(namespace | key)` is stored. `first_use` is one atomic insert: the first caller
//! wins, concurrent and later callers see `false` until the entry expires.

use super::session::Clock;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sutra_contracts::platform::auth::ReplayGuard;
use sutra_kernel::error::KernelError;
use sutra_kernel::secret;
use sutra_persistence::map_db_error;

fn digest(namespace: &str, key: &str) -> [u8; 32] {
    secret::sha256(format!("{namespace}|{key}").as_bytes())
}

/// No-database guard (dev/tests). Process local.
pub struct InMemoryReplayGuard {
    clock: Clock,
    seen: Mutex<HashMap<[u8; 32], DateTime<Utc>>>,
}

impl Default for InMemoryReplayGuard {
    fn default() -> Self {
        Self::with_clock(Arc::new(Utc::now))
    }
}

impl InMemoryReplayGuard {
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            clock,
            seen: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl ReplayGuard for InMemoryReplayGuard {
    async fn first_use(
        &self,
        namespace: &str,
        key: &str,
        ttl: Duration,
    ) -> Result<bool, KernelError> {
        let now = (self.clock)();
        let mut seen = self
            .seen
            .lock()
            .map_err(|_| KernelError::internal_msg("replay guard poisoned"))?;
        seen.retain(|_, until| *until > now);
        let k = digest(namespace, key);
        if seen.contains_key(&k) {
            return Ok(false);
        }
        seen.insert(k, now + ttl.max(Duration::seconds(1)));
        Ok(true)
    }
}

/// `platform.replay_guard`.
#[derive(Clone)]
pub struct PgReplayGuard {
    pool: PgPool,
}

impl PgReplayGuard {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl ReplayGuard for PgReplayGuard {
    async fn first_use(
        &self,
        namespace: &str,
        key: &str,
        ttl: Duration,
    ) -> Result<bool, KernelError> {
        let secs = ttl.num_seconds().max(1) as f64;
        // An expired entry may be overwritten (the sweeper deletes them eventually anyway).
        let n = sqlx::query(
            "INSERT INTO platform.replay_guard (key_hash, namespace, expires_at) \
             VALUES ($1, $2, now() + make_interval(secs => $3)) \
             ON CONFLICT (key_hash) DO UPDATE SET expires_at = EXCLUDED.expires_at, created_at = now() \
             WHERE platform.replay_guard.expires_at <= now()",
        )
        .bind(digest(namespace, key).as_slice())
        .bind(namespace)
        .bind(secs)
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n == 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[tokio::test]
    async fn first_use_wins_until_expiry() {
        let t = Arc::new(AtomicI64::new(1_000));
        let tc = t.clone();
        let g = InMemoryReplayGuard::with_clock(Arc::new(move || {
            DateTime::from_timestamp(tc.load(Ordering::SeqCst), 0).unwrap_or_default()
        }));
        let ttl = Duration::seconds(60);
        assert!(g.first_use("jti", "a", ttl).await.unwrap_or(false));
        assert!(
            !g.first_use("jti", "a", ttl).await.unwrap_or(true),
            "replay"
        );
        assert!(
            g.first_use("other", "a", ttl).await.unwrap_or(false),
            "namespaced"
        );
        t.store(1_061, Ordering::SeqCst);
        assert!(
            g.first_use("jti", "a", ttl).await.unwrap_or(false),
            "expired"
        );
    }

    /// Live PostgreSQL 18: `platform.replay_guard`.
    #[cfg(feature = "db-tests")]
    mod pg {
        use super::*;

        async fn pool() -> PgPool {
            let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for db-tests");
            let pool =
                sutra_persistence::pool::connect_lazy(&sutra_persistence::pool::PoolSettings {
                    url,
                    max_connections: 8,
                    min_connections: 0,
                    acquire_timeout: std::time::Duration::from_secs(10),
                    application_name: "sutra-platform-replay-db-tests".into(),
                })
                .expect("pool");
            sutra_persistence::migrate::run(&pool)
                .await
                .expect("migrations");
            pool
        }

        #[tokio::test]
        async fn pg_first_use_wins_is_namespaced_and_reusable_after_expiry() {
            let pool = pool().await;
            let g = PgReplayGuard::new(pool.clone());
            let key = uuid::Uuid::now_v7().to_string();
            let ttl = Duration::seconds(300);
            assert!(g.first_use("jti", &key, ttl).await.unwrap());
            assert!(!g.first_use("jti", &key, ttl).await.unwrap(), "replay");
            assert!(g.first_use("other", &key, ttl).await.unwrap(), "namespaced");

            // Only the digest is stored, never the identifier itself.
            let (hash, ns): (Vec<u8>, String) = sqlx::query_as(
                "SELECT key_hash, namespace FROM platform.replay_guard WHERE key_hash = $1",
            )
            .bind(digest("jti", &key).as_slice())
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(hash.len(), 32);
            assert_eq!(ns, "jti");

            // Once expired, the identifier may be used again (and is then guarded again).
            sqlx::query(
                "UPDATE platform.replay_guard SET expires_at = now() - interval '1 second' WHERE key_hash = $1",
            )
            .bind(digest("jti", &key).as_slice())
            .execute(&pool)
            .await
            .unwrap();
            assert!(g.first_use("jti", &key, ttl).await.unwrap(), "expired");
            assert!(!g.first_use("jti", &key, ttl).await.unwrap());
        }

        #[tokio::test]
        async fn pg_concurrent_first_use_has_exactly_one_winner() {
            let pool = pool().await;
            let g = Arc::new(PgReplayGuard::new(pool));
            let key = uuid::Uuid::now_v7().to_string();
            let tasks: Vec<_> = (0..8)
                .map(|_| {
                    let g = g.clone();
                    let key = key.clone();
                    tokio::spawn(
                        async move { g.first_use("jti", &key, Duration::seconds(60)).await },
                    )
                })
                .collect();
            let mut wins = 0;
            for t in tasks {
                if t.await.unwrap().unwrap() {
                    wins += 1;
                }
            }
            assert_eq!(wins, 1);
        }
    }
}
