//! Audit remediation 2026-10, slice A2 (SEC-1 / C-1, VAL-1, server side of VAL-2/3/4, H-2):
//! the server refuses markup / control characters in identity names, out-of-range money and seat
//! counts, and malformed applicant / enquiry contact data, each with a field-level
//! `validation-failed` problem (`validationErrors[]` naming the camelCase field).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use support::*;

const XSS: &str = "<img src=x onerror=alert(1)>";

fn tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

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

/// Asserts a 422 `validation-failed` problem naming `field` (and `code` when given).
fn assert_field(s: StatusCode, b: &Value, field: &str, code: Option<&str>) {
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert_eq!(b["errorCode"], "validation-failed", "{b}");
    let errs = b["validationErrors"].as_array().expect("validationErrors");
    assert!(
        errs.iter()
            .any(|e| e["field"] == field && code.is_none_or(|c| e["code"] == c)),
        "expected {field} ({code:?}) in {b}"
    );
}

// ------------------------------------------------------------------ institution (SEC-1)

#[tokio::test]
async fn institution_names_refuse_markup_and_control_characters() {
    let (app, _pool) = app(tenant()).await;
    let admin = login(&app, "admin@sutra.dev").await;
    for (name, code) in [
        (XSS, "markup"),
        ("Sutra <b>Institute</b>", "markup"),
        ("Sutra\u{0}Institute", "control-characters"),
        ("Sutra\nInstitute", "control-characters"),
        ("Sutra\u{202E}etutitsnI", "control-characters"),
    ] {
        let (s, b) = call(
            &app,
            &admin,
            "POST",
            INSTITUTIONS,
            json!({"code": code_for("XS"), "name": name}),
            None,
        )
        .await;
        assert_field(s, &b, "name", Some(code));
    }
    // Over-long and blank names stay refused; ordinary punctuation is fine.
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        INSTITUTIONS,
        json!({"code": code_for("XS"), "name": "x".repeat(201)}),
        None,
    )
    .await;
    assert_field(s, &b, "name", Some("too-long"));
    let (s, inst) = call(
        &app,
        &admin,
        "POST",
        INSTITUTIONS,
        json!({"code": code_for("XS"), "name": "  St. Mary's College of Arts & Science (Pune)  "}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{inst}");
    assert_eq!(inst["name"], "St. Mary's College of Arts & Science (Pune)");

    // Rename (update) applies the same policy.
    let path = format!("{INSTITUTIONS}/{}", inst["id"].as_str().unwrap());
    let v = inst["version"].as_i64().unwrap();
    let (s, b) = call(&app, &admin, "PATCH", &path, json!({"name": XSS}), Some(v)).await;
    assert_field(s, &b, "name", Some("markup"));
    let (s, b) = call(
        &app,
        &admin,
        "PATCH",
        &path,
        json!({"name": "St. Mary's College"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
}

fn code_for(prefix: &str) -> String {
    code(prefix)
}

// ------------------------------------------------------------------ admissions (VAL-3 / H-2)

#[tokio::test]
async fn admission_applications_need_valid_names_dob_mobile_and_email() {
    let t = tenant();
    let (app, _pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "A2 Admissions College").await;
    let path = format!(
        "/api/v1/student/institutions/{}/admission-applications",
        setup.institution_id
    );
    let base = || {
        json!({
            "programmeId": setup.programme_id, "givenName": "Ananya", "familyName": "Iyer",
            "dateOfBirth": "2007-05-09", "email": "ananya.iyer@example.in",
            "phone": "+91 98222 33445",
        })
    };
    let with = |k: &str, v: Value| {
        let mut b = base();
        b[k] = v;
        b
    };
    let today = Utc::now().date_naive();
    let cases: Vec<(Value, &str, &str)> = vec![
        (with("givenName", json!(XSS)), "givenName", "markup"),
        (
            with("familyName", json!("Iyer\u{7}")),
            "familyName",
            "control-characters",
        ),
        (with("dateOfBirth", Value::Null), "dateOfBirth", "required"),
        (
            with(
                "dateOfBirth",
                json!((today + Duration::days(30)).to_string()),
            ),
            "dateOfBirth",
            "future-date",
        ),
        (
            with(
                "dateOfBirth",
                json!((today - Duration::days(365)).to_string()),
            ),
            "dateOfBirth",
            "out-of-range",
        ),
        (
            with("dateOfBirth", json!("1900-01-01")),
            "dateOfBirth",
            "out-of-range",
        ),
        (with("phone", json!("123")), "phone", "format"),
        (with("phone", json!("5876543210")), "phone", "format"),
        (with("phone", json!("+1 415 555 0100")), "phone", "format"),
        (with("email", json!("abc")), "email", "format"),
        (with("email", json!("a@b")), "email", "format"),
    ];
    for (body, field, code) in cases {
        let (s, b) = call(&app, &admin, "POST", &path, body, None).await;
        assert_field(s, &b, field, Some(code));
    }
    // An unparseable date is refused before the handler (no 2xx, no draft).
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &path,
        with("dateOfBirth", json!("31-02-2007")),
        None,
    )
    .await;
    assert!(s.is_client_error(), "{s}");
    // Every 422 above left no draft behind.
    let (s, _, list) = send(&app, get(&admin, &path)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(list["items"].as_array().unwrap().len(), 0, "{list}");
    // The well-formed applicant (mobile written with spaces / +91) is accepted.
    let (s, b) = call(&app, &admin, "POST", &path, base(), None).await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
    // Editing applies the same rules.
    let app_path = format!(
        "/api/v1/student/admission-applications/{}",
        b["id"].as_str().unwrap()
    );
    let v = b["version"].as_i64().unwrap();
    let (s, e) = call(
        &app,
        &admin,
        "PATCH",
        &app_path,
        json!({"phone": "12"}),
        Some(v),
    )
    .await;
    assert_field(s, &e, "phone", Some("format"));
    let (s, e) = call(
        &app,
        &admin,
        "PATCH",
        &app_path,
        json!({"givenName": XSS}),
        Some(v),
    )
    .await;
    assert_field(s, &e, "givenName", Some("markup"));
}

#[tokio::test]
async fn enquiries_need_a_contact_and_clean_names() {
    let t = tenant();
    let (app, _pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "A2 Enquiry College").await;
    let path = format!(
        "/api/v1/student/institutions/{}/admission-enquiries",
        setup.institution_id
    );
    // A first name alone is no longer an enquiry (VAL-3).
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &path,
        json!({"givenName": "Rohan"}),
        None,
    )
    .await;
    assert_field(s, &b, "phone", Some("contact-required"));
    for (body, field, code) in [
        (
            json!({"givenName": XSS, "phone": "9820012345"}),
            "givenName",
            "markup",
        ),
        (
            json!({"givenName": "Rohan", "guardianName": "<script>", "phone": "9820012345"}),
            "guardianName",
            "markup",
        ),
        (
            json!({"givenName": "Rohan", "phone": "12345"}),
            "phone",
            "format",
        ),
        (
            json!({"givenName": "Rohan", "email": "rohan@", "phone": "9820012345"}),
            "email",
            "format",
        ),
        (
            json!({"givenName": "Rohan", "phone": "9820012345", "guardianPhone": "000"}),
            "guardianPhone",
            "format",
        ),
    ] {
        let (s, b) = call(&app, &admin, "POST", &path, body, None).await;
        assert_field(s, &b, field, Some(code));
    }
    for ok in [
        json!({"givenName": "Rohan", "phone": "+91 98200 12345"}),
        json!({"givenName": "Kavya", "email": "kavya@example.in"}),
    ] {
        let (s, b) = call(&app, &admin, "POST", &path, ok, None).await;
        assert_eq!(s, StatusCode::CREATED, "{b}");
    }
}

// ------------------------------------------------------------------ offerings (VAL-4)

#[tokio::test]
async fn offering_seats_are_one_to_ten_thousand_and_titles_are_clean() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let s = b4::academic_setup(&app, &admin, &pool, t, "A2 Offering College").await;
    let path = format!(
        "/api/v1/academic/institutions/{}/offerings",
        s.base.institution_id
    );
    let body = |code: &str, extra: Value| {
        let mut b = json!({"academicTermId": s.term_id,
            "curriculumVersionId": s.base.curriculum_version_id,
            "curriculumComponentId": s.component("CS101").id, "offeringCode": code});
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };
    for (extra, field) in [
        (json!({"capacity": 0}), "capacity"),
        (json!({"capacity": -100}), "capacity"),
        (json!({"capacity": 10_001}), "capacity"),
        (
            json!({"sections": [{"sectionCode": "A", "capacity": 0}]}),
            "capacity",
        ),
        (json!({"title": XSS}), "title"),
    ] {
        let (st, b) = call(&app, &admin, "POST", &path, body("A2-BAD", extra), None).await;
        assert_field(st, &b, field, None);
    }
    let (st, b) = call(
        &app,
        &admin,
        "POST",
        &path,
        body(
            "A2-OK",
            json!({"capacity": 10_000, "title": "Data Structures & Algorithms",
                   "sections": [{"sectionCode": "A", "capacity": 1}]}),
        ),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{b}");
}

// ------------------------------------------------------------------ staff (VAL-1)

#[tokio::test]
async fn staff_names_refuse_markup() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, t, "A2 Staff College").await;
    let path = format!(
        "/api/v1/people/institutions/{}/staff",
        setup.institution().as_uuid()
    );
    for (body, field) in [
        (
            json!({"givenName": XSS, "staffCategory": "teaching"}),
            "givenName",
        ),
        (
            json!({"givenName": "Anika", "familyName": "Rao\u{0}", "staffCategory": "teaching"}),
            "familyName",
        ),
        (
            json!({"givenName": "Anika", "displayName": "<i>Anika</i>", "staffCategory": "teaching"}),
            "displayName",
        ),
        (
            json!({"givenName": "Anika", "workEmail": "anika@b", "staffCategory": "teaching"}),
            "workEmail",
        ),
    ] {
        let (s, b) = call(&app, &admin, "POST", &path, body, None).await;
        assert_field(s, &b, field, None);
    }
}

// ------------------------------------------------------------------ payments (VAL-2 / H-2)

#[tokio::test]
async fn payment_amounts_are_positive_and_at_most_one_crore() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(&app, &admin, &pool, t, "A2 Payments College", None).await;
    let path = format!(
        "/api/v1/finance/student-accounts/{}/payments",
        setup.student_account_id
    );
    for (amount, code) in [
        ("-100", "invalid_amount"),
        ("0", "invalid_amount"),
        ("99999999999", "amount_too_large"),
        ("10000000.01", "amount_too_large"),
    ] {
        let (s, b) = call(
            &app,
            &admin,
            "POST",
            &path,
            json!({"channel": "cash", "amount": amount}),
            None,
        )
        .await;
        assert_field(s, &b, "amount", Some(code));
    }
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &path,
        json!({"channel": "cash", "amount": "10000000.00"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "exactly ₹1 crore is allowed: {b}");
}
