//! **4C** inbox rules for `sutra_contracts::academic::events::flexible` (contract doc
//! `docs/batch4-contracts.md` §8). Flexible learning, certificates, credit recognition, resources.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `flexible_enrolment.allocated` / `flexible_enrolment.waitlisted` | notification → the student |
//! | `certificate_verification.decided` | notification → the student (accepted / rejected / flagged) |
//! | `credit_recognition.posted` | notification → the student |
//! | `course_resource.published` | notification → students (role-addressed, institution-scoped) |
//!
//! Rules are pure: student notifications need `studentSubjectRef` in the payload; without it
//! the event projects nothing. Every notification is idempotent on the event id (category
//! `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::academic::events::flexible::{
    CreditRecognitionPostedV1, CERTIFICATE_VERIFICATION_DECIDED, COURSE_RESOURCE_PUBLISHED,
    CREDIT_RECOGNITION_POSTED, FLEXIBLE_ENROLMENT_ALLOCATED, FLEXIBLE_ENROLMENT_WAITLISTED,
};
use sutra_contracts::academic::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    FLEXIBLE_ENROLMENT_ALLOCATED,
    FLEXIBLE_ENROLMENT_WAITLISTED,
    CERTIFICATE_VERIFICATION_DECIDED,
    CREDIT_RECOGNITION_POSTED,
    COURSE_RESOURCE_PUBLISHED,
];

const COURSES_LINK: &str = "/courses";
const LIBRARY_LINK: &str = "/library";

fn student(subject: Option<&str>) -> Option<Recipient> {
    subject
        .filter(|s| !s.trim().is_empty())
        .map(|s| Recipient::Subject(s.to_owned()))
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    Ok(match env.event_type.as_str() {
        FLEXIBLE_ENROLMENT_ALLOCATED | FLEXIBLE_ENROLMENT_WAITLISTED => {
            let d: StatusChangedV1 = data(env)?;
            let Some(to) = student(d.student_subject_ref.as_deref()) else {
                return Ok(Vec::new());
            };
            let (title, body, severity) = if env.event_type == FLEXIBLE_ENROLMENT_ALLOCATED {
                (
                    "Flexible learning seat allocated",
                    "A seat was allocated for your flexible-learning request. Registration follows.",
                    Severity::Info,
                )
            } else {
                (
                    "Flexible learning request waitlisted",
                    "All seats are taken; your request is on the waitlist. A seat is not guaranteed.",
                    Severity::Warning,
                )
            };
            vec![notify(
                env,
                to,
                severity,
                title.into(),
                body.into(),
                Some(COURSES_LINK),
            )]
        }
        CERTIFICATE_VERIFICATION_DECIDED => {
            let d: StatusChangedV1 = data(env)?;
            let Some(to) = student(d.student_subject_ref.as_deref()) else {
                return Ok(Vec::new());
            };
            let (title, severity) = match d.to_status.as_str() {
                "accepted" => ("Certificate accepted", Severity::Info),
                "rejected" => ("Certificate rejected", Severity::Warning),
                _ => ("Certificate under review", Severity::Warning),
            };
            let body = match d.to_status.as_str() {
                "accepted" => "Your external learning certificate was verified and accepted.".to_owned(),
                "rejected" => format!(
                    "Your external learning certificate was not accepted{}.",
                    d.reason.as_deref().map(|r| format!(": {r}")).unwrap_or_default()
                ),
                _ => "Your external learning certificate needs further review by the academic office.".to_owned(),
            };
            vec![notify(
                env,
                to,
                severity,
                title.into(),
                body,
                Some(COURSES_LINK),
            )]
        }
        CREDIT_RECOGNITION_POSTED => {
            let d: CreditRecognitionPostedV1 = data(env)?;
            let Some(to) = student(d.student_subject_ref.as_deref()) else {
                return Ok(Vec::new());
            };
            vec![notify(
                env,
                to,
                Severity::Info,
                "Credits recognised".into(),
                format!(
                    "{} credits from your flexible learning were added to your credit record.",
                    d.credits
                ),
                Some(COURSES_LINK),
            )]
        }
        COURSE_RESOURCE_PUBLISHED => {
            let _d: StatusChangedV1 = data(env)?;
            vec![notify(
                env,
                Recipient::Role(Role::Student),
                Severity::Info,
                "New course resource".into(),
                "A new resource or library link was published for one of your courses.".into(),
                Some(LIBRARY_LINK),
            )]
        }
        other => return Err(format!("{other}: no 4C rule")),
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
                source: "/sutraerp/academic",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
    }

    fn status(to: &str, subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(), "toStatus": to, "version": 1,
            "reason": "x", "studentSubjectRef": subject})
    }

    #[test]
    fn student_notifications_need_a_subject() {
        for t in [
            FLEXIBLE_ENROLMENT_ALLOCATED,
            FLEXIBLE_ENROLMENT_WAITLISTED,
            CERTIFICATE_VERIFICATION_DECIDED,
        ] {
            assert_eq!(
                plan(&env(t, status("accepted", Some("dev|s"))))
                    .unwrap()
                    .len(),
                1,
                "{t}"
            );
            assert!(
                plan(&env(t, status("accepted", None))).unwrap().is_empty(),
                "{t}"
            );
        }
        for to in ["rejected", "flagged"] {
            assert_eq!(
                plan(&env(
                    CERTIFICATE_VERIFICATION_DECIDED,
                    status(to, Some("dev|s"))
                ))
                .unwrap()
                .len(),
                1
            );
        }
        let posted = serde_json::json!({"recognitionId": Uuid::nil(), "institutionId": Uuid::nil(), "studentId": Uuid::nil(),
            "credits": 4.0, "curriculumComponentId": null, "ledgerEntryId": Uuid::nil(), "studentSubjectRef": "dev|s"});
        assert_eq!(
            plan(&env(CREDIT_RECOGNITION_POSTED, posted)).unwrap().len(),
            1
        );
        assert_eq!(
            plan(&env(COURSE_RESOURCE_PUBLISHED, status("published", None)))
                .unwrap()
                .len(),
            1
        );
        assert!(plan(&env("sutraerp.academic.other.thing", status("x", None))).is_err());
    }
}
