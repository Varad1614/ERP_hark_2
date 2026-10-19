//! Part 1 · Stage 3 (S3-config): HTTP transport settings and the **production start-up gate**.
//!
//! `Config::load` calls [`HttpSecurityConfig::load`] for the transport settings and
//! [`validate`] / [`validate_production`] after everything else is parsed. Production refuses to
//! start (exit 2, `sutra-api check-config` too) on any insecure default. One rule = one message =
//! one unit test below (`docs/security/config.md` lists them).

use crate::config::{
    AuthProvider, Config, ConfigError, Environment, MailMode, SmtpTlsMode, Source,
    DEV_MFA_ENCRYPTION_KEY,
};
use crate::config_b9::{PaymentGatewayMode, SearchMode, WorkflowMode};
use std::time::Duration;
use sutra_web::rate_limit::{Limit, RateLimitSettings};

/// Variables that only make sense for the development authenticator / seed personas.
pub const DEV_ONLY_VARIABLES: &[&str] = &[
    "SUTRA_DEV_AUTH_PASSWORD",
    "SUTRA_DEV_MFA",
    "SUTRA_DEV_TENANT_ID",
    "SUTRA_DEV_INSTITUTION_ID",
];

/// Secrets that are published in this repository (compose, `.env.example`, the Keycloak dev
/// realm, the e2e harness) or are vendor defaults. Production refuses every one of them
/// (case-insensitive) as a password, client secret or webhook secret.
pub const KNOWN_DEV_SECRETS: &[&str] = &[
    "sutra",
    "sutra-api-dev-only",
    "dev-only-change-me",
    "dev-only-idp-admin-secret", // Part 2 · S4-idp: compose realm's identity admin client
    "sutra-dev-gateway-secret",
    "change-me",
    "changeme",
    "change_me",
    "password",
    "secret",
    "admin",
    "keycloak",
    "test",
    "rest-admin",
    "flowable",
    "opensearch",
    "postgres",
];

/// Minimum length of a production password / client secret.
pub const MIN_SECRET_LEN: usize = 16;
/// Minimum production HSTS max-age (180 days; the default is one year).
pub const MIN_HSTS_SECS: u64 = 180 * 24 * 3600;

