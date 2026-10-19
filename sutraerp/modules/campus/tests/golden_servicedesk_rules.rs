//! Golden vectors for `servicedesk::rules`, generated from Everything's original
//! `help-desk/src/shared/rules.js` and `runTriggers` (`src/server/tickets.js`) over random
//! tickets by `tools/golden/generate.mjs`, replayed in Everything's vocabulary.

use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use sutra_campus::servicedesk::rules::{
    apply_actions, first_name, in_view, matches, norm_tags, render_placeholders, run_triggers,
    short_duration, sla_status, test_condition, Action, Comment, Conditions, Ctx, Event,
    HashMapLike, Sla, SlaTarget, Ticket, Trigger, Vocab,
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
struct In {
    now: f64,
    me: Option<String>,
    before: Option<Ticket>,
    actor: Option<String>,
    comment: Option<Comment>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SlaIn {
    enabled: bool,
    targets: BTreeMap<String, SlaTarget>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    ticket: Ticket,
    ctx: In,
    conditions: Conditions,
    actions: Vec<Action>,
    triggers: Vec<Trigger>,
    event: String,
    sla: SlaIn,
    durations: Vec<f64>,
    text: String,
    data: Value,
    view: Value,
    tag_input: Value,
}

/// Every number as f64, so `1.0` and `1` compare equal.
fn nums(v: Value) -> Value {
    match v {
        Value::Number(n) => n.as_f64().map_or(Value::Number(n), Value::from),
        Value::Array(a) => Value::Array(a.into_iter().map(nums).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, nums(x))).collect()),
        other => other,
    }
}

/// A ticket as JSON with Everything's absent members dropped, so both sides compare alike.
fn ticket_json(t: &Ticket) -> Value {
    let mut v = serde_json::to_value(t).expect("ticket");
    if let Value::Object(o) = &mut v {
        o.retain(|k, x| {
            !(x.is_null() || (matches!(k.as_str(), "spam" | "deleted") && *x == Value::Bool(false)))
        });
    }
    nums(v)
}

fn norm_expected(v: &Value) -> Value {
    let mut v = v.clone();
    if let Value::Object(o) = &mut v {
        o.retain(|k, x| {
            !(x.is_null() || (matches!(k.as_str(), "spam" | "deleted") && *x == Value::Bool(false)))
        });
        for k in ["tags", "followers"] {
            o.entry(k).or_insert_with(|| Value::Array(vec![]));
        }
        o.entry("fields")
            .or_insert_with(|| Value::Object(Default::default()));
        o.entry("subject")
            .or_insert_with(|| Value::String(String::new()));
    }
    nums(v)
}

fn notes_json(n: &[sutra_campus::servicedesk::rules::Notification]) -> Value {
    serde_json::to_value(n).expect("notes")
}

fn s<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str()
}

fn js_string(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        other => Some(other.to_string()),
    }
}

