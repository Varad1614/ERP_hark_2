//! Dev-mode WebAuthn second factor (ADR-L5-0038 §3.9: passkeys preferred for privileged users).
//!
//! [`DevWebauthn`] is a WebAuthn Level 2 relying party built on `webauthn-rs`:
//!
//! * **Registration** — `start_registration` creates a challenge (user verification required,
//!   existing credentials excluded) and keeps the ceremony state server-side in
//!   `platform.pre_auth` (single use, 5 minutes, bound to tenant + subject); the browser only
//!   receives the options and an opaque `ceremonyId`. `finish_registration` verifies the
//!   attestation (challenge, origin, RP id hash, flags, COSE key) and stores the credential.
//! * **Authentication** — the same for assertions over the subject's credentials. After
//!   `webauthn-rs` verified the signature, the stored sign counter is advanced with an atomic
//!   compare (`counter > stored`, or both zero for counter-less passkeys); a non-increasing
//!   counter — a replayed assertion or a cloned authenticator — is refused.
//! * Ceremony state is consumed on first use, so a captured response can never be replayed
//!   against the same challenge, and a ceremony started for one user cannot finish for another.
//!
//! Keycloak mode keeps using [`super::mfa::DeferredWebauthn`] (`501`): passkeys are then managed
//! by Keycloak's `webauthn-register` required action.

use super::session::Clock;
use chrono::{DateTime, Duration, Utc};
use sqlx::{PgPool, Row};
use std::sync::{Arc, Mutex};
use sutra_contracts::platform::auth::{
    PasskeyInfo, PreAuthKind, PreAuthStore, Principal, WebauthnCeremony, WebauthnPort,
};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::ExecutionScope;
use sutra_kernel::secret;
use sutra_persistence::map_db_error;
use uuid::Uuid;
use webauthn_rs::prelude::{
    CredentialID, Passkey, PasskeyAuthentication, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Webauthn, WebauthnBuilder,
};

/// Lifetime of a registration / authentication ceremony.
pub const CEREMONY_TTL_SECS: i64 = 300;
/// Maximum passkeys per subject.
pub const MAX_CREDENTIALS: usize = 10;

/// Relying-party settings (`SUTRA_WEBAUTHN_*`).
#[derive(Clone, Debug)]
pub struct WebauthnSettings {
    /// RP id: the registrable domain the credentials are scoped to (`localhost` in dev).
    pub rp_id: String,
    pub rp_name: String,
    /// Exact web origins allowed to run ceremonies (`http://localhost:5173`, …).
    pub origins: Vec<String>,
}

