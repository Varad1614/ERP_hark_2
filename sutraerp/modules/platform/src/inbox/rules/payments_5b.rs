//! **5B** inbox rules for `sutra_contracts::finance::events::payments` (contract doc
//! `docs/batch5-contracts.md` §8). Finance payments, receipts, refunds, receivables.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `payment.completed` | notification → the student (receipt issued, link `/fees`) |
//! | `payment.failed` | notification → the student (failed / expired online payment) |
//! | `refund.requested` | notification → administrators ("Decide refund …") + to-do for the staff requester |
//! | `refund.decided` / `refund.completed` | notification → the student |
//! | `receivable.overdue` / `dunning.notice_sent` | notification → the student |
//!
//! Student notifications need `studentSubjectRef` in the payload (rules are pure, no lookup);
//! notifications are idempotent on the event id (category `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::finance::events::payments::{
    DUNNING_NOTICE_SENT, FEE_REMINDER_SENT, LATE_FEE_CHARGED, PAYMENT_COMPLETED, PAYMENT_FAILED, RECEIVABLE_OVERDUE, REFUND_COMPLETED,
    REFUND_DECIDED, REFUND_REQUESTED,
};
use sutra_contracts::finance::events::StatusChangedV1;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    PAYMENT_COMPLETED,
    PAYMENT_FAILED,
    REFUND_REQUESTED,
    REFUND_DECIDED,
    REFUND_COMPLETED,
    RECEIVABLE_OVERDUE,
    DUNNING_NOTICE_SENT,
    LATE_FEE_CHARGED,
    FEE_REMINDER_SENT,
];

const FEES_LINK: &str = "/fees";
const REFUNDS_ADMIN_LINK: &str = "/finance/refunds";

fn rupees(d: &StatusChangedV1) -> String {
    match d.amount.as_deref() {
        Some(a) => format!("₹{a}"),
        None => "the amount".into(),
    }
}

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
                Some(FEES_LINK),
            )]
        })
        .unwrap_or_default()
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let d: StatusChangedV1 = data(env)?;
    let what = d.reason.clone().unwrap_or_default();
    Ok(match env.event_type.as_str() {
        PAYMENT_COMPLETED => to_student(
            env,
            &d,
            Severity::Info,
            "Payment received",
            format!("We received your payment {what} of {}. Your receipt is on the Fees page.", rupees(&d)),
        ),
        PAYMENT_FAILED => to_student(
            env,
            &d,
            Severity::Warning,
            "Payment not completed",
            format!("Your online payment of {} did not go through ({what}). No money was taken; you can try again from the Fees page.", rupees(&d)),
        ),
        REFUND_REQUESTED => {
            let mut v = vec![notify(
                env,
                Recipient::Role(Role::Administrator),
                Severity::Info,
                "Refund awaiting a decision".into(),
                format!("A refund of {} was requested: {what}.", rupees(&d)),
                Some(REFUNDS_ADMIN_LINK),
            )];
            let student = d.student_subject_ref.as_deref();
            if let Some(a) = env
                .actorref
                .as_deref()
                .filter(|a| !a.is_empty() && Some(*a) != student && !a.starts_with("system:"))
            {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: env.institutionid,
                    assignee_subject: a.to_owned(),
                    title: format!("Decide refund of {}", rupees(&d)),
                    detail: "Review the refund request and approve or reject it in Finance → Refunds.".into(),
                    due_at: Some(env.time + Duration::days(3)),
                    link: Some(REFUNDS_ADMIN_LINK.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        REFUND_DECIDED => {
            let approved = d.to_status == "approved";
            to_student(
                env,
                &d,
                if approved { Severity::Info } else { Severity::Warning },
                if approved { "Refund approved" } else { "Refund rejected" },
                if approved {
                    format!("Your refund of {} was approved and will be paid out shortly.", rupees(&d))
                } else {
                    format!("Your refund of {} was not approved: {what}.", rupees(&d))
                },
            )
        }
        REFUND_COMPLETED => to_student(
            env,
            &d,
            Severity::Info,
            "Refund paid",
            format!("Your refund of {} has been paid out.", rupees(&d)),
        ),
        RECEIVABLE_OVERDUE => to_student(
            env,
            &d,
            Severity::Warning,
            "Fee payment overdue",
            format!("{} is overdue on {what}. Please pay from the Fees page.", rupees(&d)),
        ),
        DUNNING_NOTICE_SENT => to_student(
            env,
            &d,
            Severity::Warning,
            "Fee reminder",
            what.clone(),
        ),
        LATE_FEE_CHARGED => to_student(
            env,
            &d,
            Severity::Warning,
            "Late fee added",
            format!("A late fee of {} was added: {what}.", rupees(&d)),
        ),
        FEE_REMINDER_SENT => to_student(env, &d, Severity::Info, "Fee due soon", what.clone()),
        other => return Err(format!("{other}: no 5B rule")),
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

    fn payload(to: &str, subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "aggregateId": Uuid::nil(), "institutionId": Uuid::nil(), "toStatus": to, "version": 2,
            "amount": "1500.00", "currency": "INR", "reason": "PAY-000001", "studentSubjectRef": subject
        })
    }

    #[test]
    fn student_notifications_need_a_subject() {
        for t in EVENT_TYPES.iter().filter(|t| **t != REFUND_REQUESTED) {
            let t: &'static str = t;
            let with = plan(&env(t, payload("completed", Some("stu-1")), None)).unwrap_or_default();
            assert_eq!(with.len(), 1, "{t}");
            let without = plan(&env(t, payload("completed", None), None)).unwrap_or_default();
            assert!(without.is_empty(), "{t}");
        }
    }

    #[test]
    fn refund_requests_reach_administrators_and_the_staff_requester() {
        let staff = plan(&env(
            REFUND_REQUESTED,
            payload("under_review", Some("stu-1")),
            Some("admin-1"),
        ))
        .unwrap_or_default();
        assert_eq!(staff.len(), 2);
        assert!(matches!(staff[1], InboxAction::Todo(_)));
        let own = plan(&env(
            REFUND_REQUESTED,
            payload("under_review", Some("stu-1")),
            Some("stu-1"),
        ))
        .unwrap_or_default();
        assert_eq!(
            own.len(),
            1,
            "a student's own request makes no to-do for them"
        );
        assert!(plan(&env(REFUND_DECIDED, serde_json::json!({}), None)).is_err());
    }
}
