//! Golden vectors for `transport::maintenance`, generated from Everything's original
//! `fleet/src/core/{reminders,fuel,costs,dates}.js` by `tools/golden/generate.mjs`.

use serde::Deserialize;
use serde_json::{json, Value};
use sutra_campus::transport::maintenance::{
    add_interval, complete_reminder, complete_renewal, compliance, costs_by_vehicle, diff_days,
    fuel_derived, fuel_summary, fuel_total, reminder_next_due, reminder_state, renewal_state,
    unit_price, usage_by_vehicle, CostLine, DueSoon, FuelEntry, Interval, Reading, Reminder,
    Renewal,
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
struct Comp {
    date: String,
    meter: Option<f64>,
    entry_id: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenewOp {
    date: String,
    next_due: Option<String>,
    cost: Option<f64>,
}
#[derive(Deserialize)]
struct Price {
    volume: f64,
    price: f64,
}
#[derive(Deserialize)]
struct IntervalCase {
    date: String,
    interval: Interval,
    times: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    today: String,
    due: DueSoon,
    reminders: Vec<Reminder>,
    vehicle_meter: Option<f64>,
    completions: Vec<Comp>,
    renewals: Vec<Renewal>,
    renew_ops: Vec<RenewOp>,
    fuel: Vec<FuelEntry>,
    meter_unit: String,
    volume_unit: String,
    prices: Vec<Price>,
    lines: Vec<CostLine>,
    readings: Vec<Reading>,
    from: Option<String>,
    to: Option<String>,
    intervals: Vec<IntervalCase>,
}

/// Numbers as f64 and keys sorted, so 1 and 1.0 compare equal; tiny float noise tolerated.
fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .map_or(Value::Number(n), |f| Value::from((f * 1e9).round() / 1e9)),
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, norm(x))).collect()),
        other => other,
    }
}

fn eq(got: Value, want: &Value, what: &str) {
    assert_eq!(norm(got), norm(want.clone()), "{what}");
}

#[test]
fn fleet_maintenance_matches_everything() {
    let fx: Fixture =
        serde_json::from_str(include_str!("golden/fleet_maintenance.json")).expect("fixture");
    assert!(fx.cases.len() >= 150);
    for (ci, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let x = &c.expected;
        for (ri, r) in i.reminders.iter().enumerate() {
            let e = &x["reminders"][ri];
            let w = |s: &str| format!("case {ci} reminder {ri} {s}");
            let (nd, nm) = reminder_next_due(r);
            eq(
                json!({"nextDate": nd, "nextMeter": nm}),
                &e["next"],
                &w("next"),
            );
            let st = reminder_state(r, i.vehicle_meter, &i.today, i.due);
            eq(
                serde_json::to_value(&st).expect("state"),
                &e["state"],
                &w("state"),
            );
            let cp = &i.completions[ri];
            let done = complete_reminder(
                r,
                &cp.date,
                cp.meter,
                cp.entry_id.as_deref(),
                i.vehicle_meter,
                i.due,
            );
            eq(
                json!({"lastDate": done.last_date, "lastMeter": done.last_meter,
                    "nextDate": done.next_date, "nextMeter": done.next_meter,
                    "snoozedUntil": done.snoozed_until, "history": done.history}),
                &e["completed"],
                &w("completed"),
            );
            eq(json!(compliance(&done)), &e["compliance"], &w("compliance"));
        }
        for (ri, r) in i.renewals.iter().enumerate() {
            let e = &x["renewals"][ri];
            let (status, left) = renewal_state(r, &i.today, i.due);
            eq(
                json!({"status": status, "daysLeft": left}),
                &e["state"],
                &format!("case {ci} renewal {ri} state"),
            );
            let mut cur = r.clone();
            for (si, o) in i.renew_ops.iter().enumerate() {
                cur = complete_renewal(&cur, &o.date, o.next_due.as_deref(), o.cost);
                eq(
                    json!({"dueDate": cur.due_date, "repeatAnchor": cur.repeat_anchor,
                        "done": cur.done, "history": cur.history}),
                    &e["steps"][si],
                    &format!("case {ci} renewal {ri} step {si}"),
                );
            }
        }
        let derived = fuel_derived(&i.fuel, &i.meter_unit, &i.volume_unit);
        eq(
            serde_json::to_value(&derived).expect("derived"),
            &x["fuel"]["derived"],
            &format!("case {ci} fuel derived"),
        );
        let s = fuel_summary(&i.fuel, &derived, &i.meter_unit, &i.volume_unit);
        eq(
            serde_json::to_value(&s).expect("summary"),
            &x["fuel"]["summary"],
            &format!("case {ci} fuel summary"),
        );
        for (pi, p) in i.prices.iter().enumerate() {
            let t = fuel_total(p.volume, p.price);
            eq(
                json!({"total": t, "unit": unit_price(t, p.volume)}),
                &x["prices"][pi],
                &format!("case {ci} price {pi}"),
            );
        }
        let costs = costs_by_vehicle(&i.lines, i.from.as_deref(), i.to.as_deref());
        eq(
            serde_json::to_value(&costs).expect("costs"),
            &x["costs"],
            &format!("case {ci} costs"),
        );
        let usage = usage_by_vehicle(&i.readings, i.from.as_deref(), i.to.as_deref());
        eq(
            serde_json::to_value(&usage).expect("usage"),
            &x["usage"],
            &format!("case {ci} usage"),
        );
        for (ii, iv) in i.intervals.iter().enumerate() {
            eq(
                json!({"added": add_interval(&iv.date, &iv.interval, iv.times), "diff": diff_days(&i.today, &iv.date)}),
                &x["intervals"][ii],
                &format!("case {ci} interval {ii}"),
            );
        }
    }
}
