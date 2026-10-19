//! **8A** inbox rules for `sutra_contracts::campus::events::{facilities, SERVICE_TICKET_OPENED}` (contract doc
//! `docs/batch8-contracts.md` §8). Campus infrastructure, reservations, labs, maintenance, service desk. The foundation event `service_ticket.opened` (emitted by `shared::tickets::open_ticket` for every slice) belongs to these rules too.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `service_ticket.opened` | notification → administrators "Triage SR-…" (link `/campus/facilities`) |
//! | `service_ticket.resolved` / `closed` | notification → the requester (`subjectRef`) |
//! | `space_reservation.confirmed` / `cancelled` | notification → the requester (`subjectRef`) |
//! | `service_ticket.escalated` | notification → administrators |
//!
//! A to-do needs a login subject (`SystemTodo::assignee_subject`) and administrators are a role,
//! so the "Triage SR-…" to-do of the contract is an administrator notification (warning severity).
//! Requester notifications need `subjectRef` in the payload (rules are pure, no lookup) and are
//! skipped without it; notifications are idempotent on the event id (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::campus::events::facilities::{
    SERVICE_TICKET_CLOSED, SERVICE_TICKET_ESCALATED, SERVICE_TICKET_RESOLVED,
    SPACE_RESERVATION_CANCELLED, SPACE_RESERVATION_CONFIRMED,
};
use sutra_contracts::campus::events::{StatusChangedV1, SERVICE_TICKET_OPENED};
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    SERVICE_TICKET_OPENED,
    SERVICE_TICKET_RESOLVED,
    SERVICE_TICKET_CLOSED,
    SERVICE_TICKET_ESCALATED,
    SPACE_RESERVATION_CONFIRMED,
    SPACE_RESERVATION_CANCELLED,
];

const DESK: &str = "/campus/facilities";
const ROOMS: &str = "/campus/infrastructure";

fn to_requester(
    env: &EventEnvelope,
    d: &StatusChangedV1,
    severity: Severity,
    title: String,
    body: String,
    link: &str,
) -> Vec<InboxAction> {
    d.subject_ref
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| {
            notify(
                env,
                Recipient::Subject(s.to_owned()),
                severity,
                title,
                body,
                Some(link),
            )
        })
        .into_iter()
        .collect()
}

/// Projects one 8A event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let number = d.number.clone().unwrap_or_default();
    let detail = d.reason.clone().unwrap_or_default();
    Ok(match env.event_type.as_str() {
        SERVICE_TICKET_OPENED => vec![notify(
            env,
            Recipient::Role(Role::Administrator),
            Severity::Warning,
            format!("Triage {number}"),
            detail,
            Some(DESK),
        )],
        SERVICE_TICKET_RESOLVED => to_requester(
            env,
            &d,
            Severity::Success,
            format!("Your request {number} was resolved"),
            format!("{detail} — please confirm or reopen it."),
            DESK,
        ),
        SERVICE_TICKET_CLOSED => to_requester(
            env,
            &d,
            Severity::Info,
            format!("Your request {number} was closed"),
            detail,
            DESK,
        ),
        SERVICE_TICKET_ESCALATED => vec![notify(
            env,
            Recipient::Role(Role::Administrator),
            Severity::Warning,
            format!("{number} escalated"),
            detail,
            Some(DESK),
        )],
        SPACE_RESERVATION_CONFIRMED => to_requester(
            env,
            &d,
            Severity::Success,
            format!("Your booking {number} is confirmed"),
            detail,
            ROOMS,
        ),
        SPACE_RESERVATION_CANCELLED => to_requester(
            env,
            &d,
            Severity::Warning,
            format!("Your booking {number} was cancelled"),
            detail,
            ROOMS,
        ),
        other => return Err(format!("{other}: no 8A rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    fn env(t: &'static str, subject: Option<&str>) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/campus",
                event_type: t,
                dataschema: "urn:test",
                subject: None,
                data: serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
                    "toStatus": "resolved", "version": 2, "reason": "Projector",
                    "number": "SR-000001", "subjectRef": subject}),
            },
        )
    }

    fn recipients(a: &[InboxAction]) -> Vec<Recipient> {
        a.iter()
            .filter_map(|x| match x {
                InboxAction::Notify(n) => Some(n.recipient.clone()),
                InboxAction::Todo(_) => None,
            })
            .collect()
    }

    #[test]
    fn administrators_triage_and_hear_escalations() {
        for t in [SERVICE_TICKET_OPENED, SERVICE_TICKET_ESCALATED] {
            let a = plan(&env(t, Some("dev|a"))).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![Recipient::Role(Role::Administrator)],
                "{t}"
            );
        }
    }

    #[test]
    fn requesters_hear_about_their_requests_and_bookings() {
        for t in [
            SERVICE_TICKET_RESOLVED,
            SERVICE_TICKET_CLOSED,
            SPACE_RESERVATION_CONFIRMED,
            SPACE_RESERVATION_CANCELLED,
        ] {
            let a = plan(&env(t, Some("dev|a"))).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![Recipient::Subject("dev|a".into())],
                "{t}"
            );
            assert!(plan(&env(t, None)).unwrap_or_default().is_empty(), "{t}");
        }
        assert!(plan(&env("sutraerp.campus.nope.x", None)).is_err());
    }
}
