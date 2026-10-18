//! ERP_EXTENSIONS 5D — GST tax invoices, bills of supply and GST credit notes over the HTTP API
//! against PostgreSQL (`docs/extensions-5d-gst.md`):
//!
//! * GST profile: defaults until saved, GSTIN check character and state, series prefixes;
//! * fee-head tax setup: SAC, slab, exempt with its note, tax-inclusive pricing, validation, the
//!   system GST heads left out;
//! * a charge on an exempt tuition head + a taxable hostel head: the hostel line keeps its
//!   taxable value, CGST + SGST lines are added, the document is pending until publication and
//!   then numbered from the FY series as a tax invoice; output CGST / SGST credited; the
//!   printable document (amount in words, zero-tax note) for staff and the student;
//! * a GST credit note through the adjustments path: tax reversed in proportion (Dr output tax),
//!   its own CN series number referencing the invoice; reversing it cancels it;
//! * an exempt-only charge → bill of supply; an inter-state registered payer → IGST + B2B;
//! * the GSTR-1 friendly summary (JSON + CSV);
//! * who may: unauthenticated → 401, student → 403 on staff routes.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use support::b5::amount;
use support::*;

const T_FLOW: &str = "0192f5a0-7c3e-7000-8000-0000000e5d00";
const T_ACCESS: &str = "0192f5a0-7c3e-7000-8000-0000000e5d01";

const F: &str = "/api/v1/finance";
const GSTIN: &str = "27AAPFU0939F1ZV";
/// A Karnataka GSTIN (valid check character).
const KA_GSTIN: &str = "29AABCT1332L1ZA";

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(
        app,
        write(who, method, &format!("{F}{path}"), body, if_match),
    )
    .await;
    (s, b)
}

