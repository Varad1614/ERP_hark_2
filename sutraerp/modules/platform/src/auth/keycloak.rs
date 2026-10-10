//! Keycloak 26 OIDC adapter (ADR-L5-0024, ADR-L5-0038).
//!
//! SutraERP acts as a **backend-for-frontend**: the browser is redirected to Keycloak with an
//! Authorization Code + PKCE (`S256`) request whose `state`, `nonce` and code verifier are kept
//! server-side (`platform.pre_auth`); the callback exchanges the code over the back channel
//! (confidential client, `client_secret_basic`) and validates the ID token:
//!
//! * signature against the realm JWKS (cached for `jwks_ttl`, refreshed once on an unknown
//!   `kid`), asymmetric algorithms only;
//! * `iss` = configured issuer, `aud` ∋ client id (and `azp` = client id when present), `exp` /
//!   `nbf` with a small leeway, `nonce` = the one stored for this `state` (constant time).
//!
//! Claims are then mapped to a [`Principal`]: `sub` → subject, `preferred_username` (or `email`)
//! → login id, `name` → display name, realm roles → [`Role`] (unknown roles grant nothing; no
//! known role → `403`), the tenant / institution claims → session scope (missing or malformed
//! tenant → `403`, fail closed, ADR-L5-0004), `amr`/`acr` → [`MfaLevel`] and `sid` → IdP
//! session id for RP-initiated logout. Tokens from Keycloak are never stored or handed to the
//! browser; only the opaque SutraERP session cookie is.
//!
//! The OAuth Resource-Owner-Password grant is **not** used (ADR-L5-0038 §3.3): in Keycloak mode
//! `POST /api/v1/auth/login` answers `501` and MFA (TOTP / WebAuthn required actions) is
//! performed by Keycloak itself.

use base64::Engine;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{Algorithm, DecodingKey, Validation};
use std::time::{Duration, Instant};
use sutra_contracts::platform::auth::{
    AuthorizationParams, CredentialAuthenticator, Credentials, FederatedIdentity, MfaLevel,
    OidcIdentityProvider, Principal, Role,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_kernel::secret;
use tokio::sync::RwLock;

/// `amr` values that represent a second factor.
const MFA_AMR: [&str; 8] = ["mfa", "otp", "totp", "hwk", "swk", "webauthn", "fido", "sc"];

/// Configuration of the OIDC relying party (all from `SUTRA_OIDC_*`).
#[derive(Clone, Debug)]
pub struct KeycloakSettings {
    /// Issuer as it appears in tokens, e.g. `https://sso.example/realms/sutra`.
    pub issuer: String,
    /// Optional back-channel base replacing the issuer prefix for server-to-server calls
    /// (discovery, token, JWKS) — e.g. `http://keycloak:8180/realms/sutra` inside compose.
    pub backchannel_url: Option<String>,
    pub client_id: String,
    pub client_secret: String,
    /// Registered redirect URI of the callback (`…/api/v1/auth/oidc/callback`).
    pub redirect_uri: String,
    /// Where Keycloak sends the browser after RP-initiated logout.
    pub post_logout_redirect_uri: Option<String>,
    pub scopes: String,
    /// Claim carrying the tenant UUID (required).
    pub tenant_claim: String,
    /// Claim carrying the institution UUID (optional).
    pub institution_claim: String,
    /// `acr` values that count as multi-factor (Keycloak LoA mapping).
    pub mfa_acr_values: Vec<String>,
    /// `acr_values` requested on step-up (with `prompt=login`, `max_age=0`).
    pub step_up_acr: Option<String>,
    pub jwks_ttl: Duration,
    pub leeway_secs: u64,
    pub http_timeout: Duration,
    /// Honour `HTTP(S)_PROXY` / `NO_PROXY` for back-channel calls (default `true`).
    pub system_proxy: bool,
}

impl KeycloakSettings {
    /// Settings with the conventional defaults for the dev realm.
    pub fn new(issuer: &str, client_id: &str, client_secret: &str, redirect_uri: &str) -> Self {
        Self {
            issuer: issuer.trim_end_matches('/').to_owned(),
            backchannel_url: None,
            client_id: client_id.to_owned(),
            client_secret: client_secret.to_owned(),
            redirect_uri: redirect_uri.to_owned(),
            post_logout_redirect_uri: None,
            scopes: "openid profile email".into(),
            tenant_claim: "tenant_id".into(),
            institution_claim: "institution_id".into(),
            mfa_acr_values: vec!["2".into(), "mfa".into()],
            step_up_acr: Some("mfa".into()),
            jwks_ttl: Duration::from_secs(300),
            leeway_secs: 30,
            http_timeout: Duration::from_secs(5),
            system_proxy: true,
        }
    }

    /// Rewrites an IdP URL onto the back-channel base when one is configured.
    fn backchannel(&self, url: &str) -> String {
        match &self.backchannel_url {
            Some(base) if url.starts_with(&self.issuer) => {
                format!(
                    "{}{}",
                    base.trim_end_matches('/'),
                    &url[self.issuer.len()..]
                )
            }
            _ => url.to_owned(),
        }
    }
}

#[derive(Clone, Debug, serde::Deserialize)]
struct Discovery {
    issuer: String,
    authorization_endpoint: String,
    token_endpoint: String,
    jwks_uri: String,
    #[serde(default)]
    end_session_endpoint: Option<String>,
}

/// Why an ID token was refused (internal; the HTTP surface only ever says `401`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed token: {0}")]
    Malformed(String),
    #[error("algorithm not allowed: {0:?}")]
    AlgorithmNotAllowed(Algorithm),
    #[error("no JWKS key for kid {0:?}")]
    UnknownKey(Option<String>),
    #[error("invalid signature")]
    BadSignature,
    #[error("token expired")]
    Expired,
    #[error("token not yet valid")]
    NotYetValid,
    #[error("wrong issuer")]
    WrongIssuer,
    #[error("wrong audience")]
    WrongAudience,
    #[error("nonce mismatch")]
    NonceMismatch,
    #[error("invalid claims: {0}")]
    Claims(String),
    #[error("identity provider unavailable: {0}")]
    Unavailable(String),
}

