//! **ERP_EXTENSIONS 2C** — stock demand and reorder suggestions, ported from HeyPuter/everything
//! `src/inventory/src/lib/demand.js`, `reorder.js` (`suggestedQty`, `lineRemaining`,
//! `onOrderQty`), `reports.js` (`movementByProduct`, `logReach`) and `units.js` (MIT, see
//! THIRD_PARTY_NOTICES.md).
//!
//! A rate over a window, explainable in one sentence: units out per day from the movement log,
//! days of cover, the reorder point a supplier's lead time plus safety days implies, and how
//! much to order. When the log is shorter than a week it says nothing and the reorder level
//! someone typed stands: suggestions never write themselves. Pure; timestamps in milliseconds.

use std::collections::BTreeMap;

const DAY_MS: f64 = 86_400_000.0;
/// The window demand is measured over.
pub const DEMAND_DAYS: f64 = 30.0;
/// Less log than this and a rate would be noise.
pub const MIN_DEMAND_DAYS: f64 = 7.0;
/// Days of cover asked for on top of the supplier's lead time.
pub const DEFAULT_SAFETY_DAYS: i64 = 3;
/// The store-wide low-stock threshold when an item has none.
pub const DEFAULT_LOW_STOCK: f64 = 5.0;

/// One movement of the log: `add`, `remove` or `recount`, signed `delta`.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Movement {
    #[serde(default)]
    pub product_id: Option<String>,
    pub kind: String,
    #[serde(default)]
    pub delta: f64,
    #[serde(default)]
    pub at: Option<f64>,
    #[serde(default)]
    pub reason: Option<String>,
}

/// A product as the demand rules see it.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Product {
    pub id: String,
    pub quantity: f64,
    pub unit: Option<String>,
    pub pack_size: Option<f64>,
    pub low_stock: Option<f64>,
    pub reorder_qty: Option<f64>,
}

// ------------------------------------------------------------------ units

/// Everything's unit suggestions: a known unit keeps its canonical casing.
const UNITS: &[&str] = &["each", "kg", "g", "lb", "oz", "L", "mL", "m", "cm", "ft"];

/// `normalizeUnit`.
pub fn normalize_unit(unit: Option<&str>) -> String {
    let s = unit.unwrap_or("").trim();
    if s.is_empty() || s.eq_ignore_ascii_case("each") {
        return "each".into();
    }
    if let Some(hit) = UNITS.iter().find(|u| u.eq_ignore_ascii_case(s)) {
        return (*hit).to_owned();
    }
    s.chars().take(12).collect()
}

/// A count of items (whole numbers) rather than a measure.
pub fn is_count_unit(unit: Option<&str>) -> bool {
    normalize_unit(unit) == "each"
}

fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// Clamp and round a quantity the way its unit allows; never negative.
pub fn round_qty(n: f64, unit: Option<&str>) -> f64 {
    if !n.is_finite() || n <= 0.0 {
        return 0.0;
    }
    if is_count_unit(unit) {
        js_round(n)
    } else {
        js_round(n * 1000.0) / 1000.0
    }
}

/// Pack size when the product is bought in packs of more than one, else 0.
pub fn pack_size_of(p: &Product) -> f64 {
    let n = p.pack_size.unwrap_or(0.0);
    if n > 1.0 {
        round_qty(n, p.unit.as_deref())
    } else {
        0.0
    }
}

fn round1(n: f64) -> f64 {
    js_round(n * 10.0) / 10.0
}

fn round3(n: f64) -> f64 {
    js_round(n * 1000.0) / 1000.0
}

// ------------------------------------------------------------------ the log

/// Units in and out of one product over a window.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MovementRow {
    pub product_id: String,
    pub r#in: f64,
    pub out: f64,
}

const NON_DEMAND: &[&str] = &["damaged", "expired", "internal", "lost"];

fn is_demand(m: &Movement) -> bool {
    !(m.kind == "remove" && m.reason.as_deref().is_some_and(|r| NON_DEMAND.contains(&r)))
}

