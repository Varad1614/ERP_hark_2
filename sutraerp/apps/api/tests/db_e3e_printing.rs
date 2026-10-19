//! ERP_EXTENSIONS 3E — thermal receipt printing at the fee counter over the HTTP API against
//! PostgreSQL (`docs/extensions-3e-printing.md`):
//!
//! * receipt print settings: the defaults until saved, save and read back (title, address, two
//!   registrations, footer), at most two registrations (422), `finance.config.manage` only;
//! * a fee receipt as a slip: the doc (kind, header from the saved settings, receipt meta, amount
//!   in words), the plain-text preview within the paper width, the ESC/POS bytes (base64), the
//!   cash-drawer pulse only for a cash original, the duplicate copy, columns other than 32 → 48;
//! * the test slip (no receipt behind it);
//! * the counter day close: today (institution time) by tender, and an empty day;
//! * who may print: unauthenticated → 401, student / faculty → 403, finance approver reads.
//!
//! The charge is seeded through `support::b5::seed_charge`; the receipt comes from a real counter
//! cash payment. Every test runs in its own tenant.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use support::*;

const T_SETTINGS: &str = "0192f5a0-7c3e-7000-8000-0000000e3e00";
const T_SLIP: &str = "0192f5a0-7c3e-7000-8000-0000000e3e01";
const T_ACCESS: &str = "0192f5a0-7c3e-7000-8000-0000000e3e02";

const F: &str = "/api/v1/finance";
const GSTIN: &str = "27AAPFU0939F1ZV";
const PAN: &str = "AAPFU0939F";
/// Base64 of `ESC @` (`0x1b 0x40`) followed by another command byte: every slip starts so.
const ESCPOS_INIT_B64: &str = "G0A";

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    expect: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, &format!("{F}{path}"), body, None)).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, &format!("{F}{path}"))).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

async fn status_of(app: &Router, who: &Login, path: &str) -> StatusCode {
    send(app, get(who, &format!("{F}{path}"))).await.0
}

fn settings_body(title: &str) -> Value {
    json!({
        "title": title,
        "address": "Station Road, Ahilyanagar 414001",
        "registrations": [
            {"label": "PAN", "number": PAN},
            {"label": "GSTIN", "number": GSTIN},
        ],
        "footer": "Fees once paid are not refundable.",
        "showAmountInWords": true,
    })
}

/// Every preview line fits the paper (counted in characters).
fn fits(preview: &str, columns: usize) {
    assert!(!preview.trim().is_empty(), "empty preview");
    for line in preview.lines() {
        assert!(
            line.chars().count() <= columns,
            "line wider than {columns}: {line:?}\n{preview}"
        );
    }
}