impl From<TokenError> for KernelError {
    fn from(e: TokenError) -> Self {
        match e {
            TokenError::Unavailable(m) => KernelError::Unavailable(m),
            other => {
                tracing::warn!(reason = %other, "ID token rejected");
                KernelError::Unauthenticated
            }
        }
    }
}

/// The subset of ID-token claims SutraERP reads.
#[derive(Clone, Debug, serde::Deserialize)]
pub struct IdTokenClaims {
    pub sub: String,
    #[serde(default)]
    pub nonce: Option<String>,
    #[serde(default)]
    pub azp: Option<String>,
    #[serde(default)]
    pub preferred_username: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub given_name: Option<String>,
    #[serde(default)]
    pub family_name: Option<String>,
    #[serde(default)]
    pub realm_access: Option<RealmAccess>,
    #[serde(default)]
    pub sid: Option<String>,
    /// String or array (RFC 8176).
    #[serde(default)]
    pub amr: Option<serde_json::Value>,
    #[serde(default)]
    pub acr: Option<String>,
    /// Remaining claims (tenant / institution mappers).
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
pub struct RealmAccess {
    #[serde(default)]
    pub roles: Vec<String>,
}

/// Maps validated claims to a SutraERP identity (pure; unit-tested).
pub fn map_claims(
    c: &IdTokenClaims,
    s: &KeycloakSettings,
) -> Result<FederatedIdentity, KernelError> {
    let roles: Vec<Role> = c
        .realm_access
        .as_ref()
        .map(|r| r.roles.iter().filter_map(|n| Role::parse(n)).collect())
        .unwrap_or_default();
    let mut roles_dedup: Vec<Role> = Vec::with_capacity(roles.len());
    for r in roles {
        if !roles_dedup.contains(&r) {
            roles_dedup.push(r);
        }
    }
    if roles_dedup.is_empty() {
        return Err(KernelError::Forbidden(
            "the identity holds no SutraERP role".into(),
        ));
    }
    let uuid_claim = |name: &str| -> Result<Option<uuid::Uuid>, KernelError> {
        match c.extra.get(name) {
            None | Some(serde_json::Value::Null) => Ok(None),
            Some(serde_json::Value::String(v)) => uuid::Uuid::parse_str(v.trim())
                .map(Some)
                .map_err(|_| KernelError::Forbidden(format!("claim `{name}` is not a UUID"))),
            Some(_) => Err(KernelError::Forbidden(format!(
                "claim `{name}` must be a single UUID string"
            ))),
        }
    };
    let tenant = uuid_claim(&s.tenant_claim)?.ok_or_else(|| {
        KernelError::Forbidden(format!(
            "the identity carries no `{}` claim (no default tenant)",
            s.tenant_claim
        ))
    })?;
    let scope = ExecutionScope {
        tenant_id: TenantId(tenant),
        institution_id: uuid_claim(&s.institution_claim)?.map(InstitutionId),
    };
    let login_id = c
        .preferred_username
        .clone()
        .or_else(|| c.email.clone())
        .unwrap_or_else(|| c.sub.clone());
    let display_name = c
        .name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .or_else(|| {
            let full = format!(
                "{} {}",
                c.given_name.as_deref().unwrap_or(""),
                c.family_name.as_deref().unwrap_or("")
            );
            let full = full.trim().to_owned();
            (!full.is_empty()).then_some(full)
        })
        .unwrap_or_else(|| login_id.clone());
    let amr: Vec<String> = match &c.amr {
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        Some(serde_json::Value::String(v)) => vec![v.clone()],
        _ => vec![],
    };
    let mfa_by_amr = amr.iter().any(|m| MFA_AMR.contains(&m.as_str()));
    let mfa_by_acr = c
        .acr
        .as_ref()
        .is_some_and(|a| s.mfa_acr_values.iter().any(|v| v == a));
    Ok(FederatedIdentity {
        principal: Principal {
            subject: c.sub.clone(),
            display_name,
            login_id,
            roles: roles_dedup,
            scope,
        },
        mfa_level: if mfa_by_amr || mfa_by_acr {
            MfaLevel::Mfa
        } else {
            MfaLevel::None
        },
        amr,
        idp_sid: c.sid.clone(),
    })
}

/// `BASE64URL(SHA-256(verifier))` — PKCE `S256` challenge.
pub fn pkce_challenge(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret::sha256(verifier.as_bytes()))
}

