//! Failed-attempt throttling for the credential and second-factor checks SutraERP performs
//! itself (Part 1 S3, SEC-9D-005 / SEC-P1-AUTH-003).
//!
//! Every failed password sign-in (keyed by the client address **and** the normalised login id —
//! audit H-3: a stranger spamming failures locks only their own address out of that account, never
//! the account everywhere; an administrator can clear an account's lockouts with
//! `POST /api/v1/auth/lockouts/unlock`) and every failed TOTP check
//! (pending-MFA verification, step-up, enrolment confirmation — keyed by the subject) counts
//! against a fixed window. Once `max_failures` failures fall inside the window the key is locked
//! for `lockout`: further attempts are refused with `429 too-many-attempts` and a `Retry-After`
//! header **before** the credential is looked at, so a correct guess during the lockout is not
//! accepted either. A success clears the key.
//!
//! The counter is process-local (per API replica): with `n` replicas an attacker gets at most
//! `n × max_failures` guesses per window. Password sign-in in production is Keycloak's, whose
//! realm brute-force detection is the primary control there (`deploy/keycloak`); this
//! throttle bounds the dev authenticator and SutraERP's own TOTP checks, and the ingress adds a
//! per-client request rate limit on `/api/v1/auth/*` (Helm `ingress.authRateLimit`).

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Duration, Utc};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sutra_web::Problem;

/// Clock seam for tests.
pub type ThrottleClock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// Above this many tracked keys, stale ones are pruned on the next failure.
const PRUNE_ABOVE: usize = 10_000;

#[derive(Debug, Clone, Copy)]
struct Entry {
    failures: u32,
    window_start: DateTime<Utc>,
    locked_until: Option<DateTime<Utc>>,
}

/// Fixed-window failure counter with lockout (see the module docs).
pub struct AuthThrottle {
    max_failures: u32,
    window: Duration,
    lockout: Duration,
    entries: Mutex<HashMap<String, Entry>>,
    clock: ThrottleClock,
}

impl std::fmt::Debug for AuthThrottle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthThrottle")
            .field("max_failures", &self.max_failures)
            .field("window", &self.window)
            .field("lockout", &self.lockout)
            .finish_non_exhaustive()
    }
}

impl AuthThrottle {
    /// `max_failures = 0` disables the throttle.
    pub fn new(max_failures: u32, window_secs: i64, lockout_secs: i64) -> Self {
        Self::with_clock(max_failures, window_secs, lockout_secs, Arc::new(Utc::now))
    }

    pub fn with_clock(
        max_failures: u32,
        window_secs: i64,
        lockout_secs: i64,
        clock: ThrottleClock,
    ) -> Self {
        Self {
            max_failures,
            window: Duration::seconds(window_secs.max(1)),
            lockout: Duration::seconds(lockout_secs.max(1)),
            entries: Mutex::new(HashMap::new()),
            clock,
        }
    }

    /// Defaults: 10 failures per 15 minutes lock the key for 15 minutes.
    pub fn standard() -> Self {
        Self::new(10, 900, 900)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        // A poisoned map only holds counters: keep using it.
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// `Err(retry_after_secs)` while `key` is locked out.
    pub fn check(&self, key: &str) -> Result<(), i64> {
        if self.max_failures == 0 {
            return Ok(());
        }
        let now = (self.clock)();
        let mut map = self.lock();
        if let Some(e) = map.get(key).copied() {
            match e.locked_until {
                Some(until) if until > now => return Err((until - now).num_seconds().max(1)),
                Some(_) => {
                    map.remove(key);
                }
                None => {}
            }
        }
        Ok(())
    }

    /// Records a failure; `Some(retry_after_secs)` when this failure locked the key.
    pub fn failure(&self, key: &str) -> Option<i64> {
        if self.max_failures == 0 {
            return None;
        }
        let now = (self.clock)();
        let mut map = self.lock();
        if map.len() > PRUNE_ABOVE {
            let (window, lockout) = (self.window, self.lockout);
            map.retain(|_, e| match e.locked_until {
                Some(until) => until > now,
                None => e.window_start + window.max(lockout) > now,
            });
        }
        let e = map.entry(key.to_owned()).or_insert(Entry {
            failures: 0,
            window_start: now,
            locked_until: None,
        });
        if e.window_start + self.window <= now || e.locked_until.is_some_and(|u| u <= now) {
            *e = Entry {
                failures: 0,
                window_start: now,
                locked_until: None,
            };
        }
        e.failures = e.failures.saturating_add(1);
        if e.failures >= self.max_failures {
            let until = now + self.lockout;
            e.locked_until = Some(until);
            return Some(self.lockout.num_seconds().max(1));
        }
        None
    }

    /// Clears `key` after a successful check.
    pub fn success(&self, key: &str) {
        self.lock().remove(key);
    }

    /// Throttle key of a password sign-in attempt from `client` (audit H-3: IP + account).
    pub fn login_key(client: &str, login_id: &str) -> String {
        format!(
            "login:{}|{}",
            Self::normalise_login(login_id),
            client.trim()
        )
    }

    fn normalise_login(login_id: &str) -> String {
        login_id.trim().to_lowercase()
    }

    /// Clears every sign-in lockout / failure count of `login_id`, from any client address
    /// (administrative unlock, audit H-3). Returns how many client keys were cleared.
    pub fn unlock_login(&self, login_id: &str) -> usize {
        let prefix = format!("login:{}|", Self::normalise_login(login_id));
        let mut map = self.lock();
        let before = map.len();
        map.retain(|k, _| !k.starts_with(&prefix));
        before - map.len()
    }

    /// Throttle key of a second-factor check for `subject`.
    pub fn factor_key(subject: &str) -> String {
        format!("factor:{subject}")
    }
}

/// `429 too-many-attempts` with `Retry-After`.
pub fn too_many_attempts(retry_after_secs: i64) -> Response {
    let mut res = Problem::new(
        StatusCode::TOO_MANY_REQUESTS,
        "too-many-attempts",
        "Too many failed attempts",
    )
    .detail("too many failed attempts for this account from this client; wait before trying again")
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&retry_after_secs.max(1).to_string()) {
        res.headers_mut().insert(header::RETRY_AFTER, v);
    }
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    fn clocked(max: u32, window: i64, lockout: i64) -> (AuthThrottle, Arc<AtomicI64>) {
        let t = Arc::new(AtomicI64::new(1_900_000_000));
        let c = t.clone();
        (
            AuthThrottle::with_clock(
                max,
                window,
                lockout,
                Arc::new(move || DateTime::from_timestamp(c.load(Ordering::SeqCst), 0).unwrap()),
            ),
            t,
        )
    }

