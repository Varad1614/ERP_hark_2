//! Batch 5 slice 5B: payments → allocation + receipt + posting (L4-F-007/008), unidentified
//! receipts (F-009), gateway failure / timeout recovery with the signed webhook (F-010),
//! reversal / chargeback (F-011), cancellation (F-012), bulk collection (F-013), the refund
//! reference approval and its lifecycle (F-014 … F-019), receivables: recognition, dunning,
//! settlement, write-off decision, reopening (F-039 … F-043) and `ApprovalMode::AwaitDecision`.
//! Charges are seeded through `support::b5::seed_charge` (no dependency on 5A); every test runs
//! in its own tenant.
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
use support::b5::amount;
use support::*;
use sutra_finance::shared::approval::ApprovalMode;
use sutra_finance::MockPaymentGateway;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const T_PAY: &str = "0192f5a0-7c3e-7000-8000-0000000b5b01";
const T_GATEWAY: &str = "0192f5a0-7c3e-7000-8000-0000000b5b02";
const T_UNIDENTIFIED: &str = "0192f5a0-7c3e-7000-8000-0000000b5b03";
const T_REFUND: &str = "0192f5a0-7c3e-7000-8000-0000000b5b04";
const T_RECEIVABLE: &str = "0192f5a0-7c3e-7000-8000-0000000b5b05";
const T_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b5b06";
const T_LATE_FEE: &str = "0192f5a0-7c3e-7000-8000-0000000b5b07";

const F: &str = "/api/v1/finance";

fn day(m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, m, d).unwrap()
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

/// Σ debit − Σ credit of a seeded system account (optionally for one party).
async fn balance(pool: &sqlx::PgPool, setup: &b5::FinanceSetup, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(COALESCE(l.debit, 0) - COALESCE(l.credit, 0)), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id WHERE a.institution_id = $1 AND a.system_key = $2",
    )
    .bind(setup.institution().as_uuid())
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// The ledger is balanced overall and the receivable control account equals Σ charge outstanding.
async fn ledger_agrees(pool: &sqlx::PgPool, setup: &b5::FinanceSetup) {
    let (d, c): (Decimal, Decimal) = sqlx::query_as(
        "SELECT COALESCE(sum(l.debit), 0), COALESCE(sum(l.credit), 0) FROM finance.journal_line l \
         JOIN finance.journal_entry e ON e.id = l.journal_entry_id WHERE e.institution_id = $1",
    )
    .bind(setup.institution().as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(d, c, "trial balance");
    let outstanding: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(outstanding_amount), 0) FROM finance.charge WHERE institution_id = $1 \
         AND status IN ('published', 'active', 'disputed', 'closed')",
    )
    .bind(setup.institution().as_uuid())
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        balance(pool, setup, "student_receivables").await,
        outstanding,
        "AR control = Σ outstanding"
    );
}

