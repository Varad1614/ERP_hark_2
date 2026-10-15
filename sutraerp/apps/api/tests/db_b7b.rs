//! Batch 7 slice 7B — leave, attendance & workload (L3/L4 E-006, 030, 034, 044 … 052) against
//! PostgreSQL, through the HTTP API.
//!
//! * `leave_lifecycle_attendance_and_workload` (auto-approval, dev logins): leave policy versions
//!   (E-044: draft → rule validation with findings → approved → active → superseded; leave types,
//!   invalid / duplicate codes), accrual (E-046: opening + monthly, idempotent re-runs), leave
//!   request → decision → reservation (E-045: day counting without weekly offs / holidays, half
//!   days, month segments, overlap / balance / validation refusals, self-service, permissions),
//!   consumption (E-047: one `consume:<YYYY-MM>` entry per month segment, release, `on_leave`
//!   days, sweep, unpaid leave in `LeaveReader::payroll_inputs`), cancellation (E-048: release /
//!   reversal), adjustment (E-049), encashment (E-051: eligible → approved entry, not eligible),
//!   year-end carry forward + expiry (E-050), exceptions (E-052), leave cases (E-034), long leave
//!   (E-006: engagement active ↔ long_leave, availability), attendance days (bulk, grid,
//!   correction, self-service) and faculty workload plans (E-030: Domain D teaching load,
//!   over-allocation exception, supersession).
//! * `await_decision_routes_leave_to_the_reporting_manager` — `ApprovalMode::AwaitDecision`
//!   through `sutra_people::router_with_state`: policy review, the leave request decided by the
//!   reporting manager on `/me/leave-approvals` (404 for anyone else, `managerSubjectRef` on the
//!   submitted event), HR rejection, stale ETag, and pending adjustment / encashment / leave case /
//!   workload decisions.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b7::{self, date, dec};
use support::*;
use sutra_contracts::people::status::{EngagementStatus, StaffCategory};
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_people::shared::approval::ApprovalMode;
use sutra_people::PeopleState;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b7b00";
const TENANT_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b7b01";
const P: &str = "/api/v1/people";

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

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn sid(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
}

async fn count(pool: &sqlx::PgPool, sql: &str, id: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(Uuid::parse_str(id).unwrap())
        .fetch_one(pool)
        .await
        .unwrap()
}

fn balance<'a>(items: &'a Value, code: &str) -> &'a Value {
    items["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["leaveTypeCode"] == code)
        .unwrap_or_else(|| panic!("no {code} balance in {items}"))
}

