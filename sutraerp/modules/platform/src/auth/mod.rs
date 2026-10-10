//! Authentication boundary (D01). See `sutra_contracts::platform::auth` for the ports.

pub mod dev;
pub mod http;
pub mod keycloak;
pub mod session;
pub mod session_pg;

use std::sync::Arc;
use sutra_contracts::platform::auth::SessionStore;
use sutra_web::SessionAuth;

/// Endpoints that establish a session and therefore cannot present a CSRF token yet.
pub const CSRF_EXEMPT_PATHS: [&str; 2] = ["/api/v1/auth/login", "/api/v1/auth/mfa/verify"];

/// State for the global `sutra_web::auth::authenticate` middleware, backed by this module's
/// session store.
pub fn session_auth(sessions: Arc<dyn SessionStore>, cookie: &CookieSettings) -> SessionAuth {
    SessionAuth::new(
        Arc::new(session::StoreSessionResolver(sessions)),
        cookie.name(),
        CSRF_EXEMPT_PATHS,
    )
}

/// Session cookie settings (ADR-L5-0038 §3.4: `Secure`, `HttpOnly`, same-site governed).
#[derive(Clone, Debug)]
pub struct CookieSettings {
    /// `Secure` attribute. Only disable for plain-HTTP non-localhost dev setups.
    pub secure: bool,
    pub max_age_secs: i64,
}

impl CookieSettings {
    /// `__Host-` prefix pins the cookie to the exact origin; it requires `Secure` + `Path=/`.
    pub fn name(&self) -> &'static str {
        if self.secure {
            "__Host-sutra_session"
        } else {
            "sutra_session"
        }
    }

    pub fn set_cookie(&self, token: &str) -> String {
        let mut c = format!(
            "{}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
            self.name(),
            token,
            self.max_age_secs
        );
        if self.secure {
            c.push_str("; Secure");
        }
        c
    }

    pub fn clear_cookie(&self) -> String {
        let mut c = format!(
            "{}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0",
            self.name()
        );
        if self.secure {
            c.push_str("; Secure");
        }
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_attributes() {
        let s = CookieSettings {
            secure: true,
            max_age_secs: 3600,
        };
        let c = s.set_cookie("abc");
        assert!(c.starts_with("__Host-sutra_session=abc"));
        for attr in [
            "HttpOnly",
            "Secure",
            "SameSite=Strict",
            "Path=/",
            "Max-Age=3600",
        ] {
            assert!(c.contains(attr), "{attr} missing in {c}");
        }
        let insecure = CookieSettings {
            secure: false,
            max_age_secs: 60,
        };
        assert!(!insecure.set_cookie("x").contains("Secure"));
        assert!(insecure.clear_cookie().contains("Max-Age=0"));
    }
}
