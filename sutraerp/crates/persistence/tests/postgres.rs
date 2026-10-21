//! Live-database integration tests. Run with:
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra cargo test -p sutra-persistence --features db-tests
//! ```
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use sutra_kernel::audit::{AuditEntry, AuditOutcome};
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::idempotency::{BeginOutcome, IdempotencyStore, StoredResponse};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::{audit, idempotency::PgIdempotencyStore, migrate, outbox, pool};

async fn db() -> sqlx::PgPool {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL must be set for db-tests");
    let p = pool::connect_lazy(&pool::PoolSettings {
        url,
        max_connections: 4,
        min_connections: 0,
        acquire_timeout: std::time::Duration::from_secs(5),
        application_name: "sutra-db-tests".into(),
    })
    .expect("pool");
    migrate::run(&p).await.expect("migrations");
    p
}

#[tokio::test]
async fn audit_log_is_append_only() {
    let p = db().await;
    let e = AuditEntry::new(
        ExecutionScope::tenant(TenantId::new()),
        "test.append",
        AuditOutcome::Succeeded,
    );
    audit::insert(&p, &e).await.expect("insert");
    let upd = sqlx::query("UPDATE platform.audit_log SET action = 'x' WHERE id = $1")
        .bind(e.id)
        .execute(&p)
        .await;
    assert!(upd.is_err(), "updates must be rejected");
}

#[tokio::test]
async fn outbox_commits_with_transaction() {
    let p = db().await;
    let scope = ExecutionScope::tenant(TenantId::new());
    let env = EventEnvelope::new(
        &scope,
        NewEvent {
            source: "/t",
            event_type: "t.happened",
            dataschema: "urn:t:1",
            subject: None,
            data: serde_json::json!({}),
        },
    );
    let mut tx = p.begin().await.expect("tx");
    outbox::enqueue(&mut *tx, &env).await.expect("enqueue");
    tx.rollback().await.expect("rollback");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM platform.outbox WHERE id = $1")
        .bind(env.id)
        .fetch_one(&p)
        .await
        .expect("count");
    assert_eq!(n, 0, "rolled back intent must not exist");
}

#[tokio::test]
async fn idempotency_store_roundtrip() {
    let p = db().await;
    let s = PgIdempotencyStore::new(p);
    let scope = TenantId::new().to_string();
    assert_eq!(
        s.begin(&scope, "k", "f").await.expect("begin"),
        BeginOutcome::Started
    );
    assert_eq!(
        s.begin(&scope, "k", "f").await.expect("begin"),
        BeginOutcome::InFlight
    );
    s.complete(
        &scope,
        "k",
        StoredResponse {
            status: 201,
            content_type: None,
            body: b"{}".to_vec(),
        },
    )
    .await
    .expect("complete");
    assert!(matches!(
        s.begin(&scope, "k", "f").await.expect("begin"),
        BeginOutcome::Replay(_)
    ));
    assert_eq!(
        s.begin(&scope, "k", "other").await.expect("begin"),
        BeginOutcome::Mismatch
    );
}

// ------------------------------------------------- Audit C2 (M1): ledger in the owning tx

mod approval_ledger {
    use super::db;
    use sutra_kernel::events::{EventEnvelope, NewEvent};
    use sutra_kernel::scope::{ExecutionScope, TenantId};
    use sutra_persistence::{approval, outbox};

    fn event(scope: &ExecutionScope, kind: &'static str) -> EventEnvelope {
        EventEnvelope::new(
            scope,
            NewEvent {
                source: "/c2",
                event_type: kind,
                dataschema: "urn:c2:1",
                subject: None,
                data: serde_json::json!({}),
            },
        )
    }

    async fn ledger_rows(
        p: &sqlx::PgPool,
        tenant: uuid::Uuid,
    ) -> Vec<(String, Option<i64>, Option<String>)> {
        sqlx::query_as(
            "SELECT subject_id, subject_version, requested_by FROM platform.approval_request \
             WHERE tenant_id = $1",
        )
        .bind(tenant)
        .fetch_all(p)
        .await
        .expect("ledger")
    }

