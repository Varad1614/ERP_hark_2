//! ERP_EXTENSIONS 3A — duty rosters over the HTTP API against PostgreSQL: settings and
//! requirements, roster profiles, shifts (overlap refused, short rest warned), suggestions,
//! fill (preview + apply), coverage gaps, weekly hours, move, publish, delete.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use support::b7;
use support::*;
use sutra_contracts::people::status::{EngagementStatus, StaffCategory};

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000e3a00";
const P: &str = "/api/v1/people";

async fn ok(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn fetch(app: &axum::Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

#[tokio::test]
async fn duty_roster_shifts_rules_fill_and_publish() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Roster College").await;
    let inst = setup.institution().as_uuid();
    let active = Some(EngagementStatus::Active);
    let asha = b7::seed_staff(
        &pool,
        &setup,
        "Asha",
        None,
        StaffCategory::NonTeaching,
        active,
    )
    .await;
    let bilal = b7::seed_staff(
        &pool,
        &setup,
        "Bilal",
        None,
        StaffCategory::NonTeaching,
        active,
    )
    .await;
    let chitra = b7::seed_staff(
        &pool,
        &setup,
        "Chitra",
        None,
        StaffCategory::NonTeaching,
        active,
    )
    .await;
    let (a, b, c) = (
        asha.staff_id.as_uuid(),
        bilal.staff_id.as_uuid(),
        chitra.staff_id.as_uuid(),
    );

    let settings = ok(&app, &admin, "PUT", &format!("{P}/institutions/{inst}/roster/settings"),
        json!({"rules": {"minRestHours": 11, "maxDailyHours": 10},
            "requirements": [{"role": "Security", "days": [0,1,2,3,4,5,6], "count": 1, "start": "22:00", "end": "06:00"}]}),
        None, StatusCode::OK).await;
    assert_eq!(
        settings["requirements"].as_array().unwrap().len(),
        1,
        "{settings}"
    );
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "PUT",
            &format!("{P}/institutions/{inst}/roster/settings"),
            json!({"rules": {"minRestHours": 30}, "requirements": []}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/staff/{c}/roster-profile"),
        json!({"rosterable": false}),
        None,
        StatusCode::OK,
    )
    .await;
    ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/staff/{b}/roster-profile"),
        json!({"role": "Security", "targetHours": 20, "maxHours": 48}),
        None,
        StatusCode::OK,
    )
    .await;

    let shift = |date: &str, start: &str, end: &str, staff: Option<uuid::Uuid>| json!({"date": date, "start": start, "end": end, "role": "Security", "location": "Main gate", "staffId": staff});
    let night = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/shifts"),
        shift("2027-03-01", "22:00", "06:00", Some(b)),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(night["shift"]["overnight"], true, "{night}");
    assert_eq!(night["shift"]["netMinutes"].as_f64(), Some(480.0));
    assert!(night["warnings"].as_array().unwrap().is_empty(), "{night}");
    let (s, _, body) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{P}/institutions/{inst}/roster/shifts"),
            shift("2027-03-02", "05:00", "09:00", Some(b)),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "overlap across midnight: {body}");
    let day = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/shifts"),
        shift("2027-03-02", "10:00", "14:00", Some(b)),
        None,
        StatusCode::CREATED,
    )
    .await;
    let warnings = day["warnings"].as_array().unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap().contains("rest")),
        "short rest is a warning: {day}"
    );
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{P}/institutions/{inst}/roster/shifts"),
            shift("2027-03-02", "10:00", "10:00", None),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let open = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/shifts"),
        shift("2027-03-02", "22:00", "06:00", None),
        None,
        StatusCode::CREATED,
    )
    .await;
    let open_id = open["shift"]["id"].as_str().unwrap().to_owned();

    let sug = fetch(
        &app,
        &admin,
        &format!("{P}/roster/shifts/{open_id}/suggestions"),
    )
    .await;
    let items = sug["items"].as_array().unwrap();
    assert_eq!(
        items[0]["employeeId"],
        a.to_string(),
        "clean candidate first: {sug}"
    );
    assert!(
        items.iter().all(|i| i["employeeId"] != c.to_string()),
        "not rosterable"
    );

    let plan = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/fill"),
        json!({"from": "2027-03-01", "to": "2027-03-07"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            plan["total"].as_i64(),
            plan["filling"].as_i64(),
            plan["applied"].as_i64()
        ),
        (Some(1), Some(1), Some(0)),
        "{plan}"
    );
    let plan = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/fill"),
        json!({"from": "2027-03-01", "to": "2027-03-07", "apply": true}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(plan["applied"].as_i64(), Some(1), "{plan}");

    let board = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/roster?from=2027-03-01&to=2027-03-07"),
    )
    .await;
    assert_eq!(board["openShifts"].as_i64(), Some(0), "{board}");
    assert_eq!(board["shifts"].as_array().unwrap().len(), 3);
    assert_eq!(
        board["gaps"].as_array().unwrap().len(),
        4,
        "nights covered 1–3 March only: {board}"
    );
    let row = board["staff"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["staffId"] == b.to_string())
        .unwrap();
    assert_eq!(row["weekMinutes"].as_f64(), Some(720.0), "{row}");
    assert_eq!(row["hoursStatus"], "under");

    // Move Bilal's day shift to Asha: she works that night, no overlap.
    let moved = ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/roster/shifts/{}", day["shift"]["id"].as_str().unwrap()),
        shift("2027-03-02", "10:00", "14:00", Some(a)),
        day["shift"]["version"].as_i64(),
        StatusCode::OK,
    )
    .await;
    assert_eq!(moved["shift"]["staffId"], a.to_string(), "{moved}");

    let published = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/publish"),
        json!({"from": "2027-03-01", "to": "2027-03-07"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(published["published"].as_i64(), Some(3));
    let board = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/roster?from=2027-03-01&to=2027-03-07"),
    )
    .await;
    let first = &board["shifts"][0];
    assert_eq!(first["status"], "published");
    ok(
        &app,
        &admin,
        "DELETE",
        &format!("{P}/roster/shifts/{}", first["id"].as_str().unwrap()),
        json!({}),
        first["version"].as_i64(),
        StatusCode::NO_CONTENT,
    )
    .await;
}
