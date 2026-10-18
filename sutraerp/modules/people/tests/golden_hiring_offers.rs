//! Golden vectors for `hr::offers`, `hr::hiring_analytics` and `hr::careers`, generated from
//! Everything's original `hiring/src/core/{offers,onboard,followups,analytics,pipeline,retention,
//! reminders,jobpage,emails}.js` by `tools/golden/generate.mjs` (spec `hiring-offers`).
//!
//! Mapping: Everything's `signed` is SutraERP's `accepted`; an unknown offer status (`bogus`)
//! behaves like a terminal status everywhere but the transition table, where it is never legal.
//! Everything's `draft → sent` is SutraERP's `draft → pending_approval → approved → sent`.

use chrono::{NaiveDate, NaiveDateTime};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashSet;
use sutra_people::hr::careers::{description_html, escape_html, slug_base};
use sutra_people::hr::hiring_analytics::{
    average_days_in_stage, compute_reminders, days_in_stage, funnel_counts, median, offer_outcomes,
    reached_stages, retention_due, source_breakdown, stage_conversion, stage_keys_from,
    stage_visits, stale_candidates, time_to_hire, HistoryEvent, PipelineCandidate, ReminderPrefs,
};
use sutra_people::hr::offers::{
    build_plan, can_transition, due_followups, due_onboarding_tasks, expiring_offers, followup_due,
    group_tasks, hire_start_date, is_open, js_integer, js_number, normalize_templates,
    offset_label, offset_short, progress, render_offer_letter, render_template, sort_followups,
    task_due_date, template_for_job, validate_followup, validate_offer, Checklist, ChecklistTask,
    Followup, MergeVars, OfferRecord, OfferStatus, Template, TemplateTask,
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

fn ts(v: &Value) -> Option<NaiveDateTime> {
    let s = v.as_str()?;
    chrono::DateTime::parse_from_rfc3339(s)
        .ok()
        .map(|d| d.naive_utc())
}
fn date(v: &Value) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(v.as_str()?, "%Y-%m-%d").ok()
}
fn s(v: &Value) -> Option<String> {
    v.as_str().map(str::to_owned)
}
fn iso(t: NaiveDateTime) -> String {
    t.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}
fn ymd(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .map_or(Value::Number(n), |f| json!((f * 1e9).round() / 1e9)),
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, norm(x))).collect()),
        other => other,
    }
}

fn eq(got: Value, want: &Value, what: &str, n: usize) {
    assert_eq!(norm(got), norm(want.clone()), "case {n}: {what}");
}

fn status(v: &Value) -> OfferStatus {
    match v.as_str().unwrap_or("") {
        "draft" => OfferStatus::Draft,
        "sent" => OfferStatus::Sent,
        "signed" => OfferStatus::Accepted,
        "declined" => OfferStatus::Declined,
        _ => OfferStatus::Withdrawn,
    }
}
fn parse_status(v: &str) -> Option<OfferStatus> {
    match v {
        "draft" => Some(OfferStatus::Draft),
        "sent" => Some(OfferStatus::Sent),
        "signed" => Some(OfferStatus::Accepted),
        "declined" => Some(OfferStatus::Declined),
        "withdrawn" => Some(OfferStatus::Withdrawn),
        _ => None,
    }
}

fn offers(v: &Value) -> Vec<OfferRecord> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|o| OfferRecord {
            id: s(&o["id"]).unwrap(),
            candidate_id: s(&o["candidateId"]).unwrap(),
            status: status(&o["status"]),
            expires: date(&o["expires"]),
            joining_date: date(&o["startDate"]),
            sent_at: ts(&o["sentAt"]),
            accepted_at: ts(&o["signedAt"]),
            resolved_at: ts(&o["resolvedAt"]),
            created_at: ts(&o["createdAt"]),
        })
        .collect()
}

fn candidates(v: &Value) -> Vec<PipelineCandidate> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|c| PipelineCandidate {
            id: s(&c["id"]).unwrap(),
            name: s(&c["name"]).unwrap(),
            job_id: s(&c["jobId"]),
            stage: s(&c["stage"]).unwrap(),
            status: s(&c["status"]).unwrap(),
            created_at: ts(&c["createdAt"]).unwrap(),
            stage_changed_at: ts(&c["stageChangedAt"]),
            source: s(&c["source"]),
            pool: c["pool"].as_bool().unwrap_or(false),
            history: c["history"]
                .as_array()
                .unwrap()
                .iter()
                .map(|e| HistoryEvent {
                    kind: s(&e["type"]).unwrap(),
                    stage: s(&e["stage"]),
                    from: s(&e["from"]),
                    to: s(&e["to"]),
                    at: ts(&e["at"]).unwrap(),
                })
                .collect(),
        })
        .collect()
}

