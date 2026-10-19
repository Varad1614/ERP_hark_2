//! **8C** inbox rules for `sutra_contracts::services::events::reporting` (contract doc
//! `docs/batch8-contracts.md` §8). Dashboards and regulatory reporting.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `submission_package.approved` | notification → administrators: the package can be submitted to the regulator (link `/reports/regulatory`) |

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::services::events::reporting::SUBMISSION_PACKAGE_APPROVED;
use sutra_contracts::services::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[SUBMISSION_PACKAGE_APPROVED];

const REGULATORY: &str = "/reports/regulatory";

/// Projects one 8C event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    match env.event_type.as_str() {
        SUBMISSION_PACKAGE_APPROVED => Ok(vec![notify(
            env,
            Recipient::Role(Role::Administrator),
            Severity::Success,
            format!(
                "Submission package {} approved",
                d.number.unwrap_or_default()
            ),
            "Submit it to the regulator and record the acknowledgement reference.".into(),
            Some(REGULATORY),
        )]),
        other => Err(format!("{other}: no 8C rule")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    #[test]
    fn administrators_hear_about_approved_packages() {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        let env = EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/services",
                event_type: SUBMISSION_PACKAGE_APPROVED,
                dataschema: "urn:test",
                subject: None,
                data: serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
                    "toStatus": "approved", "version": 3, "number": "SUB-000001"}),
            },
        );
        let a = plan(&env).unwrap_or_default();
        assert!(matches!(&a[..], [InboxAction::Notify(n)]
            if n.recipient == Recipient::Role(Role::Administrator)
                && n.link.as_deref() == Some(REGULATORY)));
    }
}
