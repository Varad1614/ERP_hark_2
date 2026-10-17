//! Golden vectors for `roster::rules`, generated from Everything's original
//! `shifts/src/core/{scheduling,rules,fill}.js` by `tools/golden/generate.mjs`.

use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashSet};
use sutra_people::roster::rules::{
    availability_status, coverage_gaps, evaluate_move, find_conflicts, format_date, format_hours,
    hours_status, is_valid_date, net_minutes, plan_fill, rule_violations,
    schedule_rule_violations, start_of_week, suggest_assignees, validate_shift, weekly_minutes,
    Availability, Context, Employee, Requirement, Rules, Shift,
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
struct Move {
    shift: usize,
    date: String,
    assignee_id: Option<String>,
    copy: bool,
}
#[derive(Deserialize)]
struct Input {
    employees: Vec<Employee>,
    availability: BTreeMap<String, Availability>,
    shifts: Vec<Shift>,
    rules: Option<Rules>,
    requirements: Vec<Requirement>,
    dates: Vec<String>,
    moves: Vec<Move>,
    attendance: BTreeMap<String, bool>,
    choices: BTreeMap<String, Option<String>>,
    limit: usize,
}

fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Number(n), |f| Value::from((f * 1e9).round() / 1e9)),
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, norm(x))).collect()),
        other => other,
    }
}

fn eq(got: Value, want: &Value, what: &str) {
    assert_eq!(norm(got), norm(want.clone()), "{what}");
}

#[test]
fn duty_roster_matches_everything() {
    let fx: Fixture = serde_json::from_str(include_str!("golden/duty_roster.json")).expect("fixture");
    assert!(fx.cases.len() >= 150);
    for (ci, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let x = &c.expected;
        let name_of = |id: &str| {
            i.employees.iter().find(|e| e.id == id).map_or("This employee".to_owned(), |e| e.name.clone())
        };
        for (si, s) in i.shifts.iter().enumerate() {
            let e = &x["perShift"][si];
            let w = |k: &str| format!("case {ci} shift {si} {k}");
            eq(json!(validate_shift(s)), &e["errors"], &w("errors"));
            let conflicts: Vec<&str> =
                find_conflicts(s, &i.shifts, s.assignee_id.as_deref()).iter().map(|c| c.id.as_str()).collect();
            eq(json!(conflicts), &e["conflicts"], &w("conflicts"));
            let av = s.assignee_id.as_ref().and_then(|a| i.availability.get(a));
            eq(json!(availability_status(s, av)), &e["availability"], &w("availability"));
            eq(json!(net_minutes(s)), &e["net"], &w("net"));
            let rules = match (&i.rules, &s.assignee_id) {
                (Some(r), Some(a)) => rule_violations(s, a, &i.shifts, r, &name_of(a)),
                _ => Vec::new(),
            };
            eq(serde_json::to_value(&rules).expect("rules"), &e["rules"], &w("rules"));
            let sug = suggest_assignees(s, &i.employees, &i.shifts, &i.availability, i.rules.as_ref(), i.limit);
            eq(serde_json::to_value(&sug).expect("sug"), &e["suggestions"], &w("suggestions"));
            let valid = is_valid_date(&s.date);
            eq(json!(valid.then(|| format_date(&s.date))), &e["formatDate"], &w("formatDate"));
            eq(json!(valid.then(|| start_of_week(&s.date))), &e["week"], &w("week"));
        }
        for (ei, emp) in i.employees.iter().enumerate() {
            let e = &x["perEmployee"][ei];
            let mins = weekly_minutes(&i.shifts, &emp.id, &start_of_week(&i.dates[0]));
            let rules = i.rules.map_or_else(Vec::new, |r| {
                schedule_rule_violations(&emp.id, &i.shifts, &i.dates, &r, &emp.name)
            });
            eq(
                json!({"minutes": mins, "status": hours_status(mins, emp), "hours": format_hours(mins),
                    "rules": serde_json::to_value(&rules).expect("r")}),
                e,
                &format!("case {ci} employee {ei}"),
            );
        }
        let attendance: HashSet<String> = i.attendance.keys().cloned().collect();
        let ctx = Context { shifts: &i.shifts, employees: &i.employees, availability: &i.availability, attendance: &attendance };
        for (mi, mv) in i.moves.iter().enumerate() {
            let got = evaluate_move(&i.shifts[mv.shift], &mv.date, mv.assignee_id.as_deref(), &ctx, mv.copy);
            eq(serde_json::to_value(&got).expect("move"), &x["moves"][mi], &format!("case {ci} move {mi}"));
        }
        let gaps = coverage_gaps(&i.shifts, &i.requirements, &i.dates);
        eq(serde_json::to_value(&gaps).expect("gaps"), &x["gaps"], &format!("case {ci} gaps"));
        let plan = plan_fill(&i.shifts, &i.employees, &i.availability, i.rules.as_ref(), &i.dates, i.limit, &i.choices);
        eq(serde_json::to_value(&plan).expect("plan"), &x["plan"], &format!("case {ci} plan"));
    }
}
