//! Batch 4 slice **4C** DB tests (flexible / external learning, certificates, credit mapping
//! and recognition posted through `CreditPostingPort`, reconciliation, exception cases,
//! session archival, course resources). Turns the L4 section-d behaviour specs
//! L4-D-034…050 into executable checks; offerings, sections, faculty and rosters are seeded
//! through `support::b4` (no dependency on 4A). Covers `ApprovalMode::AwaitDecision`,
//! faculty-scoped access, permissions and tenant isolation; every test runs in its own tenant.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use chrono::{Duration, Utc};
use serde_json::{json, Value};
use std::sync::Arc;
use support::*;
use sutra_academic::shared::approval::ApprovalMode;
use sutra_academic::shared::Actor;
use sutra_academic::AcademicState;
use sutra_kernel::auth::{AuthContext, MfaLevel, Role};
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const T_SETUP: &str = "0192f5a0-7c3e-7000-8000-0000000b4c01";
const T_SEATS: &str = "0192f5a0-7c3e-7000-8000-0000000b4c02";
const T_RECOGNITION: &str = "0192f5a0-7c3e-7000-8000-0000000b4c03";
const T_CLOSURE: &str = "0192f5a0-7c3e-7000-8000-0000000b4c04";
const T_RESOURCES: &str = "0192f5a0-7c3e-7000-8000-0000000b4c05";
const T_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b4c06";
const T_OTHER: &str = "0192f5a0-7c3e-7000-8000-0000000b4c07";

const A: &str = "/api/v1/academic";
const ADMIN: &str = "dev|admin@sutra.dev";

async fn expect(app: &Router, req: Request<Body>, status: StatusCode) -> Value {
    let (s, _, body) = send(app, req).await;
    assert_eq!(s, status, "{body}");
    body
}

fn ver(v: &Value) -> i64 {
    v["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no version in {v}"))
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

async fn post(
    app: &Router,
    who: &Login,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    status: StatusCode,
) -> Value {
    expect(
        app,
        write(who, "POST", &format!("{A}{path}"), body, if_match),
        status,
    )
    .await
}

async fn get_ok(app: &Router, who: &Login, path: &str) -> Value {
    expect(app, get(who, &format!("{A}{path}")), StatusCode::OK).await
}

async fn events(pool: &sqlx::PgPool, institution: Uuid, suffix: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE institution_id = $1 AND event_type = $2",
    )
    .bind(institution)
    .bind(format!("sutraerp.academic.{suffix}"))
    .fetch_one(pool)
    .await
    .unwrap()
}

fn today() -> String {
    // the dev institutions run on Asia/Kolkata; one day of slack either side is never needed
    // because every date used below is at least "today" in UTC+05:30 or later.
    (Utc::now() + Duration::hours(6)).date_naive().to_string()
}

fn in_days(n: i64) -> String {
    (Utc::now() + Duration::hours(6) + Duration::days(n))
        .date_naive()
        .to_string()
}

struct Fixture {
    app: Router,
    pool: sqlx::PgPool,
    admin: Login,
    setup: b4::DeliverySetup,
}

impl Fixture {
    fn inst(&self) -> Uuid {
        self.setup.base.institution_id
    }
    fn term(&self) -> Uuid {
        self.setup.term_id
    }
}

async fn fixture(tenant: &'static str, name: &str) -> Fixture {
    let (app, pool) = app(tenant).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b4::academic_setup(&app, &admin, &pool, tenant, name).await;
    Fixture {
        app,
        pool,
        admin,
        setup,
    }
}

