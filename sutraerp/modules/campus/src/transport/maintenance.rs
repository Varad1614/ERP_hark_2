//! **ERP_EXTENSIONS 2D** — fleet maintenance rules, ported from HeyPuter/everything
//! `src/fleet/src/core/reminders.js` (service reminders by time / meter / whichever first,
//! renewals with anchored repeats), `fuel.js` (fill-up economy between full tanks), `costs.js`
//! (operating cost lines, per-vehicle totals, meter usage) and `dates.js` (MIT, see
//! THIRD_PARTY_NOTICES.md).
//!
//! Dates are `YYYY-MM-DD` strings in the institution's local calendar; money is in paise (the
//! original's cents); the meter is the vehicle's odometer (km by default) or hour meter. Pure.

use chrono::{Duration, Months, NaiveDate};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

// ------------------------------------------------------------------ dates

fn parse(iso: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(iso.get(..10).unwrap_or(iso), "%Y-%m-%d").ok()
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// `addDays`.
pub fn add_days(date: &str, n: i64) -> String {
    parse(date).map_or_else(|| date.to_owned(), |d| iso(d + Duration::days(n)))
}

/// `addMonths`: the same day of the month, clamped to the month's last day.
pub fn add_months(date: &str, n: i64) -> String {
    let Some(d) = parse(date) else {
        return date.to_owned();
    };
    let m = Months::new(n.unsigned_abs().min(u32::MAX as u64) as u32);
    let out = if n >= 0 {
        d.checked_add_months(m)
    } else {
        d.checked_sub_months(m)
    };
    out.map_or_else(|| date.to_owned(), iso)
}

/// Whole days from `a` to `b` (`b − a`).
pub fn diff_days(a: &str, b: &str) -> i64 {
    match (parse(a), parse(b)) {
        (Some(x), Some(y)) => (y - x).num_days(),
        _ => 0,
    }
}

/// An `{ every, unit }` interval: `days | weeks | months | years` (anything else = months).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = FleetInterval)]
pub struct Interval {
    #[serde(default)]
    pub every: f64,
    #[serde(default = "months")]
    pub unit: String,
}

fn months() -> String {
    "months".into()
}

fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// `addInterval(iso, interval, times)`.
pub fn add_interval(date: &str, interval: &Interval, times: i64) -> String {
    let every = if interval.every.is_finite() {
        js_round(interval.every) as i64
    } else {
        0
    };
    let n = every * times;
    if date.is_empty() || n == 0 {
        return date.to_owned();
    }
    match interval.unit.as_str() {
        "days" => add_days(date, n),
        "weeks" => add_days(date, n * 7),
        "years" => add_months(date, n * 12),
        _ => add_months(date, n),
    }
}

// ------------------------------------------------------------------ service reminders

/// One completion of a service reminder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = FleetReminderCompletion)]
pub struct Completion {
    pub date: String,
    #[serde(default)]
    pub meter: Option<f64>,
    #[serde(default)]
    pub entry_id: Option<String>,
    pub on_time: bool,
}

/// A service reminder (see the module docs).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Reminder {
    pub interval: Option<Interval>,
    pub meter_interval: Option<f64>,
    pub due_soon_days: Option<f64>,
    pub due_soon_meter: Option<f64>,
    pub last_date: Option<String>,
    pub last_meter: Option<f64>,
    pub next_date: Option<String>,
    pub next_meter: Option<f64>,
    pub snoozed_until: Option<String>,
    /// The day the reminder was added (counts as the start without a last completion).
    pub created_day: Option<String>,
    pub start_date: Option<String>,
    pub start_meter: Option<f64>,
    pub history: Vec<Completion>,
}

/// Fleet thresholds for "due soon".
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = FleetDueSoon)]
pub struct DueSoon {
    pub days: f64,
    pub meter: f64,
}

impl Default for DueSoon {
    fn default() -> Self {
        Self {
            days: 14.0,
            meter: 500.0,
        }
    }
}

fn some_str(s: &Option<String>) -> Option<&str> {
    s.as_deref().filter(|x| !x.is_empty())
}

