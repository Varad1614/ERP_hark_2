//! Golden vectors for `timetrack::logic`, generated from Everything's original
//! `time-tracking/src/lib/{time,util,csv,importer,store}.js` by `tools/golden/generate.mjs`.
//!
//! The store's cascade is replayed through the generic port: entry rate → project rate →
//! client rate → default (time tracking maps these to entry → staff rate → payroll-derived rate
//! → institution default), client rounding / minimum → global rounding.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use serde_json::{json, Value};
use sutra_people::timetrack::logic::{
    add_days, billed_seconds, billing_filter, csv_field, days_apart, detect_mapping,
    duration_patch, elapsed_seconds, entry_amount, fmt_clock, fmt_duration, fmt_timer,
    group_entries, month_days, month_key_of, month_keys_between, ms_at, overlapping, parse_amount,
    parse_csv, parse_duration, parse_import_date, parse_import_duration, rate_at, resolve_rate,
    resolve_rounding, round2, round_seconds, rows_to_items, shift_month, task_key, text_matches,
    time_part_of, to_csv_line, to_hours, totals, week_days, week_start_of, with_rate_from, Mapping,
    RateLevels, RateValue, Tracked,
};

fn norm(v: Value) -> Value {
    match v {
        Value::Number(n) => n
            .as_f64()
            .map_or(Value::Number(n), |f| json!((f * 1e6).round() / 1e6)),
        Value::Array(a) => Value::Array(a.into_iter().map(norm).collect()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, x)| (k, norm(x))).collect()),
        other => other,
    }
}

fn eq(got: Value, want: &Value, what: &str) {
    assert_eq!(norm(got), norm(want.clone()), "{what}");
}

fn s(v: &Value) -> String {
    v.as_str().map(str::to_owned).unwrap_or_default()
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| a.iter().map(s).collect())
        .unwrap_or_default()
}

/// `String(value ?? '')` of a JSON scalar, as the original stringifies a CSV field.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::String(x) => x.clone(),
        Value::Number(n) => n.as_i64().map_or_else(
            || {
                let f = n.as_f64().unwrap_or(0.0);
                if f.fract() == 0.0 {
                    format!("{f:.0}")
                } else {
                    f.to_string()
                }
            },
            |i| i.to_string(),
        ),
        other => other.to_string(),
    }
}

fn rate(v: &Value) -> Option<RateValue> {
    serde_json::from_value(v.clone()).ok()
}

fn opt_f(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn by_id<'a>(list: &'a Value, id: &Value) -> Option<&'a Value> {
    let id = id.as_str()?;
    list.as_array()?.iter().find(|x| x["id"] == id)
}