#[test]
fn servicedesk_rules_match_everything() {
    let raw = include_str!("golden/servicedesk_rules.json");
    let fx: Fixture = serde_json::from_str(raw).expect("fixture");
    assert!(fx.cases.len() >= 150);
    let vocab = Vocab::everything();
    for (ci, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let x = &c.expected;
        let w = |what: &str| format!("case {ci} {what}");
        let ctx = Ctx {
            me: i.ctx.me.as_deref(),
            now: i.ctx.now,
            before: i.ctx.before.as_ref(),
            actor: i.ctx.actor.as_deref(),
            comment: i.ctx.comment,
        };

        let conds: Vec<_> = i.conditions.all.iter().chain(&i.conditions.any).collect();
        let got: Vec<bool> = conds
            .iter()
            .map(|k| test_condition(k, &i.ticket, &ctx, &vocab))
            .collect();
        let want: Vec<bool> = x["tests"]
            .as_array()
            .expect("tests")
            .iter()
            .map(|b| b.as_bool().unwrap_or_default())
            .collect();
        for (k, (g, e)) in got.iter().zip(&want).enumerate() {
            assert_eq!(g, e, "{} condition {:?}", w("test"), conds[k]);
        }
        assert_eq!(got.len(), want.len(), "{}", w("tests len"));
        assert_eq!(
            matches(&i.conditions, &i.ticket, &ctx, &vocab),
            x["matches"].as_bool().unwrap_or_default(),
            "{}",
            w("matches")
        );
        let special = i.view.get("special").and_then(Value::as_str);
        let vconds: Conditions = i
            .view
            .get("conditions")
            .map(|c| serde_json::from_value(c.clone()).expect("view conditions"))
            .unwrap_or_default();
        assert_eq!(
            in_view(special, &vconds, &i.ticket, &ctx, &vocab),
            x["inView"].as_bool().unwrap_or_default(),
            "{}",
            w("inView")
        );

        let (t, notes) = apply_actions(&i.ticket, &i.actions, ctx.me, &vocab);
        assert_eq!(
            ticket_json(&t),
            norm_expected(&x["applied"]["ticket"]),
            "{}",
            w("applied ticket")
        );
        assert_eq!(
            notes_json(&notes),
            x["applied"]["notifications"],
            "{}",
            w("applied notes")
        );

        let event = if i.event == "created" {
            Event::Created
        } else {
            Event::Updated
        };
        // runTriggers reads the clock itself; trigger conditions carry no hour fields.
        let (t, fired, notes) = run_triggers(&i.triggers, &i.ticket, event, &ctx, &vocab);
        assert_eq!(
            ticket_json(&t),
            norm_expected(&x["triggered"]["ticket"]),
            "{}",
            w("triggered ticket")
        );
        assert_eq!(
            serde_json::to_value(&fired).expect("fired"),
            x["triggered"]["fired"],
            "{}",
            w("fired")
        );
        assert_eq!(
            notes_json(&notes),
            x["triggered"]["notifications"],
            "{}",
            w("trigger notes")
        );

        let sla = Sla {
            enabled: i.sla.enabled,
            targets: i.sla.targets.clone(),
        };
        match sla_status(&i.ticket, &sla, i.ctx.now, &vocab) {
            None => assert!(x["sla"].is_null(), "{} {}", w("sla"), x["sla"]),
            Some(st) => {
                assert_eq!(
                    Some(st.metric.as_str()),
                    x["sla"]["metric"].as_str(),
                    "{}",
                    w("sla metric")
                );
                assert_eq!(
                    Some(st.due_at),
                    x["sla"]["dueAt"].as_f64(),
                    "{}",
                    w("sla due")
                );
                assert_eq!(
                    Some(st.remaining),
                    x["sla"]["remaining"].as_f64(),
                    "{}",
                    w("sla remaining")
                );
                assert_eq!(
                    Some(st.breached),
                    x["sla"]["breached"].as_bool(),
                    "{}",
                    w("sla breached")
                );
            }
        }

        let durations: Vec<String> = i.durations.iter().map(|d| short_duration(*d)).collect();
        let want: Vec<&str> = x["durations"]
            .as_array()
            .expect("d")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(durations, want, "{}", w("durations"));

        let d = &i.data;
        let id = d["ticket"]["id"].as_i64().map(|n| n.to_string());
        let req_name = js_string(&d["requester"]["name"]);
        let agent_name = js_string(&d["agent"]["name"]);
        let entries = vec![
            ("ticket.id", id.as_deref()),
            ("ticket.subject", s(d, &["ticket", "subject"])),
            ("ticket.status", s(d, &["ticket", "status"])),
            ("ticket.priority", s(d, &["ticket", "priority"])),
            ("ticket.link", s(d, &["link"])),
            ("requester.name", req_name.as_deref()),
            (
                "requester.first_name",
                Some(first_name(req_name.as_deref().unwrap_or(""))),
            ),
            ("requester.email", s(d, &["requester", "email"])),
            ("agent.name", agent_name.as_deref()),
            (
                "agent.first_name",
                Some(first_name(agent_name.as_deref().unwrap_or(""))),
            ),
            ("desk.name", s(d, &["desk", "name"])),
            ("comment.text", s(d, &["commentText"])),
        ];
        assert_eq!(
            Some(render_placeholders(&i.text, &HashMapLike { entries }).as_str()),
            x["rendered"].as_str(),
            "{}",
            w("rendered")
        );
        let tags = norm_tags(&i.tag_input);
        let want: Vec<&str> = x["tags"]
            .as_array()
            .expect("tags")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        assert_eq!(tags, want, "{}", w("tags"));
    }
}