/// Transport hardening (`Config::http`).
#[derive(Debug, Clone)]
pub struct HttpSecurityConfig {
    /// `SUTRA_HSTS` (default on in production) + `SUTRA_HSTS_MAX_AGE_SECS` (default one year
    /// when on; 0 = not sent).
    pub hsts_max_age_secs: u64,
    /// `SUTRA_MAX_BODY_BYTES` (default 2 MiB): request bodies above it answer 413.
    pub max_body_bytes: usize,
    /// `SUTRA_RATE_LIMIT*` / `SUTRA_TRUSTED_PROXY_HOPS` (on by default in production).
    pub rate_limit: RateLimitSettings,
    /// `SUTRA_API_DOCS` (default off in production): serve `GET /api/openapi.json`.
    pub api_docs: bool,
    /// `SUTRA_HEALTH_DETAIL` (default off in production): error texts in `/health/ready`.
    pub health_detail: bool,
    /// Development-only variables present in the environment (refused in production).
    pub dev_variables_set: Vec<&'static str>,
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

impl HttpSecurityConfig {
    pub fn load(src: &dyn Source, env: Environment) -> Result<Self, ConfigError> {
        let prod = env == Environment::Production;
        let limit =
            |per_min: &'static str, burst: &'static str, d: Limit| -> Result<Limit, ConfigError> {
                Ok(Limit {
                    per_minute: parse(src, per_min, d.per_minute)?,
                    burst: parse(src, burst, d.burst)?,
                })
            };
        let d = RateLimitSettings::default();
        let rate_limit = RateLimitSettings {
            enabled: parse(src, "SUTRA_RATE_LIMIT", prod)?,
            credentials: limit(
                "SUTRA_RATE_LIMIT_CREDENTIALS_PER_MIN",
                "SUTRA_RATE_LIMIT_CREDENTIALS_BURST",
                d.credentials,
            )?,
            auth: limit(
                "SUTRA_RATE_LIMIT_AUTH_PER_MIN",
                "SUTRA_RATE_LIMIT_AUTH_BURST",
                d.auth,
            )?,
            public: limit(
                "SUTRA_RATE_LIMIT_PUBLIC_PER_MIN",
                "SUTRA_RATE_LIMIT_PUBLIC_BURST",
                d.public,
            )?,
            // Production sits behind one TLS-terminating proxy (ingress / nginx) by default.
            trusted_proxy_hops: parse(src, "SUTRA_TRUSTED_PROXY_HOPS", usize::from(prod))?,
            max_clients: parse(src, "SUTRA_RATE_LIMIT_MAX_CLIENTS", d.max_clients)?,
        };
        // `SUTRA_HSTS` (S3-auth's switch; default on in production or when a max-age is given)
        // turns the header on with the one-year default; `SUTRA_HSTS=false` sends none (refused
        // in production below).
        let hsts_on: bool = parse(
            src,
            "SUTRA_HSTS",
            prod || src.get("SUTRA_HSTS_MAX_AGE_SECS").is_some(),
        )?;
        let hsts_max_age_secs = if hsts_on {
            parse(src, "SUTRA_HSTS_MAX_AGE_SECS", 365 * 24 * 3600)?
        } else {
            0
        };
        Ok(Self {
            hsts_max_age_secs,
            max_body_bytes: parse(src, "SUTRA_MAX_BODY_BYTES", 2 * 1024 * 1024)?,
            rate_limit,
            api_docs: parse(src, "SUTRA_API_DOCS", !prod)?,
            health_detail: parse(src, "SUTRA_HEALTH_DETAIL", !prod)?,
            dev_variables_set: DEV_ONLY_VARIABLES
                .iter()
                .copied()
                .filter(|k| src.get(k).is_some())
                .collect(),
        })
    }
}

/// `scheme://host[:port]` with `http` / `https`, no path, query, user info or wildcard.
pub fn is_valid_origin(o: &str) -> bool {
    let Some((scheme, rest)) = o.split_once("://") else {
        return false;
    };
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")) {
        return false;
    }
    let rest = rest.trim_end_matches('/');
    !rest.is_empty()
        && !rest.contains(['/', '?', '#', '@', '*', ' ', ','])
        && rest
            .split(':')
            .next()
            .is_some_and(|h| !h.is_empty() && h != "*")
}

fn is_https(url: &str) -> bool {
    url.trim()
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
}

/// The `sslmode` query parameter of a PostgreSQL URL (lower-case), if any.
pub fn sslmode(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        k.eq_ignore_ascii_case("sslmode")
            .then(|| v.trim().to_ascii_lowercase())
    })
}

/// The password of a `scheme://user:password@host` URL, if any.
pub fn url_password(url: &str) -> Option<&str> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?']).next()?;
    let userinfo = authority.rsplit_once('@')?.0;
    userinfo.split_once(':').map(|(_, p)| p)
}

pub(crate) fn known_dev_secret(s: &str) -> bool {
    let s = s.trim();
    KNOWN_DEV_SECRETS.iter().any(|k| k.eq_ignore_ascii_case(s))
}

fn weak_secret(s: &str) -> bool {
    known_dev_secret(s) || s.trim().len() < MIN_SECRET_LEN
}

fn low_entropy(k: &[u8; 32]) -> bool {
    let mut distinct = k.to_vec();
    distinct.sort_unstable();
    distinct.dedup();
    distinct.len() < 12
}

