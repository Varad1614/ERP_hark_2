//! Golden vectors for `charges::billing`, generated from Everything's original
//! `childcare/src/core/billing.js` (`periodFor`, `planBillingRun`, `planTotals`) over random
//! rosters by `tools/golden/generate.mjs`.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use sutra_finance::charges::billing::{
    period_for, plan_billing_run, plan_totals, Enrolled, Frequency, Plan, Proration, RunOptions,
    Student,
};
use uuid::Uuid;

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    input: Input,
    expected: Value,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    plans: Vec<P>,
    students: Vec<S>,
    invoices: Vec<Inv>,
    frequency: String,
    day: NaiveDate,
    due_days: i64,
    week_start: u32,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct P {
    id: Uuid,
    name: String,
    amount_cents: i64,
    frequency: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct S {
    id: Uuid,
    status: String,
    start_date: Option<NaiveDate>,
    plans: Vec<Sp>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Sp {
    plan_id: Uuid,
    amount_cents: Option<i64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inv {
    student_id: Uuid,
    period: Option<String>,
    status: String,
}

fn cents(d: Decimal) -> i64 {
    (d * Decimal::from(100))
        .trunc()
        .try_into()
        .expect("whole paise fit in i64")
}

#[test]
fn billing_run_matches_everything() {
    let fixture: Fixture =
        serde_json::from_str(include_str!("golden/billing_run.json")).expect("fixture");
    assert!(fixture.cases.len() >= 100);
    for (n, c) in fixture.cases.into_iter().enumerate() {
        let i = c.input;
        let freq = Frequency::parse(&i.frequency).unwrap();
        let plans: Vec<Plan> = i
            .plans
            .iter()
            .map(|p| Plan {
                id: p.id,
                name: p.name.clone(),
                amount: Decimal::new(p.amount_cents, 2),
                frequency: Frequency::parse(&p.frequency).unwrap(),
                proration: Proration::None,
            })
            .collect();
        let students: Vec<Student> = i
            .students
            .iter()
            .map(|s| Student {
                id: s.id,
                active: s.status == "active",
                start: s.start_date,
                plans: s
                    .plans
                    .iter()
                    .map(|sp| Enrolled {
                        plan_id: sp.plan_id,
                        amount: sp.amount_cents.map(|c| Decimal::new(c, 2)),
                        start: None,
                        end: None,
                        on_hold: false,
                    })
                    .collect(),
            })
            .collect();
        // `period: null` in the input names the run's own period; voided ones do not count.
        let billed: HashSet<Uuid> = i
            .invoices
            .iter()
            .filter(|v| v.period.is_none() && v.status != "void")
            .map(|v| v.student_id)
            .collect();
        let opts = RunOptions {
            frequency: freq,
            day: i.day,
            due_days: i.due_days,
            week_start: i.week_start,
            fy_start_month: 4,
        };
        let run = plan_billing_run(&students, &plans, &billed, &HashSet::new(), &opts);
        let period = period_for(freq, i.day, i.week_start, 4);
        assert_eq!(period, run.period);
        let name_of = |id: Uuid| plans.iter().find(|p| p.id == id).unwrap().name.clone();
        let got = json!({
            "period": {"key": run.period.key, "start": run.period.start.to_string()},
            "drafts": run.drafts.iter().map(|d| json!({
                "studentId": d.student_id,
                "issueDate": d.issue_date.to_string(),
                "dueDate": d.due_date.to_string(),
                "period": d.period_key,
                "items": d.lines.iter().map(|l| json!([name_of(l.plan_id), cents(l.amount)])).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "totals": students.iter().map(|s| {
                let t = plan_totals(s, &plans);
                json!({
                    "weekly": cents(t.get(&Frequency::Weekly).copied().unwrap_or_default()),
                    "monthly": cents(t.get(&Frequency::Monthly).copied().unwrap_or_default()),
                })
            }).collect::<Vec<_>>(),
        });
        assert_eq!(got, c.expected, "case {n}");
    }
}
