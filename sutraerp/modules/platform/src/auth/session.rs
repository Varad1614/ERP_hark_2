//! Server-side session stores (D01, ADR-L5-0038 §3.4).
//!
//! * [`InMemorySessionStore`] — no-database development and tests.
//! * [`super::session_pg::PgSessionStore`] — durable `platform.session` table.
//!
//! Both follow the same rules:
//!
//! * the opaque token is 256 bits from the OS CSPRNG and is returned to the caller **once**;
//!   only `SHA-256(token)` is stored and used as the lookup key;
//! * each session has its own random CSRF secret; the synchronizer token handed to the browser
//!   is `HMAC-SHA256(csrf_secret, "sutraerp.csrf.v1|" ‖ token)`, so it is bound to the session,
//!   can be re-derived on `GET /api/v1/auth/session`, and cannot be computed from a database
//!   dump alone (the token is not in the database);
//! * a session dies at the earlier of its absolute expiry and its idle expiry (sliding window
//!   refreshed on successful lookups — the `last_seen_at` write is throttled to once per
//!   [`SessionPolicy::touch_interval`]); revocation is immediate;
//! * a session records its assurance (`mfa_level`, `amr`) and, for federated logins, the IdP
//!   session id (`idp_sid`).

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sutra_contracts::platform::auth::{
    IssuedSession, MfaLevel, NewSession, Principal, Session, SessionPolicy, SessionStore,
};
use sutra_kernel::auth::{AuthContext, SessionResolver};
use sutra_kernel::error::KernelError;
use sutra_kernel::secret;
use uuid::Uuid;

const CSRF_CONTEXT: &[u8] = b"sutraerp.csrf.v1|";

/// 256-bit opaque session token, hex encoded.
pub fn new_session_token() -> Result<String, KernelError> {
    secret::random_token()
}

/// Storage key of a session token: `SHA-256(token)`.
pub fn token_hash(token: &str) -> [u8; 32] {
    secret::sha256(token.as_bytes())
}

/// Session-bound synchronizer token (hex).
pub fn derive_csrf_token(csrf_secret: &[u8], token: &str) -> Result<String, KernelError> {
    let mut msg = Vec::with_capacity(CSRF_CONTEXT.len() + token.len());
    msg.extend_from_slice(CSRF_CONTEXT);
    msg.extend_from_slice(token.as_bytes());
    Ok(secret::hex(&secret::hmac_sha256(csrf_secret, &msg)?))
}

/// Source of "now" (injectable for expiry tests).
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

fn system_clock() -> Clock {
    Arc::new(Utc::now)
}

struct Entry {
    id: Uuid,
    principal: Principal,
    csrf_secret: [u8; 32],
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    mfa_level: MfaLevel,
    amr: Vec<String>,
    idp_sid: Option<String>,
}

pub struct InMemorySessionStore {
    policy: SessionPolicy,
    clock: Clock,
    sessions: Mutex<HashMap<[u8; 32], Entry>>,
}

impl InMemorySessionStore {
    pub fn new(policy: SessionPolicy) -> Self {
        Self::with_clock(policy, system_clock())
    }

    pub fn with_clock(policy: SessionPolicy, clock: Clock) -> Self {
        Self {
            policy,
            clock,
            sessions: Mutex::new(HashMap::new()),
        }
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<[u8; 32], Entry>>, KernelError> {
        self.sessions
            .lock()
            .map_err(|_| KernelError::internal_msg("session store poisoned"))
    }

    fn view(&self, e: &Entry, token: &str) -> Result<Session, KernelError> {
        Ok(Session {
            id: e.id,
            principal: e.principal.clone(),
            created_at: e.created_at,
            last_seen_at: e.last_seen_at,
            expires_at: e.expires_at,
            idle_expires_at: (e.last_seen_at + self.policy.idle_timeout).min(e.expires_at),
            csrf_token: derive_csrf_token(&e.csrf_secret, token)?,
            mfa_level: e.mfa_level,
            amr: e.amr.clone(),
            idp_sid: e.idp_sid.clone(),
        })
    }
}

#[async_trait::async_trait]
impl SessionStore for InMemorySessionStore {
    async fn create(&self, new: NewSession) -> Result<IssuedSession, KernelError> {
        let now = (self.clock)();
        let token = new_session_token()?;
        let mut expires_at = now + self.policy.absolute_timeout;
        if let Some(cap) = new.not_after {
            expires_at = expires_at.min(cap);
        }
        if expires_at <= now {
            return Err(KernelError::Unauthenticated);
        }
        let entry = Entry {
            id: sutra_kernel::id::new_v7(),
            principal: new.principal,
            csrf_secret: secret::random_bytes()?,
            created_at: now,
            last_seen_at: now,
            expires_at,
            mfa_level: new.mfa_level,
            amr: new.amr,
            idp_sid: new.idp_sid,
        };
        let session = self.view(&entry, &token)?;
        let mut map = self.lock()?;
        let policy = self.policy;
        map.retain(|_, e| policy.is_live(now, e.last_seen_at, e.expires_at));
        map.insert(token_hash(&token), entry);
        Ok(IssuedSession { token, session })
    }

