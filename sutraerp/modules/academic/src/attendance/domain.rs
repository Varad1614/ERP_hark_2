//! Pure rules of teaching sessions, attendance, thresholds and syllabus coverage
//! (L2-D04 §21, L3-D-023…026). No I/O; unit-tested here.

use sutra_contracts::academic::status::{
    AttendanceCaptureStatus, AttendanceMark, PolicyVersionStatus, SyllabusCoverageStatus,
    TeachingSessionStatus,
};

/// Legal teaching-session transitions (L2-D04 §21.1).
pub fn can_transition(from: TeachingSessionStatus, to: TeachingSessionStatus) -> bool {
    use TeachingSessionStatus::*;
    matches!(
        (from, to),
        (Scheduled, Started)
            | (Scheduled, Cancelled)
            | (Started, InProgress)
            | (Started, Paused)
            | (Started, Completed)
            | (Started, Abandoned)
            | (InProgress, Paused)
            | (InProgress, Completed)
            | (InProgress, Abandoned)
            | (Paused, InProgress)
            | (Paused, Abandoned)
            | (Completed, Locked)
            | (Cancelled, Locked)
            | (Abandoned, Locked)
            | (Locked, Archived)
    )
}

/// The session is under way or done, so attendance can be recorded.
pub fn accepts_attendance(s: TeachingSessionStatus) -> bool {
    use TeachingSessionStatus::*;
    matches!(s, Started | InProgress | Paused | Completed)
}

/// Attendance can still be captured (not finalised).
pub fn capture_open(a: AttendanceCaptureStatus) -> bool {
    !matches!(a, AttendanceCaptureStatus::Finalised)
}

/// Finalisation needs a running or completed session and captured attendance.
pub fn can_finalise(s: TeachingSessionStatus, a: AttendanceCaptureStatus) -> bool {
    use TeachingSessionStatus::*;
    matches!(s, Started | InProgress | Completed)
        && matches!(
            a,
            AttendanceCaptureStatus::Capturing | AttendanceCaptureStatus::Submitted
        )
}

/// Legal policy-version transitions.
pub fn policy_can_transition(from: PolicyVersionStatus, to: PolicyVersionStatus) -> bool {
    use PolicyVersionStatus::*;
    matches!(
        (from, to),
        (Draft, InReview)
            | (InReview, Effective)
            | (InReview, Draft)
            | (Effective, Superseded)
            | (Effective, Retired)
            | (Draft, Retired)
    )
}

/// Legal syllabus-coverage transitions.
pub fn coverage_can_transition(from: SyllabusCoverageStatus, to: SyllabusCoverageStatus) -> bool {
    use SyllabusCoverageStatus::*;
    matches!(
        (from, to),
        (Draft, Submitted) | (Submitted, Draft) | (Submitted, Published) | (Draft, Published)
    )
}

/// Counting rules of an attendance policy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CountingRules {
    pub threshold_percent: f64,
    pub min_sessions: i32,
    pub late_counts_as_present: bool,
    pub exclude_excused: bool,
}

impl Default for CountingRules {
    fn default() -> Self {
        Self {
            threshold_percent: 75.0,
            min_sessions: 1,
            late_counts_as_present: true,
            exclude_excused: true,
        }
    }
}

/// Attendance of one student over finalised sessions.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tally {
    pub attended: i32,
    pub total: i32,
    pub percent: f64,
    pub below_threshold: bool,
}

/// Tallies a student's finalised marks under the policy rules.
pub fn tally(marks: &[AttendanceMark], rules: &CountingRules) -> Tally {
    let mut attended = 0;
    let mut total = 0;
    for m in marks {
        match m {
            AttendanceMark::Present => {
                attended += 1;
                total += 1;
            }
            AttendanceMark::Late => {
                if rules.late_counts_as_present {
                    attended += 1;
                }
                total += 1;
            }
            AttendanceMark::Absent => total += 1,
            AttendanceMark::Excused => {
                if !rules.exclude_excused {
                    total += 1;
                }
            }
        }
    }
    let percent = if total == 0 {
        100.0
    } else {
        (f64::from(attended) * 10000.0 / f64::from(total)).round() / 100.0
    };
    Tally {
        attended,
        total,
        percent,
        below_threshold: total >= rules.min_sessions && percent < rules.threshold_percent,
    }
}

