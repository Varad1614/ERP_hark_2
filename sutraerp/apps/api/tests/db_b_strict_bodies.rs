//! Audit remediation 2026-10, Phase B (D-2 follow-up, mass assignment): command bodies of the
//! payment and offering / section endpoints are allow-listed (`deny_unknown_fields`). An unknown
//! member is a `422` problem that does not echo the member name back (the A3 envelope).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use serde_json::{json, Value};
use support::*;

fn tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

fn assert_refused(s: StatusCode, b: &Value) {
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert!(b["errorCode"].is_string(), "problem+json expected: {b}");
    assert!(
        !b.to_string().contains("isAdmin"),
        "member name echoed: {b}"
    );
}

#[tokio::test]
async fn payment_bodies_refuse_unknown_members() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(&app, &admin, &pool, t, "B Strict Pay College", None).await;
    let path = format!(
        "/api/v1/finance/student-accounts/{}/payments",
        setup.student_account_id
    );
    let (s, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &path,
            json!({"channel": "cash", "amount": "100.00", "isAdmin": true}),
            None,
        ),
    )
    .await;
    assert_refused(s, &b);
    let (s, _, b) = send(
        &app,
        write(
            &admin,
            "POST",
            &path,
            json!({"channel": "cash", "amount": "100.00"}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{b}");
}

#[tokio::test]
async fn offering_bodies_refuse_unknown_members() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let s = b4::academic_setup(&app, &admin, &pool, t, "B Strict Offering College").await;
    let path = format!(
        "/api/v1/academic/institutions/{}/offerings",
        s.base.institution_id
    );
    let body = json!({"academicTermId": s.term_id,
        "curriculumVersionId": s.base.curriculum_version_id,
        "curriculumComponentId": s.component("CS101").id,
        "offeringCode": "B-STRICT", "isAdmin": true});
    let (st, _, b) = send(&app, write(&admin, "POST", &path, body, None)).await;
    assert_refused(st, &b);
}
