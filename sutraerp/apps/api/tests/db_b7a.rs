//! Batch 7 slice 7A DB tests (People: staff records, engagements, positions, faculty,
//! recruitment → onboarding, HR cases, profile). Runs in CI only (`--features db-tests`).
//!
//! * `staff_records_engagements_positions_and_exit` — L3-E-058 (draft → pending_verification →
//!   verified → active), 059 (`PUT /staff/{id}`, If-Match 428 / 412), 001 (engagement →
//!   `ENGAGEMENT_ACTIVATION`), 002 (modification), 003 (transfer, Domain B validation), 066
//!   (profile synchronisation), 018 (reporting manager, cycles), 011 / 012 / 013 (position
//!   assignment, reassignment, completion), 004 (suspension → restoration), 043 (probation
//!   extended → confirmed), 041 (separation → clearance → completion), 068 (archive), 008
//!   (rejoining restores the profile), 009 (duplicate → canonical), 065 (governed correction with
//!   history), 007 (direct closure), refusals (409 / 422 / 404 / 403).
//! * `recruitment_onboarding_faculty_and_self_service` — L3-E-033 (requisition → candidates →
//!   offer → onboarding → joining creates the staff record + active engagement, requisition
//!   filled), 026 (faculty establishment), 029 (teaching eligibility), 031 (faculty suspension /
//!   reactivation and its effect on Domain D's `StaffDirectory`), 060 (qualification
//!   verification), 064 (document verification), 039 (certificate request → issue, self-service),
//!   `/me/profile`.
//! * `governed_decisions_await_a_decision` — `ApprovalMode::AwaitDecision`: engagement activation
//!   (reject → resubmit → approve), engagement change, requisition, position assignment,
//!   separation and certificate decisions.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b7;
use support::*;
use sutra_contracts::people::status::StaffCategory;
use sutra_contracts::people::StaffDirectory;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_people::shared::approval::ApprovalMode;
use uuid::Uuid;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000b7a00";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-0000000b7a01";
const TENANT_C: &str = "0192f5a0-7c3e-7000-8000-0000000b7a02";
const P: &str = "/api/v1/people";

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

/// Creates a staff record through the API.
async fn new_staff(
    app: &Router,
    admin: &Login,
    inst: &str,
    given: &str,
    family: &str,
    category: &str,
    initial: &str,
) -> Value {
    ok(
        app,
        admin,
        "POST",
        &format!("{P}/institutions/{inst}/staff"),
        json!({
            "givenName": given, "familyName": family,
            "workEmail": format!("{}.{}@b7a.dev", given.to_lowercase(), family.to_lowercase()),
            "staffCategory": category, "initialStatus": initial
        }),
        None,
        StatusCode::CREATED,
    )
    .await
}

