//! Batch 7 slice 7C — payroll (L3-E-053 … 057, 074) against PostgreSQL.
//!
//! * `payroll_lifecycle_over_http` — configuration versions (draft → review → approved →
//!   active → superseded, 422 validation, 428 / 412, 409 edits of a non-draft), salary
//!   structures + grade-mapped assignments (overlap 409), periods (duplicate 409), the final
//!   settlement of a closed engagement (pro-rata salary + leave encashment − recovery, `FNF-`
//!   number, Finance posting), the regular run (no duplicate run, 403 for faculty, LOP from
//!   seeded unpaid leave + absence, encashment paid once, settled engagement not paid twice,
//!   missing salary assignment → failed → reprocessing → calculated, approval, finalisation,
//!   immutable results, idempotent balanced Finance posting), payslips (`PS-`, bulk idempotent,
//!   `/me/payslips`), a corrective adjustment (superseding revision, signed posting, reissue) and
//!   a retro run (arrears lines only).
//! * `await_decision_keeps_payroll_steps_pending` — `ApprovalMode::AwaitDecision`: configuration
//!   review rejected back to draft, an off-cycle run submitted (pending + event), rejected,
//!   resubmitted and approved, then finalised; posting it into a closed financial period is
//!   refused (409 Conflict) and leaves the run finalised.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b7::{self, date, dec};
use support::*;
use sutra_contracts::people::status::{
    AttendanceDayStatus, EngagementStatus, LeaveEntryKind, PayrollConfigStatus, PayrollRunKind,
    PayrollRunStatus, StaffCategory,
};
use sutra_people::payroll::common::Cmd;
use sutra_people::payroll::{config as pc, runs};
use sutra_people::shared::approval::ApprovalMode;
use sutra_people::shared::lifecycle::{change_engagement_status, StatusChange};
use sutra_people::shared::rows::lock_engagement;
use sutra_people::shared::Actor;
use uuid::Uuid;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000b7c00";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-0000000b7c01";
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
    v["version"].as_i64().expect("version")
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_owned()
}

