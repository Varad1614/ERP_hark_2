//! Governance state machines and the pure closure rules of every G00 case kind
//! (L3-G-001…015). The handlers gather the facts (items, SLA tickets still open, KPI snapshot,
//! effective configuration) and [`submit_problems`] decides whether a proposed outcome may go to
//! its decision.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Declares a 9E governance enum with `ALL`, `as_str`, `parse` and `snake_case` serde names.
macro_rules! governance_enum {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        #[derive(
            Clone, Copy, Debug, PartialEq, Eq, Hash,
            serde::Serialize, serde::Deserialize, utoipa::ToSchema,
        )]
        #[serde(rename_all = "snake_case")]
        $(#[$meta])*
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

governance_enum!(
    /// What a governance case is (one per G00 scenario family).
    GovernanceKind {
        ReadinessAssessment => "readiness_assessment",
        HealthAssessment => "health_assessment",
        MaturityAssessment => "maturity_assessment",
        OperationalReview => "operational_review",
        ExecutiveReview => "executive_review",
        Risk => "risk",
        ActionPlan => "action_plan",
        SlaDeviation => "sla_deviation",
        KpiReview => "kpi_review",
        Finding => "finding",
        Escalation => "escalation",
        Exception => "exception",
    }
);
governance_enum!(
    /// Case lifecycle: draft → open → pending_decision → closed (rejection → open); cancelled.
    GovernanceStatus {
        Draft => "draft",
        Open => "open",
        PendingDecision => "pending_decision",
        Closed => "closed",
        Cancelled => "cancelled",
    }
);
governance_enum!(
    /// Canonical approval state (contract §6.4).
    ApprovalState {
        NotRequested => "not_requested",
        Pending => "pending",
        Approved => "approved",
        Rejected => "rejected",
    }
);
governance_enum!(
    /// Severity of a case.
    #[schema(as = GovernanceSeverity)]
    Severity { Low => "low", Medium => "medium", High => "high", Critical => "critical" }
);
governance_enum!(
    /// Child item of a case.
    ItemKind {
        Criterion => "criterion",
        Recommendation => "recommendation",
        Action => "action",
        CorrectiveAction => "corrective_action",
    }
);
governance_enum!(
    /// Item lifecycle: open → in_progress → done → verified; cancelled.
    ItemStatus {
        Open => "open",
        InProgress => "in_progress",
        Done => "done",
        Verified => "verified",
        Cancelled => "cancelled",
    }
);
governance_enum!(
    /// Exception type (G-013).
    ExceptionType { Evidence => "evidence", Dependency => "dependency" }
);
governance_enum!(
    /// Configuration key (G-014).
    ConfigKey {
        ReadinessThreshold => "readiness_threshold",
        RiskAppetite => "risk_appetite",
        ReviewCadence => "review_cadence",
        KpiThresholds => "kpi_thresholds",
    }
);
governance_enum!(
    /// Configuration version lifecycle.
    ConfigStatus { Draft => "draft", Effective => "effective", Superseded => "superseded", Withdrawn => "withdrawn" }
);
governance_enum!(
    /// Governance evidence lifecycle.
    #[schema(as = GovernanceEvidenceStatus)]
    EvidenceStatus { Draft => "draft", Published => "published" }
);

impl GovernanceStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use GovernanceStatus::*;
        matches!(
            (self, to),
            (Draft, Open)
                | (Draft, Cancelled)
                | (Open, PendingDecision)
                | (Open, Closed)
                | (Open, Cancelled)
                | (PendingDecision, Closed)
                | (PendingDecision, Open)
        )
    }
}

impl ItemStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use ItemStatus::*;
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

    /// Finished for a closure check (`verified_only` = verified closure).
    pub fn finished(self, verified_only: bool) -> bool {
        match self {
            ItemStatus::Verified | ItemStatus::Cancelled => true,
            ItemStatus::Done => !verified_only,
            _ => false,
        }
    }
}

