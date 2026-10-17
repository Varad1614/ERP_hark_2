//! Golden vectors for `treasury::reconcile`, generated from Everything's original
//! `accounting/src/lib/reconcile.js`, `banking.js` (findMatches) and `reports.js`
//! (reconDiscrepancy) over random books by `tools/golden/generate.mjs`.

use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use sutra_finance::treasury::reconcile::{
    beginning_balance, candidates, discrepancies, find_matches, finish_amounts,
    last_reconciliation, reconciled_ids, summarize, MatchLine, OpenDoc, Posting, Recon,
    SuggestionKind,
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
    recons: Vec<Rec>,
    bank_lines: Vec<BankLine>,
    statement_date: Option<NaiveDate>,
    line: Line,
    docs: Vec<Doc>,
    window: Option<i64>,
    select_p: f64,
    ending_delta: i64,
    select_seed: i64,
}
#[derive(Deserialize)]
struct Acc {
    id: Uuid,
    name: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Txn {
    id: Uuid,
    date: NaiveDate,
    created_at: i64,
    lines: Vec<TxnLine>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TxnLine {
    account_id: Uuid,
    debit: i64,
    credit: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rec {
    id: Uuid,
    account_id: Uuid,
    statement_date: NaiveDate,
    finished_at: i64,
    status: String,
    cleared_ids: Vec<Uuid>,
    amounts: HashMap<Uuid, i64>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BankLine {
    account_id: Uuid,
    txn_id: Uuid,
    status: String,
}
#[derive(Deserialize)]
struct Line {
    date: NaiveDate,
    amount: i64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Doc {
    id: Uuid,
    #[serde(rename = "type")]
    kind: String,
    date: NaiveDate,
    due_date: Option<NaiveDate>,
    voided: bool,
    open: i64,
}

fn postings_on(txns: &[Txn], account: Uuid) -> Vec<Posting> {
    let mut sorted: Vec<&Txn> = txns.iter().collect();
    sorted.sort_by_key(|t| (t.date, t.created_at, t.id));
    sorted
        .iter()
        .flat_map(|t| {
            t.lines
                .iter()
                .filter(move |l| l.account_id == account && l.debit != l.credit)
                .map(move |l| Posting {
                    entry: t.id,
                    date: t.date,
                    amount: l.debit - l.credit,
                })
        })
        .collect()
}

fn amount_on(txns: &[Txn], account: Uuid, id: Uuid) -> Option<i64> {
    txns.iter().find(|t| t.id == id).map(|t| {
        t.lines
            .iter()
            .filter(|l| l.account_id == account)
            .map(|l| l.debit - l.credit)
            .sum()
    })
}

#[test]
fn reconcile_matches_everything() {
    let raw = include_str!("golden/reconcile.json");
    let fx: Fixture = serde_json::from_str(raw).unwrap();
    assert!(fx.cases.len() >= 100);
    for (n, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let bank = i.accounts[0].id;
        let names: HashMap<Uuid, &str> = i.accounts.iter().map(|a| (a.id, a.name.as_str())).collect();
        let recons: Vec<Recon> = i
            .recons
            .iter()
            .map(|r| Recon {
                id: r.id,
                account: r.account_id,
                statement_date: r.statement_date,
                finished_at: r.finished_at,
                done: r.status == "done",
                cleared: r.cleared_ids.clone(),
                amounts: r.amounts.clone(),
            })
            .collect();
        let postings = postings_on(&i.txns, bank);
        let reconciled = reconciled_ids(&recons, bank);
        let cleared: HashSet<Uuid> = i
            .bank_lines
            .iter()
            .filter(|l| l.account_id == bank && l.status == "added")
            .map(|l| l.txn_id)
            .collect();
        let items = candidates(&postings, &reconciled, &cleared, i.statement_date);
        let got: Vec<Value> = items.iter().map(|x| json!([x.entry, x.amount, x.cleared])).collect();
        assert_eq!(json!(got), c.expected["candidates"], "case {n}: candidates");

        // The spec's LCG selection.
        let mut seed = i.select_seed;
        let mut selected_order = Vec::new();
        for it in &items {
            seed = (seed * 1_103_515_245 + 12_345) % 2_147_483_648;
            if (seed as f64) / 2_147_483_648.0 < i.select_p {
                selected_order.push(it.entry);
            }
        }
        assert_eq!(json!(selected_order), c.expected["selected"], "case {n}: selection");
        let selected: HashSet<Uuid> = selected_order.iter().copied().collect();
        let current = |id: Uuid| amount_on(&i.txns, bank, id);
        let begin = beginning_balance(&recons, bank, current);
        assert_eq!(json!(begin), c.expected["beginning"], "case {n}: beginning");
        let cleared_sum = begin + items.iter().filter(|x| selected.contains(&x.entry)).map(|x| x.amount).sum::<i64>();
        let s = summarize(1, begin, &items, &selected, cleared_sum + i.ending_delta);
        assert_eq!(serde_json::to_value(&s).unwrap(), c.expected["summary"], "case {n}: summary");
        let amounts = finish_amounts(&selected_order, current);
        let want: HashMap<Uuid, i64> = serde_json::from_value(c.expected["finish"]["amounts"].clone()).unwrap();
        assert_eq!(amounts, want, "case {n}: finish amounts");
        assert_eq!(json!(begin), c.expected["finish"]["beginningBalance"], "case {n}: finish beginning");
        assert_eq!(
            json!(last_reconciliation(&recons, bank).map(|r| r.id)),
            c.expected["last"],
            "case {n}: last"
        );

        let disc = discrepancies(&recons, |acc, id| amount_on(&i.txns, acc, id));
        let got: Vec<Value> = disc
            .iter()
            .map(|x| json!([names[&x.account], x.statement_date, x.entry, x.was, x.now, x.change]))
            .collect();
        assert_eq!(json!(got), c.expected["discrepancies"], "case {n}: discrepancies");

        let taken: HashSet<Uuid> = i.bank_lines.iter().map(|l| l.txn_id).collect();
        let docs: Vec<OpenDoc> = i
            .docs
            .iter()
            .map(|d| OpenDoc {
                id: d.id,
                receivable: d.kind == "invoice",
                date: d.date,
                due: d.due_date,
                voided: d.voided,
                open: d.open,
            })
            .collect();
        let m = find_matches(
            &MatchLine { date: i.line.date, amount: i.line.amount },
            &postings,
            i.window.unwrap_or(10),
            &taken,
            &reconciled,
            &docs,
        );
        let got: Vec<Value> = m
            .iter()
            .map(|x| {
                let kind = match x.kind {
                    SuggestionKind::Entry => "txn",
                    SuggestionKind::Open => "open",
                };
                json!([kind, x.id, x.score])
            })
            .collect();
        assert_eq!(json!(got), c.expected["matches"], "case {n}: matches");
    }
}
