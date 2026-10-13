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
use sutra_infra::nats::{JetStreamPublisher, JetStreamSettings, NatsReadiness};
use sutra_infra::smtp::{SmtpMailSender, SmtpSettings, SmtpTls};
use sutra_infra::valkey::{ValkeyCache, ValkeyReadiness, ValkeySettings};
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
    /// Outbox relay + sweepers; started by `serve` via [`AppContext::take_background`].
    pub background: BackgroundTasks,
}

/// Long-running workers that run next to the HTTP server.
#[derive(Default)]
pub struct BackgroundTasks {
    pub relay: Option<OutboxRelay>,
    pub scheduler: Option<Scheduler>,
    /// Inbox consumer + notification e-mail delivery (Batch 1D).
    pub consumers: Option<Scheduler>,
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
            workflow: Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
            background: BackgroundTasks {
                relay,
                scheduler,
                consumers,
            },
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

/// Domain C (Batch 3) adapters: Domain B reference reads, the real Finance clearance reader
/// (Batch 5 foundation), the real Exams results reader (Batch 6 foundation),
/// no-op ABC and external-scholarship adapters. Tests reuse it to build a `StudentState` with
/// `ApprovalMode::AwaitDecision` (`sutra_student::router_with_state`).
pub fn student_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_student::StudentDeps {
    sutra_student::StudentDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        finance: Arc::new(sutra_finance::PgFinanceClearance::new(pool.clone())),
        results: Arc::new(sutra_exams::PgResultsPort::new(pool.clone())),
        abc: Arc::new(sutra_student::adapters::NoopAbcRegistry),
        external_scholarships: Arc::new(sutra_student::adapters::NoopExternalScholarships),
        workflow,
        pool,
    }
}

/// Domain D (Batch 4) adapters: Domain B reference reads, Domain C student directory,
/// registration reader and credit posting, the Campus teaching-space stub (Batch 8) and the
/// People staff stub (Batch 7). Tests reuse it to build an `AcademicState` with
/// `ApprovalMode::AwaitDecision` (`sutra_academic::router_with_state`).
pub fn academic_deps(
    pool: PgPool,
    workflow: Arc<dyn WorkflowPort>,
) -> sutra_academic::AcademicDeps {
    sutra_academic::AcademicDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        students: Arc::new(sutra_student::PgStudentDirectory::new(pool.clone())),
        registrations: Arc::new(sutra_student::PgRegistrationReader::new(pool.clone())),
        credits: Arc::new(sutra_student::PgCreditPosting::new(pool.clone())),
        spaces: Arc::new(sutra_campus::NoTeachingSpaces),
        staff: Arc::new(sutra_people::SubjectOnlyStaff),
        workflow,
        pool,
    }
}

/// Domain F (Batch 5) adapters: Domain B reference reads, the Domain C student directory and
/// scholarship decisions handed off to Financial Aid, and the deterministic mock payment
/// gateway. Tests reuse it to build a `FinanceState` with `ApprovalMode::AwaitDecision`
/// (`sutra_finance::router_with_state`).
pub fn finance_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_finance::FinanceDeps {
    sutra_finance::FinanceDeps {
        references: Arc::new(sutra_institution::PgInstitutionReferences::new(
            pool.clone(),
        )),
        students: Arc::new(sutra_student::PgStudentDirectory::new(pool.clone())),
        aid: Arc::new(sutra_student::PgAidDecisionSource::new(pool.clone())),
        gateway: Arc::new(sutra_finance::MockPaymentGateway::default()),
        workflow,
        pool,
    }
}

/// Domain I (Batch 6) adapters: Domain B reference reads, the Domain C student directory and
/// registration reader, the Domain D offering directory and examination facts (internal
/// assessment, attendance), the Domain F clearance reader, charge directory and exam fee port.
/// Tests reuse it to build an `ExamsState` with `ApprovalMode::AwaitDecision`
/// (`sutra_exams::router_with_state`).
pub fn exams_deps(pool: PgPool, workflow: Arc<dyn WorkflowPort>) -> sutra_exams::ExamsDeps {
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
            finance_deps(pool.clone(), workflow.clone()).into_state(),
        )),
        workflow,
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
        .merge(sutra_institution::router(
            sutra_institution::InstitutionDeps {
                pool: ctx.pool.clone(),
                cache: ctx.cache.clone(),
                workflow: ctx.workflow.clone(),
            },
        ))
        .merge(sutra_student::router(student_deps(
            ctx.pool.clone(),
            ctx.workflow.clone(),
        )))
        .merge(sutra_academic::router(academic_deps(
            ctx.pool.clone(),
            ctx.workflow.clone(),
        )))
        .merge(sutra_people::router())
        .merge(sutra_finance::router(finance_deps(
            ctx.pool.clone(),
            ctx.workflow.clone(),
        )))
        .merge(sutra_campus::router())
        .merge(sutra_services::router())
        .merge(sutra_exams::router(exams_deps(
            ctx.pool.clone(),
            ctx.workflow.clone(),
        )))
        .merge(sutra_resources::router())
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