/// Next due date and meter; the time interval counts from the last completion, else from the
/// day the reminder was added.
pub fn reminder_next_due(r: &Reminder) -> (Option<String>, Option<f64>) {
    let from = some_str(&r.last_date).or(some_str(&r.created_day));
    let next_date = match some_str(&r.next_date) {
        Some(d) => Some(d.to_owned()),
        None => match (&r.interval, from) {
            (Some(i), Some(f)) if i.every != 0.0 && !i.every.is_nan() => {
                Some(add_interval(f, i, 1))
            }
            _ => None,
        },
    };
    let every = r.meter_interval.filter(|m| m.is_finite()).unwrap_or(0.0);
    let next_meter = match r.next_meter {
        Some(m) => Some(m),
        None if every != 0.0 => r.last_meter.map(|l| l + every),
        None => None,
    };
    (next_date, next_meter)
}

/// Where a reminder stands on `today`.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = FleetReminderState)]
pub struct ReminderState {
    /// `overdue | due_soon | upcoming | snoozed`.
    pub status: String,
    pub next_date: Option<String>,
    pub next_meter: Option<f64>,
    pub days_left: Option<i64>,
    pub meter_left: Option<f64>,
}

/// `reminderState`: overdue as soon as either the date or the meter is reached.
pub fn reminder_state(
    r: &Reminder,
    vehicle_meter: Option<f64>,
    today: &str,
    due: DueSoon,
) -> ReminderState {
    let (next_date, next_meter) = reminder_next_due(r);
    let days_left = next_date.as_deref().map(|d| diff_days(today, d));
    let meter_left = match (next_meter, vehicle_meter) {
        (Some(n), Some(m)) => Some(n - m),
        _ => None,
    };
    let soon_days = r.due_soon_days.unwrap_or(due.days);
    let soon_meter = r.due_soon_meter.unwrap_or(due.meter);
    let mut status = "upcoming";
    if days_left.is_some_and(|d| d < 0) || meter_left.is_some_and(|m| m <= 0.0) {
        status = "overdue";
    } else if days_left.is_some_and(|d| (d as f64) <= soon_days)
        || meter_left.is_some_and(|m| m <= soon_meter)
    {
        status = "due_soon";
    }
    if some_str(&r.snoozed_until).is_some_and(|s| s >= today) && status != "upcoming" {
        status = "snoozed";
    }
    ReminderState {
        status: status.into(),
        next_date,
        next_meter,
        days_left,
        meter_left,
    }
}

/// `completeReminder`: the schedule restarts from this date and meter (overrides and snooze
/// cleared); an older completion entered late is filed in date order and graded against the
/// one before it. History keeps the last 50.
pub fn complete_reminder(
    r: &Reminder,
    date: &str,
    meter: Option<f64>,
    entry_id: Option<&str>,
    vehicle_meter: Option<f64>,
    due: DueSoon,
) -> Reminder {
    let h = &r.history;
    let tail50 = |v: Vec<Completion>| -> Vec<Completion> {
        let skip = v.len().saturating_sub(50);
        v.into_iter().skip(skip).collect()
    };
    if let Some(at) = h.iter().position(|x| x.date.as_str() > date) {
        let prev = at.checked_sub(1).and_then(|i| h.get(i));
        let base = Reminder {
            last_date: prev
                .map(|p| p.date.clone())
                .or_else(|| r.start_date.clone()),
            last_meter: match prev {
                Some(p) => p.meter.or(r.start_meter),
                None => r.start_meter,
            },
            next_date: None,
            next_meter: None,
            snoozed_until: None,
            ..r.clone()
        };
        let st = reminder_state(&base, meter, date, due);
        let mut hist: Vec<Completion> = h[..at].to_vec();
        hist.push(Completion {
            date: date.to_owned(),
            meter,
            entry_id: entry_id.map(str::to_owned),
            on_time: st.status != "overdue",
        });
        hist.extend_from_slice(&h[at..]);
        return Reminder {
            history: tail50(hist),
            ..r.clone()
        };
    }
    let unsnoozed = Reminder {
        snoozed_until: None,
        ..r.clone()
    };
    let before = reminder_state(&unsnoozed, meter.or(vehicle_meter), date, due);
    let mut hist = h.clone();
    hist.push(Completion {
        date: date.to_owned(),
        meter,
        entry_id: entry_id.map(str::to_owned),
        on_time: before.status != "overdue",
    });
    Reminder {
        last_date: Some(date.to_owned()),
        last_meter: meter.or(r.last_meter),
        next_date: None,
        next_meter: None,
        snoozed_until: None,
        history: tail50(hist),
        ..r.clone()
    }
}

