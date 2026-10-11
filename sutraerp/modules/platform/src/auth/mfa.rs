//! Multi-factor authentication (ADR-L5-0038 §3.8).
//!
//! * **TOTP** (RFC 6238: HMAC-SHA1, 6 digits, 30 s, ±1 step tolerance) for dev mode — enrolment
//!   returns an `otpauth://` URI, the first valid code activates the factor, and every accepted
//!   code records its time step so a code can never be used twice (replay within the same step
//!   or an earlier one is refused).
//! * The shared secret is **encrypted at rest** with AES-256-GCM ([`SecretBox`]); associated
//!   data binds the ciphertext to tenant, subject and factor kind.
//! * **WebAuthn** (dev mode) lives in [`super::webauthn`]; in Keycloak mode
//!   [`DeferredWebauthn`] answers `501` and Keycloak's `webauthn-register` required action is used.
//! * In Keycloak mode none of this runs: MFA is delegated to Keycloak and the session assurance
//!   is read from the ID token's `amr`/`acr` claims (see `keycloak.rs`).

use super::session::Clock;
use chrono::Utc;
use ring::aead::{self, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::hmac;
use sqlx::{PgPool, Row};
use std::sync::{Arc, Mutex};
use sutra_contracts::platform::auth::{
    MfaFactorStore, PasskeyInfo, Principal, TotpFactor, WebauthnCeremony, WebauthnPort,
};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::ExecutionScope;
use sutra_kernel::secret;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// TOTP period (seconds).
pub const TOTP_PERIOD: i64 = 30;
/// TOTP digits.
pub const TOTP_DIGITS: u32 = 6;
/// Accepted clock drift in steps on either side.
pub const TOTP_SKEW_STEPS: i64 = 1;
/// Shared-secret length (160 bits, RFC 4226 recommendation).
pub const TOTP_SECRET_LEN: usize = 20;
/// Current encryption key version written to `platform.mfa_factor.key_version`.
pub const KEY_VERSION: i16 = 1;

// ---------------------------------------------------------------------------------------------
// RFC 4648 base32 (no padding) — the encoding authenticator apps expect.
// ---------------------------------------------------------------------------------------------

const B32: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";

pub fn base32_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(5) * 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in data {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(B32[((buf >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(B32[((buf << (5 - bits)) & 31) as usize]));
    }
    out
}

pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 5 / 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| *c != b'=' && !c.is_ascii_whitespace()) {
        let v = B32.iter().position(|x| *x == c.to_ascii_uppercase())? as u32;
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

// ---------------------------------------------------------------------------------------------
// TOTP
// ---------------------------------------------------------------------------------------------

/// Time step of a Unix timestamp.
pub fn totp_step(unix_secs: i64) -> i64 {
    unix_secs.div_euclid(TOTP_PERIOD)
}

/// HOTP value of `secret` at counter `step` (RFC 4226 dynamic truncation).
pub fn hotp(secret: &[u8], step: i64) -> u32 {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let h = tag.as_ref();
    let offset = usize::from(h[h.len() - 1] & 0x0f);
    let bin = (u32::from(h[offset] & 0x7f) << 24)
        | (u32::from(h[offset + 1]) << 16)
        | (u32::from(h[offset + 2]) << 8)
        | u32::from(h[offset + 3]);
    bin % 10u32.pow(TOTP_DIGITS)
}

/// Six-digit TOTP code for `step`.
pub fn totp_code(secret: &[u8], step: i64) -> String {
    format!("{:06}", hotp(secret, step))
}

/// Returns the time step a well-formed `code` matches within the skew window (constant-time
/// comparison per candidate), preferring the current step.
pub fn totp_match(secret: &[u8], code: &str, unix_secs: i64) -> Option<i64> {
    let code = code.trim();
    if code.len() != TOTP_DIGITS as usize || !code.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let now = totp_step(unix_secs);
    let mut found = None;
    for step in [now, now - 1, now + 1]
        .into_iter()
        .take(1 + 2 * TOTP_SKEW_STEPS as usize)
    {
        if secret::ct_eq(totp_code(secret, step).as_bytes(), code.as_bytes()) && found.is_none() {
            found = Some(step);
        }
    }
    found
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'@') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `otpauth://totp/…` key URI (Google Authenticator key-uri format).
pub fn otpauth_uri(issuer: &str, account: &str, secret_b32: &str) -> String {
    format!(
        "otpauth://totp/{}:{}?secret={secret_b32}&issuer={}&algorithm=SHA1&digits={TOTP_DIGITS}&period={TOTP_PERIOD}",
        percent_encode(issuer),
        percent_encode(account),
        percent_encode(issuer)
    )
}

// ---------------------------------------------------------------------------------------------
// Encryption at rest
// ---------------------------------------------------------------------------------------------

/// AES-256-GCM sealing of factor secrets. Output layout: `nonce(12) ‖ ciphertext ‖ tag(16)`;
/// nonces are random per seal (a key encrypts few enough secrets for random 96-bit nonces).
#[derive(Clone)]
pub struct SecretBox {
    key: Arc<LessSafeKey>,
}

impl SecretBox {
    pub fn new(key: &[u8; 32]) -> Result<Self, KernelError> {
        let unbound = UnboundKey::new(&aead::AES_256_GCM, key)
            .map_err(|_| KernelError::internal_msg("invalid MFA encryption key"))?;
        Ok(Self {
            key: Arc::new(LessSafeKey::new(unbound)),
        })
    }

    pub fn seal(&self, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>, KernelError> {
        let mut nonce = [0u8; aead::NONCE_LEN];
        nonce.copy_from_slice(&secret::random_bytes()?[..aead::NONCE_LEN]);
        let mut buf = plaintext.to_vec();
        self.key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(aad),
                &mut buf,
            )
            .map_err(|_| KernelError::internal_msg("MFA secret encryption failed"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&buf);
        Ok(out)
    }

    pub fn open(&self, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>, KernelError> {
        if sealed.len() <= aead::NONCE_LEN + aead::AES_256_GCM.tag_len() {
            return Err(KernelError::internal_msg("MFA secret ciphertext truncated"));
        }
        let (nonce, ct) = sealed.split_at(aead::NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce)
            .map_err(|_| KernelError::internal_msg("MFA secret nonce"))?;
        let mut buf = ct.to_vec();
        let plain = self
            .key
            .open_in_place(nonce, Aad::from(aad), &mut buf)
            .map_err(|_| KernelError::internal_msg("MFA secret decryption failed"))?;
        Ok(plain.to_vec())
    }
}

fn totp_aad(scope: &ExecutionScope, subject: &str) -> Vec<u8> {
    format!("sutraerp.mfa.totp.v1|{}|{subject}|totp", scope.tenant_id).into_bytes()
}

// ---------------------------------------------------------------------------------------------
// Factor stores
// ---------------------------------------------------------------------------------------------

#[derive(Clone)]
struct MemRow {
    tenant: Uuid,
    subject: String,
    factor: TotpFactor,
}

/// No-database factor store (dev/tests).
#[derive(Default)]
pub struct InMemoryMfaFactorStore {
    rows: Mutex<Vec<MemRow>>,
}

impl InMemoryMfaFactorStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Vec<MemRow>>, KernelError> {
        self.rows
            .lock()
            .map_err(|_| KernelError::internal_msg("mfa store poisoned"))
    }

    fn find(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        active: bool,
    ) -> Result<Option<TotpFactor>, KernelError> {
        Ok(self
            .lock()?
            .iter()
            .find(|r| {
                r.tenant == scope.tenant_id.as_uuid()
                    && r.subject == subject
                    && r.factor.active == active
            })
            .map(|r| r.factor.clone()))
    }
}

#[async_trait::async_trait]
impl MfaFactorStore for InMemoryMfaFactorStore {
    async fn totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Option<TotpFactor>, KernelError> {
        self.find(scope, subject, true)
    }

    async fn pending_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Option<TotpFactor>, KernelError> {
        self.find(scope, subject, false)
    }

    async fn put_pending_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        secret_ciphertext: Vec<u8>,
        key_version: i16,
    ) -> Result<Uuid, KernelError> {
        let tenant = scope.tenant_id.as_uuid();
        let mut rows = self.lock()?;
        rows.retain(|r| !(r.tenant == tenant && r.subject == subject && !r.factor.active));
        let id = sutra_kernel::id::new_v7();
        rows.push(MemRow {
            tenant,
            subject: subject.to_owned(),
            factor: TotpFactor {
                id,
                secret_ciphertext,
                key_version,
                active: false,
                last_used_step: None,
            },
        });
        Ok(id)
    }

    async fn consume_totp_step(&self, factor_id: Uuid, step: i64) -> Result<bool, KernelError> {
        let mut rows = self.lock()?;
        let Some(idx) = rows.iter().position(|r| r.factor.id == factor_id) else {
            return Ok(false);
        };
        if rows[idx]
            .factor
            .last_used_step
            .is_some_and(|last| step <= last)
        {
            return Ok(false);
        }
        if !rows[idx].factor.active {
            let (tenant, subject) = (rows[idx].tenant, rows[idx].subject.clone());
            rows.retain(|r| !(r.tenant == tenant && r.subject == subject && r.factor.active));
        }
        if let Some(r) = rows.iter_mut().find(|r| r.factor.id == factor_id) {
            r.factor.active = true;
            r.factor.last_used_step = Some(step);
        }
        Ok(true)
    }

    async fn remove_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<bool, KernelError> {
        let mut rows = self.lock()?;
        let before = rows.len();
        rows.retain(|r| !(r.tenant == scope.tenant_id.as_uuid() && r.subject == subject));
        Ok(rows.len() < before)
    }
}

/// `platform.mfa_factor` store.
#[derive(Clone)]
pub struct PgMfaFactorStore {
    pool: PgPool,
}

impl PgMfaFactorStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn find(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        status: &str,
    ) -> Result<Option<TotpFactor>, KernelError> {
        let row = sqlx::query(
            "SELECT id, secret_ciphertext, key_version, status, last_used_step \
             FROM platform.mfa_factor \
             WHERE tenant_id = $1 AND subject = $2 AND kind = 'totp' AND status = $3",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(subject)
        .bind(status)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.map(|r| {
            Ok(TotpFactor {
                id: r.try_get("id").map_err(map_db_error)?,
                secret_ciphertext: r.try_get("secret_ciphertext").map_err(map_db_error)?,
                key_version: r.try_get("key_version").map_err(map_db_error)?,
                active: r.try_get::<String, _>("status").map_err(map_db_error)? == "active",
                last_used_step: r.try_get("last_used_step").map_err(map_db_error)?,
            })
        })
        .transpose()
    }
}

#[async_trait::async_trait]
impl MfaFactorStore for PgMfaFactorStore {
    async fn totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Option<TotpFactor>, KernelError> {
        self.find(scope, subject, "active").await
    }

    async fn pending_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Option<TotpFactor>, KernelError> {
        self.find(scope, subject, "pending").await
    }

    async fn put_pending_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        secret_ciphertext: Vec<u8>,
        key_version: i16,
    ) -> Result<Uuid, KernelError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        sqlx::query(
            "DELETE FROM platform.mfa_factor \
             WHERE tenant_id = $1 AND subject = $2 AND kind = 'totp' AND status = 'pending'",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(subject)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO platform.mfa_factor \
               (tenant_id, subject, kind, status, secret_ciphertext, key_version) \
             VALUES ($1, $2, 'totp', 'pending', $3, $4) RETURNING id",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(subject)
        .bind(&secret_ciphertext)
        .bind(key_version)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(id)
    }

    async fn consume_totp_step(&self, factor_id: Uuid, step: i64) -> Result<bool, KernelError> {
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        // Row lock serialises concurrent verifications of the same factor.
        let row = sqlx::query(
            "SELECT tenant_id, subject, status, last_used_step FROM platform.mfa_factor \
             WHERE id = $1 FOR UPDATE",
        )
        .bind(factor_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let Some(row) = row else {
            return Ok(false);
        };
        let last: Option<i64> = row.try_get("last_used_step").map_err(map_db_error)?;
        if last.is_some_and(|l| step <= l) {
            return Ok(false);
        }
        let status: String = row.try_get("status").map_err(map_db_error)?;
        if status == "pending" {
            let tenant: Uuid = row.try_get("tenant_id").map_err(map_db_error)?;
            let subject: String = row.try_get("subject").map_err(map_db_error)?;
            sqlx::query(
                "DELETE FROM platform.mfa_factor \
                 WHERE tenant_id = $1 AND subject = $2 AND kind = 'totp' AND status = 'active'",
            )
            .bind(tenant)
            .bind(&subject)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        sqlx::query(
            "UPDATE platform.mfa_factor SET status = 'active', \
               confirmed_at = COALESCE(confirmed_at, now()), last_used_step = $2 \
             WHERE id = $1",
        )
        .bind(factor_id)
        .bind(step)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(true)
    }

    async fn remove_totp(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<bool, KernelError> {
        let n = sqlx::query(
            "DELETE FROM platform.mfa_factor WHERE tenant_id = $1 AND subject = $2 AND kind = 'totp'",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(subject)
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n > 0)
    }
}

// ---------------------------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------------------------

/// What the client needs to add the factor to an authenticator app.
#[derive(Clone)]
pub struct TotpEnrolment {
    pub secret_b32: String,
    pub otpauth_uri: String,
}

/// Dev-mode TOTP service: enrolment, confirmation and verification with replay protection.
#[derive(Clone)]
pub struct MfaService {
    store: Arc<dyn MfaFactorStore>,
    secret_box: SecretBox,
    issuer: String,
    clock: Clock,
}

impl MfaService {
    pub fn new(store: Arc<dyn MfaFactorStore>, secret_box: SecretBox, issuer: String) -> Self {
        Self::with_clock(store, secret_box, issuer, Arc::new(Utc::now))
    }

    pub fn with_clock(
        store: Arc<dyn MfaFactorStore>,
        secret_box: SecretBox,
        issuer: String,
        clock: Clock,
    ) -> Self {
        Self {
            store,
            secret_box,
            issuer,
            clock,
        }
    }

    /// Removes the subject's TOTP factor (active and pending). `false` when there was none.
    pub async fn disable_totp(&self, p: &Principal) -> Result<bool, KernelError> {
        self.store.remove_totp(&p.scope, &p.subject).await
    }

    /// `true` when the subject has an active TOTP factor.
    pub async fn has_totp(&self, p: &Principal) -> Result<bool, KernelError> {
        Ok(self.store.totp(&p.scope, &p.subject).await?.is_some())
    }

    /// Starts (or restarts) enrolment: a fresh 160-bit secret, stored encrypted as *pending*.
    pub async fn begin_totp_enrolment(&self, p: &Principal) -> Result<TotpEnrolment, KernelError> {
        let raw = secret::random_bytes()?;
        let secret = &raw[..TOTP_SECRET_LEN];
        let sealed = self
            .secret_box
            .seal(secret, &totp_aad(&p.scope, &p.subject))?;
        self.store
            .put_pending_totp(&p.scope, &p.subject, sealed, KEY_VERSION)
            .await?;
        let secret_b32 = base32_encode(secret);
        Ok(TotpEnrolment {
            otpauth_uri: otpauth_uri(&self.issuer, &p.login_id, &secret_b32),
            secret_b32,
        })
    }

    fn check(&self, p: &Principal, f: &TotpFactor, code: &str) -> Result<Option<i64>, KernelError> {
        let secret = self
            .secret_box
            .open(&f.secret_ciphertext, &totp_aad(&p.scope, &p.subject))?;
        Ok(totp_match(&secret, code, (self.clock)().timestamp()))
    }

    /// Confirms a pending enrolment with its first code (activates the factor).
    pub async fn confirm_totp_enrolment(
        &self,
        p: &Principal,
        code: &str,
    ) -> Result<(), KernelError> {
        let invalid = || {
            KernelError::Validation(vec![FieldViolation::new(
                "code",
                "invalid",
                "the code does not match the authenticator being enrolled",
            )])
        };
        let pending = self
            .store
            .pending_totp(&p.scope, &p.subject)
            .await?
            .ok_or_else(|| KernelError::Conflict("no TOTP enrolment is in progress".into()))?;
        let step = self.check(p, &pending, code)?.ok_or_else(invalid)?;
        if !self.store.consume_totp_step(pending.id, step).await? {
            return Err(invalid());
        }
        Ok(())
    }

    /// Verifies a login / step-up code against the active factor. Every failure (no factor,
    /// wrong code, replayed code) is the same `Unauthenticated`.
    pub async fn verify_totp(&self, p: &Principal, code: &str) -> Result<(), KernelError> {
        let factor = self
            .store
            .totp(&p.scope, &p.subject)
            .await?
            .ok_or(KernelError::Unauthenticated)?;
        let step = self
            .check(p, &factor, code)?
            .ok_or(KernelError::Unauthenticated)?;
        if !self.store.consume_totp_step(factor.id, step).await? {
            tracing::warn!(subject = %p.subject, "TOTP code replay refused");
            return Err(KernelError::Unauthenticated);
        }
        Ok(())
    }
}

/// WebAuthn adapter for Keycloak mode: every operation is `501 not-implemented` because
/// passkeys are registered and verified by Keycloak (`webauthn-register` required action).
/// Dev mode uses [`super::webauthn::DevWebauthn`].
pub struct DeferredWebauthn;

const WEBAUTHN_DEFERRED: &str =
    "passkeys are managed by Keycloak in this mode (webauthn-register required action)";

#[async_trait::async_trait]
impl WebauthnPort for DeferredWebauthn {
    fn enabled(&self) -> bool {
        false
    }
    async fn credentials(&self, _: &Principal) -> Result<Vec<PasskeyInfo>, KernelError> {
        Ok(vec![])
    }
    async fn start_registration(&self, _: &Principal) -> Result<WebauthnCeremony, KernelError> {
        Err(KernelError::NotImplemented(WEBAUTHN_DEFERRED))
    }
    async fn finish_registration(
        &self,
        _: &Principal,
        _: &str,
        _: serde_json::Value,
        _: &str,
    ) -> Result<PasskeyInfo, KernelError> {
        Err(KernelError::NotImplemented(WEBAUTHN_DEFERRED))
    }
    async fn start_authentication(&self, _: &Principal) -> Result<WebauthnCeremony, KernelError> {
        Err(KernelError::NotImplemented(WEBAUTHN_DEFERRED))
    }
    async fn finish_authentication(
        &self,
        _: &Principal,
        _: &str,
        _: serde_json::Value,
    ) -> Result<(), KernelError> {
        Err(KernelError::NotImplemented(WEBAUTHN_DEFERRED))
    }
    async fn remove(&self, _: &Principal, _: Uuid) -> Result<bool, KernelError> {
        Err(KernelError::NotImplemented(WEBAUTHN_DEFERRED))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use sutra_contracts::platform::auth::Role;
    use sutra_kernel::scope::TenantId;

    #[test]
    fn rfc6238_sha1_vectors() {
        // RFC 6238 Appendix B, SHA-1 seed "12345678901234567890", 8-digit values mod 10^6.
        let seed = b"12345678901234567890";
        for (t, code) in [
            (59, "287082"),
            (1_111_111_109, "081804"),
            (1_111_111_111, "050471"),
            (1_234_567_890, "005924"),
            (2_000_000_000, "279037"),
        ] {
            assert_eq!(totp_code(seed, totp_step(t)), code, "t={t}");
        }
    }

    #[test]
    fn base32_roundtrip_and_known_vector() {
        assert_eq!(base32_encode(b"foobar"), "MZXW6YTBOI");
        assert_eq!(base32_decode("MZXW6YTBOI").unwrap(), b"foobar");
        assert_eq!(base32_decode("mzxw6ytboi======").unwrap(), b"foobar");
        assert!(base32_decode("1!").is_none());
        let raw = [0xffu8; 20];
        assert_eq!(base32_decode(&base32_encode(&raw)).unwrap(), raw);
    }

    #[test]
    fn match_window_and_format() {
        let s = b"12345678901234567890";
        let now = 1_700_000_000;
        let step = totp_step(now);
        assert_eq!(totp_match(s, &totp_code(s, step), now), Some(step));
        assert_eq!(totp_match(s, &totp_code(s, step - 1), now), Some(step - 1));
        assert_eq!(totp_match(s, &totp_code(s, step + 1), now), Some(step + 1));
        assert_eq!(
            totp_match(s, &totp_code(s, step - 2), now),
            None,
            "outside skew"
        );
        assert_eq!(totp_match(s, "12345", now), None);
        assert_eq!(totp_match(s, "abcdef", now), None);
    }

    #[test]
    fn otpauth_uri_format() {
        let u = otpauth_uri("SutraERP (dev)", "admin@sutra.dev", "ABC");
        assert_eq!(
            u,
            "otpauth://totp/SutraERP%20%28dev%29:admin@sutra.dev?secret=ABC&issuer=SutraERP%20%28dev%29&algorithm=SHA1&digits=6&period=30"
        );
    }

    #[test]
    fn secret_box_roundtrip_and_aad_binding() {
        let b = SecretBox::new(&[3u8; 32]).unwrap();
        let sealed = b.seal(b"top secret", b"aad-1").unwrap();
        assert_ne!(&sealed[12..22], b"top secret", "not stored in clear");
        assert_eq!(b.open(&sealed, b"aad-1").unwrap(), b"top secret");
        assert!(b.open(&sealed, b"aad-2").is_err(), "AAD bound");
        assert!(SecretBox::new(&[4u8; 32])
            .unwrap()
            .open(&sealed, b"aad-1")
            .is_err());
        let mut flipped = sealed.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(b.open(&flipped, b"aad-1").is_err(), "tamper evident");
        assert_ne!(
            sealed,
            b.seal(b"top secret", b"aad-1").unwrap(),
            "random nonce"
        );
    }

    fn principal() -> Principal {
        Principal {
            subject: "dev|admin@sutra.dev".into(),
            display_name: "Admin".into(),
            login_id: "admin@sutra.dev".into(),
            roles: vec![Role::Administrator],
            scope: ExecutionScope::tenant(TenantId::new()),
        }
    }

    fn service(now: Arc<AtomicI64>) -> MfaService {
        MfaService::with_clock(
            Arc::new(InMemoryMfaFactorStore::default()),
            SecretBox::new(&[5u8; 32]).unwrap(),
            "SutraERP".into(),
            Arc::new(move || {
                chrono::DateTime::from_timestamp(now.load(Ordering::SeqCst), 0).unwrap()
            }),
        )
    }

    fn code_for(e: &TotpEnrolment, t: i64) -> String {
        totp_code(&base32_decode(&e.secret_b32).unwrap(), totp_step(t))
    }

    #[tokio::test]
    async fn enrol_confirm_verify_and_replay() {
        let now = Arc::new(AtomicI64::new(1_700_000_000));
        let svc = service(now.clone());
        let p = principal();
        assert!(!svc.has_totp(&p).await.unwrap());
        assert!(matches!(
            svc.confirm_totp_enrolment(&p, "000000").await,
            Err(KernelError::Conflict(_))
        ));
        let e = svc.begin_totp_enrolment(&p).await.unwrap();
        assert!(e
            .otpauth_uri
            .starts_with("otpauth://totp/SutraERP:admin@sutra.dev?secret="));
        assert_eq!(e.secret_b32.len(), 32);
        assert!(matches!(
            svc.confirm_totp_enrolment(&p, "000000").await,
            Err(KernelError::Validation(_))
        ));
        assert!(!svc.has_totp(&p).await.unwrap(), "still pending");
        let t = now.load(Ordering::SeqCst);
        svc.confirm_totp_enrolment(&p, &code_for(&e, t))
            .await
            .unwrap();
        assert!(svc.has_totp(&p).await.unwrap());

        // The enrolment code's step is spent: the same code cannot sign in (replay).
        assert!(matches!(
            svc.verify_totp(&p, &code_for(&e, t)).await,
            Err(KernelError::Unauthenticated)
        ));
        // Next step: accepted once, then replay within the same step is refused.
        now.store(t + 30, Ordering::SeqCst);
        let c = code_for(&e, t + 30);
        svc.verify_totp(&p, &c).await.unwrap();
        assert!(matches!(
            svc.verify_totp(&p, &c).await,
            Err(KernelError::Unauthenticated)
        ));
        // A still-valid earlier-step code (skew window) is refused after a newer step was used.
        now.store(t + 45, Ordering::SeqCst);
        assert!(svc.verify_totp(&p, &code_for(&e, t)).await.is_err());
        assert!(svc.verify_totp(&p, "999999").await.is_err());
    }

    #[tokio::test]
    async fn re_enrolment_replaces_the_active_factor_only_when_confirmed() {
        let now = Arc::new(AtomicI64::new(1_800_000_000));
        let svc = service(now.clone());
        let p = principal();
        let first = svc.begin_totp_enrolment(&p).await.unwrap();
        let t = now.load(Ordering::SeqCst);
        svc.confirm_totp_enrolment(&p, &code_for(&first, t))
            .await
            .unwrap();
        let second = svc.begin_totp_enrolment(&p).await.unwrap();
        now.store(t + 30, Ordering::SeqCst);
        // Old factor still works while the new one is pending.
        svc.verify_totp(&p, &code_for(&first, t + 30))
            .await
            .unwrap();
        now.store(t + 60, Ordering::SeqCst);
        svc.confirm_totp_enrolment(&p, &code_for(&second, t + 60))
            .await
            .unwrap();
        now.store(t + 90, Ordering::SeqCst);
        assert!(svc
            .verify_totp(&p, &code_for(&first, t + 90))
            .await
            .is_err());
        svc.verify_totp(&p, &code_for(&second, t + 90))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn factors_are_per_subject_and_tenant() {
        let now = Arc::new(AtomicI64::new(1_700_000_000));
        let svc = service(now.clone());
        let p = principal();
        let e = svc.begin_totp_enrolment(&p).await.unwrap();
        svc.confirm_totp_enrolment(&p, &code_for(&e, now.load(Ordering::SeqCst)))
            .await
            .unwrap();
        let mut other_tenant = p.clone();
        other_tenant.scope = ExecutionScope::tenant(TenantId::new());
        assert!(!svc.has_totp(&other_tenant).await.unwrap());
    }

    #[tokio::test]
    async fn webauthn_is_deferred_501_in_keycloak_mode() {
        let w = DeferredWebauthn;
        assert!(!w.enabled());
        assert!(matches!(
            w.start_registration(&principal()).await,
            Err(KernelError::NotImplemented(_))
        ));
        assert!(matches!(
            w.start_authentication(&principal()).await,
            Err(KernelError::NotImplemented(_))
        ));
        assert!(w.credentials(&principal()).await.unwrap().is_empty());
    }
}
