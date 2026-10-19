//! ERP_EXTENSIONS 5C — staff time tracking over the HTTP API against PostgreSQL: a SERB grant and
//! an internal committee, manual entries (duration parsing, start / end, overlap refused), the
//! timer, weekly timesheets (submit locks the week, the PI approves, a PI never approves their own
//! week, an administrator decides on behalf), staff hourly rates, the effort report (% effort,
//! person-months, cost), CSV export and a Toggl import that a second run does not duplicate.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use support::b7;
use support::*;
use sutra_contracts::people::status::{EngagementStatus, StaffCategory};

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000e5c00";
const P: &str = "/api/v1/people";
const DEAN: &str = "dev|dean@sutra.dev";

async fn call(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    (s, b)
}

async fn ok(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, b) = call(app, who, method, path, body, if_match).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn fetch(app: &axum::Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn num(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .unwrap_or(f64::NAN)
}

#[tokio::test]
async fn time_tracking_entries_timesheets_and_effort() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Time College").await;
    let inst = setup.institution().as_uuid();
    let active = Some(EngagementStatus::Active);
    let m = b7::seed_staff(
        &pool,
        &setup,
        "Meera",
        Some(b4::MEERA),
        StaffCategory::Teaching,
        active,
    )
    .await;
    let d = b7::seed_staff(
        &pool,
        &setup,
        "Dean",
        Some(DEAN),
        StaffCategory::Teaching,
        active,
    )
    .await;
    let (mid, did) = (m.staff_id.as_uuid(), d.staff_id.as_uuid());

    // Permissions before data: faculty cannot run the set-up.
    let (s, _) = call(&app, &meera, "PUT", &format!("{P}/institutions/{inst}/time-settings"),
        json!({"weekStart": 1, "roundingMinutes": 0, "standardMonthlyHours": 176, "defaultHourlyRate": null, "timerCapHours": 12}), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(
        &app,
        get(&meera, &format!("{P}/institutions/{inst}/time-projects")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let settings = ok(&app, &admin, "PUT", &format!("{P}/institutions/{inst}/time-settings"),
        json!({"weekStart": 1, "roundingMinutes": 0, "standardMonthlyHours": 176, "defaultHourlyRate": 300, "timerCapHours": 10}), None, StatusCode::OK).await;
    assert_eq!(settings["timerCapHours"], 10.0, "{settings}");
    let (s, _) = call(&app, &admin, "PUT", &format!("{P}/institutions/{inst}/time-settings"),
        json!({"weekStart": 1, "roundingMinutes": 7, "standardMonthlyHours": 176, "defaultHourlyRate": null, "timerCapHours": 12}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "rounding must be one of the offered steps"
    );

    // A sponsored project needs its agency and period.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/time-projects"),
        json!({"code": "serb-x", "name": "No agency", "kind": "sponsored_research"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let serb = ok(&app, &admin, "POST", &format!("{P}/institutions/{inst}/time-projects"),
        json!({"code": "serb-crg/2026/014", "name": "Monsoon rainfall nowcasting", "kind": "sponsored_research",
            "fundingAgency": "SERB", "scheme": "Core Research Grant", "sanctionNumber": "CRG/2026/000014",
            "sanctionedAmount": "3200000.00", "manpowerBudget": "1200000.00", "startDate": "2026-01-01", "endDate": "2030-12-31",
            "piStaffId": did, "costCentre": "RES-SERB-14",
            "members": [{"staffId": mid, "role": "jrf", "commitmentPercent": 100}],
            "budgetKind": "hours", "budgetPeriod": "total", "budgetValue": "2000"}), None, StatusCode::CREATED).await;
    let serb_id = serb["id"].as_str().unwrap().to_owned();
    assert_eq!(serb["code"], "SERB-CRG/2026/014");
    assert_eq!(
        serb["members"].as_array().unwrap().len(),
        2,
        "PI joins as a member: {serb}"
    );
    assert_eq!(serb["piName"], "Dean Seeded", "{serb}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/time-projects"),
        json!({"code": "SERB-CRG/2026/014", "name": "Again", "kind": "internal_project"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "codes are unique per institution");
    // An IQAC committee anyone may log on; Meera chairs it (PI) and its time is not chargeable.
    let iqac = ok(&app, &admin, "POST", &format!("{P}/institutions/{inst}/time-projects"),
        json!({"code": "IQAC", "name": "IQAC committee", "kind": "committee", "openToAll": true, "billableDefault": false, "piStaffId": mid}),
        None, StatusCode::CREATED).await;
    let iqac_id = iqac["id"].as_str().unwrap().to_owned();

    // ---- Meera's week
    let week = "2026-09-07"; // a Monday
    let mine = fetch(&app, &meera, &format!("{P}/me/time?week=2026-09-09")).await;
    assert_eq!(mine["weekStart"], week, "{mine}");
    assert_eq!(mine["projects"].as_array().unwrap().len(), 2, "{mine}");
    let e1 = ok(&app, &meera, "POST", &format!("{P}/me/time/entries"),
        json!({"projectId": serb_id, "date": "2026-09-07", "start": "09:00", "end": "10:30", "task": "Radar data cleaning"}), None, StatusCode::CREATED).await;
    assert_eq!(e1["entry"]["seconds"], 5400, "{e1}");
    assert_eq!(e1["entry"]["start"], "09:00");
    assert_eq!(e1["entry"]["billable"], true);
    ok(&app, &meera, "POST", &format!("{P}/me/time/entries"),
        json!({"projectId": serb_id, "date": "2026-09-08", "duration": "1h30", "task": "Model runs"}), None, StatusCode::CREATED).await;
    let (s, b) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/entries"),
        json!({"projectId": iqac_id, "date": "2026-09-07", "start": "10:00", "end": "11:00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "overlaps 09:00–10:30: {b}");
    let c1 = ok(&app, &meera, "POST", &format!("{P}/me/time/entries"),
        json!({"projectId": iqac_id, "date": "2026-09-07", "start": "10:30", "end": "11:30", "task": "SSR criterion 3"}), None, StatusCode::CREATED).await;
    assert_eq!(
        c1["entry"]["billable"], false,
        "committee time follows the project default"
    );
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/entries"),
        json!({"projectId": serb_id, "date": "2025-12-31", "duration": "1"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "before the sanctioned period"
    );
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/entries"),
        json!({"projectId": serb_id, "date": "2026-09-09", "duration": "1:99"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unreadable duration");

    // ---- timer
    ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/timer"),
        json!({"projectId": serb_id, "task": "Field visit"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/timer"),
        json!({"projectId": serb_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one timer at a time");
    let stopped = ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/timer/stop"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        stopped["entry"]["seconds"], 60,
        "a tap-tap still records a minute: {stopped}"
    );
    assert_eq!(stopped["entry"]["source"], "timer");
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/timer/stop"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // ---- submit, lock, approve
    let sheet = ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/timesheets"),
        json!({"projectId": serb_id, "week": "2026-09-10"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(sheet["status"], "submitted");
    assert_eq!(sheet["totalSeconds"], 10800, "{sheet}");
    let sid = sheet["id"].as_str().unwrap().to_owned();
    let entry_id = e1["entry"]["id"].as_str().unwrap();
    let (s, _) = call(
        &app,
        &meera,
        "PUT",
        &format!("{P}/me/time/entries/{entry_id}"),
        json!({"projectId": serb_id, "date": "2026-09-07", "duration": "2"}),
        Some(1),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a submitted week is locked");
    let queue = fetch(&app, &dean, &format!("{P}/me/time-approvals")).await;
    assert_eq!(queue["items"].as_array().unwrap().len(), 1, "{queue}");
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/timesheets/{sid}/decision"),
        json!({"decision": "approve"}),
        Some(1),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "staff never decide their own week"
    );
    let (s, _) = call(
        &app,
        &dean,
        "POST",
        &format!("{P}/timesheets/{sid}/decision"),
        json!({"decision": "reject"}),
        Some(1),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejection needs a reason"
    );
    let approved = ok(
        &app,
        &dean,
        "POST",
        &format!("{P}/timesheets/{sid}/decision"),
        json!({"decision": "approve"}),
        Some(1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(approved["status"], "approved");
    assert_eq!(approved["decidedBy"], DEAN);
    let detail = fetch(&app, &meera, &format!("{P}/timesheets/{sid}")).await;
    assert_eq!(detail["entries"].as_array().unwrap().len(), 2, "{detail}");
    assert!(detail["entries"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["locked"] == true));

    // ---- separation of duties: Meera chairs IQAC, yet cannot approve her own IQAC week
    let own = ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/timesheets"),
        json!({"projectId": iqac_id, "week": week}),
        None,
        StatusCode::OK,
    )
    .await;
    let own_id = own["id"].as_str().unwrap().to_owned();
    let (s, b) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/timesheets/{own_id}/decision"),
        json!({"decision": "approve"}),
        Some(1),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    let list = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/timesheets?status=submitted"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1, "{list}");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/timesheets/{own_id}/decision"),
        json!({"decision": "reject", "reason": "Split SSR drafting by criterion"}),
        Some(1),
        StatusCode::OK,
    )
    .await;
    let reopened = fetch(&app, &meera, &format!("{P}/me/time?week={week}")).await;
    let iqac_row = reopened["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["code"] == "IQAC")
        .unwrap()
        .clone();
    assert_eq!(iqac_row["timesheet"]["status"], "rejected", "{reopened}");

    // ---- rates and the effort report
    let rate = ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/staff/{mid}/time-rate"),
        json!({"rate": 500, "from": null}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(rate["source"], "staff", "{rate}");
    assert_eq!(rate["effectiveToday"], 500.0);
    let rates = fetch(&app, &admin, &format!("{P}/institutions/{inst}/time-rates")).await;
    let dean_rate = rates["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["staffId"] == did.to_string())
        .unwrap()
        .clone();
    assert_eq!(
        dean_rate["source"], "default",
        "no payroll yet: {dean_rate}"
    );
    let report = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/effort-report?from=2026-09&to=2026-09"),
    )
    .await;
    let row = report["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["projectId"] == serb_id)
        .unwrap()
        .clone();
    assert_eq!(row["seconds"], 10800, "{report}");
    assert!(
        (num(&row["percent"]) - 75.0).abs() < 1e-9,
        "3 of 4 hours: {row}"
    );
    assert!((num(&row["personMonths"]) - 0.75).abs() < 1e-9);
    assert!(
        (num(&row["cost"]) - 1500.0).abs() < 1e-9,
        "3 h × ₹500: {row}"
    );
    let proj = report["projects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["projectId"] == serb_id)
        .unwrap()
        .clone();
    assert!(
        (num(&proj["utilisationPercent"]) - 0.13).abs() < 1e-9,
        "1500 / 12 lakh: {proj}"
    );
    let approved_only = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/effort-report?from=2026-09&to=2026-09&approvedOnly=true"),
    )
    .await;
    assert!(
        approved_only["rows"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["projectId"] == serb_id),
        "{approved_only}"
    );
    let (s, h, _) = send(
        &app,
        get(
            &admin,
            &format!("{P}/institutions/{inst}/time-entries/csv?from=2026-09-01&to=2026-09-30"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/csv"));

    // ---- import a Toggl export (dry run, then for real, then again)
    let csv = "User,Email,Client,Project,Description,Billable,Start date,Start time,End date,End time,Duration,Tags\r\n\
        Meera,m@x.in,SERB,serb-crg/2026/014,Literature review,Yes,2026-09-14,09:00:00,2026-09-14,11:00:00,02:00:00,\r\n\
        Meera,m@x.in,,IQAC committee,Minutes,No,2026-09-15,,2026-09-15,,00:45:00,\r\n\
        Meera,m@x.in,,Unknown grant,X,Yes,2026-09-15,,2026-09-15,,01:00:00,\r\n\
        Meera,m@x.in,,IQAC,Bad,No,2026-02-30,,,,01:00:00,\r\n";
    let dry = ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/import"),
        json!({"csv": csv}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(dry["source"], "Toggl", "{dry}");
    assert_eq!(dry["added"], 2, "{dry}");
    assert_eq!(dry["invalid"].as_array().unwrap().len(), 2, "{dry}");
    let done = ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/import"),
        json!({"csv": csv, "dryRun": false}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(done["added"], 2, "{done}");
    let again = ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/import"),
        json!({"csv": csv, "dryRun": false}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["added"], 0, "{again}");
    assert_eq!(again["skipped"], 2);
    let w2 = fetch(&app, &meera, &format!("{P}/me/time?week=2026-09-14")).await;
    assert_eq!(w2["weekSeconds"], 9900, "2 h + 45 m: {w2}");

    // ---- project budget progress and update
    let p = fetch(&app, &admin, &format!("{P}/time-projects/{serb_id}")).await;
    assert_eq!(p["budgetProgress"]["kind"], "hours", "{p}");
    let v = p["version"].as_i64().unwrap();
    let closed = ok(&app, &admin, "PUT", &format!("{P}/time-projects/{serb_id}"),
        json!({"code": "SERB-CRG/2026/014", "name": "Monsoon rainfall nowcasting", "kind": "sponsored_research", "fundingAgency": "SERB",
            "startDate": "2026-01-01", "endDate": "2030-12-31", "piStaffId": did, "members": [{"staffId": mid, "role": "jrf"}], "status": "closed"}),
        Some(v), StatusCode::OK).await;
    assert_eq!(closed["status"], "closed");
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/time/entries"),
        json!({"projectId": serb_id, "date": "2026-09-21", "duration": "1"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "no time on a closed project"
    );
}
