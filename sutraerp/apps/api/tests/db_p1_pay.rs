//! Part 1 Stage 2 — DEF-E-007 (net pay is never disbursed; salaries payable is never cleared)
//! against PostgreSQL.
//!
//! * `disbursement_clears_salaries_and_statutory_payables_over_http` — a payroll run is posted
//!   (Dr 5410 / Cr 2410 / Cr 2420); the net-pay batch needs every payee's bank account (422),
//!   wrong role → 403 on every disbursement / bank-account endpoint, creation is idempotent (201
//!   then 200, one batch), the NEFT file balances (H / D / T records, BLOCKED_EXTERNAL
//!   submission), mark-paid needs the file first (409), posts one balanced journal Dr 2410 / Cr
//!   bank, salaries payable is back to zero, a second mark-paid is a 409 and posts nothing; the
//!   statutory remittance batch (challan reference) clears 2420 to zero the same way.
//! * `await_decision_keeps_disbursement_batches_pending` — `ApprovalMode::AwaitDecision`: the
//!   batch waits for a decision (no file, no payment while pending), rejection needs a reason
//!   and frees the run for a new batch, approval → file → paid; two concurrent mark-paid
//!   commands post exactly one journal entry and one of them is refused.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use http_body_util::BodyExt;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;
use std::sync::Arc;
use support::b7;
use support::*;
use sutra_contracts::finance::ports::PayrollPayoutKind;
use sutra_contracts::people::status::{EngagementStatus, StaffCategory};
use sutra_kernel::error::KernelError;
use sutra_people::payroll::common::Cmd;
use sutra_people::payroll::disbursement::{self as pd, PayrollDisbursementStatus};
use sutra_people::shared::approval::ApprovalMode;
use sutra_people::shared::Actor;
use tower::ServiceExt;
use uuid::Uuid;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000e0070";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-0000000e0071";
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

/// GET returning the raw body (the CSV file).
async fn raw(
    app: &axum::Router,
    who: &Login,
    path: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let res = app.clone().oneshot(get(who, path)).await.unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, String::from_utf8(bytes.to_vec()).unwrap())
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().expect("version")
}

fn id(v: &Value) -> String {
    v["id"].as_str().expect("id").to_owned()
}

fn dec(v: &Value) -> Decimal {
    Decimal::from_str(v.as_str().expect("decimal string")).unwrap()
}

