//! Scheduled maintenance jobs ("sweepers").
//!
//! * [`SweepJob`] — one periodic job. It runs inside a transaction that first takes
//!   `pg_try_advisory_xact_lock(lock_key(name))`, so with many API replicas exactly one runs a
//!   given job at a time; the others skip that tick. The lock is released by commit/rollback, so
//!   a crashed runner never leaves it held.
//! * [`Scheduler`] — tokio interval loops with jitter (first run spread over one interval, later
//!   runs ±10 %), stopped by a cancellation token.
//! * Jobs delete in bounded batches (`batch_limit` rows per run) to keep transactions short; a
//!   backlog drains over consecutive runs.
//!
//! Jobs owned by shared persistence live here ([`IdempotencyPurge`], [`OutboxPurge`]); module
//! owned jobs (sessions, pre-auth records) live in their module and only implement the trait.

use crate::backoff::{jittered, random_unit};
use crate::outbox_relay::sleep_or_cancel;
use sqlx::{PgConnection, PgPool};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// Upper bound for one job run's statements.
const STATEMENT_TIMEOUT: &str = "60s";

/// 64-bit FNV-1a, stable across builds and platforms (unlike `DefaultHasher`).
pub const fn lock_key_const(name: &str) -> i64 {
    let bytes = name.as_bytes();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        h ^= bytes[i] as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h as i64
}

/// Advisory-lock key of the job called `name`.
pub fn lock_key(name: &str) -> i64 {
    lock_key_const(&format!("sutra.sweeper.{name}"))
}

#[async_trait::async_trait]
pub trait SweepJob: Send + Sync + 'static {
    /// Stable name: log field and advisory-lock identity.
    fn name(&self) -> &'static str;
    fn interval(&self) -> Duration;
    /// Does the work on the locked transaction's connection; returns the rows affected.
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobOutcome {
    Ran {
        affected: u64,
    },
    /// Another replica holds the job's lock.
    Skipped,
}

/// Runs `job` once under its advisory lock.
pub async fn run_once(pool: &PgPool, job: &dyn SweepJob) -> Result<JobOutcome, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(lock_key(job.name()))
        .fetch_one(&mut *tx)
        .await?;
    if !locked {
        tx.rollback().await?;
        return Ok(JobOutcome::Skipped);
    }
    sqlx::query(&format!(
        "SET LOCAL statement_timeout = '{STATEMENT_TIMEOUT}'"
    ))
    .execute(&mut *tx)
    .await?;
    let affected = job.run(&mut tx).await?;
    tx.commit().await?;
    Ok(JobOutcome::Ran { affected })
}

/// Runs registered jobs on jittered intervals until cancelled.
pub struct Scheduler {
    pool: PgPool,
    jobs: Vec<Arc<dyn SweepJob>>,
}

impl Scheduler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool, jobs: vec![] }
    }

    pub fn with(mut self, job: Arc<dyn SweepJob>) -> Self {
        self.jobs.push(job);
        self
    }

    pub fn job_names(&self) -> Vec<&'static str> {
        self.jobs.iter().map(|j| j.name()).collect()
    }

    /// One task per job.
    pub fn spawn(self, cancel: CancellationToken) -> Vec<JoinHandle<()>> {
        self.jobs
            .into_iter()
            .map(|job| {
                let pool = self.pool.clone();
                let cancel = cancel.clone();
                tokio::spawn(async move { job_loop(pool, job, cancel).await })
            })
            .collect()
    }
}

async fn job_loop(pool: PgPool, job: Arc<dyn SweepJob>, cancel: CancellationToken) {
    let interval = job.interval();
    if interval.is_zero() {
        tracing::info!(job = job.name(), "sweeper disabled (zero interval)");
        return;
    }
    // Spread replicas and jobs over the first interval.
    if !sleep_or_cancel(&cancel, interval.mul_f64(random_unit())).await {
        return;
    }
    loop {
        let started = Instant::now();
        match run_once(&pool, job.as_ref()).await {
            Ok(JobOutcome::Ran { affected }) => tracing::info!(
                target: "sutra::metrics",
                job = job.name(),
                sweeper_affected = affected,
                sweeper_duration_ms = started.elapsed().as_millis() as u64,
                "sweeper run"
            ),
            Ok(JobOutcome::Skipped) => {
                tracing::debug!(
                    job = job.name(),
                    "sweeper skipped: another replica holds the lock"
                )
            }
            Err(e) => tracing::warn!(job = job.name(), error = %e, "sweeper run failed"),
        }
        if !sleep_or_cancel(&cancel, jittered(interval, 0.1)).await {
            break;
        }
    }
    tracing::debug!(job = job.name(), "sweeper stopped");
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn limit(n: u32) -> i64 {
    i64::from(n.max(1))
}

/// Deletes expired idempotency records (ADR-L5-0034 §3.6).
#[derive(Clone, Debug)]
pub struct IdempotencyPurge {
    pub interval: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for IdempotencyPurge {
    fn name(&self) -> &'static str {
        "idempotency-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM platform.idempotency_key WHERE ctid = ANY(ARRAY( \
               SELECT ctid FROM platform.idempotency_key WHERE expires_at < now() LIMIT $1))",
        )
        .bind(limit(self.batch_limit))
        .execute(conn)
        .await?
        .rows_affected())
    }
}

/// Deletes published outbox rows older than `retention`. Pending and dead-lettered rows are kept.
#[derive(Clone, Debug)]
pub struct OutboxPurge {
    pub interval: Duration,
    pub retention: Duration,
    pub batch_limit: u32,
}

#[async_trait::async_trait]
impl SweepJob for OutboxPurge {
    fn name(&self) -> &'static str {
        "outbox-purge"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        Ok(sqlx::query(
            "DELETE FROM platform.outbox WHERE ctid = ANY(ARRAY( \
               SELECT ctid FROM platform.outbox \
               WHERE published_at IS NOT NULL \
                 AND published_at < now() - make_interval(secs => $1) LIMIT $2))",
        )
        .bind(secs(self.retention))
        .bind(limit(self.batch_limit))
        .execute(conn)
        .await?
        .rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_keys_are_stable_and_distinct() {
        // FNV-1a test vector: "a" → 0xaf63dc4c8601ec8c.
        assert_eq!(lock_key_const("a") as u64, 0xaf63_dc4c_8601_ec8c);
        assert_eq!(lock_key("outbox-purge"), lock_key("outbox-purge"));
        assert_ne!(lock_key("outbox-purge"), lock_key("idempotency-purge"));
    }

    #[tokio::test]
    async fn scheduler_registers_jobs() {
        let pool = crate::pool::connect_lazy(&crate::pool::PoolSettings {
            url: "postgres://u:p@127.0.0.1:1/x".into(),
            max_connections: 1,
            min_connections: 0,
            acquire_timeout: Duration::from_millis(100),
            application_name: "t".into(),
        });
        let Ok(pool) = pool else { panic!("lazy pool") };
        let s = Scheduler::new(pool)
            .with(Arc::new(IdempotencyPurge {
                interval: Duration::from_secs(1),
                batch_limit: 10,
            }))
            .with(Arc::new(OutboxPurge {
                interval: Duration::from_secs(1),
                retention: Duration::from_secs(60),
                batch_limit: 10,
            }));
        assert_eq!(s.job_names(), vec!["idempotency-purge", "outbox-purge"]);
    }
}
