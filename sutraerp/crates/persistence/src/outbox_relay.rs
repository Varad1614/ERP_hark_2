//! Outbox relay: committed publication intents → broker (ADR-L5-0007 / 0010 / 0022).
//!
//! One relay pass ([`OutboxRelay::run_once`]):
//!
//! 1. **Claim** — a short transaction takes the relay-claim advisory lock, selects up to
//!    `batch_size` due rows in `created_at` order with `FOR UPDATE SKIP LOCKED` and leases them
//!    (`claim_token`, `next_attempt_at = now() + lease`). An event is held back while an earlier
//!    event of the same `(tenant, subject)` is still pending and not due, which keeps
//!    per-aggregate order across retries.
//! 2. **Publish** — rows are published one by one in claim order with `msg_id` = outbox id
//!    (`Nats-Msg-Id`), each bounded by `publish_timeout`.
//! 3. **Mark** — only after the broker ack is the row stamped `published_at`. A transient failure
//!    schedules a retry with exponential backoff + jitter and stops the pass (the broker is most
//!    likely down; the remaining rows are released). A permanent failure, or reaching
//!    `max_attempts`, dead-letters the row. Later rows of a failed row's aggregate are released.
//!
//! Every mark is guarded by the claim token, so a relay whose lease expired cannot overwrite
//! another relay's outcome. Delivery is at-least-once; the broker de-duplicates re-publishes
//! within its duplicate window and consumers stay idempotent.

use crate::backoff::Backoff;
use sqlx::{PgPool, Row};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use sutra_kernel::messaging::{subject_for, MessagePublisher, OutboundMessage, PublishError};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Advisory-lock key serializing relay claims (FNV-1a of `sutra.outbox.relay.claim`).
pub const RELAY_CLAIM_LOCK: i64 = crate::jobs::lock_key_const("sutra.outbox.relay.claim");

const MAX_ERROR_LEN: usize = 1000;

#[derive(Clone, Debug)]
pub struct RelaySettings {
    pub batch_size: u32,
    /// Sleep between passes when the outbox is drained.
    pub poll_interval: Duration,
    /// How long claimed rows stay reserved for this relay.
    pub lease: Duration,
    /// Attempts before a row is dead-lettered.
    pub max_attempts: u32,
    pub backoff: Backoff,
    pub publish_timeout: Duration,
    /// First subject token(s), e.g. `sutra`.
    pub subject_prefix: String,
    /// Restricts the relay to one tenant (tests, tenant-sharded relays). `None` = all tenants.
    pub tenant: Option<Uuid>,
}

impl Default for RelaySettings {
    fn default() -> Self {
        Self {
            batch_size: 100,
            poll_interval: Duration::from_millis(500),
            lease: Duration::from_secs(30),
            max_attempts: 12,
            backoff: Backoff::new(Duration::from_secs(1), Duration::from_secs(300)),
            publish_timeout: Duration::from_secs(5),
            subject_prefix: "sutra".into(),
            tenant: None,
        }
    }
}

/// Outcome counts of one pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RelayReport {
    pub claimed: usize,
    pub published: usize,
    /// Broker reported the message as already stored (de-duplicated) — counted as published too.
    pub duplicates: usize,
    pub retried: usize,
    pub dead_lettered: usize,
    /// Claimed but handed back unpublished (ordering hold or the pass stopped early).
    pub released: usize,
    /// The pass stopped on a transient broker failure.
    pub broker_unavailable: bool,
}

struct Claimed {
    id: Uuid,
    tenant_id: Uuid,
    event_type: String,
    subject: Option<String>,
    envelope: serde_json::Value,
    attempts: i32,
}

pub struct OutboxRelay {
    pool: PgPool,
    publisher: Arc<dyn MessagePublisher>,
    settings: RelaySettings,
}

fn secs(d: Duration) -> f64 {
    d.as_secs_f64()
}

fn truncate(mut s: String) -> String {
    if s.len() > MAX_ERROR_LEN {
        let mut cut = MAX_ERROR_LEN;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
    }
    s
}

impl OutboxRelay {
    pub fn new(
        pool: PgPool,
        publisher: Arc<dyn MessagePublisher>,
        settings: RelaySettings,
    ) -> Self {
        Self {
            pool,
            publisher,
            settings,
        }
    }

    pub fn settings(&self) -> &RelaySettings {
        &self.settings
    }

