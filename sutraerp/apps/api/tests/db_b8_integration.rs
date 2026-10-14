//! Batch 8 integration — the cross-slice Campus · Services · Resources flow (8A → 8B → 8C → 8D)
//! against PostgreSQL, through the HTTP API (only the infrastructure tree, people and items are
//! seeded through `support::b8`):
//!
//! * `campus_services_resources_end_to_end` —
//!   8A: a classroom of an 8A building is booked (confirmed reservation);
//!   8A × 8B: a hostel room with two beds in service refuses a capacity cut below its beds;
//!   8B: hostel policy → self-service application → allocation → `HOSTEL-FEE` charge in Finance
//!   (a staff resident is recorded but never billed); the resident's hostel complaint is an 8A
//!   service ticket followed through triage → corrective work order → verification → resolved →
//!   confirmed by the student → closed; route + stops → activation → transport request → pass →
//!   `TRANSPORT-PASS` charge;
//!   8C: library policy → membership → catalogue record → copy in a library space → issue →
//!   overdue sweep → return → fine → billed (`LIB-FINE`); the student's Finance charges list
//!   shows the hostel fee, the transport pass and the library fine;
//!   8D: supplier → requisition → direct sourcing → purchase order → goods receipt → accepted
//!   stock + an asset placed in the 8A classroom → stock issue to a staff member → vendor
//!   invoice (three-way match) → AP posting (1410 / 2310) → part payment;
//!   8C reports: the metrics catalogue and a published operational dashboard read the hostel,
//!   transport, library, booking and procurement figures through `OperationalMetricsPort`.
//!
//! Dates follow the database's clock (the institution's business day).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::{Duration, NaiveDate};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::str::FromStr;
use support::b4;
use support::b8;
use support::*;
use sutra_contracts::campus::status::SpaceType;
use sutra_contracts::resources::status::ItemKind;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b8e00";
const C: &str = "/api/v1/campus";
const R: &str = "/api/v1/resources";
const S: &str = "/api/v1/services";

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
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

fn ver(v: &Value) -> Option<i64> {
    Some(
        v["version"]
            .as_i64()
            .unwrap_or_else(|| panic!("no version in {v}")),
    )
}

fn dec(v: &Value) -> Decimal {
    Decimal::from_str(v.as_str().unwrap_or_else(|| panic!("no decimal in {v}"))).unwrap()
}

