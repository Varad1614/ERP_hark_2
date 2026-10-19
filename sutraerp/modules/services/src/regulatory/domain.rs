//! Regulatory reporting state machines (L2-H08; contract §4.2) and the pure snapshot validation:
//! every figure of the collection plan must be reported by its owning domain, every required
//! manual field must be filled.

use crate::dashboards::domain::services_enum;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use sutra_contracts::services::ports::MetricValue;
use utoipa::ToSchema;

services_enum!(
    /// Reporting definition version (L3-H-055).
    ReportingFrameworkStatus { Draft => "draft", Active => "active", Superseded => "superseded" }
);
services_enum!(
    /// Approval state of a framework draft (`REPORTING_FRAMEWORK_APPROVAL`).
    /// 9E: approvalState — wire `not_requested` (contract batch9 §6.4); the stored value stays `none`.
    ReportingApprovalState { NotRequested => "none", Pending => "pending", Approved => "approved", Rejected => "rejected" }
);
services_enum!(
    /// Reporting cycle (L3-H-056…060).
    ReportingCycleStatus {
        Planned => "planned", Collecting => "collecting", Validating => "validating",
        Packaged => "packaged", Submitted => "submitted", Closed => "closed",
    }
);
services_enum!(
    /// Collected data snapshot (L3-H-057).
    ReportSnapshotStatus { Collected => "collected", Validated => "validated", Rejected => "rejected" }
);
services_enum!(
    /// Submission package (L3-H-058…060).
    SubmissionPackageStatus {
        Draft => "draft", PendingApproval => "pending_approval", Approved => "approved",
        Submitted => "submitted", Acknowledged => "acknowledged", Corrected => "corrected",
    }
);

impl ReportingCycleStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use ReportingCycleStatus::*;
        matches!(
            (self, to),
            (Planned, Validating)
                | (Collecting, Validating)
                | (Validating, Validating)
                | (Validating, Collecting)
                | (Validating, Packaged)
                | (Packaged, Submitted)
                | (Submitted, Collecting)
                | (Submitted, Closed)
        )
    }
}

impl SubmissionPackageStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use SubmissionPackageStatus::*;
        matches!(
            (self, to),
            (Draft, PendingApproval)
                | (PendingApproval, Approved)
                | (PendingApproval, Draft)
                | (Approved, Submitted)
                | (Submitted, Acknowledged)
                | (Submitted, Corrected)
                | (Acknowledged, Corrected)
        )
    }
}

/// A field the institution fills by hand (not reported by any domain).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingManualField {
    /// `^[a-z][a-z0-9_]{0,47}$`.
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub required: bool,
}

/// One problem found when collecting a snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportingIssue {
    pub key: String,
    pub issue: String,
}

/// `true` for a manual field key.
pub fn valid_field_key(k: &str) -> bool {
    let b = k.as_bytes();
    (1..=48).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Validation issues of a collection: missing figures and missing required manual fields.
pub fn snapshot_issues(
    metric_keys: &[String],
    manual_fields: &[ReportingManualField],
    catalogue: &[MetricValue],
    manual_values: &BTreeMap<String, String>,
) -> Vec<ReportingIssue> {
    let mut out = Vec::new();
    for k in metric_keys {
        if !catalogue.iter().any(|m| &m.key == k) {
            out.push(ReportingIssue {
                key: k.clone(),
                issue: "figure not reported by its owning domain".into(),
            });
        }
    }
    for f in manual_fields {
        let filled = manual_values
            .get(&f.key)
            .is_some_and(|v| !v.trim().is_empty());
        if f.required && !filled {
            out.push(ReportingIssue {
                key: f.key.clone(),
                issue: "required manual field missing".into(),
            });
        }
    }
    for k in manual_values.keys() {
        if !manual_fields.iter().any(|f| &f.key == k) {
            out.push(ReportingIssue {
                key: k.clone(),
                issue: "not a field of this collection plan".into(),
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machines() {
        use ReportingCycleStatus as C;
        assert!(C::Planned.can_transition(C::Validating));
        assert!(C::Submitted.can_transition(C::Collecting), "correction");
        assert!(!C::Closed.can_transition(C::Collecting));
        use SubmissionPackageStatus as P;
        assert!(P::Approved.can_transition(P::Submitted));
        assert!(!P::Draft.can_transition(P::Submitted), "approval first");
        assert!(P::Acknowledged.can_transition(P::Corrected));
    }

    #[test]
    fn issues() {
        let cat = vec![MetricValue::count(
            "student",
            "active_students",
            "Students",
            10,
        )];
        let fields = vec![
            ReportingManualField {
                key: "naac_grade".into(),
                label: "NAAC grade".into(),
                required: true,
            },
            ReportingManualField {
                key: "remarks".into(),
                label: "Remarks".into(),
                required: false,
            },
        ];
        let keys = vec![
            "student.active_students".to_owned(),
            "people.unknown".to_owned(),
        ];
        let mut manual = BTreeMap::new();
        let found = snapshot_issues(&keys, &fields, &cat, &manual);
        assert_eq!(found.len(), 2);
        manual.insert("naac_grade".to_owned(), "A+".to_owned());
        manual.insert("bogus".to_owned(), "x".to_owned());
        let found = snapshot_issues(&keys[..1], &fields, &cat, &manual);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "bogus");
        assert!(valid_field_key("naac_grade") && !valid_field_key("NAAC"));
    }
}
