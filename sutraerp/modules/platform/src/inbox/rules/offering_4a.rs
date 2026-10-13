//! **4A** inbox rules for `sutra_contracts::academic::events::offering` (contract doc
//! `docs/batch4-contracts.md` §8). Course offerings, enrolment, faculty assignment.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `course_offering.submitted` awaiting a decision | to-do → the submitting administrator ("Decide offering …") |
//! | `course_offering.published` / `enrollment_opened` | notification → students (institution-scoped) |
//! | `section_enrolment.enrolled` / `dropped` / `withdrawn` | notification → the student |
//! | `faculty_assignment.activated` / `ended` | notification → the faculty member |
//!
//! Student notifications need `studentSubjectRef` in the payload (rules are pure, no lookup);
//! a student without a linked login gets none. Every notification is idempotent on the event id.

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::academic::events::offering::{
    CourseOfferingV1, FacultyAssignmentV1, SectionEnrolmentV1, COURSE_OFFERING_ENROLLMENT_OPENED,
    COURSE_OFFERING_PUBLISHED, COURSE_OFFERING_SUBMITTED, FACULTY_ASSIGNMENT_ACTIVATED,
    FACULTY_ASSIGNMENT_ENDED, SECTION_ENROLMENT_DROPPED, SECTION_ENROLMENT_ENROLLED,
    SECTION_ENROLMENT_WITHDRAWN,
};
use sutra_contracts::academic::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    COURSE_OFFERING_SUBMITTED,
    COURSE_OFFERING_PUBLISHED,
    COURSE_OFFERING_ENROLLMENT_OPENED,
    SECTION_ENROLMENT_ENROLLED,
    SECTION_ENROLMENT_DROPPED,
    SECTION_ENROLMENT_WITHDRAWN,
    FACULTY_ASSIGNMENT_ACTIVATED,
    FACULTY_ASSIGNMENT_ENDED,
];

const ADMIN_LINK: &str = "/academic-delivery/offerings";
const STUDENT_LINK: &str = "/courses";
const FACULTY_LINK: &str = "/classes";

