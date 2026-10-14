//! Batch 8 slice 8D — procurement, inventory, assets and Finance AP (Domain J) against
//! PostgreSQL through the HTTP API:
//!
//! * `procure_to_pay_end_to_end` — suppliers (J-020 registration with GSTIN / PAN checks and
//!   masked bank details, J-021 approval → active, J-030 suspension → restore, J-035 duplicate →
//!   merged onto the canonical record), requisition → released (J-038), RFQ with quotations →
//!   evaluation → award with justification (J-004), purchase order from the award → issued →
//!   acknowledged → amended → re-issued (J-043 … 045), goods receipts with over-receipt 422,
//!   inspection, partial acceptance → `receipt` movements (J-049, 054, 057, 058), asset
//!   registration from the receipt + manual, assignment / return / condition / repair /
//!   retirement (J-074 … 076, 079, 083), vendor invoice three-way match (mismatch blocks
//!   posting, F-081) → approval → AP posting through `ProcurementPostingPort` with idempotent
//!   replay (F-044 / F-082), purchase order closure (J-047), credit note, vendor payments
//!   (F-045) and the Finance journal lines behind them;
//! * `inventory_documents_and_replenishment` — item master + stores / locations (J-051, 053),
//!   issues with insufficient stock 409 + returns to store (J-064, 066), reservations (J-065),
//!   approved adjustments (J-059), reorder-level run → replenishment need + draft requisition
//!   (J-061), movement list;
//! * `sourcing_variants_and_cancellations` — direct / simplified sourcing (J-007), direct order
//!   from a requisition, order cancellation → requisition released again (J-046), sourcing /
//!   requisition cancellation (J-041), value limits;
//! * `governed_decisions_await_a_decision` — `ApprovalMode::AwaitDecision` through
//!   `sutra_resources::router_with_state`: requisition submit → `purchase_requisition.submitted`
//!   → reject (J-039) → amend (J-040) → withdraw (J-041) → resubmit → approve; supplier
//!   approval; purchase order approval; stock adjustment approval; asset retirement rejected;
//! * `permissions_and_preconditions` — reads / writes need `resources.*` (student → 403),
//!   unknown ids → 404, `If-Match` 428 / 412, validation 422.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b8;
use support::*;
use sutra_contracts::finance::ports::{ProcurementPostingKind, ProcurementPostingRequest};
use sutra_contracts::resources::status::ItemKind;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_resources::shared::approval::ApprovalMode;
use uuid::Uuid;

const TENANT_FLOW: &str = "0192f5a0-7c3e-7000-8000-0000000b8d00";
const TENANT_STOCK: &str = "0192f5a0-7c3e-7000-8000-0000000b8d01";
const TENANT_SOURCING: &str = "0192f5a0-7c3e-7000-8000-0000000b8d02";
const TENANT_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b8d03";
const TENANT_PERM: &str = "0192f5a0-7c3e-7000-8000-0000000b8d04";
const R: &str = "/api/v1/resources";

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
    v["id"].as_str().unwrap_or_default().to_owned()
}

fn ver(v: &Value) -> Option<i64> {
    v["version"].as_i64()
}

async fn events(pool: &sqlx::PgPool, subject: &str, event_type: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE subject = $1 AND event_type = $2",
    )
    .bind(subject)
    .bind(event_type)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// On hand of an item at a location through the foundation stock-on-hand API ("0.000" when
/// absent).
async fn on_hand(app: &Router, who: &Login, inst: &str, item: Uuid, location: Uuid) -> String {
    let b = fetch(
        app,
        who,
        &format!("{R}/institutions/{inst}/stock-on-hand?itemId={item}&locationId={location}"),
    )
    .await;
    b["items"]
        .as_array()
        .and_then(|a| a.first())
        .and_then(|l| l["onHand"].as_str())
        .unwrap_or("0.000")
        .to_owned()
}

/// Journal lines of a Finance entry as `(account code, debit, credit)`.
async fn journal(pool: &sqlx::PgPool, entry: &str) -> Vec<(String, String, String)> {
    let rows: Vec<(String, rust_decimal::Decimal, rust_decimal::Decimal)> = sqlx::query_as(
        "SELECT a.code, l.debit, l.credit FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE l.journal_entry_id = $1::uuid ORDER BY l.line_no",
    )
    .bind(entry)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(c, d, k)| (c, format!("{d:.2}"), format!("{k:.2}")))
        .collect()
}

/// A supplier through create → register → submit (auto-approval → active).
async fn active_supplier(
    app: &Router,
    admin: &Login,
    inst: &str,
    name: &str,
    pan: &str,
    gstin: Option<&str>,
) -> Value {
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": name, "pan": pan, "gstin": gstin, "email": "sales@example.in",
            "bankAccountName": name, "bankAccountNumber": "001234567890", "bankIfsc": "HDFC0001234"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&s);
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{R}/suppliers/{sid}/submit"),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "active", "{s}");
    s
}