/// A stored credential.
#[derive(Clone, Debug)]
pub struct StoredPasskey {
    pub id: Uuid,
    pub label: String,
    pub passkey: Passkey,
    pub sign_count: i64,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl StoredPasskey {
    pub fn info(&self) -> PasskeyInfo {
        PasskeyInfo {
            id: self.id,
            label: self.label.clone(),
            created_at: self.created_at,
            last_used_at: self.last_used_at,
        }
    }
}

/// Persistence of WebAuthn credentials (`platform.webauthn_credential`).
#[async_trait::async_trait]
pub trait WebauthnCredentialStore: Send + Sync + 'static {
    async fn list(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Vec<StoredPasskey>, KernelError>;
    /// Stores a new credential; `Conflict` when the credential id is already registered.
    async fn insert(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        label: &str,
        passkey: &Passkey,
    ) -> Result<StoredPasskey, KernelError>;
    /// Records a verified assertion: stores the updated credential and advances the counter
    /// **only if** `counter > stored` (or both are zero). `false` = replay / cloned
    /// authenticator.
    async fn record_use(
        &self,
        id: Uuid,
        passkey: &Passkey,
        counter: u32,
    ) -> Result<bool, KernelError>;
    async fn remove(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        id: Uuid,
    ) -> Result<bool, KernelError>;
}

fn counter_advances(new: i64, stored: i64) -> bool {
    new > stored || (new == 0 && stored == 0)
}

/// (tenant, subject, credential) rows of the in-memory store.
type MemRows = Vec<(Uuid, String, StoredPasskey)>;

/// No-database store (dev/tests).
pub struct InMemoryWebauthnStore {
    clock: Clock,
    rows: Mutex<MemRows>,
}

impl Default for InMemoryWebauthnStore {
    fn default() -> Self {
        Self {
            clock: Arc::new(Utc::now),
            rows: Mutex::new(vec![]),
        }
    }
}

impl InMemoryWebauthnStore {
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MemRows>, KernelError> {
        self.rows
            .lock()
            .map_err(|_| KernelError::internal_msg("webauthn store poisoned"))
    }
}

#[async_trait::async_trait]
impl WebauthnCredentialStore for InMemoryWebauthnStore {
    async fn list(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Vec<StoredPasskey>, KernelError> {
        Ok(self
            .lock()?
            .iter()
            .filter(|(t, s, _)| *t == scope.tenant_id.as_uuid() && s == subject)
            .map(|(_, _, p)| p.clone())
            .collect())
    }

    async fn insert(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        label: &str,
        passkey: &Passkey,
    ) -> Result<StoredPasskey, KernelError> {
        let mut rows = self.lock()?;
        if rows
            .iter()
            .any(|(_, _, p)| p.passkey.cred_id() == passkey.cred_id())
        {
            return Err(KernelError::Conflict(
                "this authenticator is already registered".into(),
            ));
        }
        let p = StoredPasskey {
            id: sutra_kernel::id::new_v7(),
            label: label.to_owned(),
            passkey: passkey.clone(),
            sign_count: 0,
            created_at: (self.clock)(),
            last_used_at: None,
        };
        rows.push((scope.tenant_id.as_uuid(), subject.to_owned(), p.clone()));
        Ok(p)
    }

    async fn record_use(
        &self,
        id: Uuid,
        passkey: &Passkey,
        counter: u32,
    ) -> Result<bool, KernelError> {
        let now = (self.clock)();
        let mut rows = self.lock()?;
        let Some((_, _, p)) = rows.iter_mut().find(|(_, _, p)| p.id == id) else {
            return Ok(false);
        };
        if !counter_advances(i64::from(counter), p.sign_count) {
            return Ok(false);
        }
        p.sign_count = i64::from(counter);
        p.passkey = passkey.clone();
        p.last_used_at = Some(now);
        Ok(true)
    }

    async fn remove(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        id: Uuid,
    ) -> Result<bool, KernelError> {
        let mut rows = self.lock()?;
        let before = rows.len();
        rows.retain(|(t, s, p)| !(p.id == id && *t == scope.tenant_id.as_uuid() && s == subject));
        Ok(rows.len() < before)
    }
}

/// `platform.webauthn_credential`.
#[derive(Clone)]
pub struct PgWebauthnStore {
    pool: PgPool,
}

impl PgWebauthnStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    fn row(r: &sqlx::postgres::PgRow) -> Result<StoredPasskey, KernelError> {
        let raw: serde_json::Value = r.try_get("passkey").map_err(map_db_error)?;
        Ok(StoredPasskey {
            id: r.try_get("id").map_err(map_db_error)?,
            label: r.try_get("label").map_err(map_db_error)?,
            passkey: serde_json::from_value(raw).map_err(KernelError::internal)?,
            sign_count: r.try_get("sign_count").map_err(map_db_error)?,
            created_at: r.try_get("created_at").map_err(map_db_error)?,
            last_used_at: r.try_get("last_used_at").map_err(map_db_error)?,
        })
    }
}

const PG_COLUMNS: &str = "id, label, passkey, sign_count, created_at, last_used_at";

#[async_trait::async_trait]
impl WebauthnCredentialStore for PgWebauthnStore {
    async fn list(
        &self,
        scope: &ExecutionScope,
        subject: &str,
    ) -> Result<Vec<StoredPasskey>, KernelError> {
        let rows = sqlx::query(&format!(
            "SELECT {PG_COLUMNS} FROM platform.webauthn_credential \
             WHERE tenant_id = $1 AND subject = $2 ORDER BY created_at, id"
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(subject)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        rows.iter().map(Self::row).collect()
    }

    async fn insert(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        label: &str,
        passkey: &Passkey,
    ) -> Result<StoredPasskey, KernelError> {
        let json = serde_json::to_value(passkey).map_err(KernelError::internal)?;
        let row = sqlx::query(&format!(
            "INSERT INTO platform.webauthn_credential \
               (tenant_id, subject, credential_id, label, passkey) \
             VALUES ($1, $2, $3, $4, $5) RETURNING {PG_COLUMNS}"
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(subject)
        .bind(passkey.cred_id().as_slice())
        .bind(label)
        .bind(json)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                KernelError::Conflict("this authenticator is already registered".into())
            }
            other => other,
        })?;
        Self::row(&row)
    }

    async fn record_use(
        &self,
        id: Uuid,
        passkey: &Passkey,
        counter: u32,
    ) -> Result<bool, KernelError> {
        let json = serde_json::to_value(passkey).map_err(KernelError::internal)?;
        let n = sqlx::query(
            "UPDATE platform.webauthn_credential \
             SET passkey = $2, sign_count = $3, last_used_at = now() \
             WHERE id = $1 AND ($3 > sign_count OR ($3 = 0 AND sign_count = 0))",
        )
        .bind(id)
        .bind(json)
        .bind(i64::from(counter))
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n == 1)
    }

