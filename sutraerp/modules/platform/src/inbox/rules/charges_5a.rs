//! **5A** inbox rules for `sutra_contracts::finance::events::charges` (contract doc
//! `docs/batch5-contracts.md` §8). Finance fee structures, charges, instalments, disputes, adjustments, aid.
//!
//! | Event | Projection |
//! |-------|------------|
//! | `charge.published` | notification → the student ("New fee charge …", link `/fees`) |
//! | `instalment.due` | notification → the student |
//! | `aid_allocation.allocated` | notification → the student |
//! | `adjustment.submitted` | notification → administrators; to-do for the submitter "Approve adjustment …" (only while a decision is awaited) |
//! | Domain C `scholarship_eligibility.handed_off` | notification → administrators; to-do for the actor "Allocate financial aid …" |
//!
//! Student notifications need `studentSubjectRef` in the payload (rules are pure, no lookup) and
//! are skipped without it; every projection is idempotent on the event id (category
//! `institution`).

use super::{data, notify};
use crate::inbox::consumer::InboxAction;
use crate::inbox::notifications::{Recipient, Severity};
use crate::inbox::todos::SystemTodo;
use chrono::Duration;
use sutra_contracts::finance::events::charges::{
    ADJUSTMENT_SUBMITTED, AID_ALLOCATED, CHARGE_PUBLISHED, INSTALMENT_DUE,
};
use sutra_contracts::finance::events::StatusChangedV1;
use sutra_contracts::student::events::scholarship::SCHOLARSHIP_ELIGIBILITY_HANDED_OFF;
use sutra_kernel::auth::Role;
use sutra_kernel::events::EventEnvelope;
use sutra_kernel::scope::TenantId;

/// Event types projected by these rules.
pub const EVENT_TYPES: &[&str] = &[
    CHARGE_PUBLISHED,
    INSTALMENT_DUE,
    AID_ALLOCATED,
    ADJUSTMENT_SUBMITTED,
    SCHOLARSHIP_ELIGIBILITY_HANDED_OFF,
];

const FEES: &str = "/fees";
const ADJUSTMENTS: &str = "/finance/adjustments";
const AID: &str = "/finance/aid";

/// `₹1,25,000.00` from a decimal string (no float parsing).
fn inr(amount: Option<&str>) -> String {
    let Some(a) = amount else {
        return String::new();
    };
    let negative = a.starts_with('-');
    let a = a.trim_start_matches(['-', '+']);
    let (int, frac) = a.split_once('.').unwrap_or((a, "00"));
    let digits: Vec<char> = int.chars().collect();
    let n = digits.len();
    let mut out = String::new();
    for (i, c) in digits.iter().enumerate() {
        out.push(*c);
        let rest = n - i - 1;
        if rest > 0 && (rest == 3 || (rest > 3 && (rest - 3).is_multiple_of(2))) {
            out.push(',');
        }
    }
    let frac: String = format!("{frac}00").chars().take(2).collect();
    format!("{}₹{out}.{frac}", if negative { "-" } else { "" })
}

fn student(
    env: &EventEnvelope,
    d: &StatusChangedV1,
    severity: Severity,
    title: String,
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
                title,
                body,
                Some(FEES),
            )
        })
        .into_iter()
        .collect()
}

