//! **4B** — timetable, teaching sessions, attendance, syllabus coverage, academic monitoring, internal assessment and coursework (L2-D03/D04/D05/D07, L3-D-020…033, D-051…057).
//!
//! Owned by slice 4B: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch4-contracts.md` §4.3 in the PR.

use super::event_type;
use uuid::Uuid;

event_type!(
    /// Governed timetable published (StatusChangedV1, L3-D-020).
    TIMETABLE_PUBLISHED, TIMETABLE_PUBLISHED_SCHEMA, "timetable.published");
event_type!(
    /// Operational timetable in effect (StatusChangedV1).
    TIMETABLE_ACTIVATED, TIMETABLE_ACTIVATED_SCHEMA, "timetable.activated");
event_type!(
    /// A revised version replaced the operational one (StatusChangedV1, L3-D-021).
    TIMETABLE_REVISED, TIMETABLE_REVISED_SCHEMA, "timetable.revised");
event_type!(
    /// (StatusChangedV1, L3-D-022).
    TIMETABLE_RETIRED, TIMETABLE_RETIRED_SCHEMA, "timetable.retired");
event_type!(
    /// (StatusChangedV1, L3-D-023).
    TEACHING_SESSION_STARTED, TEACHING_SESSION_STARTED_SCHEMA, "teaching_session.started");
event_type!(
    /// (StatusChangedV1, L3-D-023).
    TEACHING_SESSION_COMPLETED, TEACHING_SESSION_COMPLETED_SCHEMA, "teaching_session.completed");
event_type!(
    /// (StatusChangedV1).
    TEACHING_SESSION_CANCELLED, TEACHING_SESSION_CANCELLED_SCHEMA, "teaching_session.cancelled");
event_type!(
    /// Session attendance finalised (StatusChangedV1, L3-D-024).
    ATTENDANCE_FINALISED, ATTENDANCE_FINALISED_SCHEMA, "attendance.finalised");
event_type!(
    /// Governed correction applied (ModifiedV1, L3-D-025).
    ATTENDANCE_CORRECTED, ATTENDANCE_CORRECTED_SCHEMA, "attendance.corrected");
event_type!(
    /// A student fell below the attendance threshold ([`AttendanceThresholdV1`]); Exams (Batch 6) consumes it for eligibility.
    ATTENDANCE_THRESHOLD_BREACHED, ATTENDANCE_THRESHOLD_BREACHED_SCHEMA, "attendance.threshold_breached");
event_type!(
    /// (StatusChangedV1, L3-D-026).
    SYLLABUS_COVERAGE_PUBLISHED, SYLLABUS_COVERAGE_PUBLISHED_SCHEMA, "syllabus_coverage.published");
event_type!(
    /// (StatusChangedV1, L3-D-027).
    MONITORING_CASE_OPENED, MONITORING_CASE_OPENED_SCHEMA, "monitoring_case.opened");
event_type!(
    /// (StatusChangedV1, L3-D-028).
    MONITORING_RISK_ASSESSED, MONITORING_RISK_ASSESSED_SCHEMA, "monitoring_case.risk_assessed");
event_type!(
    /// (StatusChangedV1, L3-D-030).
    MONITORING_INTERVENTION_TRIGGERED, MONITORING_INTERVENTION_TRIGGERED_SCHEMA, "monitoring_intervention.triggered");
event_type!(
    /// (StatusChangedV1, L3-D-032).
    MONITORING_CASE_CLOSED, MONITORING_CASE_CLOSED_SCHEMA, "monitoring_case.closed");
event_type!(
    /// (StatusChangedV1, L3-D-033).
    MONITORING_CASE_REOPENED, MONITORING_CASE_REOPENED_SCHEMA, "monitoring_case.reopened");
event_type!(
    /// (StatusChangedV1, L3-D-051).
    ASSESSMENT_BLUEPRINT_PUBLISHED, ASSESSMENT_BLUEPRINT_PUBLISHED_SCHEMA, "assessment_blueprint.published");
