//! Outbox → OpenSearch projector and rebuild jobs (9C, contract §8).
//!
//! Both run under the sweeper scheduler's advisory lock (one replica at a time) inside the job
//! transaction; the checkpoint row moves in that transaction, so a failed `_bulk` (OpenSearch
//! down) rolls the checkpoint back and the next tick retries.
//!
//! Ordering caveat (documented in `docs/runbooks/backup-dr.md`): outbox ids are UUIDv7 taken at
//! enqueue time, so the projector only reads rows older than [`SETTLE`]; a transaction that
//! commits later than that after enqueueing is picked up by the next rebuild.

use super::SearchRuntime;
use sqlx::{PgConnection, Row};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;
use sutra_contracts::platform::search::{SearchDocument, SearchSource};
use sutra_kernel::error::KernelError;
use sutra_persistence::jobs::SweepJob;
use tracing::Instrument;
use uuid::Uuid;

/// Outbox rows younger than this are left for the next tick (late commits of UUIDv7 ids).
pub const SETTLE: &str = "2 seconds";

/// Page size of a rebuild scan.
const SCAN_PAGE: u32 = 500;

fn job_error(e: KernelError) -> sqlx::Error {
    sqlx::Error::Protocol(format!("search: {e}"))
}

fn lag_gauge(projection: &'static str, seconds: f64) {
    metrics::gauge!("sutra_search_projection_lag_seconds", "projection" => projection).set(seconds);
}

/// Writes the current documents of `ids` (grouped per tenant) and deletes the ids that no
/// longer exist. Returns the documents written.
pub async fn sync_ids(
    rt: &SearchRuntime,
    src: &dyn SearchSource,
    ids: &BTreeMap<Uuid, BTreeSet<Uuid>>,
) -> Result<u64, KernelError> {
    let projection = src.projection();
    let mut written = 0;
    for (tenant, set) in ids {
        let want: Vec<Uuid> = set.iter().copied().collect();
        for chunk in want.chunks(200) {
            let docs: Vec<SearchDocument> = src
                .fetch(*tenant, chunk)
                .await?
                .into_iter()
                .filter(|d| d.tenant_id == *tenant)
                .collect();
            let found: BTreeSet<Uuid> = docs.iter().map(|d| d.id).collect();
            let gone: Vec<Uuid> = chunk
                .iter()
                .copied()
                .filter(|id| !found.contains(id))
                .collect();
            written += rt.index.upsert(projection, &docs).await?;
            if !gone.is_empty() {
                rt.index.delete(projection, &gone).await?;
            }
        }
    }
    Ok(written)
}

/// Re-reads every document of one projection into a fresh index. Returns the documents written.
pub async fn rebuild_projection(
    rt: &SearchRuntime,
    src: &dyn SearchSource,
) -> Result<u64, KernelError> {
    let projection = src.projection();
    rt.index.recreate(projection).await?;
    let mut after = None;
    let mut written = 0;
    loop {
        let page = src.scan(after, SCAN_PAGE).await?;
        let Some(last) = page.last() else { break };
        after = Some(last.id);
        written += rt.index.upsert(projection, &page).await?;
        if page.len() < SCAN_PAGE as usize {
            break;
        }
    }
    rt.index.refresh(projection).await?;
    Ok(written)
}

/// Job `search_project`: outbox events → documents.
pub struct SearchProjector {
    pub runtime: Arc<SearchRuntime>,
    pub interval: Duration,
    pub batch_size: u32,
}

impl SearchProjector {
    async fn project_one(
        &self,
        conn: &mut PgConnection,
        src: &Arc<dyn SearchSource>,
    ) -> Result<u64, sqlx::Error> {
        let projection = src.projection();
        let cp = sqlx::query(
            "SELECT last_outbox_id FROM platform.search_checkpoint \
             WHERE projection = $1 AND rebuild_requested_at IS NULL FOR UPDATE",
        )
        .bind(projection)
        .fetch_optional(&mut *conn)
        .await?;
        // No row yet (or a rebuild pending): `search_rebuild` owns the projection.
        let Some(cp) = cp else { return Ok(0) };
        let last: Option<Uuid> = cp.try_get("last_outbox_id")?;
        let prefixes: Vec<String> = src
            .event_prefixes()
            .iter()
            .map(|p| format!("{}%", p.replace('%', "\\%").replace('_', "\\_")))
            .collect();
        let limit = i64::from(self.batch_size.max(1));
        let rows = sqlx::query(&format!(
            "SELECT id, tenant_id, event_type, subject, envelope -> 'data' AS data, created_at \
             FROM platform.outbox \
             WHERE ($1::uuid IS NULL OR id > $1) AND created_at < now() - interval '{SETTLE}' \
               AND event_type LIKE ANY($2) \
             ORDER BY id LIMIT $3"
        ))
        .bind(last)
        .bind(&prefixes)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
        let Some(tail) = rows.last() else {
            lag_gauge(projection, 0.0);
            return Ok(0);
        };
        let tail_id: Uuid = tail.try_get("id")?;
        let tail_at: chrono::DateTime<chrono::Utc> = tail.try_get("created_at")?;
        let mut ids: BTreeMap<Uuid, BTreeSet<Uuid>> = BTreeMap::new();
        for row in &rows {
            let tenant: Uuid = row.try_get("tenant_id")?;
            let event_type: String = row.try_get("event_type")?;
            if !src.handles(&event_type) {
                continue;
            }
            let subject: Option<String> = row.try_get("subject")?;
            let data: Option<serde_json::Value> = row.try_get("data")?;
            let data = data.unwrap_or(serde_json::Value::Null);
            let touched = src
                .affected(tenant, &event_type, subject.as_deref(), &data)
                .await
                .map_err(job_error)?;
            ids.entry(tenant).or_default().extend(touched);
        }
        self.runtime
            .index
            .ensure(projection)
            .await
            .map_err(job_error)?;
        let written = sync_ids(&self.runtime, src.as_ref(), &ids)
            .await
            .map_err(job_error)?;
        sqlx::query(
            "UPDATE platform.search_checkpoint SET last_outbox_id = $2, last_event_at = $3, \
             updated_at = now() WHERE projection = $1",
        )
        .bind(projection)
        .bind(tail_id)
        .bind(tail_at)
        .execute(&mut *conn)
        .await?;
        let lag = if rows.len() as i64 >= limit {
            (chrono::Utc::now() - tail_at).num_milliseconds() as f64 / 1000.0
        } else {
            0.0
        };
        lag_gauge(projection, lag.max(0.0));
        Ok(written)
    }
}

