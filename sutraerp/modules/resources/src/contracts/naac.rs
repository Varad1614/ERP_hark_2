//! MoU evidence for NAAC: which MoUs were *functional* in an assessment year (in force for some
//! of it and with at least one activity inside it), counted by partner type, scope and activity.
//! The AQAR year runs 1 July to 30 June.

use chrono::{Datelike, NaiveDate};
use serde::Serialize;
use std::collections::BTreeMap;
use utoipa::ToSchema;

/// Contract kinds that are MoUs.
pub const MOU_KINDS: [&str; 3] = ["mou_academic", "mou_industry", "mou_research"];

pub fn is_mou(kind: &str) -> bool {
    MOU_KINDS.contains(&kind)
}

/// The AQAR year (1 July – 30 June) containing `day`, with its label ("2026-27").
pub fn aqar_year(day: NaiveDate) -> (NaiveDate, NaiveDate, String) {
    let y = if day.month() >= 7 {
        day.year()
    } else {
        day.year() - 1
    };
    let from = NaiveDate::from_ymd_opt(y, 7, 1).unwrap_or(day);
    let to = NaiveDate::from_ymd_opt(y + 1, 6, 30).unwrap_or(day);
    (from, to, format!("{y}-{:02}", (y + 1) % 100))
}

/// Parse "2025-26" into its AQAR year.
pub fn parse_aqar(label: &str) -> Option<(NaiveDate, NaiveDate, String)> {
    let (a, b) = label.split_once('-')?;
    let y: i32 = a.parse().ok()?;
    let next: i32 = b.parse().ok()?;
    if a.len() != 4 || b.len() != 2 || (y + 1) % 100 != next {
        return None;
    }
    NaiveDate::from_ymd_opt(y, 7, 1).map(aqar_year)
}

/// An MoU as the evidence reads it.
#[derive(Debug, Clone)]
pub struct Mou {
    pub kind: String,
    pub status: String,
    pub scope: String,
    pub start: Option<NaiveDate>,
    /// When the agreement stopped (terminated) or will stop; `None` = still running.
    pub ends: Option<NaiveDate>,
    /// `(held on, activity kind, students, staff)`.
    pub activities: Vec<(NaiveDate, String, i64, i64)>,
}

/// In force for at least one day of `[from, to]`.
pub fn in_force(m: &Mou, from: NaiveDate, to: NaiveDate) -> bool {
    m.status != "draft" && m.start.is_none_or(|s| s <= to) && m.ends.is_none_or(|e| e >= from)
}

/// In force during the year and with an activity inside it.
pub fn functional(m: &Mou, from: NaiveDate, to: NaiveDate) -> bool {
    is_mou(&m.kind)
        && in_force(m, from, to)
        && m.activities.iter().any(|a| a.0 >= from && a.0 <= to)
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MouTotals {
    /// MoUs in force during the year.
    pub in_force: i64,
    /// … of which had an activity in the year.
    pub functional: i64,
    pub activities: i64,
    pub students: i64,
    pub staff: i64,
    /// Functional MoUs by kind (`mou_academic`…).
    pub functional_by_kind: BTreeMap<String, i64>,
    /// Functional MoUs by scope (`national`, `international`).
    pub functional_by_scope: BTreeMap<String, i64>,
    /// Activities in the year by kind.
    pub activities_by_kind: BTreeMap<String, i64>,
}

pub fn totals(mous: &[Mou], from: NaiveDate, to: NaiveDate) -> MouTotals {
    let mut t = MouTotals::default();
    for m in mous.iter().filter(|m| is_mou(&m.kind)) {
        if !in_force(m, from, to) {
            continue;
        }
        t.in_force += 1;
        let inside: Vec<_> = m
            .activities
            .iter()
            .filter(|a| a.0 >= from && a.0 <= to)
            .collect();
        if inside.is_empty() {
            continue;
        }
        t.functional += 1;
        *t.functional_by_kind.entry(m.kind.clone()).or_default() += 1;
        *t.functional_by_scope.entry(m.scope.clone()).or_default() += 1;
        for a in inside {
            t.activities += 1;
            t.students += a.2;
            t.staff += a.3;
            *t.activities_by_kind.entry(a.1.clone()).or_default() += 1;
        }
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn aqar_years() {
        assert_eq!(aqar_year(d("2026-10-17")).2, "2026-27");
        assert_eq!(aqar_year(d("2026-06-30")).2, "2025-26");
        assert_eq!(parse_aqar("2025-26").unwrap().0, d("2025-07-01"));
        assert!(parse_aqar("2025-27").is_none());
        assert_eq!(parse_aqar("1999-00").unwrap().1, d("2000-06-30"));
    }

    #[test]
    fn functional_needs_an_activity_in_the_year() {
        let (from, to, _) = aqar_year(d("2026-10-17"));
        let mut m = Mou {
            kind: "mou_industry".into(),
            status: "active".into(),
            scope: "national".into(),
            start: Some(d("2024-01-01")),
            ends: None,
            activities: vec![(d("2026-03-01"), "internship".into(), 12, 1)],
        };
        assert!(in_force(&m, from, to) && !functional(&m, from, to));
        m.activities
            .push((d("2026-08-20"), "workshop".into(), 40, 3));
        let t = totals(&[m], from, to);
        assert_eq!((t.functional, t.activities, t.students), (1, 1, 40));
    }
}
