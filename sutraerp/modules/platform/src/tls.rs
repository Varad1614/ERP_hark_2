//! Part 2 · S5-capacity: adds the institution's extra trusted roots (`SUTRA_TLS_CA_FILE`, installed
//! once at start-up in [`sutra_kernel::tls`]) to an outbound HTTP client. The public (webpki)
//! roots stay; verification is never disabled.

/// `builder` plus every certificate of the installed bundle as an additional trusted root.
pub fn with_extra_roots(
    mut builder: reqwest::ClientBuilder,
) -> Result<reqwest::ClientBuilder, String> {
    if let Some(pem) = sutra_kernel::tls::extra_roots_pem() {
        let certs = reqwest::Certificate::from_pem_bundle(pem)
            .map_err(|e| format!("SUTRA_TLS_CA_FILE: {e}"))?;
        for c in certs {
            builder = builder.add_root_certificate(c);
        }
    }
    Ok(builder)
}
