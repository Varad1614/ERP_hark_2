//! Golden vectors for `roster::{swaps, clock, timesheet, reports, ics}`, generated from
//! Everything's original `shifts/src/core/{swaps,attendance,timesheet,payroll,reports,ics}.js` by
//! `tools/golden/generate.mjs` (spec `shift-swaps-timesheets`).

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap, HashSet};
use sutra_people::roster::clock::{
    clock_in_record, clock_out_record, elapsed_minutes, for_range, is_in_progress, is_running_now,
    late_minutes, summarize, worked_minutes, Record,
};
use sutra_people::roster::ics::{build_schedule_ics, CalendarShift};
use sutra_people::roster::reports::{
    build_report, dates_between, previous_range, report_csv, State, SECTIONS,
};
use sutra_people::roster::rules::{Availability, Employee, Requirement, Rules, Shift};
use sutra_people::roster::swaps::{apply_swap, create_swap_request, evaluate_swap, resolve_swap};
use sutra_people::roster::timesheet::{
    budget_status, employee_summaries, labor_cost, timesheet_csv, timesheet_rows, PayEmployee,
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
struct Swap {
    shift: i64,
    to: Option<String>,
    reason: String,
    with_employees: bool,
    with_rules: bool,
    with_attendance: bool,
    resolve: Vec<String>,
    apply: String,
}
#[derive(Deserialize)]
struct Clock {
    shift: usize,
    record: Option<Record>,
    at: String,
    now: String,
    grace: Option<f64>,
    today: String,
}
#[derive(Deserialize)]
struct Cost {
    dates: Vec<String>,
    threshold: Option<f64>,
    multiplier: Option<f64>,
    budget: Option<f64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Ics {
    employee_id: Option<String>,
    generated_at: String,
}
#[derive(Deserialize)]
struct Input {
    employees: Vec<Value>,
    availability: BTreeMap<String, Availability>,
    shifts: Vec<CalendarShift>,
    attendance: BTreeMap<String, Record>,
    rules: Option<Rules>,
    coverage: Vec<Requirement>,
    swaps: Vec<Swap>,
    clocks: Vec<Clock>,
    ranges: Vec<(String, String)>,
    cost: Cost,
    ics: Ics,
}

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

fn res<T: serde::Serialize>(r: Result<T, String>) -> Value {
    match r {
        Ok(v) => json!({"ok": serde_json::to_value(v).expect("ok")}),
        Err(e) => json!({"error": e}),
    }
}

#[test]
fn shift_swaps_timesheets_match_everything() {
    let fx: Fixture =
        serde_json::from_str(include_str!("golden/shift_swaps_timesheets.json")).expect("fixture");
    assert!(fx.cases.len() >= 150);
    for (ci, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let x = &c.expected;
        let roster: Vec<Employee> = i
            .employees
            .iter()
            .map(|e| serde_json::from_value(e.clone()).expect("employee"))
            .collect();
        let pay: Vec<PayEmployee> = i
            .employees
            .iter()
            .map(|e| serde_json::from_value(e.clone()).expect("pay employee"))
            .collect();
        let shifts: Vec<Shift> = i.shifts.iter().map(|c| c.shift.clone()).collect();
        let recorded: HashSet<String> = i.attendance.keys().cloned().collect();

        // ---- swaps
        for (si, sw) in i.swaps.iter().enumerate() {
            let e = &x["swaps"][si];
            let w = |k: &str| format!("case {ci} swap {si} {k}");
            let shift = usize::try_from(sw.shift).ok().map(|k| &shifts[k]);
            let created =
                create_swap_request(shift, sw.to.as_deref(), &sw.reason, Some("w1"), Some("t0"));
            eq(res(created.clone()), &e["created"], &w("created"));
            let Ok(req) = created else { continue };
            let shift = shift.expect("shift");
            let to = sw.to.as_deref().unwrap_or("");
            let ev = evaluate_swap(
                shift,
                to,
                &shifts,
                Some(&i.availability),
                sw.with_employees.then_some(roster.as_slice()),
                sw.with_attendance.then_some(&recorded),
                if sw.with_rules {
                    i.rules.as_ref()
                } else {
                    None
                },
            );
            eq(
                serde_json::to_value(&ev).expect("ev"),
                &e["evaluation"],
                &w("evaluation"),
            );
            let first = resolve_swap(&req, &sw.resolve[0], Some("t1"));
            let second = first
                .as_ref()
                .ok()
                .map(|f| res(resolve_swap(f, &sw.resolve[1], Some("t2"))));
            eq(res(first), &e["first"], &w("first"));
            eq(json!(second), &e["second"], &w("second"));
            let list: Vec<Shift> = match sw.apply.as_str() {
                "deleted" => shifts
                    .iter()
                    .filter(|s| s.id != shift.id)
                    .cloned()
                    .collect(),
                "reassigned" => shifts
                    .iter()
                    .map(|s| {
                        if s.id == shift.id {
                            Shift {
                                assignee_id: Some("someone-else".into()),
                                ..s.clone()
                            }
                        } else {
                            s.clone()
                        }
                    })
                    .collect(),
                _ => shifts.clone(),
            };
            let applied = apply_swap(&req, &list)
                .map(|l| l.into_iter().map(|s| s.assignee_id).collect::<Vec<_>>());
            eq(res(applied), &e["applied"], &w("applied"));
        }

        // ---- clock
        for (ki, k) in i.clocks.iter().enumerate() {
            let s = &shifts[k.shift];
            let r = k.record.as_ref();
            eq(
                json!({
                    "late": late_minutes(&s.start, &s.end, &k.at),
                    "worked": worked_minutes(&s.start, &k.at),
                    "elapsed": elapsed_minutes(&k.at, &k.now),
                    "inProgress": is_in_progress(r),
                    "runningNow": is_running_now(&s.date, r, &k.today),
                    "clockIn": clock_in_record(s, r, &k.at, k.grace.unwrap_or(0.0)),
                    "clockOut": clock_out_record(r, &k.now),
                }),
                &x["clocks"][ki],
                &format!("case {ci} clock {ki}"),
            );
        }
        let all: Vec<Record> = i.attendance.values().cloned().collect();
        eq(
            serde_json::to_value(summarize(&all)).expect("summary"),
            &x["summary"],
            &format!("case {ci} summary"),
        );

        // ---- per range: timesheets, report, calendar
        let names: HashMap<String, String> =
            pay.iter().map(|e| (e.id.clone(), e.name.clone())).collect();
        let state = State {
            employees: pay.clone(),
            shifts: shifts.clone(),
            attendance: i.attendance.clone(),
            coverage: i.coverage.clone(),
            overtime_threshold_hours: i.cost.threshold,
            overtime_multiplier: i.cost.multiplier,
        };
        for (ri, (from, to)) in i.ranges.iter().enumerate() {
            let e = &x["perRange"][ri];
            let w = |k: &str| format!("case {ci} range {ri} {k}");
            let fr: Vec<Value> = for_range(&shifts, &i.attendance, from, to)
                .into_iter()
                .map(|(s, r)| json!([s.id, r.and_then(|r| r.status.clone())]))
                .collect();
            eq(json!(fr), &e["forRange"], &w("forRange"));
            let rows = timesheet_rows(&pay, &shifts, &i.attendance, from, to);
            eq(
                serde_json::to_value(&rows).expect("rows"),
                &e["rows"],
                &w("rows"),
            );
            eq(
                serde_json::to_value(employee_summaries(&rows)).expect("sum"),
                &e["summaries"],
                &w("summaries"),
            );
            assert_eq!(
                timesheet_csv(&pay, &shifts, &i.attendance, from, to),
                e["csv"].as_str().unwrap_or_default(),
                "{}",
                w("csv")
            );
            eq(json!(dates_between(from, to)), &e["dates"], &w("dates"));
            let (pf, pt) = previous_range(from, to);
            eq(
                json!({"from": pf, "to": pt}),
                &e["previous"],
                &w("previous"),
            );
            let report = build_report(&state, from, to);
            eq(
                serde_json::to_value(&report).expect("report"),
                &e["report"],
                &w("report"),
            );
            let csvs: Vec<Value> = SECTIONS
                .iter()
                .copied()
                .chain(["nonsense"])
                .map(|s| res(report_csv(&report, s)))
                .collect();
            eq(json!(csvs), &e["reportCsv"], &w("reportCsv"));
            let (ics, count) = build_schedule_ics(
                &names,
                &i.shifts,
                from,
                to,
                i.ics.employee_id.as_deref(),
                &i.ics.generated_at,
                "Shifts schedule",
            );
            assert_eq!(
                ics,
                e["ics"]["ics"].as_str().unwrap_or_default(),
                "{}",
                w("ics")
            );
            eq(json!(count), &e["ics"]["count"], &w("ics count"));
        }

        // ---- labour cost and budget
        let labor = labor_cost(
            &shifts,
            &pay,
            &i.cost.dates,
            i.cost.threshold,
            i.cost.multiplier,
        );
        eq(
            serde_json::to_value(&labor).expect("labor"),
            &x["labor"],
            &format!("case {ci} labor"),
        );
        eq(
            serde_json::to_value(budget_status(labor.cost, i.cost.budget)).expect("budget"),
            &x["budget"],
            &format!("case {ci} budget"),
        );
    }
}