event_type!(
    /// Coursework / assessment opened to students (StatusChangedV1, L3-D-052).
    ASSESSMENT_WINDOW_OPENED, ASSESSMENT_WINDOW_OPENED_SCHEMA, "assessment_window.opened");
event_type!(
    /// (StatusChangedV1).
    ASSESSMENT_WINDOW_CLOSED, ASSESSMENT_WINDOW_CLOSED_SCHEMA, "assessment_window.closed");
event_type!(
    /// (StatusChangedV1, L3-D-053).
    ASSESSMENT_SUBMISSION_RECEIVED, ASSESSMENT_SUBMISSION_RECEIVED_SCHEMA, "assessment_submission.received");
event_type!(
    /// (StatusChangedV1, L3-D-054).
    ASSESSMENT_OUTCOME_MODERATED, ASSESSMENT_OUTCOME_MODERATED_SCHEMA, "assessment_outcome.moderated");
event_type!(
    /// Finalised internal marks ([`InternalAssessmentFinalizedV1`], L3-D-055); Exams consumes it.
    INTERNAL_ASSESSMENT_FINALIZED, INTERNAL_ASSESSMENT_FINALIZED_SCHEMA, "internal_assessment.finalized");
event_type!(
    /// ([`InternalAssessmentFinalizedV1`] of the corrected version, L3-D-057).
    INTERNAL_ASSESSMENT_CORRECTED, INTERNAL_ASSESSMENT_CORRECTED_SCHEMA, "internal_assessment.corrected");
event_type!(
    /// (StatusChangedV1, L3-D-056).
    INTERNAL_ASSESSMENT_ARCHIVED, INTERNAL_ASSESSMENT_ARCHIVED_SCHEMA, "internal_assessment.archived");

pub const EVENT_TYPES: &[&str] = &[
    TIMETABLE_PUBLISHED,
    TIMETABLE_ACTIVATED,
    TIMETABLE_REVISED,
    TIMETABLE_RETIRED,
    TEACHING_SESSION_STARTED,
    TEACHING_SESSION_COMPLETED,
    TEACHING_SESSION_CANCELLED,
    ATTENDANCE_FINALISED,
    ATTENDANCE_CORRECTED,
    ATTENDANCE_THRESHOLD_BREACHED,
    SYLLABUS_COVERAGE_PUBLISHED,
    MONITORING_CASE_OPENED,
    MONITORING_RISK_ASSESSED,
    MONITORING_INTERVENTION_TRIGGERED,
    MONITORING_CASE_CLOSED,
    MONITORING_CASE_REOPENED,
    ASSESSMENT_BLUEPRINT_PUBLISHED,
    ASSESSMENT_WINDOW_OPENED,
    ASSESSMENT_WINDOW_CLOSED,
    ASSESSMENT_SUBMISSION_RECEIVED,
    ASSESSMENT_OUTCOME_MODERATED,
    INTERNAL_ASSESSMENT_FINALIZED,
    INTERNAL_ASSESSMENT_CORRECTED,
    INTERNAL_ASSESSMENT_ARCHIVED,
];

/// Payload of [`ATTENDANCE_THRESHOLD_BREACHED`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceThresholdV1 {
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Uuid,
    pub academic_term_id: Uuid,
    pub attendance_percent: f64,
    pub threshold_percent: f64,
    pub sessions_counted: i32,
    pub student_subject_ref: Option<String>,
}

/// Payload of [`INTERNAL_ASSESSMENT_FINALIZED`] / [`INTERNAL_ASSESSMENT_CORRECTED`].
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalAssessmentFinalizedV1 {
    pub result_id: Uuid,
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Uuid,
    pub academic_term_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub marks_obtained: f64,
    pub max_marks: f64,
    pub version: i64,
}