/// Share of completions done on time, or `None` without history.
pub fn compliance(r: &Reminder) -> Option<f64> {
    if r.history.is_empty() {
        return None;
    }
    Some(r.history.iter().filter(|c| c.on_time).count() as f64 / r.history.len() as f64)
}

/// "Every 6 months or 10000 km", "One time".
pub fn describe_schedule(r: &Reminder, meter_unit: &str) -> String {
    let mut parts = Vec::new();
    if let Some(i) = r
        .interval
        .as_ref()
        .filter(|i| i.every != 0.0 && !i.every.is_nan())
    {
        let n = i.every;
        let unit = if i.unit.is_empty() {
            "months"
        } else {
            i.unit.as_str()
        };
        let u = if n == 1.0 {
            unit.strip_suffix('s').unwrap_or(unit)
        } else {
            unit
        };
        parts.push(format!("{} {u}", num(n)));
    }
    if let Some(m) = r.meter_interval.filter(|m| *m != 0.0 && m.is_finite()) {
        parts.push(format!("{} {meter_unit}", num(m)));
    }
    if parts.is_empty() {
        "One time".into()
    } else {
        format!("Every {}", parts.join(" or "))
    }
}

fn num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

// ------------------------------------------------------------------ renewals

/// A renewal's repeat anchor: the series start and how many rolls it has made.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[schema(as = FleetRepeatAnchor)]
pub struct Anchor {
    pub date: String,
    pub n: i64,
}

/// One completed renewal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = FleetRenewalCompletion)]
pub struct RenewalDone {
    pub date: String,
    pub due_date: Option<String>,
    /// Paise.
    pub cost: Option<f64>,
}

/// A dated obligation (registration, insurance, fitness, PUC, permit…).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Renewal {
    pub due_date: Option<String>,
    pub due_soon_days: Option<f64>,
    pub repeat: Option<Interval>,
    pub repeat_anchor: Option<Anchor>,
    pub snoozed_until: Option<String>,
    pub history: Vec<RenewalDone>,
    pub done: bool,
}

/// `renewalState`: status and days left.
pub fn renewal_state(r: &Renewal, today: &str, due: DueSoon) -> (String, Option<i64>) {
    let Some(d) = some_str(&r.due_date) else {
        return ("upcoming".into(), None);
    };
    let left = diff_days(today, d);
    let soon = r.due_soon_days.unwrap_or(due.days);
    let mut status = if left < 0 {
        "overdue"
    } else if (left as f64) <= soon {
        "due_soon"
    } else {
        "upcoming"
    };
    if some_str(&r.snoozed_until).is_some_and(|s| s >= today) && status != "upcoming" {
        status = "snoozed";
    }
    (status.into(), Some(left))
}

