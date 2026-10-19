//! **7C** inbox rules for `sutra_contracts::people::events::payroll` (contract doc
//! `docs/batch7-contracts.md` §8). People: payroll runs, payslips.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `payslip.issued` | notification → the staff member (`staffSubjectRef`, link `/leave`) |
//! | `payroll_run.submitted` | emitted only while the approval awaits a decision: notification → administrators + to-do for the submitter "Approve payroll …" (link `/people/payroll`) |
//!
//! Staff notifications need `staffSubjectRef` in the payload (rules are pure, no lookup) and are
//! skipped without it; notifications are idempotent on the event id (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::people::events::payroll::{PAYROLL_RUN_SUBMITTED, PAYSLIP_ISSUED};
use sutra_contracts::people::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[PAYSLIP_ISSUED, PAYROLL_RUN_SUBMITTED];

const MY_HR: &str = "/leave";
const ADMIN_PAYROLL: &str = "/people/payroll";

/// Projects one 7C event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    Ok(match env.event_type.as_str() {
        PAYSLIP_ISSUED => {
            let d: StatusChangedV1 = data(env)?;
            let reissued = d.to_status == "reissued";
            let number = d.reason.clone().unwrap_or_default();
            d.staff_subject_ref
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| {
                    notify(
                        env,
                        Recipient::Subject(s.to_owned()),
                        Severity::Info,
                        if reissued {
                            format!("Your payslip {number} was reissued")
                        } else {
                            format!("Your payslip {number} is ready")
                        },
                        "View earnings, deductions and net pay under Leave → My payslips.".into(),
                        Some(MY_HR),
                    )
                })
                .into_iter()
                .collect()
        }
        PAYROLL_RUN_SUBMITTED => {
            let d: StatusChangedV1 = data(env)?;
            let what = format!(
                "Approve payroll run {}",
                env.subject.clone().unwrap_or_default()
            );
            let mut v = vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Info,
                "Payroll awaits approval".into(),
                d.reason
                    .clone()
                    .unwrap_or_else(|| "A payroll run was submitted for approval.".into()),
                Some(ADMIN_PAYROLL),
            )];
            if let Some(actor) = env
                .actorref
                .as_deref()
                .filter(|a| !a.is_empty() && !a.starts_with("system:"))
            {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: env.institutionid,
                    assignee_subject: actor.to_owned(),
                    title: what,
                    detail: "Review the calculated payroll and decide it in People → Payroll."
                        .into(),
                    due_at: Some(env.time + Duration::days(2)),
                    link: Some(ADMIN_PAYROLL.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        other => return Err(format!("{other}: no 7C rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId};
    use uuid::Uuid;

    fn env(t: &'static str, data: serde_json::Value, actor: Option<&str>) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/people",
                event_type: t,
                dataschema: "urn:test",
                subject: Some("run-1".into()),
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn status(to: &str, subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
            "toStatus": to, "version": 1, "reason": "PS-000001", "staffSubjectRef": subject})
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
    fn staff_hear_about_their_payslips() {
        let s = "dev|meera.shah@sutra.dev";
        let a = plan(&env(PAYSLIP_ISSUED, status("issued", Some(s)), None)).unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Subject(s.into())]);
        match &a[0] {
            InboxAction::Notify(n) => {
                assert_eq!(n.title, "Your payslip PS-000001 is ready");
                assert_eq!(n.link.as_deref(), Some(MY_HR));
            }
            InboxAction::Todo(_) => panic!("notification expected"),
        }
        assert!(plan(&env(PAYSLIP_ISSUED, status("issued", None), None))
            .unwrap_or_default()
            .is_empty());
    }

    #[test]
    fn submitted_runs_create_admin_work() {
        let a = plan(&env(
            PAYROLL_RUN_SUBMITTED,
            status("pending_approval", None),
            Some("dev|admin@sutra.dev"),
        ))
        .unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
        assert!(a.iter().any(
            |x| matches!(x, InboxAction::Todo(t) if t.assignee_subject == "dev|admin@sutra.dev")
        ));
        assert!(plan(&env("sutraerp.people.other.x", status("x", None), None)).is_err());
    }
}
