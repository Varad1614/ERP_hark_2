//! **ERP_EXTENSIONS 2C** — dated stock lots for items that track expiry, ported from
//! HeyPuter/everything `src/inventory/src/lib/lots.js` (MIT, see THIRD_PARTY_NOTICES.md).
//!
//! A lot is `{ date, qty }`: enough to answer "what do we need to use or write off this week?".
//! Dates are `YYYY-MM-DD`; `""` means the date is unknown (stock from before tracking, or a
//! receipt that did not say). First-expired-first-out: dated lots ascending, undated last. Pure.

use chrono::{Duration, NaiveDate};
use std::cmp::Ordering;

/// The date of a lot whose expiry nobody recorded.
pub const NO_DATE: &str = "";

/// One lot.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[schema(as = StockLot)]
pub struct Lot {
    /// `YYYY-MM-DD`, or `""` when unknown.
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub qty: f64,
}

fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

fn round3(n: f64) -> f64 {
    js_round(n * 1000.0) / 1000.0
}

/// A valid `YYYY-MM-DD` string, else [`NO_DATE`].
pub fn clean_date(d: &str) -> String {
    let b = d.as_bytes();
    let ok = b.len() == 10
        && b.iter().enumerate().all(|(i, c)| if i == 4 || i == 7 { *c == b'-' } else { c.is_ascii_digit() });
    if ok {
        d.to_owned()
    } else {
        NO_DATE.to_owned()
    }
}

fn fefo(a: &Lot, b: &Lot) -> Ordering {
    if a.date == b.date {
        return Ordering::Equal;
    }
    if a.date.is_empty() {
        return Ordering::Greater;
    }
    if b.date.is_empty() {
        return Ordering::Less;
    }
    a.date.cmp(&b.date)
}

/// Merge same dates, drop empties, sort FEFO.
pub fn normalize_lots(list: &[Lot]) -> Vec<Lot> {
    let mut by: Vec<Lot> = Vec::new();
    for lot in list {
        let qty = if lot.qty.is_finite() { lot.qty } else { 0.0 };
        if qty <= 0.0 {
            continue;
        }
        let date = clean_date(&lot.date);
        match by.iter_mut().find(|l| l.date == date) {
            Some(l) => l.qty = round3(l.qty + qty),
            None => by.push(Lot { date, qty: round3(qty) }),
        }
    }
    by.sort_by(fefo);
    by
}

pub fn lots_total(lots: &[Lot]) -> f64 {
    round3(lots.iter().map(|l| if l.qty.is_finite() { l.qty } else { 0.0 }).sum())
}

/// A receipt: one more lot (or more of an existing one).
pub fn add_lot(lots: &[Lot], date: &str, qty: f64) -> Vec<Lot> {
    if !(qty > 0.0) {
        return normalize_lots(lots);
    }
    let mut all = lots.to_vec();
    all.push(Lot { date: clean_date(date), qty });
    normalize_lots(&all)
}

/// Take `qty` in FEFO order: the lots left and the dated quantities taken. Taking more than
/// there is takes what there is.
pub fn consume_lots(lots: &[Lot], qty: f64) -> (Vec<Lot>, Vec<Lot>) {
    let mut need = if qty.is_finite() { qty } else { 0.0 };
    let mut rest = Vec::new();
    let mut taken = Vec::new();
    for lot in normalize_lots(lots) {
        if need <= 0.0 {
            rest.push(lot);
            continue;
        }
        let take = lot.qty.min(need);
        need = round3(need - take);
        if take > 0.0 {
            taken.push(Lot { date: lot.date.clone(), qty: take });
        }
        if lot.qty - take > 0.0 {
            rest.push(Lot { date: lot.date, qty: round3(lot.qty - take) });
        }
    }
    (rest, taken)
}

/// Take from the lot dated `date` first, spilling into FEFO order when it runs out.
pub fn consume_lot_at(lots: &[Lot], date: &str, qty: f64) -> (Vec<Lot>, Vec<Lot>) {
    let wanted = clean_date(date);
    let all = normalize_lots(lots);
    let Some(hit) = all.iter().find(|l| l.date == wanted).cloned() else {
        return consume_lots(&all, qty);
    };
    let need = if qty.is_finite() { qty } else { 0.0 };
    let from_hit = hit.qty.min(need);
    let hit_rest = round3(hit.qty - from_hit);
    let others: Vec<Lot> = all.into_iter().filter(|l| l.date != wanted).collect();
    let (spill_rest, spill_taken) = consume_lots(&others, round3(need - from_hit));
    let mut left = Vec::new();
    if hit_rest > 0.0 {
        left.push(Lot { date: wanted.clone(), qty: hit_rest });
    }
    left.extend(spill_rest);
    let mut taken = Vec::new();
    if from_hit > 0.0 {
        taken.push(Lot { date: wanted, qty: from_hit });
    }
    taken.extend(spill_taken);
    (normalize_lots(&left), taken)
}

