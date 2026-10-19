//! **ERP_EXTENSIONS 5B** — roster reports over any date range, ported from HeyPuter/everything
//! `src/shifts/src/core/reports.js` (MIT, see THIRD_PARTY_NOTICES.md): hours scheduled against
//! worked per person, cost by role, attendance, coverage gaps and open shifts, with the period
//! before for trends, and one CSV per section. Archived and former staff stay in the history.

use super::clock::{worked_minutes, Record, ABSENT};
use super::rules::{
    add_days, break_minutes, coverage_gaps, js_round, net_minutes, num, Requirement, Shift,
};
use super::timesheet::{
    blank, employee_summaries, labor_cost, round2, timesheet_rows, to_csv, Cell, PayEmployee,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// `datesBetween`: every date of an inclusive range.
pub fn dates_between(from: &str, to: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut day = from.to_owned();
    while day.as_str() <= to && out.len() < 3700 {
        out.push(day.clone());
        day = add_days(&day, 1);
    }
    out
}

/// `previousRange`: the period of equal length ending the day before.
pub fn previous_range(from: &str, to: &str) -> (String, String) {
    let len = dates_between(from, to).len() as i64;
    let end = add_days(from, -1);
    (add_days(&end, -(len - 1)), end)
}

/// What a report reads.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    pub employees: Vec<PayEmployee>,
    pub shifts: Vec<Shift>,
    pub attendance: BTreeMap<String, Record>,
    pub coverage: Vec<Requirement>,
    pub overtime_threshold_hours: Option<f64>,
    pub overtime_multiplier: Option<f64>,
}

/// A person in a report.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Person {
    pub employee_id: String,
    pub employee: String,
    pub shifts: i64,
    pub scheduled_hours: f64,
    pub worked_hours: f64,
    pub present: i64,
    pub late: i64,
    pub absent: i64,
    pub not_recorded: i64,
    #[serde(serialize_with = "blank")]
    pub pay: Option<f64>,
    pub name: String,
    /// `null` (active), `Archived` or `Former employee`.
    pub label: Option<String>,
    pub recorded: i64,
    pub late_minutes: f64,
    pub show_up_rate: Option<f64>,
}

/// A role in a report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RoleRow {
    pub role: String,
    pub minutes: f64,
    pub cost: f64,
    pub shifts: i64,
}

/// Attendance over the period.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceTotals {
    pub present: i64,
    pub late: i64,
    pub absent: i64,
    pub not_recorded: i64,
    pub recorded: i64,
    pub rate: Option<f64>,
    pub late_minutes: f64,
}

/// Coverage shortfall of one role.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct GapRole {
    pub role: String,
    pub days: i64,
    pub missing: i64,
}

/// Coverage over the period.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Coverage {
    pub gap_days: usize,
    pub missing: i64,
    pub by_role: Vec<GapRole>,
}

/// Open shifts nobody took.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Open {
    pub count: usize,
    pub minutes: f64,
}

/// Totals of a report.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReportTotals {
    pub shifts: usize,
    pub people: usize,
    pub scheduled_minutes: f64,
    pub worked_minutes: f64,
    pub cost: f64,
    pub overtime_cost: f64,
    pub overtime_minutes: f64,
    pub pay: f64,
    pub rated_count: usize,
    pub employee_count: usize,
}

/// `buildReport`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    pub from: String,
    pub to: String,
    pub days: usize,
    pub people: Vec<Person>,
    pub roles: Vec<RoleRow>,
    pub attendance: AttendanceTotals,
    pub coverage: Coverage,
    pub open: Open,
    pub totals: ReportTotals,
}

fn recorded_minutes(shift: &Shift, r: Option<&Record>) -> Option<f64> {
    let r = r?;
    if r.is_absent() {
        return Some(0.0);
    }
    worked_minutes(
        r.clock_in.as_deref().unwrap_or(""),
        r.clock_out.as_deref().unwrap_or(""),
    )
    .map(|g| (g as f64 - break_minutes(shift)).max(0.0))
}

fn assigned(s: &Shift) -> bool {
    s.assignee_id.as_deref().is_some_and(|a| !a.is_empty())
}

fn desc(a: f64, b: f64) -> std::cmp::Ordering {
    b.partial_cmp(&a).unwrap_or(std::cmp::Ordering::Equal)
}

