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
            Authentication (ADR-L5-0038): `POST /api/v1/auth/login` sets an opaque `HttpOnly` \
            session cookie and returns a `csrfToken`; every POST/PUT/PATCH/DELETE made with the \
            cookie must send it as `X-CSRF-Token` (login and MFA verification excepted). \
            Tenant/institution scope is taken from the session (ADR-L5-0004); scope headers \
            cannot select or widen it. Missing session: 401; CSRF failure or missing \
            permission: 403 (`application/problem+json`)."
    ),
    tags(
        (name = "health", description = "Liveness and readiness probes"),
        (name = "auth", description = "D01 authentication boundary (Keycloak-delegated, server-side session)"),
        (name = "platform", description = "A Platform: catalog and module registry"),
        (name = "institution", description = "B Institution (reference aggregate)")
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