/// Fresh 256-bit PKCE code verifier (43 chars, unreserved alphabet).
pub fn pkce_verifier() -> Result<String, KernelError> {
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(secret::random_bytes()?))
}

const ALLOWED_ALGS: [Algorithm; 8] = [
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::PS256,
    Algorithm::PS384,
    Algorithm::PS512,
    Algorithm::ES256,
    Algorithm::ES384,
];

/// Minimum spacing between JWKS refreshes triggered by unknown `kid`s.
const JWKS_MIN_REFRESH: Duration = Duration::from_secs(10);

pub struct KeycloakAuthenticator {
    settings: KeycloakSettings,
    http: reqwest::Client,
    discovery: RwLock<Option<Discovery>>,
    jwks: RwLock<Option<(JwkSet, Instant)>>,
}

impl KeycloakAuthenticator {
    pub fn new(settings: KeycloakSettings) -> Result<Self, KernelError> {
        let mut builder = reqwest::Client::builder()
            .timeout(settings.http_timeout)
            .redirect(reqwest::redirect::Policy::none());
        if !settings.system_proxy {
            builder = builder.no_proxy();
        }
        let http = builder.build().map_err(KernelError::internal)?;
        Ok(Self {
            settings,
            http,
            discovery: RwLock::new(None),
            jwks: RwLock::new(None),
        })
    }

    pub fn settings(&self) -> &KeycloakSettings {
        &self.settings
    }