    async fn remove(
        &self,
        scope: &ExecutionScope,
        subject: &str,
        id: Uuid,
    ) -> Result<bool, KernelError> {
        let n = sqlx::query(
            "DELETE FROM platform.webauthn_credential \
             WHERE id = $1 AND tenant_id = $2 AND subject = $3",
        )
        .bind(id)
        .bind(scope.tenant_id.as_uuid())
        .bind(subject)
        .execute(&self.pool)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        Ok(n == 1)
    }
}

/// Server-side ceremony record (in `platform.pre_auth`).
#[derive(serde::Serialize, serde::Deserialize)]
struct CeremonyPayload {
    tenant: Uuid,
    subject: String,
    state: serde_json::Value,
}

/// Stable, non-identifying WebAuthn user handle for (tenant, subject).
pub fn user_handle(scope: &ExecutionScope, subject: &str) -> Uuid {
    let h = secret::sha256(
        format!("sutraerp.webauthn.user.v1|{}|{subject}", scope.tenant_id).as_bytes(),
    );
    let mut b = [0u8; 16];
    b.copy_from_slice(&h[..16]);
    Uuid::from_bytes(b)
}

fn label_of(label: &str) -> Result<String, KernelError> {
    let l = label.trim();
    let l = if l.is_empty() { "Passkey" } else { l };
    if l.chars().count() > 64 || l.chars().any(char::is_control) {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "label",
            "format",
            "label must be at most 64 printable characters",
        )]));
    }
    Ok(l.to_owned())
}

/// The webauthn-rs relying party + stores.
pub struct DevWebauthn {
    webauthn: Webauthn,
    store: Arc<dyn WebauthnCredentialStore>,
    pre_auth: Arc<dyn PreAuthStore>,
}

impl DevWebauthn {
    pub fn new(
        settings: &WebauthnSettings,
        store: Arc<dyn WebauthnCredentialStore>,
        pre_auth: Arc<dyn PreAuthStore>,
    ) -> Result<Self, KernelError> {
        let bad = |what: String| KernelError::BadRequest(format!("WebAuthn configuration: {what}"));
        let mut origins = settings.origins.iter();
        let first = origins
            .next()
            .ok_or_else(|| bad("at least one origin is required".into()))?;
        let first = Url::parse(first).map_err(|e| bad(format!("origin {first}: {e}")))?;
        let mut builder = WebauthnBuilder::new(&settings.rp_id, &first)
            .map_err(|e| bad(format!("rp id {} vs origin {first}: {e}", settings.rp_id)))?
            .rp_name(&settings.rp_name);
        for o in origins {
            let u = Url::parse(o).map_err(|e| bad(format!("origin {o}: {e}")))?;
            builder = builder.append_allowed_origin(&u);
        }
        Ok(Self {
            webauthn: builder.build().map_err(|e| bad(e.to_string()))?,
            store,
            pre_auth,
        })
    }

    async fn put_ceremony(
        &self,
        kind: PreAuthKind,
        p: &Principal,
        state: serde_json::Value,
        options: serde_json::Value,
    ) -> Result<WebauthnCeremony, KernelError> {
        let payload = CeremonyPayload {
            tenant: p.scope.tenant_id.as_uuid(),
            subject: p.subject.clone(),
            state,
        };
        let ceremony_id = self
            .pre_auth
            .put(
                kind,
                serde_json::to_value(&payload).map_err(KernelError::internal)?,
                Duration::seconds(CEREMONY_TTL_SECS),
            )
            .await?;
        Ok(WebauthnCeremony {
            ceremony_id,
            options,
        })
    }

