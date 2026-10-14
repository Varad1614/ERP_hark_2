//! Slice 8C DB tests — library (Domain G) and reports & analytics (Domain H).
//!
//! * `library_policy_membership_catalogue_circulation_fines_and_clearance` — L3-G-124…139, 149,
//!   152 through the HTTP API: policy versions (`LIBRARY_POLICY_APPROVAL`, supersession),
//!   memberships (active / suspended / restored / closed, one live membership per person,
//!   administrative restriction), catalogue records (ISBN, update, publish) and copies (accession
//!   numbers, library spaces, condition → restriction, withdrawal), issue (limits, restrictions,
//!   reservation priority) → return → renewal, reservations (queue → ready → collected / expired
//!   / cancelled), the overdue sweep (idempotent per loan per day) → fine, billing a student's
//!   fine through `CampusChargePort` (`LIB-FINE`, `campus_charge` in Finance), settlement read
//!   back through `ChargeDirectory`, a staff fine waived (`FINE_WAIVER`), a lost copy → replacement
//!   charge (`LIB-LOST`), clearance (blocked → settled → approved), `/me/library` self-service and
//!   a library complaint opened as a service ticket.
//! * `library_await_decision_policy_fine_waiver_and_clearance` — `ApprovalMode::AwaitDecision`
//!   through `sutra_campus::router_with_state`: pending policy approval, fine waiver decided /
//!   rejected with stale ETags refused, and a clearance awaiting its decision.
//! * `reports_dashboards_widgets_f076_and_regulatory_cycle` — L3-H-048, 050, 052, 054, F-076,
//!   G-154, G-320 and H-055…060: widgets bound to live metrics, the finance KPI dashboard
//!   (only `finance.*` figures) published → new version → rollback, assignments → `/me/dashboards`,
//!   the library and infrastructure operational reports, and a regulatory cycle (framework
//!   versions → cycle → snapshot with issues rejected → validated frozen snapshot → package
//!   approved → submitted → acknowledged → correction → corrected package → closure).
//! * `reports_await_decision_dashboard_and_submission_approval` — `ApprovalMode::AwaitDecision`
//!   through `sutra_services::router_with_state` (`services_deps`).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use support::b4;
use support::b8;
use support::*;
use sutra_contracts::campus::status::SpaceType;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b8c00";
const TENANT_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b8c01";
const TENANT_REPORTS: &str = "0192f5a0-7c3e-7000-8000-0000000b8c02";
const TENANT_REPORTS_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b8c03";
const C: &str = "/api/v1/campus";
const S: &str = "/api/v1/services";
const ADMIN: &str = "dev|admin@sutra.dev";

async fn call(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    (s, b)
}

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn sid(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
}

