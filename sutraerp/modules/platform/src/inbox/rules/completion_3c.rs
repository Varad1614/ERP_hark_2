//! **3C** inbox rules for `sutra_contracts::student::events::{completion, guardian, scholarship, alumni}`
//! (contract doc `docs/batch3-contracts.md` §8).
//!
//! | Event | Projection |
//! |-------|------------|
//! | `qualification_completion.qualification_awarded` | notification → the student (subject) + administrators |
//! | `student_exit.completed` | notification → the student (subject) + administrators |
//! | `exit_clearance.completed` | notification → administrators (decide / complete the exit) |
//! | `guardian_relationship.proposed` | notification → administrators (verify the guardian) |
//! | `scholarship_eligibility.determined` / `renewed` / `expired` | notification → the student (subject) |
//!
//! Every notification is idempotent on the event id and keeps the event's institution scope.
//! Student notifications are skipped when the payload carries no login subject.

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::student::events::completion::{
    QualificationAwardedV1, StudentExitV1, EXIT_CLEARANCE_COMPLETED,
    QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED, STUDENT_EXIT_COMPLETED,
};
use sutra_contracts::student::events::guardian::{
    GuardianRelationshipV1, GUARDIAN_RELATIONSHIP_PROPOSED,
};
use sutra_contracts::student::events::scholarship::{
    ScholarshipEligibilityV1, SCHOLARSHIP_ELIGIBILITY_DETERMINED, SCHOLARSHIP_ELIGIBILITY_EXPIRED,
    SCHOLARSHIP_ELIGIBILITY_RENEWED,
};
use sutra_contracts::student::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED,
    STUDENT_EXIT_COMPLETED,
    EXIT_CLEARANCE_COMPLETED,
    GUARDIAN_RELATIONSHIP_PROPOSED,
    SCHOLARSHIP_ELIGIBILITY_DETERMINED,
    SCHOLARSHIP_ELIGIBILITY_RENEWED,
    SCHOLARSHIP_ELIGIBILITY_EXPIRED,
];

const EXITS: &str = "/students/exits";
const GUARDIANS: &str = "/students/guardians";

fn words(wire: &str) -> String {
    wire.replace('_', " ")
}

fn student_note(
    env: &EventEnvelope,
    subject: Option<&str>,
    severity: Severity,
    title: &str,
    body: &str,
) -> Option<InboxAction> {
    subject.map(|s| {
        notify(
            env,
            Recipient::Subject(s.to_owned()),
            severity,
            title.to_owned(),
            body.to_owned(),
            None,
        )
    })
}

