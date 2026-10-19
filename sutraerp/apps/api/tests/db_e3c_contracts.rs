//! ERP_EXTENSIONS 3C — contracts and MoUs over the HTTP API against PostgreSQL: create and
//! validate, renewal phase and attention, decisions, repeating obligations (complete / undo),
//! dashboard, term reconciliation (renewed, expired, closed out by notice), MoU activities and
//! NAAC evidence, iCalendar download.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use chrono::{Duration, NaiveDate};
use serde_json::{json, Value};
use support::b7;
use support::*;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000e3c00";
const R: &str = "/api/v1/resources";

async fn ok(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, None)).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn fetch(app: &axum::Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn day(v: &Value) -> NaiveDate {
    NaiveDate::parse_from_str(v.as_str().unwrap(), "%Y-%m-%d").unwrap()
}

#[tokio::test]
async fn contracts_mous_renewals_obligations_evidence() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Contracts College").await;
    let inst = setup.institution().as_uuid();
    let dash = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/contract-dashboard"),
    )
    .await;
    let today = day(&dash["today"]);
    let d = |n: i64| (today + Duration::days(n)).to_string();

    let s = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/contract-settings"),
    )
    .await;
    assert_eq!(s["soonDays"], 60);
    assert_eq!(s["alarmDays"], json!([7, 1]));
    let base = format!("{R}/institutions/{inst}/contracts");

    // Validation.
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"title": "X", "kind": "rental", "counterpartyName": "Y"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"title": "X", "kind": "amc", "counterpartyName": "Y", "autoRenew": true}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"title": "X", "kind": "amc"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // An AMC renewing in 20 days with 30 days' notice: the window is open.
    let amc = ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"title": "RO plant AMC", "kind": "amc", "counterpartyName": "AquaPure Services",
               "value": "4500.00", "billing": "monthly", "startOn": d(-345), "endOn": d(20),
               "autoRenew": true, "termMonths": 12, "noticeDays": 30}),
        StatusCode::CREATED,
    )
    .await;
    let amc_id = amc["contract"]["id"].as_str().unwrap().to_owned();
    assert_eq!(amc["contract"]["renewal"]["phase"], "notice");
    assert_eq!(amc["contract"]["renewal"]["noticeDate"], d(-10));
    assert_eq!(amc["contract"]["attention"], true);
    assert_eq!(amc["contract"]["annualValue"], 54000.0);
    assert!(amc["contract"]["agreementEnd"].is_null());

    let mou = ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"title": "MoU: Tata Motors internships", "kind": "mou_industry",
               "counterpartyName": "Tata Motors Ltd, Pune", "startOn": d(-200), "endOn": d(500),
               "purposes": ["internship", "placement", "internship"]}),
        StatusCode::CREATED,
    )
    .await;
    let mou_id = mou["contract"]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        mou["contract"]["purposes"],
        json!(["internship", "placement"])
    );
    assert_eq!(mou["contract"]["noticeDays"], 30, "settings default");
    assert_eq!(mou["contract"]["renewal"]["phase"], "ok");

    let att = fetch(&app, &admin, &format!("{base}?attention=true")).await;
    assert_eq!(att.as_array().unwrap().len(), 1, "{att}");
    let mous = fetch(&app, &admin, &format!("{base}?kind=mou")).await;
    assert_eq!(mous[0]["id"], mou_id.as_str());
    let found = fetch(&app, &admin, &format!("{base}?q=aquapure")).await;
    assert_eq!(found.as_array().unwrap().len(), 1);

    // Decisions answer this term end only.
    let c = format!("{R}/contracts/{amc_id}");
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{c}/decision"),
        json!({"kind": "snoozed"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let dec = ok(
        &app,
        &admin,
        "PUT",
        &format!("{c}/decision"),
        json!({"kind": "renewing", "note": "Board approved"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(dec["contract"]["attention"], false);
    assert_eq!(dec["contract"]["renewal"]["decision"]["forTermEnd"], d(20));
    assert_eq!(dec["contract"]["renewal"]["handled"], true);
    let v = dec["contract"]["version"].as_i64().unwrap();
    ok(
        &app,
        &admin,
        "PUT",
        &c,
        json!({"title": "RO plant AMC", "kind": "amc", "counterpartyName": "AquaPure Services",
               "value": "4500.00", "billing": "monthly", "startOn": d(-345), "endOn": d(20),
               "autoRenew": true, "noticeDays": 30, "version": v - 1}),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;

    // A monthly obligation, three times: complete moves it on, undo brings it back.
    let ob = ok(
        &app,
        &admin,
        "POST",
        &format!("{c}/obligations"),
        json!({"text": "Quarterly service visit report", "owner": "them", "dueOn": d(-5),
               "repeat": {"every": "monthly", "count": 3}}),
        StatusCode::CREATED,
    )
    .await;
    let ob_id = ob["id"].as_str().unwrap().to_owned();
    assert_eq!(ob["daysLeft"], -5);
    assert_eq!(ob["repeat"]["anchor"], d(-5));
    let o = format!("{R}/contract-obligations/{ob_id}");
    let done = ok(
        &app,
        &admin,
        "POST",
        &format!("{o}/complete"),
        json!({}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(done["completed"], 1);
    let next = (today - Duration::days(5))
        .checked_add_months(chrono::Months::new(1))
        .unwrap();
    assert_eq!(day(&done["dueOn"]), next);
    assert_eq!(done["done"], false);
    let back = ok(
        &app,
        &admin,
        "POST",
        &format!("{o}/undo"),
        json!({}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(back["dueOn"], d(-5));
    assert_eq!(back["completed"], 0);
    for _ in 0..3 {
        ok(
            &app,
            &admin,
            "POST",
            &format!("{o}/complete"),
            json!({}),
            StatusCode::OK,
        )
        .await;
    }
    let fin = fetch(&app, &admin, &c).await;
    assert_eq!(fin["obligations"][0]["done"], true, "{fin}");
    assert_eq!(fin["obligations"][0]["completed"], 3);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{o}/complete"),
        json!({}),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{c}/obligations"),
        json!({"text": "Renew insurance", "dueOn": d(10)}),
        StatusCode::CREATED,
    )
    .await;

    let dash = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/contract-dashboard"),
    )
    .await;
    assert_eq!(dash["stats"]["active"], 2);
    assert_eq!(dash["stats"]["openObligations"], 1);
    assert_eq!(dash["stats"]["annualByCurrency"]["INR"], 54000.0);
    assert_eq!(dash["dueObligations"][0]["contractTitle"], "RO plant AMC");
    assert_eq!(dash["upcoming"][0]["id"], amc_id.as_str());
    assert!(dash["attention"].as_array().unwrap().is_empty());

    // Reconciliation: a rolled auto-renewal, a lapsed fixed term, and a notice-closed contract.
    let rolled = ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"title": "Bus lease", "kind": "lease", "counterpartyName": "Prasanna Travels",
               "endOn": d(-400), "autoRenew": true, "termMonths": 12}),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(rolled["contract"]["renewal"]["phase"], "ok");
    let lapsed = ok(&app, &admin, "POST", &base,
        json!({"title": "Canteen licence", "kind": "license", "counterpartyName": "Annapurna Caterers",
               "endOn": d(-10)}),
        StatusCode::CREATED).await;
    assert_eq!(lapsed["contract"]["renewal"]["phase"], "expired");
    assert_eq!(lapsed["contract"]["attention"], true);
    let closing = ok(
        &app,
        &admin,
        "POST",
        &base,
        json!({"title": "ERP hosting", "kind": "service", "counterpartyName": "CloudCo",
               "endOn": d(-30), "autoRenew": true, "termMonths": 12}),
        StatusCode::CREATED,
    )
    .await;
    let closing_id = closing["contract"]["id"].as_str().unwrap().to_owned();
    sqlx::query(
        "UPDATE resources.contract SET decision = $2 WHERE id = $1::uuid",
    )
    .bind(&closing_id)
    .bind(json!({"kind": "notice-given", "date": d(-60), "note": "Moved in-house", "forTermEnd": d(-30), "until": null}))
    .execute(&pool)
    .await
    .unwrap();
    let rec = ok(
        &app,
        &admin,
        "POST",
        &format!("{base}/reconcile-terms"),
        json!({}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rec["terminated"], 1, "{rec}");
    assert_eq!(rec["termsRecorded"], 4, "{rec}");
    assert_eq!(rec["contractsUpdated"], 3, "{rec}");
    let again = ok(
        &app,
        &admin,
        "POST",
        &format!("{base}/reconcile-terms"),
        json!({}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["contractsUpdated"], 0, "{again}");
    let closed = fetch(&app, &admin, &format!("{R}/contracts/{closing_id}")).await;
    assert_eq!(closed["contract"]["status"], "terminated");
    assert_eq!(closed["terms"][0]["ended"], "notice-given");
    assert_eq!(closed["terms"][0]["note"], "Moved in-house");
    let rid = rolled["contract"]["id"].as_str().unwrap();
    let r = fetch(&app, &admin, &format!("{R}/contracts/{rid}")).await;
    assert_eq!(r["terms"].as_array().unwrap().len(), 2, "{r}");
    assert_eq!(r["terms"][0]["ended"], "renewed");

    // MoU activities and NAAC evidence.
    let m = format!("{R}/contracts/{mou_id}");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{m}/activities"),
        json!({"heldOn": d(1), "kind": "internship", "title": "Summer internship"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{m}/activities"),
        json!({"heldOn": d(-3), "kind": "visit", "title": "Plant visit"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let act = ok(
        &app,
        &admin,
        "POST",
        &format!("{m}/activities"),
        json!({"heldOn": d(-3), "kind": "internship", "title": "Shop-floor internship, batch 1",
               "students": 12, "staff": 1, "evidenceRef": "DMS/IQAC/2026/041"}),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(act["students"], 12);
    let ev = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/mou-evidence"),
    )
    .await;
    assert_eq!(ev["totals"]["inForce"], 1, "{ev}");
    assert_eq!(ev["totals"]["functional"], 1, "{ev}");
    assert_eq!(ev["totals"]["students"], 12);
    assert_eq!(ev["totals"]["functionalByKind"]["mou_industry"], 1);
    assert_eq!(ev["mous"][0]["functional"], true);
    let (s, _, b) = send(
        &app,
        get(
            &admin,
            &format!("{R}/institutions/{inst}/mou-evidence?year=2025-27"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let aid = act["id"].as_str().unwrap();
    ok(
        &app,
        &admin,
        "DELETE",
        &format!("{R}/contract-activities/{aid}"),
        json!({}),
        StatusCode::NO_CONTENT,
    )
    .await;
    let ev = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/mou-evidence"),
    )
    .await;
    assert_eq!(ev["totals"]["functional"], 0, "{ev}");

    // Calendar downloads.
    let (s, h, _) = send(&app, get(&admin, &format!("{base}/calendar.ics"))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/calendar"));
    let (s, h, _) = send(&app, get(&admin, &format!("{c}/calendar.ics"))).await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-disposition"]
        .to_str()
        .unwrap()
        .contains("ro-plant-amc.ics"));

    // Settings and delete.
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{R}/institutions/{inst}/contract-settings"),
        json!({"soonDays": 400, "obligationDays": 30, "defaultNoticeDays": 30, "alarmDays": [7]}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let s = ok(&app, &admin, "PUT", &format!("{R}/institutions/{inst}/contract-settings"),
        json!({"soonDays": 90, "obligationDays": 15, "defaultNoticeDays": 60, "alarmDays": [1, 14, 1]}),
        StatusCode::OK).await;
    assert_eq!(s["alarmDays"], json!([14, 1]));
    let list = fetch(&app, &admin, &c).await;
    let open = list["obligations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["done"] == false)
        .unwrap();
    let oid = open["id"].as_str().unwrap();
    ok(
        &app,
        &admin,
        "DELETE",
        &format!("{R}/contract-obligations/{oid}"),
        json!({}),
        StatusCode::NO_CONTENT,
    )
    .await;
}
