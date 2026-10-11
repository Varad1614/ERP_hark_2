//! Authentication boundary (D01). See `sutra_contracts::platform::auth` for the ports.

pub mod dev;
pub mod http;
pub mod http_security;
pub mod http_webauthn;
pub mod keycloak;
pub mod mfa;
pub mod preauth;
pub mod replay;
pub mod session;
pub mod session_pg;
pub mod webauthn;

use std::sync::Arc;
use sutra_contracts::platform::auth::SessionStore;
use sutra_web::{PreSessionCsrf, SessionAuth};

/// Endpoints that establish a session: they cannot present a session CSRF token yet and are
/// protected by the pre-session (login) double-submit token from `GET /api/v1/auth/csrf`.
pub const PRE_SESSION_PATHS: [&str; 4] = [
    "/api/v1/auth/login",
    "/api/v1/auth/mfa/verify",
    "/api/v1/auth/mfa/webauthn/challenge",
    "/api/v1/auth/mfa/webauthn/verify",
];

/// State for the global `sutra_web::auth::authenticate` middleware, backed by this module's
/// session store. `step_up_enforced` is stamped onto every resolved `AuthContext`.
pub fn session_auth(
    sessions: Arc<dyn SessionStore>,
    cookie: &CookieSettings,
    pre_session: PreSessionCsrf,
    step_up_enforced: bool,
) -> SessionAuth {
    SessionAuth::new(
        Arc::new(session::StoreSessionResolver {
            store: sessions,
            step_up_enforced,
        }),
        cookie.name(),
        PRE_SESSION_PATHS,
        pre_session,
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

    /// Browser-binding cookie of an OIDC authorization request. `SameSite=Lax` because the
    /// callback is a cross-site top-level navigation from Keycloak.
    pub fn oidc_cookie_name(&self) -> &'static str {
        if self.secure {
            "__Host-sutra_oidc"
        } else {
            "sutra_oidc"
        }
    }

    pub fn set_oidc_cookie(&self, value: &str, max_age_secs: i64) -> String {
        let mut c = format!(
            "{}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={max_age_secs}",
            self.oidc_cookie_name()
        );
        if self.secure {
            c.push_str("; Secure");
        }
        c
    }

    pub fn clear_oidc_cookie(&self) -> String {
        self.set_oidc_cookie("", 0)
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
