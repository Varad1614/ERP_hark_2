//! Released event contracts published by Domain I (Examinations) through the outbox
//! (CloudEvents `type` + `dataschema`, camelCase `data`).
//!
//! Naming: `type = sutraerp.exams.<aggregate>.<verb>` and
//! `dataschema = urn:sutraerp:schema:exams.<aggregate>.<verb>:<major>` (ADR-L5-0011/0035),
//! `source = /sutraerp/exams`, envelope `subject` = aggregate id, envelope scope =
//! `ExecutionScope::institution(tenant, institution_id)`. A released V1 payload is frozen:
//! additive optional members only.
//!
//! One file per Batch 6 slice — a slice edits **only its own file** (`docs/batch6-contracts.md`):
//!
//! | File | Slice | Aggregates |
//! |------|-------|------------|
//! | this file | foundation | shared payloads; `grading_scheme.created` / `retired` (foundation API) |
//! | [`setup`] | 6A | policy, sessions, schedules, seating, invigilation, registrations, hall tickets, conduct |
//! | [`marks`] | 6B | mark sheets, evaluations, moderation, withheld marks, malpractice |
//! | [`results`] | 6C | result batches, term results, publication, grade cards, revaluation, supplementary, completion, closure |
//!
//! Marks and grade points in payloads are decimal **strings** (`"8.45"`), never floats.

use uuid::Uuid;

pub mod marks;
pub mod results;
pub mod setup;

pub const SOURCE: &str = "/sutraerp/exams";

/// Declares `NAME` (CloudEvents `type`) and `NAME_SCHEMA` (`dataschema`, major 1).
macro_rules! event_type {
    ($(#[$meta:meta])* $name:ident, $schema:ident, $path:literal) => {
        $(#[$meta])*
        pub const $name: &str = concat!("sutraerp.exams.", $path);
        #[doc = concat!("`dataschema` of `", $path, "` (V1).")]
        pub const $schema: &str = concat!("urn:sutraerp:schema:exams.", $path, ":1");
    };
}
pub(crate) use event_type;

// ---- foundation events (emitted only by the foundation grading-scheme API)
event_type!(
    /// A grading scheme was created ([`ModifiedV1`] with every field).
    GRADING_SCHEME_CREATED, GRADING_SCHEME_CREATED_SCHEMA, "grading_scheme.created");
event_type!(
    /// A grading scheme was retired ([`StatusChangedV1`]).
    GRADING_SCHEME_RETIRED, GRADING_SCHEME_RETIRED_SCHEMA, "grading_scheme.retired");

/// Foundation event types.
pub const EVENT_TYPES: &[&str] = &[GRADING_SCHEME_CREATED, GRADING_SCHEME_RETIRED];

/// Shared V1 payload of every **lifecycle transition** event of a Domain I aggregate.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChangedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    /// Examination session the aggregate belongs to.
    pub session_id: Option<Uuid>,
    /// Paper concerned, when the aggregate belongs to one.
    pub paper_id: Option<Uuid>,
    /// Registration concerned (hall ticket, marks, result, revaluation, …).
    pub registration_id: Option<Uuid>,
    /// Domain C student, when the aggregate belongs to one.
    pub student_id: Option<Uuid>,
    /// `snake_case` status names from `exams::status`.
    pub from_status: Option<String>,
    pub to_status: String,
    /// Aggregate version after the transition.
    pub version: i64,
    pub reason: Option<String>,
    /// WorkflowPort process instance of the approval step, when one was involved.
    pub process_instance_id: Option<String>,
    /// Login subject of the student so pure inbox rules can notify without a lookup.
    pub student_subject_ref: Option<String>,
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

/// `term_result.published` / `term_result.revised` (6C) — what Domain C progression and the
/// student notification need without reading the `exams` schema.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResultPublishedV1 {
    pub result_id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub student_id: Uuid,
    pub academic_term_id: Uuid,
    /// Decimal strings with two places (`"8.45"`); `None` when withheld / not computable.
    pub sgpa: Option<String>,
    pub cgpa: Option<String>,
    pub credits_attempted: String,
    pub credits_earned: String,
    /// `ResultOutcome` wire name.
    pub outcome: String,
    /// Increments on every revision (recompute, correction, revaluation).
    pub revision: i32,
    /// Result this revision replaces (`term_result.revised`).
    pub supersedes_result_id: Option<Uuid>,
    pub student_subject_ref: Option<String>,
}

/// Every event type released by Domain I (foundation + all slices).
pub fn all_event_types() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend_from_slice(EVENT_TYPES);
    v.extend_from_slice(setup::EVENT_TYPES);
    v.extend_from_slice(marks::EVENT_TYPES);
    v.extend_from_slice(results::EVENT_TYPES);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_are_camel_case() {
        let s = serde_json::to_value(ResultPublishedV1 {
            result_id: Uuid::nil(),
            institution_id: Uuid::nil(),
            session_id: Uuid::nil(),
            student_id: Uuid::nil(),
            academic_term_id: Uuid::nil(),
            sgpa: Some("8.45".into()),
            cgpa: None,
            credits_attempted: "20.00".into(),
            credits_earned: "20.00".into(),
            outcome: "pass".into(),
            revision: 1,
            supersedes_result_id: None,
            student_subject_ref: None,
        })
        .unwrap_or_default();
        assert_eq!(s["sgpa"], "8.45");
        assert!(s.get("studentSubjectRef").is_some() && s.get("academicTermId").is_some());
    }

    #[test]
    fn event_types_are_unique_and_well_formed() {
        let all = all_event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "duplicate event type");
        for t in all {
            assert!(t.starts_with("sutraerp.exams."), "{t}");
            assert_eq!(t.split('.').count(), 4, "{t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{t}"
            );
        }
    }
}
