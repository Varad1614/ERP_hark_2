//! Audit chain sealer, verifier and anchor store (ADR-L5-0012, docs/batch9-contracts.md §4) —
//! **owned by slice 9A**.
//!
//! * [`seal_pending`] — the single writer: under `pg_try_advisory_lock(hashtext('sutra.audit.chain'))`
//!   (skips the tick when another process holds it), per tenant and in one transaction per tenant
//!   batch: lock the tenant (transaction advisory lock + `audit_chain_head … FOR UPDATE`), take up
//!   to `batch` unsealed `platform.audit_log` rows ordered by `id`, chain them with
//!   [`sutra_kernel::audit_chain::entry_hash`] continuing from the head, insert
//!   `platform.audit_chain_entry` rows and move `platform.audit_chain_head`. A row that commits
//!   late is simply sealed by a later tick (seal order is chain order).
//! * [`verify`] — recomputes every entry of a range from `platform.audit_log` and the previous
//!   entry, checks the `prev_hash` linkage, the head and every anchor MAC of the configured key;
//!   reports the first broken `seq` with a [`BreakReason`] (a missing entry `n` is reported as
//!   `n`, `link_broken`).
//! * [`anchor`] — signs every head that moved since its last anchor with HMAC-SHA-256
//!   ([`sutra_kernel::audit_chain::anchor_mac`]) into `platform.audit_anchor`.
//! * [`AuditSeal`] / [`AuditAnchor`] — the [`crate::jobs::SweepJob`] wrappers (`audit_seal`,
//!   `audit_anchor`) the composition root schedules when `SUTRA_AUDIT_CHAIN=true`.

use crate::jobs::SweepJob;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{Acquire, PgConnection, PgPool, Row};
use std::time::Duration;
use sutra_kernel::audit_chain::{anchor_mac, entry_hash, ChainRow, GENESIS_HASH};
use tracing::Instrument;
use uuid::Uuid;

/// Advisory-lock name of the single sealer (`hashtext` of it).
pub const SEAL_LOCK: &str = "sutra.audit.chain";
/// Advisory-lock namespace of one tenant's chain (`pg_advisory_xact_lock(hashtext(ns), hashtext(tenant))`).
pub const TENANT_LOCK: &str = "sutra.audit.chain.tenant";
/// Batches of `SUTRA_AUDIT_SEAL_BATCH` rows one tick may seal per tenant (bounds a tick's work
/// while letting the sealer catch up with a write burst).
const MAX_BATCHES_PER_TICK: u32 = 20;
/// Entries read per round trip by [`verify`].
const VERIFY_PAGE: i64 = 1000;

/// HMAC key used for anchors (`SUTRA_AUDIT_ANCHOR_KEY` / `SUTRA_AUDIT_ANCHOR_KEY_ID`).
#[derive(Clone)]
pub struct AnchorKey {
    pub key: [u8; 32],
    pub key_id: String,
}

impl std::fmt::Debug for AnchorKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnchorKey")
            .field("key", &"<redacted>")
            .field("key_id", &self.key_id)
            .finish()
    }
}

/// Why a chain failed verification (wire values of `reason`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakReason {
    /// The recomputed hash of an entry differs from the stored one (row or entry edited).
    HashMismatch,
    /// `prev_hash` does not link to the previous entry, a `seq` is missing, or the head moved.
    LinkBroken,
    /// The audit row an entry seals no longer exists.
    MissingRow,
    /// An anchor's MAC or head hash does not match.
    AnchorMismatch,
}

impl BreakReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HashMismatch => "hash_mismatch",
            Self::LinkBroken => "link_broken",
            Self::MissingRow => "missing_row",
            Self::AnchorMismatch => "anchor_mismatch",
        }
    }
}

/// Outcome of one [`seal_pending`] call.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SealReport {
    /// Entries appended over all tenants.
    pub sealed: u64,
    /// Tenants that had unsealed rows.
    pub tenants: u64,
    /// Another sealer held the lock; nothing was done.
    pub skipped: bool,
}