/// `completeRenewal`: rolls forward by its repeat, counted from where the series started (a
/// renewal due on 31 Jan comes back 31 Mar after 28 Feb), unless `next_due` is given.
pub fn complete_renewal(
    r: &Renewal,
    date: &str,
    next_due: Option<&str>,
    cost: Option<f64>,
) -> Renewal {
    let mut history = r.history.clone();
    history.push(RenewalDone {
        date: date.to_owned(),
        due_date: r.due_date.clone(),
        cost,
    });
    let skip = history.len().saturating_sub(50);
    let history: Vec<RenewalDone> = history.into_iter().skip(skip).collect();
    let repeat = r
        .repeat
        .as_ref()
        .filter(|i| i.every != 0.0 && !i.every.is_nan());
    let roll = repeat.map(|rep| {
        let from = some_str(&r.due_date).unwrap_or(date).to_owned();
        let start = match &r.repeat_anchor {
            Some(a) if !a.date.is_empty() && add_interval(&a.date, rep, a.n) == from => a.clone(),
            _ => Anchor { date: from, n: 0 },
        };
        Anchor {
            date: start.date,
            n: start.n + 1,
        }
    });
    let next = match (&roll, repeat) {
        (Some(a), Some(rep)) => Some(add_interval(&a.date, rep, a.n)),
        _ => None,
    };
    let rolled = next_due
        .filter(|d| !d.is_empty())
        .map(str::to_owned)
        .or_else(|| next.clone());
    let anchor = match (&roll, &rolled) {
        (Some(a), Some(x)) if Some(x) == next.as_ref() => Some(a.clone()),
        _ => None,
    };
    Renewal {
        history,
        done: rolled.is_none(),
        due_date: rolled,
        repeat_anchor: anchor,
        snoozed_until: None,
        ..r.clone()
    }
}

// ------------------------------------------------------------------ fuel

/// A fill-up (or charging session).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct FuelEntry {
    pub id: String,
    pub date: String,
    pub time: Option<String>,
    pub meter: Option<f64>,
    pub volume: f64,
    /// Paise.
    pub total: f64,
    pub partial: bool,
    pub reset: bool,
}

/// `economyValue`: L/100 km for km with litres or kWh; volume per hour on an hour meter;
/// else distance per volume.
pub fn economy_value(
    distance: f64,
    volume: f64,
    meter_unit: &str,
    volume_unit: &str,
) -> Option<f64> {
    if distance.is_nan() || distance <= 0.0 || volume.is_nan() || volume <= 0.0 {
        return None;
    }
    if meter_unit == "hr" {
        return Some(volume / distance);
    }
    if meter_unit == "km" && (volume_unit == "L" || volume_unit == "kWh") {
        return Some(volume / distance * 100.0);
    }
    Some(distance / volume)
}

/// `roundCents`: twelve significant digits, then half away from zero.
pub fn round_cents(v: f64) -> f64 {
    if !v.is_finite() {
        return 0.0;
    }
    let p: f64 = format!("{v:.11e}").parse().unwrap_or(v);
    let r = js_round(p.abs());
    if p < 0.0 && r != 0.0 {
        -r
    } else {
        r
    }
}

/// Total in paise from volume and unit price (rupees).
pub fn fuel_total(volume: f64, price: f64) -> f64 {
    if volume.is_nan() || volume <= 0.0 || price.is_nan() || price < 0.0 {
        return 0.0;
    }
    round_cents(volume * price * 100.0)
}

/// Unit price (rupees per unit) from a total in paise.
pub fn unit_price(total: f64, volume: f64) -> Option<f64> {
    if volume.is_nan() || volume <= 0.0 {
        return None;
    }
    Some(js_round(total / 100.0 / volume * 1000.0) / 1000.0)
}

/// A measured stretch between full fill-ups.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, utoipa::ToSchema)]
#[schema(as = FleetFuelMeasure)]
pub struct Measured {
    pub distance: f64,
    pub volume: f64,
}

/// Per-entry derived values.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = FleetFuelDerived)]
pub struct FuelDerived {
    /// Meter distance since the previous entry.
    pub usage: Option<f64>,
    pub economy: Option<f64>,
    /// Paise per meter unit.
    pub cost_per_meter: Option<f64>,
    pub measured: Option<Measured>,
}

