//! Application assembly: infrastructure → modules → middleware stack.

use crate::config::{AuthProvider, Config, StoreBackend};
use crate::openapi::ApiDoc;
use axum::routing::get;
use axum::{Json, Router};
use http::header;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::auth::{
    CredentialAuthenticator, MfaPolicy, SessionPolicy, SessionStore,
};
use sutra_kernel::audit::{AuditSink, TracingAuditSink};
use sutra_kernel::idempotency::{IdempotencyStore, InMemoryIdempotencyStore};
use sutra_kernel::module::ModuleDescriptor;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::{
    audit::PgAuditSink, idempotency::PgIdempotencyStore, pool, readiness::PgReadiness,
};
use sutra_platform::auth::dev::{DevAuthenticator, PrivilegedRoleMfaPolicy};
use sutra_platform::auth::keycloak::{KeycloakAuthenticator, KeycloakSettings};
use sutra_platform::auth::session::InMemorySessionStore;
use sutra_platform::auth::session_pg::PgSessionStore;
use sutra_platform::auth::CookieSettings;
use sutra_platform::PlatformDeps;
use sutra_web::health::Readiness;
use sutra_web::SessionAuth;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::sensitive_headers::SetSensitiveRequestHeadersLayer;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;
use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;

/// Every module composed into this build, in domain order.
pub fn modules() -> Vec<ModuleDescriptor> {
    vec![
        sutra_platform::MODULE,
        sutra_institution::MODULE,
        sutra_student::MODULE,
        sutra_academic::MODULE,
        sutra_people::MODULE,
        sutra_finance::MODULE,
        sutra_campus::MODULE,
        sutra_services::MODULE,
        sutra_exams::MODULE,
        sutra_resources::MODULE,
    ]
}

/// Fully wired dependencies.
pub struct AppContext {
    pub pool: PgPool,
    pub readiness: Readiness,
    pub platform: PlatformDeps,
    /// Global session authentication + CSRF middleware state.
    pub session_auth: SessionAuth,
    pub idempotency: Arc<dyn IdempotencyStore>,
    pub request_timeout: Duration,
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("database configuration: {0}")]
    Database(#[from] sqlx::Error),
    #[error("dev scope configuration: {0}")]
    Scope(String),
    #[error("session configuration: {0}")]
    Session(String),
}

impl AppContext {
    pub fn from_config(cfg: &Config) -> Result<Self, BuildError> {
        let pool = pool::connect_lazy(&pool::PoolSettings {
            url: cfg.database_url.clone(),
            max_connections: cfg.db_max_connections,
            min_connections: 0,
            acquire_timeout: cfg.db_acquire_timeout,
            application_name: "sutra-api".into(),
        })?;
        let readiness = Readiness::new(vec![Arc::new(PgReadiness::new(pool.clone()))]);
        let mfa = PrivilegedRoleMfaPolicy {
            enforce: cfg.dev_mfa_enforced,
        };
        let authenticator: Arc<dyn CredentialAuthenticator> = match cfg.auth_provider {
            AuthProvider::Dev => {
                let scope =
                    ExecutionScope::parse(Some(&cfg.dev_tenant_id), Some(&cfg.dev_institution_id))
                        .map_err(|e| BuildError::Scope(e.to_string()))?;
                tracing::warn!(
                    "using the DEV credential authenticator — never enable outside development"
                );
                Arc::new(DevAuthenticator::new(
                    cfg.dev_auth_password.clone(),
                    scope,
                    Box::new(PrivilegedRoleMfaPolicy {
                        enforce: cfg.dev_mfa_enforced,
                    }),
                ))
            }
            AuthProvider::Keycloak => Arc::new(KeycloakAuthenticator::new(KeycloakSettings {
                base_url: cfg.keycloak_url.clone(),
                realm: cfg.keycloak_realm.clone(),
                client_id: cfg.keycloak_client_id.clone(),
            })),
        };
        let audit: Arc<dyn AuditSink> = match cfg.audit_sink {
            StoreBackend::Postgres => Arc::new(PgAuditSink::new(pool.clone())),
            StoreBackend::Memory => Arc::new(TracingAuditSink),
        };
        let idempotency: Arc<dyn IdempotencyStore> = match cfg.idempotency_store {
            StoreBackend::Postgres => Arc::new(PgIdempotencyStore::new(pool.clone())),
            StoreBackend::Memory => Arc::new(InMemoryIdempotencyStore::default()),
        };
        let mfa: Arc<dyn MfaPolicy> = Arc::new(mfa);
        let policy = SessionPolicy::new(cfg.session_idle_secs, cfg.session_ttl_secs)
            .map_err(|e| BuildError::Session(e.to_string()))?;
        let sessions: Arc<dyn SessionStore> = match cfg.session_store {
            StoreBackend::Postgres => Arc::new(PgSessionStore::new(pool.clone(), policy)),
            StoreBackend::Memory => Arc::new(InMemorySessionStore::new(policy)),
        };
        let cookie = CookieSettings {
            secure: cfg.session_cookie_secure,
            max_age_secs: cfg.session_ttl_secs,
        };
        let session_auth = sutra_platform::auth::session_auth(sessions.clone(), &cookie);
        Ok(Self {
            pool,
            readiness,
            platform: PlatformDeps {
                authenticator,
                sessions,
                mfa,
                audit,
                cookie,
                modules: modules(),
            },
            session_auth,
            idempotency,
            request_timeout: cfg.request_timeout,
        })
    }

