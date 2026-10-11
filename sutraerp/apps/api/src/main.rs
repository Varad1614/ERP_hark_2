//! `sutra-api` binary.
//!
//! ```text
//! sutra-api            # serve (default)
//! sutra-api serve
//! sutra-api migrate    # apply migrations, then exit (ADR-L5-0008: separate from serving)
//! sutra-api openapi    # print the generated OpenAPI 3.1 document
//! ```

use std::process::ExitCode;
use sutra_api::{app, config::Config, openapi, shutdown, telemetry};

#[tokio::main]
async fn main() -> ExitCode {
    let _ = dotenvy::dotenv();
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
        "migrate" => migrate(cfg).await,
        other => Err(format!(
            "unknown command `{other}` (expected serve | migrate | openapi)"
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

async fn serve(cfg: Config) -> Result<(), String> {
    let mut ctx = app::AppContext::from_config(&cfg).map_err(|e| e.to_string())?;
    let pool = ctx.pool.clone();
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
    let server = axum::serve(listener, router).with_graceful_shutdown(signal);
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
