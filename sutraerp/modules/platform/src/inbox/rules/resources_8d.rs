//! **8D** inbox rules for `sutra_contracts::resources::events::procurement` (contract doc
//! `docs/batch8-contracts.md` §8). Procurement, inventory, assets, payables.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `purchase_requisition.submitted` | emitted only while `REQUISITION_APPROVAL` awaits a decision: notification → administrators + to-do for the submitter "Decide PR-…" (link `/campus/procurement`) |
//! | `vendor_payment.submitted` | Part 1 (DEF-J-003): notification → procurement approvers and finance approvers "Vendor payment VPAY-… awaits approval" (link `/campus/procurement`); the maker gets no decide to-do (they may not decide it) |
//! | `supplier.bank_change_requested` | Part 1 · Stage 3 (DEF-J-004): notification → procurement approvers and finance approvers "Bank details change for SUP-… awaits approval" (masked old → new account); no decide to-do for the maker |
//! | `supplier.bank_change_approved` | Part 1 · Stage 3 (DEF-J-004): warning → finance approvers and administrators "Bank details of SUP-… changed" (out-of-band check before the next payment) |
//! | `replenishment_need.raised` | notification → administrators + to-do for whoever ran the reorder-level check "Replenish …" (link `/campus/inventory`) |
//!
//! Notifications are idempotent on the event id (category `institution`); to-dos skip system
//! actors.

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::resources::events::procurement::{
    PURCHASE_REQUISITION_SUBMITTED, REPLENISHMENT_NEEDED, SUPPLIER_BANK_CHANGE_APPROVED,
    SUPPLIER_BANK_CHANGE_REQUESTED, VENDOR_PAYMENT_SUBMITTED,
};
use sutra_contracts::resources::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    PURCHASE_REQUISITION_SUBMITTED,
    REPLENISHMENT_NEEDED,
    VENDOR_PAYMENT_SUBMITTED,
    SUPPLIER_BANK_CHANGE_REQUESTED,
    SUPPLIER_BANK_CHANGE_APPROVED,
];

const PROCUREMENT: &str = "/campus/procurement";
const INVENTORY: &str = "/campus/inventory";

fn todo_for_actor(
    env: &EventEnvelope,
    title: String,
    detail: &str,
    days: i64,
    link: &str,
) -> Option<InboxAction> {
    env.actorref
        .as_deref()
        .filter(|a| !a.is_empty() && !a.starts_with("system:"))
        .map(|actor| {
            InboxAction::Todo(SystemTodo {
                tenant: TenantId(env.tenantid),
                institution: env.institutionid,
                assignee_subject: actor.to_owned(),
                title,
                detail: detail.to_owned(),
                due_at: Some(env.time + Duration::days(days)),
                link: Some(link.to_owned()),
                source_ref: env.id.to_string(),
            })
        })
}