/// Balance (credit − debit) of the institution's ledger account with `system_key`.
async fn balance(pool: &sqlx::PgPool, inst: Uuid, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(l.credit - l.debit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE a.institution_id = $1 AND a.system_key = $2",
    )
    .bind(inst)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `(debit, credit)` of one journal entry on the account with `system_key`.
async fn entry_side(pool: &sqlx::PgPool, je: Uuid, key: &str) -> (Decimal, Decimal) {
    sqlx::query_as(
        "SELECT COALESCE(sum(l.debit), 0), COALESCE(sum(l.credit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE l.journal_entry_id = $1 AND a.system_key = $2",
    )
    .bind(je)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `(debit, credit, lines)` of a journal entry.
async fn journal(pool: &sqlx::PgPool, je: Uuid) -> (Decimal, Decimal, i64) {
    sqlx::query_as(
        "SELECT COALESCE(sum(debit), 0), COALESCE(sum(credit), 0), count(*) \
         FROM finance.journal_line WHERE journal_entry_id = $1",
    )
    .bind(je)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn payout_entries(pool: &sqlx::PgPool, batch: Uuid) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE source_type = 'payroll_disbursement' \
         AND source_id = $1",
    )
    .bind(batch)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Approves `v` through `decision_path` when the approval helper left it pending (the
/// production approval mode decides whether submissions are auto-approved).
async fn approve_if_pending(
    app: &axum::Router,
    admin: &Login,
    v: Value,
    decision_path: &str,
) -> Value {
    let status = v["status"].as_str().unwrap_or_default();
    if !["pending_approval", "review", "under_review"].contains(&status) {
        return v;
    }
    let (s, b) = call(
        app,
        admin,
        "POST",
        decision_path,
        json!({"decision": "approve"}),
        Some(ver(&v)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}

struct Posted {
    setup: b7::PeopleSetup,
    run: Value,
    meera: b7::SeededStaff,
    kiran: b7::SeededStaff,
}

/// An active payroll configuration (BASIC + PF / employer PF), two staff with salary
/// assignments, and the November 2026 regular run calculated → approved → finalised → posted.
async fn posted_run(
    app: &axum::Router,
    admin: &Login,
    pool: &sqlx::PgPool,
    tenant: &str,
    name: &str,
) -> Posted {
    let setup = b7::people_setup(app, admin, pool, tenant, name).await;
    let inst = setup.institution().as_uuid();
    let meera = b7::seed_staff(
        pool,
        &setup,
        "Meera",
        Some(b4::MEERA),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let kiran = b7::seed_staff(
        pool,
        &setup,
        "Kiran",
        None,
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;

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
    let cid = id(&cfg);
    let mut v = ver(&cfg);
    for c in [
        json!({"code": "BASIC", "name": "Basic pay", "kind": "earning", "basis": "fixed", "sequence": 10}),
        json!({"code": "PF", "name": "Provident fund", "kind": "deduction", "basis": "statutory", "sequence": 100}),
        json!({"code": "PF_ER", "name": "Employer PF", "kind": "employer_contribution", "basis": "statutory", "sequence": 110}),
    ] {
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
    let b = approve_if_pending(
        app,
        admin,
        b,
        &format!("{P}/payroll-configs/{cid}/decision"),
    )
    .await;
    assert_eq!(b["status"], "approved", "{b}");
    let (s, b) = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/activate"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(
        (s, b["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{b}"
    );

    let (s, ap1) = call(
        app,
        admin,
        "POST",
        &format!("{P}/institutions/{inst}/salary-structures"),
        json!({"code": "AP-1", "name": "Assistant Professor 1", "grade": "AP-1",
               "lines": [{"componentCode": "BASIC", "amount": "30000"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ap1}");
    for st in [&meera, &kiran] {
        let (s, b) = call(
            app,
            admin,
            "POST",
            &format!(
                "{P}/engagements/{}/salary-assignments",
                st.engagement_id.unwrap().as_uuid()
            ),
            json!({"effectiveFrom": "2026-06-01"}),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
    let (s, period) = call(
        app,
        admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-periods"),
        json!({"year": 2026, "month": 11}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{period}");
    let (s, run) = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-periods/{}/runs", id(&period)),
        json!({"kind": "regular"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{run}");
    let rid = id(&run);
    let mut run = run;
    for step in ["calculate", "validate", "submit"] {
        let (s, b) = call(
            app,
            admin,
            "POST",
            &format!("{P}/payroll-runs/{rid}/{step}"),
            json!({}),
            Some(ver(&run)),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{step}: {b}");
        run = b;
    }
    let run =
        approve_if_pending(app, admin, run, &format!("{P}/payroll-runs/{rid}/decision")).await;
    assert_eq!(run["status"], "approved", "{run}");
    let mut run = run;
    for (step, want) in [("finalise", "finalized"), ("post", "posted")] {
        let (s, b) = call(
            app,
            admin,
            "POST",
            &format!("{P}/payroll-runs/{rid}/{step}"),
            json!({}),
            Some(ver(&run)),
        )
        .await;
        assert_eq!(
            (s, b["status"].as_str()),
            (StatusCode::OK, Some(want)),
            "{b}"
        );
        run = b;
    }
    assert_eq!(run["headcount"], 2, "{run}");
    Posted {
        setup,
        run,
        meera,
        kiran,
    }
}

async fn put_bank(
    app: &axum::Router,
    who: &Login,
    staff: Uuid,
    number: &str,
) -> (StatusCode, Value) {
    call(
        app,
        who,
        "PUT",
        &format!("{P}/staff/{staff}/bank-account"),
        json!({"accountHolderName": "Seeded Payee", "accountNumber": number, "ifsc": "hdfc0001234",
               "bankName": "HDFC Bank"}),
        None,
    )
    .await
}

#[tokio::test]
async fn disbursement_clears_salaries_and_statutory_payables_over_http() {
    let (app, pool) = app(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let Posted {
        setup,
        run,
        meera,
        kiran,
    } = posted_run(&app, &admin, &pool, TENANT_A, "Disbursement University").await;
    let inst = setup.institution().as_uuid();
    let rid = id(&run);
    let net = dec(&run["netPay"]);
    let dues = dec(&run["employeeDeductions"]) + dec(&run["employerContributions"]);
    assert!(net > Decimal::ZERO && dues > Decimal::ZERO, "{run}");
    // The run's posting left the liabilities open.
    assert_eq!(balance(&pool, inst, "payroll_payable").await, net);
    assert_eq!(
        balance(&pool, inst, "payroll_deductions_payable").await,
        dues
    );

    let create = format!("{P}/payroll-runs/{rid}/disbursements");
    // ---- every payee needs a salary bank account.
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &create,
        json!({"kind": "net_pay"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert!(b.to_string().contains("bank_account_missing"), "{b}");
    assert!(b.to_string().contains(&meera.staff_number), "{b}");

    // ---- wrong role: faculty may not touch bank accounts or disbursements.
    let (s, _) = put_bank(&app, &faculty, meera.staff_id.as_uuid(), "001234567890").await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        &faculty,
        "POST",
        &create,
        json!({"kind": "net_pay"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = read(
        &app,
        &faculty,
        &format!("{P}/institutions/{inst}/payroll-disbursements"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    let (s, b) = put_bank(&app, &admin, meera.staff_id.as_uuid(), "12ab").await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    for (st, number) in [(&meera, "001234567890"), (&kiran, "009876543210")] {
        let (s, b) = put_bank(&app, &admin, st.staff_id.as_uuid(), number).await;
        assert_eq!(s, StatusCode::OK, "{b}");
        assert_eq!(b["ifsc"], "HDFC0001234");
        assert!(
            b["accountNumberMasked"]
                .as_str()
                .unwrap()
                .starts_with("XXXX"),
            "{b}"
        );
        assert!(
            !b.to_string().contains(number),
            "the full account number is never returned"
        );
    }

    // ---- net pay batch: created once (201), then returned (200).
    let (s, batch) = call(
        &app,
        &admin,
        "POST",
        &create,
        json!({"kind": "net_pay"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{batch}");
    assert!(
        batch["number"].as_str().unwrap().starts_with("PD-"),
        "{batch}"
    );
    assert_eq!(batch["kind"], "net_pay");
    assert_eq!(batch["bankSubmission"], "BLOCKED_EXTERNAL");
    assert_eq!(batch["lineCount"], 2);
    assert_eq!(dec(&batch["totalAmount"]), net);
    let bid = id(&batch);
    let (s, again) = call(
        &app,
        &admin,
        "POST",
        &create,
        json!({"kind": "net_pay"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{again}");
    assert_eq!(
        again["id"], batch["id"],
        "idempotent: one batch per run and kind"
    );
    let batches: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM people.payroll_disbursement WHERE run_id = $1 AND kind = 'net_pay'",
    )
    .bind(Uuid::parse_str(&rid).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(batches, 1);

    // The approval helper decides; faculty cannot decide.
    if batch["status"] == "pending_approval" {
        let (s, _) = call(
            &app,
            &faculty,
            "POST",
            &format!("{P}/payroll-disbursements/{bid}/decision"),
            json!({"decision": "approve"}),
            Some(ver(&batch)),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    let batch = approve_if_pending(
        &app,
        &admin,
        batch,
        &format!("{P}/payroll-disbursements/{bid}/decision"),
    )
    .await;
    assert_eq!(batch["status"], "approved", "{batch}");

    // Detail: lines with masked accounts.
    let (s, detail) = read(&app, &admin, &format!("{P}/payroll-disbursements/{bid}")).await;
    assert_eq!(s, StatusCode::OK, "{detail}");
    assert_eq!(detail["lines"].as_array().unwrap().len(), 2);
    assert!(!detail.to_string().contains("001234567890"), "{detail}");
    let line_sum: Decimal = detail["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| dec(&l["amount"]))
        .sum();
    assert_eq!(line_sum, net);

    // ---- mark paid before the bank file exists → 409.
    let paid_path = format!("{P}/payroll-disbursements/{bid}/mark-paid");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &paid_path,
        json!({"reference": "UTR2026110001"}),
        Some(ver(&batch)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");

    // ---- the NEFT bulk file.
    let file_path = format!("{P}/payroll-disbursements/{bid}/file");
    let (s, _, _) = raw(&app, &faculty, &file_path).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, h, csv) = raw(&app, &admin, &file_path).await;
    assert_eq!(s, StatusCode::OK, "{csv}");
    assert!(h["content-type"].to_str().unwrap().starts_with("text/csv"));
    assert!(h["content-disposition"]
        .to_str()
        .unwrap()
        .contains("-neft.csv"));
    let records: Vec<&str> = csv.split("\r\n").filter(|l| !l.is_empty()).collect();
    assert_eq!(records.len(), 4, "{csv}");
    let head: Vec<&str> = records[0].split(',').collect();
    assert_eq!(
        (head[0], head[1], head[4], head[6]),
        ("H", batch["number"].as_str().unwrap(), "2", "INR"),
        "{csv}"
    );
    assert_eq!(Decimal::from_str(head[5]).unwrap(), net);
    let mut file_sum = Decimal::ZERO;
    for d in &records[1..3] {
        let f: Vec<&str> = d.split(',').collect();
        assert_eq!(
            (f.len(), f[0], f[1], f[3]),
            (9, "D", "NEFT", "HDFC0001234"),
            "{d}"
        );
        assert!(["001234567890", "009876543210"].contains(&f[2]), "{d}");
        file_sum += Decimal::from_str(f[5]).unwrap();
    }
    assert_eq!(file_sum, net);
    assert_eq!(records[3], format!("T,2,{}", head[5]));

    // ---- wrong role cannot mark paid; the approver can, once.
    let (_, batch) = read(&app, &admin, &format!("{P}/payroll-disbursements/{bid}")).await;
    let batch = batch["batch"].clone();
    let (s, _) = call(
        &app,
        &faculty,
        "POST",
        &paid_path,
        json!({"reference": "UTR2026110001"}),
        Some(ver(&batch)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &paid_path,
        json!({"reference": "bad ref,with comma"}),
        Some(ver(&batch)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, paid) = call(
        &app,
        &admin,
        "POST",
        &paid_path,
        json!({"reference": "UTR2026110001"}),
        Some(ver(&batch)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{paid}");
    assert_eq!(paid["status"], "paid");
    assert_eq!(paid["paymentReference"], "UTR2026110001");
    let je = Uuid::parse_str(paid["journalEntryId"].as_str().unwrap()).unwrap();
    // Balanced: Dr salaries payable / Cr bank, the batch total.
    assert_eq!(journal(&pool, je).await, (net, net, 2));
    assert_eq!(
        entry_side(&pool, je, "payroll_payable").await,
        (net, Decimal::ZERO)
    );
    assert_eq!(
        entry_side(&pool, je, "bank_main").await,
        (Decimal::ZERO, net)
    );
    assert_eq!(
        balance(&pool, inst, "payroll_payable").await,
        Decimal::ZERO,
        "salaries payable cleared"
    );

    // ---- a second mark-paid is refused and posts nothing.
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &paid_path,
        json!({"reference": "UTR2026110002"}),
        Some(ver(&paid)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert_eq!(
        payout_entries(&pool, Uuid::parse_str(&bid).unwrap()).await,
        1
    );
    assert_eq!(balance(&pool, inst, "payroll_payable").await, Decimal::ZERO);
    // Creating again still returns the (paid) batch.
    let (s, again) = call(
        &app,
        &admin,
        "POST",
        &create,
        json!({"kind": "net_pay"}),
        None,
    )
    .await;
    assert_eq!(
        (s, again["status"].as_str()),
        (StatusCode::OK, Some("paid"))
    );

    // ---- statutory dues remittance by challan: Dr 2420 / Cr bank.
    let (s, dues_batch) = call(
        &app,
        &admin,
        "POST",
        &create,
        json!({"kind": "statutory_dues"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{dues_batch}");
    assert_eq!(dec(&dues_batch["totalAmount"]), dues);
    let did = id(&dues_batch);
    let dues_batch = approve_if_pending(
        &app,
        &admin,
        dues_batch,
        &format!("{P}/payroll-disbursements/{did}/decision"),
    )
    .await;
    let (s, _, advice) = raw(
        &app,
        &admin,
        &format!("{P}/payroll-disbursements/{did}/file"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{advice}");
    assert!(
        advice.contains("\r\nD,PF,") && advice.contains("\r\nD,PF_ER,"),
        "{advice}"
    );
    let (s, paid) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/payroll-disbursements/{did}/mark-paid"),
        json!({"reference": "CHALLAN-EPF-2026-11"}),
        Some(ver(&dues_batch)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{paid}");
    let je = Uuid::parse_str(paid["journalEntryId"].as_str().unwrap()).unwrap();
    assert_eq!(journal(&pool, je).await, (dues, dues, 2));
    assert_eq!(
        entry_side(&pool, je, "payroll_deductions_payable").await,
        (dues, Decimal::ZERO)
    );
    assert_eq!(
        balance(&pool, inst, "payroll_deductions_payable").await,
        Decimal::ZERO,
        "statutory dues payable cleared"
    );
    let (s, list) = read(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/payroll-disbursements"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let (s, list) = read(&app, &admin, &create).await;
    assert_eq!(
        (s, list["items"].as_array().unwrap().len()),
        (StatusCode::OK, 2)
    );
}

#[tokio::test]
async fn await_decision_keeps_disbursement_batches_pending() {
    let (app, pool) = app(TENANT_B).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let Posted {
        setup,
        run,
        meera,
        kiran,
    } = posted_run(
        &app,
        &admin,
        &pool,
        TENANT_B,
        "Await Disbursement University",
    )
    .await;
    for (st, number) in [(&meera, "001234567890"), (&kiran, "009876543210")] {
        let (s, b) = put_bank(&app, &admin, st.staff_id.as_uuid(), number).await;
        assert_eq!(s, StatusCode::OK, "{b}");
    }
    let rid = Uuid::parse_str(&id(&run)).unwrap();
    let net = dec(&run["netPay"]);
    let inst = setup.institution().as_uuid();

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

    let first = pd::create_disbursement(&c, rid, PayrollPayoutKind::NetPay)
        .await
        .unwrap();
    assert!(first.created);
    let b = first.row;
    assert_eq!(b.status, PayrollDisbursementStatus::PendingApproval);
    assert!(b.process_instance_id.is_some());
    // Pending: no file, no payment.
    assert!(matches!(
        pd::generate_file(&c, b.id).await,
        Err(KernelError::Conflict(_))
    ));
    assert!(matches!(
        pd::mark_paid(&a, b.id, b.version, "UTR1", None).await,
        Err(KernelError::Conflict(_))
    ));
    // Rejection needs a reason, then frees the run for a new batch.
    assert!(matches!(
        pd::decide_disbursement(&a, b.id, b.version, false, None).await,
        Err(KernelError::Validation(_))
    ));
    let rejected = pd::decide_disbursement(&a, b.id, b.version, false, Some("wrong month"))
        .await
        .unwrap();
    assert_eq!(rejected.status, PayrollDisbursementStatus::Rejected);
    let second = pd::create_disbursement(&c, rid, PayrollPayoutKind::NetPay)
        .await
        .unwrap();
    assert!(second.created);
    assert_ne!(second.row.id, b.id);
    let b = pd::decide_disbursement(&a, second.row.id, second.row.version, true, None)
        .await
        .unwrap();
    assert_eq!(b.status, PayrollDisbursementStatus::Approved);
    assert!(matches!(
        pd::decide_disbursement(&a, b.id, b.version, true, None).await,
        Err(KernelError::Conflict(_))
    ));
    let f = pd::generate_file(&c, b.id).await.unwrap();
    assert!(f.content.starts_with("H,PD-"), "{}", f.content);
    let b = pd::get_disbursement(&st, t, b.id).await.unwrap();
    assert!(b.file_generated_at.is_some());

    // Two concurrent mark-paid commands: one wins, one is refused, one journal entry.
    let (x, y) = tokio::join!(
        pd::mark_paid(&a, b.id, b.version, "UTR-A", None),
        pd::mark_paid(&a, b.id, b.version, "UTR-B", None),
    );
    let oks = [x.is_ok(), y.is_ok()].iter().filter(|o| **o).count();
    assert_eq!(oks, 1, "exactly one mark-paid succeeds: {x:?} / {y:?}");
    for r in [&x, &y] {
        if let Err(e) = r {
            assert!(
                matches!(
                    e,
                    KernelError::Conflict(_) | KernelError::VersionConflict { .. }
                ),
                "{e:?}"
            );
        }
    }
    assert_eq!(payout_entries(&pool, b.id).await, 1);
    assert_eq!(balance(&pool, inst, "payroll_payable").await, Decimal::ZERO);
    let paid = pd::get_disbursement(&st, t, b.id).await.unwrap();
    assert_eq!(paid.status, PayrollDisbursementStatus::Paid);
    assert_eq!(paid.total_amount, net);
    // Paid again later → 409.
    assert!(matches!(
        pd::mark_paid(&a, b.id, paid.version, "UTR-C", None).await,
        Err(KernelError::Conflict(_))
    ));
}
