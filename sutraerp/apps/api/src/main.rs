//! `sutra-api` binary.
//!
//! ```text
//! sutra-api            # serve (default)
//! sutra-api serve
//! sutra-api migrate    # apply migrations, then exit (ADR-L5-0008: separate from serving)
//! sutra-api openapi    # print the generated OpenAPI 3.1 document
//! sutra-api check-config  # validate the environment configuration, then exit (0 ok, 2 invalid)
//! ```

use std::process::ExitCode;
use sutra_api::{app, config::Config, openapi, shutdown, telemetry};

#[tokio::main]
async fn main() -> ExitCode {
    // A missing `.env` is normal; a malformed one used to be ignored silently, leaving every
    // variable after the bad line unset (DEF-P1-DEMO-002) — refuse to start instead.
    if let Err(e) = dotenvy::dotenv() {
        if !e.not_found() {
            eprintln!(".env: {e}");
            return ExitCode::FAILURE;
        }
    }
    // Batch 9 (contract §5): secrets rendered by the OpenBao agent as a dotenv file
    // (`/vault/secrets/sutra.env`); never overrides variables already in the environment.
    if let Some(path) = std::env::var_os("SUTRA_ENV_FILE") {
        if let Err(e) = dotenvy::from_path(&path) {
            eprintln!(
                "SUTRA_ENV_FILE {}: {e}",
                std::path::Path::new(&path).display()
            );
            return ExitCode::FAILURE;
        }
    }
    // Subcommand selection only (serve / migrate / openapi …), never an authorisation input; the
    // rule targets trusting argv[0] (SEC-P1-APPSEC-007, accepted false positive).
    // nosemgrep: rust.lang.security.args.args
    let cmd = std::env::args().nth(1).unwrap_or_else(|| "serve".into());
    if cmd == "openapi" {
        return match openapi::spec_json() {
            Ok(s) => {
                print!("{s}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("failed to render OpenAPI: {e}");
                ExitCode::FAILURE
            }
        };
    }
    if cmd == "check-config" {
        // Part 1 demo: validates the effective environment (compose service env, `.env` or
        // SUTRA_ENV_FILE) exactly as `serve` would, without touching the database. Secrets are
        // never printed.
        // D1/C1: the dev authenticator's explicit-password rule is checked here as at start-up.
        let checked = Config::from_env().and_then(|c| {
            if c.auth_provider == sutra_api::config::AuthProvider::Dev {
                c.require_dev_auth_password()?;
            }
            Ok(c)
        });
        return match checked {
            Ok(c) => {
                println!(
                    "configuration ok: env={:?} auth={:?} mfa_step_up={} http_addr={} \
                     session_cookie_secure={} public_url={} documents={:?} tls_extra_roots={}",
                    c.env,
                    c.auth_provider,
                    c.mfa_step_up,
                    c.http_addr,
                    c.session_cookie_secure,
                    c.public_url,
                    c.documents.mode,
                    c.tls.certificates
                );
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("configuration error: {e}");
                ExitCode::from(2)
            }
        };
    }
    let cfg = match Config::from_env() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("configuration error: {e}");
            return ExitCode::from(2);
        }
    };
    telemetry::init(&cfg);
    let result = match cmd.as_str() {
        "serve" => serve(cfg).await,
        // Part 1 S3 (DEF-X-003): migrations and runtime-role provisioning are schema-owner work
        // across every tenant; row-level security is FORCEd, so they run in the system context.
        "migrate" => sutra_kernel::db_context::system(migrate(cfg)).await,
        other => Err(format!(
            "unknown command `{other}` (expected serve | migrate | openapi | check-config)"
        )),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "fatal");
            ExitCode::FAILURE
        }
    }
}

/// Applies migrations as the schema owner (`SUTRA_MIGRATION_DATABASE_URL`, else `DATABASE_URL`)
/// and, when `SUTRA_DB_RUNTIME_USER`/`_PASSWORD` are set, provisions the API's runtime login
/// role (member of `sutra_app`, owns nothing).
async fn migrate(cfg: Config) -> Result<(), String> {
    let url = cfg
        .migration_database_url
        .clone()
        .unwrap_or_else(|| cfg.database_url.clone());
    let pool = sutra_persistence::pool::connect_lazy(&sutra_persistence::pool::PoolSettings {
        url,
        max_connections: 2,
        min_connections: 0,
        acquire_timeout: cfg.db_acquire_timeout,
        application_name: "sutra-api-migrate".into(),
    })
    .map_err(|e| e.to_string())?;
    sutra_persistence::migrate::run(&pool)
        .await
        .map_err(|e| e.to_string())?;
    tracing::info!("migrations applied");
    if let (Some(user), Some(password)) = (&cfg.db_runtime_user, &cfg.db_runtime_password) {
        sutra_persistence::migrate::provision_runtime_login(&pool, user, &password.0)
            .await
            .map_err(|e| format!("provisioning runtime role {user}: {e}"))?;
        tracing::info!(role = %user, "runtime login role provisioned (member of sutra_app)");
    }
    pool.close().await;
    Ok(())
}

