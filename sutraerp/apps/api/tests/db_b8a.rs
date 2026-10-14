//! Batch 8 slice 8A DB tests — campus infrastructure, room booking, service desk, work orders,
//! preventive maintenance and laboratories (L3-G-016…018, 020…024, 026, 041, 042, 046, 050, 053,
//! 057, 060, 278, 281…284, 286…293, 315).
//!
//! * `infrastructure_hierarchy_and_room_booking` — registration (campus validated, hierarchy,
//!   duplicate code, planned → active), attributes / classification / capacity / capabilities /
//!   availability profiles, free / busy calendar, reservations (confirmed, overlap / hours /
//!   blackout / capacity 409s, self-service, reschedule, cancel), suspension (flags) /
//!   restoration, decommissioning (blocked by children and future reservations) and archive.
//! * `service_desk_work_orders_and_preventive_maintenance` — self-service tickets, triage (SLA
//!   recalculated), corrective work orders (assignment history, start, complete, verify →
//!   ticket resolved), requester confirmation / reopening, duplicates merged, SLA check +
//!   escalation, hold / cancel, preventive plans (approval, generation idempotent per period,
//!   deferral, suspension).
//! * `laboratories_readiness_sessions_and_batches` — lab profile, readiness checklist,
//!   equipment status → restricted, sessions planned → ready → in progress → completed, batch
//!   allocation with capacity check.
//! * `await_decision_reservations_decommissioning_and_plans` — `ApprovalMode::AwaitDecision`:
//!   reservations wait for a decision, node decommissioning and preventive plans too.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use serde_json::{json, Value};
use std::sync::Arc;
use support::b8;
use support::*;
use sutra_campus::shared::approval::ApprovalMode;
use sutra_campus::CampusState;
use sutra_contracts::campus::events::facilities as fe;
use sutra_contracts::campus::status::SpaceType;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const TENANT_INFRA: &str = "0192f5a0-7c3e-7000-8000-0000000b8a00";
const TENANT_DESK: &str = "0192f5a0-7c3e-7000-8000-0000000b8a01";
const TENANT_LABS: &str = "0192f5a0-7c3e-7000-8000-0000000b8a02";
const TENANT_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b8a03";
const C: &str = "/api/v1/campus";

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn sid(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
}

/// Node id of a node detail.
fn nid(v: &Value) -> String {
    v["node"]["id"].as_str().unwrap().to_owned()
}

/// Business "today" (Asia/Kolkata, the seeded institution's timezone).
fn today() -> NaiveDate {
    (Utc::now() + Duration::minutes(330)).date_naive()
}

fn next_monday(from: NaiveDate) -> NaiveDate {
    let mut d = from;
    while d.weekday() != Weekday::Mon {
        d += Duration::days(1);
    }
    d
}

fn iso(d: NaiveDate) -> String {
    d.format("%Y-%m-%d").to_string()
}

async fn outbox(pool: &sqlx::PgPool, event_type: &str, subject: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(event_type)
    .bind(subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn post(
    app: &axum::Router,
    who: &Login,
    path: &str,
    body: Value,
    v: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, "POST", path, body, v)).await;
    (s, b)
}

async fn put(
    app: &axum::Router,
    who: &Login,
    path: &str,
    body: Value,
    v: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, "PUT", path, body, v)).await;
    (s, b)
}

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

// ==================================================================== infrastructure + booking

