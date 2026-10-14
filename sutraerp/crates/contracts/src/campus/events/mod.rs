//! Released event contracts published by Domain G (Campus operations) through the outbox (CloudEvents `type` +
//! `dataschema`, camelCase `data`).
//!
//! Naming: `type = sutraerp.campus.<aggregate>.<verb>` and
//! `dataschema = urn:sutraerp:schema:campus.<aggregate>.<verb>:<major>` (ADR-L5-0011/0035),
//! `source = /sutraerp/campus`, envelope `subject` = aggregate id, envelope scope =
//! `ExecutionScope::institution(tenant, institution_id)`. A released V1 payload is frozen:
//! additive optional members only. Money and quantities in payloads are decimal **strings**.
//!
//! One file per Batch 8 slice — a slice edits **only its own file** (`docs/batch8-contracts.md` §4.3):
//!
//! | File | Slice | Aggregates |
//! |------|-------|------------|
//! | this file | foundation | shared payloads, foundation events |
//! | [`facilities`] | 8A | infrastructure nodes, reservations, labs, work orders, maintenance plans, ticket lifecycle |
//! | [`residence`] | 8B | hostel applications / allocations / leave / transfers / inspections / mess, transport routes / stops / passes / trips |
//! | [`library`] | 8C | library memberships, catalogue, copies, loans, reservations, fines, clearance |

use uuid::Uuid;

pub mod facilities;
pub mod library;
pub mod residence;

pub const SOURCE: &str = "/sutraerp/campus";

/// Declares `NAME` (CloudEvents `type`) and `NAME_SCHEMA` (`dataschema`, major 1).
macro_rules! event_type {
    ($(#[$meta:meta])* $name:ident, $schema:ident, $path:literal) => {
        $(#[$meta])*
        pub const $name: &str = concat!("sutraerp.campus.", $path);
        #[doc = concat!("`dataschema` of `", $path, "` (V1).")]
        pub const $schema: &str = concat!("urn:sutraerp:schema:campus.", $path, ":1");
    };
}
pub(crate) use event_type;

event_type!(
    /// A service desk ticket was opened (`shared::tickets::open_ticket`, any slice; L3-G-016 / 017).
    SERVICE_TICKET_OPENED, SERVICE_TICKET_OPENED_SCHEMA, "service_ticket.opened");

/// Foundation event types (emitted by the shared ticket helper).
pub const EVENT_TYPES: &[&str] = &[SERVICE_TICKET_OPENED];

/// Shared V1 payload of every **lifecycle transition** event of a Domain G aggregate.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChangedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    /// `snake_case` status names.
    pub from_status: Option<String>,
    pub to_status: String,
    /// Aggregate version after the transition.
    pub version: i64,
    pub reason: Option<String>,
    /// WorkflowPort process instance of the approval step, when one was involved.
    pub process_instance_id: Option<String>,
    /// Human-readable document number (`SR-000001`, `PO-000001` …), when the aggregate has one.
    pub number: Option<String>,
    /// Login subject of the person concerned (requester, resident, borrower …) so pure inbox
    /// rules can notify without a lookup.
    pub subject_ref: Option<String>,
}

/// Shared V1 payload of every **governed modification** event (attributes, not state).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModifiedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    pub version: i64,
    /// camelCase API member names that changed.
    pub changed_fields: Vec<String>,
}

/// Every event type released by this domain (foundation + all slices).
pub fn all_event_types() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend_from_slice(EVENT_TYPES);
    v.extend_from_slice(facilities::EVENT_TYPES);
    v.extend_from_slice(residence::EVENT_TYPES);
    v.extend_from_slice(library::EVENT_TYPES);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_are_camel_case() {
        let s = serde_json::to_value(StatusChangedV1 {
            aggregate_id: Uuid::nil(),
            institution_id: Uuid::nil(),
            from_status: None,
            to_status: "active".into(),
            version: 1,
            reason: None,
            process_instance_id: None,
            number: Some("X-000001".into()),
            subject_ref: Some("dev|aarav.desai@sutra.dev".into()),
        })
        .unwrap_or_default();
        assert_eq!(s["toStatus"], "active");
        assert!(s.get("subjectRef").is_some() && s.get("processInstanceId").is_some());
    }

    #[test]
    fn event_types_are_unique_and_well_formed() {
        let all = all_event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "duplicate event type");
        for t in all {
            assert!(t.starts_with("sutraerp.campus."), "{t}");
            assert_eq!(t.split('.').count(), 4, "{t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{t}"
            );
        }
    }
}