/// `movementByProduct`: per product, in first-seen order.
pub fn movement_by_product(entries: &[Movement], days: f64, now: f64, demand_only: bool) -> Vec<MovementRow> {
    let from = now - days * DAY_MS;
    let mut rows: Vec<MovementRow> = Vec::new();
    for a in entries {
        let Some(pid) = a.product_id.as_deref().filter(|p| !p.is_empty()) else {
            continue;
        };
        if !matches!(a.kind.as_str(), "add" | "remove" | "recount") {
            continue;
        }
        // `a.at < from` is false for a missing time in JS (undefined < n).
        if a.at.is_some_and(|t| t < from) {
            continue;
        }
        if demand_only && !is_demand(a) {
            continue;
        }
        if a.delta == 0.0 || a.delta.is_nan() {
            continue;
        }
        let idx = match rows.iter().position(|r| r.product_id == pid) {
            Some(i) => i,
            None => {
                rows.push(MovementRow { product_id: pid.to_owned(), r#in: 0.0, out: 0.0 });
                rows.len() - 1
            }
        };
        let row = &mut rows[idx];
        if a.delta > 0.0 {
            row.r#in = round3(row.r#in + a.delta);
        } else {
            row.out = round3(row.out - a.delta);
        }
    }
    rows
}

/// The oldest timestamp in the log.
pub fn log_reach(entries: &[Movement]) -> Option<f64> {
    entries.iter().filter_map(|a| a.at).fold(None, |acc, t| match acc {
        Some(o) if o <= t => Some(o),
        _ => Some(t),
    })
}

/// Units out per day per product, and the span they were measured over.
#[derive(Debug, Clone, PartialEq)]
pub struct DemandRates {
    pub days: f64,
    pub rates: BTreeMap<String, f64>,
    pub enough: bool,
}

/// `demandRates`: demand only, over the log's reach inside the window.
pub fn demand_rates(entries: &[Movement], days: f64, now: f64) -> DemandRates {
    let span = match log_reach(entries) {
        None => 0.0,
        Some(oldest) => days.min(((now - oldest) / DAY_MS).max(0.0)),
    };
    let mut rates = BTreeMap::new();
    if span < MIN_DEMAND_DAYS {
        return DemandRates { days: round1(span), rates, enough: false };
    }
    for row in movement_by_product(entries, days, now, true) {
        if row.out <= 0.0 {
            continue;
        }
        rates.insert(row.product_id, row.out / span);
    }
    DemandRates { days: round1(span), rates, enough: true }
}

/// Days of stock left at the current rate; `None` when demand is unknown.
pub fn cover_days(quantity: f64, per_day: f64) -> Option<f64> {
    if per_day.is_nan() || per_day <= 0.0 {
        return None;
    }
    Some(round1(quantity.max(0.0) / per_day))
}

/// Supplier lead time plus safety days, whole days.
pub fn restock_days(lead_time_days: f64, safety_days: f64) -> i64 {
    let r = |x: f64| if x.is_finite() { js_round(x).max(0.0) as i64 } else { 0 };
    r(lead_time_days) + r(safety_days)
}

/// The reorder point demand implies, rounded up to something orderable; `None` when there is
/// no arithmetic to show.
pub fn suggested_threshold(p: &Product, per_day: f64, lead_time_days: f64, safety_days: f64) -> Option<f64> {
    let window = restock_days(lead_time_days, safety_days);
    if per_day.is_nan() || per_day <= 0.0 || window <= 0 {
        return None;
    }
    let need = per_day * window as f64;
    let pack = pack_size_of(p);
    let count = is_count_unit(p.unit.as_deref());
    let up = if pack > 0.0 {
        (need / pack - 1e-9).ceil() * pack
    } else if count {
        (need - 1e-9).ceil()
    } else {
        need
    };
    Some(round_qty(up.max(if count { 1.0 } else { 0.001 }), p.unit.as_deref()))
}

/// Will it run out before a restock could land?
pub fn running_out(quantity: f64, per_day: f64, lead_time_days: f64, safety_days: f64) -> bool {
    cover_days(quantity, per_day).is_some_and(|c| c <= restock_days(lead_time_days, safety_days) as f64)
}

fn js_num(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        format!("{n}")
    }
}

/// "≈1.4 units a day"; blank without a rate.
pub fn rate_label(per_day: f64, unit: Option<&str>) -> String {
    if per_day.is_nan() || per_day <= 0.0 {
        return String::new();
    }
    let n = if per_day >= 10.0 { js_round(per_day) } else { js_round(per_day * 10.0) / 10.0 };
    let word = if is_count_unit(unit) {
        if n == 1.0 { "unit".to_owned() } else { "units".to_owned() }
    } else {
        unit.unwrap_or("").to_owned()
    };
    format!("≈{} {word} a day", js_num(n))
}

/// "4 days of cover", "under a day of cover", "out of stock".
pub fn cover_label(cover: Option<f64>) -> String {
    let Some(c) = cover else { return String::new() };
    if c <= 0.0 {
        return "out of stock".into();
    }
    if c < 1.0 {
        return "under a day of cover".into();
    }
    let n = if c >= 10.0 { js_round(c) } else { js_round(c * 10.0) / 10.0 };
    format!("{} day{} of cover", js_num(n), if n == 1.0 { "" } else { "s" })
}

/// How many to order: a fixed reorder quantity wins; else enough to reach the target (twice
/// the threshold, or `coverage_days` of demand when set), less what is already on order,
/// rounded up to whole packs. `0` = nothing left to order.
pub fn suggested_qty(p: &Product, default_low: f64, incoming: f64, per_day: f64, coverage_days: f64) -> f64 {
    let fixed = p.reorder_qty.filter(|x| x.is_finite()).unwrap_or(0.0);
    let low = p.low_stock.unwrap_or(default_low);
    let low = if low.is_finite() { low } else { 0.0 };
    let on_hand = p.quantity;
    let inbound = incoming.max(0.0);
    let rate = per_day.max(0.0);
    let horizon = if coverage_days.is_finite() { js_round(coverage_days).max(0.0) } else { 0.0 };
    let target = if horizon > 0.0 && rate > 0.0 {
        (rate * horizon).max(low + 1.0).max(1.0)
    } else {
        (low * 2.0).max(low + 1.0).max(1.0)
    };
    let unit = p.unit.as_deref();
    let need = if fixed > 0.0 {
        fixed - inbound
    } else {
        (target - on_hand).max(if is_count_unit(unit) { 1.0 } else { 0.001 }) - inbound
    };
    if need <= 0.0 {
        return 0.0;
    }
    let pack = pack_size_of(p);
    if pack == 0.0 {
        return round_qty(need, unit);
    }
    round_qty((need / pack - 1e-9).ceil() * pack, unit)
}

/// `stockStatus`: `out` at zero, `low` at or below the threshold (0 = never warn), else `ok`.
pub fn stock_status(p: &Product, default_low: f64) -> &'static str {
    if p.quantity <= 0.0 {
        return "out";
    }
    let low = p.low_stock.unwrap_or(default_low);
    if low > 0.0 && p.quantity <= low {
        "low"
    } else {
        "ok"
    }
}