#[tokio::test]
async fn infrastructure_hierarchy_and_room_booking() {
    let (app, pool) = app(TENANT_INFRA).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_INFRA, "Booking University").await;
    let inst = setup.institution().as_uuid();
    let campus = setup.campus_id;
    b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    b8::seed_staff(&pool, &setup, "Meera", Some(b4::MEERA)).await;
    let nodes = format!("{C}/institutions/{inst}/infrastructure-nodes");

    // ---- registration (L3-G-278 / 281…284): campus validated, hierarchy rule, unique code.
    let (s, b) = post(
        &app,
        &admin,
        &nodes,
        json!({"campusId": Uuid::now_v7(), "nodeType": "site", "code": "NORTH", "name": "North"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, site) = post(&app, &admin, &nodes,
        json!({"campusId": campus, "nodeType": "site", "code": "NORTH", "name": "North site", "status": "active"}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{site}");
    assert_eq!(site["node"]["status"], "active");
    let site_id = nid(&site);
    assert_eq!(
        outbox(&pool, fe::INFRASTRUCTURE_NODE_ACTIVATED, &site_id).await,
        1
    );
    let (s, b) = post(&app, &admin, &nodes,
        json!({"parentId": site_id, "nodeType": "floor", "code": "NORTH-F1", "name": "Floor under a site"}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, building) = post(&app, &admin, &nodes,
        json!({"parentId": site_id, "nodeType": "building", "code": "NORTH-B1", "name": "Block 1", "status": "active"}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{building}");
    let building_id = nid(&building);
    let (s, floor) = post(&app, &admin, &nodes,
        json!({"parentId": building_id, "nodeType": "floor", "code": "NORTH-B1-F1", "name": "Ground floor", "floorNumber": 0, "status": "active"}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{floor}");
    let floor_id = nid(&floor);
    let (s, room) = post(&app, &admin, &nodes,
        json!({"parentId": floor_id, "nodeType": "space", "code": "NORTH-B1-101", "name": "Room 101",
               "spaceType": "classroom", "capacity": 40, "areaSqM": "64.50"}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{room}");
    assert_eq!(room["node"]["status"], "planned");
    assert_eq!(room["node"]["areaSqM"], "64.50");
    let room_id = nid(&room);
    let (s, b) = post(&app, &admin, &nodes,
        json!({"parentId": floor_id, "nodeType": "space", "code": "NORTH-B1-101", "name": "Duplicate", "spaceType": "office"}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let (s, _) = post(
        &app,
        &aarav,
        &nodes,
        json!({"campusId": campus, "nodeType": "site", "code": "SOUTH", "name": "South"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::FORBIDDEN,
        "students do not register infrastructure"
    );
    let (s, b) = read(&app, &admin, &format!("{nodes}?parentId={floor_id}")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let (s, b) = read(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{building_id}"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["children"].as_array().unwrap().len(), 1);
    let (s, _) = read(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{}", Uuid::now_v7()),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // ---- activation: If-Match required, stale version 412, planned → active, active again 409.
    let room_path = format!("{C}/infrastructure-nodes/{room_id}");
    let (s, _) = post(
        &app,
        &admin,
        &format!("{room_path}/activate"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{room_path}/activate"),
        json!({}),
        Some(ver(&room) + 5),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, room) = post(
        &app,
        &admin,
        &format!("{room_path}/activate"),
        json!({}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["node"]["status"], "active");
    let (s, b) = post(
        &app,
        &admin,
        &format!("{room_path}/activate"),
        json!({}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");

    // ---- attributes / classification (L3-G-286, 288) → infrastructure_node.modified.
    let (s, room) = put(&app, &admin, &room_path,
        json!({"name": "Seminar 101", "spaceType": "seminar_room", "attributes": {"accessible": true}}), Some(ver(&room))).await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["node"]["name"], "Seminar 101");
    assert_eq!(room["node"]["spaceType"], "seminar_room");
    assert_eq!(
        outbox(&pool, fe::INFRASTRUCTURE_NODE_MODIFIED, &room_id).await,
        1
    );
    let (s, b) = put(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{building_id}"),
        json!({"spaceType": "office"}),
        Some(ver(&building)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");

    // ---- capabilities (L3-G-287) — spaces only.
    let (s, room) = put(
        &app,
        &admin,
        &format!("{room_path}/capabilities"),
        json!({"capabilities": [{"code": "projector", "label": "Projector", "quantity": 1},
                                {"code": "accessible", "label": "Wheelchair access"}]}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["capabilities"].as_array().unwrap().len(), 2);
    let (s, b) = read(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{building_id}"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = put(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{building_id}/capabilities"),
        json!({"capabilities": [{"code": "lift", "label": "Lift"}]}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // ---- availability profile (L3-G-289): Mondays 08:00–18:00, one blackout Monday.
    let monday = next_monday(today() + Duration::days(7));
    let blackout = monday + Duration::days(7);
    let (s, room) = put(
        &app,
        &admin,
        &format!("{room_path}/availability"),
        json!({"weeklyHours": [{"weekday": 1, "opensAt": "08:00", "closesAt": "18:00"}],
               "blackoutDates": [iso(blackout)]}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["availability"]["configured"], true);
    let (s, b) = read(&app, &admin, &format!("{room_path}/availability")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["weeklyHours"][0]["opensAt"], "08:00");

    // ---- reservations (L3-G-291): office booking auto-confirmed.
    let reservations = format!("{C}/institutions/{inst}/reservations");
    let slot = |date: NaiveDate, from: &str, to: &str, n: i32| {
        json!({"nodeId": room_id, "date": iso(date), "startTime": from, "endTime": to,
               "purpose": "Guest lecture", "attendees": n})
    };
    let (s, office) = post(
        &app,
        &admin,
        &reservations,
        slot(monday, "09:00", "11:00", 30),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{office}");
    assert_eq!(office["status"], "confirmed");
    assert_eq!(office["number"], "RSV-000001");
    assert_eq!(
        outbox(&pool, fe::SPACE_RESERVATION_CONFIRMED, &sid(&office)).await,
        1
    );
    let me_res = format!("{C}/me/reservations");
    for (body, why) in [
        (slot(monday, "10:00", "12:00", 5), "overlap"),
        (slot(monday, "07:00", "08:30", 5), "before opening"),
        (
            slot(monday + Duration::days(1), "09:00", "10:00", 5),
            "closed on Tuesdays",
        ),
        (slot(blackout, "09:00", "10:00", 5), "blackout date"),
        (slot(monday, "12:00", "13:00", 50), "over capacity"),
    ] {
        let (s, b) = post(&app, &meera, &me_res, body, None).await;
        assert_eq!(s, StatusCode::CONFLICT, "{why}: {b}");
    }
    let (s, b) = post(
        &app,
        &meera,
        &me_res,
        slot(today() - Duration::days(1), "09:00", "10:00", 5),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "past date: {b}");
    let (s, mine) = post(
        &app,
        &meera,
        &me_res,
        slot(monday, "11:00", "12:00", 10),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{mine}");
    assert_eq!(mine["status"], "confirmed");
    assert_eq!(mine["requesterKind"], "staff");
    let (s, b) = read(&app, &meera, &me_res).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let mine_path = format!("{C}/reservations/{}", sid(&mine));
    let (s, _) = read(&app, &aarav, &mine_path).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "someone else's booking is invisible"
    );
    let (s, _) = post(
        &app,
        &aarav,
        &reservations,
        slot(monday, "13:00", "14:00", 2),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students book through /me");

    // ---- free / busy calendar (L3-G-315).
    let (s, cal) = read(
        &app,
        &admin,
        &format!(
            "{C}/spaces/{room_id}/availability?from={}&to={}",
            iso(monday),
            iso(monday)
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cal}");
    let day = &cal["days"][0];
    assert_eq!(day["busy"].as_array().unwrap().len(), 2, "{cal}");
    assert_eq!(
        day["free"],
        json!([{"startTime": "08:00", "endTime": "09:00"}, {"startTime": "12:00", "endTime": "18:00"}])
    );

    // ---- reschedule (L3-G-292) by the requester.
    let (s, mine) = post(
        &app,
        &meera,
        &format!("{mine_path}/reschedule"),
        json!({"date": iso(monday), "startTime": "13:00", "endTime": "14:00"}),
        Some(ver(&mine)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(mine["status"], "rescheduled");
    assert_eq!(mine["startTime"], "13:00");
    assert_eq!(
        outbox(&pool, fe::SPACE_RESERVATION_RESCHEDULED, &sid(&mine)).await,
        1
    );

    // ---- capacity (L3-G-286): not below a booking's attendees.
    let (s, b) = put(
        &app,
        &admin,
        &format!("{room_path}/capacity"),
        json!({"capacity": 20}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let (s, room) = put(
        &app,
        &admin,
        &format!("{room_path}/capacity"),
        json!({"capacity": 35, "reason": "new benches"}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["node"]["capacity"], 35);

    // ---- suspension flags bookings, takes no new ones; restoration clears the flags (L3-G-290).
    let (s, b) = post(
        &app,
        &admin,
        &format!("{room_path}/suspend"),
        json!({}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required: {b}");
    let (s, room) = post(
        &app,
        &admin,
        &format!("{room_path}/suspend"),
        json!({"reason": "Ceiling repair"}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["node"]["status"], "suspended");
    let (_, b) = read(&app, &admin, &mine_path).await;
    assert_eq!(b["flagged"], true, "{b}");
    let (s, _) = post(
        &app,
        &meera,
        &me_res,
        slot(monday, "15:00", "16:00", 5),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a suspended space takes no bookings"
    );
    let (s, room) = post(
        &app,
        &admin,
        &format!("{room_path}/restore"),
        json!({}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["node"]["status"], "active");
    let (_, mine) = read(&app, &admin, &mine_path).await;
    assert_eq!(mine["flagged"], false);

    // ---- cancel (L3-G-293) by the requester.
    let (s, mine) = post(
        &app,
        &meera,
        &format!("{mine_path}/cancel"),
        json!({"reason": "Moved online"}),
        Some(ver(&mine)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(mine["status"], "cancelled");
    assert_eq!(
        outbox(&pool, fe::SPACE_RESERVATION_CANCELLED, &sid(&mine)).await,
        1
    );
    let (s, _) = post(
        &app,
        &meera,
        &format!("{mine_path}/cancel"),
        json!({}),
        Some(ver(&mine)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "cancelled is final");

    // ---- decommissioning: blocked by future bookings and live children; then archive.
    let (s, b) = post(
        &app,
        &aarav,
        &format!("{room_path}/decommission"),
        json!({"reason": "x"}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    let (s, b) = post(
        &app,
        &admin,
        &format!("{room_path}/decommission"),
        json!({"reason": "Demolition"}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "future booking blocks: {b}");
    let office_path = format!("{C}/reservations/{}", sid(&office));
    let (s, _) = post(
        &app,
        &admin,
        &format!("{office_path}/cancel"),
        json!({"reason": "Demolition"}),
        Some(ver(&office)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, building) = read(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{building_id}"),
    )
    .await;
    let (s, b) = post(
        &app,
        &admin,
        &format!("{C}/infrastructure-nodes/{building_id}/decommission"),
        json!({"reason": "Demolition"}),
        Some(ver(&building)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "live children block: {b}");
    let (s, room) = post(
        &app,
        &admin,
        &format!("{room_path}/decommission"),
        json!({"reason": "Demolition"}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["node"]["status"], "decommissioned");
    assert_eq!(room["decommission"]["status"], "approved");
    assert_eq!(
        outbox(&pool, fe::INFRASTRUCTURE_NODE_DECOMMISSIONED, &room_id).await,
        1
    );
    let (s, room) = post(
        &app,
        &admin,
        &format!("{room_path}/archive"),
        json!({}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{room}");
    assert_eq!(room["node"]["status"], "archived");
    let (s, _) = put(
        &app,
        &admin,
        &room_path,
        json!({"name": "Ghost"}),
        Some(ver(&room)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "archived nodes no longer change");
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM campus.status_history WHERE aggregate_type = 'infrastructure_node' AND aggregate_id = $1",
    )
    .bind(Uuid::parse_str(&room_id).unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        history >= 6,
        "planned, active, suspended, active, decommissioned, archived: {history}"
    );
}

// ==================================================================== service desk + maintenance

#[tokio::test]
async fn service_desk_work_orders_and_preventive_maintenance() {
    let (app, pool) = app(TENANT_DESK).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_DESK, "Desk University").await;
    let inst = setup.institution().as_uuid();
    let h = b8::seed_hierarchy(&pool, &setup, "SD", &[("101", SpaceType::Classroom, 60)]).await;
    let room = h.spaces[0].id.as_uuid();
    b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;

    // ---- self-service complaint (L3-G-016 / 017).
    let (s, t) = post(&app, &aarav, &format!("{C}/me/tickets"),
        json!({"nodeId": room, "category": "facility", "title": "Projector broken", "isComplaint": true}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    assert_eq!(
        (
            t["status"].as_str(),
            t["source"].as_str(),
            t["requesterKind"].as_str()
        ),
        (Some("open"), Some("self_service"), Some("student"))
    );
    let tid = sid(&t);
    let tp = format!("{C}/tickets/{tid}");
    let (s, b) = read(&app, &aarav, &format!("{C}/me/tickets")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let (s, _) = read(&app, &aarav, &format!("{C}/institutions/{inst}/tickets")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "the queue is the office's");
    let (s, b) = read(&app, &aarav, &tp).await;
    assert_eq!(s, StatusCode::OK, "own ticket readable: {b}");

    // ---- triage: critical → SLA 4 h from creation.
    let (s, t) = post(
        &app,
        &admin,
        &format!("{tp}/triage"),
        json!({"priority": "critical"}),
        Some(ver(&t)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(
        (t["status"].as_str(), t["priority"].as_str()),
        (Some("triaged"), Some("critical"))
    );
    let created = chrono::DateTime::parse_from_rfc3339(t["createdAt"].as_str().unwrap()).unwrap();
    let due = chrono::DateTime::parse_from_rfc3339(t["slaDueAt"].as_str().unwrap()).unwrap();
    assert_eq!((due - created).num_minutes(), 240);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{tp}/triage"),
        json!({}),
        Some(ver(&t)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "triaged once");

    // ---- corrective work order (L3-G-020 / 021 / 022): ticket follows the assignment.
    let (s, wo) = post(
        &app,
        &admin,
        &format!("{tp}/work-orders"),
        json!({"assignee": "tech.ravi"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{wo}");
    let w = &wo["workOrder"];
    assert_eq!(
        (
            w["status"].as_str(),
            w["kind"].as_str(),
            w["number"].as_str()
        ),
        (Some("assigned"), Some("corrective"), Some("WO-000001"))
    );
    let wid = w["id"].as_str().unwrap().to_owned();
    let wp = format!("{C}/work-orders/{wid}");
    let (_, t) = read(&app, &admin, &tp).await;
    assert_eq!(
        (
            t["ticket"]["status"].as_str(),
            t["ticket"]["assignee"].as_str()
        ),
        (Some("assigned"), Some("tech.ravi"))
    );
    let (s, wo) = post(
        &app,
        &admin,
        &format!("{wp}/assign"),
        json!({"assignee": "tech.sana", "reason": "Ravi on leave"}),
        Some(ver(&wo)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{wo}");
    assert_eq!(wo["assignments"].as_array().unwrap().len(), 2);
    assert_eq!(wo["assignments"][1]["previousAssignee"], "tech.ravi");
    let (s, b) = post(
        &app,
        &admin,
        &format!("{wp}/verify"),
        json!({"accepted": true}),
        Some(ver(&wo)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing to verify yet: {b}");
    let (s, wo) = post(
        &app,
        &admin,
        &format!("{wp}/start"),
        json!({}),
        Some(ver(&wo)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{wo}");
    let (_, t) = read(&app, &admin, &tp).await;
    assert_eq!(t["ticket"]["status"], "in_progress");
    let (s, wo) = post(
        &app,
        &admin,
        &format!("{wp}/complete"),
        json!({"notes": "Lamp replaced"}),
        Some(ver(&wo)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{wo}");
    let (s, wo) = post(
        &app,
        &admin,
        &format!("{wp}/verify"),
        json!({"accepted": true, "notes": "Projector works"}),
        Some(ver(&wo)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{wo}");
    assert_eq!(wo["workOrder"]["status"], "verified");
    assert_eq!(outbox(&pool, fe::WORK_ORDER_COMPLETED, &wid).await, 1);
    let (_, t) = read(&app, &admin, &tp).await;
    assert_eq!(
        (
            t["ticket"]["status"].as_str(),
            t["ticket"]["resolution"].as_str()
        ),
        (Some("resolved"), Some("Projector works"))
    );
    assert_eq!(outbox(&pool, fe::SERVICE_TICKET_RESOLVED, &tid).await, 1);

    // ---- requester verification (L3-G-017): not satisfied → reopened; then closed.
    let confirm = format!("{C}/me/tickets/{tid}/confirm");
    let tv = t["ticket"].clone();
    let (s, _) = post(
        &app,
        &aarav,
        &confirm,
        json!({"satisfied": false}),
        Some(ver(&tv)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "a comment is required");
    let (s, tv) = post(
        &app,
        &aarav,
        &confirm,
        json!({"satisfied": false, "comment": "Still flickers"}),
        Some(ver(&tv)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{tv}");
    assert_eq!(tv["status"], "reopened");
    let (s, tv) = post(
        &app,
        &admin,
        &format!("{tp}/assign"),
        json!({"assignee": "tech.sana"}),
        Some(ver(&tv)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{tv}");
    let (s, tv) = post(
        &app,
        &admin,
        &format!("{tp}/resolve"),
        json!({"resolution": "Ballast replaced"}),
        Some(ver(&tv)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{tv}");
    let (s, tv) = post(
        &app,
        &aarav,
        &confirm,
        json!({"satisfied": true}),
        Some(ver(&tv)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{tv}");
    assert_eq!(tv["status"], "closed");
    assert_eq!(outbox(&pool, fe::SERVICE_TICKET_CLOSED, &tid).await, 1);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{tp}/reopen"),
        json!({"reason": "again"}),
        Some(ver(&tv)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "closed is final");

    // ---- duplicates (L3-G-018) merged onto a canonical ticket.
    let office = format!("{C}/institutions/{inst}/tickets");
    let (s, a) = post(
        &app,
        &admin,
        &office,
        json!({"category": "maintenance", "title": "Leak in corridor", "nodeId": room}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert_eq!(a["source"], "admin");
    let (_, d) = post(
        &app,
        &admin,
        &office,
        json!({"category": "maintenance", "title": "Water on floor"}),
        None,
    )
    .await;
    let (s, _) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/merge", sid(&d)),
        json!({"canonicalTicketId": sid(&d)}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "not onto itself");
    let (s, _) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/merge", sid(&d)),
        json!({"canonicalTicketId": tid}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not onto a closed ticket");
    let (s, d) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/merge", sid(&d)),
        json!({"canonicalTicketId": sid(&a)}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(
        (d["status"].as_str(), d["canonicalTicketId"].as_str()),
        (Some("merged"), a["id"].as_str())
    );
    let (_, ad) = read(&app, &admin, &format!("{C}/tickets/{}", sid(&a))).await;
    assert_eq!(ad["duplicates"], json!([d["number"].clone()]));

    // ---- SLA check + escalation (L3-G-041 / 042).
    sqlx::query(
        "UPDATE campus.service_ticket SET sla_due_at = now() - interval '1 hour' WHERE id = $1",
    )
    .bind(Uuid::parse_str(&sid(&a)).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let check = format!("{office}/sla-check");
    let (s, sum) = post(&app, &admin, &check, json!({}), None).await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    assert_eq!(sum["processed"], 1, "{sum}");
    let (_, ad) = read(&app, &admin, &format!("{C}/tickets/{}", sid(&a))).await;
    assert_eq!(
        (
            ad["ticket"]["slaBreached"].as_bool(),
            ad["ticket"]["escalationLevel"].as_i64()
        ),
        (Some(true), Some(1))
    );
    assert_eq!(ad["escalations"][0]["kind"], "sla_breach");
    assert_eq!(
        outbox(&pool, fe::SERVICE_TICKET_ESCALATED, &sid(&a)).await,
        1
    );
    let (s, sum) = post(&app, &admin, &check, json!({}), None).await;
    assert_eq!(
        (s, sum["processed"].as_i64()),
        (StatusCode::OK, Some(0)),
        "already flagged"
    );
    let (s, b) = read(&app, &admin, &format!("{office}?breached=true")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["items"].as_array().unwrap().len(), 1);
    let (s, at) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/escalate", sid(&a)),
        json!({"reason": "Dean asked"}),
        Some(ver(&ad)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{at}");
    assert_eq!(at["escalationLevel"], 2);

    // ---- hold needs a reason; administrative cancellation (L3-G-046).
    let (_, at) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/assign", sid(&a)),
        json!({"assignee": "plumber"}),
        Some(ver(&at)),
    )
    .await;
    let (s, _) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/hold", sid(&a)),
        json!({}),
        Some(ver(&at)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, at) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/hold", sid(&a)),
        json!({"reason": "Waiting for parts"}),
        Some(ver(&at)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{at}");
    assert_eq!(at["status"], "on_hold");
    let (s, b) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/cancel", sid(&a)),
        json!({"reason": "x"}),
        Some(ver(&at)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "on hold cannot be cancelled directly: {b}"
    );
    let (s, at) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/assign", sid(&a)),
        json!({"assignee": "plumber"}),
        Some(ver(&at)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{at}");
    let (s, at) = post(
        &app,
        &admin,
        &format!("{C}/tickets/{}/cancel", sid(&a)),
        json!({"reason": "Duplicate of a work request"}),
        Some(ver(&at)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{at}");
    assert_eq!(at["status"], "cancelled");
    let (s, _) = read(&app, &admin, &format!("{C}/tickets/{}", Uuid::now_v7())).await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // ---- preventive maintenance (L3-G-023 / 024).
    let plans = format!("{C}/institutions/{inst}/maintenance-plans");
    let start = today() - Duration::days(40);
    let (s, plan) = post(
        &app,
        &admin,
        &plans,
        json!({"nodeId": room, "title": "Fire extinguisher check", "frequency": "monthly",
               "startDate": iso(start), "defaultAssignee": "tech.safety"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{plan}");
    assert_eq!(
        (
            plan["plan"]["status"].as_str(),
            plan["plan"]["approvalState"].as_str()
        ),
        (Some("draft"), Some("not_requested"))
    );
    let pid = plan["plan"]["id"].as_str().unwrap().to_owned();
    let pp = format!("{C}/maintenance-plans/{pid}");
    let through = iso(today() + Duration::days(40));
    let (s, _) = post(
        &app,
        &admin,
        &format!("{pp}/generate"),
        json!({"through": through}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a draft generates nothing");
    let (s, plan) = post(
        &app,
        &admin,
        &format!("{pp}/submit"),
        json!({}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(
        (
            plan["plan"]["status"].as_str(),
            plan["plan"]["approvalState"].as_str()
        ),
        (Some("active"), Some("approved"))
    );
    assert_eq!(outbox(&pool, fe::MAINTENANCE_PLAN_ACTIVATED, &pid).await, 1);
    let (s, sum) = post(
        &app,
        &admin,
        &format!("{pp}/generate"),
        json!({"through": through}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sum}");
    let generated = sum["processed"].as_i64().unwrap();
    assert!(generated >= 2, "{sum}");
    let (s, again) = post(
        &app,
        &admin,
        &format!("{pp}/generate"),
        json!({"through": through}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (again["processed"].as_i64(), again["skipped"].as_i64()),
        (Some(0), Some(generated)),
        "idempotent per period"
    );
    let (_, plan) = read(&app, &admin, &pp).await;
    let cycles = plan["cycles"].as_array().unwrap().clone();
    assert_eq!(cycles.len() as i64, generated);
    assert_eq!(cycles[0]["status"], "due");
    assert_eq!(cycles[0]["workOrderStatus"], "assigned");
    let (s, b) = read(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/work-orders?kind=preventive"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["items"].as_array().unwrap().len() as i64, generated);
    let c0 = &cycles[0];
    let cp = format!("{C}/maintenance-cycles/{}", c0["id"].as_str().unwrap());
    let (s, _) = post(
        &app,
        &admin,
        &format!("{cp}/complete"),
        json!({}),
        Some(ver(c0)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "its work order is not done");
    let (s, _) = post(
        &app,
        &admin,
        &format!("{cp}/defer"),
        json!({"dueOn": iso(today() + Duration::days(5))}),
        Some(ver(c0)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, plan) = post(
        &app,
        &admin,
        &format!("{cp}/defer"),
        json!({"dueOn": iso(today() + Duration::days(5)), "reason": "Exam week"}),
        Some(ver(c0)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["cycles"][0]["status"], "deferred");
    assert_eq!(plan["cycles"][0]["dueOn"], iso(today() + Duration::days(5)));
    let (s, plan) = post(
        &app,
        &admin,
        &format!("{pp}/suspend"),
        json!({"reason": "Contract renewal"}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    let (s, _) = post(
        &app,
        &admin,
        &format!("{pp}/generate"),
        json!({"through": through}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a suspended plan generates nothing"
    );
    let (s, plan) = post(
        &app,
        &admin,
        &format!("{pp}/activate"),
        json!({}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(plan["plan"]["status"], "active");
}

// ==================================================================== laboratories

#[tokio::test]
async fn laboratories_readiness_sessions_and_batches() {
    let (app, pool) = app(TENANT_LABS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_LABS, "Lab University").await;
    let inst = setup.institution().as_uuid();
    let h = b8::seed_hierarchy(
        &pool,
        &setup,
        "LB",
        &[("L1", SpaceType::Lab, 30), ("C1", SpaceType::Classroom, 30)],
    )
    .await;
    let labs = format!("{C}/institutions/{inst}/labs");

    // ---- lab profile (L3-G-050) of a lab space only.
    let (s, _) = post(&app, &admin, &labs, json!({"nodeId": h.spaces[1].id}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "a classroom is no lab");
    let (s, lab) = post(
        &app,
        &admin,
        &labs,
        json!({"nodeId": h.spaces[0].id, "name": "Chemistry lab"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{lab}");
    assert_eq!(
        (
            lab["lab"]["status"].as_str(),
            lab["lab"]["capacity"].as_i64()
        ),
        (Some("not_ready"), Some(30))
    );
    let (s, _) = post(&app, &admin, &labs, json!({"nodeId": h.spaces[0].id}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "one profile per space");
    let lid = lab["lab"]["id"].as_str().unwrap().to_owned();
    let lp = format!("{C}/labs/{lid}");
    let (s, _) = read(&app, &aarav, &labs).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ---- a session needs a ready lab (L3-G-053).
    let day = iso(today() + Duration::days(3));
    let (s, sess) = post(&app, &admin, &format!("{lp}/sessions"),
        json!({"title": "Titration", "courseCode": "CHE101", "sessionDate": day, "startTime": "14:00", "endTime": "16:00", "capacity": 2}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{sess}");
    assert_eq!(sess["status"], "planned");
    let sp = format!("{C}/practical-sessions/{}", sid(&sess));
    let (s, _) = post(
        &app,
        &admin,
        &format!("{sp}/ready"),
        json!({}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the lab is not ready");
    let (s, _) = post(
        &app,
        &admin,
        &format!("{lp}/sessions"),
        json!({"title": "Overlap", "sessionDate": day, "startTime": "15:00", "endTime": "17:00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the lab is busy");

    // ---- readiness checklist (L3-G-050).
    let (s, lab) = post(&app, &admin, &format!("{lp}/readiness"),
        json!({"checklist": [{"item": "Gas valves closed", "done": true}, {"item": "First-aid kit", "done": false}]}), Some(ver(&lab))).await;
    assert_eq!(s, StatusCode::OK, "{lab}");
    assert_eq!(lab["lab"]["status"], "not_ready");
    let (s, lab) = post(&app, &admin, &format!("{lp}/readiness"),
        json!({"checklist": [{"item": "Gas valves closed", "done": true}, {"item": "First-aid kit", "done": true}]}), Some(ver(&lab))).await;
    assert_eq!(s, StatusCode::OK, "{lab}");
    assert_eq!(lab["lab"]["status"], "ready");

    // ---- equipment status → lab restricted (L3-G-060).
    let (s, eq) = post(
        &app,
        &admin,
        &format!("{lp}/equipment"),
        json!({"code": "fume-01", "name": "Fume hood"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{eq}");
    assert_eq!(
        (eq["code"].as_str(), eq["status"].as_str()),
        (Some("FUME-01"), Some("operational"))
    );
    let ep = format!("{C}/lab-equipment/{}/status", sid(&eq));
    let (s, _) = post(
        &app,
        &admin,
        &ep,
        json!({"status": "out_of_service"}),
        Some(ver(&eq)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, eq) = post(
        &app,
        &admin,
        &ep,
        json!({"status": "out_of_service", "reason": "Fan broken"}),
        Some(ver(&eq)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{eq}");
    let (_, lab) = read(&app, &admin, &lp).await;
    assert_eq!(lab["lab"]["status"], "restricted");
    let (s, _) = post(
        &app,
        &admin,
        &format!("{sp}/ready"),
        json!({}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a restricted lab");
    let (s, eq) = post(
        &app,
        &admin,
        &ep,
        json!({"status": "operational"}),
        Some(ver(&eq)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{eq}");
    let (_, lab) = read(&app, &admin, &lp).await;
    assert_eq!(lab["lab"]["status"], "ready");
    let (s, sess) = post(
        &app,
        &admin,
        &format!("{sp}/ready"),
        json!({}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sess}");
    assert_eq!(sess["status"], "ready");

    // ---- batch allocation with capacity check (L3-G-057).
    let s1 = b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let s2 = b8::seed_student(&pool, &setup, "Kiran", None).await;
    let s3 = b8::seed_student(&pool, &setup, "Divya", None).await;
    let alloc = format!("{lp}/batch-allocations");
    let (s, b) = post(
        &app,
        &admin,
        &alloc,
        json!({"sessionId": sid(&sess), "studentIds": [Uuid::now_v7()]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown student: {b}");
    let (s, b) = post(&app, &admin, &alloc,
        json!({"sessionId": sid(&sess), "batchLabel": "B1", "studentIds": [s1.student_id, s2.student_id]}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    assert_eq!(b["items"].as_array().unwrap().len(), 2);
    let (s, b) = post(
        &app,
        &admin,
        &alloc,
        json!({"sessionId": sid(&sess), "studentIds": [s3.student_id]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "capacity 2: {b}");
    let (s, b) = post(
        &app,
        &admin,
        &alloc,
        json!({"sessionId": sid(&sess), "studentIds": [s1.student_id]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CREATED,
        "already allocated students are skipped: {b}"
    );
    assert_eq!(b["items"].as_array().unwrap().len(), 2);
    let (s, b) = read(&app, &admin, &format!("{alloc}?sessionId={}", sid(&sess))).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["items"].as_array().unwrap().len(), 2);

    // ---- run the session.
    let (_, sess) = read(&app, &admin, &format!("{lp}/sessions")).await;
    let sess = sess["items"][0].clone();
    assert_eq!(sess["allocated"], 2);
    let (s, sess) = post(
        &app,
        &admin,
        &format!("{sp}/start"),
        json!({}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sess}");
    let (s, sess) = post(
        &app,
        &admin,
        &format!("{sp}/complete"),
        json!({}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{sess}");
    assert_eq!(sess["status"], "completed");
    assert_eq!(
        outbox(&pool, fe::PRACTICAL_SESSION_COMPLETED, &sid(&sess)).await,
        1
    );
    let (s, _) = post(
        &app,
        &admin,
        &format!("{sp}/postpone"),
        json!({"reason": "late"}),
        Some(ver(&sess)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "completed is final");
}

// ==================================================================== AwaitDecision

const ADMIN: &str = "dev|admin@sutra.dev";

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
    }
}

fn as_user(st: &CampusState, who: AuthContext) -> axum::Router {
    let (router, _) = sutra_campus::router_with_state(st.clone()).split_for_parts();
    router.layer(axum::Extension(who))
}

async fn hit(
    r: &axum::Router,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::now_v7().to_string());
    if let Some(v) = if_match {
        b = b.header("if-match", format!("\"{v}\""));
    }
    let (s, _, v) = send(r, b.body(Body::from(body.to_string())).unwrap()).await;
    (s, v)
}

#[tokio::test]
async fn await_decision_reservations_decommissioning_and_plans() {
    let (app, pool) = app(TENANT_AWAIT).await;
    let admin_login = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(
        &app,
        &admin_login,
        &pool,
        TENANT_AWAIT,
        "Approval University",
    )
    .await;
    let t = setup.tenant();
    let inst = setup.institution().as_uuid();
    b8::seed_staff(&pool, &setup, "Meera", Some(b4::MEERA)).await;
    let h = b8::seed_hierarchy(
        &pool,
        &setup,
        "AW",
        &[
            ("101", SpaceType::Classroom, 40),
            ("102", SpaceType::Classroom, 40),
        ],
    )
    .await;
    let mut st = sutra_api::app::campus_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let admin = as_user(&st, auth(ADMIN, Role::Administrator, t));
    let meera = as_user(&st, auth(b4::MEERA, Role::Faculty, t));
    let day = iso(today() + Duration::days(10));

    // ---- reservations wait for a decision (RESERVATION_APPROVAL).
    let book = |from: &str, to: &str| json!({"nodeId": h.spaces[0].id, "date": day, "startTime": from, "endTime": to, "purpose": "Thesis defence", "attendees": 12});
    let (s, r) = hit(
        &meera,
        &format!("{C}/me/reservations"),
        book("09:00", "10:00"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["status"], "requested");
    assert!(r["processInstanceId"].is_string());
    let (s, b) = hit(
        &meera,
        &format!("{C}/reservations/{}/decision", sid(&r)),
        json!({"decision": "approve"}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "requesters do not decide: {b}");
    let (s, r) = hit(
        &admin,
        &format!("{C}/reservations/{}/decision", sid(&r)),
        json!({"decision": "approve"}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["status"], "confirmed");
    let (s, r2) = hit(
        &meera,
        &format!("{C}/me/reservations"),
        book("11:00", "12:00"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r2}");
    let (s, _) = hit(
        &admin,
        &format!("{C}/reservations/{}/decision", sid(&r2)),
        json!({"decision": "reject"}),
        Some(ver(&r2)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejection needs a reason"
    );
    let (s, r2) = hit(
        &admin,
        &format!("{C}/reservations/{}/decision", sid(&r2)),
        json!({"decision": "reject", "reason": "Room needed for exams"}),
        Some(ver(&r2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r2}");
    assert_eq!(r2["status"], "rejected");

    // ---- node decommissioning waits for a decision (NODE_DECOMMISSION).
    let np = format!("{C}/infrastructure-nodes/{}", h.spaces[1].id);
    let v = h.spaces[1].version;
    let (s, n) = hit(
        &admin,
        &format!("{np}/decommission"),
        json!({"reason": "Merged into 101"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{n}");
    assert_eq!(
        (
            n["node"]["status"].as_str(),
            n["decommission"]["status"].as_str()
        ),
        (Some("active"), Some("pending"))
    );
    let (s, _) = hit(
        &admin,
        &format!("{np}/decommission"),
        json!({"reason": "again"}),
        Some(ver(&n)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one pending request at a time");
    let (s, n) = hit(
        &admin,
        &format!("{np}/decommission-decision"),
        json!({"decision": "approve"}),
        Some(ver(&n)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{n}");
    assert_eq!(
        (
            n["node"]["status"].as_str(),
            n["decommission"]["status"].as_str()
        ),
        (Some("decommissioned"), Some("approved"))
    );

    // ---- preventive plans wait for a decision (MAINTENANCE_PLAN_APPROVAL).
    let (s, plan) = hit(&admin, &format!("{C}/institutions/{inst}/maintenance-plans"),
        json!({"nodeId": h.spaces[0].id, "title": "AC service", "frequency": "quarterly", "startDate": iso(today())}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{plan}");
    let pp = format!(
        "{C}/maintenance-plans/{}",
        plan["plan"]["id"].as_str().unwrap()
    );
    let (s, plan) = hit(&admin, &format!("{pp}/submit"), json!({}), Some(ver(&plan))).await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(
        (
            plan["plan"]["status"].as_str(),
            plan["plan"]["approvalState"].as_str()
        ),
        (Some("draft"), Some("pending"))
    );
    let (s, plan) = hit(
        &admin,
        &format!("{pp}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&plan)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{plan}");
    assert_eq!(
        (
            plan["plan"]["status"].as_str(),
            plan["plan"]["approvalState"].as_str()
        ),
        (Some("active"), Some("approved"))
    );
}