/// `fuelDerived` for one vehicle's entries of one kind.
pub fn fuel_derived(
    entries: &[FuelEntry],
    meter_unit: &str,
    volume_unit: &str,
) -> BTreeMap<String, FuelDerived> {
    let mut list: Vec<&FuelEntry> = entries.iter().filter(|e| e.meter.is_some()).collect();
    list.sort_by(|a, b| {
        a.meter
            .unwrap_or(0.0)
            .partial_cmp(&b.meter.unwrap_or(0.0))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.date.cmp(&b.date))
            .then_with(|| {
                a.time
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.time.as_deref().unwrap_or(""))
            })
    });
    let when = |e: &FuelEntry| format!("{}T{}", e.date, e.time.as_deref().unwrap_or(""));
    let mut loose: Vec<&FuelEntry> = entries.iter().filter(|e| e.meter.is_none()).collect();
    loose.sort_by_key(|e| when(e));
    let mut li = 0;
    let mut out = BTreeMap::new();
    let mut prev: Option<f64> = None;
    let mut last_full: Option<f64> = None;
    let mut vol_since = 0.0;
    for e in list {
        while li < loose.len() && when(loose[li]) < when(e) {
            vol_since += loose[li].volume;
            li += 1;
        }
        let meter = e.meter.unwrap_or(0.0);
        let usage = prev.map(|p| meter - p);
        let mut economy = None;
        let mut measured = None;
        if e.reset {
            last_full = if e.partial { None } else { Some(meter) };
            vol_since = 0.0;
        } else {
            vol_since += e.volume;
            if !e.partial {
                if let Some(lf) = last_full.filter(|lf| meter > *lf) {
                    let m = Measured {
                        distance: meter - lf,
                        volume: vol_since,
                    };
                    economy = economy_value(m.distance, m.volume, meter_unit, volume_unit);
                    measured = Some(m);
                }
                last_full = Some(meter);
                vol_since = 0.0;
            }
        }
        out.insert(
            e.id.clone(),
            FuelDerived {
                usage: usage.filter(|u| *u >= 0.0),
                economy,
                cost_per_meter: usage.filter(|u| *u > 0.0).map(|u| js_round(e.total / u)),
                measured,
            },
        );
        prev = Some(meter);
    }
    for e in entries {
        out.entry(e.id.clone()).or_default();
    }
    out
}

/// Totals and averages over a set of entries.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = FleetFuelSummary)]
pub struct FuelSummary {
    pub count: usize,
    /// Paise.
    pub cost: f64,
    pub volume: f64,
    pub distance: f64,
    /// Rupees per unit of volume.
    pub avg_price: Option<f64>,
    /// Total measured distance over total measured volume.
    pub avg_economy: Option<f64>,
    /// Paise per meter unit.
    pub cost_per_meter: Option<f64>,
}

/// `fuelSummary`.
pub fn fuel_summary(
    entries: &[FuelEntry],
    derived: &BTreeMap<String, FuelDerived>,
    meter_unit: &str,
    volume_unit: &str,
) -> FuelSummary {
    let (mut cost, mut volume, mut distance, mut md, mut mv) = (0.0, 0.0, 0.0, 0.0, 0.0);
    for e in entries {
        cost += e.total;
        volume += e.volume;
        if let Some(d) = derived.get(&e.id) {
            if let Some(u) = d.usage.filter(|u| *u > 0.0) {
                distance += u;
            }
            if let Some(m) = d.measured {
                md += m.distance;
                mv += m.volume;
            }
        }
    }
    FuelSummary {
        count: entries.len(),
        cost,
        volume,
        distance,
        avg_price: (volume > 0.0).then(|| cost / 100.0 / volume),
        avg_economy: if md > 0.0 && mv > 0.0 {
            economy_value(md, mv, meter_unit, volume_unit)
        } else {
            None
        },
        cost_per_meter: (distance > 0.0).then(|| cost / distance),
    }
}

// ------------------------------------------------------------------ costs

/// One operating cost.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostLine {
    pub vehicle_id: String,
    pub date: String,
    /// `fuel | service | other`.
    pub kind: String,
    /// Paise.
    pub amount: f64,
}

/// Per-vehicle totals in paise.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
pub struct VehicleCosts {
    pub fuel: f64,
    pub service: f64,
    pub other: f64,
    pub total: f64,
}

fn in_range(d: &str, from: Option<&str>, to: Option<&str>) -> bool {
    !d.is_empty() && from.is_none_or(|f| d >= f) && to.is_none_or(|t| d <= t)
}