/// `buildReport`: one period's people, roles, attendance, coverage, open shifts and totals.
pub fn build_report(state: &State, from: &str, to: &str) -> Report {
    let dates = dates_between(from, to);
    let rows = timesheet_rows(&state.employees, &state.shifts, &state.attendance, from, to);
    let by_id: HashMap<&str, &PayEmployee> =
        state.employees.iter().map(|e| (e.id.as_str(), e)).collect();
    let mut late_by: Vec<(String, f64)> = Vec::new();
    for row in &rows {
        let late = row.minutes_late.unwrap_or(0.0);
        match late_by.iter_mut().find(|x| x.0 == row.employee_id) {
            Some(x) => x.1 += late,
            None => late_by.push((row.employee_id.clone(), late)),
        }
    }
    let late_of = |id: &str| late_by.iter().find(|x| x.0 == id).map_or(0.0, |x| x.1);
    let people: Vec<Person> = employee_summaries(&rows)
        .into_iter()
        .map(|e| {
            let emp = by_id.get(e.employee_id.as_str());
            let recorded = e.present + e.late + e.absent;
            Person {
                name: e.employee.clone(),
                label: match emp {
                    None => Some("Former employee".to_owned()),
                    Some(x) if x.active => None,
                    Some(_) => Some("Archived".to_owned()),
                },
                recorded,
                late_minutes: late_of(&e.employee_id),
                show_up_rate: (recorded > 0).then(|| (e.present + e.late) as f64 / recorded as f64),
                employee_id: e.employee_id,
                employee: e.employee,
                shifts: e.shifts,
                scheduled_hours: e.scheduled_hours,
                worked_hours: e.worked_hours,
                present: e.present,
                late: e.late,
                absent: e.absent,
                not_recorded: e.not_recorded,
                pay: e.pay,
            }
        })
        .collect();
    let cost = labor_cost(
        &state.shifts,
        &state.employees,
        &dates,
        state.overtime_threshold_hours,
        state.overtime_multiplier,
    );
    let in_range = |s: &&Shift| s.date.as_str() >= from && s.date.as_str() <= to;
    let mut shifts_by_role: HashMap<String, i64> = HashMap::new();
    for s in state.shifts.iter().filter(|s| assigned(s)).filter(in_range) {
        let role = s.role.trim();
        *shifts_by_role
            .entry(if role.is_empty() { "No role" } else { role }.to_owned())
            .or_default() += 1;
    }
    let mut roles: Vec<RoleRow> = cost
        .by_role
        .iter()
        .map(|b| RoleRow {
            role: b.key.clone(),
            minutes: b.minutes,
            cost: b.cost,
            shifts: shifts_by_role.get(&b.key).copied().unwrap_or(0),
        })
        .collect();
    roles.sort_by(|a, b| {
        desc(a.cost, b.cost)
            .then_with(|| desc(a.minutes, b.minutes))
            .then_with(|| a.role.cmp(&b.role))
    });
    let mut scheduled = 0.0;
    let mut worked_total = 0.0;
    let (mut present, mut late, mut absent, mut not_recorded) = (0, 0, 0, 0);
    let mut n_assigned = 0;
    for s in state.shifts.iter().filter(|s| assigned(s)).filter(in_range) {
        n_assigned += 1;
        scheduled += net_minutes(s);
        let r = state.attendance.get(&s.id);
        match r.map(|r| r.status.as_deref().unwrap_or("")) {
            None => not_recorded += 1,
            Some("present") => present += 1,
            Some("late") => late += 1,
            Some(x) if x == ABSENT => absent += 1,
            Some(_) => {}
        }
        if let Some(w) = recorded_minutes(s, r) {
            worked_total += w;
        }
    }
    let late_total: f64 = late_by.iter().map(|x| x.1).sum();
    let gaps = coverage_gaps(&state.shifts, &state.coverage, &dates);
    let mut gap_roles: Vec<GapRole> = Vec::new();
    for g in &gaps {
        match gap_roles.iter_mut().find(|x| x.role == g.role) {
            Some(x) => {
                x.days += 1;
                x.missing += g.missing;
            }
            None => gap_roles.push(GapRole {
                role: g.role.clone(),
                days: 1,
                missing: g.missing,
            }),
        }
    }
    gap_roles.sort_by(|a, b| b.missing.cmp(&a.missing).then_with(|| a.role.cmp(&b.role)));
    let open: Vec<&Shift> = state
        .shifts
        .iter()
        .filter(|s| !assigned(s))
        .filter(in_range)
        .collect();
    let recorded = present + late + absent;
    let pay: f64 = people.iter().map(|p| p.pay.unwrap_or(0.0)).sum();
    let gap_days: HashSet<&str> = gaps.iter().map(|g| g.date.as_str()).collect();
    Report {
        from: from.to_owned(),
        to: to.to_owned(),
        days: dates.len(),
        attendance: AttendanceTotals {
            present,
            late,
            absent,
            not_recorded,
            recorded,
            rate: (recorded > 0).then(|| (present + late) as f64 / recorded as f64),
            late_minutes: late_total,
        },
        coverage: Coverage {
            gap_days: gap_days.len(),
            missing: gaps.iter().map(|g| g.missing).sum(),
            by_role: gap_roles,
        },
        open: Open {
            count: open.len(),
            minutes: open.iter().map(|s| net_minutes(s)).sum(),
        },
        totals: ReportTotals {
            shifts: n_assigned,
            people: people.len(),
            scheduled_minutes: scheduled,
            worked_minutes: worked_total,
            cost: cost.cost,
            overtime_cost: cost.overtime_cost,
            overtime_minutes: cost.overtime_minutes,
            pay,
            rated_count: cost.rated_count,
            employee_count: cost.employee_count,
        },
        people,
        roles,
    }
}

