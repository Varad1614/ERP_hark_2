//! Golden vectors for `admission::funnel`, generated from Everything's original
//! `crm/src/lib/funnel.js`, `forecast.js` and `dupes.js` over random enquiry books by
//! `tools/golden/generate.mjs`. Everything's terminal stage `won` is `admitted` here.

use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use sutra_student::admission::funnel::{
    avg_closed_value, avg_cycle_days, close_reason_breakdown, contact_duplicates,
    forecast_by_month, funnel_report, pipeline_value, probability_of, resolve_stages,
    stage_summary, stage_velocity, stalled, weighted_pipeline, win_rate, win_rate_by, Contact,
    Enquiry, Stage, ADMITTED,
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
    stages_config: Vec<Value>,
    deals: Vec<Value>,
    overrides: HashMap<String, f64>,
    from: String,
    months: u32,
    stale_days: i64,
    now: f64,
    contacts: Vec<Contact>,
}

/// Everything's `won` → `admitted`, in any stage-valued field.
fn st(s: &str) -> String {
    if s == "won" {
        ADMITTED.to_owned()
    } else {
        s.to_owned()
    }
}

fn map_value(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, v)| {
                    let v = match (k.as_str(), v) {
                        ("stage" | "id", Value::String(s)) => Value::String(st(s)),
                        _ => map_value(v),
                    };
                    (k.clone(), v)
                })
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.iter().map(map_value).collect()),
        other => other.clone(),
    }
}

fn close(a: f64, b: &Value, what: &str) {
    let b = b.as_f64().unwrap_or_else(|| panic!("{what}: {b} is not a number"));
    assert!((a - b).abs() < 1e-9, "{what}: {a} != {b}");
}

fn opt(a: Option<f64>, b: &Value, what: &str) {
    match a {
        None => assert!(b.is_null(), "{what}: None != {b}"),
        Some(a) => close(a, b, what),
    }
}

fn optu(a: Option<i64>, b: &Value, what: &str) {
    assert_eq!(a, b.as_i64(), "{what}");
}

fn n(v: &Value) -> u64 {
    v.as_u64().unwrap_or(0)
}

fn js_defaults() -> Vec<Stage> {
    [("new", "New", 0.1), ("qualified", "Qualified", 0.25), ("proposal", "Proposal", 0.5), ("negotiation", "Negotiation", 0.75)]
        .iter()
        .map(|(id, label, p)| Stage {
            id: (*id).into(),
            label: (*label).into(),
            probability: *p,
        })
        .collect()
}

