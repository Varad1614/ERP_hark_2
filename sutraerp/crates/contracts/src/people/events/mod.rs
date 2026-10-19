//! Released event contracts published by Domain E (People & workforce) through the outbox
//! (CloudEvents `type` + `dataschema`, camelCase `data`).
//!
//! Naming: `type = sutraerp.people.<aggregate>.<verb>` and
//! `dataschema = urn:sutraerp:schema:people.<aggregate>.<verb>:<major>` (ADR-L5-0011/0035),
//! `source = /sutraerp/people`, envelope `subject` = aggregate id, envelope scope =
//! `ExecutionScope::institution(tenant, institution_id)`. A released V1 payload is frozen:
//! additive optional members only.
//!
//! One file per Batch 7 slice — a slice edits **only its own file** (`docs/batch7-contracts.md`):
//!
//! | File | Slice | Aggregates |
//! |------|-------|------------|
//! | this file | foundation | shared payloads (the foundation directory API is read-only) |
//! | [`workforce`] | 7A | staff records, engagements, positions, faculty, onboarding, probation, separation, certificates, profile |
//! | [`leave`] | 7B | leave policy / requests / runs / adjustments / encashment / exceptions / cases, long leave, attendance, workload |
//! | [`payroll`] | 7C | payroll configuration, runs, payslips, adjustments, final settlement |
//!
//! Money and leave days in payloads are decimal **strings** (`"45250.00"`), never floats.

use uuid::Uuid;

pub mod leave;
pub mod payroll;
pub mod workforce;

pub const SOURCE: &str = "/sutraerp/people";

/// Declares `NAME` (CloudEvents `type`) and `NAME_SCHEMA` (`dataschema`, major 1).
macro_rules! event_type {
    ($(#[$meta:meta])* $name:ident, $schema:ident, $path:literal) => {
        $(#[$meta])*
        pub const $name: &str = concat!("sutraerp.people.", $path);
        #[doc = concat!("`dataschema` of `", $path, "` (V1).")]
        pub const $schema: &str = concat!("urn:sutraerp:schema:people.", $path, ":1");
    };
}
pub(crate) use event_type;

/// Foundation event types (none: the foundation API only reads).
pub const EVENT_TYPES: &[&str] = &[];

/// Shared V1 payload of every **lifecycle transition** event of a Domain E aggregate.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChangedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    /// Staff member the aggregate belongs to.
    pub staff_id: Option<Uuid>,
    /// Engagement concerned, when the aggregate belongs to one.
    pub engagement_id: Option<Uuid>,
    /// `snake_case` status names from `people::status`.
    pub from_status: Option<String>,
    pub to_status: String,
    /// Aggregate version after the transition.
    pub version: i64,
    pub reason: Option<String>,
    /// WorkflowPort process instance of the approval step, when one was involved.
    pub process_instance_id: Option<String>,
    /// Login subject of the staff member so pure inbox rules can notify without a lookup.
    pub staff_subject_ref: Option<String>,
    /// Login subject of the reporting manager (leave approvals routed to the manager).
    pub manager_subject_ref: Option<String>,
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

/// `payroll_run.posted` (7C): the Finance journal entry of a payroll posting.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PayrollPostedV1 {
    pub payroll_run_id: Uuid,
    pub institution_id: Uuid,
    pub journal_entry_id: Uuid,
    pub entry_number: String,
    /// Decimal strings with two places (`"45250.00"`).
    pub gross_earnings: String,
    pub employee_deductions: String,
    pub employer_contributions: String,
    pub net_pay: String,
    /// `finalise`, `adjustment:<id>`, `settlement:<id>` …
    pub posting_key: String,
}

/// Every event type released by Domain E (foundation + all slices).
pub fn all_event_types() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend_from_slice(EVENT_TYPES);
    v.extend_from_slice(workforce::EVENT_TYPES);
    v.extend_from_slice(leave::EVENT_TYPES);
    v.extend_from_slice(payroll::EVENT_TYPES);
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
            staff_id: None,
            engagement_id: None,
            from_status: Some("pending_approval".into()),
            to_status: "approved".into(),
            version: 2,
            reason: None,
            process_instance_id: None,
            staff_subject_ref: Some("dev|meera.shah@sutra.dev".into()),
            manager_subject_ref: None,
        })
        .unwrap_or_default();
        assert_eq!(s["toStatus"], "approved");
        assert!(s.get("staffSubjectRef").is_some() && s.get("managerSubjectRef").is_some());
        let p = serde_json::to_value(PayrollPostedV1 {
            payroll_run_id: Uuid::nil(),
            institution_id: Uuid::nil(),
            journal_entry_id: Uuid::nil(),
            entry_number: "JE-000001".into(),
            gross_earnings: "100.00".into(),
            employee_deductions: "10.00".into(),
            employer_contributions: "5.00".into(),
            net_pay: "90.00".into(),
            posting_key: "finalise".into(),
        })
        .unwrap_or_default();
        assert_eq!(p["netPay"], "90.00");
    }

    #[test]
    fn event_types_are_unique_and_well_formed() {
        let all = all_event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "duplicate event type");
        for t in all {
            assert!(t.starts_with("sutraerp.people."), "{t}");
            assert_eq!(t.split('.').count(), 4, "{t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{t}"
            );
        }
    }
}