    async fn claim(&self) -> Result<(Uuid, Vec<Claimed>), sqlx::Error> {
        let token = sutra_kernel::id::new_v7();
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT pg_advisory_xact_lock($1)")
            .bind(RELAY_CLAIM_LOCK)
            .execute(&mut *tx)
            .await?;
        let rows = sqlx::query(
            "WITH due AS ( \
               SELECT o.id FROM platform.outbox o \
               WHERE o.published_at IS NULL AND o.dead_lettered_at IS NULL \
                 AND o.next_attempt_at <= now() \
                 AND ($3::uuid IS NULL OR o.tenant_id = $3) \
                 AND (o.subject IS NULL OR NOT EXISTS ( \
                   SELECT 1 FROM platform.outbox e \
                   WHERE e.tenant_id = o.tenant_id AND e.subject = o.subject \
                     AND e.published_at IS NULL AND e.dead_lettered_at IS NULL \
                     AND e.next_attempt_at > now() \
                     AND (e.created_at, e.id) < (o.created_at, o.id))) \
               ORDER BY o.created_at, o.id \
               LIMIT $1 \
               FOR UPDATE OF o SKIP LOCKED) \
             UPDATE platform.outbox t \
                SET claim_token = $2, last_attempt_at = now(), \
                    next_attempt_at = now() + make_interval(secs => $4) \
               FROM due WHERE t.id = due.id \
             RETURNING t.id, t.tenant_id, t.event_type, t.subject, t.envelope, t.attempts, t.created_at",
        )
        .bind(i64::from(self.settings.batch_size.max(1)))
        .bind(token)
        .bind(self.settings.tenant)
        .bind(secs(self.settings.lease))
        .fetch_all(&mut *tx)
        .await?;
        tx.commit().await?;
        let mut claimed = Vec::with_capacity(rows.len());
        for r in rows {
            let created: chrono::DateTime<chrono::Utc> = r.try_get("created_at")?;
            let c = Claimed {
                id: r.try_get("id")?,
                tenant_id: r.try_get("tenant_id")?,
                event_type: r.try_get("event_type")?,
                subject: r.try_get("subject")?,
                envelope: r.try_get("envelope")?,
                attempts: r.try_get("attempts")?,
            };
            claimed.push((created, c));
        }
        // RETURNING has no defined order; restore the claim order.
        claimed.sort_by_key(|a| (a.0, a.1.id));
        Ok((token, claimed.into_iter().map(|(_, c)| c).collect()))
    }

    fn message(&self, row: &Claimed) -> Result<OutboundMessage, PublishError> {
        let subject = subject_for(
            &self.settings.subject_prefix,
            row.tenant_id,
            &row.event_type,
        )
        .map_err(|e| PublishError::Permanent(e.to_string()))?;
        let payload = serde_json::to_vec(&row.envelope)
            .map_err(|e| PublishError::Permanent(format!("envelope not serializable: {e}")))?;
        Ok(OutboundMessage {
            subject,
            msg_id: row.id,
            headers: vec![
                (
                    "Content-Type".into(),
                    "application/cloudevents+json; charset=utf-8".into(),
                ),
                ("ce-specversion".into(), "1.0".into()),
                ("ce-id".into(), row.id.to_string()),
                ("ce-type".into(), row.event_type.clone()),
                ("sutra-tenant".into(), row.tenant_id.to_string()),
            ],
            payload,
        })
    }

    async fn mark_published(&self, id: Uuid, token: Uuid) -> Result<bool, sqlx::Error> {
        let n = sqlx::query(
            "UPDATE platform.outbox SET published_at = now(), attempts = attempts + 1, \
             last_error = NULL, claim_token = NULL \
             WHERE id = $1 AND claim_token = $2 AND published_at IS NULL",
        )
        .bind(id)
        .bind(token)
        .execute(&self.pool)
        .await?
        .rows_affected();
        Ok(n == 1)
    }

    /// Records a failed attempt. Returns `true` when the row was dead-lettered.
    async fn mark_failed(
        &self,
        row: &Claimed,
        token: Uuid,
        err: &PublishError,
    ) -> Result<bool, sqlx::Error> {
        let attempt = u32::try_from(row.attempts.max(0))
            .unwrap_or(0)
            .saturating_add(1);
        let dead = err.is_permanent() || attempt >= self.settings.max_attempts;
        let message = truncate(err.to_string());
        if dead {
            sqlx::query(
                "UPDATE platform.outbox SET attempts = attempts + 1, last_error = $3, \
                 dead_lettered_at = now(), claim_token = NULL \
                 WHERE id = $1 AND claim_token = $2",
            )
            .bind(row.id)
            .bind(token)
            .bind(&message)
            .execute(&self.pool)
            .await?;
            tracing::error!(
                outbox.id = %row.id,
                event.type = %row.event_type,
                attempts = attempt,
                error = %message,
                "outbox event dead-lettered"
            );
        } else {
            let delay = self.settings.backoff.delay(attempt);
            sqlx::query(
                "UPDATE platform.outbox SET attempts = attempts + 1, last_error = $3, \
                 next_attempt_at = now() + make_interval(secs => $4), claim_token = NULL \
                 WHERE id = $1 AND claim_token = $2",
            )
            .bind(row.id)
            .bind(token)
            .bind(&message)
            .bind(secs(delay))
            .execute(&self.pool)
            .await?;
            tracing::warn!(
                outbox.id = %row.id,
                attempts = attempt,
                retry_in_ms = delay.as_millis() as u64,
                error = %message,
                "outbox publish failed; will retry"
            );
        }
        Ok(dead)
    }

