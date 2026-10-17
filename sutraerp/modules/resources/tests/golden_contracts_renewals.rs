//! Golden vectors for `contracts::{rules, ical}`, generated from Everything's original
//! `contracts/src/lib/{dates,store,ical}.js` by `tools/golden/generate.mjs`.

use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};
use sutra_resources::contracts::ical::{
    build_calendar, contract_events, escape_text, fold_line, DatedObligation, Event, EventContext,
    EventOptions, PRODID, UID_HOST,
};
use sutra_resources::contracts::rules::{
    advance_occurrence, agreement_end, all_obligations, completed_terms, contract_stats,
    due_obligations, needs_attention, next_term_end, parse_iso, renewal_info, reopened_series,
    repeat_anchor, rewind_occurrence, term_reconciliation, upcoming_renewals, within_series,
    Contract, Decision, DecisionKind, Obligation, Owner, Repeat,
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
    today: String,
    soon_days: i64,
    days: i64,
    renew_days: i64,
    contracts: Vec<InContract>,
    calendar: Vec<InEvent>,
    line: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InDecision {
    kind: String,
    note: String,
    for_term_end: Option<String>,
    until: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InRepeat {
    every: String,
    anchor: String,
    until: Option<String>,
    count: Option<i32>,
    contract_end: bool,
}
#[derive(Deserialize)]
struct InHist {
    due: Option<String>,
}
#[derive(Deserialize)]
struct InOb {
    id: String,
    text: String,
    owner: String,
    due: Option<String>,
    done: bool,
    repeat: Option<InRepeat>,
    history: Vec<InHist>,
}
#[derive(Deserialize)]
struct InTerm {
    end: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct InContract {
    id: String,
    title: String,
    status: String,
    value: Option<f64>,
    currency: String,
    billing: String,
    start_date: Option<String>,
    end_date: Option<String>,
    auto_renew: bool,
    term_months: i32,
    notice_days: i64,
    decision: Option<InDecision>,
    term_history: Vec<InTerm>,
    obligations: Vec<InOb>,
    updated_at: i64,
}
#[derive(Deserialize)]
struct InEvent {
    uid: String,
    date: String,
    sequence: i64,
    summary: String,
    description: String,
    url: String,
    categories: Vec<String>,
    alarms: Vec<i64>,
}

fn d(v: &Option<String>) -> Option<NaiveDate> {
    v.as_deref().and_then(parse_iso)
}

fn contract(c: &InContract) -> Contract {
    Contract {
        status: c.status.clone(),
        start_date: d(&c.start_date),
        end_date: d(&c.end_date),
        auto_renew: c.auto_renew,
        term_months: c.term_months,
        notice_days: c.notice_days,
        decision: c.decision.as_ref().map(|x| Decision {
            kind: DecisionKind::parse(&x.kind).unwrap(),
            date: None,
            note: x.note.clone(),
            for_term_end: d(&x.for_term_end),
            until: d(&x.until),
        }),
        value: c.value,
        currency: c.currency.clone(),
        billing: c.billing.clone(),
    }
}

fn obligation(o: &InOb) -> Obligation {
    Obligation {
        due: d(&o.due),
        done: o.done,
        repeat: o.repeat.as_ref().map(|r| Repeat {
            every: r.every.clone(),
            anchor: parse_iso(&r.anchor).unwrap(),
            until: d(&r.until),
            count: r.count,
            contract_end: r.contract_end,
        }),
        history: o.history.iter().map(|h| d(&h.due)).collect(),
    }
}

fn view(o: &Obligation) -> Value {
    json!({"due": o.due, "done": o.done, "history": o.history})
}

fn info_json(i: &sutra_resources::contracts::rules::RenewalInfo) -> Value {
    let mut v = serde_json::to_value(i).unwrap();
    v["decision"] = json!(i.decision.as_ref().map(|d| d.kind.as_str()));
    v
}

#[test]
fn contracts_renewals_match_everything() {
    let raw = include_str!("golden/contracts_renewals.json");
    let fx: Fixture = serde_json::from_str(raw).unwrap();
    assert_eq!(fx.cases.len(), 200);
    for (n, case) in fx.cases.iter().enumerate() {
        let i = &case.input;
        let today = parse_iso(&i.today).unwrap();
        let cs: Vec<Contract> = i.contracts.iter().map(contract).collect();
        let obs: Vec<Vec<Obligation>> = i
            .contracts
            .iter()
            .map(|c| c.obligations.iter().map(obligation).collect())
            .collect();
        let mut per = vec![];
        for (k, c) in cs.iter().enumerate() {
            let src = &i.contracts[k];
            let info = renewal_info(c, today, i.soon_days);
            let known: Vec<NaiveDate> = src
                .term_history
                .iter()
                .filter_map(|t| parse_iso(&t.end))
                .collect();
            let patch = term_reconciliation(c, &known, today).map(|p| {
                json!({
                    "additions": p.additions.iter().map(|a| json!({
                        "start": a.start, "end": a.end, "ended": a.ended.as_str(), "note": a.note,
                    })).collect::<Vec<_>>(),
                    "terminate": p.terminate,
                    "clearDecision": p.clear_decision,
                })
            });
            let obl: Vec<Value> = obs[k]
                .iter()
                .map(|o| {
                    let r = o.repeat.as_ref();
                    let h = o.history.len() as i32;
                    json!({
                        "advance": r.map(|_| view(&advance_occurrence(c, o, today))),
                        "rewind": view(&rewind_occurrence(o)),
                        "reopen": view(&reopened_series(c, o, today)),
                        "within": r.map(|r| [0, 1, 2, 3, 5, 8]
                            .iter()
                            .map(|x| within_series(r, *x, Some(c), today))
                            .collect::<Vec<_>>()),
                        "anchor": r.and_then(|r| o.due.map(|due| repeat_anchor(due, &r.every, h, Some(r)))),
                        "freshAnchor": r.and_then(|r| o.due.map(|due| repeat_anchor(due, &r.every, h, None))),
                    })
                })
                .collect();
            let dated: Vec<DatedObligation> = src
                .obligations
                .iter()
                .zip(&obs[k])
                .map(|(s, o)| DatedObligation {
                    id: s.id.clone(),
                    text: s.text.clone(),
                    owner: Owner::parse(&s.owner),
                    schedule: o.clone(),
                })
                .collect();
            let ctx = EventContext {
                id: src.id.clone(),
                title: src.title.clone(),
                counterparty: String::new(),
                updated_at_ms: src.updated_at,
                url: String::new(),
            };
            let opts = EventOptions {
                today,
                terms: 3,
                occurrences: 4,
                alarms: vec![7, 1],
            };
            let events: Vec<Value> = contract_events(c, &ctx, &dated, &opts)
                .iter()
                .map(|e: &Event| {
                    json!({
                        "uid": format!("cm-{}@contract-manager.puter", e.key.join("-")),
                        "date": e.date,
                        "categories": e.categories,
                    })
                })
                .collect();
            per.push(json!({
                "nextTermEnd": next_term_end(c, today),
                "agreementEnd": agreement_end(c, today),
                "completedTerms": completed_terms(c, today),
                "info": info_json(&info),
                "attention": needs_attention(c, &info),
                "patch": patch,
                "obligations": obl,
                "events": events,
            }));
        }
        let pairs: Vec<(&Contract, &[Obligation])> =
            cs.iter().zip(obs.iter().map(Vec::as_slice)).collect();
        let flat = |rows: Vec<(usize, usize, Option<i64>)>| {
            rows.into_iter()
                .map(|(a, b, c)| json!([a, b, c]))
                .collect::<Vec<_>>()
        };
        let stats = contract_stats(&pairs, today);
        let refs: Vec<&Contract> = cs.iter().collect();
        let cal: Vec<Event> = i
            .calendar
            .iter()
            .map(|e| Event {
                key: vec![e.uid.trim_end_matches("@x").to_owned()],
                date: parse_iso(&e.date).unwrap(),
                sequence: e.sequence,
                summary: e.summary.clone(),
                description: e.description.clone(),
                url: e.url.clone(),
                categories: e.categories.clone(),
                alarms: e.alarms.clone(),
            })
            .collect();
        let got = json!({
            "per": per,
            "all": flat(all_obligations(&pairs, today, true)),
            "due": flat(due_obligations(&pairs, today, i.days)),
            "stats": {
                "active": stats.active,
                "expiring60": stats.expiring60,
                "openObligations": stats.open_obligations,
                "annualByCurrency": stats.annual_by_currency,
                "expired": stats.expired,
            },
            "upcoming": upcoming_renewals(&refs, today, i.renew_days),
            "escaped": escape_text(&i.line),
            "folded": fold_line(&format!("DESCRIPTION:{}", escape_text(&i.line))),
            "calendar": build_calendar(&cal, "Contract deadlines, MoUs", "20261017T000000Z"),
        });
        // Our PRODID and UID domain are our own.
        let mut want = case.expected.clone();
        let mut text = want["calendar"].as_str().unwrap().to_owned();
        text = text.replace(
            "PRODID:-//Puter//Contract Manager//EN",
            &format!("PRODID:{PRODID}"),
        );
        for e in &i.calendar {
            let ours = format!("UID:sutra-{}@{UID_HOST}", e.uid.trim_end_matches("@x"));
            text = text.replace(&format!("UID:{}\r\n", e.uid), &format!("{ours}\r\n"));
        }
        want["calendar"] = json!(text);
        // Currency keys: Everything falls back to USD for a broken code, we to INR.
        let mut m = serde_json::Map::new();
        for (k, v) in want["stats"]["annualByCurrency"].as_object().unwrap() {
            m.insert(k.clone(), v.clone());
        }
        want["stats"]["annualByCurrency"] = Value::Object(m);
        assert_eq!(norm(got), norm(want), "case {n}");
    }
}

fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .map_or(Value::Number(n), |f| json!((f * 1e6).round() / 1e6)),
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, norm(x))).collect()),
        x => x,
    }
}
