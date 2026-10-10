//! Authentication boundary contracts (D01 — ADR-L5-0024, ADR-L5-0038).
//!
//! Keycloak 26.x is the credential authority. SutraERP exposes its own
//! `POST /api/v1/auth/login`, delegates credential verification to a [`CredentialAuthenticator`]
//! (Keycloak adapter in Batch 1; dev stub in Batch 0) and establishes an opaque server-side session
//! carried in a `Secure; HttpOnly; SameSite` cookie. No password hash is ever stored by SutraERP and
//! the OAuth Resource-Owner-Password grant is **not** used.

use chrono::{DateTime, Duration, Utc};
use std::fmt;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use uuid::Uuid;

/// Application role (defined in the kernel so the HTTP layer can evaluate the D02 policy without
/// depending on contracts). Authorization truth is `sutra_kernel::auth::role_permissions`.
pub use sutra_kernel::auth::Role;

/// Transient credentials. `Debug` is redacted so the password can never reach a log line.
#[derive(Clone, serde::Deserialize)]
pub struct Credentials {
    pub login_id: String,
    pub password: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credentials")
            .field("login_id", &self.login_id)
            .field("password", &"<redacted>")
            .finish()
    }
}

/// The authenticated principal (technical identity bound to a SutraERP canonical identity).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Principal {
    /// Stable subject reference (Keycloak `sub` in production).
    pub subject: String,
    pub display_name: String,
    pub login_id: String,
    pub roles: Vec<Role>,
    pub scope: ExecutionScope,
}

/// MFA method family (WebAuthn preferred for privileged users, TOTP fallback).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub enum MfaMethod {
    Webauthn,
    Totp,
}

/// Result of a primary credential check.
#[derive(Clone, Debug)]
pub enum AuthOutcome {
    Authenticated(Principal),
    /// Primary factor accepted; a second factor is required before a session is issued.
    MfaRequired {
        challenge_id: String,
        methods: Vec<MfaMethod>,
        principal: Principal,
    },
}

/// Credential verification port. Implementations: `DevAuthenticator` (Batch 0),
/// `KeycloakAuthenticator` (Batch 1).
#[async_trait::async_trait]
pub trait CredentialAuthenticator: Send + Sync + 'static {
    /// Must return [`KernelError::Unauthenticated`] for any failure (no account enumeration).
    async fn authenticate(&self, credentials: Credentials) -> Result<AuthOutcome, KernelError>;
    /// Completes an MFA challenge.
    async fn verify_mfa(&self, challenge_id: &str, code: &str) -> Result<Principal, KernelError>;
}

/// MFA policy hook: decides whether a principal must complete MFA.
pub trait MfaPolicy: Send + Sync + 'static {
    fn requires_mfa(&self, principal: &Principal) -> bool;
}

impl From<&AuthContext> for Principal {
    fn from(c: &AuthContext) -> Self {
        Principal {
            subject: c.subject.clone(),
            display_name: c.display_name.clone(),
            login_id: c.login_id.clone(),
            roles: c.roles.clone(),
            scope: c.scope,
        }
    }
}

/// Idle and absolute session lifetimes (ADR-L5-0038 §3.4). A session dies at whichever comes
/// first: `created_at + absolute_timeout`, or `last_seen_at + idle_timeout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionPolicy {
    pub idle_timeout: Duration,
    pub absolute_timeout: Duration,
}

impl SessionPolicy {
    /// Validates `0 < idle <= absolute`.
    pub fn new(idle_secs: i64, absolute_secs: i64) -> Result<Self, KernelError> {
        if idle_secs <= 0 || absolute_secs <= 0 || idle_secs > absolute_secs {
            return Err(KernelError::BadRequest(
                "session policy requires 0 < idle timeout <= absolute timeout".into(),
            ));
        }
        Ok(Self {
            idle_timeout: Duration::seconds(idle_secs),
            absolute_timeout: Duration::seconds(absolute_secs),
        })
    }

