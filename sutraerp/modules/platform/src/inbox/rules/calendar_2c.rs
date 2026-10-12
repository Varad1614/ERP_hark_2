//! **2C** inbox rules for `sutra_contracts::institution::events::calendar` (contract doc §8).
//!
//! | Event | Projection |
//! |-------|------------|
//! | `calendar_version.published` | notification → administrators |
//! | `academic_year.opened` / `academic_year.closed` | notification → administrators |
//! | `calendar_exception.approved` | notification → administrators, faculty and students |
//!
//! Every notification is idempotent on the event id (`source_event_id`) and keeps the event's
//! institution scope.

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::institution::events::calendar::{
    AcademicYearV1, CalendarExceptionV1, CalendarVersionPublishedV1, ACADEMIC_YEAR_CLOSED,
    ACADEMIC_YEAR_OPENED, CALENDAR_EXCEPTION_APPROVED, CALENDAR_VERSION_PUBLISHED,
};
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    CALENDAR_VERSION_PUBLISHED,
    ACADEMIC_YEAR_OPENED,
    ACADEMIC_YEAR_CLOSED,
    CALENDAR_EXCEPTION_APPROVED,
];

const LINK: &str = "/academics/calendar";

fn humanise(kind: &str) -> String {
    let s = kind.replace('_', " ");
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => s,
    }
}

fn period(from: chrono::NaiveDate, to: chrono::NaiveDate) -> String {
    if from == to {
        from.format("%-d %b %Y").to_string()
    } else {
        format!("{} – {}", from.format("%-d %b %Y"), to.format("%-d %b %Y"))
    }
}

/// Decides the projections of one event of [`EVENT_TYPES`] (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let admins = Recipient::Role(Role::Administrator);
    Ok(match env.event_type.as_str() {
        CALENDAR_VERSION_PUBLISHED => {
            let d: CalendarVersionPublishedV1 = data(env)?;
            let what = if d.supersedes_version_id.is_some() {
                format!("Academic calendar revised (version {})", d.version_no)
            } else {
                format!("Academic calendar published (version {})", d.version_no)
            };
            vec![notify(
                env,
                admins,
                Severity::Success,
                what,
                format!(
                    "Covers {} ({}). Assign it to the institution or a campus to make it govern.",
                    period(d.starts_on, d.ends_on),
                    d.timezone
                ),
                Some(LINK),
            )]
        }
        ACADEMIC_YEAR_OPENED => {
            let d: AcademicYearV1 = data(env)?;
            vec![notify(
                env,
                admins,
                Severity::Success,
                format!("Academic year {} is open", d.label),
                format!(
                    "The open academic context runs {}.",
                    period(d.starts_on, d.ends_on)
                ),
                Some(LINK),
            )]
        }
        ACADEMIC_YEAR_CLOSED => {
            let d: AcademicYearV1 = data(env)?;
            vec![notify(
                env,
                admins,
                Severity::Info,
                format!("Academic year {} closed", d.label),
                if d.successor_year_id.is_some() {
                    "The next year was prepared as a draft — review its calendar and open it when ready.".to_owned()
                } else {
                    "No next-cycle year was prepared.".to_owned()
                },
                Some(LINK),
            )]
        }
        CALENDAR_EXCEPTION_APPROVED => {
            let d: CalendarExceptionV1 = data(env)?;
            let title = format!("Calendar change: {}", humanise(&d.kind));
            let body = format!("{} — {}", period(d.starts_on, d.ends_on), d.reason);
            Role::ALL
                .iter()
                .map(|r| {
                    let link = if *r == Role::Administrator {
                        Some(LINK)
                    } else {
                        None
                    };
                    notify(
                        env,
                        Recipient::Role(*r),
                        Severity::Warning,
                        title.clone(),
                        body.clone(),
                        link,
                    )
                })
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
                source: "/sutraerp/institution",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
    }

    fn notifications(a: &[InboxAction]) -> Vec<&crate::inbox::notifications::NewNotification> {
        a.iter()
            .map(|x| match x {
                InboxAction::Notify(n) => n,
                InboxAction::Todo(_) => panic!("no to-dos expected"),
            })
            .collect()
    }

    #[test]
    fn publication_and_year_events_notify_administrators_in_scope() {
        let e = env(
            CALENDAR_VERSION_PUBLISHED,
            serde_json::json!({"calendarId": Uuid::nil(), "calendarVersionId": Uuid::nil(), "institutionId": Uuid::nil(),
                "academicYearId": Uuid::nil(), "versionNo": 2, "supersedesVersionId": Uuid::nil(),
                "startsOn": "2026-06-01", "endsOn": "2027-05-31", "timezone": "Asia/Kolkata", "processInstanceId": null}),
        );
        let a = plan(&e).unwrap_or_default();
        let n = notifications(&a);
        assert_eq!(n.len(), 1);
        assert_eq!(n[0].recipient, Recipient::Role(Role::Administrator));
        assert!(n[0].title.contains("revised (version 2)"), "{}", n[0].title);
        assert_eq!(
            n[0].institution, e.institutionid,
            "keeps the institution scope"
        );
        assert_eq!(n[0].source_event_id, Some(e.id));

        for t in [ACADEMIC_YEAR_OPENED, ACADEMIC_YEAR_CLOSED] {
            let e = env(
                t,
                serde_json::json!({"academicYearId": Uuid::nil(), "institutionId": Uuid::nil(), "calendarId": Uuid::nil(),
                    "label": "2026-27", "startsOn": "2026-06-01", "endsOn": "2027-05-31", "successorYearId": Uuid::nil()}),
            );
            let a = plan(&e).unwrap_or_default();
            assert_eq!(notifications(&a).len(), 1, "{t}");
            assert!(notifications(&a)[0].title.contains("2026-27"));
        }
    }

    #[test]
    fn approved_exception_reaches_every_role() {
        let e = env(
            CALENDAR_EXCEPTION_APPROVED,
            serde_json::json!({"exceptionId": Uuid::nil(), "calendarId": Uuid::nil(), "institutionId": Uuid::nil(),
                "kind": "closure", "startsOn": "2026-07-01", "endsOn": "2026-07-01", "reason": "Heavy rain", "processInstanceId": "p"}),
        );
        let a = plan(&e).unwrap_or_default();
        let n = notifications(&a);
        let roles: Vec<_> = n.iter().map(|x| x.recipient.clone()).collect();
        assert_eq!(roles.len(), 3);
        for r in Role::ALL {
            assert!(roles.contains(&Recipient::Role(r)));
        }
        assert_eq!(n[0].title, "Calendar change: Closure");
        assert!(n[0].body.contains("1 Jul 2026") && n[0].body.contains("Heavy rain"));
    }

    #[test]
    fn malformed_payload_is_an_error_and_types_are_released() {
        assert!(plan(&env(ACADEMIC_YEAR_OPENED, serde_json::json!({"x": 1}))).is_err());
        for t in EVENT_TYPES {
            assert!(sutra_contracts::institution::events::calendar::EVENT_TYPES.contains(t));
        }
    }
}
