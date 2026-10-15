//! Accreditation state machines (L2-H09) and the pure decision rules of every record kind
//! (L3-H-061…071). Handlers gather the facts (items, citations, coverage of the framework
//! criteria, obligations, metric figures) and [`record_problems`] decides whether a proposed
//! outcome may go to its decision.

use crate::dashboards::domain::services_enum;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Accrediting bodies a framework can belong to.
pub const BODIES: &[&str] = &["NAAC", "NBA", "NIRF", "OTHER"];

services_enum!(
    /// Framework version (H-061).
    FrameworkStatus { Draft => "draft", Active => "active", Superseded => "superseded" }
);
services_enum!(
    /// Canonical approval state (contract §6.4).
    AccreditationApprovalState {
        NotRequested => "not_requested", Pending => "pending", Approved => "approved", Rejected => "rejected",
    }
);
services_enum!(
    /// Accreditation cycle (H-062, H-071).
    CycleStatus { Planning => "planning", Active => "active", Submitted => "submitted", Closed => "closed" }
);
services_enum!(
    /// Record kinds (H-063…071).
    RecordKind {
        Obligation => "obligation", Evidence => "evidence", InternalAudit => "internal_audit",
        ExternalAssessment => "external_assessment", Finding => "finding",
        Improvement => "improvement", Readiness => "readiness",
        ComplianceReview => "compliance_review", SelfStudy => "self_study",
    }
);
services_enum!(
    /// Record lifecycle: draft → open → pending_decision → closed (rejection → open); cancelled.
    RecordStatus {
        Draft => "draft", Open => "open", PendingDecision => "pending_decision",
        Closed => "closed", Cancelled => "cancelled",
    }
);
services_enum!(
    /// Severity of a finding / record.
    RecordSeverity { Low => "low", Medium => "medium", High => "high", Critical => "critical" }
);
services_enum!(
    /// Child item of a record.
    RecordItemKind { Requirement => "requirement", Checklist => "checklist", Action => "action" }
);
services_enum!(
    /// Item lifecycle: open → in_progress → done → verified; cancelled.
    RecordItemStatus {
        Open => "open", InProgress => "in_progress", Done => "done", Verified => "verified",
        Cancelled => "cancelled",
    }
);

impl RecordStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use RecordStatus::*;
        matches!(
            (self, to),
            (Draft, Open)
                | (Draft, Cancelled)
                | (Open, PendingDecision)
                | (Open, Cancelled)
                | (PendingDecision, Closed)
                | (PendingDecision, Open)
        )
    }
}

impl RecordItemStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use RecordItemStatus::*;
        matches!(
            (self, to),
            (Open, InProgress)
                | (Open, Done)
                | (Open, Cancelled)
                | (InProgress, Done)
                | (InProgress, Cancelled)
                | (Done, Verified)
                | (Done, InProgress)
        )
    }

    pub fn finished(self, verified_only: bool) -> bool {
        match self {
            RecordItemStatus::Verified | RecordItemStatus::Cancelled => true,
            RecordItemStatus::Done => !verified_only,
            _ => false,
        }
    }
}

impl RecordKind {
    /// Outcomes a record of this kind may close with.
    pub fn outcomes(self) -> &'static [&'static str] {
        use RecordKind::*;
        match self {
            Obligation => &["compliant", "non_compliant", "not_applicable"],
            Evidence => &["approved_evidence", "insufficient"],
            InternalAudit => &["closed_no_findings", "closed_with_findings"],
            ExternalAssessment => &["accepted", "appealed"],
            Finding => &["capa_verified", "no_action"],
            Improvement => &["effective", "not_effective"],
            Readiness => &["ready", "not_ready"],
            ComplianceReview => &["compliant", "partially_compliant", "non_compliant"],
            SelfStudy => &["approved"],
        }
    }

    /// Item kinds a record of this kind accepts.
    pub fn item_kinds(self) -> &'static [RecordItemKind] {
        use RecordKind::*;
        match self {
            Obligation => &[RecordItemKind::Requirement],
            InternalAudit => &[RecordItemKind::Checklist],
            Finding | Improvement => &[RecordItemKind::Action],
            _ => &[],
        }
    }

    /// Kinds that belong to an accreditation cycle.
    pub fn needs_cycle(self) -> bool {
        matches!(
            self,
            RecordKind::Evidence
                | RecordKind::InternalAudit
                | RecordKind::ExternalAssessment
                | RecordKind::Readiness
                | RecordKind::SelfStudy
        )
    }
}