    async fn release(&self, id: Uuid, token: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query(
            "UPDATE platform.outbox SET claim_token = NULL, next_attempt_at = now() \
             WHERE id = $1 AND claim_token = $2",
        )
        .bind(id)
        .bind(token)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// One claim → publish → mark pass. Part 1 S3 (DEF-X-003): the relay serves every tenant's
    /// outbox, so the pass runs in the system database context.
    pub async fn run_once(&self) -> Result<RelayReport, sqlx::Error> {
        sutra_kernel::db_context::system(self.pass()).await
    }

    async fn pass(&self) -> Result<RelayReport, sqlx::Error> {
        let (token, rows) = self.claim().await?;
        let mut report = RelayReport {
            claimed: rows.len(),
            ..RelayReport::default()
        };
        let mut held: HashSet<(Uuid, String)> = HashSet::new();
        let mut stop = false;
        for row in &rows {
            let key = row.subject.clone().map(|s| (row.tenant_id, s));
            if stop || key.as_ref().is_some_and(|k| held.contains(k)) {
                self.release(row.id, token).await?;
                report.released += 1;
                continue;
            }
            let outcome = match self.message(row) {
                Ok(msg) => {
                    match tokio::time::timeout(
                        self.settings.publish_timeout,
                        self.publisher.publish(&msg),
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(PublishError::Transient(format!(
                            "no broker ack within {:?}",
                            self.settings.publish_timeout
                        ))),
                    }
                }
                Err(e) => Err(e),
            };
            match outcome {
                Ok(ack) => {
                    if !self.mark_published(row.id, token).await? {
                        tracing::warn!(outbox.id = %row.id, "lease lost before marking published (row was re-claimed)");
                    }
                    report.published += 1;
                    if ack.duplicate {
                        report.duplicates += 1;
                    }
                }
                Err(e) => {
                    if self.mark_failed(row, token, &e).await? {
                        report.dead_lettered += 1;
                    } else {
                        report.retried += 1;
                    }
                    if let Some(k) = key {
                        held.insert(k);
                    }
                    if !e.is_permanent() {
                        report.broker_unavailable = true;
                        stop = true;
                    }
                }
            }
        }
        Ok(report)
    }

    /// Relays until `cancel` fires. Prepares the publisher first (retrying with backoff), drains
    /// full batches back to back and sleeps `poll_interval` when idle. A pass in progress is
    /// finished before shutdown.
    pub async fn run(self, cancel: CancellationToken) {
        let mut attempt = 0u32;
        loop {
            let prepared = tokio::select! {
                _ = cancel.cancelled() => return,
                r = self.publisher.prepare() => r,
            };
            match prepared {
                Ok(()) => break,
                Err(e) => {
                    attempt = attempt.saturating_add(1);
                    let d = self.settings.backoff.delay(attempt);
                    tracing::warn!(publisher = self.publisher.name(), error = %e, retry_in_ms = d.as_millis() as u64, "outbox publisher not ready");
                    if !sleep_or_cancel(&cancel, d).await {
                        return;
                    }
                }
            }
        }
        tracing::info!(
            publisher = self.publisher.name(),
            batch = self.settings.batch_size,
            "outbox relay started"
        );
        let mut failures = 0u32;
        while !cancel.is_cancelled() {
            let pause = match self.run_once().await {
                Ok(r) => {
                    failures = if r.broker_unavailable {
                        failures.saturating_add(1)
                    } else {
                        0
                    };
                    if r.claimed > 0 {
                        tracing::info!(
                            target: "sutra::metrics",
                            relay_claimed = r.claimed,
                            relay_published = r.published,
                            relay_duplicates = r.duplicates,
                            relay_retried = r.retried,
                            relay_dead_lettered = r.dead_lettered,
                            relay_released = r.released,
                            "outbox relay pass"
                        );
                    }
                    if r.broker_unavailable {
                        self.settings.backoff.delay(failures)
                    } else if r.claimed >= self.settings.batch_size as usize && r.released == 0 {
                        Duration::ZERO
                    } else {
                        self.settings.poll_interval
                    }
                }
                Err(e) => {
                    failures = failures.saturating_add(1);
                    tracing::warn!(error = %e, "outbox relay pass failed (database)");
                    self.settings.backoff.delay(failures)
                }
            };
            if !pause.is_zero() && !sleep_or_cancel(&cancel, pause).await {
                break;
            }
        }
        tracing::info!("outbox relay stopped");
    }
}

/// Sleeps for `d`; returns `false` if `cancel` fired first.
pub async fn sleep_or_cancel(cancel: &CancellationToken, d: Duration) -> bool {
    tokio::select! {
        _ = cancel.cancelled() => false,
        _ = tokio::time::sleep(d) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_are_truncated_on_char_boundaries() {
        let s = "é".repeat(MAX_ERROR_LEN);
        let t = truncate(s);
        assert!(t.len() <= MAX_ERROR_LEN);
        assert!(t.chars().all(|c| c == 'é'));
        assert_eq!(truncate("short".into()), "short");
    }

    #[test]
    fn defaults_are_sane() {
        let s = RelaySettings::default();
        assert!(s.lease > s.publish_timeout * 2);
        assert!(s.max_attempts > 1);
        assert_ne!(RELAY_CLAIM_LOCK, 0);
    }
}