impl GovernanceKind {
    /// Outcomes a case of this kind may close with.
    pub fn outcomes(self) -> &'static [&'static str] {
        use GovernanceKind::*;
        match self {
            ReadinessAssessment => &["ready", "conditionally_ready", "not_ready"],
            HealthAssessment => &["healthy", "at_risk", "critical"],
            MaturityAssessment => &["level_1", "level_2", "level_3", "level_4", "level_5"],
            OperationalReview => &["closed_satisfactory", "closed_with_actions"],
            ExecutiveReview => &["recommendations_accepted", "recommendations_deferred"],
            Risk => &["mitigated", "accepted", "transferred", "avoided"],
            ActionPlan => &["completed"],
            SlaDeviation => &["resolved", "waived"],
            KpiReview => &["on_track", "off_track"],
            Finding => &["corrective_action_approved", "no_action"],
            Escalation => &["resolved", "referred_back"],
            Exception => &["granted", "denied"],
        }
    }

    /// Item kinds a case of this kind accepts.
    pub fn item_kinds(self) -> &'static [ItemKind] {
        use GovernanceKind::*;
        match self {
            ReadinessAssessment | HealthAssessment | MaturityAssessment => {
                &[ItemKind::Criterion, ItemKind::Action]
            }
            ExecutiveReview => &[ItemKind::Recommendation, ItemKind::Action],
            Finding => &[ItemKind::CorrectiveAction],
            _ => &[ItemKind::Action],
        }
    }

    pub fn is_assessment(self) -> bool {
        matches!(
            self,
            GovernanceKind::ReadinessAssessment
                | GovernanceKind::HealthAssessment
                | GovernanceKind::MaturityAssessment
        )
    }
}

/// Effective governance policy (G-014 configuration, defaults when no version is effective).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct GovernancePolicy {
    /// Lowest criterion rating (1..5) for `ready` / `healthy` (`readiness_threshold`).
    pub min_criterion_score: i32,
    /// Highest residual score (1..25) a `mitigated` risk may close with (`risk_appetite`).
    pub max_residual_score: i32,
}

impl Default for GovernancePolicy {
    fn default() -> Self {
        Self {
            min_criterion_score: 3,
            max_residual_score: 6,
        }
    }
}

/// Validates a configuration version's settings; returns a message when invalid.
pub fn config_problem(key: ConfigKey, settings: &serde_json::Value) -> Option<String> {
    let Some(obj) = settings.as_object() else {
        return Some("settings must be a JSON object".into());
    };
    let int = |k: &str, lo: i64, hi: i64| -> Option<String> {
        match obj.get(k).and_then(serde_json::Value::as_i64) {
            Some(v) if (lo..=hi).contains(&v) => None,
            _ => Some(format!("{k} must be a whole number {lo}..{hi}")),
        }
    };
    match key {
        ConfigKey::ReadinessThreshold => int("minCriterionScore", 1, 5),
        ConfigKey::RiskAppetite => int("maxResidualScore", 1, 25),
        ConfigKey::ReviewCadence => {
            int("operationalDays", 7, 366).or_else(|| int("executiveDays", 30, 366))
        }
        ConfigKey::KpiThresholds => obj
            .iter()
            .find(|(_, v)| !v.is_number())
            .map(|(k, _)| format!("{k} must be a number")),
    }
}

/// Settings must be a JSON object.
pub fn config_is_object(settings: &serde_json::Value) -> bool {
    settings.is_object()
}

/// Applies one effective configuration version to the policy.
pub fn apply_config(p: &mut GovernancePolicy, key: ConfigKey, settings: &serde_json::Value) {
    let get = |k: &str| {
        settings
            .get(k)
            .and_then(serde_json::Value::as_i64)
            .map(|v| v as i32)
    };
    match key {
        ConfigKey::ReadinessThreshold => {
            if let Some(v) = get("minCriterionScore") {
                p.min_criterion_score = v;
            }
        }
        ConfigKey::RiskAppetite => {
            if let Some(v) = get("maxResidualScore") {
                p.max_residual_score = v;
            }
        }
        ConfigKey::ReviewCadence | ConfigKey::KpiThresholds => {}
    }
}

/// One item as the closure rules see it.
#[derive(Debug, Clone)]
pub struct ItemFact {
    pub kind: ItemKind,
    pub status: ItemStatus,
    pub score: Option<i32>,
    pub domain: Option<String>,
}

