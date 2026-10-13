//! Slice-owned 6A state machines (contract §4.2: transitions of the slice enums are pure,
//! unit-tested business rules in the slice's `domain.rs`; an illegal move is `409`).
//!
//! The shared machines (session, paper, registration) keep their rules in
//! `sutra_contracts::exams::status` and are moved only through `shared::lifecycle`.

use sutra_contracts::exams::status::{
    ConductStatus, ExamExceptionStatus, ExamPolicyStatus, ExamScheduleStatus, HallTicketStatus,
    InvigilationDutyStatus, SeatingPlanStatus,
};

/// Examination policy versions (L2-I01 §20; L3-I-001 … I-003).
pub fn policy_can(from: ExamPolicyStatus, to: ExamPolicyStatus) -> bool {
    use ExamPolicyStatus::*;
    matches!(
        (from, to),
        (Draft, UnderReview)
            | (UnderReview, Approved)
            | (UnderReview, Draft)
            | (Approved, Scheduled)
            | (Approved, Active)
            | (Approved, Draft)
            | (Approved, Retired)
            | (Scheduled, Active)
            | (Scheduled, Retired)
            | (Active, Superseded)
            | (Active, Retired)
            | (Superseded, Retired)
            | (Superseded, Archived)
            | (Retired, Archived)
    )
}

/// Schedules / timetables (L2-I03 §20; L3-I-007, 008, 013, 016).
pub fn schedule_can(from: ExamScheduleStatus, to: ExamScheduleStatus) -> bool {
    use ExamScheduleStatus::*;
    matches!(
        (from, to),
        (Draft, UnderReview)
            | (Draft, Cancelled)
            | (UnderReview, Approved)
            | (UnderReview, Draft)
            | (Approved, Published)
            | (Approved, Draft)
            | (Published, Active)
            | (Published, Revised)
            | (Published, Cancelled)
            | (Active, Revised)
            | (Active, Cancelled)
            | (Active, Completed)
            | (Revised, Archived)
            | (Cancelled, Archived)
            | (Completed, Archived)
    )
}

/// The schedule is (or was) the official timetable.
pub fn schedule_is_official(s: ExamScheduleStatus) -> bool {
    matches!(
        s,
        ExamScheduleStatus::Published | ExamScheduleStatus::Active
    )
}

/// The schedule's sittings may still be edited.
pub fn schedule_is_editable(s: ExamScheduleStatus) -> bool {
    matches!(s, ExamScheduleStatus::Draft)
}

/// Hall tickets (L2-I05 §20 + `withheld`; L3-I-037 … I-042).
pub fn hall_ticket_can(from: HallTicketStatus, to: HallTicketStatus) -> bool {
    use HallTicketStatus::*;
    matches!(
        (from, to),
        (Draft, UnderReview)
            | (Draft, Active)
            | (Draft, Withheld)
            | (UnderReview, Approved)
            | (UnderReview, Active)
            | (UnderReview, Withheld)
            | (UnderReview, Cancelled)
            | (Approved, Active)
            | (Approved, Published)
            | (Published, Active)
            | (Active, Withheld)
            | (Active, Reissued)
            | (Active, Replaced)
            | (Active, Withdrawn)
            | (Active, Completed)
            | (Withheld, Active)
            | (Withheld, Withdrawn)
            | (Withheld, Cancelled)
            | (Reissued, Archived)
            | (Replaced, Archived)
            | (Withdrawn, Archived)
            | (Cancelled, Archived)
            | (Completed, Archived)
    )
}

/// Statuses holding the registration's single current ticket (unique index).
pub fn hall_ticket_is_current(s: HallTicketStatus) -> bool {
    use HallTicketStatus::*;
    matches!(
        s,
        Draft | UnderReview | Approved | Published | Active | Withheld
    )
}

/// Seating plans (L3-I-015 / I-059).
pub fn seating_can(from: SeatingPlanStatus, to: SeatingPlanStatus) -> bool {
    use SeatingPlanStatus::*;
    matches!(
        (from, to),
        (Draft, Published) | (Draft, Cancelled) | (Published, Revised) | (Published, Cancelled)
    )
}

/// Invigilation / examiner duties (L3-I-010, 011, 012, 060).
pub fn duty_can(from: InvigilationDutyStatus, to: InvigilationDutyStatus) -> bool {
    use InvigilationDutyStatus::*;
    matches!(
        (from, to),
        (Assigned, Confirmed)
            | (Assigned, Replaced)
            | (Assigned, Released)
            | (Confirmed, Replaced)
            | (Confirmed, Released)
            | (Confirmed, Completed)
            | (Assigned, Completed)
    )
}

/// Live duties count towards the sitting's staffing.
pub fn duty_is_live(s: InvigilationDutyStatus) -> bool {
    matches!(
        s,
        InvigilationDutyStatus::Assigned | InvigilationDutyStatus::Confirmed
    )
}

