//! Part 1 · Stage 2 — operational financial controls (S2-ops). Each test seeds its data through the
//! usual auto-approving setup app and then runs the governed steps through the **real composition
//! root with the default configuration** (no `SUTRA_APPROVAL_MODE`, so governed decisions wait for
//! a human) on the same database and tenant:
//!
//! * `vendor_payment_posts_only_after_an_independent_approval` (DEF-J-003) — a vendor payment is
//!   `pending_approval` on create and cannot post; its maker deciding it → 403, a user without an
//!   approve permission → 403; a procurement approver approves → `approved`, recorded (decided by /
//!   at, status history, audit, events) → only then it posts (Dr 2310 / Cr bank); a finance
//!   approver rejects another → `rejected`, never posts, and its allocation is released;
//! * `direct_purchase_limit_binds_the_ordered_value` (DEF-J-005) — the ₹1,00,000 direct-purchase
//!   limit is enforced on the PO total (caller-supplied prices), on the quotation and award of a
//!   direct sourcing event and on PO amendments; above it only the competitive route (RFQ →
//!   evaluation → award) orders; an amended order goes back to `draft` and is re-issued only
//!   through an independent PO approval;
//! * `library_fine_waiver_is_decided_by_another_authorised_user` (DEF-G-002) — a librarian's waiver
//!   is only a request (`waiver_requested`); the requester deciding it → 403, a user without
//!   `library.fine.waive.approve` → 403; a finance approver (checker) reads the fine and approves →
//!   `waived` (audited with the approver), or rejects → `assessed` (billable again).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::{Duration, NaiveDate};
use serde_json::{json, Value};
use std::collections::HashMap;
use support::b8;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_contracts::resources::status::ItemKind;
use sutra_kernel::approval::ApprovalMode;

const T_PAY: &str = "0192f5a0-7c3e-7000-8000-0000000c0e01";
const T_LIMIT: &str = "0192f5a0-7c3e-7000-8000-0000000c0e02";
const T_FINE: &str = "0192f5a0-7c3e-7000-8000-0000000c0e03";

const R: &str = "/api/v1/resources";
const C: &str = "/api/v1/campus";

const ADMIN: &str = "admin@sutra.dev";
const FACULTY: &str = "meera.shah@sutra.dev";
const PROCUREMENT_APPROVER: &str = "procurement.approver@sutra.dev";
const FINANCE_APPROVER: &str = "finance.approver@sutra.dev";

/// The API exactly as production composes it, with the **default** approval mode (human), in
/// development auth so the dev personas can sign in (same database / runtime role as
/// `support::app`).
async fn human_app(tenant: &'static str) -> Router {
    let _ = owner_pool().await; // migrations + runtime role
    let url = database_url();
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let runtime: &'static str =
        Box::leak(format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}").into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", runtime),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
    ]);
    let cfg = Config::load(&src).expect("config");
    assert_eq!(
        cfg.hardening.workflow.approvals,
        ApprovalMode::AwaitDecision,
        "the default approval mode is a human decision"
    );
    router(AppContext::from_config(&cfg).expect("context"))
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
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

