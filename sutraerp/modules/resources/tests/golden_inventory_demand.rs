//! Golden vectors for `inventory::demand` and `inventory::lots`, generated from Everything's
//! original `inventory/src/lib/{demand,reorder,lots}.js` by `tools/golden/generate.mjs`.

use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::Value;
use sutra_resources::inventory::demand::{
    cover_days, cover_label, demand_rates, on_order_qty, rate_label, restock_days, running_out,
    stock_status, suggested_qty, suggested_threshold, Movement, Order, Product,
};
use sutra_resources::inventory::lots::{
    add_lot, consume_lot_at, consume_lots, expiring_products, expiry_status, lots_total,
    normalize_lots, resize_lots, Lot, Tracked,
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
struct Plan {
    lead_time_days: f64,
    safety_days: f64,
    coverage_days: f64,
    incoming: f64,
}
#[derive(Deserialize)]
struct Op {
    op: String,
    date: String,
    qty: f64,
}
#[derive(Deserialize)]
struct Input {
    now: f64,
    today: String,
    days: f64,
    entries: Vec<Movement>,
    products: Vec<Product>,
    plans: Vec<Plan>,
    orders: Vec<Order>,
    lots: Vec<Lot>,
    ops: Vec<Op>,
    horizon: i64,
    tracked: Vec<Tracked>,
    covers: Vec<Option<f64>>,
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn lots_json(l: &[Lot]) -> Value {
    serde_json::to_value(l).expect("lots")
}

fn nums(v: Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Number(n), Value::from),
        Value::Array(a) => Value::Array(a.into_iter().map(nums).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, nums(x))).collect()),
        other => other,
    }
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-9 * a.abs().max(1.0)
}

#[test]
fn inventory_demand_and_lots_match_everything() {
    let fx: Fixture =
        serde_json::from_str(include_str!("golden/inventory_demand.json")).expect("fixture");
    assert!(fx.cases.len() >= 150);
    for (ci, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let x = &c.expected;
        let w = |what: &str| format!("case {ci} {what}");

        let dr = demand_rates(&i.entries, i.days, i.now);
        assert_eq!(Some(dr.days), num(&x["rates"]["days"]), "{}", w("days"));
        assert_eq!(Some(dr.enough), x["rates"]["enough"].as_bool(), "{}", w("enough"));
        let want = x["rates"]["rates"].as_object().expect("rates");
        assert_eq!(dr.rates.len(), want.len(), "{}", w("rates len"));
        for (k, v) in want {
            assert!(close(dr.rates[k], v.as_f64().unwrap_or_default()), "{}", w("rate"));
        }

        for (pi, (p, plan)) in i.products.iter().zip(&i.plans).enumerate() {
            let e = &x["rows"][pi];
            let per_day = dr.rates.get(&p.id).copied().unwrap_or(0.0);
            let ww = |what: &str| format!("case {ci} product {pi} {what}");
            assert_eq!(cover_days(p.quantity, per_day), num(&e["cover"]), "{}", ww("cover"));
            assert_eq!(
                suggested_threshold(p, per_day, plan.lead_time_days, plan.safety_days),
                num(&e["threshold"]),
                "{}",
                ww("threshold")
            );
            assert_eq!(
                Some(running_out(p.quantity, per_day, plan.lead_time_days, plan.safety_days)),
                e["runningOut"].as_bool(),
                "{}",
                ww("runningOut")
            );
            assert_eq!(
                Some(restock_days(plan.lead_time_days, plan.safety_days) as f64),
                num(&e["restock"]),
                "{}",
                ww("restock")
            );
            assert_eq!(Some(rate_label(per_day, p.unit.as_deref()).as_str()), e["rateLabel"].as_str(), "{}", ww("rateLabel"));
            assert_eq!(
                Some(suggested_qty(p, 5.0, plan.incoming, per_day, plan.coverage_days)),
                num(&e["qty"]),
                "{}",
                ww("qty")
            );
            assert_eq!(Some(stock_status(p, 5.0)), e["status"].as_str(), "{}", ww("status"));
        }

        let oo = on_order_qty(&i.orders, &["ordered", "partial"]);
        assert_eq!(nums(serde_json::to_value(&oo).expect("oo")), nums(x["onOrder"].clone()), "{}", w("onOrder"));

        assert_eq!(nums(lots_json(&normalize_lots(&i.lots))), nums(x["normalized"].clone()), "{}", w("normalized"));
        assert_eq!(Some(lots_total(&i.lots)), num(&x["total"]), "{}", w("total"));
        let mut cur = i.lots.clone();
        for (si, o) in i.ops.iter().enumerate() {
            let e = &x["steps"][si];
            let taken = match o.op.as_str() {
                "add" => {
                    cur = add_lot(&cur, &o.date, o.qty);
                    None
                }
                "resize" => {
                    cur = resize_lots(&cur, o.qty);
                    None
                }
                "consume" => {
                    let (l, t) = consume_lots(&cur, o.qty);
                    cur = l;
                    Some(t)
                }
                _ => {
                    let (l, t) = consume_lot_at(&cur, &o.date, o.qty);
                    cur = l;
                    Some(t)
                }
            };
            assert_eq!(nums(lots_json(&cur)), nums(e["lots"].clone()), "{} {}", w("step lots"), o.op);
            match taken {
                None => assert!(e["taken"].is_null(), "{}", w("taken")),
                Some(t) => assert_eq!(nums(lots_json(&t)), nums(e["taken"].clone()), "{}", w("taken")),
            }
        }

        let today = NaiveDate::parse_from_str(&i.today, "%Y-%m-%d").expect("today");
        let s = expiry_status(&i.lots, today, i.horizon);
        assert_eq!(Some(s.expired), num(&x["expiry"]["expired"]), "{}", w("expired"));
        assert_eq!(Some(s.soon), num(&x["expiry"]["soon"]), "{}", w("soon"));
        assert_eq!(Some(s.soonest.as_str()), x["expiry"]["soonest"].as_str(), "{}", w("soonest"));
        let got: Vec<Value> = expiring_products(&i.tracked, today, i.horizon)
            .into_iter()
            .map(|e| {
                serde_json::json!({"id": e.id, "expired": e.status.expired, "soon": e.status.soon,
                    "soonest": e.status.soonest, "value": e.value})
            })
            .collect();
        assert_eq!(nums(Value::Array(got)), nums(x["expiring"].clone()), "{}", w("expiring"));

        let labels: Vec<String> = i.covers.iter().map(|c| cover_label(*c)).collect();
        let want: Vec<&str> = x["coverLabels"].as_array().expect("labels").iter().filter_map(Value::as_str).collect();
        assert_eq!(labels, want, "{}", w("coverLabels"));
    }
}
