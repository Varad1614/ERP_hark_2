//! **2B** inbox rules for `sutra_contracts::institution::events::{campus, curriculum}`.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `campus.activated` | notification to every administrator (campus operational) |
//! | `campus.decommissioned` | warning notification to every administrator |
//! | `curriculum_version.published` | notification to every administrator (new / successor version in force) |

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use sutra_contracts::institution::events::campus::{CAMPUS_ACTIVATED, CAMPUS_DECOMMISSIONED};
use sutra_contracts::institution::events::curriculum::{
    CurriculumVersionPublishedV1, CURRICULUM_VERSION_PUBLISHED,
};
use sutra_contracts::institution::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    CAMPUS_ACTIVATED,
    CAMPUS_DECOMMISSIONED,
    CURRICULUM_VERSION_PUBLISHED,
];

fn with_reason(base: String, reason: Option<&str>) -> String {
    match reason.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) => format!("{base} Reason: {r}"),
        None => base,
    }
}

/// Decides the projections of one event of [`EVENT_TYPES`] (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let admins = || Recipient::Role(Role::Administrator);
    Ok(match env.event_type.as_str() {
        CAMPUS_ACTIVATED => {
            let d: StatusChangedV1 = data(env)?;
            vec![notify(
                env,
                admins(),
                Severity::Success,
                "Campus activated".into(),
                with_reason(
                    "A campus is now operational and can be used across SutraERP.".into(),
                    d.reason.as_deref(),
                ),
                Some("/campus/campuses"),
            )]
        }
        CAMPUS_DECOMMISSIONED => {
            let d: StatusChangedV1 = data(env)?;
            vec![notify(
                env,
                admins(),
                Severity::Warning,
                "Campus decommissioned".into(),
                with_reason(
                    "A campus was decommissioned; it accepts no new use and stays in history."
                        .into(),
                    d.reason.as_deref(),
                ),
                Some("/campus/campuses"),
            )]
        }
        CURRICULUM_VERSION_PUBLISHED => {
            let d: CurriculumVersionPublishedV1 = data(env)?;
            let what = if d.supersedes_version_id.is_some() {
                format!(
                    "Curriculum version {} was published and supersedes its predecessor for new cohorts from {}.",
                    d.version_no, d.effective_from
                )
            } else {
                format!(
                    "Curriculum version {} was published, effective {}.",
                    d.version_no, d.effective_from
                )
            };
            vec![notify(
                env,
                admins(),
                Severity::Info,
                format!("Curriculum version {} published", d.version_no),
                what,
                Some("/academics/curriculum"),
            )]
        }
        _ => vec![],
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};

    fn env(event_type: &'static str, data: serde_json::Value) -> EventEnvelope {
        EventEnvelope::new(
            &ExecutionScope::institution(TenantId::new(), InstitutionId::new()),
            NewEvent {
                source: "/test",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
    }

    fn status(to: &str, reason: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "aggregateId": uuid::Uuid::now_v7(), "institutionId": uuid::Uuid::now_v7(),
            "fromStatus": "x", "toStatus": to, "version": 3, "reason": reason,
            "processInstanceId": null,
        })
    }

    fn only_notification(a: &[InboxAction]) -> &crate::inbox::notifications::NewNotification {
        assert_eq!(a.len(), 1);
        let InboxAction::Notify(n) = &a[0] else {
            panic!("notification expected")
        };
        n
    }

    #[test]
    fn campus_events_notify_admins_in_the_institution() {
        let e = env(CAMPUS_ACTIVATED, status("active", None));
        let a = plan(&e).unwrap_or_default();
        let n = only_notification(&a);
        assert_eq!(n.recipient, Recipient::Role(Role::Administrator));
        assert_eq!(n.institution, e.institutionid);
        assert_eq!(n.source_event_id, Some(e.id));
        assert_eq!(n.severity, Severity::Success);
        let e = env(
            CAMPUS_DECOMMISSIONED,
            status("decommissioned", Some("Merged")),
        );
        let a = plan(&e).unwrap_or_default();
        let n = only_notification(&a);
        assert_eq!(n.severity, Severity::Warning);
        assert!(n.body.contains("Merged"));
    }

    #[test]
    fn curriculum_publication_notifies_and_bad_payloads_fail() {
        let e = env(
            CURRICULUM_VERSION_PUBLISHED,
            serde_json::json!({
                "curriculumId": uuid::Uuid::now_v7(), "curriculumVersionId": uuid::Uuid::now_v7(),
                "programmeId": uuid::Uuid::now_v7(), "institutionId": uuid::Uuid::now_v7(),
                "versionNo": 2, "supersedesVersionId": uuid::Uuid::now_v7(),
                "effectiveFrom": "2027-07-01", "totalCredits": "160", "processInstanceId": null,
            }),
        );
        let a = plan(&e).unwrap_or_default();
        let n = only_notification(&a);
        assert!(n.title.contains('2') && n.body.contains("supersedes"));
        assert!(plan(&env(CURRICULUM_VERSION_PUBLISHED, serde_json::json!({}))).is_err());
        assert!(plan(&env(CAMPUS_ACTIVATED, serde_json::json!({"x": 1}))).is_err());
        for t in EVENT_TYPES {
            assert!(super::super::event_types().contains(t));
        }
    }
}