    async fn get(&self, token: &str) -> Result<Option<Session>, KernelError> {
        let now = (self.clock)();
        let key = token_hash(token);
        let mut map = self.lock()?;
        let live = match map.get(&key) {
            None => return Ok(None),
            Some(e) => self.policy.is_live(now, e.last_seen_at, e.expires_at),
        };
        if !live {
            map.remove(&key);
            return Ok(None);
        }
        match map.get_mut(&key) {
            Some(e) => {
                if self.policy.should_touch(now, e.last_seen_at) {
                    e.last_seen_at = now;
                }
                Ok(Some(self.view(e, token)?))
            }
            None => Ok(None),
        }
    }

    async fn revoke(&self, token: &str) -> Result<(), KernelError> {
        self.lock()?.remove(&token_hash(token));
        Ok(())
    }
}

/// Adapts any [`SessionStore`] to the kernel's [`SessionResolver`] port used by the HTTP layer,
/// stamping the configured step-up policy onto every [`AuthContext`].
pub struct StoreSessionResolver {
    pub store: Arc<dyn SessionStore>,
    pub step_up_enforced: bool,
}

#[async_trait::async_trait]
impl SessionResolver for StoreSessionResolver {
    async fn resolve(&self, token: &str) -> Result<Option<AuthContext>, KernelError> {
        Ok(self.store.get(token).await?.map(|s| {
            let mut ctx = s.auth_context();
            ctx.step_up_enforced = self.step_up_enforced;
            ctx
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use std::sync::atomic::{AtomicI64, Ordering};
    use sutra_contracts::platform::auth::Role;
    use sutra_kernel::scope::{ExecutionScope, TenantId};

    fn p() -> NewSession {
        NewSession::password(principal())
    }

    fn principal() -> Principal {
        Principal {
            subject: "s".into(),
            display_name: "d".into(),
            login_id: "l".into(),
            roles: vec![Role::Student],
            scope: ExecutionScope::tenant(TenantId::new()),
        }
    }

    fn policy(idle: i64, absolute: i64) -> SessionPolicy {
        SessionPolicy::new(idle, absolute).unwrap()
    }

    /// Store with a manually advanced clock (seconds offset from a fixed origin).
    fn clocked(idle: i64, absolute: i64) -> (InMemorySessionStore, Arc<AtomicI64>) {
        let origin = Utc::now();
        let offset = Arc::new(AtomicI64::new(0));
        let o = offset.clone();
        let store = InMemorySessionStore::with_clock(
            policy(idle, absolute),
            Arc::new(move || origin + Duration::seconds(o.load(Ordering::SeqCst))),
        );
        (store, offset)
    }

    #[test]
    fn tokens_are_256_bit_hex_and_unique() {
        let a = new_session_token().unwrap();
        let b = new_session_token().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert_ne!(token_hash(&a), token_hash(&b));
    }

    #[test]
    fn csrf_token_is_bound_to_secret_and_session_token() {
        let s1 = [1u8; 32];
        let s2 = [2u8; 32];
        let a = derive_csrf_token(&s1, "tok-a").unwrap();
        assert_eq!(a, derive_csrf_token(&s1, "tok-a").unwrap(), "deterministic");
        assert_ne!(a, derive_csrf_token(&s1, "tok-b").unwrap());
        assert_ne!(a, derive_csrf_token(&s2, "tok-a").unwrap());
        assert_eq!(a.len(), 64);
    }

    #[tokio::test]
    async fn create_get_revoke() {
        let s = InMemorySessionStore::new(policy(60, 3600));
        let issued = s.create(p()).await.unwrap();
        let got = s.get(&issued.token).await.unwrap().unwrap();
        assert_eq!(got.id, issued.session.id);
        assert_eq!(
            got.csrf_token, issued.session.csrf_token,
            "stable per session"
        );
        assert!(s.get("not-a-token").await.unwrap().is_none());
        s.revoke(&issued.token).await.unwrap();
        assert!(s.get(&issued.token).await.unwrap().is_none());
        s.revoke(&issued.token).await.unwrap(); // idempotent
    }

    #[tokio::test]
    async fn sessions_never_share_csrf_tokens() {
        let s = InMemorySessionStore::new(policy(60, 3600));
        let a = s.create(p()).await.unwrap();
        let b = s.create(p()).await.unwrap();
        assert_ne!(a.session.csrf_token, b.session.csrf_token);
    }

    #[tokio::test]
    async fn idle_expiry_slides_with_activity() {
        let (s, clock) = clocked(60, 3600);
        let issued = s.create(p()).await.unwrap();
        clock.store(50, Ordering::SeqCst);
        let got = s.get(&issued.token).await.unwrap().unwrap();
        assert_eq!(
            got.idle_expires_at,
            got.last_seen_at + Duration::seconds(60)
        );
        clock.store(100, Ordering::SeqCst); // 50s after last activity: still alive
        assert!(s.get(&issued.token).await.unwrap().is_some());
        clock.store(161, Ordering::SeqCst); // 61s idle
        assert!(s.get(&issued.token).await.unwrap().is_none());
        clock.store(162, Ordering::SeqCst);
        assert!(
            s.get(&issued.token).await.unwrap().is_none(),
            "expired sessions stay dead"
        );
    }

    #[tokio::test]
    async fn last_seen_writes_are_throttled() {
        let (s, clock) = clocked(600, 3600); // touch interval = 60 s
        let issued = s.create(p()).await.unwrap();
        clock.store(30, Ordering::SeqCst);
        let got = s.get(&issued.token).await.unwrap().unwrap();
        assert_eq!(
            got.last_seen_at, issued.session.last_seen_at,
            "30 s: not written"
        );
        clock.store(61, Ordering::SeqCst);
        let got = s.get(&issued.token).await.unwrap().unwrap();
        assert_eq!(
            got.last_seen_at,
            issued.session.last_seen_at + Duration::seconds(61),
            "61 s: written"
        );
    }

    #[tokio::test]
    async fn mfa_metadata_and_not_after_cap() {
        let s = InMemorySessionStore::new(policy(60, 3600));
        let mut n = NewSession::password_and_totp(principal());
        n.idp_sid = Some("kc-sid".into());
        let cap = Utc::now() + Duration::seconds(100);
        n.not_after = Some(cap);
        let issued = s.create(n).await.unwrap();
        assert_eq!(issued.session.mfa_level, MfaLevel::Mfa);
        assert!(issued.session.amr.contains(&"otp".to_owned()));
        assert_eq!(issued.session.idp_sid.as_deref(), Some("kc-sid"));
        assert_eq!(issued.session.expires_at, cap);
        let mut past = NewSession::password(principal());
        past.not_after = Some(Utc::now() - Duration::seconds(1));
        assert!(
            s.create(past).await.is_err(),
            "cannot mint an already-expired session"
        );
    }

    #[tokio::test]
    async fn absolute_expiry_despite_activity() {
        let (s, clock) = clocked(60, 120);
        let issued = s.create(p()).await.unwrap();
        for t in [50, 100, 119] {
            clock.store(t, Ordering::SeqCst);
            assert!(s.get(&issued.token).await.unwrap().is_some(), "t={t}");
        }
        clock.store(120, Ordering::SeqCst);
        assert!(s.get(&issued.token).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn idle_expiry_is_capped_by_absolute_expiry() {
        let (s, clock) = clocked(60, 90);
        let issued = s.create(p()).await.unwrap();
        clock.store(50, Ordering::SeqCst);
        assert!(s.get(&issued.token).await.unwrap().is_some());
        clock.store(80, Ordering::SeqCst);
        let got = s.get(&issued.token).await.unwrap().unwrap();
        assert_eq!(got.idle_expires_at, got.expires_at);
    }

    #[tokio::test]
    async fn resolver_maps_to_auth_context() {
        let store: Arc<dyn SessionStore> = Arc::new(InMemorySessionStore::new(policy(60, 3600)));
        let issued = store.create(p()).await.unwrap();
        let r = StoreSessionResolver {
            store,
            step_up_enforced: true,
        };
        let ctx = r.resolve(&issued.token).await.unwrap().unwrap();
        assert_eq!(ctx.session_id, issued.session.id);
        assert!(ctx.step_up_enforced);
        assert_eq!(ctx.mfa_level, MfaLevel::None);
        assert_eq!(ctx.amr, vec!["pwd".to_owned()]);
        assert_eq!(ctx.csrf_token, issued.session.csrf_token);
        assert!(r.resolve("nope").await.unwrap().is_none());
    }
}
