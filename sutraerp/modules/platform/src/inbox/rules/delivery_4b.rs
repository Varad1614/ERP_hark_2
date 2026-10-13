//! **4B** inbox rules for `sutra_contracts::academic::events::delivery` (contract doc
//! `docs/batch4-contracts.md` §8). Timetable, attendance thresholds, coursework, monitoring.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `timetable.published` / `timetable.revised` | notification → students + faculty (role-addressed, institution-scoped) |
//! | `attendance.threshold_breached` | notification → the student (when linked to a login) + administrators |
//! | `assessment_window.opened` | notification → students (role-addressed) |
//! | `monitoring_intervention.triggered` | to-do → the intervention owner (else the actor) + notification → administrators |
//!
//! Rules are pure: student notifications need `studentSubjectRef` in the payload. Every
//! notification is idempotent on the event id (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::academic::events::delivery::{
    AttendanceThresholdV1, ASSESSMENT_WINDOW_OPENED, ATTENDANCE_THRESHOLD_BREACHED,
    MONITORING_INTERVENTION_TRIGGERED, TIMETABLE_PUBLISHED, TIMETABLE_REVISED,
};
use sutra_contracts::academic::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    TIMETABLE_PUBLISHED,
    TIMETABLE_REVISED,
    ATTENDANCE_THRESHOLD_BREACHED,
    ASSESSMENT_WINDOW_OPENED,
    MONITORING_INTERVENTION_TRIGGERED,
];

const TIMETABLE_LINK: &str = "/timetable";
const ASSIGNMENTS_LINK: &str = "/assignments";
const MONITORING_LINK: &str = "/academic-delivery/monitoring";
const ATTENDANCE_ADMIN_LINK: &str = "/academic-delivery/attendance";

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    Ok(match env.event_type.as_str() {
        TIMETABLE_PUBLISHED | TIMETABLE_REVISED => {
            let _d: StatusChangedV1 = data(env)?;
            let (title, body) = if env.event_type == TIMETABLE_PUBLISHED {
                (
                    "Timetable published",
                    "A new timetable was published. Check your weekly classes.",
                )
            } else {
                (
                    "Timetable revised",
                    "The timetable was revised. Check your updated weekly classes.",
                )
            };
            [Role::Student, Role::Faculty]
                .into_iter()
                .map(|r| {
                    notify(
                        env,
                        Recipient::Role(r),
                        Severity::Info,
                        title.into(),
                        body.into(),
                        Some(TIMETABLE_LINK),
                    )
                })
                .collect()
        }
        ATTENDANCE_THRESHOLD_BREACHED => {
            let d: AttendanceThresholdV1 = data(env)?;
            let mut v = Vec::new();
            if let Some(s) = d.student_subject_ref.as_deref().filter(|s| !s.is_empty()) {
                v.push(notify(
                    env,
                    Recipient::Subject(s.to_owned()),
                    Severity::Warning,
                    "Your attendance is below the required level".into(),
                    format!(
                        "Your attendance is {:.1}% against the required {:.0}%. Attend upcoming sessions to stay eligible.",
                        d.attendance_percent, d.threshold_percent
                    ),
                    Some(TIMETABLE_LINK),
                ));
            }
            v.push(notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Warning,
                "Attendance threshold breached".into(),
                format!(
                    "A student's attendance fell to {:.1}% (threshold {:.0}%) after {} session(s).",
                    d.attendance_percent, d.threshold_percent, d.sessions_counted
                ),
                Some(ATTENDANCE_ADMIN_LINK),
            ));
            v
        }
        ASSESSMENT_WINDOW_OPENED => {
            let d: StatusChangedV1 = data(env)?;
            let what = d.reason.unwrap_or_else(|| "An assessment".into());
            vec![notify(
                env,
                Recipient::Role(Role::Student),
                Severity::Info,
                "New coursework is open".into(),
                format!("{what} is open for submission. Check the due date."),
                Some(ASSIGNMENTS_LINK),
            )]
        }
        MONITORING_INTERVENTION_TRIGGERED => {
            let d: StatusChangedV1 = data(env)?;
            let what = d
                .reason
                .clone()
                .unwrap_or_else(|| "Corrective intervention".into());
            let mut v = vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Warning,
                "Monitoring intervention triggered".into(),
                format!("{what} was triggered from an approved governance decision."),
                Some(MONITORING_LINK),
            )];
            let assignee = d
                .faculty_subject_ref
                .as_deref()
                .or(env.actorref.as_deref())
                .filter(|a| !a.is_empty());
            if let Some(a) = assignee {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: env.institutionid,
                    assignee_subject: a.to_owned(),
                    title: format!("Carry out: {what}"),
                    detail: "Start the corrective action and record its outcome in Academic delivery → Monitoring.".into(),
                    due_at: Some(env.time + Duration::days(7)),
                    link: Some(MONITORING_LINK.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        other => return Err(format!("{other}: no 4B rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId};
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

    fn status(reason: Option<&str>, faculty: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(), "toStatus": "x", "version": 1,
            "reason": reason, "facultySubjectRef": faculty})
    }

    fn recipients(a: &[InboxAction]) -> Vec<Recipient> {
        a.iter()
            .filter_map(|x| match x {
                InboxAction::Notify(n) => {
                    assert!(n.institution.is_some(), "institution-scoped");
                    Some(n.recipient.clone())
                }
                InboxAction::Todo(_) => None,
            })
            .collect()
    }

    #[test]
    fn timetable_events_notify_students_and_faculty() {
        for t in [TIMETABLE_PUBLISHED, TIMETABLE_REVISED] {
            let a = plan(&env(t, status(None, None))).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![
                    Recipient::Role(Role::Student),
                    Recipient::Role(Role::Faculty)
                ]
            );
        }
    }

    #[test]
    fn threshold_breach_notifies_student_and_admins() {
        let d = |s: Option<&str>| {
            serde_json::json!({"sectionId": Uuid::nil(), "offeringId": Uuid::nil(), "institutionId": Uuid::nil(),
            "studentId": Uuid::nil(), "academicTermId": Uuid::nil(), "attendancePercent": 50.0, "thresholdPercent": 75.0,
            "sessionsCounted": 4, "studentSubjectRef": s})
        };
        let a = plan(&env(
            ATTENDANCE_THRESHOLD_BREACHED,
            d(Some("dev|aarav.desai@sutra.dev")),
        ))
        .unwrap_or_default();
        assert_eq!(
            recipients(&a),
            vec![
                Recipient::Subject("dev|aarav.desai@sutra.dev".into()),
                Recipient::Role(Role::Administrator)
            ]
        );
        let a = plan(&env(ATTENDANCE_THRESHOLD_BREACHED, d(None))).unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
    }

    #[test]
    fn coursework_and_interventions() {
        let a = plan(&env(
            ASSESSMENT_WINDOW_OPENED,
            status(Some("Essay 1"), None),
        ))
        .unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Student)]);
        let a = plan(&env(
            MONITORING_INTERVENTION_TRIGGERED,
            status(Some("Remedial classes"), Some("dev|meera.shah@sutra.dev")),
        ))
        .unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
        let todo = a.iter().find_map(|x| match x {
            InboxAction::Todo(t) => Some(t.assignee_subject.clone()),
            InboxAction::Notify(_) => None,
        });
        assert_eq!(todo.as_deref(), Some("dev|meera.shah@sutra.dev"));
        assert!(plan(&env("sutraerp.academic.unknown.x", status(None, None))).is_err());
    }
}
