//! Validation of user-supplied web links that the UI renders as `<a href>` or opens with
//! `window.open` (Part 1 · Stage 3 appsec, SEC-P1-APPSEC-004). Only absolute `http(s)` URLs with a
//! host are stored: `javascript:`, `data:`, `vbscript:`, protocol-relative and relative links, and
//! links carrying whitespace or control characters (used to smuggle schemes past naive filters)
//! are refused with a 422.

use crate::error::{FieldViolation, KernelError};

/// Maximum stored link length.
pub const MAX_LINK_LEN: usize = 2000;

/// The trimmed link when it is an absolute `http://` / `https://` URL with a host, else a
/// validation error on `field`.
pub fn http_link(field: &str, raw: &str) -> Result<String, KernelError> {
    let u = raw.trim();
    let rest = u
        .strip_prefix("https://")
        .or_else(|| u.strip_prefix("http://"));
    let ok = match rest {
        Some(r) => {
            u.len() <= MAX_LINK_LEN
                && !r.is_empty()
                && !r.starts_with(['/', '\\', '@', ':', '?', '#'])
                && !u.chars().any(|c| c.is_whitespace() || c.is_control())
        }
        None => false,
    };
    if ok {
        Ok(u.to_owned())
    } else {
        Err(KernelError::Validation(vec![FieldViolation::new(
            field,
            "invalid",
            format!(
                "{field} must be an absolute http(s) link without spaces (≤ {MAX_LINK_LEN} characters)"
            ),
        )]))
    }
}

/// [`http_link`] for an optional member: blank → `None`.
pub fn opt_http_link(field: &str, raw: Option<&str>) -> Result<Option<String>, KernelError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(u) => http_link(field, u).map(Some),
        None => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_links() {
        assert_eq!(
            http_link("url", "  https://library.example.org/a?b=1#c ").unwrap(),
            "https://library.example.org/a?b=1#c"
        );
        assert!(http_link("url", "http://10.0.0.1:8080/x").is_ok());
        assert_eq!(opt_http_link("url", Some("   ")).unwrap(), None);
    }

    #[test]
    fn refuses_script_and_smuggled_schemes() {
        for bad in [
            "javascript:alert(document.cookie)",
            "JavaScript:alert(1)",
            "data:text/html;base64,PHNjcmlwdD5hbGVydCgxKTwvc2NyaXB0Pg==",
            "vbscript:msgbox(1)",
            "//evil.example/x",
            "/relative/path",
            "https://",
            "https:///evil",
            "http://\\evil.example",
            "https://exa mple.org",
            "https://example.org/\u{0}x",
            "java\tscript:alert(1)",
            "ftp://files.example.org/a",
        ] {
            assert!(http_link("url", bad).is_err(), "{bad:?} must be refused");
        }
        let long = format!("https://e.org/{}", "a".repeat(MAX_LINK_LEN));
        assert!(http_link("url", &long).is_err());
    }
}
