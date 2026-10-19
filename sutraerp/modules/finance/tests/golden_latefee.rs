//! Golden vectors for `receivables::latefee`, generated from Everything's original
//! `invoicing/src/lib/{money,doc,aging,latefee,reminders}.js` by `tools/golden/generate.mjs`.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use sutra_finance::receivables::latefee::{
    days_late, due_date_for, everything_fee, overdue_amount, reminder_stage, schedule_rows,
    RowAmount, ScheduleRow, Stage,
};

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
    doc: Doc,
    as_of: String,
    policy: Option<Policy>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Doc {
    items: Vec<Item>,
    payments: Vec<Payment>,
    due_date: String,
    schedule: Option<Vec<Row>>,
}
#[derive(Deserialize)]
struct Item {
    rate: i64,
}
#[derive(Deserialize)]
struct Payment {
    amount: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Row {
    #[serde(rename = "type")]
    kind: String,
    value: i64,
    due_date: String,
}
#[derive(Deserialize)]
struct Policy {
    #[serde(rename = "type")]
    kind: String,
    value: i64,
}

fn date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").ok()
}

/// JSON numbers as JavaScript prints them: whole amounts without a fraction.
fn num(d: Decimal) -> Value {
    let n = d.normalize();
    if n.scale() == 0 {
        json!(i64::try_from(n.mantissa()).expect("fits"))
    } else {
        json!(f64::from_str(&n.to_string()).expect("number"))
    }
}

fn iso(d: Option<NaiveDate>) -> String {
    d.map(|d| d.to_string()).unwrap_or_default()
}

#[test]
fn late_fees_match_everything() {
    let f: Fixture =
        serde_json::from_str(include_str!("golden/latefee.json")).expect("valid fixture");
    assert!(f.cases.len() >= 100);
    for (i, c) in f.cases.iter().enumerate() {
        let doc = &c.input.doc;
        let as_of = date(&c.input.as_of).expect("asOf");
        let total = Decimal::from(doc.items.iter().map(|x| x.rate).sum::<i64>());
        let paid = Decimal::from(doc.payments.iter().map(|p| p.amount).sum::<i64>());
        let balance = total - paid;
        let rows: Vec<ScheduleRow> = doc
            .schedule
            .iter()
            .flatten()
            .map(|r| ScheduleRow {
                due: date(&r.due_date),
                amount: if r.kind == "fixed" {
                    RowAmount::Fixed(Decimal::from(r.value))
                } else {
                    RowAmount::Percent(Decimal::from(r.value))
                },
            })
            .collect();
        let inst = schedule_rows(&rows, total, paid);
        let due = due_date_for(&inst, date(&doc.due_date));
        let overdue = overdue_amount(balance, &inst, date(&doc.due_date), as_of);
        let fee = c.input.policy.as_ref().map_or(Decimal::ZERO, |p| {
            everything_fee(p.kind == "percent", Decimal::from(p.value), overdue)
        });
        let late = days_late(due, as_of);
        let stage = match reminder_stage(late, 30) {
            Stage::Upcoming => "reminderUpcoming",
            Stage::Due => "reminderDue",
            Stage::Late => "reminderLate",
        };
        let mut got = json!({
            "balance": num(balance),
            "installments": inst.iter().map(|x| json!({
                "dueDate": iso(x.due), "amount": num(x.amount), "paid": num(x.paid), "open": num(x.open),
            })).collect::<Vec<_>>(),
            "dueDate": iso(due),
            "overdue": num(overdue),
            "fee": num(fee),
            "daysLate": late,
            "stage": stage,
        });
        if c.expected["fee"].is_null() {
            got["fee"] = Value::Null;
        }
        assert_eq!(got, c.expected, "case {i}");
    }
}