async fn outbox(pool: &sqlx::PgPool, event_type: &str, subject: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(event_type)
    .bind(subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Journal totals `(debit, credit, lines)` of a journal entry.
async fn journal(
    pool: &sqlx::PgPool,
    je: &str,
) -> (rust_decimal::Decimal, rust_decimal::Decimal, i64) {
    sqlx::query_as(
        "SELECT COALESCE(sum(debit), 0), COALESCE(sum(credit), 0), count(*) FROM finance.journal_line \
         WHERE journal_entry_id = $1",
    )
    .bind(Uuid::parse_str(je).unwrap())
    .fetch_one(pool)
    .await
    .unwrap()
}

/// A draft configuration with BASIC / DA / HRA / BONUS earnings and PF + professional tax.
async fn draft_config(app: &axum::Router, admin: &Login, inst: Uuid) -> Value {
    let (s, cfg) = call(
        app,
        admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-configs"),
        json!({"name": "Payroll 2026", "effectiveFrom": "2026-04-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{cfg}");
    assert_eq!(cfg["status"], "draft");
    let cid = id(&cfg);
    let mut v = ver(&cfg);
    let comps = [
        json!({"code": "BASIC", "name": "Basic pay", "kind": "earning", "basis": "fixed", "sequence": 10}),
        json!({"code": "DA", "name": "Dearness allowance", "kind": "earning", "basis": "percent_of_basic", "rate": "10", "sequence": 20}),
        json!({"code": "HRA", "name": "House rent allowance", "kind": "earning", "basis": "percent_of_basic", "rate": "40", "sequence": 30}),
        json!({"code": "BONUS", "name": "Bonus", "kind": "earning", "basis": "fixed", "sequence": 40}),
        json!({"code": "PF", "name": "Provident fund", "kind": "deduction", "basis": "statutory", "sequence": 100}),
        json!({"code": "PF_ER", "name": "Employer PF", "kind": "employer_contribution", "basis": "statutory", "sequence": 110}),
        json!({"code": "PT", "name": "Professional tax", "kind": "deduction", "basis": "statutory", "sequence": 120}),
    ];
    for c in comps {
        let (s, b) = call(
            app,
            admin,
            "POST",
            &format!("{P}/payroll-configs/{cid}/components"),
            c,
            Some(v),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{b}");
        v = ver(&b);
    }
    let (s, b) = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/statutory-rules"),
        json!({"kind": "pf", "employeeRate": "12", "employerRate": "12", "wageCeiling": "15000",
               "employeeComponentCode": "PF", "employerComponentCode": "PF_ER"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}

/// Adds the professional-tax rule, submits (auto-approved) and activates.
async fn complete_and_activate(app: &axum::Router, admin: &Login, cfg: &Value) -> Value {
    let cid = id(cfg);
    let (s, b) = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/statutory-rules"),
        json!({"kind": "professional_tax", "employeeComponentCode": "PT",
               "slabs": [{"upTo": "15000", "amount": "0"}, {"upTo": null, "amount": "200"}]}),
        Some(ver(cfg)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, b) = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/submit"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "approved");
    let (s, b) = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/activate"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "active");
    b
}

#[tokio::test]
async fn payroll_lifecycle_over_http() {
    let (app, pool) = app(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera_login = login(&app, "meera.shah@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_A, "Payroll University").await;
    let inst = setup.institution().as_uuid();

    let meera = b7::seed_staff(
        &pool,
        &setup,
        "Meera",
        Some(b4::MEERA),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let kiran = b7::seed_staff(
        &pool,
        &setup,
        "Kiran",
        None,
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let ravi = b7::seed_staff(
        &pool,
        &setup,
        "Ravi",
        None,
        StaffCategory::Teaching,
        Some(EngagementStatus::Closed),
    )
    .await;
    let eng = |s: &b7::SeededStaff| s.engagement_id.unwrap().as_uuid();

    // Leave inputs (as 7B posts them): Meera 2 unpaid days + 1 absence + 3 encashed days;
    // Ravi 5 encashed days on exit.
    let (lwp, el) = (Uuid::now_v7(), Uuid::now_v7());
    for s in [&meera, &ravi] {
        b7::seed_leave_entry(
            &pool,
            &setup,
            s,
            el,
            "EL",
            true,
            LeaveEntryKind::Opening,
            "30.00",
            "2026-06-01",
            "open:2026",
        )
        .await;
    }
    b7::seed_leave_entry(
        &pool,
        &setup,
        &meera,
        lwp,
        "LWP",
        false,
        LeaveEntryKind::Consumption,
        "-2.00",
        "2026-11-05",
        "consume:2026-11",
    )
    .await;
    b7::seed_attendance(
        &pool,
        &setup,
        &meera,
        "2026-11-10",
        AttendanceDayStatus::Absent,
    )
    .await;
    b7::seed_leave_entry(
        &pool,
        &setup,
        &meera,
        el,
        "EL",
        true,
        LeaveEntryKind::Encashment,
        "-3.00",
        "2026-11-20",
        "encash:meera",
    )
    .await;
    b7::seed_leave_entry(
        &pool,
        &setup,
        &ravi,
        el,
        "EL",
        true,
        LeaveEntryKind::Encashment,
        "-5.00",
        "2026-11-12",
        "encash:ravi",
    )
    .await;

    // ---- E-074: configuration version 1.
    let cfg = draft_config(&app, &admin, inst).await;
    let cid = id(&cfg);
    // 428 without If-Match, 412 on a stale version.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/submit"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/submit"),
        json!({}),
        Some(1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    // 422: the PT component has no statutory rule yet.
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/submit"),
        json!({}),
        Some(ver(&cfg)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert!(b.to_string().contains("rule_missing"), "{b}");
    let (s, _) = call(
        &app,
        &meera_login,
        "POST",
        &format!("{P}/payroll-configs/{cid}/submit"),
        json!({}),
        Some(ver(&cfg)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "faculty cannot manage payroll");
    let v1 = complete_and_activate(&app, &admin, &cfg).await;
    assert_eq!(v1["versionNo"], 1);
    assert_eq!(v1["components"].as_array().unwrap().len(), 7);
    assert_eq!(v1["statutoryRules"].as_array().unwrap().len(), 2);
    assert_eq!(
        outbox(&pool, "sutraerp.people.payroll_config.activated", &cid).await,
        1
    );
    // An active configuration is not edited (L4-074: a new version is).
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/components"),
        json!({"code": "TA", "name": "Transport", "kind": "earning", "basis": "fixed", "amount": "100"}),
        Some(ver(&v1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Version 2 copies version 1; activating it supersedes version 1 (one active).
    let (s, v2) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-configs"),
        json!({"name": "Payroll 2026 rev", "effectiveFrom": "2026-11-01", "copyFrom": cid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v2}");
    assert_eq!(v2["versionNo"], 2);
    assert_eq!(v2["components"].as_array().unwrap().len(), 7);
    let v2id = id(&v2);
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{v2id}/submit"),
        json!({}),
        Some(ver(&v2)),
    )
    .await;
    assert_eq!(
        (s, b["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{b}"
    );
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-configs/{v2id}/activate"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(
        (s, b["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{b}"
    );
    let (_, old) = read(&app, &admin, &format!("{P}/payroll-configs/{cid}")).await;
    assert_eq!(old["status"], "superseded");
    let (_, list) = read(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/payroll-configs?status=active"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // ---- salary structure by grade + assignments.
    let (s, ap1) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/salary-structures"),
        json!({"code": "AP-1", "name": "Assistant Professor 1", "grade": "AP-1",
               "lines": [{"componentCode": "BASIC", "amount": "30000"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ap1}");
    let (s, ma) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/salary-assignments", eng(&meera)),
        json!({"effectiveFrom": "2026-06-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ma}");
    assert_eq!(ma["structureCode"], "AP-1");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/salary-assignments", eng(&meera)),
        json!({"effectiveFrom": "2026-09-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Ravi's engagement is already closed: his assignment predates the exit.
    sqlx::query(
        "INSERT INTO people.salary_assignment (tenant_id, institution_id, staff_id, engagement_id, \
         structure_id, effective_from) VALUES ($1, $2, $3, $4, $5, DATE '2026-06-01')",
    )
    .bind(setup.tenant().as_uuid())
    .bind(inst)
    .bind(ravi.staff_id.as_uuid())
    .bind(eng(&ravi))
    .bind(Uuid::parse_str(&id(&ap1)).unwrap())
    .execute(&pool)
    .await
    .unwrap();

    // ---- period.
    let (s, period) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-periods"),
        json!({"year": 2026, "month": 11}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{period}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-periods"),
        json!({"year": 2026, "month": 11}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let pid = id(&period);

    // ---- E-056: final settlement of Ravi (closed, last working date 2026-11-15).
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/final-settlement", eng(&meera)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "live engagement");
    let (s, fnf) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/final-settlement", eng(&ravi)),
        json!({"recoveries": [{"code": "NOTICE", "name": "Notice shortfall", "amount": "2000"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{fnf}");
    assert_eq!(fnf["status"], "initiated");
    let fid = id(&fnf);
    let (s, fnf) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/final-settlements/{fid}/calculate"),
        json!({}),
        Some(ver(&fnf)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{fnf}");
    assert_eq!(fnf["status"], "approved");
    // 15 of 30 days: 15000 + 1500 + 6000; 5 days × 1100 encashment; PF 1800, PT 200, NOTICE 2000.
    assert_eq!(fnf["salaryAmount"], "22500.00");
    assert_eq!(fnf["encashmentAmount"], "5500.00");
    assert_eq!(fnf["encashmentDays"], "5.00");
    assert_eq!(fnf["recoveryAmount"], "2000.00");
    assert_eq!(fnf["netPay"], "24000.00");
    let (s, fnf) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/final-settlements/{fid}/complete"),
        json!({}),
        Some(ver(&fnf)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{fnf}");
    assert_eq!(fnf["status"], "completed");
    assert!(fnf["number"].as_str().unwrap().starts_with("FNF-"), "{fnf}");
    let je = fnf["journalEntryNumber"].as_str().map(str::to_owned);
    assert!(je.is_some(), "{fnf}");
    assert_eq!(
        outbox(&pool, "sutraerp.people.final_settlement.completed", &fid).await,
        1
    );
    let (_, of) = read(
        &app,
        &admin,
        &format!("{P}/engagements/{}/final-settlement", eng(&ravi)),
    )
    .await;
    assert_eq!(of["id"], fnf["id"]);

    // ---- E-053: the regular run.
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-periods/{pid}/runs"),
        json!({"kind": "regular"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    assert!(run["number"].as_str().unwrap().starts_with("PR-"));
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-periods/{pid}/runs"),
        json!({"kind": "regular"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no duplicate regular run");
    let rid = id(&run);
    let (s, _) = call(
        &app,
        &meera_login,
        "POST",
        &format!("{P}/payroll-runs/{rid}/calculate"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Kiran has no salary assignment yet: calculation records the exception, validation fails.
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/calculate"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(run["status"], "calculated");
    assert_eq!(
        run["headcount"], 1,
        "Ravi was settled, Kiran has no assignment: {run}"
    );
    assert_eq!(run["exceptions"][0]["code"], "missing_salary_assignment");
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/validate"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(
        (s, run["status"].as_str()),
        (StatusCode::OK, Some("failed")),
        "{run}"
    );
    assert_eq!(
        outbox(&pool, "sutraerp.people.payroll_run.failed", &rid).await,
        1
    );
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/reprocess"),
        json!({"reason": "assignment added"}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(
        (s, run["status"].as_str()),
        (StatusCode::OK, Some("reprocessing")),
        "{run}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/salary-assignments", eng(&kiran)),
        json!({"structureId": id(&ap1), "effectiveFrom": "2026-06-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/calculate"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(run["headcount"], 2);
    assert!(run["exceptions"].as_array().unwrap().is_empty(), "{run}");
    // Meera: 27 payable days (2 unpaid + 1 absent) → 27000 + 2700 + 10800 + 3300 encashment;
    // PF 1800 + PT 200. Kiran: full month 45000 − 2000.
    assert_eq!(run["grossEarnings"], "88800.00");
    assert_eq!(run["employeeDeductions"], "4000.00");
    assert_eq!(run["employerContributions"], "3600.00");
    assert_eq!(run["netPay"], "84800.00");
    let (_, results) = read(&app, &admin, &format!("{P}/payroll-runs/{rid}/results")).await;
    let items = results["items"].as_array().unwrap().clone();
    let mres = items
        .iter()
        .find(|r| r["staffNumber"] == meera.staff_number.as_str())
        .unwrap()
        .clone();
    assert_eq!(mres["lopDays"], "3.00");
    assert_eq!(mres["payableDays"], "27.00");
    assert_eq!(mres["netPay"], "41800.00");
    assert!(mres["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l["code"] == "LEAVE_ENC" && l["amount"] == "3300.00"));
    assert_eq!(mres["snapshot"]["config"]["versionNo"], 2);
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/validate"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(
        (s, run["status"].as_str()),
        (StatusCode::OK, Some("validating")),
        "{run}"
    );
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/submit"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(
        (s, run["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{run}"
    );
    assert_eq!(
        outbox(&pool, "sutraerp.people.payroll_run.approved", &rid).await,
        1
    );
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/finalise"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(
        (s, run["status"].as_str()),
        (StatusCode::OK, Some("finalized")),
        "{run}"
    );
    // Encashment entries are paid once (Meera's here, Ravi's by his settlement).
    let paid: i64 =
        sqlx::query_scalar("SELECT count(*) FROM people.encashment_payment WHERE tenant_id = $1")
            .bind(setup.tenant().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(paid, 2);
    // Finalised results are immutable at database level.
    let tampered = sqlx::query("UPDATE people.payroll_result SET net_pay = 1 WHERE id = $1")
        .bind(Uuid::parse_str(&id(&mres)).unwrap())
        .execute(&pool)
        .await;
    assert!(tampered.is_err());
    let (s, run) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/post"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(
        (s, run["status"].as_str()),
        (StatusCode::OK, Some("posted")),
        "{run}"
    );
    let je = run["journalEntryId"].as_str().unwrap().to_owned();
    // Balanced: debit = gross + employer contributions.
    assert_eq!(
        journal(&pool, &je).await,
        (dec("92400.00"), dec("92400.00"), 3)
    );
    assert_eq!(
        outbox(&pool, "sutraerp.people.payroll_run.posted", &rid).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/post"),
        json!({}),
        Some(ver(&run)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "posted once");
    // The posting port itself is idempotent on (run, posting key).
    let replay: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE source_type = 'payroll_run' AND source_id = $1",
    )
    .bind(Uuid::parse_str(&rid).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(replay, 1);

    // ---- payslips.
    let (s, sum) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/payslips/issue"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(
        (sum["processed"].as_i64(), sum["skipped"].as_i64()),
        (Some(2), Some(0))
    );
    let (_, sum) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{rid}/payslips/issue"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (sum["processed"].as_i64(), sum["skipped"].as_i64()),
        (Some(0), Some(2))
    );
    let (s, mine) = read(&app, &meera_login, &format!("{P}/me/payslips")).await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    let slip = mine["items"][0].clone();
    assert!(slip["number"].as_str().unwrap().starts_with("PS-"));
    assert_eq!(slip["netPay"], "41800.00");
    assert_eq!(slip["lopDays"], "3.00");
    let (_, slips) = read(&app, &admin, &format!("{P}/payroll-runs/{rid}/payslips")).await;
    let kiran_slip = slips["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["staffNumber"] == kiran.staff_number.as_str())
        .unwrap()
        .clone();
    let (s, _) = read(
        &app,
        &meera_login,
        &format!("{P}/me/payslips/{}", id(&kiran_slip)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "someone else's payslip");

    // ---- E-054: a corrective adjustment never edits the finalised result.
    let mid = id(&mres);
    let (s, adj) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-results/{mid}/adjustments"),
        json!({"reason": "HRA arrears", "lines": [{"code": "HRA", "amount": "1000.00"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{adj}");
    assert_eq!(adj["status"], "requested");
    let aid = id(&adj);
    let (s, adj) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-adjustments/{aid}/validate"),
        json!({}),
        Some(ver(&adj)),
    )
    .await;
    assert_eq!(
        (s, adj["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{adj}"
    );
    let (s, adj) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-adjustments/{aid}/apply"),
        json!({}),
        Some(ver(&adj)),
    )
    .await;
    assert_eq!(
        (s, adj["status"].as_str()),
        (StatusCode::OK, Some("applied")),
        "{adj}"
    );
    let (_, old) = read(&app, &admin, &format!("{P}/payroll-results/{mid}")).await;
    assert_eq!(old["status"], "superseded");
    assert_eq!(old["netPay"], "41800.00");
    let rev = adj["revisedResultId"].as_str().unwrap().to_owned();
    let (_, new) = read(&app, &admin, &format!("{P}/payroll-results/{rev}")).await;
    assert_eq!(
        (new["revision"].as_i64(), new["netPay"].as_str()),
        (Some(2), Some("42800.00"))
    );
    assert_eq!(
        journal(&pool, adj["journalEntryId"].as_str().unwrap()).await,
        (dec("1000.00"), dec("1000.00"), 2)
    );
    assert_eq!(
        outbox(&pool, "sutraerp.people.payroll_adjustment.applied", &aid).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-results/{mid}/adjustments"),
        json!({"reason": "again", "lines": [{"code": "HRA", "amount": "1.00"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "superseded revision");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-results/{rev}/adjustments"),
        json!({"reason": "bad", "lines": [{"code": "NEWCODE", "amount": "1.00"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, slip) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payslips/{}/reissue", id(&slip)),
        json!({"reason": "adjusted"}),
        Some(ver(&slip)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{slip}");
    assert_eq!(
        (slip["status"].as_str(), slip["netPay"].as_str()),
        (Some("reissued"), Some("42800.00"))
    );

    // ---- E-055: retro — Meera's basic rises to 33000 from November.
    let (s, rev_s) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/salary-structures"),
        json!({"code": "AP-1-REV", "name": "Assistant Professor 1 (revised)",
               "lines": [{"componentCode": "BASIC", "amount": "33000"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rev_s}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/salary-assignments/{}/end", id(&ma)),
        json!({"endDate": "2026-10-31", "reason": "revision"}),
        Some(ver(&ma)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/salary-assignments", eng(&meera)),
        json!({"structureId": id(&rev_s), "effectiveFrom": "2026-11-01", "reason": "pay revision"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, retro) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-periods/{pid}/runs"),
        json!({"kind": "retro"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{retro}");
    let tid = id(&retro);
    let (s, retro) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-runs/{tid}/calculate"),
        json!({}),
        Some(ver(&retro)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{retro}");
    // Only Meera changed; arrears: 2700 + 270 + 1080 (PF / PT unchanged at the ceiling / slab).
    assert_eq!(retro["headcount"], 1, "{retro}");
    assert_eq!(retro["grossEarnings"], "4050.00");
    assert_eq!(retro["netPay"], "4050.00");
    let (_, rres) = read(&app, &admin, &format!("{P}/payroll-runs/{tid}/results")).await;
    assert!(rres["items"][0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .all(|l| l["source"] == "retro"));
    let mut r = retro;
    for step in ["validate", "submit", "finalise", "post", "close"] {
        let (s, b) = call(
            &app,
            &admin,
            "POST",
            &format!("{P}/payroll-runs/{tid}/{step}"),
            json!({}),
            Some(ver(&r)),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{step}: {b}");
        r = b;
    }
    assert_eq!(r["status"], "closed");

    // ---- E-056 no double pay: Kiran leaves on 2026-11-20 after the regular run of November
    // already paid him the whole month; his settlement pays the encashment only, no salary.
    {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "UPDATE people.staff_engagement SET last_working_date = DATE '2026-11-20' WHERE id = $1",
        )
        .bind(eng(&kiran))
        .execute(&mut *tx)
        .await
        .unwrap();
        let locked = lock_engagement(&mut tx, setup.tenant(), kiran.engagement_id.unwrap())
            .await
            .unwrap();
        change_engagement_status(
            &mut tx,
            setup.tenant(),
            &locked,
            EngagementStatus::Closed,
            &StatusChange {
                cause_event_type: "test.separation",
                reason: Some("resigned"),
                process_instance_id: None,
            },
            &b7::seed_actor(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
    }
    b7::seed_leave_entry(
        &pool,
        &setup,
        &kiran,
        el,
        "EL",
        true,
        LeaveEntryKind::Opening,
        "30.00",
        "2026-06-01",
        "open:2026",
    )
    .await;
    b7::seed_leave_entry(
        &pool,
        &setup,
        &kiran,
        el,
        "EL",
        true,
        LeaveEntryKind::Encashment,
        "-2.00",
        "2026-11-18",
        "encash:kiran",
    )
    .await;
    let (s, kf) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/final-settlement", eng(&kiran)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{kf}");
    let kfid = id(&kf);
    let (s, kf) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/final-settlements/{kfid}/calculate"),
        json!({}),
        Some(ver(&kf)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{kf}");
    assert_eq!(
        kf["salaryAmount"], "0.00",
        "November was paid by the regular run: {kf}"
    );
    assert_eq!(kf["encashmentAmount"], "2200.00", "2 days × 1100: {kf}");
    let (s, kf) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/final-settlements/{kfid}/complete"),
        json!({}),
        Some(ver(&kf)),
    )
    .await;
    assert_eq!(
        (s, kf["status"].as_str()),
        (StatusCode::OK, Some("completed")),
        "{kf}"
    );
    // Database: the month's salary (calculation earnings) was paid exactly once — by the regular
    // run — and the settlement result carries no employed days.
    let (regular_basic, settlement_basic): (rust_decimal::Decimal, rust_decimal::Decimal) =
        sqlx::query_as(
            "SELECT COALESCE(sum(l.amount) FILTER (WHERE u.kind = 'regular'), 0), \
                    COALESCE(sum(l.amount) FILTER (WHERE u.kind = 'final_settlement'), 0) \
             FROM people.payroll_result_line l \
             JOIN people.payroll_result r ON r.id = l.result_id \
             JOIN people.payroll_run u ON u.id = r.run_id \
             WHERE r.engagement_id = $1 AND r.status = 'finalized' AND l.code = 'BASIC' \
               AND l.source = 'calculation' AND r.period_start = DATE '2026-11-01'",
        )
        .bind(eng(&kiran))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(regular_basic, dec("30000.00"), "full November basic once");
    assert_eq!(settlement_basic, rust_decimal::Decimal::ZERO, "never twice");
    let (employed, paid_flag): (rust_decimal::Decimal, Option<bool>) = sqlx::query_as(
        "SELECT r.employed_days, (r.snapshot->>'regularRunPaidFinalMonth')::boolean \
         FROM people.payroll_result r JOIN people.final_settlement f ON f.run_id = r.run_id \
         WHERE f.id = $1 AND r.status = 'finalized'",
    )
    .bind(Uuid::parse_str(&kfid).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(employed, rust_decimal::Decimal::ZERO);
    assert_eq!(paid_flag, Some(true));
}

#[tokio::test]
async fn await_decision_keeps_payroll_steps_pending() {
    let (app, pool) = app(TENANT_B).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_B, "Payroll College").await;
    let inst = setup.institution().as_uuid();
    let asha = b7::seed_staff(
        &pool,
        &setup,
        "Asha",
        None,
        StaffCategory::NonTeaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let cfg = draft_config(&app, &admin, inst).await;
    complete_and_activate(&app, &admin, &cfg).await;
    let (_, period) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-periods"),
        json!({"year": 2026, "month": 12}),
        None,
    )
    .await;
    let pid = Uuid::parse_str(&id(&period)).unwrap();

    let mut st = sutra_api::app::people_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let maker = Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };
    let approver = Actor {
        id: Some("dev|approver@sutra.dev".into()),
        correlation_id: None,
    };
    let t = setup.tenant();
    let c = Cmd {
        st: &st,
        tenant: t,
        actor: &maker,
    };
    let a = Cmd {
        st: &st,
        tenant: t,
        actor: &approver,
    };

    // ---- configuration review waits for a decision; rejection returns it to draft.
    let v3 = pc::create_config(
        &c,
        setup.institution(),
        pc::NewConfig {
            name: "Next year".into(),
            effective_from: date("2027-04-01"),
            rounding: sutra_people::payroll::domain::Rounding::Rupee,
            encashment_divisor: 26,
            copy_from: Some(Uuid::parse_str(&id(&cfg)).unwrap()),
        },
    )
    .await
    .unwrap();
    let v3 = pc::submit_config(&c, v3.id, v3.version, None)
        .await
        .unwrap();
    assert_eq!(v3.status, PayrollConfigStatus::Review);
    let v3 = pc::decide_config(&a, v3.id, v3.version, false, Some("rates pending"))
        .await
        .unwrap();
    assert_eq!(v3.status, PayrollConfigStatus::Draft);

    // ---- E-057: off-cycle bonus run.
    let run = runs::create_run(
        &c,
        pid,
        runs::NewRun {
            kind: PayrollRunKind::OffCycle,
            description: Some("Festival bonus".into()),
            items: vec![runs::OffCycleItem {
                engagement_id: asha.engagement_id.unwrap().as_uuid(),
                component_code: "BONUS".into(),
                amount: dec("5000"),
                note: None,
            }],
        },
    )
    .await
    .unwrap();
    let run = runs::calculate_run(&c, run.id, run.version).await.unwrap();
    assert_eq!(run.status, PayrollRunStatus::Calculated);
    assert_eq!(run.gross_earnings, dec("5000"));
    assert_eq!(run.net_pay, dec("5000"));
    let run = runs::validate_run(&c, run.id, run.version).await.unwrap();
    let run = runs::submit_run(&c, run.id, run.version, None)
        .await
        .unwrap();
    assert_eq!(run.status, PayrollRunStatus::PendingApproval);
    assert!(run.process_instance_id.is_some());
    assert_eq!(
        outbox(
            &pool,
            "sutraerp.people.payroll_run.submitted",
            &run.id.to_string()
        )
        .await,
        1
    );
    let early = runs::finalise_run(&a, run.id, run.version).await;
    assert!(early.is_err(), "not approved yet");
    let run = runs::decide_run(&a, run.id, run.version, false, Some("wrong amount"))
        .await
        .unwrap();
    assert_eq!(run.status, PayrollRunStatus::Calculated);
    let run = runs::validate_run(&c, run.id, run.version).await.unwrap();
    let run = runs::submit_run(&c, run.id, run.version, None)
        .await
        .unwrap();
    let run = runs::decide_run(&a, run.id, run.version, true, None)
        .await
        .unwrap();
    assert_eq!(run.status, PayrollRunStatus::Approved);
    let run = runs::finalise_run(&a, run.id, run.version).await.unwrap();
    assert_eq!(run.status, PayrollRunStatus::Finalized);
    let (results, _) = runs::list_results(&st, t, run.id, false, 50, None)
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].lines[0].source, "off_cycle");

    // ---- posting into a closed financial period (Dec 2026) is refused with Conflict and the
    // run stays finalised (nothing half-posted).
    let fy: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.financial_year (tenant_id, institution_id, code, name, start_date, \
         end_date, status) VALUES ($1, $2, 'FY2026-27', 'Financial year 2026-27', '2026-04-01', \
         '2027-03-31', 'open') RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO finance.financial_period (tenant_id, institution_id, financial_year_id, \
         period_no, name, start_date, end_date, status, closed_at, closed_by) VALUES ($1, $2, $3, \
         9, 'Dec 2026', '2026-12-01', '2026-12-31', 'closed', now(), 'test')",
    )
    .bind(t.as_uuid())
    .bind(inst)
    .bind(fy)
    .execute(&pool)
    .await
    .unwrap();
    let refused = runs::post_run(&a, run.id, run.version).await;
    assert!(
        matches!(&refused, Err(sutra_kernel::error::KernelError::Conflict(m)) if m.contains("closed")),
        "{refused:?}"
    );
    let still = runs::get_run(&st, t, run.id).await.unwrap();
    assert_eq!(still.status, PayrollRunStatus::Finalized);
    assert!(still.journal_entry_id.is_none());
}
