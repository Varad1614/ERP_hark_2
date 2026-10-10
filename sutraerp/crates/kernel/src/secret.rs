//! Small, audited cryptographic helpers for opaque session material (ADR-L5-0038 §3.4).
//!
//! * session and CSRF tokens are 256-bit values from the OS CSPRNG, hex encoded;
//! * only a SHA-256 digest of a session token is ever persisted;
//! * secret comparisons are constant time ([`ct_eq`]).

use crate::error::KernelError;
use hmac::{Hmac, Mac};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Length in bytes of every generated secret (256 bits).
pub const SECRET_LEN: usize = 32;

/// Fresh 256-bit random value from the operating system CSPRNG.
pub fn random_bytes() -> Result<[u8; SECRET_LEN], KernelError> {
    let mut buf = [0u8; SECRET_LEN];
    getrandom::fill(&mut buf)
        .map_err(|e| KernelError::internal_msg(format!("entropy unavailable: {e}")))?;
    Ok(buf)
}

/// Fresh 256-bit random token, lowercase hex (64 characters).
pub fn random_token() -> Result<String, KernelError> {
    Ok(hex(&random_bytes()?))
}

/// Lowercase hex encoding.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// SHA-256 digest (used as the lookup key for opaque session tokens).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    Sha256::digest(data).into()
}

/// HMAC-SHA-256 of `msg` under `key`.
pub fn hmac_sha256(key: &[u8], msg: &[u8]) -> Result<[u8; 32], KernelError> {
    // HMAC accepts keys of any length; the error arm exists only for fixed-key MACs.
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key)
        .map_err(|e| KernelError::internal_msg(format!("hmac key: {e}")))?;
    mac.update(msg);
    Ok(mac.finalize().into_bytes().into())
}

/// Constant-time equality of two byte strings (length mismatch returns `false`; the length
/// itself is not secret for fixed-size tokens).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_256_bit_hex_and_unique() {
        let a = random_token().unwrap_or_default();
        let b = random_token().unwrap_or_default();
        assert_eq!(a.len(), 64);
        assert!(a.bytes().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(a, b);
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            hex(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn hmac_rfc4231_case_2() {
        assert_eq!(
            hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?").unwrap_or_default()),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
        assert!(!ct_eq(b"", b"a"));
    }
}
