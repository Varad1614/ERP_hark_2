//! Part 2 · Stage 4 (S4-pay) — the Razorpay gateway end to end against PostgreSQL and a local
//! HTTP stub of the Razorpay API (`sutra_infra::razorpay::stub`, documented request / response /
//! signature formats; live Razorpay is BLOCKED_EXTERNAL: no credentials).
//!
//! * `checkout_is_paid_only_on_a_server_side_capture` — order creation is idempotent (same
//!   Idempotency-Key → same payment, one Razorpay order); a forged / mismatched / incomplete
//!   checkout callback is a 422 and audited; a correctly signed callback for a payment Razorpay
//!   only *authorized* leaves the fee unpaid; it completes only when Razorpay reports the capture
//!   (ledger: Dr gateway clearing / Cr receivables); webhooks for it are idempotent; tampered /
//!   unsigned / foreign-secret webhooks are 401.
//! * `webhooks_are_idempotent_under_replay_and_concurrency` — captured / failed / late-capture
//!   webhooks, replays → `duplicate`, two concurrent deliveries → one completion, a capture of the
//!   wrong amount → 409 and nothing posted; every processed webhook audited in its tenant.
//! * `recovery_job_polls_open_and_failed_payments` — the `payment_gateway_recovery` sweeper
//!   completes captured, fails failed, expires overdue pending payments, leaves payments
//!   untouched when Razorpay is down (audited, back-off), and recovers a late capture of a
//!   failed payment.
//! * `refund_of_a_double_payment_reverses_the_ledger` — a Razorpay capture after the charge was
//!   settled at the counter is unapplied credit; its refund goes back through the Refunds API
//!   (idempotency key) and clears gateway clearing / unapplied receipts / refunds payable.
//! * `settlement_report_reconciles_and_flags_mismatches` — Razorpay settlement CSV import:
//!   matched / amount mismatch / unknown payment / duplicate line / unknown refund / ignored,
//!   re-import idempotent, already-settled detection, permissions and tenant isolation.
//! * `recovery_job_is_wired_with_the_sweepers` — background wiring per configuration.
//! * `gateway_info_tells_the_browser_which_checkout_to_open` — Part 2 S4 UI (DEF-F-007):
//!   `GET /me/payment-gateway` names the adapter and the **public** Razorpay key id (never the
//!   secret), `onlinePayments=false` when disabled, mock without a key; 401 / 403 otherwise.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;
use support::b5::amount;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_finance::payments::recovery::GatewayRecovery;
use sutra_finance::FinanceState;
use sutra_infra::razorpay::stub::{Attempt, Fault, RazorpayStub};
use uuid::Uuid;

const T_CHECKOUT: &str = "0192f5a0-7c3e-7000-8000-0000002a7001";
const T_WEBHOOK: &str = "0192f5a0-7c3e-7000-8000-0000002a7002";
const T_RECOVERY: &str = "0192f5a0-7c3e-7000-8000-0000002a7003";
const T_REFUND: &str = "0192f5a0-7c3e-7000-8000-0000002a7004";
const T_SETTLE: &str = "0192f5a0-7c3e-7000-8000-0000002a7005";
const T_SETTLE_OTHER: &str = "0192f5a0-7c3e-7000-8000-0000002a7006";
const T_GATEWAY_INFO: &str = "0192f5a0-7c3e-7000-8000-0000002a7007";

const F: &str = "/api/v1/finance";

fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

fn runtime_url(url: &str) -> String {
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}")
}

/// Same as `support::app`, with the Razorpay gateway pointed at `stub`; also returns the finance
/// state the recovery job runs with.
async fn razorpay_app(
    tenant: &'static str,
    stub: &RazorpayStub,
) -> (Router, sqlx::PgPool, FinanceState) {
    let owner = owner_pool().await;
    let url = leak(runtime_url(database_url()));
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", url),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_APPROVAL_MODE", "auto"),
        ("SUTRA_PAYMENT_GATEWAY", "razorpay"),
        ("SUTRA_RAZORPAY_BASE_URL", leak(stub.base_url.clone())),
        ("SUTRA_RAZORPAY_KEY_ID", leak(stub.key_id.clone())),
        ("SUTRA_RAZORPAY_KEY_SECRET", leak(stub.key_secret.clone())),
        (
            "SUTRA_PAYMENT_WEBHOOK_SECRET",
            leak(stub.webhook_secret.clone()),
        ),
        ("SUTRA_RAZORPAY_TIMEOUT_MS", "1500"),
        ("SUTRA_RAZORPAY_MAX_RETRIES", "1"),
    ]);
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    assert_eq!(ctx.payment_gateway.name(), "razorpay");
    let mut deps =
        sutra_api::app::finance_deps_with(ctx.pool.clone(), ctx.workflow.clone(), ctx.approvals);
    deps.gateway = ctx.payment_gateway.clone();
    let state = deps.into_state();
    (router(ctx), owner, state)
}

