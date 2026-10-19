//! **9E** — campus governance (Domain G00; L3-G-001…015; `docs/batch9-contracts.md` §12.1).
//!
//! Owned by slice 9E. Payload: [`super::StatusChangedV1`] unless noted.

use super::event_type;
use uuid::Uuid;

event_type!(
    /// A governance case (assessment, review, risk, action plan, SLA deviation, KPI review,
    /// finding, escalation, exception) was submitted for its decision.
    GOVERNANCE_CASE_SUBMITTED, GOVERNANCE_CASE_SUBMITTED_SCHEMA, "governance_case.submitted");
event_type!(
    /// A governance case was decided (approved → closed with its outcome, or rejected).
    GOVERNANCE_CASE_DECIDED, GOVERNANCE_CASE_DECIDED_SCHEMA, "governance_case.decided");
event_type!(
    /// A case was escalated to executive resolution (G-012); subject = the new escalation case.
    GOVERNANCE_CASE_ESCALATED, GOVERNANCE_CASE_ESCALATED_SCHEMA, "governance_case.escalated");
event_type!(
    /// A governance configuration version became effective (G-014).
    GOVERNANCE_CONFIG_EFFECTIVE, GOVERNANCE_CONFIG_EFFECTIVE_SCHEMA, "governance_config.effective");
event_type!(
    /// Governance evidence published for accreditation (G-002 / G-015). Payload
    /// [`GovernanceEvidencePublishedV1`].
    GOVERNANCE_EVIDENCE_PUBLISHED, GOVERNANCE_EVIDENCE_PUBLISHED_SCHEMA, "governance_evidence.published");

/// Event types of this file (keep in sync; `all_event_types` collects them).
pub const EVENT_TYPES: &[&str] = &[
    GOVERNANCE_CASE_SUBMITTED,
    GOVERNANCE_CASE_DECIDED,
    GOVERNANCE_CASE_ESCALATED,
    GOVERNANCE_CONFIG_EFFECTIVE,
    GOVERNANCE_EVIDENCE_PUBLISHED,
];

/// V1 payload of `governance_evidence.published`: what accreditation (Domain H09) cites.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceEvidencePublishedV1 {
    pub evidence_id: Uuid,
    pub institution_id: Uuid,
    pub case_id: Uuid,
    /// `snake_case` case kind (`readiness_assessment`, `operational_review` …).
    pub case_kind: String,
    pub case_number: String,
    pub case_outcome: Option<String>,
    pub title: String,
    pub summary: Option<String>,
    pub version: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_payload_is_camel_case() {
        let v = serde_json::to_value(GovernanceEvidencePublishedV1 {
            evidence_id: Uuid::nil(),
            institution_id: Uuid::nil(),
            case_id: Uuid::nil(),
            case_kind: "operational_review".into(),
            case_number: "GOV-000001".into(),
            case_outcome: Some("closed_satisfactory".into()),
            title: "t".into(),
            summary: None,
            version: 2,
        })
        .unwrap_or_default();
        assert_eq!(v["caseNumber"], "GOV-000001");
        assert!(v.get("evidenceId").is_some());
        assert_eq!(EVENT_TYPES.len(), 5);
    }
}
