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
    /// Seeded personas + shared password. Refused in production.
    Dev,
    /// Keycloak 26 credential delegation (Batch 1).
    Keycloak,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreBackend {
    Postgres,
    Memory,
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
    pub keycloak_url: String,
    pub keycloak_realm: String,
    pub keycloak_client_id: String,
    pub session_cookie_secure: bool,
    /// Absolute session lifetime (also the cookie `Max-Age`).
    pub session_ttl_secs: i64,
    /// Idle timeout: a session unused for this long is dead.
    pub session_idle_secs: i64,
    pub session_store: StoreBackend,
    pub idempotency_store: StoreBackend,
    pub audit_sink: StoreBackend,
    pub otel_endpoint: Option<String>,
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
        let auth_provider = match src.get("SUTRA_AUTH_PROVIDER").as_deref() {
            Some("keycloak") => AuthProvider::Keycloak,
            Some("dev") => AuthProvider::Dev,
            None if prod => AuthProvider::Keycloak,
            None => AuthProvider::Dev,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_AUTH_PROVIDER",
                    value: v.into(),
                })
            }
        };
        let database_url = match src.get("DATABASE_URL") {
            Some(u) => u,
            None if prod => return Err(ConfigError::RequiredInProduction("DATABASE_URL")),
            None => "postgres://sutra:sutra@localhost:5432/sutra".into(),
        };
        let default_store = if env == Environment::Test {
            StoreBackend::Memory
        } else {
            StoreBackend::Postgres
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
            dev_mfa_enforced: parse(src, "SUTRA_DEV_MFA", false)?,
            dev_tenant_id: string(src, "SUTRA_DEV_TENANT_ID", DEV_TENANT_ID),
            dev_institution_id: string(src, "SUTRA_DEV_INSTITUTION_ID", DEV_INSTITUTION_ID),
            keycloak_url: string(src, "SUTRA_KEYCLOAK_URL", "http://localhost:8180"),
            keycloak_realm: string(src, "SUTRA_KEYCLOAK_REALM", "sutra"),
            keycloak_client_id: string(src, "SUTRA_KEYCLOAK_CLIENT_ID", "sutra-api"),
            session_cookie_secure: parse(src, "SUTRA_SESSION_COOKIE_SECURE", true)?,
            session_ttl_secs: parse(src, "SUTRA_SESSION_TTL_SECS", 8 * 3600)?,
            session_idle_secs: parse(src, "SUTRA_SESSION_IDLE_SECS", 30 * 60)?,
            session_store: backend(src, "SUTRA_SESSION_STORE", default_store)?,
            idempotency_store: backend(src, "SUTRA_IDEMPOTENCY_STORE", default_store)?,
            audit_sink: backend(src, "SUTRA_AUDIT_SINK", default_store)?,
            otel_endpoint: src.get("OTEL_EXPORTER_OTLP_ENDPOINT"),
        };
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if self.env == Environment::Production {
            if self.auth_provider == AuthProvider::Dev {
                return Err(ConfigError::Forbidden(
                    "SUTRA_AUTH_PROVIDER=dev is not allowed in production",
                ));
            }
            if !self.session_cookie_secure {
                return Err(ConfigError::Forbidden(
                    "SUTRA_SESSION_COOKIE_SECURE=false is not allowed in production",
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
        let ok = HashMap::from([
            ("SUTRA_ENV", "production"),
            ("DATABASE_URL", "postgres://x"),
        ]);
        let c = Config::load(&ok).unwrap();
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
