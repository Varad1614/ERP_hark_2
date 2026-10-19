//! Released event contracts published by Domain C (Student Lifecycle) through the outbox
//! (CloudEvents `type` + `dataschema`, camelCase `data`).
//!
//! Naming: `type = sutraerp.student.<aggregate>.<verb>` and
//! `dataschema = urn:sutraerp:schema:student.<aggregate>.<verb>:<major>` (ADR-L5-0011/0035),
//! `source = /sutraerp/student`, envelope `subject` = aggregate id, envelope scope =
//! `ExecutionScope::institution(tenant, institution_id)` of the StudentContext's institution.
//! A released V1 payload is frozen: additive optional members only.
//!
//! One file per Batch 3 slice — a slice edits **only its own file** (`docs/batch3-contracts.md`):
//!
//! | File | Slice | Aggregates |
//! |------|-------|------------|
//! | this file | foundation | shared payload shapes + `student.status_changed` |
//! | [`admission`] | 3A | applications, Admission Record |
//! | [`record`] | 3A | student persona/context, enrolment, transfer, profile, identity, documents |
//! | [`registration`] | 3B | registration windows, registrations, holds |
//! | [`progression`] | 3B | progression decisions, ledger, probation, break, eligibility, ABC, policy |
//! | [`completion`] | 3C | qualification completion, conferment, exits, clearance |
//! | [`guardian`] | 3C | guardian relationships and student contacts |
//! | [`scholarship`] | 3C | scholarships and eligibility |
//! | [`alumni`] | 3C | alumni transition |

use uuid::Uuid;

pub mod admission;
pub mod alumni;
pub mod completion;
pub mod guardian;
pub mod progression;
pub mod record;
pub mod registration;
pub mod scholarship;

pub const SOURCE: &str = "/sutraerp/student";

/// Declares `NAME` (CloudEvents `type`) and `NAME_SCHEMA` (`dataschema`, major 1).
macro_rules! event_type {
    ($(#[$meta:meta])* $name:ident, $schema:ident, $path:literal) => {
        $(#[$meta])*
        pub const $name: &str = concat!("sutraerp.student.", $path);
        #[doc = concat!("`dataschema` of `", $path, "` (V1).")]
        pub const $schema: &str = concat!("urn:sutraerp:schema:student.", $path, ":1");
    };
}
pub(crate) use event_type;

event_type!(
    /// StudentStatusChanged (L2-C02 §10.8): emitted by the **foundation** helper
    /// `sutra_student::shared::lifecycle` on every persona or StudentContext status change, in the
    /// same transaction as the slice's specific business event. Downstream domains (D/F/I, …)
    /// subscribe to this one type instead of every slice verb. Payload [`StudentStatusChangedV1`].
    STUDENT_STATUS_CHANGED,
    STUDENT_STATUS_CHANGED_SCHEMA,
    "student.status_changed"
);

/// Payload of [`STUDENT_STATUS_CHANGED`].
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentStatusChangedV1 {
    pub student_id: Uuid,
    /// Set when a StudentContext changed (`scope = "context"`).
    pub student_context_id: Option<Uuid>,
    pub institution_id: Uuid,
    /// `student` (persona, `StudentStatus`) or `context` (`StudentContextStatus`).
    pub scope: String,
    pub from_status: Option<String>,
    pub to_status: String,
    /// The slice business event that caused it, e.g. `sutraerp.student.student.suspended`.
    pub cause_event_type: String,
    pub reason: Option<String>,
    pub version: i64,
}

/// Shared V1 payload of every **lifecycle transition** event of a Domain C aggregate.
/// The event `type` names the aggregate; `aggregateId` is that aggregate's id.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChangedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    /// The student the aggregate belongs to, when it belongs to one.
    pub student_id: Option<Uuid>,
    /// `snake_case` status names from `student::status`.
    pub from_status: Option<String>,
    pub to_status: String,
    /// Aggregate version after the transition.
    pub version: i64,
    pub reason: Option<String>,
    /// WorkflowPort process instance of the approval step, when one was involved.
    pub process_instance_id: Option<String>,
    /// Login subject of the student (`student.subject_ref`) so inbox rules can notify the
    /// student without a lookup (rules are pure).
    pub student_subject_ref: Option<String>,
}

/// Shared V1 payload of every **governed modification** event (attributes, not state).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModifiedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Option<Uuid>,
    pub version: i64,
    /// camelCase API member names that changed.
    pub changed_fields: Vec<String>,
    pub student_subject_ref: Option<String>,
}

/// Every event type released by Domain C (all slices).
pub fn all_event_types() -> Vec<&'static str> {
    let mut v = vec![STUDENT_STATUS_CHANGED];
    v.extend_from_slice(admission::EVENT_TYPES);
    v.extend_from_slice(record::EVENT_TYPES);
    v.extend_from_slice(registration::EVENT_TYPES);
    v.extend_from_slice(progression::EVENT_TYPES);
    v.extend_from_slice(completion::EVENT_TYPES);
    v.extend_from_slice(guardian::EVENT_TYPES);
    v.extend_from_slice(scholarship::EVENT_TYPES);
    v.extend_from_slice(alumni::EVENT_TYPES);
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
            student_id: None,
            from_status: Some("active".into()),
            to_status: "suspended".into(),
            version: 2,
            reason: None,
            process_instance_id: None,
            student_subject_ref: None,
        })
        .unwrap_or_default();
        assert!(s.get("aggregateId").is_some() && s.get("studentSubjectRef").is_some());
    }

    #[test]
    fn event_types_are_unique_and_well_formed() {
        let all = all_event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "duplicate event type");
        for t in all {
            assert!(t.starts_with("sutraerp.student."), "{t}");
            assert_eq!(t.split('.').count(), 4, "{t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{t}"
            );
        }
        assert_eq!(
            STUDENT_STATUS_CHANGED_SCHEMA,
            "urn:sutraerp:schema:student.student.status_changed:1"
        );
    }
}
