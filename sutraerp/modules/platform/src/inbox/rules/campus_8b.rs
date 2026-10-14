//! **8B** inbox rules for `sutra_contracts::campus::events::residence` (contract doc
//! `docs/batch8-contracts.md` §8). Hostel and transport.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `hostel_application.decided` | notification → the applicant (`subjectRef`; approved / waitlisted / rejected) |
//! | `hostel_allocation.activated` | notification → the resident (bed allocated) |
//! | `resident_leave.decided` | notification → the resident |
//! | `transport_pass.issued` | notification → the passenger |
//! | `trip.cancelled` | notification → administrators (trips carry no passenger subject) |
//!
//! Person notifications need `subjectRef` in the payload (rules are pure, no lookup) and are
//! skipped without it; notifications are idempotent on the event id (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::campus::events::residence::{
    HOSTEL_ALLOCATION_ACTIVATED, HOSTEL_APPLICATION_DECIDED, RESIDENT_LEAVE_DECIDED,
    TRANSPORT_PASS_ISSUED, TRIP_CANCELLED,
};
use sutra_contracts::campus::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    HOSTEL_APPLICATION_DECIDED,
    HOSTEL_ALLOCATION_ACTIVATED,
    RESIDENT_LEAVE_DECIDED,
    TRANSPORT_PASS_ISSUED,
    TRIP_CANCELLED,
];

/// Where residents / passengers look (the admin Campus screens for the office).
const ADMIN_TRANSPORT: &str = "/campus/transport";

fn to_subject(
    env: &EventEnvelope,
    subject: Option<&str>,
    severity: Severity,
    title: String,
    body: String,
) -> Vec<InboxAction> {
    subject
        .filter(|s| !s.is_empty())
        .map(|s| {
            notify(
                env,
                Recipient::Subject(s.to_owned()),
                severity,
                title,
                body,
                None,
            )
        })
        .into_iter()
        .collect()
}

/// Projects one 8B event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let number = d.number.clone().unwrap_or_default();
    let detail = d.reason.clone().unwrap_or_default();
    let subject = d.subject_ref.as_deref();
    Ok(match env.event_type.as_str() {
        HOSTEL_APPLICATION_DECIDED => {
            let (severity, title) = match d.to_status.as_str() {
                "approved" => (
                    Severity::Success,
                    format!("Your hostel application {number} was approved"),
                ),
                "waitlisted" => (
                    Severity::Info,
                    format!("Your hostel application {number} is on the waiting list"),
                ),
                _ => (
                    Severity::Warning,
                    format!("Your hostel application {number} was not approved"),
                ),
            };
            to_subject(env, subject, severity, title, detail)
        }
        HOSTEL_ALLOCATION_ACTIVATED => to_subject(
            env,
            subject,
            Severity::Success,
            "Your hostel bed is allocated".into(),
            format!("Residency under application {number} is active."),
        ),
        RESIDENT_LEAVE_DECIDED => {
            let approved = d.to_status == "approved";
            to_subject(
                env,
                subject,
                if approved {
                    Severity::Success
                } else {
                    Severity::Warning
                },
                if approved {
                    "Your hostel leave was approved".into()
                } else {
                    "Your hostel leave was rejected".into()
                },
                detail,
            )
        }
        TRANSPORT_PASS_ISSUED => to_subject(
            env,
            subject,
            Severity::Success,
            format!("Transport pass {number} issued"),
            detail,
        ),
        TRIP_CANCELLED => vec![notify(
            env,
            Recipient::Role(Role::Administrator),
            Severity::Warning,
            "A transport trip was cancelled".into(),
            detail,
            Some(ADMIN_TRANSPORT),
        )],
        other => return Err(format!("{other}: no 8B rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    fn env(t: &'static str, data: serde_json::Value) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/campus",
                event_type: t,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
    }

    fn status(to: &str, subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
            "toStatus": to, "version": 2, "number": "HAP-000001", "subjectRef": subject})
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
    fn persons_hear_about_their_hostel_and_pass() {
        for t in [
            HOSTEL_APPLICATION_DECIDED,
            HOSTEL_ALLOCATION_ACTIVATED,
            RESIDENT_LEAVE_DECIDED,
            TRANSPORT_PASS_ISSUED,
        ] {
            let a = plan(&env(t, status("approved", Some("dev|aarav")))).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![Recipient::Subject("dev|aarav".into())],
                "{t}"
            );
            let none = plan(&env(t, status("approved", None))).unwrap_or_default();
            assert!(none.is_empty(), "{t}");
        }
    }

    #[test]
    fn cancelled_trips_reach_administrators() {
        let a = plan(&env(TRIP_CANCELLED, status("cancelled", None))).unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
        assert!(plan(&env("sutraerp.campus.nope.x", status("x", None))).is_err());
    }
}
