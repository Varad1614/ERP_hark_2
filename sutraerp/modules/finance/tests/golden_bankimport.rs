//! Golden vectors for `treasury::bankfile`, generated from Everything's original
//! `accounting/src/lib/{bankimport,csv,money}.js` by `tools/golden/generate.mjs`.

use serde::Deserialize;
use serde_json::Value;
use sutra_finance::treasury::bankfile::{
    dedupe, guess_mapping, parse_csv, parse_csv_statement_original, parse_money, parse_ofx,
    parse_statement_date, CsvOptions, DateStyle, Line, Mapping,
};

#[derive(Deserialize)]
struct Fixture<I> {
    cases: Vec<Case<I>>,
}

#[derive(Deserialize)]
struct Case<I> {
    input: I,
    expected: Value,
}

fn load<I: for<'de> Deserialize<'de>>(text: &str) -> Vec<Case<I>> {
    let f: Fixture<I> = serde_json::from_str(text).expect("valid fixture");
    assert!(!f.cases.is_empty());
    f.cases
}

fn style(s: &str) -> DateStyle {
    if s == "dmy" {
        DateStyle::Dmy
    } else {
        DateStyle::Mdy
    }
}

fn mapping_json(m: &Mapping) -> Value {
    serde_json::json!({
        "date": m.date, "desc": m.desc, "amount": m.amount, "debit": m.debit, "credit": m.credit,
        "mode": if m.split { "split" } else { "single" },
    })
}

fn line_json(l: &Line) -> Value {
    serde_json::json!({ "date": l.date.to_string(), "amount": l.amount, "desc": l.desc, "fitid": l.fitid })
}

#[derive(Deserialize)]
struct Text {
    text: String,
}

#[test]
fn money_matches_everything() {
    for (i, c) in load::<Text>(include_str!("golden/bankimport_money.json"))
        .iter()
        .enumerate()
    {
        assert_eq!(
            Value::from(parse_money(&c.input.text)),
            c.expected,
            "case {i}: {:?}",
            c.input.text
        );
    }
}

#[derive(Deserialize)]
struct DateIn {
    text: String,
    style: String,
}

#[test]
fn dates_match_everything() {
    for (i, c) in load::<DateIn>(include_str!("golden/bankimport_date.json"))
        .iter()
        .enumerate()
    {
        let got = parse_statement_date(&c.input.text, style(&c.input.style))
            .map_or(Value::Null, |d| Value::from(d.to_string()));
        assert_eq!(
            got, c.expected,
            "case {i}: {:?} {}",
            c.input.text, c.input.style
        );
    }
}

#[test]
fn csv_matches_everything() {
    for (i, c) in load::<Text>(include_str!("golden/bankimport_csv.json"))
        .iter()
        .enumerate()
    {
        let got = serde_json::to_value(parse_csv(&c.input.text, None)).expect("json");
        assert_eq!(got, c.expected, "case {i}: {:?}", c.input.text);
    }
}

#[derive(Deserialize)]
struct Headers {
    headers: Vec<String>,
}

#[test]
fn mapping_matches_everything() {
    for (i, c) in load::<Headers>(include_str!("golden/bankimport_mapping.json"))
        .iter()
        .enumerate()
    {
        let got = mapping_json(&guess_mapping(&c.input.headers));
        assert_eq!(got, c.expected, "case {i}: {:?}", c.input.headers);
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct StatementIn {
    text: String,
    date_style: String,
    invert: bool,
}

#[test]
fn csv_statements_match_everything() {
    for (i, c) in load::<StatementIn>(include_str!("golden/bankimport_statement.json"))
        .iter()
        .enumerate()
    {
        let opts = CsvOptions {
            date_style: style(&c.input.date_style),
            invert: c.input.invert,
        };
        let got = match parse_csv_statement_original(&c.input.text, opts) {
            None => {
                serde_json::json!({ "headers": [], "hasHeader": null, "mapping": null, "lines": [], "skipped": [] })
            }
            Some(st) => serde_json::json!({
                "headers": st.headers,
                "hasHeader": st.header_rows == 1,
                "mapping": mapping_json(&st.mapping),
                "lines": st.lines.iter().map(line_json).collect::<Vec<_>>(),
                "skipped": st.skipped.iter().map(|s| serde_json::json!({"row": s.row, "reason": s.reason})).collect::<Vec<_>>(),
            }),
        };
        assert_eq!(got, c.expected, "case {i}: {:?}", c.input.text);
    }
}

#[test]
fn ofx_matches_everything() {
    for (i, c) in load::<Text>(include_str!("golden/bankimport_ofx.json"))
        .iter()
        .enumerate()
    {
        let got: Vec<Value> = parse_ofx(&c.input.text)
            .iter()
            .map(|s| {
                serde_json::json!({
                    "lines": s.lines.iter().map(|l| serde_json::json!({
                        "date": l.date.to_string(), "amount": l.amount, "desc": l.desc,
                        "fitid": l.fitid, "checkNum": l.reference,
                    })).collect::<Vec<_>>(),
                    "balance": s.balance,
                    "accountNumber": s.account_number,
                    "accountType": s.account_type,
                })
            })
            .collect();
        assert_eq!(Value::from(got), c.expected, "case {i}: {:?}", c.input.text);
    }
}

#[derive(Deserialize)]
struct JsLine {
    date: String,
    amount: i64,
    desc: String,
    fitid: String,
}

impl JsLine {
    fn line(&self) -> Line {
        Line {
            date: self.date.parse().expect("date"),
            amount: self.amount,
            desc: self.desc.clone(),
            fitid: self.fitid.clone(),
            reference: String::new(),
        }
    }
}

#[derive(Deserialize)]
struct DedupeIn {
    fresh: Vec<JsLine>,
    existing: Vec<JsLine>,
}

#[test]
fn dedupe_matches_everything() {
    for (i, c) in load::<DedupeIn>(include_str!("golden/bankimport_dedupe.json"))
        .iter()
        .enumerate()
    {
        let fresh: Vec<Line> = c.input.fresh.iter().map(JsLine::line).collect();
        let existing: Vec<Line> = c.input.existing.iter().map(JsLine::line).collect();
        let got = serde_json::to_value(dedupe(&fresh, &existing)).expect("json");
        assert_eq!(got, c.expected, "case {i}");
    }
}
