//! Outbox relay + sweeper integration tests against a live PostgreSQL 18. Run with:
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra cargo test -p sutra-persistence --features db-tests
//! ```
//!
//! Every test works in its own random tenant (the relay is restricted to it), so tests can run
//! in parallel with each other and with other suites sharing the database.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::messaging::{InMemoryPublisher, PublishError, Scripted};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::backoff::Backoff;
use sutra_persistence::jobs::{self, IdempotencyPurge, JobOutcome, OutboxPurge, SweepJob};
use sutra_persistence::outbox_relay::{OutboxRelay, RelayReport, RelaySettings};
use sutra_persistence::{migrate, outbox, pool};
use uuid::Uuid;

async fn db() -> PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for db-tests");
    let p = pool::connect_lazy(&pool::PoolSettings {
        url,
        max_connections: 8,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(10),
        application_name: "sutra-relay-tests".into(),
    })
    .expect("pool");
    migrate::run(&p).await.expect("migrations");
    p
}

fn settings(tenant: Uuid) -> RelaySettings {
    RelaySettings {
        batch_size: 50,
        poll_interval: Duration::from_millis(20),
        lease: Duration::from_secs(30),
        max_attempts: 3,
        // Zero backoff so retries are due immediately in tests.
        backoff: Backoff::new(Duration::ZERO, Duration::ZERO),
        publish_timeout: Duration::from_secs(2),
        subject_prefix: "sutra".into(),
        tenant: Some(tenant),
    }
}

/// Enqueues `n` events for `subject` (aggregate) in `tenant`; returns their ids in order.
async fn enqueue(
    p: &PgPool,
    tenant: TenantId,
    event_type: &'static str,
    subject: Option<&str>,
    n: usize,
) -> Vec<Uuid> {
    let scope = ExecutionScope::tenant(tenant);
    let mut ids = vec![];
    for i in 0..n {
        let env = EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/test",
                event_type,
                dataschema: "urn:sutraerp:schema:test:1",
                subject: subject.map(str::to_owned),
                data: serde_json::json!({ "n": i }),
            },
        );
        outbox::enqueue(p, &env).await.expect("enqueue");
        ids.push(env.id);
    }
    ids
}

type Ts = Option<chrono::DateTime<chrono::Utc>>;
type RowState = (Ts, i32, Option<String>, Ts, Option<Uuid>);

/// (published, attempts, last_error, dead-lettered, claim_token) of an outbox row.
async fn row(p: &PgPool, id: Uuid) -> (bool, i32, Option<String>, bool, Option<Uuid>) {
    let r: RowState = sqlx::query_as(
        "SELECT published_at, attempts, last_error, dead_lettered_at, claim_token \
         FROM platform.outbox WHERE id = $1",
    )
    .bind(id)
    .fetch_one(p)
    .await
    .expect("row");
    (r.0.is_some(), r.1, r.2, r.3.is_some(), r.4)
}

#[tokio::test]
async fn publishes_in_creation_order_and_marks_after_ack() {
    let p = db().await;
    let tenant = TenantId::new();
    let a = enqueue(&p, tenant, "sutraerp.test.thing.created", Some("agg-1"), 3).await;
    let b = enqueue(&p, tenant, "sutraerp.test.thing.renamed", None, 2).await;
    let publisher = InMemoryPublisher::new();
    let relay = OutboxRelay::new(
        p.clone(),
        Arc::new(publisher.clone()),
        settings(tenant.as_uuid()),
    );
    let r = relay.run_once().await.expect("pass");
    assert_eq!(r.claimed, 5);
    assert_eq!(r.published, 5);
    let sent = publisher.published();
    let ids: Vec<Uuid> = sent.iter().map(|m| m.msg_id).collect();
    assert_eq!(ids, [a.clone(), b.clone()].concat(), "creation order");
    assert_eq!(
        sent[0].subject,
        format!("sutra.{}.test.thing.created", tenant.as_uuid())
    );
    assert!(sent[0]
        .headers
        .iter()
        .any(|(k, v)| k == "ce-id" && v == &a[0].to_string()));
    let env: serde_json::Value = serde_json::from_slice(&sent[0].payload).unwrap();
    assert_eq!(env["id"], a[0].to_string());
    for id in a.iter().chain(&b) {
        let (published, attempts, err, dead, token) = row(&p, *id).await;
        assert!(published && !dead && err.is_none() && token.is_none());
        assert_eq!(attempts, 1);
    }
    // Nothing left: a second pass publishes nothing new.
    assert_eq!(relay.run_once().await.unwrap(), RelayReport::default());
}

