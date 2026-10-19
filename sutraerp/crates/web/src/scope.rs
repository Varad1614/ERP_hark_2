//! Tenant/Institution execution scope of a request (ADR-L5-0004).
//!
//! The authenticated session is the **only** scope authority: [`Scope`] yields the scope bound to
//! the caller's server-side session (see [`crate::auth`]). Client-supplied
//! `X-Sutra-Tenant-Id` / `X-Sutra-Institution-Id` headers never select, switch or widen it; when
//! present they must agree with the session or the request is rejected with 403. Requests
//! without a live session fail closed with 401 — there is no default tenant.

use crate::auth::Authenticated;
use crate::problem::ApiError;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use sutra_kernel::scope::ExecutionScope;

/// Legacy scope headers; only checked for consistency with the session scope.
pub const TENANT_HEADER: &str = "x-sutra-tenant-id";
pub const INSTITUTION_HEADER: &str = "x-sutra-institution-id";

/// Extractor yielding the session-bound [`ExecutionScope`] of the request.
#[derive(Debug, Clone, Copy)]
pub struct Scope(pub ExecutionScope);

impl<S: Send + Sync> FromRequestParts<S> for Scope {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Authenticated(ctx) = Authenticated::from_request_parts(parts, state).await?;
        Ok(Scope(ctx.scope))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;
    use chrono::Utc;
    use sutra_kernel::auth::{AuthContext, Role};
    use sutra_kernel::scope::TenantId;

    async fn extract(req: Request<()>) -> Result<Scope, ApiError> {
        let (mut parts, _) = req.into_parts();
        Scope::from_request_parts(&mut parts, &()).await
    }

    #[tokio::test]
    async fn fails_closed_without_session_even_with_headers() {
        let r = extract(
            Request::builder()
                .header(TENANT_HEADER, "0192f5a0-7c3e-7000-8000-000000000001")
                .body(())
                .unwrap_or_default(),
        )
        .await;
        assert!(matches!(
            r,
            Err(ApiError(sutra_kernel::error::KernelError::Unauthenticated))
        ));
    }

    #[tokio::test]
    async fn scope_comes_from_the_session() {
        let scope = ExecutionScope::tenant(TenantId::new());
        let mut req = Request::builder().body(()).unwrap_or_default();
        req.extensions_mut().insert(AuthContext {
            session_id: uuid::Uuid::now_v7(),
            subject: "s".into(),
            login_id: "l".into(),
            display_name: "d".into(),
            roles: vec![Role::Student],
            scope,
            csrf_token: String::new(),
            expires_at: Utc::now(),
            idle_expires_at: Utc::now(),
            mfa_level: sutra_kernel::auth::MfaLevel::None,
            amr: vec![],
            step_up_enforced: false,
            privileged_mfa_enforced: false,
        });
        let r = extract(req).await;
        assert!(r.is_ok_and(|s| s.0 == scope));
    }
}