fn day(m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, m, d).unwrap()
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
}

fn s(v: &Value, k: &str) -> String {
    v[k].as_str().unwrap_or_default().to_owned()
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
    let (st, _, b) = send(
        app,
        write(who, method, &format!("{F}{path}"), body, if_match),
    )
    .await;
    assert_eq!(st, expect, "{method} {path}: {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (st, _, b) = send(app, get(who, &format!("{F}{path}"))).await;
    assert_eq!(st, StatusCode::OK, "GET {path}: {b}");
    b
}

fn webhook(body: &str, sig: Option<&str>) -> Request<Body> {
    let mut b =
        Request::post(format!("{F}/gateway/webhook")).header("content-type", "application/json");
    if let Some(sig) = sig {
        b = b.header("x-razorpay-signature", sig);
    }
    b.body(Body::from(body.to_owned())).unwrap()
}

/// Σ debit − Σ credit of a seeded system account of the institution.
async fn balance(pool: &sqlx::PgPool, inst: Uuid, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(COALESCE(l.debit, 0) - COALESCE(l.credit, 0)), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id WHERE a.institution_id = $1 AND a.system_key = $2",
    )
    .bind(inst)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Trial balance of the institution + AR control = Σ outstanding.
async fn ledger_agrees(pool: &sqlx::PgPool, inst: Uuid) {
    let (d, c): (Decimal, Decimal) = sqlx::query_as(
        "SELECT COALESCE(sum(l.debit), 0), COALESCE(sum(l.credit), 0) FROM finance.journal_line l \
         JOIN finance.journal_entry e ON e.id = l.journal_entry_id WHERE e.institution_id = $1",
    )
    .bind(inst)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(d, c, "trial balance");
    let outstanding: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(outstanding_amount), 0) FROM finance.charge WHERE institution_id = $1 \
         AND status IN ('published', 'active', 'disputed', 'closed')",
    )
    .bind(inst)
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(
        balance(pool, inst, "student_receivables").await,
        outstanding,
        "AR control"
    );
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

async fn audits(pool: &sqlx::PgPool, tenant: &str, action: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1::uuid AND action = $2",
    )
    .bind(tenant)
    .bind(action)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn payment_entries(pool: &sqlx::PgPool, payment: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE source_type = 'payment' AND source_id = $1::uuid",
    )
    .bind(payment)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Student starts an online payment (fresh Idempotency-Key).
async fn initiate(app: &Router, student: &Login, amt: &str) -> Value {
    let p = call(
        app,
        student,
        "POST",
        "/me/payments",
        json!({"amount": amt}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (s(&p, "status"), s(&p, "channel")),
        ("pending".into(), "gateway".into())
    );
    assert_eq!(p["gateway"], "razorpay");
    assert!(s(&p, "gatewayOrderRef").starts_with("order_Stub"), "{p}");
    p
}

async fn confirm(
    app: &Router,
    student: &Login,
    p: &Value,
    body: Value,
    expect: StatusCode,
) -> Value {
    let cur = read_own(app, student, &id(p)).await;
    call(
        app,
        student,
        "POST",
        &format!("/me/payments/{}/confirm", id(p)),
        body,
        Some(ver(&cur)),
        expect,
    )
    .await
}

/// A payment as the finance office sees it.
async fn pay_get(app: &Router, admin: &Login, p: &Value) -> Value {
    read(app, admin, &format!("/payments/{}", id(p))).await
}

/// The caller's own payment from `/me/payments`.
async fn read_own(app: &Router, student: &Login, pid: &str) -> Value {
    let mine = read(app, student, "/me/payments").await;
    mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == pid)
        .cloned()
        .unwrap_or_else(|| panic!("payment {pid} not in /me/payments"))
}

// ------------------------------------------------------------------ checkout