/// A requisition through create → submit (auto-approval → released).
async fn released_requisition(app: &Router, admin: &Login, inst: &str, lines: Value) -> Value {
    let r = ok(
        app,
        admin,
        "POST",
        &format!("{R}/institutions/{inst}/requisitions"),
        json!({"title": "Department purchase", "justification": "Term supplies", "lines": lines}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let r = ok(
        app,
        admin,
        "POST",
        &format!("{R}/requisitions/{}/submit", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "released", "{r}");
    r
}

// ================================================================================= test 1

#[tokio::test]
async fn procure_to_pay_end_to_end() {
    let (app, pool) = app(TENANT_FLOW).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_FLOW, "Procurement University").await;
    let inst = setup.institution().as_uuid().to_string();
    let paper = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, Some("12")).await;
    let laptop = b8::seed_item(&pool, &setup, "LAPTOP", ItemKind::Asset, None).await;
    let store = b8::seed_store(&pool, &setup, "CENTRAL", None).await;
    let meera = b8::seed_staff(&pool, &setup, "Meera", None).await;

    // ---- suppliers: registration checks, approval, duplicates, suspension (J-020/021/030/035).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Bad GST", "gstin": "27ABCDE1234F1X5"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let acme = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Acme Traders", "gstin": "27abcde1234f1z5", "pan": "ABCDE1234F",
            "bankAccountName": "Acme Traders", "bankAccountNumber": "1234567890", "bankIfsc": "HDFC0001234"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(acme["status"], "prospective");
    assert_eq!(acme["number"], "SUP-000001");
    assert_eq!(acme["gstin"], "27ABCDE1234F1Z5", "normalised to upper case");
    assert_eq!(
        acme["bankAccountNumber"], "XXXXXX7890",
        "bank details masked"
    );
    let acme_id = id(&acme);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{acme_id}/submit"),
        json!({}),
        ver(&acme),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a prospective supplier is registered first"
    );
    let acme = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{acme_id}/register"),
        json!({"email": "sales@acme.in"}),
        ver(&acme),
        StatusCode::OK,
    )
    .await;
    assert_eq!(acme["status"], "registered");
    let acme = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{acme_id}/submit"),
        json!({}),
        ver(&acme),
        StatusCode::OK,
    )
    .await;
    assert_eq!(acme["status"], "active");
    assert_eq!(acme["approvalPending"], false);
    assert!(acme["processInstanceId"].is_string());
    assert_eq!(
        events(&pool, &acme_id, "sutraerp.resources.supplier.approved").await,
        1
    );
    let dup = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Acme Traders Pvt", "gstin": "27ABCDE1234F1Z5",
            "bankAccountNumber": "99887766", "bankIfsc": "ICIC0000001"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let dup_id = id(&dup);
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{dup_id}/register"),
        json!({}),
        ver(&dup),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "same GSTIN is a duplicate: {b}");
    assert!(b.to_string().contains("SUP-000001"), "{b}");
    let dup = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{dup_id}/merge"),
        json!({"canonicalSupplierId": acme_id, "reason": "duplicate registration"}),
        ver(&dup),
        StatusCode::OK,
    )
    .await;
    assert_eq!(dup["status"], "merged");
    assert_eq!(dup["canonicalSupplierId"], acme_id.as_str());
    let beta = active_supplier(&app, &admin, &inst, "Beta Supplies", "BCDEF2345G", None).await;
    let beta_id = id(&beta);
    let beta = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{beta_id}/suspend"),
        json!({"reason": "late deliveries under review"}),
        ver(&beta),
        StatusCode::OK,
    )
    .await;
    assert_eq!(beta["status"], "suspended");
    assert_eq!(
        events(&pool, &beta_id, "sutraerp.resources.supplier.suspended").await,
        1
    );

    // ---- requisition → released (J-038).
    let req = released_requisition(
        &app,
        &admin,
        &inst,
        json!([
            {"itemId": paper, "quantity": "20", "estimatedUnitCost": "100.00"},
            {"itemId": laptop, "quantity": "2", "estimatedUnitCost": "40000.00"},
            {"description": "Installation service", "quantity": "1", "estimatedUnitCost": "5000.00"}
        ]),
    )
    .await;
    let req_id = id(&req);
    assert_eq!(req["number"], "PR-000001");
    assert_eq!(req["estimatedTotal"], "87000.00");
    assert_eq!(req["lines"].as_array().unwrap().len(), 3);
    assert_eq!(req["lines"][1]["description"], "Item LAPTOP");
    assert_eq!(
        events(
            &pool,
            &req_id,
            "sutraerp.resources.purchase_requisition.approved"
        )
        .await,
        1
    );
    assert_eq!(
        events(
            &pool,
            &req_id,
            "sutraerp.resources.purchase_requisition.submitted"
        )
        .await,
        0,
        "auto-approval emits no awaiting-decision event"
    );

    // ---- RFQ → quotations → evaluation → award (J-004).
    let rfq = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": req_id, "method": "rfq"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rfq_id = id(&rfq);
    assert_eq!(rfq["status"], "open");
    assert_eq!(rfq["number"], "RFQ-000001");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": req_id, "method": "rfq"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "one live sourcing event per requisition"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/quotations"),
        json!({"supplierId": beta_id, "lines": [{"lineNo": 1, "unitPrice": "98.00"}, {"lineNo": 2, "unitPrice": "38500.00"}, {"lineNo": 3, "unitPrice": "5500.00"}]}),
        ver(&rfq),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a suspended supplier cannot quote");
    let beta = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{beta_id}/restore"),
        json!({"reason": "review closed"}),
        ver(&beta),
        StatusCode::OK,
    )
    .await;
    assert_eq!(beta["status"], "active");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/quotations"),
        json!({"supplierId": dup_id, "lines": [{"lineNo": 1, "unitPrice": "90.00"}, {"lineNo": 2, "unitPrice": "38000.00"}, {"lineNo": 3, "unitPrice": "5000.00"}]}),
        ver(&rfq),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a merged duplicate cannot quote");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/quotations"),
        json!({"supplierId": acme_id, "lines": [{"lineNo": 1, "unitPrice": "95.00"}]}),
        ver(&rfq),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "every line is priced");
    let rfq = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/quotations"),
        json!({"supplierId": acme_id, "reference": "Q-ACME-1", "deliveryDays": 5,
            "lines": [{"lineNo": 1, "unitPrice": "95.00"}, {"lineNo": 2, "unitPrice": "39000.00"}, {"lineNo": 3, "unitPrice": "5000.00"}]}),
        ver(&rfq),
        StatusCode::CREATED,
    )
    .await;
    let rfq = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/quotations"),
        json!({"supplierId": beta_id, "reference": "Q-BETA-1", "deliveryDays": 15,
            "lines": [{"lineNo": 1, "unitPrice": "98.00"}, {"lineNo": 2, "unitPrice": "38500.00"}, {"lineNo": 3, "unitPrice": "5500.00"}]}),
        ver(&rfq),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(rfq["quotations"].as_array().unwrap().len(), 2);
    let rfq = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/evaluate"),
        json!({}),
        ver(&rfq),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rfq["status"], "evaluating");
    assert_eq!(
        rfq["quotations"][0]["supplierId"],
        beta_id.as_str(),
        "lowest first"
    );
    assert_eq!(rfq["quotations"][0]["rank"], 1);
    assert_eq!(rfq["quotations"][0]["totalAmount"], "84460.00");
    assert_eq!(rfq["quotations"][1]["totalAmount"], "84900.00");
    let acme_q = rfq["quotations"][1]["id"].as_str().unwrap().to_owned();
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/award"),
        json!({"quotationId": acme_q}),
        ver(&rfq),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "not the lowest → justification"
    );
    let rfq = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{rfq_id}/award"),
        json!({"quotationId": acme_q, "reason": "delivery in 5 days for the term start"}),
        ver(&rfq),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rfq["status"], "awarded");
    assert_eq!(rfq["awardedSupplierId"], acme_id.as_str());
    assert_eq!(rfq["quotations"][1]["status"], "accepted");
    assert_eq!(rfq["quotations"][0]["status"], "rejected");
    assert_eq!(
        events(&pool, &rfq_id, "sutraerp.resources.sourcing_event.awarded").await,
        1
    );

    // ---- purchase order: issue, acknowledge, amend, re-issue (J-043/044/045).
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"sourcingEventId": rfq_id, "deliveryLocationId": store, "expectedDelivery": "2026-11-10"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let po_id = id(&po);
    assert_eq!(po["status"], "draft");
    assert_eq!(po["number"], "PO-000001");
    assert_eq!(po["totalAmount"], "84900.00");
    assert_eq!(po["supplierId"], acme_id.as_str());
    let kinds: Vec<&str> = po["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["lineKind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, vec!["stock", "asset", "service"]);
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{R}/purchase-orders/{po_id}/receipts"),
            json!({"lines": [{"lineNo": 1, "quantity": "1"}]}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no receipt against a draft order");
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "issued");
    assert_eq!(
        events(&pool, &po_id, "sutraerp.resources.purchase_order.issued").await,
        1
    );
    let req_now = fetch(&app, &admin, &format!("{R}/requisitions/{req_id}")).await;
    assert_eq!(req_now["status"], "ordered");
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/acknowledge"),
        json!({"reference": "ACME-SO-77"}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "acknowledged");
    assert_eq!(po["acknowledgementRef"], "ACME-SO-77");
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/amend"),
        json!({"lines": [{"lineNo": 1, "quantity": "25"}], "reason": "extra reams for exams"}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "draft");
    assert_eq!(po["revision"], 2);
    assert_eq!(po["totalAmount"], "85375.00");
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "issued");

    // ---- goods receipts: over-receipt, inspection, partial acceptance → stock (J-049/054/057).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/receipts"),
        json!({"lines": [{"lineNo": 1, "quantity": "30"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "over-receipt: {b}");
    assert!(b.to_string().contains("over_receipt"), "{b}");
    let g1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/receipts"),
        json!({"receivedOn": "2026-11-05", "deliveryNote": "DN-1", "lines": [{"lineNo": 1, "quantity": "10"}, {"lineNo": 2, "quantity": "2"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let g1_id = id(&g1);
    assert_eq!(g1["status"], "received");
    assert_eq!(g1["number"], "GRN-000001");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/receipts"),
        json!({"lines": [{"lineNo": 1, "quantity": "16"}]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "10 of 25 await inspection: only 15 open"
    );
    let g1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/goods-receipts/{g1_id}/inspect"),
        json!({}),
        ver(&g1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(g1["status"], "inspecting");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/goods-receipts/{g1_id}/accept"),
        json!({"lines": [{"lineNo": 1, "acceptedQuantity": "9", "rejectedQuantity": "1"}]}),
        ver(&g1),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "rejections need a reason"
    );
    let g1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/goods-receipts/{g1_id}/accept"),
        json!({"lines": [{"lineNo": 1, "acceptedQuantity": "9", "rejectedQuantity": "1", "rejectionReason": "torn ream"}]}),
        ver(&g1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(g1["status"], "partially_accepted");
    assert_eq!(g1["lines"][0]["acceptedQuantity"], "9.000");
    assert!(
        g1["lines"][0]["ledgerEntryId"].is_string(),
        "stock line posted"
    );
    assert!(
        g1["lines"][1]["ledgerEntryId"].is_null(),
        "asset line is not stock"
    );
    assert_eq!(g1["lines"][1]["acceptedQuantity"], "2.000");
    assert_eq!(
        on_hand(&app, &admin, &inst, paper.as_uuid(), store.as_uuid()).await,
        "9.000"
    );
    assert_eq!(
        events(&pool, &g1_id, "sutraerp.resources.goods_receipt.accepted").await,
        1
    );
    let po = fetch(&app, &admin, &format!("{R}/purchase-orders/{po_id}")).await;
    assert_eq!(po["status"], "partially_received");
    assert_eq!(po["lines"][0]["receivedQuantity"], "9.000");
    let g2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/receipts"),
        json!({"receivedOn": "2026-11-06", "lines": [{"lineNo": 1, "quantity": "16"}, {"lineNo": 3, "quantity": "1"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let g2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/goods-receipts/{}/accept", id(&g2)),
        json!({}),
        ver(&g2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(g2["status"], "accepted");
    assert_eq!(
        on_hand(&app, &admin, &inst, paper.as_uuid(), store.as_uuid()).await,
        "25.000"
    );
    let po = fetch(&app, &admin, &format!("{R}/purchase-orders/{po_id}")).await;
    assert_eq!(po["status"], "received");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/close"),
        json!({}),
        ver(&po),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not invoiced yet: {b}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/cancel"),
        json!({"reason": "too late"}),
        ver(&po),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "goods were received");

    // ---- assets from the receipt + manual; custody; condition; retirement (J-074/075/076/079/083).
    let asset_line = g1["lines"][1]["id"].as_str().unwrap().to_owned();
    let a1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/assets"),
        json!({"goodsReceiptLineId": asset_line, "serialNumber": "LT-001"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let a1_id = id(&a1);
    assert_eq!(a1["number"], "AST-000001");
    assert_eq!(a1["status"], "registered");
    assert_eq!(a1["acquisitionCost"], "39000.00");
    assert_eq!(a1["acquiredOn"], "2026-11-05");
    assert_eq!(a1["name"], "Item LAPTOP");
    assert_eq!(
        events(&pool, &a1_id, "sutraerp.resources.asset.registered").await,
        1
    );
    let a2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/assets"),
        json!({"goodsReceiptLineId": asset_line, "serialNumber": "LT-002"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/assets"),
        json!({"goodsReceiptLineId": asset_line, "serialNumber": "LT-003"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "two units accepted, two assets");
    let projector = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/assets"),
        json!({"name": "Projector", "serialNumber": "PJ-1", "acquisitionCost": "30000.00", "condition": "good"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(projector["number"], "AST-000003");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/assets"),
        json!({"name": "Projector copy", "serialNumber": "PJ-1"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "serial numbers are unique");
    let staff_id = meera.staff_id.as_uuid().to_string();
    let a1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a1_id}/assign"),
        json!({"assigneeKind": "staff", "assigneeId": staff_id, "assignedOn": "2026-11-07", "note": "lab coordinator"}),
        ver(&a1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["status"], "assigned");
    assert_eq!(a1["custodianId"], staff_id.as_str());
    assert_eq!(a1["assignments"].as_array().unwrap().len(), 1);
    assert_eq!(
        events(&pool, &a1_id, "sutraerp.resources.asset.assigned").await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a1_id}/retire"),
        json!({"reason": "obsolete"}),
        ver(&a1),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "an assigned asset is not retired");
    let a1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a1_id}/return"),
        json!({"condition": "fair", "returnedOn": "2026-11-20"}),
        ver(&a1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["status"], "available");
    assert_eq!(a1["condition"], "fair");
    assert_eq!(a1["assignments"][0]["returnedOn"], "2026-11-20");
    let a1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a1_id}/condition"),
        json!({"condition": "damaged", "repair": true, "reason": "screen cracked"}),
        ver(&a1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["status"], "under_repair");
    let a1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a1_id}/condition"),
        json!({"condition": "good", "repair": false, "reason": "screen replaced"}),
        ver(&a1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["status"], "available");
    assert_eq!(a1["condition"], "good");
    let a1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a1_id}/assign"),
        json!({"assigneeKind": "space", "assigneeId": Uuid::now_v7()}),
        ver(&a1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["custodianKind"], "space");
    let a2_id = id(&a2);
    let a2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a2_id}/retire"),
        json!({"reason": "dead on arrival, written off"}),
        ver(&a2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a2["status"], "retired");
    assert!(a2["retiredOn"].is_string());
    assert_eq!(
        events(&pool, &a2_id, "sutraerp.resources.asset.retired").await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{a2_id}/assign"),
        json!({"assigneeKind": "staff", "assigneeId": staff_id}),
        ver(&a2),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "retired assets stay retired");
    let assets = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/assets?status=retired"),
    )
    .await;
    assert_eq!(assets["items"].as_array().unwrap().len(), 1);

    // ---- vendor invoices: mismatch blocks posting; match → approve → post (F-081/044/082).
    let bad = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-INV-0", "invoiceDate": "2026-11-18",
            "lines": [{"lineNo": 1, "quantity": "30", "unitPrice": "95.00"}, {"lineNo": 3, "quantity": "1", "unitPrice": "6000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let bad_id = id(&bad);
    let bad = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{bad_id}/match"),
        json!({}),
        ver(&bad),
        StatusCode::OK,
    )
    .await;
    assert_eq!(bad["status"], "mismatch");
    let codes: Vec<&str> = bad["matchFindings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["quantity_exceeds_received", "price_variance"]);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{bad_id}/post"),
        json!({}),
        ver(&bad),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a mismatch never posts");
    let inv = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-INV-1", "invoiceDate": "2026-11-20", "dueDate": "2026-12-20",
            "lines": [{"lineNo": 1, "quantity": "25", "unitPrice": "95.00"}, {"lineNo": 2, "quantity": "2", "unitPrice": "39000.00"}, {"lineNo": 3, "quantity": "1", "unitPrice": "5000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let inv_id = id(&inv);
    assert_eq!(inv["number"], "VINV-000002");
    assert_eq!(inv["inventoryAmount"], "80375.00");
    assert_eq!(inv["expenseAmount"], "5000.00");
    assert_eq!(inv["totalAmount"], "85375.00");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-INV-1", "invoiceDate": "2026-11-20",
            "lines": [{"lineNo": 1, "quantity": "1", "unitPrice": "95.00"}]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "supplier invoice numbers are unique"
    );
    let inv = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/match"),
        json!({}),
        ver(&inv),
        StatusCode::OK,
    )
    .await;
    assert_eq!(inv["status"], "approved", "matched + auto-approved: {inv}");
    assert_eq!(inv["matchFindings"].as_array().unwrap().len(), 0);
    let inv = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/post"),
        json!({}),
        ver(&inv),
        StatusCode::OK,
    )
    .await;
    assert_eq!(inv["status"], "posted");
    assert!(inv["entryNumber"].as_str().unwrap().starts_with("JE-"));
    let entry = inv["journalEntryId"].as_str().unwrap().to_owned();
    assert_eq!(
        journal(&pool, &entry).await,
        vec![
            ("1410".into(), "80375.00".into(), "0.00".into()),
            ("5510".into(), "5000.00".into(), "0.00".into()),
            ("2310".into(), "0.00".into(), "85375.00".into()),
        ]
    );
    let source: String =
        sqlx::query_scalar("SELECT source_type FROM finance.journal_entry WHERE id = $1::uuid")
            .bind(&entry)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(source, "procurement_invoice");
    assert_eq!(
        events(&pool, &inv_id, "sutraerp.resources.vendor_invoice.posted").await,
        1
    );
    // Idempotent posting: the port returns the same entry for the same (source, key).
    let state = sutra_api::app::resources_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    let replay = state
        .postings
        .post_procurement(
            &setup.scope(),
            ProcurementPostingRequest {
                institution_id: setup.institution(),
                kind: ProcurementPostingKind::VendorInvoice,
                source_id: Uuid::parse_str(&inv_id).unwrap(),
                posting_key: "post".into(),
                posting_date: b8::date("2026-11-20"),
                description: "replay".into(),
                supplier_label: "SUP-000001 Acme Traders".into(),
                inventory_amount: b8::dec("80375.00"),
                expense_amount: b8::dec("5000.00"),
                payment_amount: b8::dec("0"),
                actor: Some("test:b8d".into()),
            },
        )
        .await
        .unwrap();
    assert!(replay.already_posted);
    assert_eq!(replay.journal_entry_id.as_uuid().to_string(), entry);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/post"),
        json!({}),
        ver(&inv),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "posted once");

    // ---- order closure (J-047): received + invoiced.
    let po = fetch(&app, &admin, &format!("{R}/purchase-orders/{po_id}")).await;
    assert_eq!(po["lines"][0]["invoicedQuantity"], "25.000");
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/close"),
        json!({}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "closed");
    assert_eq!(
        events(&pool, &po_id, "sutraerp.resources.purchase_order.closed").await,
        1
    );
    let req_now = fetch(&app, &admin, &format!("{R}/requisitions/{req_id}")).await;
    assert_eq!(req_now["status"], "closed");

    // ---- credit note: negative amounts flip the sides.
    let cn = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-CN-1", "invoiceDate": "2026-11-25", "creditNote": true,
            "lines": [{"lineNo": 3, "quantity": "1", "unitPrice": "1000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(cn["totalAmount"], "-1000.00");
    let cn = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{}/match", id(&cn)),
        json!({}),
        ver(&cn),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        cn["status"], "mismatch",
        "1000.00 is not the order price: {cn}"
    );
    let cn2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-CN-2", "invoiceDate": "2026-11-25", "creditNote": true,
            "lines": [{"lineNo": 1, "quantity": "1", "unitPrice": "95.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let cn2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{}/match", id(&cn2)),
        json!({}),
        ver(&cn2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cn2["status"], "approved", "{cn2}");
    let cn2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-invoices/{}/post", id(&cn2)),
        json!({}),
        ver(&cn2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cn2["totalAmount"], "-95.00");
    assert_eq!(
        journal(&pool, cn2["journalEntryId"].as_str().unwrap()).await,
        vec![
            ("1410".into(), "0.00".into(), "95.00".into()),
            ("2310".into(), "95.00".into(), "0.00".into()),
        ]
    );

    // ---- vendor payments (F-045).
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": acme_id, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": inv_id, "amount": "85375.01"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "more than outstanding");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": acme_id, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": bad_id, "amount": "10.00"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "only posted invoices are paid");
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": acme_id, "paymentDate": "2026-11-30", "reference": "NEFT-1",
            "allocations": [{"vendorInvoiceId": inv_id, "amount": "50000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(p1["status"], "draft");
    assert_eq!(p1["number"], "VPAY-000001");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": acme_id, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": inv_id, "amount": "35375.01"}]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "draft allocations count"
    );
    let p1_id = id(&p1);
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-payments/{p1_id}/post"),
        json!({}),
        ver(&p1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "posted");
    assert_eq!(
        journal(&pool, p1["journalEntryId"].as_str().unwrap()).await[0],
        ("2310".into(), "50000.00".into(), "0.00".into())
    );
    assert_eq!(
        events(&pool, &p1_id, "sutraerp.resources.vendor_payment.posted").await,
        1
    );
    let inv = fetch(&app, &admin, &format!("{R}/vendor-invoices/{inv_id}")).await;
    assert_eq!(
        (inv["status"].as_str(), inv["paidAmount"].as_str()),
        (Some("posted"), Some("50000.00"))
    );
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": acme_id, "paymentDate": "2026-11-30", "allocations": [{"vendorInvoiceId": inv_id, "amount": "35375.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-payments/{}/post", id(&p2)),
        json!({}),
        ver(&p2),
        StatusCode::OK,
    )
    .await;
    let inv = fetch(&app, &admin, &format!("{R}/vendor-invoices/{inv_id}")).await;
    assert_eq!(inv["status"], "paid");
    let payments = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/vendor-payments?status=posted"),
    )
    .await;
    assert_eq!(payments["items"].as_array().unwrap().len(), 2);
    let journals: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE institution_id = $1::uuid \
         AND source_type IN ('procurement_invoice', 'procurement_payment')",
    )
    .bind(&inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(journals, 4, "invoice, credit note, two payments");
}

// ================================================================================= test 2

#[tokio::test]
async fn inventory_documents_and_replenishment() {
    let (app, pool) = app(TENANT_STOCK).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_STOCK, "Stores University").await;
    let inst = setup.institution().as_uuid().to_string();
    let meera = b8::seed_staff(&pool, &setup, "Meera", None).await;

    // ---- item master + stores (J-051 / 053).
    let toner = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/items"),
        json!({"code": "toner-85a", "name": "Toner 85A", "kind": "consumable", "unit": "cartridge",
            "reorderLevel": "5", "reorderQuantity": "20", "standardCost": "1500.00"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(toner["code"], "TONER-85A");
    assert_eq!(toner["status"], "draft");
    assert!(
        matches!(toner["onHand"].as_str(), Some("0" | "0.000")),
        "{toner}"
    );
    let toner_id = id(&toner);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/items"),
        json!({"code": "TONER-85A", "name": "Again", "kind": "consumable"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "item codes are unique");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/items"),
        json!({"code": "X1", "name": "Bad kind", "kind": "food"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let store = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stores"),
        json!({"code": "MAIN-STORE", "name": "Main store"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let store_id = id(&store);
    let rack = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stores/{store_id}/locations"),
        json!({"code": "MAIN-R1", "name": "Rack 1"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(rack["parentId"], store_id.as_str());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/stores/{}/locations", id(&rack)),
        json!({"code": "MAIN-R1-B", "name": "Bin"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "locations sit directly in a store");
    let stores = fetch(&app, &admin, &format!("{R}/institutions/{inst}/stores")).await;
    assert_eq!(stores["items"].as_array().unwrap().len(), 2);
    let store_uuid = Uuid::parse_str(&store_id).unwrap();
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-reservations"),
        json!({"itemId": toner_id, "locationId": store_id, "quantity": "1", "purpose": "x"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a draft item moves no stock"
    );
    let toner = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/items/{toner_id}/activate"),
        json!({}),
        ver(&toner),
        StatusCode::OK,
    )
    .await;
    assert_eq!(toner["status"], "active");
    let toner = ok(
        &app,
        &admin,
        "PATCH",
        &format!("{R}/items/{toner_id}"),
        json!({"reorderLevel": "6"}),
        ver(&toner),
        StatusCode::OK,
    )
    .await;
    assert_eq!(toner["reorderLevel"], "6.000");
    let paper = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, Some("12")).await;
    let paper_id = paper.as_uuid().to_string();
    let seeded = sutra_contracts::resources::ids::StoreLocationId(store_uuid);
    b8::seed_stock(&pool, &setup, paper, seeded, "25", "line:1").await;

    // ---- issues: insufficient stock 409, issue, returns (J-064 / 066).
    let big = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-issues"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "30", "purpose": "Exam hall"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(big["number"], "SIV-000001");
    assert_eq!(big["status"], "requested");
    let big = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{}/decision", id(&big)),
        json!({"decision": "approve"}),
        ver(&big),
        StatusCode::OK,
    )
    .await;
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{}/issue", id(&big)),
        json!({}),
        ver(&big),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "insufficient stock: {b}");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{}/cancel", id(&big)),
        json!({"reason": "split into smaller issues"}),
        ver(&big),
        StatusCode::OK,
    )
    .await;
    let iss = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-issues"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "5", "purpose": "Physics lab",
            "issuedToStaffId": meera.staff_id.as_uuid()}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let iss_id = id(&iss);
    let iss = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{iss_id}/decision"),
        json!({"decision": "approve"}),
        ver(&iss),
        StatusCode::OK,
    )
    .await;
    let iss = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{iss_id}/issue"),
        json!({}),
        ver(&iss),
        StatusCode::OK,
    )
    .await;
    assert_eq!(iss["status"], "issued");
    assert!(iss["ledgerEntryId"].is_string());
    assert_eq!(
        on_hand(&app, &admin, &inst, paper.as_uuid(), store_uuid).await,
        "20.000"
    );
    assert_eq!(
        events(&pool, &iss_id, "sutraerp.resources.stock_issue.issued").await,
        1
    );
    let iss = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{iss_id}/return"),
        json!({"quantity": "2", "reason": "unused"}),
        ver(&iss),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (iss["status"].as_str(), iss["returnedQuantity"].as_str()),
        (Some("issued"), Some("2.000"))
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{iss_id}/return"),
        json!({"quantity": "4"}),
        ver(&iss),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "only 3 still out");
    let iss = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{iss_id}/return"),
        json!({"quantity": "3"}),
        ver(&iss),
        StatusCode::OK,
    )
    .await;
    assert_eq!(iss["status"], "returned");
    assert_eq!(
        on_hand(&app, &admin, &inst, paper.as_uuid(), store_uuid).await,
        "25.000"
    );

    // ---- reservations (J-065): available = on hand − reserved.
    let res = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-reservations"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "20", "purpose": "Board exams", "neededOn": "2026-12-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(res["status"], "active");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-reservations"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "10", "purpose": "Office"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "only 5 available");
    let other = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-issues"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "10", "purpose": "Office"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let other = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{}/decision", id(&other)),
        json!({"decision": "approve"}),
        ver(&other),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{}/issue", id(&other)),
        json!({}),
        ver(&other),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "reserved stock is not issued to others"
    );
    let res = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-reservations/{}/release", id(&res)),
        json!({"reason": "exam postponed"}),
        ver(&res),
        StatusCode::OK,
    )
    .await;
    assert_eq!(res["status"], "released");
    let res2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-reservations"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "4", "purpose": "Library"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let res2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-reservations/{}/fulfil", id(&res2)),
        json!({"issuedToStaffId": meera.staff_id.as_uuid()}),
        ver(&res2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(res2["status"], "fulfilled");
    assert!(res2["stockIssueId"].is_string());
    assert_eq!(
        on_hand(&app, &admin, &inst, paper.as_uuid(), store_uuid).await,
        "21.000"
    );

    // ---- adjustments (J-059): auto-approved and applied; below zero → 409.
    let adj = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-adjustments"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "-1", "reason": "water damage"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(adj["status"], "applied");
    assert!(adj["ledgerEntryId"].is_string());
    assert_eq!(
        on_hand(&app, &admin, &inst, paper.as_uuid(), store_uuid).await,
        "20.000"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-adjustments"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "-500", "reason": "typo"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "never below zero");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-adjustments"),
        json!({"itemId": paper_id, "locationId": store_id, "quantity": "0.0001", "reason": "x"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "three decimals at most"
    );

    // ---- replenishment run (J-061): toner (0 ≤ 6) needs stock, paper (20 > 12) does not.
    let run = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/replenishment"),
        json!({"draftRequisition": true}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (run["processed"].as_i64(), run["skipped"].as_i64()),
        (Some(1), Some(1)),
        "{run}"
    );
    let needs = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/replenishment"),
    )
    .await;
    assert_eq!(needs["items"].as_array().unwrap().len(), 1);
    let need = &needs["items"][0];
    assert_eq!(need["itemCode"], "TONER-85A");
    assert_eq!(need["suggestedQuantity"], "20.000");
    assert_eq!(need["status"], "requisitioned");
    assert_eq!(
        events(
            &pool,
            need["id"].as_str().unwrap(),
            "sutraerp.resources.replenishment_need.raised"
        )
        .await,
        1
    );
    let drafts = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/requisitions?status=draft"),
    )
    .await;
    assert_eq!(drafts["items"].as_array().unwrap().len(), 1);
    assert_eq!(drafts["items"][0]["source"], "replenishment");
    assert_eq!(drafts["items"][0]["estimatedTotal"], "30000.00");
    let again = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/replenishment"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (again["processed"].as_i64(), again["skipped"].as_i64()),
        (Some(0), Some(2))
    );

    // ---- movement list (newest first, paged).
    let moves = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/stock-movements?itemId={paper_id}&limit=3"),
    )
    .await;
    assert_eq!(moves["items"].as_array().unwrap().len(), 3);
    assert_eq!(moves["items"][0]["kind"], "adjustment");
    assert_eq!(moves["items"][0]["quantity"], "-1.000");
    let cursor = moves["nextCursor"].as_str().unwrap();
    let older = fetch(
        &app,
        &admin,
        &format!(
            "{R}/institutions/{inst}/stock-movements?itemId={paper_id}&limit=50&cursor={cursor}"
        ),
    )
    .await;
    assert_eq!(
        older["items"].as_array().unwrap().len(),
        3,
        "receipt, issue, two returns, fulfilment = 6 total"
    );
    let item = fetch(&app, &admin, &format!("{R}/items/{paper_id}")).await;
    assert_eq!(item["onHand"], "20.000");
}

// ================================================================================= test 3

#[tokio::test]
async fn sourcing_variants_and_cancellations() {
    let (app, pool) = app(TENANT_SOURCING).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_SOURCING, "Sourcing University").await;
    let inst = setup.institution().as_uuid().to_string();
    let paper = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, None).await;
    let acme = active_supplier(&app, &admin, &inst, "Acme Traders", "ABCDE1234F", None).await;
    let beta = active_supplier(&app, &admin, &inst, "Beta Supplies", "BCDEF2345G", None).await;

    // ---- direct / simplified sourcing (J-007).
    let req = released_requisition(
        &app,
        &admin,
        &inst,
        json!([{"itemId": paper, "quantity": "10", "estimatedUnitCost": "100.00"}]),
    )
    .await;
    let ev = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": id(&req), "method": "direct", "title": "Paper top-up"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let ev_id = id(&ev);
    assert_eq!(ev["method"], "direct");
    let ev = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/quotations"),
        json!({"supplierId": id(&beta), "lines": [{"lineNo": 1, "unitPrice": "99.00"}]}),
        ver(&ev),
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/quotations"),
        json!({"supplierId": id(&acme), "lines": [{"lineNo": 1, "unitPrice": "98.00"}]}),
        ver(&ev),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a direct event takes one quotation"
    );
    let q = ev["quotations"][0]["id"].as_str().unwrap().to_owned();
    let ev = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/award"),
        json!({"quotationId": q}),
        ver(&ev),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ev["status"], "awarded", "direct awards need no evaluation");
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"sourcingEventId": ev_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(po["totalAmount"], "990.00");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"sourcingEventId": ev_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one order per requisition");
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{}/issue", id(&po)),
        json!({}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/cancel"),
        json!({"reason": "budget cut"}),
        ver(&ev),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the event has a live order");

    // ---- order cancellation (J-046) → requisition released again; then sourcing + requisition cancellation (J-041).
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{}/cancel", id(&po)),
        json!({"reason": "budget cut"}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "cancelled");
    let req = fetch(&app, &admin, &format!("{R}/requisitions/{}", id(&req))).await;
    assert_eq!(req["status"], "released");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/requisitions/{}/cancel", id(&req)),
        json!({"reason": "budget cut"}),
        ver(&req),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "still being sourced");
    let ev = fetch(&app, &admin, &format!("{R}/sourcing-events/{ev_id}")).await;
    let ev = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/cancel"),
        json!({"reason": "budget cut"}),
        ver(&ev),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ev["status"], "cancelled");
    let req = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/requisitions/{}/cancel", id(&req)),
        json!({"reason": "budget cut"}),
        ver(&req),
        StatusCode::OK,
    )
    .await;
    assert_eq!(req["status"], "cancelled");

    // ---- direct order straight from a requisition (free-text service line).
    let svc = released_requisition(
        &app,
        &admin,
        &inst,
        json!([{"description": "Printer repair", "quantity": "1", "estimatedUnitCost": "2000.00"}]),
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"requisitionId": id(&svc)}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a direct order names its supplier"
    );
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"requisitionId": id(&svc), "supplierId": id(&acme), "prices": [{"lineNo": 1, "unitPrice": "1800.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(po["totalAmount"], "1800.00");
    assert_eq!(po["lines"][0]["lineKind"], "service");

    // ---- value limits: large demand must go through an RFQ.
    let big = released_requisition(
        &app,
        &admin,
        &inst,
        json!([{"description": "Smart boards", "quantity": "5", "estimatedUnitCost": "45000.00"}]),
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": id(&big), "method": "direct"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "above the direct sourcing limit");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"requisitionId": id(&big), "supplierId": id(&acme)}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "above the limit an order needs an award"
    );
    let rfq = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": id(&big), "method": "rfq"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{}/evaluate", id(&rfq)),
        json!({}),
        ver(&rfq),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing to evaluate");
}

