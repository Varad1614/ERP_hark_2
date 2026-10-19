//! **7A** inbox rules for `sutra_contracts::people::events::workforce` (contract doc
//! `docs/batch7-contracts.md` §8). People: staff records, engagements, positions, faculty, onboarding, probation, separation, certificates.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `staff_engagement.activated` / `onboarding_case.completed` / `probation_review.decided` / `certificate_request.issued` | notification → the staff member (link `/leave`) |
//! | `staff_engagement.submitted` / `separation_case.submitted` | notification → administrators; to-do for the submitting HR user "Approve engagement …" / "Approve separation …" |
//!
//! Staff notifications need `staffSubjectRef` in the payload (rules are pure, no lookup) and are
//! skipped without it; notifications are idempotent on the event id (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::people::events::workforce::{
    CERTIFICATE_ISSUED, ENGAGEMENT_ACTIVATED, ENGAGEMENT_SUBMITTED, ONBOARDING_COMPLETED,
    PROBATION_DECIDED, SEPARATION_SUBMITTED,
};
use sutra_contracts::people::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    ENGAGEMENT_ACTIVATED,
    ONBOARDING_COMPLETED,
    PROBATION_DECIDED,
    CERTIFICATE_ISSUED,
    ENGAGEMENT_SUBMITTED,
    SEPARATION_SUBMITTED,
];

const MY_HR: &str = "/leave";
const ADMIN_STAFF: &str = "/people/staff";
const ADMIN_HR: &str = "/people/recruitment";

fn to_staff(
    env: &EventEnvelope,
    d: &StatusChangedV1,
    severity: Severity,
    title: String,
    body: String,
) -> Vec<InboxAction> {
    d.staff_subject_ref
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| {
            notify(
                env,
                Recipient::Subject(s.to_owned()),
                severity,
                title,
                body,
                Some(MY_HR),
            )
        })
        .into_iter()
        .collect()
}

fn approval_work(env: &EventEnvelope, title: String, body: String, link: &str) -> Vec<InboxAction> {
    let mut v = vec![notify(
        env,
        Recipient::Role(Role::Administrator),
        Severity::Info,
        title.clone(),
        body.clone(),
        Some(link),
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
            title,
            detail: body,
            due_at: Some(env.time + Duration::days(3)),
            link: Some(link.to_owned()),
            source_ref: env.id.to_string(),
        }));
    }
    v
}

fn short(d: &StatusChangedV1) -> String {
    d.aggregate_id.to_string().chars().take(8).collect()
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let reason = d.reason.clone().unwrap_or_default();
    Ok(match env.event_type.as_str() {
        ENGAGEMENT_ACTIVATED => to_staff(
            env,
            &d,
            Severity::Success,
            "Your engagement is active".into(),
            "Your employment record is now active. See My HR for the details.".into(),
        ),
        ONBOARDING_COMPLETED => to_staff(
            env,
            &d,
            Severity::Success,
            "Welcome aboard".into(),
            "Your onboarding is complete and your staff record has been created.".into(),
        ),
        PROBATION_DECIDED => {
            let (severity, title) = match reason.as_str() {
                "confirmed" => (Severity::Success, "Your probation is confirmed"),
                "extended" => (Severity::Info, "Your probation was extended"),
                _ => (Severity::Warning, "Your probation review is decided"),
            };
            to_staff(
                env,
                &d,
                severity,
                title.into(),
                format!("Outcome of your probation review: {reason}."),
            )
        }
        CERTIFICATE_ISSUED => to_staff(
            env,
            &d,
            Severity::Success,
            "Your certificate is issued".into(),
            format!("Certificate {reason} is ready. Collect it from HR or view it in My HR."),
        ),
        ENGAGEMENT_SUBMITTED => approval_work(
            env,
            format!("Approve engagement {} ({reason})", short(&d)),
            "An engagement change awaits a decision in People → Staff records.".into(),
            ADMIN_STAFF,
        ),
        SEPARATION_SUBMITTED => approval_work(
            env,
            format!("Approve separation {reason}"),
            "A separation case awaits a decision in People → Recruitment & onboarding.".into(),
            ADMIN_HR,
        ),
        other => return Err(format!("{other}: no 7A rule")),
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
                subject: None,
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn status(to: &str, reason: &str, subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
            "toStatus": to, "version": 2, "reason": reason, "staffSubjectRef": subject})
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
    fn staff_hear_about_their_record() {
        let s = "dev|meera.shah@sutra.dev";
        for t in [
            ENGAGEMENT_ACTIVATED,
            ONBOARDING_COMPLETED,
            PROBATION_DECIDED,
            CERTIFICATE_ISSUED,
        ] {
            let a = plan(&env(t, status("x", "confirmed", Some(s)), None)).unwrap_or_default();
            assert_eq!(recipients(&a), vec![Recipient::Subject(s.into())], "{t}");
            assert!(plan(&env(t, status("x", "r", None), None))
                .unwrap_or_default()
                .is_empty());
        }
    }

    #[test]
    fn submissions_create_admin_work() {
        for t in [ENGAGEMENT_SUBMITTED, SEPARATION_SUBMITTED] {
            let a = plan(&env(
                t,
                status("pending", "activation", None),
                Some("dev|admin@sutra.dev"),
            ))
            .unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![Recipient::Role(Role::Administrator)],
                "{t}"
            );
            assert!(a.iter().any(
                |x| matches!(x, InboxAction::Todo(t) if t.assignee_subject == "dev|admin@sutra.dev")
            ));
            let auto = plan(&env(
                t,
                status("pending", "activation", None),
                Some("system:auto-approval"),
            ))
            .unwrap_or_default();
            assert_eq!(auto.len(), 1);
        }
        assert!(plan(&env(
            "sutraerp.people.other.x",
            status("x", "r", None),
            None
        ))
        .is_err());
    }
}
