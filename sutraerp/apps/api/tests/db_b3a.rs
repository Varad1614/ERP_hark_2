//! Slice 3A DB tests (owned by 3A; see `docs/batch3-contracts.md` §11). Shared harness:
//! `support::*` and the Batch 3 helpers in `support::b3`.
//!
//! Coverage (L3/L4 section C): C-001 + C-007 (admission → active student, document
//! verification, approval through the WorkflowPort with auto-approval **and** `AwaitDecision`,
//! financial clearance from Finance evidence), C-002 cancellation, C-003 withdrawal, C-004
//! revocation, C-005 readmission, C-006 offer expiry, C-008 enrolment update, C-009 academic
//! assignment, C-010/011 suspension + restoration, C-012/013 intra/inter-institute transfer,
//! C-014 profile, C-015 communication preferences, C-016 identity correction, C-017 duplicate
//! resolution, C-018 document requests, C-019 archival; tenant isolation, ETag/If-Match,
//! Idempotency-Key, problem+json, audit + outbox in the owner-local transaction.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::sync::Arc;
use support::*;
use sutra_contracts::finance::{
    AdmissionFinanceQuery, FinanceClearancePort, FinanceEvidence, StudentFinanceQuery,
};
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_student::shared::approval::ApprovalMode;
use sutra_student::StudentState;

const API: &str = "/api/v1/student";

// ------------------------------------------------------------------------------- helpers

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
    let req = if method == "GET" {
        get(who, path)
    } else {
        write(who, method, path, body, if_match)
    };
    let (s, _, b) = send(app, req).await;
    (s, b)
}

async fn ok(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> Value {
    let (s, b) = call(app, who, method, path, body, if_match).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

fn id(b: &Value) -> String {
    b["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {b}"))
        .to_owned()
}

fn ver(b: &Value) -> i64 {
    b["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no version in {b}"))
}

fn ctx_ver(b: &Value) -> i64 {
    b["currentContext"]["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no context in {b}"))
}

async fn outbox_types(pool: &sqlx::PgPool, subject: &str) -> Vec<String> {
    sqlx::query_scalar("SELECT event_type FROM platform.outbox WHERE subject = $1 ORDER BY id")
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

fn ev(suffix: &str) -> String {
    format!("sutraerp.student.{suffix}")
}

/// Draft application → submitted → under review → offered; returns the admission.
async fn offered(
    app: &Router,
    admin: &Login,
    setup: &b3::AcademicSetup,
    given: &str,
    subject: Option<&str>,
    offer: Value,
) -> Value {
    let inst = setup.institution_id;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/institutions/{inst}/admission-applications"),
        json!({
            "programmeId": setup.programme_id, "givenName": given, "familyName": "Kulkarni",
            "dateOfBirth": "2007-03-14", "email": format!("{}@example.in", given.to_lowercase()),
            "phone": "+91 98765 43210", "subjectRef": subject,
            "admissionTermId": setup.term_ids[0],
        }),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(a["status"], "draft");
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admission-applications/{}/submit", id(&a)),
        json!({}),
        Some(ver(&a)),
        StatusCode::OK,
    )
    .await;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admission-applications/{}/review", id(&a)),
        json!({}),
        Some(ver(&a)),
        StatusCode::OK,
    )
    .await;
    let a = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admission-applications/{}/offer", id(&a)),
        offer,
        Some(ver(&a)),
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

/// accept → documents → submit-documents → verify all → (auto) approval → clearance →
/// confirm. Returns the confirmed admission.
async fn confirmed(app: &Router, admin: &Login, adm: Value) -> Value {
    let aid = id(&adm);
    let adm = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/accept"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    let mut docs = vec![];
    for t in adm["requiredDocuments"].as_array().unwrap().clone() {
        let d = ok(
            app,
            admin,
            "POST",
            &format!("{API}/admissions/{aid}/documents"),
            json!({"documentType": t, "title": format!("{t} scan"), "fileRef": "dms://x"}),
            None,
            StatusCode::CREATED,
        )
        .await;
        docs.push(d);
    }
    let adm = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-documents"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "documents_submitted");
    for d in &docs {
        ok(
            app,
            admin,
            "POST",
            &format!("{API}/admissions/{aid}/documents/{}/verify", id(d)),
            json!({"outcome": "verified"}),
            Some(ver(d)),
            StatusCode::OK,
        )
        .await;
    }
    let adm = ok(
        app,
        admin,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "verification_completed");
    let adm = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-for-approval"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "approved", "{adm}");
    let adm = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/financial-clearance"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "financially_cleared");
    let adm = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/confirm"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "confirmed");
    adm
}

