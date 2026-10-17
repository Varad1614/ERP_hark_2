//! Part 2 · S5-capacity: extra trusted root certificates for outbound TLS to the institution's
//! own services (`SUTRA_TLS_CA_FILE`, `docs/readiness/CAPACITY_TEST_REPORT.md`).
//!
//! The HTTP clients (S3 object storage, Keycloak OIDC back channel and admin API, Flowable,
//! OpenSearch) are built with rustls and the bundled Mozilla (webpki) roots only. A college that
//! runs MinIO / Keycloak on its own server behind TLS signed by an **internal CA** could not reach
//! them in production (where `https://` is mandatory for S3 and the IdP admin URL). The composition
//! root installs the configured PEM bundle here once, at start-up, before any adapter is built;
//! the adapters add these roots **in addition to** the public roots (nothing is ever removed and
//! certificate verification is never disabled).
//!
//! The payment gateway client deliberately does not use these roots (it talks to a public
//! provider only).

use std::sync::OnceLock;

static EXTRA_ROOTS: OnceLock<Vec<u8>> = OnceLock::new();

const BEGIN: &str = "-----BEGIN CERTIFICATE-----";
const END: &str = "-----END CERTIFICATE-----";

/// Number of complete `CERTIFICATE` blocks in a PEM bundle (base64 bodies must be non-empty and
/// use only the base64 alphabet); `Err` names the first problem. The DER inside is parsed by
/// the HTTP client when the adapter is built (a broken certificate fails the start-up).
pub fn pem_certificate_count(pem: &[u8]) -> Result<usize, String> {
    let text = std::str::from_utf8(pem).map_err(|_| "the CA bundle is not UTF-8 text (PEM)")?;
    let mut count = 0usize;
    let mut rest = text;
    while let Some(start) = rest.find(BEGIN) {
        let after = &rest[start + BEGIN.len()..];
        let Some(end) = after.find(END) else {
            return Err(format!("certificate {} has no END line", count + 1));
        };
        let body: String = after[..end]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        if body.is_empty()
            || !body
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/' || b == b'=')
        {
            return Err(format!("certificate {} is not base64 PEM", count + 1));
        }
        count += 1;
        rest = &after[end + END.len()..];
    }
    if count == 0 {
        return Err("no PEM CERTIFICATE block found".into());
    }
    Ok(count)
}

/// Installs the extra roots (once per process). Installing the same bundle again is a no-op;
/// a different bundle is refused (the adapters already built would disagree).
pub fn install_extra_roots(pem: Vec<u8>) -> Result<(), String> {
    pem_certificate_count(&pem)?;
    let refused = || Err("a different SUTRA_TLS_CA_FILE bundle is already installed".to_owned());
    if let Some(existing) = EXTRA_ROOTS.get() {
        return if *existing == pem { Ok(()) } else { refused() };
    }
    match EXTRA_ROOTS.set(pem) {
        Ok(()) => Ok(()),
        // Another thread installed first: same bundle is fine.
        Err(pem) if EXTRA_ROOTS.get() == Some(&pem) => Ok(()),
        Err(_) => refused(),
    }
}

/// The installed PEM bundle, if any (adapters add each certificate as a trusted root).
pub fn extra_roots_pem() -> Option<&'static [u8]> {
    EXTRA_ROOTS.get().map(Vec::as_slice)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = "-----BEGIN CERTIFICATE-----\nMIIBszCCAVmgAwIBAgIUQ0FfdGVzdA==\n-----END CERTIFICATE-----\n";

    #[test]
    fn counts_blocks_and_refuses_garbage() {
        assert_eq!(pem_certificate_count(ONE.as_bytes()), Ok(1));
        let two = format!("subject=CN=Sutra CA\n{ONE}{ONE}");
        assert_eq!(pem_certificate_count(two.as_bytes()), Ok(2));
        assert!(pem_certificate_count(b"").is_err(), "empty file");
        assert!(pem_certificate_count(b"not a certificate").is_err());
        assert!(
            pem_certificate_count(b"-----BEGIN CERTIFICATE-----\nMIIB\n").is_err(),
            "missing END"
        );
        assert!(
            pem_certificate_count(b"-----BEGIN CERTIFICATE-----\n\n-----END CERTIFICATE-----\n")
                .is_err(),
            "empty body"
        );
        assert!(
            pem_certificate_count(
                b"-----BEGIN CERTIFICATE-----\nMI*B\n-----END CERTIFICATE-----\n"
            )
            .is_err(),
            "not base64"
        );
        assert!(pem_certificate_count(&[0xff, 0xfe]).is_err(), "not UTF-8");
    }

    #[test]
    fn installs_once_and_refuses_a_different_bundle() {
        assert!(install_extra_roots(b"garbage".to_vec()).is_err());
        install_extra_roots(ONE.as_bytes().to_vec()).unwrap();
        install_extra_roots(ONE.as_bytes().to_vec()).unwrap();
        assert_eq!(extra_roots_pem(), Some(ONE.as_bytes()));
        let other = format!("{ONE}{ONE}");
        assert!(install_extra_roots(other.into_bytes()).is_err());
        assert_eq!(extra_roots_pem(), Some(ONE.as_bytes()));
    }
}
