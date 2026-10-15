//! Batch 8 slice 8B — hostel & transport (Domain G; contract `docs/batch8-contracts.md`).
//!
//! * `hostel_policy_admission_residency_and_clearance` — L3-G-092 policy versions (draft →
//!   approved → active → superseded), beds of `hostel_room` spaces (capacity, room type), G-078
//!   applications (self-service + office) → `HOSTEL_ADMISSION` (auto-approved), G-079 allocation
//!   → active residency + `HOSTEL-FEE` charge in Finance (students only), G-080 no free bed →
//!   waiting list → vacancy offer on closure, G-082 resident leave → out → overdue → late return,
//!   G-087 room transfer → bed swap, G-089 inspection → damage → `HOSTEL-DAMAGE` charge, G-090 /
//!   091 mess allocation + change (capacity), G-088 hostel complaints → service desk tickets,
//!   G-081 vacating → clearance (open / unpaid damage refused) → closed occupancy, `/me/hostel`,
//!   withdrawal, permissions.
//! * `transport_routes_passengers_passes_and_trips` — G-096 stops, G-093 route → activation
//!   (`ROUTE_ACTIVATION`), G-099 request → allocation (seat capacity, waiting request), G-113
//!   pass + `TRANSPORT-PASS` charge, G-114 renewal, G-115 replacement, G-116 suspension /
//!   restoration, G-100 allocation change (pass follows), G-097 stop suspension → flags, G-094 new
//!   effective version (old superseded, dropped stop flagged), G-095 route suspension, G-108 /
//!   102 vehicles + drivers, G-104 trip readiness (capacity, checklist, driver), G-109 breakdown →
//!   replacement vehicle, G-106 trip run → completion, G-105 cancellation, G-101 withdrawal,
//!   transport complaints, `/me/transport`.
//! * `governed_decisions_await_a_decision` — `ApprovalMode::AwaitDecision`: hostel policy
//!   (reject → resubmit → approve), `HOSTEL_ADMISSION` (reject, then approve → allocate), resident
//!   leave, room transfer (reject → residency active again), route activation.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b4;
use support::b8;
use support::*;
use sutra_campus::shared::approval::ApprovalMode;
use sutra_contracts::campus::status::SpaceType;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const TENANT_H: &str = "0192f5a0-7c3e-7000-8000-0000000b8b00";
const TENANT_T: &str = "0192f5a0-7c3e-7000-8000-0000000b8b01";
const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000b8b02";
const C: &str = "/api/v1/campus";

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

