//! Broker-neutral message transport port used by the outbox relay (ADR-L5-0007 / ADR-L5-0022).
//!
//! The relay turns committed publication intents into [`OutboundMessage`]s and hands them to a
//! [`MessagePublisher`]. A publish only counts once the broker acknowledged it as durably stored;
//! only then may the relay mark the intent published. Delivery is at-least-once, so every message
//! carries a stable [`OutboundMessage::msg_id`] (the outbox id) that brokers use for
//! de-duplication (`Nats-Msg-Id` on JetStream) and consumers use for idempotency (ADR-L5-0010).
//!
//! * [`subject_for`] derives the routing subject `<prefix>.<tenant>.<domain>.<…event>` from the
//!   canonical event type. Broker subjects are technical routing identities, never business
//!   ownership (ADR-L5-0022 §7).
//! * [`InMemoryPublisher`] is a test adapter that records messages and can inject failures.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

/// The canonical event-type prefix that is not repeated in broker subjects.
pub const EVENT_TYPE_PREFIX: &str = "sutraerp.";

/// One message ready for the broker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundMessage {
    /// Routing subject, e.g. `sutra.<tenant>.institution.institution.registered`.
    pub subject: String,
    /// Stable de-duplication id (= outbox id = CloudEvents `id`).
    pub msg_id: Uuid,
    /// Additional transport headers (content type, CloudEvents attributes …).
    pub headers: Vec<(String, String)>,
    /// Serialized CloudEvents envelope (structured mode JSON).
    pub payload: Vec<u8>,
}

/// Broker acknowledgement of a durably stored message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublishAck {
    /// Broker sequence (JetStream stream sequence; 0 when the adapter has none).
    pub sequence: u64,
    /// The broker recognised `msg_id` as already stored (de-duplicated redelivery).
    pub duplicate: bool,
}

/// Why a publish failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PublishError {
    /// Worth retrying later (broker unavailable, timeout, no ack).
    #[error("transient publish failure: {0}")]
    Transient(String),
    /// Retrying cannot help (invalid subject, payload too large, no stream for the subject).
    #[error("permanent publish failure: {0}")]
    Permanent(String),
}

impl PublishError {
    pub fn is_permanent(&self) -> bool {
        matches!(self, Self::Permanent(_))
    }
}

/// Transport port for the outbox relay.
#[async_trait::async_trait]
pub trait MessagePublisher: Send + Sync + 'static {
    /// Prepares the transport (connect, ensure the stream exists). Called by the relay before
    /// relaying and retried with backoff until it succeeds.
    async fn prepare(&self) -> Result<(), PublishError> {
        Ok(())
    }

    /// Publishes one message and waits for the broker acknowledgement.
    async fn publish(&self, message: &OutboundMessage) -> Result<PublishAck, PublishError>;

    /// Adapter name for logs and metrics.
    fn name(&self) -> &'static str;
}

/// Why a subject could not be derived.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("cannot derive a broker subject: {0}")]
pub struct SubjectError(pub String);