/// Decides the projections of one event of [`EVENT_TYPES`] (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    Ok(match env.event_type.as_str() {
        COURSE_OFFERING_SUBMITTED => {
            let d: StatusChangedV1 = data(env)?;
            let awaiting = d.reason.as_deref() != Some("auto-approved");
            match env.actorref.as_deref().filter(|a| !a.is_empty() && !a.starts_with("system:")) {
                Some(actor) if awaiting => vec![InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: env.institutionid,
                    assignee_subject: actor.to_owned(),
                    title: "Decide a pending course offering".into(),
                    detail: "Approve or reject the course offering waiting in Academic delivery → Offerings.".into(),
                    due_at: Some(env.time + Duration::days(3)),
                    link: Some(ADMIN_LINK.into()),
                    source_ref: env.id.to_string(),
                })],
                _ => Vec::new(),
            }
        }
        COURSE_OFFERING_PUBLISHED => {
            let d: CourseOfferingV1 = data(env)?;
            vec![notify(
                env,
                Recipient::Role(Role::Student),
                Severity::Info,
                format!("New course offering: {}", d.title),
                format!(
                    "{} ({} credits) is published for the coming term.",
                    d.offering_code, d.credits
                ),
                Some(STUDENT_LINK),
            )]
        }
        COURSE_OFFERING_ENROLLMENT_OPENED => {
            let _d: StatusChangedV1 = data(env)?;
            vec![notify(
                env,
                Recipient::Role(Role::Student),
                Severity::Info,
                "Course enrolment is open".into(),
                "A course offering is open for enrolment. If your registration covers it, take your seat in Courses.".into(),
                Some(STUDENT_LINK),
            )]
        }
        SECTION_ENROLMENT_ENROLLED | SECTION_ENROLMENT_DROPPED | SECTION_ENROLMENT_WITHDRAWN => {
            let d: SectionEnrolmentV1 = data(env)?;
            let (severity, title, body) = match env.event_type.as_str() {
                SECTION_ENROLMENT_ENROLLED => (
                    Severity::Success,
                    "You are enrolled",
                    "You hold a seat in a course section. It now appears in your courses.",
                ),
                SECTION_ENROLMENT_DROPPED => (
                    Severity::Info,
                    "Course dropped",
                    "Your seat in a course section was dropped.",
                ),
                _ => (
                    Severity::Warning,
                    "Withdrawn from a course",
                    "You were withdrawn from a course section.",
                ),
            };
            d.student_subject_ref
                .filter(|s| !s.is_empty())
                .map(|s| {
                    notify(
                        env,
                        Recipient::Subject(s),
                        severity,
                        title.into(),
                        body.into(),
                        Some(STUDENT_LINK),
                    )
                })
                .into_iter()
                .collect()
        }
        FACULTY_ASSIGNMENT_ACTIVATED | FACULTY_ASSIGNMENT_ENDED => {
            let d: FacultyAssignmentV1 = data(env)?;
            let activated = env.event_type == FACULTY_ASSIGNMENT_ACTIVATED;
            let role = d.role.replace('_', " ");
            let (severity, title, body) = if activated {
                (
                    Severity::Success,
                    "New teaching assignment".to_owned(),
                    format!(
                        "You are now the {role} of a course section. It appears in My classes."
                    ),
                )
            } else {
                (
                    Severity::Info,
                    "Teaching assignment ended".to_owned(),
                    format!(
                        "Your assignment as {role} of a course section ended ({}).",
                        d.status
                    ),
                )
            };
            if d.faculty_subject_ref.is_empty() {
                Vec::new()
            } else {
                vec![notify(
                    env,
                    Recipient::Subject(d.faculty_subject_ref),
                    severity,
                    title,
                    body,
                    Some(FACULTY_LINK),
                )]
            }
        }
        other => return Err(format!("{other}: no 4A rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId};
    use uuid::Uuid;

    fn env(
        event_type: &'static str,
        data: serde_json::Value,
        actor: Option<&str>,
    ) -> EventEnvelope {
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
        .with_actor(actor.map(str::to_owned))
    }

    fn status(reason: &str) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(), "toStatus": "pending_approval", "version": 2, "reason": reason})
    }

    #[test]
    fn submitted_awaiting_creates_a_todo_for_the_submitter() {
        let a = plan(&env(
            COURSE_OFFERING_SUBMITTED,
            status("awaiting decision"),
            Some("dev|admin@sutra.dev"),
        ))
        .unwrap_or_default();
        assert_eq!(a.len(), 1);
        assert!(
            matches!(&a[0], InboxAction::Todo(t) if t.assignee_subject == "dev|admin@sutra.dev")
        );
        assert!(plan(&env(
            COURSE_OFFERING_SUBMITTED,
            status("auto-approved"),
            Some("dev|admin@sutra.dev")
        ))
        .unwrap_or_default()
        .is_empty());
        assert!(plan(&env(
            COURSE_OFFERING_SUBMITTED,
            status("awaiting decision"),
            Some("system:auto-approval")
        ))
        .unwrap_or_default()
        .is_empty());
    }

    #[test]
    fn publication_and_seats_notify() {
        let o = serde_json::json!({"offeringId": Uuid::nil(), "institutionId": Uuid::nil(), "campusId": null, "academicTermId": Uuid::nil(),
            "curriculumComponentId": Uuid::nil(), "offeringCode": "CS101-T1", "title": "Programming", "credits": 4.0, "status": "published", "version": 3});
        let a = plan(&env(COURSE_OFFERING_PUBLISHED, o, None)).unwrap_or_default();
        assert!(
            matches!(&a[0], InboxAction::Notify(n) if n.recipient == Recipient::Role(Role::Student) && n.institution.is_some())
        );
        let seat = |s: Option<&str>| {
            serde_json::json!({"enrolmentId": Uuid::nil(), "sectionId": Uuid::nil(), "offeringId": Uuid::nil(),
            "institutionId": Uuid::nil(), "studentId": Uuid::nil(), "studentContextId": Uuid::nil(), "status": "enrolled", "reason": null, "studentSubjectRef": s})
        };
        let a = plan(&env(
            SECTION_ENROLMENT_ENROLLED,
            seat(Some("dev|aarav.desai@sutra.dev")),
            None,
        ))
        .unwrap_or_default();
        assert!(
            matches!(&a[0], InboxAction::Notify(n) if n.recipient == Recipient::Subject("dev|aarav.desai@sutra.dev".into()))
        );
        assert!(plan(&env(SECTION_ENROLMENT_WITHDRAWN, seat(None), None))
            .unwrap_or_default()
            .is_empty());
        let fa = serde_json::json!({"assignmentId": Uuid::nil(), "sectionId": Uuid::nil(), "offeringId": Uuid::nil(), "institutionId": Uuid::nil(),
            "facultySubjectRef": "dev|meera.shah@sutra.dev", "role": "primary_instructor", "status": "active", "version": 4});
        let a = plan(&env(FACULTY_ASSIGNMENT_ACTIVATED, fa, None)).unwrap_or_default();
        assert!(
            matches!(&a[0], InboxAction::Notify(n) if n.recipient == Recipient::Subject("dev|meera.shah@sutra.dev".into()))
        );
        assert!(plan(&env(COURSE_OFFERING_ENROLLMENT_OPENED, status("x"), None)).is_ok());
    }
}
