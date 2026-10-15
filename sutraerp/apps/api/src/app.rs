//! Application assembly: infrastructure → modules → middleware stack.

use crate::config::{
    AuthProvider, CacheBackend, Config, MailMode, OutboxRelayMode, SmtpTlsMode, StoreBackend,
};
use crate::openapi::ApiDoc;
use axum::routing::get;
use axum::{Json, Router};
use http::header;
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::auth::{
    CredentialAuthenticator, MfaFactorStore, MfaPolicy, OidcIdentityProvider, PreAuthStore,
    ReplayGuard, SessionPolicy, SessionStore, WebauthnPort,
};
use sutra_contracts::platform::workflow::WorkflowPort;
use sutra_contracts::services::ports::OperationalMetricsPort;
use sutra_infra::nats::{JetStreamPublisher, JetStreamSettings, NatsReadiness};
use sutra_infra::smtp::{SmtpMailSender, SmtpSettings, SmtpTls};
use sutra_infra::valkey::{ValkeyCache, ValkeyReadiness, ValkeySettings};
use sutra_kernel::approval::ApprovalMode;
use sutra_kernel::audit::{AuditSink, TracingAuditSink};
use sutra_kernel::cache::{AdvisoryCache, Cache, CacheNamespace, InMemoryCache, NoCache};
use sutra_kernel::health::ReadinessCheck;
use sutra_kernel::idempotency::{IdempotencyStore, InMemoryIdempotencyStore};
use sutra_kernel::mail::{MailSender, NoopMailSender};
use sutra_kernel::module::ModuleDescriptor;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::backoff::Backoff;
use sutra_persistence::jobs::{IdempotencyPurge, OutboxPurge, Scheduler};
use sutra_persistence::outbox_relay::{OutboxRelay, RelaySettings};
use sutra_persistence::{
    audit::PgAuditSink, idempotency::PgIdempotencyStore, pool, readiness::PgReadiness,
};
use sutra_platform::auth::dev::{DevAuthenticator, PrivilegedRoleMfaPolicy};
use sutra_platform::auth::keycloak::{KeycloakAuthenticator, KeycloakSettings};
use sutra_platform::auth::mfa::{
    DeferredWebauthn, InMemoryMfaFactorStore, MfaService, PgMfaFactorStore, SecretBox,
};
use sutra_platform::auth::preauth::{InMemoryPreAuthStore, PgPreAuthStore};
use sutra_platform::auth::replay::{InMemoryReplayGuard, PgReplayGuard};
use sutra_platform::auth::session::InMemorySessionStore;
use sutra_platform::auth::session_pg::PgSessionStore;
use sutra_platform::auth::webauthn::{
    DevWebauthn, InMemoryWebauthnStore, PgWebauthnStore, WebauthnCredentialStore, WebauthnSettings,
};
use sutra_platform::auth::CookieSettings;
use sutra_platform::inbox::consumer::InboxProjector;
use sutra_platform::inbox::delivery::NotificationDelivery;
use sutra_platform::inbox::jobs::{EventReceiptPurge, NoticePurge, NotificationPurge, TodoPurge};
use sutra_platform::inbox::profile::ProfileSignInHook;
use sutra_platform::sweep::{PreAuthPurge, SessionPurge};
use sutra_platform::PlatformDeps;
use sutra_web::health::Readiness;
use sutra_web::{OriginPolicy, PreSessionCsrf, SessionAuth};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
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
    /// Cross-site `Origin` / `Sec-Fetch-Site` guard for unsafe requests.
    pub origin_policy: OriginPolicy,
    pub idempotency: Arc<dyn IdempotencyStore>,
    pub request_timeout: Duration,
    /// Advisory cache handed to modules (ADR-L5-0037).
    pub cache: AdvisoryCache,
    /// Workflow orchestration port for approval steps (no-op adapter until Flowable, ADR-L5-0023).
    pub workflow: Arc<dyn WorkflowPort>,
    /// How every module's approval steps resolve (`SUTRA_APPROVAL_MODE`; default human decision,
    /// auto-approval only as an explicit dev/test opt-in — refused in production).
    pub approvals: ApprovalMode,
    /// Outbox relay + sweepers; started by `serve` via [`AppContext::take_background`].
    pub background: BackgroundTasks,
    /// 9A: audit anchor key handed to the audit chain API (`SUTRA_AUDIT_ANCHOR_KEY`).
    pub audit_anchor: Option<sutra_persistence::audit_chain::AnchorKey>,
    /// OpenSearch projections (`GET /api/v1/platform/search`); disabled unless `SUTRA_SEARCH=opensearch`.
    pub search: sutra_platform::search::SearchState, // 9C
    /// INT: SEC-9D-003 — payment gateway of the finance routes (`SUTRA_PAYMENT_GATEWAY`).
    pub payment_gateway: Arc<dyn sutra_contracts::finance::ports::PaymentGatewayPort>,
}

/// Long-running workers that run next to the HTTP server.
#[derive(Default)]
pub struct BackgroundTasks {
    pub relay: Option<OutboxRelay>,
    pub scheduler: Option<Scheduler>,
    /// Inbox consumer + notification e-mail delivery (Batch 1D).
    pub consumers: Option<Scheduler>,
    /// 9A: audit sealer / anchor jobs, `/metrics` listener + gauges, OTLP flush on shutdown.
    pub observability: Option<ObservabilityTasks>,
}

