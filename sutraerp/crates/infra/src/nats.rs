//! NATS JetStream adapter of the [`MessagePublisher`] port (ADR-L5-0022).

use async_nats::jetstream::{self, context::PublishErrorKind, stream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use sutra_kernel::health::ReadinessCheck;
use sutra_kernel::messaging::{
    stream_subjects, MessagePublisher, OutboundMessage, PublishAck, PublishError,
};
use tokio::sync::OnceCell;

/// Connection + stream settings (from `SUTRA_NATS_*`).
#[derive(Clone, Debug)]
pub struct JetStreamSettings {
    pub url: String,
    /// Stream that captures every subject under `subject_prefix`.
    pub stream: String,
    pub subject_prefix: String,
    /// Broker-side de-duplication window for `Nats-Msg-Id`.
    pub duplicate_window: Duration,
    /// Stream retention (`max_age`); zero = unlimited.
    pub max_age: Duration,
    pub replicas: usize,
    pub connect_timeout: Duration,
    pub client_name: String,
}

impl Default for JetStreamSettings {
    fn default() -> Self {
        Self {
            url: "nats://localhost:4222".into(),
            stream: "SUTRA_EVENTS".into(),
            subject_prefix: "sutra".into(),
            duplicate_window: Duration::from_secs(120),
            max_age: Duration::from_secs(7 * 24 * 3600),
            replicas: 1,
            connect_timeout: Duration::from_secs(5),
            client_name: "sutra-api".into(),
        }
    }
}

/// Validates a stream name (JetStream forbids whitespace, `.`, `*`, `>`, path separators).
pub fn valid_stream_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 255
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

struct Connected {
    client: async_nats::Client,
    js: jetstream::Context,
}

/// JetStream publisher. Connects lazily in [`MessagePublisher::prepare`] (with
/// `retry_on_initial_connect`, so the API starts while NATS is still coming up) and ensures the
/// configured stream exists. If a publish reports that no stream captures the subject (stream
/// deleted), the stream is re-ensured before the next publish.
pub struct JetStreamPublisher {
    settings: JetStreamSettings,
    conn: OnceCell<Connected>,
    stream_ready: AtomicBool,
}

impl JetStreamPublisher {
    pub fn new(settings: JetStreamSettings) -> Result<Self, String> {
        if !valid_stream_name(&settings.stream) {
            return Err(format!(
                "invalid JetStream stream name `{}`",
                settings.stream
            ));
        }
        if !(settings.url.starts_with("nats://")
            || settings.url.starts_with("tls://")
            || settings.url.starts_with("ws://")
            || settings.url.starts_with("wss://"))
        {
            return Err("SUTRA_NATS_URL must be a nats:// or tls:// URL".into());
        }
        Ok(Self {
            settings,
            conn: OnceCell::new(),
            stream_ready: AtomicBool::new(false),
        })
    }

    pub fn settings(&self) -> &JetStreamSettings {
        &self.settings
    }

    async fn connection(&self) -> Result<&Connected, PublishError> {
        self.conn
            .get_or_try_init(|| async {
                let client = async_nats::ConnectOptions::new()
                    .name(&self.settings.client_name)
                    .connection_timeout(self.settings.connect_timeout)
                    .retry_on_initial_connect()
                    .connect(self.settings.url.as_str())
                    .await
                    .map_err(|e| PublishError::Transient(format!("nats connect: {e}")))?;
                let js = jetstream::new(client.clone());
                Ok::<_, PublishError>(Connected { client, js })
            })
            .await
    }

    /// Creates the stream if missing (idempotent). An existing stream is used as is; a subject
    /// filter that does not cover the prefix is reported.
    pub async fn ensure_stream(&self) -> Result<(), PublishError> {
        let c = self.connection().await?;
        let wanted = stream_subjects(&self.settings.subject_prefix);
        let cfg = stream::Config {
            name: self.settings.stream.clone(),
            subjects: vec![wanted.clone()],
            storage: stream::StorageType::File,
            retention: stream::RetentionPolicy::Limits,
            duplicate_window: self.settings.duplicate_window,
            max_age: self.settings.max_age,
            num_replicas: self.settings.replicas.max(1),
            ..Default::default()
        };
        let s = tokio::time::timeout(
            self.settings.connect_timeout,
            c.js.get_or_create_stream(cfg),
        )
        .await
        .map_err(|_| PublishError::Transient("timed out ensuring the JetStream stream".into()))?
        .map_err(|e| PublishError::Transient(format!("ensure stream: {e}")))?;
        let subjects = &s.cached_info().config.subjects;
        if !subjects.iter().any(|x| x == &wanted || x == ">") {
            tracing::warn!(stream = %self.settings.stream, ?subjects, wanted = %wanted, "existing JetStream stream does not capture the outbox subjects");
        }
        self.stream_ready.store(true, Ordering::SeqCst);
        tracing::info!(stream = %self.settings.stream, subjects = %wanted, "JetStream stream ready");
        Ok(())
    }

    /// `true` once connected to the server.
    pub fn is_connected(&self) -> bool {
        self.conn.get().is_some_and(|c| {
            c.client.connection_state() == async_nats::connection::State::Connected
        })
    }
}

fn map_publish_error(e: &jetstream::context::PublishError) -> PublishError {
    match e.kind() {
        PublishErrorKind::MaxPayloadExceeded => PublishError::Permanent(e.to_string()),
        _ => PublishError::Transient(e.to_string()),
    }
}

#[async_trait::async_trait]
impl MessagePublisher for JetStreamPublisher {
    async fn prepare(&self) -> Result<(), PublishError> {
        self.ensure_stream().await
    }

    async fn publish(&self, m: &OutboundMessage) -> Result<PublishAck, PublishError> {
        if !self.stream_ready.load(Ordering::SeqCst) {
            self.ensure_stream().await?;
        }
        let c = self.connection().await?;
        let mut msg = jetstream::message::PublishMessage::build()
            .payload(m.payload.clone().into())
            .message_id(m.msg_id.to_string())
            .expected_stream(&self.settings.stream);
        for (k, v) in &m.headers {
            msg = msg.header(k.as_str(), v.as_str());
        }
        let ack =
            c.js.send_publish(m.subject.clone(), msg)
                .await
                .map_err(|e| map_publish_error(&e))?
                .await
                .map_err(|e| {
                    if e.kind() == PublishErrorKind::StreamNotFound {
                        self.stream_ready.store(false, Ordering::SeqCst);
                    }
                    map_publish_error(&e)
                })?;
        Ok(PublishAck {
            sequence: ack.sequence,
            duplicate: ack.duplicate,
        })
    }

    fn name(&self) -> &'static str {
        "nats-jetstream"
    }
}