#[async_trait::async_trait]
impl SweepJob for SearchProjector {
    fn name(&self) -> &'static str {
        "search_project"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let mut total = 0;
        for src in &self.runtime.sources {
            total += self
                .project_one(&mut *conn, src)
                .instrument(tracing::info_span!(
                    "sutra.search.project",
                    projection = src.projection()
                ))
                .await?;
        }
        Ok(total)
    }
}

/// Job `search_rebuild`: full re-projection of projections without a checkpoint, with a pending
/// `rebuild_requested_at`, or — checked once per process — whose index is empty while the source
/// is not (a new or wiped OpenSearch cluster next to an existing database).
pub struct SearchRebuild {
    pub runtime: Arc<SearchRuntime>,
    pub interval: Duration,
    /// Set after the first successful tick of this process (the empty-index check ran).
    pub checked: std::sync::atomic::AtomicBool,
}

impl SearchRebuild {
    pub fn new(runtime: Arc<SearchRuntime>, interval: Duration) -> Self {
        Self {
            runtime,
            interval,
            checked: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

impl SearchRebuild {
    async fn rebuild_one(
        &self,
        conn: &mut PgConnection,
        src: &Arc<dyn SearchSource>,
    ) -> Result<u64, sqlx::Error> {
        let projection = src.projection();
        let row = sqlx::query(
            "SELECT rebuild_requested_at IS NOT NULL AS requested \
             FROM platform.search_checkpoint WHERE projection = $1",
        )
        .bind(projection)
        .fetch_optional(&mut *conn)
        .await?;
        let mut needed = match &row {
            None => true,
            Some(r) => r.try_get::<bool, _>("requested")?,
        };
        if !needed && !self.checked.load(std::sync::atomic::Ordering::Relaxed) {
            let indexed = self
                .runtime
                .index
                .count(projection, None)
                .await
                .map_err(job_error)?;
            if indexed == 0 && src.count(None).await.map_err(job_error)? > 0 {
                tracing::info!(
                    projection,
                    "search index empty but source is not: rebuilding"
                );
                needed = true;
            }
        }
        if !needed {
            return Ok(0);
        }
        let started: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
            .fetch_one(&mut *conn)
            .await?;
        // High-water mark: events up to here are covered by the scan below.
        let high: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM platform.outbox ORDER BY id DESC LIMIT 1")
                .fetch_optional(&mut *conn)
                .await?;
        let n = rebuild_projection(&self.runtime, src.as_ref())
            .await
            .map_err(job_error)?;
        sqlx::query(
            "INSERT INTO platform.search_checkpoint \
               (projection, last_outbox_id, last_event_at, rebuild_requested_at, last_rebuild_at, \
                last_rebuild_count, updated_at) \
             VALUES ($1, $2, $3, NULL, now(), $4, now()) \
             ON CONFLICT (projection) DO UPDATE SET \
               last_outbox_id = EXCLUDED.last_outbox_id, last_event_at = EXCLUDED.last_event_at, \
               rebuild_requested_at = CASE WHEN platform.search_checkpoint.rebuild_requested_at > $3 \
                 THEN platform.search_checkpoint.rebuild_requested_at ELSE NULL END, \
               last_rebuild_at = now(), last_rebuild_count = EXCLUDED.last_rebuild_count, \
               updated_at = now()",
        )
        .bind(projection)
        .bind(high)
        .bind(started)
        .bind(i64::try_from(n).unwrap_or(i64::MAX))
        .execute(&mut *conn)
        .await?;
        tracing::info!(projection, documents = n, "search rebuild finished");
        lag_gauge(projection, 0.0);
        Ok(n)
    }
}

#[async_trait::async_trait]
impl SweepJob for SearchRebuild {
    fn name(&self) -> &'static str {
        "search_rebuild"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let mut total = 0;
        for src in &self.runtime.sources {
            total += self
                .rebuild_one(&mut *conn, src)
                .instrument(tracing::info_span!(
                    "sutra.search.rebuild",
                    projection = src.projection()
                ))
                .await?;
        }
        self.checked
            .store(true, std::sync::atomic::Ordering::Relaxed);
        Ok(total)
    }
}