fn followups(v: &Value) -> Vec<Followup> {
    let mut out = Vec::new();
    for c in v.as_array().unwrap() {
        for f in c["followups"].as_array().unwrap() {
            out.push(Followup {
                id: s(&f["id"]).unwrap(),
                candidate_id: s(&c["id"]).unwrap(),
                text: s(&f["text"]).unwrap_or_default(),
                due_date: s(&f["dueDate"]),
                due_time: s(&f["dueTime"]),
                done: f["done"].as_bool().unwrap_or(false),
                completed_at: ts(&f["completedAt"]),
            });
        }
    }
    out
}

fn int_offset(v: &Value) -> Option<i64> {
    // `Number.isInteger(value)` on the raw value (no coercion).
    v.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64)
}

fn plans(v: &Value) -> Vec<Checklist> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| Checklist {
            candidate_id: s(&p["candidateId"]).unwrap(),
            tasks: p["tasks"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| ChecklistTask {
                    id: s(&t["id"]).unwrap(),
                    title: s(&t["title"]).unwrap(),
                    category: s(&t["category"]).unwrap_or_default(),
                    offset_days: int_offset(&t["offsetDays"]),
                    done: t["done"].as_bool().unwrap_or(false),
                })
                .collect(),
        })
        .collect()
}

/// Everything's shipped default (the fixture's fallback when no template is stored).
fn everything_default() -> Template {
    let t = |title: &str, cat: &str, off: Option<i64>| TemplateTask {
        title: title.into(),
        category: cat.into(),
        offset_days: off,
        owner: None,
    };
    Template {
        id: "default".into(),
        name: "Standard onboarding".into(),
        tasks: vec![
            t("Signed offer letter on file", "Paperwork", None),
            t("Employment contract and tax forms", "Paperwork", None),
            t("Background and reference checks", "Paperwork", None),
            t(
                "Create accounts: email, chat, and tools",
                "IT & access",
                Some(-3),
            ),
            t("Order laptop and equipment", "IT & access", Some(-7)),
            t("Add to payroll and benefits", "People ops", None),
            t("Schedule first-day orientation", "People ops", Some(-2)),
            t("Assign an onboarding buddy", "Team", Some(-1)),
            t("Share the 30/60/90-day plan", "Team", Some(7)),
        ],
        job_ids: vec![],
        departments: vec![],
    }
}

fn opt_num(v: Option<f64>) -> Value {
    v.map_or(Value::Null, |f| json!(f))
}