/// Everything the closure rules of one case need.
#[derive(Debug, Clone)]
pub struct CaseFacts<'a> {
    pub kind: GovernanceKind,
    pub severity: Severity,
    pub domains: &'a [String],
    pub items: &'a [ItemFact],
    pub residual_score: Option<i32>,
    pub outcome: &'a str,
    pub rationale: Option<&'a str>,
    pub expires_on: Option<NaiveDate>,
    pub today: NaiveDate,
    /// SLA deviation: snapshotted breached tickets that are still open.
    pub sla_still_open: i64,
    /// KPI review: at least one figure misses its target.
    pub kpi_off_track: bool,
}

/// Average criterion rating (`None` without criteria).
pub fn average_score(items: &[ItemFact]) -> Option<f64> {
    let scores: Vec<i32> = items
        .iter()
        .filter(|i| i.kind == ItemKind::Criterion && i.status != ItemStatus::Cancelled)
        .filter_map(|i| i.score)
        .collect();
    if scores.is_empty() {
        None
    } else {
        Some(f64::from(scores.iter().sum::<i32>()) / scores.len() as f64)
    }
}

/// Maturity level `level_1`…`level_5` from the average rating (floor).
pub fn maturity_level(avg: f64) -> &'static str {
    match avg.floor() as i64 {
        i64::MIN..=1 => "level_1",
        2 => "level_2",
        3 => "level_3",
        4 => "level_4",
        _ => "level_5",
    }
}

/// Reasons a case may not go to its decision with the proposed outcome (empty = ok).
pub fn submit_problems(c: &CaseFacts<'_>, p: &GovernancePolicy) -> Vec<String> {
    use GovernanceKind::*;
    let mut out = Vec::new();
    if !c.kind.outcomes().contains(&c.outcome) {
        out.push(format!(
            "outcome `{}` is not one of {}",
            c.outcome,
            c.kind.outcomes().join(", ")
        ));
        return out;
    }
    let of = |k: ItemKind| c.items.iter().filter(move |i| i.kind == k);
    let has_rationale = c.rationale.is_some_and(|r| !r.trim().is_empty());
    let actions_finished = |verified: bool| {
        c.items
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::Action | ItemKind::CorrectiveAction))
            .all(|i| i.status.finished(verified))
    };
    let live_actions = c
        .items
        .iter()
        .filter(|i| {
            matches!(i.kind, ItemKind::Action | ItemKind::CorrectiveAction)
                && i.status != ItemStatus::Cancelled
        })
        .count();
    if c.kind.is_assessment() {
        let criteria: Vec<&ItemFact> = of(ItemKind::Criterion)
            .filter(|i| i.status != ItemStatus::Cancelled)
            .collect();
        if criteria.is_empty() {
            out.push("an assessment needs at least one criterion".into());
            return out;
        }
        if criteria.iter().any(|i| i.score.is_none()) {
            out.push("every criterion must be rated 1..5".into());
            return out;
        }
        let below = criteria
            .iter()
            .any(|i| i.score.unwrap_or(0) < p.min_criterion_score);
        let avg = average_score(c.items).unwrap_or(0.0);
        match (c.kind, c.outcome) {
            (ReadinessAssessment, "ready") | (HealthAssessment, "healthy") if below => {
                out.push(format!(
                    "`{}` needs every criterion rated at least {}",
                    c.outcome, p.min_criterion_score
                ));
            }
            (ReadinessAssessment, "conditionally_ready") if live_actions == 0 => {
                out.push("conditional readiness needs at least one action".into());
            }
            (MaturityAssessment, level) if level != maturity_level(avg) => {
                out.push(format!(
                    "the ratings give {}; the outcome must match",
                    maturity_level(avg)
                ));
            }
            _ => {}
        }
        return out;
    }
    match c.kind {
        OperationalReview => {
            if c.outcome == "closed_with_actions" && live_actions == 0 {
                out.push("closing with actions needs at least one action".into());
            }
            if !actions_finished(false) {
                out.push("every action must be done before the review closes".into());
            }
        }
        ExecutiveReview => {
            if of(ItemKind::Recommendation).count() == 0 {
                out.push("an executive review needs at least one recommendation".into());
            }
        }
        Risk => match c.outcome {
            "mitigated" => {
                match c.residual_score {
                    Some(r) if r <= p.max_residual_score => {}
                    Some(r) => out.push(format!(
                        "residual score {r} exceeds the risk appetite {}",
                        p.max_residual_score
                    )),
                    None => out.push("a mitigated risk needs its residual score".into()),
                }
                if live_actions == 0 || !actions_finished(true) {
                    out.push("mitigation actions must exist and be verified".into());
                }
            }
            _ => {
                if !has_rationale {
                    out.push(format!("`{}` needs a rationale", c.outcome));
                }
            }
        },
        ActionPlan => {
            let domains: std::collections::BTreeSet<&str> = c
                .items
                .iter()
                .filter(|i| i.status != ItemStatus::Cancelled)
                .filter_map(|i| i.domain.as_deref())
                .collect();
            if live_actions == 0 {
                out.push("an action plan needs at least one action".into());
            }
            if c.domains.len() > 1 && domains.len() < c.domains.len() {
                out.push("every domain of the plan needs at least one action".into());
            }
            if !actions_finished(true) {
                out.push("every action must be verified (verified closure)".into());
            }
        }
        SlaDeviation => {
            if c.outcome == "resolved" && c.sla_still_open > 0 {
                out.push(format!(
                    "{} breached ticket(s) are still open",
                    c.sla_still_open
                ));
            }
            if c.outcome == "waived" && !has_rationale {
                out.push("a waiver needs a rationale".into());
            }
        }
        KpiReview => {
            let expected = if c.kpi_off_track {
                "off_track"
            } else {
                "on_track"
            };
            if c.outcome != expected {
                out.push(format!("the KPI snapshot is {expected}"));
            }
            if c.kpi_off_track && live_actions == 0 {
                out.push("an off-track review needs at least one action".into());
            }
        }
        Finding => match c.outcome {
            "corrective_action_approved" => {
                if of(ItemKind::CorrectiveAction)
                    .filter(|i| i.status != ItemStatus::Cancelled)
                    .count()
                    == 0
                {
                    out.push("a corrective action is required".into());
                }
            }
            _ => {
                if matches!(c.severity, Severity::High | Severity::Critical) {
                    out.push("a high or critical finding needs a corrective action".into());
                }
                if !has_rationale {
                    out.push("`no_action` needs a rationale".into());
                }
            }
        },
        Escalation => {
            if !has_rationale {
                out.push("an executive resolution needs a rationale".into());
            }
        }
        Exception => {
            if !has_rationale {
                out.push("an exception decision needs a rationale".into());
            }
            if c.outcome == "granted" {
                match c.expires_on {
                    Some(d) if d >= c.today && d <= c.today + chrono::Duration::days(180) => {}
                    _ => out.push("a granted exception must expire within 180 days".into()),
                }
            }
        }
        ReadinessAssessment | HealthAssessment | MaturityAssessment => {}
    }
    out
}