    async fn discovery(&self) -> Result<Discovery, TokenError> {
        if let Some(d) = self.discovery.read().await.clone() {
            return Ok(d);
        }
        let url = self.settings.backchannel(&format!(
            "{}/.well-known/openid-configuration",
            self.settings.issuer
        ));
        let d: Discovery = self
            .http
            .get(&url)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| TokenError::Unavailable(format!("OIDC discovery: {e}")))?
            .json()
            .await
            .map_err(|e| TokenError::Unavailable(format!("OIDC discovery document: {e}")))?;
        if d.issuer.trim_end_matches('/') != self.settings.issuer {
            return Err(TokenError::Unavailable(format!(
                "discovery issuer {} does not match configured issuer",
                d.issuer
            )));
        }
        *self.discovery.write().await = Some(d.clone());
        Ok(d)
    }

    async fn fetch_jwks(&self) -> Result<JwkSet, TokenError> {
        let d = self.discovery().await?;
        let set: JwkSet = self
            .http
            .get(self.settings.backchannel(&d.jwks_uri))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| TokenError::Unavailable(format!("JWKS: {e}")))?
            .json()
            .await
            .map_err(|e| TokenError::Unavailable(format!("JWKS document: {e}")))?;
        *self.jwks.write().await = Some((set.clone(), Instant::now()));
        Ok(set)
    }

    /// JWKS from cache (within TTL) or the IdP; `force` refetches unless the cache is younger
    /// than [`JWKS_MIN_REFRESH`] (bounded refresh on unknown `kid`).
    async fn jwks(&self, force: bool) -> Result<JwkSet, TokenError> {
        if let Some((set, at)) = self.jwks.read().await.clone() {
            let age = at.elapsed();
            if (!force && age < self.settings.jwks_ttl) || (force && age < JWKS_MIN_REFRESH) {
                return Ok(set);
            }
        }
        self.fetch_jwks().await
    }

    /// Validates an ID token (signature, `iss`, `aud`, `azp`, `exp`/`nbf`, `nonce`).
    pub async fn validate_id_token(
        &self,
        token: &str,
        expected_nonce: &str,
    ) -> Result<IdTokenClaims, TokenError> {
        let header =
            jsonwebtoken::decode_header(token).map_err(|e| TokenError::Malformed(e.to_string()))?;
        if !ALLOWED_ALGS.contains(&header.alg) {
            return Err(TokenError::AlgorithmNotAllowed(header.alg));
        }
        let find = |set: &JwkSet| match &header.kid {
            Some(kid) => set.find(kid).cloned(),
            None if set.keys.len() == 1 => set.keys.first().cloned(),
            None => None,
        };
        let jwk = match find(&self.jwks(false).await?) {
            Some(k) => k,
            None => find(&self.jwks(true).await?)
                .ok_or_else(|| TokenError::UnknownKey(header.kid.clone()))?,
        };
        let key = DecodingKey::from_jwk(&jwk).map_err(|e| TokenError::Malformed(e.to_string()))?;
        let mut v = Validation::new(header.alg);
        v.set_issuer(&[&self.settings.issuer]);
        v.set_audience(&[&self.settings.client_id]);
        v.leeway = self.settings.leeway_secs;
        v.validate_nbf = true;
        v.set_required_spec_claims(&["exp", "iat", "iss", "aud", "sub"]);
        let data = jsonwebtoken::decode::<IdTokenClaims>(token, &key, &v).map_err(|e| {
            use jsonwebtoken::errors::ErrorKind as K;
            match e.kind() {
                K::InvalidSignature => TokenError::BadSignature,
                K::ExpiredSignature => TokenError::Expired,
                K::ImmatureSignature => TokenError::NotYetValid,
                K::InvalidIssuer => TokenError::WrongIssuer,
                K::InvalidAudience => TokenError::WrongAudience,
                K::InvalidAlgorithm => TokenError::AlgorithmNotAllowed(header.alg),
                _ => TokenError::Claims(e.to_string()),
            }
        })?;
        let claims = data.claims;
        if let Some(azp) = &claims.azp {
            if azp != &self.settings.client_id {
                return Err(TokenError::WrongAudience);
            }
        }
        match &claims.nonce {
            Some(n) if secret::ct_eq(n.as_bytes(), expected_nonce.as_bytes()) => {}
            _ => return Err(TokenError::NonceMismatch),
        }
        Ok(claims)
    }
}

#[derive(serde::Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

#[async_trait::async_trait]
impl OidcIdentityProvider for KeycloakAuthenticator {
    async fn authorization_url(&self, p: &AuthorizationParams) -> Result<String, KernelError> {
        let d = self.discovery().await?;
        let s = &self.settings;
        let mut params: Vec<(&str, &str)> = vec![
            ("response_type", "code"),
            ("client_id", &s.client_id),
            ("redirect_uri", &s.redirect_uri),
            ("scope", &s.scopes),
            ("state", &p.state),
            ("nonce", &p.nonce),
            ("code_challenge", &p.code_challenge),
            ("code_challenge_method", "S256"),
        ];
        if p.step_up {
            params.push(("prompt", "login"));
            params.push(("max_age", "0"));
            if let Some(acr) = &s.step_up_acr {
                params.push(("acr_values", acr));
            }
        }
        let url = reqwest::Url::parse_with_params(&d.authorization_endpoint, &params)
            .map_err(|e| KernelError::Unavailable(format!("authorization endpoint: {e}")))?;
        Ok(url.into())
    }

    async fn exchange(
        &self,
        code: &str,
        code_verifier: &str,
        expected_nonce: &str,
    ) -> Result<FederatedIdentity, KernelError> {
        let d = self.discovery().await?;
        let s = &self.settings;
        let res = self
            .http
            .post(s.backchannel(&d.token_endpoint))
            .basic_auth(&s.client_id, Some(&s.client_secret))
            .form(&[
                ("grant_type", "authorization_code"),
                ("code", code),
                ("redirect_uri", &s.redirect_uri),
                ("code_verifier", code_verifier),
                ("client_id", &s.client_id),
            ])
            .send()
            .await
            .map_err(|e| KernelError::Unavailable(format!("token endpoint: {e}")))?;
        if !res.status().is_success() {
            // invalid_grant (reused/expired code, PKCE mismatch) etc.
            tracing::warn!(status = %res.status(), "OIDC code exchange refused");
            return Err(KernelError::Unauthenticated);
        }
        let body: TokenResponse = res
            .json()
            .await
            .map_err(|e| KernelError::Unavailable(format!("token response: {e}")))?;
        let id_token = body.id_token.ok_or_else(|| {
            tracing::warn!("token response carries no id_token");
            KernelError::Unauthenticated
        })?;
        let claims = self.validate_id_token(&id_token, expected_nonce).await?;
        map_claims(&claims, s)
    }

