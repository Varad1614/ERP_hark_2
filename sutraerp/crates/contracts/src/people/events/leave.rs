//! **7B** — leave policy, requests, accrual, consumption, cancellation, adjustment, carry forward, encashment, exceptions, leave cases, long leave, attendance, workload (L2-E05, E01 long leave, E03 workload; L3-E-006, 030, 034, 044…052).
//!
//! Owned by slice 7B: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch7-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;

event_type!(
    /// Leave policy version active (L3-E-044).
    LEAVE_POLICY_ACTIVATED, LEAVE_POLICY_ACTIVATED_SCHEMA, "leave_policy.activated");
event_type!(
    /// Leave request awaiting a decision (L3-E-045; `managerSubjectRef` set when routed to the reporting manager).
    LEAVE_REQUEST_SUBMITTED, LEAVE_REQUEST_SUBMITTED_SCHEMA, "leave_request.submitted");
event_type!(
    /// Leave approved and reserved (L3-E-045/047).
    LEAVE_REQUEST_APPROVED, LEAVE_REQUEST_APPROVED_SCHEMA, "leave_request.approved");
event_type!(
    /// Leave rejected (L3-E-045).
    LEAVE_REQUEST_REJECTED, LEAVE_REQUEST_REJECTED_SCHEMA, "leave_request.rejected");
event_type!(
    /// Approved / reserved leave cancelled, reservation released (L3-E-048).
    LEAVE_REQUEST_CANCELLED, LEAVE_REQUEST_CANCELLED_SCHEMA, "leave_request.cancelled");
event_type!(
    /// Leave consumed from the balance (L3-E-047).
    LEAVE_REQUEST_CONSUMED, LEAVE_REQUEST_CONSUMED_SCHEMA, "leave_request.consumed");
event_type!(
    /// Periodic accrual run completed (L3-E-046).
    LEAVE_ACCRUED, LEAVE_ACCRUED_SCHEMA, "leave_accrual_run.completed");
event_type!(
    /// Governed balance adjustment applied (L3-E-049).
    LEAVE_BALANCE_ADJUSTED, LEAVE_BALANCE_ADJUSTED_SCHEMA, "leave_adjustment.applied");
event_type!(
    /// Year-end carry forward / expiry completed (L3-E-050).
    LEAVE_CARRIED_FORWARD, LEAVE_CARRIED_FORWARD_SCHEMA, "leave_year_end_run.completed");
event_type!(
    /// Encashment eligibility decided (L3-E-051).
    LEAVE_ENCASHMENT_DECIDED, LEAVE_ENCASHMENT_DECIDED_SCHEMA, "leave_encashment.decided");
event_type!(
    /// Leave exception resolved (L3-E-052).
    LEAVE_EXCEPTION_RESOLVED, LEAVE_EXCEPTION_RESOLVED_SCHEMA, "leave_exception.resolved");
event_type!(
    /// Leave case coordination decided (L3-E-034).
    LEAVE_CASE_DECIDED, LEAVE_CASE_DECIDED_SCHEMA, "leave_case.decided");
event_type!(
    /// Long leave started: engagement long_leave (L3-E-006).
    LONG_LEAVE_STARTED, LONG_LEAVE_STARTED_SCHEMA, "long_leave.started");
event_type!(
    /// Long leave ended: engagement active again (L3-E-006).
    LONG_LEAVE_ENDED, LONG_LEAVE_ENDED_SCHEMA, "long_leave.ended");
event_type!(
    /// Attendance day corrected.
    ATTENDANCE_CORRECTED, ATTENDANCE_CORRECTED_SCHEMA, "attendance_day.corrected");
event_type!(
    /// Faculty availability / workload plan approved (L3-E-030).
    WORKLOAD_PLAN_APPROVED, WORKLOAD_PLAN_APPROVED_SCHEMA, "workload_plan.approved");

/// Event types of this slice (inbox rules may claim only these).
pub const EVENT_TYPES: &[&str] = &[
    LEAVE_POLICY_ACTIVATED,
    LEAVE_REQUEST_SUBMITTED,
    LEAVE_REQUEST_APPROVED,
    LEAVE_REQUEST_REJECTED,
    LEAVE_REQUEST_CANCELLED,
    LEAVE_REQUEST_CONSUMED,
    LEAVE_ACCRUED,
    LEAVE_BALANCE_ADJUSTED,
    LEAVE_CARRIED_FORWARD,
    LEAVE_ENCASHMENT_DECIDED,
    LEAVE_EXCEPTION_RESOLVED,
    LEAVE_CASE_DECIDED,
    LONG_LEAVE_STARTED,
    LONG_LEAVE_ENDED,
    ATTENDANCE_CORRECTED,
    WORKLOAD_PLAN_APPROVED,
];
