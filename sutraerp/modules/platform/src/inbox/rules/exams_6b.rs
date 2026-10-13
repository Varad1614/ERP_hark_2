//! **6B** inbox rules for `sutra_contracts::exams::events::marks` (contract doc
//! `docs/batch6-contracts.md` §8). Examinations: mark sheets, withheld marks, malpractice.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `mark_sheet.submitted` | notification → administrators ("Verify marks", link `/examinations/marks`) — a role cannot own a `SystemTodo` (subject only), so the "verify" work item is this role notification |
//! | `paper_mark.withheld` | notification → the student (link `/results`) |
//! | `malpractice_case.decided` / `malpractice_case.reviewed` | notification → the student (link `/results`) |
//!
//! Student notifications need `studentSubjectRef` in the payload (rules are pure, no lookup) and
//! are skipped without it; every projection is idempotent on the event id (category
//! `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::exams::events::marks::{
    MALPRACTICE_DECIDED, MALPRACTICE_REVIEWED, MARKS_SUBMITTED, MARKS_WITHHELD,
};
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    MARKS_SUBMITTED,
    MARKS_WITHHELD,
    MALPRACTICE_DECIDED,
    MALPRACTICE_REVIEWED,
];

const RESULTS: &str = "/results";
const MARKS_TAB: &str = "/examinations/marks";

fn student(
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
            notify(
                env,
                Recipient::Subject(s.to_owned()),
                severity,
                title.to_owned(),
                body,
                Some(RESULTS),
            )
        })
        .into_iter()
        .collect()
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    Ok(match env.event_type.as_str() {
        MARKS_SUBMITTED => vec![notify(
            env,
            Recipient::Role(Role::Administrator),
            Severity::Info,
            "Verify marks".into(),
            "An evaluator submitted a paper's marks; verify them in Examinations → Marks.".into(),
            Some(MARKS_TAB),
        )],
        MARKS_WITHHELD => student(
            env,
            &d,
            Severity::Warning,
            "Examination marks withheld",
            d.reason
                .clone()
                .unwrap_or_else(|| "Your marks for a paper are withheld pending review.".into()),
        ),
        MALPRACTICE_DECIDED => student(
            env,
            &d,
            Severity::Warning,
            "Malpractice case decided",
            d.reason.clone().unwrap_or_else(|| {
                "The committee decided a malpractice case concerning you.".into()
            }),
        ),
        MALPRACTICE_REVIEWED => student(
            env,
            &d,
            Severity::Info,
            "Malpractice appeal reviewed",
            d.reason
                .clone()
                .unwrap_or_else(|| "Your malpractice appeal was reviewed.".into()),
        ),
        other => return Err(format!("{other}: no 6B rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
    use uuid::Uuid;

    fn env(t: &'static str, subject: Option<&str>) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/exams",
                event_type: t,
                dataschema: "urn:test",
                subject: None,
                data: serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
                    "toStatus": "withheld", "version": 2, "reason": "Script missing",
                    "studentSubjectRef": subject}),
            },
        )
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
    fn students_are_notified_only_with_a_subject() {
        for t in [MARKS_WITHHELD, MALPRACTICE_DECIDED, MALPRACTICE_REVIEWED] {
            let a = plan(&env(t, Some("dev|aarav.desai@sutra.dev"))).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![Recipient::Subject("dev|aarav.desai@sutra.dev".into())],
                "{t}"
            );
            assert!(plan(&env(t, None)).unwrap_or_default().is_empty(), "{t}");
        }
    }

    #[test]
    fn submitted_marks_notify_administrators() {
        let a = plan(&env(MARKS_SUBMITTED, None)).unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
        assert!(plan(&env("sutraerp.exams.other.thing", None)).is_err());
    }
}
