//! Part 2 · Stage 4.3 (S4-idp): identity provisioning settings (`SUTRA_IDP_*`) and their
//! production rules (`docs/readiness/part2/s4-idp.md`).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SUTRA_IDP_PROVISIONING` | `off` | `off` · `keycloak` (Admin REST adapter) · `memory` (tests; refused in production) |
//! | `SUTRA_IDP_URL` | server root of `SUTRA_OIDC_BACKCHANNEL_URL` / `SUTRA_OIDC_ISSUER` | Keycloak base URL the API calls (`https://sso.example.edu`) |
//! | `SUTRA_IDP_REALM` | realm of `SUTRA_OIDC_ISSUER` | realm name |
//! | `SUTRA_IDP_CLIENT_ID` | `sutra-idp-admin` | confidential service-account client (never the sign-in client) |
//! | `SUTRA_IDP_CLIENT_SECRET` | — (required with `keycloak`) | its secret |
//! | `SUTRA_IDP_TIMEOUT_MS` | `10000` | per-call timeout |
//! | `SUTRA_IDP_AUTO_PROVISION` | `true` | create identities when a student / staff member becomes active |
//! | `SUTRA_IDP_SEND_ACTIONS_EMAIL` | `false` | ask Keycloak to e-mail "execute actions" links (needs realm SMTP) |
//! | `SUTRA_IDP_ACTIONS_CLIENT_ID` / `SUTRA_IDP_ACTIONS_REDIRECT_URI` | — | where the e-mailed link lands |
//! | `SUTRA_IDP_POLL_MS` · `SUTRA_IDP_BATCH_SIZE` | `5000` · `50` | job cadence |
//! | `SUTRA_IDP_RETRY_BASE_SECS` · `SUTRA_IDP_RETRY_MAX_SECS` | `10` · `3600` | back-off of a failed sync |
//! | `SUTRA_IDP_STARTUP_CHECK` | `true` | verify the admin client's least-privilege grants at start-up (refused off in production; a failed check stops a production start) |
//!
//! Production refuses: `memory`; a plain-http (non-loopback) URL; a missing / published / short
//! client secret; the sign-in client (`SUTRA_OIDC_CLIENT_ID`) reused as admin client; the
//! start-up check switched off.

use crate::config::{ConfigError, Environment, Source};
use crate::config_http::{KNOWN_DEV_SECRETS, MIN_SECRET_LEN};
use std::time::Duration;

/// `SUTRA_IDP_PROVISIONING`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdpMode {
    Off,
    Keycloak,
    Memory,
}