    async fn end_session_url(&self, _idp_sid: Option<&str>) -> Result<Option<String>, KernelError> {
        let d = self.discovery().await?;
        let Some(endpoint) = d.end_session_endpoint else {
            return Ok(None);
        };
        let s = &self.settings;
        let mut params: Vec<(&str, &str)> = vec![("client_id", &s.client_id)];
        if let Some(uri) = &s.post_logout_redirect_uri {
            params.push(("post_logout_redirect_uri", uri));
        }
        let url = reqwest::Url::parse_with_params(&endpoint, &params)
            .map_err(|e| KernelError::Unavailable(format!("end-session endpoint: {e}")))?;
        Ok(Some(url.into()))
    }
}

#[async_trait::async_trait]
impl CredentialAuthenticator for KeycloakAuthenticator {
    async fn authenticate(&self, _credentials: Credentials) -> Result<Principal, KernelError> {
        // No ROPC: passwords are only ever typed into Keycloak.
        Err(KernelError::NotImplemented(
            "password sign-in is not offered in Keycloak mode; use GET /api/v1/auth/oidc/start",
        ))
    }
}

/// Test identity provider: an in-process Axum server speaking just enough OIDC (discovery,
/// JWKS, token endpoint) plus helpers to mint ID tokens with the fixture RSA keys.
#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
pub(crate) mod test_idp {
    use super::*;
    use axum::extract::State;
    use axum::routing::{get, post};
    use axum::{Form, Json, Router};
    use jsonwebtoken::{EncodingKey, Header};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    pub const KEY_A: &str = include_str!("../../tests/fixtures/oidc-test-key-a.pem");
    pub const KEY_B: &str = include_str!("../../tests/fixtures/oidc-test-key-b.pem");
    pub const JWKS_A: &str = include_str!("../../tests/fixtures/oidc-test-jwks.json");
    pub const JWK_B: &str = include_str!("../../tests/fixtures/oidc-test-jwk-b.json");
    pub const CLIENT_ID: &str = "sutra-web";
    pub const TENANT: &str = "0192f5a0-7c3e-7000-8000-000000000001";
    pub const INSTITUTION: &str = "0192f5a0-7c3e-7000-8000-0000000000aa";

    #[derive(Default)]
    pub struct IdpState {
        pub issuer: Mutex<String>,
        pub jwks: Mutex<serde_json::Value>,
        /// ID token the token endpoint returns next.
        pub id_token: Mutex<Option<String>>,
        /// Last form posted to the token endpoint.
        pub last_form: Mutex<HashMap<String, String>>,
        pub last_auth: Mutex<Option<String>>,
        pub jwks_fetches: AtomicUsize,
        pub token_status: Mutex<u16>,
    }

    pub struct TestIdp {
        pub state: Arc<IdpState>,
        pub issuer: String,
    }

    impl TestIdp {
        pub async fn start() -> Self {
            let state = Arc::new(IdpState::default());
            *state.jwks.lock().unwrap() = serde_json::from_str(JWKS_A).unwrap();
            *state.token_status.lock().unwrap() = 200;
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let issuer = format!("http://{}/realms/test", listener.local_addr().unwrap());
            *state.issuer.lock().unwrap() = issuer.clone();
            let app = Router::new()
                .route(
                    "/realms/test/.well-known/openid-configuration",
                    get(|State(s): State<Arc<IdpState>>| async move {
                        let i = s.issuer.lock().unwrap().clone();
                        Json(serde_json::json!({
                            "issuer": i,
                            "authorization_endpoint": format!("{i}/protocol/openid-connect/auth"),
                            "token_endpoint": format!("{i}/protocol/openid-connect/token"),
                            "jwks_uri": format!("{i}/protocol/openid-connect/certs"),
                            "end_session_endpoint": format!("{i}/protocol/openid-connect/logout"),
                        }))
                    }),
                )
                .route(
                    "/realms/test/protocol/openid-connect/certs",
                    get(|State(s): State<Arc<IdpState>>| async move {
                        s.jwks_fetches.fetch_add(1, Ordering::SeqCst);
                        Json(s.jwks.lock().unwrap().clone())
                    }),
                )
                .route(
                    "/realms/test/protocol/openid-connect/token",
                    post(
                        |State(s): State<Arc<IdpState>>,
                         headers: axum::http::HeaderMap,
                         Form(f): Form<HashMap<String, String>>| async move {
                            *s.last_form.lock().unwrap() = f;
                            *s.last_auth.lock().unwrap() = headers
                                .get("authorization")
                                .and_then(|v| v.to_str().ok())
                                .map(str::to_owned);
                            let status = *s.token_status.lock().unwrap();
                            let body = serde_json::json!({
                                "access_token": "opaque-at",
                                "token_type": "Bearer",
                                "id_token": s.id_token.lock().unwrap().clone(),
                            });
                            (
                                axum::http::StatusCode::from_u16(status).unwrap(),
                                Json(body),
                            )
                        },
                    ),
                )
                .with_state(state.clone());
            tokio::spawn(async move {
                axum::serve(listener, app).await.unwrap();
            });
            Self { state, issuer }
        }

