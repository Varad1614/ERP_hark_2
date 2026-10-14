//! **8C** inbox rules for `sutra_contracts::campus::events::library` (contract doc
//! `docs/batch8-contracts.md` §8). Library.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `library_loan.overdue` | notification (warning) → the borrower (link `/library`) |
//! | `library_reservation.ready` | notification (success) → the member: collect before the hold ends |
//! | `library_fine.raised` | notification (warning) → the student billed |
//! | `library_clearance.decided` | notification → the member (approved / rejected) |
//!
//! Recipients come from the payload's `subjectRef` (pure rules, no lookup); events without one
//! (members without a linked login) project nothing.

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::campus::events::library::{
    LIBRARY_CLEARANCE_DECIDED, LIBRARY_FINE_RAISED, LIBRARY_LOAN_OVERDUE, LIBRARY_RESERVATION_READY,
};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    LIBRARY_LOAN_OVERDUE,
    LIBRARY_RESERVATION_READY,
    LIBRARY_FINE_RAISED,
    LIBRARY_CLEARANCE_DECIDED,
];

const MY_LIBRARY: &str = "/library";

/// Projects one 8C event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let Some(subject) = d.subject_ref.clone().filter(|s| !s.is_empty()) else {
        return Ok(Vec::new());
    };
    let number = d.number.clone().unwrap_or_default();
    let reason = d.reason.clone().unwrap_or_default();
    let (severity, title, body) = match env.event_type.as_str() {
        LIBRARY_LOAN_OVERDUE => (
            Severity::Warning,
            format!("Library loan {number} is overdue"),
            format!("Please return it as soon as possible ({reason}); a daily fine applies."),
        ),
        LIBRARY_RESERVATION_READY => (
            Severity::Success,
            "Your reserved book is ready".to_owned(),
            format!("{reason} (copy {number}) is held for you at the library desk."),
        ),
        LIBRARY_FINE_RAISED => (
            Severity::Warning,
            "A library fine was added to your fees".to_owned(),
            format!("{reason}. Pay it under Fees to keep borrowing."),
        ),
        LIBRARY_CLEARANCE_DECIDED => {
            if d.to_status == "approved" {
                (
                    Severity::Success,
                    "Your library clearance was approved".to_owned(),
                    "You have no loans or library dues outstanding.".to_owned(),
                )
            } else {
                (
                    Severity::Warning,
                    format!(
                        "Your library clearance is {}",
                        d.to_status.replace('_', " ")
                    ),
                    reason,
                )
            }
        }
        other => return Err(format!("{other}: no 8C rule")),
    };
    Ok(vec![notify(
        env,
        Recipient::Subject(subject),
        severity,
        title,
        body,
        Some(MY_LIBRARY),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    fn env(t: &'static str, to: &str, subject: Option<&str>) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/campus",
                event_type: t,
                dataschema: "urn:test",
                subject: None,
                data: serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
                    "toStatus": to, "version": 2, "reason": "Thermodynamics", "number": "LN-000001",
                    "subjectRef": subject}),
            },
        )
    }

    #[test]
    fn members_are_notified_by_subject() {
        for &t in EVENT_TYPES {
            let a = plan(&env(t, "approved", Some("dev|aarav"))).unwrap_or_default();
            assert_eq!(a.len(), 1, "{t}");
            assert!(matches!(&a[0], InboxAction::Notify(n)
                if n.recipient == Recipient::Subject("dev|aarav".into())
                    && n.link.as_deref() == Some(MY_LIBRARY)));
        }
        let none = plan(&env(LIBRARY_LOAN_OVERDUE, "overdue", None)).unwrap_or_default();
        assert!(none.is_empty(), "no linked login, nothing to project");
    }
}