/// Outcome of one [`verify`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    pub ok: bool,
    /// Entries recomputed.
    pub checked: u64,
    pub first_broken_seq: Option<i64>,
    pub reason: Option<BreakReason>,
    /// Current head (`0` / `None` before the first seal).
    pub head_seq: i64,
    pub head_hash: Option<[u8; 32]>,
}

/// Head + counts of one tenant chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChainStatus {
    pub head_seq: i64,
    pub head_hash: Option<[u8; 32]>,
    pub sealed: i64,
    pub unsealed: i64,
    pub last_sealed_at: Option<DateTime<Utc>>,
    pub last_anchor: Option<AnchorSummary>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorSummary {
    pub seq: i64,
    pub key_id: String,
    pub anchored_at: DateTime<Utc>,
}

/// One stored anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnchorRecord {
    pub seq: i64,
    pub head_hash: [u8; 32],
    pub key_id: String,
    pub mac: [u8; 32],
    pub anchored_at: DateTime<Utc>,
}

/// An owned `platform.audit_log` row (all 12 columns).
#[derive(Clone, Debug, PartialEq)]
pub struct AuditRow {
    pub id: Uuid,
    pub occurred_at: DateTime<Utc>,
    pub tenant_id: Uuid,
    pub institution_id: Option<Uuid>,
    pub actor_kind: String,
    pub actor_id: Option<String>,
    pub action: String,
    pub target_type: Option<String>,
    pub target_id: Option<String>,
    pub outcome: String,
    pub correlation_id: Option<String>,
    pub details: serde_json::Value,
}

impl AuditRow {
    pub fn as_chain_row(&self) -> ChainRow<'_> {
        ChainRow {
            id: self.id,
            occurred_at: self.occurred_at,
            tenant_id: self.tenant_id,
            institution_id: self.institution_id,
            actor_kind: &self.actor_kind,
            actor_id: self.actor_id.as_deref(),
            action: &self.action,
            target_type: self.target_type.as_deref(),
            target_id: self.target_id.as_deref(),
            outcome: &self.outcome,
            correlation_id: self.correlation_id.as_deref(),
            details: &self.details,
        }
    }

    /// The entry hash of this row on top of `prev`.
    pub fn hash(&self, prev: &[u8; 32]) -> [u8; 32] {
        entry_hash(prev, &self.as_chain_row())
    }

    /// Reads the `a_`-aliased audit columns of a joined row ([`AUDIT_COLUMNS`]).
    fn from_row(r: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: r.try_get("a_id")?,
            occurred_at: r.try_get("a_occurred_at")?,
            tenant_id: r.try_get("a_tenant_id")?,
            institution_id: r.try_get("a_institution_id")?,
            actor_kind: r.try_get("a_actor_kind")?,
            actor_id: r.try_get("a_actor_id")?,
            action: r.try_get("a_action")?,
            target_type: r.try_get("a_target_type")?,
            target_id: r.try_get("a_target_id")?,
            outcome: r.try_get("a_outcome")?,
            correlation_id: r.try_get("a_correlation_id")?,
            details: r.try_get("a_details")?,
        })
    }
}

/// The 12 audit columns, aliased with `a_` for joins.
const AUDIT_COLUMNS: &str =
    "a.id AS a_id, a.occurred_at AS a_occurred_at, a.tenant_id AS a_tenant_id, \
    a.institution_id AS a_institution_id, a.actor_kind AS a_actor_kind, a.actor_id AS a_actor_id, \
    a.action AS a_action, a.target_type AS a_target_type, a.target_id AS a_target_id, \
    a.outcome AS a_outcome, a.correlation_id AS a_correlation_id, a.details AS a_details";

/// Reads one audit row back from PostgreSQL (tests use it for canonical-hash stability).
pub async fn load_audit_row(pool: &PgPool, id: Uuid) -> Result<Option<AuditRow>, sqlx::Error> {
    let row = sqlx::query(&format!(
        "SELECT {AUDIT_COLUMNS} FROM platform.audit_log a WHERE a.id = $1"
    ))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    match row {
        Some(r) => Ok(Some(AuditRow::from_row(&r)?)),
        None => Ok(None),
    }
}

