//! Cross-slice setup helpers (integration-owned). Satisfies the **2B** and **2C** required
//! activation-readiness items of an institution through the real HTTP API:
//! an active campus, a published academic structure (higher-education model, so the school
//! model is `not_applicable`), a published programme with a published curriculum version, and a
//! published calendar version with an open academic year and an effective institution-level
//! assignment. Used by `db_b2a.rs` (activation-success tests) and `db_b2_integration.rs`.

use super::{send, write, Login};
use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};

const API: &str = "/api/v1/institution";

async fn ok(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, v)).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

fn id(b: &Value) -> String {
    b["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {b}"))
        .to_owned()
}

fn ver(b: &Value) -> Option<i64> {
    b["version"].as_i64()
}

/// Registers + activates a campus (main or satellite). Returns the campus body.
pub async fn active_campus(
    app: &Router,
    who: &Login,
    inst: &str,
    code: &str,
    is_main: bool,
) -> Value {
    let c = ok(
        app,
        who,
        "POST",
        &format!("{API}/institutions/{inst}/campuses"),
        json!({
            "code": code, "name": format!("{code} Campus"), "campusType": if is_main { "main" } else { "satellite" }, "isMain": is_main,
            "timezone": "Asia/Kolkata", "establishedOn": "1999-06-01", "declaredCapacity": 2000,
            "contactEmail": "campus@sutra.dev",
            "address": {"line1": "1 University Road", "city": "Pune", "state": "Maharashtra", "postalCode": "411007", "country": "IN"}
        }),
        None,
        StatusCode::CREATED,
    )
    .await;
    let c = ok(
        app,
        who,
        "POST",
        &format!("{API}/campuses/{}/activate", id(&c)),
        json!({}),
        ver(&c),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "active", "{c}");
    c
}

/// Published structure → published programme → curriculum with a published first version.
/// Returns (programme, curriculum version).
pub async fn published_curriculum(app: &Router, who: &Login, inst: &str) -> (Value, Value) {
    let s = ok(
        app,
        who,
        "POST",
        &format!("{API}/institutions/{inst}/academic-structures"),
        json!({
            "name": "University structure", "model": "higher_education", "nepAligned": true,
            "effectiveFrom": "2026-06-01",
            "nodes": [
                {"kind": "level", "key": "ug", "name": "Undergraduate", "nheqfLevel": 5},
                {"kind": "degree_type", "key": "btech", "name": "B.Tech", "parentKey": "ug", "nheqfLevel": 6},
            ]
        }),
        None,
        StatusCode::CREATED,
    )
    .await;
    let s = ok(
        app,
        who,
        "POST",
        &format!("{API}/academic-structures/{}/publish", id(&s)),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "published", "{s}");
    let p = ok(
        app,
        who,
        "POST",
        &format!("{API}/institutions/{inst}/programmes"),
        json!({"code": "BTECH-CSE", "name": "B.Tech CSE", "level": "ug", "degreeType": "btech", "durationTerms": 4}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p = ok(
        app,
        who,
        "POST",
        &format!("{API}/programmes/{}/publish", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "published", "{p}");
    let mut components = vec![];
    for t in 1..=4 {
        components.push(json!({"termNo": t, "slotCode": format!("CS{t}01"), "title": "Core", "category": "major_core", "credits": 12.0, "lectureHours": 3, "tutorialHours": 1}));
        components.push(json!({"termNo": t, "slotCode": format!("MD{t}01"), "title": "Multidisciplinary", "category": "multidisciplinary", "credits": 8.0, "lectureHours": 3, "tutorialHours": 1}));
    }
    let c = ok(
        app,
        who,
        "POST",
        &format!("{API}/programmes/{}/curricula", id(&p)),
        json!({"code": "CSE-2026", "title": "B.Tech CSE 2026", "initialVersion": {
            "effectiveFrom": "2026-07-01", "appliesFromCohort": "2026-27", "termStructure": "semester",
            "termCount": 4, "creditFramework": "nep2020", "minTotalCredits": 80, "minCreditsPerTerm": 16,
            "maxCreditsPerTerm": 28, "nepAligned": true, "nheqfLevel": 6, "multipleEntryExit": false,
            "abcEnabled": true, "exitAwards": [], "components": components
        }}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let vid = c["openVersionId"]
        .as_str()
        .unwrap_or_else(|| panic!("{c}"))
        .to_owned();
    let (_, _, v1) = send(
        app,
        super::get(who, &format!("{API}/curriculum-versions/{vid}")),
    )
    .await;
    let v1 = ok(
        app,
        who,
        "POST",
        &format!("{API}/curriculum-versions/{vid}/publish"),
        json!({}),
        ver(&v1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v1["status"], "published", "{v1}");
    (p, v1)
}

/// Calendar with a published version (two terms), its year opened, and an effective
/// institution-level assignment. Returns (calendar id, version body).
pub async fn published_calendar(app: &Router, who: &Login, inst: &str) -> (String, Value) {
    let cal = ok(
        app,
        who,
        "POST",
        &format!("{API}/institutions/{inst}/calendars"),
        json!({"code": super::code("CAL"), "name": "Main calendar"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let cal = id(&cal);
    let year = ok(
        app,
        who,
        "POST",
        &format!("{API}/institutions/{inst}/academic-years"),
        json!({"calendarId": cal, "label": "2026-27", "startsOn": "2026-06-01", "endsOn": "2027-05-31"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let mut v = ok(
        app,
        who,
        "POST",
        &format!("{API}/calendars/{cal}/versions"),
        json!({"academicYearId": id(&year)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let vid = id(&v);
    for (code, seq, s, e) in [
        ("T1", 1, "2026-06-15", "2026-11-30"),
        ("T2", 2, "2026-12-01", "2027-04-30"),
    ] {
        v = ok(
            app,
            who,
            "POST",
            &format!("{API}/calendar-versions/{vid}/terms"),
            json!({"code": code, "name": format!("Term {seq}"), "sequence": seq, "startsOn": s, "endsOn": e}),
            ver(&v),
            StatusCode::CREATED,
        )
        .await;
    }
    let v = ok(
        app,
        who,
        "POST",
        &format!("{API}/calendar-versions/{vid}/publish"),
        json!({}),
        ver(&v),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v["status"], "published", "{v}");
    let y = ok(
        app,
        who,
        "POST",
        &format!("{API}/academic-years/{}/open", id(&year)),
        json!({}),
        ver(&year),
        StatusCode::OK,
    )
    .await;
    assert_eq!(y["status"], "open", "{y}");
    let a = ok(
        app,
        who,
        "POST",
        &format!("{API}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": vid, "scopeKind": "institution", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let a = ok(
        app,
        who,
        "POST",
        &format!("{API}/calendar-assignments/{}/activate", id(&a)),
        json!({}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "effective", "{a}");
    (cal, v)
}

/// Everything 2B and 2C require for activation readiness.
pub async fn satisfy_2b_2c(app: &Router, who: &Login, inst: &str) {
    active_campus(app, who, inst, &super::code("MAIN"), true).await;
    published_curriculum(app, who, inst).await;
    published_calendar(app, who, inst).await;
}
