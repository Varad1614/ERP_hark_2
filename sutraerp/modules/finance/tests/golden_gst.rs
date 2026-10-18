//! Golden vectors for `gst::rules`, generated from Everything's original
//! `invoicing/src/lib/{money,taxnote,doc}.js` (`docTotals`, `taxNoteValue` / `taxNoteText` /
//! `chargesUntaxed`, `formatDocNumber`) by `tools/golden/generate.mjs` (spec `gst-invoicing.mjs`).

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use std::str::FromStr;
use sutra_finance::gst::rules::{
    charges_untaxed, doc_totals, format_doc_number, is_tax_note_preset, tax_note_text,
    tax_note_value, Discount, Doc, Item, NamedTax,
};

#[derive(Deserialize)]
struct Fixture {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    input: Value,
    expected: Value,
}

fn dec(v: &Value) -> Decimal {
    match v {
        Value::Number(n) => Decimal::from_str(&n.to_string())
            .or_else(|_| Decimal::from_scientific(&n.to_string()))
            .expect("number"),
        _ => Decimal::ZERO,
    }
}

/// JSON numbers as JavaScript prints them: whole amounts without a fraction.
fn num(d: Decimal) -> Value {
    let n = d.normalize();
    if n.scale() == 0 {
        json!(i64::try_from(n.mantissa()).expect("fits"))
    } else {
        json!(f64::from_str(&n.to_string()).expect("number"))
    }
}

fn taxes(v: &Value) -> Vec<NamedTax> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|t| NamedTax {
                    label: t["label"].as_str().unwrap_or_default().to_owned(),
                    rate: dec(&t["rate"]),
                })
                .collect()
        })
        .unwrap_or_default()
}

fn doc(v: &Value) -> Doc {
    Doc {
        items: v["items"]
            .as_array()
            .expect("items")
            .iter()
            .map(|it| Item {
                qty: dec(&it["qty"]),
                rate: dec(&it["rate"]),
                taxes: it.get("taxes").map(taxes),
            })
            .collect(),
        taxes: taxes(&v["taxes"]),
        discount: match v["discountType"].as_str() {
            Some("percent") => Discount::Percent(dec(&v["discountValue"])),
            Some("fixed") => Discount::Fixed(dec(&v["discountValue"])),
            _ => Discount::None,
        },
        tax_inclusive: v["taxInclusive"].as_bool().unwrap_or(false),
    }
}

fn note(v: &Value) -> Option<&str> {
    v.get("taxNote").and_then(Value::as_str)
}

#[test]
fn gst_documents_match_everything() {
    let f: Fixture =
        serde_json::from_str(include_str!("golden/gst_invoicing.json")).expect("valid fixture");
    assert!(f.cases.len() >= 200);
    let mut compared = 0;
    for (i, c) in f.cases.iter().enumerate() {
        let input = &c.input;
        let got = match input["kind"].as_str() {
            Some("number") => {
                let n = input["n"].as_f64().expect("n").floor() as i64;
                let date =
                    NaiveDate::parse_from_str(input["date"].as_str().expect("date"), "%Y-%m-%d")
                        .expect("date");
                json!({"number": format_doc_number(input["prefix"].as_str().unwrap_or_default(), n, date)})
            }
            Some("note") => {
                let value = tax_note_value(
                    note(&input["settings"]),
                    note(&input["doc"]),
                    note(&input["client"]),
                );
                let text = if is_tax_note_preset(&value) {
                    Value::Null
                } else {
                    json!(tax_note_text(&value))
                };
                json!({"value": value, "text": text, "untaxed": charges_untaxed(&doc(&input["doc"]))})
            }
            _ => {
                let t = doc_totals(&doc(&input["doc"]));
                let mut got = json!({
                    "subtotal": num(t.subtotal),
                    "discount": num(t.discount),
                    "taxable": num(t.taxable),
                    "tax": num(t.tax),
                    "taxes": t.taxes.iter().map(|r| json!({"label": r.label, "rate": num(r.rate), "amount": num(r.amount)})).collect::<Vec<_>>(),
                    "total": num(t.total),
                });
                // Figures landing on half a paisa in JS floats are flagged null by the spec.
                for k in ["discount", "taxable", "tax", "taxes", "total"] {
                    if c.expected[k].is_null() {
                        got[k] = Value::Null;
                    }
                }
                got
            }
        };
        assert_eq!(got, c.expected, "case {i}: {input}");
        compared += 1;
    }
    assert_eq!(compared, f.cases.len());
}
