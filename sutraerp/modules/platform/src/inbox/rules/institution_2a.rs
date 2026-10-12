//! **2A** inbox rules for `sutra_contracts::institution::events::{lifecycle, org}`.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `institution.submitted_for_approval` | to-do "Decide activation" for the submitting administrator (the Institution Activation Authority maps to administrators in Batch 2) + warning notification to administrators |
//! | `institution.activated` / `returned_for_correction` / `rejected` / `suspended` / `reactivated` / `archived` | notification to administrators |
//! | `org_structure.restructured` | notification to administrators |

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::institution::events::lifecycle::{
    INSTITUTION_ACTIVATED, INSTITUTION_ARCHIVED, INSTITUTION_REACTIVATED, INSTITUTION_REJECTED,
    INSTITUTION_RETURNED_FOR_CORRECTION, INSTITUTION_SUBMITTED_FOR_APPROVAL, INSTITUTION_SUSPENDED,
};
use sutra_contracts::institution::events::org::{
    OrgStructureRestructuredV1, ORG_STRUCTURE_RESTRUCTURED,
};
use sutra_contracts::institution::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    INSTITUTION_SUBMITTED_FOR_APPROVAL,
    INSTITUTION_ACTIVATED,
    INSTITUTION_RETURNED_FOR_CORRECTION,
    INSTITUTION_REJECTED,
    INSTITUTION_SUSPENDED,
    INSTITUTION_REACTIVATED,
    INSTITUTION_ARCHIVED,
    ORG_STRUCTURE_RESTRUCTURED,
];

const INSTITUTION_LINK: &str = "/campus/institution";

fn admins() -> Recipient {
    Recipient::Role(Role::Administrator)
}

fn with_reason(base: &str, reason: Option<&str>) -> String {
    match reason {
        Some(r) if !r.trim().is_empty() => format!("{base} Reason: {r}"),
        _ => base.to_owned(),
    }
}