    /// Consumes the ceremony (single use) and checks it belongs to `p`.
    async fn take_ceremony(
        &self,
        kind: PreAuthKind,
        p: &Principal,
        ceremony_id: &str,
    ) -> Result<serde_json::Value, KernelError> {
        let raw = self
            .pre_auth
            .take(kind, ceremony_id.trim())
            .await?
            .ok_or(KernelError::Unauthenticated)?;
        let c: CeremonyPayload = serde_json::from_value(raw).map_err(KernelError::internal)?;
        if c.tenant != p.scope.tenant_id.as_uuid() || c.subject != p.subject {
            tracing::warn!(subject = %p.subject, "WebAuthn ceremony presented by another account");
            return Err(KernelError::Unauthenticated);
        }
        Ok(c.state)
    }
}

#[async_trait::async_trait]
impl WebauthnPort for DevWebauthn {
    fn enabled(&self) -> bool {
        true
    }

    async fn credentials(&self, p: &Principal) -> Result<Vec<PasskeyInfo>, KernelError> {
        Ok(self
            .store
            .list(&p.scope, &p.subject)
            .await?
            .iter()
            .map(StoredPasskey::info)
            .collect())
    }

    async fn start_registration(&self, p: &Principal) -> Result<WebauthnCeremony, KernelError> {
        let existing = self.store.list(&p.scope, &p.subject).await?;
        if existing.len() >= MAX_CREDENTIALS {
            return Err(KernelError::Conflict(format!(
                "at most {MAX_CREDENTIALS} passkeys can be registered"
            )));
        }
        let exclude: Vec<CredentialID> = existing
            .iter()
            .map(|c| c.passkey.cred_id().clone())
            .collect();
        let (ccr, state) = self
            .webauthn
            .start_passkey_registration(
                user_handle(&p.scope, &p.subject),
                &p.login_id,
                &p.display_name,
                (!exclude.is_empty()).then_some(exclude),
            )
            .map_err(|e| KernelError::internal_msg(format!("WebAuthn registration: {e}")))?;
        self.put_ceremony(
            PreAuthKind::WebauthnRegistration,
            p,
            serde_json::to_value(&state).map_err(KernelError::internal)?,
            serde_json::to_value(&ccr).map_err(KernelError::internal)?,
        )
        .await
    }

    async fn finish_registration(
        &self,
        p: &Principal,
        ceremony_id: &str,
        response: serde_json::Value,
        label: &str,
    ) -> Result<PasskeyInfo, KernelError> {
        let label = label_of(label)?;
        let state = self
            .take_ceremony(PreAuthKind::WebauthnRegistration, p, ceremony_id)
            .await?;
        let state: PasskeyRegistration =
            serde_json::from_value(state).map_err(KernelError::internal)?;
        let invalid = |why: String| {
            tracing::warn!(subject = %p.subject, reason = %why, "WebAuthn registration rejected");
            KernelError::Validation(vec![FieldViolation::new(
                "credential",
                "invalid",
                "the authenticator response could not be verified",
            )])
        };
        let reg: RegisterPublicKeyCredential =
            serde_json::from_value(response).map_err(|e| invalid(e.to_string()))?;
        let passkey = self
            .webauthn
            .finish_passkey_registration(&reg, &state)
            .map_err(|e| invalid(e.to_string()))?;
        Ok(self
            .store
            .insert(&p.scope, &p.subject, &label, &passkey)
            .await?
            .info())
    }

    async fn start_authentication(&self, p: &Principal) -> Result<WebauthnCeremony, KernelError> {
        let creds: Vec<Passkey> = self
            .store
            .list(&p.scope, &p.subject)
            .await?
            .into_iter()
            .map(|c| c.passkey)
            .collect();
        if creds.is_empty() {
            return Err(KernelError::Conflict("no passkey is registered".into()));
        }
        let (rcr, state) = self
            .webauthn
            .start_passkey_authentication(&creds)
            .map_err(|e| KernelError::internal_msg(format!("WebAuthn authentication: {e}")))?;
        self.put_ceremony(
            PreAuthKind::WebauthnAuthentication,
            p,
            serde_json::to_value(&state).map_err(KernelError::internal)?,
            serde_json::to_value(&rcr).map_err(KernelError::internal)?,
        )
        .await
    }

