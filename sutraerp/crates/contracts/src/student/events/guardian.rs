//! **3C** — guardian relationships, custody, consent, communication, emergency contacts, financial responsibility, pickup (L2-C06, L3-C-051…061).
//!
//! Owned by slice 3C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;
use uuid::Uuid;

event_type!(
    /// ([`GuardianRelationshipV1`]).
    GUARDIAN_RELATIONSHIP_PROPOSED, GUARDIAN_RELATIONSHIP_PROPOSED_SCHEMA, "guardian_relationship.proposed");
event_type!(
    /// (StatusChangedV1).
    GUARDIAN_RELATIONSHIP_VERIFIED, GUARDIAN_RELATIONSHIP_VERIFIED_SCHEMA, "guardian_relationship.verified");
event_type!(
    /// (StatusChangedV1).
    GUARDIAN_RELATIONSHIP_ACTIVATED, GUARDIAN_RELATIONSHIP_ACTIVATED_SCHEMA, "guardian_relationship.activated");
event_type!(
    /// (ModifiedV1, L3-C-052).
    GUARDIAN_RELATIONSHIP_MODIFIED, GUARDIAN_RELATIONSHIP_MODIFIED_SCHEMA, "guardian_relationship.modified");
event_type!(
    /// (StatusChangedV1, L3-C-059).
    GUARDIAN_RELATIONSHIP_SUSPENDED, GUARDIAN_RELATIONSHIP_SUSPENDED_SCHEMA, "guardian_relationship.suspended");
event_type!(
    /// (StatusChangedV1, L3-C-060).
    GUARDIAN_RELATIONSHIP_REINSTATED, GUARDIAN_RELATIONSHIP_REINSTATED_SCHEMA, "guardian_relationship.reinstated");
event_type!(
    /// (StatusChangedV1, L3-C-061).
    GUARDIAN_RELATIONSHIP_TERMINATED, GUARDIAN_RELATIONSHIP_TERMINATED_SCHEMA, "guardian_relationship.terminated");
event_type!(
    /// ([`GuardianRelationshipV1`], L3-C-053).
    GUARDIAN_CUSTODY_CHANGED, GUARDIAN_CUSTODY_CHANGED_SCHEMA, "guardian_custody.changed");
event_type!(
    /// (StatusChangedV1, L3-C-054).
    GUARDIAN_CONSENT_GRANTED, GUARDIAN_CONSENT_GRANTED_SCHEMA, "guardian_consent.granted");
event_type!(
    /// (StatusChangedV1).
    GUARDIAN_CONSENT_WITHDRAWN, GUARDIAN_CONSENT_WITHDRAWN_SCHEMA, "guardian_consent.withdrawn");
event_type!(
    /// (StatusChangedV1, L3-C-055).
    GUARDIAN_COMMUNICATION_AUTHORISED, GUARDIAN_COMMUNICATION_AUTHORISED_SCHEMA, "guardian_communication.authorised");
event_type!(
    /// (StatusChangedV1).
    GUARDIAN_COMMUNICATION_REVOKED, GUARDIAN_COMMUNICATION_REVOKED_SCHEMA, "guardian_communication.revoked");
event_type!(
    /// (StatusChangedV1, L3-C-056).
    EMERGENCY_CONTACT_ASSIGNED, EMERGENCY_CONTACT_ASSIGNED_SCHEMA, "emergency_contact.assigned");
event_type!(
    /// (StatusChangedV1).
    EMERGENCY_CONTACT_REMOVED, EMERGENCY_CONTACT_REMOVED_SCHEMA, "emergency_contact.removed");
event_type!(
    /// (StatusChangedV1, L3-C-057).
    FINANCIAL_RESPONSIBILITY_ASSIGNED, FINANCIAL_RESPONSIBILITY_ASSIGNED_SCHEMA, "financial_responsibility.assigned");
event_type!(
    /// (StatusChangedV1).
    FINANCIAL_RESPONSIBILITY_ENDED, FINANCIAL_RESPONSIBILITY_ENDED_SCHEMA, "financial_responsibility.ended");
event_type!(
    /// (StatusChangedV1, L3-C-058).
    PICKUP_AUTHORISATION_GRANTED, PICKUP_AUTHORISATION_GRANTED_SCHEMA, "pickup_authorisation.granted");
event_type!(
    /// (StatusChangedV1).
    PICKUP_AUTHORISATION_REVOKED, PICKUP_AUTHORISATION_REVOKED_SCHEMA, "pickup_authorisation.revoked");

pub const EVENT_TYPES: &[&str] = &[
    GUARDIAN_RELATIONSHIP_PROPOSED,
    GUARDIAN_RELATIONSHIP_VERIFIED,
    GUARDIAN_RELATIONSHIP_ACTIVATED,
    GUARDIAN_RELATIONSHIP_MODIFIED,
    GUARDIAN_RELATIONSHIP_SUSPENDED,
    GUARDIAN_RELATIONSHIP_REINSTATED,
    GUARDIAN_RELATIONSHIP_TERMINATED,
    GUARDIAN_CUSTODY_CHANGED,
    GUARDIAN_CONSENT_GRANTED,
    GUARDIAN_CONSENT_WITHDRAWN,
    GUARDIAN_COMMUNICATION_AUTHORISED,
    GUARDIAN_COMMUNICATION_REVOKED,
    EMERGENCY_CONTACT_ASSIGNED,
    EMERGENCY_CONTACT_REMOVED,
    FINANCIAL_RESPONSIBILITY_ASSIGNED,
    FINANCIAL_RESPONSIBILITY_ENDED,
    PICKUP_AUTHORISATION_GRANTED,
    PICKUP_AUTHORISATION_REVOKED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuardianRelationshipV1 {
    pub relationship_id: Uuid,
    pub guardian_id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    /// e.g. `mother`, `father`, `legal_guardian`, `sponsor`.
    pub relationship_type: String,
    pub has_legal_custody: bool,
    pub status: String,
    pub version: i64,
}