// ================================================================================= test 4

/// Resources router with a hand-built state and an injected administrator session.
fn custom_app(st: sutra_resources::ResourcesState, tenant: &str, subject: &str) -> Router {
    let (router, _) = sutra_resources::router_with_state(st).split_for_parts();
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

#[tokio::test]
async fn governed_decisions_await_a_decision() {
    let (app, pool) = app(TENANT_AWAIT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_AWAIT, "Approvals University").await;
    let inst = setup.institution().as_uuid().to_string();
    let mut st = sutra_api::app::resources_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let hr = custom_app(st, TENANT_AWAIT, "dev|admin@sutra.dev");
    let me = Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    };
    let paper = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, None).await;
    let store = b8::seed_store(&pool, &setup, "CENTRAL", None).await;
    b8::seed_stock(&pool, &setup, paper, store, "10", "line:1").await;

    // ---- REQUISITION_APPROVAL: submit → pending → reject → amend → withdraw → resubmit → approve.
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/institutions/{inst}/requisitions"),
        json!({"title": "Lab glassware", "lines": [{"description": "Beakers 250 ml", "quantity": "40", "estimatedUnitCost": "85.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid = id(&r);
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/submit"),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "submitted");
    assert!(r["processInstanceId"].is_string());
    assert_eq!(
        events(
            &pool,
            &rid,
            "sutraerp.resources.purchase_requisition.submitted"
        )
        .await,
        1,
        "awaiting a decision → administrators' inbox"
    );
    let (s, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/decision"),
        json!({"decision": "reject"}),
        ver(&r),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "rejection reason");
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/decision"),
        json!({"decision": "reject", "reason": "quantity too high"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "rejected");
    assert_eq!(
        events(
            &pool,
            &rid,
            "sutraerp.resources.purchase_requisition.rejected"
        )
        .await,
        1
    );
    let (s, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/amend"),
        json!({"lines": [{"description": "Beakers 250 ml", "quantity": "20", "estimatedUnitCost": "85.00"}]}),
        ver(&r),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "an amendment after review needs a reason"
    );
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/amend"),
        json!({"lines": [{"description": "Beakers 250 ml", "quantity": "20", "estimatedUnitCost": "85.00"}], "reason": "halved"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "amended");
    assert_eq!(r["revision"], 2);
    assert_eq!(r["estimatedTotal"], "1700.00");
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/submit"),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/withdraw"),
        json!({"reason": "wrong department"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "withdrawn");
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/amend"),
        json!({"lines": [{"description": "Beakers 250 ml", "quantity": "20", "estimatedUnitCost": "85.00"}], "reason": "right department"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/submit"),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/requisitions/{rid}/decision"),
        json!({"decision": "approve"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "released");
    assert_eq!(r["revision"], 3);
    assert_eq!(r["decidedBy"], "dev|admin@sutra.dev");

    // ---- SUPPLIER_APPROVAL stays pending until decided.
    let s = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Glassworks", "pan": "CDEFG3456H", "bankAccountNumber": "55667788", "bankIfsc": "SBIN0000123"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&s);
    let s = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    let s = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/suppliers/{sid}/submit"),
        json!({}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (s["status"].as_str(), s["approvalPending"].as_bool()),
        (Some("registered"), Some(true))
    );
    let s = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/suppliers/{sid}/decision"),
        json!({"decision": "approve"}),
        ver(&s),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "active");

    // ---- PURCHASE_ORDER_APPROVAL: issue → pending → approve → issued.
    let po = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"requisitionId": rid, "supplierId": sid}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let po_id = id(&po);
    let po = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (po["status"].as_str(), po["approvalPending"].as_bool()),
        (Some("draft"), Some(true))
    );
    let (s2, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        ver(&po),
    )
    .await;
    assert_eq!(s2, StatusCode::CONFLICT, "already awaiting approval");
    let po = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/decision"),
        json!({"decision": "approve"}),
        ver(&po),
        StatusCode::OK,
    )
    .await;
    assert_eq!(po["status"], "issued");
    assert_eq!(
        events(&pool, &po_id, "sutraerp.resources.purchase_order.issued").await,
        1
    );

    // ---- STOCK_ADJUSTMENT_APPROVAL: requested until decided, then applied.
    let adj = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/institutions/{inst}/stock-adjustments"),
        json!({"itemId": paper, "locationId": store, "quantity": "-2", "reason": "stock count difference"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(adj["status"], "requested");
    assert!(adj["ledgerEntryId"].is_null());
    let adj = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/stock-adjustments/{}/decision", id(&adj)),
        json!({"decision": "approve"}),
        ver(&adj),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adj["status"], "applied");
    let oh: rust_decimal::Decimal = sqlx::query_scalar(
        "SELECT sum(quantity) FROM resources.stock_ledger_entry WHERE item_id = $1",
    )
    .bind(paper.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(oh.to_string(), "8.000");

    // ---- ASSET_RETIREMENT: pending → rejected keeps the asset in service.
    let a = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/institutions/{inst}/assets"),
        json!({"name": "Microscope", "acquisitionCost": "25000.00"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let a = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/assets/{}/retire", id(&a)),
        json!({"reason": "lens scratched"}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (a["status"].as_str(), a["retirementPending"].as_bool()),
        (Some("registered"), Some(true))
    );
    let a = ok(
        &hr,
        &me,
        "POST",
        &format!("{R}/assets/{}/decision", id(&a)),
        json!({"decision": "reject", "reason": "repairable"}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (a["status"].as_str(), a["retirementPending"].as_bool()),
        (Some("registered"), Some(false))
    );
}

// ================================================================================= test 5

#[tokio::test]
async fn permissions_and_preconditions() {
    let (app, pool) = app(TENANT_PERM).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_PERM, "Guarded University").await;
    let inst = setup.institution().as_uuid().to_string();
    let (s, _, _) = send(
        &app,
        get(&aarav, &format!("{R}/institutions/{inst}/suppliers")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students do not read procurement");
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Sneaky"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(
        &app,
        get(&admin, &format!("{R}/suppliers/{}", Uuid::now_v7())),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = send(
        &app,
        get(
            &admin,
            &format!("{R}/institutions/{}/items", Uuid::now_v7()),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "unknown institution");
    let sup = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Precise Ltd", "pan": "DEFGH4567J"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&sup);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED, "If-Match is required");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({}),
        Some(99),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale ETag");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({}),
        ver(&sup),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "bank details are required: {b}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sid}/register"),
        json!({"gstin": "29ZZZZZ9999Z1Z5", "bankAccountNumber": "11223344", "bankIfsc": "HDFC0001234"}),
        ver(&sup),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "GSTIN must carry the PAN"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/requisitions"),
        json!({"title": "Empty", "lines": []}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "at least one line");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/requisitions"),
        json!({"title": "Bad org", "orgUnitId": Uuid::now_v7(), "lines": [{"description": "x", "quantity": "1", "estimatedUnitCost": "1.00"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown org unit");
}