/// Conduct of a sitting (L3-I-043, 055, 174, 175, 185).
pub fn conduct_can(from: ConductStatus, to: ConductStatus) -> bool {
    use ConductStatus::*;
    matches!(
        (from, to),
        (Scheduled, InProgress)
            | (Scheduled, Postponed)
            | (InProgress, Suspended)
            | (InProgress, Completed)
            | (InProgress, Terminated)
            | (InProgress, Postponed)
            | (Suspended, InProgress)
            | (Suspended, Postponed)
            | (Suspended, Terminated)
    )
}

/// Examination exception cases (L2-I15; L3-I-172).
pub fn exception_can(from: ExamExceptionStatus, to: ExamExceptionStatus) -> bool {
    use ExamExceptionStatus::*;
    matches!(
        (from, to),
        (Raised, Classified)
            | (Raised, UnderReview)
            | (Classified, UnderReview)
            | (Classified, Resolved)
            | (UnderReview, Resolved)
            | (UnderReview, Classified)
            | (Resolved, Closed)
            | (Resolved, UnderReview)
    )
}

/// Minimum invigilators for `candidates` at one invigilator per `ratio` candidates (≥ 1 when
/// anyone sits).
pub fn invigilators_required(candidates: i64, ratio: i64) -> i64 {
    if candidates <= 0 {
        0
    } else {
        let r = ratio.max(1);
        (candidates + r - 1) / r
    }
}

/// `true` when two sittings `[start, start + duration)` on the same date overlap.
pub fn overlaps(
    a: (chrono::NaiveDate, chrono::NaiveTime, i32),
    b: (chrono::NaiveDate, chrono::NaiveTime, i32),
) -> bool {
    if a.0 != b.0 {
        return false;
    }
    use chrono::Timelike;
    let minute = |t: chrono::NaiveTime| i64::from(t.num_seconds_from_midnight() / 60);
    let (sa, sb) = (minute(a.1), minute(b.1));
    sa < sb + i64::from(b.2) && sb < sa + i64::from(a.2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};

    #[test]
    fn policy_lifecycle() {
        use ExamPolicyStatus::*;
        assert!(policy_can(Draft, UnderReview) && policy_can(Approved, Active));
        assert!(policy_can(Active, Superseded) && policy_can(Superseded, Retired));
        assert!(!policy_can(Draft, Active) && !policy_can(Retired, Active));
        assert!(!policy_can(Archived, Draft));
    }

    #[test]
    fn schedule_and_ticket_lifecycles() {
        use ExamScheduleStatus as S;
        assert!(schedule_can(S::Approved, S::Published) && schedule_can(S::Published, S::Revised));
        assert!(!schedule_can(S::Draft, S::Published) && !schedule_can(S::Cancelled, S::Draft));
        assert!(schedule_is_official(S::Published) && !schedule_is_official(S::Revised));
        use HallTicketStatus as H;
        assert!(
            hall_ticket_can(H::Withheld, H::Active) && hall_ticket_can(H::Active, H::Withdrawn)
        );
        assert!(!hall_ticket_can(H::Withdrawn, H::Active));
        assert!(hall_ticket_is_current(H::Withheld) && !hall_ticket_is_current(H::Replaced));
    }

    #[test]
    fn conduct_duty_seating_exception() {
        use ConductStatus as C;
        assert!(
            conduct_can(C::InProgress, C::Suspended) && conduct_can(C::Suspended, C::InProgress)
        );
        assert!(
            !conduct_can(C::Completed, C::InProgress) && !conduct_can(C::Scheduled, C::Completed)
        );
        use InvigilationDutyStatus as D;
        assert!(duty_can(D::Confirmed, D::Replaced) && !duty_can(D::Replaced, D::Assigned));
        assert!(duty_is_live(D::Assigned) && !duty_is_live(D::Released));
        use SeatingPlanStatus as P;
        assert!(seating_can(P::Draft, P::Published) && !seating_can(P::Revised, P::Published));
        use ExamExceptionStatus as E;
        assert!(exception_can(E::Raised, E::Classified) && exception_can(E::Resolved, E::Closed));
        assert!(!exception_can(E::Closed, E::Raised));
    }

    #[test]
    fn staffing_and_overlaps() {
        assert_eq!(invigilators_required(0, 30), 0);
        assert_eq!(invigilators_required(1, 30), 1);
        assert_eq!(invigilators_required(61, 30), 3);
        let d = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap_or_default();
        let t = |h| NaiveTime::from_hms_opt(h, 0, 0).unwrap_or_default();
        assert!(overlaps((d, t(9), 180), (d, t(11), 60)));
        assert!(!overlaps((d, t(9), 120), (d, t(11), 60)));
        assert!(!overlaps(
            (d, t(9), 180),
            (d.succ_opt().unwrap_or_default(), t(9), 60)
        ));
    }
}
