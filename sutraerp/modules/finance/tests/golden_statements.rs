//! Golden vectors for `ledger::statements`, generated from Everything's original
//! `accounting/src/lib/reports.js` (profitAndLoss, balanceSheet, cashFlow, generalLedger) over
//! random journal books by `tools/golden/generate.mjs`.

use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sutra_finance::ledger::statements::{
    balance_sheet, cash_flow, general_ledger, income_expenditure, Account, Books, Columns, Compare,
    EntryRef, Posting, RowKind, SheetColumns, Statement, StatementGroup,
};
use uuid::Uuid;

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
    accounts: Vec<Acc>,
    txns: Vec<Txn>,
    from: NaiveDate,
    to: NaiveDate,
    as_of: NaiveDate,
    columns: String,
    compare: Option<String>,
    sheet: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Acc {
    id: Uuid,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    parent_id: Option<Uuid>,
    role: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Txn {
    id: Uuid,
    date: NaiveDate,
    created_at: i64,
    lines: Vec<Line>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    account_id: Uuid,
    debit: i64,
    credit: i64,
}

fn num(d: Decimal) -> Value {
    let n = d.normalize();
    assert_eq!(n.scale(), 0, "whole amounts only: {d}");
    json!(i64::try_from(n.mantissa()).expect("fits"))
}

fn kind(k: RowKind) -> &'static str {
    match k {
        RowKind::Section => "section",
        RowKind::Account => "account",
        RowKind::Subtotal => "subtotal",
        RowKind::Total => "total",
        RowKind::Grand => "grand",
        RowKind::Row => "row",
    }
}

fn compact(s: &Statement, expected: &Value) -> Value {
    let rows: Vec<Value> = s
        .rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let who = r
                .account_id
                .map(|a| json!(a))
                .unwrap_or_else(|| json!(r.key.clone().unwrap_or_default()));
            let mut v: Vec<Value> = r
                .values
                .iter()
                .map(|x| x.map_or(Value::Null, num))
                .collect();
            if expected[i][3][3] == "tie" {
                v[3] = json!("tie");
            }
            json!([kind(r.kind), r.depth, who, v])
        })
        .collect();
    json!(rows)
}

#[test]
fn statements_match_everything() {
    let f: Fixture =
        serde_json::from_str(include_str!("golden/statements.json")).expect("valid fixture");
    assert!(f.cases.len() >= 50);
    for (i, c) in f.cases.iter().enumerate() {
        let inp = &c.input;
        let accounts: Vec<Account> = inp
            .accounts
            .iter()
            .map(|a| Account {
                id: a.id,
                code: String::new(),
                name: a.name.clone(),
                group: StatementGroup::from_everything(&a.kind).expect("type"),
                parent: a.parent_id,
            })
            .collect();
        let fund = inp
            .accounts
            .iter()
            .find(|a| a.role.as_deref() == Some("retainedEarnings"))
            .map(|a| a.id);
        let mut txns: Vec<&Txn> = inp.txns.iter().collect();
        txns.sort_by(|a, b| (a.date, a.created_at, a.id).cmp(&(b.date, b.created_at, b.id)));
        let mut postings = Vec::new();
        let mut entries = Vec::new();
        for (e, t) in txns.iter().enumerate() {
            entries.push(EntryRef {
                id: t.id,
                ..EntryRef::default()
            });
            for l in &t.lines {
                let amount = Decimal::from(l.debit - l.credit);
                if !amount.is_zero() {
                    postings.push(Posting {
                        date: t.date,
                        account: l.account_id,
                        amount,
                        entry: e,
                        memo: String::new(),
                    });
                }
            }
        }
        let books = Books::new(accounts, postings, entries, 4, fund);
        let ex = &c.expected;

        let columns = match inp.columns.as_str() {
            "month" => Columns::Month,
            "quarter" => Columns::Quarter,
            "year" => Columns::Year,
            _ => Columns::Total,
        };
        let compare = match inp.compare.as_deref() {
            Some("prevYear") => Some(Compare::PreviousYear),
            Some("prevPeriod") => Some(Compare::PreviousPeriod),
            _ => None,
        };
        let pnl = income_expenditure(
            &books,
            inp.from,
            inp.to,
            if compare.is_some() {
                Columns::Total
            } else {
                columns
            },
            compare,
        );
        assert_eq!(
            compact(&pnl, &ex["pnl"]["rows"]),
            ex["pnl"]["rows"],
            "case {i} pnl rows"
        );
        assert_eq!(
            num(pnl.net.expect("net")),
            ex["pnl"]["net"],
            "case {i} pnl net"
        );
        assert_eq!(
            json!(pnl.columns.len()),
            ex["pnl"]["cols"],
            "case {i} pnl cols"
        );

        let (sheet_cols, prev) = match inp.sheet.as_str() {
            "month" => (SheetColumns::Month, false),
            "quarter" => (SheetColumns::Quarter, false),
            "prevYear" => (SheetColumns::Total, true),
            _ => (SheetColumns::Total, false),
        };
        let bs = balance_sheet(&books, inp.as_of, sheet_cols, prev);
        assert_eq!(
            compact(&bs, &ex["bs"]["rows"]),
            ex["bs"]["rows"],
            "case {i} bs rows"
        );
        assert_eq!(
            json!(bs.balanced),
            ex["bs"]["balanced"],
            "case {i} balanced"
        );
        assert_eq!(
            json!(bs.columns.len()),
            ex["bs"]["cols"],
            "case {i} bs cols"
        );

        let cf = cash_flow(&books, inp.from, inp.to);
        assert_eq!(
            compact(&cf, &ex["cf"]["rows"]),
            ex["cf"]["rows"],
            "case {i} cf rows"
        );
        assert_eq!(
            json!(cf.reconciled),
            json!(ex["cf"]["net"] == ex["cf"]["cash"]),
            "case {i} cf check"
        );

        let gl = general_ledger(&books, inp.from, inp.to, None);
        let got: Vec<Value> = gl
            .rows
            .iter()
            .map(|r| {
                let who = match (r.kind, &r.detail, r.key.as_deref()) {
                    (RowKind::Section, _, _) => json!(r.account_id),
                    (RowKind::Subtotal, _, _) => Value::Null,
                    (_, _, Some("Beginning Balance")) => json!("open"),
                    (_, Some(d), _) => json!(d.date.to_string()),
                    _ => Value::Null,
                };
                let v: Vec<Value> = if r.kind == RowKind::Section {
                    vec![]
                } else {
                    r.values.iter().flatten().map(|x| num(*x)).collect()
                };
                json!([kind(r.kind), who, v])
            })
            .collect();
        assert_eq!(json!(got), ex["gl"], "case {i} gl");
    }
}
