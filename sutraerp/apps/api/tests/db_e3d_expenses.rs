//! ERP_EXTENSIONS 3D — staff expense claims over the HTTP API against PostgreSQL: receipt upload
//! (deduplicated by content, OCR disabled in tests), expenses with category policy and GST
//! checks, claims draft → submitted → approved → reimbursed (withdraw, return with a reason),
//! separation of duties, ledger postings (Dr 5510 / Cr 2310 on approval, Dr 2310 / Cr bank on
//! reimbursement), finance list and dashboard, receipt file access, settings.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use chrono::Duration;
use serde_json::{json, Value};
use support::*;
use support::{b4, b7};
use sutra_contracts::people::status::{EngagementStatus, StaffCategory};

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000e3d00";
const TENANT_SOD: &str = "0192f5a0-7c3e-7000-8000-0000000e3d01";
const R: &str = "/api/v1/resources";
/// 1×1 white PNG.
const PNG_WHITE: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4//8/AAX+Av4N70a4AAAAAElFTkSuQmCC";
/// 1×1 black PNG (a different file).
const PNG_BLACK: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGNgYGAAAAAEAAH2FzhVAAAAAElFTkSuQmCC";
const GSTIN: &str = "27AAPFU0939F1ZV";

async fn ok(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, None)).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn fetch(app: &axum::Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn num(v: &Value) -> f64 {
    match v {
        Value::String(s) => s.parse().unwrap(),
        other => other.as_f64().unwrap(),
    }
}

fn s(v: &Value) -> String {
    v.as_str().unwrap().to_owned()
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

/// A date a few days back: never in the future in the institution's zone, never too old.
fn spent_on() -> String {
    (chrono::Utc::now().date_naive() - Duration::days(3)).to_string()
}

/// Journal lines of a Finance entry as `(account code, debit, credit)`.
async fn journal(pool: &sqlx::PgPool, entry: uuid::Uuid) -> Vec<(String, String, String)> {
    let rows: Vec<(String, rust_decimal::Decimal, rust_decimal::Decimal)> = sqlx::query_as(
        "SELECT a.code, l.debit, l.credit FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE l.journal_entry_id = $1 ORDER BY l.line_no",
    )
    .bind(entry)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|(c, d, k)| (c, format!("{d:.2}"), format!("{k:.2}")))
        .collect()
}

/// The `(id, entry_number)` of the entry `source_type` posted for `source`.
async fn entry_of(
    pool: &sqlx::PgPool,
    source_type: &str,
    source: &str,
) -> (uuid::Uuid, String) {
    sqlx::query_as(
        "SELECT id, entry_number FROM finance.journal_entry WHERE tenant_id = $1::uuid \
         AND source_type = $2 AND source_id = $3::uuid",
    )
    .bind(TENANT)
    .bind(source_type)
    .bind(source)
    .fetch_one(pool)
    .await
    .expect("journal entry")
}

#[tokio::test]
async fn expense_claims_receipts_policy_approval_reimbursement() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let approver = login(&app, "procurement.approver@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "Expenses College").await;
    let inst = setup.institution().as_uuid();
    let staff = b7::seed_staff(
        &pool,
        &setup,
        "Meera",
        Some(b4::MEERA),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let day = spent_on();

    // Policy as the claimant sees it: the defaults until finance saves settings.
    let mine = fetch(&app, &meera, &format!("{R}/me/expenses")).await;
    assert_eq!(mine["institutionId"], inst.to_string(), "{mine}");
    assert_eq!(num(&mine["settings"]["receiptRequiredAbove"]), 250.0);
    assert_eq!(mine["settings"]["maxAgeDays"], 90);
    let cats = mine["settings"]["categories"].as_array().unwrap();
    assert_eq!(cats.len(), 10, "{mine}");
    let travel = cats.iter().find(|c| c["id"] == "travel").unwrap();
    assert_eq!(num(&travel["receiptAbove"]), 0.0);
    assert_eq!(mine["ocrEnabled"], false);
    assert!(mine["unclaimed"].as_array().unwrap().is_empty());
    assert!(mine["claims"].as_array().unwrap().is_empty());

    // Receipts: stored once per file, read by no OCR engine here.
    let up = |data: &str, name: &str, ctype: &str| {
        json!({"fileName": name, "contentType": ctype, "dataBase64": data})
    };
    let receipt = ok(
        &app,
        &meera,
        "POST",
        &format!("{R}/me/expense-receipts"),
        up(PNG_WHITE, "dinner bill.png", "image/png"),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(receipt["duplicate"], false);
    assert_eq!(receipt["reading"]["status"], "disabled", "{receipt}");
    assert_eq!(receipt["contentType"], "image/png");
    let r1 = s(&receipt["id"]);
    let again = ok(
        &app,
        &meera,
        "POST",
        &format!("{R}/me/expense-receipts"),
        up(PNG_WHITE, "same bill again.png", "image/png"),
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["duplicate"], true);
    assert_eq!(again["id"], r1.as_str());
    ok(
        &app,
        &meera,
        "POST",
        &format!("{R}/me/expense-receipts"),
        up(PNG_WHITE, "bill.gif", "image/gif"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &meera,
        "POST",
        &format!("{R}/me/expense-receipts"),
        up("not base64!!", "bill.png", "image/png"),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let ticket = ok(
        &app,
        &meera,
        "POST",
        &format!("{R}/me/expense-receipts"),
        up(PNG_BLACK, "bus ticket.png", "image/png"),
        StatusCode::CREATED,
    )
    .await;
    let r2 = s(&ticket["id"]);

    // Expenses.
    let base = format!("{R}/me/expenses");
    let meals = ok(
        &app,
        &meera,
        "POST",
        &base,
        json!({"spentOn": day, "merchant": "Hotel Shivneri", "amount": "378.00",
               "categoryId": "meals", "paymentMethod": "Card", "tax": "18.00",
               "gstin": "27aapfu0939f1zv", "receiptId": r1}),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(meals["gstin"], GSTIN, "uppercased: {meals}");
    assert_eq!(num(&meals["amount"]), 378.0);
    assert_eq!(num(&meals["tax"]), 18.0);
    assert_eq!(meals["receiptId"], r1.as_str());
    assert_eq!(meals["categoryName"], "Meals (DA)");
    assert!(meals["flags"].as_array().unwrap().is_empty(), "{meals}");
    let meals_id = s(&meals["id"]);
    let travel = ok(
        &app,
        &meera,
        "POST",
        &base,
        json!({"spentOn": day, "merchant": "MSRTC Shivneri", "amount": "900.00",
               "categoryId": "travel", "paymentMethod": "Cash"}),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(travel["flags"], json!(["receipt_missing"]), "{travel}");
    let travel_id = s(&travel["id"]);
    ok(
        &app,
        &meera,
        "POST",
        &base,
        json!({"spentOn": day, "merchant": "X", "amount": "100.00", "gstin": "27AAPFU0939F1ZX"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &meera,
        "POST",
        &base,
        json!({"spentOn": day, "merchant": "X", "amount": "100.00", "tax": "150.00"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &meera,
        "POST",
        &base,
        json!({"spentOn": day, "merchant": "X", "amount": "100.00", "categoryId": "yachts"}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let mine = fetch(&app, &meera, &format!("{R}/me/expenses")).await;
    assert_eq!(mine["unclaimed"].as_array().unwrap().len(), 2, "{mine}");
    assert_eq!(num(&mine["unclaimedTotal"]), 1278.0);

    // A claim with both lines: the missing bus-ticket receipt blocks submission.
    let claim = ok(
        &app,
        &meera,
        "POST",
        &format!("{R}/me/expense-claims"),
        json!({"title": "Pune workshop", "purpose": "NAAC workshop",
               "expenseIds": [meals_id, travel_id]}),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(claim["status"], "draft", "{claim}");
    assert_eq!(claim["canSubmit"], false);
    assert_eq!(claim["nextStep"], "Submit claim");
    assert_eq!(claim["lineCount"], 2);
    assert_eq!(claim["withReceipt"], 1);
    assert_eq!(num(&claim["total"]), 1278.0);
    assert_eq!(num(&claim["taxTotal"]), 18.0);
    assert!(claim["flags"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f == "receipt_missing"));
    assert_eq!(claim["staffId"], staff.staff_id.as_uuid().to_string());
    assert!(claim["number"].as_str().unwrap().starts_with("EXP"), "{claim}");
    let cid = s(&claim["id"]);
    let me_claim = format!("{R}/me/expense-claims/{cid}");
    let office = format!("{R}/expense-claims/{cid}");
    let (st, _, b) = send(
        &app,
        write(
            &meera,
            "POST",
            &format!("{me_claim}/submit"),
            json!({"version": ver(&claim)}),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{b}");
    assert!(b.to_string().contains("receipt_missing"), "{b}");

    // Attach the ticket (the line's version moved when it joined the claim).
    let line = claim["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == travel_id.as_str())
        .unwrap()
        .clone();
    let fixed = ok(
        &app,
        &meera,
        "PUT",
        &format!("{base}/{travel_id}"),
        json!({"spentOn": day, "merchant": "MSRTC Shivneri", "amount": "900.00",
               "categoryId": "travel", "paymentMethod": "Cash", "receiptId": r2,
               "version": ver(&line)}),
        StatusCode::OK,
    )
    .await;
    assert!(fixed["flags"].as_array().unwrap().is_empty(), "{fixed}");
    assert_eq!(fixed["claimId"], cid.as_str());
    let claim = fetch(&app, &meera, &office).await;
    assert_eq!(claim["canSubmit"], true, "{claim}");

    let submitted = ok(
        &app,
        &meera,
        "POST",
        &format!("{me_claim}/submit"),
        json!({"version": ver(&claim)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(submitted["status"], "submitted", "{submitted}");
    assert_eq!(submitted["nextStep"], "Approve");
    assert_eq!(submitted["canSubmit"], false);
    assert_eq!(num(&submitted["total"]), 1278.0);
    assert!(!submitted["submittedAt"].is_null());
    // Lines are locked once submitted.
    let locked_line = submitted["lines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == meals_id.as_str())
        .unwrap()
        .clone();
    ok(
        &app,
        &meera,
        "PUT",
        &format!("{base}/{meals_id}"),
        json!({"spentOn": day, "merchant": "Hotel Shivneri", "amount": "3780.00",
               "categoryId": "meals", "receiptId": r1, "version": ver(&locked_line)}),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &meera,
        "DELETE",
        &format!("{base}/{meals_id}"),
        json!({}),
        StatusCode::CONFLICT,
    )
    .await;

    // Withdraw and submit again.
    let back = ok(
        &app,
        &meera,
        "POST",
        &format!("{me_claim}/withdraw"),
        json!({"version": ver(&submitted)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(back["status"], "draft", "{back}");
    assert!(back["submittedAt"].is_null());
    let submitted = ok(
        &app,
        &meera,
        "POST",
        &format!("{me_claim}/submit"),
        json!({"version": ver(&back)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(submitted["status"], "submitted");
    let dash = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/expense-dashboard"),
    )
    .await;
    assert_eq!(dash["awaitingApproval"], 1, "{dash}");
    assert_eq!(num(&dash["awaitingApprovalAmount"]), 1278.0);

    // Decisions: never the claimant; a return needs a reason.
    ok(
        &app,
        &meera,
        "POST",
        &format!("{office}/decision"),
        json!({"decision": "approve", "version": ver(&submitted)}),
        StatusCode::FORBIDDEN,
    )
    .await;
    ok(
        &app,
        &approver,
        "POST",
        &format!("{office}/decision"),
        json!({"decision": "return", "version": ver(&submitted)}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &approver,
        "POST",
        &format!("{office}/decision"),
        json!({"decision": "maybe", "version": ver(&submitted)}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let returned = ok(
        &app,
        &approver,
        "POST",
        &format!("{office}/decision"),
        json!({"decision": "return", "reason": "Attach the hotel's GST invoice",
               "version": ver(&submitted)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(returned["status"], "draft", "{returned}");
    assert_eq!(returned["returnedReason"], "Attach the hotel's GST invoice");
    let mine = fetch(&app, &meera, &format!("{R}/me/expenses")).await;
    assert_eq!(
        mine["claims"][0]["returnedReason"],
        "Attach the hotel's GST invoice",
        "{mine}"
    );
    let resubmitted = ok(
        &app,
        &meera,
        "POST",
        &format!("{me_claim}/submit"),
        json!({"version": ver(&returned)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(resubmitted["status"], "submitted");
    assert!(resubmitted["returnedReason"].is_null(), "{resubmitted}");

    let approved = ok(
        &app,
        &approver,
        "POST",
        &format!("{office}/decision"),
        json!({"decision": "approve", "reason": "OK", "version": ver(&resubmitted)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(approved["status"], "approved", "{approved}");
    assert_eq!(approved["nextStep"], "Mark reimbursed");
    assert_eq!(approved["decidedBy"], "dev|procurement.approver@sutra.dev");
    assert_eq!(approved["decisionNote"], "OK");
    assert!(!approved["approvedAt"].is_null());
    let approval_entry = s(&approved["approvalEntry"]);
    let (entry, number) = entry_of(&pool, "procurement_invoice", &cid).await;
    assert_eq!(number, approval_entry);
    assert_eq!(
        journal(&pool, entry).await,
        vec![
            ("5510".to_owned(), "1278.00".to_owned(), "0.00".to_owned()),
            ("2310".to_owned(), "0.00".to_owned(), "1278.00".to_owned()),
        ]
    );
    ok(
        &app,
        &approver,
        "POST",
        &format!("{office}/decision"),
        json!({"decision": "approve", "version": ver(&approved)}),
        StatusCode::CONFLICT,
    )
    .await;
    ok(
        &app,
        &meera,
        "POST",
        &format!("{me_claim}/withdraw"),
        json!({"version": ver(&approved)}),
        StatusCode::CONFLICT,
    )
    .await;

    // Reimbursement: procurement manage only.
    ok(
        &app,
        &approver,
        "POST",
        &format!("{office}/reimburse"),
        json!({"reference": "UTR123", "version": ver(&approved)}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let paid = ok(
        &app,
        &admin,
        "POST",
        &format!("{office}/reimburse"),
        json!({"reference": "UTR123", "version": ver(&approved)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(paid["status"], "reimbursed", "{paid}");
    assert!(paid["nextStep"].is_null());
    assert_eq!(paid["paymentReference"], "UTR123");
    assert!(!paid["reimbursedAt"].is_null());
    let payment_entry = s(&paid["paymentEntry"]);
    let (pentry, pnumber) = entry_of(&pool, "procurement_payment", &cid).await;
    assert_eq!(pnumber, payment_entry);
    let plines = journal(&pool, pentry).await;
    assert_eq!(plines.len(), 2, "{plines:?}");
    assert_eq!(
        plines[0],
        ("2310".to_owned(), "1278.00".to_owned(), "0.00".to_owned())
    );
    assert_eq!(plines[1].2, "1278.00");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{office}/reimburse"),
        json!({"reference": "UTR123", "version": ver(&paid)}),
        StatusCode::CONFLICT,
    )
    .await;

    // Finance views.
    let list = fetch(
        &app,
        &approver,
        &format!("{R}/institutions/{inst}/expense-claims?status=reimbursed"),
    )
    .await;
    let list = list.as_array().unwrap();
    assert_eq!(list.len(), 1, "{list:?}");
    assert_eq!(list[0]["id"], cid.as_str());
    assert_eq!(list[0]["lineCount"], 2);
    assert_eq!(num(&list[0]["total"]), 1278.0);
    let drafts = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/expense-claims?status=draft"),
    )
    .await;
    assert!(drafts.as_array().unwrap().is_empty());
    let (st, _, _) = send(
        &app,
        get(
            &admin,
            &format!("{R}/institutions/{inst}/expense-claims?status=paid"),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);
    let (st, _, _) = send(
        &app,
        get(&meera, &format!("{R}/institutions/{inst}/expense-claims")),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let dash = fetch(
        &app,
        &admin,
        &format!("{R}/institutions/{inst}/expense-dashboard"),
    )
    .await;
    assert_eq!(dash["awaitingApproval"], 0, "{dash}");
    assert_eq!(dash["awaitingReimbursement"], 0);
    let trend = dash["trend"].as_array().unwrap();
    assert_eq!(trend.len(), 6, "{dash}");
    let spent: f64 = trend.iter().map(|m| num(&m["amount"])).sum();
    assert!((spent - 1278.0).abs() < 1e-6, "{dash}");
    let by_cat = dash["byCategory"].as_array().unwrap();
    assert_eq!(by_cat.len(), 2, "{dash}");
    assert_eq!(by_cat[0]["categoryId"], "travel");
    assert_eq!(dash["ocrMismatches"], 0);

    // The claimant reads the claim and the receipt file.
    let mine_view = fetch(&app, &meera, &office).await;
    assert_eq!(mine_view["status"], "reimbursed");
    let (st, h, _) = send(
        &app,
        get(&meera, &format!("{R}/expense-receipts/{r1}/file")),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(h["content-type"].to_str().unwrap(), "image/png");
    let (st, _, _) = send(
        &app,
        get(&approver, &format!("{R}/expense-receipts/{r1}/file")),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "finance reads receipts");

    // Settings: finance manages them.
    let settings = format!("{R}/institutions/{inst}/expense-settings");
    let cats = json!([
        {"id": "travel", "name": "Travel (TA)", "receiptAbove": "0.00"},
        {"id": "meals", "name": "Meals (DA)", "limit": "500.00"},
        {"id": "travel", "name": "Travel again"}
    ]);
    ok(
        &app,
        &admin,
        "PUT",
        &settings,
        json!({"receiptRequiredAbove": "200.00", "maxAgeDays": 60, "categories": cats}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &admin,
        "PUT",
        &settings,
        json!({"receiptRequiredAbove": "200.00", "maxAgeDays": 1000,
               "categories": [{"id": "other", "name": "Other"}]}),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    ok(
        &app,
        &approver,
        "PUT",
        &settings,
        json!({"receiptRequiredAbove": "200.00", "maxAgeDays": 60,
               "categories": [{"id": "other", "name": "Other"}]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let saved = ok(
        &app,
        &admin,
        "PUT",
        &settings,
        json!({"receiptRequiredAbove": "200.00", "maxAgeDays": 60, "categories": [
            {"id": "travel", "name": " Travel (TA) ", "receiptAbove": "0.00"},
            {"id": "meals", "name": "Meals (DA)", "limit": "500.00"}
        ]}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(saved["categories"][0]["name"], "Travel (TA)", "trimmed: {saved}");
    let read = fetch(&app, &admin, &settings).await;
    assert_eq!(num(&read["receiptRequiredAbove"]), 200.0, "{read}");
    assert_eq!(read["maxAgeDays"], 60);
    assert_eq!(read["categories"].as_array().unwrap().len(), 2);
    assert_eq!(num(&read["categories"][1]["limit"]), 500.0);
    let mine = fetch(&app, &meera, &format!("{R}/me/expenses")).await;
    assert_eq!(num(&mine["settings"]["receiptRequiredAbove"]), 200.0);

    // Another staff member signed in with the same login cannot see Meera's receipt: move the
    // login to a new staff record (the subject resolves per request).
    sqlx::query("UPDATE people.staff_member SET subject_ref = NULL WHERE id = $1")
        .bind(staff.staff_id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    b7::seed_staff(
        &pool,
        &setup,
        "Ravi",
        Some(b4::MEERA),
        StaffCategory::NonTeaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let (st, _, b) = send(
        &app,
        get(&meera, &format!("{R}/expense-receipts/{r1}/file")),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{b}");
    let (st, _, _) = send(&app, get(&meera, &office)).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let other = fetch(&app, &meera, &format!("{R}/me/expenses")).await;
    assert!(other["claims"].as_array().unwrap().is_empty(), "{other}");
}

/// A claimant who also holds `resources.approve` still cannot decide their own claim.
#[tokio::test]
async fn expense_claim_approver_cannot_decide_own_claim() {
    let (app, pool) = app(TENANT_SOD).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let approver = login(&app, "procurement.approver@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_SOD, "Expenses SoD College").await;
    b7::seed_staff(
        &pool,
        &setup,
        "Dhruv",
        Some("dev|admin@sutra.dev"),
        StaffCategory::NonTeaching,
        Some(EngagementStatus::Active),
    )
    .await;
    // Under the receipt threshold: no receipt needed.
    let e = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/me/expenses"),
        json!({"spentOn": spent_on(), "merchant": "Auto rickshaw", "amount": "120.00",
               "categoryId": "local_conveyance"}),
        StatusCode::CREATED,
    )
    .await;
    assert!(e["flags"].as_array().unwrap().is_empty(), "{e}");
    let claim = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/me/expense-claims"),
        json!({"expenseIds": [s(&e["id"])]}),
        StatusCode::CREATED,
    )
    .await;
    assert!(
        claim["title"].as_str().unwrap().ends_with("expenses"),
        "month title: {claim}"
    );
    let cid = s(&claim["id"]);
    let submitted = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/me/expense-claims/{cid}/submit"),
        json!({"version": ver(&claim)}),
        StatusCode::OK,
    )
    .await;
    let (st, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{R}/expense-claims/{cid}/decision"),
            json!({"decision": "approve", "version": ver(&submitted)}),
            None,
        ),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{b}");
    assert!(b.to_string().contains("separation of duties"), "{b}");
    let approved = ok(
        &app,
        &approver,
        "POST",
        &format!("{R}/expense-claims/{cid}/decision"),
        json!({"decision": "approve", "version": ver(&submitted)}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(approved["status"], "approved", "{approved}");
    assert_eq!(num(&approved["total"]), 120.0);
}