/// A finance institution with one published charge and a cash payment that issued a receipt;
/// returns `(institution id, receipt id, receipt number)`.
async fn paid_receipt(
    app: &Router,
    admin: &Login,
    pool: &sqlx::PgPool,
    tenant: &str,
    name: &str,
) -> (String, String, String) {
    let setup = b5::finance_setup(app, admin, pool, tenant, name, Some(b4::AARAV)).await;
    let acc = setup.student_account_id;
    let inst = setup.institution().as_uuid().to_string();
    let c = b5::seed_charge(pool, &setup, "25000.00", "published", None).await;
    let p = call(
        app,
        admin,
        "POST",
        &format!("/student-accounts/{acc}/payments"),
        json!({
            "channel": "cash",
            "amount": "25000.00",
            "allocations": [{"chargeId": c.charge_id, "amount": "25000.00"}],
        }),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(p["status"], "completed", "{p}");
    let receipt_id = p["receipt"]["id"].as_str().unwrap().to_owned();
    let number = p["receipt"]["receiptNumber"].as_str().unwrap().to_owned();
    (inst, receipt_id, number)
}

// ------------------------------------------------------------------ settings

#[tokio::test]
async fn print_settings_defaults_save_validate_and_permission() {
    let (app, pool) = app(T_SETTINGS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(&app, &admin, &pool, T_SETTINGS, "Print College", None).await;
    let inst = setup.institution().as_uuid().to_string();
    let path = format!("/institutions/{inst}/receipt-print-settings");

    // Defaults until the institution saves its own.
    let d = read(&app, &admin, &path).await;
    assert_eq!(d["institutionId"], inst, "{d}");
    assert_eq!(d["title"], "");
    assert_eq!(d["address"], "");
    assert!(d["registrations"].as_array().unwrap().is_empty(), "{d}");
    assert_eq!(d["footer"], "");
    assert_eq!(d["showAmountInWords"], true);
    assert!(d["updatedAt"].is_null(), "{d}");

    // Save and read back.
    let saved = call(
        &app,
        &admin,
        "PUT",
        &path,
        settings_body("Print College Fee Counter"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["title"], "Print College Fee Counter", "{saved}");
    assert!(saved["updatedAt"].is_string(), "{saved}");
    let back = read(&app, &admin, &path).await;
    assert_eq!(back["title"], "Print College Fee Counter");
    assert_eq!(back["address"], "Station Road, Ahilyanagar 414001");
    assert_eq!(
        back["registrations"],
        json!([
            {"label": "PAN", "number": PAN},
            {"label": "GSTIN", "number": GSTIN},
        ]),
        "{back}"
    );
    assert_eq!(back["footer"], "Fees once paid are not refundable.");
    assert_eq!(back["showAmountInWords"], true);
    assert!(back["updatedAt"].is_string(), "{back}");

    // At most two registrations.
    let mut three = settings_body("Too many");
    three["registrations"] = json!([
        {"label": "PAN", "number": PAN},
        {"label": "GSTIN", "number": GSTIN},
        {"label": "Trust reg.", "number": "E-1234 (Pune)"},
    ]);
    call(
        &app,
        &admin,
        "PUT",
        &path,
        three,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let unchanged = read(&app, &admin, &path).await;
    assert_eq!(unchanged["title"], "Print College Fee Counter");

    // The finance checker reads the settings but cannot change them.
    let approver = login(&app, "finance.approver@sutra.dev").await;
    let seen = read(&app, &approver, &path).await;
    assert_eq!(seen["title"], "Print College Fee Counter");
    call(
        &app,
        &approver,
        "PUT",
        &path,
        settings_body("Not allowed"),
        StatusCode::FORBIDDEN,
    )
    .await;
}

// ------------------------------------------------------------------ slips and day close

#[tokio::test]
async fn receipt_slip_test_slip_and_counter_day_close() {
    let (app, pool) = app(T_SLIP).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, receipt, number) = paid_receipt(&app, &admin, &pool, T_SLIP, "Slip College").await;
    call(
        &app,
        &admin,
        "PUT",
        &format!("/institutions/{inst}/receipt-print-settings"),
        settings_body("Slip College Fee Counter"),
        StatusCode::OK,
    )
    .await;

    // The original at 58 mm, opening the cash drawer.
    let s = read(
        &app,
        &admin,
        &format!("/receipts/{receipt}/slip?columns=32&kick=true"),
    )
    .await;
    assert_eq!(s["receiptId"], receipt, "{s}");
    assert_eq!(s["receiptNumber"], number);
    assert_eq!(s["columns"], 32);
    assert_eq!(s["kickDrawer"], true, "a cash original opens the drawer");
    let doc = &s["doc"];
    assert_eq!(doc["kind"], "FEE RECEIPT", "{doc}");
    assert_eq!(doc["storeName"], "Slip College Fee Counter");
    assert_eq!(doc["address"], "Station Road, Ahilyanagar 414001");
    assert_eq!(
        doc["registrations"],
        json!([format!("PAN {PAN}"), format!("GSTIN {GSTIN}")])
    );
    assert_eq!(doc["voided"], false);
    let meta0 = doc["meta"][0].as_str().unwrap();
    assert!(meta0.starts_with("Receipt #"), "{doc}");
    assert_eq!(meta0, format!("Receipt #{number}"));
    let note = doc["note"].as_str().unwrap();
    assert!(note.starts_with("Rupees "), "{note}");
    assert_eq!(note, "Rupees Twenty-Five Thousand Only");
    assert_eq!(doc["footer"], "Fees once paid are not refundable.");
    assert_eq!(doc["items"].as_array().unwrap().len(), 1, "{doc}");
    let preview = s["preview"].as_str().unwrap();
    assert!(preview.contains("FEE RECEIPT"), "{preview}");
    assert!(preview.contains("Rs."), "{preview}");
    assert!(preview.contains(&number), "{preview}");
    fits(preview, 32);
    let bytes = s["escposBase64"].as_str().unwrap();
    assert!(bytes.starts_with(ESCPOS_INIT_B64), "{bytes}");

    // A reprint is marked and never opens the drawer.
    let dup = read(
        &app,
        &admin,
        &format!("/receipts/{receipt}/slip?columns=32&kick=true&copy=duplicate"),
    )
    .await;
    assert_eq!(dup["doc"]["kind"], "DUPLICATE COPY", "{dup}");
    assert_eq!(dup["kickDrawer"], false);
    assert!(dup["preview"].as_str().unwrap().contains("DUPLICATE COPY"));
    fits(dup["preview"].as_str().unwrap(), 32);

    // Any other width prints at 80 mm; no kick asked → no pulse.
    let wide = read(
        &app,
        &admin,
        &format!("/receipts/{receipt}/slip?columns=99"),
    )
    .await;
    assert_eq!(wide["columns"], 48, "{wide}");
    assert_eq!(wide["kickDrawer"], false);
    fits(wide["preview"].as_str().unwrap(), 48);

    // An unknown receipt.
    let missing = format!("/receipts/{}/slip", uuid::Uuid::now_v7());
    assert_eq!(
        status_of(&app, &admin, &missing).await,
        StatusCode::NOT_FOUND
    );

    // The test slip.
    let t = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/receipt-test-slip?columns=48"),
    )
    .await;
    assert!(t["receiptId"].is_null(), "{t}");
    assert!(t["receiptNumber"].is_null(), "{t}");
    assert_eq!(t["columns"], 48);
    assert_eq!(t["kickDrawer"], false);
    assert_eq!(t["doc"]["kind"], "TEST PRINT");
    assert_eq!(t["doc"]["storeName"], "Slip College Fee Counter");
    assert!(t["preview"].as_str().unwrap().contains("TEST PRINT"));
    fits(t["preview"].as_str().unwrap(), 48);
    assert!(t["escposBase64"]
        .as_str()
        .unwrap()
        .starts_with(ESCPOS_INIT_B64));

    // The day close: today in the institution's timezone.
    let r = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/counter-report"),
    )
    .await;
    assert_eq!(r["institutionId"], inst, "{r}");
    assert_eq!(r["mine"], false);
    assert!(r["receiptCount"].as_i64().unwrap() >= 1, "{r}");
    assert_eq!(r["cancelledCount"], 0);
    let cash = r["byChannel"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["channel"] == "cash")
        .unwrap_or_else(|| panic!("no cash line: {r}"));
    assert_eq!(cash["count"], 1, "{r}");
    assert_eq!(cash["amount"], "25000.00");
    assert_eq!(r["total"], "25000.00");
    assert!(!r["rows"].as_array().unwrap().is_empty(), "{r}");
    let rp = r["preview"].as_str().unwrap();
    assert!(rp.contains("COUNTER DAY CLOSE"), "{rp}");
    assert!(rp.contains(&number), "first / last receipt: {rp}");
    fits(rp, 48);
    assert!(r["escposBase64"]
        .as_str()
        .unwrap()
        .starts_with(ESCPOS_INIT_B64));

    // An empty day.
    let empty = read(
        &app,
        &admin,
        &format!("/institutions/{inst}/counter-report?date=2000-01-01&columns=32"),
    )
    .await;
    assert_eq!(empty["date"], "2000-01-01", "{empty}");
    assert_eq!(empty["receiptCount"], 0);
    assert_eq!(empty["cancelledCount"], 0);
    assert_eq!(empty["total"], "0.00");
    assert!(empty["byChannel"].as_array().unwrap().is_empty(), "{empty}");
    let ep = empty["preview"].as_str().unwrap();
    assert!(ep.contains("COUNTER DAY CLOSE"), "{ep}");
    assert!(ep.contains("No receipts"), "{ep}");
    fits(ep, 32);
}

// ------------------------------------------------------------------ who may print

#[tokio::test]
async fn slips_need_finance_read() {
    let (app, pool) = app(T_ACCESS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, receipt, _) = paid_receipt(&app, &admin, &pool, T_ACCESS, "Access College").await;
    let slip = format!("{F}/receipts/{receipt}/slip?columns=32");

    // No session.
    let (s, _, _) = send(
        &app,
        Request::get(slip.as_str()).body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let test_slip = format!("{F}/institutions/{inst}/receipt-test-slip");
    let (s, _, _) = send(
        &app,
        Request::get(test_slip.as_str())
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);

    // The student (even the one who paid) and faculty have no finance.read.
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    for who in [&student, &faculty] {
        let (s, _, b) = send(&app, get(who, &slip)).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
        let report = format!("/institutions/{inst}/counter-report");
        assert_eq!(status_of(&app, who, &report).await, StatusCode::FORBIDDEN);
        let settings = format!("/institutions/{inst}/receipt-print-settings");
        assert_eq!(status_of(&app, who, &settings).await, StatusCode::FORBIDDEN);
    }

    // The finance checker reads slips.
    let approver = login(&app, "finance.approver@sutra.dev").await;
    let (s, _, b) = send(&app, get(&approver, &slip)).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["doc"]["kind"], "FEE RECEIPT");
}