/// `REPORT_SECTIONS`.
pub const SECTIONS: [&str; 5] = ["hours", "cost", "roles", "attendance", "coverage"];

/// `reportCSV`: one section as CSV, or why not.
pub fn report_csv(r: &Report, section: &str) -> Result<String, String> {
    let label = |p: &Person| p.label.clone().unwrap_or_else(|| "Active".to_owned());
    let (headers, rows): (Vec<&str>, Vec<Vec<Cell>>) = match section {
        "hours" => (
            vec![
                "Employee",
                "Status",
                "Shifts",
                "Scheduled hours",
                "Worked hours",
                "Difference",
            ],
            r.people
                .iter()
                .map(|p| {
                    vec![
                        p.name.clone().into(),
                        label(p).into(),
                        p.shifts.into(),
                        p.scheduled_hours.into(),
                        p.worked_hours.into(),
                        round2(p.worked_hours - p.scheduled_hours).into(),
                    ]
                })
                .collect(),
        ),
        "cost" => (
            vec![
                "Employee",
                "Status",
                "Scheduled hours",
                "Worked hours",
                "Pay",
            ],
            r.people
                .iter()
                .filter(|p| p.pay.is_some())
                .map(|p| {
                    vec![
                        p.name.clone().into(),
                        label(p).into(),
                        p.scheduled_hours.into(),
                        p.worked_hours.into(),
                        p.pay.into(),
                    ]
                })
                .collect(),
        ),
        "roles" => (
            vec!["Role", "Shifts", "Hours", "Projected cost"],
            r.roles
                .iter()
                .map(|x| {
                    vec![
                        x.role.clone().into(),
                        x.shifts.into(),
                        round2(x.minutes / 60.0).into(),
                        round2(x.cost).into(),
                    ]
                })
                .collect(),
        ),
        "attendance" => (
            vec![
                "Employee",
                "Recorded",
                "Present",
                "Late",
                "Absent",
                "Not recorded",
                "Show-up rate",
                "Minutes late",
            ],
            r.people
                .iter()
                .map(|p| {
                    vec![
                        p.name.clone().into(),
                        p.recorded.into(),
                        p.present.into(),
                        p.late.into(),
                        p.absent.into(),
                        p.not_recorded.into(),
                        p.show_up_rate
                            .map_or(String::new(), |x| format!("{}%", num(js_round(x * 100.0))))
                            .into(),
                        p.late_minutes.into(),
                    ]
                })
                .collect(),
        ),
        "coverage" => (
            vec!["Role", "Days short", "People short"],
            r.coverage
                .by_role
                .iter()
                .map(|g| vec![g.role.clone().into(), g.days.into(), g.missing.into()])
                .collect(),
        ),
        other => return Err(format!("Unknown report section: {other}")),
    };
    Ok(format!("{}\r\n", to_csv(&headers, &rows)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_and_an_empty_period() {
        assert_eq!(dates_between("2026-08-17", "2026-08-19").len(), 3);
        assert_eq!(
            previous_range("2026-08-01", "2026-08-31"),
            ("2026-07-01".to_owned(), "2026-07-31".to_owned())
        );
        let r = build_report(&State::default(), "2026-09-01", "2026-09-07");
        assert!(r.people.is_empty() && r.attendance.rate.is_none());
        assert!(report_csv(&r, "nonsense").is_err());
        assert!(report_csv(&r, "hours")
            .unwrap()
            .starts_with("Employee,Status"));
    }
}