async fn ok(app: &Router, who: &Login, method: &str, path: &str, body: Value) -> Value {
    let (s, b) = call(app, who, method, path, body, None).await;
    assert!(s.is_success(), "{method} {path}: {s} {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, &format!("{F}{path}"))).await;
    (s, b)
}

fn v(b: &Value) -> i64 {
    b["version"].as_i64().unwrap()
}

/// Net credit balance of a GST output-tax account.
async fn output(pool: &sqlx::PgPool, inst: &str, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(l.credit - l.debit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE a.institution_id = $1::uuid AND a.system_key = $2",
    )
    .bind(inst)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Raises, validates and publishes a manual charge; returns the published charge.
async fn charge(app: &Router, admin: &Login, inst: &str, student: &str, lines: Value) -> Value {
    let (s, c) = call(
        app,
        admin,
        "POST",
        &format!("/institutions/{inst}/charges"),
        json!({"studentId": student, "description": "Term 2 fees", "lines": lines}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c}");
    let id = c["id"].as_str().unwrap().to_owned();
    let (s, c) = call(
        app,
        admin,
        "POST",
        &format!("/charges/{id}/publish"),
        json!({}),
        Some(v(&c)),
    )
    .await;
    assert_eq!(
        (s, c["status"].as_str()),
        (StatusCode::OK, Some("published")),
        "{c}"
    );
    c
}

#[tokio::test]
async fn gst_invoices_credit_notes_and_summary() {
    let (app, pool) = app(T_FLOW).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup =
        b5::finance_setup(&app, &admin, &pool, T_FLOW, "GST College", Some(b4::AARAV)).await;
    let inst = setup.institution().as_uuid().to_string();
    let student = setup.student.student_id.to_string();
    let tuition = setup.fee_head_id.as_uuid().to_string();
    let acc = setup.student_account_id.as_uuid().to_string();

    // ---- profile: defaults, validation, save.
    let (s, p) = read(&app, &admin, &format!("/institutions/{inst}/gst-profile")).await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["configured"], false);
    assert!(p["presets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["id"] == "preset:educationExempt"));
    let (s, bad) = call(&app, &admin, "PUT", &format!("/institutions/{inst}/gst-profile"),
        json!({"registrationType": "regular", "gstin": "27AAPFU0939F1ZX", "legalName": "GST College Trust"}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "check character: {bad}"
    );
    let (s, _) = call(&app, &admin, "PUT", &format!("/institutions/{inst}/gst-profile"),
        json!({"registrationType": "regular", "gstin": GSTIN, "legalName": "GST College Trust", "stateCode": "29"}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "GSTIN of another state"
    );
    let (s, _) = call(&app, &admin, "PUT", &format!("/institutions/{inst}/gst-profile"),
        json!({"registrationType": "regular", "gstin": GSTIN, "legalName": "GST College Trust", "invoicePrefix": "TAXINVOICE/{FY}/"}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "numbers over 16 characters"
    );
    let p = ok(
        &app,
        &admin,
        "PUT",
        &format!("/institutions/{inst}/gst-profile"),
        json!({
        "registrationType": "regular", "gstin": GSTIN, "legalName": "GST College Trust",
        "tradeName": "GST College", "address": "Station Road, Ahilyanagar 414001"}),
    )
    .await;
    assert_eq!(p["configured"], true);
    assert_eq!(p["stateCode"], "27", "state from the GSTIN");
    assert_eq!(p["stateName"], "Maharashtra");
    assert!(
        p["sampleNumbers"][0].as_str().unwrap().starts_with("TI/"),
        "{p}"
    );

    // ---- fee heads: tuition exempt, hostel 12 % exclusive.
    let hostel = ok(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/fee-heads"),
        json!({"code": "HOSTEL", "name": "Hostel fee", "category": "hostel"}),
    )
    .await;
    let hostel = hostel["id"].as_str().unwrap().to_owned();
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("/institutions/{inst}/gst-fee-heads/{hostel}"),
        json!({"sac": "996311", "gstRate": "7"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "not a slab");
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("/institutions/{inst}/gst-fee-heads/{tuition}"),
        json!({"sac": "9992", "gstRate": "18", "exempt": true}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "exempt is at rate 0");
    let t = ok(
        &app,
        &admin,
        "PUT",
        &format!("/institutions/{inst}/gst-fee-heads/{tuition}"),
        json!({"sac": "9992", "gstRate": "0", "exempt": true, "taxNote": "preset:educationExempt"}),
    )
    .await;
    assert_eq!(t["tax"]["treatment"], "exempt", "{t}");
    let h = ok(
        &app,
        &admin,
        "PUT",
        &format!("/institutions/{inst}/gst-fee-heads/{hostel}"),
        json!({"sac": "996311", "gstRate": "12"}),
    )
    .await;
    assert_eq!(h["tax"]["treatment"], "taxable", "{h}");

    // ---- tuition + hostel charge: CGST / SGST lines added, pending until publication.
    let (s, draft) = call(
        &app,
        &admin,
        "POST",
        &format!("/institutions/{inst}/charges"),
        json!({
        "studentId": student, "description": "Term 2 fees", "lines": [
            {"feeHeadId": tuition, "description": "Tuition", "amount": "50000.00"},
            {"feeHeadId": hostel, "description": "Hostel rent", "amount": "10000.00"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{draft}");
    assert_eq!(draft["totalAmount"], "61200.00", "{draft}");
    let lines: Vec<(String, String)> = draft["lines"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["description"].as_str().unwrap().to_owned(),
                l["amount"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        lines,
        vec![
            ("Tuition".to_owned(), "50000.00".to_owned()),
            ("Hostel rent".to_owned(), "10000.00".to_owned()),
            ("CGST @ 6%".to_owned(), "600.00".to_owned()),
            ("SGST @ 6%".to_owned(), "600.00".to_owned()),
        ]
    );
    let cid = draft["id"].as_str().unwrap().to_owned();
    let (s, pending) = read(&app, &admin, &format!("/charges/{cid}/tax-document")).await;
    assert_eq!(s, StatusCode::OK, "{pending}");
    assert_eq!(pending["status"], "pending");
    assert!(pending["number"].is_null());
    let gst_head = draft["lines"][2]["feeHeadId"].as_str().unwrap().to_owned();
    let (s, _) = call(&app, &admin, "POST", &format!("/institutions/{inst}/charges"), json!({
        "studentId": student, "description": "Sneaky", "lines": [{"feeHeadId": gst_head, "amount": "10.00"}]}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "GST lines are the server's");
    let (s, heads) = read(&app, &admin, &format!("/institutions/{inst}/gst-fee-heads")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(
        heads
            .as_array()
            .unwrap()
            .iter()
            .all(|h| !h["code"].as_str().unwrap().starts_with("GST-")),
        "{heads}"
    );

    let (s, published) = call(
        &app,
        &admin,
        "POST",
        &format!("/charges/{cid}/publish"),
        json!({}),
        Some(v(&draft)),
    )
    .await;
    assert_eq!(published["status"], "published", "{s} {published}");
    let (_, inv) = read(&app, &admin, &format!("/charges/{cid}/tax-document")).await;
    assert_eq!(inv["status"], "issued", "{inv}");
    assert_eq!(inv["kind"], "tax_invoice");
    assert_eq!(inv["title"], "Tax Invoice");
    let number = inv["number"].as_str().unwrap().to_owned();
    assert!(
        number.starts_with("TI/") && number.ends_with("/0001") && number.len() <= 16,
        "{number}"
    );
    assert_eq!(inv["total"], "61200.00");
    assert_eq!(inv["taxableValue"], "60000.00");
    assert_eq!(inv["cgst"], "600.00");
    assert_eq!(inv["igst"], "0.00");
    assert_eq!(inv["placeOfSupply"], "27-Maharashtra");
    assert_eq!(inv["supplier"]["gstin"], GSTIN);
    assert_eq!(
        inv["amountInWords"],
        "Rupees Sixty-One Thousand Two Hundred Only"
    );
    assert!(
        inv["taxNote"].as_str().unwrap().contains("12/2017"),
        "exempt line explained: {inv}"
    );
    assert_eq!(inv["lines"][0]["treatment"], "exempt");
    assert_eq!(inv["lines"][1]["cgst"], "600.00");
    assert_eq!(inv["taxes"][0]["label"], "CGST @ 6%");
    assert_eq!(output(&pool, &inst, "output_cgst").await, amount("600"));
    assert_eq!(output(&pool, &inst, "output_sgst").await, amount("600"));
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let (s, mine) = read(&app, &aarav, &format!("/me/charges/{cid}/tax-document")).await;
    assert_eq!(
        (s, mine["number"].as_str()),
        (StatusCode::OK, Some(number.as_str())),
        "{mine}"
    );
    let (s, _) = read(&app, &aarav, &format!("/charges/{cid}/tax-document")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ---- GST credit note: 10 % of the charge → 60 + 60 tax reversed.
    let (s, cn) = call(
        &app,
        &admin,
        "POST",
        &format!("/charges/{cid}/adjustments"),
        json!({"kind": "credit_note", "amount": "6120.00", "reason": "Hostel room changed"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{cn}");
    let cn_id = cn["id"].as_str().unwrap().to_owned();
    let (_, cn) = call(
        &app,
        &admin,
        "POST",
        &format!("/adjustments/{cn_id}/submit"),
        json!({}),
        Some(v(&cn)),
    )
    .await;
    assert_eq!(cn["status"], "posted", "{cn}");
    assert_eq!(output(&pool, &inst, "output_cgst").await, amount("540"));
    let (_, docs) = read(&app, &admin, &format!("/institutions/{inst}/gst-documents")).await;
    let note = docs
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "credit_note")
        .expect("credit note");
    assert!(
        note["number"].as_str().unwrap().starts_with("CN/"),
        "{note}"
    );
    assert_eq!(note["totalTax"], "120.00");
    let (_, note) = read(
        &app,
        &admin,
        &format!("/gst-documents/{}", note["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(note["original"]["number"], number.as_str(), "{note}");
    assert_eq!(note["taxableValue"], "6000.00");

    // ---- summary while the credit note stands.
    let (s, sum) = read(&app, &admin, &format!("/institutions/{inst}/gst-summary")).await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(sum["b2c"][0]["rate"], "12", "{sum}");
    assert_eq!(sum["b2c"][0]["amounts"]["taxableValue"], "10000.00");
    assert_eq!(sum["net"]["cgst"], "540.00");
    assert_eq!(sum["creditNotes"][0]["originalNumber"], number.as_str());
    assert_eq!(
        sum["exempt"]["intraExempt"], "45000.00",
        "50000 less the credited share"
    );
    let (s, h, csv) = send(
        &app,
        get(&admin, &format!("{F}/institutions/{inst}/gst-summary/csv")),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/csv"));
    let csv = csv
        .as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| csv.to_string());
    assert!(csv.contains("cdnur") && csv.contains("hsn"), "{csv}");

    // ---- reversing the credit note cancels it and restores the tax.
    let (s, cn) = call(
        &app,
        &admin,
        "POST",
        &format!("/adjustments/{cn_id}/reverse"),
        json!({"reason": "Room change withdrawn"}),
        Some(v(&cn)),
    )
    .await;
    assert_eq!(
        (s, cn["status"].as_str()),
        (StatusCode::OK, Some("reversed")),
        "{cn}"
    );
    assert_eq!(output(&pool, &inst, "output_cgst").await, amount("600"));
    let (_, docs) = read(&app, &admin, &format!("/institutions/{inst}/gst-documents")).await;
    let note = docs
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["kind"] == "credit_note")
        .unwrap();
    assert_eq!(note["status"], "cancelled");

    // ---- exempt only → bill of supply.
    let c = charge(
        &app,
        &admin,
        &inst,
        &student,
        json!([{"feeHeadId": tuition, "amount": "45000.00"}]),
    )
    .await;
    assert_eq!(c["totalAmount"], "45000.00");
    let (_, bos) = read(
        &app,
        &admin,
        &format!("/charges/{}/tax-document", c["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(bos["kind"], "bill_of_supply", "{bos}");
    assert!(bos["number"].as_str().unwrap().starts_with("BOS/"));
    assert_eq!(bos["totalTax"], "0.00");

    // ---- inter-state registered payer → IGST, B2B.
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("/student-accounts/{acc}/gst-details"),
        json!({"gstin": KA_GSTIN, "stateCode": "27"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "GSTIN of another state"
    );
    let party = ok(
        &app,
        &admin,
        "PUT",
        &format!("/student-accounts/{acc}/gst-details"),
        json!({"gstin": KA_GSTIN, "legalName": "Tata Sponsorships Ltd"}),
    )
    .await;
    assert_eq!(party["stateCode"], "29", "{party}");
    let c = charge(
        &app,
        &admin,
        &inst,
        &student,
        json!([{"feeHeadId": hostel, "amount": "10000.00"}]),
    )
    .await;
    assert_eq!(c["totalAmount"], "11200.00");
    let (_, igst) = read(
        &app,
        &admin,
        &format!("/charges/{}/tax-document", c["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(igst["igst"], "1200.00", "{igst}");
    assert_eq!(igst["interState"], true);
    assert_eq!(igst["recipient"]["name"], "Tata Sponsorships Ltd");
    assert!(igst["number"].as_str().unwrap().ends_with("/0002"));
    assert_eq!(output(&pool, &inst, "output_igst").await, amount("1200"));
    let (_, sum) = read(&app, &admin, &format!("/institutions/{inst}/gst-summary")).await;
    assert_eq!(sum["b2b"][0]["recipientGstin"], KA_GSTIN, "{sum}");
    assert_eq!(sum["b2b"][0]["invoiceValue"], "11200.00");
    assert_eq!(sum["net"]["igst"], "1200.00");
    assert!(
        sum["documents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["kind"] == "credit_note" && d["cancelled"] == 1),
        "{sum}"
    );
}

#[tokio::test]
async fn gst_routes_need_finance_permissions() {
    let (app, pool) = app(T_ACCESS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_ACCESS,
        "GST Access College",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid().to_string();
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    for path in [
        format!("/institutions/{inst}/gst-profile"),
        format!("/institutions/{inst}/gst-fee-heads"),
        format!("/institutions/{inst}/gst-summary"),
    ] {
        let (s, _) = read(&app, &aarav, &path).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path}");
    }
    let (s, _) = call(
        &app,
        &aarav,
        "PUT",
        &format!("/institutions/{inst}/gst-profile"),
        json!({"registrationType": "regular", "gstin": GSTIN, "legalName": "X"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(
        &app,
        axum::http::Request::get(format!("{F}/institutions/{inst}/gst-profile"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    // Without a GST profile a charge is untouched and has no tax document.
    let c = charge(
        &app,
        &admin,
        &inst,
        &setup.student.student_id.to_string(),
        json!([{"feeHeadId": setup.fee_head_id.as_uuid().to_string(), "amount": "25000.00"}]),
    )
    .await;
    assert_eq!(c["lines"].as_array().unwrap().len(), 1);
    let (s, _) = read(
        &app,
        &admin,
        &format!("/charges/{}/tax-document", c["id"].as_str().unwrap()),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}