#[tokio::test]
async fn leave_lifecycle_attendance_and_workload() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Leave University").await;
    let inst = setup.institution().as_uuid();
    let scope = setup.scope();
    let state = sutra_api::app::people_deps_with(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
        // Part 1: explicit dev/test auto-approval opt-in (the subject here is not approval).
        sutra_kernel::approval::ApprovalMode::AutoApprove,
    )
    .into_state();
    let ms = b7::seed_staff(
        &pool,
        &setup,
        "Meera",
        Some(b4::MEERA),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let ravi = b7::seed_staff(
        &pool,
        &setup,
        "Ravi",
        None,
        StaffCategory::NonTeaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let sana = b7::seed_staff(
        &pool,
        &setup,
        "Sana",
        None,
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;

    // ================================================================ E-044 leave policy
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/institutions/{inst}/leave-policies"),
        json!({"name": "x", "effectiveFrom": "2026-01-01"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "faculty do not manage leave policy"
    );
    let (s, pol) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-policies"),
        json!({"name": "Leave Policy 2026", "effectiveFrom": "2026-01-01",
            "weeklyOffDays": [7], "holidays": ["2026-07-31"]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{pol}");
    assert_eq!(pol["status"], "draft");
    assert_eq!(pol["versionNo"], 1);
    let pid = sid(&pol);
    // Rule validation: no leave type → 422 and the findings stay on the draft.
    let (s, body) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/submit"),
        json!({}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (_, pol) = read(&app, &admin, &format!("{P}/leave-policies/{pid}")).await;
    assert!(!pol["validationFindings"].as_array().unwrap().is_empty());
    assert_eq!(pol["status"], "draft");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/submit"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let mut types = std::collections::HashMap::new();
    for t in [
        json!({"code": "CL", "name": "Casual leave", "annualEntitlement": "12.00",
            "accrualFrequency": "yearly", "carryForwardCap": "5.00"}),
        json!({"code": "EL", "name": "Earned leave", "annualEntitlement": "15.00",
            "accrualFrequency": "monthly", "carryForwardCap": "30.00", "encashmentCap": "10.00"}),
        json!({"code": "LOP", "name": "Loss of pay", "accrualFrequency": "none",
            "isPaid": false, "requiresBalance": false}),
    ] {
        let (s, body) = call(
            &app,
            &admin,
            "POST",
            &format!("{P}/leave-policies/{pid}/leave-types"),
            t.clone(),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{body}");
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
    let (cl, el, lop) = (
        types["CL"].clone(),
        types["EL"].clone(),
        types["LOP"].clone(),
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/leave-types"),
        json!({"code": "cl", "name": "bad"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "invalid code");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/leave-types"),
        json!({"code": "CL", "name": "dup", "annualEntitlement": "1.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate code");
    let (_, pol) = read(&app, &admin, &format!("{P}/leave-policies/{pid}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/activate"),
        json!({}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a draft is not activated");
    let (s, pol) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/submit"),
        json!({}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{pol}");
    assert_eq!(pol["status"], "approved", "auto-approved");
    assert!(pol["processInstanceId"].is_string());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/leave-types"),
        json!({"code": "SL", "name": "Sick leave", "annualEntitlement": "6.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "types are frozen after submission");
    let (s, pol) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/activate"),
        json!({}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{pol}");
    assert_eq!(pol["status"], "active");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM platform.outbox WHERE event_type = \
             'sutraerp.people.leave_policy.activated' AND subject = $1::text",
            &pid
        )
        .await,
        1
    );

    // ================================================================ E-046 accrual
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-accruals"),
        json!({"leaveYear": 2026}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(run["processed"], 3, "CL opening for three staff: {run}");
    let (_, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-accruals"),
        json!({"leaveYear": 2026}),
        None,
    )
    .await;
    assert_eq!(run["processed"], 0, "idempotent re-run: {run}");
    assert_eq!(run["skipped"], 3);
    assert_eq!(run["runs"], 2);
    let (_, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-accruals"),
        json!({"leaveYear": 2026, "month": 7}),
        None,
    )
    .await;
    assert_eq!(run["processed"], 3, "EL monthly accrual: {run}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-accruals"),
        json!({"leaveYear": 2026, "month": 13}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, bal) = read(
        &app,
        &admin,
        &format!("{P}/staff/{}/leave-balances?leaveYear=2026", ms.staff_id),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{bal}");
    assert_eq!(balance(&bal, "CL")["available"], "12.00");
    assert_eq!(balance(&bal, "EL")["entitled"], "1.25");
    let (s, mine) = read(
        &app,
        &meera,
        &format!("{P}/me/leave-balances?leaveYear=2026"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);
    let (s, mt) = read(&app, &meera, &format!("{P}/me/leave-types")).await;
    assert_eq!(s, StatusCode::OK, "{mt}");
    assert_eq!(mt["items"].as_array().unwrap().len(), 3);

    // ================================================================ E-045 request → decision
    let (s, req) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/leave-requests"),
        json!({"leaveTypeId": cl, "startDate": "2026-07-30", "endDate": "2026-08-04",
            "halfDayEnd": true, "reason": "family function"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{req}");
    assert_eq!(req["status"], "reserved", "auto-approved and reserved");
    assert_eq!(
        req["days"], "3.50",
        "31 Jul holiday, 2 Aug Sunday, half last day"
    );
    assert!(req["number"].as_str().unwrap().starts_with("LV-"));
    let segs = req["segments"].as_array().unwrap();
    assert_eq!(segs.len(), 2);
    assert_eq!(segs[0]["month"], "2026-07");
    assert_eq!(segs[0]["days"], "1.00");
    assert_eq!(segs[1]["month"], "2026-08");
    assert_eq!(segs[1]["days"], "2.50");
    let cl_req = sid(&req);
    let (_, bal) = read(
        &app,
        &meera,
        &format!("{P}/me/leave-balances?leaveYear=2026"),
    )
    .await;
    assert_eq!(balance(&bal, "CL")["reserved"], "3.50");
    assert_eq!(balance(&bal, "CL")["available"], "8.50");
    for (body, expect, why) in [
        (
            json!({"leaveTypeId": cl, "startDate": "2026-08-03", "endDate": "2026-08-03"}),
            StatusCode::CONFLICT,
            "overlap",
        ),
        (
            json!({"leaveTypeId": cl, "startDate": "2026-09-01", "endDate": "2026-09-12"}),
            StatusCode::CONFLICT,
            "insufficient balance",
        ),
        (
            json!({"leaveTypeId": cl, "startDate": "2026-09-10", "endDate": "2026-09-01"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "end before start",
        ),
        (
            json!({"leaveTypeId": lop, "startDate": "2026-12-31", "endDate": "2027-01-04"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "spans leave years",
        ),
        (
            json!({"leaveTypeId": cl, "startDate": "2026-05-04", "endDate": "2026-05-05"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "before the engagement",
        ),
        (
            json!({"leaveTypeId": Uuid::now_v7(), "startDate": "2026-09-01", "endDate": "2026-09-01"}),
            StatusCode::UNPROCESSABLE_ENTITY,
            "unknown type",
        ),
    ] {
        let (s, b) = call(
            &app,
            &meera,
            "POST",
            &format!("{P}/me/leave-requests"),
            body,
            None,
        )
        .await;
        assert_eq!(s, expect, "{why}: {b}");
    }
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{P}/me/leave-requests"),
        json!({"leaveTypeId": cl, "startDate": "2026-09-01", "endDate": "2026-09-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students have no HR self-service");
    let (s, _) = read(&app, &admin, &format!("{P}/me/leave-requests")).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "the admin login has no staff record"
    );
    let (s, _) = read(
        &app,
        &meera,
        &format!("{P}/institutions/{inst}/leave-requests"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // HR requests unpaid leave on behalf of Ravi; Meera plans unpaid leave in December.
    let (s, lop_req) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-requests"),
        json!({"staffId": ravi.staff_id, "leaveTypeId": lop, "startDate": "2026-09-07",
            "endDate": "2026-09-08"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{lop_req}");
    assert_eq!(lop_req["days"], "2.00");
    assert_eq!(lop_req["isPaid"], false);
    let (s, dec_req) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/leave-requests"),
        json!({"leaveTypeId": lop, "startDate": "2026-12-01", "endDate": "2026-12-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{dec_req}");
    let (s, list) = read(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/leave-requests?limit=2"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert!(list["nextCursor"].is_string());
    let (_, mine) = read(&app, &meera, &format!("{P}/me/leave-requests")).await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);

    // ================================================================ E-047 consumption
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-requests/{}/consume", sid(&dec_req)),
        json!({}),
        Some(ver(&dec_req)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "December leave has not started");
    let (s, req) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-requests/{cl_req}/consume"),
        json!({}),
        Some(ver(&req)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{req}");
    assert_eq!(req["status"], "consumed");
    let entries: Vec<(String, String, rust_decimal::Decimal, chrono::NaiveDate)> = sqlx::query_as(
        "SELECT posting_key, kind, days, effective_on FROM people.leave_ledger_entry \
         WHERE source_type = 'leave_request' AND source_id = $1 ORDER BY created_at, posting_key",
    )
    .bind(Uuid::parse_str(&cl_req).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    let find = |k: &str| entries.iter().find(|e| e.0 == k).unwrap().clone();
    assert_eq!(find("reserve").2, dec("-3.50"));
    assert_eq!(find("consume:2026-07").2, dec("-1.00"));
    assert_eq!(find("consume:2026-07").3, date("2026-07-30"));
    assert_eq!(find("consume:2026-08").2, dec("-2.50"));
    assert_eq!(find("consume:2026-08").3, date("2026-08-01"));
    assert_eq!(find("release").2, dec("3.50"));
    assert_eq!(entries.len(), 4);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM people.attendance_day WHERE leave_request_id = $1 \
             AND status = 'on_leave'",
            &cl_req
        )
        .await,
        4
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-requests/{cl_req}/consume"),
        json!({}),
        Some(ver(&req)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "consumed only once");
    let (_, bal) = read(
        &app,
        &meera,
        &format!("{P}/me/leave-balances?leaveYear=2026"),
    )
    .await;
    assert_eq!(balance(&bal, "CL")["consumed"], "3.50");
    assert_eq!(balance(&bal, "CL")["reserved"], "0.00");
    assert_eq!(balance(&bal, "CL")["available"], "8.50");
    let (s, sweep) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-consumption"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sweep}");
    assert_eq!(sweep["processed"], 1, "Ravi's September leave: {sweep}");
    assert_eq!(sweep["skipped"], 1, "Meera's December leave");
    let inputs = state
        .leave
        .payroll_inputs(
            &scope,
            ravi.staff_id,
            date("2026-09-01"),
            date("2026-09-30"),
        )
        .await
        .unwrap();
    assert_eq!(inputs.unpaid_leave_days, dec("2.00"), "7C loss of pay");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM platform.outbox WHERE event_type = \
             'sutraerp.people.leave_request.consumed' AND subject = $1::text",
            &cl_req
        )
        .await,
        1
    );

    // ================================================================ E-048 cancellation
    let (s, req) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-requests/{cl_req}/cancel"),
        json!({"reason": "returned early to campus"}),
        Some(ver(&req)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{req}");
    assert_eq!(req["status"], "cancelled");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM people.leave_ledger_entry WHERE source_id = $1 AND kind = 'reversal'",
            &cl_req
        )
        .await,
        2,
        "both month consumptions reversed"
    );
    let (_, bal) = read(
        &app,
        &meera,
        &format!("{P}/me/leave-balances?leaveYear=2026"),
    )
    .await;
    assert_eq!(balance(&bal, "CL")["consumed"], "0.00");
    assert_eq!(balance(&bal, "CL")["available"], "12.00");
    let (s, d2) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/leave-requests/{}/cancel", sid(&dec_req)),
        json!({}),
        Some(ver(&dec_req)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d2}");
    assert_eq!(d2["status"], "cancelled");
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM people.leave_ledger_entry WHERE source_id = $1 AND posting_key = 'release'",
            &sid(&dec_req)
        )
        .await,
        1,
        "reservation released"
    );
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/leave-requests/{}/cancel", sid(&lop_req)),
        json!({}),
        Some(ver(&lop_req)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "not my request");

    // ================================================================ E-049 adjustment
    let (s, adj) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-adjustments", ms.staff_id),
        json!({"leaveTypeId": cl, "leaveYear": 2026, "days": "2.00", "reason": "compensatory off"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{adj}");
    assert_eq!(adj["status"], "applied");
    assert!(adj["ledgerEntryId"].is_string());
    for (body, expect) in [
        (
            json!({"leaveTypeId": cl, "leaveYear": 2026, "days": "-50.00", "reason": "x"}),
            StatusCode::CONFLICT,
        ),
        (
            json!({"leaveTypeId": cl, "leaveYear": 2026, "days": "0", "reason": "x"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"leaveTypeId": cl, "leaveYear": 2026, "days": "1.00"}),
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (s, b) = call(
            &app,
            &admin,
            "POST",
            &format!("{P}/staff/{}/leave-adjustments", ms.staff_id),
            body,
            None,
        )
        .await;
        assert_eq!(s, expect, "{b}");
    }
    let (_, adjs) = read(
        &app,
        &admin,
        &format!("{P}/staff/{}/leave-adjustments", ms.staff_id),
    )
    .await;
    assert_eq!(adjs["items"].as_array().unwrap().len(), 1);

    // ================================================================ E-051 encashment
    let (s, enc) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-encashments", ms.staff_id),
        json!({"leaveTypeId": el, "leaveYear": 2026, "days": "1.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{enc}");
    assert_eq!(enc["status"], "approved");
    let enc_entry = enc["ledgerEntryId"].as_str().unwrap().to_owned();
    let (_, ne) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-encashments", ms.staff_id),
        json!({"leaveTypeId": el, "leaveYear": 2026, "days": "5.00"}),
        None,
    )
    .await;
    assert_eq!(ne["status"], "not_eligible");
    assert_eq!(ne["statusReason"], "insufficient_balance");
    let (_, ne) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-encashments", ms.staff_id),
        json!({"leaveTypeId": cl, "leaveYear": 2026, "days": "1.00"}),
        None,
    )
    .await;
    assert_eq!(ne["statusReason"], "not_encashable");
    let inputs = state
        .leave
        .payroll_inputs(&scope, ms.staff_id, date("2026-01-01"), date("2026-12-31"))
        .await
        .unwrap();
    assert_eq!(inputs.encashments.len(), 1, "7C pays the encashment entry");
    assert_eq!(inputs.encashments[0].days, dec("1.00"));
    assert_eq!(
        inputs.encashments[0].entry_id.as_uuid().to_string(),
        enc_entry
    );

    // ================================================================ E-052 exceptions
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-exceptions"),
        json!({"category": "bogus", "description": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, ex) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-exceptions"),
        json!({"leaveRequestId": cl_req, "category": "attendance_mismatch",
            "description": "biometric shows presence on a leave day"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ex}");
    assert!(ex["number"].as_str().unwrap().starts_with("LEX-"));
    assert_eq!(ex["staffId"], ms.staff_id.as_uuid().to_string());
    let exid = sid(&ex);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-exceptions/{exid}/resolve"),
        json!({"resolution": "x"}),
        Some(ver(&ex)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "review first");
    let (s, ex) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-exceptions/{exid}/review"),
        json!({}),
        Some(ver(&ex)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ex}");
    assert_eq!(ex["status"], "under_review");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-exceptions/{exid}/resolve"),
        json!({}),
        Some(ver(&ex)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "resolution required");
    let (s, ex) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-exceptions/{exid}/resolve"),
        json!({"resolution": "leave was cancelled and reversed"}),
        Some(ver(&ex)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ex}");
    assert_eq!(ex["status"], "resolved");
    let (_, exs) = read(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/leave-exceptions?status=resolved"),
    )
    .await;
    assert_eq!(exs["items"].as_array().unwrap().len(), 1);

    // ================================================================ E-034 leave case
    let (s, case) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-cases", sana.staff_id),
        json!({"caseType": "medical", "description": "planned surgery"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{case}");
    assert_eq!(case["status"], "processing", "handed to Leave Management");
    let case_id = sid(&case);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-cases/{case_id}/complete"),
        json!({}),
        Some(ver(&case)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "no leave request linked"
    );
    let (s, sreq) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-requests"),
        json!({"staffId": sana.staff_id, "leaveTypeId": cl, "startDate": "2026-09-14",
            "endDate": "2026-09-15", "reason": "surgery"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sreq}");
    let (s, case) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-cases/{case_id}/complete"),
        json!({"leaveRequestId": sid(&sreq)}),
        Some(ver(&case)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{case}");
    assert_eq!(case["status"], "completed");
    assert_eq!(case["leaveOutcome"], "approved");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-cases/{case_id}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&case)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // ================================================================ E-006 long leave
    let sana_eng = sana.engagement_id.unwrap();
    let (s, ll) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{sana_eng}/long-leave"),
        json!({"plannedStart": "2026-10-01", "plannedEnd": "2026-12-31",
            "reason": "medical recovery", "leaveCaseId": case_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ll}");
    assert_eq!(ll["status"], "planned");
    assert_eq!(ll["available"], true);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{sana_eng}/long-leave"),
        json!({"plannedStart": "2026-10-05", "reason": "again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open long leave");
    let (s, ll) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/long-leaves/{}/start", sid(&ll)),
        json!({}),
        Some(ver(&ll)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ll}");
    assert_eq!(ll["status"], "active");
    assert_eq!(ll["engagementStatus"], "long_leave");
    assert_eq!(ll["available"], false);
    let es: String = sqlx::query_scalar("SELECT status FROM people.staff_engagement WHERE id = $1")
        .bind(sana_eng.as_uuid())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(es, "long_leave");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-requests"),
        json!({"staffId": sana.staff_id, "leaveTypeId": lop, "startDate": "2026-10-20",
            "endDate": "2026-10-20"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no leave requests on long leave");
    let (_, grid) = read(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/attendance?date=2026-10-02"),
    )
    .await;
    let sana_row = grid["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["staffId"] == sana.staff_id.as_uuid().to_string())
        .unwrap()
        .clone();
    assert_eq!(sana_row["available"], false);
    let (s, ll) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/long-leaves/{}/end", sid(&ll)),
        json!({"reason": "fit to resume"}),
        Some(ver(&ll)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ll}");
    assert_eq!(ll["status"], "ended");
    assert_eq!(ll["engagementStatus"], "active");
    let (_, lls) = read(
        &app,
        &admin,
        &format!("{P}/engagements/{sana_eng}/long-leave"),
    )
    .await;
    assert_eq!(lls["items"].as_array().unwrap().len(), 1);

    // ================================================================ attendance days
    let (s, sum) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/attendance/bulk"),
        json!({"workDate": "2026-08-03", "entries": [
            {"staffId": ravi.staff_id, "status": "present"},
            {"staffId": sana.staff_id, "status": "absent", "remark": "no intimation"},
            {"staffId": ms.staff_id, "status": "present"},
            {"staffId": Uuid::now_v7(), "status": "present"}
        ]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(sum["processed"], 2, "{sum}");
    assert_eq!(
        sum["failed"].as_array().unwrap().len(),
        2,
        "on approved leave + unknown staff: {sum}"
    );
    let (_, sum) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/attendance/bulk"),
        json!({"workDate": "2026-08-03", "entries": [{"staffId": ravi.staff_id, "status": "present"}]}),
        None,
    )
    .await;
    assert_eq!(sum["skipped"], 1, "unchanged day");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/attendance/bulk"),
        json!({"workDate": "2099-01-01", "entries": [{"staffId": ravi.staff_id, "status": "present"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "future date");
    let (s, one) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/attendance"),
        json!({"staffId": ravi.staff_id, "workDate": "2026-08-04", "status": "holiday"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{one}");
    let (s, grid) = read(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/attendance?date=2026-08-03"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{grid}");
    assert_eq!(grid["items"].as_array().unwrap().len(), 3);
    let (s, days) = read(
        &app,
        &admin,
        &format!(
            "{P}/staff/{}/attendance?from=2026-08-01&to=2026-08-31",
            sana.staff_id
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{days}");
    let day = days["items"][0].clone();
    assert_eq!(day["status"], "absent");
    let day_id = sid(&day);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/attendance-days/{day_id}/correct"),
        json!({"status": "half_day", "remark": "left after lunch"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/attendance-days/{day_id}/correct"),
        json!({"status": "half_day"}),
        Some(ver(&day)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "remark required");
    let (s, fixed) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/attendance-days/{day_id}/correct"),
        json!({"status": "half_day", "remark": "left after lunch"}),
        Some(ver(&day)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{fixed}");
    assert_eq!(fixed["status"], "half_day");
    assert_eq!(fixed["source"], "correction");
    assert!(
        count(
            &pool,
            "SELECT count(*) FROM people.status_history WHERE aggregate_type = 'attendance_day' \
             AND aggregate_id = $1",
            &day_id
        )
        .await
            >= 2,
        "corrections keep history"
    );
    let inputs = state
        .leave
        .payroll_inputs(
            &scope,
            sana.staff_id,
            date("2026-08-01"),
            date("2026-08-31"),
        )
        .await
        .unwrap();
    assert_eq!(inputs.absent_days, dec("0.50"));
    let (s, my_days) = read(
        &app,
        &meera,
        &format!("{P}/me/attendance?from=2026-07-01&to=2026-08-31"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{my_days}");
    assert_eq!(my_days["items"].as_array().unwrap().len(), 4);

    // ================================================================ E-030 workload
    let offering = b4::seed_offering(&pool, &setup.delivery, "CS101", "active").await;
    b4::seed_faculty(&pool, &setup.delivery, &offering, b4::MEERA, "active").await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/workload-plans", ravi.staff_id),
        json!({"termLabel": "Term 1", "availability": "available", "plannedWeeklyHours": "10.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "non-teaching staff");
    let (s, plan) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/workload-plans", ms.staff_id),
        json!({"termLabel": "Term 1", "availability": "available",
            "plannedWeeklyHours": "10.00", "maxWeeklyHours": "12.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{plan}");
    assert_eq!(plan["status"], "draft");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/workload-plans", ms.staff_id),
        json!({"termLabel": "Term 1 again", "availability": "available", "plannedWeeklyHours": "8.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open plan per period");
    let (s, plan) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/workload-plans/{}/submit", sid(&plan)),
        json!({}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["status"], "active");
    assert_eq!(plan["teachingWeeklyHours"], "4.00");
    assert_eq!(plan["overAllocated"], false);
    let first_plan = sid(&plan);
    let (_, over) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/workload-plans", ms.staff_id),
        json!({"termLabel": "Term 1 revised", "availability": "available",
            "plannedWeeklyHours": "14.00", "maxWeeklyHours": "12.00"}),
        None,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/workload-plans/{}/submit", sid(&over)),
        json!({}),
        Some(ver(&over)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "over-allocation needs an exception"
    );
    let (s, over) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/workload-plans/{}/submit", sid(&over)),
        json!({"exceptionReason": "accreditation visit term"}),
        Some(ver(&over)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{over}");
    assert_eq!(over["status"], "active");
    assert_eq!(over["overAllocated"], true);
    let (_, plans) = read(
        &app,
        &admin,
        &format!("{P}/staff/{}/workload-plans", ms.staff_id),
    )
    .await;
    let first = plans["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == first_plan.as_str())
        .unwrap()
        .clone();
    assert_eq!(first["status"], "superseded");
    let (s, wl) = read(&app, &admin, &format!("{P}/staff/{}/workload", ms.staff_id)).await;
    assert_eq!(s, StatusCode::OK, "{wl}");
    assert_eq!(wl["totalWeeklyHours"], "4.00");
    assert_eq!(wl["items"].as_array().unwrap().len(), 1);
    assert_eq!(wl["activePlan"]["plannedWeeklyHours"], "14.00");
    assert_eq!(wl["overAllocated"], true);
    let (s, mw) = read(&app, &meera, &format!("{P}/me/workload")).await;
    assert_eq!(s, StatusCode::OK, "{mw}");
    assert_eq!(mw["available"], true);

    // ================================================================ E-050 year end
    let (s, ye) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-year-end"),
        json!({"leaveYear": 2026}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ye}");
    let (_, b26) = read(
        &app,
        &admin,
        &format!("{P}/staff/{}/leave-balances?leaveYear=2026", ms.staff_id),
    )
    .await;
    assert_eq!(
        balance(&b26, "CL")["available"],
        "0.00",
        "closed year emptied"
    );
    let (_, b27) = read(
        &app,
        &admin,
        &format!("{P}/staff/{}/leave-balances?leaveYear=2027", ms.staff_id),
    )
    .await;
    assert_eq!(
        balance(&b27, "CL")["available"],
        "5.00",
        "carry forward capped at 5"
    );
    assert_eq!(balance(&b27, "EL")["available"], "0.25");
    let expired: rust_decimal::Decimal = sqlx::query_scalar(
        "SELECT -sum(days) FROM people.leave_ledger_entry WHERE staff_id = $1 AND kind = 'expiry'",
    )
    .bind(ms.staff_id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(expired, dec("9.00"), "14 available − 5 carried");
    let (_, ye2) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-year-end"),
        json!({"leaveYear": 2026}),
        None,
    )
    .await;
    assert_eq!(ye2["runs"], 2);
    assert_eq!(ye2["carriedDays"], ye["carriedDays"], "idempotent");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-year-end"),
        json!({"leaveYear": 2099}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // ================================================================ E-044 supersession
    let (_, v2) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-policies"),
        json!({"name": "Leave Policy 2026 rev", "effectiveFrom": "2026-01-01"}),
        None,
    )
    .await;
    assert_eq!(v2["versionNo"], 2);
    let (_, v2) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{}/leave-types", sid(&v2)),
        json!({"code": "CL", "name": "Casual leave", "annualEntitlement": "10.00"}),
        None,
    )
    .await;
    let (_, v2) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{}/submit", sid(&v2)),
        json!({}),
        Some(ver(&v2)),
    )
    .await;
    let (s, v2) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/leave-policies/{}/activate", sid(&v2)),
        json!({}),
        Some(ver(&v2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v2}");
    assert_eq!(v2["supersedesPolicyId"], pid.as_str());
    let (_, old) = read(&app, &admin, &format!("{P}/leave-policies/{pid}")).await;
    assert_eq!(old["status"], "superseded");
    let (_, list) = read(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/leave-policies?status=active"),
    )
    .await;
    assert_eq!(
        list["items"].as_array().unwrap().len(),
        1,
        "one active policy"
    );

    // Audit evidence of the governed steps.
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE action LIKE 'people.leave_%'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(audits >= 10, "{audits}");
}

// ==================================================================== AwaitDecision

const KAVYA: &str = "dev|kavya.b7b@sutra.test";
const ROHAN: &str = "dev|rohan.b7b@sutra.test";
const ISHA: &str = "dev|isha.b7b@sutra.test";
const ADMIN: &str = "dev|admin@sutra.dev";

fn auth(subject: &str, role: Role, tenant: TenantId) -> AuthContext {
    AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![role],
        scope: ExecutionScope::tenant(tenant),
        csrf_token: "a".repeat(64),
        expires_at: Utc::now(),
        idle_expires_at: Utc::now(),
        mfa_level: MfaLevel::None,
        amr: vec!["pwd".into()],
        step_up_enforced: false,
    }
}

/// The 7B router with an explicit state, as `who`.
fn as_user(st: &PeopleState, who: AuthContext) -> axum::Router {
    let (router, _) = sutra_people::router_with_state(st.clone()).split_for_parts();
    router.layer(axum::Extension(who))
}

async fn hit(
    r: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::now_v7().to_string());
    if let Some(v) = if_match {
        b = b.header("if-match", format!("\"{v}\""));
    }
    let (s, _, v) = send(r, b.body(Body::from(body.to_string())).unwrap()).await;
    (s, v)
}

async fn look(r: &axum::Router, path: &str) -> (StatusCode, Value) {
    let (s, _, v) = send(r, Request::get(path).body(Body::empty()).unwrap()).await;
    (s, v)
}

#[tokio::test]
async fn await_decision_routes_leave_to_the_reporting_manager() {
    let (app, pool) = app(TENANT_AWAIT).await;
    let admin_login = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(
        &app,
        &admin_login,
        &pool,
        TENANT_AWAIT,
        "Approval University",
    )
    .await;
    let t = setup.tenant();
    let inst = setup.institution().as_uuid();
    let mut st = sutra_api::app::people_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let kavya = b7::seed_staff(
        &pool,
        &setup,
        "Kavya",
        Some(KAVYA),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let rohan = b7::seed_staff(
        &pool,
        &setup,
        "Rohan",
        Some(ROHAN),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    b7::seed_staff(
        &pool,
        &setup,
        "Isha",
        Some(ISHA),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    b7::set_manager(&pool, &kavya, &rohan).await;
    let admin = as_user(&st, auth(ADMIN, Role::Administrator, t));
    let kavya_r = as_user(&st, auth(KAVYA, Role::Faculty, t));
    let rohan_r = as_user(&st, auth(ROHAN, Role::Faculty, t));
    let isha_r = as_user(&st, auth(ISHA, Role::Faculty, t));

    // ---- policy review stays pending until decided.
    let (s, pol) = hit(
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-policies"),
        json!({"name": "Policy", "effectiveFrom": "2026-01-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{pol}");
    let pid = sid(&pol);
    let mut ty = std::collections::HashMap::new();
    for body in [
        json!({"code": "CL", "name": "Casual", "annualEntitlement": "12.00"}),
        json!({"code": "EL", "name": "Earned", "annualEntitlement": "12.00", "encashmentCap": "5.00"}),
    ] {
        let (s, p) = hit(
            &admin,
            "POST",
            &format!("{P}/leave-policies/{pid}/leave-types"),
            body.clone(),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{p}");
        let code = body["code"].as_str().unwrap().to_owned();
        let id = p["leaveTypes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|x| x["code"] == code.as_str())
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned();
        ty.insert(code, id);
    }
    let (_, pol) = look(&admin, &format!("{P}/leave-policies/{pid}")).await;
    let (s, pol) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/submit"),
        json!({}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{pol}");
    assert_eq!(pol["status"], "under_review");
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/decision"),
        json!({"decision": "reject"}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "rejection needs a reason"
    );
    let (s, pol) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{pol}");
    assert_eq!(pol["status"], "approved");
    let (s, pol) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-policies/{pid}/activate"),
        json!({}),
        Some(ver(&pol)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{pol}");
    assert_eq!(pol["status"], "active");
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/leave-accruals"),
        json!({"leaveYear": 2026}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);

    // ---- the reference approval: routed to the reporting manager.
    let (s, req) = hit(
        &kavya_r,
        "POST",
        &format!("{P}/me/leave-requests"),
        json!({"leaveTypeId": ty["CL"], "startDate": "2026-09-21", "endDate": "2026-09-22"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{req}");
    assert_eq!(req["status"], "pending_approval");
    assert_eq!(req["approverStaffId"], rohan.staff_id.as_uuid().to_string());
    let rid = sid(&req);
    let manager: Option<String> = sqlx::query_scalar(
        "SELECT envelope->'data'->>'managerSubjectRef' FROM platform.outbox \
         WHERE event_type = 'sutraerp.people.leave_request.submitted' AND subject = $1",
    )
    .bind(&rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(manager.as_deref(), Some(ROHAN));
    let (_, mine) = look(&kavya_r, &format!("{P}/me/leave-approvals")).await;
    assert!(mine["items"].as_array().unwrap().is_empty());
    let (s, _) = hit(
        &isha_r,
        "POST",
        &format!("{P}/me/leave-approvals/{rid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&req)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "not Isha's report");
    let (s, _) = hit(
        &kavya_r,
        "POST",
        &format!("{P}/me/leave-approvals/{rid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&req)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "nobody approves their own leave");
    let (s, queue) = look(&rohan_r, &format!("{P}/me/leave-approvals")).await;
    assert_eq!(s, StatusCode::OK, "{queue}");
    assert_eq!(queue["items"].as_array().unwrap().len(), 1);
    let (s, _) = hit(
        &rohan_r,
        "POST",
        &format!("{P}/me/leave-approvals/{rid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&req) + 7),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale ETag");
    let (s, req) = hit(
        &rohan_r,
        "POST",
        &format!("{P}/me/leave-approvals/{rid}/decision"),
        json!({"decision": "approve", "reason": "enjoy"}),
        Some(ver(&req)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{req}");
    assert_eq!(req["status"], "reserved");
    assert_eq!(req["decidedBy"], ROHAN);
    assert_eq!(
        count(
            &pool,
            "SELECT count(*) FROM people.leave_ledger_entry WHERE source_id = $1 \
             AND kind = 'reservation' AND posting_key = 'reserve'",
            &rid
        )
        .await,
        1
    );
    // HR decides on behalf of any manager.
    let (_, req2) = hit(
        &kavya_r,
        "POST",
        &format!("{P}/me/leave-requests"),
        json!({"leaveTypeId": ty["CL"], "startDate": "2026-09-28", "endDate": "2026-09-28"}),
        None,
    )
    .await;
    let (s, req2) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-requests/{}/decision", sid(&req2)),
        json!({"decision": "reject", "reason": "exam week"}),
        Some(ver(&req2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{req2}");
    assert_eq!(req2["status"], "rejected");
    let (s, _) = hit(
        &rohan_r,
        "POST",
        &format!("{P}/me/leave-approvals/{}/decision", sid(&req2)),
        json!({"decision": "approve"}),
        Some(ver(&req2)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already decided");
    let (s, _) = hit(
        &kavya_r,
        "POST",
        &format!("{P}/leave-requests/{}/decision", sid(&req2)),
        json!({"decision": "approve"}),
        Some(ver(&req2)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "faculty lack people.leave.decide");

    // ---- other governed 7B decisions stay pending until decided.
    let (s, adj) = hit(
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-adjustments", kavya.staff_id),
        json!({"leaveTypeId": ty["CL"], "leaveYear": 2026, "days": "1.00", "reason": "weekend duty"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{adj}");
    assert_eq!(adj["status"], "pending_approval");
    let (s, adj) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-adjustments/{}/decision", sid(&adj)),
        json!({"decision": "approve"}),
        Some(ver(&adj)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{adj}");
    assert_eq!(adj["status"], "applied");
    let (s, enc) = hit(
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-encashments", kavya.staff_id),
        json!({"leaveTypeId": ty["EL"], "leaveYear": 2026, "days": "2.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{enc}");
    assert_eq!(enc["status"], "eligible");
    let (s, enc) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-encashments/{}/decision", sid(&enc)),
        json!({"decision": "reject", "reason": "budget closed"}),
        Some(ver(&enc)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{enc}");
    assert_eq!(enc["status"], "rejected");
    assert!(enc["ledgerEntryId"].is_null());
    let (s, case) = hit(
        &admin,
        "POST",
        &format!("{P}/staff/{}/leave-cases", kavya.staff_id),
        json!({"caseType": "study_leave", "description": "conference", "leaveRequestId": rid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{case}");
    assert_eq!(case["status"], "pending_approval");
    let (s, case) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-cases/{}/decision", sid(&case)),
        json!({"decision": "approve"}),
        Some(ver(&case)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{case}");
    assert_eq!(case["status"], "processing");
    let (s, case) = hit(
        &admin,
        "POST",
        &format!("{P}/leave-cases/{}/complete", sid(&case)),
        json!({}),
        Some(ver(&case)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{case}");
    assert_eq!(case["leaveOutcome"], "approved");
    let (s, plan) = hit(
        &admin,
        "POST",
        &format!("{P}/staff/{}/workload-plans", kavya.staff_id),
        json!({"termLabel": "Term 1", "availability": "partially_available",
            "plannedWeeklyHours": "6.00", "maxWeeklyHours": "16.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{plan}");
    let (s, plan) = hit(
        &admin,
        "POST",
        &format!("{P}/workload-plans/{}/submit", sid(&plan)),
        json!({}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["status"], "submitted");
    let (s, plan) = hit(
        &admin,
        "POST",
        &format!("{P}/workload-plans/{}/decision", sid(&plan)),
        json!({"decision": "approve"}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["status"], "active");
}
