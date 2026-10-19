//! Batch 7 integration — the cross-slice People flow (7A → 7B → 7C) against PostgreSQL, through
//! the HTTP API only (no seeding helpers for People records):
//!
//! * `hire_leave_payroll_and_separation` — 7A hire (staff record verified → active, engagement
//!   submitted → active, reporting manager); 7B leave policy (CL paid, EL encashable, LWP unpaid)
//!   → active, opening entitlements; the faculty applies one day of unpaid leave through
//!   `/me/leave-requests` (auto-approved → reserved), HR consumes it (`consume:<YYYY-MM>`,
//!   `is_paid = false`); a suspended engagement refuses new leave (7A status → 7B eligibility);
//!   7C configuration → active, salary structure + assignments, the regular run of the leave
//!   month: loss of pay read through `LeaveReader` (1 day), validate → approve → finalise →
//!   post (balanced Finance journal) → payslips (`/me/payslips`).
//!   Separation: a second staff member requests leave after the exit date and an encashment
//!   (7B `encashment` entry); the separation case closes the engagement (open leave after the
//!   last working date is cancelled, its reservation released); the final settlement pays the
//!   encashment (`encashment_payment`, once) and posts to Finance.
//!
//! Dates follow the database's `current_date` (activation and consumption need started dates).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::{Datelike, Duration, NaiveDate, Weekday};
use serde_json::{json, Value};
use support::b7;
use support::*;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b7e00";
const P: &str = "/api/v1/people";

