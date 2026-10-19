//! **5C** inbox rules for `sutra_contracts::finance::events::ledger` (contract doc
//! `docs/batch5-contracts.md` §8). Finance ledger, periods, treasury.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `manual_journal.submitted` | while `pending_approval` (AwaitDecision): notification → administrators "Manual journal awaiting approval" (auto-approved submissions project nothing) |
//! | `financial_period.closed` | notification → administrators |
//! | `bank_transaction.failed` | notification → administrators + to-do for the staff member who recorded the return ("Resolve returned bank transaction") |
//!
//! Rules are pure (no lookup); notifications are idempotent on the event id (category
//! `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::finance::events::ledger::{
    BANK_TRANSACTION_FAILED, FINANCIAL_PERIOD_CLOSED, MANUAL_JOURNAL_SUBMITTED,
};
use sutra_contracts::finance::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    MANUAL_JOURNAL_SUBMITTED,
    FINANCIAL_PERIOD_CLOSED,
    BANK_TRANSACTION_FAILED,
];

const LEDGER_LINK: &str = "/finance/ledger";
const PERIODS_LINK: &str = "/finance/periods";
const TREASURY_LINK: &str = "/finance/treasury";

fn admins(
    env: &EventEnvelope,
    severity: Severity,
    title: &str,
    body: String,
    link: &str,
) -> InboxAction {
    notify(
        env,
        Recipient::Role(Role::Administrator),
        severity,
        title.into(),
        body,
        Some(link),
    )
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let what = d.reason.clone().unwrap_or_default();
    let amount = d
        .amount
        .as_deref()
        .map(|a| format!("₹{a}"))
        .unwrap_or_else(|| "an amount".into());
    Ok(match env.event_type.as_str() {
        MANUAL_JOURNAL_SUBMITTED if d.to_status == "pending_approval" => vec![admins(
            env,
            Severity::Info,
            "Manual journal awaiting approval",
            format!(
                "Manual journal {what} of {amount} was submitted. A second administrator approves or rejects it in Finance → Ledger."
            ),
            LEDGER_LINK,
        )],
        MANUAL_JOURNAL_SUBMITTED => Vec::new(),
        FINANCIAL_PERIOD_CLOSED => vec![admins(
            env,
            Severity::Success,
            "Financial period closed",
            format!("Financial period {what} was closed; postings dated into it are now refused."),
            PERIODS_LINK,
        )],
        BANK_TRANSACTION_FAILED => {
            let mut v = vec![admins(
                env,
                Severity::Warning,
                "Bank transaction returned",
                format!(
                    "The bank returned {amount} ({what}). The settlement entries were reversed; the payments are back on gateway clearing."
                ),
                TREASURY_LINK,
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
                    title: format!("Resolve returned bank transaction of {amount}"),
                    detail: format!("{what}. Re-settle the payments or follow up with the bank in Finance → Treasury."),
                    due_at: Some(env.time + Duration::days(2)),
                    link: Some(TREASURY_LINK.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        other => return Err(format!("{other}: no 5C rule")),
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
                source: "/sutraerp/finance",
                event_type,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn payload(to: &str) -> serde_json::Value {
        serde_json::json!({
            "aggregateId": Uuid::nil(), "institutionId": Uuid::nil(), "toStatus": to, "version": 2,
            "amount": "1500.00", "currency": "INR", "reason": "MJ-000001"
        })
    }

    #[test]
    fn pending_manual_journals_reach_administrators() {
        let p = plan(&env(
            MANUAL_JOURNAL_SUBMITTED,
            payload("pending_approval"),
            None,
        ))
        .unwrap();
        assert_eq!(p.len(), 1);
        assert!(matches!(p[0], InboxAction::Notify(_)));
        // auto-approved submissions arrive already posted → nothing to do
        assert!(
            plan(&env(MANUAL_JOURNAL_SUBMITTED, payload("posted"), None))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn closes_and_returns_notify_and_assign() {
        assert_eq!(
            plan(&env(FINANCIAL_PERIOD_CLOSED, payload("closed"), None))
                .unwrap()
                .len(),
            1
        );
        let staff = plan(&env(
            BANK_TRANSACTION_FAILED,
            payload("returned"),
            Some("admin-1"),
        ))
        .unwrap();
        assert_eq!(staff.len(), 2);
        assert!(matches!(staff[1], InboxAction::Todo(_)));
        let system = plan(&env(
            BANK_TRANSACTION_FAILED,
            payload("returned"),
            Some("system:x"),
        ))
        .unwrap();
        assert_eq!(system.len(), 1);
        assert!(plan(&env("sutraerp.finance.other.thing", payload("x"), None)).is_err());
    }
}