    #[test]
    fn locks_after_max_failures_and_unlocks_after_lockout() {
        let (th, clock) = clocked(3, 60, 120);
        let k = AuthThrottle::login_key("192.0.2.1", " Admin@Sutra.dev ");
        assert_eq!(k, "login:admin@sutra.dev|192.0.2.1");
        assert!(th.check(&k).is_ok());
        assert_eq!(th.failure(&k), None);
        assert_eq!(th.failure(&k), None);
        assert_eq!(th.failure(&k), Some(120), "third failure locks");
        assert_eq!(th.check(&k), Err(120));
        clock.fetch_add(100, Ordering::SeqCst);
        assert_eq!(th.check(&k), Err(20));
        clock.fetch_add(21, Ordering::SeqCst);
        assert!(th.check(&k).is_ok(), "lockout over");
        assert_eq!(th.failure(&k), None, "counting starts again");
    }

    #[test]
    fn window_resets_and_success_clears() {
        let (th, clock) = clocked(3, 60, 120);
        let k = AuthThrottle::factor_key("s1");
        th.failure(&k);
        th.failure(&k);
        clock.fetch_add(61, Ordering::SeqCst);
        assert_eq!(th.failure(&k), None, "old failures expired with the window");
        th.failure(&k);
        th.success(&k);
        th.failure(&k);
        th.failure(&k);
        assert!(th.check(&k).is_ok(), "success cleared the earlier failures");
        // Keys are independent.
        assert!(th.check(&AuthThrottle::factor_key("s2")).is_ok());
    }

    #[test]
    fn disabled_never_locks() {
        let (th, _) = clocked(0, 60, 120);
        for _ in 0..100 {
            assert_eq!(th.failure("k"), None);
        }
        assert!(th.check("k").is_ok());
    }

    #[test]
    fn response_is_429_with_retry_after() {
        let r = too_many_attempts(42);
        assert_eq!(r.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(r.headers()[header::RETRY_AFTER], "42");
    }

    /// Audit H-3: failures from one address lock only that address out of the account; the
    /// owner signing in from elsewhere is unaffected, and an administrative unlock clears all.
    #[test]
    fn lockout_is_scoped_to_client_and_account_and_can_be_unlocked() {
        let (th, _) = clocked(3, 60, 300);
        let attacker = AuthThrottle::login_key("203.0.113.66", "admin@sutra.dev");
        let owner = AuthThrottle::login_key("198.51.100.7", "Admin@Sutra.dev");
        for _ in 0..3 {
            th.failure(&attacker);
        }
        assert!(th.check(&attacker).is_err(), "attacker address locked");
        assert!(th.check(&owner).is_ok(), "owner's address not locked");
        th.failure(&owner);
        let other = AuthThrottle::login_key("203.0.113.66", "dean@sutra.dev");
        th.failure(&other);
        assert_eq!(th.unlock_login(" ADMIN@sutra.dev "), 2);
        assert!(th.check(&attacker).is_ok(), "unlocked");
        assert_eq!(th.unlock_login("admin@sutra.dev"), 0);
        assert_eq!(
            th.unlock_login("dean@sutra.dev"),
            1,
            "other accounts untouched"
        );
    }
}