    async fn outbox_rows(p: &sqlx::PgPool, ids: &[uuid::Uuid]) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM platform.outbox WHERE id = ANY($1)")
            .bind(ids)
            .fetch_one(p)
            .await
            .expect("outbox")
    }

    /// The governed state change (here: its "submitted" event) and the ledger row commit together.
    #[tokio::test]
    async fn ledger_row_commits_with_the_owning_transaction() {
        let p = db().await;
        let tenant = TenantId::new();
        let scope = ExecutionScope::tenant(tenant);
        let env = event(&scope, "c2.submitted");
        let ((), left) = approval::with_request_ledger(async {
            approval::record_request(
                &p,
                tenant.as_uuid(),
                "c2.approval",
                "thing:t1:v3",
                "pid-1",
                Some("dev|maker"),
            )
            .await
            .expect("queued");
            assert!(
                ledger_rows(&p, tenant.as_uuid()).await.is_empty(),
                "queued, not written yet"
            );
            let mut tx = p.begin().await.expect("tx");
            outbox::enqueue(&mut *tx, &env).await.expect("enqueue");
            tx.commit().await.expect("commit");
        })
        .await;
        assert!(left.is_empty());
        assert_eq!(
            ledger_rows(&p, tenant.as_uuid()).await,
            vec![("t1".to_owned(), Some(3), Some("dev|maker".to_owned()))]
        );
        assert_eq!(outbox_rows(&p, &[env.id]).await, 1);
    }

    /// A failed / rolled-back decision leaves no ledger row.
    #[tokio::test]
    async fn rolled_back_decision_leaves_no_ledger_row() {
        let p = db().await;
        let tenant = TenantId::new();
        let scope = ExecutionScope::tenant(tenant);
        let env = event(&scope, "c2.submitted");
        let ((), left) = approval::with_request_ledger(async {
            approval::record_request(
                &p,
                tenant.as_uuid(),
                "c2.approval",
                "thing:t2:v1",
                "pid-2",
                Some("dev|maker"),
            )
            .await
            .expect("queued");
            let mut tx = p.begin().await.expect("tx");
            outbox::enqueue(&mut *tx, &env).await.expect("enqueue");
            tx.rollback().await.expect("rollback");
        })
        .await;
        assert!(left.is_empty(), "the owning transaction took the row");
        assert!(ledger_rows(&p, tenant.as_uuid()).await.is_empty());
        assert_eq!(outbox_rows(&p, &[env.id]).await, 0);
    }

    /// A ledger failure fails the owning transaction: the state change made before it in the same
    /// transaction is rolled back too.
    #[tokio::test]
    async fn ledger_failure_rolls_the_decision_back() {
        let p = db().await;
        let tenant = TenantId::new();
        let scope = ExecutionScope::tenant(tenant);
        let state = event(&scope, "c2.state_changed");
        let decided = event(&scope, "c2.decided");
        let ((), _left) = approval::with_request_ledger(async {
            let mut tx = p.begin().await.expect("tx");
            // A transaction-local trigger (DDL is transactional) that makes this ledger insert fail.
            sqlx::query(
                "CREATE FUNCTION platform.c2_test_fail_ledger() RETURNS trigger LANGUAGE plpgsql AS \
                 $$ BEGIN IF NEW.process_definition_key = 'c2.fail' THEN \
                 RAISE EXCEPTION 'ledger unavailable'; END IF; RETURN NEW; END $$",
            )
            .execute(&mut *tx)
            .await
            .expect("fn");
            sqlx::query(
                "CREATE TRIGGER c2_fail_ledger BEFORE INSERT ON platform.approval_request \
                 FOR EACH ROW EXECUTE FUNCTION platform.c2_test_fail_ledger()",
            )
            .execute(&mut *tx)
            .await
            .expect("trigger");
            // the owning domain write of the decision
            outbox::enqueue(&mut *tx, &state).await.expect("state change");
            approval::record_request(&p, tenant.as_uuid(), "c2.fail", "thing:t3:v1", "pid-3", Some("dev|maker"))
                .await
                .expect("queued");
            let r = outbox::enqueue(&mut *tx, &decided).await;
            assert!(r.is_err(), "the ledger failure fails the owning statement");
            tx.rollback().await.expect("rollback");
        })
        .await;
        assert!(ledger_rows(&p, tenant.as_uuid()).await.is_empty());
        assert_eq!(
            outbox_rows(&p, &[state.id, decided.id]).await,
            0,
            "the decision rolled back"
        );
        let triggers: i64 =
            sqlx::query_scalar("SELECT count(*) FROM pg_trigger WHERE tgname = 'c2_fail_ledger'")
                .fetch_one(&p)
                .await
                .expect("triggers");
        assert_eq!(
            triggers, 0,
            "the test trigger rolled back with its transaction"
        );
    }

    /// Outside a request (background work) the row is written at once; inside one, rows no
    /// transaction took are handed back to the caller (the middleware writes them on success).
    #[tokio::test]
    async fn background_writes_immediately_and_leftovers_are_returned() {
        let p = db().await;
        let tenant = TenantId::new();
        approval::record_request(
            &p,
            tenant.as_uuid(),
            "c2.approval",
            "thing:t4:v1",
            "pid-4",
            None,
        )
        .await
        .expect("written");
        assert_eq!(ledger_rows(&p, tenant.as_uuid()).await.len(), 1);
        let ((), left) = approval::with_request_ledger(async {
            approval::record_request(
                &p,
                tenant.as_uuid(),
                "c2.approval",
                "thing:t4:v2",
                "pid-5",
                Some("dev|m"),
            )
            .await
            .expect("queued");
        })
        .await;
        assert_eq!(left.len(), 1);
        assert_eq!(ledger_rows(&p, tenant.as_uuid()).await.len(), 1);
        approval::write_leftover(left).await;
        assert_eq!(ledger_rows(&p, tenant.as_uuid()).await.len(), 2);
    }
}
