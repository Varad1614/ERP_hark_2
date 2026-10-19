//! **6A** — policy, sessions, scheduling, venues, seating, invigilation, registration, hall tickets, conduct basics (L2-I01…I07, I15; L3-I-001…043, 055…060, 172…185 in scope).
//!
//! Owned by slice 6A: add events here (constant + `EVENT_TYPES` entry + payload) and list
//! them in `docs/batch6-contracts.md` §4.3 in the PR. Payload: [`super::StatusChangedV1`] unless
//! noted.

use super::event_type;

event_type!(
    /// Examination policy version became active (L3-I-001).
    EXAM_POLICY_ACTIVATED, EXAM_POLICY_ACTIVATED_SCHEMA, "exam_policy.activated");
event_type!(
    /// Policy replaced by a newer approved version (L3-I-002).
    EXAM_POLICY_SUPERSEDED, EXAM_POLICY_SUPERSEDED_SCHEMA, "exam_policy.superseded");
event_type!(
    /// Policy retired with history preserved (L3-I-003).
    EXAM_POLICY_RETIRED, EXAM_POLICY_RETIRED_SCHEMA, "exam_policy.retired");
event_type!(
    /// Session submitted for review (L3-I-004).
    EXAM_SESSION_SUBMITTED, EXAM_SESSION_SUBMITTED_SCHEMA, "exam_session.submitted");
event_type!(
    /// Session approved (L3-I-004).
    EXAM_SESSION_APPROVED, EXAM_SESSION_APPROVED_SCHEMA, "exam_session.approved");
event_type!(
    /// Session active for operational workflows (L3-I-004).
    EXAM_SESSION_ACTIVATED, EXAM_SESSION_ACTIVATED_SCHEMA, "exam_session.activated");
event_type!(
    /// Session suspended (L3-I-005).
    EXAM_SESSION_SUSPENDED, EXAM_SESSION_SUSPENDED_SCHEMA, "exam_session.suspended");
event_type!(
    /// Suspended session reopened (L3-I-005).
    EXAM_SESSION_REOPENED, EXAM_SESSION_REOPENED_SCHEMA, "exam_session.reopened");
event_type!(
    /// Examination schedule approved (L3-I-007/008).
    EXAM_SCHEDULE_APPROVED, EXAM_SCHEDULE_APPROVED_SCHEMA, "exam_schedule.approved");
event_type!(
    /// Official timetable published to candidates and staff (L3-I-013).
    EXAM_TIMETABLE_PUBLISHED, EXAM_TIMETABLE_PUBLISHED_SCHEMA, "exam_schedule.published");
event_type!(
    /// Published timetable withdrawn, stakeholders notified (L3-I-016).
    EXAM_TIMETABLE_WITHDRAWN, EXAM_TIMETABLE_WITHDRAWN_SCHEMA, "exam_schedule.withdrawn");
event_type!(
    /// Seating arrangement published (L3-I-015).
    SEATING_PLAN_PUBLISHED, SEATING_PLAN_PUBLISHED_SCHEMA, "seating_plan.published");
event_type!(
    /// Invigilation / examiner duty assigned (L3-I-010/012).
    INVIGILATION_ASSIGNED, INVIGILATION_ASSIGNED_SCHEMA, "invigilation_duty.assigned");
event_type!(
    /// Registration confirmed (L3-I-017/018/019/034).
    EXAM_REGISTRATION_CONFIRMED, EXAM_REGISTRATION_CONFIRMED_SCHEMA, "exam_registration.confirmed");
event_type!(
    /// Registration rejected / ineligible (L3-I-026).
    EXAM_REGISTRATION_REJECTED, EXAM_REGISTRATION_REJECTED_SCHEMA, "exam_registration.rejected");
event_type!(
    /// Registration withdrawn (L3-I-023).
    EXAM_REGISTRATION_WITHDRAWN, EXAM_REGISTRATION_WITHDRAWN_SCHEMA, "exam_registration.withdrawn");
event_type!(
    /// Registration population finalised for downstream processing (L3-I-036).
    EXAM_REGISTRATION_LOCKED, EXAM_REGISTRATION_LOCKED_SCHEMA, "exam_registration.locked");
event_type!(
    /// Hall ticket issued (L3-I-037/040).
    HALL_TICKET_ISSUED, HALL_TICKET_ISSUED_SCHEMA, "hall_ticket.issued");
event_type!(
    /// Hall ticket withheld: entry conditions unsatisfied, e.g. finance dues (L3-I-038).
    HALL_TICKET_WITHHELD, HALL_TICKET_WITHHELD_SCHEMA, "hall_ticket.withheld");
event_type!(
    /// Hall ticket invalidated (L3-I-039/041).
    HALL_TICKET_WITHDRAWN, HALL_TICKET_WITHDRAWN_SCHEMA, "hall_ticket.withdrawn");
event_type!(
    /// Sitting conducted; attendance recorded (L3-I-043/055).
    EXAM_CONDUCT_COMPLETED, EXAM_CONDUCT_COMPLETED_SCHEMA, "conduct_record.completed");
event_type!(
    /// Conduct suspended under an authorised contingency (L3-I-174).
    EXAM_CONDUCT_SUSPENDED, EXAM_CONDUCT_SUSPENDED_SCHEMA, "conduct_record.suspended");
event_type!(
    /// Sitting postponed and rescheduled (L3-I-185).
    EXAM_POSTPONED, EXAM_POSTPONED_SCHEMA, "conduct_record.postponed");

/// Event types of this slice.
pub const EVENT_TYPES: &[&str] = &[
    EXAM_POLICY_ACTIVATED,
    EXAM_POLICY_SUPERSEDED,
    EXAM_POLICY_RETIRED,
    EXAM_SESSION_SUBMITTED,
    EXAM_SESSION_APPROVED,
    EXAM_SESSION_ACTIVATED,
    EXAM_SESSION_SUSPENDED,
    EXAM_SESSION_REOPENED,
    EXAM_SCHEDULE_APPROVED,
    EXAM_TIMETABLE_PUBLISHED,
    EXAM_TIMETABLE_WITHDRAWN,
    SEATING_PLAN_PUBLISHED,
    INVIGILATION_ASSIGNED,
    EXAM_REGISTRATION_CONFIRMED,
    EXAM_REGISTRATION_REJECTED,
    EXAM_REGISTRATION_WITHDRAWN,
    EXAM_REGISTRATION_LOCKED,
    HALL_TICKET_ISSUED,
    HALL_TICKET_WITHHELD,
    HALL_TICKET_WITHDRAWN,
    EXAM_CONDUCT_COMPLETED,
    EXAM_CONDUCT_SUSPENDED,
    EXAM_POSTPONED,
];
