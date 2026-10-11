//! Valkey adapter of the advisory [`Cache`] port (ADR-L5-0037).
//!
//! * Lazy: no connection at startup, so a missing Valkey never blocks the API.
//! * Every command is bounded by `timeout`; after a failure the adapter fails fast for
//!   `cooldown` instead of queueing requests behind a dead server. Callers (the
//!   `AdvisoryCache`) turn every error into a miss.
//! * `rediss://` URLs use rustls (webpki roots); credentials go in the URL.

use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::AsyncCommands;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use sutra_kernel::cache::{Cache, CacheError, CacheKey};
use sutra_kernel::health::ReadinessCheck;
use tokio::sync::OnceCell;

#[derive(Clone, Debug)]
pub struct ValkeySettings {
    /// `redis://[user:pass@]host:port[/db]` or `rediss://…` (TLS).
    pub url: String,
    /// Bound on each command (connect included).
    pub timeout: Duration,
    /// Fail-fast period after an error.
    pub cooldown: Duration,
}

impl Default for ValkeySettings {
    fn default() -> Self {
        Self {
            url: "redis://localhost:6379".into(),
            timeout: Duration::from_millis(250),
            cooldown: Duration::from_secs(5),
        }
    }
}

pub struct ValkeyCache {
    client: redis::Client,
    settings: ValkeySettings,
    conn: OnceCell<ConnectionManager>,
    failed_at: Mutex<Option<Instant>>,
}

impl ValkeyCache {
    /// Validates the URL; does not connect.
    pub fn new(settings: ValkeySettings) -> Result<Self, String> {
        let client = redis::Client::open(settings.url.as_str())
            .map_err(|e| format!("invalid SUTRA_VALKEY_URL: {e}"))?;
        Ok(Self {
            client,
            settings,
            conn: OnceCell::new(),
            failed_at: Mutex::new(None),
        })
    }

    fn cooling_down(&self) -> bool {
        let g = self.failed_at.lock().unwrap_or_else(|p| p.into_inner());
        g.is_some_and(|t| t.elapsed() < self.settings.cooldown)
    }

    fn record(&self, ok: bool) {
        let mut g = self.failed_at.lock().unwrap_or_else(|p| p.into_inner());
        *g = if ok { None } else { Some(Instant::now()) };
    }

    async fn manager(&self) -> Result<ConnectionManager, CacheError> {
        if self.cooling_down() {
            return Err(CacheError("valkey cooling down after a failure".into()));
        }
        let m = self
            .conn
            .get_or_try_init(|| async {
                let cfg = ConnectionManagerConfig::new()
                    .set_connection_timeout(Some(self.settings.timeout))
                    .set_response_timeout(Some(self.settings.timeout))
                    .set_number_of_retries(1);
                ConnectionManager::new_lazy_with_config(self.client.clone(), cfg)
                    .map_err(|e| CacheError(e.to_string()))
            })
            .await?;
        Ok(m.clone())
    }

    /// Runs `f` with a timeout and updates the cool-down state.
    async fn call<T, F, Fut>(&self, f: F) -> Result<T, CacheError>
    where
        F: FnOnce(ConnectionManager) -> Fut,
        Fut: std::future::Future<Output = redis::RedisResult<T>>,
    {
        let m = self.manager().await?;
        let r = match tokio::time::timeout(self.settings.timeout, f(m)).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(CacheError(e.to_string())),
            Err(_) => Err(CacheError("valkey command timed out".into())),
        };
        self.record(r.is_ok());
        r
    }

    /// `PING` (used by readiness).
    pub async fn ping(&self) -> Result<(), CacheError> {
        self.call(|mut c| async move { redis::cmd("PING").query_async::<String>(&mut c).await })
            .await
            .map(|_| ())
    }
}

#[async_trait::async_trait]
impl Cache for ValkeyCache {
    async fn get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>, CacheError> {
        let k = key.as_str().to_owned();
        self.call(|mut c| async move { c.get::<_, Option<Vec<u8>>>(k).await })
            .await
    }

    async fn set(&self, key: &CacheKey, value: &[u8], ttl: Duration) -> Result<(), CacheError> {
        let k = key.as_str().to_owned();
        let v = value.to_vec();
        let ms = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1);
        self.call(|mut c| async move { c.pset_ex::<_, _, ()>(k, v, ms).await })
            .await
    }

    async fn delete(&self, key: &CacheKey) -> Result<(), CacheError> {
        let k = key.as_str().to_owned();
        self.call(|mut c| async move { c.del::<_, ()>(k).await })
            .await
    }

    fn backend(&self) -> &'static str {
        "valkey"
    }
}

/// Readiness of the advisory cache. Non-critical: a cache outage degrades to misses.
pub struct ValkeyReadiness(pub std::sync::Arc<ValkeyCache>);

#[async_trait::async_trait]
impl ReadinessCheck for ValkeyReadiness {
    fn name(&self) -> &'static str {
        "valkey"
    }

    async fn check(&self) -> Result<(), String> {
        self.0.ping().await.map_err(|e| e.0)
    }

    fn critical(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::cache::CacheNamespace;
    use sutra_kernel::scope::{ExecutionScope, TenantId};

    #[test]
    fn rejects_invalid_urls() {
        assert!(ValkeyCache::new(ValkeySettings {
            url: "http://nope".into(),
            ..ValkeySettings::default()
        })
        .is_err());
        assert!(ValkeyCache::new(ValkeySettings::default()).is_ok());
        assert!(ValkeyCache::new(ValkeySettings {
            url: "rediss://cache.example:6380".into(),
            ..ValkeySettings::default()
        })
        .is_ok());
    }

    #[tokio::test]
    async fn unreachable_server_fails_fast_and_cools_down() {
        let c = ValkeyCache::new(ValkeySettings {
            url: "redis://127.0.0.1:1".into(),
            timeout: Duration::from_millis(200),
            cooldown: Duration::from_secs(30),
        })
        .unwrap_or_else(|e| panic!("{e}"));
        let k = CacheNamespace::new("t").key("o", &ExecutionScope::tenant(TenantId::new()), &["k"]);
        assert!(c.get(&k).await.is_err());
        let started = Instant::now();
        assert!(c.get(&k).await.is_err());
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "cool-down short-circuits"
        );
        let r = ValkeyReadiness(std::sync::Arc::new(c));
        assert!(r.check().await.is_err());
        assert!(!r.critical());
    }
}