async fn call(
    app: &Router,
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
    app: &Router,
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

async fn fetch(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

fn ver(v: &Value) -> Option<i64> {
    Some(
        v["version"]
            .as_i64()
            .unwrap_or_else(|| panic!("no version in {v}")),
    )
}

fn d(x: NaiveDate) -> String {
    x.format("%Y-%m-%d").to_string()
}

fn weekday(x: NaiveDate) -> bool {
    !matches!(x.weekday(), Weekday::Sat | Weekday::Sun)
}

async fn count(pool: &sqlx::PgPool, sql: &str, arg: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(Uuid::parse_str(arg).unwrap())
        .fetch_one(pool)
        .await
        .unwrap()
}

/// Staff record (verified → active through the API) + engagement (submitted → active).
async fn hire(
    app: &Router,
    admin: &Login,
    inst: &str,
    given: &str,
    subject: Option<&str>,
    start: NaiveDate,
) -> (Value, Value) {
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{P}/institutions/{inst}/staff"),
        json!({"givenName": given, "familyName": "Integration",
               "workEmail": format!("{}.integration@b7i.dev", given.to_lowercase()),
               "staffCategory": "teaching", "subjectRef": subject}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&s);
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{P}/staff/{sid}/submit-verification"),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{P}/staff/{sid}/verify"),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{P}/staff/{sid}/activate"),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "active", "{s}");
    let e = ok(
        app,
        admin,
        "POST",
        &format!("{P}/staff/{sid}/engagements"),
        json!({"employmentType": "permanent", "designation": "Assistant Professor",
               "grade": "AP-1", "startDate": d(start)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let e = ok(
        app,
        admin,
        "POST",
        &format!("{P}/engagements/{}/submit", id(&e)),
        json!({}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "active", "auto-approved and activated: {e}");
    (s, e)
}

#[tokio::test]
async fn hire_leave_payroll_and_separation() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera_login = login(&app, "meera.shah@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Integration University").await;
    let inst = setup.institution().as_uuid().to_string();

    // ---- dates from the database clock.
    let today: NaiveDate = sqlx::query_scalar("SELECT current_date")
        .fetch_one(&pool)
        .await
        .unwrap();
    let mut leave_day = today - Duration::days(1);
    while !weekday(leave_day) {
        leave_day -= Duration::days(1);
    }
    let leave_month_start = leave_day.with_day(1).unwrap();
    let eng_start = (leave_month_start - Duration::days(1)).with_day(1).unwrap();
    let mut future_day = today + Duration::days(7);
    while !weekday(future_day) {
        future_day += Duration::days(1);
    }
    let year = today.year();

    // ---- 7A: hire Meera (linked to the dev faculty login) and her manager Anil.
    let (anil, _anil_eng) = hire(&app, &admin, &inst, "Anil", None, eng_start).await;
    let (meera, meera_eng) = hire(&app, &admin, &inst, "Meera", Some(b4::MEERA), eng_start).await;
    let (meera_id, meera_eid) = (id(&meera), id(&meera_eng));
    let meera_eng = ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/engagements/{meera_eid}/reporting"),
        json!({"reportsToStaffId": id(&anil), "reason": "head of department"}),
        ver(&meera_eng),
        StatusCode::OK,
    )
    .await;
    assert_eq!(meera_eng["reportsToStaffId"], id(&anil).as_str());
    let me = fetch(&app, &meera_login, &format!("{P}/me/profile")).await;
    assert_eq!(me["staff"]["id"], meera_id.as_str(), "{me}");

    // ---- 7B: leave policy → active, opening entitlements.
    let pol = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-policies"),
        json!({"name": "Integration leave policy", "effectiveFrom": format!("{}-01-01", eng_start.year()),
               "weeklyOffDays": [6, 7]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let pid = id(&pol);
    let mut types = std::collections::HashMap::new();
    for t in [
        json!({"code": "CL", "name": "Casual leave", "annualEntitlement": "12.00", "accrualFrequency": "yearly"}),
        json!({"code": "EL", "name": "Earned leave", "annualEntitlement": "15.00", "accrualFrequency": "yearly",
               "encashmentCap": "10.00"}),
        json!({"code": "LWP", "name": "Leave without pay", "accrualFrequency": "none",
               "isPaid": false, "requiresBalance": false}),
    ] {
        let body = ok(
            &app,
            &admin,
            "POST",
            &format!("{P}/leave-policies/{pid}/leave-types"),
            t.clone(),
            None,
            StatusCode::CREATED,
        )
        .await;
        let code = t["code"].as_str().unwrap().to_owned();
        let ty = body["leaveTypes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["code"] == code.as_str())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        types.insert(code, ty);
    }
    let pol = fetch(&app, &admin, &format!("{P}/leave-policies/{pid}")).await;
    let pol = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/submit"),
        json!({}),
        ver(&pol),
        StatusCode::OK,
    )
    .await;
    let pol = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/activate"),
        json!({}),
        ver(&pol),
        StatusCode::OK,
    )
    .await;
    assert_eq!(pol["status"], "active");
    for y in [leave_day.year(), year] {
        let (s, b) = call(
            &app,
            &admin,
            "POST",
            &format!("{P}/institutions/{inst}/leave-accruals"),
            json!({"leaveYear": y}),
            None,
        )
        .await;
        assert!(s.is_success(), "opening entitlements {y}: {b}");
    }

    // ---- faculty applies one day of unpaid leave; HR consumes it.
    let req = ok(
        &app,
        &meera_login,
        "POST",
        &format!("{P}/me/leave-requests"),
        json!({"leaveTypeId": types["LWP"], "startDate": d(leave_day), "endDate": d(leave_day),
               "reason": "family function"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(req["status"], "reserved", "auto-approved: {req}");
    assert_eq!(req["days"], "1.00");
    let req = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-requests/{}/consume", id(&req)),
        json!({}),
        ver(&req),
        StatusCode::OK,
    )
    .await;
    assert_eq!(req["status"], "consumed");
    let (key, paid): (String, bool) = sqlx::query_as(
        "SELECT posting_key, is_paid FROM people.leave_ledger_entry WHERE source_type = 'leave_request' \
         AND source_id = $1 AND kind = 'consumption'",
    )
    .bind(Uuid::parse_str(&id(&req)).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(key, format!("consume:{}", leave_day.format("%Y-%m")));
    assert!(!paid, "unpaid leave posts is_paid = false");

    // ---- 7A status → 7B eligibility: a suspended engagement takes no new leave.
    let e = fetch(&app, &admin, &format!("{P}/engagements/{meera_eid}")).await;
    let e = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{meera_eid}/suspend"),
        json!({"reason": "inquiry"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    let (s, b) = call(
        &app,
        &meera_login,
        "POST",
        &format!("{P}/me/leave-requests"),
        json!({"leaveTypeId": types["CL"], "startDate": d(future_day), "endDate": d(future_day)}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "suspended engagement: {b}");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{meera_eid}/restore"),
        json!({}),
        ver(&e),
        StatusCode::OK,
    )
    .await;

    // ---- 7C: configuration → active, structure, assignments.
    let cfg = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-configs"),
        json!({"name": "Integration payroll", "effectiveFrom": d(eng_start)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let cid = id(&cfg);
    let mut cfg = cfg;
    for c in [
        json!({"code": "BASIC", "name": "Basic pay", "kind": "earning", "basis": "fixed", "sequence": 10}),
        json!({"code": "HRA", "name": "House rent allowance", "kind": "earning", "basis": "percent_of_basic", "rate": "40", "sequence": 20}),
    ] {
        cfg = ok(
            &app,
            &admin,
            "POST",
            &format!("{P}/payroll-configs/{cid}/components"),
            c,
            ver(&cfg),
            StatusCode::OK,
        )
        .await;
    }
    let cfg = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/submit"),
        json!({}),
        ver(&cfg),
        StatusCode::OK,
    )
    .await;
    let cfg = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/activate"),
        json!({}),
        ver(&cfg),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cfg["status"], "active");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/salary-structures"),
        json!({"code": "AP-1", "name": "Assistant Professor 1", "grade": "AP-1",
               "lines": [{"componentCode": "BASIC", "amount": "30000"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let engagements_of = |sid: String| format!("{P}/staff/{sid}/engagements");
    let anil_eid = id(&fetch(&app, &admin, &engagements_of(id(&anil))).await["items"][0]);
    for eid in [&meera_eid, &anil_eid] {
        let a = ok(
            &app,
            &admin,
            "POST",
            &format!("{P}/engagements/{eid}/salary-assignments"),
            json!({"effectiveFrom": d(eng_start)}),
            None,
            StatusCode::CREATED,
        )
        .await;
        assert_eq!(a["structureCode"], "AP-1", "grade-mapped: {a}");
    }

    // ---- regular run of the leave month: LOP from 7B through LeaveReader.
    let period = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-periods"),
        json!({"year": leave_day.year(), "month": leave_day.month()}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let run = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-periods/{}/runs", id(&period)),
        json!({"kind": "regular"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid = id(&run);
    let run = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/calculate"),
        json!({}),
        ver(&run),
        StatusCode::OK,
    )
    .await;
    assert_eq!(run["status"], "calculated", "{run}");
    assert_eq!(run["headcount"], 2, "{run}");
    assert!(run["exceptions"].as_array().unwrap().is_empty(), "{run}");
    let results = fetch(&app, &admin, &format!("{P}/payroll-runs/{rid}/results")).await;
    let items = results["items"].as_array().unwrap();
    let staff_no = |v: &Value| v["staffNumber"].as_str().unwrap().to_owned();
    let mres = items
        .iter()
        .find(|r| staff_no(r) == staff_no(&meera))
        .unwrap()
        .clone();
    let ares = items
        .iter()
        .find(|r| staff_no(r) == staff_no(&anil))
        .unwrap()
        .clone();
    assert_eq!(mres["lopDays"], "1.00", "{mres}");
    assert_eq!(ares["lopDays"], "0.00", "{ares}");
    assert_eq!(
        ares["grossEarnings"], "42000.00",
        "full month BASIC + 40 % HRA: {ares}"
    );
    let mg = b7::dec(mres["grossEarnings"].as_str().unwrap());
    assert!(
        mg < b7::dec("42000.00"),
        "loss of pay reduces gross: {mres}"
    );
    let mut run = run;
    for step in ["validate", "submit", "finalise", "post"] {
        run = ok(
            &app,
            &admin,
            "POST",
            &format!("{P}/payroll-runs/{rid}/{step}"),
            json!({}),
            ver(&run),
            StatusCode::OK,
        )
        .await;
    }
    assert_eq!(run["status"], "posted", "{run}");
    let je = run["journalEntryId"].as_str().unwrap().to_owned();
    let (dr, cr): (rust_decimal::Decimal, rust_decimal::Decimal) = sqlx::query_as(
        "SELECT COALESCE(sum(debit), 0), COALESCE(sum(credit), 0) FROM finance.journal_line \
         WHERE journal_entry_id = $1",
    )
    .bind(Uuid::parse_str(&je).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(dr, cr, "balanced payroll journal");
    assert!(dr > rust_decimal::Decimal::ZERO);
    let issued = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/payslips/issue"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(issued["processed"], 2, "{issued}");
    let slips = fetch(&app, &meera_login, &format!("{P}/me/payslips")).await;
    let slips = slips["items"].as_array().unwrap();
    assert_eq!(slips.len(), 1, "{slips:?}");
    assert!(slips[0]["number"].as_str().unwrap().starts_with("PS-"));
    assert_eq!(slips[0]["lopDays"], "1.00");

    // ---- separation → final settlement (Anil): leave after the exit, encashment.
    let future = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-requests"),
        json!({"staffId": id(&anil), "leaveTypeId": types["CL"], "startDate": d(future_day),
               "endDate": d(future_day)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(future["status"], "reserved", "{future}");
    let enc = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-encashments", id(&anil)),
        json!({"leaveTypeId": types["EL"], "leaveYear": year, "days": "3.00"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(enc["status"], "approved", "{enc}");
    let enc_entry = enc["ledgerEntryId"].as_str().unwrap().to_owned();
    let sep = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anil_eid}/separation"),
        json!({"kind": "resignation", "lastWorkingDate": d(today), "reason": "relocation"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sep = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/separation-cases/{}/clearance", id(&sep)),
        json!({"items": [{"department": "library", "cleared": true}, {"department": "finance", "cleared": true}]}),
        ver(&sep),
        StatusCode::OK,
    )
    .await;
    let sep = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/separation-cases/{}/complete", id(&sep)),
        json!({}),
        ver(&sep),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sep["status"], "completed", "{sep}");
    let e = fetch(&app, &admin, &format!("{P}/engagements/{anil_eid}")).await;
    assert_eq!(e["status"], "closed");
    // Integration: the leave after the last working date was cancelled and released.
    let f = fetch(&app, &admin, &format!("{P}/leave-requests/{}", id(&future))).await;
    assert_eq!(f["status"], "cancelled", "{f}");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM people.leave_ledger_entry WHERE source_id = $1 AND kind = 'release'",
            &id(&future),
        )
        .await,
        1
    );
    let fnf = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anil_eid}/final-settlement"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let fid = id(&fnf);
    let fnf = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/final-settlements/{fid}/calculate"),
        json!({}),
        ver(&fnf),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        fnf["encashmentDays"], "3.00",
        "7B encashment feeds 7C: {fnf}"
    );
    assert_eq!(fnf["encashmentAmount"], "3000.00", "30000 / 30 × 3: {fnf}");
    if leave_month_start == today.with_day(1).unwrap() {
        // The regular run of this month (finalised + posted above) already paid Anil's salary:
        // the settlement must not pay it again.
        assert_eq!(fnf["salaryAmount"], "0.00", "no double pay: {fnf}");
    } else {
        assert_ne!(fnf["salaryAmount"], "0.00", "unpaid final month: {fnf}");
    }
    let fnf = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/final-settlements/{fid}/complete"),
        json!({}),
        ver(&fnf),
        StatusCode::OK,
    )
    .await;
    assert_eq!(fnf["status"], "completed", "{fnf}");
    assert!(fnf["number"].as_str().unwrap().starts_with("FNF-"));
    assert!(fnf["journalEntryNumber"].is_string(), "{fnf}");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM people.encashment_payment WHERE ledger_entry_id = $1",
            &enc_entry,
        )
        .await,
        1,
        "the encashment is paid once"
    );
}
