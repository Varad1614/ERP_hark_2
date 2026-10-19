//! Live JetStream tests. Run against a local `nats-server -js`:
//!
//! ```sh
//! SUTRA_TEST_NATS_URL=nats://127.0.0.1:4222 cargo test -p sutra-infra --features nats-tests
//! ```
#![cfg(feature = "nats-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::time::Duration;
use sutra_infra::nats::{JetStreamPublisher, JetStreamSettings};
use sutra_kernel::messaging::{MessagePublisher, OutboundMessage};
use uuid::Uuid;

fn url() -> String {
    std::env::var("SUTRA_TEST_NATS_URL").unwrap_or_else(|_| "nats://127.0.0.1:4222".into())
}

fn settings(stream: &str, prefix: &str) -> JetStreamSettings {
    JetStreamSettings {
        url: url(),
        stream: stream.into(),
        subject_prefix: prefix.into(),
        duplicate_window: Duration::from_secs(60),
        max_age: Duration::from_secs(600),
        replicas: 1,
        connect_timeout: Duration::from_secs(5),
        client_name: "sutra-infra-tests".into(),
    }
}

#[tokio::test]
async fn ensures_stream_publishes_with_ack_and_dedups_by_msg_id() {
    let tag = Uuid::now_v7().simple().to_string();
    let stream = format!("SUTRA_TEST_{tag}");
    let prefix = format!("t{}", &tag[..12]);
    let p = JetStreamPublisher::new(settings(&stream, &prefix)).unwrap();
    p.prepare().await.expect("stream ensured");
    p.prepare().await.expect("idempotent");

    let msg = OutboundMessage {
        subject: format!(
            "{prefix}.{}.institution.institution.registered",
            Uuid::now_v7()
        ),
        msg_id: Uuid::now_v7(),
        headers: vec![(
            "ce-type".into(),
            "sutraerp.institution.institution.registered".into(),
        )],
        payload: br#"{"specversion":"1.0"}"#.to_vec(),
    };
    let first = p.publish(&msg).await.expect("ack");
    assert!(!first.duplicate);
    assert!(first.sequence >= 1);
    let again = p.publish(&msg).await.expect("ack");
    assert!(again.duplicate, "Nats-Msg-Id de-duplicates the re-publish");
    assert_eq!(again.sequence, first.sequence);

    // The message is in the stream with its headers.
    let client = async_nats::connect(url()).await.unwrap();
    let js = async_nats::jetstream::new(client);
    let s = js.get_stream(&stream).await.unwrap();
    let raw = s.get_raw_message(first.sequence).await.unwrap();
    assert_eq!(raw.subject.as_str(), msg.subject);
    let h = raw.headers;
    assert_eq!(
        h.get("Nats-Msg-Id").map(|v| v.as_str().to_owned()),
        Some(msg.msg_id.to_string())
    );
    assert_eq!(raw.payload.as_ref(), msg.payload.as_slice());
    js.delete_stream(&stream).await.unwrap();

    // Stream deleted underneath the publisher: the next publish re-ensures it.
    let late = OutboundMessage {
        msg_id: Uuid::now_v7(),
        ..msg.clone()
    };
    let r = p.publish(&late).await;
    let r = match r {
        Ok(a) => a,
        // First attempt reports the missing stream; the retry recreates it.
        Err(_) => p.publish(&late).await.expect("re-ensured"),
    };
    assert!(!r.duplicate);
    js.delete_stream(&stream).await.unwrap();
}

#[tokio::test]
async fn unreachable_server_is_a_transient_failure() {
    let mut s = settings("SUTRA_TEST_DOWN", "down");
    s.url = "nats://127.0.0.1:1".into();
    s.connect_timeout = Duration::from_millis(300);
    let p = JetStreamPublisher::new(s).unwrap();
    let r = tokio::time::timeout(Duration::from_secs(5), p.prepare()).await;
    // A timeout means it is still retrying the initial connect: also "not ready".
    if let Ok(res) = r {
        assert!(res.is_err_and(|e| !e.is_permanent()));
    }
    assert!(!p.is_connected());
}