async fn read(app: &Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

async fn fetch(app: &Router, who: &Login, path: &str) -> Value {
    let (s, b) = read(app, who, path).await;
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

async fn charge_amount(pool: &sqlx::PgPool, charge_id: &str) -> (Decimal, String) {
    let r: (Decimal, String) =
        sqlx::query_as("SELECT total_amount, source_type FROM finance.charge WHERE id = $1")
            .bind(Uuid::parse_str(charge_id).unwrap())
            .fetch_one(pool)
            .await
            .unwrap();
    r
}

fn ev(name: &str) -> String {
    format!("sutraerp.campus.{name}")
}

// ====================================================================== hostel

#[tokio::test]
async fn hostel_policy_admission_residency_and_clearance() {
    let (app, pool) = app(TENANT_H).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_H, "Hostel University").await;
    let inst = setup.institution().as_uuid().to_string();
    let h = b8::seed_hierarchy(
        &pool,
        &setup,
        "HST",
        &[
            ("101", SpaceType::HostelRoom, 2),
            ("102", SpaceType::HostelRoom, 1),
            ("103", SpaceType::HostelRoom, 1),
            ("LAB", SpaceType::Lab, 10),
        ],
    )
    .await;
    let (r101, r102, r103, lab) = (
        h.spaces[0].id.as_uuid().to_string(),
        h.spaces[1].id.as_uuid().to_string(),
        h.spaces[2].id.as_uuid().to_string(),
        h.spaces[3].id.as_uuid().to_string(),
    );
    let me = b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let diya = b8::seed_student(&pool, &setup, "Diya", None).await;
    let kabir = b8::seed_student(&pool, &setup, "Kabir", None).await;
    let isha = b8::seed_student(&pool, &setup, "Isha", None).await;
    let meera = b8::seed_staff(&pool, &setup, "Meera", None).await;

    // ---- G-092: policy versions; admissions need an active policy.
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/hostel/application"),
        json!({"institutionId": inst}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no active policy yet");
    let p1 = ok(&app, &admin, "POST", &format!("{C}/institutions/{inst}/hostel-policies"),
        json!({"name": "2026 policy", "feePerTerm": "45000.00", "deposit": "5000", "curfewTime": "21:30",
               "eligibilityNote": "UG and PG students; staff on request", "maxStayTerms": 8, "effectiveFrom": "2026-06-01"}),
        None, StatusCode::CREATED).await;
    assert_eq!(p1["status"], "draft");
    assert_eq!(p1["feePerTerm"], "45000.00");
    assert_eq!(p1["deposit"], "5000.00");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-policies/{}/activate", id(&p1)),
        json!({}),
        ver(&p1),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a draft cannot be activated");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-policies"),
        json!({"name": "Bad", "feePerTerm": "-1", "effectiveFrom": "2026-06-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-policies/{}/submit", id(&p1)),
        json!({}),
        ver(&p1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "approved");
    assert!(p1["processInstanceId"].is_string());
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-policies/{}/activate", id(&p1)),
        json!({}),
        ver(&p1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "active");
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-policies"),
        json!({"name": "2027 policy", "feePerTerm": "48000.00", "effectiveFrom": "2027-01-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-policies/{}/submit", id(&p2)),
        json!({}),
        ver(&p2),
        StatusCode::OK,
    )
    .await;
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-policies/{}/activate", id(&p2)),
        json!({}),
        ver(&p2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p2["status"], "active");
    let p1 = fetch(&app, &admin, &format!("{C}/hostel-policies/{}", id(&p1))).await;
    assert_eq!(
        p1["status"], "superseded",
        "one active policy per institution"
    );
    let active = fetch(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/hostel-policies?status=active"),
    )
    .await;
    assert_eq!(active["items"].as_array().unwrap().len(), 1);

    // ---- beds: hostel rooms only, at most `capacity` beds per room.
    for (room, bed) in [(&r101, "A"), (&r101, "B"), (&r102, "A")] {
        ok(
            &app,
            &admin,
            "POST",
            &format!("{C}/hostel-rooms/{room}/beds"),
            json!({"bedCode": bed}),
            None,
            StatusCode::CREATED,
        )
        .await;
    }
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{r101}/beds"),
        json!({"bedCode": "C"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "room 101 holds 2 beds");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{lab}/beds"),
        json!({"bedCode": "A"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a lab is not a hostel room"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{r102}/beds"),
        json!({"bedCode": "A"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let beds = fetch(&app, &admin, &format!("{C}/hostel-rooms/{r101}/beds")).await;
    assert_eq!(beds["items"].as_array().unwrap().len(), 2);
    assert_eq!(beds["items"][0]["roomCode"], "HST-B1-101");

    // ---- G-078: self-service + office applications → HOSTEL_ADMISSION (auto-approved).
    let app_me = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/hostel/application"),
        json!({"institutionId": inst, "preference": "Ground floor"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(app_me["status"], "approved");
    assert_eq!(app_me["channel"], "self_service");
    assert!(app_me["number"].as_str().unwrap().starts_with("HAP-"));
    assert!(app_me["processInstanceId"].is_string());
    assert_eq!(app_me["personId"], me.student_id.to_string());
    assert_eq!(
        events(&pool, &id(&app_me), &ev("hostel_application.decided")).await,
        1
    );
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/hostel/application"),
        json!({"institutionId": inst}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open application per person");
    let office = |p: Uuid, kind: &str| json!({"personKind": kind, "personId": p});
    let app_diya = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        office(diya.student_id, "student"),
        None,
        StatusCode::CREATED,
    )
    .await;
    let app_meera = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        office(meera.staff_id.as_uuid(), "staff"),
        None,
        StatusCode::CREATED,
    )
    .await;
    let app_kabir = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        office(kabir.student_id, "student"),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(app_meera["status"], "approved");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        office(Uuid::now_v7(), "student"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown person");
    let (s, _) = read(
        &app,
        &aarav,
        &format!("{C}/institutions/{inst}/hostel-applications"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "office queue needs campus.read");
    let queue = fetch(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/hostel-applications?status=approved"),
    )
    .await;
    assert_eq!(queue["items"].as_array().unwrap().len(), 4);

    // ---- G-079: allocation → active residency + hostel fee (students) through CampusChargePort.
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&app_me)),
        json!({}),
        ver(&app_me),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "allocated");
    let alloc_me = a["allocationId"].as_str().unwrap().to_owned();
    let res_me = fetch(&app, &admin, &format!("{C}/hostel-allocations/{alloc_me}")).await;
    assert_eq!(res_me["status"], "active");
    assert_eq!(res_me["feeAmount"], "48000.00", "fee of the active policy");
    assert!(res_me["chargeNumber"].as_str().unwrap().starts_with("CHG-"));
    let (amount, source) = charge_amount(&pool, res_me["chargeId"].as_str().unwrap()).await;
    assert_eq!(
        (amount, source.as_str()),
        (b8::dec("48000.00"), "campus_charge")
    );
    assert_eq!(
        events(&pool, &alloc_me, &ev("hostel_allocation.activated")).await,
        1
    );
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&app_diya)),
        json!({}),
        ver(&app_diya),
        StatusCode::OK,
    )
    .await;
    let alloc_diya = a["allocationId"].as_str().unwrap().to_owned();
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&app_meera)),
        json!({}),
        ver(&app_meera),
        StatusCode::OK,
    )
    .await;
    let alloc_meera = a["allocationId"].as_str().unwrap().to_owned();
    let res_meera = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-allocations/{alloc_meera}"),
    )
    .await;
    assert!(
        res_meera["chargeId"].is_null(),
        "staff are not billed in Batch 8"
    );
    assert_eq!(res_meera["personKind"], "staff");
    let occupancy = fetch(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/hostel-beds?status=occupied"),
    )
    .await;
    assert_eq!(occupancy["items"].as_array().unwrap().len(), 3);

    // ---- G-080: no free bed → waiting list.
    let w = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&app_kabir)),
        json!({}),
        ver(&app_kabir),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "waitlisted");
    assert!(w["waitlistedAt"].is_string());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&w)),
        json!({}),
        ver(&w),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "still no bed");

    // ---- G-082: resident leave → out → overdue → late return (self-service request).
    let lv = ok(&app, &aarav, "POST", &format!("{C}/me/hostel/leave"),
        json!({"fromDate": "2020-01-01", "toDate": "2020-01-02", "destination": "Home", "reason": "Festival"}), None, StatusCode::CREATED).await;
    assert_eq!(lv["status"], "approved");
    assert_eq!(
        events(&pool, &id(&lv), &ev("resident_leave.decided")).await,
        1
    );
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/hostel/leave"),
        json!({"fromDate": "2020-01-02", "toDate": "2020-01-03"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "overlapping leave");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/resident-leaves/{}/decision", id(&lv)),
        json!({"decision": "approve"}),
        ver(&lv),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already decided");
    let lv = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/resident-leaves/{}/depart", id(&lv)),
        json!({}),
        ver(&lv),
        StatusCode::OK,
    )
    .await;
    assert_eq!(lv["status"], "out");
    let res = fetch(&app, &admin, &format!("{C}/hostel-allocations/{alloc_me}")).await;
    assert_eq!(res["status"], "on_leave");
    let sweep = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/resident-leaves/overdue-check"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(sweep["processed"], 1);
    let lv = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-allocations/{alloc_me}/leaves"),
    )
    .await["items"][0]
        .clone();
    assert_eq!(lv["status"], "overdue");
    let lv = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/resident-leaves/{}/return", id(&lv)),
        json!({}),
        ver(&lv),
        StatusCode::OK,
    )
    .await;
    assert_eq!(lv["status"], "returned");
    assert_eq!(lv["returnedLate"], true);
    let res = fetch(&app, &admin, &format!("{C}/hostel-allocations/{alloc_me}")).await;
    assert_eq!(res["status"], "active");
    let lv2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_diya}/leave"),
        json!({"fromDate": "2099-01-01", "toDate": "2099-01-05"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(lv2["status"], "approved");
    let lv2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/resident-leaves/{}/depart", id(&lv2)),
        json!({}),
        ver(&lv2),
        StatusCode::OK,
    )
    .await;
    let lv2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/resident-leaves/{}/return", id(&lv2)),
        json!({}),
        ver(&lv2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(lv2["returnedLate"], false);

    // ---- G-087: room transfer → completed bed swap.
    let free = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{r103}/beds"),
        json!({"bedCode": "A"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let before = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-allocations/{alloc_diya}"),
    )
    .await;
    let tr = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_diya}/transfer"),
        json!({"toBedId": id(&free), "reason": "Quieter room"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(tr["status"], "completed");
    assert_eq!(tr["toRoomCode"], "HST-B1-103");
    assert_eq!(
        events(&pool, &id(&tr), &ev("room_transfer.completed")).await,
        1
    );
    let after = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-allocations/{alloc_diya}"),
    )
    .await;
    assert_eq!(after["status"], "active");
    assert_eq!(after["bedId"], id(&free));
    assert_eq!(after["roomCode"], "HST-B1-103");
    let old_bed = fetch(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/hostel-beds"),
    )
    .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["id"] == before["bedId"])
        .cloned()
        .unwrap();
    assert_eq!(old_bed["status"], "available");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_diya}/transfer"),
        json!({"toBedId": id(&free)}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the bed is taken");
    assert_eq!(
        fetch(
            &app,
            &admin,
            &format!("{C}/hostel-allocations/{alloc_diya}/transfers")
        )
        .await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // ---- G-089: inspection → damage → HOSTEL-DAMAGE charge; clean inspection → closed.
    let room_me = res_me["roomNodeId"].as_str().unwrap().to_owned();
    let insp = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{room_me}/inspections"),
        json!({"scheduledOn": "2026-11-20", "allocationId": alloc_me}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(insp["status"], "scheduled");
    let insp = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/room-inspections/{}/complete", id(&insp)),
        json!({"findings": "Broken window", "damageAmount": "1200.00"}),
        ver(&insp),
        StatusCode::OK,
    )
    .await;
    assert_eq!(insp["status"], "charged");
    assert_eq!(insp["damageAmount"], "1200.00");
    let (amount, _) = charge_amount(&pool, insp["chargeId"].as_str().unwrap()).await;
    assert_eq!(amount, b8::dec("1200.00"));
    assert_eq!(
        events(&pool, &id(&insp), &ev("room_inspection.completed")).await,
        1
    );
    let clean = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{room_me}/inspections"),
        json!({"scheduledOn": "2026-11-21"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let clean = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/room-inspections/{}/complete", id(&clean)),
        json!({"findings": "OK"}),
        ver(&clean),
        StatusCode::OK,
    )
    .await;
    assert_eq!(clean["status"], "completed");
    let clean = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/room-inspections/{}/close", id(&clean)),
        json!({}),
        ver(&clean),
        StatusCode::OK,
    )
    .await;
    assert_eq!(clean["status"], "closed");
    let room_meera = res_meera["roomNodeId"].as_str().unwrap().to_owned();
    let insp_meera = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{room_meera}/inspections"),
        json!({"scheduledOn": "2026-11-20", "allocationId": alloc_meera}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let insp_meera = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/room-inspections/{}/complete", id(&insp_meera)),
        json!({"damageAmount": "300.00"}),
        ver(&insp_meera),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        insp_meera["status"], "damage_found",
        "staff damage is recorded, not billed"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-rooms/{lab}/inspections"),
        json!({"scheduledOn": "2026-11-20"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // ---- G-090 / 091: mess allocation (capacity) and change.
    let veg = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/messes"),
        json!({"code": "veg", "name": "Veg mess", "menuType": "veg", "capacity": 1}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(veg["code"], "VEG");
    let mix = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/messes"),
        json!({"code": "MIX", "name": "Main mess", "menuType": "mixed", "capacity": 5}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let m1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/messes/{}/allocations", id(&veg)),
        json!({"hostelAllocationId": alloc_me, "startDate": "2025-01-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(m1["status"], "active");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/messes/{}/allocations", id(&veg)),
        json!({"hostelAllocationId": alloc_diya, "startDate": "2025-01-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "veg mess is full");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/messes/{}/allocations", id(&mix)),
        json!({"hostelAllocationId": alloc_me, "startDate": "2025-01-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one active mess per residency");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/messes/{}/allocations", id(&mix)),
        json!({"hostelAllocationId": alloc_diya, "startDate": "2025-01-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let m2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/mess-allocations/{}/change", id(&m1)),
        json!({"messId": id(&mix), "effectiveFrom": "2025-02-01", "reason": "Prefers mixed menu"}),
        ver(&m1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m2["status"], "active");
    assert_eq!(m2["messCode"], "MIX");
    assert_eq!(m2["previousId"], id(&m1));
    let old = fetch(
        &app,
        &admin,
        &format!("{C}/messes/{}/allocations", id(&veg)),
    )
    .await;
    assert_eq!(old["items"][0]["status"], "changed");
    assert_eq!(old["items"][0]["endDate"], "2025-01-31");
    let messes = fetch(&app, &admin, &format!("{C}/institutions/{inst}/messes")).await;
    let mix_row = messes["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["code"] == "MIX")
        .cloned()
        .unwrap();
    assert_eq!(mix_row["assigned"], 2);

    // ---- G-088: hostel complaints → service desk tickets (source hostel).
    let c1 = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/hostel/complaints"),
        json!({"title": "No hot water", "description": "Since Monday", "priority": "high"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert!(c1["number"].as_str().unwrap().starts_with("SR-"));
    assert_eq!(
        (
            c1["source"].as_str(),
            c1["category"].as_str(),
            c1["status"].as_str()
        ),
        (Some("hostel"), Some("hostel"), Some("open"))
    );
    assert_eq!(c1["sourceRefId"], alloc_me);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_diya}/complaints"),
        json!({"title": "Fan broken"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        fetch(
            &app,
            &admin,
            &format!("{C}/hostel-allocations/{alloc_me}/complaints")
        )
        .await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        events(&pool, &id(&c1), &ev("service_ticket.opened")).await,
        1
    );

    // ---- /me/hostel.
    let mine = fetch(&app, &aarav, &format!("{C}/me/hostel")).await;
    assert_eq!(mine["person"]["kind"], "student");
    assert_eq!(mine["application"]["status"], "allocated");
    assert_eq!(mine["allocation"]["id"], alloc_me);
    assert_eq!(mine["leaves"].as_array().unwrap().len(), 1);
    assert_eq!(mine["complaints"].as_array().unwrap().len(), 1);

    // ---- G-081: vacating → clearance → closed occupancy, vacancy offered to the waiting list.
    let res = fetch(&app, &admin, &format!("{C}/hostel-allocations/{alloc_me}")).await;
    let res = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_me}/vacate"),
        json!({"reason": "Course over"}),
        ver(&res),
        StatusCode::OK,
    )
    .await;
    assert_eq!(res["status"], "vacating");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_me}/close"),
        json!({}),
        ver(&res),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "unpaid damage charge: {b}");
    let res = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-allocations/{alloc_meera}"),
    )
    .await;
    let res = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_meera}/vacate"),
        json!({}),
        ver(&res),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_meera}/close"),
        json!({}),
        ver(&res),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "unbilled damage is still open");
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/room-inspections/{}/close", id(&insp_meera)),
        json!({"reason": "Deducted from deposit"}),
        ver(&insp_meera),
        StatusCode::OK,
    )
    .await;
    let closed = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_meera}/close"),
        json!({}),
        ver(&res),
        StatusCode::OK,
    )
    .await;
    assert_eq!(closed["status"], "closed");
    assert!(closed["closedAt"].is_string());
    assert_eq!(
        events(&pool, &alloc_meera, &ev("hostel_allocation.closed")).await,
        1
    );
    let kab = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-applications/{}", id(&app_kabir)),
    )
    .await;
    assert_eq!(
        kab["status"], "approved",
        "vacancy offered to the waiting list"
    );
    let kab = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&kab)),
        json!({}),
        ver(&kab),
        StatusCode::OK,
    )
    .await;
    assert_eq!(kab["status"], "allocated");
    let res = fetch(
        &app,
        &admin,
        &format!("{C}/hostel-allocations/{alloc_diya}"),
    )
    .await;
    let res = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_diya}/vacate"),
        json!({}),
        ver(&res),
        StatusCode::OK,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-allocations/{alloc_diya}/close"),
        json!({}),
        ver(&res),
        StatusCode::OK,
    )
    .await;
    let mess_diya = fetch(
        &app,
        &admin,
        &format!("{C}/messes/{}/allocations", id(&mix)),
    )
    .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["hostelAllocationId"] == alloc_diya)
        .cloned()
        .unwrap();
    assert_eq!(
        mess_diya["status"], "ended",
        "the mess assignment ends with the residency"
    );

    // ---- withdrawal.
    let app_isha = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        office(isha.student_id, "student"),
        None,
        StatusCode::CREATED,
    )
    .await;
    let app_isha = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/withdraw", id(&app_isha)),
        json!({"reason": "Found a flat"}),
        ver(&app_isha),
        StatusCode::OK,
    )
    .await;
    assert_eq!(app_isha["status"], "withdrawn");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/hostel-applications/{}/withdraw", id(&app_isha)),
        json!({}),
        ver(&app_isha),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // S3-appr (SoD): the requester is refused first; an independent administrator gets the
    // stale-version answer.
    let dean = login(&app, "dean@sutra.dev").await;
    let (s, _) = call(
        &app,
        &dean,
        "POST",
        &format!("{C}/hostel-applications/{}/decision", id(&app_isha)),
        json!({"decision": "approve"}),
        Some(1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale If-Match");

    // ---- history trail.
    let hist: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.status_history WHERE aggregate_type = 'hostel_application' AND aggregate_id = $1",
    )
    .bind(Uuid::parse_str(&id(&app_me)).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        hist >= 4,
        "submitted → under_review → approved → allocated ({hist})"
    );
}

