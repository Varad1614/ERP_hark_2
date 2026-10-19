//! Released event contracts published by Domain D (Academic delivery) through the outbox
//! (CloudEvents `type` + `dataschema`, camelCase `data`).
//!
//! Naming: `type = sutraerp.academic.<aggregate>.<verb>` and
//! `dataschema = urn:sutraerp:schema:academic.<aggregate>.<verb>:<major>` (ADR-L5-0011/0035),
//! `source = /sutraerp/academic`, envelope `subject` = aggregate id, envelope scope =
//! `ExecutionScope::institution(tenant, institution_id)` of the offering's institution.
//! A released V1 payload is frozen: additive optional members only.
//!
//! One file per Batch 4 slice — a slice edits **only its own file** (`docs/batch4-contracts.md`):
//!
//! | File | Slice | Aggregates |
//! |------|-------|------------|
//! | this file | foundation | shared payload shapes |
//! | [`offering`] | 4A | course offerings, sections, enrolment, waitlist, faculty assignment |
//! | [`delivery`] | 4B | timetable, sessions, attendance, syllabus coverage, monitoring, internal assessment |
//! | [`flexible`] | 4C | providers, opportunities, flexible enrolment, certificates, credit recognition, resources |

use uuid::Uuid;

pub mod delivery;
pub mod flexible;
pub mod offering;

pub const SOURCE: &str = "/sutraerp/academic";

/// Declares `NAME` (CloudEvents `type`) and `NAME_SCHEMA` (`dataschema`, major 1).
macro_rules! event_type {
    ($(#[$meta:meta])* $name:ident, $schema:ident, $path:literal) => {
        $(#[$meta])*
        pub const $name: &str = concat!("sutraerp.academic.", $path);
        #[doc = concat!("`dataschema` of `", $path, "` (V1).")]
        pub const $schema: &str = concat!("urn:sutraerp:schema:academic.", $path, ":1");
    };
}
pub(crate) use event_type;

/// Shared V1 payload of every **lifecycle transition** event of a Domain D aggregate.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChangedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    /// Offering the aggregate belongs to, when it belongs to one.
    pub offering_id: Option<Uuid>,
    /// Section the aggregate belongs to, when it belongs to one.
    pub section_id: Option<Uuid>,
    /// Student the aggregate belongs to, when it belongs to one.
    pub student_id: Option<Uuid>,
    /// `snake_case` status names from `academic::status`.
    pub from_status: Option<String>,
    pub to_status: String,
    /// Aggregate version after the transition.
    pub version: i64,
    pub reason: Option<String>,
    /// WorkflowPort process instance of the approval step, when one was involved.
    pub process_instance_id: Option<String>,
    /// Login subject of the student so pure inbox rules can notify without a lookup.
    pub student_subject_ref: Option<String>,
    /// Login subject of the faculty member concerned (assignment, session, coursework).
    pub faculty_subject_ref: Option<String>,
}

/// Shared V1 payload of every **governed modification** event (attributes, not state).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModifiedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Option<Uuid>,
    pub section_id: Option<Uuid>,
    pub version: i64,
    /// camelCase API member names that changed.
    pub changed_fields: Vec<String>,
}

/// Every event type released by Domain D (all slices).
pub fn all_event_types() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend_from_slice(offering::EVENT_TYPES);
    v.extend_from_slice(delivery::EVENT_TYPES);
    v.extend_from_slice(flexible::EVENT_TYPES);
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
            offering_id: None,
            section_id: None,
            student_id: None,
            from_status: None,
            to_status: "active".into(),
            version: 1,
            reason: None,
            process_instance_id: None,
            student_subject_ref: None,
            faculty_subject_ref: None,
        })
        .unwrap_or_default();
        assert!(s.get("aggregateId").is_some() && s.get("facultySubjectRef").is_some());
    }

    #[test]
    fn event_types_are_unique_and_well_formed() {
        let all = all_event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "duplicate event type");
        for t in all {
            assert!(t.starts_with("sutraerp.academic."), "{t}");
            assert_eq!(t.split('.').count(), 4, "{t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{t}"
            );
        }
        assert_eq!(
            offering::COURSE_OFFERING_ACTIVATED_SCHEMA,
            "urn:sutraerp:schema:academic.course_offering.activated:1"
        );
    }
}
