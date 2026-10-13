//! **6C** inbox rules for `sutra_contracts::exams::events::results` (contract doc
//! `docs/batch6-contracts.md` §8). Examinations: results, publication, grade cards, revaluation.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `term_result.published` / `term_result.revised` | notification → the student (link `/results`) |
//! | `term_result.withdrawn` | notification → the student |
//! | `revaluation_request.decided` / `revaluation_request.completed` | notification → the student |
//! | `result_batch.computed` | notification → administrators; to-do for the actor "Validate and approve results …" |
//!
//! Student notifications need `studentSubjectRef` in the payload (rules are pure, no lookup)
//! and are skipped without it; notifications are idempotent on the event id (category
//! `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::exams::events::results::{
    RESULT_COMPUTED, RESULT_PUBLISHED, RESULT_REVISED, RESULT_WITHDRAWN, REVALUATION_COMPLETED,
    REVALUATION_DECIDED,
};
use sutra_contracts::exams::events::{ResultPublishedV1, StatusChangedV1};
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    RESULT_PUBLISHED,
    RESULT_REVISED,
    RESULT_WITHDRAWN,
    REVALUATION_DECIDED,
    REVALUATION_COMPLETED,
    RESULT_COMPUTED,
];

const RESULTS: &str = "/results";
const ADMIN_RESULTS: &str = "/examinations/results";

fn to_student(
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
                Some(RESULTS),
            )
        })
        .into_iter()
        .collect()
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    Ok(match env.event_type.as_str() {
        RESULT_PUBLISHED | RESULT_REVISED => {
            let d: ResultPublishedV1 = data(env)?;
            let sgpa = d
                .sgpa
                .as_deref()
                .map(|s| format!(" — SGPA {s}"))
                .unwrap_or_default();
            let (title, body) = if env.event_type == RESULT_REVISED {
                (
                    format!("Your result was revised{sgpa}"),
                    format!(
                        "A revised result (revision {}) replaces the earlier one.",
                        d.revision
                    ),
                )
            } else {
                (
                    format!("Your result is published{sgpa}"),
                    "View your marks, grades and grade card under Results.".to_owned(),
                )
            };
            to_student(
                env,
                d.student_subject_ref.as_deref(),
                Severity::Success,
                title,
                body,
            )
        }
        RESULT_WITHDRAWN => {
            let d: StatusChangedV1 = data(env)?;
            to_student(
                env,
                d.student_subject_ref.as_deref(),
                Severity::Warning,
                "Your result was withdrawn".into(),
                d.reason
                    .clone()
                    .unwrap_or_else(|| "The examinations office withdrew a result.".into()),
            )
        }
        REVALUATION_DECIDED => {
            let d: StatusChangedV1 = data(env)?;
            let approved = d.to_status == "approved";
            to_student(
                env,
                d.student_subject_ref.as_deref(),
                if approved {
                    Severity::Info
                } else {
                    Severity::Warning
                },
                if approved {
                    "Your review request was approved".into()
                } else {
                    "Your review request was rejected".into()
                },
                d.reason.clone().unwrap_or_default(),
            )
        }
        REVALUATION_COMPLETED => {
            let d: StatusChangedV1 = data(env)?;
            to_student(
                env,
                d.student_subject_ref.as_deref(),
                Severity::Success,
                "Your review is complete".into(),
                d.reason.clone().unwrap_or_default(),
            )
        }
        RESULT_COMPUTED => {
            let d: StatusChangedV1 = data(env)?;
            let what = format!(
                "Validate and approve results ({})",
                d.reason.clone().unwrap_or_default()
            );
            let mut v = vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Info,
                "Results computed".into(),
                what.clone(),
                Some(ADMIN_RESULTS),
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
                    detail: "Review, validate and approve the computed results in Examinations → Results."
                        .into(),
                    due_at: Some(env.time + Duration::days(3)),
                    link: Some(ADMIN_RESULTS.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        other => return Err(format!("{other}: no 6C rule")),
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
                source: "/sutraerp/exams",
                event_type: t,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn published(subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({"resultId": Uuid::nil(), "institutionId": Uuid::nil(),
            "sessionId": Uuid::nil(), "studentId": Uuid::nil(), "academicTermId": Uuid::nil(),
            "sgpa": "8.45", "cgpa": "8.10", "creditsAttempted": "20.00",
            "creditsEarned": "20.00", "outcome": "pass", "revision": 1,
            "studentSubjectRef": subject})
    }

    fn status(to: &str, subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
            "toStatus": to, "version": 2, "reason": "r", "studentSubjectRef": subject})
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
    fn students_hear_about_their_results_and_reviews() {
        let s = "dev|aarav.desai@sutra.dev";
        for t in [RESULT_PUBLISHED, RESULT_REVISED] {
            let a = plan(&env(t, published(Some(s)), None)).unwrap_or_default();
            assert_eq!(recipients(&a), vec![Recipient::Subject(s.into())], "{t}");
            assert!(plan(&env(t, published(None), None))
                .unwrap_or_default()
                .is_empty());
        }
        let a = plan(&env(RESULT_PUBLISHED, published(Some(s)), None)).unwrap_or_default();
        match &a[0] {
            InboxAction::Notify(n) => assert_eq!(n.title, "Your result is published — SGPA 8.45"),
            InboxAction::Todo(_) => panic!("notification expected"),
        }
        for (t, to) in [
            (RESULT_WITHDRAWN, "withdrawn"),
            (REVALUATION_DECIDED, "approved"),
            (REVALUATION_COMPLETED, "closed"),
        ] {
            let a = plan(&env(t, status(to, Some(s)), None)).unwrap_or_default();
            assert_eq!(recipients(&a), vec![Recipient::Subject(s.into())], "{t}");
        }
    }

    #[test]
    fn computed_results_create_admin_work() {
        let a = plan(&env(
            RESULT_COMPUTED,
            status("computed", None),
            Some("dev|admin@sutra.dev"),
        ))
        .unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
        assert!(a.iter().any(
            |x| matches!(x, InboxAction::Todo(t) if t.assignee_subject == "dev|admin@sutra.dev")
        ));
        assert!(plan(&env("sutraerp.exams.other.x", status("x", None), None)).is_err());
    }
}