#[tokio::test]
async fn checkout_is_paid_only_on_a_server_side_capture() {
    let stub = RazorpayStub::start().await.unwrap();
    let (app, pool, _) = razorpay_app(T_CHECKOUT, &stub).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_CHECKOUT,
        "Razorpay Checkout University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    let charge = b5::seed_charge(&pool, &setup, "50000.00", "published", Some(day(12, 31))).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;

    // Idempotent initiation: the same Idempotency-Key returns the same payment / one order.
    let key = Uuid::now_v7().to_string();
    let first = |k: &str| {
        Request::post(format!("{F}/me/payments"))
            .header("content-type", "application/json")
            .header("cookie", &student.cookie)
            .header("x-csrf-token", &student.csrf)
            .header("idempotency-key", k)
            .body(Body::from(
                json!({"amount": "20000.00", "chargeIds": [charge.charge_id]}).to_string(),
            ))
            .unwrap()
    };
    let (st1, _, p) = send(&app, first(&key)).await;
    assert_eq!(st1, StatusCode::CREATED, "{p}");
    let (st2, _, again) = send(&app, first(&key)).await;
    assert!(st2.is_success(), "{again}");
    assert_eq!(id(&again), id(&p));
    assert_eq!(
        stub.orders(),
        1,
        "one Razorpay order for one idempotency key"
    );
    assert!(
        p["checkoutUrl"].is_null(),
        "Razorpay Checkout opens client-side"
    );
    let order = s(&p, "gatewayOrderRef");
    let pid = id(&p);

    // Forged, mismatched and incomplete callbacks: 422, audited, nothing paid.
    let forged = confirm(&app, &student, &p, json!({"razorpay_order_id": order, "razorpay_payment_id": "pay_Forged0001", "razorpay_signature": "0".repeat(64)}), StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(
        forged["validationErrors"][0]["code"], "invalid_signature",
        "{forged}"
    );
    let other_order = confirm(&app, &student, &p, json!({"gatewayOrderId": "order_Other0001", "gatewayPaymentId": "pay_X1", "gatewaySignature": stub.checkout_signature("order_Other0001", "pay_X1")}), StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(other_order["validationErrors"][0]["code"], "order_mismatch");
    confirm(
        &app,
        &student,
        &p,
        json!({"razorpay_payment_id": "pay_X1"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(
        audits(&pool, T_CHECKOUT, "finance.payment.checkout_rejected").await,
        3
    );

    // A genuine, correctly signed callback for a payment Razorpay has only AUTHORIZED: the
    // browser says "success" but the fee stays unpaid until Razorpay reports the capture.
    let attempt = stub.pay(&order, Attempt::Authorized);
    let rzp = attempt["id"].as_str().unwrap().to_owned();
    let cb = json!({"razorpay_order_id": order, "razorpay_payment_id": rzp, "razorpay_signature": stub.checkout_signature(&order, &rzp)});
    let still = confirm(&app, &student, &p, cb.clone(), StatusCode::OK).await;
    assert_eq!(
        still["status"], "pending",
        "verified callback alone never marks the fee paid"
    );
    assert!(still["journalEntryId"].is_null());
    assert_eq!(
        balance(&pool, inst, "gateway_clearing").await,
        Decimal::ZERO
    );
    assert_eq!(
        audits(&pool, T_CHECKOUT, "finance.payment.checkout_verified").await,
        1
    );

    // Razorpay captures (auto-capture) → the same confirm now completes it from the API answer.
    stub.capture(&rzp);
    let done = confirm(&app, &student, &p, cb, StatusCode::OK).await;
    assert_eq!(done["status"], "completed");
    assert_eq!(done["gatewayPaymentRef"], rzp.as_str());
    assert_eq!(done["allocations"][0]["amount"], "20000.00");
    assert_eq!(
        balance(&pool, inst, "gateway_clearing").await,
        amount("20000.00")
    );
    assert_eq!(payment_entries(&pool, &pid).await, 1);
    ledger_agrees(&pool, inst).await;

    // Webhooks for the completed payment are idempotent (payment.captured, order.paid, replay).
    let mut paid = attempt.clone();
    paid["status"] = json!("captured");
    paid["captured"] = json!(true);
    let (body, sig) = stub.webhook("payment.captured", &paid);
    let (st, _, ack) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(st, StatusCode::OK, "{ack}");
    assert_eq!(
        (s(&ack, "status"), s(&ack, "paymentStatus")),
        ("processed".into(), "completed".into())
    );
    let (_, _, dup) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(dup["status"], "duplicate");
    let (obody, osig) = stub.webhook("order.paid", &paid);
    let (st, _, _) = send(&app, webhook(&obody, Some(&osig))).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        events(&pool, T_CHECKOUT, "payment.completed").await,
        1,
        "no double completion"
    );
    assert_eq!(payment_entries(&pool, &pid).await, 1, "no double posting");

    // Tampered body, no signature, a signature made with another secret → 401.
    let tampered = body.replace("2000000", "1");
    assert_ne!(tampered, body);
    let (st, _, _) = send(&app, webhook(&tampered, Some(&sig))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let (st, _, _) = send(&app, webhook(&body, None)).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let foreign = sutra_infra::razorpay::webhook_signature(
        "someone-elses-secret-0123456789abcdef",
        body.as_bytes(),
    );
    let (st, _, _) = send(&app, webhook(&body, Some(&foreign))).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Permissions: staff cannot pay as a student; a student cannot run recovery.
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
        &format!("/payments/{pid}/recover"),
        json!({}),
        Some(ver(&done)),
        StatusCode::FORBIDDEN,
    )
    .await;
}

// ------------------------------------------------------------------ webhooks

#[tokio::test]
async fn webhooks_are_idempotent_under_replay_and_concurrency() {
    let stub = RazorpayStub::start().await.unwrap();
    let (app, pool, _) = razorpay_app(T_WEBHOOK, &stub).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_WEBHOOK,
        "Razorpay Webhook University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    b5::seed_charge(&pool, &setup, "90000.00", "published", Some(day(12, 31))).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;

    // Captured webhook completes a pending payment; three replays are duplicates.
    let a = initiate(&app, &student, "1000.00").await;
    let pa = stub.pay(&s(&a, "gatewayOrderRef"), Attempt::Captured);
    let (body, sig) = stub.webhook("payment.captured", &pa);
    let (_, _, ack) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(
        (s(&ack, "status"), s(&ack, "paymentStatus")),
        ("processed".into(), "completed".into()),
        "{ack}"
    );
    for _ in 0..3 {
        let (st, _, dup) = send(&app, webhook(&body, Some(&sig))).await;
        assert_eq!(st, StatusCode::OK);
        assert_eq!(dup["status"], "duplicate");
    }
    assert_eq!(payment_entries(&pool, &id(&a)).await, 1);

    // Two concurrent deliveries of one event: exactly one completion.
    let b = initiate(&app, &student, "2000.00").await;
    let pb = stub.pay(&s(&b, "gatewayOrderRef"), Attempt::Captured);
    let (body, sig) = stub.webhook("payment.captured", &pb);
    let (r1, r2) = tokio::join!(
        send(&app, webhook(&body, Some(&sig))),
        send(&app, webhook(&body, Some(&sig)))
    );
    let mut st = vec![s(&r1.2, "status"), s(&r2.2, "status")];
    st.sort();
    assert_eq!(
        st,
        vec!["duplicate".to_owned(), "processed".to_owned()],
        "{r1:?} {r2:?}"
    );
    assert_eq!(payment_entries(&pool, &id(&b)).await, 1);
    assert_eq!(events(&pool, T_WEBHOOK, "payment.completed").await, 2);

    // payment.failed fails a pending payment; a later capture on the same order completes it.
    let c = initiate(&app, &student, "3000.00").await;
    let corder = s(&c, "gatewayOrderRef");
    let pc = stub.pay(&corder, Attempt::Failed);
    let (body, sig) = stub.webhook("payment.failed", &pc);
    let (_, _, ack) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(ack["paymentStatus"], "failed", "{ack}");
    let failed = read(&app, &admin, &format!("/payments/{}", id(&c))).await;
    assert_eq!(failed["failureCode"], "BAD_REQUEST_ERROR");
    assert!(failed["journalEntryId"].is_null());
    let late = stub.pay(&corder, Attempt::Captured);
    let (body, sig) = stub.webhook("payment.captured", &late);
    let (_, _, ack) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(
        ack["paymentStatus"], "completed",
        "late capture of a failed payment: {ack}"
    );

    // A capture of the wrong amount is refused (409), nothing posted, payment still pending.
    let d = initiate(&app, &student, "4000.00").await;
    let mut pd = stub.pay(&s(&d, "gatewayOrderRef"), Attempt::Captured);
    pd["amount"] = json!(100);
    let (body, sig) = stub.webhook("payment.captured", &pd);
    let (st, _, _) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(st, StatusCode::CONFLICT);
    let d = read(&app, &admin, &format!("/payments/{}", id(&d))).await;
    assert_eq!(d["status"], "pending");
    assert_eq!(payment_entries(&pool, &id(&d)).await, 0);
    // DEF-F-004: the refused event leaves a trace (event row + audit) and a replay is a duplicate.
    assert_eq!(
        audits(&pool, T_WEBHOOK, "finance.gateway.webhook_rejected").await,
        1
    );
    let outcome: String = sqlx::query_scalar(
        "SELECT outcome FROM finance.gateway_webhook_event WHERE payment_id = $1::uuid",
    )
    .bind(id(&d))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(outcome, "amount_mismatch");
    let (st, _, dup) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(
        (st, s(&dup, "status")),
        (StatusCode::OK, "duplicate".to_owned())
    );

    // Unknown order → ignored; every processed webhook is audited in the payment's tenant.
    let mut ghost = pa.clone();
    ghost["id"] = json!("pay_Ghost0000001");
    ghost["order_id"] = json!("order_Ghost000001");
    let (body, sig) = stub.webhook("payment.captured", &ghost);
    let (_, _, ack) = send(&app, webhook(&body, Some(&sig))).await;
    assert_eq!(ack["status"], "ignored");
    assert_eq!(audits(&pool, T_WEBHOOK, "finance.gateway.webhook").await, 4);
    let foreign_audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.audit_log WHERE action = 'finance.gateway.webhook' \
         AND target_id = $1 AND tenant_id <> $2::uuid",
    )
    .bind(id(&a))
    .bind(T_WEBHOOK)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(foreign_audits, 0);
    ledger_agrees(&pool, inst).await;
}

// ------------------------------------------------------------------ recovery job

#[tokio::test]
async fn recovery_job_polls_open_and_failed_payments() {
    let stub = RazorpayStub::start().await.unwrap();
    let (app, pool, state) = razorpay_app(T_RECOVERY, &stub).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_RECOVERY,
        "Razorpay Recovery University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    b5::seed_charge(&pool, &setup, "90000.00", "published", Some(day(12, 31))).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let job = GatewayRecovery {
        state,
        interval: Duration::from_secs(60),
        min_age: Duration::from_secs(1),
        expire_after: Duration::from_secs(48 * 3600),
        batch_size: 50,
    };
    let run = || sutra_kernel::db_context::system(job.run_now());

    let captured = initiate(&app, &student, "1100.00").await;
    stub.pay(&s(&captured, "gatewayOrderRef"), Attempt::Captured);
    let failed = initiate(&app, &student, "1200.00").await;
    stub.pay(&s(&failed, "gatewayOrderRef"), Attempt::Failed);
    let abandoned = initiate(&app, &student, "1300.00").await;
    let down = initiate(&app, &student, "1400.00").await;
    let fresh_pending = initiate(&app, &student, "1500.00").await;
    // `abandoned` is three days old: past the 48 h recovery deadline.
    sqlx::query(
        "UPDATE finance.payment SET created_at = now() - interval '3 days' WHERE id = $1::uuid",
    )
    .bind(id(&abandoned))
    .execute(&pool)
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    // Razorpay is down while `down` is polled: the others were "just polled" (not due yet).
    sqlx::query(
        "UPDATE finance.payment SET gateway_polled_at = now(), gateway_poll_count = 0 \
         WHERE tenant_id = $1::uuid AND id <> $2::uuid",
    )
    .bind(T_RECOVERY)
    .bind(id(&down))
    .execute(&pool)
    .await
    .unwrap();
    stub.inject("GET /v1/orders/payments", Fault::Status(503));
    stub.inject("GET /v1/orders/payments", Fault::Status(503));
    let report = run().await.unwrap();
    assert_eq!((report.polled, report.errors), (1, 1), "{report:?}");
    let d = read(&app, &admin, &format!("/payments/{}", id(&down))).await;
    assert_eq!(
        d["status"], "pending",
        "a gateway outage never guesses an outcome"
    );
    let (polls, at): (i32, Option<chrono::DateTime<chrono::Utc>>) = sqlx::query_as(
        "SELECT gateway_poll_count, gateway_polled_at FROM finance.payment WHERE id = $1::uuid",
    )
    .bind(id(&down))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(polls, 1);
    assert!(at.is_some());
    assert_eq!(
        audits(&pool, T_RECOVERY, "finance.payment.recovery_poll").await,
        1
    );

    // Everyone becomes due again (back-off base 1 s × 2^0); `down` backs off to 2 s.
    sqlx::query(
        "UPDATE finance.payment SET gateway_polled_at = NULL WHERE tenant_id = $1::uuid AND id <> $2::uuid",
    )
    .bind(T_RECOVERY)
    .bind(id(&down))
    .execute(&pool)
    .await
    .unwrap();
    let report = run().await.unwrap();
    assert_eq!(report.errors, 0, "{report:?}");
    assert_eq!(
        (report.completed, report.failed, report.expired),
        (1, 1, 1),
        "{report:?}"
    );
    let c = pay_get(&app, &admin, &captured).await;
    assert_eq!(c["status"], "completed");
    assert!(s(&c, "gatewayPaymentRef").starts_with("pay_Stub"));
    let f = pay_get(&app, &admin, &failed).await;
    assert_eq!(
        (f["status"].as_str(), f["failureCode"].as_str()),
        (Some("failed"), Some("BAD_REQUEST_ERROR"))
    );
    let e = pay_get(&app, &admin, &abandoned).await;
    assert_eq!(
        (e["status"].as_str(), e["failureCode"].as_str()),
        (Some("expired"), Some("TIMEOUT"))
    );
    assert_eq!(
        pay_get(&app, &admin, &fresh_pending).await["status"],
        "pending"
    );
    assert_eq!(pay_get(&app, &admin, &down).await["status"], "pending");

    // Immediately again: nothing is due (back-off), so Razorpay is not called.
    sqlx::query("UPDATE finance.payment SET gateway_polled_at = now() WHERE id = $1::uuid")
        .bind(id(&down))
        .execute(&pool)
        .await
        .unwrap();
    let before = stub.calls("GET /v1/orders/payments");
    let report = run().await.unwrap();
    assert_eq!(report.polled, 0, "{report:?}");
    assert_eq!(stub.calls("GET /v1/orders/payments"), before);

    // A late capture of the failed payment is recovered on a later poll (failed → completed).
    stub.pay(&s(&failed, "gatewayOrderRef"), Attempt::Captured);
    tokio::time::sleep(Duration::from_millis(2300)).await;
    let report = run().await.unwrap();
    assert!(report.completed >= 1, "{report:?}");
    assert_eq!(pay_get(&app, &admin, &failed).await["status"], "completed");
    assert!(audits(&pool, T_RECOVERY, "finance.payment.recovery_poll").await >= 6);
    ledger_agrees(&pool, inst).await;
}

// ------------------------------------------------------------------ refunds

#[tokio::test]
async fn refund_of_a_double_payment_reverses_the_ledger() {
    let stub = RazorpayStub::start().await.unwrap();
    let (app, pool, _) = razorpay_app(T_REFUND, &stub).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_REFUND,
        "Razorpay Refund University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    let acc = setup.student_account_id;
    let charge = b5::seed_charge(&pool, &setup, "6000.00", "published", Some(day(12, 1))).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;

    // The student pays online while the office records the same fee in cash.
    let g = call(
        &app,
        &student,
        "POST",
        "/me/payments",
        json!({"amount": "6000.00", "chargeIds": [charge.charge_id]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(&app, &admin, "POST", &format!("/student-accounts/{acc}/payments"), json!({"channel": "cash", "amount": "6000.00", "allocations": [{"chargeId": charge.charge_id, "amount": "6000.00"}]}), None, StatusCode::CREATED).await;
    stub.pay(&s(&g, "gatewayOrderRef"), Attempt::Captured);
    let g = confirm(&app, &student, &g, json!({}), StatusCode::OK).await;
    assert_eq!(
        (s(&g, "status"), s(&g, "unappliedAmount")),
        ("completed".into(), "6000.00".into())
    );
    assert_eq!(
        balance(&pool, inst, "gateway_clearing").await,
        amount("6000.00")
    );
    assert_eq!(
        balance(&pool, inst, "unapplied_receipts").await,
        amount("-6000.00")
    );

    // Refund to the original instrument through the Razorpay Refunds API.
    let r = call(
        &app,
        &student,
        "POST",
        "/me/refunds",
        json!({"amount": "6000.00", "reason": "paid twice", "paymentId": id(&g)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["method"], "original");
    let r = call(
        &app,
        &admin,
        "POST",
        &format!("/refunds/{}/process", id(&r)),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "completed", "{r}");
    assert!(s(&r, "gatewayRefundRef").starts_with("rfnd_Stub"), "{r}");
    assert_eq!(stub.calls("POST /v1/payments/refund"), 1);
    let refunds = stub.state.lock().unwrap().refunds.clone();
    assert_eq!(refunds.len(), 1);
    assert_eq!(refunds[0]["amount"], 600_000, "paise");
    assert_eq!(refunds[0]["payment_id"], g["gatewayPaymentRef"]);

    // Every leg nets to zero: the capture and its refund are mirror entries.
    for key in ["gateway_clearing", "unapplied_receipts", "refunds_payable"] {
        assert_eq!(balance(&pool, inst, key).await, Decimal::ZERO, "{key}");
    }
    ledger_agrees(&pool, inst).await;
    let refund_entries: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE source_type = 'refund' AND source_id = $1::uuid",
    )
    .bind(id(&r))
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(refund_entries >= 1, "refund postings");
    // Nothing left to refund.
    call(
        &app,
        &student,
        "POST",
        "/me/refunds",
        json!({"amount": "1.00", "reason": "again", "paymentId": id(&g)}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
}

// ------------------------------------------------------------------ settlement reconciliation

#[tokio::test]
async fn settlement_report_reconciles_and_flags_mismatches() {
    let stub = RazorpayStub::start().await.unwrap();
    let (app, pool, _) = razorpay_app(T_SETTLE, &stub).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_SETTLE,
        "Razorpay Settlement University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    b5::seed_charge(&pool, &setup, "90000.00", "published", Some(day(12, 31))).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let mut paid = vec![];
    for amt in ["1000.00", "2000.00"] {
        let p = initiate(&app, &student, amt).await;
        stub.pay(&s(&p, "gatewayOrderRef"), Attempt::Captured);
        let p = confirm(&app, &student, &p, json!({}), StatusCode::OK).await;
        assert_eq!(p["status"], "completed");
        paid.push((s(&p, "gatewayPaymentRef"), s(&p, "gatewayOrderRef"), id(&p)));
    }
    let (p1, o1, p1_id) = paid[0].clone();
    let (p2, o2, _) = paid[1].clone();
    // Razorpay settlement recon columns (https://razorpay.com/docs/api/settlements/fetch-recon/).
    let report = format!(
        "entity_id,type,debit,credit,amount,currency,fee,tax,settlement_id,settlement_utr,order_id\n\
         {p1},payment,0,980.00,1000.00,INR,20.00,0.00,setl_T1,UTR1,{o1}\n\
         {p2},payment,0,1959.00,1999.00,INR,40.00,0.00,setl_T1,UTR1,{o2}\n\
         pay_UnknownX01,payment,0,98.00,100.00,INR,2.00,0.00,setl_T1,UTR1,\n\
         {p1},payment,0,980.00,1000.00,INR,20.00,0.00,setl_T1,UTR1,{o1}\n\
         rfnd_UnknownX01,refund,100.00,0,100.00,INR,0,0,setl_T1,UTR1,\n\
         adj_X01,adjustment,0,10.00,10.00,INR,0,0,setl_T1,UTR1,\n"
    );
    let import = |who: &Login, body: String, inst: Uuid| {
        Request::post(format!(
            "{F}/institutions/{inst}/gateway-settlements?amountUnit=rupees&fileName=setl_T1.csv"
        ))
        .header("content-type", "text/csv")
        .header("cookie", &who.cookie)
        .header("x-csrf-token", &who.csrf)
        .header("idempotency-key", Uuid::now_v7().to_string())
        .body(Body::from(body))
        .unwrap()
    };
    let (st, _, v) = send(&app, import(&admin, report.clone(), inst)).await;
    assert_eq!(st, StatusCode::CREATED, "{v}");
    let results: Vec<String> = v["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| s(l, "result"))
        .collect();
    assert_eq!(
        results,
        [
            "matched",
            "amount_mismatch",
            "unknown_payment",
            "duplicate_line",
            "unknown_refund",
            "ignored"
        ]
    );
    assert_eq!(
        (
            v["matchedCount"].as_i64(),
            v["mismatchCount"].as_i64(),
            v["lineCount"].as_i64()
        ),
        (Some(1), Some(4), Some(6))
    );
    assert_eq!(
        (s(&v, "settledTotal"), s(&v, "feeTotal")),
        ("3927.00".into(), "82.00".into())
    );
    assert_eq!(v["lines"][0]["paymentId"], p1_id.as_str());
    assert_eq!(v["alreadyImported"], false);
    assert_eq!(
        audits(&pool, T_SETTLE, "finance.gateway_settlement.import").await,
        1
    );

    // The same file again → the first import, nothing new recorded.
    let (st, _, again) = send(&app, import(&admin, report.clone(), inst)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        (id(&again), again["alreadyImported"].as_bool()),
        (id(&v), Some(true))
    );
    // A later report: p1 again → already settled; p2 at the right amount → matched.
    let later = format!(
        "entity_id,type,amount,fee,credit\n{p1},payment,1000.00,20.00,980.00\n{p2},payment,2000.00,40.00,1960.00\n"
    );
    let (st, _, l) = send(&app, import(&admin, later, inst)).await;
    assert_eq!(st, StatusCode::CREATED, "{l}");
    assert_eq!(l["lines"][0]["result"], "already_settled");
    assert_eq!(l["lines"][1]["result"], "matched");
    let lines: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.gateway_settlement_line WHERE tenant_id = $1::uuid",
    )
    .bind(T_SETTLE)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(lines, 8);
    // Lines are append-only.
    assert!(sqlx::query(
        "UPDATE finance.gateway_settlement_line SET result = 'matched' WHERE tenant_id = $1::uuid"
    )
    .bind(T_SETTLE)
    .execute(&pool)
    .await
    .is_err());
    // Bad content → 422; read back; a student may neither import nor read.
    let (st, _, _) = send(&app, import(&admin, "foo,bar\n1,2\n".into(), inst)).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let got = read(&app, &admin, &format!("/gateway-settlements/{}", id(&v))).await;
    assert_eq!(got["lines"].as_array().unwrap().len(), 6);
    let (st, _, _) = send(&app, import(&student, report.clone(), inst)).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _, _) = send(
        &app,
        get(&student, &format!("{F}/gateway-settlements/{}", id(&v))),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Tenant isolation: another tenant's finance office sees nothing.
    let other_stub = RazorpayStub::start().await.unwrap();
    let (other_app, _, _) = razorpay_app(T_SETTLE_OTHER, &other_stub).await;
    let other_admin = login(&other_app, "admin@sutra.dev").await;
    let (st, _, _) = send(
        &other_app,
        get(&other_admin, &format!("{F}/gateway-settlements/{}", id(&v))),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = send(&other_app, import(&other_admin, report, inst)).await;
    assert!(st.is_client_error(), "foreign institution: {st}");
}

// ------------------------------------------------------------------ wiring

#[tokio::test]
async fn recovery_job_is_wired_with_the_sweepers() {
    fn jobs(extra: &[(&'static str, &'static str)]) -> Vec<String> {
        let mut src: HashMap<&'static str, &'static str> = HashMap::from([
            ("SUTRA_ENV", "test"),
            ("DATABASE_URL", "postgres://sutra:sutra@127.0.0.1:1/sutra"),
            ("SUTRA_DB_ACQUIRE_TIMEOUT_SECS", "1"),
            ("SUTRA_SWEEPERS", "true"),
        ]);
        src.extend(extra.iter().copied());
        let mut ctx = AppContext::from_config(&Config::load(&src).unwrap()).unwrap();
        ctx.take_background().describe()
    }
    let has = |v: Vec<String>| v.iter().any(|j| j == "payment_gateway_recovery");
    let razorpay = [
        ("SUTRA_PAYMENT_GATEWAY", "razorpay"),
        ("SUTRA_RAZORPAY_KEY_ID", "rzp_test_Wiring01"),
        ("SUTRA_RAZORPAY_KEY_SECRET", "wiring-key-secret"),
        (
            "SUTRA_PAYMENT_WEBHOOK_SECRET",
            "wiring-webhook-secret-0123456789abcdef",
        ),
    ];
    assert!(has(jobs(&razorpay)));
    assert!(has(jobs(&[("SUTRA_PAYMENT_GATEWAY", "mock")])));
    assert!(!has(jobs(&[("SUTRA_PAYMENT_GATEWAY", "disabled")])));
    let mut off = razorpay.to_vec();
    off.push(("SUTRA_PAYMENT_RECOVERY_INTERVAL_SECS", "0"));
    assert!(!has(jobs(&off)));
}

// ------------------------------------------------------------------ UI gateway capability

/// Same as `support::app` with `SUTRA_PAYMENT_GATEWAY` set to `gateway` (`mock` / `disabled`).
async fn app_with_gateway(tenant: &'static str, gateway: &'static str) -> Router {
    let url = leak(runtime_url(database_url()));
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", url),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_APPROVAL_MODE", "auto"),
        ("SUTRA_PAYMENT_GATEWAY", gateway),
    ]);
    let cfg = Config::load(&src).expect("config");
    let ctx = AppContext::from_config(&cfg).expect("context");
    assert_eq!(ctx.payment_gateway.name(), gateway);
    router(ctx)
}

#[tokio::test]
async fn gateway_info_tells_the_browser_which_checkout_to_open() {
    let stub = RazorpayStub::start().await.unwrap();
    let (app, _pool, _) = razorpay_app(T_GATEWAY_INFO, &stub).await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let info = read(&app, &student, "/me/payment-gateway").await;
    assert_eq!(info["gateway"], "razorpay", "{info}");
    assert_eq!(info["onlinePayments"], true, "{info}");
    assert_eq!(info["checkoutKeyId"], stub.key_id.as_str(), "{info}");
    assert!(
        !info.to_string().contains(stub.key_secret.as_str())
            && !info.to_string().contains(stub.webhook_secret.as_str()),
        "no secret reaches the browser: {info}"
    );
    // Not signed in → 401; staff without finance.self → 403.
    let (st, _, _) = send(
        &app,
        Request::get(format!("{F}/me/payment-gateway"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
    let admin = login(&app, "admin@sutra.dev").await;
    let (st, _, b) = send(&app, get(&admin, &format!("{F}/me/payment-gateway"))).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");

    // Mock (dev / CI): online payments on, no checkout key (the mock flow confirms server-side).
    let mock = app_with_gateway(T_GATEWAY_INFO, "mock").await;
    let student = login(&mock, "aarav.desai@sutra.dev").await;
    let info = read(&mock, &student, "/me/payment-gateway").await;
    assert_eq!(info["gateway"], "mock", "{info}");
    assert_eq!(info["onlinePayments"], true, "{info}");
    assert!(info["checkoutKeyId"].is_null(), "{info}");
    // Disabled (production default without a gateway): the UI hides "Pay now".
    let off = app_with_gateway(T_GATEWAY_INFO, "disabled").await;
    let student = login(&off, "aarav.desai@sutra.dev").await;
    let info = read(&off, &student, "/me/payment-gateway").await;
    assert_eq!(info["gateway"], "disabled", "{info}");
    assert_eq!(info["onlinePayments"], false, "{info}");
    assert!(info["checkoutKeyId"].is_null(), "{info}");
}