#[test]
fn admission_funnel_matches_everything() {
    let raw = include_str!("golden/admission_funnel.json");
    let fx: Fixture = serde_json::from_str(raw).expect("fixture");
    assert!(fx.cases.len() >= 100);
    for (ci, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let x = &c.expected;
        let w = |s: &str| format!("case {ci} {s}");
        let config: Vec<Value> = i.stages_config.iter().map(map_value).collect();
        let stages = resolve_stages(&config, &js_defaults());
        let exp_stages = x["stages"].as_array().expect("stages");
        assert_eq!(stages.len(), exp_stages.len(), "{}", w("stages"));
        for (s, e) in stages.iter().zip(exp_stages) {
            assert_eq!(s.id, st(e["id"].as_str().unwrap_or_default()), "{}", w("stage id"));
            assert_eq!(s.label, e["label"].as_str().unwrap_or_default(), "{}", w("label"));
            close(s.probability, &e["probability"], &w("stage probability"));
        }
        let deals: Vec<Enquiry> = i
            .deals
            .iter()
            .map(|d| serde_json::from_value(map_value(d)).expect("deal"))
            .collect();
        let overrides: HashMap<String, f64> =
            i.overrides.iter().map(|(k, v)| (st(k), *v)).collect();

        // funnel
        let f = funnel_report(&deals, &stages);
        let ef = &x["funnel"];
        let rows = ef["rows"].as_array().expect("rows");
        assert_eq!(f.rows.len(), rows.len(), "{}", w("funnel rows"));
        for (r, e) in f.rows.iter().zip(rows) {
            assert_eq!(r.stage, e["stage"].as_str().unwrap_or_default(), "{}", w("row stage"));
            assert_eq!(r.label, e["label"].as_str().unwrap_or_default(), "{}", w("row label"));
            assert_eq!(r.gone, e["gone"].as_bool().unwrap_or_default(), "{}", w("gone"));
            assert_eq!(
                (r.entered as u64, r.advanced as u64, r.lost_here as u64, r.open as u64),
                (n(&e["entered"]), n(&e["advanced"]), n(&e["lostHere"]), n(&e["open"])),
                "{} {}",
                w("row counts"),
                r.stage
            );
            opt(r.advance_rate, &e["advanceRate"], &w("advanceRate"));
            opt(r.loss_share, &e["lossShare"], &w("lossShare"));
        }
        let t = &ef["totals"];
        assert_eq!(
            (f.totals.enquiries as u64, f.totals.admitted as u64, f.totals.lost as u64, f.totals.open as u64, f.totals.lost_share_base as u64),
            (n(&t["deals"]), n(&t["won"]), n(&t["lost"]), n(&t["open"]), n(&t["lostShareBase"])),
            "{}",
            w("totals")
        );
        opt(f.totals.admitted_rate, &t["wonRate"], &w("wonRate"));
        assert_eq!(f.untracked as u64, n(&ef["untracked"]), "{}", w("untracked"));

        // probabilities
        for (d, e) in deals.iter().zip(x["probabilities"].as_array().expect("p")) {
            close(probability_of(d, &overrides, &stages), e, &w("probability"));
        }

        // forecast (the original reads the close month in local time; won close times sit at
        // noon UTC mid-month, so offset 0 matches any zone)
        let fc = forecast_by_month(&deals, i.months, &i.from, &i.from, 0, &overrides, &stages);
        let efc = x["forecast"].as_array().expect("forecast");
        assert_eq!(fc.len(), efc.len(), "{}", w("forecast len"));
        for (r, e) in fc.iter().zip(efc) {
            assert_eq!(r.key, e["key"].as_str().unwrap_or_default(), "{}", w("key"));
            close(r.best, &e["best"], &w("best"));
            close(r.weighted, &e["weighted"], &w("weighted"));
            close(r.won, &e["won"], &w("won"));
            assert_eq!(r.count as u64, n(&e["count"]), "{}", w("count"));
        }

        // stage summary + pipeline
        let ss = stage_summary(&deals, &overrides, &stages);
        for (r, e) in ss.iter().zip(x["summary"].as_array().expect("summary")) {
            assert_eq!(r.stage, st(e["stage"].as_str().unwrap_or_default()), "{}", w("summary"));
            assert_eq!(r.count as u64, n(&e["count"]), "{}", w("summary count"));
            close(r.value, &e["value"], &w("summary value"));
            close(r.weighted, &e["weighted"], &w("summary weighted"));
        }
        close(pipeline_value(&deals), &x["pipeline"], &w("pipeline"));
        close(weighted_pipeline(&deals, &overrides, &stages), &x["weighted"], &w("weightedPipeline"));

        // win rates
        let wr = win_rate(&deals);
        assert_eq!(
            (wr.admitted as u64, wr.lost as u64),
            (n(&x["winRate"]["won"]), n(&x["winRate"]["lost"])),
            "{}",
            w("winRate")
        );
        opt(wr.rate, &x["winRate"]["rate"], &w("winRate rate"));
        let by = win_rate_by(&deals, |d| d.source.clone());
        let eby = x["bySource"].as_array().expect("bySource");
        assert_eq!(by.len(), eby.len(), "{}", w("bySource len"));
        for (g, e) in by.iter().zip(eby) {
            assert_eq!(g.key, e["key"].as_str().unwrap_or_default(), "{}", w("bySource key"));
            assert_eq!(
                (g.admitted as u64, g.lost as u64, g.total as u64),
                (n(&e["won"]), n(&e["lost"]), n(&e["total"])),
                "{}",
                w("bySource counts")
            );
            close(g.admitted_value, &e["wonValue"], &w("wonValue"));
            close(g.rate, &e["rate"], &w("bySource rate"));
        }

        // close reasons
        for (stage, field) in [(ADMITTED, "reasonsWon"), ("lost", "reasonsLost")] {
            let b = close_reason_breakdown(&deals, stage);
            let e = &x[field];
            assert_eq!(
                (b.unrecorded as u64, b.total as u64),
                (n(&e["unrecorded"]), n(&e["total"])),
                "{}",
                w(field)
            );
            let er = e["rows"].as_array().expect("rows");
            assert_eq!(b.rows.len(), er.len(), "{}", w(field));
            for (r, e) in b.rows.iter().zip(er) {
                assert_eq!(r.reason, e["reason"].as_str().unwrap_or_default(), "{}", w(field));
                assert_eq!(r.count as u64, n(&e["count"]), "{}", w(field));
                close(r.value, &e["value"], &w(field));
            }
        }

        let ac = avg_closed_value(&deals);
        opt(ac.admitted, &x["avgClosed"]["won"], &w("avgClosed won"));
        opt(ac.lost, &x["avgClosed"]["lost"], &w("avgClosed lost"));

        for (v, e) in stage_velocity(&deals, &stages)
            .iter()
            .zip(x["velocity"].as_array().expect("velocity"))
        {
            assert_eq!(v.stage, e["stage"].as_str().unwrap_or_default(), "{}", w("velocity"));
            optu(v.days, &e["days"], &w("velocity days"));
            assert_eq!(v.count as u64, n(&e["count"]), "{}", w("velocity count"));
        }

        let ids: Vec<&str> = stalled(&deals, i.stale_days, i.now)
            .iter()
            .map(|d| d.id.as_str())
            .collect();
        let eids: Vec<&str> = x["stalled"]
            .as_array()
            .expect("stalled")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(ids, eids, "{}", w("stalled"));
        optu(avg_cycle_days(&deals), &x["cycle"], &w("cycle"));

        // duplicates
        let pairs = contact_duplicates(&i.contacts);
        let ep = x["dupes"].as_array().expect("dupes");
        assert_eq!(pairs.len(), ep.len(), "{}", w("dupes len"));
        for (p, e) in pairs.iter().zip(ep) {
            assert_eq!(p.a.id, e["a"].as_str().unwrap_or_default(), "{}", w("dupe a"));
            assert_eq!(p.b.id, e["b"].as_str().unwrap_or_default(), "{}", w("dupe b"));
            let reasons: Vec<&str> = e["reasons"]
                .as_array()
                .expect("reasons")
                .iter()
                .filter_map(Value::as_str)
                .collect();
            assert_eq!(p.reasons, reasons, "{}", w("dupe reasons"));
        }
    }
}
