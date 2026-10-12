//! **3B** inbox rules for `sutra_contracts::student::events::{registration, progression}` (contract doc
//! `docs/batch3-contracts.md` §8).
//!
//! | Event | Projection |
//! |-------|------------|
//! | `registration_window.opened` | notification → students (institution-scoped) |
//! | `registration.finalised` | notification → the student |
//! | `registration_hold.placed` / `released` | notification → the student |
//! | `progression.academic_probation_assigned` / `academic_detention_applied` / `conditional_promotion_granted` | notification → the student + administrators |
//!
//! Student notifications need `studentSubjectRef` in the payload (no lookup: rules are pure); a
//! student without a linked login gets none. Every notification is idempotent on the event id.

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::student::events::progression::{
    ProgressionDecisionV1, PROGRESSION_ACADEMIC_DETENTION_APPLIED,
    PROGRESSION_ACADEMIC_PROBATION_ASSIGNED, PROGRESSION_CONDITIONAL_PROMOTION_GRANTED,
};
use sutra_contracts::student::events::registration::{
    RegistrationHoldV1, RegistrationV1, REGISTRATION_FINALISED, REGISTRATION_HOLD_PLACED,
    REGISTRATION_HOLD_RELEASED, REGISTRATION_WINDOW_OPENED,
};
use sutra_contracts::student::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    REGISTRATION_WINDOW_OPENED,
    REGISTRATION_FINALISED,
    REGISTRATION_HOLD_PLACED,
    REGISTRATION_HOLD_RELEASED,
    PROGRESSION_ACADEMIC_PROBATION_ASSIGNED,
    PROGRESSION_ACADEMIC_DETENTION_APPLIED,
    PROGRESSION_CONDITIONAL_PROMOTION_GRANTED,
];

const STUDENT_LINK: &str = "/registration";

fn humanise(s: &str) -> String {
    let s = s.replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => s,
    }
}

fn to_student(
    env: &EventEnvelope,
    subject: Option<&str>,
    severity: Severity,
    title: &str,
    body: &str,
) -> Vec<InboxAction> {
    subject
        .filter(|s| !s.is_empty())
        .map(|s| {
            notify(
                env,
                Recipient::Subject(s.to_owned()),
                severity,
                title.to_owned(),
                body.to_owned(),
                Some(STUDENT_LINK),
            )
        })
        .into_iter()
        .collect()
}

