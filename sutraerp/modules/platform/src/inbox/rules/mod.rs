//! Batch 2 (Domain B) inbox projection rules — **hook points**.
//!
//! The foundation-owned [`plan`] dispatches each event to the rule file of the slice that owns
//! its type. Each slice edits **only its own file**:
//!
//! | File | Slice | Handles event types from |
//! |------|-------|--------------------------|
//! | [`institution_2a`] | 2A | `institution::events::lifecycle`, `institution::events::org` |
//! | [`structure_2b`] | 2B | `institution::events::campus`, `institution::events::curriculum` |
//! | [`calendar_2c`] | 2C | `institution::events::calendar` |
//!
//! A rule file declares `EVENT_TYPES` (the types it projects — add a type only together with its
//! rule; the consumer records a receipt for every listed type, so a type listed with no rule is
//! marked `ignored` forever) and `plan(env) -> Result<Vec<InboxAction>, String>`.
//! Build notifications with [`notify`] and to-dos with [`super::todos::SystemTodo`].

pub mod calendar_2c;
pub mod institution_2a;
pub mod structure_2b;

use super::consumer::InboxAction;
use super::notifications::{NewNotification, NotificationCategory, Recipient, Severity};
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Every Batch 2 event type the consumer subscribes to.
pub fn event_types() -> Vec<&'static str> {
    let mut v = Vec::new();
    v.extend_from_slice(institution_2a::EVENT_TYPES);
    v.extend_from_slice(structure_2b::EVENT_TYPES);
    v.extend_from_slice(calendar_2c::EVENT_TYPES);
    v
}

/// Dispatches to the owning slice's rules. `None` = not a Batch 2 type handled here.
pub fn plan(env: &EventEnvelope) -> Option<Result<Vec<InboxAction>, String>> {
    let t = env.event_type.as_str();
    if institution_2a::EVENT_TYPES.contains(&t) {
        Some(institution_2a::plan(env))
    } else if structure_2b::EVENT_TYPES.contains(&t) {
        Some(structure_2b::plan(env))
    } else if calendar_2c::EVENT_TYPES.contains(&t) {
        Some(calendar_2c::plan(env))
    } else {
        None
    }
}

/// Decodes the event's `data` into its V1 payload.
pub fn data<T: serde::de::DeserializeOwned>(env: &EventEnvelope) -> Result<T, String> {
    serde_json::from_value(env.data.clone()).map_err(|e| format!("{}: {e}", env.event_type))
}

/// A notification for one Domain B event, idempotent on the event id. Institution-lifecycle
/// events (`sutraerp.institution.institution.*`) are tenant-wide (`institution = None`);
/// everything else keeps the envelope's institution.
pub fn notify(
    env: &EventEnvelope,
    recipient: Recipient,
    severity: Severity,
    title: String,
    body: String,
    link: Option<&str>,
) -> InboxAction {
    let institution = if env
        .event_type
        .starts_with("sutraerp.institution.institution.")
    {
        None
    } else {
        env.institutionid
    };
    InboxAction::Notify(NewNotification {
        tenant: TenantId(env.tenantid),
        institution,
        recipient,
        category: NotificationCategory::Institution,
        severity,
        title,
        body,
        link: link.map(str::to_owned),
        source_event_id: Some(env.id),
        visible_at: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slices_never_claim_the_same_type() {
        let all = event_types();
        let set: std::collections::HashSet<_> = all.iter().collect();
        assert_eq!(set.len(), all.len());
        for t in &all {
            assert!(
                sutra_contracts::institution::events::all_event_types().contains(t),
                "{t} is not a released Domain B event type"
            );
        }
    }
}