/// Units of an order line still undelivered (never negative).
pub fn line_remaining(qty: f64, received: f64) -> f64 {
    round3(qty - received).max(0.0)
}

/// An order line: product, ordered and received quantities.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrderLine {
    #[serde(default)]
    pub product_id: Option<String>,
    #[serde(default)]
    pub qty: f64,
    #[serde(default)]
    pub received: f64,
}

/// An order and its lines.
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
pub struct Order {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub lines: Vec<OrderLine>,
}

/// Units on their way per product, from orders still open (`ordered`, `partial`).
pub fn on_order_qty(orders: &[Order], open: &[&str]) -> BTreeMap<String, f64> {
    let mut map = BTreeMap::new();
    for o in orders.iter().filter(|o| open.contains(&o.status.as_str())) {
        for l in &o.lines {
            let rem = line_remaining(l.qty, l.received);
            let Some(pid) = l.product_id.as_deref().filter(|p| !p.is_empty()) else { continue };
            if rem <= 0.0 {
                continue;
            }
            let e = map.entry(pid.to_owned()).or_insert(0.0);
            *e = round3(*e + rem);
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mv(pid: &str, kind: &str, delta: f64, days_ago: f64, reason: Option<&str>) -> Movement {
        Movement {
            product_id: Some(pid.into()),
            kind: kind.into(),
            delta,
            at: Some(100.0 * DAY_MS - days_ago * DAY_MS),
            reason: reason.map(str::to_owned),
        }
    }

    #[test]
    fn rates_need_a_week_and_skip_write_offs() {
        let now = 100.0 * DAY_MS;
        let short = [mv("a", "remove", -5.0, 3.0, None)];
        assert!(!demand_rates(&short, 30.0, now).enough);
        let log = [
            mv("a", "add", 50.0, 20.0, None),
            mv("a", "remove", -10.0, 5.0, None),
            mv("a", "remove", -30.0, 4.0, Some("damaged")),
        ];
        let r = demand_rates(&log, 30.0, now);
        assert!(r.enough);
        assert_eq!(r.days, 20.0);
        assert_eq!(r.rates["a"], 0.5);
        let p = Product { id: "a".into(), quantity: 3.0, pack_size: Some(6.0), ..Product::default() };
        assert_eq!(cover_days(3.0, 0.5), Some(6.0));
        assert_eq!(suggested_threshold(&p, 0.5, 7.0, 3.0), Some(6.0));
        assert!(running_out(3.0, 0.5, 7.0, 3.0));
        assert_eq!(rate_label(0.5, None), "≈0.5 units a day");
        assert_eq!(cover_label(Some(1.0)), "1 day of cover");
        assert_eq!(suggested_qty(&p, 5.0, 0.0, 0.0, 0.0), 12.0);
        assert_eq!(suggested_qty(&p, 5.0, 12.0, 0.0, 0.0), 0.0);
        assert_eq!(normalize_unit(Some("KG")), "kg");
    }
}