/// Projects one 8D event (only types listed in [`EVENT_TYPES`] reach it).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    Ok(match env.event_type.as_str() {
        PURCHASE_REQUISITION_SUBMITTED => {
            let d: StatusChangedV1 = data(env)?;
            let number = d.number.clone().unwrap_or_default();
            let mut v = vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Info,
                format!("Purchase requisition {number} awaits approval"),
                d.reason
                    .clone()
                    .unwrap_or_else(|| "A purchase requisition was submitted for approval.".into()),
                Some(PROCUREMENT),
            )];
            v.extend(todo_for_actor(
                env,
                format!("Decide purchase requisition {number}"),
                "Approve or reject the requisition in Campus → Procurement.",
                2,
                PROCUREMENT,
            ));
            v
        }
        VENDOR_PAYMENT_SUBMITTED => {
            let d: StatusChangedV1 = data(env)?;
            let number = d.number.clone().unwrap_or_default();
            let body = d
                .reason
                .clone()
                .unwrap_or_else(|| "A vendor payment was requested.".into());
            [Role::ProcurementApprover, Role::FinanceApprover]
                .into_iter()
                .map(|r| {
                    notify(
                        env,
                        Recipient::Role(r),
                        Severity::Info,
                        format!("Vendor payment {number} awaits approval"),
                        body.clone(),
                        Some(PROCUREMENT),
                    )
                })
                .collect()
        }
        SUPPLIER_BANK_CHANGE_REQUESTED | SUPPLIER_BANK_CHANGE_APPROVED => {
            let d: StatusChangedV1 = data(env)?;
            let number = d.number.clone().unwrap_or_default();
            let body = d
                .reason
                .clone()
                .unwrap_or_else(|| "Supplier bank details.".into());
            let (roles, severity, title) = if env.event_type == SUPPLIER_BANK_CHANGE_REQUESTED {
                (
                    [Role::ProcurementApprover, Role::FinanceApprover],
                    Severity::Info,
                    format!("Bank details change for {number} awaits approval"),
                )
            } else {
                (
                    [Role::FinanceApprover, Role::Administrator],
                    Severity::Warning,
                    format!("Bank details of {number} changed"),
                )
            };
            roles
                .into_iter()
                .map(|r| {
                    notify(
                        env,
                        Recipient::Role(r),
                        severity,
                        title.clone(),
                        body.clone(),
                        Some(PROCUREMENT),
                    )
                })
                .collect()
        }
        REPLENISHMENT_NEEDED => {
            let d: StatusChangedV1 = data(env)?;
            let item = d.number.clone().unwrap_or_default();
            let mut v = vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Warning,
                format!("{item} is at its reorder level"),
                d.reason
                    .clone()
                    .unwrap_or_else(|| "Stock fell to the reorder level.".into()),
                Some(INVENTORY),
            )];
            v.extend(todo_for_actor(
                env,
                format!("Replenish {item}"),
                "Raise or release a purchase requisition for the replenishment need in Campus → Inventory.",
                7,
                INVENTORY,
            ));
            v
        }
        other => return Err(format!("{other}: no 8D rule")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use sutra_kernel::events::NewEvent;
    use sutra_kernel::scope::{ExecutionScope, InstitutionId};
    use uuid::Uuid;

    fn env(t: &'static str, data: serde_json::Value, actor: Option<&str>) -> EventEnvelope {
        let scope = ExecutionScope::institution(TenantId(Uuid::nil()), InstitutionId(Uuid::nil()));
        EventEnvelope::new(
            &scope,
            NewEvent {
                source: "/sutraerp/resources",
                event_type: t,
                dataschema: "urn:test",
                subject: Some("agg-1".into()),
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn payload(number: &str) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
            "toStatus": "submitted", "version": 2, "number": number, "reason": "PR-000001 Lab glassware"})
    }

    #[test]
    fn requisitions_awaiting_approval_reach_administrators() {
        let a = plan(&env(
            PURCHASE_REQUISITION_SUBMITTED,
            payload("PR-000001"),
            Some("dev|admin@sutra.dev"),
        ))
        .unwrap_or_default();
        assert_eq!(a.len(), 2);
        match &a[0] {
            InboxAction::Notify(n) => {
                assert_eq!(n.recipient, Recipient::Role(Role::Administrator));
                assert_eq!(n.title, "Purchase requisition PR-000001 awaits approval");
                assert_eq!(n.link.as_deref(), Some(PROCUREMENT));
            }
            InboxAction::Todo(_) => panic!("notification first"),
        }
        match &a[1] {
            InboxAction::Todo(t) => {
                assert_eq!(t.assignee_subject, "dev|admin@sutra.dev");
                assert_eq!(t.title, "Decide purchase requisition PR-000001");
            }
            InboxAction::Notify(_) => panic!("to-do second"),
        }
    }

    #[test]
    fn vendor_payments_awaiting_approval_reach_the_checkers() {
        let a = plan(&env(
            VENDOR_PAYMENT_SUBMITTED,
            payload("VPAY-000001"),
            Some("dev|admin@sutra.dev"),
        ))
        .unwrap_or_default();
        assert_eq!(a.len(), 2, "no decide to-do for the maker");
        let roles: Vec<_> = a
            .iter()
            .filter_map(|x| match x {
                InboxAction::Notify(n) => Some(n.recipient.clone()),
                InboxAction::Todo(_) => None,
            })
            .collect();
        assert_eq!(
            roles,
            vec![
                Recipient::Role(Role::ProcurementApprover),
                Recipient::Role(Role::FinanceApprover)
            ]
        );
    }

    // Part 1 · Stage 3 (DEF-J-004).
    #[test]
    fn supplier_bank_changes_reach_the_checkers_and_finance() {
        let roles = |t: &'static str| -> Vec<Recipient> {
            plan(&env(t, payload("SUP-000001"), Some("dev|admin@sutra.dev")))
                .unwrap_or_default()
                .iter()
                .filter_map(|x| match x {
                    InboxAction::Notify(n) => Some(n.recipient.clone()),
                    InboxAction::Todo(_) => None,
                })
                .collect()
        };
        assert_eq!(
            roles(SUPPLIER_BANK_CHANGE_REQUESTED),
            vec![
                Recipient::Role(Role::ProcurementApprover),
                Recipient::Role(Role::FinanceApprover)
            ],
            "no decide to-do for the maker"
        );
        assert_eq!(
            roles(SUPPLIER_BANK_CHANGE_APPROVED),
            vec![
                Recipient::Role(Role::FinanceApprover),
                Recipient::Role(Role::Administrator)
            ]
        );
    }

    #[test]
    fn replenishment_needs_notify_and_skip_system_todos() {
        let a = plan(&env(
            REPLENISHMENT_NEEDED,
            payload("PAPER-A4"),
            Some("system:auto-approval"),
        ))
        .unwrap_or_default();
        assert_eq!(a.len(), 1);
        match &a[0] {
            InboxAction::Notify(n) => {
                assert_eq!(n.title, "PAPER-A4 is at its reorder level");
                assert_eq!(n.link.as_deref(), Some(INVENTORY));
            }
            InboxAction::Todo(_) => panic!("notification only"),
        }
        assert!(plan(&env("sutraerp.resources.x.y", payload("x"), None)).is_err());
    }
}
