//! Part 1 Stage 2 (acad) regression tests for Domain C admissions, on real PostgreSQL:
//!
//! * DEF-C-002 — one closed admission reopens at most one live readmission; a person with a live
//!   record is not readmitted a second time (application check + unique index).
//! * DEF-C-003 — duplicate-applicant detection at application time (normalised name + DOB +
//!   e-mail / phone) returns 409 naming the candidate; an explicit override with a reason is
//!   recorded in the audit trail.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use support::*;

const API: &str = "/api/v1/student";

fn tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, v)).await;
    (s, b)
}

async fn ok(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    v: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, b) = call(app, who, method, path, body, v).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

fn ver(b: &Value) -> Option<i64> {
    b["version"].as_i64()
}

fn id(b: &Value) -> String {
    b["id"].as_str().unwrap().to_owned()
}

/// Application → submit → review → offer; returns the initiated admission (no student yet).
async fn offered(app: &Router, admin: &Login, setup: &b3::AcademicSetup, body: Value) -> Value {
    let inst = setup.institution_id;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{inst}/admission-applications"),
        body,
        None,
        StatusCode::CREATED,
    )
    .await;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admission-applications/{}/submit", id(&a)),
        json!({}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admission-applications/{}/review", id(&a)),
        json!({}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admission-applications/{}/offer", id(&a)),
        json!({}),
        ver(&a),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "offered");
    ok(
        app,
        admin,
        "GET",
        &format!("{API}/admissions/{}", a["admissionId"].as_str().unwrap()),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await
}

async fn cancel(app: &Router, admin: &Login, adm: &Value) -> Value {
    let c = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{}/cancel", id(adm)),
        json!({"reason": "applicant request"}),
        ver(adm),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "cancelled");
    c
}

