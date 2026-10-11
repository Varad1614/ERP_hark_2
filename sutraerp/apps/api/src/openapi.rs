//! OpenAPI document root. Paths/schemas are collected from the module routers (utoipa-axum).

use utoipa::OpenApi;

#[derive(OpenApi)]
#[openapi(
    info(
        title = "SutraERP API",
        version = "1.0.0",
        description = "SutraERP education ERP — synchronous REST/JSON API (major version v1).\n\n\
            Conventions (ADR-L5-0034): camelCase JSON members; URL major versioning (`/api/v1`); \
            RFC 9457 `application/problem+json` errors with `errorCode`, `correlationId` and \
            `validationErrors` extensions; retry-sensitive commands accept `Idempotency-Key`.\n\n\
            Authentication (ADR-L5-0024/0038): dev mode signs in with `POST /api/v1/auth/login` \
            (+ TOTP `POST /api/v1/auth/mfa/verify`); Keycloak mode with the OIDC Authorization \
            Code + PKCE flow `GET /api/v1/auth/oidc/start` → `/oidc/callback`. Both set an opaque \
            `HttpOnly` session cookie; the session's `csrfToken` must be sent as `X-CSRF-Token` \
            on every POST/PUT/PATCH/DELETE made with the cookie. Login and MFA verification \
            instead need the signed double-submit token from `GET /api/v1/auth/csrf`. Unsafe \
            requests from another site (`Origin` / `Sec-Fetch-Site`) are rejected. \
            Tenant/institution scope is taken from the session (ADR-L5-0004); scope headers \
            cannot select or widen it. Admin-sensitive permissions need an MFA-verified session \
            (`mfa-step-up-required`). Missing session: 401; CSRF failure, cross-site request, \
            missing permission or step-up: 403 (`application/problem+json`)."
    ),
    tags(
        (name = "health", description = "Liveness and readiness probes"),
        (name = "auth", description = "D01 authentication boundary (Keycloak-delegated, server-side session)"),
        (name = "platform", description = "A Platform: catalog and module registry"),
        (name = "institution", description = "B Institution (reference aggregate)"),
        (name = "notifications", description = "A Platform: in-app notifications (subject or role addressed, per-reader read state)"),
        (name = "notices", description = "A Platform: institution-wide notices with role audience and publish/expiry window"),
        (name = "todos", description = "A Platform: per-user to-do inbox"),
        (name = "profile", description = "A Platform: self-service profile and notification preferences")
    ),
    components(schemas(sutra_web::Problem)),
    modifiers(&SecuritySchemes)
)]
pub struct ApiDoc;

/// Declares the session cookie and CSRF header security schemes referenced by protected paths.
struct SecuritySchemes;

impl utoipa::Modify for SecuritySchemes {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
        let components = openapi.components.get_or_insert_with(Default::default);
        components.add_security_scheme(
            "session_cookie",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "__Host-sutra_session",
                "Opaque server-side session token (`sutra_session` when Secure cookies are off in dev).",
            ))),
        );
        components.add_security_scheme(
            "login_csrf",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-CSRF-Token",
                "Pre-session double-submit token from GET /api/v1/auth/csrf (must equal the \
                 `__Host-sutra_login_csrf` cookie).",
            ))),
        );
        components.add_security_scheme(
            "csrf_token",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "X-CSRF-Token",
                "Session-bound synchronizer token from login or GET /api/v1/auth/session.",
            ))),
        );
    }
}

/// Pretty JSON of the full generated specification (used by `sutra-api openapi` and CI drift check).
pub fn spec_json() -> Result<String, serde_json::Error> {
    let (_, api) = crate::app::build(crate::app::AppContext::for_spec()).split_for_parts();
    let mut s = serde_json::to_string_pretty(&api)?;
    s.push('\n');
    Ok(s)
}