/// create-student + enrol. Returns (admission, student view after enrolment).
async fn enrolled(app: &Router, admin: &Login, adm: Value, enrol: Value) -> (Value, Value) {
    let aid = id(&adm);
    let adm = ok(
        app,
        admin,
        "POST",
        &format!("{API}/admissions/{aid}/create-student"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "student_created");
    let sid = adm["studentId"].as_str().unwrap().to_owned();
    let s = ok(
        app,
        admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let pending = s["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["status"] == "pending_activation")
        .cloned()
        .expect("pending context");
    let s = ok(
        app,
        admin,
        "POST",
        &format!("{API}/students/{sid}/enrol"),
        enrol,
        Some(ver(&pending)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["currentContext"]["status"], "active", "{s}");
    let adm = ok(
        app,
        admin,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    (adm, s)
}

/// Full admission flow → active student. Returns (admission, student).
async fn admit(
    app: &Router,
    admin: &Login,
    setup: &b3::AcademicSetup,
    given: &str,
    subject: Option<&str>,
) -> (Value, Value) {
    let adm = offered(app, admin, setup, given, subject, json!({})).await;
    let adm = confirmed(app, admin, adm).await;
    enrolled(
        app,
        admin,
        adm,
        json!({"batchCode": "2026", "sectionCode": "A"}),
    )
    .await
}

/// Finance double: outstanding admission dues and student dues.
struct OutstandingFinance;

#[async_trait::async_trait]
impl FinanceClearancePort for OutstandingFinance {
    async fn admission_clearance(
        &self,
        _scope: &ExecutionScope,
        _q: AdmissionFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        Ok(FinanceEvidence::Outstanding {
            detail: "admission fee unpaid".into(),
        })
    }
    async fn student_dues(
        &self,
        _scope: &ExecutionScope,
        _q: StudentFinanceQuery,
    ) -> Result<FinanceEvidence, KernelError> {
        Ok(FinanceEvidence::Outstanding {
            detail: "tuition unpaid".into(),
        })
    }
}

/// Domain C router with a hand-built state (e.g. `AwaitDecision`) and an administrator session
/// injected directly (no cookie / CSRF layer: this tests the handlers, not the session layer).
fn custom_app(st: StudentState, tenant: &str) -> Router {
    let (router, _) = sutra_student::router_with_state(st).split_for_parts();
    let ctx = AuthContext {
        session_id: uuid::Uuid::now_v7(),
        subject: "dev|admin@sutra.dev".into(),
        login_id: "admin@sutra.dev".into(),
        display_name: "Admin".into(),
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

fn state(pool: &sqlx::PgPool, mode: ApprovalMode) -> StudentState {
    let mut st = sutra_api::app::student_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = mode;
    st
}

/// Session-less login stand-in for the custom app (cookie/CSRF are ignored there).
fn injected() -> Login {
    Login {
        cookie: "x=y".into(),
        csrf: "x".into(),
    }
}

// --------------------------------------------------------------------------------- tests

/// L3-C-001 + L3-C-007 (auto-approval): application → offer (AdmissionInitiated) → acceptance →
/// documents → verification → approval through the WorkflowPort → Finance-evidence clearance →
/// confirmation → student created + activated → enrolled (context active, persona active,
/// admission completed). Events, audit, history, directory, Student 360, self-service, roles,
/// tenant isolation and the HTTP preconditions along the way.
#[tokio::test]
async fn admission_to_active_student() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Admissions University").await;
    let inst = setup.institution_id;

    // Validation, idempotency and permissions on create.
    let path = format!("{API}/institutions/{inst}/admission-applications");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &path,
        json!({"programmeId": uuid::Uuid::now_v7(), "givenName": "X"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    assert!(
        b["validationErrors"].to_string().contains("programmeId"),
        "{b}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &path,
        json!({"programmeId": setup.programme_id, "givenName": " ", "email": "bad"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let req = Request::post(&path)
        .header("content-type", "application/json")
        .header("cookie", &admin.cookie)
        .header("x-csrf-token", &admin.csrf)
        .body(Body::from(
            json!({"programmeId": setup.programme_id, "givenName": "Y"}).to_string(),
        ))
        .unwrap();
    let (s, _, _) = send(&app, req).await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "missing Idempotency-Key");
    let (s, _) = call(
        &app,
        &faculty,
        "POST",
        &path,
        json!({"programmeId": setup.programme_id, "givenName": "Y"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&app, &faculty, "GET", &path, json!(null), None).await;
    assert_eq!(s, StatusCode::OK, "faculty read the admissions list");

    // Offer → admission initiated; the offer cannot expire in the past.
    let adm = offered(
        &app,
        &admin,
        &setup,
        "Aarav",
        Some("dev|aarav.desai@sutra.dev"),
        json!({}),
    )
    .await;
    let aid = id(&adm);
    assert_eq!(adm["status"], "initiated");
    assert_eq!(
        adm["curriculumVersionId"],
        json!(setup.curriculum_version_id)
    );
    assert_eq!(
        adm["requiredDocuments"],
        json!(["identity_proof", "qualifying_marksheet"])
    );
    assert!(adm["offerExpiresOn"].is_string());
    assert!(outbox_types(&pool, &aid)
        .await
        .contains(&ev("admission.initiated")));

    // Documents cannot be declared submitted before acceptance; If-Match is enforced.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-documents"),
        json!({}),
        Some(ver(&adm)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/accept"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/accept"),
        json!({}),
        Some(ver(&adm) + 5),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let adm = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/accept"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert!(adm["offerAcceptedAt"].is_string());
    // Approval needs completed verification.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-for-approval"),
        json!({}),
        Some(ver(&adm)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Missing required documents → 422; one rejected document blocks verification until replaced.
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-documents"),
        json!({}),
        Some(ver(&adm)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let id_doc = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents"),
        json!({"documentType": "identity_proof", "title": "Aadhaar"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let marks = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents"),
        json!({"documentType": "qualifying_marksheet", "title": "HSC marksheet"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let submitted = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-documents"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(submitted["status"], "documents_submitted");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents/{}/verify", id(&marks)),
        json!({"outcome": "rejected"}),
        Some(ver(&marks)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "a rejection needs a note"
    );
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents/{}/verify", id(&marks)),
        json!({"outcome": "rejected", "note": "illegible"}),
        Some(ver(&marks)),
        StatusCode::OK,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents/{}/verify", id(&id_doc)),
        json!({"outcome": "verified"}),
        Some(ver(&id_doc)),
        StatusCode::OK,
    )
    .await;
    let adm = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        adm["status"], "documents_submitted",
        "a rejected required document blocks verification"
    );
    let marks2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents"),
        json!({"documentType": "qualifying_marksheet", "title": "HSC marksheet (clear)"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/documents/{}/verify", id(&marks2)),
        json!({"outcome": "verified"}),
        Some(ver(&marks2)),
        StatusCode::OK,
    )
    .await;
    let docs = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{aid}/documents"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let statuses: Vec<_> = docs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, vec!["verified", "superseded", "verified"]);
    let adm = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "verification_completed");

    // Approval through the WorkflowPort (auto-approval in Batch 3).
    let adm = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/submit-for-approval"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "approved");
    assert!(adm["processInstanceId"].is_string());
    assert_eq!(adm["approvedBy"], "system:auto-approval");
    // Confirmation needs financial clearance first.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/confirm"),
        json!({}),
        Some(ver(&adm)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let adm = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/financial-clearance"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["financeEvidence"]["kind"], "notApplicable");
    // Student creation needs confirmation.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/create-student"),
        json!({}),
        Some(ver(&adm)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let adm = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/confirm"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    let (adm, stu) = enrolled(
        &app,
        &admin,
        adm,
        json!({"batchCode": "2026", "sectionCode": "A", "rollNumber": "17"}),
    )
    .await;
    let sid = id(&stu);
    assert_eq!(adm["status"], "completed");
    assert_eq!(stu["status"], "active");
    assert!(stu["studentNumber"].as_str().unwrap().starts_with('S'));
    assert!(stu["subjectLinked"].as_bool().unwrap());
    let cur = &stu["currentContext"];
    assert_eq!(cur["institutionId"], json!(inst));
    assert_eq!(cur["programmeId"], json!(setup.programme_id));
    assert_eq!(
        cur["curriculumVersionId"],
        json!(setup.curriculum_version_id)
    );
    assert_eq!(cur["currentTermNo"], 1);
    assert_eq!(cur["sectionCode"], "A");
    assert!(cur["enrolmentNumber"].as_str().unwrap().starts_with("EN"));

    // Events of the whole flow, in the owner-local transactions.
    let adm_events = outbox_types(&pool, &aid).await;
    for e in [
        "admission.initiated",
        "admission.offer_accepted",
        "admission.documents_submitted",
        "admission.documents_verified",
        "admission.approval_requested",
        "admission.approved",
        "admission.financially_cleared",
        "admission.confirmed",
        "admission.student_created",
        "admission.completed",
    ] {
        assert!(adm_events.contains(&ev(e)), "missing {e} in {adm_events:?}");
    }
    let stu_events = outbox_types(&pool, &sid).await;
    assert!(
        stu_events.contains(&ev("student.created")),
        "{stu_events:?}"
    );
    assert_eq!(
        stu_events
            .iter()
            .filter(|e| **e == ev("student.status_changed"))
            .count(),
        5,
        "{stu_events:?}"
    );
    let ctx_events = outbox_types(&pool, cur["id"].as_str().unwrap()).await;
    assert!(
        ctx_events.contains(&ev("student.activated"))
            && ctx_events.contains(&ev("student.enrolled")),
        "{ctx_events:?}"
    );
    let actions = audit_actions(&pool, &aid).await;
    assert!(
        actions.contains(&"student.admission.approve".to_owned()),
        "{actions:?}"
    );
    let actor_kind: String = sqlx::query_scalar("SELECT actor_kind FROM platform.audit_log WHERE target_id = $1 AND action = 'student.admission.approve'")
        .bind(&aid).fetch_one(&pool).await.unwrap();
    assert_eq!(actor_kind, "system");
    let hist = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{aid}/history"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let to: Vec<_> = hist["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        to,
        vec![
            "initiated",
            "documents_submitted",
            "verification_completed",
            "approval_pending",
            "approved",
            "financially_cleared",
            "confirmed",
            "student_created",
            "completed"
        ]
    );
    let sh = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}/status-history"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let persona: Vec<_> = sh["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|h| h["scope"] == "student")
        .map(|h| h["toStatus"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        persona,
        vec!["activated", "enrolled", "academically_assigned", "active"]
    );

    // Admission documents moved onto the student record.
    let sdocs = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}/documents"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(sdocs["items"].as_array().unwrap().len(), 2);

    // Directory search + programme filter; faculty can read; students cannot.
    let dir = ok(
        &app,
        &faculty,
        "GET",
        &format!("{API}/institutions/{inst}/students?q=aar"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(dir["items"].as_array().unwrap().len(), 1);
    assert_eq!(dir["items"][0]["currentContext"]["status"], "active");
    let dir = ok(
        &app,
        &admin,
        "GET",
        &format!(
            "{API}/institutions/{inst}/students?programmeId={}",
            uuid::Uuid::now_v7()
        ),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(dir["items"].as_array().unwrap().is_empty());
    let (s, _) = call(
        &app,
        &student,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // Self-service resolves the student from the session subject.
    let me = ok(
        &app,
        &student,
        "GET",
        &format!("{API}/me/student"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(id(&me), sid);
    let (s, _) = call(
        &app,
        &admin,
        "GET",
        &format!("{API}/me/student"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Another tenant sees nothing.
    let (app2, _) = support::app(tenant()).await;
    let admin2 = login(&app2, "admin@sutra.dev").await;
    let (s, b) = call(
        &app2,
        &admin2,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "{b}");
    assert_eq!(b["status"], 404);
    let (s, _) = call(
        &app2,
        &admin2,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Idempotent replay of a create returns the same resource.
    let key = uuid::Uuid::now_v7().to_string();
    let mk = || {
        Request::post(format!("{API}/students/{sid}/documents"))
            .header("content-type", "application/json")
            .header("cookie", &admin.cookie)
            .header("x-csrf-token", &admin.csrf)
            .header("idempotency-key", &key)
            .body(Body::from(
                json!({"documentType": "photo", "title": "Photo"}).to_string(),
            ))
            .unwrap()
    };
    let (s1, _, b1) = send(&app, mk()).await;
    let (s2, _, b2) = send(&app, mk()).await;
    assert_eq!(s1, StatusCode::CREATED, "{b1}");
    assert_eq!(s2, StatusCode::CREATED);
    assert_eq!(b1["id"], b2["id"], "replayed, not created twice");
}

/// L3-C-007 with `ApprovalMode::AwaitDecision`: the admission waits in `approval_pending`
/// (to-do event emitted), a rejection needs a reason and cancels, a fresh admission is
/// approved explicitly; Finance evidence `outstanding` blocks clearance (409, nothing changes).
#[tokio::test]
async fn admission_approval_await_decision_and_finance_evidence() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Awaiting College").await;
    let waiting = custom_app(state(&pool, ApprovalMode::AwaitDecision), t);
    let inj = injected();

    // Bring two admissions to verification_completed through the normal app.
    let mut ready = vec![];
    for name in ["Ishaan", "Kavya"] {
        let adm = offered(
            &app,
            &admin,
            &setup,
            name,
            None,
            json!({"requiredDocuments": ["identity_proof"]}),
        )
        .await;
        let aid = id(&adm);
        let adm = ok(
            &app,
            &admin,
            "POST",
            &format!("{API}/admissions/{aid}/accept"),
            json!({}),
            Some(ver(&adm)),
            StatusCode::OK,
        )
        .await;
        let d = ok(
            &app,
            &admin,
            "POST",
            &format!("{API}/admissions/{aid}/documents"),
            json!({"documentType": "identity_proof", "title": "Passport"}),
            None,
            StatusCode::CREATED,
        )
        .await;
        ok(
            &app,
            &admin,
            "POST",
            &format!("{API}/admissions/{aid}/submit-documents"),
            json!({}),
            Some(ver(&adm)),
            StatusCode::OK,
        )
        .await;
        // Waiver also completes verification.
        ok(
            &app,
            &admin,
            "POST",
            &format!("{API}/admissions/{aid}/documents/{}/verify", id(&d)),
            json!({"outcome": "waived", "note": "known applicant"}),
            Some(ver(&d)),
            StatusCode::OK,
        )
        .await;
        ready.push(
            ok(
                &app,
                &admin,
                "GET",
                &format!("{API}/admissions/{aid}"),
                json!(null),
                None,
                StatusCode::OK,
            )
            .await,
        );
    }

    // AwaitDecision: stays approval_pending.
    let a1 = &ready[0];
    let p1 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/submit-for-approval", id(a1)),
        json!({}),
        Some(ver(a1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p1["status"], "approval_pending");
    assert!(p1["processInstanceId"].is_string());
    let evs = outbox_types(&pool, &id(a1)).await;
    assert!(
        evs.contains(&ev("admission.approval_requested"))
            && !evs.contains(&ev("admission.approved"))
    );
    // Clearance before the decision is refused.
    let (s, _) = call(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/financial-clearance", id(a1)),
        json!({}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Reject without a reason → 422; with a reason → cancelled.
    let (s, _) = call(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/decision", id(a1)),
        json!({"decision": "reject"}),
        Some(ver(&p1)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let r1 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/decision", id(a1)),
        json!({"decision": "reject", "reason": "eligibility not met"}),
        Some(ver(&p1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r1["status"], "cancelled");
    assert_eq!(r1["approvalDecision"], "reject");
    assert!(outbox_types(&pool, &id(a1))
        .await
        .contains(&ev("admission.cancelled")));
    // Decide again → 409.
    let (s, _) = call(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/decision", id(a1)),
        json!({"decision": "approve"}),
        Some(ver(&r1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Second admission: explicit approval.
    let a2 = &ready[1];
    let p2 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/submit-for-approval", id(a2)),
        json!({}),
        Some(ver(a2)),
        StatusCode::OK,
    )
    .await;
    let ap2 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/decision", id(a2)),
        json!({"decision": "approve"}),
        Some(ver(&p2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ap2["status"], "approved");
    assert_eq!(ap2["approvedBy"], "dev|admin@sutra.dev");
    assert_eq!(ap2["processInstanceId"], p2["processInstanceId"]);

    // Outstanding Finance evidence blocks the clearance decision.
    let mut st = state(&pool, ApprovalMode::AutoApprove);
    st.finance = Arc::new(OutstandingFinance);
    let strict = custom_app(st, t);
    let (s, b) = call(
        &strict,
        &inj,
        "POST",
        &format!("{API}/admissions/{}/financial-clearance", id(a2)),
        json!({}),
        Some(ver(&ap2)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(
        b["detail"]
            .as_str()
            .unwrap()
            .contains("admission fee unpaid"),
        "{b}"
    );
    let same = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{}", id(a2)),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(same["status"], "approved");
    // The stub (not applicable) permits it.
    let c = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/financial-clearance", id(a2)),
        json!({}),
        Some(ver(&same)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "financially_cleared");
}

/// L3-C-002 cancellation, L3-C-003 withdrawal, L3-C-006 offer expiry (explicit + sweep),
/// application rejection / withdrawal.
#[tokio::test]
async fn cancellation_withdrawal_and_offer_expiry() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Exit Polytechnic").await;
    let inst = setup.institution_id;

    // Application reject needs a reason; withdrawal of an application.
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/institutions/{inst}/admission-applications"),
        json!({"programmeId": setup.programme_id, "givenName": "Neel"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admission-applications/{}/submit", id(&a)),
        json!({}),
        Some(ver(&a)),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admission-applications/{}/reject", id(&a)),
        json!({}),
        Some(ver(&a)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let a = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admission-applications/{}/reject", id(&a)),
        json!({"reason": "below cut-off"}),
        Some(ver(&a)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a["status"], "rejected");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admission-applications/{}/offer", id(&a)),
        json!({}),
        Some(ver(&a)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(outbox_types(&pool, &id(&a))
        .await
        .contains(&ev("admission_application.rejected")));

    // C-002: cancellation with a reason, from initiated.
    let c = offered(&app, &admin, &setup, "Riya", None, json!({})).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/cancel", id(&c)),
        json!({}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "cancellation needs a reason"
    );
    let c = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/cancel", id(&c)),
        json!({"reason": "applicant request"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "cancelled");
    assert_eq!(c["statusReason"], "applicant request");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/cancel", id(&c)),
        json!({"reason": "again"}),
        Some(ver(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "terminal outcome preserved");

    // C-003: withdrawal only from confirmed; not cancellable once confirmed.
    let w = offered(&app, &admin, &setup, "Tara", None, json!({})).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/withdraw", id(&w)),
        json!({"reason": "x"}),
        Some(ver(&w)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let w = confirmed(&app, &admin, w).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/cancel", id(&w)),
        json!({"reason": "x"}),
        Some(ver(&w)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let w = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/withdraw", id(&w)),
        json!({"reason": "joined another college"}),
        Some(ver(&w)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(w["status"], "withdrawn");
    assert!(outbox_types(&pool, &id(&w))
        .await
        .contains(&ev("admission.withdrawn")));

    // C-006: expiry.
    let e1 = offered(&app, &admin, &setup, "Om", None, json!({})).await;
    let e2 = offered(&app, &admin, &setup, "Pia", None, json!({})).await;
    let e3 = offered(&app, &admin, &setup, "Qadir", None, json!({})).await;
    let e3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/accept", id(&e3)),
        json!({}),
        Some(ver(&e3)),
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/expire", id(&e1)),
        json!({}),
        Some(ver(&e1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "offer still valid");
    sqlx::query(
        "UPDATE student.admission SET offer_expires_on = DATE '2020-01-01' WHERE id = ANY($1)",
    )
    .bind(vec![
        id(&e1).parse::<uuid::Uuid>().unwrap(),
        id(&e2).parse().unwrap(),
        id(&e3).parse().unwrap(),
    ])
    .execute(&pool)
    .await
    .unwrap();
    let e1x = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/expire", id(&e1)),
        json!({}),
        Some(ver(&e1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e1x["status"], "expired");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/accept", id(&e2)),
        json!({}),
        Some(ver(&e2)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "an expired offer cannot be accepted"
    );
    let sweep = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/institutions/{inst}/admissions/expire-offers"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        sweep["expired"],
        json!([id(&e2)]),
        "accepted offers do not expire"
    );
    let sweep = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/institutions/{inst}/admissions/expire-offers"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(sweep["expired"], json!([]));
    assert!(outbox_types(&pool, &id(&e2))
        .await
        .contains(&ev("admission.expired")));
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/cancel", id(&e1x)),
        json!({"reason": "x"}),
        Some(ver(&e1x)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "expired stays expired");
    let list = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/institutions/{inst}/admissions?status=expired"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
}

/// L3-C-004 revocation (step-up permission, context terminated), L3-C-005 readmission (new
/// Admission Record, historical record kept, persona reused, predecessor context), L3-C-019
/// archival guard and permanent record closure.
#[tokio::test]
async fn revocation_readmission_and_archival() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Revocation Institute").await;
    let (adm, stu) = admit(&app, &admin, &setup, "Vihaan", None).await;
    let sid = id(&stu);
    let aid = id(&adm);
    let ctx1 = stu["currentContext"]["id"].as_str().unwrap().to_owned();

    // Archival needs every context closed.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/archive"),
        json!({"reason": "closure"}),
        Some(ver(&stu)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Revocation: reason + evidence required; faculty forbidden.
    let (s, _) = call(
        &app,
        &faculty,
        "POST",
        &format!("{API}/admissions/{aid}/revoke"),
        json!({"reason": "x", "evidence": "y"}),
        Some(ver(&adm)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/revoke"),
        json!({"reason": "forged marksheet", "evidence": " "}),
        Some(ver(&adm)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/revoke"),
        json!({"reason": "forged marksheet", "evidence": "board verification letter 42"}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "revoked");
    let s1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let c1 = s1["contexts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == json!(ctx1))
        .unwrap()
        .clone();
    assert_eq!(c1["status"], "terminated");
    assert_eq!(s1["status"], "active", "persona unchanged until archival");
    let ce: Vec<String> = sqlx::query_scalar("SELECT cause_event_type FROM student.student_status_history WHERE student_context_id = $1 ORDER BY occurred_at DESC LIMIT 1")
        .bind(ctx1.parse::<uuid::Uuid>().unwrap()).fetch_all(&pool).await.unwrap();
    assert_eq!(ce, vec![ev("admission.revoked")]);

    // C-005: readmission from the revoked admission → new record, same persona.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/readmit"),
        json!({"reason": " "}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let re = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/readmit"),
        json!({"reason": "appeal upheld", "requiredDocuments": ["identity_proof"]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(re["status"], "initiated");
    assert_eq!(re["previousAdmissionId"], json!(aid));
    assert_eq!(re["studentId"], json!(sid));
    assert_eq!(re["enrolmentType"], "readmission");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{aid}/readmit"),
        json!({"reason": "again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no duplicate open admission: {b}");
    let old = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/admissions/{aid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(old["status"], "revoked", "history preserved");
    let re = confirmed(&app, &admin, re).await;
    let (re, s2) = enrolled(&app, &admin, re, json!({})).await;
    assert_eq!(re["status"], "completed");
    assert_eq!(id(&s2), sid, "same canonical student");
    assert_eq!(s2["currentContext"]["enrolmentType"], "readmission");
    assert_eq!(s2["currentContext"]["predecessorContextId"], json!(ctx1));
    assert_eq!(s2["contexts"].as_array().unwrap().len(), 2);
    let persons: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student WHERE origin_admission_id = $1::uuid",
    )
    .bind(&aid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(persons, 1);

    // Close the second context (revoke again), then archive (C-019).
    let r2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/revoke", id(&re)),
        json!({"reason": "left", "evidence": "letter"}),
        Some(ver(&re)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r2["status"], "revoked");
    let cur = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/archive"),
        json!({}),
        Some(ver(&cur)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "archival needs a reason"
    );
    let arch = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/archive"),
        json!({"reason": "permanent record closure"}),
        Some(ver(&cur)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(arch["status"], "archived");
    assert!(outbox_types(&pool, &sid)
        .await
        .contains(&ev("student.archived")));
    // Archived is read-only; nothing was deleted.
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/students/{sid}/profile"),
        json!({"preferredName": "V"}),
        Some(ver(&arch)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/archive"),
        json!({"reason": "again"}),
        Some(ver(&arch)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let still = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(still["contexts"].as_array().unwrap().len(), 2);

    // Readmission of an archived student (completed? no — revoked admission): re-entry reactivates the persona.
    let re3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/readmit", id(&r2)),
        json!({"reason": "re-entry", "requiredDocuments": ["identity_proof"]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let re3 = confirmed(&app, &admin, re3).await;
    let (_, s3) = enrolled(&app, &admin, re3, json!({})).await;
    assert_eq!(s3["status"], "active", "archived → activated → … → active");
}

/// L3-C-008 enrolment update, L3-C-009 assignment, L3-C-010/011 suspension + restoration,
/// L3-C-014 profile, L3-C-015 communication preferences (staff + self-service).
#[tokio::test]
async fn record_maintenance_suspension_profile_preferences() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Record College").await;
    let (_, stu) = admit(
        &app,
        &admin,
        &setup,
        "Aarav",
        Some("dev|aarav.desai@sutra.dev"),
    )
    .await;
    let sid = id(&stu);
    let cid = stu["currentContext"]["id"].as_str().unwrap().to_owned();

    // C-008 enrolment update; no-op → 422; programme/batch are not enrolment members.
    let p = format!("{API}/students/{sid}/enrolment");
    let (s, _) = call(&app, &admin, "PATCH", &p, json!({}), Some(ctx_ver(&stu))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"batchCode": "X"}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert!(s.is_client_error(), "{s}");
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"rollNumber": "42"}),
        Some(ctx_ver(&stu) - 1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let stu = ok(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"rollNumber": "42", "enrolmentType": "lateral_entry", "reason": "corrected"}),
        Some(ctx_ver(&stu)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stu["currentContext"]["rollNumber"], "42");
    assert_eq!(stu["currentContext"]["enrolmentType"], "lateral_entry");
    assert!(outbox_types(&pool, &cid)
        .await
        .contains(&ev("student.enrolment_updated")));

    // C-009 assignment: a programme change needs programme migration (3B).
    let p = format!("{API}/students/{sid}/assignment");
    let (s, b) = call(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"programmeId": uuid::Uuid::now_v7()}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        b.to_string().contains("programme-migration-required"),
        "{b}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"orgUnitId": uuid::Uuid::now_v7()}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown org unit");
    let stu = ok(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"sectionCode": "B", "batchCode": "2026-B"}),
        Some(ctx_ver(&stu)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stu["currentContext"]["sectionCode"], "B");
    let rh = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}/record-history"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let kinds: Vec<_> = rh["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds, vec!["enrolment", "enrolment", "assignment"]);
    assert_eq!(rh["items"][2]["before"]["sectionCode"], "A");

    // C-010 suspension.
    let p = format!("{API}/students/{sid}/suspend");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &p,
        json!({"reason": "x", "category": "bogus"}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let stu = ok(
        &app,
        &admin,
        "POST",
        &p,
        json!({"reason": "disciplinary hearing", "category": "disciplinary"}),
        Some(ctx_ver(&stu)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stu["currentContext"]["status"], "suspended");
    assert_eq!(stu["status"], "suspended");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &p,
        json!({"reason": "again"}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &format!("{API}/students/{sid}/assignment"),
        json!({"sectionCode": "C"}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "suspended students are not reassigned"
    );
    // C-011 restoration (financial suspensions consult Finance evidence).
    let stu = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/restore"),
        json!({"reason": "hearing closed"}),
        Some(ctx_ver(&stu)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(stu["currentContext"]["status"], "active");
    assert_eq!(stu["status"], "active");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/restore"),
        json!({"reason": "again"}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let stu = ok(
        &app,
        &admin,
        "POST",
        &p,
        json!({"reason": "fees", "category": "financial"}),
        Some(ctx_ver(&stu)),
        StatusCode::OK,
    )
    .await;
    let mut st = state(&pool, ApprovalMode::AutoApprove);
    st.finance = Arc::new(OutstandingFinance);
    let strict = custom_app(st, t);
    let (s, _) = call(
        &strict,
        &injected(),
        "POST",
        &format!("{API}/students/{sid}/restore"),
        json!({"reason": "paid?"}),
        Some(ctx_ver(&stu)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "Finance evidence blocks restoring a financial suspension"
    );
    let stu = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/restore"),
        json!({"reason": "cleared"}),
        Some(ctx_ver(&stu)),
        StatusCode::OK,
    )
    .await;
    let sus = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}/suspensions"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(sus["items"].as_array().unwrap().len(), 2);
    assert!(sus["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|x| x["status"] == "restored"));
    let evs = outbox_types(&pool, &cid).await;
    assert!(evs.contains(&ev("student.suspended")) && evs.contains(&ev("student.restored")));

    // C-014 profile: identity attributes are rejected here; persona version guards.
    let p = format!("{API}/students/{sid}/profile");
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"givenName": "Changed"}),
        Some(ver(&stu)),
    )
    .await;
    assert!(s.is_client_error(), "{s}");
    let (s, _) = call(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"primaryEmail": "not-an-email"}),
        Some(ver(&stu)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let prof = ok(
        &app,
        &admin,
        "PATCH",
        &p,
        json!({"preferredName": "Aaru", "primaryEmail": "aarav@college.in"}),
        Some(ver(&stu)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(prof["preferredName"], "Aaru");
    assert_eq!(prof["givenName"], "Aarav");
    assert_eq!(prof["status"], "active");
    assert!(outbox_types(&pool, &sid)
        .await
        .contains(&ev("student.profile_updated")));

    // C-015 communication preferences (staff + self).
    let p = format!("{API}/students/{sid}/communication-preferences");
    let cp = ok(&app, &admin, "GET", &p, json!(null), None, StatusCode::OK).await;
    let body = json!({"preferredChannel": "sms", "language": "mr-IN", "emailOptIn": true, "smsOptIn": false, "whatsappOptIn": false, "marketingOptIn": false});
    let (s, _) = call(&app, &admin, "PUT", &p, body.clone(), Some(ver(&cp))).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "preferred channel must be opted in"
    );
    let mut good = body.clone();
    good["smsOptIn"] = json!(true);
    let cp2 = ok(
        &app,
        &admin,
        "PUT",
        &p,
        good,
        Some(ver(&cp)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cp2["preferredChannel"], "sms");
    assert_eq!(cp2["version"], ver(&cp) + 1);
    let mine = ok(
        &app,
        &student,
        "GET",
        &format!("{API}/me/communication-preferences"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(mine["language"], "mr-IN");
    let mine = ok(&app, &student, "PUT", &format!("{API}/me/communication-preferences"), json!({"preferredChannel": "in_app", "language": "en", "emailOptIn": true, "smsOptIn": true, "whatsappOptIn": false, "marketingOptIn": false}), Some(ver(&mine)), StatusCode::OK).await;
    assert_eq!(mine["preferredChannel"], "in_app");
    assert!(outbox_types(&pool, &sid)
        .await
        .contains(&ev("student.communication_preferences_updated")));
    let (s, _) = call(&app, &student, "PUT", &p, json!({"preferredChannel": "email", "language": "en", "emailOptIn": true, "smsOptIn": false, "whatsappOptIn": false, "marketingOptIn": false}), Some(1)).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students use the me endpoint");
}

/// L3-C-012 intra-institute reassignment and L3-C-013 inter-institute transfer with context
/// re-establishment (auto-approval + `AwaitDecision`).
#[tokio::test]
async fn intra_and_inter_institute_transfers() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Source University").await;
    let dest = b3::academic_setup(&app, &admin, t, "Destination University").await;
    let (_, stu) = admit(&app, &admin, &setup, "Diya", None).await;
    let sid = id(&stu);
    let src_ctx = stu["currentContext"]["id"].as_str().unwrap().to_owned();
    let campus = b2::active_campus(
        &app,
        &admin,
        &setup.institution_id.to_string(),
        &code("NC"),
        true,
    )
    .await;

    // Intra: programme change refused; foreign campus refused; then section + campus move.
    let p = format!("{API}/students/{sid}/transfers");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &p,
        json!({"kind": "intra_institute", "toProgrammeId": uuid::Uuid::now_v7(), "reason": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &p,
        json!({"kind": "intra_institute", "toCampusId": uuid::Uuid::now_v7(), "reason": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let tr = ok(&app, &admin, "POST", &p, json!({"kind": "intra_institute", "toCampusId": campus["id"], "toSectionCode": "C", "reason": "campus capacity"}), None, StatusCode::CREATED).await;
    assert_eq!(tr["status"], "requested");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &p,
        json!({"kind": "intra_institute", "toSectionCode": "D", "reason": "dup"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open transfer per context");
    let tr = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/transfers/{}/submit", id(&tr)),
        json!({}),
        Some(ver(&tr)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tr["status"], "completed");
    assert_eq!(tr["toContextId"], json!(src_ctx));
    let s1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(s1["currentContext"]["sectionCode"], "C");
    assert_eq!(s1["currentContext"]["campusId"], campus["id"]);
    assert_eq!(s1["currentContext"]["status"], "active");
    assert!(outbox_types(&pool, &sid)
        .await
        .contains(&ev("student.transferred")));

    // Inter with AwaitDecision: stays under review; reject; then a new request auto-approved.
    let inter = json!({"kind": "inter_institute", "toInstitutionId": dest.institution_id, "toProgrammeId": dest.programme_id, "toSectionCode": "A", "reason": "family relocation"});
    let (s, _) = call(&app, &admin, "POST", &p, json!({"kind": "inter_institute", "toInstitutionId": setup.institution_id, "toProgrammeId": setup.programme_id, "reason": "x"}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "same institution is intra"
    );
    let (s, _) = call(&app, &admin, "POST", &p, json!({"kind": "inter_institute", "toInstitutionId": dest.institution_id, "toProgrammeId": setup.programme_id, "reason": "x"}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "programme must belong to the destination"
    );
    let waiting = custom_app(state(&pool, ApprovalMode::AwaitDecision), t);
    let inj = injected();
    let t1 = ok(
        &app,
        &admin,
        "POST",
        &p,
        inter.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;
    let t1 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/transfers/{}/submit", id(&t1)),
        json!({}),
        Some(ver(&t1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t1["status"], "under_review");
    let t1 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/transfers/{}/decision", id(&t1)),
        json!({"decision": "reject", "reason": "seat unavailable"}),
        Some(ver(&t1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t1["status"], "rejected");
    let t2 = ok(&app, &admin, "POST", &p, inter, None, StatusCode::CREATED).await;
    let t2 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/transfers/{}/submit", id(&t2)),
        json!({}),
        Some(ver(&t2)),
        StatusCode::OK,
    )
    .await;
    let t2 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/transfers/{}/decision", id(&t2)),
        json!({"decision": "approve"}),
        Some(ver(&t2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(t2["status"], "completed");
    let new_ctx = t2["toContextId"].as_str().unwrap().to_owned();
    let s2 = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(id(&s2), sid, "same canonical student");
    let cs = s2["contexts"].as_array().unwrap();
    let old = cs.iter().find(|c| c["id"] == json!(src_ctx)).unwrap();
    let new = cs.iter().find(|c| c["id"] == json!(new_ctx)).unwrap();
    assert_eq!(old["status"], "transferred");
    assert_eq!(new["status"], "pending_activation");
    assert_eq!(new["enrolmentType"], "transfer_in");
    assert_eq!(new["predecessorContextId"], json!(src_ctx));
    assert_eq!(new["institutionId"], json!(dest.institution_id));
    assert_eq!(
        new["curriculumVersionId"],
        json!(dest.curriculum_version_id)
    );
    // Enrol at the destination: context active, persona stays active.
    let s3 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/enrol"),
        json!({}),
        Some(ver(new)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s3["currentContext"]["id"], json!(new_ctx));
    assert_eq!(s3["currentContext"]["status"], "active");
    assert_eq!(s3["status"], "active");
    let dir = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/institutions/{}/students", dest.institution_id),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(dir["items"].as_array().unwrap().len(), 1);
}

/// L3-C-016 identity correction (auto + AwaitDecision reject) and L3-C-017 duplicate
/// resolution (same-institution live contexts conflict, merge after one is closed).
#[tokio::test]
async fn identity_correction_and_duplicate_resolution() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Identity College").await;
    let (_, stu) = admit(&app, &admin, &setup, "Sneha", None).await;
    let sid = id(&stu);
    let number = stu["studentNumber"].clone();

    let p = format!("{API}/students/{sid}/identity-corrections");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &p,
        json!({"changes": {}, "evidence": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &p,
        json!({"changes": {"studentNumber": "S1"}, "evidence": "x"}),
        None,
    )
    .await;
    assert!(s.is_client_error(), "student number is immutable: {s}");
    let ic = ok(&app, &admin, "POST", &p, json!({"changes": {"familyName": "Kulkarni-Rao", "dateOfBirth": "2007-04-14"}, "evidence": "birth certificate 1234"}), None, StatusCode::CREATED).await;
    assert_eq!(ic["status"], "requested");
    let ic = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/identity-corrections/{}/submit", id(&ic)),
        json!({}),
        Some(ver(&ic)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ic["status"], "completed");
    assert_eq!(ic["previous"]["familyName"], "Kulkarni");
    let s1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(s1["familyName"], "Kulkarni-Rao");
    assert_eq!(s1["dateOfBirth"], "2007-04-14");
    assert_eq!(s1["studentNumber"], number, "identifiers unchanged");
    assert!(outbox_types(&pool, &sid)
        .await
        .contains(&ev("student.identity_corrected")));
    // AwaitDecision: under review, then rejected → persona untouched.
    let waiting = custom_app(state(&pool, ApprovalMode::AwaitDecision), t);
    let inj = injected();
    let ic2 = ok(
        &app,
        &admin,
        "POST",
        &p,
        json!({"changes": {"givenName": "Snehal"}, "evidence": "affidavit"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let ic2 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/identity-corrections/{}/submit", id(&ic2)),
        json!({}),
        Some(ver(&ic2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ic2["status"], "under_review");
    let ic2 = ok(
        &waiting,
        &inj,
        "POST",
        &format!("{API}/identity-corrections/{}/decision", id(&ic2)),
        json!({"decision": "reject", "reason": "insufficient evidence"}),
        Some(ver(&ic2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ic2["status"], "rejected");
    let s1 = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(s1["givenName"], "Sneha");

    // C-017: a duplicate admitted twice in the same institution.
    let (dup_adm, dup) = admit(&app, &admin, &setup, "Sneha", None).await;
    let did = id(&dup);
    let cands = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{did}/duplicate-candidates"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(
        cands["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["id"] == json!(sid))
            || cands["items"].as_array().unwrap().is_empty()
    );
    let dc = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/duplicate-cases"),
        json!({"survivorStudentId": sid, "duplicateStudentId": did, "evidence": "same Aadhaar"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/duplicate-cases"),
        json!({"survivorStudentId": did, "duplicateStudentId": sid, "evidence": "again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open case per pair");
    let (s, b) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/duplicate-cases/{}/submit", id(&dc)),
        json!({}),
        Some(ver(&dc)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "both live in the same institution: {b}"
    );
    // Close the duplicate's context through revocation, then merge.
    ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/admissions/{}/revoke", id(&dup_adm)),
        json!({"reason": "duplicate record", "evidence": "case"}),
        Some(ver(&dup_adm)),
        StatusCode::OK,
    )
    .await;
    let dc = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/duplicate-cases/{}", id(&dc)),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    let dc = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/duplicate-cases/{}/submit", id(&dc)),
        json!({}),
        Some(ver(&dc)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(dc["status"], "completed");
    let d2 = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{did}"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(d2["status"], "archived");
    assert_eq!(d2["mergedIntoId"], json!(sid));
    assert_eq!(
        d2["contexts"].as_array().unwrap().len(),
        1,
        "closed contexts stay as lineage"
    );
    let survivor_docs = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}/documents"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        survivor_docs["items"].as_array().unwrap().len(),
        4,
        "documents follow the survivor"
    );
    assert!(outbox_types(&pool, &sid)
        .await
        .contains(&ev("student.duplicate_resolved")));
    let list = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/duplicate-cases?status=completed"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
}

/// L3-C-018 document / certificate requests (self-service + staff), student documents.
#[tokio::test]
async fn document_requests_and_student_documents() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, t, "Documents College").await;
    let inst = setup.institution_id;
    let (s, _) = call(
        &app,
        &student,
        "GET",
        &format!("{API}/me/student"),
        json!(null),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "no linked student yet");
    let (_, stu) = admit(
        &app,
        &admin,
        &setup,
        "Aarav",
        Some("dev|aarav.desai@sutra.dev"),
    )
    .await;
    let sid = id(&stu);

    // Self-service request; unsupported type 422; duplicate active request 409.
    let p = format!("{API}/me/document-requests");
    let (s, _) = call(
        &app,
        &student,
        "POST",
        &p,
        json!({"documentType": "degree_certificate"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let r = ok(
        &app,
        &student,
        "POST",
        &p,
        json!({"documentType": "bonafide_certificate", "purpose": "bank account", "copies": 2}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["status"], "requested");
    assert_eq!(r["channel"], "self_service");
    let (s, _) = call(
        &app,
        &student,
        "POST",
        &p,
        json!({"documentType": "bonafide_certificate"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(outbox_types(&pool, &id(&r))
        .await
        .contains(&ev("document_request.submitted")));
    // Staff queue + processing.
    let q = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/institutions/{inst}/document-requests?status=requested"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(q["items"].as_array().unwrap().len(), 1);
    let (s, _) = call(
        &app,
        &student,
        "POST",
        &format!("{API}/document-requests/{}/start", id(&r)),
        json!({}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/document-requests/{}/start", id(&r)),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "in_progress");
    let r = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/document-requests/{}/issue", id(&r)),
        json!({"issuedReference": "DOC-2026-0001"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "issued");
    assert!(r["issuedOn"].is_string());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/document-requests/{}/cancel", id(&r)),
        json!({}),
        Some(ver(&r)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert!(outbox_types(&pool, &id(&r))
        .await
        .contains(&ev("document_request.issued")));
    let mine = ok(
        &app,
        &student,
        "GET",
        &format!("{API}/me/documents"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert!(mine["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|d| d["source"] == "request"));
    // Staff-raised request rejected with a reason; self cancel of an own request.
    let r2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/document-requests"),
        json!({"documentType": "character_certificate"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/document-requests/{}/reject", id(&r2)),
        json!({}),
        Some(ver(&r2)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let r2 = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/document-requests/{}/reject", id(&r2)),
        json!({"reason": "pending disciplinary review"}),
        Some(ver(&r2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r2["status"], "rejected");
    let r3 = ok(
        &app,
        &student,
        "POST",
        &p,
        json!({"documentType": "enrolment_letter"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let r3 = ok(
        &app,
        &student,
        "POST",
        &format!("{API}/me/document-requests/{}/cancel", id(&r3)),
        json!({}),
        Some(ver(&r3)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r3["status"], "cancelled");
    let all = ok(&app, &student, "GET", &p, json!(null), None, StatusCode::OK).await;
    assert_eq!(all["items"].as_array().unwrap().len(), 3);
    // A Finance-gated certificate is refused while dues are outstanding.
    let mut st = state(&pool, ApprovalMode::AutoApprove);
    st.finance = Arc::new(OutstandingFinance);
    let strict = custom_app(st, t);
    let (s, _) = call(
        &strict,
        &injected(),
        "POST",
        &format!("{API}/students/{sid}/document-requests"),
        json!({"documentType": "migration_certificate"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Student documents: add + verify (note required unless verified).
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/documents"),
        json!({"documentType": "transfer_certificate", "title": "TC from school"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/documents/{}/verify", id(&d)),
        json!({"outcome": "rejected"}),
        Some(ver(&d)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let d = ok(
        &app,
        &admin,
        "POST",
        &format!("{API}/students/{sid}/documents/{}/verify", id(&d)),
        json!({"outcome": "verified"}),
        Some(ver(&d)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "verified");
    let docs = ok(
        &app,
        &admin,
        "GET",
        &format!("{API}/students/{sid}/documents"),
        json!(null),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(docs["items"].as_array().unwrap().len(), 4);
}