/// Readiness of the relay's broker connection. Non-critical: the outbox buffers events while NATS
/// is down, so the API keeps serving and readiness only reports `degraded`.
pub struct NatsReadiness(pub std::sync::Arc<JetStreamPublisher>);

#[async_trait::async_trait]
impl ReadinessCheck for NatsReadiness {
    fn name(&self) -> &'static str {
        "nats"
    }

    async fn check(&self) -> Result<(), String> {
        if !self.0.is_connected() {
            return Err("not connected".into());
        }
        if !self.0.stream_ready.load(Ordering::SeqCst) {
            return Err("stream not ensured".into());
        }
        Ok(())
    }

    fn critical(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_settings() {
        assert!(JetStreamPublisher::new(JetStreamSettings::default()).is_ok());
        for bad in ["", "a.b", "a b", "a*", "a>", "a/b"] {
            assert!(!valid_stream_name(bad), "{bad}");
            assert!(JetStreamPublisher::new(JetStreamSettings {
                stream: bad.into(),
                ..JetStreamSettings::default()
            })
            .is_err());
        }
        assert!(JetStreamPublisher::new(JetStreamSettings {
            url: "http://x".into(),
            ..JetStreamSettings::default()
        })
        .is_err());
    }

    #[tokio::test]
    async fn readiness_reports_not_connected_before_prepare() {
        let p = std::sync::Arc::new(
            JetStreamPublisher::new(JetStreamSettings::default()).unwrap_or_else(|e| panic!("{e}")),
        );
        let r = NatsReadiness(p);
        assert_eq!(r.check().await, Err("not connected".into()));
        assert!(!r.critical());
    }
}