/// Business "today" of the institution (its timezone).
async fn today(pool: &sqlx::PgPool, inst: Uuid) -> NaiveDate {
    sqlx::query_scalar(
        "SELECT (now() AT TIME ZONE timezone)::date FROM institution.institution WHERE id = $1",
    )
    .bind(inst)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Source type, fee head code and amount of a Finance charge.
async fn charge(pool: &sqlx::PgPool, charge_id: &str) -> (String, String, Decimal) {
    sqlx::query_as(
        "SELECT c.source_type, h.code, c.total_amount FROM finance.charge c \
         JOIN finance.charge_line l ON l.charge_id = c.id JOIN finance.fee_head h ON h.id = l.fee_head_id \
         WHERE c.id = $1::uuid",
    )
    .bind(charge_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn campus_services_resources_end_to_end() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT, "Integrated University").await;
    let inst = setup.institution().as_uuid().to_string();
    let day = today(&pool, setup.institution().as_uuid()).await;
    let h = b8::seed_hierarchy(
        &pool,
        &setup,
        "INT",
        &[
            ("101", SpaceType::Classroom, 40),
            ("H1", SpaceType::HostelRoom, 2),
            ("LIB", SpaceType::Library, 100),
        ],
    )
    .await;
    let classroom = h.spaces[0].id.as_uuid().to_string();
    let hostel_room = h.spaces[1].id.as_uuid().to_string();
    let library_space = h.spaces[2].id.as_uuid().to_string();
    let student = b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let meera = b8::seed_staff(&pool, &setup, "Meera", None).await;
    let staff_id = meera.staff_id.as_uuid().to_string();

    // ================================================================ 8A: room booking
    let booking = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/reservations"),
        json!({"nodeId": classroom, "date": (day + Duration::days(7)).to_string(),
            "startTime": "10:00", "endTime": "11:00", "purpose": "Orientation", "attendees": 30}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(booking["status"], "confirmed", "{booking}");

    // ================================================================ 8B: hostel
    let policy = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-policies"),
        json!({"name": "Hostel rules", "feePerTerm": "45000.00", "effectiveFrom": "2026-01-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let policy = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-policies/{}/submit", id(&policy)),
        json!({}),
        ver(&policy),
        StatusCode::OK,
    )
    .await;
    let policy = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-policies/{}/activate", id(&policy)),
        json!({}),
        ver(&policy),
        StatusCode::OK,
    )
    .await;
    assert_eq!(policy["status"], "active");
    for bed in ["A", "B"] {
        ok(
            &app,
            &admin,
            "POST",
            &format!("{C}/hostel-rooms/{hostel_room}/beds"),
            json!({"bedCode": bed}),
            None,
            StatusCode::CREATED,
        )
        .await;
    }

    // ---- 8A × 8B: the room keeps two beds in service, so its capacity cannot drop to one.
    let node = fetch(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{hostel_room}"),
    )
    .await;
    let node_version = node["version"]
        .as_i64()
        .or_else(|| node["node"]["version"].as_i64());
    let (s, b) = call(
        &app,
        &admin,
        "PUT",
        &format!("{C}/infrastructure-nodes/{hostel_room}/capacity"),
        json!({"capacity": 1, "reason": "convert to a single room"}),
        node_version,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "below the beds in service: {b}");
    assert!(b.to_string().contains("beds in service"), "{b}");

    // ---- self-service application → allocation → hostel fee in Finance (students only).
    let application = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/hostel/application"),
        json!({"institutionId": inst, "preference": "Near the library"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(application["status"], "approved", "{application}");
    let allocated = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&application)),
        json!({}),
        ver(&application),
        StatusCode::OK,
    )
    .await;
    let allocation = allocated["allocationId"].as_str().unwrap().to_owned();
    let residency = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-allocations/{allocation}"),
    )
    .await;
    assert_eq!(residency["status"], "active");
    let hostel_charge = residency["chargeId"].as_str().unwrap().to_owned();
    let hostel_charge_no = residency["chargeNumber"].as_str().unwrap().to_owned();
    assert_eq!(
        charge(&pool, &hostel_charge).await,
        (
            "campus_charge".to_owned(),
            "HOSTEL-FEE".to_owned(),
            b8::dec("45000.00")
        )
    );
    let staff_app = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        json!({"personKind": "staff", "personId": staff_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let staff_alloc = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&staff_app)),
        json!({}),
        ver(&staff_app),
        StatusCode::OK,
    )
    .await;
    let staff_residency = fetch(
        &app,
        &admin,
        &format!(
            "{C}/hostel-allocations/{}",
            staff_alloc["allocationId"].as_str().unwrap()
        ),
    )
    .await;
    assert!(
        staff_residency["chargeId"].is_null(),
        "staff are recorded but never billed: {staff_residency}"
    );

    // ---- hostel complaint → 8A service desk → work order → resolved → confirmed → closed.
    let complaint = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/hostel/complaints"),
        json!({"title": "No hot water", "description": "Since Monday", "priority": "high"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let ticket = id(&complaint);
    let tp = format!("{C}/tickets/{ticket}");
    let t = fetch(&app, &admin, &tp).await;
    assert_eq!(
        (
            t["ticket"]["source"].as_str(),
            t["ticket"]["status"].as_str()
        ),
        (Some("hostel"), Some("open")),
        "{t}"
    );
    let t = ok(
        &app,
        &admin,
        "POST",
        &format!("{tp}/triage"),
        json!({"priority": "high"}),
        ver(&t["ticket"]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t["status"], "triaged");
    let wo = ok(
        &app,
        &admin,
        "POST",
        &format!("{tp}/work-orders"),
        json!({"assignee": "tech.ravi"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let wp = format!(
        "{C}/work-orders/{}",
        wo["workOrder"]["id"].as_str().unwrap()
    );
    let wo = ok(
        &app,
        &admin,
        "POST",
        &format!("{wp}/start"),
        json!({}),
        ver(&wo),
        StatusCode::OK,
    )
    .await;
    let wo = ok(
        &app,
        &admin,
        "POST",
        &format!("{wp}/complete"),
        json!({"notes": "Geyser element replaced"}),
        ver(&wo),
        StatusCode::OK,
    )
    .await;
    let wo = ok(
        &app,
        &admin,
        "POST",
        &format!("{wp}/verify"),
        json!({"accepted": true, "notes": "Hot water restored"}),
        ver(&wo),
        StatusCode::OK,
    )
    .await;
    assert_eq!(wo["workOrder"]["status"], "verified");
    let t = fetch(&app, &admin, &tp).await;
    assert_eq!(t["ticket"]["status"], "resolved", "{t}");
    let t = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/tickets/{ticket}/confirm"),
        json!({"satisfied": true}),
        ver(&t["ticket"]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t["status"], "closed", "the resident confirms the fix");

    // ---- transport: route → stops → activation → request → pass → transport fee.
    let mut stops = Vec::new();
    for (code, name) in [("ST-A", "Main gate"), ("ST-B", "Railway station")] {
        let s = ok(
            &app,
            &admin,
            "POST",
            &format!("{C}/institutions/{inst}/transport-stops"),
            json!({"code": code, "name": name}),
            None,
            StatusCode::CREATED,
        )
        .await;
        stops.push(id(&s));
    }
    let route = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/transport-routes"),
        json!({"code": "R-01", "name": "City loop", "capacity": 40, "fare": "6000.00", "effectiveFrom": "2025-01-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid = id(&route);
    let versions = fetch(
        &app,
        &admin,
        &format!("{C}/transport-routes/{rid}/versions"),
    )
    .await;
    let v1 = versions["items"][0].clone();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/route-versions/{}/stops", id(&v1)),
        json!({"stops": [{"stopId": stops[0], "sequence": 1, "pickupTime": "07:30"},
                         {"stopId": stops[1], "sequence": 2, "pickupTime": "07:50"}]}),
        ver(&v1),
        StatusCode::OK,
    )
    .await;
    let route = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/activate"),
        json!({}),
        ver(&route),
        StatusCode::OK,
    )
    .await;
    assert_eq!(route["status"], "active");
    let ride = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/transport/request"),
        json!({"institutionId": inst, "routeId": rid, "stopId": stops[1]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(ride["status"], "active", "{ride}");
    let pass = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-allocations/{}/pass", id(&ride)),
        json!({"validFrom": "2025-01-01", "validTo": "2027-06-30"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(pass["status"], "active");
    let pass_charge = pass["chargeId"].as_str().unwrap().to_owned();
    assert_eq!(
        charge(&pool, &pass_charge).await,
        (
            "campus_charge".to_owned(),
            "TRANSPORT-PASS".to_owned(),
            b8::dec("6000.00")
        )
    );

    // ================================================================ 8C: library fine
    let lp = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-policies"),
        json!({"name": "Library rules", "effectiveFrom": "2026-01-01", "loanDaysStudent": 14,
            "loanDaysStaff": 30, "maxLoansStudent": 2, "maxLoansStaff": 5, "maxRenewals": 1,
            "finePerDay": "2.00", "maxFine": "50.00", "reservationHoldDays": 3, "lostChargeFactor": "1.5"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let lp = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{}/submit", id(&lp)),
        json!({}),
        ver(&lp),
        StatusCode::OK,
    )
    .await;
    let lp = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{}/activate", id(&lp)),
        json!({}),
        ver(&lp),
        StatusCode::OK,
    )
    .await;
    assert_eq!(lp["status"], "active");
    let member = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-memberships"),
        json!({"personKind": "student", "personId": student.student_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let record = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/catalogue"),
        json!({"title": "Fluid Mechanics", "authors": ["White"], "replacementCost": "400.00"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid_lib = id(&record);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{rid_lib}/publish"),
        json!({}),
        ver(&record),
        StatusCode::OK,
    )
    .await;
    let copy = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{rid_lib}/copies"),
        json!({"nodeId": library_space, "shelfMark": "ENG-1"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let loan = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-loans"),
        json!({"membershipId": id(&member), "copyId": id(&copy)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let loan_id = id(&loan);
    sqlx::query("UPDATE campus.library_loan SET issued_on = $2, due_on = $3 WHERE id = $1::uuid")
        .bind(&loan_id)
        .bind(day - Duration::days(19))
        .bind(day - Duration::days(5))
        .execute(&pool)
        .await
        .unwrap();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-loans/overdue-sweep"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    let loan = fetch(&app, &admin, &format!("{C}/library-loans/{loan_id}")).await;
    assert_eq!(loan["status"], "overdue", "{loan}");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-loans/{loan_id}/return"),
        json!({}),
        ver(&loan),
        StatusCode::OK,
    )
    .await;
    let fines = fetch(
        &app,
        &admin,
        &format!(
            "{C}/institutions/{inst}/library-fines?membershipId={}",
            id(&member)
        ),
    )
    .await;
    let fine = fines["items"][0].clone();
    assert_eq!(fine["amount"], "10.00", "5 days × 2.00: {fines}");
    let fine = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/bill", id(&fine)),
        json!({}),
        ver(&fine),
        StatusCode::OK,
    )
    .await;
    assert_eq!(fine["status"], "billed");
    let fine_charge = fine["chargeId"].as_str().unwrap().to_owned();
    assert_eq!(
        charge(&pool, &fine_charge).await,
        (
            "campus_charge".to_owned(),
            "LIB-FINE".to_owned(),
            b8::dec("10.00")
        )
    );

    // ---- the student's dues in Finance: hostel fee, transport pass, library fine.
    let mine = fetch(&app, &aarav, "/api/v1/finance/me/charges").await;
    let campus: Vec<&Value> = mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["sourceType"] == "campus_charge")
        .collect();
    assert_eq!(campus.len(), 3, "{mine}");
    assert!(campus
        .iter()
        .any(|c| c["chargeNumber"] == hostel_charge_no.as_str()));
    let dues: Decimal = campus.iter().map(|c| dec(&c["outstandingAmount"])).sum();
    assert_eq!(dues, b8::dec("51010.00"));

    // ================================================================ 8D: procure → stock → asset → AP
    let paper = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, None).await;
    let laptop = b8::seed_item(&pool, &setup, "LAPTOP", ItemKind::Asset, None).await;
    let store = b8::seed_store(&pool, &setup, "CENTRAL", None).await;
    let supplier = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Acme Traders", "pan": "ABCDE1234F", "email": "sales@acme.in",
            "bankAccountName": "Acme Traders", "bankAccountNumber": "001234567890", "bankIfsc": "HDFC0001234"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sup = id(&supplier);
    let supplier = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sup}/register"),
        json!({}),
        ver(&supplier),
        StatusCode::OK,
    )
    .await;
    let supplier = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sup}/submit"),
        json!({}),
        ver(&supplier),
        StatusCode::OK,
    )
    .await;
    assert_eq!(supplier["status"], "active");
    let req = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/requisitions"),
        json!({"title": "Hostel office supplies", "justification": "Term start", "lines": [
            {"itemId": paper, "quantity": "20", "estimatedUnitCost": "100.00"},
            {"itemId": laptop, "quantity": "1", "estimatedUnitCost": "50000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let req = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/requisitions/{}/submit", id(&req)),
        json!({}),
        ver(&req),
        StatusCode::OK,
    )
    .await;
    assert_eq!(req["status"], "released");
    let ev = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/sourcing-events"),
        json!({"requisitionId": id(&req), "method": "direct", "title": "Term supplies"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let ev_id = id(&ev);
    let ev = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/quotations"),
        json!({"supplierId": sup, "lines": [{"lineNo": 1, "unitPrice": "95.00"}, {"lineNo": 2, "unitPrice": "48000.00"}]}),
        ver(&ev),
        StatusCode::CREATED,
    )
    .await;
    let quote = ev["quotations"][0]["id"].as_str().unwrap().to_owned();
    ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/sourcing-events/{ev_id}/award"),
        json!({"quotationId": quote}),
        ver(&ev),
        StatusCode::OK,
    )
    .await;
    let po = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"sourcingEventId": ev_id, "deliveryLocationId": store}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let po_id = id(&po);
    assert_eq!(po["totalAmount"], "49900.00");
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
    let grn = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/receipts"),
        json!({"receivedOn": day.to_string(), "deliveryNote": "DN-1",
            "lines": [{"lineNo": 1, "quantity": "20"}, {"lineNo": 2, "quantity": "1"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let grn = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/goods-receipts/{}/accept", id(&grn)),
        json!({}),
        ver(&grn),
        StatusCode::OK,
    )
    .await;
    assert_eq!(grn["status"], "accepted");
    let on_hand = |b: &Value| {
        b["items"]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|l| l["onHand"].as_str())
            .unwrap_or("0")
            .to_owned()
    };
    let stock_path = format!(
        "{R}/institutions/{inst}/stock-on-hand?itemId={}&locationId={}",
        paper.as_uuid(),
        store.as_uuid()
    );
    assert_eq!(
        Decimal::from_str(&on_hand(&fetch(&app, &admin, &stock_path).await)).unwrap(),
        b8::dec("20")
    );
    // The laptop becomes an asset placed in the 8A classroom.
    let asset = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/assets"),
        json!({"goodsReceiptLineId": grn["lines"][1]["id"], "serialNumber": "LT-001"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(asset["acquisitionCost"], "48000.00");
    let asset = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/assets/{}/assign", id(&asset)),
        json!({"assigneeKind": "space", "assigneeId": classroom, "note": "lecture podium"}),
        ver(&asset),
        StatusCode::OK,
    )
    .await;
    assert_eq!(asset["status"], "assigned");
    // Stock issue to a staff member.
    let issue = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/stock-issues"),
        json!({"itemId": paper, "locationId": store, "quantity": "5", "purpose": "Hostel office",
            "issuedToStaffId": staff_id}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let iss = id(&issue);
    let issue = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{iss}/decision"),
        json!({"decision": "approve"}),
        ver(&issue),
        StatusCode::OK,
    )
    .await;
    let issue = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/stock-issues/{iss}/issue"),
        json!({}),
        ver(&issue),
        StatusCode::OK,
    )
    .await;
    assert_eq!(issue["status"], "issued");
    assert_eq!(
        Decimal::from_str(&on_hand(&fetch(&app, &admin, &stock_path).await)).unwrap(),
        b8::dec("15")
    );
    // Vendor invoice → three-way match → AP posting → part payment.
    let inv = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-INV-1", "invoiceDate": day.to_string(),
            "dueDate": (day + Duration::days(30)).to_string(),
            "lines": [{"lineNo": 1, "quantity": "20", "unitPrice": "95.00"}, {"lineNo": 2, "quantity": "1", "unitPrice": "48000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let inv_id = id(&inv);
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
    assert_eq!(inv["status"], "approved", "{inv}");
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
    let lines: Vec<(String, Decimal, Decimal)> = sqlx::query_as(
        "SELECT a.code, l.debit, l.credit FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE l.journal_entry_id = $1::uuid ORDER BY l.line_no",
    )
    .bind(inv["journalEntryId"].as_str().unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    let debit: Decimal = lines.iter().map(|l| l.1).sum();
    let credit: Decimal = lines.iter().map(|l| l.2).sum();
    assert_eq!((debit, credit), (b8::dec("49900.00"), b8::dec("49900.00")));
    assert!(lines
        .iter()
        .any(|l| l.0 == "2310" && l.2 == b8::dec("49900.00")));
    let pay = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-payments"),
        json!({"supplierId": sup, "paymentDate": day.to_string(),
            "allocations": [{"vendorInvoiceId": inv_id, "amount": "20000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let pay = ok(
        &app,
        &admin,
        "POST",
        &format!("{R}/vendor-payments/{}/post", id(&pay)),
        json!({}),
        ver(&pay),
        StatusCode::OK,
    )
    .await;
    assert_eq!(pay["status"], "posted");

    // ================================================================ 8C reports over every slice
    let metrics = fetch(&app, &admin, &format!("{S}/institutions/{inst}/metrics")).await;
    let value = |key: &str| {
        let m = metrics["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["key"] == key)
            .unwrap_or_else(|| panic!("metric {key} in {metrics}"));
        dec(&m["value"])
    };
    for (key, want) in [
        ("campus.upcoming_reservations", "1"),
        ("campus.open_work_orders", "0"),
        ("campus.hostel_beds", "2"),
        ("campus.hostel_beds_occupied", "2"),
        ("campus.hostel_residents", "2"),
        ("campus.transport_active_routes", "1"),
        ("campus.transport_active_passes", "1"),
        ("campus.library_members", "1"),
        ("campus.library_fines_unsettled", "10"),
        ("resources.active_suppliers", "1"),
        ("resources.assets_in_use", "1"),
        ("resources.asset_value", "48000"),
        ("resources.payables_outstanding", "29900"),
    ] {
        assert_eq!(value(key), b8::dec(want), "{key}");
    }
    assert!(value("student.active_students") >= Decimal::ONE);

    // An operational dashboard bound to those figures, published and rendered.
    let mut widgets = Vec::new();
    for (code, key) in [
        ("RESIDENTS", "campus.hostel_residents"),
        ("PASSES", "campus.transport_active_passes"),
        ("FINES", "campus.library_fines_unsettled"),
        ("PAYABLES", "resources.payables_outstanding"),
    ] {
        let w = ok(
            &app,
            &admin,
            "POST",
            &format!("{S}/institutions/{inst}/widgets"),
            json!({"code": code, "title": code, "metricKey": key}),
            None,
            StatusCode::CREATED,
        )
        .await;
        ok(
            &app,
            &admin,
            "POST",
            &format!("{S}/widgets/{}/activate", id(&w)),
            json!({}),
            ver(&w),
            StatusCode::OK,
        )
        .await;
        widgets.push(id(&w));
    }
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{S}/institutions/{inst}/dashboards"),
        json!({"code": "CAMPUS-OPS", "name": "Campus operations", "purpose": "Residence, transport, library and payables",
            "category": "operational"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let did = id(&d);
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/versions"),
        json!({"widgetIds": widgets, "changeNote": "v1"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/submit"),
        json!({}),
        ver(&d),
        StatusCode::OK,
    )
    .await;
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/publish"),
        json!({}),
        ver(&d),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "published");
    let view = fetch(&app, &admin, &format!("{S}/dashboards/{did}/view")).await;
    let shown: Vec<(String, Decimal)> = view["widgets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["metric"]["key"].as_str().unwrap().to_owned(),
                dec(&w["metric"]["value"]),
            )
        })
        .collect();
    assert_eq!(
        shown,
        vec![
            ("campus.hostel_residents".to_owned(), b8::dec("2")),
            ("campus.transport_active_passes".to_owned(), b8::dec("1")),
            ("campus.library_fines_unsettled".to_owned(), b8::dec("10")),
            (
                "resources.payables_outstanding".to_owned(),
                b8::dec("29900")
            ),
        ],
        "{view}"
    );
}
