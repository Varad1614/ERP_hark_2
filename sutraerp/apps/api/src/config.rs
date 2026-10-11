//! Typed configuration from environment variables (12-factor). `.env` is honoured in development.
//! See `.env.example` for the full list.

use std::net::SocketAddr;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Environment {
    Development,
    Test,
    Production,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogFormat {
    Json,
    Pretty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthProvider {
    /// Seeded personas + shared password + SutraERP TOTP. Refused in production.
    Dev,
    /// Keycloak 26 via OIDC Authorization Code + PKCE (backend-for-frontend).
    Keycloak,
}

/// Development-only default key for the TOTP secret encryption (AES-256-GCM). Refused in
/// production; set `SUTRA_MFA_ENCRYPTION_KEY` (64 hex chars) everywhere else that matters.
pub const DEV_MFA_ENCRYPTION_KEY: &str =
    "5375747261455250206465762d6f6e6c79204d4641206b6579202d2d20646576";

/// OIDC relying-party settings (Keycloak mode).
#[derive(Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub backchannel_url: Option<String>,
    pub client_id: String,
    pub client_secret: String,
    pub redirect_uri: String,
    pub post_logout_redirect_uri: Option<String>,
    pub tenant_claim: String,
    pub institution_claim: String,
    pub mfa_acr_values: Vec<String>,
    pub step_up_acr: Option<String>,
}

impl std::fmt::Debug for OidcConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OidcConfig")
            .field("issuer", &self.issuer)
            .field("client_id", &self.client_id)
            .field("client_secret", &"<redacted>")
            .field("redirect_uri", &self.redirect_uri)
            .finish_non_exhaustive()
    }
}

/// A 256-bit key given as 64 hex characters. `Debug` is redacted.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Key32(pub [u8; 32]);

impl std::fmt::Debug for Key32 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key32(<redacted>)")
    }
}

impl std::str::FromStr for Key32 {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        let s = s.trim();
        if s.len() != 64 || !s.is_ascii() {
            return Err(());
        }
        let mut k = [0u8; 32];
        for (i, b) in k.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| ())?;
        }
        Ok(Key32(k))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreBackend {
    Postgres,
    Memory,
}

/// `SUTRA_OUTBOX_RELAY`: where committed outbox rows go.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxRelayMode {
    /// No relay in this process (rows accumulate until a relaying replica publishes them).
    Off,
    /// Publish to NATS JetStream (ADR-L5-0022).
    Nats,
}

/// `SUTRA_CACHE`: advisory cache backend (ADR-L5-0037).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheBackend {
    Off,
    /// Process-local; invalidation only reaches this replica.
    Memory,
    Valkey,
}

/// NATS JetStream settings for the outbox relay.
#[derive(Debug, Clone)]
pub struct NatsConfig {
    pub url: String,
    pub stream: String,
    pub subject_prefix: String,
    pub duplicate_window: Duration,
    pub max_age: Duration,
    pub replicas: usize,
}

/// Outbox relay tuning.
#[derive(Debug, Clone)]
pub struct RelayConfig {
    pub mode: OutboxRelayMode,
    pub batch_size: u32,
    pub poll_interval: Duration,
    pub lease: Duration,
    pub max_attempts: u32,
    pub backoff_base: Duration,
    pub backoff_max: Duration,
    pub publish_timeout: Duration,
}

/// Advisory cache settings.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    pub backend: CacheBackend,
    pub valkey_url: String,
    /// Environment segment of every key (defaults to the environment name).
    pub namespace: String,
    pub default_ttl: Duration,
    pub timeout: Duration,
    pub memory_max_entries: usize,
}

