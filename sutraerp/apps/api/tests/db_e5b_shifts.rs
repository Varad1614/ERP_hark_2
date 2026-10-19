//! ERP_EXTENSIONS 5B — shift swaps and roster timesheets over the HTTP API against PostgreSQL.
//!
//! * `swap_needs_peer_then_an_independent_manager` — a staff member asks a colleague to swap
//!   published shifts, the colleague agrees, neither of them can decide (403), an independent
//!   manager approves and the two shifts exchange holders.
//! * `timesheet_reconciles_punches_and_feeds_payroll` — a late, long day reconciles to late
//!   minutes and overtime, the staff member submits, cannot approve their own timesheet, an
//!   independent manager approves, and the payroll export locks it and writes payroll inputs.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::Datelike;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b7;
use support::*;
use sutra_contracts::people::status::{EngagementStatus, StaffCategory};
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const TENANT_SWAP: &str = "0192f5a0-7c3e-7000-8000-0000000e5b00";
const TENANT_TS: &str = "0192f5a0-7c3e-7000-8000-0000000e5b01";
const P: &str = "/api/v1/people";

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, v)).await;
    (s, b)
}

async fn ok(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, b) = call(app, who, method, path, body, v).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn fetch(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn ver(v: &Value) -> Option<i64> {
    v["version"].as_i64()
}

/// The people router acting as `subject` (an administrator, so it holds `people.self` too).
fn as_subject(st: sutra_people::PeopleState, tenant: &str, subject: &str) -> Router {
    let (router, _) = sutra_people::router_with_state(st).split_for_parts();
    let ctx = AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![Role::Administrator],
        scope: ExecutionScope::tenant(tenant.parse::<TenantId>().unwrap()),
        csrf_token: "x".into(),
        expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        idle_expires_at: chrono::Utc::now() + chrono::Duration::hours(1),
        mfa_level: MfaLevel::Mfa,
        amr: vec!["pwd".into(), "otp".into()],
        step_up_enforced: false,
        privileged_mfa_enforced: false,
    };
    router.layer(axum::middleware::from_fn(
        move |mut req: Request<Body>, next: axum::middleware::Next| {
            let ctx = ctx.clone();
            async move {
                req.extensions_mut().insert(ctx);
                next.run(req).await
            }
        },
    ))
}

fn people_state(pool: &sqlx::PgPool) -> sutra_people::PeopleState {
    sutra_api::app::people_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state()
}

fn me() -> Login {
    Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    }
}

fn shift(date: &str, start: &str, end: &str, staff: Uuid) -> Value {
    json!({"date": date, "start": start, "end": end, "role": "Front desk",
        "location": "Main office", "staffId": staff})
}

