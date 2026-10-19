//! **3C** — scholarship definitions, eligibility policy, determinations, renewal, expiry, external sync, bulk recalculation, financial-aid hand-off (L2-C07, L3-C-062…071).
//!
//! Owned by slice 3C: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch3-contracts.md` §4.3 in the PR.

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// (StatusChangedV1, L3-C-062).
    SCHOLARSHIP_PUBLISHED, SCHOLARSHIP_PUBLISHED_SCHEMA, "scholarship.published");
event_type!(
    /// (StatusChangedV1).
    SCHOLARSHIP_ACTIVATED, SCHOLARSHIP_ACTIVATED_SCHEMA, "scholarship.activated");
event_type!(
    /// (StatusChangedV1).
    SCHOLARSHIP_SUSPENDED, SCHOLARSHIP_SUSPENDED_SCHEMA, "scholarship.suspended");
event_type!(
    /// (StatusChangedV1).
    SCHOLARSHIP_EXPIRED, SCHOLARSHIP_EXPIRED_SCHEMA, "scholarship.expired");
event_type!(
    /// (StatusChangedV1).
    SCHOLARSHIP_ARCHIVED, SCHOLARSHIP_ARCHIVED_SCHEMA, "scholarship.archived");
event_type!(
    /// (StatusChangedV1, L3-C-063).
    SCHOLARSHIP_POLICY_ACTIVATED, SCHOLARSHIP_POLICY_ACTIVATED_SCHEMA, "scholarship_policy.activated");
event_type!(
    /// (StatusChangedV1).
    SCHOLARSHIP_POLICY_RETIRED, SCHOLARSHIP_POLICY_RETIRED_SCHEMA, "scholarship_policy.retired");
event_type!(
    /// Eligible / not eligible / conditional ([`ScholarshipEligibilityV1`], L3-C-064).
    SCHOLARSHIP_ELIGIBILITY_DETERMINED, SCHOLARSHIP_ELIGIBILITY_DETERMINED_SCHEMA, "scholarship_eligibility.determined");
event_type!(
    /// Recommendation generated ([`ScholarshipEligibilityV1`]).
    SCHOLARSHIP_ELIGIBILITY_RECOMMENDED, SCHOLARSHIP_ELIGIBILITY_RECOMMENDED_SCHEMA, "scholarship_eligibility.recommended");
event_type!(
    /// (StatusChangedV1, L3-C-065).
    SCHOLARSHIP_ELIGIBILITY_CONDITION_RESOLVED, SCHOLARSHIP_ELIGIBILITY_CONDITION_RESOLVED_SCHEMA, "scholarship_eligibility.condition_resolved");
event_type!(
    /// ([`ScholarshipEligibilityV1`], L3-C-066).
    SCHOLARSHIP_ELIGIBILITY_REASSESSED, SCHOLARSHIP_ELIGIBILITY_REASSESSED_SCHEMA, "scholarship_eligibility.reassessed");
event_type!(
    /// ([`ScholarshipEligibilityV1`], L3-C-067).
    SCHOLARSHIP_ELIGIBILITY_RENEWED, SCHOLARSHIP_ELIGIBILITY_RENEWED_SCHEMA, "scholarship_eligibility.renewed");
event_type!(
    /// (StatusChangedV1, L3-C-068).
    SCHOLARSHIP_ELIGIBILITY_EXPIRED, SCHOLARSHIP_ELIGIBILITY_EXPIRED_SCHEMA, "scholarship_eligibility.expired");
event_type!(
    /// Hand-off to Financial Aid (Domain F, Batch 5) — **event only**, no Finance call ([`ScholarshipEligibilityV1`], L3-C-071).
    SCHOLARSHIP_ELIGIBILITY_HANDED_OFF, SCHOLARSHIP_ELIGIBILITY_HANDED_OFF_SCHEMA, "scholarship_eligibility.handed_off");
event_type!(
    /// External ecosystem sync result (StatusChangedV1, L3-C-069; port, no-op adapter).
    SCHOLARSHIP_EXTERNAL_SYNCHRONISED, SCHOLARSHIP_EXTERNAL_SYNCHRONISED_SCHEMA, "scholarship_external.synchronised");
event_type!(
    /// Bulk recalculation run finished ([`ScholarshipRecalculationV1`], L3-C-070).
    SCHOLARSHIP_RECALCULATION_COMPLETED, SCHOLARSHIP_RECALCULATION_COMPLETED_SCHEMA, "scholarship_recalculation.completed");

pub const EVENT_TYPES: &[&str] = &[
    SCHOLARSHIP_PUBLISHED,
    SCHOLARSHIP_ACTIVATED,
    SCHOLARSHIP_SUSPENDED,
    SCHOLARSHIP_EXPIRED,
    SCHOLARSHIP_ARCHIVED,
    SCHOLARSHIP_POLICY_ACTIVATED,
    SCHOLARSHIP_POLICY_RETIRED,
    SCHOLARSHIP_ELIGIBILITY_DETERMINED,
    SCHOLARSHIP_ELIGIBILITY_RECOMMENDED,
    SCHOLARSHIP_ELIGIBILITY_CONDITION_RESOLVED,
    SCHOLARSHIP_ELIGIBILITY_REASSESSED,
    SCHOLARSHIP_ELIGIBILITY_RENEWED,
    SCHOLARSHIP_ELIGIBILITY_EXPIRED,
    SCHOLARSHIP_ELIGIBILITY_HANDED_OFF,
    SCHOLARSHIP_EXTERNAL_SYNCHRONISED,
    SCHOLARSHIP_RECALCULATION_COMPLETED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipEligibilityV1 {
    pub eligibility_id: Uuid,
    pub scholarship_id: Uuid,
    pub policy_id: Option<Uuid>,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    /// `ScholarshipEligibilityStatus` wire name.
    pub status: String,
    pub academic_year_id: Option<Uuid>,
    pub valid_until: Option<NaiveDate>,
    pub student_subject_ref: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScholarshipRecalculationV1 {
    pub recalculation_id: Uuid,
    pub institution_id: Uuid,
    pub scholarship_id: Option<Uuid>,
    pub evaluated: i64,
    pub changed: i64,
}