/// `(seq, head_hash, key_id, mac, anchored_at)` of `platform.audit_anchor`.
type AnchorTuple = (i64, Vec<u8>, String, Vec<u8>, DateTime<Utc>);
/// `(seq, head_hash, mac, entry_hash at seq)` for anchor verification.
type AnchorCheckTuple = (i64, Vec<u8>, Vec<u8>, Option<Vec<u8>>);

fn to_hash(v: &[u8]) -> Result<[u8; 32], sqlx::Error> {
    <[u8; 32]>::try_from(v)
        .map_err(|_| sqlx::Error::Protocol("audit chain hash is not 32 bytes".into()))
}

/// Seals up to `batch` unsealed rows per tenant. Returns `skipped` when another sealer runs.
pub async fn seal_pending(pool: &PgPool, batch: u32) -> Result<SealReport, sqlx::Error> {
    let span = tracing::info_span!("sutra.audit.seal", sealed = tracing::field::Empty);
    seal_pending_in_span(pool, batch).instrument(span).await
}

async fn seal_pending_in_span(pool: &PgPool, batch: u32) -> Result<SealReport, sqlx::Error> {
    let mut conn = pool.acquire().await?;
    let locked: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock(hashtext($1))")
        .bind(SEAL_LOCK)
        .fetch_one(&mut *conn)
        .await?;
    if !locked {
        return Ok(SealReport {
            skipped: true,
            ..SealReport::default()
        });
    }
    let result = seal_locked(&mut conn, batch).await;
    let unlocked = sqlx::query_scalar::<_, bool>("SELECT pg_advisory_unlock(hashtext($1))")
        .bind(SEAL_LOCK)
        .fetch_one(&mut *conn)
        .await;
    if unlocked.is_err() {
        // Never return a connection that may still hold the session lock to the pool.
        let _ = conn.detach();
    }
    let report = result?;
    unlocked?;
    tracing::Span::current().record("sealed", report.sealed);
    Ok(report)
}

async fn seal_locked(conn: &mut PgConnection, batch: u32) -> Result<SealReport, sqlx::Error> {
    let tenants: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT a.tenant_id FROM platform.audit_log a \
         WHERE NOT EXISTS (SELECT 1 FROM platform.audit_chain_entry e WHERE e.audit_id = a.id)",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut report = SealReport::default();
    for tenant in tenants {
        // PERF: drain the tenant's backlog in short batches (one transaction each) instead of
        // one batch per tick: at the capacity gate's ~130 audited writes/s a single 500-row
        // batch every 10 s fell behind for good, so every tick re-scanned a growing backlog.
        let mut sealed = 0;
        for _ in 0..MAX_BATCHES_PER_TICK {
            let n = seal_tenant(&mut *conn, tenant, batch).await?;
            sealed += n;
            if n < u64::from(batch.max(1)) {
                break;
            }
        }
        if sealed > 0 {
            report.tenants += 1;
            report.sealed += sealed;
        }
    }
    Ok(report)
}