/// Effective policy (default rules overridable).
async fn effective_policy(f: &Fixture, rules: Value) -> Value {
    let mut r = json!({"maxConcurrentEnrolments": 3, "maxCreditsPerTerm": 20, "requireHumanApproval": false,
        "waitlistValidityDays": 14, "withdrawalAllowedUntil": "registered"});
    if let (Value::Object(m), Value::Object(x)) = (&mut r, rules) {
        m.extend(x);
    }
    let p = post(
        &f.app,
        &f.admin,
        &format!("/institutions/{}/flexible-learning-policies", f.inst()),
        json!({"name": "Flexible policy", "rules": r, "effectiveFrom": today()}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let p = post(
        &f.app,
        &f.admin,
        &format!("/flexible-learning-policies/{}/submit", id(&p)),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "effective", "{p}");
    p
}

async fn active_provider(f: &Fixture, code: &str) -> Value {
    let p = post(&f.app, &f.admin, &format!("/institutions/{}/learning-providers", f.inst()), json!({
        "code": code, "name": format!("{code} platform"), "providerKind": "government_platform",
        "accreditationRef": "MoE/2026/17", "accreditationValidUntil": in_days(900), "modes": ["nptel", "mooc"]}), None, StatusCode::CREATED).await;
    let p = post(
        &f.app,
        &f.admin,
        &format!("/learning-providers/{}/submit", id(&p)),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "active", "{p}");
    p
}

async fn active_opportunity(
    f: &Fixture,
    provider: &Value,
    code: &str,
    capacity: i32,
    extra: Value,
) -> Value {
    let mut body = json!({"code": code, "title": format!("{code} course"), "mode": "nptel", "providerId": id(provider),
        "academicTermId": f.term(), "credits": 4, "minHours": 20, "capacity": capacity,
        "enrolmentOpensOn": in_days(-1), "enrolmentClosesOn": in_days(30)});
    if let (Value::Object(m), Value::Object(x)) = (&mut body, extra) {
        m.extend(x);
    }
    let o = post(
        &f.app,
        &f.admin,
        &format!("/institutions/{}/learning-opportunities", f.inst()),
        body,
        None,
        StatusCode::CREATED,
    )
    .await;
    let o = post(
        &f.app,
        &f.admin,
        &format!("/learning-opportunities/{}/submit", id(&o)),
        json!({}),
        Some(ver(&o)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "active", "{o}");
    o
}

async fn opportunity(f: &Fixture, oid: &str) -> Value {
    get_ok(&f.app, &f.admin, &format!("/learning-opportunities/{oid}")).await
}

async fn enrol(f: &Fixture, oid: &str, student: Uuid, priority: i32) -> Value {
    let e = post(
        &f.app,
        &f.admin,
        &format!("/learning-opportunities/{oid}/enrolments"),
        json!({"studentId": student, "priority": priority}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(e["status"], "eligible", "{e}");
    e
}

async fn enrolment(f: &Fixture, eid: &str) -> Value {
    get_ok(&f.app, &f.admin, &format!("/flexible-enrolments/{eid}")).await
}

async fn seat_cmd(f: &Fixture, oid: &str, cmd: &str, status: StatusCode) -> Value {
    let o = opportunity(f, oid).await;
    post(
        &f.app,
        &f.admin,
        &format!("/learning-opportunities/{oid}/{cmd}"),
        json!({}),
        Some(ver(&o)),
        status,
    )
    .await
}

/// Drives an allocated enrolment to `registered` through the provider hand-off.
async fn register_with_provider(f: &Fixture, eid: &str, reference: &str) -> Value {
    let e = enrolment(f, eid).await;
    let e = post(
        &f.app,
        &f.admin,
        &format!("/flexible-enrolments/{eid}/register"),
        json!({}),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "allocated");
    assert!(
        e["registrations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["status"] == "requested"),
        "{e}"
    );
    let e = post(&f.app, &f.admin, &format!("/flexible-enrolments/{eid}/registration-outcome"),
        json!({"outcome": "accepted", "providerReference": reference, "evidenceKey": format!("ack-{reference}")}), Some(ver(&e)), StatusCode::OK).await;
    assert_eq!(e["status"], "registered", "{e}");
    e
}

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
        privileged_mfa_enforced: false,
    }
}

fn actor(subject: &str) -> Actor {
    Actor {
        id: Some(subject.into()),
        correlation_id: None,
    }
}

// ------------------------------------------------------------------------- setup + lifecycle

/// L4-D-045 (versioned policy, supersession, validation), L4-D-044 (provider onboarding:
/// uniqueness, accreditation readiness, suspension blocks enrolment), L4-D-040 (opportunity
/// establishment against authoritative references, readiness before activation), L4-D-041
/// (suspension with reason, no new enrolment, history), permissions + tenant isolation.
#[tokio::test]
async fn policy_provider_and_opportunity_lifecycle() {
    let f = fixture(T_SETUP, "Flexible Setup University").await;
    let (app, admin, inst) = (&f.app, &f.admin, f.inst());
    let meera = login(app, "meera.shah@sutra.dev").await;
    let aarav = login(app, "aarav.desai@sutra.dev").await;

    // ---- policy (L4-D-045)
    post(app, admin, &format!("/institutions/{inst}/flexible-learning-policies"),
        json!({"name": "Bad", "effectiveFrom": today(), "rules": {"maxConcurrentEnrolments": 0, "maxCreditsPerTerm": 8,
            "waitlistValidityDays": 14, "withdrawalAllowedUntil": "never"}}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    post(app, admin, &format!("/institutions/{inst}/flexible-learning-policies"),
        json!({"name": "Past", "effectiveFrom": "2020-01-01", "rules": {"maxConcurrentEnrolments": 2, "maxCreditsPerTerm": 8,
            "waitlistValidityDays": 14, "withdrawalAllowedUntil": "registered"}}), None, StatusCode::UNPROCESSABLE_ENTITY).await;
    let v1 = effective_policy(&f, json!({})).await;
    assert_eq!(v1["versionNo"], 1);
    let v2 = effective_policy(&f, json!({"maxConcurrentEnrolments": 2})).await;
    assert_eq!(v2["versionNo"], 2);
    assert_eq!(v2["supersedesId"], v1["id"]);
    let v1 = get_ok(
        app,
        admin,
        &format!("/flexible-learning-policies/{}", id(&v1)),
    )
    .await;
    assert_eq!(v1["status"], "superseded", "prior versions are retained");
    assert_eq!(
        events(&f.pool, inst, "flexible_learning_policy.effective").await,
        2
    );

    // ---- provider onboarding (L4-D-044)
    post(
        app,
        &meera,
        &format!("/institutions/{inst}/learning-providers"),
        json!({"code": "X", "name": "X", "providerKind": "other"}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let draft = post(app, admin, &format!("/institutions/{inst}/learning-providers"),
        json!({"code": "SWAYAM", "name": "SWAYAM", "providerKind": "government_platform", "modes": ["swayam"]}), None, StatusCode::CREATED).await;
    assert_eq!(draft["status"], "proposed");
    // BH-004/005: no accreditation evidence → not ready for review
    post(
        app,
        admin,
        &format!("/learning-providers/{}/submit", id(&draft)),
        json!({}),
        Some(ver(&draft)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    // BH-001: uniqueness
    post(
        app,
        admin,
        &format!("/institutions/{inst}/learning-providers"),
        json!({"code": "swayam", "name": "dup", "providerKind": "other"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let nptel = active_provider(&f, "NPTEL").await;
    assert!(nptel["processInstanceId"].is_string());
    assert_eq!(
        nptel["history"].as_array().unwrap().len(),
        3,
        "proposed → under_review → active: {nptel}"
    );
    assert_eq!(
        events(&f.pool, inst, "learning_provider.activated").await,
        1
    );

    // ---- opportunity establishment (L4-D-040)
    let opps = format!("/institutions/{inst}/learning-opportunities");
    let base = json!({"code": "NPTEL-DS", "title": "Data Science", "mode": "nptel", "providerId": id(&nptel),
        "academicTermId": f.term(), "credits": 4, "capacity": 10});
    let mut bad = base.clone();
    bad["mode"] = json!("internship");
    post(
        app,
        admin,
        &opps,
        bad,
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await; // provider scope
    let mut bad = base.clone();
    bad["academicTermId"] = json!(Uuid::now_v7());
    post(
        app,
        admin,
        &opps,
        bad,
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await; // unknown term
    let mut bad = base.clone();
    bad["providerId"] = json!(id(&draft));
    bad["mode"] = json!("swayam");
    bad["code"] = json!("SWAYAM-DS");
    let swayam_draft = post(app, admin, &opps, bad, None, StatusCode::CREATED).await; // proposed provider may be referenced …
    post(
        app,
        admin,
        &format!("/learning-opportunities/{}/submit", id(&swayam_draft)),
        json!({}),
        Some(ver(&swayam_draft)),
        StatusCode::CONFLICT,
    )
    .await; // … but not activated
    let o = post(app, admin, &opps, base.clone(), None, StatusCode::CREATED).await;
    assert_eq!(o["status"], "draft");
    post(app, admin, &opps, base.clone(), None, StatusCode::CONFLICT).await; // duplicate code in term
                                                                             // BH-007: readiness — the enrolment window must be set
    post(
        app,
        admin,
        &format!("/learning-opportunities/{}/submit", id(&o)),
        json!({}),
        Some(ver(&o)),
        StatusCode::CONFLICT,
    )
    .await;
    let o = post(app, admin, &format!("/learning-opportunities/{}/capacity", id(&o)),
        json!({"capacity": 10, "reservedSeats": 2, "enrolmentOpensOn": in_days(-1), "enrolmentClosesOn": in_days(20)}), Some(ver(&o)), StatusCode::OK).await;
    let o = post(
        app,
        admin,
        &format!("/learning-opportunities/{}/submit", id(&o)),
        json!({}),
        Some(ver(&o)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "active");
    assert_eq!(o["providerName"], "NPTEL platform");
    assert_eq!(
        events(&f.pool, inst, "learning_opportunity.activated").await,
        1
    );

    // open elective on a 4A offering (read through the offering read port)
    let cs = b4::seed_offering(&f.pool, &f.setup, "CS101", "active").await;
    let elective = post(
        app,
        admin,
        &opps,
        json!({"code": "OE-CS101", "title": "Open elective CS101", "mode": "open_elective",
        "offeringId": cs.offering_id, "academicTermId": f.term(), "capacity": 5}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        elective["credits"], 12.0,
        "credits default to the offering's"
    );
    assert_eq!(elective["offeringCode"], cs.offering_code.as_str());
    post(app, admin, &opps, json!({"code": "OE-X", "title": "x", "mode": "open_elective", "academicTermId": f.term(), "capacity": 5}), None, StatusCode::UNPROCESSABLE_ENTITY).await;

    // ---- suspension (L4-D-041) and provider suspension (L4-D-044-BH-008)
    let a = b4::seed_student(&f.pool, &f.setup, "Aarav", Some(b4::AARAV)).await;
    let oid = id(&o);
    post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/suspend"),
        json!({}),
        Some(ver(&o)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let o = post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/suspend"),
        json!({"reason": "Provider audit"}),
        Some(ver(&o)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "suspended");
    assert!(o["suspendedAt"].is_string());
    post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": oid}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let o = post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/reactivate"),
        json!({}),
        Some(ver(&o)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["status"], "active");
    let hist: Vec<&str> = o["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap())
        .collect();
    assert_eq!(
        hist,
        ["draft", "pending_approval", "active", "suspended", "active"],
        "suspension history kept"
    );
    let p = post(
        app,
        admin,
        &format!("/learning-providers/{}/suspend", id(&nptel)),
        json!({"reason": "Accreditation review"}),
        Some(ver(&nptel)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": oid}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // offboarding is refused while the provider still has live opportunities
    post(
        app,
        admin,
        &format!("/learning-providers/{}/offboard", id(&p)),
        json!({"reason": "contract ended"}),
        Some(ver(&p)),
        StatusCode::CONFLICT,
    )
    .await;
    let p = post(
        app,
        admin,
        &format!("/learning-providers/{}/reactivate", id(&p)),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "active");
    let e = post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": oid}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(e["status"], "eligible", "approved by the policy path: {e}");
    assert_eq!(e["studentId"], json!(a.student_id));
    assert_eq!(
        e["history"].as_array().unwrap()[1]["actor"],
        "system:auto-approval"
    );
    let me = get_ok(app, &aarav, "/me/flexible-learning").await;
    assert!(me["opportunities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|x| x["id"] == json!(oid)));
    assert_eq!(me["enrolments"].as_array().unwrap().len(), 1);

    // staff reads need academic.read; students cannot read admin lists
    expect(
        app,
        get(
            &aarav,
            &format!("{A}/institutions/{inst}/learning-providers"),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    get_ok(
        app,
        &meera,
        &format!("/institutions/{inst}/learning-opportunities"),
    )
    .await;
    // tenant isolation: another tenant's admin sees nothing
    let (other_app, _) = support::app(T_OTHER).await;
    let other = login(&other_app, "admin@sutra.dev").await;
    expect(
        &other_app,
        get(&other, &format!("{A}/learning-opportunities/{oid}")),
        StatusCode::NOT_FOUND,
    )
    .await;
    expect(
        &other_app,
        get(&other, &format!("{A}/learning-providers/{}", id(&p))),
        StatusCode::NOT_FOUND,
    )
    .await;
    // If-Match is required on commands
    expect(
        app,
        write(
            admin,
            "POST",
            &format!("{A}/learning-opportunities/{oid}/suspend"),
            json!({"reason": "x"}),
            None,
        ),
        StatusCode::PRECONDITION_REQUIRED,
    )
    .await;
    expect(
        app,
        write(
            admin,
            "POST",
            &format!("{A}/learning-opportunities/{oid}/suspend"),
            json!({"reason": "x"}),
            Some(1),
        ),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
}

// ------------------------------------------------------------------------- seats

/// L4-D-034 (request validation, duplicate prevention, policy limits), L4-D-035 (allocation by
/// priority within governed capacity), L4-D-036 (waitlist: queue position, no capacity consumed,
/// promotion, expiry, removal), L4-D-048 (conflict resolution never exceeds capacity and keeps
/// the released allocation as evidence).
#[tokio::test]
async fn demand_allocation_waitlist_and_conflicts() {
    let f = fixture(T_SEATS, "Flexible Seats University").await;
    let (app, admin, inst) = (&f.app, &f.admin, f.inst());
    let aarav = login(app, "aarav.desai@sutra.dev").await;
    let provider = active_provider(&f, "NPTEL").await;
    let o = active_opportunity(&f, &provider, "NPTEL-ML", 2, json!({})).await;
    let oid = id(&o);

    // no policy in force → requests are refused (L4-D-034-BH-002/003)
    let _a = b4::seed_student(&f.pool, &f.setup, "Aarav", Some(b4::AARAV)).await;
    let b = b4::seed_student(&f.pool, &f.setup, "Bina", None).await;
    let c = b4::seed_student(&f.pool, &f.setup, "Chetan", None).await;
    post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": oid}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    effective_policy(
        &f,
        json!({"maxConcurrentEnrolments": 2, "maxCreditsPerTerm": 8}),
    )
    .await;

    let ea = post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": oid}),
        None,
        StatusCode::CREATED,
    )
    .await;
    post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": oid}),
        None,
        StatusCode::CONFLICT,
    )
    .await; // BH-008
    post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/enrolments"),
        json!({"studentId": Uuid::now_v7()}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let eb = post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/enrolments"),
        json!({"studentId": b.student_id, "priority": 1, "sourceKey": "portal:req:b"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let again = post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/enrolments"),
        json!({"studentId": b.student_id, "priority": 1, "sourceKey": "portal:req:b"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        id(&again),
        id(&eb),
        "the same authoritative fact is one enrolment"
    );
    let ec = enrol(&f, &oid, c.student_id, 200).await;
    assert_eq!(
        events(&f.pool, inst, "flexible_enrolment.requested").await,
        3
    );

    // policy credit limit: 4 + 4 + 4 > 8 for Aarav's term
    let o2 = active_opportunity(&f, &provider, "NPTEL-AI", 5, json!({})).await;
    let o3 = active_opportunity(&f, &provider, "NPTEL-DB", 5, json!({})).await;
    post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": id(&o2)}),
        None,
        StatusCode::CREATED,
    )
    .await;
    post(
        app,
        &aarav,
        "/me/flexible-learning/requests",
        json!({"opportunityId": id(&o3)}),
        None,
        StatusCode::CONFLICT,
    )
    .await;

    // ---- allocation (L4-D-035)
    let run = seat_cmd(&f, &oid, "allocate", StatusCode::OK).await;
    assert_eq!(
        run["allocated"],
        json!([id(&eb), id(&ea)]),
        "priority 1 then 100: {run}"
    );
    assert_eq!(run["waitlisted"], json!([id(&ec)]));
    assert_eq!(run["opportunity"]["seatsAllocated"], 2);
    assert_eq!(run["opportunity"]["waitlistCount"], 1);
    let ecv = enrolment(&f, &id(&ec)).await;
    assert_eq!(ecv["waitlistPosition"], 1);
    assert!(ecv["waitlistExpiresOn"].is_string());
    let run = seat_cmd(&f, &oid, "allocate", StatusCode::OK).await;
    assert!(
        run["allocated"].as_array().unwrap().is_empty(),
        "no second allocation for the same demand"
    );
    assert_eq!(
        events(&f.pool, inst, "flexible_enrolment.allocated").await,
        2
    );
    assert_eq!(
        events(&f.pool, inst, "flexible_enrolment.waitlisted").await,
        1
    );

    // ---- conflict (L4-D-048): capacity lowered below the allocations
    let o = opportunity(&f, &oid).await;
    let o = post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/capacity"),
        json!({"capacity": 1}),
        Some(ver(&o)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(o["overAllocated"], true);
    seat_cmd(&f, &oid, "allocate", StatusCode::CONFLICT).await;
    let run = seat_cmd(&f, &oid, "resolve-conflicts", StatusCode::OK).await;
    assert_eq!(
        run["released"],
        json!([id(&ea)]),
        "the lower-priority allocation is released: {run}"
    );
    assert_eq!(run["opportunity"]["seatsAllocated"], 1);
    assert_eq!(run["opportunity"]["overAllocated"], false);
    let eav = enrolment(&f, &id(&ea)).await;
    assert_eq!(eav["status"], "waitlisted");
    assert_eq!(eav["waitlistPosition"], 1, "priority 100 queues before 200");
    assert_eq!(
        eav["allocations"][0]["status"], "released",
        "conflict evidence kept: {eav}"
    );
    let seats: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.flexible_seat_allocation WHERE opportunity_id = $1 AND status = 'confirmed'")
        .bind(Uuid::parse_str(&oid).unwrap()).fetch_one(&f.pool).await.unwrap();
    assert_eq!(seats, 1);

    // ---- waitlist promotion, expiry, removal (L4-D-036)
    let o = opportunity(&f, &oid).await;
    post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/capacity"),
        json!({"capacity": 2}),
        Some(ver(&o)),
        StatusCode::OK,
    )
    .await;
    sqlx::query("UPDATE academic.flexible_enrolment SET waitlist_expires_on = CURRENT_DATE - 2 WHERE id = $1")
        .bind(Uuid::parse_str(&id(&ec)).unwrap()).execute(&f.pool).await.unwrap();
    let run = seat_cmd(&f, &oid, "promote-waitlist", StatusCode::OK).await;
    assert_eq!(run["allocated"], json!([id(&ea)]), "{run}");
    assert_eq!(run["expired"], json!([id(&ec)]));
    let ecv = enrolment(&f, &id(&ec)).await;
    assert_eq!(ecv["status"], "withdrawn");
    assert_eq!(ecv["statusReason"], "waitlist entry expired");
    // student withdrawal (policy: allowed until registered) releases the seat
    let eav = enrolment(&f, &id(&ea)).await;
    let eav = post(
        app,
        &aarav,
        &format!("/me/flexible-learning/{}/withdraw", id(&ea)),
        json!({}),
        Some(ver(&eav)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(eav["status"], "withdrawn");
    assert_eq!(opportunity(&f, &oid).await["seatsAllocated"], 1);
    // another student's enrolment is invisible to Aarav
    let ebv = enrolment(&f, &id(&eb)).await;
    post(
        app,
        &aarav,
        &format!("/me/flexible-learning/{}/withdraw", id(&eb)),
        json!({}),
        Some(ver(&ebv)),
        StatusCode::NOT_FOUND,
    )
    .await;
    // staff removal needs a reason
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{}/withdraw", id(&eb)),
        json!({}),
        Some(ver(&ebv)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // without a waitlist, exhausted capacity gives the no-seat outcome
    let tiny = active_opportunity(
        &f,
        &provider,
        "NPTEL-OS",
        1,
        json!({"waitlistEnabled": false}),
    )
    .await;
    let tid = id(&tiny);
    let x = enrol(&f, &tid, b.student_id, 1).await;
    let y = enrol(&f, &tid, c.student_id, 2).await;
    let run = seat_cmd(&f, &tid, "allocate", StatusCode::OK).await;
    assert_eq!(run["allocated"], json!([id(&x)]));
    assert_eq!(run["unallocated"], json!([id(&y)]));
    assert_eq!(enrolment(&f, &id(&y)).await["status"], "rejected");
}

// ------------------------------------------------------------------------- recognition

/// L4-D-037 (provider registration: acceptance needs the reference, rejection keeps an
/// explicit unsuccessful position, repeated acknowledgement is one fact), L4-D-038 (completion
/// validated against hours / assessment / provider evidence, no automatic credit, idempotent),
/// L4-D-043 (certificate: rejected / accepted / flagged with history), L4-D-046 (mapping rules
/// against Section B definitions, credit limits, supersession), L4-D-039 (recognition only from
/// verified evidence, governed decision, posting through `CreditPostingPort` into the Domain C
/// ledger, idempotent).
#[tokio::test]
async fn registration_completion_certificate_and_credit_posting() {
    let f = fixture(T_RECOGNITION, "Flexible Recognition University").await;
    let (app, admin, inst) = (&f.app, &f.admin, f.inst());
    effective_policy(&f, json!({})).await;
    let provider = active_provider(&f, "NPTEL").await;
    let o = active_opportunity(&f, &provider, "NPTEL-ML", 5, json!({})).await;
    let oid = id(&o);
    let a = b4::seed_student(&f.pool, &f.setup, "Aarav", Some(b4::AARAV)).await;
    let b = b4::seed_student(&f.pool, &f.setup, "Bina", None).await;
    let ea = enrol(&f, &oid, a.student_id, 1).await;
    let eb = enrol(&f, &oid, b.student_id, 2).await;
    let (ea, eb) = (id(&ea), id(&eb));
    // registration needs an allocated seat
    let e = enrolment(&f, &ea).await;
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/register"),
        json!({}),
        Some(ver(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    seat_cmd(&f, &oid, "allocate", StatusCode::OK).await;

    // ---- provider registration (L4-D-037)
    let e = enrolment(&f, &ea).await;
    let e = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/register"),
        json!({}),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/register"),
        json!({}),
        Some(ver(&e)),
        StatusCode::CONFLICT,
    )
    .await; // one pending
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/registration-outcome"),
        json!({"outcome": "accepted"}),
        Some(ver(&e)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let e = post(app, admin, &format!("/flexible-enrolments/{ea}/registration-outcome"),
        json!({"outcome": "rejected", "reason": "Learner email unknown to provider", "evidenceKey": "ack-1"}), Some(ver(&e)), StatusCode::OK).await;
    assert_eq!(
        e["status"], "allocated",
        "explicit unsuccessful position, seat kept"
    );
    assert!(e["statusReason"].as_str().unwrap().contains("rejected"));
    let e = register_with_provider(&f, &ea, "NPTEL-77").await;
    assert_eq!(e["registrations"].as_array().unwrap().len(), 2);
    let same = post(app, admin, &format!("/flexible-enrolments/{ea}/registration-outcome"),
        json!({"outcome": "accepted", "providerReference": "NPTEL-77", "evidenceKey": "ack-NPTEL-77"}), Some(ver(&e)), StatusCode::OK).await;
    assert_eq!(
        ver(&same),
        ver(&e),
        "a repeated acknowledgement changes nothing"
    );
    assert_eq!(
        events(&f.pool, inst, "flexible_enrolment.registered").await,
        1
    );

    // ---- completion (L4-D-038)
    let done = |hours: f64, assessment: &str, evidence: Option<&str>| json!({"hoursCompleted": hours, "assessmentStatus": assessment, "providerEvidenceRef": evidence, "sourceKey": "nptel:completion:77"});
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/completion"),
        done(25.0, "passed", None),
        Some(ver(&e)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/completion"),
        done(10.0, "passed", Some("NPTEL/CERT/77")),
        Some(ver(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/completion"),
        done(25.0, "failed", Some("NPTEL/CERT/77")),
        Some(ver(&e)),
        StatusCode::CONFLICT,
    )
    .await;
    let e = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/progress"),
        json!({"hoursCompleted": 12}),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "in_progress");
    let e = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/completion"),
        done(25.0, "passed", Some("NPTEL/CERT/77")),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "completed");
    assert_eq!(e["completion"]["hoursCompleted"], 25.0);
    let again = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/completion"),
        done(25.0, "passed", Some("NPTEL/CERT/77")),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ver(&again), ver(&e), "same completion fact recorded once");
    let ledger: i64 = sqlx::query_scalar("SELECT count(*) FROM student.credit_ledger_entry WHERE student_id = $1 AND source_type = 'academic_flexible_learning'")
        .bind(a.student_id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(ledger, 0, "completion never grants credit by itself");

    // ---- recognition needs verified evidence and a mapping (L4-D-039 BH-002/003)
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/credit-recognitions"),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;

    // ---- certificate verification (L4-D-043)
    let c1 = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/certificates"),
        json!({"certificateNumber": "NPTEL/CERT/77-A", "issuedOn": today()}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let c1 = post(
        app,
        admin,
        &format!("/certificate-verifications/{}/verify", id(&c1)),
        json!({"authenticityConfirmed": true, "identityMatched": false}),
        Some(ver(&c1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c1["status"], "rejected", "{c1}");
    assert_eq!(
        c1["checks"]["providerAccredited"], true,
        "derived from the provider's accreditation"
    );
    let c2 = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/certificates"),
        json!({"certificateNumber": "NPTEL/CERT/77"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/certificates"),
        json!({"certificateNumber": "nptel/cert/77"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let c2 = post(
        app,
        admin,
        &format!("/certificate-verifications/{}/verify", id(&c2)),
        json!({"authenticityConfirmed": true, "identityMatched": true}),
        Some(ver(&c2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c2["status"], "accepted");
    let hist: Vec<&str> = c2["history"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["toStatus"].as_str().unwrap())
        .collect();
    assert_eq!(hist, ["submitted", "under_verification", "accepted"]);
    assert_eq!(
        events(&f.pool, inst, "certificate_verification.decided").await,
        2
    );
    // fraud indicators flag the certificate and block Bina's recognition
    seat_cmd(&f, &oid, "promote-waitlist", StatusCode::OK).await;
    register_with_provider(&f, &eb, "NPTEL-78").await;
    let cb = post(
        app,
        admin,
        &format!("/flexible-enrolments/{eb}/certificates"),
        json!({"certificateNumber": "FAKE-1"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let cb = post(app, admin, &format!("/certificate-verifications/{}/verify", id(&cb)),
        json!({"authenticityConfirmed": true, "identityMatched": true, "fraudIndicators": ["serial reused"]}), Some(ver(&cb)), StatusCode::OK).await;
    assert_eq!(cb["status"], "flagged");
    let e = enrolment(&f, &eb).await;
    let e = post(
        app,
        admin,
        &format!("/flexible-enrolments/{eb}/completion"),
        done(30.0, "passed", Some("X")),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    let _ = e;

    // ---- credit mapping (L4-D-046)
    let comp = f.setup.component("CS101").clone();
    let rules = format!("/institutions/{inst}/credit-mapping-rules");
    post(app, admin, &rules, json!({"opportunityId": oid, "equivalency": "elective_credit", "credits": 5, "effectiveFrom": today()}), None, StatusCode::UNPROCESSABLE_ENTITY).await; // > opportunity credits
    post(app, admin, &rules, json!({"opportunityId": oid, "equivalency": "component_equivalent", "credits": 4, "effectiveFrom": today()}), None, StatusCode::UNPROCESSABLE_ENTITY).await; // no component
    post(app, admin, &rules, json!({"opportunityId": oid, "equivalency": "component_equivalent", "curriculumVersionId": f.setup.base.curriculum_version_id,
        "curriculumComponentId": Uuid::now_v7(), "credits": 4, "effectiveFrom": today()}), None, StatusCode::UNPROCESSABLE_ENTITY).await; // not in the curriculum
    let m1 = post(app, admin, &rules, json!({"opportunityId": oid, "equivalency": "elective_credit", "credits": 3, "effectiveFrom": today()}), None, StatusCode::CREATED).await;
    post(app, admin, &rules, json!({"opportunityId": oid, "equivalency": "elective_credit", "credits": 2, "effectiveFrom": today()}), None, StatusCode::CONFLICT).await; // one pending version
    let m1 = post(
        app,
        admin,
        &format!("/credit-mapping-rules/{}/submit", id(&m1)),
        json!({}),
        Some(ver(&m1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m1["status"], "effective");
    let m2 = post(app, admin, &rules, json!({"opportunityId": oid, "equivalency": "component_equivalent", "curriculumVersionId": f.setup.base.curriculum_version_id,
        "curriculumComponentId": comp.id, "credits": 4, "effectiveFrom": today()}), None, StatusCode::CREATED).await;
    let m2 = post(
        app,
        admin,
        &format!("/credit-mapping-rules/{}/submit", id(&m2)),
        json!({}),
        Some(ver(&m2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m2["versionNo"], 2);
    assert_eq!(m2["supersedesId"], m1["id"]);
    assert_eq!(
        get_ok(app, admin, &format!("/credit-mapping-rules/{}", id(&m1))).await["status"],
        "superseded"
    );
    assert_eq!(
        events(&f.pool, inst, "credit_mapping_rule.effective").await,
        2
    );

    // ---- recognition + posting (L4-D-039)
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{eb}/credit-recognitions"),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await; // flagged
    let r = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/credit-recognitions"),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["status"], "approved", "{r}");
    assert_eq!(r["credits"], 4.0);
    assert_eq!(r["curriculumComponentId"], json!(comp.id));
    assert_eq!(r["mappingRuleId"], m2["id"]);
    assert_eq!(r["certificateVerificationId"], c2["id"]);
    let again = post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/credit-recognitions"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(id(&again), id(&r), "one recognition per achievement");
    let rid = id(&r);
    let r = post(
        app,
        admin,
        &format!("/credit-recognitions/{rid}/post"),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "posted");
    let ledger_id: Uuid = r["ledgerEntryId"].as_str().unwrap().parse().unwrap();
    let (kind, credits, src): (String, f64, Uuid) = sqlx::query_as(
        "SELECT kind, credits::float8, source_id FROM student.credit_ledger_entry WHERE id = $1",
    )
    .bind(ledger_id)
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        (kind.as_str(), credits, src),
        ("recognised", 4.0, rid.parse().unwrap())
    );
    let r2 = post(
        app,
        admin,
        &format!("/credit-recognitions/{rid}/post"),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        r2["ledgerEntryId"], r["ledgerEntryId"],
        "posting is idempotent"
    );
    assert_eq!(r2["postings"].as_array().unwrap().len(), 1);
    assert_eq!(
        events(&f.pool, inst, "credit_recognition.approved").await,
        1
    );
    assert_eq!(events(&f.pool, inst, "credit_recognition.posted").await, 1);
    let ledger: i64 = sqlx::query_scalar("SELECT count(*) FROM student.credit_ledger_entry WHERE student_id = $1 AND source_type = 'academic_flexible_learning'")
        .bind(a.student_id).fetch_one(&f.pool).await.unwrap();
    assert_eq!(ledger, 1);
    // the posting history is append-only
    let rewrite = sqlx::query(
        "UPDATE academic.credit_recognition_posting SET credits = 9 WHERE recognition_id = $1",
    )
    .bind(Uuid::parse_str(&rid).unwrap())
    .execute(&f.pool)
    .await;
    assert!(rewrite.is_err());
    // the student sees the recognition
    let aarav = login(app, "aarav.desai@sutra.dev").await;
    let mine = get_ok(app, &aarav, "/me/flexible-learning/recognitions").await;
    assert_eq!(mine["items"][0]["status"], "posted");
}

// ------------------------------------------------------------------------- closure

/// L4-D-049 (reconciliation keeps both evidence sets, uphold needs notes, escalation raises an
/// exception case), L4-D-050 (exception: plan approval before correction, override needs a
/// justification, holds keep it open, closure with evidence, idempotent raise), L4-D-042
/// (archival refused while obligations remain), L4-D-047 (session archival needs the closure
/// fact and no open obligation; moves only D06 records).
#[tokio::test]
async fn reconciliation_exceptions_and_session_archival() {
    let f = fixture(T_CLOSURE, "Flexible Closure University").await;
    let (app, admin, inst) = (&f.app, &f.admin, f.inst());
    effective_policy(&f, json!({})).await;
    let provider = active_provider(&f, "NPTEL").await;
    let o = active_opportunity(&f, &provider, "NPTEL-ML", 5, json!({})).await;
    let oid = id(&o);
    let a = b4::seed_student(&f.pool, &f.setup, "Aarav", None).await;
    let ea = id(&enrol(&f, &oid, a.student_id, 1).await);
    seat_cmd(&f, &oid, "allocate", StatusCode::OK).await;
    register_with_provider(&f, &ea, "REF-1").await;

    // ---- reconciliation (L4-D-049)
    let runs = format!("/institutions/{inst}/reconciliation-runs");
    let ok = post(
        app,
        admin,
        &runs,
        json!({"opportunityId": oid, "providerRecords": [
        {"studentNumber": a.student_number, "providerReference": "REF-1", "completed": false}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(ok["status"], "reconciled", "{ok}");
    assert_eq!(ok["resolution"], "consistent");
    let bad = post(app, admin, &runs, json!({"opportunityId": oid, "providerRecords": [
        {"studentNumber": a.student_number, "providerReference": "REF-9", "completed": true, "hoursCompleted": 30},
        {"studentNumber": "NOPE-1", "completed": true}]}), None, StatusCode::CREATED).await;
    assert_eq!(bad["status"], "discrepancies");
    let kinds: Vec<&str> = bad["discrepancies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"registration_reference_mismatch")
            && kinds.contains(&"completion_unrecorded")
            && kinds.contains(&"unknown_learner"),
        "{kinds:?}"
    );
    assert_eq!(
        bad["providerRecords"].as_array().unwrap().len(),
        2,
        "provider evidence kept as received"
    );
    assert_eq!(bad["institutionalRecords"][0]["providerReference"], "REF-1");
    post(
        app,
        admin,
        &format!("/reconciliation-runs/{}/resolve", id(&bad)),
        json!({"resolution": "institutional_record_upheld"}),
        Some(ver(&bad)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let esc = post(
        app,
        admin,
        &format!("/reconciliation-runs/{}/resolve", id(&bad)),
        json!({"resolution": "escalated", "notes": "Provider disputes"}),
        Some(ver(&bad)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(esc["status"], "escalated");
    let case_id = esc["exceptionCaseId"].as_str().unwrap().to_owned();

    // ---- archival refused while obligations remain (L4-D-042)
    let o = opportunity(&f, &oid).await;
    let refused = post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/archive"),
        json!({}),
        Some(ver(&o)),
        StatusCode::CONFLICT,
    )
    .await;
    let detail = refused["detail"].as_str().unwrap_or_default();
    assert!(
        detail.contains("open flexible enrolments") && detail.contains("exception cases"),
        "{refused}"
    );

    // ---- exception case → governed closure (L4-D-050)
    let x = get_ok(app, admin, &format!("/flexible-exceptions/{case_id}")).await;
    assert_eq!(x["category"], "reconciliation");
    assert_eq!(x["status"], "open");
    let raise = json!({"category": "registration", "title": "Provider lost the registration", "sourceKey": "nptel:incident:9", "enrolmentId": ea});
    let x2 = post(
        app,
        admin,
        &format!("/institutions/{inst}/flexible-exceptions"),
        raise.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;
    let again = post(
        app,
        admin,
        &format!("/institutions/{inst}/flexible-exceptions"),
        raise,
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(id(&again), id(&x2), "the same failure fact is one case");
    post(
        app,
        admin,
        &format!("/flexible-exceptions/{}/resolve", id(&x2)),
        json!({"correctiveAction": "x", "resolutionEvidence": "y"}),
        Some(ver(&x2)),
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        admin,
        &format!("/flexible-exceptions/{}/plan", id(&x2)),
        json!({"correctivePlan": "Re-register", "overrideRequired": true}),
        Some(ver(&x2)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    for cid in [case_id.clone(), id(&x2)] {
        let c = get_ok(app, admin, &format!("/flexible-exceptions/{cid}")).await;
        let c = post(
            app,
            admin,
            &format!("/flexible-exceptions/{cid}/plan"),
            json!({"correctivePlan": "Request provider correction"}),
            Some(ver(&c)),
            StatusCode::OK,
        )
        .await;
        assert_eq!(c["status"], "plan_approved", "{c}");
        let c = post(app, admin, &format!("/flexible-exceptions/{cid}/resolve"), json!({"correctiveAction": "Provider corrected its record", "resolutionEvidence": "Ticket 4411"}), Some(ver(&c)), StatusCode::OK).await;
        let c = post(
            app,
            admin,
            &format!("/flexible-exceptions/{cid}/hold"),
            json!({"onHold": true, "reason": "Compliance investigation"}),
            Some(ver(&c)),
            StatusCode::OK,
        )
        .await;
        post(
            app,
            admin,
            &format!("/flexible-exceptions/{cid}/close"),
            json!({}),
            Some(ver(&c)),
            StatusCode::CONFLICT,
        )
        .await;
        let c = post(
            app,
            admin,
            &format!("/flexible-exceptions/{cid}/hold"),
            json!({"onHold": false}),
            Some(ver(&c)),
            StatusCode::OK,
        )
        .await;
        let c = post(
            app,
            admin,
            &format!("/flexible-exceptions/{cid}/close"),
            json!({}),
            Some(ver(&c)),
            StatusCode::OK,
        )
        .await;
        assert_eq!(c["status"], "closed");
        let hist: Vec<&str> = c["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| h["toStatus"].as_str().unwrap())
            .collect();
        assert_eq!(
            hist,
            [
                "open",
                "plan_pending",
                "plan_approved",
                "resolved",
                "closed"
            ],
            "failure history preserved"
        );
    }

    // ---- session archival (L4-D-047)
    let arch = format!("/institutions/{inst}/learning-opportunities/archive-session");
    // the term ends after today and no closure decision is referenced
    post(
        app,
        admin,
        &arch,
        json!({"academicTermId": f.term()}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // with the closure fact, the open enrolment still blocks
    post(
        app,
        admin,
        &arch,
        json!({"academicTermId": f.term(), "closureEvidence": "Senate resolution 2026/41"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let e = enrolment(&f, &ea).await;
    post(
        app,
        admin,
        &format!("/flexible-enrolments/{ea}/withdraw"),
        json!({"reason": "Session closing"}),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    let res = post(
        app,
        admin,
        &arch,
        json!({"academicTermId": f.term(), "closureEvidence": "Senate resolution 2026/41"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(res["archived"], json!([oid]), "{res}");
    let o = opportunity(&f, &oid).await;
    assert_eq!(o["status"], "archived");
    assert!(o["sessionArchivedAt"].is_string());
    let res = post(
        app,
        admin,
        &arch,
        json!({"academicTermId": f.term(), "closureEvidence": "Senate resolution 2026/41"}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(res["alreadyArchived"], 1);
    // archived opportunities accept no change; the Domain B term is untouched
    post(
        app,
        admin,
        &format!("/learning-opportunities/{oid}/reactivate"),
        json!({}),
        Some(ver(&o)),
        StatusCode::CONFLICT,
    )
    .await;
    let term_status: String = sqlx::query_scalar("SELECT v.status FROM institution.calendar_version v JOIN institution.academic_term t ON t.calendar_version_id = v.id WHERE t.id = $1")
        .bind(f.term()).fetch_one(&f.pool).await.unwrap();
    assert_eq!(term_status, "published");
    assert_eq!(
        events(&f.pool, inst, "learning_opportunity.archived").await,
        1
    );
}

// ------------------------------------------------------------------------- resources

/// Course resources: faculty-scoped (`academic.teach` + an active assignment), offering-wide
/// resources by administrators, students see published resources of their seated sections.
#[tokio::test]
async fn course_resources_and_library_links() {
    let f = fixture(T_RESOURCES, "Resources University").await;
    let (app, admin) = (&f.app, &f.admin);
    let meera = login(app, "meera.shah@sutra.dev").await;
    let aarav = login(app, "aarav.desai@sutra.dev").await;
    let cs = b4::seed_offering(&f.pool, &f.setup, "CS101", "active").await;
    let md = b4::seed_offering(&f.pool, &f.setup, "MD101", "active").await;
    b4::seed_faculty(&f.pool, &f.setup, &cs, b4::MEERA, "active").await;
    let a = b4::seed_student(&f.pool, &f.setup, "Aarav", Some(b4::AARAV)).await;
    b4::seed_enrolment(&f.pool, &f.setup, &cs, &a, Some(b4::AARAV)).await;

    let body = json!({"title": "Week 1 reading", "kind": "reading", "url": "https://library.example.edu/cs101/w1"});
    let r = post(
        app,
        &meera,
        &format!("/sections/{}/resources", cs.section_id),
        body.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(r["status"], "draft");
    post(
        app,
        &meera,
        &format!("/sections/{}/resources", md.section_id),
        body.clone(),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    post(
        app,
        &aarav,
        &format!("/sections/{}/resources", cs.section_id),
        body.clone(),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    post(
        app,
        &meera,
        &format!("/sections/{}/resources", cs.section_id),
        json!({"title": "x", "kind": "link", "url": "ftp://nope"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert!(
        get_ok(app, &aarav, "/me/library").await["items"]
            .as_array()
            .unwrap()
            .is_empty(),
        "drafts are not visible"
    );
    let r = post(
        app,
        &meera,
        &format!("/resources/{}/publish", id(&r)),
        json!({}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "published");
    post(app, admin, &format!("/offerings/{}/resources", cs.offering_id), json!({"title": "Course handbook", "kind": "document", "url": "https://example.edu/cs101.pdf", "publish": true}), None, StatusCode::CREATED).await;
    post(app, admin, &format!("/offerings/{}/resources", md.offering_id), json!({"title": "MD handbook", "kind": "document", "url": "https://example.edu/md.pdf", "publish": true}), None, StatusCode::CREATED).await;
    post(
        app,
        &meera,
        &format!("/offerings/{}/resources", cs.offering_id),
        body.clone(),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let lib = get_ok(app, &aarav, "/me/library").await;
    let titles: Vec<&str> = lib["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["title"].as_str().unwrap())
        .collect();
    assert_eq!(titles.len(), 2, "{lib}");
    assert!(titles.contains(&"Week 1 reading") && titles.contains(&"Course handbook"));
    assert_eq!(
        lib["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["title"] == "Week 1 reading")
            .unwrap()["sectionCode"],
        "A"
    );
    let mine = get_ok(app, &meera, "/my/resources").await;
    assert_eq!(mine["sections"].as_array().unwrap().len(), 1);
    assert_eq!(mine["items"].as_array().unwrap().len(), 1);
    let r = expect(
        app,
        write(
            &meera,
            "PATCH",
            &format!("{A}/resources/{}", id(&r)),
            json!({"title": "Week 1 readings"}),
            Some(ver(&r)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["title"], "Week 1 readings");
    let r = post(
        app,
        &meera,
        &format!("/resources/{}/withdraw", id(&r)),
        json!({"reason": "Replaced"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "withdrawn");
    assert_eq!(
        get_ok(app, &aarav, "/me/library").await["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        events(&f.pool, f.inst(), "course_resource.published").await,
        3
    );
}

// ------------------------------------------------------------------------- AwaitDecision

fn await_state(pool: &sqlx::PgPool) -> AcademicState {
    let mut st = sutra_api::app::academic_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    st
}

/// Every 4C approval step stays pending under `ApprovalMode::AwaitDecision` until the explicit
/// decision runs (Flowable, Batch 9): provider onboarding, opportunity activation, enrolment
/// review, certificate verification, mapping approval, credit recognition (partial approval),
/// exception plan.
#[tokio::test]
async fn approval_steps_await_a_decision() {
    use sutra_academic::flexible::{enrolment, exception, opportunity, provider};
    use sutra_academic::recognition::{certificate, credit, mapping};
    let f = fixture(T_AWAIT, "Await Flexible University").await;
    let st = await_state(&f.pool);
    let tenant = f.setup.tenant();
    let admin = auth(ADMIN, Role::Administrator, tenant);
    let who = actor(ADMIN);
    let inst = f.inst();
    effective_policy(&f, json!({})).await;

    // provider onboarding
    let p = post(&f.app, &f.admin, &format!("/institutions/{inst}/learning-providers"), json!({"code": "NPTEL", "name": "NPTEL",
        "providerKind": "government_platform", "accreditationRef": "MoE", "accreditationValidUntil": in_days(400), "modes": ["nptel"]}), None, StatusCode::CREATED).await;
    let pid: Uuid = id(&p).parse().unwrap();
    let p = provider::submit(&st, &admin, pid, Version(ver(&p)), &who)
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "under_review");
    assert!(p.process_instance_id.is_some());
    let p = provider::decide(&st, &admin, pid, Version(p.version), true, None, &who)
        .await
        .unwrap();
    assert_eq!(p.status.as_str(), "active");

    // opportunity activation (requires human review of enrolments)
    let o = post(&f.app, &f.admin, &format!("/institutions/{inst}/learning-opportunities"), json!({"code": "NPTEL-ML", "title": "ML", "mode": "nptel",
        "providerId": pid, "academicTermId": f.term(), "credits": 4, "capacity": 3, "requiresApproval": true,
        "enrolmentOpensOn": in_days(-1), "enrolmentClosesOn": in_days(30)}), None, StatusCode::CREATED).await;
    let oid: Uuid = id(&o).parse().unwrap();
    let o = opportunity::submit(&st, &admin, oid, Version(ver(&o)), &who)
        .await
        .unwrap();
    assert_eq!(o.status.as_str(), "pending_approval");
    let err = opportunity::decide(&st, &admin, oid, Version(o.version), false, None, &who)
        .await
        .unwrap_err();
    assert!(format!("{err:?}").contains("reason"), "{err:?}");
    let o = opportunity::decide(&st, &admin, oid, Version(o.version), true, None, &who)
        .await
        .unwrap();
    assert_eq!(o.status.as_str(), "active");

    // enrolment review
    let a = b4::seed_student(&f.pool, &f.setup, "Aarav", None).await;
    let (e, new) = enrolment::request(&st, &admin, oid, a.student_id, None, None, &who)
        .await
        .unwrap();
    assert!(new);
    assert_eq!(e.status.as_str(), "requested", "human review pending");
    assert!(e.process_instance_id.is_some());
    let e = enrolment::decide(&st, &admin, e.id, Version(e.version), true, None, &who)
        .await
        .unwrap();
    assert_eq!(e.status.as_str(), "eligible");
    enrolment::run_allocation(
        &st,
        &admin,
        oid,
        Version(opportunity::get(&st, tenant, oid).await.unwrap().version),
        &who,
    )
    .await
    .unwrap();
    let eid = e.id;
    register_with_provider(&f, &eid.to_string(), "R-1").await;
    let e = enrolment(&f, &eid.to_string()).await;
    let e = post(
        &f.app,
        &f.admin,
        &format!("/flexible-enrolments/{eid}/completion"),
        json!({"hoursCompleted": 10, "assessmentStatus": "passed", "providerEvidenceRef": "EV"}),
        Some(ver(&e)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(e["status"], "completed");

    // certificate verification awaits the decision; accepted needs every check
    let c = certificate::submit(
        &st,
        &admin,
        eid,
        serde_json::from_value(json!({"certificateNumber": "C-1"})).unwrap(),
        &who,
    )
    .await
    .unwrap();
    let c = certificate::verify(
        &st,
        &admin,
        c.id,
        Version(c.version),
        serde_json::from_value(json!({"authenticityConfirmed": true})).unwrap(),
        &who,
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "under_verification");
    let accept = || {
        serde_json::from_value::<certificate::CertificateDecision>(json!({"outcome": "accepted"}))
            .unwrap()
    };
    assert!(
        certificate::decide(&st, &admin, c.id, Version(c.version), accept(), &who)
            .await
            .is_err(),
        "identity not yet confirmed"
    );
    let c = certificate::verify(
        &st,
        &admin,
        c.id,
        Version(c.version),
        serde_json::from_value(json!({"authenticityConfirmed": true, "identityMatched": true}))
            .unwrap(),
        &who,
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "under_verification");
    let c = certificate::decide(&st, &admin, c.id, Version(c.version), accept(), &who)
        .await
        .unwrap();
    assert_eq!(c.status.as_str(), "accepted");

    // mapping approval
    let m = mapping::create(&st, &admin, inst, serde_json::from_value(json!({"opportunityId": oid, "equivalency": "elective_credit", "credits": 4, "effectiveFrom": today()})).unwrap(), &who).await.unwrap();
    let m = mapping::submit(&st, &admin, m.id, Version(m.version), &who)
        .await
        .unwrap();
    assert_eq!(m.status.as_str(), "in_review");
    let m = mapping::decide(&st, &admin, m.id, Version(m.version), true, None, &who)
        .await
        .unwrap();
    assert_eq!(m.status.as_str(), "effective");

    // credit recognition: partial approval needs a reason
    let (r, _) = credit::request(&st, &admin, eid, Default::default(), &who)
        .await
        .unwrap();
    assert_eq!(r.status.as_str(), "under_review");
    let partial = |reason: Option<&str>| {
        serde_json::from_value::<credit::CreditRecognitionDecision>(
            json!({"decision": "approve", "credits": 2, "reason": reason}),
        )
        .unwrap()
    };
    assert!(
        credit::decide(&st, &admin, r.id, Version(r.version), partial(None), &who)
            .await
            .is_err()
    );
    let r = credit::decide(
        &st,
        &admin,
        r.id,
        Version(r.version),
        partial(Some("Only half the syllabus maps")),
        &who,
    )
    .await
    .unwrap();
    assert_eq!(
        (r.status.as_str(), r.credits, r.requested_credits),
        ("approved", 2.0, 4.0)
    );
    let r = credit::post(&st, &admin, r.id, Version(r.version), &who)
        .await
        .unwrap();
    assert_eq!(r.status.as_str(), "posted");

    // exception plan awaits its decision
    let (x, _) = exception::raise(
        &st,
        &admin,
        inst,
        serde_json::from_value(json!({"category": "other", "title": "Await"})).unwrap(),
        &who,
    )
    .await
    .unwrap();
    let x = exception::plan(
        &st,
        &admin,
        x.id,
        Version(x.version),
        serde_json::from_value(json!({"correctivePlan": "Fix"})).unwrap(),
        &who,
    )
    .await
    .unwrap();
    assert_eq!(x.status, "plan_pending");
    let x = exception::decide(
        &st,
        &admin,
        x.id,
        Version(x.version),
        false,
        Some("Plan too vague".into()),
        &who,
    )
    .await
    .unwrap();
    assert_eq!(x.status, "open");
}