/// Rules that hold in every environment.
pub fn validate(cfg: &Config) -> Result<(), ConfigError> {
    for o in &cfg.allowed_origins {
        if o == "*" || o.eq_ignore_ascii_case("null") || !is_valid_origin(o) {
            return Err(ConfigError::Invalid {
                key: "SUTRA_ALLOWED_ORIGINS",
                value: format!("{o} (exact origins only: scheme://host[:port]; no wildcard)"),
            });
        }
    }
    let h = &cfg.http;
    if !(1024..=64 * 1024 * 1024).contains(&h.max_body_bytes) {
        return Err(ConfigError::Invalid {
            key: "SUTRA_MAX_BODY_BYTES",
            value: format!("{} (must be 1 KiB ..= 64 MiB)", h.max_body_bytes),
        });
    }
    if cfg.request_timeout.is_zero() || cfg.request_timeout > Duration::from_secs(300) {
        return Err(ConfigError::Invalid {
            key: "SUTRA_REQUEST_TIMEOUT_SECS",
            value: format!("{:?} (must be 1 ..= 300 s)", cfg.request_timeout),
        });
    }
    let rl = &h.rate_limit;
    for (key, l) in [
        ("SUTRA_RATE_LIMIT_CREDENTIALS_PER_MIN", rl.credentials),
        ("SUTRA_RATE_LIMIT_AUTH_PER_MIN", rl.auth),
        ("SUTRA_RATE_LIMIT_PUBLIC_PER_MIN", rl.public),
    ] {
        if l.per_minute == 0 || l.burst == 0 {
            return Err(ConfigError::Invalid {
                key,
                value: "0 (rate and burst must be > 0; set SUTRA_RATE_LIMIT=false to disable)"
                    .into(),
            });
        }
    }
    if rl.max_clients < 100 || rl.trusted_proxy_hops > 5 {
        return Err(ConfigError::Invalid {
            key: "SUTRA_TRUSTED_PROXY_HOPS",
            value: format!(
                "hops {} (<= 5), max clients {} (>= 100)",
                rl.trusted_proxy_hops, rl.max_clients
            ),
        });
    }
    let t = &cfg.hardening.telemetry;
    if t.metrics_enabled && t.metrics_addr.port() == cfg.http_addr.port() {
        return Err(ConfigError::Invalid {
            key: "SUTRA_METRICS_ADDR",
            value: format!(
                "{} (metrics need their own port, never the public API listener)",
                t.metrics_addr
            ),
        });
    }
    Ok(())
}

