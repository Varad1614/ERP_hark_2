//! # Domain A — Platform module
//!
//! Scope (Batch 0 + Batch 1 slices A and B):
//! * **Auth (D01 boundary)** — `GET /api/v1/auth/csrf`, `POST /api/v1/auth/login`,
//!   `POST /api/v1/auth/mfa/verify`, `GET /api/v1/auth/session`, `POST /api/v1/auth/logout` with
//!   an opaque server-side session cookie. Two modes (`SUTRA_AUTH_MODE`): **dev** (seeded
//!   personas + real TOTP MFA, enrolment and step-up) and **keycloak** (OIDC Authorization Code +
//!   PKCE back-end-for-frontend: `GET /api/v1/auth/oidc/start|callback`, MFA delegated to
//!   Keycloak and read from `amr`/`acr`, RP-initiated logout) — ADR-L5-0024 / ADR-L5-0038.
//! * **Sessions** — durable `platform.session` store (SHA-256 token digests only, idle + absolute
//!   expiry, revocation) or an in-memory store for no-database development and tests.
//! * **CSRF** — per-session synchronizer token, returned by login and `GET /auth/session` and
//!   enforced on unsafe cookie requests by `sutra_web::auth::authenticate` (wired via
//!   [`auth::session_auth`]).
//! * **Workflow port** — `NoopWorkflowAdapter` behind `WorkflowPort` (Flowable adapter later,
//!   ADR-L5-0023).
//! * **Catalog** — read-only workspace/workflow navigation catalog (99 workspaces / 595 workflows).
//! * **Modules** — enumerates the modular-monolith module descriptors.
//! * **Sweepers** (Batch 1C) — [`sweep::SessionPurge`] and [`sweep::PreAuthPurge`] jobs for the
//!   advisory-locked scheduler in `sutra_persistence::jobs`.
//! * **Self-service** (Batch 1D, [`inbox`]) — profile, notifications (in-app + e-mail channel),
//!   notices, to-do inbox, the outbox-driven inbox consumer and their sweepers; dev-mode
//!   WebAuthn ([`auth::webauthn`]), session list / revoke, OIDC back-channel logout.
//! * **Documents** (Part 2 · S4-docs, [`documents`]) — the document service over S3-compatible
//!   object storage: metadata, integrity, audit, retention / legal hold and its sweeper.
//! * **Entitlements** (Part 2 · S7-entitle, [`entitlement`]) — active modules + licence state of
//!   the tenant, maker-checker entitlement / licence changes, the PostgreSQL entitlement store.

pub mod audit; // 9A
pub mod auth;
pub mod catalog;
pub mod dataimport; // Part 2 · S4-import
pub mod documents; // Part 2 · S4-docs
pub mod entitlement; // Part 2 · S7-entitle
pub mod identity; // Part 2 · S4-idp
pub mod inbox;
pub mod search; // 9C
pub mod sweep;
pub mod workflow;
pub mod workflow_flowable; // 9C

use std::sync::Arc;
use sutra_contracts::platform::auth::{
    CredentialAuthenticator, MfaPolicy, OidcIdentityProvider, PreAuthStore, ReplayGuard,
    SessionStore, WebauthnPort,
};
use sutra_kernel::audit::AuditSink;
use sutra_kernel::module::{ModuleDescriptor, ModuleStatus};
use utoipa_axum::router::OpenApiRouter;

pub const MODULE: ModuleDescriptor = ModuleDescriptor {
    domain: "A",
    key: "platform",
    name: "Platform",
    schema: "platform",
    batch: 1,
    status: ModuleStatus::Active,
};

/// Dependencies injected by the composition root.
#[derive(Clone)]
pub struct PlatformDeps {
    pub authenticator: Arc<dyn CredentialAuthenticator>,
    pub sessions: Arc<dyn SessionStore>,
    pub mfa: Arc<dyn MfaPolicy>,
    pub audit: Arc<dyn AuditSink>,
    pub cookie: auth::CookieSettings,
    pub modules: Vec<ModuleDescriptor>,
    /// Pending OIDC authorization requests and MFA challenges.
    pub pre_auth: Arc<dyn PreAuthStore>,
    /// Dev-mode TOTP service; `None` when MFA is delegated to Keycloak.
    pub mfa_service: Option<auth::mfa::MfaService>,
    /// OIDC identity provider (Keycloak mode); `None` in dev mode.
    pub oidc: Option<Arc<dyn OidcIdentityProvider>>,
    pub webauthn: Arc<dyn WebauthnPort>,
    /// Signed double-submit token for the pre-session endpoints.
    pub pre_session: sutra_web::PreSessionCsrf,
    /// Seen back-channel logout token ids.
    pub replay: Arc<dyn ReplayGuard>,
    /// Profile refresh at sign-in (`None` without PostgreSQL).
    pub sign_in_hook: Option<Arc<dyn auth::http::SignInHook>>,
    /// Failed-attempt lockout for password sign-in and TOTP checks (SEC-9D-005).
    pub throttle: Arc<auth::throttle::AuthThrottle>,
}

/// Platform routes: auth boundary, catalog, and the Batch 1D self-service APIs (profile,
/// notifications, notices, to-dos) over `pool`.
pub fn router(deps: PlatformDeps, pool: sqlx::PgPool) -> OpenApiRouter {
    let modules = deps.modules.clone();
    OpenApiRouter::new()
        .merge(auth::http::router(deps))
        .merge(catalog::router(modules))
        .merge(audit::router(pool.clone())) // 9A
        .merge(inbox::router(pool))
}
