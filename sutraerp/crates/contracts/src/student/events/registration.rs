//! **3B** — academic registration: windows, registration, add/drop, cancellation, backlog/repeat, finalisation, holds (L2-C03, L3-C-020…026).
//!
//! Owned by slice 3B: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;
use uuid::Uuid;

event_type!(
    /// (StatusChangedV1, L3-C-020).
    REGISTRATION_WINDOW_OPENED, REGISTRATION_WINDOW_OPENED_SCHEMA, "registration_window.opened");
event_type!(
    /// (StatusChangedV1).
    REGISTRATION_WINDOW_CLOSED, REGISTRATION_WINDOW_CLOSED_SCHEMA, "registration_window.closed");
event_type!(
    /// (StatusChangedV1).
    REGISTRATION_SUBMITTED, REGISTRATION_SUBMITTED_SCHEMA, "registration.submitted");
event_type!(
    /// (StatusChangedV1).
    REGISTRATION_APPROVED, REGISTRATION_APPROVED_SCHEMA, "registration.approved");
event_type!(
    /// StudentRegistered: effective registration ([`RegistrationV1`]).
    REGISTRATION_REGISTERED, REGISTRATION_REGISTERED_SCHEMA, "registration.registered");
event_type!(
    /// Add/drop applied ([`RegistrationV1`], L3-C-022).
    REGISTRATION_MODIFIED, REGISTRATION_MODIFIED_SCHEMA, "registration.modified");
event_type!(
    /// (StatusChangedV1, L3-C-023).
    REGISTRATION_CANCELLED, REGISTRATION_CANCELLED_SCHEMA, "registration.cancelled");
event_type!(
    /// Frozen + published ([`RegistrationV1`], L3-C-025).
    REGISTRATION_FINALISED, REGISTRATION_FINALISED_SCHEMA, "registration.finalised");
event_type!(
    /// ([`RegistrationHoldV1`], L3-C-026).
    REGISTRATION_HOLD_PLACED, REGISTRATION_HOLD_PLACED_SCHEMA, "registration_hold.placed");
event_type!(
    /// ([`RegistrationHoldV1`]).
    REGISTRATION_HOLD_RELEASED, REGISTRATION_HOLD_RELEASED_SCHEMA, "registration_hold.released");

pub const EVENT_TYPES: &[&str] = &[
    REGISTRATION_WINDOW_OPENED,
    REGISTRATION_WINDOW_CLOSED,
    REGISTRATION_SUBMITTED,
    REGISTRATION_APPROVED,
    REGISTRATION_REGISTERED,
    REGISTRATION_MODIFIED,
    REGISTRATION_CANCELLED,
    REGISTRATION_FINALISED,
    REGISTRATION_HOLD_PLACED,
    REGISTRATION_HOLD_RELEASED,
];

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationV1 {
    pub registration_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Uuid,
    pub status: String,
    /// Curriculum component ids currently registered (after add/drop).
    pub component_ids: Vec<Uuid>,
    pub total_credits: f64,
    pub version: i64,
    pub student_subject_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationHoldV1 {
    pub hold_id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    /// `HoldSource` wire name.
    pub source: String,
    pub reason: String,
    pub status: String,
    /// 3B: login subject of the student, so inbox rules can notify the student (additive).
    #[serde(default)]
    pub student_subject_ref: Option<String>,
}
