//! Part 2 · Stage 7 (S7-entitle): module entitlement settings
//! (`docs/readiness/ENTITLEMENT_IMPLEMENTATION.md`).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SUTRA_ENTITLEMENT_STORE` | `postgres` (`memory` when `SUTRA_ENV=test`) | `postgres`: `platform.tenant_entitlement` · `memory`: no stored entitlements, every tenant gets the default (tests without a database; refused in production) |
//! | `SUTRA_ENTITLEMENT_DEFAULT` | `complete_erp` (registry `defaultPackage`) in development / test; **required** in production | entitlement of a tenant without a stored row: a package key (perpetual, active) or `none` (core only until an entitlement is approved) |
//!
//! The development default keeps every tenant that existed before entitlements (dev seed, demo,
//! CI) on every module. **Production refuses to start without an explicit value**
//! (`SUTRA_ENTITLEMENT_DEFAULT is required in production`): what an institution without a stored
//! licence may use is a commercial decision the operator states, never a silent `complete_erp`.
//! A strict SaaS deployment sets `none`; a single-institution on-premises install sets the
//! package it bought.

use crate::config::{ConfigError, Environment, Source, StoreBackend};
use sutra_kernel::entitlement::{registry, DefaultEntitlement};

/// Entitlement settings (`Config::entitlement`).
#[derive(Clone, Debug)]
pub struct EntitlementConfig {
    pub store: StoreBackend,
    pub default: DefaultEntitlement,
    /// `SUTRA_ENTITLEMENT_DEFAULT` was set (production requires it, [`validate_production`]).
    pub default_explicit: bool,
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
        let explicit = src.get("SUTRA_ENTITLEMENT_DEFAULT");
        let default_explicit = explicit.is_some();
        let raw = explicit.unwrap_or_else(|| reg.default_package.clone());
        let default = DefaultEntitlement::parse(&raw, reg).ok_or_else(|| ConfigError::Invalid {
            key: "SUTRA_ENTITLEMENT_DEFAULT",
            value: raw.clone(),
        })?;
        if env == Environment::Production && store == StoreBackend::Memory {
            return Err(ConfigError::Forbidden(
                "SUTRA_ENTITLEMENT_STORE=memory is not allowed in production",
            ));
        }
        Ok(Self {
            store,
            default,
            default_explicit,
        })
    }
}

/// Production start-up rule (called last by `Config::validate`, so the earlier production rules
/// keep reporting first): the entitlement default must be stated.
pub fn validate_production(c: &EntitlementConfig) -> Result<(), ConfigError> {
    if c.default_explicit {
        Ok(())
    } else {
        Err(ConfigError::RequiredInProduction(
            "SUTRA_ENTITLEMENT_DEFAULT",
        ))
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
        assert!(!c.default_explicit, "registry default");
        let mem = HashMap::from([("SUTRA_ENTITLEMENT_STORE", "memory")]);
        assert!(EntitlementConfig::load(&mem, Environment::Development).is_ok());
        assert!(matches!(
            EntitlementConfig::load(&mem, Environment::Production),
            Err(ConfigError::Forbidden(_))
        ));
    }

    #[test]
    fn production_requires_an_explicit_default() {
        let unset: HashMap<&'static str, &'static str> = HashMap::new();
        let c = EntitlementConfig::load(&unset, Environment::Production).unwrap();
        assert!(matches!(
            validate_production(&c),
            Err(ConfigError::RequiredInProduction(
                "SUTRA_ENTITLEMENT_DEFAULT"
            ))
        ));
        for v in ["complete_erp", "core_academic", "none"] {
            let set = HashMap::from([("SUTRA_ENTITLEMENT_DEFAULT", v)]);
            let c = EntitlementConfig::load(&set, Environment::Production).unwrap();
            assert!(validate_production(&c).is_ok(), "{v}");
        }
        // The whole production configuration refuses to load without it …
        let mut prod = crate::config_http::tests::prod();
        assert!(crate::config::Config::load(&prod).is_ok());
        prod.remove("SUTRA_ENTITLEMENT_DEFAULT");
        assert!(matches!(
            crate::config::Config::load(&prod),
            Err(ConfigError::RequiredInProduction(
                "SUTRA_ENTITLEMENT_DEFAULT"
            ))
        ));
        // … while development / demo keep the complete_erp default.
        let dev = crate::config::Config::load(&unset).unwrap();
        assert_eq!(
            dev.entitlement.default,
            DefaultEntitlement::Package("complete_erp".into())
        );
    }
}