#[test]
fn time_tracking_matches_everything() {
    let fx: Value =
        serde_json::from_str(include_str!("golden/time_tracking.json")).expect("fixture");
    let cases = fx["cases"].as_array().expect("cases");
    assert!(cases.len() >= 200, "{}", cases.len());
    let empty = vec![];
    for (n, c) in cases.iter().enumerate() {
        let i = &c["input"];
        let x = &c["expected"];
        let at = |k: &str| format!("case {n}: {k}");
        let today = s(&i["today"]);

        eq(
            json!(strs(&i["durations"])
                .iter()
                .map(|d| parse_duration(d))
                .collect::<Vec<_>>()),
            &x["durations"],
            &at("durations"),
        );
        eq(
            json!(strs(&i["importDurations"])
                .iter()
                .map(|d| parse_import_duration(d))
                .collect::<Vec<_>>()),
            &x["importDurations"],
            &at("importDurations"),
        );
        eq(
            json!(strs(&i["importDates"])
                .iter()
                .map(|d| parse_import_date(d))
                .collect::<Vec<_>>()),
            &x["importDates"],
            &at("importDates"),
        );
        let times: Vec<Option<String>> =
            strs(&i["times"]).iter().map(|t| time_part_of(t)).collect();
        eq(json!(times), &x["times"], &at("times"));
        let date = s(&i["date"]);
        let shift = i["shift"].as_i64().unwrap_or(0);
        let plus = i64::from(shift > 20);
        eq(
            json!(times
                .iter()
                .map(|t| ms_at(&date, t.as_deref(), plus))
                .collect::<Vec<_>>()),
            &x["msAt"],
            &at("msAt"),
        );
        eq(
            json!(strs(&i["amounts"])
                .iter()
                .map(|a| parse_amount(a))
                .collect::<Vec<_>>()),
            &x["amounts"],
            &at("amounts"),
        );
        let secs: Vec<i64> = i["seconds"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .filter_map(Value::as_i64)
            .collect();
        eq(
            json!(secs
                .iter()
                .map(|v| round2(*v as f64 / 7.0))
                .collect::<Vec<_>>()),
            &x["round2"],
            &at("round2"),
        );
        let round_to = i["roundTo"].as_i64().unwrap_or(0);
        let sx: Vec<Value> = secs
            .iter()
            .map(|v| {
                let f = *v as f64;
                json!({"clock": fmt_clock(f), "timer": fmt_timer(f), "human": fmt_duration(f),
                    "hours": to_hours(f, 2), "hours4": to_hours(f, 4), "rounded": round_seconds(*v, round_to)})
            })
            .collect();
        eq(json!(sx), &x["seconds"], &at("seconds"));

        let date2 = s(&i["date2"]);
        let month = month_key_of(&date);
        let (a, b) = if date <= date2 {
            (date.clone(), date2.clone())
        } else {
            (date2.clone(), date.clone())
        };
        let mon = week_start_of(&date, 1).unwrap_or_default();
        eq(
            json!({
                "addDays": add_days(&date, shift), "daysApart": days_apart(&date, &date2),
                "weekMon": mon, "weekSun": week_start_of(&date, 0), "weekDays": week_days(&mon),
                "monthDays": month_days(&month).len(), "shiftMonth": shift_month(&month, shift),
                "monthKeysBetween": month_keys_between(&a, &b), "monthKeyOf": month,
            }),
            &x["calendar"],
            &at("calendar"),
        );

        let fields: Vec<String> = i["fields"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(js_string)
            .collect();
        eq(
            json!(fields.iter().map(|f| csv_field(f)).collect::<Vec<_>>()),
            &x["fields"],
            &at("fields"),
        );
        eq(json!(to_csv_line(&fields)), &x["csvLine"], &at("csvLine"));
        let rows: Vec<Vec<String>> = i["csvRows"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(strs)
            .collect();
        let eol = if i["now"].as_i64().unwrap_or(0) % 2 == 1 {
            "\r\n"
        } else {
            "\n"
        };
        let text = format!(
            "{}{}",
            rows.iter()
                .map(|r| to_csv_line(r))
                .collect::<Vec<_>>()
                .join(eol),
            s(&i["csvExtra"])
        );
        eq(json!(text), &x["csvText"], &at("csvText"));
        eq(json!(parse_csv(&text)), &x["parsedCsv"], &at("parsedCsv"));

        let headers = strs(&i["headers"]);
        let (source, mapping): (String, Mapping) = detect_mapping(&headers);
        eq(json!(source), &x["source"], &at("source"));
        eq(json!(mapping), &x["mapping"], &at("mapping"));
        let data: Vec<Vec<String>> = i["rows"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(strs)
            .collect();
        let (items, invalid) = rows_to_items(&data, &mapping);
        eq(
            json!({"items": items, "invalid": invalid}),
            &x["items"],
            &at("items"),
        );

        let rv = rate(&i["rateValue"]);
        eq(
            json!(strs(&i["rateDates"])
                .iter()
                .map(|d| rate_at(rv.as_ref(), d))
                .collect::<Vec<_>>()),
            &x["rateAt"],
            &at("rateAt"),
        );
        let mut cur = rate(&i["rateStart"]);
        let mut hist = Vec::new();
        for op in i["rateOps"].as_array().unwrap_or(&empty) {
            cur = with_rate_from(
                cur.as_ref(),
                opt_f(&op["rate"]),
                op["from"].as_str(),
                &today,
            );
            hist.push(json!(cur));
        }
        eq(json!(hist), &x["withRateFrom"], &at("withRateFrom"));

        // ---- the store's billing maths
        let settings = &i["settings"];
        let default_rate = rate(&settings["defaultRate"]);
        let global_rounding = opt_f(&settings["rounding"]);
        let mut tracked = Vec::new();
        let mut per = Vec::new();
        let entries = i["entries"].as_array().cloned().unwrap_or_default();
        let patch = i["patchSeconds"].as_f64().unwrap_or(0.0);
        for e in &entries {
            let project = by_id(&i["projects"], &e["projectId"]);
            let client = project.and_then(|p| by_id(&i["clients"], &p["clientId"]));
            let (prate, crate_) = (
                project.and_then(|p| rate(&p["rate"])),
                client.and_then(|c| rate(&c["rate"])),
            );
            let levels = RateLevels {
                entry: opt_f(&e["rate"]),
                project: prate.as_ref(),
                client: crate_.as_ref(),
                default: default_rate.as_ref(),
            };
            let r = resolve_rate(&levels, &s(&e["date"]), &today);
            let rounding =
                resolve_rounding(client.and_then(|c| opt_f(&c["rounding"])), global_rounding);
            let minimum = client
                .and_then(|c| c["minimumMinutes"].as_i64())
                .unwrap_or(0);
            let billable = e["billable"].as_bool().unwrap_or(false);
            let seconds = e["seconds"].as_i64().unwrap_or(0);
            let billed = billed_seconds(billable, seconds, rounding, minimum);
            let amount = entry_amount(billable, billed, r);
            let (st, en) = (e["startedAt"].as_i64(), e["endedAt"].as_i64());
            let (ps, pe) = duration_patch(st, en, patch);
            let p = pe.map_or_else(
                || json!({"seconds": ps}),
                |end| json!({"seconds": ps, "endedAt": end}),
            );
            per.push(json!({"rate": r, "rounding": rounding, "minimum": minimum, "billed": billed, "amount": amount, "patch": p}));
            tracked.push(Tracked {
                id: s(&e["id"]),
                seconds,
                billable,
                billed,
                amount,
                started_at: st,
                ended_at: en,
            });
        }
        eq(json!(per), &x["entries"], &at("entries"));
        eq(json!(totals(&tracked)), &x["totals"], &at("totals"));
        let meta = |t: &Tracked| {
            entries
                .iter()
                .find(|e| s(&e["id"]) == t.id)
                .cloned()
                .unwrap_or(Value::Null)
        };
        let groups: Vec<Value> = ["project", "client", "task"]
            .iter()
            .map(|by| {
                json!(group_entries(&tracked, |t| {
                    let e = meta(t);
                    let project = by_id(&i["projects"], &e["projectId"]);
                    let client = project.and_then(|p| by_id(&i["clients"], &p["clientId"]));
                    let name = |v: Option<&Value>, d: &str| {
                        v.and_then(|x| x["name"].as_str())
                            .filter(|x| !x.is_empty())
                            .unwrap_or(d)
                            .to_owned()
                    };
                    let id = |v: Option<&Value>| {
                        v.and_then(|x| x["id"].as_str())
                            .unwrap_or("none")
                            .to_owned()
                    };
                    match *by {
                        "project" => (id(project), name(project, "No project"), name(client, "")),
                        "client" => (id(client), name(client, "No client"), String::new()),
                        _ => {
                            let (k, l) = task_key(&s(&e["task"]));
                            (k, l, String::new())
                        }
                    }
                }))
            })
            .collect();
        eq(json!(groups), &x["groups"], &at("groups"));
        let probe = &i["probe"];
        let hits: Vec<String> = overlapping(
            &tracked,
            probe["startedAt"].as_i64(),
            probe["endedAt"].as_i64(),
            probe["id"].as_str(),
        )
        .into_iter()
        .map(|t| t.id.clone())
        .collect();
        eq(json!(hits), &x["overlaps"], &at("overlaps"));
        let f = &i["filter"];
        let (query, client_id, billing) = (s(&f["query"]), s(&f["clientId"]), s(&f["billing"]));
        let filtered: Vec<String> = entries
            .iter()
            .filter(|e| {
                let project = by_id(&i["projects"], &e["projectId"]);
                if !client_id.is_empty()
                    && project.and_then(|p| p["clientId"].as_str()) != Some(client_id.as_str())
                {
                    return false;
                }
                if !billing_filter(&billing, e["billable"].as_bool().unwrap_or(false)) {
                    return false;
                }
                if query.is_empty() {
                    return true;
                }
                let client = project.and_then(|p| by_id(&i["clients"], &p["clientId"]));
                [
                    Some(&e["task"]),
                    Some(&e["notes"]),
                    project.map(|p| &p["name"]),
                    client.map(|c| &c["name"]),
                ]
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .any(|t| !t.is_empty() && text_matches(t, &query))
            })
            .map(|e| s(&e["id"]))
            .collect();
        eq(json!(filtered), &x["filtered"], &at("filtered"));
        let elapsed = i["timerStart"]
            .as_i64()
            .map_or(0, |st| elapsed_seconds(st, i["now"].as_i64().unwrap_or(0)));
        eq(json!(elapsed), &x["elapsed"], &at("elapsed"));
    }
}
