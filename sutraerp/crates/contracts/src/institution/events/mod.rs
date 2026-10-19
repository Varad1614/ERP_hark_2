//! Released event contracts published by Domain B through the outbox (CloudEvents `type` +
//! `dataschema`, camelCase `data`). Consumers (e.g. the platform inbox projector) deserialize
//! these types instead of guessing at the payload.
//!
//! Naming: `type = sutraerp.institution.<aggregate>.<verb>` and
//! `dataschema = urn:sutraerp:schema:institution.<aggregate>.<verb>:<major>` (ADR-L5-0011/0035).
//! A released V1 payload is frozen: additive optional members only; anything else is a V2 with a
//! new schema constant.
//!
//! One file per Batch 2 slice — a slice edits **only its own file** (see
//! `docs/batch2-contracts.md`):
//!
//! | File | Slice | Aggregates |
//! |------|-------|------------|
//! | this file | foundation | registered/renamed (Batch 0) + shared payload shapes |
//! | [`lifecycle`] | 2A | institution lifecycle, profile, timezone, policies |
//! | [`org`] | 2A | organisation units, type blueprints, governance, restructure, positions |
//! | [`campus`] | 2B | campuses |
//! | [`curriculum`] | 2B | academic structure, programmes, curriculum versions, school model |
//! | [`calendar`] | 2C | calendars, years, assignments, exceptions, closure |

use uuid::Uuid;

pub mod calendar;
pub mod campus;
pub mod curriculum;
pub mod lifecycle;
pub mod org;

pub const SOURCE: &str = "/sutraerp/institution";

/// Declares `NAME` (CloudEvents `type`) and `NAME_SCHEMA` (`dataschema`, major 1).
macro_rules! event_type {
    ($(#[$meta:meta])* $name:ident, $schema:ident, $path:literal) => {
        $(#[$meta])*
        pub const $name: &str = concat!("sutraerp.institution.", $path);
        #[doc = concat!("`dataschema` of `", $path, "` (V1).")]
        pub const $schema: &str = concat!("urn:sutraerp:schema:institution.", $path, ":1");
    };
}
pub(crate) use event_type;

event_type!(
    INSTITUTION_REGISTERED,
    INSTITUTION_REGISTERED_SCHEMA,
    "institution.registered"
);
event_type!(
    INSTITUTION_RENAMED,
    INSTITUTION_RENAMED_SCHEMA,
    "institution.renamed"
);

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstitutionRegisteredV1 {
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstitutionRenamedV1 {
    pub institution_id: Uuid,
    pub name: String,
    pub version: i64,
}

/// Shared V1 payload of every **lifecycle transition** event of a Domain B aggregate
/// (`…activated`, `…suspended`, `…restored`, `…decommissioned`, `…archived`, `…retired`, …).
/// The event `type` names the aggregate; `aggregateId` is that aggregate's id.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChangedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    /// `snake_case` status names from `institution::status`.
    pub from_status: String,
    pub to_status: String,
    /// Aggregate version after the transition.
    pub version: i64,
    /// Business reason / decision note, when the command carries one.
    pub reason: Option<String>,
    /// WorkflowPort process instance of the approval step, when one was involved.
    pub process_instance_id: Option<String>,
}

/// Shared V1 payload of every **governed modification** event that changes attributes but not
/// the lifecycle state (`…modified`, `…profile_revised`).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModifiedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    pub version: i64,
    /// camelCase API member names that changed.
    pub changed_fields: Vec<String>,
}

/// Every event type released by Domain B (all slices). Used by tests and the contract doc.
pub fn all_event_types() -> Vec<&'static str> {
    let mut v = vec![INSTITUTION_REGISTERED, INSTITUTION_RENAMED];
    v.extend_from_slice(lifecycle::EVENT_TYPES);
    v.extend_from_slice(org::EVENT_TYPES);
    v.extend_from_slice(campus::EVENT_TYPES);
    v.extend_from_slice(curriculum::EVENT_TYPES);
    v.extend_from_slice(calendar::EVENT_TYPES);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_are_camel_case() {
        let v = serde_json::to_value(InstitutionRenamedV1 {
            institution_id: Uuid::nil(),
            name: "N".into(),
            version: 2,
        })
        .unwrap_or_default();
        assert!(v.get("institutionId").is_some() && v.get("version").is_some());
        let s = serde_json::to_value(StatusChangedV1 {
            aggregate_id: Uuid::nil(),
            institution_id: Uuid::nil(),
            from_status: "draft".into(),
            to_status: "active".into(),
            version: 2,
            reason: None,
            process_instance_id: None,
        })
        .unwrap_or_default();
        assert!(s.get("aggregateId").is_some() && s.get("toStatus").is_some());
    }

    #[test]
    fn event_types_are_unique_and_well_formed() {
        let all = all_event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "duplicate event type");
        for t in all {
            assert!(t.starts_with("sutraerp.institution."), "{t}");
            assert_eq!(t.split('.').count(), 4, "{t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{t}"
            );
        }
        assert_eq!(
            INSTITUTION_REGISTERED_SCHEMA,
            "urn:sutraerp:schema:institution.institution.registered:1"
        );
    }
}