/// Production start-up gate: refuses insecure defaults (see the module docs).
pub fn validate_production(cfg: &Config) -> Result<(), ConfigError> {
    let forbid = |m: &'static str| Err(ConfigError::Forbidden(m));
    let h = &cfg.http;
    // ---- debug / development switches
    if let Some(k) = h.dev_variables_set.first() {
        return Err(ConfigError::Invalid {
            key: "SUTRA_ENV",
            value: format!("production with development-only variable {k} set"),
        });
    }
    // DEF-F-002 (mock payment gateway) is refused while loading (`config_b9`), defence in depth:
    if cfg.hardening.payments.gateway == PaymentGatewayMode::Mock {
        return forbid("SUTRA_PAYMENT_GATEWAY=mock is not allowed in production");
    }
    if !h.rate_limit.enabled {
        return forbid("SUTRA_RATE_LIMIT=false is not allowed in production");
    }
    if h.api_docs {
        return forbid(
            "SUTRA_API_DOCS=true is not allowed in production (the OpenAPI document is published \
             with the release, not served by the API)",
        );
    }
    if h.health_detail {
        return forbid("SUTRA_HEALTH_DETAIL=true is not allowed in production");
    }
    // ---- transport security
    if h.hsts_max_age_secs < MIN_HSTS_SECS {
        return forbid(
            "SUTRA_HSTS_MAX_AGE_SECS must be at least 15552000 (180 days) in production",
        );
    }
    if !is_https(&cfg.public_url) {
        return forbid("SUTRA_PUBLIC_URL must be an https:// URL in production");
    }
    if cfg.auth_provider == AuthProvider::Keycloak {
        let o = &cfg.oidc;
        if !is_https(&o.issuer) || !is_https(&o.redirect_uri) {
            return forbid(
                "SUTRA_OIDC_ISSUER and SUTRA_OIDC_REDIRECT_URI must be https:// URLs in production",
            );
        }
        if o.post_logout_redirect_uri
            .as_deref()
            .is_some_and(|u| !is_https(u))
        {
            return forbid("SUTRA_OIDC_POST_LOGOUT_REDIRECT_URI must be https:// in production");
        }
        if weak_secret(&o.client_secret) {
            return forbid(
                "SUTRA_OIDC_CLIENT_SECRET is a development / default value or shorter than 16 \
                 characters",
            );
        }
    }
    if cfg.webauthn.origins.iter().any(|o| !is_https(o)) {
        return forbid("SUTRA_WEBAUTHN_ORIGINS must all be https:// origins in production");
    }
    if cfg.allowed_origins.iter().any(|o| !is_https(o)) {
        return forbid("SUTRA_ALLOWED_ORIGINS must all be https:// origins in production");
    }
    for (key, url) in [
        ("DATABASE_URL", Some(cfg.database_url.as_str())),
        (
            "SUTRA_MIGRATION_DATABASE_URL",
            cfg.migration_database_url.as_deref(),
        ),
    ] {
        let Some(url) = url else { continue };
        if !matches!(
            sslmode(url).as_deref(),
            Some("require" | "verify-ca" | "verify-full")
        ) {
            return Err(ConfigError::Invalid {
                key,
                value: "production needs sslmode=require, verify-ca or verify-full".into(),
            });
        }
        if url_password(url).is_some_and(weak_secret) {
            return Err(ConfigError::Invalid {
                key,
                value: "the database password is a development / default value or shorter than \
                        16 characters"
                    .into(),
            });
        }
    }
    if cfg.mail.mode == MailMode::Smtp && cfg.mail.smtp_tls == SmtpTlsMode::None {
        return forbid("SUTRA_SMTP_TLS=none is not allowed in production (use starttls or tls)");
    }
    // ---- secrets
    let secrets = [
        (
            "SUTRA_DB_RUNTIME_PASSWORD",
            cfg.db_runtime_password.as_ref().map(|s| s.0.as_str()),
        ),
        ("SUTRA_SMTP_PASSWORD", cfg.mail.smtp_password.as_deref()),
        (
            "SUTRA_FLOWABLE_PASSWORD",
            cfg.hardening
                .workflow
                .flowable_password
                .as_ref()
                .map(|s| s.0.as_str()),
        ),
        (
            "SUTRA_OPENSEARCH_PASSWORD",
            cfg.hardening.search.password.as_ref().map(|s| s.0.as_str()),
        ),
    ];
    for (key, value) in secrets {
        if value.is_some_and(weak_secret) {
            return Err(ConfigError::Invalid {
                key,
                value: "a development / default value or shorter than 16 characters".into(),
            });
        }
    }
    if cfg.hardening.workflow.mode == WorkflowMode::Flowable
        && cfg.hardening.workflow.flowable_password.is_none()
    {
        return Err(ConfigError::RequiredInProduction("SUTRA_FLOWABLE_PASSWORD"));
    }
    if cfg.hardening.search.mode == SearchMode::OpenSearch
        && cfg.hardening.search.password.is_none()
    {
        return Err(ConfigError::RequiredInProduction(
            "SUTRA_OPENSEARCH_PASSWORD",
        ));
    }
    // ---- keys: distinct, random-looking, never the published development key
    let dev_key: Option<[u8; 32]> = DEV_MFA_ENCRYPTION_KEY
        .parse::<crate::config::Key32>()
        .ok()
        .map(|k| k.0);
    let mut keys: Vec<(&'static str, [u8; 32])> =
        vec![("SUTRA_MFA_ENCRYPTION_KEY", cfg.mfa_encryption_key.0)];
    if let Some(k) = cfg.csrf_key {
        keys.push(("SUTRA_CSRF_KEY", k.0));
    }
    if let Some(k) = cfg.hardening.audit_chain.anchor_key {
        keys.push(("SUTRA_AUDIT_ANCHOR_KEY", k.0));
    }
    for (i, &(name, k)) in keys.iter().enumerate() {
        if Some(k) == dev_key || low_entropy(&k) {
            return Err(ConfigError::Invalid {
                key: name,
                value: "<the published development key or a low-entropy key; generate 32 random \
                        bytes: openssl rand -hex 32>"
                    .into(),
            });
        }
        if keys.iter().skip(i + 1).any(|(_, other)| *other == k) {
            return Err(ConfigError::Invalid {
                key: name,
                value: "<the same key is used for another purpose; each key must be distinct>"
                    .into(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    const CSRF: &str = "3f9a1c7e5b2d8046a1e9c3b7d5f20864e1a3c5b7d9f1e2a4c6b8d0f2e4a6c8b1";
    const MFA: &str = "c1d2e3f405162738495a6b7c8d9eafb0c1d2e3f405162738495a6b7c8d9eafb1";
    const ANCHOR: &str = "8e7d6c5b4a39281706f5e4d3c2b1a0998e7d6c5b4a39281706f5e4d3c2b1a092";

    /// A complete, valid production environment (each test breaks exactly one rule).
    pub(crate) fn prod() -> HashMap<&'static str, &'static str> {
        HashMap::from([
            ("SUTRA_ENV", "production"),
            (
                "DATABASE_URL",
                "postgres://sutra_api:Zq8w3Lr0Vb6Xn2Ty@pg.internal:5432/sutra?sslmode=verify-full",
            ),
            ("SUTRA_OIDC_ISSUER", "https://idp.example.edu/realms/sutra"),
            (
                "SUTRA_OIDC_REDIRECT_URI",
                "https://erp.example.edu/api/v1/auth/oidc/callback",
            ),
            (
                "SUTRA_OIDC_POST_LOGOUT_REDIRECT_URI",
                "https://erp.example.edu/sign-in",
            ),
            (
                "SUTRA_OIDC_CLIENT_SECRET",
                "unit-test-only-oidc-client-secret",
            ),
            ("SUTRA_PUBLIC_URL", "https://erp.example.edu"),
            ("SUTRA_WEBAUTHN_RP_ID", "erp.example.edu"),
            ("SUTRA_WEBAUTHN_ORIGINS", "https://erp.example.edu"),
            ("SUTRA_CSRF_KEY", CSRF),
            ("SUTRA_MFA_ENCRYPTION_KEY", MFA),
            ("SUTRA_AUDIT_ANCHOR_KEY", ANCHOR),
            ("SUTRA_WORKFLOW", "flowable"),
            (
                "SUTRA_FLOWABLE_PASSWORD",
                "unit-test-only-flowable-password",
            ),
            // Part 2 · S7-entitle: production states the default entitlement explicitly.
            ("SUTRA_ENTITLEMENT_DEFAULT", "complete_erp"),
        ])
        .into_iter()
        // Part 2 · S4-docs: production needs S3 document storage.
        .chain(crate::config_docs::tests::PROD)
        .collect()
    }

    fn load(m: &HashMap<&'static str, &'static str>) -> Result<Config, ConfigError> {
        Config::load(m)
    }

    fn with(extra: &[(&'static str, &'static str)]) -> Result<Config, ConfigError> {
        let mut m = prod();
        m.extend(extra.iter().copied());
        load(&m)
    }

    fn without(key: &'static str) -> Result<Config, ConfigError> {
        let mut m = prod();
        m.remove(key);
        load(&m)
    }

    fn refused(r: Result<Config, ConfigError>, why: &str) {
        assert!(r.is_err(), "production must refuse: {why}");
    }

    #[test]
    fn the_reference_production_environment_is_accepted() {
        let c = load(&prod()).expect("valid production config");
        assert_eq!(c.http.hsts_max_age_secs, 31_536_000);
        assert!(c.http.rate_limit.enabled);
        assert_eq!(c.http.rate_limit.trusted_proxy_hops, 1);
        assert!(!c.http.api_docs && !c.http.health_detail);
        assert_eq!(c.hardening.payments.gateway, PaymentGatewayMode::Disabled);
        assert_eq!(c.http.max_body_bytes, 2 * 1024 * 1024);
    }

    #[test]
    fn development_defaults_stay_convenient() {
        let c = Config::load(&HashMap::<&str, &str>::new()).unwrap();
        assert_eq!(c.http.hsts_max_age_secs, 0);
        assert!(!c.http.rate_limit.enabled);
        assert!(c.http.api_docs && c.http.health_detail);
        assert_eq!(c.http.rate_limit.trusted_proxy_hops, 0);
    }

    // ---- rule: DEF-F-002 mock payment gateway
    #[test]
    fn refuses_the_mock_payment_gateway_even_with_a_strong_secret() {
        refused(
            with(&[
                ("SUTRA_PAYMENT_GATEWAY", "mock"),
                (
                    "SUTRA_PAYMENT_WEBHOOK_SECRET",
                    "0123456789abcdef0123456789abcdef0123",
                ),
            ]),
            "mock gateway",
        );
        assert!(with(&[("SUTRA_PAYMENT_GATEWAY", "disabled")]).is_ok());
    }

    // ---- rule: development-only variables / debug auth flags
    #[test]
    fn refuses_development_only_variables() {
        for k in DEV_ONLY_VARIABLES {
            refused(with(&[(*k, "x")]), k);
        }
        refused(with(&[("SUTRA_DEV_MFA", "false")]), "SUTRA_DEV_MFA present");
    }

    // ---- rule: default / weak secrets
    #[test]
    fn refuses_default_and_short_secrets() {
        for s in ["dev-only-change-me", "s", "CHANGE-ME", "short-secret-15"] {
            refused(with(&[("SUTRA_OIDC_CLIENT_SECRET", s)]), s);
        }
        for (k, v) in [
            ("SUTRA_FLOWABLE_PASSWORD", "test"),
            ("SUTRA_FLOWABLE_PASSWORD", "rest-admin"),
            ("SUTRA_SMTP_PASSWORD", "password"),
            ("SUTRA_OPENSEARCH_PASSWORD", "admin"),
        ] {
            refused(with(&[(k, v)]), k);
        }
        refused(
            with(&[
                ("SUTRA_DB_RUNTIME_USER", "sutra_api"),
                ("SUTRA_DB_RUNTIME_PASSWORD", "sutra-api-dev-only"),
            ]),
            "compose runtime password",
        );
        refused(
            with(&[(
                "DATABASE_URL",
                "postgres://sutra:sutra@pg.internal:5432/sutra?sslmode=require",
            )]),
            "sutra:sutra",
        );
        refused(
            without("SUTRA_FLOWABLE_PASSWORD"),
            "flowable without password",
        );
        refused(
            with(&[("SUTRA_SEARCH", "opensearch")]),
            "opensearch without password",
        );
    }

    // ---- rule: keys
    #[test]
    fn refuses_reused_low_entropy_and_published_keys() {
        refused(with(&[("SUTRA_CSRF_KEY", MFA)]), "csrf == mfa");
        refused(with(&[("SUTRA_AUDIT_ANCHOR_KEY", CSRF)]), "anchor == csrf");
        refused(
            with(&[(
                "SUTRA_CSRF_KEY",
                "0000000000000000000000000000000000000000000000000000000000000000",
            )]),
            "all-zero key",
        );
        refused(
            with(&[("SUTRA_MFA_ENCRYPTION_KEY", DEV_MFA_ENCRYPTION_KEY)]),
            "published dev key",
        );
    }

    // ---- rule: TLS everywhere
    #[test]
    fn refuses_plain_http_urls_and_missing_database_tls() {
        for (k, v) in [
            ("SUTRA_PUBLIC_URL", "http://erp.example.edu"),
            ("SUTRA_OIDC_ISSUER", "http://idp.example.edu/realms/sutra"),
            (
                "SUTRA_OIDC_REDIRECT_URI",
                "http://erp.example.edu/api/v1/auth/oidc/callback",
            ),
            (
                "SUTRA_OIDC_POST_LOGOUT_REDIRECT_URI",
                "http://erp.example.edu/sign-in",
            ),
            ("SUTRA_WEBAUTHN_ORIGINS", "http://erp.example.edu"),
            ("SUTRA_ALLOWED_ORIGINS", "http://partner.example.edu"),
        ] {
            refused(with(&[(k, v)]), k);
        }
        for url in [
            "postgres://sutra_api:Zq8w3Lr0Vb6Xn2Ty@pg.internal:5432/sutra",
            "postgres://sutra_api:Zq8w3Lr0Vb6Xn2Ty@pg.internal:5432/sutra?sslmode=prefer",
            "postgres://sutra_api:Zq8w3Lr0Vb6Xn2Ty@pg.internal:5432/sutra?sslmode=disable",
        ] {
            refused(with(&[("DATABASE_URL", url)]), url);
        }
        refused(
            with(&[(
                "SUTRA_MIGRATION_DATABASE_URL",
                "postgres://owner:Zq8w3Lr0Vb6Xn2TyAB@pg.internal:5432/sutra",
            )]),
            "migration URL without TLS",
        );
        assert!(with(&[(
            "SUTRA_MIGRATION_DATABASE_URL",
            "postgres://owner:Zq8w3Lr0Vb6Xn2TyAB@pg.internal:5432/sutra?sslmode=require",
        )])
        .is_ok());
        refused(
            with(&[("SUTRA_NOTIFY_EMAIL", "smtp"), ("SUTRA_SMTP_TLS", "none")]),
            "smtp without tls",
        );
        assert!(with(&[
            ("SUTRA_NOTIFY_EMAIL", "smtp"),
            ("SUTRA_SMTP_TLS", "starttls")
        ])
        .is_ok());
        refused(with(&[("SUTRA_HSTS_MAX_AGE_SECS", "0")]), "hsts off");
        refused(with(&[("SUTRA_HSTS_MAX_AGE_SECS", "86400")]), "hsts 1 day");
        // S3-auth's switch: `SUTRA_HSTS=false` sends no header, so production refuses it too.
        refused(with(&[("SUTRA_HSTS", "false")]), "hsts switched off");
    }

    // ---- rule: public surface (rate limit, docs, health detail)
    #[test]
    fn refuses_disabled_rate_limit_and_exposed_ops_endpoints() {
        refused(with(&[("SUTRA_RATE_LIMIT", "false")]), "rate limit off");
        refused(with(&[("SUTRA_API_DOCS", "true")]), "openapi served");
        refused(with(&[("SUTRA_HEALTH_DETAIL", "true")]), "health detail");
    }

    // ---- rules in every environment
    #[test]
    fn wildcard_cors_and_bad_limits_are_refused_everywhere() {
        for o in [
            "*",
            "null",
            "https://*.example.edu",
            "https://a.example/path",
            "a.example",
        ] {
            assert!(
                Config::load(&HashMap::from([("SUTRA_ALLOWED_ORIGINS", o)])).is_err(),
                "{o}"
            );
        }
        assert!(Config::load(&HashMap::from([(
            "SUTRA_ALLOWED_ORIGINS",
            "http://localhost:5173, https://partner.example.edu:8443"
        )]))
        .is_ok());
        for (k, v) in [
            ("SUTRA_MAX_BODY_BYTES", "0"),
            ("SUTRA_MAX_BODY_BYTES", "1073741824"),
            ("SUTRA_REQUEST_TIMEOUT_SECS", "0"),
            ("SUTRA_REQUEST_TIMEOUT_SECS", "3600"),
            ("SUTRA_RATE_LIMIT_CREDENTIALS_PER_MIN", "0"),
            ("SUTRA_RATE_LIMIT_PUBLIC_BURST", "0"),
            ("SUTRA_TRUSTED_PROXY_HOPS", "9"),
            ("SUTRA_METRICS_ADDR", "0.0.0.0:8080"),
        ] {
            assert!(Config::load(&HashMap::from([(k, v)])).is_err(), "{k}={v}");
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(
            sslmode("postgres://u:p@h/db?application_name=x&sslmode=Verify-Full").as_deref(),
            Some("verify-full")
        );
        assert_eq!(sslmode("postgres://u:p@h/db"), None);
        assert_eq!(
            url_password("postgres://u:p%40ss@h:5432/db"),
            Some("p%40ss")
        );
        assert_eq!(url_password("postgres://u@h/db"), None);
        assert!(is_valid_origin("https://erp.example.edu"));
        assert!(is_valid_origin("http://localhost:5173/"));
        assert!(!is_valid_origin("https://erp.example.edu/app"));
    }
}