/// Sweeper intervals and retention windows. A zero interval disables that job.
#[derive(Debug, Clone)]
pub struct SweeperConfig {
    pub enabled: bool,
    pub batch_limit: u32,
    pub idempotency_interval: Duration,
    pub outbox_interval: Duration,
    pub outbox_retention: Duration,
    pub session_interval: Duration,
    pub session_grace: Duration,
    pub pre_auth_interval: Duration,
    pub pre_auth_grace: Duration,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub env: Environment,
    pub http_addr: SocketAddr,
    pub log_format: LogFormat,
    pub log_filter: String,
    pub database_url: String,
    pub db_max_connections: u32,
    pub db_acquire_timeout: Duration,
    pub request_timeout: Duration,
    pub shutdown_grace: Duration,
    pub auth_provider: AuthProvider,
    pub dev_auth_password: String,
    pub dev_mfa_enforced: bool,
    pub dev_tenant_id: String,
    pub dev_institution_id: String,
    pub oidc: OidcConfig,
    /// Require an MFA-verified session for step-up permissions (e.g. institution register).
    pub mfa_step_up: bool,
    /// AES-256-GCM key for TOTP secrets at rest (dev mode).
    pub mfa_encryption_key: Key32,
    /// Issuer label shown in authenticator apps.
    pub mfa_issuer: String,
    /// HMAC key of the pre-session (login) CSRF token. `None` → random per process (single
    /// instance only; required in production).
    pub csrf_key: Option<Key32>,
    pub login_csrf_ttl_secs: i64,
    /// Origins allowed to send unsafe requests besides the API's own host.
    pub allowed_origins: Vec<String>,
    pub session_cookie_secure: bool,
    /// Absolute session lifetime (also the cookie `Max-Age`).
    pub session_ttl_secs: i64,
    /// Idle timeout: a session unused for this long is dead.
    pub session_idle_secs: i64,
    /// `last_seen_at` is written at most once per this many seconds per session.
    pub session_touch_secs: i64,
    pub session_store: StoreBackend,
    pub idempotency_store: StoreBackend,
    pub audit_sink: StoreBackend,
    pub otel_endpoint: Option<String>,
    pub relay: RelayConfig,
    /// Production only: permit `SUTRA_OUTBOX_RELAY=off` (e.g. a dedicated relay deployment).
    pub relay_allow_off: bool,
    pub nats: NatsConfig,
    pub cache: CacheConfig,
    pub sweepers: SweeperConfig,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid value for {key}: {value}")]
    Invalid { key: &'static str, value: String },
    #[error("{0} is required in production")]
    RequiredInProduction(&'static str),
    #[error("{0}")]
    Forbidden(&'static str),
}

/// Source of raw values; abstracted for tests.
pub trait Source {
    fn get(&self, key: &str) -> Option<String>;
}

pub struct EnvSource;

impl Source for EnvSource {
    fn get(&self, key: &str) -> Option<String> {
        std::env::var(key).ok().filter(|v| !v.trim().is_empty())
    }
}

impl<S: std::hash::BuildHasher> Source
    for std::collections::HashMap<&'static str, &'static str, S>
{
    fn get(&self, key: &str) -> Option<String> {
        std::collections::HashMap::get(self, key).map(|s| (*s).to_owned())
    }
}

fn parse<T: std::str::FromStr>(
    src: &dyn Source,
    key: &'static str,
    default: T,
) -> Result<T, ConfigError> {
    match src.get(key) {
        None => Ok(default),
        Some(v) => v
            .trim()
            .parse()
            .map_err(|_| ConfigError::Invalid { key, value: v }),
    }
}

fn string(src: &dyn Source, key: &'static str, default: &str) -> String {
    src.get(key).unwrap_or_else(|| default.to_owned())
}

fn backend(
    src: &dyn Source,
    key: &'static str,
    default: StoreBackend,
) -> Result<StoreBackend, ConfigError> {
    match src.get(key).as_deref().map(str::to_lowercase).as_deref() {
        None => Ok(default),
        Some("postgres") => Ok(StoreBackend::Postgres),
        Some("memory") => Ok(StoreBackend::Memory),
        Some(v) => Err(ConfigError::Invalid {
            key,
            value: v.to_owned(),
        }),
    }
}

fn secs_cfg(src: &dyn Source, key: &'static str, default: u64) -> Result<Duration, ConfigError> {
    Ok(Duration::from_secs(parse(src, key, default)?))
}

fn millis_cfg(src: &dyn Source, key: &'static str, default: u64) -> Result<Duration, ConfigError> {
    Ok(Duration::from_millis(parse(src, key, default)?))
}

pub const DEV_TENANT_ID: &str = "0192f5a0-7c3e-7000-8000-000000000001";
pub const DEV_INSTITUTION_ID: &str = "0192f5a0-7c3e-7000-8000-0000000000aa";

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::load(&EnvSource)
    }

    pub fn load(src: &dyn Source) -> Result<Self, ConfigError> {
        let env = match src
            .get("SUTRA_ENV")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            None | Some("development") | Some("dev") => Environment::Development,
            Some("test") => Environment::Test,
            Some("production") | Some("prod") => Environment::Production,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_ENV",
                    value: v.into(),
                })
            }
        };
        let prod = env == Environment::Production;
        let log_format = match src.get("SUTRA_LOG_FORMAT").as_deref() {
            Some("json") => LogFormat::Json,
            Some("pretty") => LogFormat::Pretty,
            None if prod => LogFormat::Json,
            None => LogFormat::Pretty,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_LOG_FORMAT",
                    value: v.into(),
                })
            }
        };
        let database_url = match src.get("DATABASE_URL") {
            Some(u) => u,
            None if prod => return Err(ConfigError::RequiredInProduction("DATABASE_URL")),
            None => "postgres://sutra:sutra@localhost:5432/sutra".into(),
        };
        // SUTRA_AUTH_MODE (Batch 1B); SUTRA_AUTH_PROVIDER is the Batch 0/1A alias.
        let auth_mode = src
            .get("SUTRA_AUTH_MODE")
            .or_else(|| src.get("SUTRA_AUTH_PROVIDER"));
        let auth_provider = match auth_mode.as_deref().map(str::to_lowercase).as_deref() {
            Some("keycloak") => AuthProvider::Keycloak,
            Some("dev") => AuthProvider::Dev,
            None if prod => AuthProvider::Keycloak,
            None => AuthProvider::Dev,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_AUTH_MODE",
                    value: v.into(),
                })
            }
        };
        let keycloak_url = string(src, "SUTRA_KEYCLOAK_URL", "http://localhost:8180");
        let keycloak_realm = string(src, "SUTRA_KEYCLOAK_REALM", "sutra");
        let issuer = src.get("SUTRA_OIDC_ISSUER").unwrap_or_else(|| {
            format!(
                "{}/realms/{keycloak_realm}",
                keycloak_url.trim_end_matches('/')
            )
        });
        let list = |key: &str, default: &str| -> Vec<String> {
            src.get(key)
                .unwrap_or_else(|| default.to_owned())
                .split(',')
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
                .collect()
        };
        let client_secret = match src.get("SUTRA_OIDC_CLIENT_SECRET") {
            Some(s) => s,
            None if prod && auth_provider == AuthProvider::Keycloak => {
                return Err(ConfigError::RequiredInProduction(
                    "SUTRA_OIDC_CLIENT_SECRET",
                ))
            }
            None => "dev-only-change-me".into(),
        };
        let oidc = OidcConfig {
            issuer,
            backchannel_url: src.get("SUTRA_OIDC_BACKCHANNEL_URL"),
            client_id: src
                .get("SUTRA_OIDC_CLIENT_ID")
                .or_else(|| src.get("SUTRA_KEYCLOAK_CLIENT_ID"))
                .unwrap_or_else(|| "sutra-web".into()),
            client_secret,
            redirect_uri: string(
                src,
                "SUTRA_OIDC_REDIRECT_URI",
                "http://localhost:5173/api/v1/auth/oidc/callback",
            ),
            post_logout_redirect_uri: Some(string(
                src,
                "SUTRA_OIDC_POST_LOGOUT_REDIRECT_URI",
                "http://localhost:5173/sign-in",
            )),
            tenant_claim: string(src, "SUTRA_OIDC_TENANT_CLAIM", "tenant_id"),
            institution_claim: string(src, "SUTRA_OIDC_INSTITUTION_CLAIM", "institution_id"),
            mfa_acr_values: list("SUTRA_OIDC_MFA_ACR", "2,mfa"),
            step_up_acr: Some(string(src, "SUTRA_OIDC_STEP_UP_ACR", "mfa"))
                .filter(|v| !v.is_empty()),
        };
        let dev_mfa_enforced: bool = parse(src, "SUTRA_DEV_MFA", false)?;
        let mfa_step_up_default =
            prod || auth_provider == AuthProvider::Keycloak || dev_mfa_enforced;
        let default_store = if env == Environment::Test {
            StoreBackend::Memory
        } else {
            StoreBackend::Postgres
        };
        let relay_mode = match src
            .get("SUTRA_OUTBOX_RELAY")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("off") => OutboxRelayMode::Off,
            Some("nats") => OutboxRelayMode::Nats,
            None if prod => OutboxRelayMode::Nats,
            None => OutboxRelayMode::Off,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_OUTBOX_RELAY",
                    value: v.into(),
                })
            }
        };
        let cache_backend = match src
            .get("SUTRA_CACHE")
            .as_deref()
            .map(str::to_lowercase)
            .as_deref()
        {
            Some("off") => CacheBackend::Off,
            Some("memory") => CacheBackend::Memory,
            Some("valkey") => CacheBackend::Valkey,
            // Process-local caches are incoherent across replicas: off unless chosen in production.
            None if prod => CacheBackend::Off,
            None => CacheBackend::Memory,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_CACHE",
                    value: v.into(),
                })
            }
        };
        let env_name = match env {
            Environment::Development => "development",
            Environment::Test => "test",
            Environment::Production => "production",
        };
        let relay = RelayConfig {
            mode: relay_mode,
            batch_size: parse(src, "SUTRA_OUTBOX_BATCH_SIZE", 100)?,
            poll_interval: millis_cfg(src, "SUTRA_OUTBOX_POLL_MS", 500)?,
            lease: secs_cfg(src, "SUTRA_OUTBOX_LEASE_SECS", 30)?,
            max_attempts: parse(src, "SUTRA_OUTBOX_MAX_ATTEMPTS", 12)?,
            backoff_base: millis_cfg(src, "SUTRA_OUTBOX_BACKOFF_BASE_MS", 1000)?,
            backoff_max: secs_cfg(src, "SUTRA_OUTBOX_BACKOFF_MAX_SECS", 300)?,
            publish_timeout: secs_cfg(src, "SUTRA_OUTBOX_PUBLISH_TIMEOUT_SECS", 5)?,
        };
        let nats = NatsConfig {
            url: string(src, "SUTRA_NATS_URL", "nats://localhost:4222"),
            stream: string(src, "SUTRA_NATS_STREAM", "SUTRA_EVENTS"),
            subject_prefix: string(src, "SUTRA_NATS_SUBJECT_PREFIX", "sutra"),
            duplicate_window: secs_cfg(src, "SUTRA_NATS_DUPLICATE_WINDOW_SECS", 120)?,
            max_age: secs_cfg(src, "SUTRA_NATS_STREAM_MAX_AGE_SECS", 7 * 24 * 3600)?,
            replicas: parse(src, "SUTRA_NATS_STREAM_REPLICAS", 1)?,
        };
        let cache = CacheConfig {
            backend: cache_backend,
            valkey_url: string(src, "SUTRA_VALKEY_URL", "redis://localhost:6379"),
            namespace: string(src, "SUTRA_CACHE_NAMESPACE", env_name),
            default_ttl: secs_cfg(src, "SUTRA_CACHE_TTL_SECS", 60)?,
            timeout: millis_cfg(src, "SUTRA_CACHE_TIMEOUT_MS", 250)?,
            memory_max_entries: parse(src, "SUTRA_CACHE_MEMORY_MAX_ENTRIES", 10_000)?,
        };
        let sweepers = SweeperConfig {
            // Sweepers need PostgreSQL; the test profile runs on in-memory stores.
            enabled: parse(src, "SUTRA_SWEEPERS", env != Environment::Test)?,
            batch_limit: parse(src, "SUTRA_SWEEP_BATCH_LIMIT", 5000)?,
            idempotency_interval: secs_cfg(src, "SUTRA_SWEEP_IDEMPOTENCY_SECS", 300)?,
            outbox_interval: secs_cfg(src, "SUTRA_SWEEP_OUTBOX_SECS", 3600)?,
            outbox_retention: secs_cfg(src, "SUTRA_OUTBOX_RETENTION_SECS", 7 * 24 * 3600)?,
            session_interval: secs_cfg(src, "SUTRA_SWEEP_SESSION_SECS", 900)?,
            session_grace: secs_cfg(src, "SUTRA_SESSION_PURGE_GRACE_SECS", 24 * 3600)?,
            pre_auth_interval: secs_cfg(src, "SUTRA_SWEEP_PREAUTH_SECS", 300)?,
            pre_auth_grace: secs_cfg(src, "SUTRA_PREAUTH_PURGE_GRACE_SECS", 300)?,
        };
        let cfg = Config {
            env,
            http_addr: parse(
                src,
                "SUTRA_HTTP_ADDR",
                SocketAddr::from(([0, 0, 0, 0], 8080)),
            )?,
            log_format,
            log_filter: string(
                src,
                "RUST_LOG",
                "info,sutra=debug,tower_http=info,sqlx=warn",
            ),
            database_url,
            db_max_connections: parse(src, "SUTRA_DB_MAX_CONNECTIONS", 10)?,
            db_acquire_timeout: Duration::from_secs(parse(
                src,
                "SUTRA_DB_ACQUIRE_TIMEOUT_SECS",
                5,
            )?),
            request_timeout: Duration::from_secs(parse(src, "SUTRA_REQUEST_TIMEOUT_SECS", 30)?),
            shutdown_grace: Duration::from_secs(parse(src, "SUTRA_SHUTDOWN_GRACE_SECS", 20)?),
            auth_provider,
            dev_auth_password: string(src, "SUTRA_DEV_AUTH_PASSWORD", "sutra"),
            dev_mfa_enforced,
            dev_tenant_id: string(src, "SUTRA_DEV_TENANT_ID", DEV_TENANT_ID),
            dev_institution_id: string(src, "SUTRA_DEV_INSTITUTION_ID", DEV_INSTITUTION_ID),
            oidc,
            mfa_step_up: parse(src, "SUTRA_MFA_STEP_UP", mfa_step_up_default)?,
            mfa_encryption_key: match src.get("SUTRA_MFA_ENCRYPTION_KEY") {
                Some(v) => v.parse().map_err(|_| ConfigError::Invalid {
                    key: "SUTRA_MFA_ENCRYPTION_KEY",
                    value: "<64 hex characters expected>".into(),
                })?,
                None => DEV_MFA_ENCRYPTION_KEY
                    .parse()
                    .map_err(|_| ConfigError::Invalid {
                        key: "SUTRA_MFA_ENCRYPTION_KEY",
                        value: "<built-in dev key>".into(),
                    })?,
            },
            mfa_issuer: string(src, "SUTRA_MFA_ISSUER", "SutraERP"),
            csrf_key: src
                .get("SUTRA_CSRF_KEY")
                .map(|v| {
                    v.parse().map_err(|_| ConfigError::Invalid {
                        key: "SUTRA_CSRF_KEY",
                        value: "<64 hex characters expected>".into(),
                    })
                })
                .transpose()?,
            login_csrf_ttl_secs: parse(src, "SUTRA_LOGIN_CSRF_TTL_SECS", 1800)?,
            allowed_origins: list("SUTRA_ALLOWED_ORIGINS", ""),
            session_cookie_secure: parse(src, "SUTRA_SESSION_COOKIE_SECURE", true)?,
            session_ttl_secs: parse(src, "SUTRA_SESSION_TTL_SECS", 8 * 3600)?,
            session_idle_secs: parse(src, "SUTRA_SESSION_IDLE_SECS", 30 * 60)?,
            session_touch_secs: parse(src, "SUTRA_SESSION_TOUCH_SECS", 60)?,
            session_store: backend(src, "SUTRA_SESSION_STORE", default_store)?,
            idempotency_store: backend(src, "SUTRA_IDEMPOTENCY_STORE", default_store)?,
            audit_sink: backend(src, "SUTRA_AUDIT_SINK", default_store)?,
            otel_endpoint: src.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
            relay,
            relay_allow_off: parse(src, "SUTRA_OUTBOX_RELAY_ALLOW_OFF", false)?,
            nats,
            cache,
            sweepers,
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.env == Environment::Production {
            if self.auth_provider == AuthProvider::Dev {
                return Err(ConfigError::Forbidden(
                    "SUTRA_AUTH_MODE=dev is not allowed in production",
                ));
            }
            if !self.session_cookie_secure {
                return Err(ConfigError::Forbidden(
                    "SUTRA_SESSION_COOKIE_SECURE=false is not allowed in production",
                ));
            }
            if !self.mfa_step_up {
                return Err(ConfigError::Forbidden(
                    "SUTRA_MFA_STEP_UP=false is not allowed in production",
                ));
            }
            if self.csrf_key.is_none() {
                return Err(ConfigError::RequiredInProduction("SUTRA_CSRF_KEY"));
            }
            if self.mfa_encryption_key.0.as_slice()
                == DEV_MFA_ENCRYPTION_KEY
                    .parse::<Key32>()
                    .map(|k| k.0)
                    .unwrap_or_default()
                    .as_slice()
            {
                return Err(ConfigError::Forbidden(
                    "the built-in dev SUTRA_MFA_ENCRYPTION_KEY is not allowed in production",
                ));
            }
            if self.idempotency_store == StoreBackend::Memory
                || self.audit_sink == StoreBackend::Memory
                || self.session_store == StoreBackend::Memory
            {
                return Err(ConfigError::Forbidden(
                    "in-memory session/idempotency/audit stores are not allowed in production",
                ));
            }
            if self.relay.mode == OutboxRelayMode::Off && !self.relay_allow_off {
                return Err(ConfigError::Forbidden(
                    "SUTRA_OUTBOX_RELAY=off is not allowed in production unless \
                     SUTRA_OUTBOX_RELAY_ALLOW_OFF=true (another deployment relays the outbox)",
                ));
            }
        }
        if self.relay.batch_size == 0 || self.relay.batch_size > 10_000 {
            return Err(ConfigError::Invalid {
                key: "SUTRA_OUTBOX_BATCH_SIZE",
                value: format!("{} (must be 1..=10000)", self.relay.batch_size),
            });
        }
        if self.relay.max_attempts == 0 {
            return Err(ConfigError::Invalid {
                key: "SUTRA_OUTBOX_MAX_ATTEMPTS",
                value: "0".into(),
            });
        }
        if self.relay.publish_timeout.is_zero() || self.relay.lease < self.relay.publish_timeout * 2
        {
            return Err(ConfigError::Invalid {
                key: "SUTRA_OUTBOX_LEASE_SECS",
                value: format!(
                    "{:?} (must be at least twice SUTRA_OUTBOX_PUBLISH_TIMEOUT_SECS, which must be > 0)",
                    self.relay.lease
                ),
            });
        }
        if self.relay.mode == OutboxRelayMode::Nats
            && self.nats.duplicate_window <= self.relay.lease
        {
            return Err(ConfigError::Invalid {
                key: "SUTRA_NATS_DUPLICATE_WINDOW_SECS",
                value: format!(
                    "{:?} (must exceed SUTRA_OUTBOX_LEASE_SECS so a re-claimed row is de-duplicated)",
                    self.nats.duplicate_window
                ),
            });
        }
        if self.sweepers.batch_limit == 0 {
            return Err(ConfigError::Invalid {
                key: "SUTRA_SWEEP_BATCH_LIMIT",
                value: "0".into(),
            });
        }
        if self.session_ttl_secs <= 0 {
            return Err(ConfigError::Invalid {
                key: "SUTRA_SESSION_TTL_SECS",
                value: self.session_ttl_secs.to_string(),
            });
        }
        if self.session_idle_secs <= 0 || self.session_idle_secs > self.session_ttl_secs {
            return Err(ConfigError::Invalid {
                key: "SUTRA_SESSION_IDLE_SECS",
                value: format!(
                    "{} (must be > 0 and <= SUTRA_SESSION_TTL_SECS)",
                    self.session_idle_secs
                ),
            });
        }
        if self.session_touch_secs < 0 || self.session_touch_secs >= self.session_idle_secs {
            return Err(ConfigError::Invalid {
                key: "SUTRA_SESSION_TOUCH_SECS",
                value: format!(
                    "{} (must be >= 0 and < SUTRA_SESSION_IDLE_SECS)",
                    self.session_touch_secs
                ),
            });
        }
        if self.login_csrf_ttl_secs <= 0 {
            return Err(ConfigError::Invalid {
                key: "SUTRA_LOGIN_CSRF_TTL_SECS",
                value: self.login_csrf_ttl_secs.to_string(),
            });
        }
        if self.db_max_connections == 0 {
            return Err(ConfigError::Invalid {
                key: "SUTRA_DB_MAX_CONNECTIONS",
                value: "0".into(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const KEY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    #[test]
    fn auth_mode_oidc_and_mfa_settings() {
        let dev = Config::load(&HashMap::<&str, &str>::new()).unwrap();
        assert!(
            !dev.mfa_step_up,
            "dev without enforced MFA keeps step-up off"
        );
        assert_eq!(dev.oidc.issuer, "http://localhost:8180/realms/sutra");
        assert_eq!(dev.oidc.client_id, "sutra-web");
        assert_eq!(dev.session_touch_secs, 60);
        assert!(dev.csrf_key.is_none());
        let kc = Config::load(&HashMap::from([
            ("SUTRA_AUTH_MODE", "keycloak"),
            ("SUTRA_OIDC_ISSUER", "https://sso.example/realms/x"),
            ("SUTRA_OIDC_MFA_ACR", "gold, silver"),
            (
                "SUTRA_ALLOWED_ORIGINS",
                "https://a.example, https://b.example",
            ),
        ]))
        .unwrap();
        assert_eq!(kc.auth_provider, AuthProvider::Keycloak);
        assert!(kc.mfa_step_up);
        assert_eq!(kc.oidc.issuer, "https://sso.example/realms/x");
        assert_eq!(kc.oidc.mfa_acr_values, vec!["gold", "silver"]);
        assert_eq!(kc.allowed_origins.len(), 2);
        let legacy = Config::load(&HashMap::from([("SUTRA_AUTH_PROVIDER", "keycloak")])).unwrap();
        assert_eq!(legacy.auth_provider, AuthProvider::Keycloak);
        let enforced = Config::load(&HashMap::from([("SUTRA_DEV_MFA", "true")])).unwrap();
        assert!(enforced.mfa_step_up);
        assert!(Config::load(&HashMap::from([("SUTRA_AUTH_MODE", "saml")])).is_err());
        assert!(Config::load(&HashMap::from([("SUTRA_CSRF_KEY", "abc")])).is_err());
        assert_eq!(
            Config::load(&HashMap::from([("SUTRA_CSRF_KEY", KEY)]))
                .unwrap()
                .csrf_key
                .map(|k| k.0[1]),
            Some(0x11)
        );
        assert!(
            Config::load(&HashMap::from([("SUTRA_SESSION_TOUCH_SECS", "1800")])).is_err(),
            "touch interval must be below the idle timeout"
        );
    }

    #[test]
    fn development_defaults() {
        let c = Config::load(&HashMap::<&str, &str>::new()).unwrap();
        assert_eq!(c.env, Environment::Development);
        assert_eq!(c.auth_provider, AuthProvider::Dev);
        assert_eq!(c.http_addr.port(), 8080);
        assert!(c.session_cookie_secure);
    }

    #[test]
    fn production_requires_database_and_forbids_dev_auth() {
        let only_env = HashMap::from([("SUTRA_ENV", "production")]);
        assert!(matches!(
            Config::load(&only_env),
            Err(ConfigError::RequiredInProduction("DATABASE_URL"))
        ));
        let dev_auth = HashMap::from([
            ("SUTRA_ENV", "production"),
            ("DATABASE_URL", "postgres://x"),
            ("SUTRA_AUTH_PROVIDER", "dev"),
        ]);
        assert!(matches!(
            Config::load(&dev_auth),
            Err(ConfigError::Forbidden(_))
        ));
        let no_secret = HashMap::from([
            ("SUTRA_ENV", "production"),
            ("DATABASE_URL", "postgres://x"),
        ]);
        assert!(matches!(
            Config::load(&no_secret),
            Err(ConfigError::RequiredInProduction(
                "SUTRA_OIDC_CLIENT_SECRET"
            ))
        ));
        let no_csrf_key = HashMap::from([
            ("SUTRA_ENV", "production"),
            ("DATABASE_URL", "postgres://x"),
            ("SUTRA_OIDC_CLIENT_SECRET", "s"),
        ]);
        assert!(matches!(
            Config::load(&no_csrf_key),
            Err(ConfigError::RequiredInProduction("SUTRA_CSRF_KEY"))
        ));
        let dev_key = HashMap::from([
            ("SUTRA_ENV", "production"),
            ("DATABASE_URL", "postgres://x"),
            ("SUTRA_OIDC_CLIENT_SECRET", "s"),
            ("SUTRA_CSRF_KEY", KEY),
        ]);
        assert!(matches!(
            Config::load(&dev_key),
            Err(ConfigError::Forbidden(_))
        ));
        let ok = HashMap::from([
            ("SUTRA_ENV", "production"),
            ("DATABASE_URL", "postgres://x"),
            ("SUTRA_OIDC_CLIENT_SECRET", "s"),
            ("SUTRA_CSRF_KEY", KEY),
            ("SUTRA_MFA_ENCRYPTION_KEY", KEY),
        ]);
        let c = Config::load(&ok).unwrap();
        assert_eq!(c.relay.mode, OutboxRelayMode::Nats, "relay on by default");
        assert_eq!(c.cache.backend, CacheBackend::Off);
        assert_eq!(c.cache.namespace, "production");
        assert!(c.mfa_step_up, "step-up is always enforced in production");
        assert!(
            !format!("{:?}", c.oidc).contains("\"s\""),
            "secret redacted"
        );
        assert_eq!(c.auth_provider, AuthProvider::Keycloak);
        assert_eq!(c.log_format, LogFormat::Json);
    }

    #[test]
    fn session_store_and_timeouts() {
        let c = Config::load(&HashMap::<&str, &str>::new()).unwrap();
        assert_eq!(c.session_store, StoreBackend::Postgres);
        assert_eq!((c.session_idle_secs, c.session_ttl_secs), (1800, 28800));
        let test = Config::load(&HashMap::from([("SUTRA_ENV", "test")])).unwrap();
        assert_eq!(test.session_store, StoreBackend::Memory);
        let mem = Config::load(&HashMap::from([("SUTRA_SESSION_STORE", "memory")])).unwrap();
        assert_eq!(mem.session_store, StoreBackend::Memory);
        let prod_mem = HashMap::from([
            ("SUTRA_ENV", "production"),
            ("DATABASE_URL", "postgres://x"),
            ("SUTRA_OIDC_CLIENT_SECRET", "s"),
            ("SUTRA_CSRF_KEY", KEY),
            ("SUTRA_MFA_ENCRYPTION_KEY", KEY),
            ("SUTRA_SESSION_STORE", "memory"),
        ]);
        assert!(matches!(
            Config::load(&prod_mem),
            Err(ConfigError::Forbidden(_))
        ));
        let idle_too_long = HashMap::from([
            ("SUTRA_SESSION_TTL_SECS", "600"),
            ("SUTRA_SESSION_IDLE_SECS", "601"),
        ]);
        assert!(matches!(
            Config::load(&idle_too_long),
            Err(ConfigError::Invalid {
                key: "SUTRA_SESSION_IDLE_SECS",
                ..
            })
        ));
        assert!(Config::load(&HashMap::from([("SUTRA_SESSION_STORE", "redis")])).is_err());
    }

    #[test]
    fn relay_cache_and_sweeper_settings() {
        let dev = Config::load(&HashMap::<&str, &str>::new()).unwrap();
        assert_eq!(dev.relay.mode, OutboxRelayMode::Off);
        assert_eq!(dev.cache.backend, CacheBackend::Memory);
        assert_eq!(dev.cache.namespace, "development");
        assert!(dev.sweepers.enabled);
        assert_eq!(dev.nats.stream, "SUTRA_EVENTS");
        assert_eq!(dev.sweepers.outbox_retention, Duration::from_secs(604_800));
        let test = Config::load(&HashMap::from([("SUTRA_ENV", "test")])).unwrap();
        assert!(!test.sweepers.enabled, "test profile has no database");
        let custom = Config::load(&HashMap::from([
            ("SUTRA_OUTBOX_RELAY", "nats"),
            ("SUTRA_NATS_URL", "nats://nats:4222"),
            ("SUTRA_CACHE", "valkey"),
            ("SUTRA_VALKEY_URL", "rediss://cache:6380"),
            ("SUTRA_SWEEP_SESSION_SECS", "0"),
            ("SUTRA_OUTBOX_RETENTION_SECS", "60"),
        ]))
        .unwrap();
        assert_eq!(custom.relay.mode, OutboxRelayMode::Nats);
        assert_eq!(custom.nats.url, "nats://nats:4222");
        assert_eq!(custom.cache.backend, CacheBackend::Valkey);
        assert!(custom.sweepers.session_interval.is_zero());
        assert_eq!(custom.sweepers.outbox_retention, Duration::from_secs(60));
        for bad in [
            ("SUTRA_OUTBOX_RELAY", "kafka"),
            ("SUTRA_CACHE", "redis"),
            ("SUTRA_OUTBOX_BATCH_SIZE", "0"),
            ("SUTRA_OUTBOX_MAX_ATTEMPTS", "0"),
            ("SUTRA_OUTBOX_LEASE_SECS", "5"),
            ("SUTRA_SWEEP_BATCH_LIMIT", "0"),
        ] {
            assert!(Config::load(&HashMap::from([bad])).is_err(), "{bad:?}");
        }
        assert!(
            Config::load(&HashMap::from([
                ("SUTRA_OUTBOX_RELAY", "nats"),
                ("SUTRA_NATS_DUPLICATE_WINDOW_SECS", "30"),
            ]))
            .is_err(),
            "duplicate window must exceed the lease"
        );
        let prod = |extra: (&'static str, &'static str)| {
            Config::load(&HashMap::from([
                ("SUTRA_ENV", "production"),
                ("DATABASE_URL", "postgres://x"),
                ("SUTRA_OIDC_CLIENT_SECRET", "s"),
                ("SUTRA_CSRF_KEY", KEY),
                ("SUTRA_MFA_ENCRYPTION_KEY", KEY),
                ("SUTRA_OUTBOX_RELAY", "off"),
                extra,
            ]))
        };
        assert!(matches!(
            prod(("SUTRA_CACHE", "off")),
            Err(ConfigError::Forbidden(_))
        ));
        assert!(prod(("SUTRA_OUTBOX_RELAY_ALLOW_OFF", "true")).is_ok());
    }

    #[test]
    fn rejects_garbage() {
        let bad = HashMap::from([("SUTRA_HTTP_ADDR", "nope")]);
        assert!(matches!(
            Config::load(&bad),
            Err(ConfigError::Invalid {
                key: "SUTRA_HTTP_ADDR",
                ..
            })
        ));
    }
}
