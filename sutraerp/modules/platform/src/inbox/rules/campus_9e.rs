//! **9E** inbox rules for `sutra_contracts::campus::events::governance` (Batch 9 contract §12.1).
//!
//! | Event | Projection |
//! |-------|------------|
//! | `governance_case.escalated` | notification (warning) → administrators: executive resolution needed (link `/campus/governance`) |
//! | `governance_evidence.published` | notification (success) → administrators: evidence can be cited by accreditation (link `/reports/accreditation`) |

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::campus::events::governance::{
    GovernanceEvidencePublishedV1, GOVERNANCE_CASE_ESCALATED, GOVERNANCE_EVIDENCE_PUBLISHED,
};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[GOVERNANCE_CASE_ESCALATED, GOVERNANCE_EVIDENCE_PUBLISHED];

const GOVERNANCE: &str = "/campus/governance";
const ACCREDITATION: &str = "/reports/accreditation";

/// Projects one 9E governance event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    match env.event_type.as_str() {
        GOVERNANCE_CASE_ESCALATED => {
            let d: StatusChangedV1 = data(env)?;
            Ok(vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Warning,
                format!(
                    "Governance escalation {} needs an executive resolution",
                    d.number.unwrap_or_default()
                ),
                d.reason.unwrap_or_default(),
                Some(GOVERNANCE),
            )])
        }
        GOVERNANCE_EVIDENCE_PUBLISHED => {
            let d: GovernanceEvidencePublishedV1 = data(env)?;
            Ok(vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Success,
                format!("Governance evidence published: {}", d.title),
                format!(
                    "From {} ({}). Cite it in accreditation evidence records.",
                    d.case_number, d.case_kind
                ),
                Some(ACCREDITATION),
            )])
        }
        other => Err(format!("{other}: no 9E governance rule")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    #[test]
    fn administrators_hear_about_published_evidence() {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        let env = EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/campus",
                event_type: GOVERNANCE_EVIDENCE_PUBLISHED,
                dataschema: "urn:test",
                subject: None,
                data: serde_json::json!({"evidenceId": Uuid::nil(), "institutionId": Uuid::nil(),
                    "caseId": Uuid::nil(), "caseKind": "operational_review",
                    "caseNumber": "GOV-000001", "caseOutcome": null, "title": "Ops review",
                    "summary": null, "version": 2}),
            },
        );
        let a = plan(&env).unwrap_or_default();
        assert!(matches!(&a[..], [InboxAction::Notify(n)]
            if n.recipient == Recipient::Role(Role::Administrator)
                && n.link.as_deref() == Some(ACCREDITATION)));
    }
}