/// Whether an item's closure (`verify`) is required by the case kind before closing.
pub fn verified_closure(kind: GovernanceKind) -> bool {
    matches!(kind, GovernanceKind::ActionPlan | GovernanceKind::Risk)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap_or_default()
    }

    fn item(kind: ItemKind, status: ItemStatus, score: Option<i32>) -> ItemFact {
        ItemFact {
            kind,
            status,
            score,
            domain: Some("facilities".into()),
        }
    }

    fn facts<'a>(
        kind: GovernanceKind,
        items: &'a [ItemFact],
        outcome: &'a str,
        domains: &'a [String],
    ) -> CaseFacts<'a> {
        CaseFacts {
            kind,
            severity: Severity::Medium,
            domains,
            items,
            residual_score: None,
            outcome,
            rationale: None,
            expires_on: None,
            today: d("2026-11-20"),
            sla_still_open: 0,
            kpi_off_track: false,
        }
    }

    #[test]
    fn machines() {
        use GovernanceStatus as S;
        assert!(S::Draft.can_transition(S::Open));
        assert!(S::PendingDecision.can_transition(S::Open), "rejection");
        assert!(!S::Closed.can_transition(S::Open));
        assert!(ItemStatus::Done.can_transition(ItemStatus::Verified));
        assert!(!ItemStatus::Open.can_transition(ItemStatus::Verified));
        assert!(ItemStatus::Done.finished(false) && !ItemStatus::Done.finished(true));
        for k in GovernanceKind::ALL {
            assert!(!k.outcomes().is_empty());
            assert_eq!(GovernanceKind::parse(k.as_str()), Some(*k));
        }
    }

    #[test]
    fn assessments() {
        let p = GovernancePolicy::default();
        let none: Vec<String> = vec![];
        let low = [
            item(ItemKind::Criterion, ItemStatus::Open, Some(4)),
            item(ItemKind::Criterion, ItemStatus::Open, Some(2)),
        ];
        let f = facts(GovernanceKind::ReadinessAssessment, &low, "ready", &none);
        assert_eq!(submit_problems(&f, &p).len(), 1);
        let f = facts(
            GovernanceKind::ReadinessAssessment,
            &low,
            "not_ready",
            &none,
        );
        assert!(submit_problems(&f, &p).is_empty());
        let f = facts(GovernanceKind::MaturityAssessment, &low, "level_3", &none);
        assert!(submit_problems(&f, &p).is_empty());
        let f = facts(GovernanceKind::MaturityAssessment, &low, "level_4", &none);
        assert!(!submit_problems(&f, &p).is_empty());
        let unrated = [item(ItemKind::Criterion, ItemStatus::Open, None)];
        let f = facts(GovernanceKind::HealthAssessment, &unrated, "healthy", &none);
        assert!(!submit_problems(&f, &p).is_empty());
        let f = facts(GovernanceKind::HealthAssessment, &low, "bogus", &none);
        assert!(submit_problems(&f, &p)[0].contains("not one of"));
        let mut lenient = p;
        apply_config(
            &mut lenient,
            ConfigKey::ReadinessThreshold,
            &serde_json::json!({"minCriterionScore": 2}),
        );
        let f = facts(GovernanceKind::ReadinessAssessment, &low, "ready", &none);
        assert!(submit_problems(&f, &lenient).is_empty());
    }

    #[test]
    fn risks_plans_and_findings() {
        let p = GovernancePolicy::default();
        let domains = vec!["facilities".to_owned(), "hostel".to_owned()];
        let done = [item(ItemKind::Action, ItemStatus::Done, None)];
        let mut f = facts(GovernanceKind::Risk, &done, "mitigated", &domains);
        f.residual_score = Some(4);
        assert_eq!(submit_problems(&f, &p).len(), 1, "actions not verified");
        let verified = [item(ItemKind::Action, ItemStatus::Verified, None)];
        let mut f = facts(GovernanceKind::Risk, &verified, "mitigated", &domains);
        f.residual_score = Some(9);
        assert_eq!(submit_problems(&f, &p).len(), 1, "above appetite");
        f.residual_score = Some(6);
        assert!(submit_problems(&f, &p).is_empty());
        let f = facts(GovernanceKind::ActionPlan, &verified, "completed", &domains);
        assert_eq!(submit_problems(&f, &p).len(), 1, "hostel has no action");
        let mut f = facts(GovernanceKind::Finding, &[], "no_action", &domains);
        f.severity = Severity::High;
        f.rationale = Some("x");
        assert_eq!(submit_problems(&f, &p).len(), 1);
        let mut f = facts(GovernanceKind::Exception, &[], "granted", &domains);
        f.rationale = Some("vendor delay");
        f.expires_on = Some(d("2027-01-10"));
        assert!(submit_problems(&f, &p).is_empty());
        f.expires_on = Some(d("2027-12-31"));
        assert!(!submit_problems(&f, &p).is_empty());
        let mut f = facts(GovernanceKind::SlaDeviation, &[], "resolved", &domains);
        f.sla_still_open = 2;
        assert!(!submit_problems(&f, &p).is_empty());
        let mut f = facts(GovernanceKind::KpiReview, &[], "on_track", &domains);
        f.kpi_off_track = true;
        assert_eq!(submit_problems(&f, &p).len(), 2);
    }

    #[test]
    fn configs() {
        assert!(config_problem(
            ConfigKey::RiskAppetite,
            &serde_json::json!({"maxResidualScore": 30})
        )
        .is_some());
        assert!(config_problem(
            ConfigKey::RiskAppetite,
            &serde_json::json!({"maxResidualScore": 8})
        )
        .is_none());
        assert!(config_problem(
            ConfigKey::KpiThresholds,
            &serde_json::json!({"campus.tickets_past_sla": "x"})
        )
        .is_some());
        assert!(config_is_object(&serde_json::json!({})));
        assert_eq!(maturity_level(4.9), "level_4");
        assert!(verified_closure(GovernanceKind::ActionPlan));
    }
}