impl BackgroundTasks {
    /// Names of the configured workers (for logs and tests).
    pub fn describe(&self) -> Vec<String> {
        let mut v = vec![];
        if let Some(r) = &self.relay {
            v.push(format!("outbox-relay(batch={})", r.settings().batch_size));
        }
        if let Some(s) = &self.scheduler {
            v.extend(s.job_names().into_iter().map(str::to_owned));
        }
        if let Some(s) = &self.consumers {
            v.extend(s.job_names().into_iter().map(str::to_owned));
        }
        if let Some(o) = &self.observability {
            v.extend(o.describe()); // 9A
        }
        v
    }

    /// Spawns every worker; all stop when `cancel` fires.
    pub fn spawn(self, cancel: &CancellationToken) -> Vec<JoinHandle<()>> {
        let mut handles = vec![];
        if let Some(relay) = self.relay {
            handles.push(tokio::spawn(relay.run(cancel.clone())));
        }
        if let Some(s) = self.scheduler {
            handles.extend(s.spawn(cancel.clone()));
        }
        if let Some(s) = self.consumers {
            handles.extend(s.spawn(cancel.clone()));
        }
        if let Some(o) = self.observability {
            handles.extend(o.spawn(cancel)); // 9A
        }
        handles
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("database configuration: {0}")]
    Database(#[from] sqlx::Error),
    #[error("dev scope configuration: {0}")]
    Scope(String),
    #[error("session configuration: {0}")]
    Session(String),
    #[error("auth configuration: {0}")]
    Auth(String),
    #[error("messaging configuration: {0}")]
    Messaging(String),
    #[error("cache configuration: {0}")]
    Cache(String),
    #[error("mail configuration: {0}")]
    Mail(String),
    #[error("data platform configuration: {0}")]
    DataPlatform(String), // 9C
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
        let mut checks: Vec<Arc<dyn ReadinessCheck>> =
            vec![Arc::new(PgReadiness::new(pool.clone()))];
        let (cache, cache_check) = build_cache(cfg)?;
        checks.extend(cache_check);
        let (relay, relay_check) = build_relay(cfg, &pool)?;
        checks.extend(relay_check);
        let readiness = Readiness::new(checks);
        let scheduler = cfg.sweepers.enabled.then(|| build_scheduler(cfg, &pool));
        let mailer = build_mailer(cfg)?;
        let consumers = cfg
            .inbox
            .consumer
            .then(|| build_consumers(cfg, &pool, mailer.clone()));
        // ---- 9C ---- workflow adapter + decision sync, search projector / rebuild (see wire_9c)
        let (workflow, search, consumers) = wire_9c(cfg, &pool, consumers)?;
        // ---- end 9C ----
        let mfa = PrivilegedRoleMfaPolicy {
            enforce: cfg.dev_mfa_enforced,
        };
        let (authenticator, oidc): (
            Arc<dyn CredentialAuthenticator>,
            Option<Arc<dyn OidcIdentityProvider>>,
        ) = match cfg.auth_provider {
            AuthProvider::Dev => {
                let scope =
                    ExecutionScope::parse(Some(&cfg.dev_tenant_id), Some(&cfg.dev_institution_id))
                        .map_err(|e| BuildError::Scope(e.to_string()))?;
                tracing::warn!(
                    "using the DEV credential authenticator — never enable outside development"
                );
                (
                    Arc::new(DevAuthenticator::new(cfg.dev_auth_password.clone(), scope)),
                    None,
                )
            }
            AuthProvider::Keycloak => {
                let o = &cfg.oidc;
                let mut settings = KeycloakSettings::new(
                    &o.issuer,
                    &o.client_id,
                    &o.client_secret,
                    &o.redirect_uri,
                );
                settings.backchannel_url = o.backchannel_url.clone();
                settings.post_logout_redirect_uri = o.post_logout_redirect_uri.clone();
                settings.tenant_claim = o.tenant_claim.clone();
                settings.institution_claim = o.institution_claim.clone();
                settings.mfa_acr_values = o.mfa_acr_values.clone();
                settings.step_up_acr = o.step_up_acr.clone();
                let kc = Arc::new(
                    KeycloakAuthenticator::new(settings)
                        .map_err(|e| BuildError::Auth(e.to_string()))?,
                );
                (kc.clone(), Some(kc))
            }
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
            .and_then(|p| p.with_touch_interval(cfg.session_touch_secs))
            .map_err(|e| BuildError::Session(e.to_string()))?;
        // Session, pre-auth, MFA-factor, passkey and replay records share the session store backend.
        type Stores = (
            Arc<dyn SessionStore>,
            Arc<dyn PreAuthStore>,
            Arc<dyn MfaFactorStore>,
            Arc<dyn WebauthnCredentialStore>,
            Arc<dyn ReplayGuard>,
        );
        let (sessions, pre_auth, factors, passkeys, replay): Stores = match cfg.session_store {
            StoreBackend::Postgres => (
                Arc::new(PgSessionStore::new(pool.clone(), policy)),
                Arc::new(PgPreAuthStore::new(pool.clone())),
                Arc::new(PgMfaFactorStore::new(pool.clone())),
                Arc::new(PgWebauthnStore::new(pool.clone())),
                Arc::new(PgReplayGuard::new(pool.clone())),
            ),
            StoreBackend::Memory => (
                Arc::new(InMemorySessionStore::new(policy)),
                Arc::new(InMemoryPreAuthStore::default()),
                Arc::new(InMemoryMfaFactorStore::default()),
                Arc::new(InMemoryWebauthnStore::default()),
                Arc::new(InMemoryReplayGuard::default()),
            ),
        };
        // Dev mode runs its own WebAuthn relying party; Keycloak mode delegates passkeys.
        let webauthn: Arc<dyn WebauthnPort> = match cfg.auth_provider {
            AuthProvider::Dev => Arc::new(
                DevWebauthn::new(
                    &WebauthnSettings {
                        rp_id: cfg.webauthn.rp_id.clone(),
                        rp_name: cfg.webauthn.rp_name.clone(),
                        origins: cfg.webauthn.origins.clone(),
                    },
                    passkeys,
                    pre_auth.clone(),
                )
                .map_err(|e| BuildError::Auth(e.to_string()))?,
            ),
            AuthProvider::Keycloak => Arc::new(DeferredWebauthn),
        };
        let sign_in_hook: Option<Arc<dyn sutra_platform::auth::http::SignInHook>> =
            (cfg.session_store == StoreBackend::Postgres)
                .then(|| Arc::new(ProfileSignInHook { pool: pool.clone() }) as _);
        // Dev mode verifies TOTP itself; Keycloak mode delegates MFA to Keycloak.
        let mfa_service = match cfg.auth_provider {
            AuthProvider::Dev => Some(MfaService::new(
                factors,
                SecretBox::new(&cfg.mfa_encryption_key.0)
                    .map_err(|e| BuildError::Auth(e.to_string()))?,
                cfg.mfa_issuer.clone(),
            )),
            AuthProvider::Keycloak => None,
        };
        let csrf_key = match cfg.csrf_key {
            Some(k) => k.0,
            None => {
                tracing::info!("SUTRA_CSRF_KEY unset: using a per-process random login-CSRF key");
                sutra_kernel::secret::random_bytes().map_err(|e| BuildError::Auth(e.to_string()))?
            }
        };
        let pre_session =
            PreSessionCsrf::new(csrf_key, cfg.session_cookie_secure, cfg.login_csrf_ttl_secs);
        let cookie = CookieSettings {
            secure: cfg.session_cookie_secure,
            max_age_secs: cfg.session_ttl_secs,
        };
        let session_auth = sutra_platform::auth::session_auth(
            sessions.clone(),
            &cookie,
            pre_session.clone(),
            cfg.mfa_step_up,
        );
        let observability = Some(build_observability(cfg, &pool)); // 9A
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
                pre_auth,
                mfa_service,
                oidc,
                webauthn,
                pre_session,
                replay,
                sign_in_hook,
            },
            session_auth,
            origin_policy: OriginPolicy::new(&cfg.allowed_origins),
            idempotency,
            request_timeout: cfg.request_timeout,
            cache,
            workflow, // 9C: noop unless SUTRA_WORKFLOW=flowable
            approvals: cfg.hardening.workflow.approvals, // Part 1: DEF-X-001
            audit_anchor: anchor_key(cfg), // 9A
            background: BackgroundTasks {
                relay,
                scheduler,
                consumers,
                observability, // 9A
            },
            search,                                // 9C
            payment_gateway: payment_gateway(cfg), // INT: SEC-9D-003
        })
    }

    /// Moves the background workers out (the rest of the context becomes the router).
    pub fn take_background(&mut self) -> BackgroundTasks {
        std::mem::take(&mut self.background)
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

/// A built adapter plus its optional readiness check.
type WithCheck<T> = (T, Option<Arc<dyn ReadinessCheck>>);

/// Advisory cache from `SUTRA_CACHE` (+ its non-critical readiness check for Valkey).
fn build_cache(cfg: &Config) -> Result<WithCheck<AdvisoryCache>, BuildError> {
    let c = &cfg.cache;
    let (inner, check): (Arc<dyn Cache>, Option<Arc<dyn ReadinessCheck>>) = match c.backend {
        CacheBackend::Off => (Arc::new(NoCache), None),
        CacheBackend::Memory => (Arc::new(InMemoryCache::new(c.memory_max_entries)), None),
        CacheBackend::Valkey => {
            let v = Arc::new(
                ValkeyCache::new(ValkeySettings {
                    url: c.valkey_url.clone(),
                    timeout: c.timeout,
                    cooldown: Duration::from_secs(5),
                })
                .map_err(BuildError::Cache)?,
            );
            (v.clone(), Some(Arc::new(ValkeyReadiness(v))))
        }
    };
    tracing::info!(backend = inner.backend(), ttl = ?c.default_ttl, "advisory cache");
    Ok((
        AdvisoryCache::new(inner, CacheNamespace::new(&c.namespace), c.default_ttl),
        check,
    ))
}

/// Outbox relay from `SUTRA_OUTBOX_RELAY` (+ its non-critical NATS readiness check).
fn build_relay(cfg: &Config, pool: &PgPool) -> Result<WithCheck<Option<OutboxRelay>>, BuildError> {
    if cfg.relay.mode == OutboxRelayMode::Off {
        return Ok((None, None));
    }
    let n = &cfg.nats;
    let publisher = Arc::new(
        JetStreamPublisher::new(JetStreamSettings {
            url: n.url.clone(),
            stream: n.stream.clone(),
            subject_prefix: n.subject_prefix.clone(),
            duplicate_window: n.duplicate_window,
            max_age: n.max_age,
            replicas: n.replicas,
            connect_timeout: Duration::from_secs(5),
            client_name: "sutra-api".into(),
        })
        .map_err(BuildError::Messaging)?,
    );
    // Validate the prefix once at startup instead of dead-lettering every event.
    sutra_kernel::messaging::subject_for(&n.subject_prefix, uuid::Uuid::nil(), "sutraerp.a.b")
        .map_err(|e| BuildError::Messaging(e.to_string()))?;
    let r = &cfg.relay;
    let relay = OutboxRelay::new(
        pool.clone(),
        publisher.clone(),
        RelaySettings {
            batch_size: r.batch_size,
            poll_interval: r.poll_interval,
            lease: r.lease,
            max_attempts: r.max_attempts,
            backoff: Backoff::new(r.backoff_base, r.backoff_max),
            publish_timeout: r.publish_timeout,
            subject_prefix: n.subject_prefix.clone(),
            tenant: None,
        },
    );
    Ok((Some(relay), Some(Arc::new(NatsReadiness(publisher)))))
}

/// Notification e-mail channel from `SUTRA_NOTIFY_EMAIL`.
fn build_mailer(cfg: &Config) -> Result<Arc<dyn MailSender>, BuildError> {
    let m = &cfg.mail;
    Ok(match m.mode {
        MailMode::Off => Arc::new(NoopMailSender),
        MailMode::Smtp => {
            if m.smtp_tls == SmtpTlsMode::None && cfg.env == crate::config::Environment::Production
            {
                tracing::warn!("SUTRA_SMTP_TLS=none in production: e-mail travels unencrypted");
            }
            Arc::new(
                SmtpMailSender::new(&SmtpSettings {
                    host: m.smtp_host.clone(),
                    port: m.smtp_port,
                    tls: match m.smtp_tls {
                        SmtpTlsMode::None => SmtpTls::None,
                        SmtpTlsMode::StartTls => SmtpTls::StartTls,
                        SmtpTlsMode::Tls => SmtpTls::Tls,
                    },
                    username: m.smtp_username.clone(),
                    password: m.smtp_password.clone(),
                    from: m.from.clone(),
                    timeout: m.timeout,
                })
                .map_err(BuildError::Mail)?,
            )
        }
    })
}

/// Inbox consumer (+ e-mail delivery when the channel is on).
fn build_consumers(cfg: &Config, pool: &PgPool, mailer: Arc<dyn MailSender>) -> Scheduler {
    let i = &cfg.inbox;
    let email = cfg.mail.mode == MailMode::Smtp;
    let mut s = Scheduler::new(pool.clone()).with(Arc::new(InboxProjector {
        interval: i.poll_interval,
        lookback: i.lookback,
        batch_size: i.batch_size,
        email,
    }));
    if email {
        s = s.with(Arc::new(NotificationDelivery {
            interval: cfg.mail.delivery_interval,
            mailer,
            public_url: cfg.public_url.clone(),
            batch_size: 20,
            max_attempts: cfg.mail.max_attempts,
            send_timeout: cfg.mail.timeout,
        }));
    }
    s
}

fn build_scheduler(cfg: &Config, pool: &PgPool) -> Scheduler {
    let s = &cfg.sweepers;
    let i = &cfg.inbox;
    Scheduler::new(pool.clone())
        .with(Arc::new(NotificationPurge {
            interval: i.sweep_interval,
            retention: i.notification_retention,
            batch_limit: s.batch_limit,
        }))
        .with(Arc::new(NoticePurge {
            interval: i.sweep_interval,
            retention: i.notice_retention,
            batch_limit: s.batch_limit,
        }))
        .with(Arc::new(TodoPurge {
            interval: i.sweep_interval,
            retention: i.todo_retention,
            batch_limit: s.batch_limit,
        }))
        .with(Arc::new(EventReceiptPurge {
            interval: i.sweep_interval,
            lookback: i.lookback,
            batch_limit: s.batch_limit,
        }))
        .with(Arc::new(IdempotencyPurge {
            interval: s.idempotency_interval,
            batch_limit: s.batch_limit,
        }))
        .with(Arc::new(OutboxPurge {
            interval: s.outbox_interval,
            retention: s.outbox_retention,
            batch_limit: s.batch_limit,
        }))
        .with(Arc::new(SessionPurge {
            interval: s.session_interval,
            grace: s.session_grace,
            idle_timeout: Duration::from_secs(u64::try_from(cfg.session_idle_secs).unwrap_or(0)),
            batch_limit: s.batch_limit,
        }))
        .with(Arc::new(PreAuthPurge {
            interval: s.pre_auth_interval,
            grace: s.pre_auth_grace,
            batch_limit: s.batch_limit,
        }))
}

// ---- 9A ----
// Observability + tamper-evident audit chain wiring (docs/batch9-contracts.md §3 / §4): the
// `audit_seal` / `audit_anchor` jobs (when `SUTRA_AUDIT_CHAIN=true` and audit rows go to
// PostgreSQL), the `/metrics` listener + its gauges (`SUTRA_METRICS`), `sutra_build_info`, and the
// OTLP flush when `serve` cancels its workers.

/// 9A background work started by `serve`.
pub struct ObservabilityTasks {
    pub audit: Option<Scheduler>,
    /// `SUTRA_METRICS_ADDR` when `SUTRA_METRICS=true`.
    pub metrics_addr: Option<std::net::SocketAddr>,
    pub pool: PgPool,
}

impl ObservabilityTasks {
    pub fn describe(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .audit
            .as_ref()
            .map(|s| s.job_names().into_iter().map(str::to_owned).collect())
            .unwrap_or_default();
        if let Some(addr) = self.metrics_addr {
            v.push(format!("metrics({addr})"));
        }
        v.push("telemetry-flush".into());
        v
    }

    pub fn spawn(self, cancel: &CancellationToken) -> Vec<JoinHandle<()>> {
        let mut handles = vec![];
        if let Some(s) = self.audit {
            handles.extend(s.spawn(cancel.clone()));
        }
        if let Some(addr) = self.metrics_addr {
            handles.push(tokio::spawn(crate::metrics::serve(addr, cancel.clone())));
            handles.push(tokio::spawn(crate::metrics::collect(
                self.pool.clone(),
                Duration::from_secs(15),
                cancel.clone(),
            )));
        }
        handles.push(tokio::spawn(crate::telemetry::flush_on_shutdown(
            cancel.clone(),
        )));
        handles
    }
}

/// `SUTRA_AUDIT_ANCHOR_KEY` / `_KEY_ID` as the persistence anchor key.
/// INT: SEC-9D-003 — `SUTRA_PAYMENT_GATEWAY` → adapter (production default: disabled → 503).
fn payment_gateway(cfg: &Config) -> Arc<dyn sutra_contracts::finance::ports::PaymentGatewayPort> {
    use crate::config_b9::PaymentGatewayMode;
    let p = &cfg.hardening.payments;
    match p.gateway {
        PaymentGatewayMode::Disabled => {
            tracing::info!("payments: online gateway disabled (checkout + webhook answer 503)");
            Arc::new(sutra_finance::DisabledPaymentGateway)
        }
        PaymentGatewayMode::Mock => {
            tracing::warn!(
                "payments: MOCK gateway (development / CI only; captures without money)"
            );
            match &p.webhook_secret {
                Some(s) => Arc::new(sutra_finance::MockPaymentGateway::new(s.0.clone())),
                None => Arc::new(sutra_finance::MockPaymentGateway::default()),
            }
        }
    }
}

fn anchor_key(cfg: &Config) -> Option<sutra_persistence::audit_chain::AnchorKey> {
    let a = &cfg.hardening.audit_chain;
    a.anchor_key
        .as_ref()
        .map(|k| sutra_persistence::audit_chain::AnchorKey {
            key: k.0,
            key_id: a.anchor_key_id.clone(),
        })
}

fn build_observability(cfg: &Config, pool: &PgPool) -> ObservabilityTasks {
    let t = &cfg.hardening.telemetry;
    let a = &cfg.hardening.audit_chain;
    crate::metrics::set_build_info(&t.git_sha);
    crate::metrics_bridge::install(); // INT: 9C metrics into the 9A /metrics registry
    let audit = (a.enabled && cfg.audit_sink == StoreBackend::Postgres).then(|| {
        let mut s = Scheduler::new(pool.clone()).with(Arc::new(
            sutra_persistence::audit_chain::AuditSeal {
                pool: pool.clone(),
                batch: a.seal_batch,
                interval: a.seal_interval,
            },
        ));
        match anchor_key(cfg) {
            Some(key) if !a.anchor_interval.is_zero() => {
                s = s.with(Arc::new(sutra_persistence::audit_chain::AuditAnchor {
                    pool: pool.clone(),
                    key,
                    interval: a.anchor_interval,
                }));
            }
            Some(_) => tracing::info!("SUTRA_AUDIT_ANCHOR_SECS=0: audit anchors are off"),
            None if !a.anchor_interval.is_zero() => tracing::warn!(
                "SUTRA_AUDIT_ANCHOR_KEY is not set: the audit chain is sealed but not anchored"
            ),
            None => {}
        }
        for job in s.job_names() {
            crate::metrics::prime_job(job);
        }
        s
    });
    ObservabilityTasks {
        audit,
        metrics_addr: t.metrics_enabled.then_some(t.metrics_addr),
        pool: pool.clone(),
    }
}
// ---- end 9A ----

/// Domain C (Batch 3) adapters: Domain B reference reads, the real Finance clearance reader
/// (Batch 5 foundation), the real Exams results reader (Batch 6 foundation),
/// no-op ABC and external-scholarship adapters. Tests reuse it to build a `StudentState` with
/// `ApprovalMode::AwaitDecision` (`sutra_student::router_with_state`).
pub fn student_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_student::StudentDeps {
    student_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`student_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn student_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_student::StudentDeps {
    sutra_student::StudentDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        finance: Arc::new(sutra_finance::PgFinanceClearance::new(pool.clone())),
        results: Arc::new(sutra_exams::PgResultsPort::new(pool.clone())),
        abc: Arc::new(sutra_student::adapters::NoopAbcRegistry),
        external_scholarships: Arc::new(sutra_student::adapters::NoopExternalScholarships),
        workflow,
        approvals,
        pool,
    }
}

/// Domain D (Batch 4) adapters: Domain B reference reads, Domain C student directory,
/// registration reader and credit posting, the real Campus teaching spaces (Batch 8 foundation) and the
/// real People staff directory (Batch 7 foundation; subject-only fallback when no staff record
/// exists). Tests reuse it to build an `AcademicState` with
/// `ApprovalMode::AwaitDecision` (`sutra_academic::router_with_state`).
pub fn academic_deps(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
) -> sutra_academic::AcademicDeps {
    academic_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`academic_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn academic_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_academic::AcademicDeps {
    sutra_academic::AcademicDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        students: Arc::new(sutra_student::PgStudentDirectory::new(pool.clone())),
        registrations: Arc::new(sutra_student::PgRegistrationReader::new(pool.clone())),
        credits: Arc::new(sutra_student::PgCreditPosting::new(pool.clone())),
        spaces: Arc::new(sutra_campus::PgTeachingSpaces::new(pool.clone())),
        staff: Arc::new(sutra_people::PgStaffDirectory::new(pool.clone())),
        workflow,
        approvals,
        pool,
    }
}

/// Domain F (Batch 5) adapters: Domain B reference reads, the Domain C student directory and
/// scholarship decisions handed off to Financial Aid, and the deterministic mock payment
/// gateway. Tests reuse it to build a `FinanceState` with `ApprovalMode::AwaitDecision`
/// (`sutra_finance::router_with_state`).
pub fn finance_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_finance::FinanceDeps {
    finance_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`finance_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn finance_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_finance::FinanceDeps {
    sutra_finance::FinanceDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        students: Arc::new(sutra_student::PgStudentDirectory::new(pool.clone())),
        aid: Arc::new(sutra_student::PgAidDecisionSource::new(pool.clone())),
        gateway: Arc::new(sutra_finance::MockPaymentGateway::default()),
        workflow,
        approvals,
        pool,
    }
}

/// Domain I (Batch 6) adapters: Domain B reference reads, the Domain C student directory and
/// registration reader, the Domain D offering directory and examination facts (internal
/// assessment, attendance), the Domain F clearance reader, charge directory and exam fee port.
/// Tests reuse it to build an `ExamsState` with `ApprovalMode::AwaitDecision`
/// (`sutra_exams::router_with_state`).
pub fn exams_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_exams::ExamsDeps {
    exams_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`exams_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn exams_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_exams::ExamsDeps {
    sutra_exams::ExamsDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        students: Arc::new(sutra_student::PgStudentDirectory::new(pool.clone())),
        registrations: Arc::new(sutra_student::PgRegistrationReader::new(pool.clone())),
        offerings: Arc::new(sutra_academic::PgOfferingDirectory::new(pool.clone())),
        academic: Arc::new(sutra_academic::PgAcademicExamFacts::new(pool.clone())),
        finance: Arc::new(sutra_finance::PgFinanceClearance::new(pool.clone())),
        charges: Arc::new(sutra_finance::PgChargeDirectory::new(pool.clone())),
        fees: Arc::new(sutra_finance::PgExamFees::new(
            finance_deps_with(pool.clone(), workflow.clone(), approvals).into_state(),
        )),
        workflow,
        approvals,
        pool,
    }
}

/// Domain E (Batch 7) adapters: Domain B reference reads, the Domain D faculty workload reader
/// and the Domain F payroll posting port. Tests reuse it to build a `PeopleState` with
/// `ApprovalMode::AwaitDecision` (`sutra_people::router_with_state`).
pub fn people_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_people::PeopleDeps {
    people_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`people_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn people_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_people::PeopleDeps {
    sutra_people::PeopleDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        workload: Arc::new(sutra_academic::PgFacultyWorkload::new(pool.clone())),
        postings: Arc::new(sutra_finance::PgPayrollPostings::new(
            finance_deps_with(pool.clone(), workflow.clone(), approvals).into_state(),
        )),
        workflow,
        approvals,
        pool,
    }
}

/// Domain G (Batch 8) adapters: Domain B reference reads, the Domain C student directory, the
/// Domain E workforce directory and the Domain F campus charge port. Tests reuse it to build a
/// `CampusState` with `ApprovalMode::AwaitDecision` (`sutra_campus::router_with_state`).
pub fn campus_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_campus::CampusDeps {
    campus_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`campus_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn campus_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_campus::CampusDeps {
    sutra_campus::CampusDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        students: Arc::new(sutra_student::PgStudentDirectory::new(pool.clone())),
        workforce: Arc::new(sutra_people::PgWorkforceDirectory::new(pool.clone())),
        charges: Arc::new(sutra_finance::PgCampusCharges::new(
            finance_deps_with(pool.clone(), workflow.clone(), approvals).into_state(),
        )),
        finance: Arc::new(sutra_finance::PgChargeDirectory::new(pool.clone())),
        workflow,
        approvals,
        pool,
    }
}

/// Domain H (Batch 8) adapters: Domain B reference reads and one `OperationalMetricsPort` per
/// domain (student, people, finance, campus, resources). Tests reuse it to build a
/// `ServicesState` with `ApprovalMode::AwaitDecision` (`sutra_services::router_with_state`).
pub fn services_deps(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
) -> sutra_services::ServicesDeps {
    services_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`services_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn services_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_services::ServicesDeps {
    sutra_services::ServicesDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        metrics: {
            let ports: Vec<Arc<dyn OperationalMetricsPort>> = vec![
                Arc::new(sutra_student::PgStudentMetrics::new(pool.clone())),
                Arc::new(sutra_people::PgPeopleMetrics::new(pool.clone())),
                Arc::new(sutra_finance::PgFinanceMetrics::new(pool.clone())),
                Arc::new(sutra_campus::PgCampusMetrics::new(pool.clone())),
                Arc::new(sutra_resources::PgResourceMetrics::new(pool.clone())),
            ];
            ports
        },
        workflow,
        approvals,
        pool,
    }
}

/// Domain J (Batch 8) adapters: Domain B reference reads, the Domain E workforce directory and
/// the Domain F procurement posting port. Tests reuse it to build a `ResourcesState` with
/// `ApprovalMode::AwaitDecision` (`sutra_resources::router_with_state`).
pub fn resources_deps(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
) -> sutra_resources::ResourcesDeps {
    resources_deps_with(pool, workflow, ApprovalMode::default())
}

/// [`resources_deps`] with the configured approval mode (`SUTRA_APPROVAL_MODE`), also handed to
/// the nested Domain F ports.
pub fn resources_deps_with(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
    approvals: ApprovalMode,
) -> sutra_resources::ResourcesDeps {
    sutra_resources::ResourcesDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        workforce: Arc::new(sutra_people::PgWorkforceDirectory::new(pool.clone())),
        postings: Arc::new(sutra_finance::PgProcurementPostings::new(
            finance_deps_with(pool.clone(), workflow.clone(), approvals).into_state(),
        )),
        workflow,
        approvals,
        pool,
    }
}

/// Builds the router plus the generated OpenAPI document.
///
/// Layer order (outermost first): cross-site Origin guard → session authentication + CSRF →
/// idempotency → handlers. The
/// idempotency middleware keys its records by the authenticated owner, so it must run inside
/// the authentication layer.
pub fn build(ctx: AppContext) -> OpenApiRouter {
    OpenApiRouter::with_openapi(ApiDoc::openapi())
        .merge(sutra_web::health::router(ctx.readiness.clone()))
        .merge(sutra_platform::router(ctx.platform, ctx.pool.clone()))
        .merge(sutra_platform::search::router(ctx.search.clone())) // 9C
        .merge(sutra_institution::router(
            sutra_institution::InstitutionDeps {
                pool: ctx.pool.clone(),
                cache: ctx.cache.clone(),
                workflow: ctx.workflow.clone(),
                approvals: ctx.approvals,
            },
        ))
        .merge(sutra_student::router(student_deps_with(
            ctx.pool.clone(),
            ctx.workflow.clone(),
            ctx.approvals,
        )))
        .merge(sutra_academic::router(academic_deps_with(
            ctx.pool.clone(),
            ctx.workflow.clone(),
            ctx.approvals,
        )))
        .merge(sutra_people::router(people_deps_with(
            ctx.pool.clone(),
            ctx.workflow.clone(),
            ctx.approvals,
        )))
        .merge(sutra_finance::router({
            // INT: SEC-9D-003 — the configured gateway, not the dev mock.
            let mut deps = finance_deps_with(ctx.pool.clone(), ctx.workflow.clone(), ctx.approvals);
            deps.gateway = ctx.payment_gateway.clone();
            deps
        }))
        .merge(sutra_campus::router(campus_deps_with(
            ctx.pool.clone(),
            ctx.workflow.clone(),
            ctx.approvals,
        )))
        .merge(sutra_services::router(services_deps_with(
            ctx.pool.clone(),
            ctx.workflow.clone(),
            ctx.approvals,
        )))
        .merge(sutra_exams::router(exams_deps_with(
            ctx.pool.clone(),
            ctx.workflow.clone(),
            ctx.approvals,
        )))
        .merge(sutra_resources::router(resources_deps_with(
            ctx.pool.clone(),
            ctx.workflow.clone(),
            ctx.approvals,
        )))
        .layer(axum::middleware::from_fn_with_state(
            ctx.idempotency,
            sutra_web::idempotency::idempotency,
        ))
        .layer(axum::middleware::from_fn_with_state(
            ctx.session_auth,
            sutra_web::auth::authenticate,
        ))
        .layer(axum::middleware::from_fn_with_state(
            ctx.origin_policy,
            sutra_web::auth::origin_guard,
        ))
}

/// Final Axum application with the full middleware stack (outermost last).
pub fn router(ctx: AppContext) -> Router {
    let timeout = ctx.request_timeout;
    let audit_anchor = ctx.audit_anchor.clone(); // 9A
    let (router, api) = build(ctx).split_for_parts();
    // 9A: the audit chain API checks anchor MACs with this key when it is configured.
    let router = match audit_anchor {
        Some(key) => router.layer(axum::Extension(key)),
        None => router,
    };
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
        .layer(axum::middleware::from_fn(crate::metrics::track_http)) // 9A
        .layer(TraceLayer::new_for_http().make_span_with(crate::telemetry::make_span))
        .layer(SetSensitiveRequestHeadersLayer::new([
            header::AUTHORIZATION,
            header::COOKIE,
            header::SET_COOKIE,
        ]))
        .layer(axum::middleware::from_fn(sutra_web::request_id::request_id))
}

// ---- 9C ---- Flowable workflow adapter, decision sync, OpenSearch projections
// (docs/batch9-contracts.md §7 / §8). Settings come only from `Config::hardening`.

/// Workflow port, search state and the consumer scheduler extended with the 9C jobs.
type Wired9c = (
    Arc<dyn WorkflowPort>,
    sutra_platform::search::SearchState,
    Option<Scheduler>,
);

/// `SUTRA_WORKFLOW=flowable` → `FlowableWorkflowAdapter` + job `workflow_decision_sync`;
/// `SUTRA_SEARCH=opensearch` → `OpenSearchIndex` + the three module sources + jobs
/// `search_project` / `search_rebuild`. Defaults (noop / off) add nothing.
fn wire_9c(
    cfg: &Config,
    pool: &PgPool,
    consumers: Option<Scheduler>,
) -> Result<Wired9c, BuildError> {
    use crate::config_b9::{SearchMode, WorkflowMode};
    use sutra_contracts::platform::search::{SearchIndex, SearchSource};
    use sutra_persistence::jobs::SweepJob;
    use sutra_platform::search::projector::{SearchProjector, SearchRebuild};
    use sutra_platform::workflow_flowable::{
        DecisionSync, FlowableSettings, FlowableWorkflowAdapter,
    };
    let h = &cfg.hardening;
    let mut jobs: Vec<Arc<dyn SweepJob>> = vec![];
    tracing::info!(
        approvals = h.workflow.approvals.as_str(),
        "approvals: governed decisions {}",
        match h.workflow.approvals {
            ApprovalMode::AwaitDecision => "wait for an authorised human approver",
            ApprovalMode::AutoApprove => "are AUTO-APPROVED (development / test opt-in)",
        }
    );
    let workflow: Arc<dyn WorkflowPort> = match h.workflow.mode {
        WorkflowMode::Noop => Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
        WorkflowMode::Flowable => {
            let w = &h.workflow;
            let adapter = Arc::new(
                FlowableWorkflowAdapter::new(FlowableSettings {
                    url: w.flowable_url.clone(),
                    user: w.flowable_user.clone(),
                    password: w.flowable_password.as_ref().map(|p| p.0.clone()),
                    timeout: w.timeout,
                    deploy_on_start: w.deploy_on_start,
                    allow_auto_approve: w.flowable_auto_approve, // Part 1: DEF-X-001
                })
                .map_err(BuildError::DataPlatform)?,
            );
            tracing::info!(url = %w.flowable_url, "workflow: Flowable REST adapter");
            jobs.push(Arc::new(DecisionSync {
                adapter: adapter.clone(),
                interval: cfg.inbox.poll_interval,
                lookback: cfg.inbox.lookback,
                batch_size: cfg.inbox.batch_size,
            }));
            adapter
        }
    };
    let search = match h.search.mode {
        SearchMode::Off => sutra_platform::search::SearchState::disabled(),
        SearchMode::OpenSearch => {
            let s = &h.search;
            let client = sutra_infra::opensearch::OpenSearchClient::new(
                sutra_infra::opensearch::OpenSearchSettings {
                    url: s.url.clone(),
                    user: s.user.clone(),
                    password: s.password.as_ref().map(|p| p.0.clone()),
                    index_prefix: s.index_prefix.clone(),
                    timeout: Duration::from_secs(15),
                },
            )
            .map_err(BuildError::DataPlatform)?;
            let index: Arc<dyn SearchIndex> = Arc::new(OpenSearchIndex(client));
            let sources: Vec<Arc<dyn SearchSource>> = search_sources(pool.clone());
            let state = sutra_platform::search::SearchState::enabled(index, sources);
            if let Some(rt) = state.runtime() {
                jobs.push(Arc::new(SearchProjector {
                    runtime: rt.clone(),
                    interval: s.poll_interval,
                    batch_size: s.batch_size,
                }));
                jobs.push(Arc::new(SearchRebuild::new(rt, s.poll_interval)));
            }
            tracing::info!(url = %s.url, prefix = %s.index_prefix, "search: OpenSearch projections");
            state
        }
    };
    let consumers = if jobs.is_empty() {
        consumers
    } else {
        let mut sched = consumers.unwrap_or_else(|| Scheduler::new(pool.clone()));
        for j in jobs {
            sched = sched.with(j);
        }
        Some(sched)
    };
    Ok((workflow, search, consumers))
}

/// The three v1 projection sources, each implemented by its owning module.
pub fn search_sources(
    pool: PgPool,
) -> Vec<Arc<dyn sutra_contracts::platform::search::SearchSource>> {
    let student: Arc<dyn sutra_contracts::platform::search::SearchSource> = Arc::new(
        sutra_student::search_source::PgStudentSearchSource::new(pool.clone()),
    );
    let staff: Arc<dyn sutra_contracts::platform::search::SearchSource> = Arc::new(
        sutra_people::search_source::PgStaffSearchSource::new(pool.clone()),
    );
    let catalogue: Arc<dyn sutra_contracts::platform::search::SearchSource> = Arc::new(
        sutra_campus::search_source::PgCatalogueSearchSource::new(pool),
    );
    vec![student, staff, catalogue]
}

/// `SearchIndex` port over the OpenSearch REST client (`sutra-infra` may only depend on the
/// kernel, so the composition root adapts its plain-JSON API to the contract types).
pub struct OpenSearchIndex(pub sutra_infra::opensearch::OpenSearchClient);

#[async_trait::async_trait]
impl sutra_contracts::platform::search::SearchIndex for OpenSearchIndex {
    async fn ready(&self) -> Result<(), sutra_kernel::error::KernelError> {
        self.0.health().await
    }

    async fn ensure(&self, projection: &str) -> Result<(), sutra_kernel::error::KernelError> {
        self.0.ensure_index(projection).await
    }

    async fn recreate(&self, projection: &str) -> Result<(), sutra_kernel::error::KernelError> {
        self.0.delete_index(projection).await?;
        self.0.ensure_index(projection).await
    }

    async fn upsert(
        &self,
        projection: &str,
        docs: &[sutra_contracts::platform::search::SearchDocument],
    ) -> Result<u64, sutra_kernel::error::KernelError> {
        let bulk: Vec<sutra_infra::opensearch::BulkDoc> = docs
            .iter()
            .map(|d| sutra_infra::opensearch::BulkDoc {
                id: d.id.to_string(),
                version: d.version,
                source: d.body.clone(),
            })
            .collect();
        self.0.bulk_index(projection, &bulk).await
    }

    async fn delete(
        &self,
        projection: &str,
        ids: &[uuid::Uuid],
    ) -> Result<u64, sutra_kernel::error::KernelError> {
        let ids: Vec<String> = ids.iter().map(ToString::to_string).collect();
        self.0.bulk_delete(projection, &ids).await
    }

    async fn refresh(&self, projection: &str) -> Result<(), sutra_kernel::error::KernelError> {
        self.0.refresh(projection).await
    }

    async fn search(
        &self,
        projection: &str,
        tenant: uuid::Uuid,
        q: &str,
        limit: u32,
    ) -> Result<sutra_contracts::platform::search::SearchResults, sutra_kernel::error::KernelError>
    {
        let (total, hits) = self
            .0
            .search(projection, &tenant.to_string(), q, limit)
            .await?;
        Ok(sutra_contracts::platform::search::SearchResults {
            projection: projection.to_owned(),
            total,
            items: hits
                .into_iter()
                .filter_map(|h| {
                    Some(sutra_contracts::platform::search::SearchHit {
                        id: uuid::Uuid::parse_str(&h.id).ok()?,
                        projection: projection.to_owned(),
                        score: h.score,
                        document: h.source,
                    })
                })
                .collect(),
        })
    }

    async fn count(
        &self,
        projection: &str,
        tenant: Option<uuid::Uuid>,
    ) -> Result<u64, sutra_kernel::error::KernelError> {
        let t = tenant.map(|t| t.to_string());
        self.0.count(projection, t.as_deref()).await
    }
}
// ---- end 9C ----