/// Make the lots sum to `target`: growth is undated, shrinkage leaves FEFO.
pub fn resize_lots(lots: &[Lot], target: f64) -> Vec<Lot> {
    let want = if target.is_finite() { target.max(0.0) } else { 0.0 };
    let have = lots_total(lots);
    if want == have {
        return normalize_lots(lots);
    }
    if want > have {
        return add_lot(lots, NO_DATE, round3(want - have));
    }
    consume_lots(lots, round3(have - want)).0
}

/// How dated stock stands against the calendar.
#[derive(Debug, Clone, PartialEq, serde::Serialize, utoipa::ToSchema)]
#[schema(as = StockExpiryStatus)]
pub struct ExpiryStatus {
    /// Units already past their date.
    pub expired: f64,
    /// Units expiring within the horizon (expired excluded).
    pub soon: f64,
    /// The earliest date on hand (`""` when none is dated).
    pub soonest: String,
}

fn stamp(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

/// `expiryStatus` over merged lots, `today` the local date and `days` the horizon.
pub fn expiry_status(lots: &[Lot], today: NaiveDate, days: i64) -> ExpiryStatus {
    let t = stamp(today);
    let horizon = stamp(today + Duration::days(days));
    let mut s = ExpiryStatus { expired: 0.0, soon: 0.0, soonest: String::new() };
    for lot in normalize_lots(lots) {
        if lot.date.is_empty() {
            continue;
        }
        if s.soonest.is_empty() {
            s.soonest = lot.date.clone();
        }
        if lot.date < t {
            s.expired = round3(s.expired + lot.qty);
        } else if lot.date <= horizon {
            s.soon = round3(s.soon + lot.qty);
        }
    }
    s
}

/// One tracked product for [`expiring_products`].
#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tracked {
    pub id: String,
    #[serde(default)]
    pub cost: f64,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub track_expiry: bool,
    #[serde(default)]
    pub lots: Vec<Lot>,
}

/// An item with something expired or expiring.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Expiring {
    pub id: String,
    pub status: ExpiryStatus,
    /// (expired + soon) × cost, two decimals.
    pub value: f64,
}

/// Tracked products with anything expired or expiring, expired first, then by value at cost.
pub fn expiring_products(products: &[Tracked], today: NaiveDate, days: i64) -> Vec<Expiring> {
    let mut out: Vec<Expiring> = products
        .iter()
        .filter(|p| !p.archived && p.track_expiry)
        .filter_map(|p| {
            let s = expiry_status(&p.lots, today, days);
            if s.expired <= 0.0 && s.soon <= 0.0 {
                return None;
            }
            let value = js_round((s.expired + s.soon) * p.cost * 100.0) / 100.0;
            Some(Expiring { id: p.id.clone(), status: s, value })
        })
        .collect();
    out.sort_by(|a, b| {
        let ea = a.status.expired > 0.0;
        let eb = b.status.expired > 0.0;
        eb.cmp(&ea).then(b.value.partial_cmp(&a.value).unwrap_or(Ordering::Equal))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(date: &str, qty: f64) -> Lot {
        Lot { date: date.into(), qty }
    }

    #[test]
    fn fefo_consumes_dated_first() {
        let lots = vec![l("", 5.0), l("2027-03-01", 2.0), l("2027-01-01", 3.0), l("2027-01-01", 1.0)];
        assert_eq!(normalize_lots(&lots), vec![l("2027-01-01", 4.0), l("2027-03-01", 2.0), l("", 5.0)]);
        let (rest, taken) = consume_lots(&lots, 5.0);
        assert_eq!(taken, vec![l("2027-01-01", 4.0), l("2027-03-01", 1.0)]);
        assert_eq!(lots_total(&rest), 6.0);
        let (_, taken) = consume_lot_at(&lots, "", 6.0);
        assert_eq!(taken, vec![l("", 5.0), l("2027-01-01", 1.0)]);
        assert_eq!(resize_lots(&lots, 13.0).last(), Some(&l("", 7.0)));
        let today = NaiveDate::from_ymd_opt(2027, 1, 15).unwrap_or_default();
        let s = expiry_status(&lots, today, 60);
        assert_eq!((s.expired, s.soon, s.soonest.as_str()), (4.0, 2.0, "2027-01-01"));
        assert_eq!(clean_date("2027-1-1"), "");
    }
}
