//! **3A** inbox rules for `sutra_contracts::student::events::{admission, record}` (contract doc
//! `docs/batch3-contracts.md` §8).
//!
//! | Event | Projection |
//! |-------|------------|
//! | `admission.approval_requested` | to-do for the requesting administrator ("Decide admission …") + notification → administrators |
//! | `admission.confirmed` / `admission.expired` / `admission.revoked` | notification → administrators |
//! | `student.created` | notification → the student (when a login is linked) |
//! | `student.enrolled` / `student.restored` | notification → the student |
//! | `student.suspended` | notification → the student + administrators |
//! | `document_request.submitted` | notification → administrators; to-do for the staff member who raised it |
//! | `document_request.issued` / `document_request.rejected` | notification → the student |
//!
//! Every projection is idempotent on the event id. Domain C reuses category `institution`.
//! Note: under the Batch 3 auto-approval the approval to-do is created even though the decision
//! was applied in the same transaction; the Flowable adapter (Batch 9) will own task creation.

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::student::events::admission::{
    ADMISSION_APPROVAL_REQUESTED, ADMISSION_CONFIRMED, ADMISSION_EXPIRED, ADMISSION_REVOKED,
};
use sutra_contracts::student::events::record::{
    StudentCreatedV1, DOCUMENT_REQUEST_ISSUED, DOCUMENT_REQUEST_REJECTED,
    DOCUMENT_REQUEST_SUBMITTED, STUDENT_CREATED, STUDENT_ENROLLED, STUDENT_RESTORED,
    STUDENT_SUSPENDED,
};
use sutra_contracts::student::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    ADMISSION_APPROVAL_REQUESTED,
    ADMISSION_CONFIRMED,
    ADMISSION_EXPIRED,
    ADMISSION_REVOKED,
    STUDENT_CREATED,
    STUDENT_ENROLLED,
    STUDENT_SUSPENDED,
    STUDENT_RESTORED,
    DOCUMENT_REQUEST_SUBMITTED,
    DOCUMENT_REQUEST_ISSUED,
    DOCUMENT_REQUEST_REJECTED,
];

const ADMISSIONS_LINK: &str = "/students/admissions";
const DOCUMENTS_LINK: &str = "/documents";

fn admins() -> Recipient {
    Recipient::Role(Role::Administrator)
}

fn student_link(id: Option<uuid::Uuid>) -> Option<String> {
    id.map(|s| format!("/students/record/{s}"))
}

