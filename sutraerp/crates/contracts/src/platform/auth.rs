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
pub use sutra_kernel::auth::{MfaLevel, Role};

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

/// Primary-factor credential verification port (password login through
/// `POST /api/v1/auth/login`). Implementations: `DevAuthenticator` (development personas);
/// `KeycloakAuthenticator` refuses password login (`501`) because Keycloak mode signs in through
/// the OIDC Authorization Code + PKCE flow and never uses the ROPC grant (ADR-L5-0038 §3.3).
///
/// The second factor is not the authenticator's concern: the platform module evaluates
/// [`MfaPolicy`] and verifies TOTP itself (dev mode) or reads Keycloak's `amr`/`acr` claims.
#[async_trait::async_trait]
pub trait CredentialAuthenticator: Send + Sync + 'static {
    /// Must return [`KernelError::Unauthenticated`] for any failure (no account enumeration).
    async fn authenticate(&self, credentials: Credentials) -> Result<Principal, KernelError>;
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

/// Default `last_seen_at` write throttle (seconds).
pub const DEFAULT_TOUCH_INTERVAL_SECS: i64 = 60;

/// Idle and absolute session lifetimes (ADR-L5-0038 §3.4). A session dies at whichever comes
/// first: `created_at + absolute_timeout`, or `last_seen_at + idle_timeout`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionPolicy {
    pub idle_timeout: Duration,
    pub absolute_timeout: Duration,
    /// `last_seen_at` is only written when it is older than this (write throttling). The idle
    /// window therefore slides in steps of at most this interval.
    pub touch_interval: Duration,
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
            touch_interval: Duration::seconds(DEFAULT_TOUCH_INTERVAL_SECS.min(idle_secs / 2)),
        })
    }

    /// Sets the `last_seen_at` write throttle. Must be `>= 0` and smaller than the idle timeout
    /// (otherwise an active session could idle out between two recorded touches).
    pub fn with_touch_interval(mut self, secs: i64) -> Result<Self, KernelError> {
        if secs < 0 || secs >= self.idle_timeout.num_seconds() {
            return Err(KernelError::BadRequest(
                "session touch interval must be >= 0 and < idle timeout".into(),
            ));
        }
        self.touch_interval = Duration::seconds(secs);
        Ok(self)
    }

    /// `true` when activity at `now` should be persisted (throttled `last_seen_at` write).
    pub fn should_touch(&self, now: DateTime<Utc>, last_seen_at: DateTime<Utc>) -> bool {
        now - last_seen_at >= self.touch_interval
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
    /// Authentication assurance of the session.
    pub mfa_level: MfaLevel,
    /// Authentication methods used (`pwd`, `otp`, `hwk`, …).
    pub amr: Vec<String>,
    /// Identity-provider session id (Keycloak `sid`), for RP-initiated / back-channel logout.
    pub idp_sid: Option<String>,
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
            .field("mfa_level", &self.mfa_level)
            .field("amr", &self.amr)
            .field("idp_sid", &self.idp_sid)
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
            mfa_level: self.mfa_level,
            amr: self.amr.clone(),
            step_up_enforced: false,
        }
    }
}

/// Everything needed to mint a session besides the token material.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewSession {
    pub principal: Principal,
    pub mfa_level: MfaLevel,
    pub amr: Vec<String>,
    pub idp_sid: Option<String>,
    /// Caps the absolute expiry (used when a session is rotated on step-up so the original
    /// absolute lifetime is preserved).
    pub not_after: Option<DateTime<Utc>>,
}

impl NewSession {
    /// Single-factor password session.
    pub fn password(principal: Principal) -> Self {
        Self {
            principal,
            mfa_level: MfaLevel::None,
            amr: vec!["pwd".into()],
            idp_sid: None,
            not_after: None,
        }
    }