/// Plans the inbox actions of one event of this slice (pure).
pub fn plan(env: &EventEnvelope) -> Result<Vec<InboxAction>, String> {
    let admins = Recipient::Role(Role::Administrator);
    Ok(match env.event_type.as_str() {
        CHARGE_PUBLISHED => {
            let d: StatusChangedV1 = data(env)?;
            student(
                env,
                &d,
                Severity::Info,
                format!("New fee charge: {}", inr(d.amount.as_deref())),
                d.reason
                    .clone()
                    .unwrap_or_else(|| "A new charge was added to your fees.".into()),
            )
        }
        INSTALMENT_DUE => {
            let d: StatusChangedV1 = data(env)?;
            student(
                env,
                &d,
                Severity::Warning,
                format!("Fee instalment due: {}", inr(d.amount.as_deref())),
                d.reason.clone().unwrap_or_default(),
            )
        }
        AID_ALLOCATED => {
            let d: StatusChangedV1 = data(env)?;
            let amount = d
                .amount
                .as_deref()
                .map(|a| format!(" of {}", inr(Some(a))))
                .unwrap_or_default();
            student(
                env,
                &d,
                Severity::Success,
                format!("Financial aid allocated{amount}"),
                d.reason.clone().unwrap_or_default(),
            )
        }
        ADJUSTMENT_SUBMITTED => {
            let d: StatusChangedV1 = data(env)?;
            let what = format!(
                "Approve adjustment of {} ({})",
                inr(d.amount.as_deref()),
                d.reason.clone().unwrap_or_default()
            );
            let mut v = vec![notify(
                env,
                admins,
                Severity::Warning,
                "Adjustment awaits a decision".into(),
                what.clone(),
                Some(ADJUSTMENTS),
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
                    title: what,
                    detail: "Approve or reject the pending adjustment in Finance → Adjustments."
                        .into(),
                    due_at: Some(env.time + Duration::days(3)),
                    link: Some(ADJUSTMENTS.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        SCHOLARSHIP_ELIGIBILITY_HANDED_OFF => {
            let mut v = vec![notify(
                env,
                admins,
                Severity::Info,
                "Scholarship decision handed off to Financial Aid".into(),
                "Allocate the financial aid in Finance → Financial aid.".into(),
                Some(AID),
            )];
            if let Some(actor) = env.actorref.as_deref().filter(|a| !a.is_empty()) {
                v.push(InboxAction::Todo(SystemTodo {
                    tenant: TenantId(env.tenantid),
                    institution: env.institutionid,
                    assignee_subject: actor.to_owned(),
                    title: "Allocate financial aid for a handed-off scholarship".into(),
                    detail: "Review the decision and allocate it in Finance → Financial aid."
                        .into(),
                    due_at: Some(env.time + Duration::days(7)),
                    link: Some(AID.into()),
                    source_ref: env.id.to_string(),
                }));
            }
            v
        }
        other => return Err(format!("{other}: no 5A rule")),
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
                source: "/sutraerp/finance",
                event_type: t,
                dataschema: "urn:test",
                subject: None,
                data,
            },
        )
        .with_actor(actor.map(str::to_owned))
    }

    fn payload(subject: Option<&str>) -> serde_json::Value {
        serde_json::json!({"aggregateId": Uuid::nil(), "institutionId": Uuid::nil(),
            "toStatus": "published", "version": 2, "amount": "125000.00", "currency": "INR",
            "reason": "Tuition", "studentSubjectRef": subject})
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
    fn student_notifications_need_a_subject() {
        for t in [CHARGE_PUBLISHED, INSTALMENT_DUE, AID_ALLOCATED] {
            let a =
                plan(&env(t, payload(Some("dev|aarav.desai@sutra.dev")), None)).unwrap_or_default();
            assert_eq!(
                recipients(&a),
                vec![Recipient::Subject("dev|aarav.desai@sutra.dev".into())],
                "{t}"
            );
            assert!(plan(&env(t, payload(None), None))
                .unwrap_or_default()
                .is_empty());
        }
        let a = plan(&env(CHARGE_PUBLISHED, payload(Some("s")), None)).unwrap_or_default();
        match &a[0] {
            InboxAction::Notify(n) => assert_eq!(n.title, "New fee charge: ₹1,25,000.00"),
            InboxAction::Todo(_) => panic!("notification expected"),
        }
    }

    #[test]
    fn adjustments_and_hand_offs_create_admin_work() {
        let a = plan(&env(
            ADJUSTMENT_SUBMITTED,
            payload(None),
            Some("dev|admin@sutra.dev"),
        ))
        .unwrap_or_default();
        assert_eq!(recipients(&a), vec![Recipient::Role(Role::Administrator)]);
        assert!(a.iter().any(
            |x| matches!(x, InboxAction::Todo(t) if t.assignee_subject == "dev|admin@sutra.dev")
        ));
        let auto = plan(&env(
            ADJUSTMENT_SUBMITTED,
            payload(None),
            Some("system:auto-approval"),
        ))
        .unwrap_or_default();
        assert_eq!(auto.len(), 1, "no to-do for automatic steps");
        let h = plan(&env(
            SCHOLARSHIP_ELIGIBILITY_HANDED_OFF,
            serde_json::json!({}),
            Some("dev|admin@sutra.dev"),
        ))
        .unwrap_or_default();
        assert_eq!(h.len(), 2);
        assert_eq!(inr(Some("-50.5")), "-₹50.50");
        assert_eq!(inr(Some("12345678.90")), "₹1,23,45,678.90");
    }
}
