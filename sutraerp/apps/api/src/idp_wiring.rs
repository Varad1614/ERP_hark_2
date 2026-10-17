//! Part 2 · S4-idp: composition of identity provisioning (`docs/readiness/part2/s4-idp.md`).
//!
//! `SUTRA_IDP_PROVISIONING=keycloak` → [`KeycloakAdmin`], the student / staff identity sources
//! and the jobs `idp-events` / `idp-sync` on the consumer scheduler; `memory` → the in-memory
//! adapter (development / tests; production refuses it while loading the configuration); `off` →
//! the admin API answers reads and refuses changes with `503`.

use crate::config::{Config, Environment};
use crate::config_idp::IdpMode;
use sqlx::PgPool;
use std::sync::Arc;
use sutra_contracts::platform::auth::SessionStore;
use sutra_contracts::platform::identity::{IdentityProvisioningPort, IdentitySubjectSource};
use sutra_persistence::jobs::Scheduler;
use sutra_platform::identity::http::IdentityState;
use sutra_platform::identity::keycloak_admin::{KeycloakAdmin, KeycloakAdminSettings};
use sutra_platform::identity::memory::MemoryIdentityProvider;
use sutra_platform::identity::sync::{IdpEventConsumer, IdpSyncWorker};
use sutra_platform::identity::{IdentityRuntime, IdentitySettings};

/// The student and staff identity sources (each implemented by its owning module).
pub fn identity_sources(pool: &PgPool) -> Vec<Arc<dyn IdentitySubjectSource>> {
    let student: Arc<dyn IdentitySubjectSource> =
        Arc::new(sutra_student::identity_source::PgStudentIdentitySource::new(pool.clone()));
    let staff: Arc<dyn IdentitySubjectSource> = Arc::new(
        sutra_people::identity_source::PgStaffIdentitySource::new(pool.clone()),
    );
    vec![student, staff]
}

/// Runtime settings from the configuration.
pub fn identity_settings(cfg: &Config) -> IdentitySettings {
    let i = &cfg.idp;
    IdentitySettings {
        auto_provision: i.auto_provision,
        send_actions_email: i.send_actions_email,
        poll_interval: i.poll_interval,
        batch_size: i.batch_size,
        lookback: cfg.inbox.lookback,
        retry_base: i.retry_base,
        retry_max: i.retry_max,
        tenants: i.tenants.clone(),
    }
}

/// The configured provisioning port (`None` when off).
pub fn identity_port(cfg: &Config) -> Result<Option<Arc<dyn IdentityProvisioningPort>>, String> {
    let i = &cfg.idp;
    Ok(match i.mode {
        IdpMode::Off => None,
        IdpMode::Memory => {
            tracing::warn!(
                "identity provisioning: IN-MEMORY identity provider (development / tests only)"
            );
            Some(Arc::new(MemoryIdentityProvider::new()))
        }
        IdpMode::Keycloak => {
            let mut s = KeycloakAdminSettings::new(
                &i.url,
                &i.realm,
                &i.client_id,
                i.client_secret.as_deref().unwrap_or_default(),
            );
            s.timeout = i.timeout;
            s.actions_client_id = i.actions_client_id.clone();
            s.actions_redirect_uri = i.actions_redirect_uri.clone();
            tracing::info!(url = %i.url, realm = %i.realm, client = %i.client_id, "identity provisioning: Keycloak Admin REST");
            Some(Arc::new(KeycloakAdmin::new(s).map_err(|e| e.to_string())?))
        }
    })
}

/// Identity admin API state, provisioning runtime and the consumer scheduler extended with
/// `idp-events` / `idp-sync`.
pub type WiredIdp = (IdentityState, Option<IdentityRuntime>, Option<Scheduler>);

pub fn wire(
    cfg: &Config,
    pool: &PgPool,
    sessions: Arc<dyn SessionStore>,
    consumers: Option<Scheduler>,
) -> Result<WiredIdp, String> {
    let Some(port) = identity_port(cfg)? else {
        return Ok((IdentityState::disabled(pool.clone()), None, consumers));
    };
    let runtime = IdentityRuntime {
        port: port.clone(),
        sources: identity_sources(pool),
        sessions,
        settings: identity_settings(cfg),
    };
    let state = IdentityState {
        pool: pool.clone(),
        sources: runtime.sources.clone(),
        idp: Some(port.kind()),
        send_actions_email: cfg.idp.send_actions_email,
    };
    let sched = consumers
        .unwrap_or_else(|| Scheduler::new(pool.clone()))
        .with(Arc::new(IdpEventConsumer {
            runtime: runtime.clone(),
        }))
        .with(Arc::new(IdpSyncWorker {
            runtime: runtime.clone(),
        }));
    Ok((state, Some(runtime), Some(sched)))
}

/// Start-up gate: the admin client must authenticate and hold exactly the least-privilege
/// `realm-management` roles. Fatal in production; a warning elsewhere (the jobs retry).
pub async fn startup_check(cfg: &Config, runtime: &IdentityRuntime) -> Result<(), String> {
    if !cfg.idp.startup_check {
        return Ok(());
    }
    let res = tokio::time::timeout(std::time::Duration::from_secs(20), runtime.port.check())
        .await
        .map_err(|_| "timed out".to_owned())
        .and_then(|r| r.map_err(|e| e.to_string()));
    match res {
        Ok(()) => {
            tracing::info!(
                idp = runtime.port.kind(),
                "identity provisioning: admin client verified (least privilege)"
            );
            Ok(())
        }
        Err(e) if cfg.env == Environment::Production => Err(format!(
            "identity provisioning: the identity admin client failed its start-up check: {e}"
        )),
        Err(e) => {
            tracing::warn!(error = %e, "identity provisioning: admin client check failed (jobs will retry)");
            Ok(())
        }
    }
}
