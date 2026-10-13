//! **7B** inbox rules for `sutra_contracts::people::events::leave` (contract doc
//! `docs/batch7-contracts.md` §8). People: leave, long leave, attendance, workload.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `leave_request.submitted` | notification + to-do → the reporting manager (`managerSubjectRef`, link `/leave`); without a manager → administrators (link `/people/leave`) |
//! | `leave_request.approved` / `rejected` / `cancelled` | notification → the applicant (link `/leave`) |
//! | `leave_encashment.decided` | notification → the staff member (link `/leave`) |
//!
//! Staff notifications need `staffSubjectRef` / `managerSubjectRef` in the payload (rules are
//! pure, no lookup) and are skipped without them; notifications are idempotent on the event id
//! (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::people::events::leave::{
    LEAVE_ENCASHMENT_DECIDED, LEAVE_REQUEST_APPROVED, LEAVE_REQUEST_CANCELLED,
    LEAVE_REQUEST_REJECTED, LEAVE_REQUEST_SUBMITTED,
};
use sutra_contracts::people::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    LEAVE_REQUEST_SUBMITTED,
    LEAVE_REQUEST_APPROVED,
    LEAVE_REQUEST_REJECTED,
    LEAVE_REQUEST_CANCELLED,
    LEAVE_ENCASHMENT_DECIDED,
];

const MY_LEAVE: &str = "/leave";
const ADMIN_LEAVE: &str = "/people/leave";

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
                Some(MY_LEAVE),
            )
        })
        .into_iter()
        .collect()
}

/// Projects one 7B event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let detail = d.reason.clone().unwrap_or_default();
    Ok(match env.event_type.as_str() {
        LEAVE_REQUEST_SUBMITTED => {
            match d.manager_subject_ref.as_deref().filter(|s| !s.is_empty()) {
                Some(manager) => vec![
                    notify(
                        env,
                        Recipient::Subject(manager.to_owned()),
                        Severity::Info,
                        "A leave request awaits your decision".into(),
                        detail.clone(),
                        Some(MY_LEAVE),
                    ),
                    InboxAction::Todo(SystemTodo {
                        tenant: TenantId(env.tenantid),
                        institution: env.institutionid,
                        assignee_subject: manager.to_owned(),
                        title: format!("Decide leave request ({detail})"),
                        detail: "Approve or reject the leave request under Leave → Approvals."
                            .into(),
                        due_at: Some(env.time + Duration::days(2)),
                        link: Some(MY_LEAVE.into()),
                        source_ref: env.id.to_string(),
                    }),
                ],
                None => vec![notify(
                    env,
                    Recipient::Role(Role::Administrator),
                    Severity::Info,
                    "A leave request awaits a decision".into(),
                    detail,
                    Some(ADMIN_LEAVE),
                )],
            }
        }
        LEAVE_REQUEST_APPROVED => to_subject(
            env,
            d.staff_subject_ref.as_deref(),
            Severity::Success,
            "Your leave was approved".into(),
            detail,
        ),
        LEAVE_REQUEST_REJECTED => to_subject(
            env,
            d.staff_subject_ref.as_deref(),
            Severity::Warning,
            "Your leave was rejected".into(),
            detail,
        ),
        LEAVE_REQUEST_CANCELLED => to_subject(
            env,
            d.staff_subject_ref.as_deref(),
            Severity::Info,
            "Your leave was cancelled".into(),
            detail,
        ),
        LEAVE_ENCASHMENT_DECIDED => {
            let approved = d.to_status == "approved";
            to_subject(
                env,
                d.staff_subject_ref.as_deref(),
                if approved {
                    Severity::Success
                } else {
                    Severity::Warning
                },
                if approved {
                    "Your leave encashment was approved".into()
                } else {
                    format!("Your leave encashment is {}", d.to_status.replace('_', " "))
                },
                detail,
            )
        }
        other => return Err(format!("{other}: no 7B rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId};
    use uuid::Uuid;

    fn env(t: &'static str, data: serde_json::Value) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/people",
                event_type: t,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
    }

    fn status(to: &str, staff: Option<&str>, manager: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
            "toStatus": to, "version": 1, "reason": "LV-000001",
            "staffSubjectRef": staff, "managerSubjectRef": manager})
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
    fn submitted_goes_to_the_manager_else_administrators() {
        let a = plan(&env(
            LEAVE_REQUEST_SUBMITTED,
            status("pending_approval", Some("dev|a"), Some("dev|m")),
        ))
        .unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Subject("dev|m".into())]);
        assert!(a
            .iter()
            .any(|x| matches!(x, InboxAction::Todo(t) if t.assignee_subject == "dev|m")));
        let a = plan(&env(
            LEAVE_REQUEST_SUBMITTED,
            status("pending_approval", Some("dev|a"), None),
        ))
        .unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
    }

    #[test]
    fn applicants_hear_about_decisions() {
        for t in [
            LEAVE_REQUEST_APPROVED,
            LEAVE_REQUEST_REJECTED,
            LEAVE_REQUEST_CANCELLED,
            LEAVE_ENCASHMENT_DECIDED,
        ] {
            let a = plan(&env(t, status("approved", Some("dev|a"), None))).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![Recipient::Subject("dev|a".into())],
                "{t}"
            );
            let none = plan(&env(t, status("approved", None, None))).unwrap_or_default();
            assert!(none.is_empty(), "{t}");
        }
        assert!(plan(&env("sutraerp.people.nope.x", status("x", None, None))).is_err());
    }
}