/// Seals one batch of one tenant in one transaction; returns the entries appended.
pub async fn seal_tenant(
    conn: &mut PgConnection,
    tenant: Uuid,
    batch: u32,
) -> Result<u64, sqlx::Error> {
    let mut tx = conn.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext($1), hashtext($2::uuid::text))")
        .bind(TENANT_LOCK)
        .bind(tenant)
        .execute(&mut *tx)
        .await?;
    let head: Option<(i64, Vec<u8>)> = sqlx::query_as(
        "SELECT seq, head_hash FROM platform.audit_chain_head WHERE tenant_id = $1 FOR UPDATE",
    )
    .bind(tenant)
    .fetch_optional(&mut *tx)
    .await?;
    let (mut seq, mut prev) = match head {
        Some((s, h)) => (s, to_hash(&h)?),
        None => (0, GENESIS_HASH),
    };
    let rows = sqlx::query(&format!(
        "SELECT {AUDIT_COLUMNS} FROM platform.audit_log a \
         LEFT JOIN platform.audit_chain_entry e ON e.audit_id = a.id \
         WHERE e.audit_id IS NULL AND a.tenant_id = $1 ORDER BY a.id LIMIT $2"
    ))
    .bind(tenant)
    .bind(i64::from(batch.max(1)))
    .fetch_all(&mut *tx)
    .await?;
    if rows.is_empty() {
        tx.rollback().await?;
        return Ok(0);
    }
    // PERF: one multi-row INSERT per batch (UNNEST of the column arrays) instead of one round
    // trip per entry, so the tenant lock and the transaction are held for milliseconds.
    let mut seqs: Vec<i64> = Vec::with_capacity(rows.len());
    let mut ids: Vec<Uuid> = Vec::with_capacity(rows.len());
    let mut prevs: Vec<Vec<u8>> = Vec::with_capacity(rows.len());
    let mut hashes: Vec<Vec<u8>> = Vec::with_capacity(rows.len());
    for r in &rows {
        let row = AuditRow::from_row(r)?;
        let hash = row.hash(&prev);
        seq += 1;
        seqs.push(seq);
        ids.push(row.id);
        prevs.push(prev.to_vec());
        hashes.push(hash.to_vec());
        prev = hash;
    }
    sqlx::query(
        "INSERT INTO platform.audit_chain_entry (tenant_id, seq, audit_id, prev_hash, entry_hash) \
         SELECT $1, t.seq, t.audit_id, t.prev_hash, t.entry_hash \
         FROM UNNEST($2::bigint[], $3::uuid[], $4::bytea[], $5::bytea[]) \
         AS t(seq, audit_id, prev_hash, entry_hash)",
    )
    .bind(tenant)
    .bind(seqs)
    .bind(ids)
    .bind(prevs)
    .bind(hashes)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO platform.audit_chain_head (tenant_id, seq, head_hash, updated_at) \
         VALUES ($1, $2, $3, now()) \
         ON CONFLICT (tenant_id) DO UPDATE SET seq = EXCLUDED.seq, head_hash = EXCLUDED.head_hash, \
         updated_at = now()",
    )
    .bind(tenant)
    .bind(seq)
    .bind(prev.as_slice())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(u64::try_from(rows.len()).unwrap_or(u64::MAX))
}

/// Rows of `tenant` (all tenants when `None`) that have no chain entry yet.
pub async fn unsealed_count(pool: &PgPool, tenant: Option<Uuid>) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log a \
         WHERE ($1::uuid IS NULL OR a.tenant_id = $1) \
         AND NOT EXISTS (SELECT 1 FROM platform.audit_chain_entry e WHERE e.audit_id = a.id)",
    )
    .bind(tenant)
    .fetch_one(pool)
    .await
}

/// Rows of all tenants without a chain entry, computed as `count(audit_log)` minus the sum of
/// the chain heads' `seq` (every entry references one audit row and `seq` counts a tenant's
/// entries), so the `/metrics` gauge refresh never runs the anti-join over the whole log.
/// PERF: [`unsealed_count`] stays the exact per-tenant answer of the status API.
pub async fn unsealed_total_estimate(pool: &PgPool) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT GREATEST(0, (SELECT count(*) FROM platform.audit_log) \
         - COALESCE((SELECT sum(seq) FROM platform.audit_chain_head), 0))::bigint",
    )
    .fetch_one(pool)
    .await
}

async fn head(pool: &PgPool, tenant: Uuid) -> Result<Option<(i64, [u8; 32])>, sqlx::Error> {
    let row: Option<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT seq, head_hash FROM platform.audit_chain_head WHERE tenant_id = $1")
            .bind(tenant)
            .fetch_optional(pool)
            .await?;
    match row {
        Some((s, h)) => Ok(Some((s, to_hash(&h)?))),
        None => Ok(None),
    }
}