/// DEF-C-002.
#[tokio::test]
async fn a_closed_admission_is_readmitted_only_once() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "P1 Acad Readmission College").await;
    let applicant = json!({
        "programmeId": setup.programme_id, "givenName": "Kabir", "familyName": "Rao",
        "dateOfBirth": "2007-02-02", "email": "kabir.rao@example.in", "phone": "+91 98111 22334",
        "admissionTermId": setup.term_ids[0],
    });
    let a1 = offered(&app, &admin, &setup, applicant).await;
    assert!(a1["studentId"].is_null(), "no student yet: {a1}");
    let a1 = cancel(&app, &admin, &a1).await;
    let readmit = |adm: &Value| format!("{API}/admissions/{}/readmit", id(adm));

    let a2 = ok(
        &app,
        &admin,
        "POST",
        &readmit(&a1),
        json!({"reason": "applicant returned"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a2["status"], "initiated");
    assert_eq!(a2["previousAdmissionId"], json!(id(&a1)));
    // the same closed admission again (new Idempotency-Key): refused, naming the open one
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &readmit(&a1),
        json!({"reason": "again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(b["detail"].as_str().unwrap().contains(&id(&a2)), "{b}");

    // the live readmission is cancelled and readmitted in turn (one live admission at a time)
    let a2 = cancel(&app, &admin, &a2).await;
    let a3 = ok(
        &app,
        &admin,
        "POST",
        &readmit(&a2),
        json!({"reason": "second return"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    // the original admission now has no live readmission of its own, but the person does
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &readmit(&a1),
        json!({"reason": "third try"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(
        b["detail"].as_str().unwrap().contains(&id(&a3)),
        "person-key candidate: {b}"
    );

    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.admission WHERE tenant_id = $1::uuid AND lower(given_name) = 'kabir' \
         AND status NOT IN ('cancelled', 'withdrawn', 'revoked', 'expired', 'completed')",
    )
    .bind(t)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(live, 1, "one live admission for the person");

    // the database refuses a second live readmission of the same admission, whatever the caller
    let dup = sqlx::query(
        "INSERT INTO student.admission SELECT (jsonb_populate_record(NULL::student.admission, \
           to_jsonb(a) || jsonb_build_object('id', uuidv7()))).* FROM student.admission a WHERE a.id = $1::uuid",
    )
    .bind(id(&a3))
    .execute(&pool)
    .await;
    let err = dup
        .expect_err("a second live readmission must be refused")
        .to_string();
    assert!(err.contains("admission_one_live_readmission"), "{err}");
}

/// DEF-C-003.
#[tokio::test]
async fn duplicate_applicants_are_detected_at_application_time() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "P1 Acad Intake College").await;
    let inst = setup.institution_id;
    let path = format!("{API}/institutions/{inst}/admission-applications");
    let base = json!({
        "programmeId": setup.programme_id, "givenName": "Ananya", "familyName": "Iyer",
        "dateOfBirth": "2007-05-09", "email": "ananya.iyer@example.in", "phone": "+91 98222 33445",
    });
    let first = ok(
        &app,
        &admin,
        "POST",
        &path,
        base.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;

    // same person: name differs only in case / spacing, same DOB, phone written differently
    let mut again = json!({
        "programmeId": setup.programme_id, "givenName": " ananya ", "familyName": "IYER",
        "dateOfBirth": "2007-05-09", "email": "someone.else@example.in", "phone": "98222-33445",
    });
    let (s, b) = call(&app, &admin, "POST", &path, again.clone(), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let detail = b["detail"].as_str().unwrap();
    assert!(
        detail.contains(&format!("candidate application {}", id(&first))),
        "{b}"
    );

    // the override must name a detected candidate and give a reason
    again["duplicateOverride"] =
        json!({"candidateId": uuid::Uuid::now_v7(), "reason": "different person"});
    let (s, b) = call(&app, &admin, "POST", &path, again.clone(), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    again["duplicateOverride"] = json!({"candidateId": id(&first), "reason": "  "});
    let (s, b) = call(&app, &admin, "POST", &path, again.clone(), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    again["duplicateOverride"] = json!({"candidateId": id(&first), "reason": "Twin sister, same birthday, shared family phone"});
    let twin = ok(
        &app,
        &admin,
        "POST",
        &path,
        again,
        None,
        StatusCode::CREATED,
    )
    .await;
    let audit: Vec<(String, Value)> = sqlx::query_as(
        "SELECT actor_id, details FROM platform.audit_log WHERE target_id = $1 \
         AND action = 'student.admission_application.duplicate_override'",
    )
    .bind(id(&twin))
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(audit.len(), 1, "override audited");
    assert_eq!(audit[0].0, "dev|admin@sutra.dev");
    assert_eq!(audit[0].1["candidateId"], json!(id(&first)));
    assert_eq!(
        audit[0].1["reason"],
        "Twin sister, same birthday, shared family phone"
    );

    // not the same person: another date of birth, or no contact to match on
    let mut other = base.clone();
    other["dateOfBirth"] = json!("2006-05-09");
    ok(
        &app,
        &admin,
        "POST",
        &path,
        other,
        None,
        StatusCode::CREATED,
    )
    .await;
    ok(&app, &admin, "POST", &path, json!({"programmeId": setup.programme_id, "givenName": "Ananya", "familyName": "Iyer", "dateOfBirth": "2007-05-09"}), None, StatusCode::CREATED).await;

    // a withdrawn application is no longer a candidate
    let w = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admission-applications/{}/withdraw", id(&first)),
        json!({"reason": "moved city"}),
        ver(&first),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "withdrawn");
    let (s, b) = call(&app, &admin, "POST", &path, base.clone(), None).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "the overridden twin application is still a candidate: {b}"
    );
    assert!(b["detail"].as_str().unwrap().contains(&id(&twin)), "{b}");

    // an existing student of the institution is a candidate too
    let st = b3::seed_student(&pool, &setup, "Dev", None).await;
    sqlx::query("UPDATE student.student SET family_name = 'Malhotra', date_of_birth = DATE '2006-12-01', primary_email = 'dev.m@example.in' WHERE id = $1")
        .bind(st.student_id)
        .execute(&pool)
        .await
        .unwrap();
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &path,
        json!({
            "programmeId": setup.programme_id, "givenName": "Dev", "familyName": "Malhotra",
            "dateOfBirth": "2006-12-01", "email": "DEV.M@example.in",
        }),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(
        b["detail"]
            .as_str()
            .unwrap()
            .contains(&format!("candidate student {}", st.student_id)),
        "{b}"
    );
}
