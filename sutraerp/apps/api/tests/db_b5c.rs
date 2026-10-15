//! Batch 5 slice 5C: the period guard (`shared::period::require_open`) refusing 5A/5B-shaped
//! postings into closed / locked / planned / uncovered periods (L4-F-051…055), financial year +
//! period lifecycle with the trial balance snapshot at close (F-030/031), chart of accounts
//! management, manual journals through the WorkflowPort (auto-approval **and**
//! `ApprovalMode::AwaitDecision` with maker-checker, F-025/026), manual journal reversal through
//! `shared::posting::reverse` (F-028), the receivables reconciliation (F-029), bank accounts
//! (F-056), gateway settlement to the bank and its return (F-057/061), bank statement import
//! (JSON lines) + reconciliation (F-058) and the cash position (F-059). Charges and payments are
//! seeded through `support::b5` (no dependency on 5A/5B); every test runs in its own tenant.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{NaiveDate, Utc};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b5::{amount, seed_actor};
use support::*;
use sutra_contracts::finance::events::JOURNAL_ENTRY_POSTED;
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::JournalEntryKind;
use sutra_finance::shared::approval::ApprovalMode;
use sutra_finance::shared::charge::{apply_settlement, load_charge, lock_charge};
use sutra_finance::shared::posting;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const T_PERIODS: &str = "0192f5a0-7c3e-7000-8000-0000000b5c01";
const T_JOURNALS: &str = "0192f5a0-7c3e-7000-8000-0000000b5c02";
const T_TREASURY: &str = "0192f5a0-7c3e-7000-8000-0000000b5c03";
const T_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b5c04";
const T_IMPORTED: &str = "0192f5a0-7c3e-7000-8000-0000000b5c05";

const F: &str = "/api/v1/finance";

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> Value {
    let (s, _, b) = send(
        app,
        write(who, method, &format!("{F}{path}"), body, if_match),
    )
    .await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, &format!("{F}{path}"))).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

async fn events(pool: &sqlx::PgPool, tenant: &str, suffix: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE tenant_id = $1::uuid AND event_type = $2",
    )
    .bind(tenant)
    .bind(format!("sutraerp.finance.{suffix}"))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Σ debit − Σ credit of a seeded system account.
