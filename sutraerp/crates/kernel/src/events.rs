//! Business-event publication primitives.
//!
//! * ADR-L5-0007: state + Durable Publication Intent commit atomically in the owner-local
//!   transaction (the transactional outbox, see `sutra-persistence::outbox`).
//! * ADR-L5-0035: the wire envelope is CloudEvents 1.0 compatible JSON. Standard attributes keep
//!   their standard names. SutraERP context travels as CloudEvents *extension attributes*, which
//!   the CloudEvents spec restricts to lowercase alphanumerics (`tenantid`, `correlationid`, …);
//!   SutraERP-defined members inside `data` use camelCase.
//! * ADR-L5-0022: NATS JetStream is the transport. The outbox relay publishes committed envelopes
//!   through the broker-neutral [`crate::messaging::MessagePublisher`] port (Batch 1C);
//!   [`EventPublisher`] remains the in-process envelope port.

use crate::error::KernelError;
use crate::scope::ExecutionScope;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// CloudEvents 1.0 compatible envelope.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EventEnvelope {
    pub specversion: String,
    pub id: Uuid,
    /// URI-reference of the publishing owner, e.g. `/sutraerp/institution`.
    pub source: String,
    /// Canonical event contract name, e.g. `sutraerp.institution.institution.registered`.
    #[serde(rename = "type")]
    pub event_type: String,
    pub time: DateTime<Utc>,
    pub datacontenttype: String,
    /// Governed JSON Schema reference of the released event contract version.
    pub dataschema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    // ---- SutraERP extension attributes ----
    pub tenantid: Uuid,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub institutionid: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlationid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub causationid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actorref: Option<String>,
    pub data: serde_json::Value,
}

/// Builder-style constructor that enforces the mandatory parts of the envelope.
#[derive(Debug, Clone)]
pub struct NewEvent {
    pub source: &'static str,
    pub event_type: &'static str,
    pub dataschema: &'static str,
    pub subject: Option<String>,
    pub data: serde_json::Value,
}

impl EventEnvelope {
    pub fn new(scope: &ExecutionScope, event: NewEvent) -> Self {
        Self {
            specversion: "1.0".into(),
            id: crate::id::new_v7(),
            source: event.source.into(),
            event_type: event.event_type.into(),
            time: Utc::now(),
            datacontenttype: "application/json".into(),
            dataschema: event.dataschema.into(),
            subject: event.subject,
            tenantid: scope.tenant_id.as_uuid(),
            institutionid: scope.institution_id.map(|i| i.as_uuid()),
            correlationid: None,
            causationid: None,
            actorref: None,
            data: event.data,
        }
    }

    pub fn with_correlation(mut self, correlation_id: Option<String>) -> Self {
        self.correlationid = correlation_id;
        self
    }

    pub fn with_actor(mut self, actor: Option<String>) -> Self {
        self.actorref = actor;
        self
    }
}

/// Transport port. The outbox relay hands committed publication intents to an implementation of
/// this trait; delivery is at-least-once, so consumers must be idempotent (ADR-L5-0010).
#[async_trait::async_trait]
pub trait EventPublisher: Send + Sync + 'static {
    async fn publish(&self, envelope: &EventEnvelope) -> Result<(), KernelError>;
}

/// Development publisher that only logs. Durable publication goes through the outbox relay and
/// the JetStream `MessagePublisher` adapter instead.
#[derive(Debug, Default, Clone)]
pub struct LoggingEventPublisher;

#[async_trait::async_trait]
impl EventPublisher for LoggingEventPublisher {
    async fn publish(&self, envelope: &EventEnvelope) -> Result<(), KernelError> {
        tracing::info!(
            event.id = %envelope.id,
            event.type = %envelope.event_type,
            tenant = %envelope.tenantid,
            "event published (logging publisher)"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{InstitutionId, TenantId};

    #[test]
    fn envelope_uses_cloudevents_attribute_names() {
        let scope = ExecutionScope::institution(TenantId::new(), InstitutionId::new());
        let env = EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/institution",
                event_type: "sutraerp.institution.institution.registered",
                dataschema: "urn:sutraerp:schema:institution.registered:1",
                subject: Some("inst-1".into()),
                data: serde_json::json!({"displayName": "Sutra"}),
            },
        )
        .with_correlation(Some("req-1".into()));
        let v = serde_json::to_value(&env).unwrap_or_default();
        for key in [
            "specversion",
            "id",
            "source",
            "type",
            "time",
            "datacontenttype",
            "dataschema",
            "tenantid",
            "institutionid",
            "correlationid",
            "data",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert_eq!(v["specversion"], "1.0");
        assert_eq!(v["data"]["displayName"], "Sutra");
        assert!(v.get("actorref").is_none());
    }

    #[tokio::test]
    async fn logging_publisher_accepts_events() {
        let scope = ExecutionScope::tenant(TenantId::new());
        let env = EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/t",
                event_type: "t",
                dataschema: "urn:t",
                subject: None,
                data: serde_json::Value::Null,
            },
        );
        assert!(LoggingEventPublisher.publish(&env).await.is_ok());
    }
}