/// One criterion of a framework version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AccreditationCriterion {
    /// `^[A-Z0-9][A-Z0-9_.-]{0,15}$` (`C1`, `C2.1` …).
    pub code: String,
    pub title: String,
    /// Weight in percent; the weights of a framework add up to 100.
    pub weight: i32,
    /// Operational figures (`<domain>.<key>`) that support the criterion.
    #[serde(default)]
    pub metric_keys: Vec<String>,
}

/// One entry of a cycle's assessment plan (H-062).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlanEntry {
    pub criterion_code: String,
    pub owner: String,
    #[schema(example = "2027-01-31")]
    pub due_on: String,
}

/// `true` for a criterion code.
pub fn valid_criterion_code(c: &str) -> bool {
    let b = c.as_bytes();
    (1..=16).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|x| {
            x.is_ascii_uppercase() || x.is_ascii_digit() || matches!(x, b'_' | b'.' | b'-')
        })
}

/// Problems of a framework's criteria list (empty = ok). `known` says whether a metric key is
/// reported by its owning domain.
pub fn criteria_problems(
    criteria: &[AccreditationCriterion],
    known: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let mut out = Vec::new();
    if criteria.is_empty() {
        out.push("a framework needs at least one criterion".into());
        return out;
    }
    let mut seen = std::collections::BTreeSet::new();
    for c in criteria {
        if !valid_criterion_code(&c.code) {
            out.push(format!("criterion code `{}` is invalid", c.code));
        }
        if !seen.insert(c.code.as_str()) {
            out.push(format!("criterion `{}` appears twice", c.code));
        }
        if c.title.trim().is_empty() {
            out.push(format!("criterion `{}` needs a title", c.code));
        }
        if !(1..=100).contains(&c.weight) {
            out.push(format!("criterion `{}` weight must be 1..100", c.code));
        }
        for k in &c.metric_keys {
            if !known(k) {
                out.push(format!("figure `{k}` is not reported by its owning domain"));
            }
        }
    }
    let total: i32 = criteria.iter().map(|c| c.weight).sum();
    if total != 100 {
        out.push(format!("criterion weights add up to {total}, not 100"));
    }
    out
}

/// Problems of a cycle's assessment plan against the framework criteria (every criterion
/// exactly once, an owner each).
pub fn plan_problems(plan: &[PlanEntry], criteria: &[AccreditationCriterion]) -> Vec<String> {
    let mut out = Vec::new();
    for c in criteria {
        let n = plan.iter().filter(|p| p.criterion_code == c.code).count();
        if n != 1 {
            out.push(format!(
                "criterion `{}` must be planned exactly once ({n})",
                c.code
            ));
        }
    }
    for p in plan {
        if !criteria.iter().any(|c| c.code == p.criterion_code) {
            out.push(format!(
                "`{}` is not a criterion of the framework",
                p.criterion_code
            ));
        }
        if p.owner.trim().is_empty() {
            out.push(format!("criterion `{}` needs an owner", p.criterion_code));
        }
    }
    out
}

/// One item as the decision rules see it.
#[derive(Debug, Clone, Copy)]
pub struct ItemFact {
    pub kind: RecordItemKind,
    pub status: RecordItemStatus,
}

/// Improvement measurement (H-068).
#[derive(Debug, Clone, Copy)]
pub struct Measurement {
    pub baseline: Decimal,
    pub current: Decimal,
    pub higher_is_better: bool,
}

impl Measurement {
    pub fn improved(&self) -> bool {
        if self.higher_is_better {
            self.current > self.baseline
        } else {
            self.current < self.baseline
        }
    }
}

/// Everything the decision rules of one record need.
#[derive(Debug, Clone)]
pub struct RecordFacts<'a> {
    pub kind: RecordKind,
    pub severity: RecordSeverity,
    pub outcome: &'a str,
    pub rationale: Option<&'a str>,
    pub items: &'a [ItemFact],
    /// Evidence: regulatory snapshots + governance evidence + source URL cited.
    pub citations: usize,
    /// Internal audit: findings raised under it.
    pub child_findings: i64,
    /// External assessment: grade / score awarded.
    pub grade: Option<&'a str>,
    pub measurement: Option<Measurement>,
    /// Readiness: criteria without approved evidence.
    pub coverage_missing: &'a [String],
    /// Readiness: open high / critical findings of the cycle.
    pub open_major_findings: i64,
    /// Compliance review: status computed over the obligations.
    pub computed_compliance: Option<&'a str>,
    /// Self-study: the cycle's readiness decision is `ready`.
    pub cycle_ready: bool,
    /// Self-study: approved evidence records of the cycle.
    pub approved_evidence: i64,
}