async fn outstanding(
    pool: &sqlx::PgPool,
    charge: sutra_contracts::finance::ids::ChargeId,
) -> (String, Decimal) {
    sqlx::query_as("SELECT status, outstanding_amount FROM finance.charge WHERE id = $1")
        .bind(charge.as_uuid())
        .fetch_one(pool)
        .await
        .unwrap()
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

// ------------------------------------------------------------------ F-007 / 008 / 011 / 012

#[tokio::test]
async fn counter_payments_allocate_receipt_post_and_reverse() {
    let (app, pool) = app(T_PAY).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_PAY,
        "Payments University",
        Some(b4::AARAV),
    )
    .await;
    let acc = setup.student_account_id;
    let a = b5::seed_charge(&pool, &setup, "125000.00", "published", Some(day(7, 15))).await;
    let b = b5::seed_charge(&pool, &setup, "20000.00", "published", Some(day(8, 15))).await;
    let draft = b5::seed_charge(&pool, &setup, "999.00", "draft", None).await;

    // Missing Idempotency-Key → 400; gateway channel cannot be recorded; bad amount → 422.
    let (s, _, _) = send(
        &app,
        Request::post(format!("{F}/student-accounts/{acc}/payments"))
            .header("content-type", "application/json")
            .header("cookie", &admin.cookie)
            .header("x-csrf-token", &admin.csrf)
            .body(Body::from(
                json!({"channel": "cash", "amount": "10.00"}).to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    call(
        &app,
        &admin,
        "POST",
        &format!("/student-accounts/{acc}/payments"),
        json!({"channel": "gateway", "amount": "10.00"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/student-accounts/{acc}/payments"),
        json!({"channel": "cash", "amount": "10.001"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // 150000 cash: oldest due first (A 125000, B 20000), 5000 kept unapplied; one balanced entry.
    let req = |key: &str| {
        Request::post(format!("{F}/student-accounts/{acc}/payments"))
            .header("content-type", "application/json")
            .header("cookie", &admin.cookie)
            .header("x-csrf-token", &admin.csrf)
            .header("idempotency-key", key)
            .body(Body::from(
                json!({"channel": "cash", "amount": "150000.00", "externalRef": "CTR-1"})
                    .to_string(),
            ))
            .unwrap()
    };
    let (s, h, p) = send(&app, req("pay-fixed-1")).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert!(h.get("etag").is_some());
    assert_eq!(p["status"], "completed");
    assert!(p["paymentNumber"].as_str().unwrap().starts_with("PAY-"));
    assert!(p["receipt"]["receiptNumber"]
        .as_str()
        .unwrap()
        .starts_with("RCT-"));
    assert_eq!(p["unappliedAmount"], "5000.00");
    let allocs = p["allocations"].as_array().unwrap();
    assert_eq!(allocs.len(), 2);
    assert_eq!(
        (
            allocs[0]["chargeNumber"].as_str().unwrap(),
            allocs[0]["amount"].as_str().unwrap()
        ),
        (a.charge_number.as_str(), "125000.00")
    );
    assert_eq!(allocs[1]["amount"], "20000.00");
    // Replay with the same key → the same payment, nothing posted twice.
    let (s, h2, again) = send(&app, req("pay-fixed-1")).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        h2.get("idempotency-replayed").map(|v| v.to_str().unwrap()),
        Some("true")
    );
    assert_eq!(again["id"], p["id"]);
    let payments: i64 =
        sqlx::query_scalar("SELECT count(*) FROM finance.payment WHERE student_account_id = $1")
            .bind(acc.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payments, 1);
    assert_eq!(
        outstanding(&pool, a.charge_id).await,
        ("active".into(), amount("0"))
    );
    assert_eq!(
        outstanding(&pool, b.charge_id).await,
        ("active".into(), amount("0"))
    );
    assert_eq!(
        balance(&pool, &setup, "cash_on_hand").await,
        amount("150000")
    );
    assert_eq!(
        balance(&pool, &setup, "unapplied_receipts").await,
        amount("-5000")
    );
    ledger_agrees(&pool, &setup).await;
    assert_eq!(events(&pool, T_PAY, "payment.completed").await, 1);
    assert_eq!(events(&pool, T_PAY, "receipt.issued").await, 1);

    // Receipt views.
    let pid = id(&p);
    let r = read(&app, &admin, &format!("/payments/{pid}/receipt")).await;
    assert_eq!(r["amountDisplay"], "₹1,50,000.00");
    assert_eq!(r["allocations"].as_array().unwrap().len(), 2);
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let mine = read(&app, &student, "/me/receipts").await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    let (s, _, _) = send(&app, get(&student, &format!("{F}/payments/{pid}"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "staff reads need finance.read");

    // Later allocation of the unapplied part to a new charge (F-008): If-Match required.
    let c = b5::seed_charge(&pool, &setup, "8000.00", "published", Some(day(9, 30))).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/allocate"),
        json!({}),
        None,
        StatusCode::PRECONDITION_REQUIRED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/allocate"),
        json!({}),
        Some(ver(&p) + 5),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/allocate"),
        json!({"allocations": [{"chargeId": c.charge_id, "amount": "6000.00"}]}),
        Some(ver(&p)),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/allocate"),
        json!({"allocations": [{"chargeId": draft.charge_id, "amount": "100.00"}]}),
        Some(ver(&p)),
        StatusCode::CONFLICT,
    )
    .await;
    let p = call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/allocate"),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["unappliedAmount"], "0.00");
    assert_eq!(p["allocations"].as_array().unwrap().len(), 3);
    assert_eq!(p["allocations"][2]["source"], "unapplied");
    assert_eq!(outstanding(&pool, c.charge_id).await.1, amount("3000"));
    assert_eq!(
        balance(&pool, &setup, "unapplied_receipts").await,
        amount("0")
    );
    ledger_agrees(&pool, &setup).await;

    // Explicit allocation on a sponsor payment (F-008) and over-allocation refused.
    call(&app, &admin, "POST", &format!("/student-accounts/{acc}/payments"), json!({"channel": "sponsor", "amount": "1000.00", "allocations": [{"chargeId": c.charge_id, "amount": "1500.00"}]}), None, StatusCode::CONFLICT).await;
    let sp = call(&app, &admin, "POST", &format!("/student-accounts/{acc}/payments"), json!({"channel": "sponsor", "amount": "1000.00", "payerName": "Tata Trust", "allocations": [{"chargeId": c.charge_id, "amount": "1000.00"}]}), None, StatusCode::CREATED).await;
    assert_eq!(sp["payerKind"], "sponsor");
    assert_eq!(outstanding(&pool, c.charge_id).await.1, amount("2000"));

    // Reversal (F-011): all three allocations un-settled, receipt cancelled, mirror entries.
    let p = read(&app, &admin, &format!("/payments/{pid}")).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/reverse"),
        json!({}),
        Some(ver(&p)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let rev = call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/reverse"),
        json!({"reason": "cheque bounced"}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rev["status"], "reversed");
    assert!(rev["reversalEntryId"].is_string());
    assert!(rev["processInstanceId"].is_string());
    assert_eq!(rev["receipt"]["status"], "cancelled");
    assert!(rev["allocations"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["status"] == "reversed"));
    assert_eq!(outstanding(&pool, a.charge_id).await.1, amount("125000"));
    assert_eq!(outstanding(&pool, c.charge_id).await.1, amount("7000"));
    assert_eq!(balance(&pool, &setup, "cash_on_hand").await, amount("0"));
    ledger_agrees(&pool, &setup).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/reverse"),
        json!({"reason": "again"}),
        Some(ver(&rev)),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{pid}/cancel"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::CONFLICT,
    )
    .await;
    // Chargeback of the sponsor payment.
    let cb = call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{}/chargeback", id(&sp)),
        json!({"reason": "sponsor disputed"}),
        Some(ver(&sp)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cb["status"], "charged_back");
    assert_eq!(events(&pool, T_PAY, "payment.reversed").await, 2);
    ledger_agrees(&pool, &setup).await;

    // Lists.
    let list = read(
        &app,
        &admin,
        &format!(
            "/institutions/{}/payments?status=reversed",
            setup.institution()
        ),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let page = read(
        &app,
        &admin,
        &format!("/institutions/{}/payments?limit=1", setup.institution()),
    )
    .await;
    assert!(page["nextCursor"].is_string());
    let acc_list = read(&app, &admin, &format!("/student-accounts/{acc}/payments")).await;
    assert_eq!(acc_list["items"].as_array().unwrap().len(), 2);
    // Status history keeps the payment's life.
    let hist: i64 = sqlx::query_scalar("SELECT count(*) FROM finance.status_history WHERE aggregate_type = 'payment' AND aggregate_id = $1::uuid")
        .bind(&pid)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(hist, 3, "initiated → completed → reversed");
}

// ------------------------------------------------------------------ F-007 gateway / F-010 / F-012

#[tokio::test]
async fn gateway_checkout_webhook_failure_timeout_and_cancel() {
    let (app, pool) = app(T_GATEWAY).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_GATEWAY,
        "Gateway University",
        Some(b4::AARAV),
    )
    .await;
    let charge = b5::seed_charge(&pool, &setup, "50000.00", "published", Some(day(12, 31))).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    call(
        &app,
        &admin,
        "POST",
        "/me/payments",
        json!({"amount": "10.00"}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "50000.01"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "10.00", "chargeIds": [Uuid::now_v7()]}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // Happy path: order created (pending), checkout return confirms → completed + receipt.
    let p = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "20000.00", "chargeIds": [charge.charge_id]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(p["status"], "pending");
    assert_eq!(p["channel"], "gateway");
    let order = p["gatewayOrderRef"].as_str().unwrap().to_owned();
    assert!(order.starts_with("order_mock_"));
    assert!(p["checkoutUrl"].as_str().unwrap().contains(&order));
    assert_eq!(events(&pool, T_GATEWAY, "payment.initiated").await, 1);
    let done = call(
        &app,
        &student,
        "POST",
        &format!("/me/payments/{}/confirm", id(&p)),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(done["status"], "completed");
    assert!(done["gatewayPaymentRef"]
        .as_str()
        .unwrap()
        .starts_with("pay_mock_"));
    assert_eq!(done["allocations"][0]["amount"], "20000.00");
    assert_eq!(
        balance(&pool, &setup, "gateway_clearing").await,
        amount("20000")
    );
    // A late captured webhook for it is idempotent (already completed).
    let gw = MockPaymentGateway::default();
    let hook = |event_id: &str, event: &str, order: &str, amt: &str| {
        let body = MockPaymentGateway::webhook_body(event_id, event, order, amount(amt));
        let sig = gw.sign(body.as_bytes());
        Request::post(format!("{F}/gateway/webhook"))
            .header("content-type", "application/json")
            .header("x-razorpay-signature", sig)
            .body(Body::from(body))
            .unwrap()
    };
    let (s, _, ack) = send(&app, hook("evt_a", "payment.captured", &order, "20000.00")).await;
    assert_eq!(s, StatusCode::OK, "{ack}");
    assert_eq!(
        (ack["status"].as_str(), ack["paymentStatus"].as_str()),
        (Some("processed"), Some("completed"))
    );
    assert_eq!(
        events(&pool, T_GATEWAY, "payment.completed").await,
        1,
        "no double completion"
    );

    // Failure (paise .13): confirm → failed, nothing posted.
    let f = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "100.13"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let f = call(
        &app,
        &student,
        "POST",
        &format!("/me/payments/{}/confirm", id(&f)),
        json!({}),
        Some(ver(&f)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["status"], "failed");
    assert_eq!(f["failureCode"], "BAD_REQUEST_ERROR");
    assert!(f["journalEntryId"].is_null());
    assert_eq!(events(&pool, T_GATEWAY, "payment.failed").await, 1);

    // Timeout (paise .99): confirm keeps it pending; admin recovery asks the gateway (still
    // pending); a signed captured webhook later completes it; duplicates and forgeries are refused.
    let t = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "500.99"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let tid = id(&t);
    let t = call(
        &app,
        &student,
        "POST",
        &format!("/me/payments/{tid}/confirm"),
        json!({}),
        Some(ver(&t)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t["status"], "pending");
    let t = call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{tid}/recover"),
        json!({}),
        Some(ver(&t)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t["status"], "pending");
    let torder = t["gatewayOrderRef"].as_str().unwrap().to_owned();
    let (s, _, _) = send(
        &app,
        Request::post(format!("{F}/gateway/webhook"))
            .header("content-type", "application/json")
            .header("x-razorpay-signature", "0".repeat(64))
            .body(Body::from(MockPaymentGateway::webhook_body(
                "evt_x",
                "payment.captured",
                &torder,
                amount("500.99"),
            )))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "forged signature");
    let (s, _, _) = send(
        &app,
        Request::post(format!("{F}/gateway/webhook"))
            .header("content-type", "application/json")
            .body(Body::from("{}"))
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED, "no signature");
    let (s, _, ack) = send(&app, hook("evt_b", "payment.captured", &torder, "500.99")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ack["paymentStatus"], "completed");
    let (_, _, dup) = send(&app, hook("evt_b", "payment.captured", &torder, "500.99")).await;
    assert_eq!(dup["status"], "duplicate");
    let (_, _, unknown) = send(
        &app,
        hook("evt_c", "payment.captured", "order_mock_unknown", "1.00"),
    )
    .await;
    assert_eq!(unknown["status"], "ignored");
    // Another timeout expires on recovery with `expire`.
    let e = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "200.99"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let e = call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{}/recover", id(&e)),
        json!({"expire": true}),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "expired");
    assert_eq!(e["failureCode"], "TIMEOUT");
    // A failed webhook on a pending payment fails it.
    let w = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "300.99"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (_, _, ack) = send(
        &app,
        hook(
            "evt_d",
            "payment.failed",
            w["gatewayOrderRef"].as_str().unwrap(),
            "300.99",
        ),
    )
    .await;
    assert_eq!(ack["paymentStatus"], "failed");

    // Cancellation (F-012): the payer abandons an open payment; a completed one cannot be cancelled.
    let c = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "1000.00"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let c = call(
        &app,
        &student,
        "POST",
        &format!("/me/payments/{}/cancel", id(&c)),
        json!({}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "cancelled");
    call(
        &app,
        &student,
        "POST",
        &format!("/me/payments/{}/cancel", id(&done)),
        json!({}),
        Some(ver(&done)),
        StatusCode::CONFLICT,
    )
    .await;
    // Webhook for an unrelated student's view: the student sees only their own payments.
    let mine = read(&app, &student, "/me/payments").await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 6);
    assert_eq!(
        outstanding(&pool, charge.charge_id).await.1,
        amount("29499.01")
    );
    ledger_agrees(&pool, &setup).await;
}

// ------------------------------------------------------------------ F-009 / F-013

#[tokio::test]
async fn unidentified_receipts_and_bulk_collection() {
    let (app, pool) = app(T_UNIDENTIFIED).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_UNIDENTIFIED,
        "Suspense University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution();
    let acc = setup.student_account_id;
    let charge = b5::seed_charge(&pool, &setup, "40000.00", "published", Some(day(7, 1))).await;

    // Unidentified bank transfer: Dr bank · Cr unapplied at once.
    let u = call(&app, &admin, "POST", &format!("/institutions/{inst}/unidentified-receipts"), json!({"channel": "bank_transfer", "amount": "15000.00", "externalRef": "UTR123", "payerName": "A DESAI"}), None, StatusCode::CREATED).await;
    assert_eq!(u["status"], "unidentified");
    assert_eq!(balance(&pool, &setup, "bank_main").await, amount("15000"));
    assert_eq!(
        balance(&pool, &setup, "unapplied_receipts").await,
        amount("-15000")
    );
    call(
        &app,
        &admin,
        "POST",
        &format!("/unidentified-receipts/{}/allocate", id(&u)),
        json!({}),
        Some(ver(&u)),
        StatusCode::CONFLICT,
    )
    .await;
    let u = call(
        &app,
        &admin,
        "POST",
        &format!("/unidentified-receipts/{}/investigate", id(&u)),
        json!({"reason": "matching UTR"}),
        Some(ver(&u)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(u["status"], "under_investigation");
    call(
        &app,
        &admin,
        "POST",
        &format!("/unidentified-receipts/{}/identify", id(&u)),
        json!({"studentAccountId": Uuid::now_v7()}),
        Some(ver(&u)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let u = call(
        &app,
        &admin,
        "POST",
        &format!("/unidentified-receipts/{}/identify", id(&u)),
        json!({"studentAccountId": acc}),
        Some(ver(&u)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(u["status"], "identified");
    let pid = u["paymentId"].as_str().unwrap().to_owned();
    let p = read(&app, &admin, &format!("/payments/{pid}")).await;
    assert_eq!(
        (p["status"].as_str(), p["unappliedAmount"].as_str()),
        (Some("completed"), Some("15000.00"))
    );
    assert!(p["receipt"]["receiptNumber"].is_string());
    let u = call(
        &app,
        &admin,
        "POST",
        &format!("/unidentified-receipts/{}/allocate", id(&u)),
        json!({}),
        Some(ver(&u)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(u["status"], "allocated");
    assert_eq!(
        outstanding(&pool, charge.charge_id).await.1,
        amount("25000")
    );
    assert_eq!(
        balance(&pool, &setup, "unapplied_receipts").await,
        amount("0")
    );
    assert_eq!(
        events(&pool, T_UNIDENTIFIED, "unidentified_receipt.resolved").await,
        1
    );
    // A second unidentified receipt is returned to the remitter.
    let u2 = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/unidentified-receipts"),
        json!({"channel": "cash", "amount": "700.00"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/unidentified-receipts/{}/refund", id(&u2)),
        json!({}),
        Some(ver(&u2)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let u2 = call(
        &app,
        &admin,
        "POST",
        &format!("/unidentified-receipts/{}/refund", id(&u2)),
        json!({"reason": "payer unknown after 30 days"}),
        Some(ver(&u2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(u2["status"], "refunded");
    assert!(u2["resolutionEntryId"].is_string());
    assert_eq!(balance(&pool, &setup, "cash_on_hand").await, amount("0"));
    let list = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/unidentified-receipts?status=refunded"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    ledger_agrees(&pool, &setup).await;

    // Bulk collection: one valid item, one unknown party → partially completed.
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/collections"),
        json!({"name": "x", "channel": "cash", "items": []}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let b = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/collections"),
        json!({"name": "Hostel camp counter", "channel": "cash", "items": [
            {"studentAccountId": acc, "amount": "5000.00", "externalRef": "CAMP-1"},
            {"studentAccountId": Uuid::now_v7(), "amount": "100.00"}
        ]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (b["status"].as_str(), b["totalAmount"].as_str()),
        (Some("draft"), Some("5100.00"))
    );
    call(
        &app,
        &admin,
        "POST",
        &format!("/collections/{}/execute", id(&b)),
        json!({}),
        Some(ver(&b)),
        StatusCode::CONFLICT,
    )
    .await;
    let b = call(
        &app,
        &admin,
        "POST",
        &format!("/collections/{}/validate", id(&b)),
        json!({}),
        Some(ver(&b)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(b["status"], "validated");
    assert_eq!(b["items"][1]["status"], "invalid");
    let b = call(
        &app,
        &admin,
        "POST",
        &format!("/collections/{}/execute", id(&b)),
        json!({}),
        Some(ver(&b)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(b["status"], "completed", "the invalid item never ran: {b}");
    assert_eq!(b["items"][0]["status"], "completed");
    assert!(b["items"][0]["paymentId"].is_string());
    assert_eq!(
        outstanding(&pool, charge.charge_id).await.1,
        amount("20000")
    );
    assert_eq!(
        events(&pool, T_UNIDENTIFIED, "payment_batch.completed").await,
        1
    );
    let all = read(&app, &admin, &format!("/institutions/{inst}/collections")).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 1);
    ledger_agrees(&pool, &setup).await;
}

// ------------------------------------------------------------------ F-014 … F-019

#[tokio::test]
async fn refund_reference_approval_and_lifecycle() {
    let (app, pool) = app(T_REFUND).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_REFUND,
        "Refund University",
        Some(b4::AARAV),
    )
    .await;
    let acc = setup.student_account_id;
    let charge = b5::seed_charge(&pool, &setup, "10000.00", "published", Some(day(12, 1))).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;

    // Overpayment by bank transfer leaves 8000 of credit.
    call(
        &app,
        &admin,
        "POST",
        &format!("/student-accounts/{acc}/payments"),
        json!({"channel": "bank_transfer", "amount": "18000.00"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &app,
        &student,
        "POST",
        "/me/refunds",
        json!({"amount": "8000.01", "reason": "overpaid"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &student,
        "POST",
        "/me/refunds",
        json!({"amount": "100.00", "reason": " "}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // Student request → requested → under_review → approved (auto) in one call.
    let r = call(
        &app,
        &student,
        "POST",
        "/me/refunds",
        json!({"amount": "3000.00", "reason": "overpaid tuition"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["status"], "approved", "{r}");
    assert_eq!(r["method"], "bank_transfer");
    assert_eq!(r["requestedVia"], "self");
    assert!(r["refundNumber"].as_str().unwrap().starts_with("RFD-"));
    assert!(r["processInstanceId"].is_string());
    assert_eq!(r["decidedBy"], "system:auto-approval");
    assert!(r["journalEntryId"].is_string());
    let path: Vec<&str> = r["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap())
        .collect();
    assert_eq!(path, ["requested", "under_review", "approved"]);
    assert_eq!(
        balance(&pool, &setup, "refunds_payable").await,
        amount("-3000")
    );
    assert_eq!(
        balance(&pool, &setup, "unapplied_receipts").await,
        amount("-5000")
    );
    let rid = id(&r);
    // Processing (bank transfer) then confirmation → completed (Dr refunds payable · Cr bank).
    call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{rid}/complete"),
        json!({}),
        Some(ver(&r)),
        StatusCode::CONFLICT,
    )
    .await;
    let r = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{rid}/process"),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (r["status"].as_str(), r["attempts"].as_i64()),
        (Some("processing"), Some(1))
    );
    let r = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{rid}/complete"),
        json!({"reference": "NEFT-77"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "completed");
    assert!(r["payEntryId"].is_string());
    assert_eq!(balance(&pool, &setup, "refunds_payable").await, amount("0"));
    assert_eq!(balance(&pool, &setup, "bank_main").await, amount("15000"));
    // Reversal (F-018): money recovered, credit back to the party.
    call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{rid}/reverse"),
        json!({}),
        Some(ver(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{rid}/reverse"),
        json!({"reason": "returned by the bank"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "reversed");
    assert_eq!(
        balance(&pool, &setup, "unapplied_receipts").await,
        amount("-8000")
    );
    assert_eq!(balance(&pool, &setup, "bank_main").await, amount("18000"));
    // Closure (F-019) keeps the full history.
    let r = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{rid}/close"),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    let path: Vec<&str> = r["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap())
        .collect();
    assert_eq!(
        path,
        [
            "requested",
            "under_review",
            "approved",
            "processing",
            "completed",
            "reversed",
            "closed"
        ]
    );
    call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{rid}/close"),
        json!({}),
        Some(ver(&r)),
        StatusCode::CONFLICT,
    )
    .await;

    // Cancellation of an approved refund (F-016) gives the credit back.
    let r2 = call(
        &app,
        &admin,
        "POST",
        &format!("/student-accounts/{acc}/refunds"),
        json!({"amount": "2000.00", "reason": "hostel deposit"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (r2["status"].as_str(), r2["requestedVia"].as_str()),
        (Some("approved"), Some("staff"))
    );
    let r2 = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{}/cancel", id(&r2)),
        json!({"reason": "student withdrew the request"}),
        Some(ver(&r2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r2["status"], "cancelled");
    assert_eq!(
        balance(&pool, &setup, "unapplied_receipts").await,
        amount("-8000")
    );
    // A payment holding refunded credit cannot be reversed while the refund is live.
    let r3 = call(
        &app,
        &admin,
        "POST",
        &format!("/student-accounts/{acc}/refunds"),
        json!({"amount": "500.00", "reason": "fee revision"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let pay = read(&app, &admin, &format!("/student-accounts/{acc}/payments")).await;
    let p0 = &pay["items"][0];
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{}/reverse", id(p0)),
        json!({"reason": "x"}),
        Some(ver(p0)),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{}/cancel", id(&r3)),
        json!({"reason": "not needed"}),
        Some(ver(&r3)),
        StatusCode::OK,
    )
    .await;

    // Gateway refunds back to the original instrument, with failure → retry → manual resolution (F-017).
    // A gateway payment captured after the charge was settled at the counter is all credit.
    let c2 = b5::seed_charge(&pool, &setup, "6000.00", "published", Some(day(12, 1))).await;
    let g = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "6000.00", "chargeIds": [c2.charge_id]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(&app, &admin, "POST", &format!("/student-accounts/{acc}/payments"), json!({"channel": "cash", "amount": "6000.00", "allocations": [{"chargeId": c2.charge_id, "amount": "6000.00"}]}), None, StatusCode::CREATED).await;
    let g = call(
        &app,
        &student,
        "POST",
        &format!("/me/payments/{}/confirm", id(&g)),
        json!({}),
        Some(ver(&g)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (g["status"].as_str(), g["unappliedAmount"].as_str()),
        (Some("completed"), Some("6000.00"))
    );
    let gid = id(&g);
    let f = call(
        &app,
        &student,
        "POST",
        "/me/refunds",
        json!({"amount": "1000.13", "reason": "paid twice", "paymentId": gid}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(f["method"], "original");
    let f = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{}/process", id(&f)),
        json!({}),
        Some(ver(&f)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["status"], "failed");
    assert!(f["gatewayRefundRef"]
        .as_str()
        .unwrap()
        .starts_with("rfnd_mock_"));
    let f = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{}/retry", id(&f)),
        json!({}),
        Some(ver(&f)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (f["status"].as_str(), f["attempts"].as_i64()),
        (Some("failed"), Some(2))
    );
    let f = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{}/cancel", id(&f)),
        json!({"reason": "refund by cheque instead"}),
        Some(ver(&f)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["status"], "cancelled");
    let ok = call(
        &app,
        &student,
        "POST",
        "/me/refunds",
        json!({"amount": "2500.00", "reason": "paid twice", "paymentId": gid}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let ok = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{}/process", id(&ok)),
        json!({}),
        Some(ver(&ok)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ok["status"], "completed");
    assert_eq!(
        balance(&pool, &setup, "gateway_clearing").await,
        amount("3500")
    );
    assert_eq!(events(&pool, T_REFUND, "refund.failed").await, 2);
    assert_eq!(events(&pool, T_REFUND, "refund.completed").await, 2);
    let mine = read(&app, &student, "/me/refunds").await;
    assert_eq!(
        mine["items"].as_array().unwrap().len(),
        5,
        "every refund of the student's account"
    );
    let all = read(
        &app,
        &admin,
        &format!(
            "/institutions/{}/refunds?status=cancelled",
            setup.institution()
        ),
    )
    .await;
    assert_eq!(all["items"].as_array().unwrap().len(), 3);
    assert_eq!(outstanding(&pool, charge.charge_id).await.1, amount("0"));
    ledger_agrees(&pool, &setup).await;
}

// ------------------------------------------------------------------ F-039 … F-043

#[tokio::test]
async fn receivables_dunning_write_off_and_reopening() {
    let (app, pool) = app(T_RECEIVABLE).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_RECEIVABLE,
        "Dues University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution();
    let acc = setup.student_account_id;
    let overdue = b5::seed_charge(&pool, &setup, "30000.00", "published", Some(day(6, 30))).await;
    let future = b5::seed_charge(&pool, &setup, "12000.00", "published", Some(day(12, 31))).await;
    let paid = b5::seed_charge(&pool, &setup, "5000.00", "published", Some(day(6, 30))).await;
    b5::seed_charge(&pool, &setup, "700.00", "draft", None).await;
    b5::seed_payment(&pool, &setup, paid.charge_id, "5000.00").await;
    b5::seed_payment(&pool, &setup, future.charge_id, "2000.00").await;

    // Recognition + evaluation (F-039/040/041).
    let ev = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/receivables"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            ev["recognised"].as_i64(),
            ev["overdue"].as_i64(),
            ev["settled"].as_i64()
        ),
        (Some(3), Some(1), Some(1)),
        "{ev}"
    );
    let again = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/receivables"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["recognised"], 0, "idempotent");
    let list = read(&app, &admin, &format!("/institutions/{inst}/receivables")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    assert_eq!(list["totalOutstanding"], "40000.00");
    assert_eq!(list["totalOverdue"], "30000.00");
    let find = |num: &str| {
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["chargeNumber"] == num)
            .cloned()
            .unwrap()
    };
    let od = find(&overdue.charge_number);
    assert_eq!(od["status"], "overdue");
    assert_eq!(od["ageingBucket"], "90+");
    assert_eq!(find(&future.charge_number)["status"], "partially_settled");
    assert_eq!(find(&paid.charge_number)["status"], "settled");
    assert_eq!(
        events(&pool, T_RECEIVABLE, "receivable.recognised").await,
        3
    );
    assert_eq!(events(&pool, T_RECEIVABLE, "receivable.overdue").await, 1);

    // Dunning: levels 1 … 3, the third escalates; a not-overdue receivable is refused.
    let oid = id(&od);
    let fut = find(&future.charge_number);
    call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{}/dunning", id(&fut)),
        json!({}),
        Some(ver(&fut)),
        StatusCode::CONFLICT,
    )
    .await;
    let mut r = call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/dunning"),
        json!({}),
        Some(ver(&od)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            r["dunning"]["status"].as_str(),
            r["dunning"]["level"].as_i64()
        ),
        (Some("notice_sent"), Some(1))
    );
    assert!(r["dunning"]["notices"][0]["message"]
        .as_str()
        .unwrap()
        .contains("₹30,000.00"));
    let run = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/dunning-runs"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(run["noticesSent"], 0, "already noticed today");
    r = call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/dunning"),
        json!({"message": "Second reminder"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    r = call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/dunning"),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["dunning"]["status"], "escalated");
    call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/dunning"),
        json!({}),
        Some(ver(&r)),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(events(&pool, T_RECEIVABLE, "dunning.notice_sent").await, 3);
    let case = read(
        &app,
        &admin,
        &format!("/dunning-cases/{}", r["dunning"]["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(case["notices"].as_array().unwrap().len(), 3);

    // Write-off decision (F-042): auto-approved → Dr bad debts · Cr receivables, charge cleared,
    // dunning case resolved.
    call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/write-off"),
        json!({"reason": "uncollectible", "amount": "30000.01"}),
        Some(ver(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let w = call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/write-off"),
        json!({"reason": "student untraceable"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "written_off", "{w}");
    assert_eq!(w["writtenOffAmount"], "30000.00");
    assert_eq!(w["writeOffs"][0]["status"], "posted");
    assert_eq!(w["dunning"]["status"], "resolved");
    assert_eq!(
        balance(&pool, &setup, "bad_debt_expense").await,
        amount("30000")
    );
    assert_eq!(outstanding(&pool, overdue.charge_id).await.1, amount("0"));
    ledger_agrees(&pool, &setup).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/write-off"),
        json!({"reason": "again"}),
        Some(ver(&w)),
        StatusCode::CONFLICT,
    )
    .await;

    // Reopening on new evidence (F-043): the write-off is reversed, the debt is back.
    call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/reopen"),
        json!({}),
        Some(ver(&w)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let re = call(
        &app,
        &admin,
        "POST",
        &format!("/receivables/{oid}/reopen"),
        json!({"reason": "student traced; employer will pay"}),
        Some(ver(&w)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(re["status"], "reopened");
    assert_eq!(re["outstandingAmount"], "30000.00");
    assert_eq!(re["writeOffs"][0]["status"], "reversed");
    assert_eq!(
        balance(&pool, &setup, "bad_debt_expense").await,
        amount("0")
    );
    assert_eq!(
        outstanding(&pool, overdue.charge_id).await.1,
        amount("30000")
    );
    ledger_agrees(&pool, &setup).await;
    // A settled receivable whose payment is reversed re-opens on evaluation; paying it settles it.
    let pay = call(&app, &admin, "POST", &format!("/student-accounts/{acc}/payments"), json!({"channel": "cash", "amount": "30000.00", "allocations": [{"chargeId": overdue.charge_id, "amount": "30000.00"}]}), None, StatusCode::CREATED).await;
    let ev = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/receivables"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(ev["settled"], 1);
    call(
        &app,
        &admin,
        "POST",
        &format!("/payments/{}/reverse", id(&pay)),
        json!({"reason": "cheque bounced"}),
        Some(ver(&pay)),
        StatusCode::OK,
    )
    .await;
    let ev = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/receivables"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(ev["reopened"], 1);
    let rec = read(&app, &admin, &format!("/receivables/{oid}")).await;
    assert_eq!(rec["status"], "reopened");
    assert_eq!(events(&pool, T_RECEIVABLE, "receivable.reopened").await, 2);
    assert_eq!(events(&pool, T_RECEIVABLE, "receivable.settled").await, 2);
    assert_eq!(
        events(&pool, T_RECEIVABLE, "receivable.written_off").await,
        1
    );
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let (s, _, _) = send(
        &app,
        get(&student, &format!("{F}/institutions/{inst}/receivables")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    ledger_agrees(&pool, &setup).await;
}

// ------------------------------------------------------------------ AwaitDecision

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

/// Refund approval (the Batch 5 reference) and the receivable write-off stay pending under
/// `AwaitDecision` until `…/decision` runs; rejection needs a reason and posts nothing.
#[tokio::test]
async fn approvals_await_a_decision() {
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
    let acc = setup.student_account_id;
    let charge = b5::seed_charge(&pool, &setup, "10000.00", "published", Some(day(6, 1))).await;
    let tenant = setup.tenant();
    let api = await_app(
        &pool,
        auth("dev|admin@sutra.dev", Role::Administrator, tenant),
    );
    // Part 1 (DEF-X-002): an independent checker decides (finance approver persona).
    let checker = await_app(
        &pool,
        auth(
            "dev|finance.approver@sutra.dev",
            Role::FinanceApprover,
            tenant,
        ),
    );

    direct(&api, "POST", &format!("/student-accounts/{acc}/payments"), json!({"channel": "cash", "amount": "4000.00", "allocations": [{"chargeId": charge.charge_id, "amount": "1000.00"}]}), None, StatusCode::CREATED).await;
    let r = direct(
        &api,
        "POST",
        &format!("/student-accounts/{acc}/refunds"),
        json!({"amount": "1200.00", "reason": "overpaid"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["status"], "under_review");
    assert!(r["processInstanceId"].is_string());
    assert!(
        r["journalEntryId"].is_null(),
        "nothing posted before the decision"
    );
    assert_eq!(balance(&pool, &setup, "refunds_payable").await, amount("0"));
    let rid = id(&r);
    direct(
        &api,
        "POST",
        &format!("/refunds/{rid}/process"),
        json!({}),
        Some(ver(&r)),
        StatusCode::CONFLICT,
    )
    .await;
    direct(
        &checker,
        "POST",
        &format!("/refunds/{rid}/decision"),
        json!({"decision": "reject"}),
        Some(ver(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    direct(
        &checker,
        "POST",
        &format!("/refunds/{rid}/decision"),
        json!({"decision": "maybe"}),
        Some(ver(&r)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // The requester cannot approve their own refund (separation of duties).
    direct(
        &api,
        "POST",
        &format!("/refunds/{rid}/decision"),
        json!({"decision": "approve", "reason": "mine"}),
        Some(ver(&r)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let r = direct(
        &checker,
        "POST",
        &format!("/refunds/{rid}/decision"),
        json!({"decision": "approve", "reason": "verified"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "approved");
    assert_eq!(r["decidedBy"], "dev|finance.approver@sutra.dev");
    assert_eq!(
        balance(&pool, &setup, "refunds_payable").await,
        amount("-1200")
    );
    let r2 = direct(
        &api,
        "POST",
        &format!("/student-accounts/{acc}/refunds"),
        json!({"amount": "300.00", "reason": "second"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let r2 = direct(
        &checker,
        "POST",
        &format!("/refunds/{}/decision", id(&r2)),
        json!({"decision": "reject", "reason": "not eligible"}),
        Some(ver(&r2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r2["status"], "rejected");
    assert!(r2["journalEntryId"].is_null());
    direct(
        &checker,
        "POST",
        &format!("/refunds/{}/decision", id(&r2)),
        json!({"decision": "approve"}),
        Some(ver(&r2)),
        StatusCode::CONFLICT,
    )
    .await;

    // Receivable write-off awaits its decision; reject, then approve a new request.
    direct(
        &api,
        "POST",
        &format!("/institutions/{}/receivables", setup.institution()),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    let rec_id: Uuid = sqlx::query_scalar("SELECT id FROM finance.receivable WHERE charge_id = $1")
        .bind(charge.charge_id.as_uuid())
        .fetch_one(&pool)
        .await
        .unwrap();
    let rec = direct(
        &api,
        "GET",
        &format!("/receivables/{rec_id}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(rec["status"], "overdue");
    let rec = direct(
        &api,
        "POST",
        &format!("/receivables/{rec_id}/write-off"),
        json!({"reason": "hardship"}),
        Some(ver(&rec)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rec["status"], "overdue", "still owed until decided");
    assert_eq!(rec["writeOffs"][0]["status"], "pending_approval");
    direct(
        &api,
        "POST",
        &format!("/receivables/{rec_id}/write-off"),
        json!({"reason": "again"}),
        Some(ver(&rec)),
        StatusCode::CONFLICT,
    )
    .await;
    let rec = direct(
        &checker,
        "POST",
        &format!("/receivables/{rec_id}/write-off/decision"),
        json!({"decision": "reject", "reason": "pursue"}),
        Some(ver(&rec)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rec["writeOffs"][0]["status"], "rejected");
    let rec = direct(
        &api,
        "POST",
        &format!("/receivables/{rec_id}/write-off"),
        json!({"reason": "hardship confirmed", "amount": "9000.00"}),
        Some(ver(&rec)),
        StatusCode::OK,
    )
    .await;
    direct(
        &api,
        "POST",
        &format!("/receivables/{rec_id}/write-off/decision"),
        json!({"decision": "approve"}),
        Some(ver(&rec)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let rec = direct(
        &checker,
        "POST",
        &format!("/receivables/{rec_id}/write-off/decision"),
        json!({"decision": "approve"}),
        Some(ver(&rec)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rec["status"], "written_off");
    assert_eq!(
        balance(&pool, &setup, "bad_debt_expense").await,
        amount("9000")
    );
    ledger_agrees(&pool, &setup).await;
    // The inbox rule makes a to-do for the staff requester of a refund.
    assert_eq!(events(&pool, T_AWAIT, "refund.requested").await, 2);
}

// ------------------------------------------------------------------ ERP_EXTENSIONS 1D

/// Makes `parameters` the effective version of a rule (the governed lifecycle is 5A's own test).
async fn effective_rule(
    pool: &sqlx::PgPool,
    setup: &b5::FinanceSetup,
    key: &str,
    parameters: Value,
) {
    sqlx::query(
        "UPDATE finance.finance_rule SET status = 'superseded', effective_until = now() \
         WHERE institution_id = $1 AND rule_key = $2 AND status = 'effective'",
    )
    .bind(setup.institution().as_uuid())
    .bind(key)
    .execute(pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO finance.finance_rule (tenant_id, institution_id, rule_key, version_no, parameters, status, \
         effective_from, approved_at) VALUES ($1, $2, $3, (SELECT coalesce(max(version_no), 0) + 1 FROM \
         finance.finance_rule WHERE institution_id = $2 AND rule_key = $3), $4, 'effective', now(), now())",
    )
    .bind(setup.tenant().as_uuid())
    .bind(setup.institution().as_uuid())
    .bind(key)
    .bind(parameters)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn late_fees_accrue_idempotently_and_reminders_follow_the_schedule() {
    let (app, pool) = app(T_LATE_FEE).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_LATE_FEE,
        "Fines College",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution();
    let today = Utc::now().date_naive();
    let late = b5::seed_charge(
        &pool,
        &setup,
        "20000.00",
        "published",
        Some(today - chrono::Duration::days(40)),
    )
    .await;
    // Within the grace days: priced, nothing to charge.
    b5::seed_charge(
        &pool,
        &setup,
        "5000.00",
        "published",
        Some(today - chrono::Duration::days(2)),
    )
    .await;

    // No rule yet; a malformed rule is refused by the configuration validator.
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/late-fees/preview"),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/finance-rules"),
        json!({"ruleKey": "late_fee", "values": {"mode": "per_day"}}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    call(&app, &admin, "POST", &format!("/institutions/{inst}/finance-rules"),
        json!({"ruleKey": "late_fee", "values": {"mode": "per_day", "amount": "10.00", "graceDays": 5, "cap": "300.00"}}),
        None, StatusCode::CREATED).await;

    // ₹10 a day after 5 grace days, capped at ₹300.
    effective_rule(
        &pool,
        &setup,
        "late_fee",
        json!({"mode": "per_day", "amount": "10.00", "graceDays": 5, "cap": "300.00"}),
    )
    .await;
    let preview = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/late-fees/preview"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (preview["charged"].as_i64(), preview["total"].as_str()),
        (Some(1), Some("300.00")),
        "{preview}"
    );
    assert_eq!(preview["items"].as_array().unwrap().len(), 2);
    let as_of: NaiveDate = preview["asOf"].as_str().unwrap().parse().unwrap();
    let run = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/late-fees"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (run["charged"].as_i64(), run["total"].as_str()),
        (Some(1), Some("300.00")),
        "{run}"
    );
    let item = run["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["chargeNumber"] == late.charge_number.as_str())
        .unwrap()
        .clone();
    assert!(item["feeChargeNumber"].is_string());
    let (src, status, total): (String, String, Decimal) = sqlx::query_as(
        "SELECT source_type, status, total_amount FROM finance.charge WHERE source_id = $1",
    )
    .bind(late.charge_id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (src.as_str(), status.as_str(), total),
        ("fine", "published", amount("300.00"))
    );
    let again = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/late-fees"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["charged"], 0, "idempotent: {again}");

    // A richer rule charges only the difference: 2 % a month, 40 days late = 2 months = ₹800.
    effective_rule(
        &pool,
        &setup,
        "late_fee",
        json!({"mode": "percent", "percent": "2", "frequency": "monthly"}),
    )
    .await;
    let run = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/late-fees"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (run["charged"].as_i64(), run["total"].as_str()),
        (Some(1), Some("500.00")),
        "{run}"
    );
    assert_eq!(events(&pool, T_LATE_FEE, "late_fee.charged").await, 2);
    ledger_agrees(&pool, &setup).await;

    // Pre-due reminders: 7 days before the due date, once.
    b5::seed_charge(
        &pool,
        &setup,
        "10000.00",
        "published",
        Some(as_of + chrono::Duration::days(7)),
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/fee-reminders"),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    effective_rule(
        &pool,
        &setup,
        "reminder_schedule",
        json!({"beforeDays": [7, 1], "afterDays": [1, 15, 30]}),
    )
    .await;
    let sent = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/fee-reminders"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(sent["sent"], 1, "{sent}");
    assert!(sent["items"][0]["message"]
        .as_str()
        .unwrap()
        .contains("₹10,000.00"));
    let again = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/fee-reminders"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["sent"], 0);
    assert_eq!(events(&pool, T_LATE_FEE, "fee_reminder.sent").await, 1);
}