/// Decides the projections of one event of [`EVENT_TYPES`] (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let admins = Recipient::Role(Role::Administrator);
    Ok(match env.event_type.as_str() {
        QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED => {
            let d: QualificationAwardedV1 = data(env)?;
            let what = words(&d.outcome);
            let mut v: Vec<InboxAction> = student_note(
                env,
                d.student_subject_ref.as_deref(),
                Severity::Success,
                &format!("Your {what} has been conferred"),
                &format!("Conferred on {}.", d.conferred_on.format("%-d %b %Y")),
            )
            .into_iter()
            .collect();
            v.push(notify(
                env,
                admins,
                Severity::Success,
                format!("Qualification conferred ({what})"),
                "Finalise the academic record and complete the student exit.".into(),
                Some(EXITS),
            ));
            v
        }
        STUDENT_EXIT_COMPLETED => {
            let d: StudentExitV1 = data(env)?;
            let kind = words(&d.kind);
            let mut v: Vec<InboxAction> = student_note(
                env,
                d.student_subject_ref.as_deref(),
                Severity::Info,
                "Your student record has been closed",
                &format!("Exit completed ({kind})."),
            )
            .into_iter()
            .collect();
            v.push(notify(
                env,
                admins,
                Severity::Info,
                format!("Student exit completed ({kind})"),
                d.reason.unwrap_or_default(),
                Some(EXITS),
            ));
            v
        }
        EXIT_CLEARANCE_COMPLETED => {
            let d: StatusChangedV1 = data(env)?;
            let body = if d.to_status == "approved" {
                "Every clearance item is resolved and the exit was approved — complete it."
                    .to_owned()
            } else {
                "Every clearance item is resolved — the exit awaits its decision.".to_owned()
            };
            vec![notify(
                env,
                admins,
                Severity::Info,
                "Exit clearance completed".into(),
                body,
                Some(EXITS),
            )]
        }
        GUARDIAN_RELATIONSHIP_PROPOSED => {
            let d: GuardianRelationshipV1 = data(env)?;
            vec![notify(
                env,
                admins,
                Severity::Info,
                format!(
                    "Guardian relationship to verify ({})",
                    words(&d.relationship_type)
                ),
                "Submit the relationship for verification with its evidence.".into(),
                Some(GUARDIANS),
            )]
        }
        SCHOLARSHIP_ELIGIBILITY_DETERMINED | SCHOLARSHIP_ELIGIBILITY_RENEWED => {
            let d: ScholarshipEligibilityV1 = data(env)?;
            let renewed = env.event_type == SCHOLARSHIP_ELIGIBILITY_RENEWED;
            let (severity, title) = match (renewed, d.status.as_str()) {
                (true, "active") => (Severity::Success, "Your scholarship was renewed".to_owned()),
                (true, s) => (
                    Severity::Warning,
                    format!("Scholarship renewal: {}", words(s)),
                ),
                (false, "eligible") => (
                    Severity::Success,
                    "You are eligible for a scholarship".to_owned(),
                ),
                (false, "conditionally_eligible") => (
                    Severity::Warning,
                    "Scholarship eligibility needs more evidence".to_owned(),
                ),
                (false, s) => (
                    Severity::Info,
                    format!("Scholarship eligibility: {}", words(s)),
                ),
            };
            let body = match d.valid_until {
                Some(u) => format!("Valid until {}.", u.format("%-d %b %Y")),
                None => "Contact the scholarship office for details.".to_owned(),
            };
            student_note(
                env,
                d.student_subject_ref.as_deref(),
                severity,
                &title,
                &body,
            )
            .into_iter()
            .collect()
        }
        SCHOLARSHIP_ELIGIBILITY_EXPIRED => {
            let d: StatusChangedV1 = data(env)?;
            student_note(
                env,
                d.student_subject_ref.as_deref(),
                Severity::Warning,
                "Your scholarship eligibility has expired",
                d.reason.as_deref().unwrap_or("The validity period ended."),
            )
            .into_iter()
            .collect()
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
                InboxAction::Notify(n) => n.recipient.clone(),
                InboxAction::Todo(t) => Recipient::Subject(t.assignee_subject.clone()),
            })
            .collect()
    }

    #[test]
    fn award_and_exit_notify_student_and_admins() {
        let e = env(
            QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED,
            serde_json::json!({"completionId": Uuid::nil(), "studentId": Uuid::nil(), "studentContextId": Uuid::nil(),
                "institutionId": Uuid::nil(), "programmeId": Uuid::nil(), "outcome": "degree", "conferredOn": "2026-10-12",
                "supersedesCompletionId": null, "processInstanceId": null, "studentSubjectRef": "dev|s@x"}),
        );
        let r = recipients(&plan(&e).unwrap_or_default());
        assert_eq!(
            r,
            vec![
                Recipient::Subject("dev|s@x".into()),
                Recipient::Role(Role::Administrator)
            ]
        );
        let e = env(
            STUDENT_EXIT_COMPLETED,
            serde_json::json!({"exitId": Uuid::nil(), "studentId": Uuid::nil(), "studentContextId": Uuid::nil(),
                "institutionId": Uuid::nil(), "kind": "withdrawal", "status": "completed", "effectiveOn": null,
                "reason": "family", "processInstanceId": null}),
        );
        assert_eq!(
            recipients(&plan(&e).unwrap_or_default()),
            vec![Recipient::Role(Role::Administrator)]
        );
    }

    #[test]
    fn scholarship_notes_need_a_subject() {
        let mk = |subject: serde_json::Value| {
            env(
                SCHOLARSHIP_ELIGIBILITY_DETERMINED,
                serde_json::json!({"eligibilityId": Uuid::nil(), "scholarshipId": Uuid::nil(), "policyId": null,
                    "studentId": Uuid::nil(), "institutionId": Uuid::nil(), "status": "eligible", "academicYearId": null,
                    "validUntil": null, "studentSubjectRef": subject}),
            )
        };
        assert_eq!(
            plan(&mk(serde_json::json!("dev|s@x")))
                .unwrap_or_default()
                .len(),
            1
        );
        assert!(plan(&mk(serde_json::Value::Null))
            .unwrap_or_default()
            .is_empty());
        let e = env(
            GUARDIAN_RELATIONSHIP_PROPOSED,
            serde_json::json!({"relationshipId": Uuid::nil(), "guardianId": Uuid::nil(), "studentId": Uuid::nil(),
                "institutionId": Uuid::nil(), "relationshipType": "legal_guardian", "hasLegalCustody": false,
                "status": "proposed", "version": 1}),
        );
        let a = plan(&e).unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
        for t in EVENT_TYPES {
            assert!(sutra_contracts::student::events::all_event_types().contains(t));
        }
    }
}