/// Decides the projections of one event of [`EVENT_TYPES`] (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    Ok(match env.event_type.as_str() {
        REGISTRATION_WINDOW_OPENED => {
            let _d: StatusChangedV1 = data(env)?;
            vec![notify(
                env,
                Recipient::Role(Role::Student),
                Severity::Info,
                "Registration is open".into(),
                "The academic registration window is open. Choose your courses and submit your registration before it closes.".into(),
                Some(STUDENT_LINK),
            )]
        }
        REGISTRATION_FINALISED => {
            let d: RegistrationV1 = data(env)?;
            to_student(
                env,
                d.student_subject_ref.as_deref(),
                Severity::Success,
                "Your registration is final",
                &format!(
                    "Your registration of {} component(s), {} credits, is finalised and published.",
                    d.component_ids.len(),
                    d.total_credits
                ),
            )
        }
        REGISTRATION_HOLD_PLACED | REGISTRATION_HOLD_RELEASED => {
            let d: RegistrationHoldV1 = data(env)?;
            let placed = env.event_type == REGISTRATION_HOLD_PLACED;
            let (sev, title, body) = if placed {
                (
                    Severity::Warning,
                    format!("{} hold on your registration", humanise(&d.source)),
                    format!("You cannot register until it is released: {}", d.reason),
                )
            } else {
                (
                    Severity::Success,
                    format!("{} hold released", humanise(&d.source)),
                    "The hold on your registration has been released.".to_owned(),
                )
            };
            to_student(env, d.student_subject_ref.as_deref(), sev, &title, &body)
        }
        PROGRESSION_ACADEMIC_PROBATION_ASSIGNED
        | PROGRESSION_ACADEMIC_DETENTION_APPLIED
        | PROGRESSION_CONDITIONAL_PROMOTION_GRANTED => {
            let d: ProgressionDecisionV1 = data(env)?;
            let (sev, title, body) = match env.event_type.as_str() {
                PROGRESSION_ACADEMIC_PROBATION_ASSIGNED => (
                    Severity::Warning,
                    "Academic probation",
                    "You are on academic probation. Speak to your academic advisor about the review.",
                ),
                PROGRESSION_ACADEMIC_DETENTION_APPLIED => (
                    Severity::Critical,
                    "Academic detention",
                    "Progression requirements were not met: you are detained and register repeat or backlog components only.",
                ),
                _ => (
                    Severity::Info,
                    "Conditional promotion",
                    "You progressed to the next term with conditions. Fulfil them by their due dates.",
                ),
            };
            let mut v = to_student(env, d.student_subject_ref.as_deref(), sev, title, body);
            v.push(notify(
                env,
                Recipient::Role(Role::Administrator),
                sev,
                format!("{title} recorded"),
                format!(
                    "A progression decision ({}) is effective for a student.",
                    humanise(&d.outcome)
                ),
                Some(&format!("/students/record/{}/progression", d.student_id)),
            ));
            v
        }
        _ => vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    fn env(event_type: &'static str, data: serde_json::Value) -> EventEnvelope {
        EventEnvelope::new(
            &ExecutionScope::institution(TenantId::new(), InstitutionId::new()),
            NewEvent {
                source: "/sutraerp/student",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
    }

    fn recipients(a: &[InboxAction]) -> Vec<Recipient> {
        a.iter()
            .map(|x| match x {
                InboxAction::Notify(n) => {
                    assert!(n.institution.is_some(), "institution-scoped");
                    n.recipient.clone()
                }
                InboxAction::Todo(_) => panic!("no to-dos expected"),
            })
            .collect()
    }

    #[test]
    fn window_opened_notifies_students() {
        let e = env(
            REGISTRATION_WINDOW_OPENED,
            serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(), "toStatus": "open", "version": 2}),
        );
        let a = plan(&e).unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Student)]);
    }

    #[test]
    fn student_events_need_a_subject() {
        let hold = |subject: Option<&str>| {
            serde_json::json!({"holdId": Uuid::nil(), "studentId": Uuid::nil(), "institutionId": Uuid::nil(),
                "source": "finance", "reason": "Dues", "status": "active", "studentSubjectRef": subject})
        };
        let a = plan(&env(
            REGISTRATION_HOLD_PLACED,
            hold(Some("dev|aarav.desai@sutra.dev")),
        ))
        .unwrap_or_default();
        assert_eq!(
            recipients(&a),
            vec![Recipient::Subject("dev|aarav.desai@sutra.dev".into())]
        );
        assert!(plan(&env(REGISTRATION_HOLD_RELEASED, hold(None)))
            .unwrap_or_default()
            .is_empty());
        let reg = serde_json::json!({"registrationId": Uuid::nil(), "studentId": Uuid::nil(), "studentContextId": Uuid::nil(),
            "institutionId": Uuid::nil(), "academicTermId": Uuid::nil(), "status": "finalised", "componentIds": [],
            "totalCredits": 20.0, "version": 5, "studentSubjectRef": "dev|s"});
        assert_eq!(
            plan(&env(REGISTRATION_FINALISED, reg))
                .unwrap_or_default()
                .len(),
            1
        );
    }

    #[test]
    fn progression_outcomes_notify_student_and_admins() {
        let d = serde_json::json!({"decisionId": Uuid::nil(), "studentId": Uuid::nil(), "studentContextId": Uuid::nil(),
            "institutionId": Uuid::nil(), "academicTermId": null, "outcome": "probation", "policyId": null,
            "supersedesDecisionId": null, "processInstanceId": null, "studentSubjectRef": "dev|s"});
        for t in [
            PROGRESSION_ACADEMIC_PROBATION_ASSIGNED,
            PROGRESSION_ACADEMIC_DETENTION_APPLIED,
            PROGRESSION_CONDITIONAL_PROMOTION_GRANTED,
        ] {
            let a = plan(&env(t, d.clone())).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![
                    Recipient::Subject("dev|s".into()),
                    Recipient::Role(Role::Administrator)
                ]
            );
        }
        assert!(plan(&env(REGISTRATION_FINALISED, serde_json::json!({}))).is_err());
    }
}