/// Head, counts, last seal and last anchor of a tenant chain.
pub async fn status(pool: &PgPool, tenant: Uuid) -> Result<ChainStatus, sqlx::Error> {
    let head = head(pool, tenant).await?;
    let (sealed, last_sealed_at): (i64, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT count(*), max(sealed_at) FROM platform.audit_chain_entry WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_one(pool)
    .await?;
    let unsealed = unsealed_count(pool, Some(tenant)).await?;
    let last_anchor: Option<(i64, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT seq, key_id, anchored_at FROM platform.audit_anchor WHERE tenant_id = $1 \
         ORDER BY anchored_at DESC, seq DESC LIMIT 1",
    )
    .bind(tenant)
    .fetch_optional(pool)
    .await?;
    Ok(ChainStatus {
        head_seq: head.map_or(0, |h| h.0),
        head_hash: head.map(|h| h.1),
        sealed,
        unsealed,
        last_sealed_at,
        last_anchor: last_anchor.map(|(seq, key_id, anchored_at)| AnchorSummary {
            seq,
            key_id,
            anchored_at,
        }),
    })
}

/// Newest anchors of a tenant (`limit` clamped to 1…500).
pub async fn list_anchors(
    pool: &PgPool,
    tenant: Uuid,
    limit: i64,
) -> Result<Vec<AnchorRecord>, sqlx::Error> {
    let rows: Vec<AnchorTuple> = sqlx::query_as(
        "SELECT seq, head_hash, key_id, mac, anchored_at FROM platform.audit_anchor \
         WHERE tenant_id = $1 ORDER BY anchored_at DESC, seq DESC LIMIT $2",
    )
    .bind(tenant)
    .bind(limit.clamp(1, 500))
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (seq, head_hash, key_id, mac, anchored_at) in rows {
        out.push(AnchorRecord {
            seq,
            head_hash: to_hash(&head_hash)?,
            key_id,
            mac: to_hash(&mac)?,
            anchored_at,
        });
    }
    Ok(out)
}

fn broken(
    checked: u64,
    seq: i64,
    reason: BreakReason,
    head: Option<(i64, [u8; 32])>,
) -> VerifyReport {
    VerifyReport {
        ok: false,
        checked,
        first_broken_seq: Some(seq),
        reason: Some(reason),
        head_seq: head.map_or(0, |h| h.0),
        head_hash: head.map(|h| h.1),
    }
}

fn intact(checked: u64, head: Option<(i64, [u8; 32])>) -> VerifyReport {
    VerifyReport {
        ok: true,
        checked,
        first_broken_seq: None,
        reason: None,
        head_seq: head.map_or(0, |h| h.0),
        head_hash: head.map(|h| h.1),
    }
}

/// Recomputes entries `from_seq ..= to_seq` (default: the whole chain) of `tenant` and checks
/// the linkage, the head and the anchors signed with `key` (anchors of other key ids are
/// skipped). Returns the first broken `seq`.
pub async fn verify(
    pool: &PgPool,
    tenant: Uuid,
    from_seq: Option<i64>,
    to_seq: Option<i64>,
    key: Option<&AnchorKey>,
) -> Result<VerifyReport, sqlx::Error> {
    let head = head(pool, tenant).await?;
    let head_seq = head.map_or(0, |h| h.0);
    if head.is_none() {
        // No head yet: an orphan entry would be a broken chain.
        let orphan: Option<i64> = sqlx::query_scalar(
            "SELECT min(seq) FROM platform.audit_chain_entry WHERE tenant_id = $1",
        )
        .bind(tenant)
        .fetch_one(pool)
        .await?;
        return Ok(match orphan {
            Some(seq) => broken(0, seq, BreakReason::LinkBroken, head),
            None => intact(0, head),
        });
    }
    let from = from_seq.unwrap_or(1).max(1);
    let to = to_seq.unwrap_or(head_seq).min(head_seq);
    if from > to {
        return Ok(intact(0, head));
    }
    let mut prev = if from == 1 {
        GENESIS_HASH
    } else {
        let before: Option<Vec<u8>> = sqlx::query_scalar(
            "SELECT entry_hash FROM platform.audit_chain_entry WHERE tenant_id = $1 AND seq = $2",
        )
        .bind(tenant)
        .bind(from - 1)
        .fetch_optional(pool)
        .await?;
        match before {
            Some(h) => to_hash(&h)?,
            None => return Ok(broken(0, from - 1, BreakReason::LinkBroken, head)),
        }
    };
    let mut expected = from;
    let mut checked = 0u64;
    while expected <= to {
        let rows = sqlx::query(&format!(
            "SELECT e.seq, e.prev_hash, e.entry_hash, (a.id IS NOT NULL) AS row_exists, {AUDIT_COLUMNS} \
             FROM platform.audit_chain_entry e \
             LEFT JOIN platform.audit_log a ON a.id = e.audit_id \
             WHERE e.tenant_id = $1 AND e.seq >= $2 AND e.seq <= $3 \
             ORDER BY e.seq LIMIT {VERIFY_PAGE}"
        ))
        .bind(tenant)
        .bind(expected)
        .bind(to)
        .fetch_all(pool)
        .await?;
        if rows.is_empty() {
            break;
        }
        for r in &rows {
            let seq: i64 = r.try_get("seq")?;
            if seq != expected {
                return Ok(broken(checked, expected, BreakReason::LinkBroken, head));
            }
            let stored_prev = to_hash(&r.try_get::<Vec<u8>, _>("prev_hash")?)?;
            let stored = to_hash(&r.try_get::<Vec<u8>, _>("entry_hash")?)?;
            if stored_prev != prev {
                return Ok(broken(checked, seq, BreakReason::LinkBroken, head));
            }
            if !r.try_get::<bool, _>("row_exists")? {
                return Ok(broken(checked, seq, BreakReason::MissingRow, head));
            }
            let row = AuditRow::from_row(r)?;
            if row.tenant_id != tenant || row.hash(&prev) != stored {
                return Ok(broken(checked, seq, BreakReason::HashMismatch, head));
            }
            prev = stored;
            expected += 1;
            checked += 1;
        }
    }
    if expected <= to {
        return Ok(broken(checked, expected, BreakReason::LinkBroken, head));
    }
    if to == head_seq && head.map(|h| h.1) != Some(prev) {
        return Ok(broken(checked, head_seq, BreakReason::LinkBroken, head));
    }
    if let Some(k) = key {
        let anchors: Vec<AnchorCheckTuple> = sqlx::query_as(
            "SELECT an.seq, an.head_hash, an.mac, e.entry_hash FROM platform.audit_anchor an \
             LEFT JOIN platform.audit_chain_entry e ON e.tenant_id = an.tenant_id AND e.seq = an.seq \
             WHERE an.tenant_id = $1 AND an.key_id = $2 AND an.seq >= $3 AND an.seq <= $4 \
             ORDER BY an.seq",
        )
        .bind(tenant)
        .bind(&k.key_id)
        .bind(from)
        .bind(to)
        .fetch_all(pool)
        .await?;
        for (seq, head_hash, mac, entry) in anchors {
            let head_hash = to_hash(&head_hash)?;
            let expected_mac = anchor_mac(&k.key, tenant, seq, &head_hash)
                .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
            let entry_matches = entry.as_deref() == Some(head_hash.as_slice());
            if !sutra_kernel::secret::ct_eq(&expected_mac, &mac) || !entry_matches {
                return Ok(broken(checked, seq, BreakReason::AnchorMismatch, head));
            }
        }
    }
    Ok(intact(checked, head))
}

/// Writes one anchor per tenant whose head has no anchor of `key.key_id` yet; returns the
/// anchors written.
pub async fn anchor(pool: &PgPool, key: &AnchorKey) -> Result<u64, sqlx::Error> {
    let heads: Vec<(Uuid, i64, Vec<u8>)> = sqlx::query_as(
        "SELECT h.tenant_id, h.seq, h.head_hash FROM platform.audit_chain_head h \
         WHERE NOT EXISTS (SELECT 1 FROM platform.audit_anchor a \
           WHERE a.tenant_id = h.tenant_id AND a.seq = h.seq AND a.key_id = $1)",
    )
    .bind(&key.key_id)
    .fetch_all(pool)
    .await?;
    let mut written = 0u64;
    for (tenant, seq, head_hash) in heads {
        let head_hash = to_hash(&head_hash)?;
        let mac = anchor_mac(&key.key, tenant, seq, &head_hash)
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        written += sqlx::query(
            "INSERT INTO platform.audit_anchor (tenant_id, seq, head_hash, key_id, mac) \
             VALUES ($1, $2, $3, $4, $5) ON CONFLICT (tenant_id, seq, key_id) DO NOTHING",
        )
        .bind(tenant)
        .bind(seq)
        .bind(head_hash.as_slice())
        .bind(&key.key_id)
        .bind(mac.as_slice())
        .execute(pool)
        .await?
        .rows_affected();
    }
    Ok(written)
}

/// `audit_seal` job: [`seal_pending`] every `interval` (`SUTRA_AUDIT_SEAL_SECS`).
#[derive(Clone, Debug)]
pub struct AuditSeal {
    pub pool: PgPool,
    pub batch: u32,
    pub interval: Duration,
}

#[async_trait::async_trait]
impl SweepJob for AuditSeal {
    fn name(&self) -> &'static str {
        "audit_seal"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    /// Seals on its own connections (one transaction per tenant batch); the scheduler's locked
    /// transaction only keeps replicas from running the job concurrently.
    async fn run(&self, _conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let span = tracing::info_span!("sutra.job", sutra.job.name = "audit_seal");
        let report = seal_pending(&self.pool, self.batch)
            .instrument(span)
            .await?;
        Ok(report.sealed)
    }
}

/// `audit_anchor` job: [`anchor`] every `interval` (`SUTRA_AUDIT_ANCHOR_SECS`).
#[derive(Clone)]
pub struct AuditAnchor {
    pub pool: PgPool,
    pub key: AnchorKey,
    pub interval: Duration,
}

impl std::fmt::Debug for AuditAnchor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuditAnchor")
            .field("key", &self.key)
            .field("interval", &self.interval)
            .finish()
    }
}