/// Identity provisioning settings (`Config::idp`).
#[derive(Clone, Debug)]
pub struct IdpConfig {
    pub mode: IdpMode,
    pub url: String,
    pub realm: String,
    pub client_id: String,
    pub client_secret: Option<String>,
    pub timeout: Duration,
    pub auto_provision: bool,
    pub send_actions_email: bool,
    pub actions_client_id: Option<String>,
    pub actions_redirect_uri: Option<String>,
    pub poll_interval: Duration,
    pub batch_size: u32,
    pub retry_base: Duration,
    pub retry_max: Duration,
    pub startup_check: bool,
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

fn non_empty(src: &dyn Source, key: &str) -> Option<String> {
    src.get(key)
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// `(server root, realm)` of an issuer like `https://sso.example/realms/sutra`.
pub fn split_issuer(issuer: &str) -> Option<(String, String)> {
    let (root, realm) = issuer
        .trim()
        .trim_end_matches('/')
        .rsplit_once("/realms/")?;
    (!root.is_empty() && !realm.is_empty() && !realm.contains('/'))
        .then(|| (root.to_owned(), realm.to_owned()))
}

fn https_or_loopback(url: &str) -> bool {
    let u = url.trim().to_ascii_lowercase();
    if u.starts_with("https://") {
        return true;
    }
    let Some(rest) = u.strip_prefix("http://") else {
        return false;
    };
    let host = rest.split(['/', '?']).next().unwrap_or("");
    let host = host.rsplit_once(':').map_or(host, |(h, _)| h);
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

impl IdpConfig {
    pub fn load(src: &dyn Source, env: Environment) -> Result<Self, ConfigError> {
        let prod = env == Environment::Production;
        let mode = match src
            .get("SUTRA_IDP_PROVISIONING")
            .map(|v| v.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("" | "off" | "none" | "false") => IdpMode::Off,
            Some("keycloak") => IdpMode::Keycloak,
            Some("memory") => IdpMode::Memory,
            Some(other) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_IDP_PROVISIONING",
                    value: other.to_owned(),
                })
            }
        };
        let issuer = non_empty(src, "SUTRA_OIDC_BACKCHANNEL_URL")
            .or_else(|| non_empty(src, "SUTRA_OIDC_ISSUER"))
            .and_then(|i| split_issuer(&i));
        let url = non_empty(src, "SUTRA_IDP_URL")
            .or_else(|| issuer.as_ref().map(|(r, _)| r.clone()))
            .unwrap_or_else(|| "http://localhost:8180".into())
            .trim_end_matches('/')
            .to_owned();
        let realm = non_empty(src, "SUTRA_IDP_REALM")
            .or_else(|| issuer.as_ref().map(|(_, r)| r.clone()))
            .unwrap_or_else(|| "sutra".into());
        let cfg = Self {
            mode,
            url,
            realm,
            client_id: non_empty(src, "SUTRA_IDP_CLIENT_ID")
                .unwrap_or_else(|| "sutra-idp-admin".into()),
            client_secret: non_empty(src, "SUTRA_IDP_CLIENT_SECRET"),
            timeout: Duration::from_millis(parse(src, "SUTRA_IDP_TIMEOUT_MS", 10_000u64)?.max(100)),
            auto_provision: parse(src, "SUTRA_IDP_AUTO_PROVISION", true)?,
            send_actions_email: parse(src, "SUTRA_IDP_SEND_ACTIONS_EMAIL", false)?,
            actions_client_id: non_empty(src, "SUTRA_IDP_ACTIONS_CLIENT_ID"),
            actions_redirect_uri: non_empty(src, "SUTRA_IDP_ACTIONS_REDIRECT_URI"),
            poll_interval: Duration::from_millis(
                parse(src, "SUTRA_IDP_POLL_MS", 5_000u64)?.max(200),
            ),
            batch_size: parse(src, "SUTRA_IDP_BATCH_SIZE", 50u32)?.clamp(1, 1000),
            retry_base: Duration::from_secs(parse(src, "SUTRA_IDP_RETRY_BASE_SECS", 10u64)?.max(1)),
            retry_max: Duration::from_secs(
                parse(src, "SUTRA_IDP_RETRY_MAX_SECS", 3_600u64)?.max(1),
            ),
            startup_check: parse(src, "SUTRA_IDP_STARTUP_CHECK", true)?,
        };
        if cfg.mode == IdpMode::Keycloak && cfg.client_secret.is_none() {
            return Err(ConfigError::Invalid {
                key: "SUTRA_IDP_CLIENT_SECRET",
                value: "required with SUTRA_IDP_PROVISIONING=keycloak".into(),
            });
        }
        if prod {
            cfg.validate_production(non_empty(src, "SUTRA_OIDC_CLIENT_ID").as_deref())?;
        }
        Ok(cfg)
    }