    /// Password + verified TOTP.
    pub fn password_and_totp(principal: Principal) -> Self {
        Self {
            mfa_level: MfaLevel::Mfa,
            amr: vec!["pwd".into(), "otp".into(), "mfa".into()],
            ..Self::password(principal)
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
    async fn create(&self, new: NewSession) -> Result<IssuedSession, KernelError>;
    /// Resolves a live session by its token and records activity (slides the idle window; the
    /// `last_seen_at` write is throttled by [`SessionPolicy::touch_interval`]).
    /// Unknown, revoked, idle-expired and absolutely-expired sessions yield `None`.
    async fn get(&self, token: &str) -> Result<Option<Session>, KernelError>;
    /// Revokes the session (idempotent; unknown tokens are ignored).
    async fn revoke(&self, token: &str) -> Result<(), KernelError>;
}

// ---------------------------------------------------------------------------------------------
// Pre-authentication records (OIDC state/nonce/PKCE, pending MFA challenges)
// ---------------------------------------------------------------------------------------------

/// Kind of a short-lived pre-authentication record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreAuthKind {
    /// OIDC authorization request in flight (state → nonce, PKCE verifier, return path).
    OidcAuthorization,
    /// Primary factor accepted; waiting for the second factor.
    MfaChallenge,
}

impl PreAuthKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            PreAuthKind::OidcAuthorization => "oidc",
            PreAuthKind::MfaChallenge => "mfa",
        }
    }
}

/// Server-side store of single-use, short-lived pre-auth records. The handle (OIDC `state`,
/// MFA `challengeId`) is a 256-bit random value handed to the client; only `SHA-256(handle)`
/// is stored. [`PreAuthStore::take`] is atomic and single use: a record can be consumed once,
/// and never after it expired.
#[async_trait::async_trait]
pub trait PreAuthStore: Send + Sync + 'static {
    /// Stores `payload` and returns a fresh random handle.
    async fn put(
        &self,
        kind: PreAuthKind,
        payload: serde_json::Value,
        ttl: Duration,
    ) -> Result<String, KernelError>;
    /// Consumes the record (whatever the caller then decides). `None` when unknown, expired,
    /// already consumed or of another kind.
    async fn take(
        &self,
        kind: PreAuthKind,
        handle: &str,
    ) -> Result<Option<serde_json::Value>, KernelError>;
}

// ---------------------------------------------------------------------------------------------
// MFA factors (dev-mode TOTP; WebAuthn port deferred)
// ---------------------------------------------------------------------------------------------

/// A stored TOTP factor. The shared secret is only ever held encrypted (AES-256-GCM).
#[derive(Clone, PartialEq, Eq)]
pub struct TotpFactor {
    pub id: Uuid,
    /// `nonce ‖ ciphertext ‖ tag`, bound to tenant + subject as associated data.
    pub secret_ciphertext: Vec<u8>,
    pub key_version: i16,
    /// `false` while enrolment awaits the first valid code.
    pub active: bool,
    /// Last accepted TOTP time step (replay protection).
    pub last_used_step: Option<i64>,
}

impl fmt::Debug for TotpFactor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TotpFactor")
            .field("id", &self.id)
            .field("active", &self.active)
            .field("last_used_step", &self.last_used_step)
            .finish_non_exhaustive()
    }
}

/// Persistence port for MFA factors (`platform.mfa_factor`). Keyed by tenant + subject.
#[async_trait::async_trait]
pub trait MfaFactorStore: Send + Sync + 'static {
    async fn totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Option<TotpFactor>, KernelError>;
    /// Replaces any *pending* TOTP enrolment with a new one (an active factor is kept until
    /// the new one is confirmed). Returns the pending factor id.
    async fn put_pending_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        secret_ciphertext: Vec<u8>,
        key_version: i16,
    ) -> Result<Uuid, KernelError>;
    /// Pending factor of the subject, if an enrolment is in progress.
    async fn pending_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Option<TotpFactor>, KernelError>;
    /// Atomically records `step` as used if it is newer than the last used step. For a pending
    /// factor this also activates it (replacing any previous active factor). Returns `false`
    /// on replay (step not newer) or when the factor no longer exists.
    async fn consume_totp_step(&self, factor_id: Uuid, step: i64) -> Result<bool, KernelError>;
}