#[async_trait::async_trait]
impl SweepJob for AuditAnchor {
    fn name(&self) -> &'static str {
        "audit_anchor"
    }
    fn interval(&self) -> Duration {
        self.interval
    }
    async fn run(&self, _conn: &mut PgConnection) -> Result<u64, sqlx::Error> {
        let span = tracing::info_span!("sutra.job", sutra.job.name = "audit_anchor");
        anchor(&self.pool, &self.key).instrument(span).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_have_wire_names() {
        assert_eq!(BreakReason::HashMismatch.as_str(), "hash_mismatch");
        assert_eq!(BreakReason::LinkBroken.as_str(), "link_broken");
        assert_eq!(BreakReason::MissingRow.as_str(), "missing_row");
        assert_eq!(BreakReason::AnchorMismatch.as_str(), "anchor_mismatch");
    }

    #[test]
    fn anchor_key_debug_is_redacted() {
        let k = AnchorKey {
            key: [9u8; 32],
            key_id: "k1".into(),
        };
        let dbg = format!("{k:?}");
        assert!(dbg.contains("redacted") && dbg.contains("k1") && !dbg.contains("9, 9"));
    }

    #[test]
    fn owned_rows_hash_like_the_kernel_contract() {
        let row = AuditRow {
            id: Uuid::nil(),
            occurred_at: DateTime::<Utc>::from_timestamp(1_800_000_000, 123_456_000).unwrap(),
            tenant_id: Uuid::nil(),
            institution_id: None,
            actor_kind: "system".into(),
            actor_id: None,
            action: "x.y".into(),
            target_type: None,
            target_id: None,
            outcome: "succeeded".into(),
            correlation_id: None,
            details: serde_json::json!({"b": 1, "a": [1, 2]}),
        };
        assert_eq!(
            row.hash(&GENESIS_HASH),
            entry_hash(&GENESIS_HASH, &row.as_chain_row())
        );
        assert_ne!(row.hash(&GENESIS_HASH), row.hash(&[1u8; 32]));
    }
}