fn valid_token(t: &str) -> bool {
    !t.is_empty()
        && t.len() <= 64
        && t.bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// Derives the routing subject for an event: `<prefix>.<tenant>.<event type without the
/// "sutraerp." prefix>`, e.g. `sutra.0192….institution.institution.registered`, so the second
/// token after the tenant is the owning domain. Every token must be lowercase `[a-z0-9_-]` (no
/// NATS wildcards or whitespace); the event type needs at least a domain and an event name.
pub fn subject_for(prefix: &str, tenant: Uuid, event_type: &str) -> Result<String, SubjectError> {
    if prefix.is_empty() || !prefix.split('.').all(valid_token) {
        return Err(SubjectError(format!("invalid subject prefix `{prefix}`")));
    }
    let rest = event_type
        .strip_prefix(EVENT_TYPE_PREFIX)
        .unwrap_or(event_type);
    let tokens: Vec<&str> = rest.split('.').collect();
    if tokens.len() < 2 || !tokens.iter().all(|t| valid_token(t)) {
        return Err(SubjectError(format!("invalid event type `{event_type}`")));
    }
    Ok(format!("{prefix}.{tenant}.{rest}"))
}

/// The wildcard that covers every subject under `prefix` (the stream's subject filter).
pub fn stream_subjects(prefix: &str) -> String {
    format!("{prefix}.>")
}

/// Failure script entry for [`InMemoryPublisher`].
#[derive(Clone, Debug)]
pub enum Scripted {
    Ok,
    Fail(PublishError),
}

#[derive(Default)]
struct InMemoryState {
    published: Vec<OutboundMessage>,
    seen: std::collections::HashSet<Uuid>,
    script: VecDeque<Scripted>,
    fail_all: Option<PublishError>,
    fail_subjects: Vec<(String, PublishError)>,
}

/// Test adapter: records messages in publish order, de-duplicates by `msg_id` like JetStream, and
/// can fail on demand. Cheap to clone (shared state).
#[derive(Clone, Default)]
pub struct InMemoryPublisher {
    state: Arc<Mutex<InMemoryState>>,
}

impl InMemoryPublisher {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, InMemoryState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Messages accepted so far (duplicates excluded), in publish order.
    pub fn published(&self) -> Vec<OutboundMessage> {
        self.lock().published.clone()
    }

    /// Every following publish fails with `err` until [`Self::heal`].
    pub fn fail_all(&self, err: PublishError) {
        self.lock().fail_all = Some(err);
    }

    /// Publishes to subjects containing `needle` fail with `err`.
    pub fn fail_subject(&self, needle: &str, err: PublishError) {
        self.lock().fail_subjects.push((needle.to_owned(), err));
    }

    /// Queues outcomes for the next publishes (consumed in order before other rules apply).
    pub fn script(&self, outcomes: impl IntoIterator<Item = Scripted>) {
        self.lock().script.extend(outcomes);
    }

    /// Clears every failure rule.
    pub fn heal(&self) {
        let mut s = self.lock();
        s.fail_all = None;
        s.fail_subjects.clear();
        s.script.clear();
    }
}

#[async_trait::async_trait]
impl MessagePublisher for InMemoryPublisher {
    async fn publish(&self, message: &OutboundMessage) -> Result<PublishAck, PublishError> {
        let mut s = self.lock();
        if let Some(Scripted::Fail(e)) = s.script.pop_front() {
            return Err(e);
        }
        if let Some(e) = &s.fail_all {
            return Err(e.clone());
        }
        if let Some((_, e)) = s
            .fail_subjects
            .iter()
            .find(|(n, _)| message.subject.contains(n.as_str()))
        {
            return Err(e.clone());
        }
        if !s.seen.insert(message.msg_id) {
            return Ok(PublishAck {
                sequence: 0,
                duplicate: true,
            });
        }
        s.published.push(message.clone());
        Ok(PublishAck {
            sequence: s.published.len() as u64,
            duplicate: false,
        })
    }

    fn name(&self) -> &'static str {
        "memory"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "0192f5a0-7c3e-7000-8000-000000000001";

    fn tenant() -> Uuid {
        Uuid::parse_str(T).unwrap_or_default()
    }

    #[test]
    fn subject_strips_the_canonical_prefix_and_keeps_the_domain() {
        assert_eq!(
            subject_for(
                "sutra",
                tenant(),
                "sutraerp.institution.institution.registered"
            )
            .as_deref(),
            Ok("sutra.0192f5a0-7c3e-7000-8000-000000000001.institution.institution.registered")
        );
        assert_eq!(
            subject_for("acme.sutra", tenant(), "finance.payment_received").as_deref(),
            Ok("acme.sutra.0192f5a0-7c3e-7000-8000-000000000001.finance.payment_received")
        );
    }

    #[test]
    fn subject_rejects_wildcards_whitespace_and_bad_shapes() {
        for bad in [
            "sutraerp.institution",
            "sutraerp.institution.*",
            "sutraerp.institution.>",
            "sutraerp.Institution.renamed",
            "sutraerp.institution..renamed",
            "sutraerp.institution.re named",
            "",
        ] {
            assert!(subject_for("sutra", tenant(), bad).is_err(), "{bad}");
        }
        assert!(subject_for("", tenant(), "a.b").is_err());
        assert!(subject_for("su*tra", tenant(), "a.b").is_err());
        assert_eq!(stream_subjects("sutra"), "sutra.>");
    }

    fn msg(subject: &str) -> OutboundMessage {
        OutboundMessage {
            subject: subject.into(),
            msg_id: crate::id::new_v7(),
            headers: vec![],
            payload: b"{}".to_vec(),
        }
    }

    #[tokio::test]
    async fn in_memory_publisher_records_dedups_and_fails_on_demand() {
        let p = InMemoryPublisher::new();
        let m = msg("sutra.t.a.b");
        assert!(!p.publish(&m).await.map(|a| a.duplicate).unwrap_or(true));
        assert!(p.publish(&m).await.map(|a| a.duplicate).unwrap_or(false));
        assert_eq!(p.published().len(), 1);
        p.script([Scripted::Fail(PublishError::Transient("x".into()))]);
        assert!(p.publish(&msg("sutra.t.a.c")).await.is_err());
        assert!(p.publish(&msg("sutra.t.a.c")).await.is_ok());
        p.fail_subject("poison", PublishError::Permanent("no".into()));
        assert!(p
            .publish(&msg("sutra.t.poison.x"))
            .await
            .is_err_and(|e| e.is_permanent()));
        p.fail_all(PublishError::Transient("down".into()));
        assert!(p.publish(&msg("sutra.t.a.d")).await.is_err());
        p.heal();
        assert!(p.publish(&msg("sutra.t.a.d")).await.is_ok());
        assert_eq!(p.published().len(), 3);
    }
}