async fn count(pool: &sqlx::PgPool, sql: &str, arg: &str) -> i64 {
    sqlx::query_scalar(sql)
        .bind(arg)
        .fetch_one(pool)
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

/// Moves a loan's dates back so it is `days_late` days overdue today.
async fn backdate_loan(pool: &sqlx::PgPool, loan: &str, today: NaiveDate, days_late: i64) {
    sqlx::query("UPDATE campus.library_loan SET issued_on = $2, due_on = $3 WHERE id = $1::uuid")
        .bind(loan)
        .bind(today - Duration::days(days_late + 14))
        .bind(today - Duration::days(days_late))
        .execute(pool)
        .await
        .unwrap();
}

/// Settles a Finance charge in full (stands in for a fee payment).
async fn settle(pool: &sqlx::PgPool, charge: &str) {
    sqlx::query(
        "UPDATE finance.charge SET settled_amount = total_amount + adjustment_amount WHERE id = $1::uuid",
    )
    .bind(charge)
    .execute(pool)
    .await
    .unwrap();
}

fn policy_body(name: &str, fine: &str) -> Value {
    json!({"name": name, "effectiveFrom": "2026-01-01", "loanDaysStudent": 14, "loanDaysStaff": 30,
        "maxLoansStudent": 2, "maxLoansStaff": 5, "maxRenewals": 1, "finePerDay": fine,
        "maxFine": "50.00", "reservationHoldDays": 3, "lostChargeFactor": "1.5"})
}

#[tokio::test]
async fn library_policy_membership_catalogue_circulation_fines_and_clearance() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT, "Library University").await;
    let inst = setup.institution().as_uuid();
    let day = today(&pool, inst).await;
    let h = b8::seed_hierarchy(
        &pool,
        &setup,
        "LIB",
        &[
            ("RR1", SpaceType::ReadingRoom, 40),
            ("STACK", SpaceType::Library, 100),
            ("CLS", SpaceType::Classroom, 30),
        ],
    )
    .await;
    let student = b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let diya = b8::seed_student(&pool, &setup, "Diya", None).await;
    let meera = b8::seed_staff(&pool, &setup, "Meera", Some(b4::MEERA)).await;

    // ---- no policy yet: the student's self-service view has no membership.
    let (s, me) = read(&app, &aarav, &format!("{C}/me/library")).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert!(me["membership"].is_null());
    let (s, _) = read(&app, &admin, &format!("{C}/me/library")).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "the admin login is no campus person"
    );
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/institutions/{inst}/library-policies"),
        policy_body("Nope", "1.00"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ================================================================ policy versions
    let pol_path = format!("{C}/institutions/{inst}/library-policies");
    let (s, p1) = call(
        &app,
        &admin,
        "POST",
        &pol_path,
        policy_body("2026 rules", "1.00"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{p1}");
    assert_eq!(
        (p1["status"].as_str(), p1["approvalState"].as_str()),
        (Some("draft"), Some("not_submitted"))
    );
    assert_eq!(p1["lostChargeFactor"], "1.5000");
    let p1id = sid(&p1);
    let (s, bad) = call(
        &app,
        &admin,
        "POST",
        &pol_path,
        json!({"name": "Bad", "effectiveFrom": "2026-01-01", "loanDaysStudent": 0, "loanDaysStaff": 30,
            "maxLoansStudent": 2, "maxLoansStaff": 5, "maxRenewals": 1, "finePerDay": "1.00",
            "reservationHoldDays": 3, "lostChargeFactor": "1"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{p1id}/activate"),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "activation needs an approved draft"
    );
    let (s, p1) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{p1id}/submit"),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p1}");
    assert_eq!(p1["approvalState"], "approved", "auto-approval");
    assert!(p1["processInstanceId"].is_string());
    let (s, p1) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{p1id}/activate"),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(
        (s, p1["status"].as_str()),
        (StatusCode::OK, Some("active")),
        "{p1}"
    );
    let (s, p2) = call(
        &app,
        &admin,
        "POST",
        &pol_path,
        policy_body("2026 rules v2", "2.00"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let p2id = sid(&p2);
    let (_, p2) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{p2id}/submit"),
        json!({}),
        Some(ver(&p2)),
    )
    .await;
    let (s, p2) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-policies/{p2id}/activate"),
        json!({}),
        Some(ver(&p2)),
    )
    .await;
    assert_eq!((s, p2["status"].as_str()), (StatusCode::OK, Some("active")));
    let (_, p1) = read(&app, &admin, &format!("{C}/library-policies/{p1id}")).await;
    assert_eq!(p1["status"], "superseded");
    assert_eq!(p1["supersededBy"], p2id.as_str());
    assert_eq!(
        events(&pool, "sutraerp.campus.library_policy.activated", &p2id).await,
        1
    );

    // ================================================================ G-124 memberships
    let mem_path = format!("{C}/institutions/{inst}/library-memberships");
    let (s, m_aarav) = call(
        &app,
        &admin,
        "POST",
        &mem_path,
        json!({"personKind": "student", "personId": student.student_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{m_aarav}");
    assert_eq!(m_aarav["status"], "active");
    assert!(m_aarav["number"].as_str().unwrap().starts_with("LM-"));
    let aid = sid(&m_aarav);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &mem_path,
        json!({"personKind": "student", "personId": student.student_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one live membership per person");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &mem_path,
        json!({"personKind": "student", "personId": Uuid::now_v7()}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (_, m_diya) = call(
        &app,
        &admin,
        "POST",
        &mem_path,
        json!({"personKind": "student", "personId": diya.student_id}),
        None,
    )
    .await;
    let did = sid(&m_diya);
    let (s, m_meera) = call(
        &app,
        &admin,
        "POST",
        &mem_path,
        json!({"personKind": "staff", "personId": meera.staff_id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{m_meera}");
    let mid = sid(&m_meera);
    assert_eq!(
        events(&pool, "sutraerp.campus.library_membership.activated", &aid).await,
        1
    );

    // ================================================================ G-127 / 128 catalogue
    let cat_path = format!("{C}/institutions/{inst}/catalogue");
    let (s, rec) = call(
        &app,
        &admin,
        "POST",
        &cat_path,
        json!({"title": "Thermodynamics", "authors": ["Cengel"],
        "isbn": "978-0-306-40615-7", "subjects": ["Physics"], "replacementCost": "400.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rec}");
    assert_eq!(
        (rec["status"].as_str(), rec["isbn"].as_str()),
        (Some("draft"), Some("9780306406157"))
    );
    let rid = sid(&rec);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &cat_path,
        json!({"title": "Copy", "isbn": "9780306406157"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate ISBN");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &cat_path,
        json!({"title": "Bad", "isbn": "9780306406158"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "check digit");
    let (s, rec) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{C}/catalogue-records/{rid}"),
        json!({"edition": "8th"}),
        Some(ver(&rec)),
    )
    .await;
    assert_eq!(
        (s, rec["edition"].as_str()),
        (StatusCode::OK, Some("8th")),
        "{rec}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{C}/catalogue-records/{rid}"),
        json!({"edition": "9th"}),
        Some(1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale ETag");
    let (s, rec) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{rid}/publish"),
        json!({}),
        Some(ver(&rec)),
    )
    .await;
    assert_eq!(
        (s, rec["status"].as_str()),
        (StatusCode::OK, Some("published"))
    );
    assert_eq!(
        events(&pool, "sutraerp.campus.catalogue_record.published", &rid).await,
        1
    );
    let mut records = Vec::new();
    for title in ["Fluid Mechanics", "Heat Transfer"] {
        let (_, r) = call(
            &app,
            &admin,
            "POST",
            &cat_path,
            json!({"title": title, "replacementCost": "300.00"}),
            None,
        )
        .await;
        let (s, r) = call(
            &app,
            &admin,
            "POST",
            &format!("{C}/catalogue-records/{}/publish", sid(&r)),
            json!({}),
            Some(ver(&r)),
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        records.push(sid(&r));
    }
    let (fluid, heat) = (records[0].clone(), records[1].clone());

    // ================================================================ G-129 / 130 copies
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{rid}/copies"),
        json!({"nodeId": h.spaces[2].id}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a classroom is no library space"
    );
    let (s, c1) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{rid}/copies"),
        json!({"nodeId": h.spaces[1].id, "shelfMark": "PHY-1"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{c1}");
    assert_eq!(c1["status"], "available");
    assert_eq!(c1["nodeCode"], "LIB-B1-STACK");
    let acc1 = c1["accessionNumber"].as_str().unwrap().to_owned();
    assert!(acc1.starts_with("ACC-"));
    let c1id = sid(&c1);
    let (_, c2) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{rid}/copies"),
        json!({"nodeId": h.spaces[0].id}),
        None,
    )
    .await;
    let c2id = sid(&c2);
    let (_, cf) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{fluid}/copies"),
        json!({}),
        None,
    )
    .await;
    let cfid = sid(&cf);
    let (_, ch) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{heat}/copies"),
        json!({}),
        None,
    )
    .await;
    let chid = sid(&ch);
    let (_, list) = read(&app, &admin, &format!("{C}/catalogue-records/{rid}/copies")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);

    // ================================================================ G-131 issue
    let loan_path = format!("{C}/institutions/{inst}/library-loans");
    let (s, l1) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": aid, "accessionNumber": acc1}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{l1}");
    assert_eq!(l1["status"], "active");
    assert!(l1["number"].as_str().unwrap().starts_with("LN-"));
    assert_eq!(
        l1["dueOn"],
        (day + Duration::days(14)).to_string(),
        "student loan days of the active policy"
    );
    let l1id = sid(&l1);
    assert_eq!(
        events(&pool, "sutraerp.campus.library_loan.issued", &l1id).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": mid, "copyId": c1id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "copy already issued");
    let (s, l2) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": mid, "copyId": c2id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        l2["dueOn"],
        (day + Duration::days(30)).to_string(),
        "staff loan days"
    );
    let l2id = sid(&l2);
    let (s, lf) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": aid, "copyId": cfid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{lf}");
    let lfid = sid(&lf);
    let (s, limit) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": aid, "copyId": chid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "student limit of 2 loans: {limit}");

    // ================================================================ G-134 reservations + G-133 renewal
    let (s, r_diya) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{rid}/reservations"),
        json!({"membershipId": did}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r_diya}");
    assert_eq!(
        (r_diya["status"].as_str(), r_diya["queuePosition"].as_i64()),
        (Some("queued"), Some(1))
    );
    assert_eq!(r_diya["currentPosition"], 1);
    let rdid = sid(&r_diya);
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/reservations"),
        json!({"recordId": rid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already holds this title");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/catalogue-records/{heat}/reservations"),
        json!({"membershipId": did}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a copy is on the shelf");
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/loans/{l1id}/renew"),
        json!({}),
        Some(ver(&l1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "reserved by another member");
    let (s, lf) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/loans/{lfid}/renew"),
        json!({}),
        Some(ver(&lf)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{lf}");
    assert_eq!(lf["renewals"], 1);
    assert_eq!(lf["dueOn"], (day + Duration::days(28)).to_string());
    assert_eq!(
        events(&pool, "sutraerp.campus.library_loan.renewed", &lfid).await,
        1
    );
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/loans/{lfid}/renew"),
        json!({}),
        Some(ver(&lf)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "out of renewals");
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/loans/{l2id}/renew"),
        json!({}),
        Some(ver(&l2)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "another member's loan");

    // ================================================================ G-132 return → G-135 ready → collected
    let (s, l2) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-loans/{l2id}/return"),
        json!({}),
        Some(ver(&l2)),
    )
    .await;
    assert_eq!(
        (s, l2["status"].as_str()),
        (StatusCode::OK, Some("returned")),
        "{l2}"
    );
    assert!(l2["fineAmount"].is_null(), "on time");
    let (_, r_diya) = read(
        &app,
        &admin,
        &format!("{C}/catalogue-records/{rid}/reservations"),
    )
    .await;
    let r = &r_diya["items"][0];
    assert_eq!(
        (r["status"].as_str(), r["copyId"].as_str()),
        (Some("ready"), Some(c2id.as_str()))
    );
    assert_eq!(r["holdUntil"], (day + Duration::days(3)).to_string());
    assert_eq!(
        events(&pool, "sutraerp.campus.library_reservation.ready", &rdid).await,
        1
    );
    let (_, c2) = read(&app, &admin, &format!("{C}/catalogue-records/{rid}/copies")).await;
    assert!(c2["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == c2id.as_str() && c["status"] == "reserved"));
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": mid, "copyId": c2id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "held for another member");
    let (s, collected) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-reservations/{rdid}/collect"),
        json!({}),
        Some(r["version"].as_i64().unwrap()),
    )
    .await;
    assert_eq!(
        (s, collected["status"].as_str()),
        (StatusCode::OK, Some("collected")),
        "{collected}"
    );
    let l_diya = collected["loanId"].as_str().unwrap().to_owned();

    // ================================================================ G-137 overdue sweep → fine
    backdate_loan(&pool, &l1id, day, 6).await;
    let sweep = format!("{C}/institutions/{inst}/library-loans/overdue-sweep");
    let (s, run) = call(&app, &admin, "POST", &sweep, json!({}), None).await;
    assert_eq!(s, StatusCode::OK, "{run}");
    assert_eq!(
        (
            run["processed"].as_i64(),
            run["failed"].as_array().unwrap().len()
        ),
        (Some(1), 0)
    );
    let (s, run) = call(&app, &admin, "POST", &sweep, json!({}), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(
        (run["processed"].as_i64(), run["skipped"].as_i64()),
        (Some(0), Some(1)),
        "idempotent per loan per day"
    );
    assert_eq!(
        events(&pool, "sutraerp.campus.library_loan.overdue", &l1id).await,
        1
    );
    let (_, l1) = read(&app, &admin, &format!("{C}/library-loans/{l1id}")).await;
    assert_eq!(
        (l1["status"].as_str(), l1["fineAmount"].as_str()),
        (Some("overdue"), Some("12.00")),
        "6 days × 2.00"
    );
    let (_, elig) = read(
        &app,
        &admin,
        &format!("{C}/library-memberships/{aid}/eligibility"),
    )
    .await;
    assert_eq!(elig["eligible"], false);
    assert_eq!(elig["unsettledFines"], "12.00");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-loans/{l1id}/renew"),
        json!({}),
        Some(ver(&l1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "an overdue loan is not renewed");
    let (s, l1) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-loans/{l1id}/return"),
        json!({}),
        Some(ver(&l1)),
    )
    .await;
    assert_eq!(
        (s, l1["status"].as_str()),
        (StatusCode::OK, Some("returned"))
    );
    assert_eq!(l1["fineAmount"], "12.00");
    assert_eq!(
        events(&pool, "sutraerp.campus.library_loan.returned", &l1id).await,
        1
    );
    let (_, copies) = read(&app, &admin, &format!("{C}/catalogue-records/{rid}/copies")).await;
    assert!(copies["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == c1id.as_str() && c["status"] == "available"));

    // ---- billing the student's fine through CampusChargePort (LIB-FINE).
    let (_, fines) = read(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/library-fines?membershipId={aid}"),
    )
    .await;
    let fine = fines["items"][0].clone();
    assert_eq!(
        (
            fine["status"].as_str(),
            fine["amount"].as_str(),
            fine["days"].as_i64()
        ),
        (Some("assessed"), Some("12.00"), Some(6))
    );
    let fid = sid(&fine);
    let (s, fine) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{fid}/bill"),
        json!({}),
        Some(ver(&fine)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{fine}");
    assert_eq!(fine["status"], "billed");
    assert!(fine["chargeNumber"].as_str().unwrap().starts_with("CHG-"));
    let charge = fine["chargeId"].as_str().unwrap().to_owned();
    let (src, status, outstanding, fee): (String, String, rust_decimal::Decimal, String) = sqlx::query_as(
        "SELECT c.source_type, c.status, c.outstanding_amount, h.code FROM finance.charge c \
         JOIN finance.charge_line l ON l.charge_id = c.id JOIN finance.fee_head h ON h.id = l.fee_head_id \
         WHERE c.id = $1::uuid",
    )
    .bind(&charge)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (src.as_str(), status.as_str(), outstanding, fee.as_str()),
        ("campus_charge", "published", b8::dec("12.00"), "LIB-FINE")
    );
    assert_eq!(
        events(&pool, "sutraerp.campus.library_fine.raised", &fid).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{fid}/bill"),
        json!({}),
        Some(ver(&fine)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "billed once");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{fid}/waiver"),
        json!({"reason": "late"}),
        Some(ver(&fine)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "waivers happen before billing");
    let (s, blocked) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": aid, "copyId": c1id}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "unsettled billed fine blocks borrowing: {blocked}"
    );
    settle(&pool, &charge).await;
    let (s, me) = read(&app, &aarav, &format!("{C}/me/library")).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    assert_eq!(me["membership"]["number"], m_aarav["number"]);
    assert_eq!(
        me["fines"][0]["status"], "paid",
        "settlement read back from Finance"
    );
    assert_eq!(me["eligibility"]["eligible"], true, "{me}");
    assert!(me["loans"].as_array().unwrap().len() >= 2);

    // ================================================================ G-138 staff fine waived (FINE_WAIVER)
    let (s, lh) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": mid, "copyId": chid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{lh}");
    let lhid = sid(&lh);
    backdate_loan(&pool, &lhid, day, 3).await;
    let (_, lh) = read(&app, &admin, &format!("{C}/library-loans/{lhid}")).await;
    let (_, lh) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-loans/{lhid}/return"),
        json!({}),
        Some(ver(&lh)),
    )
    .await;
    assert_eq!(lh["fineAmount"], "6.00");
    let (_, fines) = read(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/library-fines?membershipId={mid}"),
    )
    .await;
    let sfine = fines["items"][0].clone();
    let sfid = sid(&sfine);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{sfid}/bill"),
        json!({}),
        Some(ver(&sfine)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "staff fines are not billed in Batch 8"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{sfid}/waiver"),
        json!({}),
        Some(ver(&sfine)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, sfine) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{sfid}/waiver"),
        json!({"reason": "Faculty on exam duty"}),
        Some(ver(&sfine)),
    )
    .await;
    assert_eq!(
        (s, sfine["status"].as_str()),
        (StatusCode::OK, Some("waived")),
        "{sfine}"
    );
    assert_eq!(
        events(&pool, "sutraerp.campus.library_fine.waived", &sfid).await,
        1
    );

    // ================================================================ G-139 lost copy → LIB-LOST
    let (_, c2) = read(&app, &admin, &format!("{C}/catalogue-records/{rid}/copies")).await;
    let c2v = c2["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == c2id.as_str())
        .unwrap()
        .clone();
    assert_eq!(c2v["status"], "issued");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-copies/{c2id}/lost"),
        json!({}),
        Some(ver(&c2v)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, lost) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-copies/{c2id}/lost"),
        json!({"reason": "Lost on the bus"}),
        Some(ver(&c2v)),
    )
    .await;
    assert_eq!(
        (s, lost["status"].as_str()),
        (StatusCode::OK, Some("lost")),
        "{lost}"
    );
    let (_, ld) = read(&app, &admin, &format!("{C}/library-loans/{l_diya}")).await;
    assert_eq!(ld["status"], "lost");
    let (_, fines) = read(
        &app,
        &admin,
        &format!("{C}/institutions/{inst}/library-fines?membershipId={did}"),
    )
    .await;
    let rfine = fines["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "replacement")
        .unwrap()
        .clone();
    assert_eq!(rfine["amount"], "600.00", "400.00 × 1.5");
    let (s, rfine) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/bill", sid(&rfine)),
        json!({}),
        Some(ver(&rfine)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rfine}");
    let lost_charge = rfine["chargeId"].as_str().unwrap().to_owned();
    let fee: String = sqlx::query_scalar(
        "SELECT h.code FROM finance.charge_line l JOIN finance.fee_head h ON h.id = l.fee_head_id WHERE l.charge_id = $1::uuid",
    )
    .bind(&lost_charge)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(fee, "LIB-LOST");

    // ================================================================ G-149 clearance
    let clr = format!("{C}/institutions/{inst}/library-clearances");
    let (s, k) = call(
        &app,
        &admin,
        "POST",
        &clr,
        json!({"membershipId": did, "purpose": "Programme exit"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{k}");
    assert_eq!(
        k["status"], "requested",
        "blocked by the unsettled replacement charge"
    );
    assert!(!k["verification"]["blockers"].as_array().unwrap().is_empty());
    let kid = sid(&k);
    assert_eq!(
        events(&pool, "sutraerp.campus.library_clearance.requested", &kid).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &clr,
        json!({"membershipId": did, "purpose": "Again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open request");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-clearances/{kid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&k)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "blocked");
    settle(&pool, &lost_charge).await;
    let (s, k) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-clearances/{kid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&k)),
    )
    .await;
    assert_eq!(
        (s, k["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{k}"
    );
    assert_eq!(
        events(&pool, "sutraerp.campus.library_clearance.decided", &kid).await,
        1
    );
    // Aarav asks for clearance himself while a loan is still out.
    let (s, mk) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/clearance"),
        json!({"purpose": "Transfer"}),
        None,
    )
    .await;
    assert_eq!(
        (s, mk["status"].as_str()),
        (StatusCode::CREATED, Some("requested")),
        "{mk}"
    );
    assert_eq!(mk["verification"]["activeLoans"], 1);
    let (s, mine) = read(&app, &aarav, &format!("{C}/me/library/clearance")).await;
    assert_eq!((s, mine["id"].clone()), (StatusCode::OK, mk["id"].clone()));
    let (_, lf) = read(&app, &admin, &format!("{C}/library-loans/{lfid}")).await;
    let (_, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-loans/{lfid}/return"),
        json!({}),
        Some(ver(&lf)),
    )
    .await;
    let (s, mk) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-clearances/{}/decision", sid(&mk)),
        json!({"decision": "approve"}),
        Some(ver(&mk)),
    )
    .await;
    assert_eq!(
        (s, mk["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{mk}"
    );

    // ================================================================ G-125 / 126 suspension, restoration, closure
    let (_, m) = read(&app, &admin, &format!("{C}/library-memberships/{mid}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{mid}/suspend"),
        json!({}),
        Some(ver(&m)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, m) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{mid}/suspend"),
        json!({"reason": "Repeated damage"}),
        Some(ver(&m)),
    )
    .await;
    assert_eq!(
        (s, m["status"].as_str()),
        (StatusCode::OK, Some("suspended"))
    );
    assert_eq!(
        events(&pool, "sutraerp.campus.library_membership.suspended", &mid).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": mid, "copyId": c1id}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a suspended member does not borrow"
    );
    let (s, m) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{mid}/restore"),
        json!({}),
        Some(ver(&m)),
    )
    .await;
    assert_eq!((s, m["status"].as_str()), (StatusCode::OK, Some("active")));
    let (s, m) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{mid}/close"),
        json!({"reason": "Left"}),
        Some(ver(&m)),
    )
    .await;
    assert_eq!(
        (s, m["status"].as_str()),
        (StatusCode::OK, Some("closed")),
        "waived fine, no loans: {m}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{mid}/restore"),
        json!({}),
        Some(ver(&m)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "closed is final");

    // ================================================================ G-152 restriction → resolution
    let (_, md) = read(&app, &admin, &format!("{C}/library-memberships/{did}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{did}/restriction"),
        json!({"restricted": true}),
        Some(ver(&md)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, md) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{did}/restriction"),
        json!({"restricted": true, "reason": "Lost copy dispute"}),
        Some(ver(&md)),
    )
    .await;
    assert_eq!(
        (s, md["restrictionReason"].as_str()),
        (StatusCode::OK, Some("Lost copy dispute"))
    );
    let (_, elig) = read(
        &app,
        &admin,
        &format!("{C}/library-memberships/{did}/eligibility"),
    )
    .await;
    assert!(elig["blockers"]
        .as_array()
        .unwrap()
        .iter()
        .any(|b| b.as_str().unwrap().contains("restriction")));
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": did, "copyId": c1id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "restricted");
    let (s, md) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{did}/restriction"),
        json!({"restricted": false}),
        Some(ver(&md)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert!(md["restrictionReason"].is_null());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-memberships/{did}/restriction"),
        json!({"restricted": false}),
        Some(ver(&md)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing to resolve");

    // ================================================================ G-136 expiry + cancellation (self-service)
    let (s, ld2) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": did, "copyId": c1id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ld2}");
    let (s, mr) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/reservations"),
        json!({"recordId": rid}),
        None,
    )
    .await;
    assert_eq!(
        (s, mr["status"].as_str()),
        (StatusCode::CREATED, Some("queued")),
        "{mr}"
    );
    let mrid = sid(&mr);
    let (_, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-loans/{}/return", sid(&ld2)),
        json!({}),
        Some(ver(&ld2)),
    )
    .await;
    let (_, mine) = read(&app, &aarav, &format!("{C}/me/library/reservations")).await;
    let mr = mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == mrid.as_str())
        .unwrap()
        .clone();
    assert_eq!(mr["status"], "ready");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-reservations/{mrid}/expire"),
        json!({}),
        Some(ver(&mr)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "the hold still runs");
    sqlx::query("UPDATE campus.library_reservation SET hold_until = $2 WHERE id = $1::uuid")
        .bind(&mrid)
        .bind(day - Duration::days(1))
        .execute(&pool)
        .await
        .unwrap();
    let (s, mr) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-reservations/{mrid}/expire"),
        json!({}),
        Some(ver(&mr)),
    )
    .await;
    assert_eq!(
        (s, mr["status"].as_str()),
        (StatusCode::OK, Some("expired")),
        "{mr}"
    );
    let (_, copies) = read(&app, &admin, &format!("{C}/catalogue-records/{rid}/copies")).await;
    assert!(
        copies["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == c1id.as_str() && c["status"] == "available"),
        "no queue left"
    );
    let (_, ld3) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": did, "copyId": c1id}),
        None,
    )
    .await;
    let (s, mr2) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/reservations"),
        json!({"recordId": rid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{mr2} {ld3}");
    let (s, mr2) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/reservations/{}/cancel", sid(&mr2)),
        json!({}),
        Some(ver(&mr2)),
    )
    .await;
    assert_eq!(
        (s, mr2["status"].as_str()),
        (StatusCode::OK, Some("cancelled"))
    );

    // ================================================================ copy condition + withdrawal
    let (_, cf) = read(
        &app,
        &admin,
        &format!("{C}/catalogue-records/{fluid}/copies"),
    )
    .await;
    let cfv = cf["items"][0].clone();
    assert_eq!(cfv["status"], "available");
    let (s, cfv) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-copies/{cfid}/condition"),
        json!({"condition": "damaged", "reason": "Water damage"}),
        Some(ver(&cfv)),
    )
    .await;
    assert_eq!(
        (s, cfv["status"].as_str()),
        (StatusCode::OK, Some("damaged")),
        "{cfv}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &loan_path,
        json!({"membershipId": aid, "copyId": cfid}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a damaged copy does not circulate");
    let (s, cfv) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-copies/{cfid}/condition"),
        json!({"condition": "fair"}),
        Some(ver(&cfv)),
    )
    .await;
    assert_eq!(
        (s, cfv["status"].as_str()),
        (StatusCode::OK, Some("available"))
    );
    let (s, cfv) = call(
        &app,
        &admin,
        "POST",
        &format!("{C}/library-copies/{cfid}/withdraw"),
        json!({"reason": "Old edition"}),
        Some(ver(&cfv)),
    )
    .await;
    assert_eq!(
        (s, cfv["status"].as_str()),
        (StatusCode::OK, Some("withdrawn"))
    );

    // ================================================================ /me/library catalogue + complaint
    let (s, cat) = read(&app, &aarav, &format!("{C}/me/library/catalogue?q=thermo")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(cat["items"].as_array().unwrap().len(), 1);
    assert_eq!(cat["items"][0]["title"], "Thermodynamics");
    let (s, t) = call(
        &app,
        &aarav,
        "POST",
        &format!("{C}/me/library/complaints"),
        json!({"title": "Reading room AC not working", "copyId": c1id}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{t}");
    assert!(t["number"].as_str().unwrap().starts_with("SR-"));
    let (source, category, node): (String, String, Option<Uuid>) = sqlx::query_as(
        "SELECT source, category, node_id FROM campus.service_ticket WHERE id = $1::uuid",
    )
    .bind(t["ticketId"].as_str().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        (source.as_str(), category.as_str(), node),
        ("library", "library", Some(h.spaces[1].id.as_uuid()))
    );
    let (s, _) = read(&app, &aarav, &format!("{C}/me/library/fines")).await;
    assert_eq!(s, StatusCode::OK);
    let (s, loans) = read(&app, &admin, &format!("{loan_path}?limit=2")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(loans["items"].as_array().unwrap().len(), 2);
    let cursor = loans["nextCursor"].as_str().unwrap().to_owned();
    let (_, next) = read(
        &app,
        &admin,
        &format!("{loan_path}?limit=2&cursor={cursor}"),
    )
    .await;
    assert!(!next["items"].as_array().unwrap().is_empty());

    // ================================================================ evidence
    assert!(
        count(
            &pool,
            "SELECT count(*) FROM campus.status_history WHERE institution_id = $1::uuid AND aggregate_type LIKE 'library%'",
            &inst.to_string()
        )
        .await
            >= 20
    );
    assert!(
        count(
            &pool,
            "SELECT count(*) FROM platform.audit_log WHERE action LIKE 'campus.library_%' AND $1 <> ''",
            "x"
        )
        .await
            >= 20
    );
    let delete = sqlx::query("DELETE FROM campus.library_loan WHERE id = $1::uuid")
        .bind(&l1id)
        .execute(&pool)
        .await;
    assert!(delete.is_err(), "loans are never deleted");
}

// ==================================================================== AwaitDecision (campus)

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

async fn hit(
    r: &axum::Router,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("idempotency-key", Uuid::now_v7().to_string());
    if let Some(v) = if_match {
        b = b.header("if-match", format!("\"{v}\""));
    }
    let (s, _, v) = send(r, b.body(Body::from(body.to_string())).unwrap()).await;
    (s, v)
}

async fn look(r: &axum::Router, path: &str) -> (StatusCode, Value) {
    let (s, _, v) = send(r, Request::get(path).body(Body::empty()).unwrap()).await;
    (s, v)
}

#[tokio::test]
async fn library_await_decision_policy_fine_waiver_and_clearance() {
    let (app, pool) = app(TENANT_AWAIT).await;
    let admin_login = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin_login, &pool, TENANT_AWAIT, "Await Library").await;
    let t = setup.tenant();
    let inst = setup.institution().as_uuid();
    let day = today(&pool, inst).await;
    let mut st = sutra_api::app::campus_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = sutra_campus::shared::approval::ApprovalMode::AwaitDecision;
    let (router, _) = sutra_campus::router_with_state(st).split_for_parts();
    let admin = router.layer(axum::Extension(auth(ADMIN, Role::Administrator, t)));
    let student = b8::seed_student(&pool, &setup, "Ishaan", None).await;

    // ---- policy approval stays pending until decided.
    let (s, p) = hit(
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-policies"),
        policy_body("Awaited", "3.00"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    let pid = sid(&p);
    let (s, p) = hit(
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/submit"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(
        (s, p["approvalState"].as_str()),
        (StatusCode::OK, Some("pending")),
        "{p}"
    );
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/activate"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not approved yet");
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/decision"),
        json!({"decision": "reject"}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejection needs a reason"
    );
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&p) - 1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, p) = hit(
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(
        (s, p["approvalState"].as_str()),
        (StatusCode::OK, Some("approved"))
    );
    let (s, p) = hit(
        &admin,
        "POST",
        &format!("{C}/library-policies/{pid}/activate"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!((s, p["status"].as_str()), (StatusCode::OK, Some("active")));

    // ---- two late returns → two fines.
    let (_, m) = hit(
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-memberships"),
        json!({"personKind": "student", "personId": student.student_id}),
        None,
    )
    .await;
    let mid = sid(&m);
    let mut fines = Vec::new();
    let mut lids = Vec::new();
    for title in ["Optics", "Acoustics"] {
        let (_, r) = hit(
            &admin,
            "POST",
            &format!("{C}/institutions/{inst}/catalogue"),
            json!({"title": title}),
            None,
        )
        .await;
        let rid = sid(&r);
        let (_, _) = hit(
            &admin,
            "POST",
            &format!("{C}/catalogue-records/{rid}/publish"),
            json!({}),
            Some(ver(&r)),
        )
        .await;
        let (_, c) = hit(
            &admin,
            "POST",
            &format!("{C}/catalogue-records/{rid}/copies"),
            json!({}),
            None,
        )
        .await;
        let (s, l) = hit(
            &admin,
            "POST",
            &format!("{C}/institutions/{inst}/library-loans"),
            json!({"membershipId": mid, "copyId": sid(&c)}),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{l}");
        lids.push(sid(&l));
    }
    // Both loans are issued before either is returned late: a late return leaves an
    // unsettled fine, which (correctly) blocks any further issue to the membership.
    for lid in lids {
        backdate_loan(&pool, &lid, day, 2).await;
        let (_, l) = look(&admin, &format!("{C}/library-loans/{lid}")).await;
        let (s, l) = hit(
            &admin,
            "POST",
            &format!("{C}/library-loans/{lid}/return"),
            json!({}),
            Some(ver(&l)),
        )
        .await;
        assert_eq!(
            (s, l["fineAmount"].as_str()),
            (StatusCode::OK, Some("6.00")),
            "{l}"
        );
    }
    let (_, list) = look(
        &admin,
        &format!("{C}/institutions/{inst}/library-fines?membershipId={mid}"),
    )
    .await;
    for f in list["items"].as_array().unwrap() {
        fines.push(f.clone());
    }
    assert_eq!(fines.len(), 2);

    // ---- waiver awaits the decision; a stale ETag is refused; approve and reject.
    let f1 = fines[0].clone();
    let (s, f1) = hit(
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/waiver", sid(&f1)),
        json!({"reason": "Medical leave"}),
        Some(ver(&f1)),
    )
    .await;
    assert_eq!(
        (s, f1["status"].as_str()),
        (StatusCode::OK, Some("waiver_requested")),
        "{f1}"
    );
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/bill", sid(&f1)),
        json!({}),
        Some(ver(&f1)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a fine awaiting a waiver is not billed"
    );
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/decision", sid(&f1)),
        json!({"decision": "approve"}),
        Some(ver(&f1) - 1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, f1) = hit(
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/decision", sid(&f1)),
        json!({"decision": "approve", "reason": "Certificate seen"}),
        Some(ver(&f1)),
    )
    .await;
    assert_eq!((s, f1["status"].as_str()), (StatusCode::OK, Some("waived")));
    assert_eq!(
        events(&pool, "sutraerp.campus.library_fine.waived", &sid(&f1)).await,
        1
    );
    let f2 = fines[1].clone();
    let (_, f2) = hit(
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/waiver", sid(&f2)),
        json!({"reason": "Forgot"}),
        Some(ver(&f2)),
    )
    .await;
    let (s, f2) = hit(
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/decision", sid(&f2)),
        json!({"decision": "reject", "reason": "No ground"}),
        Some(ver(&f2)),
    )
    .await;
    assert_eq!(
        (s, f2["status"].as_str()),
        (StatusCode::OK, Some("assessed"))
    );
    let (s, f2) = hit(
        &admin,
        "POST",
        &format!("{C}/library-fines/{}/bill", sid(&f2)),
        json!({}),
        Some(ver(&f2)),
    )
    .await;
    assert_eq!(
        (s, f2["status"].as_str()),
        (StatusCode::OK, Some("billed")),
        "{f2}"
    );
    settle(&pool, f2["chargeId"].as_str().unwrap()).await;

    // ---- clearance without blockers still awaits the approver.
    let (s, k) = hit(
        &admin,
        "POST",
        &format!("{C}/institutions/{inst}/library-clearances"),
        json!({"membershipId": mid, "purpose": "Exit"}),
        None,
    )
    .await;
    assert_eq!(
        (s, k["status"].as_str()),
        (StatusCode::CREATED, Some("requested")),
        "{k}"
    );
    assert!(
        k["verification"]["blockers"].as_array().unwrap().is_empty(),
        "{k}"
    );
    let (s, k) = hit(
        &admin,
        "POST",
        &format!("{C}/library-clearances/{}/decision", sid(&k)),
        json!({"decision": "approve"}),
        Some(ver(&k)),
    )
    .await;
    assert_eq!(
        (s, k["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{k}"
    );
}

// ==================================================================== reports & analytics

#[tokio::test]
async fn reports_dashboards_widgets_f076_and_regulatory_cycle() {
    let (app, pool) = app(TENANT_REPORTS).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b8::campus_setup(&app, &admin, &pool, TENANT_REPORTS, "Reports University").await;
    let inst = setup.institution().as_uuid();
    b8::seed_hierarchy(
        &pool,
        &setup,
        "RPT",
        &[
            ("101", SpaceType::Classroom, 60),
            ("L1", SpaceType::Library, 80),
        ],
    )
    .await;
    b8::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;

    // ================================================================ H-054 widgets
    let wpath = format!("{S}/institutions/{inst}/widgets");
    let mut widgets = std::collections::HashMap::new();
    for (code, key) in [
        ("FIN-REC", "finance.receivables_outstanding"),
        ("FIN-OPEN", "finance.open_charges"),
        ("SPACES", "campus.active_spaces"),
        ("STUDENTS", "student.active_students"),
    ] {
        let (s, w) = call(
            &app,
            &admin,
            "POST",
            &wpath,
            json!({"code": code, "title": code, "metricKey": key}),
            None,
        )
        .await;
        assert_eq!(
            (s, w["status"].as_str()),
            (StatusCode::CREATED, Some("draft")),
            "{w}"
        );
        let wid = sid(&w);
        let (s, w) = call(
            &app,
            &admin,
            "POST",
            &format!("{S}/widgets/{wid}/activate"),
            json!({}),
            Some(ver(&w)),
        )
        .await;
        assert_eq!((s, w["status"].as_str()), (StatusCode::OK, Some("active")));
        assert_eq!(
            events(&pool, "sutraerp.services.widget_definition.activated", &wid).await,
            1
        );
        widgets.insert(code, wid);
    }
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &wpath,
        json!({"code": "NOPE", "title": "x", "metricKey": "campus.nope"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown figure");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &wpath,
        json!({"code": "SPACES", "title": "x", "metricKey": "campus.active_spaces"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate code");
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &wpath,
        json!({"code": "X1", "title": "x", "metricKey": "campus.active_spaces"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ================================================================ H-048 / F-076 finance KPI dashboard
    let (s, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/institutions/{inst}/dashboards"),
        json!({"code": "FIN-KPI",
        "name": "Finance KPIs", "purpose": "Receivables for the bursar", "category": "finance"}),
        None,
    )
    .await;
    assert_eq!(
        (s, d["status"].as_str()),
        (StatusCode::CREATED, Some("draft")),
        "{d}"
    );
    let did = sid(&d);
    let (s, bad) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/versions"),
        json!({"widgetIds": [widgets["SPACES"]]}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "finance dashboards bind only finance.* figures: {bad}"
    );
    let (s, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/versions"),
        json!({"widgetIds": [widgets["FIN-REC"]], "changeNote": "v1"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{d}");
    assert_eq!(d["versions"][0]["status"], "draft");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/publish"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing approved yet");
    let (s, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/submit"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!(
        (d["status"].as_str(), d["versions"][0]["status"].as_str()),
        (Some("in_review"), Some("approved")),
        "auto-approval"
    );
    let (s, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/publish"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(
        (s, d["status"].as_str(), d["currentVersionNo"].as_i64()),
        (StatusCode::OK, Some("published"), Some(1)),
        "{d}"
    );
    assert_eq!(
        events(&pool, "sutraerp.services.dashboard.published", &did).await,
        1
    );
    let (s, view) = read(&app, &admin, &format!("{S}/dashboards/{did}/view")).await;
    assert_eq!(s, StatusCode::OK, "{view}");
    assert_eq!(
        view["widgets"][0]["metric"]["key"],
        "finance.receivables_outstanding"
    );
    assert!(view["widgets"][0]["metric"]["value"].is_string());
    // H-052: v2 → published, v1 superseded and kept; rollback re-publishes v1.
    let (_, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/versions"),
        json!({"widgetIds": [widgets["FIN-REC"], widgets["FIN-OPEN"]]}),
        None,
    )
    .await;
    let (_, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/submit"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    let (s, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/publish"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(
        (s, d["currentVersionNo"].as_i64()),
        (StatusCode::OK, Some(2)),
        "{d}"
    );
    let statuses: Vec<(i64, String)> = d["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| {
            (
                v["versionNo"].as_i64().unwrap(),
                v["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        vec![(2, "published".to_owned()), (1, "superseded".to_owned())]
    );
    assert_eq!(
        d["versions"][1]["widgets"].as_array().unwrap().len(),
        1,
        "v1 layout preserved"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/rollback"),
        json!({"versionNo": 1}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, d) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/rollback"),
        json!({"versionNo": 1, "reason": "v2 figure disputed"}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(
        (s, d["currentVersionNo"].as_i64()),
        (StatusCode::OK, Some(1)),
        "{d}"
    );
    assert_eq!(
        events(&pool, "sutraerp.services.dashboard.rolled_back", &did).await,
        1
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/widgets/{}/retire", widgets["FIN-REC"]),
        json!({"reason": "x"}),
        Some(2),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "shown on a published dashboard");

    // ================================================================ H-050 assignments → /me/dashboards
    let (s, a) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/assignments"),
        json!({"role": "administrator"}),
        None,
    )
    .await;
    assert_eq!(
        (s, a["status"].as_str()),
        (StatusCode::CREATED, Some("active")),
        "{a}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/assignments"),
        json!({"role": "administrator"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/assignments"),
        json!({"role": "janitor"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, mine) = read(&app, &admin, &format!("{S}/me/dashboards")).await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert!(mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["code"] == "FIN-KPI"));
    let (_, theirs) = read(&app, &aarav, &format!("{S}/me/dashboards")).await;
    assert!(theirs["items"].as_array().unwrap().is_empty());
    let (_, sa) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/assignments"),
        json!({"role": "student"}),
        None,
    )
    .await;
    let (_, theirs) = read(&app, &aarav, &format!("{S}/me/dashboards")).await;
    assert_eq!(theirs["items"].as_array().unwrap().len(), 1);
    let (s, sa) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/assignments/{}/revoke", sid(&sa)),
        json!({}),
        Some(ver(&sa)),
    )
    .await;
    assert_eq!(
        (s, sa["status"].as_str()),
        (StatusCode::OK, Some("revoked"))
    );
    let (_, theirs) = read(&app, &aarav, &format!("{S}/me/dashboards")).await;
    assert!(theirs["items"].as_array().unwrap().is_empty());

    // ================================================================ G-154 / G-320 operational reports
    let (s, lib) = read(
        &app,
        &admin,
        &format!("{S}/institutions/{inst}/reports/library"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{lib}");
    let keys: Vec<&str> = lib["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["key"].as_str().unwrap())
        .collect();
    assert!(
        keys.contains(&"campus.library_loans_active")
            && keys.contains(&"campus.library_fines_unsettled"),
        "{keys:?}"
    );
    assert!(keys.iter().all(|k| k.starts_with("campus.library_")));
    let (s, infra) = read(
        &app,
        &admin,
        &format!("{S}/institutions/{inst}/reports/infrastructure"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(infra["items"].as_array().unwrap().len(), 5);
    assert_eq!(infra["items"][1]["key"], "campus.active_spaces");
    assert_eq!(infra["items"][1]["value"], "2");

    // ================================================================ H-055 frameworks
    let fpath = format!("{S}/institutions/{inst}/reporting-frameworks");
    let (s, _) = call(&app, &admin, "POST", &fpath, json!({"code": "AISHE", "name": "AISHE", "regulator": "MoE", "metricKeys": ["people.nope"]}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, f1) = call(
        &app,
        &admin,
        "POST",
        &fpath,
        json!({"code": "AISHE", "name": "AISHE DCF", "regulator": "MoE",
        "metricKeys": ["student.active_students"]}),
        None,
    )
    .await;
    assert_eq!(
        (s, f1["versionNo"].as_i64()),
        (StatusCode::CREATED, Some(1)),
        "{f1}"
    );
    let f1id = sid(&f1);
    let (_, f1) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{f1id}/submit"),
        json!({}),
        Some(ver(&f1)),
    )
    .await;
    assert_eq!(f1["approvalState"], "approved");
    let (s, f1) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{f1id}/activate"),
        json!({}),
        Some(ver(&f1)),
    )
    .await;
    assert_eq!((s, f1["status"].as_str()), (StatusCode::OK, Some("active")));
    let (_, f2) = call(
        &app,
        &admin,
        "POST",
        &fpath,
        json!({"code": "AISHE", "name": "AISHE DCF 2026", "regulator": "MoE",
        "metricKeys": ["student.active_students", "people.teaching_staff"],
        "manualFields": [{"key": "naac_grade", "label": "NAAC grade", "required": true}]}),
        None,
    )
    .await;
    assert_eq!(
        (f2["versionNo"].as_i64(), f2["predecessorId"].as_str()),
        (Some(2), Some(f1id.as_str()))
    );
    let f2id = sid(&f2);
    let (_, f2) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{f2id}/submit"),
        json!({}),
        Some(ver(&f2)),
    )
    .await;
    let (s, f2) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{f2id}/activate"),
        json!({}),
        Some(ver(&f2)),
    )
    .await;
    assert_eq!((s, f2["status"].as_str()), (StatusCode::OK, Some("active")));
    assert_eq!(
        events(
            &pool,
            "sutraerp.services.reporting_framework.activated",
            &f2id
        )
        .await,
        1
    );
    let (_, f1) = read(&app, &admin, &format!("{S}/reporting-frameworks/{f1id}")).await;
    assert_eq!(f1["status"], "superseded");

    // ================================================================ H-056 cycle
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{f1id}/cycles"),
        json!({"periodLabel": "2025-26"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "cycles open on the active version");
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{f2id}/cycles"),
        json!({"periodLabel": "2025-26", "dueOn": "2026-12-31"}),
        None,
    )
    .await;
    assert_eq!(
        (s, c["status"].as_str()),
        (StatusCode::CREATED, Some("planned")),
        "{c}"
    );
    assert!(c["number"].as_str().unwrap().starts_with("RC-"));
    assert_eq!(
        c["collectionPlan"]["metricKeys"].as_array().unwrap().len(),
        2
    );
    let cid = sid(&c);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{f2id}/cycles"),
        json!({"periodLabel": "2025-26"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one cycle per period");

    // ================================================================ H-057 snapshot: issues → reject → validated
    let (s, s1) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/collect"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{s1}");
    assert_eq!(s1["issues"][0]["key"], "naac_grade");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/report-snapshots/{}/validate", sid(&s1)),
        json!({}),
        Some(ver(&s1)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a snapshot with issues is not validated"
    );
    let (s, s1) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/report-snapshots/{}/reject", sid(&s1)),
        json!({"reason": "grade missing"}),
        Some(ver(&s1)),
    )
    .await;
    assert_eq!(
        (s, s1["status"].as_str()),
        (StatusCode::OK, Some("rejected"))
    );
    let (_, cyc) = read(&app, &admin, &format!("{S}/reporting-cycles/{cid}")).await;
    assert_eq!(cyc["cycle"]["status"], "collecting");
    let (s, s2) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/collect"),
        json!({"manualValues": {"naac_grade": "A+"}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{s2}");
    assert!(s2["issues"].as_array().unwrap().is_empty(), "{s2}");
    assert_eq!(s2["seq"], 2);
    let students = s2["values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "student.active_students")
        .unwrap();
    assert_eq!(students["source"], "metric");
    assert!(students["value"].is_string(), "{students}");
    let grade = s2["values"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["key"] == "naac_grade")
        .unwrap();
    assert_eq!(grade["valueText"], "A+");
    let s2id = sid(&s2);
    let (s, s2) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/report-snapshots/{s2id}/validate"),
        json!({}),
        Some(ver(&s2)),
    )
    .await;
    assert_eq!(
        (s, s2["status"].as_str()),
        (StatusCode::OK, Some("validated"))
    );
    assert_eq!(
        events(&pool, "sutraerp.services.report_snapshot.validated", &s2id).await,
        1
    );
    let frozen = sqlx::query(
        "UPDATE services.report_snapshot_value SET value_text = 'B' WHERE snapshot_id = $1::uuid",
    )
    .bind(&s2id)
    .execute(&pool)
    .await;
    assert!(frozen.is_err(), "snapshot values are frozen");

    // ================================================================ H-058 / 059 package → approved → submitted → acknowledged
    let (s, pkg) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/packages"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, pkg["status"].as_str()),
        (StatusCode::CREATED, Some("draft")),
        "{pkg}"
    );
    assert!(pkg["number"].as_str().unwrap().starts_with("SUB-"));
    let pk1 = sid(&pkg);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/close"),
        json!({}),
        Some(ver(&cyc["cycle"]) + 3),
    )
    .await;
    assert!(
        s == StatusCode::CONFLICT || s == StatusCode::PRECONDITION_FAILED,
        "{s}"
    );
    let (s, pkg) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk1}/submit"),
        json!({}),
        Some(ver(&pkg)),
    )
    .await;
    assert_eq!(
        (s, pkg["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{pkg}"
    );
    assert_eq!(
        events(&pool, "sutraerp.services.submission_package.approved", &pk1).await,
        1
    );
    let (s, pkg) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk1}/submit"),
        json!({}),
        Some(ver(&pkg)),
    )
    .await;
    assert_eq!(
        (s, pkg["status"].as_str()),
        (StatusCode::OK, Some("submitted")),
        "{pkg}"
    );
    assert!(pkg["submittedAt"].is_string());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk1}/acknowledge"),
        json!({"reference": " "}),
        Some(ver(&pkg)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, pkg) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk1}/acknowledge"),
        json!({"reference": "AISHE/2026/0042"}),
        Some(ver(&pkg)),
    )
    .await;
    assert_eq!(
        (s, pkg["acknowledgementRef"].as_str()),
        (StatusCode::OK, Some("AISHE/2026/0042")),
        "{pkg}"
    );
    assert_eq!(
        events(
            &pool,
            "sutraerp.services.submission_package.acknowledged",
            &pk1
        )
        .await,
        1
    );

    // ================================================================ H-060 correction → corrected package → closure
    let (s, pkg) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk1}/correct"),
        json!({"reason": "Student count revised"}),
        Some(ver(&pkg)),
    )
    .await;
    assert_eq!(
        (s, pkg["status"].as_str()),
        (StatusCode::OK, Some("corrected")),
        "{pkg}"
    );
    let (_, s3) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/collect"),
        json!({"manualValues": {"naac_grade": "A++"}}),
        None,
    )
    .await;
    let (_, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/report-snapshots/{}/validate", sid(&s3)),
        json!({}),
        Some(ver(&s3)),
    )
    .await;
    let (s, pk2) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/packages"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(
        (s, pk2["correctedFromId"].as_str()),
        (StatusCode::CREATED, Some(pk1.as_str())),
        "{pk2}"
    );
    let pk2id = sid(&pk2);
    let (_, pk2) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk2id}/submit"),
        json!({}),
        Some(ver(&pk2)),
    )
    .await;
    let (_, pk2) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk2id}/submit"),
        json!({}),
        Some(ver(&pk2)),
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pk2id}/acknowledge"),
        json!({"reference": "AISHE/2026/0042-R1"}),
        Some(ver(&pk2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, cyc) = read(&app, &admin, &format!("{S}/reporting-cycles/{cid}")).await;
    assert_eq!(cyc["cycle"]["status"], "submitted");
    assert_eq!(cyc["packages"].as_array().unwrap().len(), 2);
    let (s, cyc) = call(
        &app,
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/close"),
        json!({}),
        Some(ver(&cyc["cycle"])),
    )
    .await;
    assert_eq!(
        (s, cyc["cycle"]["status"].as_str()),
        (StatusCode::OK, Some("closed")),
        "{cyc}"
    );
    assert_eq!(
        events(&pool, "sutraerp.services.reporting_cycle.closed", &cid).await,
        1
    );
    assert!(
        count(
            &pool,
            "SELECT count(*) FROM services.status_history WHERE institution_id = $1::uuid",
            &inst.to_string()
        )
        .await
            >= 20
    );
}

#[tokio::test]
async fn reports_await_decision_dashboard_and_submission_approval() {
    let (app, pool) = app(TENANT_REPORTS_AWAIT).await;
    let admin_login = login(&app, "admin@sutra.dev").await;
    let setup = b8::campus_setup(
        &app,
        &admin_login,
        &pool,
        TENANT_REPORTS_AWAIT,
        "Await Reports",
    )
    .await;
    let t = setup.tenant();
    let inst = setup.institution().as_uuid();
    let mut st = sutra_api::app::services_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = sutra_services::shared::approval::ApprovalMode::AwaitDecision;
    let (router, _) = sutra_services::router_with_state(st).split_for_parts();
    let admin = router.layer(axum::Extension(auth(ADMIN, Role::Administrator, t)));

    // ---- dashboard publication awaits the decision.
    let (_, w) = hit(
        &admin,
        "POST",
        &format!("{S}/institutions/{inst}/widgets"),
        json!({"code": "STAFF", "title": "Staff", "metricKey": "people.working_staff"}),
        None,
    )
    .await;
    let (s, w) = hit(
        &admin,
        "POST",
        &format!("{S}/widgets/{}/activate", sid(&w)),
        json!({}),
        Some(ver(&w)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{w}");
    let (_, d) = hit(
        &admin,
        "POST",
        &format!("{S}/institutions/{inst}/dashboards"),
        json!({"code": "OPS", "name": "Operations", "purpose": "Staffing"}),
        None,
    )
    .await;
    let did = sid(&d);
    let (_, d) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/versions"),
        json!({"widgetIds": [sid(&w)]}),
        None,
    )
    .await;
    let (s, d) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/submit"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(
        (s, d["versions"][0]["status"].as_str()),
        (StatusCode::OK, Some("in_review")),
        "{d}"
    );
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/publish"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "not approved yet");
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/decision"),
        json!({"decision": "reject"}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, d) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/decision"),
        json!({"decision": "reject", "reason": "Add a target"}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(
        (s, d["status"].as_str(), d["versions"][0]["status"].as_str()),
        (StatusCode::OK, Some("draft"), Some("rejected")),
        "{d}"
    );
    let (_, d) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/versions"),
        json!({"widgetIds": [sid(&w)], "changeNote": "with target"}),
        None,
    )
    .await;
    assert_eq!(d["versions"][0]["versionNo"], 2);
    let (_, d) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/submit"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&d) - 1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, d) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(
        (s, d["versions"][0]["status"].as_str()),
        (StatusCode::OK, Some("approved"))
    );
    let (s, d) = hit(
        &admin,
        "POST",
        &format!("{S}/dashboards/{did}/publish"),
        json!({}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(
        (s, d["status"].as_str()),
        (StatusCode::OK, Some("published")),
        "{d}"
    );

    // ---- framework + submission approval await their decisions.
    let (_, f) = hit(
        &admin,
        "POST",
        &format!("{S}/institutions/{inst}/reporting-frameworks"),
        json!({"code": "NIRF", "name": "NIRF data", "regulator": "NBA",
        "metricKeys": ["people.teaching_staff"]}),
        None,
    )
    .await;
    let fid = sid(&f);
    let (s, f) = hit(
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{fid}/submit"),
        json!({}),
        Some(ver(&f)),
    )
    .await;
    assert_eq!(
        (s, f["approvalState"].as_str()),
        (StatusCode::OK, Some("pending")),
        "{f}"
    );
    let (s, f) = hit(
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{fid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&f)),
    )
    .await;
    assert_eq!(
        (s, f["approvalState"].as_str()),
        (StatusCode::OK, Some("approved"))
    );
    let (_, _) = hit(
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{fid}/activate"),
        json!({}),
        Some(ver(&f)),
    )
    .await;
    let (_, c) = hit(
        &admin,
        "POST",
        &format!("{S}/reporting-frameworks/{fid}/cycles"),
        json!({"periodLabel": "2026"}),
        None,
    )
    .await;
    let cid = sid(&c);
    let (_, snap) = hit(
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/collect"),
        json!({}),
        None,
    )
    .await;
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{S}/report-snapshots/{}/validate", sid(&snap)),
        json!({}),
        Some(ver(&snap)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, p) = hit(
        &admin,
        "POST",
        &format!("{S}/reporting-cycles/{cid}/packages"),
        json!({}),
        None,
    )
    .await;
    let pid = sid(&p);
    let (s, p) = hit(
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pid}/submit"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(
        (s, p["status"].as_str()),
        (StatusCode::OK, Some("pending_approval")),
        "{p}"
    );
    let (s, _) = hit(
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pid}/submit"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "awaiting approval");
    let (s, p) = hit(
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pid}/decision"),
        json!({"decision": "reject", "reason": "Recheck faculty count"}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!((s, p["status"].as_str()), (StatusCode::OK, Some("draft")));
    let (_, p) = hit(
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pid}/submit"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    let (s, p) = hit(
        &admin,
        "POST",
        &format!("{S}/submission-packages/{pid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(
        (s, p["status"].as_str()),
        (StatusCode::OK, Some("approved")),
        "{p}"
    );
    assert_eq!(
        events(&pool, "sutraerp.services.submission_package.approved", &pid).await,
        1
    );
}