#[tokio::test]
async fn nothing_is_marked_without_an_ack() {
    let p = db().await;
    let tenant = TenantId::new();
    let ids = enqueue(&p, tenant, "sutraerp.test.thing.created", None, 2).await;
    let publisher = InMemoryPublisher::new();
    publisher.fail_all(PublishError::Transient("broker down".into()));
    let relay = OutboxRelay::new(
        p.clone(),
        Arc::new(publisher.clone()),
        settings(tenant.as_uuid()),
    );
    let r = relay.run_once().await.unwrap();
    assert!(r.broker_unavailable);
    assert_eq!(
        (r.claimed, r.published, r.retried, r.released),
        (2, 0, 1, 1)
    );
    let (published, attempts, err, dead, token) = row(&p, ids[0]).await;
    assert!(!published && !dead && token.is_none());
    assert_eq!(attempts, 1);
    assert!(err.unwrap().contains("broker down"));
    // The second row was released untouched (pass stopped on the transient failure).
    let (published, attempts, _, _, token) = row(&p, ids[1]).await;
    assert!(!published && token.is_none());
    assert_eq!(attempts, 0);
    assert!(publisher.published().is_empty());
}

#[tokio::test]
async fn transient_failures_retry_then_dead_letter() {
    let p = db().await;
    let tenant = TenantId::new();
    let ok = enqueue(&p, tenant, "sutraerp.test.thing.created", None, 1).await;
    let poison = enqueue(&p, tenant, "sutraerp.test.poison.created", Some("agg-p"), 1).await;
    let after = enqueue(&p, tenant, "sutraerp.test.poison.updated", Some("agg-p"), 1).await;
    let publisher = InMemoryPublisher::new();
    publisher.fail_subject("poison", PublishError::Transient("nack".into()));
    let relay = OutboxRelay::new(
        p.clone(),
        Arc::new(publisher.clone()),
        settings(tenant.as_uuid()),
    );

    // Attempts 1 and 2 retry; attempt 3 (= max_attempts) dead-letters.
    let r1 = relay.run_once().await.unwrap();
    assert_eq!((r1.published, r1.retried, r1.released), (1, 1, 1));
    assert_eq!(row(&p, poison[0]).await.1, 1);
    let r2 = relay.run_once().await.unwrap();
    assert_eq!((r2.retried, r2.dead_lettered), (1, 0));
    let r3 = relay.run_once().await.unwrap();
    assert_eq!(r3.dead_lettered, 1);
    let (published, attempts, err, dead, _) = row(&p, poison[0]).await;
    assert!(!published && dead);
    assert_eq!(attempts, 3);
    assert!(err.unwrap().contains("nack"));
    // The later event of the same aggregate was held back until the poison row left the
    // pending set; it then goes out (and fails the same way here, so heal first).
    assert_eq!(row(&p, after[0]).await.1, 0, "held, never attempted");
    publisher.heal();
    let r4 = relay.run_once().await.unwrap();
    assert_eq!(r4.published, 1);
    assert!(row(&p, after[0]).await.0);
    // Dead-lettered rows are never claimed again.
    assert_eq!(relay.run_once().await.unwrap().claimed, 0);
    let order: Vec<Uuid> = publisher.published().iter().map(|m| m.msg_id).collect();
    assert_eq!(order, vec![ok[0], after[0]]);
}

#[tokio::test]
async fn permanent_failures_dead_letter_immediately() {
    let p = db().await;
    let tenant = TenantId::new();
    // An event type that cannot form a subject is a permanent failure.
    let bad = enqueue(&p, tenant, "sutraerp.Bad Type", None, 1).await;
    let good = enqueue(&p, tenant, "sutraerp.test.thing.created", None, 1).await;
    let publisher = InMemoryPublisher::new();
    let relay = OutboxRelay::new(
        p.clone(),
        Arc::new(publisher.clone()),
        settings(tenant.as_uuid()),
    );
    let r = relay.run_once().await.unwrap();
    assert_eq!(
        (r.dead_lettered, r.published, r.broker_unavailable),
        (1, 1, false)
    );
    let (_, attempts, err, dead, _) = row(&p, bad[0]).await;
    assert!(dead);
    assert_eq!(attempts, 1);
    assert!(err.unwrap().contains("permanent"));
    assert!(row(&p, good[0]).await.0);
}

#[tokio::test]
async fn scripted_single_failure_is_retried_and_succeeds() {
    let p = db().await;
    let tenant = TenantId::new();
    let ids = enqueue(&p, tenant, "sutraerp.test.thing.created", Some("agg"), 2).await;
    let publisher = InMemoryPublisher::new();
    publisher.script([Scripted::Fail(PublishError::Transient("blip".into()))]);
    let relay = OutboxRelay::new(
        p.clone(),
        Arc::new(publisher.clone()),
        settings(tenant.as_uuid()),
    );
    let r1 = relay.run_once().await.unwrap();
    assert_eq!((r1.retried, r1.released), (1, 1));
    let r2 = relay.run_once().await.unwrap();
    assert_eq!(r2.published, 2);
    let order: Vec<Uuid> = publisher.published().iter().map(|m| m.msg_id).collect();
    assert_eq!(order, ids, "order kept across the retry");
    assert_eq!(row(&p, ids[0]).await.1, 2);
}

