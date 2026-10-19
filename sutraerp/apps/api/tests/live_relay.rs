//! End-to-end outbox → NATS JetStream relay against live PostgreSQL 18 and `nats-server -js`:
//!
//! ```sh
//! DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra SUTRA_TEST_NATS_URL=nats://127.0.0.1:4222 \
//!   cargo test -p sutra-api --features db-tests,nats-tests --test live_relay
//! ```
#![cfg(all(feature = "db-tests", feature = "nats-tests"))]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;
use std::time::Duration;
use sutra_infra::nats::{JetStreamPublisher, JetStreamSettings};
use sutra_kernel::events::{EventEnvelope, NewEvent};
use sutra_kernel::messaging::MessagePublisher;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::outbox_relay::{OutboxRelay, RelaySettings};
use sutra_persistence::{migrate, outbox, pool};
use uuid::Uuid;

#[tokio::test]
async fn committed_intents_reach_jetstream_once_with_msg_id() {
    let db = pool::connect_lazy(&pool::PoolSettings {
        url: std::env::var("DATABASE_URL").expect("DATABASE_URL"),
        max_connections: 4,
        min_connections: 0,
        acquire_timeout: Duration::from_secs(5),
        application_name: "sutra-live-relay".into(),
    })
    .unwrap();
    migrate::run(&db).await.unwrap();
    let nats_url =
        std::env::var("SUTRA_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into());
    let tag = Uuid::now_v7().simple().to_string();
    let stream = format!("SUTRA_E2E_{tag}");
    let prefix = format!("e{}", &tag[..12]);

    let tenant = TenantId::new();
    let scope = ExecutionScope::tenant(tenant);
    let mut ids = vec![];
    for event_type in [
        "sutraerp.institution.institution.registered",
        "sutraerp.institution.institution.renamed",
    ] {
        let env = EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/institution",
                event_type,
                dataschema: "urn:sutraerp:schema:test:1",
                subject: Some("inst-1".into()),
                data: serde_json::json!({}),
            },
        );
        let mut tx = db.begin().await.unwrap();
        outbox::enqueue(&mut *tx, &env).await.unwrap();
        tx.commit().await.unwrap();
        ids.push(env.id);
    }

    let publisher = Arc::new(
        JetStreamPublisher::new(JetStreamSettings {
            url: nats_url.clone(),
            stream: stream.clone(),
            subject_prefix: prefix.clone(),
            ..JetStreamSettings::default()
        })
        .unwrap(),
    );
    publisher.prepare().await.expect("stream");
    let relay = OutboxRelay::new(
        db.clone(),
        publisher,
        RelaySettings {
            subject_prefix: prefix.clone(),
            tenant: Some(tenant.as_uuid()),
            ..RelaySettings::default()
        },
    );
    let r = relay.run_once().await.unwrap();
    assert_eq!((r.claimed, r.published, r.duplicates), (2, 2, 0));

    // Simulate a lost mark (relay crashed after the ack, lease expired): the row is claimed again
    // and the re-publish is de-duplicated by the broker.
    sqlx::query(
        "UPDATE platform.outbox SET published_at = NULL, next_attempt_at = now() - interval '1 second' \
         WHERE id = $1",
    )
        .bind(ids[0])
        .execute(&db)
        .await
        .unwrap();
    let r = relay.run_once().await.unwrap();
    assert_eq!((r.published, r.duplicates), (1, 1));

    let js = async_nats::jetstream::new(async_nats::connect(nats_url).await.unwrap());
    let mut s = js.get_stream(&stream).await.unwrap();
    assert_eq!(
        s.info().await.unwrap().state.messages,
        2,
        "exactly once in the stream"
    );
    for (seq, id) in [(1u64, ids[0]), (2, ids[1])] {
        let m = s.get_raw_message(seq).await.unwrap();
        assert!(m.subject.as_str().starts_with(&format!(
            "{prefix}.{}.institution.institution.",
            tenant.as_uuid()
        )));
        assert_eq!(
            m.headers.get("Nats-Msg-Id").map(|v| v.as_str().to_owned()),
            Some(id.to_string())
        );
        let env: serde_json::Value = serde_json::from_slice(&m.payload).unwrap();
        assert_eq!(env["id"], id.to_string());
    }
    js.delete_stream(&stream).await.unwrap();
}
