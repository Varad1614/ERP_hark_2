//! Released event contracts published by Domain F (Finance) through the outbox (CloudEvents
//! `type` + `dataschema`, camelCase `data`).
//!
//! Naming: `type = sutraerp.finance.<aggregate>.<verb>` and
//! `dataschema = urn:sutraerp:schema:finance.<aggregate>.<verb>:<major>` (ADR-L5-0011/0035),
//! `source = /sutraerp/finance`, envelope `subject` = aggregate id, envelope scope =
//! `ExecutionScope::institution(tenant, institution_id)`. A released V1 payload is frozen:
//! additive optional members only.
//!
//! One file per Batch 5 slice — a slice edits **only its own file** (`docs/batch5-contracts.md`):
//!
//! | File | Slice | Aggregates |
//! |------|-------|------------|
//! | this file | foundation | shared payloads; `student_account.opened`, `journal_entry.posted` / `reversed` (emitted by `shared::account` / `shared::posting`) |
//! | [`charges`] | 5A | fee structures, charges, instalments, disputes, adjustments, aid, configuration |
//! | [`payments`] | 5B | payments, receipts, unidentified receipts, bulk collection, refunds, receivables, dunning |
//! | [`ledger`] | 5C | manual journals, accounting batches, reconciliation, periods, treasury |
//!
//! Money in payloads is a decimal **string** (`"1250.00"`) plus `currency`.

use uuid::Uuid;

pub mod charges;
pub mod ledger;
pub mod payments;

pub const SOURCE: &str = "/sutraerp/finance";

/// Declares `NAME` (CloudEvents `type`) and `NAME_SCHEMA` (`dataschema`, major 1).
macro_rules! event_type {
    ($(#[$meta:meta])* $name:ident, $schema:ident, $path:literal) => {
        $(#[$meta])*
        pub const $name: &str = concat!("sutraerp.finance.", $path);
        #[doc = concat!("`dataschema` of `", $path, "` (V1).")]
        pub const $schema: &str = concat!("urn:sutraerp:schema:finance.", $path, ":1");
    };
}
pub(crate) use event_type;

// ---- foundation events (emitted only by the foundation helpers)
event_type!(
    /// A finance party account was opened for a student / applicant ([`StudentAccountOpenedV1`]).
    STUDENT_ACCOUNT_OPENED, STUDENT_ACCOUNT_OPENED_SCHEMA, "student_account.opened");
event_type!(
    /// A balanced journal entry was posted by `shared::posting::post` ([`JournalEntryPostedV1`]).
    JOURNAL_ENTRY_POSTED, JOURNAL_ENTRY_POSTED_SCHEMA, "journal_entry.posted");
event_type!(
    /// A journal entry was reversed by a mirror entry (`shared::posting::reverse`, [`JournalEntryPostedV1`] of the reversal).
    JOURNAL_ENTRY_REVERSED, JOURNAL_ENTRY_REVERSED_SCHEMA, "journal_entry.reversed");

/// Foundation event types.
pub const EVENT_TYPES: &[&str] = &[
    STUDENT_ACCOUNT_OPENED,
    JOURNAL_ENTRY_POSTED,
    JOURNAL_ENTRY_REVERSED,
];

/// Shared V1 payload of every **lifecycle transition** event of a Domain F aggregate.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusChangedV1 {
    pub aggregate_id: Uuid,
    pub institution_id: Uuid,
    /// Finance party account, when the aggregate belongs to one.
    pub student_account_id: Option<Uuid>,
    /// Domain C student, when the aggregate belongs to one.
    pub student_id: Option<Uuid>,
    /// Charge the aggregate belongs to (instalment, dispute, adjustment, allocation, …).
    pub charge_id: Option<Uuid>,
    /// `snake_case` status names from `finance::status`.
    pub from_status: Option<String>,
    pub to_status: String,
    /// Aggregate version after the transition.
    pub version: i64,
    /// Amount concerned (decimal string), when the transition moves money.
    pub amount: Option<String>,
    pub currency: Option<String>,
    pub reason: Option<String>,
    /// WorkflowPort process instance of the approval step, when one was involved.
    pub process_instance_id: Option<String>,
    /// Journal entry posted with the transition, when there was one.
    pub journal_entry_id: Option<Uuid>,
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

/// `student_account.opened`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StudentAccountOpenedV1 {
    pub student_account_id: Uuid,
    pub institution_id: Uuid,
    /// `student` | `applicant`.
    pub party_kind: String,
    pub student_id: Option<Uuid>,
    pub admission_id: Option<Uuid>,
    pub student_subject_ref: Option<String>,
}

/// `journal_entry.posted` / `journal_entry.reversed`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JournalEntryPostedV1 {
    pub journal_entry_id: Uuid,
    pub institution_id: Uuid,
    pub entry_number: String,
    pub posting_date: chrono::NaiveDate,
    /// `standard` | `reversal` | `manual` | …
    pub kind: String,
    pub source_type: String,
    pub source_id: Uuid,
    pub posting_key: String,
    /// Total debit = total credit (decimal string).
    pub amount: String,
    pub currency: String,
    pub financial_period_id: Option<Uuid>,
    pub reverses_entry_id: Option<Uuid>,
}

/// Every event type released by Domain F (foundation + all slices).
pub fn all_event_types() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend_from_slice(EVENT_TYPES);
    v.extend_from_slice(charges::EVENT_TYPES);
    v.extend_from_slice(payments::EVENT_TYPES);
    v.extend_from_slice(ledger::EVENT_TYPES);
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
            student_account_id: None,
            student_id: None,
            charge_id: None,
            from_status: None,
            to_status: "published".into(),
            version: 1,
            amount: Some("10.00".into()),
            currency: Some("INR".into()),
            reason: None,
            process_instance_id: None,
            journal_entry_id: None,
            student_subject_ref: None,
        })
        .unwrap_or_default();
        assert!(s.get("aggregateId").is_some() && s.get("studentSubjectRef").is_some());
        assert_eq!(s["amount"], "10.00");
    }

    #[test]
    fn event_types_are_unique_and_well_formed() {
        let all = all_event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len(), "duplicate event type");
        for t in all {
            assert!(t.starts_with("sutraerp.finance."), "{t}");
            assert_eq!(t.split('.').count(), 4, "{t}");
            assert!(
                t.bytes()
                    .all(|b| b.is_ascii_lowercase() || b == b'.' || b == b'_'),
                "{t}"
            );
        }
    }
}