        pub fn settings(&self) -> KeycloakSettings {
            let mut s = KeycloakSettings::new(
                &self.issuer,
                CLIENT_ID,
                "test-secret",
                "http://localhost/api/v1/auth/oidc/callback",
            );
            s.post_logout_redirect_uri = Some("http://localhost/sign-in".into());
            s.system_proxy = false; // hermetic: the IdP is on loopback
            s
        }

        pub fn adapter(&self) -> KeycloakAuthenticator {
            KeycloakAuthenticator::new(self.settings()).unwrap()
        }

        /// Standard valid claims for `nonce`.
        pub fn claims(&self, nonce: &str) -> serde_json::Value {
            let now = chrono::Utc::now().timestamp();
            serde_json::json!({
                "iss": self.issuer,
                "aud": CLIENT_ID,
                "azp": CLIENT_ID,
                "sub": "f3b9c1de-0000-4000-8000-00000000a001",
                "iat": now,
                "exp": now + 300,
                "nonce": nonce,
                "sid": "kc-session-1",
                "preferred_username": "admin@sutra.dev",
                "email": "admin@sutra.dev",
                "name": "Dhruv Naik",
                "realm_access": {"roles": ["administrator", "offline_access", "uma_authorization"]},
                "tenant_id": TENANT,
                "institution_id": INSTITUTION,
                "amr": ["pwd", "otp"],
                "acr": "2",
            })
        }

        pub fn sign_with(claims: &serde_json::Value, pem: &str, kid: Option<&str>) -> String {
            let mut h = Header::new(Algorithm::RS256);
            h.kid = kid.map(str::to_owned);
            jsonwebtoken::encode(
                &h,
                claims,
                &EncodingKey::from_rsa_pem(pem.as_bytes()).unwrap(),
            )
            .unwrap()
        }