/// WebAuthn (passkey / security key) port. **Deferred**: the shipped adapter answers `501`;
/// Keycloak mode already offers WebAuthn through its own required action.
#[async_trait::async_trait]
pub trait WebauthnPort: Send + Sync + 'static {
    /// Begins credential registration; returns `PublicKeyCredentialCreationOptions` JSON.
    async fn registration_options(
        &self,
        principal: &Principal,
    ) -> Result<serde_json::Value, KernelError>;
    /// Finishes registration with the authenticator's attestation response.
    async fn register(
        &self,
        principal: &Principal,
        response: serde_json::Value,
    ) -> Result<(), KernelError>;
    /// Begins an assertion; returns `PublicKeyCredentialRequestOptions` JSON.
    async fn assertion_options(
        &self,
        principal: &Principal,
    ) -> Result<serde_json::Value, KernelError>;
    /// Verifies an assertion response.
    async fn verify_assertion(
        &self,
        principal: &Principal,
        response: serde_json::Value,
    ) -> Result<(), KernelError>;
}

// ---------------------------------------------------------------------------------------------
// Federated identity (OIDC — Keycloak 26)
// ---------------------------------------------------------------------------------------------

/// Parameters of one OIDC authorization request (generated and kept server-side).
#[derive(Clone, PartialEq, Eq)]
pub struct AuthorizationParams {
    pub state: String,
    pub nonce: String,
    /// `BASE64URL(SHA-256(code_verifier))` (PKCE `S256`).
    pub code_challenge: String,
    /// Ask the IdP for a fresh, multi-factor authentication (step-up).
    pub step_up: bool,
}

impl fmt::Debug for AuthorizationParams {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthorizationParams")
            .field("step_up", &self.step_up)
            .finish_non_exhaustive()
    }
}

/// Identity asserted by a validated ID token, mapped to SutraERP terms.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FederatedIdentity {
    pub principal: Principal,
    pub mfa_level: MfaLevel,
    pub amr: Vec<String>,
    /// IdP session id (`sid` claim).
    pub idp_sid: Option<String>,
}

/// OIDC identity-provider port used by the backend-for-frontend login flow.
#[async_trait::async_trait]
pub trait OidcIdentityProvider: Send + Sync + 'static {
    /// Authorization-endpoint URL to redirect the browser to.
    async fn authorization_url(&self, params: &AuthorizationParams) -> Result<String, KernelError>;
    /// Exchanges the authorization code (with the PKCE verifier) and validates the ID token
    /// (signature via JWKS, `iss`, `aud`, `exp`, `nonce`). Any failure is `Unauthenticated`
    /// (or `Forbidden` when the identity is valid but maps to no SutraERP role/tenant).
    async fn exchange(
        &self,
        code: &str,
        code_verifier: &str,
        expected_nonce: &str,
    ) -> Result<FederatedIdentity, KernelError>;
    /// RP-initiated logout URL (`end_session_endpoint`), when the IdP offers one.
    async fn end_session_url(&self, idp_sid: Option<&str>) -> Result<Option<String>, KernelError>;
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
        assert_eq!(p.touch_interval, Duration::seconds(30), "capped at idle/2");
        let p = SessionPolicy::new(600, 3600)
            .and_then(|p| p.with_touch_interval(60))
            .unwrap_or_else(|_| unreachable!());
        assert!(!p.should_touch(now, now - Duration::seconds(59)));
        assert!(p.should_touch(now, now - Duration::seconds(60)));
        assert!(p.with_touch_interval(600).is_err());
        assert!(p.with_touch_interval(-1).is_err());
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
                mfa_level: MfaLevel::None,
                amr: vec!["pwd".into()],
                idp_sid: None,
            },
        };
        let d = format!("{s:?}");
        assert!(!d.contains("tok-secret") && !d.contains("csrf-secret"));
        let ctx = s.session.auth_context();
        assert_eq!(Principal::from(&ctx), s.session.principal);
    }
}