/// Compliance status over obligations (H-070): `(compliant, non_compliant, overdue open)`.
pub fn compliance_status(compliant: i64, non_compliant: i64, overdue_open: i64) -> &'static str {
    if non_compliant == 0 && overdue_open == 0 {
        "compliant"
    } else if compliant == 0 {
        "non_compliant"
    } else {
        "partially_compliant"
    }
}

/// Reasons a record may not go to its decision with the proposed outcome (empty = ok).
pub fn record_problems(f: &RecordFacts<'_>) -> Vec<String> {
    use RecordKind::*;
    let mut out = Vec::new();
    if !f.kind.outcomes().contains(&f.outcome) {
        out.push(format!(
            "outcome `{}` is not one of {}",
            f.outcome,
            f.kind.outcomes().join(", ")
        ));
        return out;
    }
    let has_rationale = f.rationale.is_some_and(|r| !r.trim().is_empty());
    let live = |k: RecordItemKind| {
        f.items
            .iter()
            .filter(|i| i.kind == k && i.status != RecordItemStatus::Cancelled)
            .count()
    };
    let all_finished = |k: RecordItemKind, verified: bool| {
        f.items
            .iter()
            .filter(|i| i.kind == k)
            .all(|i| i.status.finished(verified))
    };
    match f.kind {
        Obligation => {
            if live(RecordItemKind::Requirement) == 0 {
                out.push("an obligation needs at least one requirement".into());
            }
            match f.outcome {
                "compliant" if !all_finished(RecordItemKind::Requirement, true) => {
                    out.push("every requirement must be verified to be compliant".into());
                }
                "not_applicable" | "non_compliant" if !has_rationale => {
                    out.push(format!("`{}` needs a rationale", f.outcome));
                }
                _ => {}
            }
        }
        Evidence => match f.outcome {
            "approved_evidence" if f.citations == 0 => {
                out.push("approved evidence cites at least one source".into());
            }
            "insufficient" if !has_rationale => {
                out.push("`insufficient` needs a rationale".into());
            }
            _ => {}
        },
        InternalAudit => {
            if live(RecordItemKind::Checklist) == 0 {
                out.push("an internal audit needs a checklist".into());
            }
            if !all_finished(RecordItemKind::Checklist, false) {
                out.push("every checklist item must be done".into());
            }
            match f.outcome {
                "closed_with_findings" if f.child_findings == 0 => {
                    out.push("no finding was raised under this audit".into());
                }
                "closed_no_findings" if f.child_findings > 0 => {
                    out.push(format!("{} finding(s) were raised", f.child_findings));
                }
                _ => {}
            }
        }
        ExternalAssessment => {
            if !f.grade.is_some_and(|g| !g.trim().is_empty()) {
                out.push("the awarded grade / score is required".into());
            }
            if f.outcome == "appealed" && !has_rationale {
                out.push("an appeal needs a rationale".into());
            }
        }
        Finding => match f.outcome {
            "capa_verified" => {
                if live(RecordItemKind::Action) == 0 || !all_finished(RecordItemKind::Action, true)
                {
                    out.push("CAPA actions must exist and be verified".into());
                }
            }
            _ => {
                if f.severity != RecordSeverity::Low {
                    out.push("only a low finding closes without CAPA".into());
                }
                if !has_rationale {
                    out.push("`no_action` needs a rationale".into());
                }
            }
        },
        Improvement => {
            if live(RecordItemKind::Action) == 0 || !all_finished(RecordItemKind::Action, false) {
                out.push("improvement actions must exist and be done".into());
            }
            match f.measurement {
                Some(m) => {
                    let expected = if m.improved() {
                        "effective"
                    } else {
                        "not_effective"
                    };
                    if f.outcome != expected {
                        out.push(format!(
                            "the figure moved from {} to {}: the outcome is {expected}",
                            m.baseline, m.current
                        ));
                    }
                }
                None => out.push("the improvement figure could not be measured".into()),
            }
        }
        Readiness => {
            if f.outcome == "ready" {
                if !f.coverage_missing.is_empty() {
                    out.push(format!(
                        "criteria without approved evidence: {}",
                        f.coverage_missing.join(", ")
                    ));
                }
                if f.open_major_findings > 0 {
                    out.push(format!(
                        "{} high / critical finding(s) are still open",
                        f.open_major_findings
                    ));
                }
            } else if !has_rationale {
                out.push("`not_ready` needs a rationale".into());
            }
        }
        ComplianceReview => match f.computed_compliance {
            Some(c) if c == f.outcome => {}
            Some(c) => out.push(format!("the obligations give {c}")),
            None => out.push("there are no obligations to review".into()),
        },
        SelfStudy => {
            if !f.cycle_ready {
                out.push("the cycle has no `ready` readiness decision".into());
            }
            if f.approved_evidence == 0 {
                out.push("the report needs approved evidence".into());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts<'a>(kind: RecordKind, outcome: &'a str, items: &'a [ItemFact]) -> RecordFacts<'a> {
        RecordFacts {
            kind,
            severity: RecordSeverity::Medium,
            outcome,
            rationale: None,
            items,
            citations: 0,
            child_findings: 0,
            grade: None,
            measurement: None,
            coverage_missing: &[],
            open_major_findings: 0,
            computed_compliance: None,
            cycle_ready: false,
            approved_evidence: 0,
        }
    }

    #[test]
    fn machines_and_codes() {
        assert!(RecordStatus::PendingDecision.can_transition(RecordStatus::Open));
        assert!(!RecordStatus::Closed.can_transition(RecordStatus::Open));
        assert!(RecordItemStatus::Done.can_transition(RecordItemStatus::Verified));
        assert!(valid_criterion_code("C2.1") && !valid_criterion_code("c1"));
        for k in RecordKind::ALL {
            assert!(!k.outcomes().is_empty());
        }
        assert_eq!(compliance_status(3, 0, 0), "compliant");
        assert_eq!(compliance_status(0, 2, 0), "non_compliant");
        assert_eq!(compliance_status(2, 0, 1), "partially_compliant");
    }

    #[test]
    fn criteria_and_plans() {
        let known = |k: &str| k == "student.active_students";
        let c = vec![
            AccreditationCriterion {
                code: "C1".into(),
                title: "Curricular aspects".into(),
                weight: 60,
                metric_keys: vec!["student.active_students".into()],
            },
            AccreditationCriterion {
                code: "C2".into(),
                title: "Teaching".into(),
                weight: 40,
                metric_keys: vec![],
            },
        ];
        assert!(criteria_problems(&c, &known).is_empty());
        let mut bad = c.clone();
        bad[1].weight = 30;
        bad[1].metric_keys = vec!["people.nope".into()];
        assert_eq!(criteria_problems(&bad, &known).len(), 2);
        let plan = vec![PlanEntry {
            criterion_code: "C1".into(),
            owner: "IQAC".into(),
            due_on: "2027-01-31".into(),
        }];
        assert_eq!(plan_problems(&plan, &c).len(), 1);
    }

    #[test]
    fn rules() {
        let verified = [ItemFact {
            kind: RecordItemKind::Action,
            status: RecordItemStatus::Verified,
        }];
        let done = [ItemFact {
            kind: RecordItemKind::Action,
            status: RecordItemStatus::Done,
        }];
        assert!(
            record_problems(&facts(RecordKind::Finding, "capa_verified", &verified)).is_empty()
        );
        assert!(!record_problems(&facts(RecordKind::Finding, "capa_verified", &done)).is_empty());
        let mut f = facts(RecordKind::Evidence, "approved_evidence", &[]);
        assert!(!record_problems(&f).is_empty());
        f.citations = 2;
        assert!(record_problems(&f).is_empty());
        let missing = vec!["C2".to_owned()];
        let mut f = facts(RecordKind::Readiness, "ready", &[]);
        f.coverage_missing = &missing;
        assert!(!record_problems(&f).is_empty());
        let mut f = facts(RecordKind::Improvement, "effective", &done);
        f.measurement = Some(Measurement {
            baseline: Decimal::from(5),
            current: Decimal::from(2),
            higher_is_better: false,
        });
        assert!(record_problems(&f).is_empty());
        let mut f = facts(RecordKind::ComplianceReview, "compliant", &[]);
        f.computed_compliance = Some("partially_compliant");
        assert!(!record_problems(&f).is_empty());
        let f = facts(RecordKind::SelfStudy, "approved", &[]);
        assert_eq!(record_problems(&f).len(), 2);
        assert!(!record_problems(&facts(RecordKind::Obligation, "x", &[]))[0].is_empty());
    }
}