#[tokio::test]
async fn swap_needs_peer_then_an_independent_manager() {
    let (app, pool) = app(TENANT_SWAP).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_SWAP, "Swap College").await;
    let inst = setup.institution().as_uuid();
    let active = Some(EngagementStatus::Active);
    let (sa, sb) = ("dev|asha.swap@sutra.dev", "dev|bilal.swap@sutra.dev");
    let asha = b7::seed_staff(
        &pool,
        &setup,
        "Asha",
        Some(sa),
        StaffCategory::NonTeaching,
        active,
    )
    .await;
    let bilal = b7::seed_staff(
        &pool,
        &setup,
        "Bilal",
        Some(sb),
        StaffCategory::NonTeaching,
        active,
    )
    .await;
    let (a, b) = (asha.staff_id.as_uuid(), bilal.staff_id.as_uuid());
    let st = people_state(&pool);
    let asha_app = as_subject(st.clone(), TENANT_SWAP, sa);
    let bilal_app = as_subject(st, TENANT_SWAP, sb);
    let me = me();

    let shifts = format!("{P}/institutions/{inst}/roster/shifts");
    let mine = ok(
        &app,
        &admin,
        "POST",
        &shifts,
        shift("2027-04-05", "09:00", "17:00", a),
        None,
        StatusCode::CREATED,
    )
    .await;
    let theirs = ok(
        &app,
        &admin,
        "POST",
        &shifts,
        shift("2027-04-06", "09:00", "17:00", b),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s1, s2) = (
        mine["shift"]["id"].as_str().unwrap().to_owned(),
        theirs["shift"]["id"].as_str().unwrap().to_owned(),
    );
    let published = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/publish"),
        json!({"from": "2027-04-05", "to": "2027-04-11"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(published["published"].as_i64(), Some(2), "{published}");

    let req = ok(
        &asha_app,
        &me,
        "POST",
        &format!("{P}/me/roster/swaps"),
        json!({"shiftId": s1, "kind": "swap", "toStaffId": b, "otherShiftId": s2,
            "reason": "Family function"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(req["status"], "awaiting_peer", "{req}");
    assert_eq!(req["fromStaffId"], a.to_string());
    let id = req["id"].as_str().unwrap().to_owned();

    // The requester cannot answer their own request.
    let (s, body) = call(
        &asha_app,
        &me,
        "POST",
        &format!("{P}/roster/swaps/{id}/respond"),
        json!({"accept": true}),
        ver(&req),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "not addressed to the requester: {body}"
    );
    let req = ok(
        &bilal_app,
        &me,
        "POST",
        &format!("{P}/roster/swaps/{id}/respond"),
        json!({"accept": true}),
        ver(&req),
        StatusCode::OK,
    )
    .await;
    assert_eq!(req["status"], "awaiting_manager", "{req}");

    // Neither the requester nor the colleague may decide (separation of duties).
    for (who, label) in [(&asha_app, "requester"), (&bilal_app, "colleague")] {
        let (s, body) = call(
            who,
            &me,
            "POST",
            &format!("{P}/roster/swaps/{id}/decision"),
            json!({"decision": "approve"}),
            ver(&req),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{label} cannot decide: {body}");
    }
    let done = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/roster/swaps/{id}/decision"),
        json!({"decision": "approve"}),
        ver(&req),
        StatusCode::OK,
    )
    .await;
    assert_eq!(done["status"], "approved", "{done}");
    assert!(done["decidedAt"].is_string(), "{done}");

    let holder = |sid: String| {
        let pool = pool.clone();
        async move {
            sqlx::query_scalar::<_, Option<Uuid>>(
                "SELECT staff_id FROM people.duty_shift WHERE id = $1",
            )
            .bind(Uuid::parse_str(&sid).unwrap())
            .fetch_one(&pool)
            .await
            .expect("duty shift")
        }
    };
    assert_eq!(holder(s1).await, Some(b), "Asha's shift went to Bilal");
    assert_eq!(holder(s2).await, Some(a), "Bilal's shift went to Asha");
}

#[tokio::test]
async fn timesheet_reconciles_punches_and_feeds_payroll() {
    let (app, pool) = app(TENANT_TS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_TS, "Timesheet College").await;
    let inst = setup.institution().as_uuid();
    let subject = "dev|chitra.ts@sutra.dev";
    let chitra = b7::seed_staff(
        &pool,
        &setup,
        "Chitra",
        Some(subject),
        StaffCategory::NonTeaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let c = chitra.staff_id.as_uuid();
    let staff_app = as_subject(people_state(&pool), TENANT_TS, subject);
    let me = me();

    // Last week (Monday–Sunday), wholly in the past: one 09:00–17:00 shift on the Tuesday.
    let today = chrono::Utc::now().date_naive();
    let monday =
        today - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()) + 7);
    let sunday = monday + chrono::Duration::days(6);
    let tuesday = (monday + chrono::Duration::days(1)).to_string();
    let (from, to) = (monday.to_string(), sunday.to_string());

    let created = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/shifts"),
        shift(&tuesday, "09:00", "17:00", c),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = created["shift"]["id"].as_str().unwrap().to_owned();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/publish"),
        json!({"from": from, "to": to}),
        None,
        StatusCode::OK,
    )
    .await;
    // In 40 minutes late, out at 19:30: 9 h 50 m worked, over the 9-hour daily limit.
    let punch = ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/roster/shifts/{sid}/attendance"),
        json!({"status": "late", "clockIn": "09:40", "clockOut": "19:30"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(punch["lateMinutes"].as_i64().unwrap() > 0, "{punch}");

    let mine = fetch(
        &staff_app,
        &me,
        &format!("{P}/me/roster/timesheet?from={from}&to={to}"),
    )
    .await;
    assert_eq!(mine["lines"].as_array().unwrap().len(), 1, "{mine}");
    assert!(
        mine["totals"]["lateMinutes"].as_i64().unwrap() > 0,
        "{mine}"
    );
    assert!(
        mine["totals"]["overtimeMinutes"].as_f64().unwrap() > 0.0,
        "{mine}"
    );
    assert!(mine["saved"].is_null(), "{mine}");

    let ts = ok(
        &staff_app,
        &me,
        "POST",
        &format!("{P}/me/roster/timesheets"),
        json!({"from": from, "to": to, "note": "Stayed for the audit"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(ts["status"], "submitted", "{ts}");
    assert_eq!(ts["periodKind"], "weekly");
    let id = ts["id"].as_str().unwrap().to_owned();

    let (s, body) = call(
        &staff_app,
        &me,
        "POST",
        &format!("{P}/roster/timesheets/{id}/decision"),
        json!({"decision": "approve"}),
        ver(&ts),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "own timesheet: {body}");
    let ts = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/roster/timesheets/{id}/decision"),
        json!({"decision": "approve"}),
        ver(&ts),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ts["status"], "approved", "{ts}");

    let export = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/payroll-export"),
        json!({"from": from, "to": from}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(export["exported"].as_u64(), Some(1), "{export}");
    assert_eq!(export["locked"].as_u64(), Some(1), "{export}");
    assert!(export["inputs"].as_u64().unwrap() >= 1, "{export}");
    assert_eq!(export["items"][0]["timesheetId"], id.as_str());
    assert!(
        export["items"][0]["quantities"]["overtime_hours"]
            .as_f64()
            .unwrap()
            > 0.0,
        "{export}"
    );

    let after = fetch(&app, &admin, &format!("{P}/roster/timesheets/{id}")).await;
    assert_eq!(after["status"], "locked", "{after}");
    assert!(after["exportedAt"].is_string(), "{after}");
    let rows: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM people.payroll_variable_input WHERE source_id = $1 AND staff_id = $2",
    )
    .bind(Uuid::parse_str(&id).unwrap())
    .bind(c)
    .fetch_one(&pool)
    .await
    .expect("payroll inputs");
    assert!(rows >= 1, "payroll inputs written");

    // A second export hands nothing over again.
    let again = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/roster/payroll-export"),
        json!({"from": from, "to": from}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["exported"].as_u64(), Some(0), "{again}");
}
