//! Derived, advisory cache port (ADR-L5-0037).
//!
//! * Cached values are **derived and non-authoritative**: the owner's database stays the source
//!   of truth. The cache is never a session store, an authorization input, a lock manager or a
//!   queue (ADR-L5-0037 §2.8). Callers authorize and resolve scope *before* touching the cache.
//! * Keys are bound to environment, canonical owner and tenant (+ institution) scope
//!   ([`CacheKey`]), so entries of one tenant can never be served to another and a key never
//!   implies authority (ADR-L5-0037 §2.5, §3).
//! * A cache failure is a miss: [`AdvisoryCache`] logs it and carries on through the owner path
//!   (ADR-L5-0037 §2.4). Undecodable entries are dropped and treated as misses.
//!
//! Adapters: [`InMemoryCache`] (single process), [`NoCache`] (disabled) and the Valkey adapter in
//! `sutra-infra`.

use crate::scope::ExecutionScope;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A fully namespaced cache key: `sutra:<env>:<owner>:t:<tenant>[:i:<institution>]:<parts…>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CacheKey(String);

impl CacheKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for CacheKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Escapes a key segment so it can never introduce a separator or whitespace: `%`, `:`, `*`,
/// `?`, `[`, `]`, control characters and whitespace are percent-encoded.
fn segment(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if matches!(c, '%' | ':' | '*' | '?' | '[' | ']') || c.is_whitespace() || c.is_control() {
            let mut buf = [0u8; 4];
            for b in c.encode_utf8(&mut buf).bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        } else {
            out.push(c);
        }
    }
    if out.is_empty() {
        out.push('_');
    }
    out
}

/// Builds scope-bound keys for one environment namespace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheNamespace {
    env: String,
}

impl CacheNamespace {
    pub fn new(env: &str) -> Self {
        Self { env: segment(env) }
    }

    /// Key for an entry owned by `owner` within `scope`. The institution id is part of the key
    /// when the scope carries one.
    pub fn key(&self, owner: &str, scope: &ExecutionScope, parts: &[&str]) -> CacheKey {
        let mut k = format!(
            "sutra:{}:{}:t:{}",
            self.env,
            segment(owner),
            scope.tenant_id.as_uuid()
        );
        if let Some(i) = scope.institution_id {
            k.push_str(&format!(":i:{}", i.as_uuid()));
        }
        for p in parts {
            k.push(':');
            k.push_str(&segment(p));
        }
        CacheKey(k)
    }
}

/// Technical cache failure (never shown to clients).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("cache unavailable: {0}")]
pub struct CacheError(pub String);

/// Raw cache port. Implementations should bound every call with a short timeout.
#[async_trait::async_trait]
pub trait Cache: Send + Sync + 'static {
    async fn get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>, CacheError>;
    async fn set(&self, key: &CacheKey, value: &[u8], ttl: Duration) -> Result<(), CacheError>;
    async fn delete(&self, key: &CacheKey) -> Result<(), CacheError>;
    /// Adapter name for logs (`memory`, `valkey`, `off`).
    fn backend(&self) -> &'static str;
}

/// Disabled cache: every read misses, writes are dropped.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoCache;

#[async_trait::async_trait]
impl Cache for NoCache {
    async fn get(&self, _: &CacheKey) -> Result<Option<Vec<u8>>, CacheError> {
        Ok(None)
    }
    async fn set(&self, _: &CacheKey, _: &[u8], _: Duration) -> Result<(), CacheError> {
        Ok(())
    }
    async fn delete(&self, _: &CacheKey) -> Result<(), CacheError> {
        Ok(())
    }
    fn backend(&self) -> &'static str {
        "off"
    }
}

type Entries = HashMap<CacheKey, (Vec<u8>, Instant)>;

/// Process-local TTL cache with a size bound (oldest-expiry eviction). Not shared between
/// replicas, so invalidation only reaches this process.
#[derive(Clone)]
pub struct InMemoryCache {
    entries: Arc<Mutex<Entries>>,
    max_entries: usize,
}

impl Default for InMemoryCache {
    fn default() -> Self {
        Self::new(10_000)
    }
}

impl InMemoryCache {
    pub fn new(max_entries: usize) -> Self {
        Self {
            entries: Arc::new(Mutex::new(HashMap::new())),
            max_entries: max_entries.max(1),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[async_trait::async_trait]
impl Cache for InMemoryCache {
    async fn get(&self, key: &CacheKey) -> Result<Option<Vec<u8>>, CacheError> {
        let mut m = self.lock();
        match m.get(key) {
            Some((v, exp)) if *exp > Instant::now() => Ok(Some(v.clone())),
            Some(_) => {
                m.remove(key);
                Ok(None)
            }
            None => Ok(None),
        }
    }

    async fn set(&self, key: &CacheKey, value: &[u8], ttl: Duration) -> Result<(), CacheError> {
        let now = Instant::now();
        let mut m = self.lock();
        if m.len() >= self.max_entries && !m.contains_key(key) {
            m.retain(|_, (_, exp)| *exp > now);
            if m.len() >= self.max_entries {
                if let Some(oldest) = m
                    .iter()
                    .min_by_key(|(_, (_, e))| *e)
                    .map(|(k, _)| k.clone())
                {
                    m.remove(&oldest);
                }
            }
        }
        m.insert(key.clone(), (value.to_vec(), now + ttl));
        Ok(())
    }

    async fn delete(&self, key: &CacheKey) -> Result<(), CacheError> {
        self.lock().remove(key);
        Ok(())
    }

    fn backend(&self) -> &'static str {
        "memory"
    }
}

/// The handle modules use: namespaced keys, typed JSON helpers and fail-open behaviour. Every
/// method is infallible — failures are logged and degrade to a miss / no-op.
#[derive(Clone)]
pub struct AdvisoryCache {
    inner: Arc<dyn Cache>,
    namespace: CacheNamespace,
    default_ttl: Duration,
}

impl std::fmt::Debug for AdvisoryCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdvisoryCache")
            .field("backend", &self.inner.backend())
            .field("namespace", &self.namespace)
            .field("default_ttl", &self.default_ttl)
            .finish()
    }
}

impl AdvisoryCache {
    pub fn new(inner: Arc<dyn Cache>, namespace: CacheNamespace, default_ttl: Duration) -> Self {
        Self {
            inner,
            namespace,
            default_ttl,
        }
    }

    /// A disabled cache (every read misses).
    pub fn disabled() -> Self {
        Self::new(
            Arc::new(NoCache),
            CacheNamespace::new("off"),
            Duration::ZERO,
        )
    }

    pub fn backend(&self) -> &'static str {
        self.inner.backend()
    }

    pub fn default_ttl(&self) -> Duration {
        self.default_ttl
    }

    pub fn key(&self, owner: &str, scope: &ExecutionScope, parts: &[&str]) -> CacheKey {
        self.namespace.key(owner, scope, parts)
    }

    /// Typed read. Cache errors and undecodable entries are misses.
    pub async fn get_json<T: serde::de::DeserializeOwned>(&self, key: &CacheKey) -> Option<T> {
        match self.inner.get(key).await {
            Ok(Some(bytes)) => match serde_json::from_slice(&bytes) {
                Ok(v) => {
                    tracing::trace!(cache.key = %key, "cache hit");
                    Some(v)
                }
                Err(e) => {
                    tracing::warn!(cache.key = %key, error = %e, "undecodable cache entry dropped");
                    self.invalidate(key).await;
                    None
                }
            },
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(cache.backend = self.backend(), error = %e, "cache read failed; treating as a miss");
                None
            }
        }
    }

    /// Typed write with the default TTL.
    pub async fn put_json<T: serde::Serialize>(&self, key: &CacheKey, value: &T) {
        self.put_json_ttl(key, value, self.default_ttl).await;
    }

    /// Typed write. A zero TTL skips the write.
    pub async fn put_json_ttl<T: serde::Serialize>(
        &self,
        key: &CacheKey,
        value: &T,
        ttl: Duration,
    ) {
        if ttl.is_zero() {
            return;
        }
        let bytes = match serde_json::to_vec(value) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(cache.key = %key, error = %e, "cache value not serializable");
                return;
            }
        };
        if let Err(e) = self.inner.set(key, &bytes, ttl).await {
            tracing::warn!(cache.backend = self.backend(), error = %e, "cache write failed; ignored");
        }
    }

    /// Owner-driven invalidation.
    pub async fn invalidate(&self, key: &CacheKey) {
        if let Err(e) = self.inner.delete(key).await {
            tracing::warn!(cache.backend = self.backend(), error = %e, "cache invalidation failed; entry expires by TTL");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{InstitutionId, TenantId};

    #[test]
    fn keys_are_bound_to_env_owner_and_scope() {
        let ns = CacheNamespace::new("production");
        let t = TenantId::new();
        let i = InstitutionId::new();
        let k = ns.key(
            "institution",
            &ExecutionScope::tenant(t),
            &["institution", "42"],
        );
        assert_eq!(
            k.as_str(),
            format!(
                "sutra:production:institution:t:{}:institution:42",
                t.as_uuid()
            )
        );
        let ki = ns.key("institution", &ExecutionScope::institution(t, i), &["x"]);
        assert!(ki.as_str().contains(&format!(":i:{}:", i.as_uuid())));
        let other = ns.key(
            "institution",
            &ExecutionScope::tenant(TenantId::new()),
            &["institution", "42"],
        );
        assert_ne!(k, other, "another tenant never shares a key");
        assert_ne!(
            k,
            CacheNamespace::new("staging").key(
                "institution",
                &ExecutionScope::tenant(t),
                &["institution", "42"]
            )
        );
    }

    #[test]
    fn segments_cannot_inject_separators() {
        let ns = CacheNamespace::new("dev:x");
        let t = TenantId::new();
        let k = ns.key(
            "a:b",
            &ExecutionScope::tenant(t),
            &["x:y", "sp ace", "*", ""],
        );
        assert_eq!(
            k.as_str(),
            format!("sutra:dev%3Ax:a%3Ab:t:{}:x%3Ay:sp%20ace:%2A:_", t.as_uuid())
        );
    }

    struct Broken;
    #[async_trait::async_trait]
    impl Cache for Broken {
        async fn get(&self, _: &CacheKey) -> Result<Option<Vec<u8>>, CacheError> {
            Err(CacheError("connection refused".into()))
        }
        async fn set(&self, _: &CacheKey, _: &[u8], _: Duration) -> Result<(), CacheError> {
            Err(CacheError("connection refused".into()))
        }
        async fn delete(&self, _: &CacheKey) -> Result<(), CacheError> {
            Err(CacheError("connection refused".into()))
        }
        fn backend(&self) -> &'static str {
            "broken"
        }
    }

    #[tokio::test]
    async fn failures_degrade_to_a_miss() {
        let c = AdvisoryCache::new(
            Arc::new(Broken),
            CacheNamespace::new("t"),
            Duration::from_secs(5),
        );
        let k = c.key("o", &ExecutionScope::tenant(TenantId::new()), &["k"]);
        c.put_json(&k, &1u32).await;
        assert_eq!(c.get_json::<u32>(&k).await, None);
        c.invalidate(&k).await;
    }

    #[tokio::test]
    async fn memory_cache_roundtrip_ttl_and_bad_entries() {
        let mem = InMemoryCache::new(2);
        let c = AdvisoryCache::new(
            Arc::new(mem.clone()),
            CacheNamespace::new("t"),
            Duration::from_secs(60),
        );
        let s = ExecutionScope::tenant(TenantId::new());
        let k = c.key("o", &s, &["a"]);
        c.put_json(&k, &vec![1, 2, 3]).await;
        assert_eq!(c.get_json::<Vec<i32>>(&k).await, Some(vec![1, 2, 3]));
        // Wrong type → dropped and treated as a miss.
        assert_eq!(c.get_json::<String>(&k).await, None);
        assert_eq!(c.get_json::<Vec<i32>>(&k).await, None);
        // Expiry.
        c.put_json_ttl(&k, &1, Duration::from_millis(1)).await;
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(c.get_json::<i32>(&k).await, None);
        // Size bound.
        for n in ["x", "y", "z"] {
            c.put_json(&c.key("o", &s, &[n]), &n).await;
        }
        assert!(mem.len() <= 2);
        // Invalidation + disabled cache.
        let k2 = c.key("o", &s, &["z"]);
        c.invalidate(&k2).await;
        assert_eq!(c.get_json::<String>(&k2).await, None);
        let off = AdvisoryCache::disabled();
        off.put_json(&k2, &1).await;
        assert_eq!(off.get_json::<i32>(&k2).await, None);
        assert_eq!(off.backend(), "off");
    }
}