#[test]
fn hiring_offers_match_everything() {
    let fx: Fixture =
        serde_json::from_str(include_str!("golden/hiring_offers.json")).expect("fixture");
    assert!(fx.cases.len() >= 200);
    for (n, c) in fx.cases.iter().enumerate() {
        let i = &c.input;
        let e = &c.expected;
        let today = date(&i["today"]).unwrap();
        let now = NaiveDateTime::parse_from_str(
            &format!(
                "{}T{}:00",
                i["today"].as_str().unwrap(),
                i["nowTime"].as_str().unwrap()
            ),
            "%Y-%m-%dT%H:%M:%S",
        )
        .unwrap();
        let offers = offers(&i["offers"]);
        let cands = candidates(&i["candidates"]);
        let fus = followups(&i["candidates"]);
        let plans = plans(&i["onboarding"]);
        let stages = stage_keys_from(&i["stages"]);

        // offers
        let tr: Vec<Value> = i["pairs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let (f, t) = (p[0].as_str().unwrap(), p[1].as_str().unwrap());
                let ok = match (parse_status(f), parse_status(t)) {
                    (Some(OfferStatus::Draft), Some(OfferStatus::Sent)) => {
                        // Through approval in SutraERP.
                        !can_transition(OfferStatus::Draft, OfferStatus::Sent)
                            && can_transition(OfferStatus::Draft, OfferStatus::PendingApproval)
                            && can_transition(OfferStatus::PendingApproval, OfferStatus::Approved)
                            && can_transition(OfferStatus::Approved, OfferStatus::Sent)
                    }
                    (Some(a), Some(b)) => can_transition(a, b),
                    _ => false,
                };
                json!(ok)
            })
            .collect();
        eq(json!(tr), &e["transitions"], "transitions", n);
        let open: Vec<bool> = i["offers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| parse_status(o["status"].as_str().unwrap()).is_some_and(is_open))
            .collect();
        eq(json!(open), &e["open"], "open", n);
        let val: Vec<Vec<&str>> = i["terms"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                let bonus = match &t["bonus"] {
                    Value::Null => None,
                    Value::String(x) if x.is_empty() => None,
                    b => Some(js_number(Some(b))),
                };
                let start = t["startDate"].as_str().unwrap_or("").to_owned();
                let exp = t["expires"].as_str().filter(|x| !x.is_empty());
                validate_offer(js_number(Some(&t["salary"])), bonus, &start, exp)
                    .into_iter()
                    .map(|p| p.code)
                    .collect()
            })
            .collect();
        eq(json!(val), &e["validation"], "validation", n);
        let within = i["withinDays"].as_i64().unwrap();
        let exp: Vec<&str> = expiring_offers(&offers, within, now)
            .into_iter()
            .map(|o| o.id.as_str())
            .collect();
        eq(json!(exp), &e["expiring"], "expiring", n);
        let vars: MergeVars = i["vars"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), s(v)))
            .collect();
        let tpl = i["template"].as_str().unwrap();
        eq(
            json!(render_offer_letter(tpl, &vars)),
            &e["letter"],
            "letter",
            n,
        );
        eq(
            json!(render_template(tpl, &vars)),
            &e["rendered"],
            "rendered",
            n,
        );

        // onboarding
        let stored = normalize_templates(&i["templates"]);
        eq(
            serde_json::to_value(&stored).unwrap(),
            &e["templates"],
            "templates",
            n,
        );
        let fallback = everything_default();
        let job = &i["job"];
        let chosen = template_for_job(
            &stored,
            job["id"].as_str(),
            job["department"].as_str(),
            &fallback,
        );
        eq(json!(chosen.id), &e["templateId"], "templateId", n);
        let mut k = 0;
        let plan = build_plan(chosen, || {
            let id = format!("task_{k}");
            k += 1;
            id
        });
        eq(serde_json::to_value(&plan).unwrap(), &e["plan"], "plan", n);
        let start = i["offers"]
            .as_array()
            .unwrap()
            .iter()
            .find_map(|o| date(&o["startDate"]));
        let due: Vec<Value> = plan
            .iter()
            .map(|t| task_due_date(t.offset_days, start).map_or(Value::Null, |d| json!(ymd(d))))
            .collect();
        eq(json!(due), &e["planDue"], "planDue", n);
        let offs: Vec<Option<i64>> = i["offsets"]
            .as_array()
            .unwrap()
            .iter()
            .map(int_offset)
            .collect();
        eq(
            json!(offs
                .iter()
                .map(|o| offset_label(*o).replace("joining date", "start date"))
                .collect::<Vec<_>>()),
            &e["offsetLabels"],
            "offsetLabels",
            n,
        );
        eq(
            json!(offs
                .iter()
                .map(|o| offset_short(*o).replace("Joining date", "Start date"))
                .collect::<Vec<_>>()),
            &e["offsetShort"],
            "offsetShort",
            n,
        );
        let dt: Vec<Value> = due_onboarding_tasks(&plans, &offers, today, within)
            .into_iter()
            .map(|d| json!({"candidateId": d.candidate_id, "taskId": d.task.id, "dueDate": ymd(d.due_date), "overdue": d.overdue, "daysLeft": d.days_left}))
            .collect();
        eq(json!(dt), &e["dueTasks"], "dueTasks", n);
        let pr: Vec<Value> = plans
            .iter()
            .map(|p| serde_json::to_value(progress(p.tasks.iter().map(|t| t.done))).unwrap())
            .collect();
        eq(json!(pr), &e["progress"], "progress", n);
        let gr: Vec<Value> = plans
            .iter()
            .map(|p| {
                json!(group_tasks(&p.tasks, |t| t.category.as_str())
                    .into_iter()
                    .map(|(k, ts)| json!([k, ts.iter().map(|t| t.id.clone()).collect::<Vec<_>>()]))
                    .collect::<Vec<_>>())
            })
            .collect();
        eq(json!(gr), &e["groups"], "groups", n);
        let hs: Vec<Value> = cands
            .iter()
            .map(|c| hire_start_date(&offers, &c.id).map_or(Value::Null, |d| json!(ymd(d))))
            .collect();
        eq(json!(hs), &e["hireStart"], "hireStart", n);

        // follow-ups
        let fd: Vec<Value> = fus
            .iter()
            .map(|f| followup_due(f, now).map_or(Value::Null, |o| json!({ "overdue": o })))
            .collect();
        eq(json!(fd), &e["followupDue"], "followupDue", n);
        let dfu: Vec<Value> = due_followups(&fus, now)
            .into_iter()
            .map(|(f, o)| json!({"candidateId": f.candidate_id, "followupId": f.id, "overdue": o}))
            .collect();
        eq(json!(dfu), &e["dueFollowups"], "dueFollowups", n);
        let sf: Vec<Value> = cands
            .iter()
            .map(|c| {
                let mine: Vec<Followup> = fus
                    .iter()
                    .filter(|f| f.candidate_id == c.id)
                    .cloned()
                    .collect();
                json!(sort_followups(&mine)
                    .into_iter()
                    .map(|f| f.id.clone())
                    .collect::<Vec<_>>())
            })
            .collect();
        eq(json!(sf), &e["sortedFollowups"], "sortedFollowups", n);
        let fv: Vec<Vec<&str>> = i["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c["followups"].as_array().unwrap().iter())
            .map(|f| {
                validate_followup(
                    f["text"].as_str().unwrap_or(""),
                    f["dueDate"].as_str().unwrap_or(""),
                    f["dueTime"].as_str(),
                )
            })
            .collect();
        eq(json!(fv), &e["followupValidation"], "followupValidation", n);

        // analytics
        let vis: Vec<Value> = cands
            .iter()
            .map(|c| {
                json!(stage_visits(c, now)
                    .into_iter()
                    .map(|v| json!({"stage": v.stage, "days": v.days, "open": v.to.is_none()}))
                    .collect::<Vec<_>>())
            })
            .collect();
        eq(json!(vis), &e["visits"], "visits", n);
        let rs: Vec<Vec<String>> = cands
            .iter()
            .map(|c| reached_stages(c).into_iter().collect())
            .collect();
        eq(json!(rs), &e["reached"], "reached", n);
        eq(
            serde_json::to_value(stage_conversion(&cands, &stages)).unwrap(),
            &e["conversion"],
            "conversion",
            n,
        );
        let since = ts(&i["since"]);
        eq(
            serde_json::to_value(time_to_hire(&cands, since)).unwrap(),
            &e["timeToHire"],
            "timeToHire",
            n,
        );
        eq(
            serde_json::to_value(average_days_in_stage(&cands, now, &stages)).unwrap(),
            &e["averageDays"],
            "averageDays",
            n,
        );
        eq(
            serde_json::to_value(source_breakdown(&cands)).unwrap(),
            &e["sources"],
            "sources",
            n,
        );
        let scoped: Option<HashSet<String>> = i["scoped"]
            .as_array()
            .map(|a| a.iter().filter_map(s).collect());
        let oo = offer_outcomes(&offers, scoped.as_ref(), since);
        eq(
            json!({"sent": oo.sent, "signed": oo.accepted, "declined": oo.declined, "acceptance": opt_num(oo.acceptance)}),
            &e["outcomes"],
            "outcomes",
            n,
        );
        let mo: Vec<f64> = i["medianOf"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        eq(opt_num(median(&mo)), &e["median"], "median", n);
        eq(json!(stages), &e["stageKeys"], "stageKeys", n);
        let funnel: Map<String, Value> = funnel_counts(&cands, job["id"].as_str(), &stages)
            .into_iter()
            .map(|(k, v)| (k, json!(v)))
            .collect();
        eq(Value::Object(funnel), &e["funnel"], "funnel", n);
        let dis: Vec<i64> = cands.iter().map(|c| days_in_stage(c, now)).collect();
        eq(json!(dis), &e["daysInStage"], "daysInStage", n);
        let stale: Vec<&str> = stale_candidates(&cands, i["threshold"].as_i64().unwrap(), now)
            .into_iter()
            .map(|c| c.id.as_str())
            .collect();
        eq(json!(stale), &e["stale"], "stale", n);
        let ret: Vec<Value> = retention_due(&cands, i["retentionMonths"].as_i64().unwrap(), now)
            .into_iter()
            .map(|(c, at)| json!({"id": c.id, "archivedAt": iso(at)}))
            .collect();
        eq(json!(ret), &e["retention"], "retention", n);
        let rem: Vec<Value> =
            compute_reminders(&cands, &offers, &fus, &plans, ReminderPrefs::default(), now)
                .into_iter()
                .map(|r| json!({"key": r.key, "kind": r.kind}))
                .collect();
        eq(json!(rem), &e["reminders"], "reminders", n);

        // job page
        let desc = i["desc"].as_str().unwrap();
        eq(json!(escape_html(desc)), &e["escaped"], "escaped", n);
        eq(
            json!(description_html(desc)),
            &e["descriptionHtml"],
            "descriptionHtml",
            n,
        );
        eq(
            json!(slug_base(i["company"].as_str().unwrap_or(""))),
            &e["slug"],
            "slug",
            n,
        );
        let _ = js_integer(None);
    }
}