/// `costsByVehicle` over an inclusive date range.
pub fn costs_by_vehicle(
    lines: &[CostLine],
    from: Option<&str>,
    to: Option<&str>,
) -> BTreeMap<String, VehicleCosts> {
    let mut out: BTreeMap<String, VehicleCosts> = BTreeMap::new();
    for l in lines.iter().filter(|l| in_range(&l.date, from, to)) {
        let row = out.entry(l.vehicle_id.clone()).or_default();
        match l.kind.as_str() {
            "fuel" => row.fuel += l.amount,
            "service" => row.service += l.amount,
            _ => row.other += l.amount,
        }
        row.total += l.amount;
    }
    out
}

/// A meter reading.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Reading {
    pub vehicle_id: String,
    pub date: String,
    #[serde(default)]
    pub value: Option<f64>,
    #[serde(default)]
    pub void: bool,
}

/// `usageByVehicle`: distance from the highest reading before the range (else the lowest in
/// it) to the highest in it.
pub fn usage_by_vehicle(
    readings: &[Reading],
    from: Option<&str>,
    to: Option<&str>,
) -> BTreeMap<String, f64> {
    let mut groups: BTreeMap<&str, Vec<&Reading>> = BTreeMap::new();
    for r in readings.iter().filter(|r| !r.void && r.value.is_some()) {
        groups.entry(r.vehicle_id.as_str()).or_default().push(r);
    }
    let mut out = BTreeMap::new();
    for (vid, list) in groups {
        let (mut start, mut end, mut before): (Option<f64>, Option<f64>, Option<f64>) =
            (None, None, None);
        for r in list {
            let v = r.value.unwrap_or(0.0);
            if from.is_some_and(|f| r.date.as_str() < f) {
                if before.is_none_or(|b| v > b) {
                    before = Some(v);
                }
            } else if in_range(&r.date, from, to) {
                if start.is_none_or(|s| v < s) {
                    start = Some(v);
                }
                if end.is_none_or(|e| v > e) {
                    end = Some(v);
                }
            }
        }
        if let (Some(base), Some(e)) = (before.or(start), end) {
            if e > base {
                out.insert(vid.to_owned(), e - base);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reminders_renewals_and_fuel() {
        assert_eq!(add_months("2027-01-31", 1), "2027-02-28");
        let six = Interval {
            every: 6.0,
            unit: "months".into(),
        };
        let r = Reminder {
            interval: Some(six.clone()),
            meter_interval: Some(10_000.0),
            last_date: Some("2026-04-01".into()),
            last_meter: Some(20_000.0),
            ..Reminder::default()
        };
        let s = reminder_state(&r, Some(29_800.0), "2026-09-01", DueSoon::default());
        assert_eq!(s.status, "due_soon");
        assert_eq!(s.next_date.as_deref(), Some("2026-10-01"));
        let done = complete_reminder(
            &r,
            "2026-09-02",
            Some(29_900.0),
            None,
            None,
            DueSoon::default(),
        );
        assert!(done.history[0].on_time);
        assert_eq!(compliance(&done), Some(1.0));
        assert_eq!(describe_schedule(&r, "km"), "Every 6 months or 10000 km");
        let ren = Renewal {
            due_date: Some("2027-01-31".into()),
            repeat: Some(Interval {
                every: 1.0,
                unit: "months".into(),
            }),
            ..Renewal::default()
        };
        let a = complete_renewal(&ren, "2027-01-30", None, None);
        assert_eq!(a.due_date.as_deref(), Some("2027-02-28"));
        let b = complete_renewal(&a, "2027-02-27", None, None);
        assert_eq!(b.due_date.as_deref(), Some("2027-03-31"));
        let e = |id: &str, m: f64, v: f64, p: bool| FuelEntry {
            id: id.into(),
            date: "2026-10-01".into(),
            meter: Some(m),
            volume: v,
            total: v * 10_000.0,
            partial: p,
            ..FuelEntry::default()
        };
        let d = fuel_derived(
            &[
                e("a", 1000.0, 40.0, false),
                e("b", 1300.0, 10.0, true),
                e("c", 1500.0, 20.0, false),
            ],
            "km",
            "L",
        );
        assert_eq!(d["c"].economy, Some(6.0));
        assert_eq!(fuel_total(12.5, 102.37), 127_963.0);
    }
}
