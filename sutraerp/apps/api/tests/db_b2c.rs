//! Slice 2C DB tests (owned by 2C). Run against this slice's own database:
//! `DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra_2c cargo test -p sutra-api --features db-tests --test db_b2c`.
//!
//! Academic calendar (L3/L4-B-027 … B-033) end to end through the HTTP API on PostgreSQL as the
//! runtime role: transitions (incl. illegal ones), date-range validation and overlaps, the
//! business timezone, side-effect-free simulation, tenant isolation, permissions, `ETag` /
//! `If-Match`, idempotency, audit and outbox, plus the activation-readiness endpoint.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_arguments)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use support::*;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b2c01";
const OTHER_TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b2c02";
const B: &str = "/api/v1/institution";

fn etag(h: &axum::http::HeaderMap) -> i64 {
    h["etag"]
        .to_str()
        .unwrap()
        .trim_matches('"')
        .parse()
        .unwrap()
}

async fn register_in_zone(app: &Router, who: &Login, name: &str, tz: &str) -> String {
    let (s, _, inst) = send(
        app,
        write(
            who,
            "POST",
            INSTITUTIONS,
            json!({"code": code("CAL"), "name": name, "timezone": tz}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inst}");
    inst["id"].as_str().unwrap().to_owned()
}

/// POST/PATCH/DELETE asserting the status; returns (body, etag).
async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> (Value, i64) {
    let (s, h, b) = send(app, write(who, method, path, body, if_match)).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    let e = if h.contains_key("etag") { etag(&h) } else { 0 };
    (b, e)
}

async fn read(app: &Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

/// Calendar + planned year + draft version (no terms yet). Returns (calendar, year, version, version etag).
async fn calendar_with_draft(
    app: &Router,
    admin: &Login,
    inst: &str,
    start: &str,
    end: &str,
) -> (String, String, String, i64) {
    let (cal, _) = call(
        app,
        admin,
        "POST",
        &format!("{B}/institutions/{inst}/calendars"),
        json!({"code": code("C"), "name": "Main calendar"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let cal = cal["id"].as_str().unwrap().to_owned();
    let (year, _) = call(
        app,
        admin,
        "POST",
        &format!("{B}/institutions/{inst}/academic-years"),
        json!({"calendarId": cal, "label": "2026-27", "startsOn": start, "endsOn": end}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(year["status"], "planned");
    let year = year["id"].as_str().unwrap().to_owned();
    let (v, e) = call(
        app,
        admin,
        "POST",
        &format!("{B}/calendars/{cal}/versions"),
        json!({"academicYearId": year}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(v["status"], "draft");
    assert_eq!(v["versionNo"], 1);
    (cal, year, v["id"].as_str().unwrap().to_owned(), e)
}

async fn add_term(
    app: &Router,
    admin: &Login,
    v: &str,
    e: i64,
    code: &str,
    seq: i32,
    s: &str,
    end: &str,
) -> i64 {
    let (_, e) = call(
        app,
        admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/terms"),
        json!({"code": code, "name": format!("Term {seq}"), "sequence": seq, "startsOn": s, "endsOn": end}),
        Some(e),
        StatusCode::CREATED,
    )
    .await;
    e
}

async fn outbox_types(pool: &sqlx::PgPool, subject: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT event_type FROM platform.outbox WHERE subject = $1 ORDER BY created_at, id",
    )
    .bind(subject)
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn audit_actions(pool: &sqlx::PgPool, target: &str) -> Vec<String> {
    sqlx::query_scalar(
        "SELECT action FROM platform.audit_log WHERE target_id = $1 ORDER BY occurred_at, id",
    )
    .bind(target)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn readiness_item<'a>(r: &'a Value, key: &str) -> &'a Value {
    r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == key)
        .unwrap_or_else(|| panic!("{key} missing in {r}"))
}

/// L3-B-027 + B-028 + readiness: establish → publish → open → assign → readiness met.
#[tokio::test]
async fn establishment_publication_open_context_assignment_and_readiness() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_in_zone(&app, &admin, "Calendar College", "Asia/Kolkata").await;

    // Readiness before: every 2C key present and unmet.
    let (s, r) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/activation-readiness"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(r["institutionStatus"], "draft");
    assert_eq!(r["ready"], false);
    for k in [
        "calendar.version_published",
        "calendar.academic_year_open",
        "calendar.assignment_effective",
    ] {
        let i = readiness_item(&r, k);
        assert_eq!(i["status"], "unmet", "{k}");
        assert_eq!(i["area"], "calendar");
        assert_eq!(i["link"], "/academics/calendar");
    }

    let (cal, year, v, e) =
        calendar_with_draft(&app, &admin, &inst, "2026-06-01", "2027-05-31").await;
    // Publication needs a valid definition (BH-002): no terms → 422, nothing changes.
    let (b, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/publish"),
        json!({}),
        Some(e),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert!(b.to_string().contains("terms"), "{b}");
    // Opening a year without a published version is refused (BH-001).
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/academic-years/{year}/open"),
        json!({}),
        Some(1),
        StatusCode::CONFLICT,
    )
    .await;

    let e = add_term(&app, &admin, &v, e, "T1", 1, "2026-06-15", "2026-11-30").await;
    let e = add_term(&app, &admin, &v, e, "T2", 2, "2026-12-01", "2027-04-30").await;
    let (kd, e) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/key-dates"),
        json!({"kind": "holiday", "name": "Independence Day", "startsOn": "2026-08-15"}),
        Some(e),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(kd["keyDates"][0]["endsOn"], "2026-08-15");
    assert_eq!(kd["terms"].as_array().unwrap().len(), 2);

    let (p, pe) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/publish"),
        json!({}),
        Some(e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "published");
    assert_eq!(
        p["timezone"], "Asia/Kolkata",
        "zone snapshotted at publication"
    );
    assert!(
        p["processInstanceId"]
            .as_str()
            .unwrap()
            .starts_with("noop-"),
        "{p}"
    );
    assert_eq!(pe, p["version"].as_i64().unwrap());
    // Published is immutable: no second publish, no edit, no term (L4-B-029-BH-001).
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/publish"),
        json!({}),
        Some(pe),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "PATCH",
        &format!("{B}/calendar-versions/{v}"),
        json!({"notes": "x"}),
        Some(pe),
        StatusCode::CONFLICT,
    )
    .await;
    call(&app, &admin, "POST", &format!("{B}/calendar-versions/{v}/terms"), json!({"code": "T3", "name": "T3", "sequence": 3, "startsOn": "2027-05-01", "endsOn": "2027-05-20"}), Some(pe), StatusCode::CONFLICT).await;
    let (s, c) = read(&app, &admin, &format!("{B}/calendars/{cal}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(c["status"], "published");

    // A planned year with a published version already satisfies the year check.
    let (_, r) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/activation-readiness"),
    )
    .await;
    assert_eq!(
        readiness_item(&r, "calendar.version_published")["status"],
        "met"
    );
    assert_eq!(
        readiness_item(&r, "calendar.academic_year_open")["status"],
        "met"
    );
    assert_eq!(
        readiness_item(&r, "calendar.assignment_effective")["status"],
        "unmet"
    );

    // Open the academic context; opening twice is an illegal transition.
    let (y, ye) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/academic-years/{year}/open"),
        json!({}),
        Some(1),
        StatusCode::OK,
    )
    .await;
    assert_eq!(y["status"], "open");
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/academic-years/{year}/open"),
        json!({}),
        Some(ye),
        StatusCode::CONFLICT,
    )
    .await;
    // Activation does not create assignments (L4-B-027-BH-006).
    let (_, list) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/calendar-assignments"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 0);

    // Assignment (B-028): draft → effective; a draft does not govern.
    let (a, ae) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": v, "scopeKind": "institution", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a["status"], "draft");
    assert_eq!(
        a["scopeId"],
        inst.as_str(),
        "institution scope defaults to the institution"
    );
    assert_eq!(a["governing"], false);
    let aid = a["id"].as_str().unwrap().to_owned();
    let (a, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-assignments/{aid}/activate"),
        json!({}),
        Some(ae),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "effective");
    // Unknown scope types / ids are refused (BH-001/-003).
    call(&app, &admin, "POST", &format!("{B}/calendars/{cal}/assignments"), json!({"calendarVersionId": v, "scopeKind": "institution", "scopeId": uuid::Uuid::now_v7(), "effectiveFrom": "2026-06-01"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": v, "scopeKind": "batch", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // A second, overlapping assignment for the same scope cannot become effective (BH-005).
    let (a2, a2e) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": v, "scopeKind": "institution", "effectiveFrom": "2027-01-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let a2 = a2["id"].as_str().unwrap().to_owned();
    let (b, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-assignments/{a2}/activate"),
        json!({}),
        Some(a2e),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(b["detail"].as_str().unwrap().contains("overlapping"), "{b}");

    let (_, r) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/activation-readiness"),
    )
    .await;
    for k in [
        "calendar.version_published",
        "calendar.academic_year_open",
        "calendar.assignment_effective",
    ] {
        assert_eq!(readiness_item(&r, k)["status"], "met", "{k}: {r}");
    }

    // Resolution: today's governing version for the institution and for a campus (fallback).
    let (s, res) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/calendar-resolution?on=2026-09-01"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{res}");
    assert_eq!(res["calendarVersion"]["id"], v.as_str());
    assert_eq!(res["resolvedVia"], "institution");
    let (_, res) = read(
        &app,
        &admin,
        &format!(
            "{B}/institutions/{inst}/calendar-resolution?scopeKind=campus&scopeId={}&on=2026-09-01",
            uuid::Uuid::now_v7()
        ),
    )
    .await;
    assert_eq!(res["resolvedVia"], "institution");
    let (_, res) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/calendar-resolution?on=2020-01-01"),
    )
    .await;
    assert!(res["calendarVersion"].is_null());

    // Outbox + audit in the same transactions.
    assert_eq!(
        outbox_types(&pool, &v).await,
        vec!["sutraerp.institution.calendar_version.published"]
    );
    assert_eq!(
        outbox_types(&pool, &year).await,
        vec!["sutraerp.institution.academic_year.opened"]
    );
    assert_eq!(
        outbox_types(&pool, &aid).await,
        vec!["sutraerp.institution.calendar_assignment.effective"]
    );
    let actions = audit_actions(&pool, &v).await;
    for a in [
        "institution.calendar_version.create",
        "institution.calendar_version.add_term",
        "institution.calendar_version.submit",
        "institution.calendar_version.publish",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} in {actions:?}");
    }
    let auto: String = sqlx::query_scalar("SELECT actor_id FROM platform.audit_log WHERE target_id = $1 AND action = 'institution.calendar_version.publish'")
        .bind(&v)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(auto, "system:auto-approval");
    let payload: Value =
        sqlx::query_scalar("SELECT envelope -> 'data' FROM platform.outbox WHERE subject = $1")
            .bind(&v)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload["timezone"], "Asia/Kolkata");
    assert_eq!(payload["startsOn"], "2026-06-01");
}

/// Validation of business dates and overlap rules.
#[tokio::test]
async fn invalid_and_overlapping_date_ranges_are_refused() {
    let (app, _pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_in_zone(&app, &admin, "Dates College", "Asia/Kolkata").await;
    let (cal, _year, v, e) =
        calendar_with_draft(&app, &admin, &inst, "2026-06-01", "2027-05-31").await;
    let years = format!("{B}/institutions/{inst}/academic-years");
    // End before start, and a span beyond two years.
    let (b, _) = call(&app, &admin, "POST", &years, json!({"calendarId": cal, "label": "bad", "startsOn": "2027-06-01", "endsOn": "2027-05-01"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(b["validationErrors"][0]["field"], "endsOn", "{b}");
    call(&app, &admin, "POST", &years, json!({"calendarId": cal, "label": "long", "startsOn": "2030-01-01", "endsOn": "2033-01-01"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    // Overlapping another year of the same calendar; duplicate label.
    let (b, _) = call(&app, &admin, "POST", &years, json!({"calendarId": cal, "label": "2027-28", "startsOn": "2027-05-01", "endsOn": "2028-04-30"}), None, StatusCode::CONFLICT).await;
    assert!(b["detail"].as_str().unwrap().contains("2026-27"), "{b}");
    call(&app, &admin, "POST", &years, json!({"calendarId": cal, "label": "2026-27", "startsOn": "2030-06-01", "endsOn": "2031-05-31"}), None, StatusCode::CONFLICT).await;
    // Malformed date → 4xx (JSON rejection), never 500.
    let (s, _, _) = send(&app, write(&admin, "POST", &years, json!({"calendarId": cal, "label": "x", "startsOn": "2026-13-01", "endsOn": "2027-01-01"}), None)).await;
    assert!(s.is_client_error(), "{s}");
    // Draft period outside its year.
    call(
        &app,
        &admin,
        "PATCH",
        &format!("{B}/calendar-versions/{v}"),
        json!({"startsOn": "2026-05-01"}),
        Some(e),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // A second draft for the same year is refused.
    // Terms: end before start (422), overlapping terms / duplicate sequence / key date outside → publish 422.
    call(&app, &admin, "POST", &format!("{B}/calendar-versions/{v}/terms"), json!({"code": "T0", "name": "T0", "sequence": 1, "startsOn": "2026-07-01", "endsOn": "2026-06-01"}), Some(e), StatusCode::UNPROCESSABLE_ENTITY).await;
    let e = add_term(&app, &admin, &v, e, "T1", 1, "2026-06-15", "2026-12-15").await;
    let e = add_term(&app, &admin, &v, e, "T2", 1, "2026-12-01", "2027-04-30").await;
    let (_, e) = call(&app, &admin, "POST", &format!("{B}/calendar-versions/{v}/key-dates"), json!({"kind": "exam_window", "name": "Late exams", "startsOn": "2027-06-10", "endsOn": "2027-06-20"}), Some(e), StatusCode::CREATED).await;
    let (b, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/publish"),
        json!({}),
        Some(e),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let codes: Vec<&str> = b["validationErrors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["code"].as_str().unwrap())
        .collect();
    for c in ["overlap", "sequence", "outside-version"] {
        assert!(codes.contains(&c), "{c} in {b}");
    }
    // Draft stays a draft and editable after a refused publication.
    let (s, cur) = read(&app, &admin, &format!("{B}/calendar-versions/{v}")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(cur["status"], "draft");
    let term2 = cur["terms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["code"] == "T2")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (cur, _) = call(
        &app,
        &admin,
        "DELETE",
        &format!("{B}/calendar-versions/{v}/terms/{term2}"),
        json!({}),
        Some(e),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cur["terms"].as_array().unwrap().len(), 1);
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/versions"),
        json!({"academicYearId": cur["academicYearId"]}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
}

/// L3-B-029: successor version under the same calendar; predecessor superseded; assignments not migrated.
#[tokio::test]
async fn revision_creates_an_immutable_successor_without_migrating_assignments() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_in_zone(&app, &admin, "Revision College", "Asia/Kolkata").await;
    let (cal, _year, v1, e) =
        calendar_with_draft(&app, &admin, &inst, "2026-06-01", "2027-05-31").await;
    let e = add_term(&app, &admin, &v1, e, "T1", 1, "2026-06-15", "2026-11-30").await;
    let (_, e1) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v1}/publish"),
        json!({}),
        Some(e),
        StatusCode::OK,
    )
    .await;
    let (a, ae) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": v1, "scopeKind": "institution", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let aid = a["id"].as_str().unwrap().to_owned();
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-assignments/{aid}/activate"),
        json!({}),
        Some(ae),
        StatusCode::OK,
    )
    .await;

    // Only the published version can be revised; a stale ETag is 412.
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v1}/revise"),
        json!({}),
        Some(e1 + 7),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    let (v2, e2) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v1}/revise"),
        json!({"reason": "Shift term 1"}),
        Some(e1),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(v2["status"], "draft");
    assert_eq!(v2["versionNo"], 2);
    assert_eq!(v2["supersedesVersionId"], v1.as_str());
    assert_eq!(v2["calendarId"], cal.as_str(), "same calendar identity");
    assert_eq!(v2["notes"], "Shift term 1");
    assert_eq!(v2["terms"][0]["code"], "T1", "structure copied");
    let v2id = v2["id"].as_str().unwrap().to_owned();
    // Only one draft successor at a time.
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v1}/revise"),
        json!({}),
        Some(e1),
        StatusCode::CONFLICT,
    )
    .await;
    let t1 = v2["terms"][0]["id"].as_str().unwrap().to_owned();
    let (_, e2) = call(
        &app,
        &admin,
        "DELETE",
        &format!("{B}/calendar-versions/{v2id}/terms/{t1}"),
        json!({}),
        Some(e2),
        StatusCode::OK,
    )
    .await;
    let e2 = add_term(&app, &admin, &v2id, e2, "T1", 1, "2026-07-01", "2026-12-15").await;
    // The predecessor stays authoritative until the successor is published (BH-004).
    let (_, p1) = read(&app, &admin, &format!("{B}/calendar-versions/{v1}")).await;
    assert_eq!(p1["status"], "published");
    assert_eq!(
        p1["terms"][0]["startsOn"], "2026-06-15",
        "predecessor untouched"
    );
    let (p2, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v2id}/publish"),
        json!({}),
        Some(e2),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p2["status"], "published");
    let (_, p1) = read(&app, &admin, &format!("{B}/calendar-versions/{v1}")).await;
    assert_eq!(p1["status"], "superseded");
    // Superseded cannot be republished; assignment still points at v1 (no auto-migration, BH-005).
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v1}/publish"),
        json!({}),
        Some(p1["version"].as_i64().unwrap()),
        StatusCode::CONFLICT,
    )
    .await;
    let (_, a) = read(&app, &admin, &format!("{B}/calendar-assignments/{aid}")).await;
    assert_eq!(a["calendarVersionId"], v1.as_str());
    assert_eq!(a["status"], "effective");
    // A new assignment on a superseded version is refused; the successor is accepted.
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": v1, "scopeKind": "institution", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let (_, list) = read(&app, &admin, &format!("{B}/calendars/{cal}/versions")).await;
    let statuses: Vec<&str> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, vec!["superseded", "published"]);
    let payload: Value =
        sqlx::query_scalar("SELECT envelope -> 'data' FROM platform.outbox WHERE subject = $1")
            .bind(&v2id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(payload["supersedesVersionId"], v1.as_str());
    assert_eq!(payload["versionNo"], 2);
    // Historical resolution keeps v1 for the assignment that governed then (BH-007).
    let (_, res) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/calendar-resolution?on=2026-06-20"),
    )
    .await;
    assert_eq!(res["calendarVersion"]["id"], v1.as_str());
}

/// Business dates are interpreted in the institution timezone (ADR-L5-0036 §3.4).
#[tokio::test]
async fn exceptions_apply_by_business_date_in_the_institution_timezone() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    // UTC+14 and UTC−11: the local date in Kiritimati is always ahead of Pago Pago.
    let kiri: chrono::NaiveDate =
        sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Pacific/Kiritimati')::date")
            .fetch_one(&pool)
            .await
            .unwrap();
    let start = (kiri - chrono::Duration::days(30)).to_string();
    let end = (kiri + chrono::Duration::days(300)).to_string();
    let mut results = vec![];
    for tz in ["Pacific/Kiritimati", "Pacific/Pago_Pago"] {
        let inst = register_in_zone(&app, &admin, &format!("Zone {tz}"), tz).await;
        let (cal, _y, v, e) = calendar_with_draft(&app, &admin, &inst, &start, &end).await;
        let e = add_term(
            &app,
            &admin,
            &v,
            e,
            "T1",
            1,
            &start,
            &(kiri + chrono::Duration::days(100)).to_string(),
        )
        .await;
        let (p, _) = call(
            &app,
            &admin,
            "POST",
            &format!("{B}/calendar-versions/{v}/publish"),
            json!({}),
            Some(e),
            StatusCode::OK,
        )
        .await;
        assert_eq!(p["timezone"], tz);
        // The exception starts on Kiritimati's today.
        let (x, xe) = call(
            &app,
            &admin,
            "POST",
            &format!("{B}/calendars/{cal}/exceptions"),
            json!({"calendarVersionId": v, "kind": "closure", "startsOn": kiri.to_string(), "endsOn": (kiri + chrono::Duration::days(1)).to_string(), "reason": "Cyclone warning"}),
            None,
            StatusCode::CREATED,
        )
        .await;
        assert_eq!(
            x["status"], "approved",
            "auto-approved via the workflow port"
        );
        assert_eq!(x["governing"], false, "approved is not applied (BH-003)");
        let xid = x["id"].as_str().unwrap().to_owned();
        let (s, _, b) = send(
            &app,
            write(
                &admin,
                "POST",
                &format!("{B}/calendar-exceptions/{xid}/apply"),
                json!({}),
                Some(xe),
            ),
        )
        .await;
        results.push((tz, s, b, xid, xe, inst));
    }
    let (_, s, b, kx, _, _) = &results[0];
    assert_eq!(*s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "in_effect");
    assert_eq!(b["governing"], true);
    let (_, s, b, px, pxe, pinst) = &results[1];
    assert_eq!(
        *s,
        StatusCode::CONFLICT,
        "not yet the start date in Pago Pago: {b}"
    );
    assert!(
        b["detail"].as_str().unwrap().contains("Pacific/Pago_Pago"),
        "{b}"
    );
    // Withdraw the not-yet-applied one; complete the applied one (expiry, BH-006).
    let (w, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-exceptions/{px}/withdraw"),
        json!({"reason": "Cyclone passed"}),
        Some(*pxe),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "withdrawn");
    let (_, cur) = read(&app, &admin, &format!("{B}/calendar-exceptions/{kx}")).await;
    let (done, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-exceptions/{kx}/complete"),
        json!({}),
        Some(cur["version"].as_i64().unwrap()),
        StatusCode::OK,
    )
    .await;
    assert_eq!(done["status"], "completed");
    assert_eq!(done["governing"], false);
    // Completed is terminal.
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-exceptions/{kx}/apply"),
        json!({}),
        Some(done["version"].as_i64().unwrap()),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        outbox_types(&pool, kx).await,
        vec![
            "sutraerp.institution.calendar_exception.approved",
            "sutraerp.institution.calendar_exception.completed"
        ]
    );
    let actions = audit_actions(&pool, kx).await;
    assert_eq!(
        actions,
        vec![
            "institution.calendar_exception.request",
            "institution.calendar_exception.approve",
            "institution.calendar_exception.apply",
            "institution.calendar_exception.complete",
        ]
    );
    // Overlapping exception of the same kind and scope is refused (BH-004) while one is approved.
    let (_, cals) = read(&app, &admin, &format!("{B}/institutions/{pinst}/calendars")).await;
    let pcal = cals["items"][0]["id"].as_str().unwrap().to_owned();
    let (_, vs) = read(&app, &admin, &format!("{B}/calendars/{pcal}/versions")).await;
    let pv = vs["items"][0]["id"].as_str().unwrap().to_owned();
    let body = json!({"calendarVersionId": pv, "kind": "additional_holiday", "startsOn": kiri.to_string(), "endsOn": kiri.to_string(), "reason": "Festival"});
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{pcal}/exceptions"),
        body.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{pcal}/exceptions"),
        body,
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // Outside the base version → 422.
    call(&app, &admin, "POST", &format!("{B}/calendars/{pcal}/exceptions"), json!({"calendarVersionId": pv, "kind": "closure", "startsOn": "2001-01-01", "endsOn": "2001-01-02", "reason": "x"}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
}

async fn calendar_counts(pool: &sqlx::PgPool, inst: &str) -> Vec<i64> {
    let mut out = vec![];
    for q in [
        "SELECT count(*) + COALESCE(sum(version), 0)::bigint FROM institution.calendar_version WHERE institution_id = $1::uuid",
        "SELECT count(*) FROM institution.academic_term WHERE institution_id = $1::uuid",
        "SELECT count(*) FROM institution.calendar_key_date WHERE institution_id = $1::uuid",
        "SELECT count(*) + COALESCE(sum(version), 0)::bigint FROM institution.calendar_assignment WHERE institution_id = $1::uuid",
        "SELECT count(*) + COALESCE(sum(version), 0)::bigint FROM institution.calendar_exception WHERE institution_id = $1::uuid",
        "SELECT count(*) + COALESCE(sum(version), 0)::bigint FROM institution.academic_year WHERE institution_id = $1::uuid",
        "SELECT count(*) + COALESCE(sum(version), 0)::bigint FROM institution.academic_calendar WHERE institution_id = $1::uuid",
        "SELECT count(*) FROM platform.outbox WHERE institution_id = $1::uuid",
    ] {
        let n: i64 = sqlx::query_scalar::<_, Option<i64>>(q).bind(inst).fetch_one(pool).await.unwrap().unwrap_or(0);
        out.push(n);
    }
    out
}

/// L3-B-032: the simulation reports impact against a point-in-time baseline and changes nothing.
#[tokio::test]
async fn change_simulation_has_no_side_effects() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_in_zone(&app, &admin, "Simulation College", "Asia/Kolkata").await;
    let (cal, _y, v, e) =
        calendar_with_draft(&app, &admin, &inst, "2026-06-01", "2027-05-31").await;
    let e = add_term(&app, &admin, &v, e, "T1", 1, "2026-06-15", "2026-11-30").await;
    let e = add_term(&app, &admin, &v, e, "T2", 2, "2026-12-01", "2027-04-30").await;
    let (_, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/publish"),
        json!({}),
        Some(e),
        StatusCode::OK,
    )
    .await;
    let (a, ae) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": v, "scopeKind": "institution", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!(
            "{B}/calendar-assignments/{}/activate",
            a["id"].as_str().unwrap()
        ),
        json!({}),
        Some(ae),
        StatusCode::OK,
    )
    .await;

    let before = calendar_counts(&pool, &inst).await;
    let (sim, se) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/simulations"),
        json!({"baselineVersionId": v, "proposal": {"endsOn": "2027-04-30"}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(sim["status"], "evaluated");
    assert_eq!(
        sim["outcome"], "uncertain",
        "downstream impact unknown, not assumed: {sim}"
    );
    let codes: Vec<&str> = sim["impact"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"period.changed") && codes.contains(&"downstream.unknown"),
        "{codes:?}"
    );
    assert_eq!(sim["baseline"]["calendarVersionId"], v.as_str());
    assert_eq!(
        sim["baseline"]["terms"].as_array().unwrap().len(),
        2,
        "baseline captured"
    );
    assert_eq!(sim["baseline"]["assignments"].as_array().unwrap().len(), 1);
    // A clearly invalid proposal is unfavourable; still nothing changes.
    let (bad, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/simulations"),
        json!({"baselineVersionId": v, "proposal": {"startsOn": "2026-01-01"}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(bad["outcome"], "unfavourable");
    let (cut, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/simulations"),
        json!({"baselineVersionId": v, "proposal": {"endsOn": "2027-03-31"}}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert!(
        cut["impact"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["code"] == "terms.outside_version" && i["severity"] == "blocking"),
        "{cut}"
    );
    assert_eq!(
        calendar_counts(&pool, &inst).await,
        before,
        "no calendar state or outbox change"
    );

    // The impact decision is recorded; accepting applies nothing (BH-006); decided twice → 409.
    let sid = sim["id"].as_str().unwrap().to_owned();
    let (d, de) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-simulations/{sid}/decision"),
        json!({"decision": "accept", "reason": "Board agrees"}),
        Some(se),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "accepted");
    assert_eq!(d["decisionReason"], "Board agrees");
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-simulations/{sid}/decision"),
        json!({"decision": "discard"}),
        Some(de),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(calendar_counts(&pool, &inst).await, before);
    let (_, p) = read(&app, &admin, &format!("{B}/calendar-versions/{v}")).await;
    assert_eq!(p["endsOn"], "2027-05-31");
    assert!(
        outbox_types(&pool, &sid).await.is_empty(),
        "simulation publishes no event"
    );
    assert_eq!(
        audit_actions(&pool, &sid).await,
        vec![
            "institution.calendar_simulation.evaluate",
            "institution.calendar_simulation.accept"
        ]
    );
    let (_, list) = read(&app, &admin, &format!("{B}/calendars/{cal}/simulations")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
}

/// L3-B-031 + B-033: closure evidence, next-cycle foundation, retirement with history.
#[tokio::test]
async fn year_closure_next_cycle_foundation_and_retirement() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_in_zone(&app, &admin, "Closure College", "Asia/Kolkata").await;
    let (cal, year, v, e) =
        calendar_with_draft(&app, &admin, &inst, "2026-06-01", "2027-05-31").await;
    let e = add_term(&app, &admin, &v, e, "T1", 1, "2026-06-15", "2026-11-30").await;
    let (_, ve) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/publish"),
        json!({}),
        Some(e),
        StatusCode::OK,
    )
    .await;
    let (_, ye) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/academic-years/{year}/open"),
        json!({}),
        Some(1),
        StatusCode::OK,
    )
    .await;
    let (a, ae) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": v, "scopeKind": "institution", "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let aid = a["id"].as_str().unwrap().to_owned();
    let (_, ae) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-assignments/{aid}/activate"),
        json!({}),
        Some(ae),
        StatusCode::OK,
    )
    .await;

    // Retirement is blocked while an assignment governs (L4-B-033-BH-002) …
    let (b, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/retire"),
        json!({}),
        Some(ve),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(
        b["detail"]
            .as_str()
            .unwrap()
            .contains("effective assignment"),
        "{b}"
    );
    let (_, c) = read(&app, &admin, &format!("{B}/calendars/{cal}")).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/retire"),
        json!({}),
        Some(c["version"].as_i64().unwrap()),
        StatusCode::CONFLICT,
    )
    .await;
    // … and while the version governs an open year (no resolution gap).
    let (_, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-assignments/{aid}/retire"),
        json!({"reason": "moving"}),
        Some(ae),
        StatusCode::OK,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/retire"),
        json!({}),
        Some(ve),
        StatusCode::CONFLICT,
    )
    .await;

    // Closure before the year ends needs a governed early-closure exception (BH-002).
    let today: chrono::NaiveDate =
        sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Asia/Kolkata')::date")
            .fetch_one(&pool)
            .await
            .unwrap();
    if today <= chrono::NaiveDate::from_ymd_opt(2027, 5, 31).unwrap() {
        let (b, _) = call(
            &app,
            &admin,
            "POST",
            &format!("{B}/academic-years/{year}/close"),
            json!({}),
            Some(ye),
            StatusCode::CONFLICT,
        )
        .await;
        assert!(
            b["detail"]
                .as_str()
                .unwrap()
                .contains("calendar.year_ended"),
            "{b}"
        );
    }
    let (closed, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/academic-years/{year}/close"),
        json!({"earlyClosureReason": "Board resolution 7/2027"}),
        Some(ye),
        StatusCode::OK,
    )
    .await;
    assert_eq!(closed["year"]["status"], "closed");
    let ev: Vec<&str> = closed["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["key"].as_str().unwrap())
        .collect();
    assert!(ev.contains(&"calendar.year_ended") && ev.contains(&"assessment.results_finalised"));
    // Next-cycle foundation: planned (never opened, BH-006) + draft version, no exceptions carried.
    let succ = closed["year"]["successorYearId"]
        .as_str()
        .expect("successor prepared")
        .to_owned();
    let (_, sy) = read(&app, &admin, &format!("{B}/academic-years/{succ}")).await;
    assert_eq!(sy["status"], "planned");
    assert_eq!(sy["label"], "2027-28");
    assert_eq!(sy["startsOn"], "2027-06-01");
    assert_eq!(sy["endsOn"], "2028-05-31");
    assert_eq!(sy["predecessorYearId"], year.as_str());
    let (_, drafts) = read(
        &app,
        &admin,
        &format!("{B}/calendars/{cal}/versions?status=draft"),
    )
    .await;
    assert_eq!(drafts["items"][0]["academicYearId"], succ.as_str());
    assert_eq!(drafts["items"][0]["terms"][0]["code"], "T1");
    // Closed is terminal; closing twice / reopening is illegal.
    let ce = closed["year"]["version"].as_i64().unwrap();
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/academic-years/{year}/close"),
        json!({}),
        Some(ce),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/academic-years/{year}/open"),
        json!({}),
        Some(ce),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        outbox_types(&pool, &year).await,
        vec![
            "sutraerp.institution.academic_year.opened",
            "sutraerp.institution.academic_year.closed"
        ]
    );
    let closure: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.academic_year_closure WHERE academic_year_id = $1::uuid",
    )
    .bind(&year)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(closure, 1);

    // Retirement: version first (now allowed), then the whole calendar; history stays readable.
    let (r, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/retire"),
        json!({"reason": "cycle over"}),
        Some(ve),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "retired");
    // Retired never returns (BH-007).
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/publish"),
        json!({}),
        Some(r["version"].as_i64().unwrap()),
        StatusCode::CONFLICT,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendar-versions/{v}/revise"),
        json!({}),
        Some(r["version"].as_i64().unwrap()),
        StatusCode::CONFLICT,
    )
    .await;
    let (_, c) = read(&app, &admin, &format!("{B}/calendars/{cal}")).await;
    let (rc, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{B}/calendars/{cal}/retire"),
        json!({"reason": "replaced"}),
        Some(c["version"].as_i64().unwrap()),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rc["status"], "retired");
    let (_, all) = read(&app, &admin, &format!("{B}/calendars/{cal}/versions")).await;
    assert!(
        all["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v["status"] == "retired"),
        "{all}"
    );
    call(&app, &admin, "POST", &format!("{B}/institutions/{inst}/academic-years"), json!({"calendarId": cal, "label": "2030-31", "startsOn": "2030-06-01", "endsOn": "2031-05-31"}), None, StatusCode::CONFLICT).await;
    let (_, a) = read(&app, &admin, &format!("{B}/calendar-assignments/{aid}")).await;
    assert_eq!(a["status"], "retired", "assignment history kept");
    assert_eq!(
        outbox_types(&pool, &cal).await,
        vec!["sutraerp.institution.calendar.retired"]
    );
    assert_eq!(
        outbox_types(&pool, &v).await,
        vec![
            "sutraerp.institution.calendar_version.published",
            "sutraerp.institution.calendar_version.retired"
        ]
    );
}

/// Tenant isolation, permissions, `If-Match`, idempotency replay and the institution status guard.
#[tokio::test]
async fn isolation_permissions_etag_idempotency_and_status_guard() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let inst = register_in_zone(&app, &admin, "Guarded College", "Asia/Kolkata").await;
    let (cal, year, v, e) =
        calendar_with_draft(&app, &admin, &inst, "2026-06-01", "2027-05-31").await;

    // Reads are open to every role (institution.read); writes and readiness are admin-only.
    let (s, _) = read(&app, &student, &format!("{B}/calendars/{cal}")).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = send(
        &app,
        write(
            &student,
            "POST",
            &format!("{B}/institutions/{inst}/calendars"),
            json!({"code": "STU", "name": "x"}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(
        &app,
        write(
            &student,
            "POST",
            &format!("{B}/calendar-versions/{v}/publish"),
            json!({}),
            Some(e),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = read(
        &app,
        &student,
        &format!("{B}/institutions/{inst}/activation-readiness"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // If-Match: missing → 428, stale → 412, malformed → 400.
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{B}/academic-years/{year}/open"),
            json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _, b) = send(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{B}/calendar-versions/{v}"),
            json!({"notes": "n"}),
            Some(e + 5),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "{b}");
    // Idempotency-Key: required on create; a replay returns the stored response, one row only.
    let path = format!("{B}/institutions/{inst}/calendars");
    let mk = |key: Option<&str>| {
        let mut r = Request::post(&path)
            .header("content-type", "application/json")
            .header("cookie", &admin.cookie)
            .header("x-csrf-token", &admin.csrf);
        if let Some(k) = key {
            r = r.header("idempotency-key", k);
        }
        r.body(Body::from(
            json!({"code": "IDEM", "name": "Idempotent"}).to_string(),
        ))
        .unwrap()
    };
    let (s, _, _) = send(&app, mk(None)).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let key = uuid::Uuid::now_v7().to_string();
    let (s1, _, b1) = send(&app, mk(Some(&key))).await;
    let (s2, h2, b2) = send(&app, mk(Some(&key))).await;
    assert_eq!((s1, s2), (StatusCode::CREATED, StatusCode::CREATED));
    assert_eq!(b1["id"], b2["id"]);
    assert_eq!(h2["idempotency-replayed"], "true");
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM institution.academic_calendar WHERE institution_id = $1::uuid AND code = 'IDEM'").bind(&inst).fetch_one(&pool).await.unwrap();
    assert_eq!(n, 1);
    // Duplicate code (new key) → 409.
    let (s, _, _) = send(&app, mk(Some(&uuid::Uuid::now_v7().to_string()))).await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Another tenant cannot see or touch any of it (404, indistinguishable from missing).
    let (other, _) = support::app(OTHER_TENANT).await;
    let oadmin = login(&other, "admin@sutra.dev").await;
    for p in [
        format!("{B}/calendars/{cal}"),
        format!("{B}/calendar-versions/{v}"),
        format!("{B}/academic-years/{year}"),
        format!("{B}/institutions/{inst}/calendars"),
        format!("{B}/institutions/{inst}/activation-readiness"),
    ] {
        let (s, b) = read(&other, &oadmin, &p).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "{p}: {b}");
    }
    let (s, _, _) = send(
        &other,
        write(
            &oadmin,
            "POST",
            &format!("{B}/calendars/{cal}/versions"),
            json!({"academicYearId": year}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, _) = send(
        &other,
        write(
            &oadmin,
            "PATCH",
            &format!("{B}/calendar-versions/{v}"),
            json!({"notes": "x"}),
            Some(e),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (_, list) = read(&other, &oadmin, &format!("{B}/calendars/{cal}/versions")).await;
    assert!(list.get("items").is_none(), "{list}");

    // A suspended institution's calendar is read-only (shared guard → 409); reads still work.
    sqlx::query("UPDATE institution.institution SET status = 'suspended' WHERE id = $1::uuid")
        .bind(&inst)
        .execute(&pool)
        .await
        .unwrap();
    let (s, _, b) = send(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{B}/calendar-versions/{v}"),
            json!({"notes": "n"}),
            Some(e),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "POST",
            &path,
            json!({"code": "SUSP", "name": "x"}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, r) = read(
        &app,
        &admin,
        &format!("{B}/institutions/{inst}/activation-readiness"),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(r["institutionStatus"], "suspended");
}

/// `ApprovalMode::AwaitDecision` (Flowable later): publication and exceptions stay pending until
/// the explicit decision.
#[tokio::test]
async fn await_decision_mode_keeps_steps_pending_until_decided() {
    use std::sync::Arc;
    use sutra_contracts::institution::ids::*;
    use sutra_contracts::institution::status::{CalendarExceptionStatus, PublicationStatus};
    use sutra_institution::calendar::{domain as d, repo};
    use sutra_institution::shared::approval::ApprovalMode;
    use sutra_institution::shared::{Actor, InstitutionState};
    use sutra_kernel::cache::AdvisoryCache;
    use sutra_kernel::concurrency::Version;
    use sutra_kernel::scope::TenantId;

    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_in_zone(&app, &admin, "Awaiting College", "Asia/Kolkata").await;
    let (cal, _year, v, e) =
        calendar_with_draft(&app, &admin, &inst, "2026-06-01", "2027-05-31").await;
    let e = add_term(&app, &admin, &v, e, "T1", 1, "2026-06-15", "2026-11-30").await;

    let mut st = InstitutionState::new(
        pool.clone(),
        AdvisoryCache::disabled(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    );
    st.approvals = ApprovalMode::AwaitDecision;
    let tenant: TenantId = TENANT.parse().unwrap();
    let actor = Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };
    let vid: CalendarVersionId = v.parse().unwrap();
    let pending = repo::publish_version(&st, tenant, vid, Version(e), None, &actor)
        .await
        .unwrap();
    assert_eq!(pending.status, PublicationStatus::InReview);
    assert!(
        outbox_types(&pool, &v).await.is_empty(),
        "no publication event while pending"
    );
    // Return to draft, then approve.
    let back = repo::decide_version(
        &pool,
        tenant,
        vid,
        pending.version,
        d::Decision {
            decision: d::DecisionKind::Reject,
            reason: Some("fix".into()),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(back.status, PublicationStatus::Draft);
    let pending = repo::publish_version(&st, tenant, vid, back.version, None, &actor)
        .await
        .unwrap();
    let published = repo::decide_version(
        &pool,
        tenant,
        vid,
        pending.version,
        d::Decision {
            decision: d::DecisionKind::Approve,
            reason: None,
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(published.status, PublicationStatus::Published);
    assert_eq!(
        outbox_types(&pool, &v).await,
        vec!["sutraerp.institution.calendar_version.published"]
    );

    let cid: AcademicCalendarId = cal.parse().unwrap();
    let x = repo::create_exception(
        &st,
        tenant,
        cid,
        d::NewException {
            calendar_version_id: vid,
            kind: d::ExceptionKind::Closure,
            scope_kind: None,
            scope_id: None,
            starts_on: "2026-07-01".parse().unwrap(),
            ends_on: "2026-07-02".parse().unwrap(),
            reason: "Strike".into(),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(x.status, CalendarExceptionStatus::Requested);
    assert!(x.process_instance_id.is_some());
    let r = repo::decide_exception(
        &pool,
        tenant,
        x.id,
        x.version,
        d::Decision {
            decision: d::DecisionKind::Reject,
            reason: Some("no".into()),
        },
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(r.status, CalendarExceptionStatus::Rejected);
    assert!(repo::decide_exception(
        &pool,
        tenant,
        x.id,
        r.version,
        d::Decision {
            decision: d::DecisionKind::Approve,
            reason: None
        },
        &actor
    )
    .await
    .is_err());
}
