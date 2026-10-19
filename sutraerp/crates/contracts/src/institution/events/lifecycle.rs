//! **2A** — institution lifecycle, profile revision, business timezone, institutional policies.
//!
//! Transition events use [`StatusChangedV1`](super::StatusChangedV1) (`aggregateId` =
//! institution id); profile revision uses [`ModifiedV1`](super::ModifiedV1).

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// Draft → Configured (StatusChangedV1).
    INSTITUTION_CONFIGURED, INSTITUTION_CONFIGURED_SCHEMA, "institution.configured");
event_type!(
    /// Configured → PendingApproval (StatusChangedV1, `processInstanceId` set).
    INSTITUTION_SUBMITTED_FOR_APPROVAL, INSTITUTION_SUBMITTED_FOR_APPROVAL_SCHEMA,
    "institution.submitted_for_approval");
event_type!(
    /// PendingApproval → Returned (StatusChangedV1).
    INSTITUTION_RETURNED_FOR_CORRECTION, INSTITUTION_RETURNED_FOR_CORRECTION_SCHEMA,
    "institution.returned_for_correction");
event_type!(
    /// PendingApproval → Rejected (StatusChangedV1).
    INSTITUTION_REJECTED, INSTITUTION_REJECTED_SCHEMA, "institution.rejected");
event_type!(
    /// PendingApproval → Active (StatusChangedV1). Read by 2C readiness consumers.
    INSTITUTION_ACTIVATED, INSTITUTION_ACTIVATED_SCHEMA, "institution.activated");
event_type!(
    /// Active → Suspended (StatusChangedV1).
    INSTITUTION_SUSPENDED, INSTITUTION_SUSPENDED_SCHEMA, "institution.suspended");
event_type!(
    /// Suspended → Active (StatusChangedV1).
    INSTITUTION_REACTIVATED, INSTITUTION_REACTIVATED_SCHEMA, "institution.reactivated");
event_type!(
    /// → Archived, terminal (StatusChangedV1).
    INSTITUTION_ARCHIVED, INSTITUTION_ARCHIVED_SCHEMA, "institution.archived");
event_type!(
    /// Profile / master information revised (ModifiedV1).
    INSTITUTION_PROFILE_REVISED, INSTITUTION_PROFILE_REVISED_SCHEMA, "institution.profile_revised");
event_type!(
    /// Business timezone changed ([`InstitutionTimezoneChangedV1`]).
    INSTITUTION_TIMEZONE_CHANGED, INSTITUTION_TIMEZONE_CHANGED_SCHEMA,
    "institution.timezone_changed");
event_type!(
    /// A policy revision became effective ([`InstitutionPolicyRevisedV1`]).
    INSTITUTION_POLICY_REVISED, INSTITUTION_POLICY_REVISED_SCHEMA, "institution_policy.revised");

pub const EVENT_TYPES: &[&str] = &[
    INSTITUTION_CONFIGURED,
    INSTITUTION_SUBMITTED_FOR_APPROVAL,
    INSTITUTION_RETURNED_FOR_CORRECTION,
    INSTITUTION_REJECTED,
    INSTITUTION_ACTIVATED,
    INSTITUTION_SUSPENDED,
    INSTITUTION_REACTIVATED,
    INSTITUTION_ARCHIVED,
    INSTITUTION_PROFILE_REVISED,
    INSTITUTION_TIMEZONE_CHANGED,
    INSTITUTION_POLICY_REVISED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstitutionTimezoneChangedV1 {
    pub institution_id: Uuid,
    /// IANA zone, e.g. `Asia/Kolkata`.
    pub timezone: String,
    pub previous_timezone: String,
    pub version: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstitutionPolicyRevisedV1 {
    pub institution_id: Uuid,
    pub policy_id: Uuid,
    /// Stable policy key, e.g. `attendance.minimum_percentage`.
    pub policy_key: String,
    /// Monotonic revision number of this policy (1-based).
    pub revision: i32,
    /// Business-local date (institution timezone) from which the revision applies.
    pub effective_from: NaiveDate,
}
