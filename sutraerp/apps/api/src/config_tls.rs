//! Part 2 · S5-capacity: `SUTRA_TLS_CA_FILE` — the institution's own CA certificates (PEM bundle)
//! trusted **in addition to** the public roots by the outbound HTTP clients of internal services
//! (S3 object storage, Keycloak OIDC back channel + admin API, Flowable, OpenSearch).
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SUTRA_TLS_CA_FILE` | — | path of a PEM file with one or more CA certificates |
//!
//! On a college server MinIO and Keycloak are usually served with certificates of an internal
//! CA; production requires `https://` for both, so without this setting the API could not start
//! (it would refuse the certificate). The file is read and checked at configuration load (a
//! missing, empty or non-PEM file is refused in every environment) and installed once by
//! `AppContext::from_config` before any adapter is built; a certificate the TLS library cannot
//! parse fails the start-up. Verification is never relaxed. The payment gateway keeps the public
//! roots only.

use crate::config::{ConfigError, Source};

/// Extra trusted roots, when configured.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TlsTrustConfig {
    /// `SUTRA_TLS_CA_FILE`.
    pub ca_file: Option<String>,
    /// The bundle read from `ca_file` (PEM).
    pub pem: Option<Vec<u8>>,
    /// Number of certificates in the bundle.
    pub certificates: usize,
}

impl TlsTrustConfig {
    pub fn load(src: &dyn Source) -> Result<Self, ConfigError> {
        let Some(path) = src.get("SUTRA_TLS_CA_FILE") else {
            return Ok(Self::default());
        };
        let invalid = |why: String| ConfigError::Invalid {
            key: "SUTRA_TLS_CA_FILE",
            value: format!("{path}: {why}"),
        };
        let pem = std::fs::read(&path).map_err(|e| invalid(e.to_string()))?;
        let certificates = sutra_kernel::tls::pem_certificate_count(&pem).map_err(invalid)?;
        Ok(Self {
            ca_file: Some(path),
            pem: Some(pem),
            certificates,
        })
    }

    /// Installs the bundle for every adapter built afterwards (once per process).
    pub fn install(&self) -> Result<(), String> {
        match &self.pem {
            Some(pem) => sutra_kernel::tls::install_extra_roots(pem.clone()),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct Map(HashMap<&'static str, String>);
    impl Source for Map {
        fn get(&self, key: &str) -> Option<String> {
            self.0.get(key).cloned()
        }
    }

    const PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIBszCCAVmgAwIBAgIUQ0FfdGVzdA==\n-----END CERTIFICATE-----\n";

    /// Checked-in fixtures (`tests/fixtures/tls/`): no files are written at test time (no shared
    /// temporary directory, semgrep `rust.lang.security.temp-dir`). `two-blocks.pem` holds `PEM` twice.
    fn fixture(name: &str) -> String {
        format!("{}/tests/fixtures/tls/{name}", env!("CARGO_MANIFEST_DIR"))
    }

    #[test]
    fn unset_means_public_roots_only() {
        let c = TlsTrustConfig::load(&Map(HashMap::new())).unwrap();
        assert_eq!(c, TlsTrustConfig::default());
        assert!(c.install().is_ok(), "nothing to install");
    }

    #[test]
    fn reads_a_pem_bundle_and_refuses_missing_or_garbage_files() {
        let ok = fixture("two-blocks.pem");
        assert_eq!(std::fs::read_to_string(&ok).unwrap(), format!("{PEM}{PEM}"));
        let c =
            TlsTrustConfig::load(&Map(HashMap::from([("SUTRA_TLS_CA_FILE", ok.clone())]))).unwrap();
        assert_eq!(c.certificates, 2);
        assert_eq!(c.ca_file.as_deref(), Some(ok.as_str()));
        for name in ["empty.pem", "not-pem.txt"] {
            let p = fixture(name);
            let e = TlsTrustConfig::load(&Map(HashMap::from([("SUTRA_TLS_CA_FILE", p)])));
            assert!(
                matches!(
                    e,
                    Err(ConfigError::Invalid {
                        key: "SUTRA_TLS_CA_FILE",
                        ..
                    })
                ),
                "{name}: {e:?}"
            );
        }
        let missing = TlsTrustConfig::load(&Map(HashMap::from([(
            "SUTRA_TLS_CA_FILE",
            "/nonexistent/sutra-ca.pem".to_owned(),
        )])));
        assert!(matches!(
            missing,
            Err(ConfigError::Invalid {
                key: "SUTRA_TLS_CA_FILE",
                ..
            })
        ));
    }
}