/// Part 1 S3 (DEF-X-003): the API must serve as a role that row-level security applies to.
/// Audit A3 (M5): in production a superuser / BYPASSRLS login **and** a probe that errors or
/// times out refuse start-up (fail closed); elsewhere they are reported loudly.
async fn check_runtime_role(cfg: &Config, pool: &sqlx::PgPool) -> Result<(), String> {
    use sutra_api::startup_checks::{runtime_role_verdict, RoleProbe, RoleVerdict};
    let probe = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        sutra_persistence::pool::role_security(pool),
    )
    .await
    {
        Ok(Ok(r)) if r.bypasses_rls() => RoleProbe::BypassesRls {
            role: r.role,
            superuser: r.superuser,
            bypass_rls: r.bypass_rls,
        },
        Ok(Ok(r)) => RoleProbe::SubjectToRls { role: r.role },
        Ok(Err(e)) => RoleProbe::Failed(e.to_string()),
        Err(_) => RoleProbe::TimedOut,
    };
    let production = cfg.env == sutra_api::config::Environment::Production;
    match runtime_role_verdict(production, &probe) {
        RoleVerdict::Ok => {
            if let RoleProbe::SubjectToRls { role } = &probe {
                tracing::info!(role = %role, "runtime role is subject to row-level security");
            }
            Ok(())
        }
        RoleVerdict::Warn(msg) => {
            tracing::warn!("{msg} (refused in production)");
            Ok(())
        }
        RoleVerdict::Refuse(msg) => Err(msg),
    }
}

async fn serve(cfg: Config) -> Result<(), String> {
    let mut ctx = app::AppContext::from_config(&cfg).map_err(|e| e.to_string())?;
    let pool = ctx.pool.clone();
    check_runtime_role(&cfg, &pool).await?;
    // Part 2 · S4-idp: the identity admin client must be least-privilege (fatal in production).
    if let Some(rt) = &ctx.identity_runtime {
        sutra_api::idp_wiring::startup_check(&cfg, rt).await?;
    }
    // Outbox relay + sweepers run beside the server and stop after HTTP draining finished.
    let background = ctx.take_background();
    tracing::info!(workers = ?background.describe(), "background workers");
    let cancel = tokio_util::sync::CancellationToken::new();
    let workers = background.spawn(&cancel);
    let readiness = ctx.readiness.clone();
    let router = app::router(ctx);
    let listener = tokio::net::TcpListener::bind(cfg.http_addr)
        .await
        .map_err(|e| format!("bind {}: {e}", cfg.http_addr))?;
    tracing::info!(addr = %cfg.http_addr, env = ?cfg.env, "sutra-api listening");

    let (signal, started) = shutdown::coordinator(readiness);
    // Part 1 · Stage 3: the peer address feeds the public-endpoint rate limiter.
    let server = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(signal);
    tokio::select! {
        r = server => r.map_err(|e| e.to_string())?,
        _ = shutdown::deadline(started, cfg.shutdown_grace) => {
            tracing::warn!(grace = ?cfg.shutdown_grace, "grace period elapsed; forcing shutdown");
        }
    }
    // Workers finish their current pass/run (bounded by publish and statement timeouts).
    cancel.cancel();
    let join = futures_join_all(workers);
    if tokio::time::timeout(std::time::Duration::from_secs(10), join)
        .await
        .is_err()
    {
        tracing::warn!("background workers did not stop within 10s");
    }
    pool.close().await;
    tracing::info!("shutdown complete");
    Ok(())
}

async fn futures_join_all(handles: Vec<tokio::task::JoinHandle<()>>) {
    for h in handles {
        if let Err(e) = h.await {
            tracing::warn!(error = %e, "background worker ended abnormally");
        }
    }
}