/// Decides the projections of one event of [`EVENT_TYPES`] (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let t = env.event_type.as_str();
    if t == ORG_STRUCTURE_RESTRUCTURED {
        let d: OrgStructureRestructuredV1 = data(env)?;
        let body = format!(
            "{} units moved, {} decommissioned; effective {}.",
            d.moves.len(),
            d.decommissioned_unit_ids.len(),
            d.effective_from
        );
        return Ok(vec![notify(
            env,
            admins(),
            Severity::Info,
            "Organisation structure changed".into(),
            body,
            Some("/campus/organisation"),
        )]);
    }
    let d: StatusChangedV1 = data(env)?;
    let reason = d.reason.as_deref();
    Ok(match t {
        INSTITUTION_SUBMITTED_FOR_APPROVAL => {
            let mut v = vec![notify(
                env,
                admins(),
                Severity::Warning,
                "Institution activation awaits a decision".into(),
                "An activation request was submitted to the Institution Activation Authority."
                    .into(),
                Some(INSTITUTION_LINK),
            )];
            if let Some(actor) = env.actorref.as_deref().filter(|a| !a.is_empty()) {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: None,
                    assignee_subject: actor.to_owned(),
                    title: "Decide the institution activation request".into(),
                    detail:
                        "Approve, return for correction or reject the pending activation request."
                            .into(),
                    due_at: Some(env.time + Duration::days(3)),
                    link: Some(INSTITUTION_LINK.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        INSTITUTION_ACTIVATED => vec![notify(
            env,
            admins(),
            Severity::Success,
            "Institution activated".into(),
            "The institution is now active.".into(),
            Some(INSTITUTION_LINK),
        )],
        INSTITUTION_RETURNED_FOR_CORRECTION => vec![notify(
            env,
            admins(),
            Severity::Warning,
            "Activation request returned for correction".into(),
            with_reason("Correct the configuration and submit again.", reason),
            Some(INSTITUTION_LINK),
        )],
        INSTITUTION_REJECTED => vec![notify(
            env,
            admins(),
            Severity::Critical,
            "Activation request rejected".into(),
            with_reason("A new activation request cycle is needed.", reason),
            Some(INSTITUTION_LINK),
        )],
        INSTITUTION_SUSPENDED => vec![notify(
            env,
            admins(),
            Severity::Critical,
            "Institution suspended".into(),
            with_reason(
                "Its configuration is read-only until it is restored.",
                reason,
            ),
            Some(INSTITUTION_LINK),
        )],
        INSTITUTION_REACTIVATED => vec![notify(
            env,
            admins(),
            Severity::Success,
            "Institution restored".into(),
            with_reason("The institution is active again.", reason),
            Some(INSTITUTION_LINK),
        )],
        INSTITUTION_ARCHIVED => vec![notify(
            env,
            admins(),
            Severity::Warning,
            "Institution archived".into(),
            with_reason("The institution is permanently read-only.", reason),
            Some(INSTITUTION_LINK),
        )],
        _ => vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::notifications::NotificationCategory;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId};

    fn env(event_type: &'static str, data: serde_json::Value) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId::new(), InstitutionId::new());
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/institution",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
        .with_actor(Some("dev|admin@sutra.dev".into()))
    }

    fn status(to: &str, reason: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "aggregateId": uuid::Uuid::now_v7(), "institutionId": uuid::Uuid::now_v7(),
            "fromStatus": "configured", "toStatus": to, "version": 3, "reason": reason,
            "processInstanceId": "noop-1"
        })
    }

    #[test]
    fn submission_assigns_a_decision_todo_and_warns_admins() {
        let e = env(
            INSTITUTION_SUBMITTED_FOR_APPROVAL,
            status("pending_approval", None),
        );
        let a = plan(&e).unwrap_or_default();
        assert_eq!(a.len(), 2);
        let InboxAction::Notify(n) = &a[0] else {
            panic!("notification first")
        };
        assert_eq!(n.recipient, Recipient::Role(Role::Administrator));
        assert_eq!(n.category, NotificationCategory::Institution);
        assert_eq!(n.institution, None, "institution lifecycle is tenant-wide");
        assert_eq!(n.source_event_id, Some(e.id));
        let InboxAction::Todo(t) = &a[1] else {
            panic!("to-do second")
        };
        assert_eq!(t.assignee_subject, "dev|admin@sutra.dev");
        assert_eq!(t.source_ref, e.id.to_string());
    }

    #[test]
    fn lifecycle_outcomes_notify_admins_with_the_reason() {
        for (t, sev) in [
            (INSTITUTION_ACTIVATED, Severity::Success),
            (INSTITUTION_RETURNED_FOR_CORRECTION, Severity::Warning),
            (INSTITUTION_REJECTED, Severity::Critical),
            (INSTITUTION_SUSPENDED, Severity::Critical),
            (INSTITUTION_REACTIVATED, Severity::Success),
            (INSTITUTION_ARCHIVED, Severity::Warning),
        ] {
            let a = plan(&env(t, status("x", Some("audit finding")))).unwrap_or_default();
            assert_eq!(a.len(), 1, "{t}");
            let InboxAction::Notify(n) = &a[0] else {
                panic!("notification")
            };
            assert_eq!(n.severity, sev, "{t}");
            if t != INSTITUTION_ACTIVATED {
                assert!(n.body.contains("audit finding"), "{t}");
            }
        }
    }

    #[test]
    fn restructure_notifies_with_its_institution_and_bad_payloads_fail() {
        let e = env(
            ORG_STRUCTURE_RESTRUCTURED,
            serde_json::json!({
                "restructureId": uuid::Uuid::now_v7(), "institutionId": uuid::Uuid::now_v7(),
                "moves": [{"orgUnitId": uuid::Uuid::now_v7(), "fromParentId": null, "toParentId": uuid::Uuid::now_v7()}],
                "decommissionedUnitIds": [], "effectiveFrom": "2026-10-12", "processInstanceId": null
            }),
        );
        let a = plan(&e).unwrap_or_default();
        let InboxAction::Notify(n) = &a[0] else {
            panic!("notification")
        };
        assert_eq!(n.institution, e.institutionid);
        assert!(n.body.contains("1 units moved"));
        assert!(plan(&env(INSTITUTION_ACTIVATED, serde_json::json!({"bogus": 1}))).is_err());
        assert_eq!(EVENT_TYPES.len(), 8);
    }
}
