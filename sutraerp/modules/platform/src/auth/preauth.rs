//! Single-use, short-lived pre-authentication records (OIDC authorization requests, pending MFA
//! challenges). Same rules as the session stores: the handle is 256 bits from the OS CSPRNG,
//! handed out once, and only `SHA-256(handle)` is stored; [`PreAuthStore::take`] consumes a
//! record atomically so a `state` or `challengeId` can never be replayed.
//!
//! * [`InMemoryPreAuthStore`] — no-database development and tests.
//! * [`PgPreAuthStore`] — `platform.pre_auth`.

use super::session::Clock;
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sutra_contracts::platform::auth::{PreAuthKind, PreAuthStore};
use sutra_kernel::error::KernelError;
use sutra_kernel::secret;
use sutra_persistence::map_db_error;

struct Record {
    kind: PreAuthKind,
    payload: serde_json::Value,
    expires_at: DateTime<Utc>,
}

pub struct InMemoryPreAuthStore {
    clock: Clock,
    records: Mutex<HashMap<[u8; 32], Record>>,
}

impl Default for InMemoryPreAuthStore {
    fn default() -> Self {
        Self::with_clock(Arc::new(Utc::now))
    }
}

impl InMemoryPreAuthStore {
    pub fn with_clock(clock: Clock) -> Self {
        Self {
            clock,
            records: Mutex::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl PreAuthStore for InMemoryPreAuthStore {
    async fn put(
        &self,
        kind: PreAuthKind,
        payload: serde_json::Value,
        ttl: Duration,
    ) -> Result<String, KernelError> {
        let now = (self.clock)();
        let handle = secret::random_token()?;
        let mut map = self
            .records
            .lock()
            .map_err(|_| KernelError::internal_msg("pre-auth store poisoned"))?;
        map.retain(|_, r| r.expires_at > now);
        map.insert(
            secret::sha256(handle.as_bytes()),
            Record {
                kind,
                payload,
                expires_at: now + ttl,
            },
        );
        Ok(handle)
    }

    async fn take(
        &self,
        kind: PreAuthKind,
        handle: &str,
    ) -> Result<Option<serde_json::Value>, KernelError> {
        let now = (self.clock)();
        let mut map = self
            .records
            .lock()
            .map_err(|_| KernelError::internal_msg("pre-auth store poisoned"))?;
        let key = secret::sha256(handle.as_bytes());
        // Only a record of the requested kind is consumed (an OIDC state cannot burn an MFA
        // challenge and vice versa).
        match map.get(&key) {
            Some(r) if r.kind == kind => {}
            _ => return Ok(None),
        }
        Ok(map
            .remove(&key)
            .filter(|r| r.expires_at > now)
            .map(|r| r.payload))
    }
}

#[derive(Clone)]
pub struct PgPreAuthStore {
    pool: PgPool,
}

impl PgPreAuthStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Deletes expired records. Returns the number removed (scheduled by the sweeper slice).
    pub async fn purge_expired(&self) -> Result<u64, KernelError> {
        Ok(
            sqlx::query("DELETE FROM platform.pre_auth WHERE expires_at <= now()")
                .execute(&self.pool)
                .await
                .map_err(map_db_error)?
                .rows_affected(),
        )
    }
}

#[async_trait::async_trait]
impl PreAuthStore for PgPreAuthStore {
    async fn put(
        &self,
        kind: PreAuthKind,
        payload: serde_json::Value,
        ttl: Duration,
    ) -> Result<String, KernelError> {
        let handle = secret::random_token()?;
        sqlx::query(
            "INSERT INTO platform.pre_auth (handle_hash, kind, payload, expires_at) \
             VALUES ($1, $2, $3, now() + make_interval(secs => $4))",
        )
        .bind(secret::sha256(handle.as_bytes()).as_slice())
        .bind(kind.as_str())
        .bind(&payload)
        .bind(ttl.num_seconds() as f64)
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(handle)
    }

    async fn take(
        &self,
        kind: PreAuthKind,
        handle: &str,
    ) -> Result<Option<serde_json::Value>, KernelError> {
        // DELETE … RETURNING is atomic: of two concurrent callbacks only one gets the row.
        let row: Option<(serde_json::Value, bool)> = sqlx::query_as(
            "DELETE FROM platform.pre_auth WHERE handle_hash = $1 AND kind = $2 \
             RETURNING payload, expires_at > now()",
        )
        .bind(secret::sha256(handle.as_bytes()).as_slice())
        .bind(kind.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        Ok(row.and_then(|(payload, live)| live.then_some(payload)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    #[tokio::test]
    async fn single_use_kind_bound_and_expiring() {
        let origin = Utc::now();
        let offset = Arc::new(AtomicI64::new(0));
        let o = offset.clone();
        let s = InMemoryPreAuthStore::with_clock(Arc::new(move || {
            origin + Duration::seconds(o.load(Ordering::SeqCst))
        }));
        let h = s
            .put(
                PreAuthKind::OidcAuthorization,
                serde_json::json!({"n": 1}),
                Duration::seconds(60),
            )
            .await
            .unwrap();
        assert_eq!(h.len(), 64);
        assert!(s
            .take(PreAuthKind::MfaChallenge, &h)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            s.take(PreAuthKind::OidcAuthorization, &h).await.unwrap(),
            Some(serde_json::json!({"n": 1})),
            "a wrong-kind lookup does not burn the record"
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
                Duration::seconds(60),
            )
            .await
            .unwrap();
        offset.store(61, Ordering::SeqCst);
        assert!(
            s.take(PreAuthKind::MfaChallenge, &h)
                .await
                .unwrap()
                .is_none(),
            "expired"
        );
        assert!(s
            .take(PreAuthKind::MfaChallenge, "nope")
            .await
            .unwrap()
            .is_none());
    }
}