    /// Self-contained context with in-memory stores and a never-contacted lazy pool. Used by
    /// tests and OpenAPI generation; needs no database.
    pub fn for_spec() -> Self {
        let mut src: std::collections::HashMap<&'static str, &'static str> =
            std::collections::HashMap::new();
        src.insert("SUTRA_ENV", "test");
        src.insert("DATABASE_URL", "postgres://sutra:sutra@127.0.0.1:1/sutra");
        src.insert("SUTRA_DB_ACQUIRE_TIMEOUT_SECS", "1");
        #[allow(clippy::expect_used)]
        let cfg = Config::load(&src).expect("static test configuration is valid");
        #[allow(clippy::expect_used)]
        Self::from_config(&cfg).expect("static test context builds")
    }
}

/// Builds the router plus the generated OpenAPI document.
///
/// Layer order (outermost first): session authentication + CSRF → idempotency → handlers. The
/// idempotency middleware keys its records by the authenticated owner, so it must run inside
/// the authentication layer.
pub fn build(ctx: AppContext) -> OpenApiRouter {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(sutra_web::health::router(ctx.readiness.clone()))
        .merge(sutra_platform::router(ctx.platform))
        .merge(sutra_institution::router(ctx.pool.clone()))
        .merge(sutra_student::router())
        .merge(sutra_academic::router())
        .merge(sutra_people::router())
        .merge(sutra_finance::router())
        .merge(sutra_campus::router())
        .merge(sutra_services::router())
        .merge(sutra_exams::router())
        .merge(sutra_resources::router())
        .layer(axum::middleware::from_fn_with_state(
            ctx.idempotency,
            sutra_web::idempotency::idempotency,
        ))
        .layer(axum::middleware::from_fn_with_state(
            ctx.session_auth,
            sutra_web::auth::authenticate,
        ))
}

/// Final Axum application with the full middleware stack (outermost last).
pub fn router(ctx: AppContext) -> Router {
    let timeout = ctx.request_timeout;
    let (router, api) = build(ctx).split_for_parts();
    let api = Arc::new(api);
    router
        .route(
            "/api/openapi.json",
            get(move || {
                let api = api.clone();
                async move { Json((*api).clone()) }
            }),
        )
        .fallback(sutra_web::problem::not_found)
        .layer(TimeoutLayer::with_status_code(
            http::StatusCode::GATEWAY_TIMEOUT,
            timeout,
        ))
        .layer(RequestBodyLimitLayer::new(2 * 1024 * 1024))
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http().make_span_with(crate::telemetry::make_span))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            header::COOKIE,
            header::SET_COOKIE,
        ]))
        .layer(axum::middleware::from_fn(sutra_web::request_id::request_id))
}