// ====================================================================== transport

#[tokio::test]
async fn transport_routes_passengers_passes_and_trips() {
    let (app, pool) = app(TENANT_T).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_T, "Transport University").await;
    let inst = setup.institution().as_uuid().to_string();
    let _me = b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let diya = b8::seed_student(&pool, &setup, "Diya", None).await;
    let kabir = b8::seed_student(&pool, &setup, "Kabir", None).await;
    let ravi = b8::seed_staff(&pool, &setup, "Ravi", None).await;

    // ---- G-096: stops.
    let mut stops = Vec::new();
    for (code, name) in [
        ("ST-A", "Main gate"),
        ("ST-B", "Market"),
        ("ST-C", "Railway station"),
    ] {
        let s = ok(
            &app,
            &admin,
            "POST",
            &format!("{C}/institutions/{inst}/transport-stops"),
            json!({"code": code, "name": name, "landmark": "Near the signal"}),
            None,
            StatusCode::CREATED,
        )
        .await;
        assert_eq!(s["status"], "active");
        stops.push(id(&s));
    }
    let (sa, sb, sc) = (stops[0].clone(), stops[1].clone(), stops[2].clone());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/transport-stops"),
        json!({"code": "ST-A", "name": "Dup"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // ---- G-093: route → stops → activation (ROUTE_ACTIVATION, auto-approved).
    let r = ok(&app, &admin, "POST", &format!("{C}/institutions/{inst}/transport-routes"),
        json!({"code": "R-01", "name": "City loop", "capacity": 2, "fare": "6000.00", "effectiveFrom": "2025-01-01"}), None, StatusCode::CREATED).await;
    assert_eq!(r["status"], "draft");
    assert_eq!(r["hasDraft"], true);
    let rid = id(&r);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/activate"),
        json!({}),
        ver(&r),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no stops yet");
    let versions = fetch(
        &app,
        &admin,
        &format!("{C}/transport-routes/{rid}/versions"),
    )
    .await;
    let v1 = versions["items"][0].clone();
    assert_eq!(
        (v1["versionNo"].as_i64(), v1["status"].as_str()),
        (Some(1), Some("draft"))
    );
    let v1 = ok(&app, &admin, "POST", &format!("{C}/route-versions/{}/stops", id(&v1)),
        json!({"stops": [{"stopId": sa, "sequence": 1, "pickupTime": "07:30"}, {"stopId": sb, "sequence": 2, "pickupTime": "07:45"}]}),
        ver(&v1), StatusCode::OK).await;
    assert_eq!(v1["stops"].as_array().unwrap().len(), 2);
    assert_eq!(v1["stops"][1]["code"], "ST-B");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/route-versions/{}/stops", id(&v1)),
        json!({"stops": [{"stopId": sa, "sequence": 1}, {"stopId": sa, "sequence": 2}]}),
        ver(&v1),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a stop once per version"
    );
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/activate"),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "active");
    assert_eq!(r["currentVersionNo"], 1);
    assert_eq!(r["fare"], "6000.00");
    assert!(r["processInstanceId"].is_null());
    assert_eq!(
        events(&pool, &rid, &ev("transport_route.activated")).await,
        1
    );
    let v1 = fetch(
        &app,
        &admin,
        &format!("{C}/transport-routes/{rid}/versions"),
    )
    .await["items"][0]
        .clone();
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/route-versions/{}/stops", id(&v1)),
        json!({"stops": [{"stopId": sa, "sequence": 1}, {"stopId": sc, "sequence": 2}]}),
        ver(&v1),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "an active version is never rewritten"
    );

    // ---- G-099: request → allocation (seat capacity 2).
    let a_me = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/transport/request"),
        json!({"institutionId": inst, "routeId": rid, "stopId": sa}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a_me["status"], "active");
    assert_eq!(a_me["channel"], "self_service");
    assert_eq!(
        events(&pool, &id(&a_me), &ev("transport_allocation.activated")).await,
        1
    );
    let a_diya = ok(&app, &admin, "POST", &format!("{C}/institutions/{inst}/transport-allocations"),
        json!({"personKind": "student", "personId": diya.student_id, "routeId": rid, "stopId": sb, "startDate": "2025-01-01"}), None, StatusCode::CREATED).await;
    assert_eq!(a_diya["status"], "active");
    let a_kabir = ok(&app, &admin, "POST", &format!("{C}/institutions/{inst}/transport-allocations"),
        json!({"personKind": "student", "personId": kabir.student_id, "routeId": rid, "stopId": sa}), None, StatusCode::CREATED).await;
    assert_eq!(
        a_kabir["status"], "requested",
        "route full: waiting for a seat"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-allocations/{}/activate", id(&a_kabir)),
        json!({}),
        ver(&a_kabir),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/transport-allocations"),
        json!({"personKind": "student", "personId": diya.student_id, "routeId": rid, "stopId": sc}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "stop C is not on the route"
    );

    // ---- G-113: pass issuance + TRANSPORT-PASS charge.
    let p = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-allocations/{}/pass", id(&a_me)),
        json!({"validFrom": "2025-01-01", "validTo": "2026-06-30"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(p["status"], "active");
    assert_eq!(p["kind"], "new");
    assert!(p["number"].as_str().unwrap().starts_with("TP-"));
    assert_eq!(p["feeAmount"], "6000.00");
    let (amount, source) = charge_amount(&pool, p["chargeId"].as_str().unwrap()).await;
    assert_eq!(
        (amount, source.as_str()),
        (b8::dec("6000.00"), "campus_charge")
    );
    assert_eq!(
        events(&pool, &id(&p), &ev("transport_pass.issued")).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-allocations/{}/pass", id(&a_me)),
        json!({"validFrom": "2025-01-01", "validTo": "2026-06-30"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one live pass per allocation");

    // ---- G-114: renewal.
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-passes/{}/renew", id(&p)),
        json!({"validTo": "2026-12-31"}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (p2["kind"].as_str(), p2["validFrom"].as_str()),
        (Some("renewal"), Some("2026-07-01"))
    );
    assert_eq!(p2["replacesId"], id(&p));
    assert!(p2["chargeId"].is_string() && p2["chargeId"] != p["chargeId"]);
    assert_eq!(
        fetch(&app, &admin, &format!("{C}/transport-passes/{}", id(&p))).await["status"],
        "expired"
    );

    // ---- G-115: lost pass → replacement.
    let p3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-passes/{}/replace", id(&p2)),
        json!({"reason": "Lost", "fee": "200.00"}),
        ver(&p2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (p3["kind"].as_str(), p3["validTo"].as_str()),
        (Some("replacement"), Some("2026-12-31"))
    );
    let (amount, _) = charge_amount(&pool, p3["chargeId"].as_str().unwrap()).await;
    assert_eq!(amount, b8::dec("200.00"));
    assert_eq!(
        fetch(&app, &admin, &format!("{C}/transport-passes/{}", id(&p2))).await["status"],
        "replaced"
    );

    // ---- G-116: suspension / restoration.
    let p3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-passes/{}/suspend", id(&p3)),
        json!({"reason": "Misconduct"}),
        ver(&p3),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p3["status"], "suspended");
    assert_eq!(
        events(&pool, &id(&p3), &ev("transport_pass.suspended")).await,
        1
    );
    let p3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-passes/{}/restore", id(&p3)),
        json!({}),
        ver(&p3),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p3["status"], "active");

    // ---- G-100: allocation change (the live pass follows).
    let a_me2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-allocations/{}/change", id(&a_me)),
        json!({"stopId": sb, "effectiveFrom": "2099-01-01", "reason": "Moved house"}),
        ver(&a_me),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (a_me2["status"].as_str(), a_me2["stopCode"].as_str()),
        (Some("active"), Some("ST-B"))
    );
    assert_eq!(a_me2["previousId"], id(&a_me));
    assert_eq!(a_me2["passId"], id(&p3));
    let old = fetch(
        &app,
        &admin,
        &format!("{C}/transport-allocations/{}", id(&a_me)),
    )
    .await;
    assert_eq!(
        (old["status"].as_str(), old["endDate"].as_str()),
        (Some("changed"), Some("2098-12-31"))
    );

    // ---- G-097: stop suspension flags the passengers boarding there.
    let stop_b = fetch(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/transport-stops"),
    )
    .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["code"] == "ST-B")
        .cloned()
        .unwrap();
    assert_eq!(stop_b["passengers"], 2);
    let stop_b = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-stops/{sb}/suspend"),
        json!({"reason": "Road works"}),
        ver(&stop_b),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stop_b["status"], "suspended");
    let flagged = fetch(
        &app,
        &admin,
        &format!("{C}/transport-allocations/{}", id(&a_diya)),
    )
    .await;
    assert_eq!(
        (flagged["flagged"].as_bool(), flagged["flagReason"].as_str()),
        (Some(true), Some("stop suspended"))
    );
    let stop_b = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-stops/{sb}/restore"),
        json!({}),
        ver(&stop_b),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stop_b["status"], "active");
    assert_eq!(
        fetch(
            &app,
            &admin,
            &format!("{C}/transport-allocations/{}", id(&a_diya))
        )
        .await["flagged"],
        false
    );

    // ---- G-094: modification → new effective version; old superseded; dropped stop flagged.
    let r = fetch(&app, &admin, &format!("{C}/transport-routes/{rid}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/versions"),
        json!({"effectiveFrom": "2024-12-01"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a new version starts after the current one"
    );
    let v2 = ok(&app, &admin, "POST", &format!("{C}/transport-routes/{rid}/versions"),
        json!({"effectiveFrom": "2026-01-01", "capacity": 3, "fare": "6500.00", "note": "Station added, market dropped"}), None, StatusCode::CREATED).await;
    assert_eq!(
        (
            v2["versionNo"].as_i64(),
            v2["stops"].as_array().unwrap().len()
        ),
        (Some(2), 2)
    );
    let v2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/route-versions/{}/stops", id(&v2)),
        json!({"stops": [{"stopId": sa, "sequence": 1}, {"stopId": sc, "sequence": 2}]}),
        ver(&v2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v2["stops"][1]["code"], "ST-C");
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/activate"),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            r["status"].as_str(),
            r["currentVersionNo"].as_i64(),
            r["capacity"].as_i64()
        ),
        (Some("active"), Some(2), Some(3))
    );
    let versions = fetch(
        &app,
        &admin,
        &format!("{C}/transport-routes/{rid}/versions"),
    )
    .await;
    let v1 = versions["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["versionNo"] == 1)
        .cloned()
        .unwrap();
    assert_eq!(
        (v1["status"].as_str(), v1["effectiveTo"].as_str()),
        (Some("superseded"), Some("2025-12-31"))
    );
    assert_eq!(v1["stops"][1]["code"], "ST-B", "history is kept");
    let flagged = fetch(
        &app,
        &admin,
        &format!("{C}/transport-allocations/{}", id(&a_diya)),
    )
    .await;
    assert_eq!(
        flagged["flagReason"],
        "stop not on the active route version"
    );
    let a_kabir = fetch(
        &app,
        &admin,
        &format!("{C}/transport-allocations/{}", id(&a_kabir)),
    )
    .await;
    let a_kabir = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-allocations/{}/activate", id(&a_kabir)),
        json!({}),
        ver(&a_kabir),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a_kabir["status"], "active", "the new version seats 3");

    // ---- G-095: suspension flags passengers; re-activation clears them.
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/suspend"),
        json!({"reason": "Bridge closed"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "suspended");
    assert_eq!(
        events(&pool, &rid, &ev("transport_route.suspended")).await,
        1
    );
    assert_eq!(
        fetch(
            &app,
            &admin,
            &format!("{C}/transport-allocations/{}", id(&a_kabir))
        )
        .await["flagReason"],
        "route suspended"
    );
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/activate"),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "active");
    assert_eq!(
        fetch(
            &app,
            &admin,
            &format!("{C}/transport-allocations/{}", id(&a_kabir))
        )
        .await["flagged"],
        false
    );

    // ---- G-108 / 102: vehicles and drivers.
    let small = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/vehicles"),
        json!({"registrationNo": "ka01ab0001", "label": "Van", "capacity": 2}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (small["registrationNo"].as_str(), small["status"].as_str()),
        (Some("KA01AB0001"), Some("available"))
    );
    let bus = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/vehicles"),
        json!({"registrationNo": "KA01AB0002", "label": "Bus 1", "capacity": 40}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let spare = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/vehicles"),
        json!({"registrationNo": "KA01AB0003", "label": "Bus 2", "capacity": 40}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let driver = ok(&app, &admin, "POST", &format!("{C}/transport-routes/{rid}/drivers"),
        json!({"staffId": ravi.staff_id.as_uuid(), "licenceNo": "DL-0420110012345", "validFrom": "2025-01-01"}), None, StatusCode::CREATED).await;
    assert_eq!(
        (driver["status"].as_str(), driver["routeCode"].as_str()),
        (Some("active"), Some("R-01"))
    );
    let (s, _) = call(&app, &admin, "POST", &format!("{C}/transport-routes/{rid}/drivers"),
        json!({"staffId": ravi.staff_id.as_uuid(), "licenceNo": "DL-0420110012345", "validFrom": "2025-01-01"}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "once per route while active");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/drivers"),
        json!({"staffId": Uuid::now_v7(), "licenceNo": "DL-1", "validFrom": "2025-01-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // ---- G-104: trip planning → ready (capacity, checklist, driver).
    let trip = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/trips"),
        json!({"tripDate": "2026-11-20", "direction": "pickup"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(trip["status"], "planned");
    assert_eq!(trip["allocatedPassengers"], 3);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/trips"),
        json!({"tripDate": "2026-11-20", "direction": "pickup"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "one trip per route, date and direction"
    );
    let checklist = json!({"brakes": true, "tyres": true, "firstAid": true});
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/ready", id(&trip)),
        json!({"vehicleId": id(&small), "driverAssignmentId": id(&driver), "checklist": checklist}),
        ver(&trip),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the van seats 2 for 3 passengers");
    let (s, _) = call(&app, &admin, "POST", &format!("{C}/trips/{}/ready", id(&trip)),
        json!({"vehicleId": id(&bus), "driverAssignmentId": id(&driver), "checklist": {"brakes": true, "tyres": false}}), ver(&trip)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "incomplete checklist");
    let trip = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/ready", id(&trip)),
        json!({"vehicleId": id(&bus), "driverAssignmentId": id(&driver), "checklist": checklist}),
        ver(&trip),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            trip["status"].as_str(),
            trip["vehicleRegistration"].as_str()
        ),
        (Some("ready"), Some("KA01AB0002"))
    );
    let bus = fetch(&app, &admin, &format!("{C}/institutions/{inst}/vehicles")).await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == bus["id"])
        .cloned()
        .unwrap();
    assert_eq!(bus["status"], "assigned");

    // ---- G-109: breakdown → replacement vehicle on the trip.
    let bus = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/vehicles/{}/breakdown", id(&bus)),
        json!({"reason": "Engine failure"}),
        ver(&bus),
        StatusCode::OK,
    )
    .await;
    assert_eq!(bus["status"], "broken_down");
    let trip =
        fetch(&app, &admin, &format!("{C}/transport-routes/{rid}/trips")).await["items"][0].clone();
    assert_eq!(trip["needsVehicle"], true);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/start", id(&trip)),
        json!({}),
        ver(&trip),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "replace the vehicle first");
    let trip = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/replace-vehicle", id(&trip)),
        json!({"vehicleId": id(&spare), "reason": "Bus 1 broke down"}),
        ver(&trip),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (
            trip["needsVehicle"].as_bool(),
            trip["vehicleRegistration"].as_str()
        ),
        (Some(false), Some("KA01AB0003"))
    );

    // ---- G-106: run → completion (vehicle released).
    let trip = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/start", id(&trip)),
        json!({}),
        ver(&trip),
        StatusCode::OK,
    )
    .await;
    assert_eq!(trip["status"], "in_progress");
    let trip = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/complete", id(&trip)),
        json!({"passengerCount": 3}),
        ver(&trip),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (trip["status"].as_str(), trip["passengerCount"].as_i64()),
        (Some("completed"), Some(3))
    );
    assert_eq!(events(&pool, &id(&trip), &ev("trip.completed")).await, 1);
    let vehicles = fetch(&app, &admin, &format!("{C}/institutions/{inst}/vehicles")).await;
    let spare = vehicles["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == spare["id"])
        .cloned()
        .unwrap();
    assert_eq!(spare["status"], "available");

    // ---- G-105: cancellation.
    let drop = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/trips"),
        json!({"tripDate": "2026-11-20", "direction": "drop"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/cancel", id(&drop)),
        json!({"reason": " "}),
        ver(&drop),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "a reason is required");
    let drop = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/trips/{}/cancel", id(&drop)),
        json!({"reason": "Holiday declared"}),
        ver(&drop),
        StatusCode::OK,
    )
    .await;
    assert_eq!(drop["status"], "cancelled");
    assert_eq!(events(&pool, &id(&drop), &ev("trip.cancelled")).await, 1);
    assert_eq!(
        fetch(
            &app,
            &admin,
            &format!("{C}/institutions/{inst}/trips?status=cancelled")
        )
        .await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // ---- vehicle availability (G-108): broken → maintenance → available → retired.
    let bus = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/vehicles/{}/status", id(&bus)),
        json!({"status": "under_maintenance"}),
        ver(&bus),
        StatusCode::OK,
    )
    .await;
    let bus = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/vehicles/{}/status", id(&bus)),
        json!({"status": "available"}),
        ver(&bus),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/vehicles/{}/status", id(&bus)),
        json!({"status": "assigned"}),
        ver(&bus),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "assignment happens on trips"
    );
    let bus = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/vehicles/{}/status", id(&bus)),
        json!({"status": "retired"}),
        ver(&bus),
        StatusCode::OK,
    )
    .await;
    assert_eq!(bus["status"], "retired");

    // ---- complaints + self-service.
    let c = ok(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/transport/complaints"),
        json!({"title": "Bus was late"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (c["source"].as_str(), c["sourceRefId"].as_str()),
        (Some("transport"), Some(id(&a_me2).as_str()))
    );
    let mine = fetch(&app, &aarav, &format!("{C}/me/transport")).await;
    assert_eq!(mine["allocation"]["id"], id(&a_me2));
    assert_eq!(mine["pass"]["id"], id(&p3));
    assert_eq!(mine["complaints"].as_array().unwrap().len(), 1);
    assert_eq!(
        fetch(&app, &aarav, &format!("{C}/me/transport/pass")).await["number"],
        p3["number"]
    );
    let routes = fetch(
        &app,
        &aarav,
        &format!("{C}/me/transport/routes?institutionId={inst}"),
    )
    .await;
    assert_eq!(routes["items"].as_array().unwrap().len(), 1);
    let (s, _) = read(
        &app,
        &aarav,
        &format!("{C}/institutions/{inst}/transport-allocations"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ---- G-101: withdrawal (a live pass is cancelled); retirement needs no passengers.
    let a_me2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-allocations/{}/withdraw", id(&a_me2)),
        json!({"reason": "Left"}),
        ver(&a_me2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a_me2["status"], "withdrawn");
    assert!(a_me2["passId"].is_null());
    assert_eq!(
        fetch(&app, &admin, &format!("{C}/transport-passes/{}", id(&p3))).await["status"],
        "cancelled"
    );
    let r = fetch(&app, &admin, &format!("{C}/transport-routes/{rid}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/retire"),
        json!({}),
        ver(&r),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "passengers still allocated");
    for a in [&a_diya, &a_kabir] {
        let cur = fetch(
            &app,
            &admin,
            &format!("{C}/transport-allocations/{}", id(a)),
        )
        .await;
        ok(
            &app,
            &admin,
            "POST",
            &format!("{C}/transport-allocations/{}/withdraw", id(a)),
            json!({}),
            ver(&cur),
            StatusCode::OK,
        )
        .await;
    }
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{C}/transport-routes/{rid}/retire"),
        json!({"reason": "Merged"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "retired");
}

// ====================================================================== AwaitDecision

fn custom_app(st: sutra_campus::CampusState, tenant: &str, subject: &str) -> Router {
    let (router, _) = sutra_campus::router_with_state(st).split_for_parts();
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
    let (app, pool) = app(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_A, "Approvals Campus").await;
    let inst = setup.institution().as_uuid().to_string();
    let h = b8::seed_hierarchy(&pool, &setup, "AWD", &[("201", SpaceType::HostelRoom, 2)]).await;
    let room = h.spaces[0].id.as_uuid().to_string();
    let s1 = b8::seed_student(&pool, &setup, "Neha", None).await;
    let mut st = sutra_api::app::campus_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let hq = custom_app(st, TENANT_A, "dev|admin@sutra.dev");
    let me = Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    };

    // ---- HOSTEL_POLICY_APPROVAL: submit waits; reject → resubmit → approve → activate.
    let p = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-policies"),
        json!({"name": "Policy", "feePerTerm": "30000.00", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-policies/{}/submit", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "draft");
    assert!(p["processInstanceId"].is_string(), "awaiting a decision");
    let (s, _) = call(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-policies/{}/submit", id(&p)),
        json!({}),
        ver(&p),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already submitted");
    let (s, _) = call(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-policies/{}/decision", id(&p)),
        json!({"decision": "reject"}),
        ver(&p),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejection needs a reason"
    );
    let p = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-policies/{}/decision", id(&p)),
        json!({"decision": "reject", "reason": "Fee too high"}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (p["status"].as_str(), p["statusReason"].as_str()),
        (Some("draft"), Some("Fee too high"))
    );
    assert!(p["processInstanceId"].is_null());
    let p = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-policies/{}/submit", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    let p = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-policies/{}/decision", id(&p)),
        json!({"decision": "approve"}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "approved");
    let p = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-policies/{}/activate", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "active");
    let bed_a = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-rooms/{room}/beds"),
        json!({"bedCode": "A"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let bed_b = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-rooms/{room}/beds"),
        json!({"bedCode": "B"}),
        None,
        StatusCode::CREATED,
    )
    .await;

    // ---- HOSTEL_ADMISSION (the Batch 8 reference approval): under review until decided.
    let body = json!({"personKind": "student", "personId": s1.student_id});
    let a = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        body.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a["status"], "under_review");
    assert!(a["processInstanceId"].is_string());
    assert_eq!(
        events(&pool, &id(&a), &ev("hostel_application.decided")).await,
        0
    );
    let (s, _) = call(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&a)),
        json!({}),
        ver(&a),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not decided yet");
    let a = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-applications/{}/decision", id(&a)),
        json!({"decision": "reject", "reason": "Incomplete documents"}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "rejected");
    assert_eq!(
        events(&pool, &id(&a), &ev("hostel_application.decided")).await,
        1
    );
    let a = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/institutions/{inst}/hostel-applications"),
        body,
        None,
        StatusCode::CREATED,
    )
    .await;
    let a = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-applications/{}/decision", id(&a)),
        json!({"decision": "approve"}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "approved");
    let a = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-applications/{}/allocate", id(&a)),
        json!({"bedId": id(&bed_a)}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "allocated");
    let alloc = a["allocationId"].as_str().unwrap().to_owned();
    let res = ok(
        &hq,
        &me,
        "GET",
        &format!("{C}/hostel-allocations/{alloc}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(res["feeAmount"], "30000.00");
    assert!(res["chargeId"].is_string());

    // ---- RESIDENT_LEAVE_APPROVAL: requested until decided.
    let lv = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-allocations/{alloc}/leave"),
        json!({"fromDate": "2099-02-01", "toDate": "2099-02-03"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(lv["status"], "requested");
    let (s, _) = call(
        &hq,
        &me,
        "POST",
        &format!("{C}/resident-leaves/{}/depart", id(&lv)),
        json!({}),
        ver(&lv),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let lv = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/resident-leaves/{}/decision", id(&lv)),
        json!({"decision": "approve"}),
        ver(&lv),
        StatusCode::OK,
    )
    .await;
    assert_eq!(lv["status"], "approved");

    // ---- ROOM_TRANSFER_APPROVAL: pending transfer; rejection → residency active again.
    let tr = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-allocations/{alloc}/transfer"),
        json!({"toBedId": id(&bed_b)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(tr["status"], "requested");
    let res = ok(
        &hq,
        &me,
        "GET",
        &format!("{C}/hostel-allocations/{alloc}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(res["status"], "transfer_pending");
    let tr = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/room-transfers/{}/decision", id(&tr)),
        json!({"decision": "reject", "reason": "No reason given"}),
        ver(&tr),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tr["status"], "rejected");
    let res = ok(
        &hq,
        &me,
        "GET",
        &format!("{C}/hostel-allocations/{alloc}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(res["status"], "active");
    let tr = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/hostel-allocations/{alloc}/transfer"),
        json!({"toBedId": id(&bed_b)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let tr = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/room-transfers/{}/decision", id(&tr)),
        json!({"decision": "approve"}),
        ver(&tr),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tr["status"], "completed");

    // ---- ROUTE_ACTIVATION: pending until decided.
    let mut stops = Vec::new();
    for code in ["A1", "A2"] {
        stops.push(id(&ok(
            &hq,
            &me,
            "POST",
            &format!("{C}/institutions/{inst}/transport-stops"),
            json!({"code": code, "name": code}),
            None,
            StatusCode::CREATED,
        )
        .await));
    }
    let r = ok(&hq, &me, "POST", &format!("{C}/institutions/{inst}/transport-routes"),
        json!({"code": "AW-1", "name": "Approval route", "capacity": 10, "fare": "0", "effectiveFrom": "2025-01-01"}), None, StatusCode::CREATED).await;
    let v1 = ok(
        &hq,
        &me,
        "GET",
        &format!("{C}/transport-routes/{}/versions", id(&r)),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await["items"][0]
        .clone();
    ok(&hq, &me, "POST", &format!("{C}/route-versions/{}/stops", id(&v1)),
        json!({"stops": [{"stopId": stops[0], "sequence": 1}, {"stopId": stops[1], "sequence": 2}]}), ver(&v1), StatusCode::OK).await;
    let r = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/transport-routes/{}/activate", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "draft");
    assert!(r["processInstanceId"].is_string());
    let (s, _) = call(
        &hq,
        &me,
        "POST",
        &format!("{C}/transport-routes/{}/activate", id(&r)),
        json!({}),
        ver(&r),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already awaiting a decision");
    let r = ok(
        &hq,
        &me,
        "POST",
        &format!("{C}/transport-routes/{}/decision", id(&r)),
        json!({"decision": "approve"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (r["status"].as_str(), r["currentVersionNo"].as_i64()),
        (Some("active"), Some(1))
    );
    assert!(r["processInstanceId"].is_null());
}