    async fn finish_authentication(
        &self,
        p: &Principal,
        ceremony_id: &str,
        response: serde_json::Value,
    ) -> Result<(), KernelError> {
        let state = self
            .take_ceremony(PreAuthKind::WebauthnAuthentication, p, ceremony_id)
            .await?;
        let state: PasskeyAuthentication =
            serde_json::from_value(state).map_err(KernelError::internal)?;
        let reject = |why: String| {
            tracing::warn!(subject = %p.subject, reason = %why, "WebAuthn assertion rejected");
            KernelError::Unauthenticated
        };
        let cred: PublicKeyCredential =
            serde_json::from_value(response).map_err(|e| reject(e.to_string()))?;
        let result = self
            .webauthn
            .finish_passkey_authentication(&cred, &state)
            .map_err(|e| reject(e.to_string()))?;
        let stored = self
            .store
            .list(&p.scope, &p.subject)
            .await?
            .into_iter()
            .find(|c| c.passkey.cred_id() == result.cred_id())
            .ok_or_else(|| reject("credential no longer registered".into()))?;
        let mut updated = stored.passkey.clone();
        updated.update_credential(&result);
        if !self
            .store
            .record_use(stored.id, &updated, result.counter())
            .await?
        {
            return Err(reject(format!(
                "sign counter {} did not advance past {} (replay or cloned authenticator)",
                result.counter(),
                stored.sign_count
            )));
        }
        Ok(())
    }

    async fn remove(&self, p: &Principal, id: Uuid) -> Result<bool, KernelError> {
        self.store.remove(&p.scope, &p.subject, id).await
    }
}