    /// `true` when a session with these timestamps is still alive at `now`.
    pub fn is_live(
        &self,
        now: DateTime<Utc>,
        last_seen_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> bool {
        now < expires_at && now < last_seen_at + self.idle_timeout
    }
}

/// Server-side session record. The cookie only carries the opaque token; the store keeps a
/// SHA-256 digest of it, never the token itself.
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    /// Technical session id (UUIDv7) — safe for logs and audit; not a credential.
    pub id: Uuid,
    pub principal: Principal,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    /// Absolute expiry.
    pub expires_at: DateTime<Utc>,
    /// Expiry if no further activity is seen.
    pub idle_expires_at: DateTime<Utc>,
    /// Synchronizer CSRF token bound to this session (derived; never stored in clear).
    pub csrf_token: String,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("principal", &self.principal)
            .field("created_at", &self.created_at)
            .field("last_seen_at", &self.last_seen_at)
            .field("expires_at", &self.expires_at)
            .field("idle_expires_at", &self.idle_expires_at)
            .field("csrf_token", &"<redacted>")
            .finish()
    }
}

impl Session {
    /// The HTTP-layer view of this session.
    pub fn auth_context(&self) -> AuthContext {
        AuthContext {
            session_id: self.id,
            subject: self.principal.subject.clone(),
            login_id: self.principal.login_id.clone(),
            display_name: self.principal.display_name.clone(),
            roles: self.principal.roles.clone(),
            scope: self.principal.scope,
            csrf_token: self.csrf_token.clone(),
            expires_at: self.expires_at,
            idle_expires_at: self.idle_expires_at,
        }
    }
}

/// A freshly created session together with its opaque token. The token is handed to the client
/// exactly once (in the session cookie) and is never persisted or logged.
#[derive(Clone)]
pub struct IssuedSession {
    pub token: String,
    pub session: Session,
}

impl fmt::Debug for IssuedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IssuedSession")
            .field("token", &"<redacted>")
            .field("session", &self.session)
            .finish()
    }
}

/// Session storage port (D01-owned): in-memory (dev/tests) or PostgreSQL (`platform.session`).
/// Valkey may cache but is never the session authority (ADR-L5-0037 §2.8).
#[async_trait::async_trait]
pub trait SessionStore: Send + Sync + 'static {
    /// Mints a new session with a fresh 256-bit token and CSRF secret.
    async fn create(&self, principal: Principal) -> Result<IssuedSession, KernelError>;
    /// Resolves a live session by its token and records activity (slides the idle window).
    /// Unknown, revoked, idle-expired and absolutely-expired sessions yield `None`.
    async fn get(&self, token: &str) -> Result<Option<Session>, KernelError>;
    /// Revokes the session (idempotent; unknown tokens are ignored).
    async fn revoke(&self, token: &str) -> Result<(), KernelError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_debug_is_redacted() {
        let c = Credentials {
            login_id: "a@b".into(),
            password: "s3cret".into(),
        };
        let s = format!("{c:?}");
        assert!(!s.contains("s3cret"));
        assert!(s.contains("redacted"));
    }

    #[test]
    fn session_policy_validation_and_liveness() {
        assert!(SessionPolicy::new(0, 10).is_err());
        assert!(SessionPolicy::new(20, 10).is_err());
        let p = SessionPolicy::new(60, 3600).unwrap_or_else(|_| unreachable!());
        let now = Utc::now();
        let expires = now + Duration::seconds(3600);
        assert!(p.is_live(now, now, expires));
        assert!(
            !p.is_live(now, now - Duration::seconds(61), expires),
            "idle"
        );
        assert!(!p.is_live(expires, expires, expires), "absolute");
    }

    #[test]
    fn issued_session_debug_redacts_secrets() {
        let now = Utc::now();
        let s = IssuedSession {
            token: "tok-secret".into(),
            session: Session {
                id: Uuid::now_v7(),
                principal: Principal {
                    subject: "s".into(),
                    display_name: "d".into(),
                    login_id: "l".into(),
                    roles: vec![Role::Student],
                    scope: ExecutionScope::tenant(sutra_kernel::scope::TenantId::new()),
                },
                created_at: now,
                last_seen_at: now,
                expires_at: now,
                idle_expires_at: now,
                csrf_token: "csrf-secret".into(),
            },
        };
        let d = format!("{s:?}");
        assert!(!d.contains("tok-secret") && !d.contains("csrf-secret"));
        let ctx = s.session.auth_context();
        assert_eq!(Principal::from(&ctx), s.session.principal);
    }
}