#[tokio::test]
async fn concurrent_relays_never_double_publish() {
    let p = db().await;
    let tenant = TenantId::new();
    let mut ids = vec![];
    for agg in 0..10 {
        let subject = format!("agg-{agg}");
        ids.extend(
            enqueue(
                &p,
                tenant,
                "sutraerp.test.thing.changed",
                Some(&subject),
                12,
            )
            .await,
        );
    }
    ids.extend(enqueue(&p, tenant, "sutraerp.test.thing.loose", None, 30).await);
    let publishers: Vec<InMemoryPublisher> = (0..4).map(|_| InMemoryPublisher::new()).collect();
    let mut tasks = vec![];
    for pubr in &publishers {
        let mut s = settings(tenant.as_uuid());
        s.batch_size = 7;
        let relay = OutboxRelay::new(p.clone(), Arc::new(pubr.clone()), s);
        tasks.push(tokio::spawn(async move {
            let mut total = 0;
            for _ in 0..200 {
                let r = relay.run_once().await.unwrap();
                total += r.published;
                if r.claimed == 0 {
                    tokio::task::yield_now().await;
                }
            }
            total
        }));
    }
    let mut total = 0;
    for t in tasks {
        total += t.await.unwrap();
    }
    let mut all: Vec<Uuid> = publishers
        .iter()
        .flat_map(|p| p.published())
        .map(|m| m.msg_id)
        .collect();
    assert_eq!(total, ids.len());
    assert_eq!(all.len(), ids.len(), "every event exactly once");
    all.sort();
    all.dedup();
    assert_eq!(all.len(), ids.len(), "no event published twice");
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE tenant_id = $1 AND published_at IS NULL",
    )
    .bind(tenant.as_uuid())
    .fetch_one(&p)
    .await
    .unwrap();
    assert_eq!(pending, 0);
}

#[tokio::test]
async fn expired_leases_are_reclaimed() {
    let p = db().await;
    let tenant = TenantId::new();
    let ids = enqueue(&p, tenant, "sutraerp.test.thing.created", None, 1).await;
    // Simulate a relay that claimed the row and crashed: lease already expired.
    sqlx::query(
        "UPDATE platform.outbox SET claim_token = $2, next_attempt_at = now() - interval '1 second' WHERE id = $1",
    )
    .bind(ids[0])
    .bind(Uuid::now_v7())
    .execute(&p)
    .await
    .unwrap();
    let publisher = InMemoryPublisher::new();
    let relay = OutboxRelay::new(
        p.clone(),
        Arc::new(publisher.clone()),
        settings(tenant.as_uuid()),
    );
    assert_eq!(relay.run_once().await.unwrap().published, 1);
    assert!(row(&p, ids[0]).await.0);
}

#[tokio::test]
async fn run_loop_drains_and_stops_on_cancel() {
    let p = db().await;
    let tenant = TenantId::new();
    let ids = enqueue(&p, tenant, "sutraerp.test.thing.created", None, 5).await;
    let publisher = InMemoryPublisher::new();
    let relay = OutboxRelay::new(
        p.clone(),
        Arc::new(publisher.clone()),
        settings(tenant.as_uuid()),
    );
    let cancel = tokio_util::sync::CancellationToken::new();
    let handle = tokio::spawn(relay.run(cancel.clone()));
    for _ in 0..100 {
        if publisher.published().len() == ids.len() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), handle)
        .await
        .expect("relay stops promptly")
        .unwrap();
    assert_eq!(publisher.published().len(), ids.len());
}