/// A minimal software authenticator (ES256, `none` attestation, UV) for tests of the WebAuthn
/// ceremonies — it produces exactly what a browser hands back from
/// `navigator.credentials.create/get`.
#[cfg(test)]
pub(crate) mod soft {
    use base64::Engine;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_ASN1_SIGNING};
    use sutra_kernel::secret;

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b)
    }

    // --- tiny CBOR encoder (just what attestation objects and COSE keys need) ---
    fn head(major: u8, n: u64, out: &mut Vec<u8>) {
        let m = major << 5;
        match n {
            0..=23 => out.push(m | n as u8),
            24..=0xff => out.extend([m | 24, n as u8]),
            0x100..=0xffff => {
                out.push(m | 25);
                out.extend((n as u16).to_be_bytes());
            }
            _ => {
                out.push(m | 26);
                out.extend((n as u32).to_be_bytes());
            }
        }
    }
    fn int(i: i64, out: &mut Vec<u8>) {
        if i >= 0 {
            head(0, i as u64, out)
        } else {
            head(1, (-1 - i) as u64, out)
        }
    }
    fn bytes(b: &[u8], out: &mut Vec<u8>) {
        head(2, b.len() as u64, out);
        out.extend_from_slice(b);
    }
    fn text(s: &str, out: &mut Vec<u8>) {
        head(3, s.len() as u64, out);
        out.extend_from_slice(s.as_bytes());
    }

    pub struct SoftAuthenticator {
        key: EcdsaKeyPair,
        pub credential_id: Vec<u8>,
        pub counter: u32,
        pub rp_id: String,
        pub origin: String,
    }

    impl SoftAuthenticator {
        pub fn new(rp_id: &str, origin: &str) -> Self {
            let rng = SystemRandom::new();
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng)
                .expect("keygen");
            let key =
                EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
                    .expect("key");
            Self {
                key,
                credential_id: secret::random_bytes().expect("rng").to_vec(),
                counter: 0,
                rp_id: rp_id.into(),
                origin: origin.into(),
            }
        }

        fn cose_key(&self) -> Vec<u8> {
            let pk = self.key.public_key().as_ref(); // 0x04 ‖ x ‖ y
            let mut out = vec![];
            head(5, 5, &mut out);
            int(1, &mut out);
            int(2, &mut out); // kty: EC2
            int(3, &mut out);
            int(-7, &mut out); // alg: ES256
            int(-1, &mut out);
            int(1, &mut out); // crv: P-256
            int(-2, &mut out);
            bytes(&pk[1..33], &mut out);
            int(-3, &mut out);
            bytes(&pk[33..65], &mut out);
            out
        }

        fn auth_data(&self, attested: bool) -> Vec<u8> {
            let mut d = secret::sha256(self.rp_id.as_bytes()).to_vec();
            // UP | UV (| AT)
            d.push(if attested { 0x45 } else { 0x05 });
            d.extend(self.counter.to_be_bytes());
            if attested {
                d.extend([0u8; 16]); // AAGUID
                d.extend((self.credential_id.len() as u16).to_be_bytes());
                d.extend_from_slice(&self.credential_id);
                d.extend(self.cose_key());
            }
            d
        }

        fn client_data(&self, kind: &str, options: &serde_json::Value) -> Vec<u8> {
            serde_json::json!({
                "type": kind,
                "challenge": options["publicKey"]["challenge"],
                "origin": self.origin,
                "crossOrigin": false,
            })
            .to_string()
            .into_bytes()
        }

        /// Response to `navigator.credentials.create(options)`.
        pub fn create(&mut self, options: &serde_json::Value) -> serde_json::Value {
            let cdj = self.client_data("webauthn.create", options);
            let mut att = vec![];
            head(5, 3, &mut att);
            text("fmt", &mut att);
            text("none", &mut att);
            text("attStmt", &mut att);
            head(5, 0, &mut att);
            text("authData", &mut att);
            bytes(&self.auth_data(true), &mut att);
            serde_json::json!({
                "id": b64(&self.credential_id),
                "rawId": b64(&self.credential_id),
                "type": "public-key",
                "extensions": {},
                "response": {
                    "attestationObject": b64(&att),
                    "clientDataJSON": b64(&cdj),
                    "transports": ["internal"],
                },
            })
        }

        /// Response to `navigator.credentials.get(options)`; `bump` advances the counter first.
        pub fn get(&mut self, options: &serde_json::Value, bump: bool) -> serde_json::Value {
            if bump {
                self.counter += 1;
            }
            let cdj = self.client_data("webauthn.get", options);
            let ad = self.auth_data(false);
            let mut msg = ad.clone();
            msg.extend(secret::sha256(&cdj));
            let sig = self
                .key
                .sign(&SystemRandom::new(), &msg)
                .expect("sign")
                .as_ref()
                .to_vec();
            serde_json::json!({
                "id": b64(&self.credential_id),
                "rawId": b64(&self.credential_id),
                "type": "public-key",
                "extensions": {},
                "response": {
                    "authenticatorData": b64(&ad),
                    "clientDataJSON": b64(&cdj),
                    "signature": b64(&sig),
                    "userHandle": null,
                },
            })
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::soft::SoftAuthenticator;
    use super::*;
    use crate::auth::preauth::InMemoryPreAuthStore;
    use sutra_contracts::platform::auth::Role;
    use sutra_kernel::scope::TenantId;

    pub const ORIGIN: &str = "http://localhost:5173";

    fn principal() -> Principal {
        Principal {
            subject: format!("dev|{}", Uuid::now_v7()),
            display_name: "Dhruv Naik".into(),
            login_id: "admin@sutra.dev".into(),
            roles: vec![Role::Administrator],
            scope: ExecutionScope::tenant(TenantId::new()),
        }
    }

    fn rp() -> DevWebauthn {
        DevWebauthn::new(
            &WebauthnSettings {
                rp_id: "localhost".into(),
                rp_name: "SutraERP".into(),
                origins: vec![ORIGIN.into(), "http://localhost:3000".into()],
            },
            Arc::new(InMemoryWebauthnStore::default()),
            Arc::new(InMemoryPreAuthStore::default()),
        )
        .unwrap()
    }

    async fn registered(rp: &DevWebauthn, p: &Principal) -> SoftAuthenticator {
        let mut auth = SoftAuthenticator::new("localhost", ORIGIN);
        let c = rp.start_registration(p).await.unwrap();
        assert_eq!(c.options["publicKey"]["rp"]["id"], "localhost");
        let info = rp
            .finish_registration(p, &c.ceremony_id, auth.create(&c.options), "  YubiKey  ")
            .await
            .unwrap();
        assert_eq!(info.label, "YubiKey");
        auth
    }

    #[tokio::test]
    async fn registration_and_assertion_round_trip_with_counter_replay_protection() {
        let rp = rp();
        let p = principal();
        let mut auth = registered(&rp, &p).await;
        assert_eq!(rp.credentials(&p).await.unwrap().len(), 1);

        let c = rp.start_authentication(&p).await.unwrap();
        let first = auth.get(&c.options, true);
        rp.finish_authentication(&p, &c.ceremony_id, first.clone())
            .await
            .unwrap();
        assert!(rp.credentials(&p).await.unwrap()[0].last_used_at.is_some());

        // The ceremony is single use: replaying the same response fails.
        assert!(matches!(
            rp.finish_authentication(&p, &c.ceremony_id, first).await,
            Err(KernelError::Unauthenticated)
        ));
        // A fresh challenge answered with a counter that did not advance (cloned key) fails.
        let c2 = rp.start_authentication(&p).await.unwrap();
        let stale = auth.get(&c2.options, false);
        assert!(rp
            .finish_authentication(&p, &c2.ceremony_id, stale)
            .await
            .is_err());
        // …and an advancing counter works again.
        let c3 = rp.start_authentication(&p).await.unwrap();
        let ok = auth.get(&c3.options, true);
        rp.finish_authentication(&p, &c3.ceremony_id, ok)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn ceremony_state_is_bound_to_kind_account_and_challenge() {
        let rp = rp();
        let p = principal();
        let mut auth = registered(&rp, &p).await;

        // Another account cannot finish p's ceremony (and burns it).
        let c = rp.start_authentication(&p).await.unwrap();
        let resp = auth.get(&c.options, true);
        let mallory = principal();
        assert!(matches!(
            rp.finish_authentication(&mallory, &c.ceremony_id, resp.clone())
                .await,
            Err(KernelError::Unauthenticated)
        ));
        assert!(rp
            .finish_authentication(&p, &c.ceremony_id, resp)
            .await
            .is_err());

        // A registration ceremony id cannot be used as an authentication ceremony.
        let reg = rp.start_registration(&p).await.unwrap();
        let resp = auth.get(&reg.options, true);
        assert!(rp
            .finish_authentication(&p, &reg.ceremony_id, resp)
            .await
            .is_err());

        // A response signed over another challenge is refused.
        let c1 = rp.start_authentication(&p).await.unwrap();
        let c2 = rp.start_authentication(&p).await.unwrap();
        let for_c2 = auth.get(&c2.options, true);
        assert!(rp
            .finish_authentication(&p, &c1.ceremony_id, for_c2)
            .await
            .is_err());

        // Unknown ceremony ids and garbage responses fail closed.
        assert!(rp
            .finish_authentication(&p, "nope", serde_json::json!({}))
            .await
            .is_err());
        let c = rp.start_registration(&p).await.unwrap();
        assert!(matches!(
            rp.finish_registration(&p, &c.ceremony_id, serde_json::json!({"x": 1}), "k")
                .await,
            Err(KernelError::Validation(_))
        ));
    }

    #[tokio::test]
    async fn registration_rejects_wrong_origin_and_duplicates_and_needs_credentials_to_assert() {
        let rp = rp();
        let p = principal();
        assert!(matches!(
            rp.start_authentication(&p).await,
            Err(KernelError::Conflict(_))
        ));
        let mut evil = SoftAuthenticator::new("localhost", "http://evil.example");
        let c = rp.start_registration(&p).await.unwrap();
        assert!(rp
            .finish_registration(&p, &c.ceremony_id, evil.create(&c.options), "x")
            .await
            .is_err());
        // Same authenticator twice: the second start excludes it; registering it anyway conflicts.
        let mut auth = registered(&rp, &p).await;
        let c = rp.start_registration(&p).await.unwrap();
        assert_eq!(
            c.options["publicKey"]["excludeCredentials"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        assert!(rp
            .finish_registration(&p, &c.ceremony_id, auth.create(&c.options), "again")
            .await
            .is_err());
        let id = rp.credentials(&p).await.unwrap()[0].id;
        assert!(!rp.remove(&principal(), id).await.unwrap(), "not theirs");
        assert!(rp.remove(&p, id).await.unwrap());
        assert!(rp.credentials(&p).await.unwrap().is_empty());
    }

    /// Live PostgreSQL 18: `PgWebauthnStore` + `PgPreAuthStore` behind the same relying party.
    #[cfg(feature = "db-tests")]
    mod pg {
        use super::*;
        use crate::auth::preauth::PgPreAuthStore;

        async fn pool() -> PgPool {
            let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for db-tests");
            let pool =
                sutra_persistence::pool::connect_lazy(&sutra_persistence::pool::PoolSettings {
                    url,
                    max_connections: 4,
                    min_connections: 0,
                    acquire_timeout: std::time::Duration::from_secs(10),
                    application_name: "sutra-platform-webauthn-db-tests".into(),
                })
                .expect("pool");
            sutra_persistence::migrate::run(&pool)
                .await
                .expect("migrations");
            pool
        }

        fn pg_rp(pool: &PgPool) -> DevWebauthn {
            DevWebauthn::new(
                &WebauthnSettings {
                    rp_id: "localhost".into(),
                    rp_name: "SutraERP".into(),
                    origins: vec![ORIGIN.into()],
                },
                Arc::new(PgWebauthnStore::new(pool.clone())),
                Arc::new(PgPreAuthStore::new(pool.clone())),
            )
            .unwrap()
        }

        #[tokio::test]
        async fn pg_store_round_trip_counter_scope_and_uniqueness() {
            let pool = pool().await;
            let rp = pg_rp(&pool);
            let p = principal();
            let mut auth = registered(&rp, &p).await;

            // Stored: credential id bytes, label, serialised passkey, counter 0, scoped row.
            let row: (Vec<u8>, String, i64, uuid::Uuid, String) = sqlx::query_as(
                "SELECT credential_id, label, sign_count, tenant_id, subject \
                 FROM platform.webauthn_credential WHERE subject = $1",
            )
            .bind(&p.subject)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(row.0, auth.credential_id);
            assert_eq!(row.1, "YubiKey");
            assert_eq!(row.2, 0);
            assert_eq!(row.3, p.scope.tenant_id.as_uuid());

            // Assertion through the database: counter advances atomically, last_used_at set.
            let c = rp.start_authentication(&p).await.unwrap();
            let ok = auth.get(&c.options, true);
            rp.finish_authentication(&p, &c.ceremony_id, ok)
                .await
                .unwrap();
            let count: i64 = sqlx::query_scalar(
                "SELECT sign_count FROM platform.webauthn_credential WHERE subject = $1",
            )
            .bind(&p.subject)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(count, i64::from(auth.counter));
            assert!(rp.credentials(&p).await.unwrap()[0].last_used_at.is_some());

            // A non-advancing counter (cloned authenticator / replay) is refused by the UPDATE.
            let c = rp.start_authentication(&p).await.unwrap();
            let stale = auth.get(&c.options, false);
            assert!(rp
                .finish_authentication(&p, &c.ceremony_id, stale)
                .await
                .is_err());

            // Store level: record_use only moves forward; duplicate credential ids conflict
            // across tenants too (credential ids are globally unique).
            let store = PgWebauthnStore::new(pool.clone());
            let stored = store.list(&p.scope, &p.subject).await.unwrap();
            assert_eq!(stored.len(), 1);
            let s0 = &stored[0];
            assert!(!store
                .record_use(s0.id, &s0.passkey, u32::try_from(s0.sign_count).unwrap())
                .await
                .unwrap());
            assert!(store
                .record_use(
                    s0.id,
                    &s0.passkey,
                    u32::try_from(s0.sign_count + 5).unwrap()
                )
                .await
                .unwrap());
            let other = principal();
            assert!(matches!(
                store
                    .insert(&other.scope, &other.subject, "copy", &s0.passkey)
                    .await,
                Err(KernelError::Conflict(_))
            ));

            // Tenant / subject isolation of list and remove.
            assert!(store
                .list(&other.scope, &p.subject)
                .await
                .unwrap()
                .is_empty());
            assert!(!store.remove(&other.scope, &p.subject, s0.id).await.unwrap());
            assert!(!store.remove(&p.scope, &other.subject, s0.id).await.unwrap());
            assert!(store.remove(&p.scope, &p.subject, s0.id).await.unwrap());
            assert!(rp.credentials(&p).await.unwrap().is_empty());
        }

        #[tokio::test]
        async fn pg_ceremony_state_is_single_use() {
            let pool = pool().await;
            let rp = pg_rp(&pool);
            let p = principal();
            let mut auth = registered(&rp, &p).await;
            let c = rp.start_authentication(&p).await.unwrap();
            let resp = auth.get(&c.options, true);
            rp.finish_authentication(&p, &c.ceremony_id, resp.clone())
                .await
                .unwrap();
            assert!(matches!(
                rp.finish_authentication(&p, &c.ceremony_id, resp).await,
                Err(KernelError::Unauthenticated)
            ));
        }
    }

    #[test]
    fn user_handle_is_stable_and_scoped() {
        let s = ExecutionScope::tenant(TenantId::new());
        assert_eq!(user_handle(&s, "a"), user_handle(&s, "a"));
        assert_ne!(user_handle(&s, "a"), user_handle(&s, "b"));
        assert_ne!(
            user_handle(&s, "a"),
            user_handle(&ExecutionScope::tenant(TenantId::new()), "a")
        );
        assert!(label_of(&"x".repeat(65)).is_err());
        assert_eq!(label_of("").unwrap(), "Passkey");
        assert!(counter_advances(0, 0) && counter_advances(2, 1) && !counter_advances(1, 1));
    }
}
