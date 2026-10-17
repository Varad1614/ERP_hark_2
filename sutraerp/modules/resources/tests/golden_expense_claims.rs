//! Golden vectors for `expenses::rules`, generated from Everything's original
//! `expense/src/lib/{money,util,dates,currencies,ocr,reports}.js` by `tools/golden/generate.mjs`.
//!
//! Mapping: JS `Map` results are `[currency, amount]` pairs in insertion order; `isEditable(null)`
//! is `is_editable(None)` and `isEditable({ status })` is `is_editable(Some(status))`;
//! `reportTotals(report, lines)` is `claim_totals` of the lines whose `reportId` is the report's,
//! with `has_receipt = !!receiptPath`. `now` is the case's `today`, so its year feeds
//! `parse_loose_date` and `today` feeds `normalize_ocr_result`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde::Deserialize;
use serde_json::{json, Value};
use sutra_resources::expenses::rules::{
    can_transition, claim_totals, extract_json, is_editable, match_category, next_status,
    normalize_ocr_result, normalize_text, parse_amount, parse_loose_date, resolve_currency, tokens,
    totals_by_currency, Category, LineFacts,
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
struct InReport {
    status: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InLine {
    report_id: String,
    amount: f64,
    currency: String,
    receipt_path: Option<String>,
    reconciled: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    today: String,
    base_currency: String,
    amounts: Vec<Value>,
    texts: Vec<String>,
    jsons: Vec<Value>,
    dates: Vec<(String, bool)>,
    currencies: Vec<Value>,
    categories: Vec<Category>,
    labels: Vec<Value>,
    ocr: Vec<Value>,
    statuses: Vec<String>,
    reports: Vec<Option<InReport>>,
    lines: Vec<InLine>,
    report_id: String,
}

/// Numbers compared as `f64` to 1e-9 (JS has one number type; serde keeps ints and floats apart).
fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .map_or(Value::Number(n), |f| json!((f * 1e9).round() / 1e9)),
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, norm(x))).collect()),
        x => x,
    }
}

fn pairs(v: &[(String, f64)]) -> Value {
    Value::Array(v.iter().map(|(c, a)| json!([c, a])).collect())
}

#[test]
fn expense_claims_match_everything() {
    let raw = include_str!("golden/expense_claims.json");
    let fx: Fixture = serde_json::from_str(raw).unwrap();
    assert_eq!(fx.cases.len(), 200);
    for (n, case) in fx.cases.iter().enumerate() {
        let i = &case.input;
        let now_year: i32 = i.today[0..4].parse().unwrap();
        let in_report: Vec<LineFacts> = i
            .lines
            .iter()
            .filter(|l| l.report_id == i.report_id)
            .map(|l| LineFacts {
                amount: l.amount,
                currency: l.currency.clone(),
                has_receipt: l.receipt_path.as_deref().is_some_and(|p| !p.is_empty()),
                reconciled: l.reconciled,
            })
            .collect();
        let totals = claim_totals(&in_report);
        let all = totals_by_currency(i.lines.iter().map(|l| (l.currency.as_str(), l.amount)));
        let amounts: Vec<Option<f64>> = i.amounts.iter().map(parse_amount).collect();
        let normalized: Vec<String> = i.texts.iter().map(|t| normalize_text(t)).collect();
        let toks: Vec<Vec<String>> = i.texts.iter().map(|t| tokens(t)).collect();
        let jsons: Vec<Option<Value>> = i.jsons.iter().map(extract_json).collect();
        let dates: Vec<Option<String>> = i
            .dates
            .iter()
            .map(|(d, day_first)| parse_loose_date(d, *day_first, now_year))
            .collect();
        let currencies: Vec<Option<String>> = i.currencies.iter().map(resolve_currency).collect();
        let categories: Vec<Option<String>> = i
            .labels
            .iter()
            .map(|l| match_category(l, &i.categories).map(|c| c.id.clone()))
            .collect();
        let ocr: Vec<Value> = i
            .ocr
            .iter()
            .map(|r| {
                let fields = normalize_ocr_result(r, &i.categories, &i.base_currency, &i.today);
                serde_json::to_value(fields).unwrap()
            })
            .collect();
        let next: Vec<Option<&str>> = i.statuses.iter().map(|s| next_status(s)).collect();
        let transitions: Vec<Vec<bool>> = i
            .statuses
            .iter()
            .map(|a| i.statuses.iter().map(|b| can_transition(a, b)).collect())
            .collect();
        let editable: Vec<bool> = i
            .reports
            .iter()
            .map(|r| is_editable(r.as_ref().map(|r| r.status.as_str())))
            .collect();
        let got = norm(json!({
            "amounts": amounts,
            "normalized": normalized,
            "tokens": toks,
            "jsons": jsons,
            "dates": dates,
            "currencies": currencies,
            "categories": categories,
            "ocr": ocr,
            "next": next,
            "transitions": transitions,
            "editable": editable,
            "totals": pairs(&all),
            "report": {
                "count": totals.count,
                "byCurrency": pairs(&totals.by_currency),
                "withReceipt": totals.with_receipt,
                "reconciled": totals.reconciled,
            },
        }));
        let want = norm(case.expected.clone());
        let ctx = json!({
            "today": i.today,
            "amounts": i.amounts,
            "texts": i.texts,
            "jsons": i.jsons,
            "dates": i.dates,
            "currencies": i.currencies,
            "labels": i.labels,
            "ocr": i.ocr,
        });
        // Per section, so a failure names the function that drifted.
        let want = want.as_object().unwrap();
        for (key, w) in want {
            assert_eq!(&got[key], w, "case {n}, {key}: input {ctx}");
        }
        assert_eq!(got.as_object().unwrap().len(), want.len(), "case {n}");
    }
}