/// A breach is raised only on the change from "not below" to "below" (one event per breach).
pub fn is_new_breach(previously_below: Option<bool>, now: &Tally) -> bool {
    now.below_threshold && previously_below != Some(true)
}

/// Duplicate students in one capture request (L4-D-024-BH-004).
pub fn duplicates<T: PartialEq + Copy>(ids: &[T]) -> Vec<T> {
    let mut out = Vec::new();
    for (i, a) in ids.iter().enumerate() {
        if ids[..i].contains(a) && !out.contains(a) {
            out.push(*a);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use AttendanceMark::*;
    use TeachingSessionStatus as S;

    #[test]
    fn session_lifecycle_has_one_terminal_outcome() {
        assert!(can_transition(S::Scheduled, S::Started));
        assert!(can_transition(S::Started, S::Paused));
        assert!(can_transition(S::Paused, S::InProgress));
        assert!(can_transition(S::InProgress, S::Completed));
        assert!(can_transition(S::Completed, S::Locked));
        assert!(
            !can_transition(S::Completed, S::Cancelled),
            "one terminal outcome"
        );
        assert!(!can_transition(S::Cancelled, S::Started));
        assert!(
            !can_transition(S::Scheduled, S::Completed),
            "a session must start first"
        );
        assert!(
            !can_transition(S::Paused, S::Completed),
            "resume before completing"
        );
        assert!(!can_transition(S::Locked, S::InProgress));
    }

    #[test]
    fn attendance_gates() {
        assert!(accepts_attendance(S::InProgress));
        assert!(!accepts_attendance(S::Scheduled));
        assert!(!accepts_attendance(S::Cancelled));
        assert!(capture_open(AttendanceCaptureStatus::Submitted));
        assert!(!capture_open(AttendanceCaptureStatus::Finalised));
        assert!(can_finalise(
            S::Completed,
            AttendanceCaptureStatus::Capturing
        ));
        assert!(!can_finalise(
            S::Completed,
            AttendanceCaptureStatus::NotStarted
        ));
        assert!(!can_finalise(
            S::Cancelled,
            AttendanceCaptureStatus::Submitted
        ));
    }

    #[test]
    fn tally_follows_the_policy() {
        let r = CountingRules {
            threshold_percent: 75.0,
            min_sessions: 3,
            ..Default::default()
        };
        let t = tally(&[Present, Late, Absent, Excused], &r);
        assert_eq!((t.attended, t.total), (2, 3));
        assert!((t.percent - 66.67).abs() < 0.01);
        assert!(t.below_threshold);
        let strict = CountingRules {
            late_counts_as_present: false,
            exclude_excused: false,
            ..r
        };
        let t = tally(&[Present, Late, Absent, Excused], &strict);
        assert_eq!((t.attended, t.total), (1, 4));
        // not evaluated before min_sessions
        let t = tally(&[Absent, Absent], &r);
        assert!(!t.below_threshold);
        assert_eq!(tally(&[], &r).percent, 100.0);
    }

    #[test]
    fn breaches_are_raised_once() {
        let below = Tally {
            attended: 1,
            total: 4,
            percent: 25.0,
            below_threshold: true,
        };
        let fine = Tally {
            below_threshold: false,
            ..below
        };
        assert!(is_new_breach(None, &below));
        assert!(is_new_breach(Some(false), &below));
        assert!(
            !is_new_breach(Some(true), &below),
            "same breach is not raised twice"
        );
        assert!(!is_new_breach(None, &fine));
    }

    #[test]
    fn policy_and_coverage_machines() {
        use PolicyVersionStatus as P;
        assert!(policy_can_transition(P::Draft, P::InReview));
        assert!(policy_can_transition(P::InReview, P::Effective));
        assert!(policy_can_transition(P::Effective, P::Superseded));
        assert!(
            !policy_can_transition(P::Draft, P::Effective),
            "approval first"
        );
        assert!(!policy_can_transition(P::Superseded, P::Effective));
        use SyllabusCoverageStatus as C;
        assert!(coverage_can_transition(C::Submitted, C::Published));
        assert!(
            !coverage_can_transition(C::Published, C::Draft),
            "published is immutable"
        );
        assert_eq!(duplicates(&[1, 2, 1, 3, 2, 1]), vec![1, 2]);
    }
}
