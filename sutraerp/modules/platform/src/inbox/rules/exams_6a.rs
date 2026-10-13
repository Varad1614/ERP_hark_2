//! **6A** inbox rules for `sutra_contracts::exams::events::setup` (contract doc
//! `docs/batch6-contracts.md` §8). Examinations: policy, sessions, timetables, seating, invigilation, registrations, hall tickets, conduct.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `exam_schedule.published` | notification → students (link `/results`) and faculty (link `/assessments`) |
//! | `exam_schedule.withdrawn` | notification → students and faculty (L3-I-016) |
//! | `hall_ticket.issued` / `hall_ticket.withheld` | notification → the student (link `/results`) |
//! | `exam_registration.rejected` | notification → the student |
//! | `exam_session.submitted` (awaiting a decision) | notification → administrators + to-do for the staff submitter |
//!
//! Student notifications need `studentSubjectRef` in the payload (rules are pure, no lookup);
//! notifications are idempotent on the event id (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::exams::events::setup::{
    EXAM_REGISTRATION_REJECTED, EXAM_SESSION_SUBMITTED, EXAM_TIMETABLE_PUBLISHED,
    EXAM_TIMETABLE_WITHDRAWN, HALL_TICKET_ISSUED, HALL_TICKET_WITHHELD,
};
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    EXAM_TIMETABLE_PUBLISHED,
    EXAM_TIMETABLE_WITHDRAWN,
    HALL_TICKET_ISSUED,
    HALL_TICKET_WITHHELD,
    EXAM_REGISTRATION_REJECTED,
    EXAM_SESSION_SUBMITTED,
];

const RESULTS_LINK: &str = "/results";
const ASSESSMENTS_LINK: &str = "/assessments";
const ADMIN_LINK: &str = "/examinations/sessions";
/// Reason the session API records on automatic approvals (no decision is awaited).
const AUTO_APPROVED: &str = "auto-approved";

fn to_student(
    env: &EventEnvelope,
    d: &StatusChangedV1,
    severity: Severity,
    title: &str,
    body: String,
) -> Vec<InboxAction> {
    d.student_subject_ref
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(|s| {
            vec![notify(
                env,
                Recipient::Subject(s.to_owned()),
                severity,
                title.into(),
                body,
                Some(RESULTS_LINK),
            )]
        })
        .unwrap_or_default()
}

fn timetable(
    env: &EventEnvelope,
    severity: Severity,
    title: &str,
    body: String,
) -> Vec<InboxAction> {
    vec![
        notify(
            env,
            Recipient::Role(Role::Student),
            severity,
            title.into(),
            body.clone(),
            Some(RESULTS_LINK),
        ),
        notify(
            env,
            Recipient::Role(Role::Faculty),
            severity,
            title.into(),
            body,
            Some(ASSESSMENTS_LINK),
        ),
    ]
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let what = d.reason.clone().unwrap_or_default();
    Ok(match env.event_type.as_str() {
        EXAM_TIMETABLE_PUBLISHED => timetable(
            env,
            Severity::Info,
            "Examination timetable published",
            format!("The official examination timetable {what} is available."),
        ),
        EXAM_TIMETABLE_WITHDRAWN => timetable(
            env,
            Severity::Warning,
            "Examination timetable withdrawn",
            format!(
                "The examination timetable was withdrawn: {what}. A revised timetable will follow."
            ),
        ),
        HALL_TICKET_ISSUED => to_student(
            env,
            &d,
            Severity::Info,
            "Hall ticket issued",
            format!("Your hall ticket {what} is ready on the Results page."),
        ),
        HALL_TICKET_WITHHELD => to_student(
            env,
            &d,
            Severity::Warning,
            "Hall ticket withheld",
            format!("Your hall ticket is withheld: {what}. Resolve the condition to receive it."),
        ),
        EXAM_REGISTRATION_REJECTED => to_student(
            env,
            &d,
            Severity::Warning,
            "Examination registration rejected",
            format!("Your examination registration was not accepted: {what}."),
        ),
        EXAM_SESSION_SUBMITTED => {
            if what == AUTO_APPROVED {
                return Ok(Vec::new());
            }
            let mut v = vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Info,
                "Examination session awaiting approval".into(),
                format!("Examination session {what} was submitted for approval."),
                Some(ADMIN_LINK),
            )];
            if let Some(a) = env
                .actorref
                .as_deref()
                .filter(|a| !a.is_empty() && !a.starts_with("system:"))
            {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: env.institutionid,
                    assignee_subject: a.to_owned(),
                    title: format!("Approve examination session {what}"),
                    detail: "Review the session and approve or return it in Admin → Examinations → Sessions.".into(),
                    due_at: Some(env.time + Duration::days(3)),
                    link: Some(ADMIN_LINK.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        other => return Err(format!("{other}: no 6A rule")),
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
                source: "/sutraerp/exams",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn payload(reason: &str, subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "aggregateId": Uuid::nil(), "institutionId": Uuid::nil(), "toStatus": "active",
            "version": 2, "reason": reason, "studentSubjectRef": subject
        })
    }

    #[test]
    fn student_rules_need_a_subject() {
        for t in [
            HALL_TICKET_ISSUED,
            HALL_TICKET_WITHHELD,
            EXAM_REGISTRATION_REJECTED,
        ] {
            assert_eq!(
                plan(&env(t, payload("HT-000001", Some("stu")), None))
                    .unwrap_or_default()
                    .len(),
                1,
                "{t}"
            );
            assert!(
                plan(&env(t, payload("HT-000001", None), None))
                    .unwrap_or_default()
                    .is_empty(),
                "{t}"
            );
        }
    }

    #[test]
    fn timetable_reaches_students_and_faculty() {
        assert_eq!(
            plan(&env(EXAM_TIMETABLE_PUBLISHED, payload("v1", None), None))
                .unwrap_or_default()
                .len(),
            2
        );
        assert_eq!(
            plan(&env(EXAM_TIMETABLE_WITHDRAWN, payload("venue", None), None))
                .unwrap_or_default()
                .len(),
            2
        );
    }

    #[test]
    fn session_submission_awaiting_a_decision() {
        let pending = plan(&env(
            EXAM_SESSION_SUBMITTED,
            payload("ESE — End sem", None),
            Some("admin-1"),
        ))
        .unwrap_or_default();
        assert_eq!(pending.len(), 2);
        assert!(matches!(pending[1], InboxAction::Todo(_)));
        let auto = plan(&env(
            EXAM_SESSION_SUBMITTED,
            payload(AUTO_APPROVED, None),
            Some("admin-1"),
        ))
        .unwrap_or_default();
        assert!(auto.is_empty());
        assert!(plan(&env(EXAM_SESSION_SUBMITTED, serde_json::json!({}), None)).is_err());
    }
}