// ---------------------------------------------------------------------------------------------
// Sweepers
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn idempotency_purge_removes_only_expired_keys() {
    let p = db().await;
    let scope = format!("test:{}", Uuid::now_v7());
    sqlx::query(
        "INSERT INTO platform.idempotency_key (scope_key, idempotency_key, fingerprint, state, expires_at) VALUES \
         ($1, 'old', 'f', 'completed', now() - interval '1 minute'), \
         ($1, 'live', 'f', 'completed', now() + interval '1 hour')",
    )
    .bind(&scope)
    .execute(&p)
    .await
    .unwrap();
    let job = IdempotencyPurge {
        interval: Duration::from_secs(60),
        batch_limit: 100_000,
    };
    let mut outcome = jobs::run_once(&p, &job).await.unwrap();
    // A concurrent test may hold the lock for a moment.
    for _ in 0..50 {
        if outcome != JobOutcome::Skipped {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        outcome = jobs::run_once(&p, &job).await.unwrap();
    }
    assert!(matches!(outcome, JobOutcome::Ran { affected } if affected >= 1));
    let left: Vec<String> = sqlx::query_scalar(
        "SELECT idempotency_key FROM platform.idempotency_key WHERE scope_key = $1",
    )
    .bind(&scope)
    .fetch_all(&p)
    .await
    .unwrap();
    assert_eq!(left, vec!["live".to_owned()]);
}

#[tokio::test]
async fn outbox_purge_keeps_pending_recent_and_dead_lettered_rows() {
    let p = db().await;
    let tenant = TenantId::new();
    let ids = enqueue(&p, tenant, "sutraerp.test.thing.created", None, 4).await;
    // 0: published long ago → purged; 1: published recently → kept;
    // 2: dead-lettered long ago → kept; 3: pending → kept.
    sqlx::query(
        "UPDATE platform.outbox SET published_at = now() - interval '2 hours' WHERE id = $1",
    )
    .bind(ids[0])
    .execute(&p)
    .await
    .unwrap();
    sqlx::query("UPDATE platform.outbox SET published_at = now() WHERE id = $1")
        .bind(ids[1])
        .execute(&p)
        .await
        .unwrap();
    sqlx::query("UPDATE platform.outbox SET dead_lettered_at = now() - interval '30 days', created_at = now() - interval '30 days' WHERE id = $1")
        .bind(ids[2])
        .execute(&p)
        .await
        .unwrap();
    let job = OutboxPurge {
        interval: Duration::from_secs(60),
        retention: Duration::from_secs(3600),
        batch_limit: 100_000,
    };
    let mut outcome = jobs::run_once(&p, &job).await.unwrap();
    for _ in 0..50 {
        if outcome != JobOutcome::Skipped {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        outcome = jobs::run_once(&p, &job).await.unwrap();
    }
    assert!(matches!(outcome, JobOutcome::Ran { affected } if affected >= 1));
    let left: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM platform.outbox WHERE tenant_id = $1 ORDER BY created_at, id",
    )
    .bind(tenant.as_uuid())
    .fetch_all(&p)
    .await
    .unwrap();
    assert_eq!(
        left,
        vec![ids[2], ids[1], ids[3]],
        "only the old published row is purged"
    );
}

/// A job that records how many runs overlap.
struct Probe {
    name: &'static str,
    running: Arc<std::sync::atomic::AtomicUsize>,
    max_seen: Arc<std::sync::atomic::AtomicUsize>,
}

#[async_trait::async_trait]
impl SweepJob for Probe {
    fn name(&self) -> &'static str {
        self.name
    }
    fn interval(&self) -> Duration {
        Duration::from_secs(60)
    }
    async fn run(&self, conn: &mut sqlx::PgConnection) -> Result<u64, sqlx::Error> {
        use std::sync::atomic::Ordering;
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_seen.fetch_max(now, Ordering::SeqCst);
        sqlx::query("SELECT pg_sleep(0.2)")
            .execute(&mut *conn)
            .await?;
        self.running.fetch_sub(1, Ordering::SeqCst);
        Ok(1)
    }
}

#[tokio::test]
async fn advisory_lock_prevents_concurrent_runs() {
    let p = db().await;
    // Unique job name → its own lock, unaffected by other tests.
    let name: &'static str = Box::leak(format!("probe-{}", Uuid::now_v7()).into_boxed_str());
    // 1. Held by "another replica" → skipped.
    let mut other = p.begin().await.unwrap();
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(jobs::lock_key(name))
        .execute(&mut *other)
        .await
        .unwrap();
    let probe = Probe {
        name,
        running: Arc::default(),
        max_seen: Arc::default(),
    };
    assert_eq!(
        jobs::run_once(&p, &probe).await.unwrap(),
        JobOutcome::Skipped
    );
    other.rollback().await.unwrap();
    assert_eq!(
        jobs::run_once(&p, &probe).await.unwrap(),
        JobOutcome::Ran { affected: 1 }
    );
    // 2. Several runners racing: exactly one runs at a time, the rest skip.
    let probe = Arc::new(probe);
    let mut tasks = vec![];
    for _ in 0..4 {
        let (p, probe) = (p.clone(), probe.clone());
        tasks.push(tokio::spawn(async move {
            jobs::run_once(&p, probe.as_ref()).await.unwrap()
        }));
    }
    let mut ran = 0;
    for t in tasks {
        if matches!(t.await.unwrap(), JobOutcome::Ran { .. }) {
            ran += 1;
        }
    }
    assert!(ran >= 1);
    assert_eq!(
        probe.max_seen.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "never two runs at once"
    );
}