/// Creates + submits an engagement (auto-approved → active).
async fn active_engagement(
    app: &Router,
    admin: &Login,
    staff_id: &str,
    employment: &str,
    start: &str,
    probation_end: Option<&str>,
) -> Value {
    let e = ok(
        app,
        admin,
        "POST",
        &format!("{P}/staff/{staff_id}/engagements"),
        json!({"employmentType": employment, "designation": "Assistant Professor", "grade": "AP-1",
               "startDate": start, "probationEndDate": probation_end}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(e["status"], "pending", "{e}");
    let e = ok(
        app,
        admin,
        "POST",
        &format!("{P}/engagements/{}/submit", id(&e)),
        json!({}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "active", "{e}");
    assert!(e["pendingChange"].is_null(), "{e}");
    e
}

async fn engagement(app: &Router, admin: &Login, eid: &str) -> Value {
    fetch(app, admin, &format!("{P}/engagements/{eid}")).await
}

async fn staff(app: &Router, admin: &Login, sid: &str) -> Value {
    fetch(app, admin, &format!("{P}/staff/{sid}")).await
}

async fn campus_of(app: &Router, admin: &Login, pool: &sqlx::PgPool, inst: &str) -> String {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.campus WHERE institution_id = $1 AND status = 'active' \
         ORDER BY created_at LIMIT 1",
    )
    .bind(Uuid::parse_str(inst).unwrap())
    .fetch_optional(pool)
    .await
    .unwrap();
    match existing {
        Some(c) => c.to_string(),
        None => id(&b2::active_campus(app, admin, inst, &code("SAT"), false).await),
    }
}

// ================================================================================= test 1

#[tokio::test]
async fn staff_records_engagements_positions_and_exit() {
    let (app, pool) = app(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_A, "Workforce University").await;
    let inst = setup.institution().as_uuid().to_string();

    // ---- L3-E-058: creation (draft) → pending_verification → verified → active.
    let (s, h, a) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{P}/institutions/{inst}/staff"),
            json!({"givenName": "Anika", "familyName": "Rao", "workEmail": "anika.rao@b7a.dev",
                   "phone": "+91 98200 11111", "gender": "female", "staffCategory": "teaching"}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    assert!(h.get("etag").is_some());
    assert_eq!(a["status"], "draft");
    assert_eq!(a["displayName"], "Anika Rao");
    assert!(a["staffNumber"].as_str().unwrap().starts_with("EMP-"));
    let anika = id(&a);
    assert_eq!(
        events(&pool, &anika, "sutraerp.people.staff_member.created").await,
        1
    );
    // Refusals: faculty may not create (403), invalid e-mail (422), duplicate e-mail (409).
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/institutions/{inst}/staff"),
        json!({"givenName": "X", "staffCategory": "teaching"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/staff"),
        json!({"givenName": "X", "workEmail": "no-at-sign", "staffCategory": "teaching"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, b) = call(&app, &admin, "POST", &format!("{P}/institutions/{inst}/staff"),
        json!({"givenName": "Anika", "workEmail": "ANIKA.RAO@b7a.dev", "staffCategory": "teaching"}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "possible duplicate: {b}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{}/staff", Uuid::now_v7()),
        json!({"givenName": "X", "staffCategory": "teaching"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);

    // ---- L3-E-059: core profile maintenance with If-Match.
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/staff/{anika}"),
        json!({"givenName": "Anika", "familyName": "Rao", "staffCategory": "teaching"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let a = ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/staff/{anika}"),
        json!({"givenName": "Anika", "familyName": "Rao", "workEmail": "anika.rao@b7a.dev",
               "phone": "+91 98200 22222", "gender": "female", "dateOfBirth": "1990-04-12",
               "staffCategory": "teaching"}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["phone"], "+91 98200 22222");
    assert_eq!(a["dateOfBirth"], "1990-04-12");
    assert_eq!(
        events(&pool, &anika, "sutraerp.people.staff_member.modified").await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/staff/{anika}"),
        json!({"givenName": "Anika", "staffCategory": "teaching"}),
        Some(1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale If-Match");

    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{anika}/activate"),
        json!({}),
        ver(&a),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "draft cannot activate directly: {b}"
    );
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{anika}/submit-verification"),
        json!({}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "pending_verification");
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{anika}/verify"),
        json!({}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "verified");
    assert!(a["verifiedAt"].is_string());
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{anika}/activate"),
        json!({}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "active");
    assert_eq!(
        events(&pool, &anika, "sutraerp.people.staff_member.verified").await,
        1
    );
    assert_eq!(
        events(&pool, &anika, "sutraerp.people.staff_member.activated").await,
        1
    );

    // ---- L3-E-001: engagement pending → ENGAGEMENT_ACTIVATION (auto) → active.
    let (s, b) = call(&app, &admin, "POST", &format!("{P}/staff/{anika}/engagements"),
        json!({"employmentType": "probation", "designation": "Assistant Professor", "startDate": "2023-01-01"}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "probation needs its end date: {b}"
    );
    let (s, b) = call(&app, &admin, "POST", &format!("{P}/staff/{anika}/engagements"),
        json!({"employmentType": "permanent", "designation": "Assistant Professor", "startDate": "2023-01-01",
               "orgUnitId": Uuid::now_v7()}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown org unit: {b}");
    let e = active_engagement(
        &app,
        &admin,
        &anika,
        "probation",
        "2023-01-01",
        Some("2023-12-31"),
    )
    .await;
    let anika_eng = id(&e);
    assert!(e["engagementNumber"].as_str().unwrap().starts_with("ENG-"));
    assert_eq!(
        events(
            &pool,
            &anika_eng,
            "sutraerp.people.staff_engagement.submitted"
        )
        .await,
        1
    );
    assert_eq!(
        events(
            &pool,
            &anika_eng,
            "sutraerp.people.staff_engagement.activated"
        )
        .await,
        1
    );
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{anika}/engagements"),
        json!({"employmentType": "contract", "designation": "Visiting", "startDate": "2023-02-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one live engagement: {b}");
    // The foundation staff view shows the engagement.
    let sv = staff(&app, &admin, &anika).await;
    assert_eq!(sv["engagement"]["status"], "active");
    // An unverified profile cannot start an engagement.
    let d = new_staff(&app, &admin, &inst, "Dev", "Draft", "teaching", "draft").await;
    let de = ok(&app, &admin, "POST", &format!("{P}/staff/{}/engagements", id(&d)),
        json!({"employmentType": "permanent", "designation": "Lecturer", "startDate": "2023-01-01"}), None, StatusCode::CREATED).await;
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/submit", id(&de)),
        json!({}),
        ver(&de),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");

    // ---- L3-E-018: reporting manager (no self, no cycle, manager needs a live engagement).
    let bh = new_staff(&app, &admin, &inst, "Bhavna", "Iyer", "teaching", "active").await;
    let bhavna = id(&bh);
    let bhe = active_engagement(&app, &admin, &bhavna, "permanent", "2022-06-01", None).await;
    let bhavna_eng = id(&bhe);
    let e = engagement(&app, &admin, &anika_eng).await;
    let (s, b) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/engagements/{anika_eng}/reporting"),
        json!({"reportsToStaffId": anika}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "self reporting: {b}");
    let lone = new_staff(&app, &admin, &inst, "Lone", "Wolf", "teaching", "active").await;
    let (s, b) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/engagements/{anika_eng}/reporting"),
        json!({"reportsToStaffId": id(&lone)}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "manager without engagement: {b}");
    let e = ok(
        &app,
        &admin,
        "PUT",
        &format!("{P}/engagements/{anika_eng}/reporting"),
        json!({"reportsToStaffId": bhavna, "reason": "head of department"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["reportsToStaffId"], bhavna.as_str());
    assert_eq!(
        events(
            &pool,
            &anika_eng,
            "sutraerp.people.staff_engagement.reporting_set"
        )
        .await,
        1
    );
    let (s, b) = call(
        &app,
        &admin,
        "PUT",
        &format!("{P}/engagements/{bhavna_eng}/reporting"),
        json!({"reportsToStaffId": anika}),
        ver(&bhe),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "reporting cycle: {b}");
    let mgr: Option<Uuid> =
        sqlx::query_scalar("SELECT reports_to_staff_id FROM people.staff_engagement WHERE id = $1")
            .bind(Uuid::parse_str(&anika_eng).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(mgr.map(|m| m.to_string()), Some(bhavna.clone()));

    // ---- L3-E-002 + L3-E-066: modification → synchronised into the profile.
    let e = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/modify"),
        json!({"designation": "Associate Professor", "grade": "AP-2", "reason": "promotion board"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["designation"], "Associate Professor");
    assert_eq!(e["grade"], "AP-2");
    assert!(e["pendingChange"].is_null());
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/modify"),
        json!({"designation": "Associate Professor", "reason": "noop"}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "no change: {b}");
    let sv = staff(&app, &admin, &anika).await;
    assert_eq!(sv["engagement"]["designation"], "Associate Professor");
    let syncs = fetch(&app, &admin, &format!("{P}/staff/{anika}/profile-syncs")).await;
    let first = &syncs["items"][0];
    assert_eq!(first["cause"], "modification");
    let fields: Vec<&str> = first["fields"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f.as_str().unwrap())
        .collect();
    assert!(
        fields.contains(&"designation") && fields.contains(&"grade"),
        "{fields:?}"
    );
    assert_eq!(first["beforeValues"]["designation"], "Assistant Professor");
    assert_eq!(
        events(&pool, &anika, "sutraerp.people.staff_member.synchronised").await,
        1
    );
    assert_eq!(
        events(
            &pool,
            &anika_eng,
            "sutraerp.people.staff_engagement.modified"
        )
        .await,
        1
    );

    // ---- L3-E-003: transfer (campus validated through Domain B).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/transfer"),
        json!({"orgUnitId": Uuid::now_v7(), "reason": "restructuring"}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let campus = campus_of(&app, &admin, &pool, &inst).await;
    let e = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/transfer"),
        json!({"campusId": campus, "effectiveDate": "2023-04-01", "reason": "campus move"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["campusId"], campus.as_str());
    assert_eq!(
        events(
            &pool,
            &anika_eng,
            "sutraerp.people.staff_engagement.transferred"
        )
        .await,
        1
    );
    let syncs = fetch(&app, &admin, &format!("{P}/staff/{anika}/profile-syncs")).await;
    assert_eq!(syncs["items"].as_array().unwrap().len(), 2);
    assert_eq!(syncs["items"][0]["cause"], "transfer");

    // ---- L3-E-011 / 012 / 013: positions, assignment, reassignment, completion.
    let p1 = ok(&app, &admin, "POST", &format!("{P}/institutions/{inst}/positions"),
        json!({"code": "prof-cs-01", "title": "Professor of Computer Science", "staffCategory": "teaching"}), None, StatusCode::CREATED).await;
    assert_eq!(p1["code"], "PROF-CS-01");
    assert_eq!(p1["status"], "draft");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/positions"),
        json!({"code": "PROF-CS-01", "title": "Again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate code");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/assignments", id(&p1)),
        json!({"engagementId": anika_eng, "startDate": "2023-02-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "draft position: {b}");
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/activate", id(&p1)),
        json!({}),
        ver(&p1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "active");
    let a1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/assignments", id(&p1)),
        json!({"engagementId": anika_eng, "startDate": "2023-02-01", "reason": "appointment"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a1["status"], "active", "{a1}");
    assert_eq!(a1["isPrimary"], true);
    assert_eq!(
        events(
            &pool,
            &id(&a1),
            "sutraerp.people.position_assignment.activated"
        )
        .await,
        1
    );
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/assignments", id(&p1)),
        json!({"engagementId": bhavna_eng, "startDate": "2023-02-01"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "occupied: {b}");
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/positions"),
        json!({"code": "HOD-CS", "title": "Head of Department"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/activate", id(&p2)),
        json!({}),
        ver(&p2),
        StatusCode::OK,
    )
    .await;
    let a2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/position-assignments/{}/change", id(&a1)),
        json!({"positionId": id(&p2), "startDate": "2023-06-01", "reason": "reassigned as head"}),
        ver(&a1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a2["status"], "active", "{a2}");
    assert_eq!(a2["supersedesId"], id(&a1).as_str());
    let old = fetch(
        &app,
        &admin,
        &format!("{P}/position-assignments/{}", id(&a1)),
    )
    .await;
    assert_eq!(old["status"], "superseded");
    assert_eq!(old["endOutcome"], "superseded");
    assert_eq!(old["endDate"], "2023-05-31");
    let hist = fetch(
        &app,
        &admin,
        &format!("{P}/positions/{}/assignments", id(&p1)),
    )
    .await;
    assert_eq!(hist["items"].as_array().unwrap().len(), 1, "history kept");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/retire", id(&p2)),
        json!({"reason": "restructure"}),
        ver(&p2),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "occupied position: {b}");
    let p1 = fetch(&app, &admin, &format!("{P}/positions/{}", id(&p1))).await;
    let p1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/freeze", id(&p1)),
        json!({"reason": "budget"}),
        ver(&p1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "frozen");
    let a2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/position-assignments/{}/end", id(&a2)),
        json!({"endDate": "2023-12-31", "outcome": "completed", "reason": "term over"}),
        ver(&a2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a2["status"], "closed");
    assert_eq!(a2["endOutcome"], "completed");
    let p2 = fetch(&app, &admin, &format!("{P}/positions/{}", id(&p2))).await;
    let p2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/positions/{}/retire", id(&p2)),
        json!({"reason": "restructure"}),
        ver(&p2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p2["status"], "retired");
    let list = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/positions?status=retired"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // ---- L3-E-004: suspension → restoration.
    let e = engagement(&app, &admin, &anika_eng).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/suspend"),
        json!({}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let e = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/suspend"),
        json!({"reason": "inquiry"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "suspended");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/suspend"),
        json!({"reason": "again"}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let e = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/restore"),
        json!({}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "active");

    // ---- L3-E-043: probation review extended, then confirmed (→ permanent).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/probation-reviews"),
        json!({"outcome": "extended"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let r = ok(&app, &admin, "POST", &format!("{P}/engagements/{anika_eng}/probation-reviews"),
        json!({"reviewDate": "2023-12-15", "outcome": "extended", "extendedUntil": "2024-03-31", "assessment": "needs mentoring"}), None, StatusCode::CREATED).await;
    assert_eq!(r["status"], "completed", "{r}");
    assert_eq!(r["previousProbationEnd"], "2023-12-31");
    let e = engagement(&app, &admin, &anika_eng).await;
    assert_eq!(e["probationEndDate"], "2024-03-31");
    assert_eq!(e["employmentType"], "probation");
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/probation-reviews"),
        json!({"reviewDate": "2024-03-20", "outcome": "confirmed"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["status"], "completed");
    let e = engagement(&app, &admin, &anika_eng).await;
    assert_eq!(e["employmentType"], "permanent");
    assert_eq!(
        events(&pool, &id(&r), "sutraerp.people.probation_review.decided").await,
        1
    );
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/probation-reviews"),
        json!({"outcome": "confirmed"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no longer on probation: {b}");
    let reviews = fetch(
        &app,
        &admin,
        &format!("{P}/engagements/{anika_eng}/probation-reviews"),
    )
    .await;
    assert_eq!(reviews["items"].as_array().unwrap().len(), 2);

    // ---- L3-E-041 + 068: separation → clearance → completion → archive.
    let sep = ok(&app, &admin, "POST", &format!("{P}/engagements/{anika_eng}/separation"),
        json!({"kind": "resignation", "noticeDate": "2024-05-31", "lastWorkingDate": "2024-06-30", "reason": "higher studies"}), None, StatusCode::CREATED).await;
    assert_eq!(sep["status"], "approved", "{sep}");
    assert!(sep["number"].as_str().unwrap().starts_with("SEP-"));
    assert_eq!(
        events(
            &pool,
            &id(&sep),
            "sutraerp.people.separation_case.submitted"
        )
        .await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/separation"),
        json!({"kind": "resignation", "lastWorkingDate": "2024-06-30", "reason": "again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open case");
    let e = engagement(&app, &admin, &anika_eng).await;
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{anika_eng}/close"),
        json!({"lastWorkingDate": "2024-06-30", "reason": "exit"}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "close through the case: {b}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/separation-cases/{}/complete", id(&sep)),
        json!({}),
        ver(&sep),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "clearance first");
    let sep = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/separation-cases/{}/clearance", id(&sep)),
        json!({"items": [{"department": "Library", "cleared": false, "remarks": "2 books"}]}),
        ver(&sep),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sep["status"], "processing");
    assert_eq!(sep["clearanceComplete"], false);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/separation-cases/{}/complete", id(&sep)),
        json!({}),
        ver(&sep),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "clearance incomplete");
    let sep = ok(&app, &admin, "POST", &format!("{P}/separation-cases/{}/clearance", id(&sep)),
        json!({"items": [{"department": "library", "cleared": true}, {"department": "Finance", "cleared": true}]}), ver(&sep), StatusCode::OK).await;
    assert_eq!(sep["clearanceComplete"], true);
    assert_eq!(sep["clearance"].as_array().unwrap().len(), 2);
    let sep = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/separation-cases/{}/complete", id(&sep)),
        json!({}),
        ver(&sep),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sep["status"], "completed");
    assert_eq!(sep["archivedProfile"], true);
    let e = engagement(&app, &admin, &anika_eng).await;
    assert_eq!(e["status"], "closed");
    assert_eq!(e["lastWorkingDate"], "2024-06-30");
    let sv = staff(&app, &admin, &anika).await;
    assert_eq!(sv["status"], "archived");
    assert_eq!(
        events(&pool, &anika_eng, "sutraerp.people.staff_engagement.closed").await,
        1
    );
    assert_eq!(
        events(&pool, &anika, "sutraerp.people.staff_member.archived").await,
        1
    );
    let (s, b) = read(
        &app,
        &admin,
        &format!("{P}/engagements/{anika_eng}/separation"),
    )
    .await;
    assert_eq!(
        (s, b["status"].as_str()),
        (StatusCode::OK, Some("completed"))
    );

    // ---- L3-E-008: rejoining (only after closure) restores the archived profile.
    let (s, b) = call(&app, &admin, "POST", &format!("{P}/staff/{anika}/engagements"),
        json!({"employmentType": "contract", "designation": "Visiting Professor", "startDate": "2024-07-01",
               "rejoinOfEngagementId": bhavna_eng}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "other staff's engagement: {b}"
    );
    let (s, b) = call(&app, &admin, "POST", &format!("{P}/staff/{anika}/engagements"),
        json!({"employmentType": "contract", "designation": "Visiting Professor", "startDate": "2024-06-15",
               "rejoinOfEngagementId": anika_eng}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "before the exit: {b}");
    let (s, b) = call(&app, &admin, "POST", &format!("{P}/staff/{anika}/engagements"),
        json!({"employmentType": "contract", "designation": "Visiting Professor", "startDate": "2024-07-01"}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "archived without rejoining: {b}");
    let re = ok(&app, &admin, "POST", &format!("{P}/staff/{anika}/engagements"),
        json!({"employmentType": "contract", "designation": "Visiting Professor", "startDate": "2024-07-01",
               "endDate": "2030-06-30", "rejoinOfEngagementId": anika_eng}), None, StatusCode::CREATED).await;
    assert_eq!(re["rejoinOfEngagementId"], anika_eng.as_str());
    assert_eq!(staff(&app, &admin, &anika).await["status"], "active");
    let re = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/submit", id(&re)),
        json!({}),
        ver(&re),
        StatusCode::OK,
    )
    .await;
    assert_eq!(re["status"], "active");
    let all = fetch(&app, &admin, &format!("{P}/staff/{anika}/engagements")).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 2);

    // ---- L3-E-009: duplicate → canonical (engagements closed / cancelled first).
    let dup = new_staff(&app, &admin, &inst, "Anisha", "Rao", "teaching", "draft").await;
    let dup_id = id(&dup);
    ok(&app, &admin, "POST", &format!("{P}/staff/{dup_id}/qualifications"),
        json!({"qualificationType": "postgraduate", "title": "MSc Physics", "awardingBody": "Pune University", "yearOfCompletion": 2014}), None, StatusCode::CREATED).await;
    let de = ok(&app, &admin, "POST", &format!("{P}/staff/{dup_id}/engagements"),
        json!({"employmentType": "permanent", "designation": "Lecturer", "startDate": "2024-01-01"}), None, StatusCode::CREATED).await;
    let dupv = staff(&app, &admin, &dup_id).await;
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{dup_id}/merge"),
        json!({"canonicalStaffId": anika, "reason": "same person"}),
        ver(&dupv),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "live engagement on the duplicate: {b}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{dup_id}/merge"),
        json!({"canonicalStaffId": dup_id, "reason": "self"}),
        ver(&dupv),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{}/cancel", id(&de)),
        json!({"reason": "duplicate record"}),
        ver(&de),
        StatusCode::OK,
    )
    .await;
    let merged = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{dup_id}/merge"),
        json!({"canonicalStaffId": anika, "reason": "same person, two records"}),
        ver(&dupv),
        StatusCode::OK,
    )
    .await;
    assert_eq!(merged["status"], "merged");
    assert_eq!(merged["canonicalStaffId"], anika.as_str());
    let quals = fetch(&app, &admin, &format!("{P}/staff/{anika}/qualifications")).await;
    assert_eq!(
        quals["items"].as_array().unwrap().len(),
        1,
        "qualification moved"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{dup_id}/merge"),
        json!({"canonicalStaffId": anika, "reason": "again"}),
        ver(&merged),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already merged");
    assert_eq!(
        events(&pool, &dup_id, "sutraerp.people.staff_member.merged").await,
        1
    );

    // ---- L3-E-065: governed correction keeps before / after.
    let av = staff(&app, &admin, &anika).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{anika}/corrections"),
        json!({"changes": {"familyName": "Rao-Kulkarni"}, "reason": " "}),
        ver(&av),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{anika}/corrections"),
        json!({"changes": {"familyName": "Rao"}, "reason": "noop"}),
        ver(&av),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "nothing changes");
    let av = ok(&app, &admin, "POST", &format!("{P}/staff/{anika}/corrections"),
        json!({"changes": {"familyName": "Rao-Kulkarni", "dateOfBirth": "1990-04-21"}, "reason": "gazette notification"}), ver(&av), StatusCode::OK).await;
    assert_eq!(av["familyName"], "Rao-Kulkarni");
    assert_eq!(av["dateOfBirth"], "1990-04-21");
    let cs = fetch(&app, &admin, &format!("{P}/staff/{anika}/corrections")).await;
    let items = cs["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    let fam = items.iter().find(|c| c["field"] == "familyName").unwrap();
    assert_eq!(
        (fam["beforeValue"].as_str(), fam["afterValue"].as_str()),
        (Some("Rao"), Some("Rao-Kulkarni"))
    );
    assert_eq!(
        events(&pool, &anika, "sutraerp.people.staff_member.corrected").await,
        1
    );
    let (s, _) = read(&app, &meera, &format!("{P}/staff/{anika}/corrections")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ---- L3-E-007 + 068: direct closure, then archive (409 while live).
    let bv = staff(&app, &admin, &bhavna).await;
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{bhavna}/archive"),
        json!({"reason": "left"}),
        ver(&bv),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "live engagement: {b}");
    let bhe = engagement(&app, &admin, &bhavna_eng).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/engagements/{bhavna_eng}/close"),
        json!({"lastWorkingDate": "2020-01-01", "reason": "retired"}),
        ver(&bhe),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "before start");
    let bhe = ok(&app, &admin, "POST", &format!("{P}/engagements/{bhavna_eng}/close"),
        json!({"lastWorkingDate": "2024-08-31", "separationKind": "retirement", "reason": "superannuation"}), ver(&bhe), StatusCode::OK).await;
    assert_eq!(bhe["status"], "closed");
    let bv = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{bhavna}/archive"),
        json!({"reason": "retired"}),
        ver(&bv),
        StatusCode::OK,
    )
    .await;
    assert_eq!(bv["status"], "archived");

    // ---- not found / permissions.
    let (s, _) = read(&app, &admin, &format!("{P}/engagements/{}", Uuid::now_v7())).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = read(
        &app,
        &admin,
        &format!("{P}/staff/{}/engagements", Uuid::now_v7()),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = read(&app, &meera, &format!("{P}/staff/{anika}/engagements")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/institutions/{inst}/positions"),
        json!({"code": "X", "title": "X"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // History of the shared aggregates was appended by the shared lifecycle.
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM people.status_history WHERE aggregate_id = $1")
            .bind(Uuid::parse_str(&anika).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert!(n >= 6, "staff history rows: {n}");
}

// ================================================================================= test 2

#[tokio::test]
async fn recruitment_onboarding_faculty_and_self_service() {
    let (app, pool) = app(TENANT_B).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_B, "Onboarding University").await;
    let inst = setup.institution().as_uuid().to_string();

    // ---- L3-E-033: requisition → open.
    let req = ok(&app, &admin, "POST", &format!("{P}/institutions/{inst}/requisitions"),
        json!({"title": "Assistant Professor (CS)", "staffCategory": "teaching", "employmentType": "probation",
               "designation": "Assistant Professor", "grade": "AP-1", "openings": 1, "justification": "new section"}), None, StatusCode::CREATED).await;
    assert_eq!(req["status"], "draft");
    assert!(req["number"].as_str().unwrap().starts_with("REQ-"));
    let rid = id(&req);
    let (s, _) = call(&app, &admin, "POST", &format!("{P}/institutions/{inst}/requisitions"),
        json!({"title": "X", "staffCategory": "teaching", "employmentType": "permanent", "designation": "X", "openings": 0}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/requisitions/{rid}/candidates"),
        json!({"givenName": "Early", "email": "early@b7a.dev"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "requisition not open");
    let req = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/requisitions/{rid}/submit"),
        json!({}),
        ver(&req),
        StatusCode::OK,
    )
    .await;
    assert_eq!(req["status"], "open", "{req}");
    assert!(req["processInstanceId"].is_string());
    assert_eq!(
        events(&pool, &rid, "sutraerp.people.requisition.decided").await,
        1
    );
    // S3-appr (SoD): the requester is refused before anything else, so an independent
    // administrator shows that nothing is pending any more.
    let dean = login(&app, "dean@sutra.dev").await;
    let (s, _) = call(
        &app,
        &dean,
        "POST",
        &format!("{P}/requisitions/{rid}/decision"),
        json!({"decision": "approve"}),
        ver(&req),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing pending");

    // Candidates.
    let c1 = ok(&app, &admin, "POST", &format!("{P}/requisitions/{rid}/candidates"),
        json!({"givenName": "Meera", "familyName": "Shah", "email": "Meera.Shah@b7a.dev", "source": "campus drive"}), None, StatusCode::CREATED).await;
    assert_eq!(c1["status"], "applied");
    assert_eq!(c1["email"], "meera.shah@b7a.dev");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/requisitions/{rid}/candidates"),
        json!({"givenName": "Meera", "email": "meera.shah@b7a.dev"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate candidate");
    let c2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/requisitions/{rid}/candidates"),
        json!({"givenName": "Ravi", "familyName": "Patil", "email": "ravi.patil@b7a.dev"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/offer", id(&c1)),
        json!({"proposedJoiningDate": "2024-01-15"}),
        ver(&c1),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "offer needs a shortlist");
    let c1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/shortlist", id(&c1)),
        json!({}),
        ver(&c1),
        StatusCode::OK,
    )
    .await;
    let c1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/interview", id(&c1)),
        json!({"notes": "strong systems background"}),
        ver(&c1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c1["status"], "interviewed");
    let c1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/offer", id(&c1)),
        json!({"proposedJoiningDate": "2024-01-15"}),
        ver(&c1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c1["status"], "offered");
    assert_eq!(c1["offeredDesignation"], "Assistant Professor");
    let c1 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/accept", id(&c1)),
        json!({}),
        ver(&c1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c1["status"], "offer_accepted");
    let c2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/shortlist", id(&c2)),
        json!({}),
        ver(&c2),
        StatusCode::OK,
    )
    .await;
    let c2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/offer", id(&c2)),
        json!({"proposedJoiningDate": "2024-02-01"}),
        ver(&c2),
        StatusCode::OK,
    )
    .await;
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/accept", id(&c2)),
        json!({}),
        ver(&c2),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "openings taken: {b}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/decline", id(&c2)),
        json!({}),
        ver(&c2),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let c2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/decline", id(&c2)),
        json!({"reason": "counter offer"}),
        ver(&c2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c2["status"], "offer_declined");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/reject", id(&c2)),
        json!({"reason": "x"}),
        ver(&c2),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let cands = fetch(&app, &admin, &format!("{P}/requisitions/{rid}/candidates")).await;
    assert_eq!(cands["items"].as_array().unwrap().len(), 2);

    // ---- Onboarding case → documents → verification → ready → joined.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/onboarding", id(&c2)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "offer not accepted");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/onboarding", id(&c1)),
        json!({"subjectRef": b4::MEERA}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "probation end required: {b}"
    );
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/onboarding", id(&c1)),
        json!({"subjectRef": b4::MEERA, "probationEndDate": "2024-07-15"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(o["status"], "initiated");
    assert_eq!(o["joiningDate"], "2024-01-15");
    assert!(o["number"].as_str().unwrap().starts_with("ONB-"));
    let oid = id(&o);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/candidates/{}/onboarding", id(&c1)),
        json!({"subjectRef": b4::MEERA, "probationEndDate": "2024-07-15"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one case per candidate");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{oid}/join"),
        json!({}),
        ver(&o),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not ready");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{oid}/documents"),
        json!({"documents": [], "submitForVerification": true}),
        ver(&o),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let o = ok(&app, &admin, "POST", &format!("{P}/onboarding-cases/{oid}/documents"),
        json!({"documents": [{"documentType": "identity", "reference": "AADHAAR-XXXX-1234"}], "submitForVerification": true}), ver(&o), StatusCode::OK).await;
    assert_eq!(o["status"], "verification_pending");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{oid}/ready"),
        json!({}),
        ver(&o),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "documents not verified");
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{oid}/verify"),
        json!({"approved": false, "reason": "relieving letter missing"}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "documentation_pending");
    let o = ok(&app, &admin, "POST", &format!("{P}/onboarding-cases/{oid}/documents"),
        json!({"documents": [{"documentType": "employment", "reference": "RELIEVING-2023-77"}], "submitForVerification": true}), ver(&o), StatusCode::OK).await;
    assert_eq!(o["documents"].as_array().unwrap().len(), 2);
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{oid}/verify"),
        json!({"approved": true}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["documentsVerified"], true);
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{oid}/ready"),
        json!({}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "ready_for_joining");
    let o = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/onboarding-cases/{oid}/join"),
        json!({"reason": "joined on time"}),
        ver(&o),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "joined", "{o}");
    let meera_staff = o["staffId"].as_str().unwrap().to_owned();
    let meera_eng = o["engagementId"].as_str().unwrap().to_owned();
    assert_eq!(
        events(&pool, &oid, "sutraerp.people.onboarding_case.submitted").await,
        1
    );
    assert_eq!(
        events(&pool, &oid, "sutraerp.people.onboarding_case.completed").await,
        1
    );
    let sv = staff(&app, &admin, &meera_staff).await;
    assert_eq!(sv["status"], "active");
    assert_eq!(sv["subjectRef"], b4::MEERA);
    assert_eq!(sv["engagement"]["status"], "active");
    assert_eq!(sv["engagement"]["employmentType"], "probation");
    assert_eq!(sv["engagement"]["probationEndDate"], "2024-07-15");
    assert_eq!(
        fetch(&app, &admin, &format!("{P}/requisitions/{rid}")).await["status"],
        "filled"
    );
    let onb = fetch(
        &app,
        &admin,
        &format!("{P}/institutions/{inst}/onboarding-cases?status=joined"),
    )
    .await;
    assert_eq!(onb["items"].as_array().unwrap().len(), 1);

    // ---- /me/profile.
    let (s, me) = read(&app, &meera, &format!("{P}/me/profile")).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert_eq!(me["staff"]["id"], meera_staff.as_str());
    assert_eq!(me["engagement"]["designation"], "Assistant Professor");
    let none = fetch(&app, &admin, &format!("{P}/me/profile")).await;
    assert!(
        none["staff"].is_null(),
        "admin login has no staff record: {none}"
    );
    assert_eq!(none["qualifications"].as_array().unwrap().len(), 0);
    let (s, _) = read(&app, &aarav, &format!("{P}/me/profile")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ---- L3-E-026 / 029: faculty establishment + teaching eligibility.
    let bare = new_staff(&app, &admin, &inst, "Neel", "Joshi", "teaching", "active").await;
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/faculty-profile", id(&bare)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no active engagement: {b}");
    let office = new_staff(
        &app,
        &admin,
        &inst,
        "Asha",
        "Office",
        "non_teaching",
        "active",
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/faculty-profile", id(&office)),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "non-teaching staff");
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{meera_staff}/faculty-profile"),
        json!({"disciplines": "Computer Science", "highestQualification": "PhD"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(f["status"], "active");
    assert!(f["eligibilityStatus"].is_null());
    let fid = id(&f);
    assert_eq!(
        events(&pool, &fid, "sutraerp.people.faculty_profile.established").await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{meera_staff}/faculty-profile"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/eligibility"),
        json!({"action": "approve"}),
        ver(&f),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing requested");
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/eligibility"),
        json!({"action": "request"}),
        ver(&f),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/eligibility"),
        json!({"action": "request"}),
        ver(&f),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["eligibilityStatus"], "requested");
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/eligibility"),
        json!({"action": "review"}),
        ver(&f),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/eligibility"),
        json!({"action": "approve", "validUntil": "2000-01-01"}),
        ver(&f),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/eligibility"),
        json!({"action": "approve", "validUntil": "2099-12-31"}),
        ver(&f),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["eligibilityStatus"], "approved");
    assert_eq!(f["eligibilityValidUntil"], "2099-12-31");

    // ---- L3-E-031: faculty suspension / reactivation drives Domain D's StaffDirectory.
    let port = sutra_people::PgStaffDirectory::new(pool.clone());
    let scope = setup.scope();
    assert!(
        port.staff_by_subject(&scope, b4::MEERA)
            .await
            .unwrap()
            .unwrap()
            .active
    );
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/suspend"),
        json!({"reason": "inquiry"}),
        ver(&f),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["status"], "suspended");
    assert_eq!(f["eligibilityStatus"], "suspended");
    assert_eq!(
        engagement(&app, &admin, &meera_eng).await["status"],
        "suspended"
    );
    assert!(
        !port
            .staff_by_subject(&scope, b4::MEERA)
            .await
            .unwrap()
            .unwrap()
            .active
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/suspend"),
        json!({"reason": "again"}),
        ver(&f),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let f = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/faculty-profiles/{fid}/reactivate"),
        json!({}),
        ver(&f),
        StatusCode::OK,
    )
    .await;
    assert_eq!(f["status"], "active");
    assert_eq!(f["eligibilityStatus"], "approved");
    assert_eq!(
        engagement(&app, &admin, &meera_eng).await["status"],
        "active"
    );
    assert!(
        port.staff_by_subject(&scope, b4::MEERA)
            .await
            .unwrap()
            .unwrap()
            .active
    );

    // ---- L3-E-060: qualifications with verification.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{meera_staff}/qualifications"),
        json!({"qualificationType": "phd", "title": "PhD", "awardingBody": "IIT"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let q = ok(&app, &admin, "POST", &format!("{P}/staff/{meera_staff}/qualifications"),
        json!({"qualificationType": "doctorate", "title": "PhD Computer Science", "awardingBody": "IIT Bombay", "yearOfCompletion": 2019}), None, StatusCode::CREATED).await;
    assert_eq!(q["status"], "pending");
    let q = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff-qualifications/{}/verify", id(&q)),
        json!({"outcome": "verified"}),
        ver(&q),
        StatusCode::OK,
    )
    .await;
    assert_eq!(q["status"], "verified");
    assert!(q["verifiedBy"].is_string());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff-qualifications/{}/verify", id(&q)),
        json!({"outcome": "verified"}),
        ver(&q),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        events(
            &pool,
            &id(&q),
            "sutraerp.people.staff_qualification.verified"
        )
        .await,
        1
    );

    // ---- L3-E-064: documents with verification.
    let (s, _) = call(&app, &admin, "POST", &format!("{P}/staff/{meera_staff}/documents"),
        json!({"documentType": "identity", "title": "Passport", "reference": "P1234567", "validUntil": "2001-01-01"}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "already expired");
    let d = ok(&app, &admin, "POST", &format!("{P}/staff/{meera_staff}/documents"),
        json!({"documentType": "identity", "title": "Passport", "reference": "P1234567", "validUntil": "2099-01-01"}), None, StatusCode::CREATED).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff-documents/{}/verify", id(&d)),
        json!({"outcome": "rejected"}),
        ver(&d),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "rejection reason");
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff-documents/{}/verify", id(&d)),
        json!({"outcome": "verified"}),
        ver(&d),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "verified");
    assert_eq!(
        events(&pool, &id(&d), "sutraerp.people.staff_document.verified").await,
        1
    );

    // ---- L3-E-039: certificate request (self) → issue.
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/staff/{meera_staff}/certificate-requests"),
        json!({"kind": "service", "purpose": "bank loan"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "HR channel");
    let c = ok(
        &app,
        &meera,
        "POST",
        &format!("{P}/me/certificate-requests"),
        json!({"kind": "service", "purpose": "Bank loan application"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(c["status"], "approved", "{c}");
    assert_eq!(c["requestedVia"], "self");
    let mine = fetch(&app, &meera, &format!("{P}/me/certificate-requests")).await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    let (s, _) = call(
        &app,
        &meera,
        "POST",
        &format!("{P}/certificate-requests/{}/issue", id(&c)),
        json!({}),
        ver(&c),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let c = ok(
        &app,
        &admin,
        "POST",
        &format!("{P}/certificate-requests/{}/issue", id(&c)),
        json!({}),
        ver(&c),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "completed");
    assert!(c["certificateNumber"]
        .as_str()
        .unwrap()
        .starts_with("CERT-"));
    assert_eq!(c["content"]["designation"], "Assistant Professor");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/certificate-requests/{}/issue", id(&c)),
        json!({}),
        ver(&c),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(
        events(&pool, &id(&c), "sutraerp.people.certificate_request.issued").await,
        1
    );
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/staff/{}/certificate-requests", id(&bare)),
        json!({"kind": "experience", "purpose": "visa"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no service: {b}");

    // My HR shows everything.
    let me = fetch(&app, &meera, &format!("{P}/me/profile")).await;
    assert_eq!(me["qualifications"].as_array().unwrap().len(), 1);
    assert_eq!(me["documents"].as_array().unwrap().len(), 1);
    assert_eq!(me["certificateRequests"].as_array().unwrap().len(), 1);
    assert_eq!(me["faculty"]["eligibilityStatus"], "approved");
}

// ================================================================================= test 3

/// People router with a hand-built state and an injected administrator session.
fn custom_app(st: sutra_people::PeopleState, tenant: &str, subject: &str) -> Router {
    let (router, _) = sutra_people::router_with_state(st).split_for_parts();
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
    let (app, pool) = app(TENANT_C).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT_C, "Approvals University").await;
    let inst = setup.institution().as_uuid().to_string();
    let mut st = sutra_api::app::people_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let hr = custom_app(st, TENANT_C, "dev|admin@sutra.dev");
    let me = Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    };

    // ---- ENGAGEMENT_ACTIVATION: submit stays pending; reject → resubmit → approve.
    let k = b7::seed_staff(&pool, &setup, "Kavya", None, StaffCategory::Teaching, None).await;
    let kid = k.staff_id.as_uuid().to_string();
    let e = ok(&hr, &me, "POST", &format!("{P}/staff/{kid}/engagements"),
        json!({"employmentType": "permanent", "designation": "Lecturer", "grade": "L-10", "startDate": "2024-01-01"}), None, StatusCode::CREATED).await;
    let eid = id(&e);
    let e = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/submit"),
        json!({}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "pending");
    assert_eq!(e["pendingChange"]["status"], "pending_approval");
    assert_eq!(e["pendingChange"]["kind"], "activation");
    assert!(e["pendingChange"]["processInstanceId"].is_string());
    let (s, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/submit"),
        json!({}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already submitted");
    let (s, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/decision"),
        json!({"decision": "reject"}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "rejection reason");
    let e = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/decision"),
        json!({"decision": "reject", "reason": "grade not sanctioned"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "pending");
    assert!(e["pendingChange"].is_null());
    let (s, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/decision"),
        json!({"decision": "approve"}),
        ver(&e),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing pending");
    let e = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/submit"),
        json!({"reason": "grade sanctioned"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    let e = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/decision"),
        json!({"decision": "approve"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "active", "{e}");
    assert_eq!(
        events(&pool, &eid, "sutraerp.people.staff_engagement.submitted").await,
        2
    );
    assert_eq!(
        events(&pool, &eid, "sutraerp.people.staff_engagement.activated").await,
        1
    );

    // A future start date: approved, then scheduled until /activate.
    let f = b7::seed_staff(&pool, &setup, "Farhan", None, StaffCategory::Teaching, None).await;
    let fe = ok(&hr, &me, "POST", &format!("{P}/staff/{}/engagements", f.staff_id),
        json!({"employmentType": "permanent", "designation": "Lecturer", "startDate": "2099-01-01"}), None, StatusCode::CREATED).await;
    let fe = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{}/submit", id(&fe)),
        json!({}),
        ver(&fe),
        StatusCode::OK,
    )
    .await;
    let fe = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{}/decision", id(&fe)),
        json!({"decision": "approve"}),
        ver(&fe),
        StatusCode::OK,
    )
    .await;
    assert_eq!(fe["status"], "pending");
    assert_eq!(fe["pendingChange"]["status"], "approved");
    let (s, b) = call(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{}/activate", id(&fe)),
        json!({}),
        ver(&fe),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "starts in 2099: {b}");

    // ---- ENGAGEMENT_CHANGE: modification waits for its decision.
    let e = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/modify"),
        json!({"designation": "Senior Lecturer", "reason": "promotion"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["designation"], "Lecturer");
    assert_eq!(e["pendingChange"]["kind"], "modification");
    assert_eq!(
        e["pendingChange"]["proposal"]["designation"],
        "Senior Lecturer"
    );
    let e = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/engagements/{eid}/decision"),
        json!({"decision": "approve"}),
        ver(&e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["designation"], "Senior Lecturer");
    assert!(e["pendingChange"].is_null());

    // ---- POSITION_ASSIGNMENT.
    let p = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/institutions/{inst}/positions"),
        json!({"code": "LECT-01", "title": "Lecturer", "allowsMultiple": true}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/positions/{}/activate", id(&p)),
        json!({}),
        ver(&p),
        StatusCode::OK,
    )
    .await;
    let a = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/positions/{}/assignments", id(&p)),
        json!({"engagementId": eid, "startDate": "2024-01-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a["status"], "pending_approval");
    let a = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/position-assignments/{}/decision", id(&a)),
        json!({"decision": "approve"}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "active");

    // ---- REQUISITION_APPROVAL.
    let r = ok(&hr, &me, "POST", &format!("{P}/institutions/{inst}/requisitions"),
        json!({"title": "Lab assistant", "staffCategory": "non_teaching", "employmentType": "contract", "designation": "Lab Assistant", "openings": 2}), None, StatusCode::CREATED).await;
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/requisitions/{}/submit", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "pending_approval");
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/requisitions/{}/decision", id(&r)),
        json!({"decision": "reject", "reason": "no budget"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "draft");
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/requisitions/{}/submit", id(&r)),
        json!({}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    let r = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/requisitions/{}/decision", id(&r)),
        json!({"decision": "approve"}),
        ver(&r),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "open");

    // ---- SEPARATION_APPROVAL + CERTIFICATE_APPROVAL.
    let sep = ok(&hr, &me, "POST", &format!("{P}/engagements/{eid}/separation"),
        json!({"kind": "contract_end", "lastWorkingDate": "2024-12-31", "reason": "project closed"}), None, StatusCode::CREATED).await;
    assert_eq!(sep["status"], "pending_approval");
    let (s, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{P}/separation-cases/{}/clearance", id(&sep)),
        json!({"items": [{"department": "IT", "cleared": true}]}),
        ver(&sep),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not approved yet");
    let sep = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/separation-cases/{}/decision", id(&sep)),
        json!({"decision": "approve"}),
        ver(&sep),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sep["status"], "approved");
    let c = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/staff/{kid}/certificate-requests"),
        json!({"kind": "service", "purpose": "passport"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(c["status"], "pending_approval");
    let (s, _) = call(
        &hr,
        &me,
        "POST",
        &format!("{P}/certificate-requests/{}/issue", id(&c)),
        json!({}),
        ver(&c),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not approved");
    let c = ok(
        &hr,
        &me,
        "POST",
        &format!("{P}/certificate-requests/{}/decision", id(&c)),
        json!({"decision": "approve"}),
        ver(&c),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "approved");
}