async fn balance(pool: &sqlx::PgPool, setup: &b5::FinanceSetup, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(l.debit - l.credit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id WHERE a.institution_id = $1 AND a.system_key = $2",
    )
    .bind(setup.institution().as_uuid())
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The ledger of the institution is balanced overall.
async fn ledger_balanced(pool: &sqlx::PgPool, setup: &b5::FinanceSetup) {
    let (d, c): (Decimal, Decimal) = sqlx::query_as(
        "SELECT COALESCE(sum(l.debit), 0), COALESCE(sum(l.credit), 0) FROM finance.journal_line l \
         JOIN finance.journal_entry e ON e.id = l.journal_entry_id WHERE e.institution_id = $1",
    )
    .bind(setup.institution().as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(d, c, "trial balance");
}

/// A posting shaped exactly like `b5::seed_payment` (5B's counter receipt), dated `date`.
fn payment_shaped(
    setup: &b5::FinanceSetup,
    charge: ChargeId,
    paid: &str,
    date: NaiveDate,
) -> PostingRequest {
    PostingRequest {
        institution_id: setup.institution(),
        posting_date: date,
        kind: JournalEntryKind::Standard,
        source_type: source::PAYMENT.into(),
        source_id: Uuid::now_v7(),
        posting_key: format!("settle:{charge}"),
        description: "Payment".into(),
        currency: CurrencyCode::INR,
        lines: vec![
            PostingLine::debit(AccountRef::System(SystemAccount::BankMain), amount(paid)),
            PostingLine::credit(
                AccountRef::System(SystemAccount::StudentReceivables),
                amount(paid),
            )
            .for_charge(setup.student_account_id, Some(charge)),
        ],
    }
}

/// A posting shaped exactly like `b5::seed_charge`'s publication (5A), dated `date`.
fn publication_shaped(
    setup: &b5::FinanceSetup,
    charge: ChargeId,
    total: &str,
    date: NaiveDate,
) -> PostingRequest {
    PostingRequest {
        institution_id: setup.institution(),
        posting_date: date,
        kind: JournalEntryKind::Standard,
        source_type: source::CHARGE.into(),
        source_id: charge.as_uuid(),
        posting_key: format!("publish-again:{}", Uuid::now_v7()),
        description: "Charge published".into(),
        currency: CurrencyCode::INR,
        lines: vec![
            PostingLine::debit(
                AccountRef::System(SystemAccount::StudentReceivables),
                amount(total),
            )
            .for_charge(setup.student_account_id, Some(charge)),
            PostingLine::credit(AccountRef::System(SystemAccount::FeeIncome), amount(total)),
        ],
    }
}

/// A 5B-shaped settlement (apply_settlement + payment posting) in one transaction; returns the
/// posting result (the transaction is committed only on success).
async fn try_settle(
    pool: &sqlx::PgPool,
    setup: &b5::FinanceSetup,
    charge: ChargeId,
    paid: &str,
    date: NaiveDate,
) -> Result<Option<Uuid>, KernelError> {
    let mut tx = pool.begin().await.unwrap();
    let c = lock_charge(&mut tx, setup.tenant(), charge).await.unwrap();
    apply_settlement(
        &mut tx,
        &c,
        amount(paid),
        JOURNAL_ENTRY_POSTED,
        &seed_actor(),
    )
    .await?;
    let r = posting::post(
        &mut tx,
        setup.tenant(),
        &payment_shaped(setup, charge, paid, date),
        &seed_actor(),
    )
    .await?;
    tx.commit().await.unwrap();
    Ok(r.financial_period_id)
}

async fn try_post(
    pool: &sqlx::PgPool,
    setup: &b5::FinanceSetup,
    req: &PostingRequest,
) -> Result<Option<Uuid>, KernelError> {
    let mut tx = pool.begin().await.unwrap();
    let r = posting::post(&mut tx, setup.tenant(), req, &seed_actor()).await?;
    tx.commit().await.unwrap();
    Ok(r.financial_period_id)
}

fn period<'a>(year: &'a Value, name: &str) -> &'a Value {
    year["periods"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap_or_else(|| panic!("period {name} in {year}"))
}

// ------------------------------------------------------------------ F-051 … F-055 + the guard

#[tokio::test]
async fn period_guard_refuses_postings_into_closed_periods() {
    let (app, pool) = app(T_PERIODS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_PERIODS,
        "Periods University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();

    // No financial year configured: every date is open (foundation behaviour), no period id.
    let open_charge = b5::seed_charge(&pool, &setup, "125000.00", "published", None).await;
    let pid: Option<Uuid> =
        sqlx::query_scalar("SELECT financial_period_id FROM finance.journal_entry WHERE id = $1")
            .bind(open_charge.publication_entry.unwrap().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(pid.is_none());

    // ---- establish FY 2026-27 (Apr–Mar): 12 monthly periods, all planned.
    let body = json!({"code": "fy2026-27", "name": "Financial year 2026-27",
                      "startDate": "2026-04-01", "endDate": "2027-03-31"});
    let year = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/financial-years"),
        body.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(year["code"], "FY2026-27");
    assert_eq!(year["status"], "draft");
    assert_eq!(year["periods"].as_array().unwrap().len(), 12);
    assert_eq!(period(&year, "Jul 2026")["startDate"], "2026-07-01");
    assert_eq!(period(&year, "Feb 2027")["endDate"], "2027-02-28");
    // duplicates / overlaps / bad ranges
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/financial-years"),
        body,
        None,
        StatusCode::CONFLICT,
    )
    .await;
    call(&app, &admin, "POST", &format!("/institutions/{inst}/financial-years"),
        json!({"code": "FY-OVERLAP", "name": "x", "startDate": "2027-01-01", "endDate": "2027-12-31"}), None, StatusCode::CONFLICT).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/financial-years"),
        json!({"code": "FY-LONG", "name": "x", "startDate": "2028-01-01", "endDate": "2029-06-30"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let yid = id(&year);
    let jul = id(period(&year, "Jul 2026"));

    // ---- planned periods refuse postings (5A-shaped publication and 5B-shaped settlement) …
    let charge = b5::seed_charge(&pool, &setup, "50000.00", "draft", None).await;
    let err = try_post(
        &pool,
        &setup,
        &publication_shaped(&setup, open_charge.charge_id, "10.00", day(2026, 7, 10)),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, KernelError::Conflict(m) if m.contains("planned")),
        "{err:?}"
    );
    let err = try_settle(
        &pool,
        &setup,
        open_charge.charge_id,
        "1000.00",
        day(2026, 7, 10),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, KernelError::Conflict(m) if m.contains("refused")),
        "{err:?}"
    );
    // … and a date no period covers is refused once the institution has periods.
    let err = try_post(
        &pool,
        &setup,
        &publication_shaped(&setup, open_charge.charge_id, "10.00", day(2028, 1, 5)),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, KernelError::Conflict(m) if m.contains("no financial period")),
        "{err:?}"
    );
    // the business change rolled back with the refused posting
    let c = load_charge(
        &mut pool.acquire().await.unwrap(),
        setup.tenant(),
        open_charge.charge_id,
    )
    .await
    .unwrap();
    assert_eq!(c.settled_amount, Decimal::ZERO);
    let _ = charge;

    // ---- a period cannot open before its year; open the year and July.
    let p = read(&app, &admin, &format!("/financial-periods/{jul}")).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/open"),
        json!({}),
        Some(ver(&p)),
        StatusCode::CONFLICT,
    )
    .await;
    // If-Match is required
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{F}/financial-years/{yid}/open"),
            json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let y = call(
        &app,
        &admin,
        "POST",
        &format!("/financial-years/{yid}/open"),
        json!({}),
        Some(ver(&year)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(y["status"], "open");
    let p = call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/open"),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "open");
    assert_eq!(p["acceptsPostings"], true);
    assert_eq!(events(&pool, T_PERIODS, "financial_period.opened").await, 1);

    // ---- open: seed_charge / seed_payment (exact 5A/5B shapes) post and carry the period id.
    let jul_charge = b5::seed_charge(&pool, &setup, "80000.00", "published", None).await;
    let pid: Option<Uuid> =
        sqlx::query_scalar("SELECT financial_period_id FROM finance.journal_entry WHERE id = $1")
            .bind(jul_charge.publication_entry.unwrap().as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(pid.map(|u| u.to_string()), Some(jul.clone()));
    b5::seed_payment(&pool, &setup, jul_charge.charge_id, "30000.00").await;
    assert_eq!(
        try_settle(
            &pool,
            &setup,
            jul_charge.charge_id,
            "1000.00",
            day(2026, 7, 20)
        )
        .await
        .unwrap()
        .map(|u| u.to_string()),
        Some(jul.clone())
    );
    // August is still planned
    assert!(try_settle(
        &pool,
        &setup,
        jul_charge.charge_id,
        "1000.00",
        day(2026, 8, 3)
    )
    .await
    .is_err());

    // ---- lock July: postings refused; unlock (open) again works.
    let p = call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/lock"),
        json!({"reason": "close preparation"}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "locked");
    let err = try_settle(
        &pool,
        &setup,
        jul_charge.charge_id,
        "500.00",
        day(2026, 7, 21),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, KernelError::Conflict(m) if m.contains("locked")),
        "{err:?}"
    );

    // ---- close July (controlled close): trial balance snapshot + event; reopen needs a reason.
    let p = call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/close"),
        json!({"reason": "month end"}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "closed");
    assert_eq!(p["acceptsPostings"], false);
    assert!(p["entryCount"].as_i64().unwrap() >= 3, "{p}");
    let (tb_count, tb_balanced): (i64, bool) = sqlx::query_as(
        "SELECT count(*), bool_and(balanced) FROM finance.trial_balance WHERE financial_period_id = $1::uuid",
    )
    .bind(&jul)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((tb_count, tb_balanced), (1, true));
    assert_eq!(events(&pool, T_PERIODS, "financial_period.closed").await, 1);
    assert_eq!(events(&pool, T_PERIODS, "trial_balance.generated").await, 1);
    // the trial balance of the period end is available and balanced
    let tb = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/trial-balance?periodId={jul}"),
    )
    .await;
    assert_eq!(tb["asOf"], "2026-07-31");
    assert_eq!(tb["balanced"], true);

    // ---- THE contract scenario: postings into the closed period are refused, both shapes,
    // and the business change (settlement) is rolled back with them.
    let before = load_charge(
        &mut pool.acquire().await.unwrap(),
        setup.tenant(),
        jul_charge.charge_id,
    )
    .await
    .unwrap();
    let err = try_settle(
        &pool,
        &setup,
        jul_charge.charge_id,
        "5000.00",
        day(2026, 7, 25),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, KernelError::Conflict(m) if m.contains("closed") && m.contains("refused")),
        "{err:?}"
    );
    let err = try_post(
        &pool,
        &setup,
        &publication_shaped(&setup, jul_charge.charge_id, "100.00", day(2026, 7, 31)),
    )
    .await
    .unwrap_err();
    assert!(matches!(&err, KernelError::Conflict(_)), "{err:?}");
    let after = load_charge(
        &mut pool.acquire().await.unwrap(),
        setup.tenant(),
        jul_charge.charge_id,
    )
    .await
    .unwrap();
    assert_eq!(
        after.settled_amount, before.settled_amount,
        "settlement rolled back"
    );
    assert_eq!(after.version, before.version);
    // a manual journal into the closed period cannot be approved either (its posting is refused)
    let accounts = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/ledger-accounts"),
    )
    .await;
    let acc = |key: &str| {
        accounts["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["systemKey"] == key)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let mj = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-15", "description": "Back-dated reclass", "lines": [
            {"ledgerAccountId": acc("bank_charges"), "debit": "10.00"},
            {"ledgerAccountId": acc("bank_main"), "credit": "10.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/manual-journals/{}/submit", id(&mj)),
        json!({}),
        Some(ver(&mj)),
        StatusCode::CONFLICT,
    )
    .await;
    let mj = read(&app, &admin, &format!("/manual-journals/{}", id(&mj))).await;
    assert_eq!(mj["status"], "draft", "the whole submission rolled back");

    // ---- illegal transitions; the year cannot close while periods are open/planned.
    call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/lock"),
        json!({}),
        Some(ver(&p)),
        StatusCode::CONFLICT,
    )
    .await;
    let y = read(&app, &admin, &format!("/financial-years/{yid}")).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/financial-years/{yid}/close"),
        json!({}),
        Some(ver(&y)),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/reopen"),
        json!({}),
        Some(ver(&p)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // ---- reopen for a correction: postings are accepted again, the history shows the trail.
    let p = call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/reopen"),
        json!({"reason": "late bank receipt"}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "reopened");
    assert_eq!(p["reopenCount"], 1);
    assert!(try_settle(
        &pool,
        &setup,
        jul_charge.charge_id,
        "5000.00",
        day(2026, 7, 25)
    )
    .await
    .unwrap()
    .is_some());
    let p = call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/close"),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    let trail: Vec<&str> = p["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap())
        .collect();
    assert_eq!(
        trail,
        vec!["planned", "open", "locked", "closed", "reopened", "closed"]
    );
    assert_eq!(
        events(&pool, T_PERIODS, "financial_period.reopened").await,
        1
    );
    let p = call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/archive"),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "archived");
    call(
        &app,
        &admin,
        "POST",
        &format!("/financial-periods/{jul}/reopen"),
        json!({"reason": "x"}),
        Some(ver(&p)),
        StatusCode::CONFLICT,
    )
    .await;

    // ---- the student has no access to periods; foreign ids are 404.
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let (s, _, _) = send(
        &app,
        get(
            &student,
            &format!("{F}/institutions/{inst}/financial-years"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(
        &app,
        get(&admin, &format!("{F}/financial-periods/{}", Uuid::now_v7())),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    ledger_balanced(&pool, &setup).await;
}

// ------------------------------------------------------------------ F-025 … F-032 (auto-approval)

#[tokio::test]
async fn manual_journals_reversal_trial_balance_and_reconciliation() {
    let (app, pool) = app(T_JOURNALS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_JOURNALS,
        "Ledger University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    let charge = b5::seed_charge(&pool, &setup, "125000.00", "published", None).await;
    b5::seed_payment(&pool, &setup, charge.charge_id, "100000.00").await;

    // ---- chart of accounts: 14 seeded accounts with balances; a user account.
    let chart = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/ledger-accounts"),
    )
    .await;
    let items = chart["items"].as_array().unwrap();
    assert_eq!(items.len(), 14);
    let sys = |key: &str| {
        items
            .iter()
            .find(|a| a["systemKey"] == key)
            .unwrap()
            .clone()
    };
    assert_eq!(sys("student_receivables")["balance"], "25000.00");
    assert_eq!(sys("fee_income")["balance"], "125000.00");
    let supplies = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/ledger-accounts"),
        json!({"code": "5950", "name": "Office supplies", "accountType": "expense"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(supplies["normalBalance"], "debit");
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/ledger-accounts"),
        json!({"code": "5950", "name": "Dup", "accountType": "expense"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/ledger-accounts"),
        json!({"code": "bad code", "name": "x", "accountType": "expense"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // a system account with postings is never renamed / deactivated
    let ar = sys("student_receivables");
    call(
        &app,
        &admin,
        "PATCH",
        &format!("/ledger-accounts/{}", id(&ar)),
        json!({"status": "inactive"}),
        Some(ver(&ar)),
        StatusCode::CONFLICT,
    )
    .await;

    // ---- manual journal: unbalanced → 409, too few lines → 422; draft → submit → posted.
    let lines = |dr: &str, cr: &str| {
        json!([
        {"ledgerAccountId": id(&supplies), "debit": dr, "memo": "stationery"},
        {"ledgerAccountId": id(&sys("bank_main")), "credit": cr}])
    };
    call(&app, &admin, "POST", &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-10", "description": "Stationery", "lines": lines("500.00", "499.99")}), None, StatusCode::CONFLICT).await;
    call(&app, &admin, "POST", &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-10", "description": "Stationery", "lines": [{"ledgerAccountId": id(&supplies), "debit": "5.00"}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    call(&app, &admin, "POST", &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-10", "description": "Bad account", "lines": [
            {"ledgerAccountId": Uuid::now_v7(), "debit": "5.00"}, {"ledgerAccountId": id(&supplies), "credit": "5.00"}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let mj = call(&app, &admin, "POST", &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-10", "description": "Stationery", "lines": lines("400.00", "400.00")}), None, StatusCode::CREATED).await;
    assert_eq!(mj["status"], "draft");
    assert!(mj["journalNumber"].as_str().unwrap().starts_with("MJ-"));
    let mj = call(&app, &admin, "PUT", &format!("/manual-journals/{}", id(&mj)),
        json!({"postingDate": "2026-07-10", "description": "Stationery July", "lines": lines("500.00", "500.00")}), Some(ver(&mj)), StatusCode::OK).await;
    assert_eq!(mj["totalAmount"], "500.00");
    let posted = call(
        &app,
        &admin,
        "POST",
        &format!("/manual-journals/{}/submit", id(&mj)),
        json!({}),
        Some(ver(&mj)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(posted["status"], "posted");
    assert_eq!(posted["decidedBy"], "system:auto-approval");
    let trail: Vec<&str> = posted["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap())
        .collect();
    assert_eq!(
        trail,
        vec!["draft", "pending_approval", "approved", "posted"]
    );
    let entry_id = posted["journalEntryId"].as_str().unwrap().to_owned();
    let entry = read(&app, &admin, &format!("/journal-entries/{entry_id}")).await;
    assert_eq!(entry["kind"], "manual");
    assert_eq!(entry["sourceType"], "manual_journal");
    assert_eq!(entry["totalDebit"], "500.00");
    assert_eq!(entry["lines"][0]["accountCode"], "5950");
    assert_eq!(entry["lines"][0]["memo"], "stationery");
    assert_eq!(events(&pool, T_JOURNALS, "manual_journal.posted").await, 1);
    // posted journals are immutable; submitting again is illegal
    call(
        &app,
        &admin,
        "PUT",
        &format!("/manual-journals/{}", id(&mj)),
        json!({"postingDate": "2026-07-10", "description": "x", "lines": lines("1.00", "1.00")}),
        Some(ver(&posted)),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/manual-journals/{}/submit", id(&mj)),
        json!({}),
        Some(ver(&posted)),
        StatusCode::CONFLICT,
    )
    .await;
    // an account with a balance cannot be deactivated
    let s5950 = read(&app, &admin, &format!("/ledger-accounts/{}", id(&supplies))).await;
    assert_eq!(s5950["balance"], "500.00");
    call(
        &app,
        &admin,
        "PATCH",
        &format!("/ledger-accounts/{}", id(&supplies)),
        json!({"status": "inactive"}),
        Some(ver(&s5950)),
        StatusCode::CONFLICT,
    )
    .await;

    // ---- journal entry list: filters + cursor pagination.
    let page = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/journal-entries?limit=2"),
    )
    .await;
    assert_eq!(page["items"].as_array().unwrap().len(), 2);
    let cursor = page["nextCursor"].as_str().unwrap();
    let rest = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/journal-entries?limit=50&cursor={cursor}"),
    )
    .await;
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert!(rest["nextCursor"].is_null());
    let manual = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/journal-entries?sourceType=manual_journal"),
    )
    .await;
    assert_eq!(manual["items"].as_array().unwrap().len(), 1);

    // ---- trial balance: balanced, the user account shows up.
    let tb = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/trial-balance?asOf=2026-12-31"),
    )
    .await;
    assert_eq!(tb["balanced"], true);
    assert_eq!(tb["totalDebit"], "225500.00");
    let row = tb["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["code"] == "5950")
        .unwrap();
    assert_eq!(row["balance"], "500.00");
    let early = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/trial-balance?asOf=2026-07-01"),
    )
    .await;
    assert_eq!(early["totalDebit"], "125000.00");

    // ---- reconciliation: receivables agree; a receivable touched outside a charge is caught.
    let rec = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/reconciliations"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(rec["status"], "matched", "{rec}");
    assert_eq!(rec["sourceTotal"], "25000.00");
    assert_eq!(rec["ledgerTotal"], "25000.00");
    let stray = call(&app, &admin, "POST", &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-11", "description": "Misposted receivable", "lines": [
            {"ledgerAccountId": id(&ar), "debit": "300.00", "studentAccountId": setup.student_account_id},
            {"ledgerAccountId": id(&sys("other_income")), "credit": "300.00"}]}), None, StatusCode::CREATED).await;
    let stray = call(
        &app,
        &admin,
        "POST",
        &format!("/manual-journals/{}/submit", id(&stray)),
        json!({}),
        Some(ver(&stray)),
        StatusCode::OK,
    )
    .await;
    let rec = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/reconciliations"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(rec["status"], "exceptions");
    assert_eq!(rec["difference"], "-300.00");
    assert_eq!(
        events(&pool, T_JOURNALS, "ledger_reconciliation.completed").await,
        2
    );
    let list = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/reconciliations"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    read(&app, &admin, &format!("/reconciliations/{}", id(&rec))).await;

    // ---- reversal (F-028): mirror entry via shared::posting::reverse; once only.
    let stray_entry = stray["journalEntryId"].as_str().unwrap().to_owned();
    let rev = call(
        &app,
        &admin,
        "POST",
        &format!("/journal-entries/{stray_entry}/reverse"),
        json!({"reason": "misposting", "postingDate": "2026-07-12"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(rev["kind"], "reversal");
    assert_eq!(rev["reversesEntryId"], stray_entry.as_str());
    assert_eq!(rev["lines"][0]["credit"], "300.00");
    call(
        &app,
        &admin,
        "POST",
        &format!("/journal-entries/{stray_entry}/reverse"),
        json!({"reason": "again"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/journal-entries/{}/reverse", id(&rev)),
        json!({"reason": "x"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let stray = read(&app, &admin, &format!("/manual-journals/{}", id(&stray))).await;
    assert_eq!(stray["status"], "reversed");
    assert_eq!(stray["reversalEntryId"], id(&rev).as_str());
    let e = read(&app, &admin, &format!("/journal-entries/{stray_entry}")).await;
    assert_eq!(e["reversedByEntryId"], id(&rev).as_str());
    // business-source entries are reversed by their owners, never here
    call(
        &app,
        &admin,
        "POST",
        &format!(
            "/journal-entries/{}/reverse",
            charge.publication_entry.unwrap()
        ),
        json!({"reason": "nope"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let rec = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/reconciliations"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(rec["status"], "matched");

    // ---- reverse the stationery journal → zero balance → the user account may be deactivated;
    // an inactive account refuses new journals.
    call(
        &app,
        &admin,
        "POST",
        &format!("/journal-entries/{entry_id}/reverse"),
        json!({"reason": "returned"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let s5950 = read(&app, &admin, &format!("/ledger-accounts/{}", id(&supplies))).await;
    assert_eq!(s5950["balance"], "0.00");
    let s5950 = call(
        &app,
        &admin,
        "PATCH",
        &format!("/ledger-accounts/{}", id(&supplies)),
        json!({"status": "inactive"}),
        Some(ver(&s5950)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s5950["status"], "inactive");
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-10", "description": "x", "lines": lines("1.00", "1.00")}),
        None,
        StatusCode::CONFLICT,
    )
    .await;

    // ---- permissions: students can't read the ledger.
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let (s, _, _) = send(
        &app,
        get(&student, &format!("{F}/institutions/{inst}/trial-balance")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    ledger_balanced(&pool, &setup).await;
}

// ------------------------------------------------------------------ F-056 … F-061

/// A 5B-shaped gateway capture (`payment` / `capture`: Dr gateway clearing · Cr receivables)
/// with its settlement on the charge; returns the payment id.
async fn seed_gateway_capture(
    pool: &sqlx::PgPool,
    setup: &b5::FinanceSetup,
    charge: ChargeId,
    paid: &str,
) -> Uuid {
    let pid = Uuid::now_v7();
    let mut tx = pool.begin().await.unwrap();
    let c = lock_charge(&mut tx, setup.tenant(), charge).await.unwrap();
    apply_settlement(
        &mut tx,
        &c,
        amount(paid),
        JOURNAL_ENTRY_POSTED,
        &seed_actor(),
    )
    .await
    .unwrap();
    posting::post(
        &mut tx,
        setup.tenant(),
        &PostingRequest {
            institution_id: setup.institution(),
            posting_date: day(2026, 7, 2),
            kind: JournalEntryKind::Standard,
            source_type: source::PAYMENT.into(),
            source_id: pid,
            posting_key: "capture".into(),
            description: "Gateway payment captured".into(),
            currency: CurrencyCode::INR,
            lines: vec![
                PostingLine::debit(
                    AccountRef::System(SystemAccount::GatewayClearing),
                    amount(paid),
                ),
                PostingLine::credit(
                    AccountRef::System(SystemAccount::StudentReceivables),
                    amount(paid),
                )
                .for_charge(setup.student_account_id, Some(charge)),
            ],
        },
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    pid
}

#[tokio::test]
async fn treasury_settlement_statement_reconciliation_and_returns() {
    let (app, pool) = app(T_TREASURY).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_TREASURY,
        "Treasury University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    let charge = b5::seed_charge(&pool, &setup, "125000.00", "published", None).await;
    let p1 = seed_gateway_capture(&pool, &setup, charge.charge_id, "60000.00").await;
    let p2 = seed_gateway_capture(&pool, &setup, charge.charge_id, "40000.00").await;
    let p3 = seed_gateway_capture(&pool, &setup, charge.charge_id, "1000.00").await;
    assert_eq!(
        balance(&pool, &setup, "gateway_clearing").await,
        amount("101000.00")
    );

    // ---- bank accounts (F-056): validation, auto-approval → active, ledger mapping.
    call(&app, &admin, "POST", &format!("/institutions/{inst}/bank-accounts"),
        json!({"name": "Main", "bankName": "HDFC Bank", "accountNumberLast4": "1234", "ifsc": "bad"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let main = call(&app, &admin, "POST", &format!("/institutions/{inst}/bank-accounts"),
        json!({"name": "Main collections", "bankName": "HDFC Bank", "accountNumberLast4": "1234", "ifsc": "HDFC0001234"}), None, StatusCode::CREATED).await;
    assert_eq!(main["status"], "active");
    assert_eq!(main["ledgerAccountCode"], "1200");
    let trail: Vec<&str> = main["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap())
        .collect();
    assert_eq!(trail, vec!["requested", "approved", "active"]);
    let ops = call(&app, &admin, "POST", &format!("/institutions/{inst}/bank-accounts"),
        json!({"name": "Operating", "bankName": "SBI", "accountNumberLast4": "9876", "ifsc": "SBIN0004321", "purpose": "operating"}), None, StatusCode::CREATED).await;
    assert_eq!(ops["ledgerAccountCode"], "1201");
    call(&app, &admin, "POST", &format!("/institutions/{inst}/bank-accounts"),
        json!({"name": "Dup map", "bankName": "SBI", "accountNumberLast4": "1111", "ifsc": "SBIN0004321",
               "ledgerAccountId": main["ledgerAccountId"]}), None, StatusCode::CONFLICT).await;
    assert_eq!(events(&pool, T_TREASURY, "bank_account.activated").await, 2);
    let mid = id(&main);

    // ---- gateway settlement to the bank (payment / settle): Dr bank + charges · Cr clearing.
    let settle = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/settlements"),
        json!({"reference": "SETL-0001", "valueDate": "2026-07-05",
               "items": [{"paymentId": p1, "fee": "10.00"}, {"paymentId": p2, "fee": "10.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(settle["amount"], "99980.00");
    assert_eq!(settle["feeAmount"], "20.00");
    assert_eq!(settle["status"], "executed");
    assert_eq!(
        balance(&pool, &setup, "gateway_clearing").await,
        amount("1000.00")
    );
    assert_eq!(
        balance(&pool, &setup, "bank_main").await,
        amount("99980.00")
    );
    assert_eq!(
        balance(&pool, &setup, "bank_charges").await,
        amount("20.00")
    );
    let (n, settles): (i64, i64) = sqlx::query_as(
        "SELECT count(*), count(*) FILTER (WHERE posting_key = 'settle:' || $2) FROM finance.journal_entry \
         WHERE source_type = 'payment' AND source_id = ANY($1)",
    )
    .bind(vec![p1, p2])
    .bind(id(&settle))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((n, settles), (4, 2), "capture + settle per payment");
    // never twice; unknown / uncaptured payments; fee ≥ gross
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/settlements"),
        json!({"reference": "SETL-0009", "valueDate": "2026-07-05", "items": [{"paymentId": p1}]}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/settlements"),
        json!({"reference": "SETL-0001", "valueDate": "2026-07-05", "items": [{"paymentId": p3}]}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    call(&app, &admin, "POST", &format!("/bank-accounts/{mid}/settlements"),
        json!({"reference": "SETL-0010", "valueDate": "2026-07-05", "items": [{"paymentId": Uuid::now_v7()}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    call(&app, &admin, "POST", &format!("/bank-accounts/{mid}/settlements"),
        json!({"reference": "SETL-0011", "valueDate": "2026-07-05", "items": [{"paymentId": p3, "fee": "1000.00"}]}), None, StatusCode::UNPROCESSABLE_ENTITY).await;

    // ---- a settlement the bank returns (F-061): reversed, back on clearing, settle again.
    let s2 = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/settlements"),
        json!({"reference": "SETL-0002", "valueDate": "2026-07-06", "items": [{"paymentId": p3}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        balance(&pool, &setup, "gateway_clearing").await,
        Decimal::ZERO
    );
    let s2 = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-transactions/{}/return", id(&s2)),
        json!({"reason": "beneficiary account mismatch", "postingDate": "2026-07-08"}),
        Some(ver(&s2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s2["status"], "returned");
    assert!(s2["items"][0]["reversalEntryId"].is_string());
    assert_eq!(
        balance(&pool, &setup, "gateway_clearing").await,
        amount("1000.00")
    );
    assert_eq!(
        events(&pool, T_TREASURY, "bank_transaction.failed").await,
        1
    );
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-transactions/{}/return", id(&s2)),
        json!({"reason": "again"}),
        Some(ver(&s2)),
        StatusCode::CONFLICT,
    )
    .await;
    let s3 = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/settlements"),
        json!({"reference": "SETL-0003", "valueDate": "2026-07-09", "items": [{"paymentId": p3}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let txns = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/bank-transactions"),
    )
    .await;
    assert_eq!(txns["items"].as_array().unwrap().len(), 3);

    // ---- statement import (JSON lines) (F-058): validation, balance agreement, duplicates.
    let content = [
        r#"{"valueDate":"2026-07-05","direction":"credit","amount":"99980.00","reference":"SETL-0001","description":"RAZORPAY SETTLEMENT"}"#,
        r#"{"valueDate":"2026-07-07","direction":"debit","amount":"59.00","description":"SMS alert charges"}"#,
        r#"{"valueDate":"2026-07-10","direction":"credit","amount":"1000.00","reference":"SETL-0003"}"#,
        r#"{"valueDate":"2026-07-12","direction":"credit","amount":"5000.00","reference":"NEFT-SPONSOR-77"}"#,
    ]
    .join("\n");
    call(&app, &admin, "POST", &format!("/bank-accounts/{mid}/statements"),
        json!({"statementRef": "JUL-2026", "periodFrom": "2026-07-01", "periodTo": "2026-07-31",
               "content": "{\"valueDate\":\"2026-07-05\",\"direction\":\"credit\",\"amount\":99980}"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/statements"),
        json!({"statementRef": "JUL-2026", "periodFrom": "2026-07-01", "periodTo": "2026-07-31",
               "openingBalance": "0.00", "closingBalance": "1.00", "content": content}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let stmt = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/statements"),
        json!({"statementRef": "JUL-2026", "periodFrom": "2026-07-01", "periodTo": "2026-07-31",
               "openingBalance": "0.00", "closingBalance": "105921.00", "content": content}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(stmt["lineCount"], 4);
    assert_eq!(stmt["totalCredits"], "105980.00");
    assert_eq!(stmt["status"], "imported");
    call(&app, &admin, "POST", &format!("/bank-accounts/{mid}/statements"),
        json!({"statementRef": "JUL-2026-B", "periodFrom": "2026-07-01", "periodTo": "2026-07-31", "content": content}), None, StatusCode::CONFLICT).await;

    // ---- first reconciliation: both settlements auto-match their bank transactions; the
    // sponsor credit and the bank charge are exceptions.
    let r1 = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-statements/{}/reconcile", id(&stmt)),
        json!({}),
        Some(ver(&stmt)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r1["status"], "reconciling");
    let line = |v: &Value, n: i64| {
        v["lines"]
            .as_array()
            .unwrap()
            .iter()
            .find(|l| l["lineNo"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(line(&r1, 1)["matchStatus"], "matched");
    assert_eq!(line(&r1, 1)["matchedTransactionId"], id(&settle).as_str());
    assert_eq!(line(&r1, 3)["matchedTransactionId"], id(&s3).as_str());
    assert_eq!(line(&r1, 2)["matchStatus"], "unmatched");
    assert_eq!(line(&r1, 4)["matchStatus"], "unmatched");
    assert_eq!(r1["reconciliations"][0]["unmatchedLines"], 2);
    // a matched settlement cannot be returned any more
    let s3v = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/bank-transactions"),
    )
    .await;
    let s3row = s3v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == id(&s3).as_str())
        .unwrap()
        .clone();
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-transactions/{}/return", id(&s3)),
        json!({"reason": "x"}),
        Some(ver(&s3row)),
        StatusCode::CONFLICT,
    )
    .await;

    // ---- explain the exceptions: post the bank charge and the sponsor receipt as manual
    // journals; the reconciliation auto-matches them (reference in the memo), then it closes.
    let chart = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/ledger-accounts"),
    )
    .await;
    let acc = |key: &str| {
        chart["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["systemKey"] == key)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    for (date, dr, cr, amt, memo) in [
        (
            "2026-07-07",
            acc("bank_charges"),
            acc("bank_main"),
            "59.00",
            "SMS alert charges",
        ),
        (
            "2026-07-12",
            acc("bank_main"),
            acc("unapplied_receipts"),
            "5000.00",
            "NEFT-SPONSOR-77",
        ),
    ] {
        let mj = call(&app, &admin, "POST", &format!("/institutions/{inst}/manual-journals"),
            json!({"postingDate": date, "description": memo, "lines": [
                {"ledgerAccountId": dr, "debit": amt, "memo": memo}, {"ledgerAccountId": cr, "credit": amt, "memo": memo}]}), None, StatusCode::CREATED).await;
        let mj = call(
            &app,
            &admin,
            "POST",
            &format!("/manual-journals/{}/submit", id(&mj)),
            json!({}),
            Some(ver(&mj)),
            StatusCode::OK,
        )
        .await;
        assert_eq!(mj["status"], "posted");
    }
    let r2 = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-statements/{}/reconcile", id(&stmt)),
        json!({}),
        Some(ver(&r1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r2["status"], "reconciled", "{r2}");
    assert!(line(&r2, 4)["matchedEntryNumber"]
        .as_str()
        .unwrap()
        .starts_with("JE-"));
    assert_eq!(r2["reconciliations"][0]["ledgerBalance"], "105921.00");
    assert_eq!(r2["reconciliations"][0]["statementClosing"], "105921.00");
    assert_eq!(
        events(&pool, T_TREASURY, "bank_statement.reconciled").await,
        1
    );
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-statements/{}/reconcile", id(&stmt)),
        json!({}),
        Some(ver(&r2)),
        StatusCode::CONFLICT,
    )
    .await;

    // ---- a second statement with a manual match + an ignored line.
    let aug = call(&app, &admin, "POST", &format!("/bank-accounts/{mid}/statements"),
        json!({"statementRef": "AUG-2026", "periodFrom": "2026-08-01", "periodTo": "2026-08-31",
               "content": "{\"valueDate\":\"2026-08-02\",\"direction\":\"credit\",\"amount\":\"10.00\",\"description\":\"interest\"}\n{\"valueDate\":\"2026-08-03\",\"direction\":\"debit\",\"amount\":\"10.00\",\"description\":\"interest reversal\"}"}), None, StatusCode::CREATED).await;
    let l1 = line(&aug, 1)["id"].as_str().unwrap().to_owned();
    let l2 = line(&aug, 2)["id"].as_str().unwrap().to_owned();
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-statements/{}/reconcile", id(&aug)),
        json!({"matches": [{"lineId": l1, "bankTransactionId": id(&settle)}]}),
        Some(ver(&aug)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let a2 = call(&app, &admin, "POST", &format!("/bank-statements/{}/reconcile", id(&aug)),
        json!({"autoMatch": false, "ignore": [{"lineId": l1, "note": "bank-side pair"}, {"lineId": l2, "note": "bank-side pair"}]}), Some(ver(&aug)), StatusCode::OK).await;
    assert_eq!(a2["status"], "reconciled");
    assert_eq!(line(&a2, 1)["matchStatus"], "ignored");

    // ---- cash position (F-059): banks + cash + in transit, straight from the ledger.
    let cash = read(&app, &admin, &format!("/institutions/{inst}/cash-position")).await;
    let main_row = cash["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["bankAccountId"] == mid.as_str())
        .unwrap();
    assert_eq!(main_row["balance"], "105921.00");
    assert_eq!(main_row["unreconciledLines"], 0);
    assert_eq!(cash["inTransit"], "0.00");
    assert_eq!(cash["available"], "105921.00");

    // ---- suspend / close rules.
    let ops = read(&app, &admin, &format!("/bank-accounts/{}", id(&ops))).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{}/suspend", id(&ops)),
        json!({}),
        Some(ver(&ops)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let ops = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{}/suspend", id(&ops)),
        json!({"reason": "KYC refresh"}),
        Some(ver(&ops)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ops["status"], "suspended");
    call(&app, &admin, "POST", &format!("/bank-accounts/{}/statements", id(&ops)),
        json!({"statementRef": "X", "periodFrom": "2026-08-01", "periodTo": "2026-08-31", "content": "{\"valueDate\":\"2026-08-02\",\"direction\":\"credit\",\"amount\":\"1.00\"}"}), None, StatusCode::CONFLICT).await;
    let ops = call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{}/close", id(&ops)),
        json!({}),
        Some(ver(&ops)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ops["status"], "closed");
    let main = read(&app, &admin, &format!("/bank-accounts/{mid}")).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/bank-accounts/{mid}/close"),
        json!({}),
        Some(ver(&main)),
        StatusCode::CONFLICT,
    )
    .await;
    ledger_balanced(&pool, &setup).await;
}

// ------------------------------------------------------------------ AwaitDecision

fn auth(subject: &str, tenant: TenantId) -> AuthContext {
    AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![Role::Administrator],
        scope: ExecutionScope::tenant(tenant),
        csrf_token: "a".repeat(64),
        expires_at: Utc::now(),
        idle_expires_at: Utc::now(),
        mfa_level: MfaLevel::None,
        amr: vec!["pwd".into()],
        step_up_enforced: false,
        privileged_mfa_enforced: false,
    }
}

/// The finance router alone (no session middleware) with `ApprovalMode::AwaitDecision` and a
/// fixed caller.
fn await_app(pool: &sqlx::PgPool, ctx: AuthContext) -> Router {
    let mut st = sutra_api::app::finance_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let (router, _) = sutra_finance::router_with_state(st).split_for_parts();
    router.layer(axum::Extension(ctx))
}

async fn direct(
    app: &Router,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> Value {
    let mut b = Request::builder()
        .method(method)
        .uri(format!("{F}{path}"))
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::now_v7().to_string());
    if let Some(v) = if_match {
        b = b.header("if-match", format!("\"{v}\""));
    }
    let (s, _, v) = send(app, b.body(Body::from(body.to_string())).unwrap()).await;
    assert_eq!(s, expect, "{method} {path}: {v}");
    v
}

/// Manual journal approval and bank account approval stay pending under `AwaitDecision` until a
/// *second* administrator decides (maker-checker); rejection needs a reason and posts nothing;
/// the period close waits for pending journals of the period.
#[tokio::test]
async fn approvals_await_a_decision_with_maker_checker() {
    let (app, pool) = app(T_AWAIT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_AWAIT,
        "Await University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    let t = setup.tenant();
    let maker = await_app(&pool, auth("staff|maker", t));
    let checker = await_app(&pool, auth("staff|checker", t));

    // A year with July open, so the period close can be exercised.
    let year = direct(&maker, "POST", &format!("/institutions/{inst}/financial-years"),
        json!({"code": "FY26", "name": "FY 2026-27", "startDate": "2026-04-01", "endDate": "2027-03-31"}), None, StatusCode::CREATED).await;
    let year = direct(
        &maker,
        "POST",
        &format!("/financial-years/{}/open", id(&year)),
        json!({}),
        Some(ver(&year)),
        StatusCode::OK,
    )
    .await;
    let jul = period(&year, "Jul 2026").clone();
    let jul = direct(
        &maker,
        "POST",
        &format!("/financial-periods/{}/open", id(&jul)),
        json!({}),
        Some(ver(&jul)),
        StatusCode::OK,
    )
    .await;

    let chart = direct(
        &maker,
        "GET",
        &format!("/institutions/{inst}/ledger-accounts"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let acc = |key: &str| {
        chart["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["systemKey"] == key)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let mj = direct(&maker, "POST", &format!("/institutions/{inst}/manual-journals"),
        json!({"postingDate": "2026-07-20", "entryKind": "adjusting", "description": "Accrued bank charges", "lines": [
            {"ledgerAccountId": acc("bank_charges"), "debit": "250.00"}, {"ledgerAccountId": acc("bank_main"), "credit": "250.00"}]}), None, StatusCode::CREATED).await;
    let mj = direct(
        &maker,
        "POST",
        &format!("/manual-journals/{}/submit", id(&mj)),
        json!({"reason": "month-end accrual"}),
        Some(ver(&mj)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(mj["status"], "pending_approval");
    assert!(mj["journalEntryId"].is_null());
    assert!(mj["processInstanceId"].is_string());
    let submitted: Value = sqlx::query_scalar(
        "SELECT envelope -> 'data' FROM platform.outbox WHERE tenant_id = $1::uuid AND event_type = 'sutraerp.finance.manual_journal.submitted'",
    )
    .bind(T_AWAIT)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(submitted["toStatus"], "pending_approval");

    // the period cannot close while the journal awaits its decision
    let lock = direct(
        &maker,
        "POST",
        &format!("/financial-periods/{}/lock", id(&jul)),
        json!({}),
        Some(ver(&jul)),
        StatusCode::OK,
    )
    .await;
    direct(
        &maker,
        "POST",
        &format!("/financial-periods/{}/close", id(&jul)),
        json!({}),
        Some(ver(&lock)),
        StatusCode::CONFLICT,
    )
    .await;
    let jul = direct(
        &maker,
        "POST",
        &format!("/financial-periods/{}/open", id(&jul)),
        json!({}),
        Some(ver(&lock)),
        StatusCode::OK,
    )
    .await;

    // maker-checker; rejection needs a reason; reject → nothing posted → revise → resubmit.
    direct(
        &maker,
        "POST",
        &format!("/manual-journals/{}/decision", id(&mj)),
        json!({"decision": "approve"}),
        Some(ver(&mj)),
        StatusCode::FORBIDDEN,
    )
    .await;
    direct(
        &checker,
        "POST",
        &format!("/manual-journals/{}/decision", id(&mj)),
        json!({"decision": "reject"}),
        Some(ver(&mj)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    direct(
        &checker,
        "POST",
        &format!("/manual-journals/{}/decision", id(&mj)),
        json!({"decision": "maybe"}),
        Some(ver(&mj)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let rejected = direct(
        &checker,
        "POST",
        &format!("/manual-journals/{}/decision", id(&mj)),
        json!({"decision": "reject", "reason": "amount not evidenced"}),
        Some(ver(&mj)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rejected["status"], "rejected");
    assert_eq!(rejected["decidedBy"], "staff|checker");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM finance.journal_entry WHERE source_type = 'manual_journal' AND source_id = $1::uuid")
        .bind(id(&mj))
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
    let revised = direct(&maker, "PUT", &format!("/manual-journals/{}", id(&mj)),
        json!({"postingDate": "2026-07-20", "entryKind": "adjusting", "description": "Accrued bank charges (evidenced)", "lines": [
            {"ledgerAccountId": acc("bank_charges"), "debit": "236.00"}, {"ledgerAccountId": acc("bank_main"), "credit": "236.00"}]}), Some(ver(&rejected)), StatusCode::OK).await;
    assert_eq!(revised["status"], "draft");
    let pending = direct(
        &maker,
        "POST",
        &format!("/manual-journals/{}/submit", id(&mj)),
        json!({}),
        Some(ver(&revised)),
        StatusCode::OK,
    )
    .await;
    direct(
        &checker,
        "POST",
        &format!("/manual-journals/{}/decision", id(&mj)),
        json!({"decision": "approve"}),
        Some(ver(&pending) - 1),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    let posted = direct(
        &checker,
        "POST",
        &format!("/manual-journals/{}/decision", id(&mj)),
        json!({"decision": "approve", "reason": "ok"}),
        Some(ver(&pending)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(posted["status"], "posted");
    assert_eq!(posted["decidedBy"], "staff|checker");
    let entry = direct(
        &checker,
        "GET",
        &format!(
            "/journal-entries/{}",
            posted["journalEntryId"].as_str().unwrap()
        ),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(entry["kind"], "adjusting");
    assert_eq!(entry["financialPeriodId"], id(&jul).as_str());
    assert_eq!(entry["postedBy"], "staff|checker");
    // now the period closes
    let lock = direct(
        &maker,
        "POST",
        &format!("/financial-periods/{}/lock", id(&jul)),
        json!({}),
        Some(ver(&jul)),
        StatusCode::OK,
    )
    .await;
    let closed = direct(
        &checker,
        "POST",
        &format!("/financial-periods/{}/close", id(&jul)),
        json!({}),
        Some(ver(&lock)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(closed["closedBy"], "staff|checker");

    // bank account approval: requested until a second administrator decides.
    let bank = direct(&maker, "POST", &format!("/institutions/{inst}/bank-accounts"),
        json!({"name": "Main", "bankName": "Axis", "accountNumberLast4": "4321", "ifsc": "UTIB0000001"}), None, StatusCode::CREATED).await;
    assert_eq!(bank["status"], "requested");
    direct(&maker, "POST", &format!("/bank-accounts/{}/settlements", id(&bank)),
        json!({"reference": "R", "valueDate": "2026-07-05", "items": [{"paymentId": Uuid::now_v7()}]}), None, StatusCode::CONFLICT).await;
    direct(
        &maker,
        "POST",
        &format!("/bank-accounts/{}/decision", id(&bank)),
        json!({"decision": "approve"}),
        Some(ver(&bank)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let bank = direct(
        &checker,
        "POST",
        &format!("/bank-accounts/{}/decision", id(&bank)),
        json!({"decision": "approve"}),
        Some(ver(&bank)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(bank["status"], "active");
    ledger_balanced(&pool, &setup).await;
}

// ------------------------------------------------------------------ chart imported with other codes

/// A chart of accounts inserted outside Finance (a migration import, the re-skin demo seed) may
/// number the system accounts differently from `DEFAULT_CHART` (bank 1100 / cash 1200 / gateway
/// 1150). Reading the chart must provision only the roles that are still missing instead of
/// answering 409 on the `(institution, system_key)` unique index; it stays idempotent.
#[tokio::test]
async fn chart_listing_keeps_system_accounts_imported_under_other_codes() {
    let (app, pool) = app(T_IMPORTED).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let delivery =
        b4::academic_setup(&app, &admin, &pool, T_IMPORTED, "Imported Chart College").await;
    let (t, inst) = (delivery.tenant(), delivery.institution());
    sqlx::query("UPDATE institution.institution SET status = 'active' WHERE id = $1")
        .bind(inst.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    for (key, code, name, ty, normal) in [
        ("bank_main", "1100", "Main bank account", "asset", "debit"),
        (
            "gateway_clearing",
            "1150",
            "Payment gateway clearing",
            "asset",
            "debit",
        ),
        ("cash_on_hand", "1200", "Cash on hand", "asset", "debit"),
        (
            "fee_income",
            "4100",
            "Tuition and academic fee income",
            "income",
            "credit",
        ),
    ] {
        sqlx::query(
            "INSERT INTO finance.ledger_account (tenant_id, institution_id, code, name, account_type, \
             normal_balance, system_key) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(t.as_uuid())
        .bind(inst.as_uuid())
        .bind(code)
        .bind(name)
        .bind(ty)
        .bind(normal)
        .bind(key)
        .execute(&pool)
        .await
        .unwrap();
    }
    let path = format!("/institutions/{}/ledger-accounts", inst.as_uuid());
    let first = read(&app, &admin, &path).await;
    let again = read(&app, &admin, &path).await;
    assert_eq!(
        first["items"], again["items"],
        "listing the chart twice changes nothing"
    );
    let items = first["items"].as_array().unwrap();
    let code_of = |key: &str| {
        let hits: Vec<&Value> = items.iter().filter(|a| a["systemKey"] == key).collect();
        assert_eq!(hits.len(), 1, "exactly one {key} account");
        hits[0]["code"].as_str().unwrap().to_owned()
    };
    // The imported numbering is kept …
    assert_eq!(code_of("bank_main"), "1100");
    assert_eq!(code_of("gateway_clearing"), "1150");
    assert_eq!(code_of("cash_on_hand"), "1200");
    // … and the roles that were missing are provisioned under their default codes.
    assert_eq!(code_of("student_receivables"), "1300");
    assert_eq!(code_of("bank_charges"), "5900");
    let system = items.iter().filter(|a| a["systemKey"].is_string()).count();
    assert_eq!(system, 14, "every system role exists once");
}