/// Decides the projections of one event of [`EVENT_TYPES`] (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let t = env.event_type.as_str();
    if t == STUDENT_CREATED {
        let d: StudentCreatedV1 = data(env)?;
        return Ok(match d.student_subject_ref {
            Some(subject) => vec![notify(
                env,
                Recipient::Subject(subject),
                Severity::Success,
                "Your student record is ready".into(),
                format!(
                    "Welcome! Your student number is {}. Enrolment completes your admission.",
                    d.student_number
                ),
                Some("/documents"),
            )],
            None => vec![],
        });
    }
    let d: StatusChangedV1 = data(env)?;
    let reason = d
        .reason
        .as_deref()
        .map(|r| format!(" Reason: {r}"))
        .unwrap_or_default();
    let to_student = |severity: Severity, title: &str, body: String, link: &str| {
        d.student_subject_ref.as_ref().map(|s| {
            notify(
                env,
                Recipient::Subject(s.clone()),
                severity,
                title.to_owned(),
                body,
                Some(link),
            )
        })
    };
    Ok(match t {
        ADMISSION_APPROVAL_REQUESTED => {
            let mut v = vec![notify(
                env,
                admins(),
                Severity::Warning,
                "Admission awaits an approval decision".into(),
                "An admission completed document verification and was sent for approval.".into(),
                Some(ADMISSIONS_LINK),
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
                    title: "Decide a pending admission".into(),
                    detail: "Approve or reject the admission waiting in Students → Admissions."
                        .into(),
                    due_at: Some(env.time + Duration::days(3)),
                    link: Some(ADMISSIONS_LINK.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        ADMISSION_CONFIRMED => vec![notify(
            env,
            admins(),
            Severity::Success,
            "Admission confirmed".into(),
            "The admission is confirmed — create the student record and complete enrolment.".into(),
            Some(ADMISSIONS_LINK),
        )],
        ADMISSION_EXPIRED => vec![notify(
            env,
            admins(),
            Severity::Info,
            "Admission offer expired".into(),
            format!("An offer lapsed without acceptance.{reason}"),
            Some(ADMISSIONS_LINK),
        )],
        ADMISSION_REVOKED => vec![notify(
            env,
            admins(),
            Severity::Warning,
            "Admission revoked".into(),
            format!("An admission was revoked.{reason}"),
            Some(ADMISSIONS_LINK),
        )],
        STUDENT_ENROLLED => to_student(
            Severity::Success,
            "You are enrolled",
            "Your enrolment is complete and your student record is active.".into(),
            "/",
        )
        .into_iter()
        .collect(),
        STUDENT_SUSPENDED => {
            let mut v: Vec<InboxAction> = to_student(
                Severity::Critical,
                "Your studentship is suspended",
                format!("Your student record was suspended.{reason}"),
                "/",
            )
            .into_iter()
            .collect();
            v.push(notify(
                env,
                admins(),
                Severity::Warning,
                "Student suspended".into(),
                format!("A student was suspended.{reason}"),
                student_link(d.student_id).as_deref(),
            ));
            v
        }
        STUDENT_RESTORED => to_student(
            Severity::Success,
            "Your studentship is restored",
            "Your suspension was lifted and your student record is active again.".into(),
            "/",
        )
        .into_iter()
        .collect(),
        DOCUMENT_REQUEST_SUBMITTED => {
            let mut v = vec![notify(
                env,
                admins(),
                Severity::Info,
                "New document request".into(),
                "A student document / certificate request is waiting to be processed.".into(),
                student_link(d.student_id).as_deref(),
            )];
            let actor = env.actorref.as_deref().filter(|a| !a.is_empty());
            if let Some(actor) = actor.filter(|a| Some(*a) != d.student_subject_ref.as_deref()) {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: env.institutionid,
                    assignee_subject: actor.to_owned(),
                    title: "Process a document request".into(),
                    detail: "Start, issue or reject the student's document request.".into(),
                    due_at: Some(env.time + Duration::days(5)),
                    link: student_link(d.student_id),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        DOCUMENT_REQUEST_ISSUED => to_student(
            Severity::Success,
            "Your document is ready",
            "Your document request was issued.".into(),
            DOCUMENTS_LINK,
        )
        .into_iter()
        .collect(),
        DOCUMENT_REQUEST_REJECTED => to_student(
            Severity::Warning,
            "Document request declined",
            format!("Your document request was declined.{reason}"),
            DOCUMENTS_LINK,
        )
        .into_iter()
        .collect(),
        _ => vec![],
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
                source: "/sutraerp/student",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn changed(subject: Option<&str>) -> serde_json::Value {
        serde_json::to_value(StatusChangedV1 {
            aggregate_id: Uuid::now_v7(),
            institution_id: Uuid::now_v7(),
            student_id: Some(Uuid::now_v7()),
            from_status: Some("a".into()),
            to_status: "b".into(),
            version: 2,
            reason: Some("because".into()),
            process_instance_id: None,
            student_subject_ref: subject.map(str::to_owned),
        })
        .unwrap_or_default()
    }

    fn recipients(a: &[InboxAction]) -> Vec<String> {
        a.iter()
            .map(|x| match x {
                InboxAction::Notify(n) => match &n.recipient {
                    Recipient::Subject(s) => format!("subject:{s}"),
                    Recipient::Role(r) => format!("role:{r:?}"),
                },
                InboxAction::Todo(t) => format!("todo:{}", t.assignee_subject),
            })
            .collect()
    }

    #[test]
    fn approval_request_creates_a_todo_for_the_human_requester() {
        let a = plan(&env(
            ADMISSION_APPROVAL_REQUESTED,
            changed(None),
            Some("dev|admin"),
        ))
        .unwrap();
        assert_eq!(recipients(&a), vec!["role:Administrator", "todo:dev|admin"]);
        let a = plan(&env(
            ADMISSION_APPROVAL_REQUESTED,
            changed(None),
            Some("system:auto-approval"),
        ))
        .unwrap();
        assert_eq!(a.len(), 1);
    }

    #[test]
    fn student_notifications_need_a_linked_login() {
        let a = plan(&env(STUDENT_ENROLLED, changed(Some("dev|s")), None)).unwrap();
        assert_eq!(recipients(&a), vec!["subject:dev|s"]);
        assert!(plan(&env(STUDENT_ENROLLED, changed(None), None))
            .unwrap()
            .is_empty());
        let a = plan(&env(STUDENT_SUSPENDED, changed(Some("dev|s")), None)).unwrap();
        assert_eq!(recipients(&a), vec!["subject:dev|s", "role:Administrator"]);
        let created = serde_json::to_value(StudentCreatedV1 {
            student_id: Uuid::now_v7(),
            student_context_id: Uuid::now_v7(),
            institution_id: Uuid::now_v7(),
            admission_id: None,
            student_number: "S202600001".into(),
            programme_id: None,
            student_subject_ref: Some("dev|s".into()),
        })
        .unwrap_or_default();
        let a = plan(&env(STUDENT_CREATED, created, None)).unwrap();
        assert_eq!(recipients(&a), vec!["subject:dev|s"]);
        for t in [ADMISSION_CONFIRMED, ADMISSION_EXPIRED, ADMISSION_REVOKED] {
            assert_eq!(
                recipients(&plan(&env(t, changed(None), None)).unwrap()),
                vec!["role:Administrator"]
            );
        }
    }

    #[test]
    fn document_requests() {
        // Self-service: admins notified, no to-do for the student.
        let a = plan(&env(
            DOCUMENT_REQUEST_SUBMITTED,
            changed(Some("dev|s")),
            Some("dev|s"),
        ))
        .unwrap();
        assert_eq!(recipients(&a), vec!["role:Administrator"]);
        // Staff-raised: to-do for the staff member.
        let a = plan(&env(
            DOCUMENT_REQUEST_SUBMITTED,
            changed(Some("dev|s")),
            Some("dev|admin"),
        ))
        .unwrap();
        assert_eq!(recipients(&a), vec!["role:Administrator", "todo:dev|admin"]);
        let a = plan(&env(
            DOCUMENT_REQUEST_REJECTED,
            changed(Some("dev|s")),
            None,
        ))
        .unwrap();
        assert_eq!(recipients(&a), vec!["subject:dev|s"]);
        let a = plan(&env(DOCUMENT_REQUEST_ISSUED, changed(Some("dev|s")), None)).unwrap();
        assert_eq!(recipients(&a), vec!["subject:dev|s"]);
    }
}
