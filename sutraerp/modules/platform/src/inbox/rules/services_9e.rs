//! **9E** inbox rules for `sutra_contracts::services::events::accreditation` (Batch 9 contract
//! §12.2).
//!
//! | Event | Projection |
//! |-------|------------|
//! | `accreditation_record.handed_off` | notification (success) → administrators: the self-study report was handed off (link `/reports/accreditation`) |

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::services::events::accreditation::SELF_STUDY_HANDED_OFF;
use sutra_contracts::services::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[SELF_STUDY_HANDED_OFF];

const ACCREDITATION: &str = "/reports/accreditation";

/// Projects one 9E accreditation event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    match env.event_type.as_str() {
        SELF_STUDY_HANDED_OFF => Ok(vec![notify(
            env,
            Recipient::Role(Role::Administrator),
            Severity::Success,
            format!(
                "Self-study report {} handed off",
                d.number.unwrap_or_default()
            ),
            format!("Reference {}.", d.reason.unwrap_or_default()),
            Some(ACCREDITATION),
        )]),
        other => Err(format!("{other}: no 9E accreditation rule")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    #[test]
    fn administrators_hear_about_the_hand_off() {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        let env = EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/services",
                event_type: SELF_STUDY_HANDED_OFF,
                dataschema: "urn:test",
                subject: None,
                data: serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
                    "toStatus": "handed_off", "version": 5, "number": "ACR-000009",
                    "reason": "NAAC-SSR-1"}),
            },
        );
        let a = plan(&env).unwrap_or_default();
        assert!(
            matches!(&a[..], [InboxAction::Notify(n)] if n.link.as_deref() == Some(ACCREDITATION))
        );
    }
}