    fn validate_production(&self, oidc_client: Option<&str>) -> Result<(), ConfigError> {
        let forbid = |m: &'static str| Err(ConfigError::Forbidden(m));
        match self.mode {
            IdpMode::Off => Ok(()),
            IdpMode::Memory => forbid(
                "SUTRA_IDP_PROVISIONING=memory (in-memory identity provider) is not allowed in \
                 production",
            ),
            IdpMode::Keycloak => {
                if !https_or_loopback(&self.url) {
                    return forbid("SUTRA_IDP_URL must use https in production");
                }
                let secret = self.client_secret.as_deref().unwrap_or("");
                if secret.len() < MIN_SECRET_LEN
                    || KNOWN_DEV_SECRETS
                        .iter()
                        .any(|k| k.eq_ignore_ascii_case(secret))
                {
                    return forbid(
                        "SUTRA_IDP_CLIENT_SECRET is a published / default or too short secret \
                         (at least 16 characters) — not allowed in production",
                    );
                }
                if oidc_client.unwrap_or("sutra-web") == self.client_id {
                    return forbid(
                        "SUTRA_IDP_CLIENT_ID must be a dedicated service-account client, not the \
                         sign-in client SUTRA_OIDC_CLIENT_ID",
                    );
                }
                if !self.startup_check {
                    return forbid(
                        "SUTRA_IDP_STARTUP_CHECK=false (no least-privilege check of the identity \
                         admin client) is not allowed in production",
                    );
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::collections::HashMap;

    fn prod_with(extra: &[(&'static str, &'static str)]) -> Result<Config, ConfigError> {
        let mut m = crate::config_http::tests::prod();
        m.extend(extra.iter().copied());
        Config::load(&m)
    }

    const KC: (&str, &str) = ("SUTRA_IDP_PROVISIONING", "keycloak");
    const SECRET: (&str, &str) = ("SUTRA_IDP_CLIENT_SECRET", "unit-test-only-idp-admin-secret");

    #[test]
    fn defaults_are_off_and_derived_from_the_issuer() {
        let c = Config::load(&HashMap::<&str, &str>::new()).unwrap();
        assert_eq!(c.idp.mode, IdpMode::Off);
        let c = Config::load(&HashMap::from([
            KC,
            ("SUTRA_IDP_CLIENT_SECRET", "x"),
            ("SUTRA_OIDC_ISSUER", "http://localhost:8180/realms/sutra"),
            (
                "SUTRA_OIDC_BACKCHANNEL_URL",
                "http://keycloak:8180/realms/sutra",
            ),
        ]))
        .unwrap();
        assert_eq!(c.idp.url, "http://keycloak:8180");
        assert_eq!(c.idp.realm, "sutra");
        assert_eq!(c.idp.client_id, "sutra-idp-admin");
        assert!(c.idp.auto_provision && !c.idp.send_actions_email);
        assert!(
            Config::load(&HashMap::from([KC])).is_err(),
            "secret required"
        );
        assert!(Config::load(&HashMap::from([("SUTRA_IDP_PROVISIONING", "ldap")])).is_err());
    }

    #[test]
    fn production_accepts_a_proper_admin_client() {
        let c = prod_with(&[KC, SECRET]).expect("valid");
        assert_eq!(c.idp.mode, IdpMode::Keycloak);
        assert_eq!(c.idp.url, "https://idp.example.edu");
        assert!(prod_with(&[]).is_ok(), "off is allowed");
    }

    #[test]
    fn production_refuses_a_misconfigured_admin_client() {
        for (bad, why) in [
            (vec![("SUTRA_IDP_PROVISIONING", "memory")], "memory adapter"),
            (
                vec![KC, ("SUTRA_IDP_CLIENT_SECRET", "dev-only-change-me")],
                "dev secret",
            ),
            (
                vec![KC, ("SUTRA_IDP_CLIENT_SECRET", "short")],
                "short secret",
            ),
            (
                vec![KC, SECRET, ("SUTRA_IDP_URL", "http://sso.example.edu")],
                "plain http",
            ),
            (
                vec![KC, SECRET, ("SUTRA_IDP_CLIENT_ID", "sutra-web")],
                "sign-in client",
            ),
            (
                vec![KC, SECRET, ("SUTRA_IDP_STARTUP_CHECK", "false")],
                "no start-up check",
            ),
        ] {
            assert!(prod_with(&bad).is_err(), "production must refuse: {why}");
        }
    }

    #[test]
    fn helpers() {
        assert_eq!(
            split_issuer("https://a.b/auth/realms/x/"),
            Some(("https://a.b/auth".into(), "x".into()))
        );
        assert_eq!(split_issuer("https://a.b"), None);
        assert!(https_or_loopback("http://localhost:8180"));
        assert!(https_or_loopback("http://127.0.0.1:8180/x"));
        assert!(!https_or_loopback("http://keycloak:8180"));
        assert!(https_or_loopback("HTTPS://sso"));
    }
}
