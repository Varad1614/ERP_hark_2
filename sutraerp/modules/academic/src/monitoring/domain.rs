//! Pure academic-monitoring rules (L2-D05 + purity amendment, L3-D-027…033). No I/O.

use sutra_contracts::academic::status::{InterventionStatus, MonitoringCaseStatus, RiskLevel};

/// Legal monitoring-case transitions. A closed case never re-opens in place: reopening creates
/// a new case whose first status is `reopened` (L4-D-033-BH-005).
pub fn can_transition(from: MonitoringCaseStatus, to: MonitoringCaseStatus) -> bool {
    use MonitoringCaseStatus::*;
    matches!(
        (from, to),
        (Open, UnderAssessment)
            | (Open, UnderReview)
            | (Reopened, UnderAssessment)
            | (Reopened, UnderReview)
            | (UnderAssessment, UnderReview)
            | (UnderAssessment, Certified)
            | (UnderReview, Intervention)
            | (UnderReview, UnderAssessment)
            | (Intervention, UnderReview)
            | (Intervention, UnderAssessment)
            | (Intervention, Certified)
            | (Certified, Closed)
    )
}

/// Legal intervention transitions.
pub fn intervention_can_transition(from: InterventionStatus, to: InterventionStatus) -> bool {
    use InterventionStatus::*;
    matches!(
        (from, to),
        (Planned, Triggered)
            | (Triggered, InProgress)
            | (InProgress, Completed)
            | (Planned, Cancelled)
            | (Triggered, Cancelled)
            | (InProgress, Cancelled)
    )
}

/// An intervention still blocks closure.
pub fn intervention_open(s: InterventionStatus) -> bool {
    matches!(
        s,
        InterventionStatus::Planned
            | InterventionStatus::Triggered
            | InterventionStatus::InProgress
    )
}

/// Governance-exception states (`monitoring_exception.status`).
pub const EXCEPTION_STATES: &[&str] = &[
    "raised",
    "planned",
    "resolved",
    "closed",
    "escalated",
    "withdrawn",
];

/// Legal governance-exception transitions (L3-D-029).
pub fn exception_can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("raised", "planned")
            | ("escalated", "planned")
            | ("planned", "resolved")
            | ("resolved", "planned")
            | ("resolved", "closed")
            | ("raised", "escalated")
            | ("planned", "escalated")
            | ("raised", "withdrawn")
            | ("planned", "withdrawn")
            | ("escalated", "withdrawn")
    )
}

/// An exception still blocks closure.
pub fn exception_open(s: &str) -> bool {
    !matches!(s, "closed" | "withdrawn")
}

/// Priority of a risk level (1 = most urgent).
pub fn priority(level: RiskLevel) -> i16 {
    match level {
        RiskLevel::Critical => 1,
        RiskLevel::High => 2,
        RiskLevel::Moderate => 3,
        RiskLevel::Low => 4,
    }
}

/// Risk of one student's attendance against the threshold (approved D05 attendance rule).
pub fn student_attendance_risk(percent: f64, threshold: f64) -> RiskLevel {
    if percent >= threshold {
        RiskLevel::Low
    } else if percent >= threshold - 10.0 {
        RiskLevel::Moderate
    } else if percent >= threshold - 25.0 {
        RiskLevel::High
    } else {
        RiskLevel::Critical
    }
}

/// Risk of a section by the share of its students below the threshold.
pub fn section_attendance_risk(below: usize, total: usize) -> RiskLevel {
    if total == 0 || below == 0 {
        return RiskLevel::Low;
    }
    let share = below as f64 / total as f64;
    if share < 0.10 {
        RiskLevel::Moderate
    } else if share < 0.25 {
        RiskLevel::High
    } else {
        RiskLevel::Critical
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use MonitoringCaseStatus::*;

    #[test]
    fn case_lifecycle() {
        assert!(can_transition(Open, UnderAssessment));
        assert!(can_transition(UnderAssessment, UnderReview));
        assert!(can_transition(UnderReview, Intervention));
        assert!(can_transition(Intervention, Certified));
        assert!(can_transition(Certified, Closed));
        assert!(
            !can_transition(Closed, Reopened),
            "reopening creates a new case"
        );
        assert!(!can_transition(Closed, UnderAssessment));
        assert!(!can_transition(Open, Closed), "certification first");
        assert!(can_transition(Reopened, UnderAssessment));
    }

    #[test]
    fn interventions_and_exceptions() {
        use InterventionStatus as I;
        assert!(intervention_can_transition(I::Planned, I::Triggered));
        assert!(
            !intervention_can_transition(I::Planned, I::InProgress),
            "trigger first"
        );
        assert!(!intervention_can_transition(I::Completed, I::Cancelled));
        assert!(intervention_open(I::Triggered) && !intervention_open(I::Completed));
        assert!(exception_can_transition("raised", "planned"));
        assert!(exception_can_transition("resolved", "closed"));
        assert!(
            !exception_can_transition("raised", "closed"),
            "validation needs a resolution"
        );
        assert!(exception_open("escalated") && !exception_open("withdrawn"));
        assert!(EXCEPTION_STATES.iter().all(|s| !s.is_empty()));
    }

    #[test]
    fn risk_rules() {
        assert_eq!(student_attendance_risk(80.0, 75.0), RiskLevel::Low);
        assert_eq!(student_attendance_risk(70.0, 75.0), RiskLevel::Moderate);
        assert_eq!(student_attendance_risk(55.0, 75.0), RiskLevel::High);
        assert_eq!(student_attendance_risk(20.0, 75.0), RiskLevel::Critical);
        assert_eq!(section_attendance_risk(0, 10), RiskLevel::Low);
        assert_eq!(section_attendance_risk(1, 20), RiskLevel::Moderate);
        assert_eq!(section_attendance_risk(2, 10), RiskLevel::High);
        assert_eq!(section_attendance_risk(1, 2), RiskLevel::Critical);
        assert_eq!(priority(RiskLevel::Critical), 1);
        assert_eq!(priority(RiskLevel::Low), 4);
    }
}
