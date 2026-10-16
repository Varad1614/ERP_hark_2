//! Part 2 · Stage 7 (S7-entitle): module entitlement settings
//! (`docs/readiness/ENTITLEMENT_IMPLEMENTATION.md`).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SUTRA_ENTITLEMENT_STORE` | `postgres` (`memory` when `SUTRA_ENV=test`) | `postgres`: `platform.tenant_entitlement` · `memory`: no stored entitlements, every tenant gets the default (tests without a database; refused in production) |
//! | `SUTRA_ENTITLEMENT_DEFAULT` | `complete_erp` (registry `defaultPackage`) | entitlement of a tenant without a stored row: a package key (perpetual, active) or `none` (core only until an entitlement is approved) |
//!
//! The default keeps every tenant that existed before entitlements (dev seed, demo, CI, pilot)
//! on every module. A strict SaaS deployment sets `SUTRA_ENTITLEMENT_DEFAULT=none`.

use crate::config::{ConfigError, Environment, Source, StoreBackend};
use sutra_kernel::entitlement::{registry, DefaultEntitlement};

/// Entitlement settings (`Config::entitlement`).
#[derive(Clone, Debug)]
pub struct EntitlementConfig {
    pub store: StoreBackend,
    pub default: DefaultEntitlement,
}

impl EntitlementConfig {
    pub fn load(src: &dyn Source, env: Environment) -> Result<Self, ConfigError> {
        let reg = registry().map_err(|_| ConfigError::Invalid {
            key: "catalog/entitlements.json",
            value: "the embedded entitlement registry is invalid".into(),
        })?;
        let store = match src
            .get("SUTRA_ENTITLEMENT_STORE")
            .as_deref()
            .map(|s| s.trim().to_ascii_lowercase())
            .as_deref()
        {
            None if env == Environment::Test => StoreBackend::Memory,
            None | Some("postgres") => StoreBackend::Postgres,
            Some("memory") => StoreBackend::Memory,
            Some(v) => {
                return Err(ConfigError::Invalid {
                    key: "SUTRA_ENTITLEMENT_STORE",
                    value: v.to_owned(),
                })
            }
        };
        let raw = src
            .get("SUTRA_ENTITLEMENT_DEFAULT")
            .unwrap_or_else(|| reg.default_package.clone());
        let default = DefaultEntitlement::parse(&raw, reg).ok_or_else(|| ConfigError::Invalid {
            key: "SUTRA_ENTITLEMENT_DEFAULT",
            value: raw.clone(),
        })?;
        if env == Environment::Production && store == StoreBackend::Memory {
            return Err(ConfigError::Forbidden(
                "SUTRA_ENTITLEMENT_STORE=memory is not allowed in production",
            ));
        }
        Ok(Self { store, default })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn defaults_strict_parse_and_production_rule() {
        let none: HashMap<&'static str, &'static str> = HashMap::new();
        let c = EntitlementConfig::load(&none, Environment::Development).unwrap();
        assert_eq!(c.store, StoreBackend::Postgres);
        assert_eq!(
            c.default,
            DefaultEntitlement::Package("complete_erp".into())
        );
        let t = EntitlementConfig::load(&none, Environment::Test).unwrap();
        assert_eq!(t.store, StoreBackend::Memory);
        let strict = HashMap::from([("SUTRA_ENTITLEMENT_DEFAULT", "none")]);
        assert_eq!(
            EntitlementConfig::load(&strict, Environment::Production)
                .unwrap()
                .default,
            DefaultEntitlement::Nothing
        );
        let pkg = HashMap::from([("SUTRA_ENTITLEMENT_DEFAULT", "core_academic")]);
        assert_eq!(
            EntitlementConfig::load(&pkg, Environment::Development)
                .unwrap()
                .default,
            DefaultEntitlement::Package("core_academic".into())
        );
        for (k, v) in [
            ("SUTRA_ENTITLEMENT_DEFAULT", "platinum"),
            ("SUTRA_ENTITLEMENT_STORE", "redis"),
        ] {
            assert!(
                EntitlementConfig::load(&HashMap::from([(k, v)]), Environment::Development)
                    .is_err(),
                "{k}={v}"
            );
        }
        let mem = HashMap::from([("SUTRA_ENTITLEMENT_STORE", "memory")]);
        assert!(EntitlementConfig::load(&mem, Environment::Development).is_ok());
        assert!(matches!(
            EntitlementConfig::load(&mem, Environment::Production),
            Err(ConfigError::Forbidden(_))
        ));
    }
}