/// `(actor_id, details)` of the succeeded audit events `action` on `target_id`.
async fn audit_events(
    pool: &sqlx::PgPool,
    tenant: &str,
    action: &str,
    target_id: &str,
) -> Vec<(Option<String>, Value)> {
    sqlx::query_as(
        "SELECT actor_id, details FROM platform.audit_log WHERE tenant_id = $1::uuid AND action = $2 \
         AND target_id = $3 AND outcome = 'succeeded' ORDER BY occurred_at",
    )
    .bind(tenant)
    .bind(action)
    .bind(target_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn events(pool: &sqlx::PgPool, event_type: &str, subject: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(event_type)
    .bind(subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `(from, to, actor)` rows of an aggregate's status history, oldest first.
async fn history(
    pool: &sqlx::PgPool,
    table: &str,
    aggregate_type: &str,
    id: &str,
) -> Vec<(Option<String>, String, Option<String>)> {
    sqlx::query_as(&format!(
        "SELECT from_status, to_status, actor FROM {table} WHERE aggregate_type = $1 \
         AND aggregate_id = $2::uuid ORDER BY created_at, id"
    ))
    .bind(aggregate_type)
    .bind(id)
    .fetch_all(pool)
    .await
    .unwrap()
}

/// Supplier through create → register → submit (auto-approval → active).
async fn active_supplier(app: &Router, admin: &Login, inst: &str, name: &str, pan: &str) -> String {
    let s = call(
        app,
        admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": name, "pan": pan, "email": "sales@example.in",
            "bankAccountName": name, "bankAccountNumber": "001234567890", "bankIfsc": "HDFC0001234"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&s);
    let s = call(
        app,
        admin,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = call(
        app,
        admin,
        "POST",
        &format!("{R}/suppliers/{sid}/submit"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "active", "{s}");
    sid
}

/// Requisition through create → submit (auto-approval → released).
async fn released_requisition(app: &Router, admin: &Login, inst: &str, lines: Value) -> Value {
    let r = call(
        app,
        admin,
        "POST",
        &format!("{R}/institutions/{inst}/requisitions"),
        json!({"title": "Department purchase", "justification": "Term supplies", "lines": lines}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let r = call(
        app,
        admin,
        "POST",
        &format!("{R}/requisitions/{}/submit", id(&r)),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "released", "{r}");
    r
}

// ============================================================================ DEF-J-003

#[tokio::test]
async fn vendor_payment_posts_only_after_an_independent_approval() {
    let (setup_app, pool) = app(T_PAY).await;
    let admin = login(&setup_app, ADMIN).await;
    let setup = b8::campus_setup(
        &setup_app,
        &admin,
        &pool,
        T_PAY,
        "Payment Control University",
    )
    .await;
    let inst = setup.institution().as_uuid().to_string();
    let paper = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, None).await;
    let store = b8::seed_store(&pool, &setup, "CENTRAL", None).await;
    let sup = active_supplier(&setup_app, &admin, &inst, "Acme Traders", "ABCDE1234F").await;

    // A posted invoice of ₹10,000 (direct order, receipt, three-way match, AP posting).
    let req = released_requisition(
        &setup_app,
        &admin,
        &inst,
        json!([{"itemId": paper, "quantity": "100", "estimatedUnitCost": "100.00"}]),
    )
    .await;
    let po = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"requisitionId": id(&req), "supplierId": sup, "deliveryLocationId": store}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let po_id = id(&po);
    call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        Some(ver(&po)),
        StatusCode::OK,
    )
    .await;
    let day = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
    let grn = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/receipts"),
        json!({"receivedOn": day.to_string(), "deliveryNote": "DN-1", "lines": [{"lineNo": 1, "quantity": "100"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/goods-receipts/{}/accept", id(&grn)),
        json!({}),
        Some(ver(&grn)),
        StatusCode::OK,
    )
    .await;
    let inv = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-1", "invoiceDate": day.to_string(),
            "dueDate": (day + Duration::days(30)).to_string(),
            "lines": [{"lineNo": 1, "quantity": "100", "unitPrice": "100.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let inv_id = id(&inv);
    let inv = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/match"),
        json!({}),
        Some(ver(&inv)),
        StatusCode::OK,
    )
    .await;
    let inv = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/post"),
        json!({}),
        Some(ver(&inv)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(inv["status"], "posted");

    // ---- the governed part, through the default-configured API.
    let api = human_app(T_PAY).await;
    let maker = login(&api, ADMIN).await;
    let checker = login(&api, PROCUREMENT_APPROVER).await;
    let finance = login(&api, FINANCE_APPROVER).await;
    let faculty = login(&api, FACULTY).await;

    // A checker holds the approve permission only: it cannot raise the payment it decides.
    call(
        &api,
        &checker,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": sup, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": inv_id, "amount": "100.00"}]}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;

    // Pending until decided: no posting, no journal.
    let p1 = call(
        &api,
        &maker,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": sup, "paymentDate": "2026-11-30", "reference": "NEFT-1",
            "allocations": [{"vendorInvoiceId": inv_id, "amount": "6000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p1_id = id(&p1);
    assert_eq!(p1["status"], "pending_approval", "{p1}");
    assert!(p1["processInstanceId"].is_string());
    assert!(p1["approvalRequestedBy"].as_str().unwrap().ends_with(ADMIN));
    assert!(p1["journalEntryId"].is_null());
    assert_eq!(
        events(&pool, "sutraerp.resources.vendor_payment.submitted", &p1_id).await,
        1
    );
    let (s, _, b) = send(
        &api,
        write(
            &maker,
            "POST",
            &format!("{R}/vendor-payments/{p1_id}/post"),
            json!({}),
            Some(ver(&p1)),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a pending payment never posts: {b}"
    );

    // The maker deciding their own payment → 403; a user without an approve permission → 403.
    for who in [&maker, &faculty] {
        let (s, _, b) = send(
            &api,
            write(
                who,
                "POST",
                &format!("{R}/vendor-payments/{p1_id}/decision"),
                json!({"decision": "approve"}),
                Some(ver(&p1)),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    }
    let still = read(&api, &maker, &format!("{R}/vendor-payments/{p1_id}")).await;
    assert_eq!(still["status"], "pending_approval");
    let journals = |pool: sqlx::PgPool, inst: String| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM finance.journal_entry WHERE institution_id = $1::uuid \
             AND source_type = 'procurement_payment'",
        )
        .bind(inst)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(
        journals(pool.clone(), inst.clone()).await,
        0,
        "nothing booked yet"
    );

    // An independent procurement approver approves → `approved`, recorded.
    let p1 = call(
        &api,
        &checker,
        "POST",
        &format!("{R}/vendor-payments/{p1_id}/decision"),
        json!({"decision": "approve", "reason": "invoice verified"}),
        Some(ver(&still)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "approved", "{p1}");
    assert!(p1["decidedBy"]
        .as_str()
        .unwrap()
        .ends_with(PROCUREMENT_APPROVER));
    assert!(p1["decidedAt"].is_string());
    assert_eq!(p1["decisionReason"], "invoice verified");
    assert_eq!(
        events(&pool, "sutraerp.resources.vendor_payment.approved", &p1_id).await,
        1
    );
    let h = history(&pool, "resources.status_history", "vendor_payment", &p1_id).await;
    assert_eq!(h.len(), 2, "{h:?}");
    assert_eq!(
        (h[0].0.as_deref(), h[0].1.as_str()),
        (None, "pending_approval")
    );
    assert!(h[0].2.as_deref().unwrap().ends_with(ADMIN));
    assert_eq!(
        (h[1].0.as_deref(), h[1].1.as_str()),
        (Some("pending_approval"), "approved")
    );
    assert!(h[1].2.as_deref().unwrap().ends_with(PROCUREMENT_APPROVER));
    let audit = audit_events(&pool, T_PAY, "resources.vendor_payment.decide", &p1_id).await;
    assert_eq!(audit.len(), 1);
    assert!(audit[0]
        .0
        .as_deref()
        .unwrap()
        .ends_with(PROCUREMENT_APPROVER));
    assert_eq!(audit[0].1["decision"], "approve");
    // Already decided → 409.
    let (s, _, _) = send(
        &api,
        write(
            &checker,
            "POST",
            &format!("{R}/vendor-payments/{p1_id}/decision"),
            json!({"decision": "approve"}),
            Some(ver(&p1)),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Only now it posts (Dr 2310 / Cr bank).
    let p1 = call(
        &api,
        &maker,
        "POST",
        &format!("{R}/vendor-payments/{p1_id}/post"),
        json!({}),
        Some(ver(&p1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "posted");
    assert_eq!(journals(pool.clone(), inst.clone()).await, 1);
    let inv = read(&api, &maker, &format!("{R}/vendor-invoices/{inv_id}")).await;
    assert_eq!(inv["paidAmount"], "6000.00");

    // A finance approver rejects the next one → `rejected`; it never posts; its allocation is
    // released for a new request.
    let p2 = call(
        &api,
        &maker,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": sup, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": inv_id, "amount": "4000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p2_id = id(&p2);
    call(
        &api,
        &maker,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": sup, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": inv_id, "amount": "0.01"}]}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await; // the pending payment holds the rest of the invoice
    call(
        &api,
        &finance,
        "POST",
        &format!("{R}/vendor-payments/{p2_id}/decision"),
        json!({"decision": "reject"}),
        Some(ver(&p2)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await; // a rejection needs a reason
    let p2 = call(
        &api,
        &finance,
        "POST",
        &format!("{R}/vendor-payments/{p2_id}/decision"),
        json!({"decision": "reject", "reason": "duplicate of NEFT-1"}),
        Some(ver(&p2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p2["status"], "rejected");
    assert!(p2["decidedBy"]
        .as_str()
        .unwrap()
        .ends_with(FINANCE_APPROVER));
    assert_eq!(
        events(&pool, "sutraerp.resources.vendor_payment.rejected", &p2_id).await,
        1
    );
    let (s, _, _) = send(
        &api,
        write(
            &maker,
            "POST",
            &format!("{R}/vendor-payments/{p2_id}/post"),
            json!({}),
            Some(ver(&p2)),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a rejected payment never posts");
    assert_eq!(journals(pool.clone(), inst.clone()).await, 1);
    let p3 = call(
        &api,
        &maker,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": sup, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": inv_id, "amount": "4000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(p3["status"], "pending_approval");
}

// ============================================================================ DEF-J-005

#[tokio::test]
async fn direct_purchase_limit_binds_the_ordered_value() {
    let (app, pool) = app(T_LIMIT).await;
    let admin = login(&app, ADMIN).await;
    let setup = b8::campus_setup(&app, &admin, &pool, T_LIMIT, "Purchase Limit University").await;
    let inst = setup.institution().as_uuid().to_string();
    let acme = active_supplier(&app, &admin, &inst, "Acme Traders", "ABCDE1234F").await;
    let beta = active_supplier(&app, &admin, &inst, "Beta Supplies", "BCDEF2345G").await;
    let desks =
        json!([{"description": "Lab desks", "quantity": "10", "estimatedUnitCost": "9000.00"}]);

    // ---- direct order straight from a requisition estimated at ₹90,000 (within the limit).
    let r1 = released_requisition(&app, &admin, &inst, desks.clone()).await;
    let (s, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{R}/institutions/{inst}/purchase-orders"),
            json!({"requisitionId": id(&r1), "supplierId": acme, "prices": [{"lineNo": 1, "unitPrice": "12000.00"}]}),
            None,
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "the PO total (₹1,20,000) is capped, not the estimate: {b}"
    );
    assert!(
        b["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("100000.00"),
        "{b}"
    );
    let po = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"requisitionId": id(&r1), "supplierId": acme, "prices": [{"lineNo": 1, "unitPrice": "9500.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(po["totalAmount"], "95000.00");
    let po_id = id(&po);
    let po = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        Some(ver(&po)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "issued");
    // An amendment never lifts a direct order over the limit.
    for change in [
        json!({"lineNo": 1, "quantity": "11"}),
        json!({"lineNo": 1, "unitPrice": "10500.00"}),
    ] {
        let (s, _, b) = send(
            &app,
            write(
                &admin,
                "POST",
                &format!("{R}/purchase-orders/{po_id}/amend"),
                json!({"lines": [change], "reason": "more desks"}),
                Some(ver(&po)),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{b}");
    }
    let po = read(&app, &admin, &format!("{R}/purchase-orders/{po_id}")).await;
    assert_eq!(
        (po["status"].as_str(), po["totalAmount"].as_str()),
        (Some("issued"), Some("95000.00"))
    );
    // Within the limit the amendment goes back to draft for re-approval; through the
    // default-configured API the re-issue waits for an independent approver.
    let po = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/amend"),
        json!({"lines": [{"lineNo": 1, "unitPrice": "9900.00"}], "reason": "price revision"}),
        Some(ver(&po)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (po["status"].as_str(), po["revision"].as_i64()),
        (Some("draft"), Some(2))
    );
    assert_eq!(po["totalAmount"], "99000.00");
    let api = human_app(T_LIMIT).await;
    let maker = login(&api, ADMIN).await;
    let checker = login(&api, PROCUREMENT_APPROVER).await;
    let po = call(
        &api,
        &maker,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        Some(ver(&po)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (po["status"].as_str(), po["approvalPending"].as_bool()),
        (Some("draft"), Some(true))
    );
    let (s, _, _) = send(
        &api,
        write(
            &maker,
            "POST",
            &format!("{R}/purchase-orders/{po_id}/decision"),
            json!({"decision": "approve"}),
            Some(ver(&po)),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the amender does not re-approve");
    let po = call(
        &api,
        &checker,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&po)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "issued");

    // ---- direct sourcing event: the quotation (and the award) are capped too.
    let r2 = released_requisition(&app, &admin, &inst, desks.clone()).await;
    let ev = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": id(&r2), "method": "direct", "title": "Desks"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let ev_id = id(&ev);
    let (s, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{R}/sourcing-events/{ev_id}/quotations"),
            json!({"supplierId": beta, "lines": [{"lineNo": 1, "unitPrice": "10500.00"}]}),
            Some(ver(&ev)),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a ₹1,05,000 quotation on a direct event: {b}"
    );
    let ev = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/quotations"),
        json!({"supplierId": beta, "lines": [{"lineNo": 1, "unitPrice": "9800.00"}]}),
        Some(ver(&ev)),
        StatusCode::CREATED,
    )
    .await;
    let q = ev["quotations"][0]["id"].as_str().unwrap().to_owned();
    let ev = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/award"),
        json!({"quotationId": q}),
        Some(ver(&ev)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ev["status"], "awarded");
    let po2 = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"sourcingEventId": ev_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(po2["totalAmount"], "98000.00");

    // ---- above the limit only the competitive route orders (RFQ → evaluation → award).
    let r3 = released_requisition(
        &app,
        &admin,
        &inst,
        json!([{"description": "Smart boards", "quantity": "3", "estimatedUnitCost": "45000.00"}]),
    )
    .await;
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{R}/institutions/{inst}/purchase-orders"),
            json!({"requisitionId": id(&r3), "supplierId": acme}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no direct order above the limit");
    let rfq = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": id(&r3), "method": "rfq", "title": "Smart boards"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rfq_id = id(&rfq);
    let rfq = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/quotations"),
        json!({"supplierId": acme, "lines": [{"lineNo": 1, "unitPrice": "44000.00"}]}),
        Some(ver(&rfq)),
        StatusCode::CREATED,
    )
    .await;
    let rfq = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/quotations"),
        json!({"supplierId": beta, "lines": [{"lineNo": 1, "unitPrice": "46000.00"}]}),
        Some(ver(&rfq)),
        StatusCode::CREATED,
    )
    .await;
    let rfq = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/evaluate"),
        json!({}),
        Some(ver(&rfq)),
        StatusCode::OK,
    )
    .await;
    let lowest = rfq["quotations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|q| q["totalAmount"] == "132000.00")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let rfq = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/award"),
        json!({"quotationId": lowest}),
        Some(ver(&rfq)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rfq["status"], "awarded");
    let po3 = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"sourcingEventId": rfq_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        po3["totalAmount"], "132000.00",
        "a competitive award may exceed the limit"
    );
}

// ============================================================================ DEF-G-002

#[tokio::test]
async fn library_fine_waiver_is_decided_by_another_authorised_user() {
    let (setup_app, pool) = app(T_FINE).await;
    let admin = login(&setup_app, ADMIN).await;
    let setup =
        b8::campus_setup(&setup_app, &admin, &pool, T_FINE, "Fine Control University").await;
    let inst = setup.institution().as_uuid();
    let today: NaiveDate = sqlx::query_scalar(
        "SELECT (now() AT TIME ZONE timezone)::date FROM institution.institution WHERE id = $1",
    )
    .bind(inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    let student = b8::seed_student(&pool, &setup, "Ishaan", None).await;

    // Active policy (auto-approved in the setup app), membership, two late returns → two fines.
    let p = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-policies"),
        json!({"name": "Rules", "effectiveFrom": "2026-01-01", "loanDaysStudent": 14, "loanDaysStaff": 30,
            "maxLoansStudent": 2, "maxLoansStaff": 5, "maxRenewals": 1, "finePerDay": "3.00",
            "maxFine": "50.00", "reservationHoldDays": 3, "lostChargeFactor": "1.5"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let pid = id(&p);
    let p = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/submit"),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    call(
        &setup_app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/activate"),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    let m = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-memberships"),
        json!({"personKind": "student", "personId": student.student_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let mid = id(&m);
    let mut loans = Vec::new();
    for title in ["Optics", "Acoustics"] {
        let r = call(
            &setup_app,
            &admin,
            "POST",
            &format!("{C}/institutions/{inst}/catalogue"),
            json!({"title": title}),
            None,
            StatusCode::CREATED,
        )
        .await;
        let rid = id(&r);
        call(
            &setup_app,
            &admin,
            "POST",
            &format!("{C}/catalogue-records/{rid}/publish"),
            json!({}),
            Some(ver(&r)),
            StatusCode::OK,
        )
        .await;
        let c = call(
            &setup_app,
            &admin,
            "POST",
            &format!("{C}/catalogue-records/{rid}/copies"),
            json!({}),
            None,
            StatusCode::CREATED,
        )
        .await;
        let l = call(
            &setup_app,
            &admin,
            "POST",
            &format!("{C}/institutions/{inst}/library-loans"),
            json!({"membershipId": mid, "copyId": id(&c)}),
            None,
            StatusCode::CREATED,
        )
        .await;
        loans.push(id(&l));
    }
    for lid in &loans {
        sqlx::query(
            "UPDATE campus.library_loan SET issued_on = $2, due_on = $3 WHERE id = $1::uuid",
        )
        .bind(lid)
        .bind(today - Duration::days(16))
        .bind(today - Duration::days(2))
        .execute(&pool)
        .await
        .unwrap();
        let l = read(&setup_app, &admin, &format!("{C}/library-loans/{lid}")).await;
        let l = call(
            &setup_app,
            &admin,
            "POST",
            &format!("{C}/library-loans/{lid}/return"),
            json!({}),
            Some(ver(&l)),
            StatusCode::OK,
        )
        .await;
        assert_eq!(l["fineAmount"], "6.00", "{l}");
    }
    let list = read(
        &setup_app,
        &admin,
        &format!("{C}/institutions/{inst}/library-fines?membershipId={mid}"),
    )
    .await;
    let fines: Vec<Value> = list["items"].as_array().unwrap().clone();
    assert_eq!(fines.len(), 2);

    // ---- governed part, through the default-configured API.
    let api = human_app(T_FINE).await;
    let librarian = login(&api, ADMIN).await;
    let checker = login(&api, FINANCE_APPROVER).await;
    let faculty = login(&api, FACULTY).await;
    let procurement = login(&api, PROCUREMENT_APPROVER).await;

    // A checker cannot raise the waiver it decides (no campus.library.manage).
    let f1 = &fines[0];
    let f1_id = id(f1);
    let (s, _, _) = send(
        &api,
        write(
            &checker,
            "POST",
            &format!("{C}/library-fines/{f1_id}/waiver"),
            json!({"reason": "Medical leave"}),
            Some(ver(f1)),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // The librarian's waiver is only a request.
    let f1 = call(
        &api,
        &librarian,
        "POST",
        &format!("{C}/library-fines/{f1_id}/waiver"),
        json!({"reason": "Medical leave"}),
        Some(ver(f1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f1["status"], "waiver_requested", "{f1}");
    assert_eq!(
        events(&pool, "sutraerp.campus.library_fine.waived", &f1_id).await,
        0,
        "not waived by the request"
    );

    // The requester deciding → 403; users without library.fine.waive.approve → 403.
    for who in [&librarian, &faculty, &procurement] {
        let (s, _, b) = send(
            &api,
            write(
                who,
                "POST",
                &format!("{C}/library-fines/{f1_id}/decision"),
                json!({"decision": "approve", "reason": "ok"}),
                Some(ver(&f1)),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    }
    // The checker reads the fine it decides and approves → waived, audited with the approver.
    let seen = read(&api, &checker, &format!("{C}/library-fines/{f1_id}")).await;
    assert_eq!(seen["status"], "waiver_requested");
    let f1 = call(
        &api,
        &checker,
        "POST",
        &format!("{C}/library-fines/{f1_id}/decision"),
        json!({"decision": "approve", "reason": "Certificate seen"}),
        Some(ver(&seen)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f1["status"], "waived");
    assert_eq!(
        events(&pool, "sutraerp.campus.library_fine.waived", &f1_id).await,
        1
    );
    let audit = audit_events(&pool, T_FINE, "campus.library_fine.waive", &f1_id).await;
    assert_eq!(audit.len(), 1, "{audit:?}");
    assert!(audit[0].0.as_deref().unwrap().ends_with(FINANCE_APPROVER));
    let requested = audit_events(&pool, T_FINE, "campus.library_fine.request_waiver", &f1_id).await;
    assert_eq!(requested.len(), 1);
    assert!(requested[0].0.as_deref().unwrap().ends_with(ADMIN));
    let h = history(&pool, "campus.status_history", "library_fine", &f1_id).await;
    let last = h.last().unwrap();
    assert_eq!(
        (last.0.as_deref(), last.1.as_str()),
        (Some("waiver_requested"), "waived")
    );
    assert!(last.2.as_deref().unwrap().ends_with(FINANCE_APPROVER));

    // A rejected waiver leaves the fine billable.
    let f2 = &fines[1];
    let f2_id = id(f2);
    let f2 = call(
        &api,
        &librarian,
        "POST",
        &format!("{C}/library-fines/{f2_id}/waiver"),
        json!({"reason": "Forgot"}),
        Some(ver(f2)),
        StatusCode::OK,
    )
    .await;
    let f2 = call(
        &api,
        &checker,
        "POST",
        &format!("{C}/library-fines/{f2_id}/decision"),
        json!({"decision": "reject", "reason": "No ground"}),
        Some(ver(&f2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f2["status"], "assessed");
}