        pub fn sign(claims: &serde_json::Value) -> String {
            Self::sign_with(claims, KEY_A, Some("test-key-a"))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::test_idp::*;
    use super::*;
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn valid_token_maps_roles_scope_mfa_and_sid() {
        let idp = TestIdp::start().await;
        let a = idp.adapter();
        let claims = a
            .validate_id_token(&TestIdp::sign(&idp.claims("n-1")), "n-1")
            .await
            .unwrap();
        let id = map_claims(&claims, a.settings()).unwrap();
        assert_eq!(id.principal.roles, vec![Role::Administrator]);
        assert_eq!(id.principal.subject, "f3b9c1de-0000-4000-8000-00000000a001");
        assert_eq!(id.principal.login_id, "admin@sutra.dev");
        assert_eq!(id.principal.display_name, "Dhruv Naik");
        assert_eq!(id.principal.scope.tenant_id.to_string(), TENANT);
        assert_eq!(
            id.principal
                .scope
                .institution_id
                .map(|i| i.to_string())
                .as_deref(),
            Some(INSTITUTION)
        );
        assert_eq!(id.mfa_level, MfaLevel::Mfa);
        assert_eq!(id.idp_sid.as_deref(), Some("kc-session-1"));
    }

    #[tokio::test]
    async fn rejects_expired_wrong_aud_wrong_iss_bad_nonce() {
        let idp = TestIdp::start().await;
        let a = idp.adapter();
        let now = chrono::Utc::now().timestamp();
        let mut expired = idp.claims("n");
        expired["exp"] = (now - 120).into();
        expired["iat"] = (now - 600).into();
        assert_eq!(
            a.validate_id_token(&TestIdp::sign(&expired), "n")
                .await
                .unwrap_err(),
            TokenError::Expired
        );
        let mut aud = idp.claims("n");
        aud["aud"] = "someone-else".into();
        aud["azp"] = "someone-else".into();
        assert_eq!(
            a.validate_id_token(&TestIdp::sign(&aud), "n")
                .await
                .unwrap_err(),
            TokenError::WrongAudience
        );
        let mut azp = idp.claims("n");
        azp["aud"] = serde_json::json!([CLIENT_ID, "other"]);
        azp["azp"] = "other".into();
        assert_eq!(
            a.validate_id_token(&TestIdp::sign(&azp), "n")
                .await
                .unwrap_err(),
            TokenError::WrongAudience,
            "azp must be this client"
        );
        let mut iss = idp.claims("n");
        iss["iss"] = "https://evil.example/realms/test".into();
        assert_eq!(
            a.validate_id_token(&TestIdp::sign(&iss), "n")
                .await
                .unwrap_err(),
            TokenError::WrongIssuer
        );
        assert_eq!(
            a.validate_id_token(&TestIdp::sign(&idp.claims("n")), "other-nonce")
                .await
                .unwrap_err(),
            TokenError::NonceMismatch
        );
        let mut no_nonce = idp.claims("n");
        no_nonce.as_object_mut().unwrap().remove("nonce");
        assert_eq!(
            a.validate_id_token(&TestIdp::sign(&no_nonce), "n")
                .await
                .unwrap_err(),
            TokenError::NonceMismatch
        );
        let mut future = idp.claims("n");
        future["nbf"] = (now + 3600).into();
        assert_eq!(
            a.validate_id_token(&TestIdp::sign(&future), "n")
                .await
                .unwrap_err(),
            TokenError::NotYetValid
        );
    }

    #[tokio::test]
    async fn rejects_untrusted_signers_and_symmetric_algorithms() {
        let idp = TestIdp::start().await;
        let a = idp.adapter();
        let c = idp.claims("n");
        // Key B presenting key A's kid: signature check fails.
        assert_eq!(
            a.validate_id_token(&TestIdp::sign_with(&c, KEY_B, Some("test-key-a")), "n")
                .await
                .unwrap_err(),
            TokenError::BadSignature
        );
        // Unknown kid: one forced JWKS refresh, then refused.
        assert!(matches!(
            a.validate_id_token(&TestIdp::sign_with(&c, KEY_B, Some("test-key-b")), "n")
                .await
                .unwrap_err(),
            TokenError::UnknownKey(_)
        ));
        // HS256 with the public modulus as secret (classic alg confusion) is refused up front.
        let hs = jsonwebtoken::encode(
            &jsonwebtoken::Header::new(Algorithm::HS256),
            &c,
            &jsonwebtoken::EncodingKey::from_secret(b"secret"),
        )
        .unwrap();
        assert_eq!(
            a.validate_id_token(&hs, "n").await.unwrap_err(),
            TokenError::AlgorithmNotAllowed(Algorithm::HS256)
        );
        assert!(matches!(
            a.validate_id_token("not.a.jwt", "n").await.unwrap_err(),
            TokenError::Malformed(_)
        ));
    }

    #[tokio::test]
    async fn jwks_is_cached_and_key_rotation_is_picked_up() {
        let idp = TestIdp::start().await;
        let a = idp.adapter();
        for _ in 0..3 {
            a.validate_id_token(&TestIdp::sign(&idp.claims("n")), "n")
                .await
                .unwrap();
        }
        assert_eq!(idp.state.jwks_fetches.load(Ordering::SeqCst), 1, "cached");
        // Rotate: the IdP now publishes key B. Age the cache past the refresh floor.
        let jwk_b: serde_json::Value = serde_json::from_str(JWK_B).unwrap();
        *idp.state.jwks.lock().unwrap() = serde_json::json!({"keys": [jwk_b]});
        if let Some((_, at)) = a.jwks.write().await.as_mut() {
            *at = Instant::now() - JWKS_MIN_REFRESH - Duration::from_secs(1);
        }
        let rotated = TestIdp::sign_with(&idp.claims("n"), KEY_B, Some("test-key-b"));
        a.validate_id_token(&rotated, "n").await.unwrap();
        assert_eq!(idp.state.jwks_fetches.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn authorization_url_and_code_exchange() {
        let idp = TestIdp::start().await;
        let a = idp.adapter();
        let verifier = pkce_verifier().unwrap();
        assert_eq!(verifier.len(), 43);
        let url = a
            .authorization_url(&AuthorizationParams {
                state: "st".into(),
                nonce: "no".into(),
                code_challenge: pkce_challenge(&verifier),
                step_up: false,
            })
            .await
            .unwrap();
        let parsed = reqwest::Url::parse(&url).unwrap();
        let q: std::collections::HashMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert!(url.starts_with(&format!("{}/protocol/openid-connect/auth?", idp.issuer)));
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["code_challenge"], pkce_challenge(&verifier));
        assert_eq!(q["state"], "st");
        assert_eq!(q["nonce"], "no");
        assert!(q["scope"].contains("openid"));
        assert!(!q.contains_key("prompt"));
        let step_up = a
            .authorization_url(&AuthorizationParams {
                state: "s".into(),
                nonce: "n".into(),
                code_challenge: "c".into(),
                step_up: true,
            })
            .await
            .unwrap();
        assert!(step_up.contains("prompt=login") && step_up.contains("acr_values=mfa"));

        *idp.state.id_token.lock().unwrap() = Some(TestIdp::sign(&idp.claims("no")));
        let id = a.exchange("the-code", &verifier, "no").await.unwrap();
        assert_eq!(id.principal.roles, vec![Role::Administrator]);
        let form = idp.state.last_form.lock().unwrap().clone();
        assert_eq!(form["grant_type"], "authorization_code");
        assert_eq!(form["code"], "the-code");
        assert_eq!(form["code_verifier"], verifier);
        assert!(idp
            .state
            .last_auth
            .lock()
            .unwrap()
            .as_deref()
            .unwrap()
            .starts_with("Basic "));
        // IdP refuses the code (e.g. replayed) → 401.
        *idp.state.token_status.lock().unwrap() = 400;
        assert!(matches!(
            a.exchange("the-code", &verifier, "no").await,
            Err(KernelError::Unauthenticated)
        ));
        let logout = a.end_session_url(Some("sid")).await.unwrap().unwrap();
        assert!(logout.contains("/protocol/openid-connect/logout?client_id=sutra-web"));
        assert!(logout.contains("post_logout_redirect_uri="));
    }

    #[test]
    fn pkce_rfc7636_appendix_b() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    fn claims(v: serde_json::Value) -> IdTokenClaims {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn claim_mapping_fails_closed() {
        let s = KeycloakSettings::new("https://kc/realms/t", CLIENT_ID, "x", "http://cb");
        let base = serde_json::json!({
            "sub": "u1",
            "realm_access": {"roles": ["faculty", "student", "faculty", "superuser"]},
            "tenant_id": TENANT,
            "given_name": "Meera",
            "family_name": "Shah",
            "email": "meera.shah@sutra.dev",
            "amr": "pwd",
        });
        let id = map_claims(&claims(base.clone()), &s).unwrap();
        assert_eq!(id.principal.roles, vec![Role::Faculty, Role::Student]);
        assert_eq!(id.principal.display_name, "Meera Shah");
        assert_eq!(id.principal.login_id, "meera.shah@sutra.dev");
        assert_eq!(id.principal.scope.institution_id, None);
        assert_eq!(id.mfa_level, MfaLevel::None, "pwd only");
        let mut acr = base.clone();
        acr["acr"] = "2".into();
        assert_eq!(
            map_claims(&claims(acr), &s).unwrap().mfa_level,
            MfaLevel::Mfa
        );
        let mut hwk = base.clone();
        hwk["amr"] = serde_json::json!(["hwk"]);
        assert_eq!(
            map_claims(&claims(hwk), &s).unwrap().mfa_level,
            MfaLevel::Mfa
        );

        let mut no_roles = base.clone();
        no_roles["realm_access"] = serde_json::json!({"roles": ["offline_access"]});
        assert!(matches!(
            map_claims(&claims(no_roles), &s),
            Err(KernelError::Forbidden(_))
        ));
        let mut no_tenant = base.clone();
        no_tenant.as_object_mut().unwrap().remove("tenant_id");
        assert!(matches!(
            map_claims(&claims(no_tenant), &s),
            Err(KernelError::Forbidden(_))
        ));
        let mut bad_tenant = base.clone();
        bad_tenant["tenant_id"] = "not-a-uuid".into();
        assert!(matches!(
            map_claims(&claims(bad_tenant), &s),
            Err(KernelError::Forbidden(_))
        ));
        let mut multi = base;
        multi["tenant_id"] = serde_json::json!([TENANT, TENANT]);
        assert!(matches!(
            map_claims(&claims(multi), &s),
            Err(KernelError::Forbidden(_))
        ));
    }

    #[test]
    fn backchannel_rewrite() {
        let mut s = KeycloakSettings::new("http://localhost:8180/realms/sutra", "c", "x", "cb");
        assert_eq!(
            s.backchannel("http://localhost:8180/realms/sutra/x"),
            "http://localhost:8180/realms/sutra/x"
        );
        s.backchannel_url = Some("http://keycloak:8180/realms/sutra/".into());
        assert_eq!(
            s.backchannel("http://localhost:8180/realms/sutra/protocol/openid-connect/certs"),
            "http://keycloak:8180/realms/sutra/protocol/openid-connect/certs"
        );
        assert_eq!(s.backchannel("https://other/x"), "https://other/x");
    }

    #[tokio::test]
    async fn password_login_is_not_offered() {
        let idp = TestIdp::start().await;
        let r = idp
            .adapter()
            .authenticate(Credentials {
                login_id: "a".into(),
                password: "b".into(),
            })
            .await;
        assert!(matches!(r, Err(KernelError::NotImplemented(_))));
    }
}
