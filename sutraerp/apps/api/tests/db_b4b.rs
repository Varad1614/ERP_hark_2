//! Batch 4 slice **4B** DB tests (academic delivery: timetable, sessions + attendance, syllabus
//! coverage, academic monitoring, internal assessment + coursework). Turns the L4 section-d
//! behaviour specs L4-D-020…033 and L4-D-051…057 into executable checks; offerings, sections,
//! faculty and rosters are seeded through `support::b4` (no dependency on 4A). Covers
//! `ApprovalMode::AwaitDecision`, faculty-scoped access and permissions; every test runs in its
//! own tenant.
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

const T_TIMETABLE: &str = "0192f5a0-7c3e-7000-8000-0000000b4b01";
const T_ATTENDANCE: &str = "0192f5a0-7c3e-7000-8000-0000000b4b02";
const T_MONITORING: &str = "0192f5a0-7c3e-7000-8000-0000000b4b03";
const T_ASSESSMENT: &str = "0192f5a0-7c3e-7000-8000-0000000b4b04";
const T_AWAIT: &str = "0192f5a0-7c3e-7000-8000-0000000b4b05";

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

fn actor(subject: &str) -> Actor {
    Actor {
        id: Some(subject.into()),
        correlation_id: None,
    }
}

/// A delivery fixture: one active CS101 offering taught by Meera with Aarav (linked to the dev
/// student login) and Bina seated, plus an active MD101 offering.
struct Fixture {
    app: Router,
    pool: sqlx::PgPool,
    admin: Login,
    meera: Login,
    aarav: Login,
    setup: b4::DeliverySetup,
    cs: b4::SeededOffering,
    md: b4::SeededOffering,
    aarav_id: Uuid,
    bina_id: Uuid,
}

impl Fixture {
    fn inst(&self) -> Uuid {
        self.setup.base.institution_id
    }
    fn cs_section(&self) -> String {
        self.cs.section_id.to_string()
    }
}

async fn fixture(tenant: &'static str, name: &str, meera_on_md: bool) -> Fixture {
    // internal errors (500) log their cause; surface them in the test output
    let _ = tracing_subscriber::fmt()
        .with_env_filter("error")
        .with_test_writer()
        .try_init();
    let (app, pool) = app(tenant).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b4::academic_setup(&app, &admin, &pool, tenant, name).await;
    let cs = b4::seed_offering(&pool, &setup, "CS101", "active").await;
    let md = b4::seed_offering(&pool, &setup, "MD101", "active").await;
    b4::seed_faculty(&pool, &setup, &cs, b4::MEERA, "active").await;
    if meera_on_md {
        b4::seed_faculty(&pool, &setup, &md, b4::MEERA, "active").await;
    }
    let a = b4::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let b = b4::seed_student(&pool, &setup, "Bina", None).await;
    b4::seed_enrolment(&pool, &setup, &cs, &a, Some(b4::AARAV)).await;
    b4::seed_enrolment(&pool, &setup, &cs, &b, None).await;
    Fixture {
        app,
        pool,
        admin,
        meera,
        aarav,
        setup,
        cs,
        md,
        aarav_id: a.student_id,
        bina_id: b.student_id,
    }
}

// ------------------------------------------------------------------------------- timetable

/// L4-D-020 (requirement → generated draft → validation → approval → publication → active),
/// L4-D-021 (governed revision as a distinct version), L4-D-022 (retirement → archive).
#[tokio::test]
async fn timetable_generation_publication_revision_and_retirement() {
    let f = fixture(T_TIMETABLE, "Timetable University", true).await;
    let (app, admin) = (&f.app, &f.admin);
    let inst = f.inst();
    let term = f.setup.term_id;

    // teachable sections of the term (offering read port)
    let secs = get_ok(
        app,
        admin,
        &format!("/institutions/{inst}/teaching-sections?academicTermId={term}"),
    )
    .await;
    assert_eq!(secs["items"].as_array().unwrap().len(), 2, "{secs}");

    // scheduling requirements (BH-001: authoritative sections only)
    post(app, admin, &format!("/institutions/{inst}/scheduling-requirements"),
        json!({"sectionId": f.cs.section_id, "sessionsPerWeek": 3, "sessionMinutes": 60, "roomLabel": "R1"}), None, StatusCode::CREATED).await;
    post(
        app,
        admin,
        &format!("/institutions/{inst}/scheduling-requirements"),
        json!({"sectionId": f.cs.section_id, "sessionsPerWeek": 2, "sessionMinutes": 60}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    post(app, admin, &format!("/institutions/{inst}/scheduling-requirements"),
        json!({"sectionId": f.md.section_id, "sessionsPerWeek": 2, "sessionMinutes": 60, "roomLabel": "R1", "preferredDays": [1, 2]}), None, StatusCode::CREATED).await;
    // faculty cannot manage the timetable
    post(
        app,
        &f.meera,
        &format!("/institutions/{inst}/timetables"),
        json!({"academicTermId": term, "name": "T1"}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;

    let tt = post(
        app,
        admin,
        &format!("/institutions/{inst}/timetables"),
        json!({"academicTermId": term, "name": "Term 1 timetable"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(tt["status"], "planning");
    let tid = id(&tt);
    // publication needs validation and approval (BH-005/007)
    post(
        app,
        admin,
        &format!("/timetables/{tid}/publish"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::CONFLICT,
    )
    .await;
    let tt = post(
        app,
        admin,
        &format!("/timetables/{tid}/generate"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tt["status"], "draft");
    assert_eq!(tt["slots"].as_array().unwrap().len(), 5, "{tt}");
    assert_eq!(
        tt["slots"][0]["startsAt"].as_str().unwrap().len(),
        5,
        "HH:MM"
    );
    assert_eq!(tt["slots"][0]["facultyNames"][0], "Dr. Meera Shah");

    // a manual slot that double-books Meera and room R1 is a critical conflict
    let first = tt["slots"][0].clone();
    let other_section = if first["sectionId"] == json!(f.cs.section_id) {
        f.md.section_id
    } else {
        f.cs.section_id
    };
    let tt = post(
        app,
        admin,
        &format!("/timetables/{tid}/slots"),
        json!({"sectionId": other_section, "dayOfWeek": first["dayOfWeek"],
        "startsAt": first["startsAt"], "endsAt": first["endsAt"], "roomLabel": "R1"}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    let tt = post(
        app,
        admin,
        &format!("/timetables/{tid}/validate"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tt["status"], "draft", "conflicts keep the draft");
    let codes: Vec<&str> = tt["validationReport"]["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"faculty_overlap") && codes.contains(&"room_overlap"),
        "{codes:?}"
    );
    post(
        app,
        admin,
        &format!("/timetables/{tid}/submit"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::CONFLICT,
    )
    .await;
    let extra = tt["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| {
            s["sectionId"] == json!(other_section)
                && s["dayOfWeek"] == first["dayOfWeek"]
                && s["startsAt"] == first["startsAt"]
        })
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tt = expect(
        app,
        write(
            admin,
            "DELETE",
            &format!("{A}/timetables/{tid}/slots/{extra}"),
            json!({}),
            Some(ver(&tt)),
        ),
        StatusCode::OK,
    )
    .await;
    let tt = post(
        app,
        admin,
        &format!("/timetables/{tid}/validate"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tt["status"], "validated", "{}", tt["validationReport"]);
    assert_eq!(tt["validationReport"]["valid"], true);

    // approval through the WorkflowPort (auto-approve in Batch 4) → publish → activate
    let tt = post(
        app,
        admin,
        &format!("/timetables/{tid}/submit"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tt["status"], "approved");
    assert!(tt["processInstanceId"].is_string());
    // registrar authority: faculty cannot publish
    post(
        app,
        &f.meera,
        &format!("/timetables/{tid}/publish"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let tt = post(
        app,
        admin,
        &format!("/timetables/{tid}/publish"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tt["status"], "published");
    // published versions are immutable
    post(app, admin, &format!("/timetables/{tid}/slots"), json!({"sectionId": f.cs.section_id, "dayOfWeek": 6, "startsAt": "09:00", "endsAt": "10:00"}), Some(ver(&tt)), StatusCode::CONFLICT).await;
    let tt = post(
        app,
        admin,
        &format!("/timetables/{tid}/activate"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(tt["status"], "active");
    assert_eq!(events(&f.pool, inst, "timetable.published").await, 1);
    assert_eq!(events(&f.pool, inst, "timetable.activated").await, 1);

    // only one active timetable per term (BH-009)
    let t2 = post(
        app,
        admin,
        &format!("/institutions/{inst}/timetables"),
        json!({"academicTermId": term, "name": "Rival"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let t2id = id(&t2);
    let t2 = post(app, admin, &format!("/timetables/{t2id}/slots"), json!({"sectionId": f.cs.section_id, "dayOfWeek": 6, "startsAt": "09:00", "endsAt": "10:00"}), Some(ver(&t2)), StatusCode::OK).await;
    let t2 = post(
        app,
        admin,
        &format!("/timetables/{t2id}/validate"),
        json!({}),
        Some(ver(&t2)),
        StatusCode::OK,
    )
    .await;
    let t2 = post(
        app,
        admin,
        &format!("/timetables/{t2id}/submit"),
        json!({}),
        Some(ver(&t2)),
        StatusCode::OK,
    )
    .await;
    let t2 = post(
        app,
        admin,
        &format!("/timetables/{t2id}/publish"),
        json!({}),
        Some(ver(&t2)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        admin,
        &format!("/timetables/{t2id}/activate"),
        json!({}),
        Some(ver(&t2)),
        StatusCode::CONFLICT,
    )
    .await;

    // student and faculty weekly views read the operational timetable only
    let mine = get_ok(app, &f.aarav, "/me/timetable").await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 3, "{mine}");
    assert!(mine["weekOf"].is_string());
    let teach = get_ok(app, &f.meera, "/my/timetable").await;
    assert_eq!(teach["items"].as_array().unwrap().len(), 5);
    expect(
        app,
        get(admin, &format!("{A}/me/timetable")),
        StatusCode::FORBIDDEN,
    )
    .await;

    // a teaching session from an operational slot (weekday must match the slot)
    let cs_slot = tt["slots"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["sectionId"] == json!(f.cs.section_id))
        .unwrap()
        .clone();
    let dow = cs_slot["dayOfWeek"].as_i64().unwrap();
    let monday = chrono::NaiveDate::from_ymd_opt(2026, 7, 6).unwrap();
    let date = monday + Duration::days(dow - 1);
    let s = post(
        app,
        &f.meera,
        &format!("/sections/{}/sessions", f.cs.section_id),
        json!({"sessionDate": date, "timetableSlotId": cs_slot["id"]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(s["startsAt"], cs_slot["startsAt"]);
    post(
        app,
        &f.meera,
        &format!("/sections/{}/sessions", f.cs.section_id),
        json!({"sessionDate": date + Duration::days(1), "timetableSlotId": cs_slot["id"]}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // governed revision: a distinct version; the operational one is untouched (L4-D-021)
    let rev = post(
        app,
        admin,
        &format!("/timetables/{tid}/revisions"),
        json!({"reason": "Room R1 under repair"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid = id(&rev);
    assert_eq!(rev["status"], "draft");
    assert_eq!(rev["versionNo"], 2);
    assert_eq!(rev["revisionOfId"], json!(tid));
    assert_eq!(rev["slots"].as_array().unwrap().len(), 5);
    post(
        app,
        admin,
        &format!("/timetables/{tid}/revisions"),
        json!({"reason": "again"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        admin,
        &format!("/timetables/{tid}/retire"),
        json!({"reason": "x"}),
        Some(ver(&tt)),
        StatusCode::CONFLICT,
    )
    .await;
    let drop = rev["slots"][0]["id"].as_str().unwrap().to_owned();
    let rev = expect(
        app,
        write(
            admin,
            "DELETE",
            &format!("{A}/timetables/{rid}/slots/{drop}"),
            json!({}),
            Some(ver(&rev)),
        ),
        StatusCode::OK,
    )
    .await;
    let rev = post(
        app,
        admin,
        &format!("/timetables/{rid}/validate"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::OK,
    )
    .await;
    let rev = post(
        app,
        admin,
        &format!("/timetables/{rid}/submit"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::OK,
    )
    .await;
    let rev = post(
        app,
        admin,
        &format!("/timetables/{rid}/publish"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::OK,
    )
    .await;
    let base = get_ok(app, admin, &format!("/timetables/{tid}")).await;
    assert_eq!(
        base["status"], "active",
        "republication waits for activation"
    );
    let rev = post(
        app,
        admin,
        &format!("/timetables/{rid}/activate"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rev["status"], "active");
    let base = get_ok(app, admin, &format!("/timetables/{tid}")).await;
    assert_eq!(base["status"], "revised");
    assert_eq!(
        base["slots"].as_array().unwrap().len(),
        5,
        "prior version unchanged"
    );
    assert_eq!(events(&f.pool, inst, "timetable.revised").await, 1);
    let teach = get_ok(app, &f.meera, "/my/timetable").await;
    assert_eq!(teach["items"].as_array().unwrap().len(), 4);

    // retirement needs a reason (L4-D-022), then archive; archived never returns
    post(
        app,
        admin,
        &format!("/timetables/{rid}/retire"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let rev = post(
        app,
        admin,
        &format!("/timetables/{rid}/retire"),
        json!({"reason": "Term ended"}),
        Some(ver(&rev)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rev["status"], "retired");
    assert_eq!(events(&f.pool, inst, "timetable.retired").await, 1);
    let rev = post(
        app,
        admin,
        &format!("/timetables/{rid}/archive"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(rev["status"], "archived");
    post(
        app,
        admin,
        &format!("/timetables/{rid}/activate"),
        json!({}),
        Some(ver(&rev)),
        StatusCode::CONFLICT,
    )
    .await;
    // stale If-Match → 412, missing → 428
    post(
        app,
        admin,
        &format!("/timetables/{t2id}/freeze"),
        json!({}),
        Some(1),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    post(
        app,
        admin,
        &format!("/timetables/{t2id}/freeze"),
        json!({}),
        None,
        StatusCode::PRECONDITION_REQUIRED,
    )
    .await;
    let list = get_ok(
        app,
        admin,
        &format!("/institutions/{inst}/timetables?status=archived"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
}

// ------------------------------------------------------------------------------- sessions + attendance

async fn new_session(f: &Fixture, who: &Login, date: &str, start: &str) -> Value {
    post(
        &f.app,
        who,
        &format!("/sections/{}/sessions", f.cs_section()),
        json!({"sessionDate": date, "startsAt": start, "endsAt": "23:00", "topic": "Intro"}),
        None,
        StatusCode::CREATED,
    )
    .await
}

/// Runs one session with the given marks for (Aarav, Bina) and finalises it.
async fn taught_session(f: &Fixture, date: &str, aarav: &str, bina: &str) -> Value {
    let s = new_session(f, &f.meera, date, "09:00").await;
    let sid = id(&s);
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/start"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [
        {"studentId": f.aarav_id, "mark": aarav}, {"studentId": f.bina_id, "mark": bina}]}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/complete"),
        json!({"notes": "covered"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    post(
        &f.app,
        &f.meera,
        &format!("/sessions/{sid}/finalise-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await
}

async fn effective_policy(f: &Fixture, threshold: f64, min_sessions: i64) -> Value {
    let p = post(&f.app, &f.admin, &format!("/institutions/{}/attendance-policies", f.inst()),
        json!({"name": "Default attendance rule", "thresholdPercent": threshold, "minSessions": min_sessions}), None, StatusCode::CREATED).await;
    let p = post(
        &f.app,
        &f.admin,
        &format!("/attendance-policies/{}/submit", id(&p)),
        json!({}),
        Some(ver(&p)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "effective");
    p
}

/// L4-D-023 (session outcome), L4-D-024 (capture → finalisation, thresholds), L4-D-025 (governed
/// correction), L4-D-026 (syllabus coverage publication).
#[tokio::test]
async fn sessions_attendance_thresholds_corrections_and_syllabus() {
    let f = fixture(T_ATTENDANCE, "Attendance University", false).await;
    let (app, admin, meera) = (&f.app, &f.admin, &f.meera);
    let inst = f.inst();
    effective_policy(&f, 75.0, 2).await;

    // L4-D-023: faculty authority is validated (Meera does not teach MD101)
    post(
        app,
        meera,
        &format!("/sections/{}/sessions", f.md.section_id),
        json!({"sessionDate": "2026-07-06", "startsAt": "09:00", "endsAt": "10:00"}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    expect(
        app,
        get(meera, &format!("{A}/sections/{}/sessions", f.md.section_id)),
        StatusCode::FORBIDDEN,
    )
    .await;
    expect(
        app,
        get(
            &f.aarav,
            &format!("{A}/sections/{}/sessions", f.cs.section_id),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;

    let s = new_session(&f, meera, "2026-07-06", "09:00").await;
    let sid = id(&s);
    assert_eq!(s["status"], "scheduled");
    assert_eq!(s["records"].as_array().unwrap().len(), 2, "seated roster");
    assert!(s["records"][0]["mark"].is_null());
    post(
        app,
        meera,
        &format!("/sections/{}/sessions", f.cs_section()),
        json!({"sessionDate": "2026-07-06", "startsAt": "09:00", "endsAt": "23:00"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // no attendance before the session starts; no completion before it starts
    post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [{"studentId": f.aarav_id, "mark": "present"}]}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        meera,
        &format!("/sessions/{sid}/complete"),
        json!({}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/start"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "started");
    assert_eq!(events(&f.pool, inst, "teaching_session.started").await, 1);
    // interruption + resumption keep the history
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/pause"),
        json!({"reason": "fire drill"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/resume"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["interruptionCount"], 1);
    assert_eq!(s["status"], "in_progress");

    // L4-D-024: duplicates and non-seated students are rejected
    let stranger = Uuid::now_v7();
    post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [{"studentId": stranger, "mark": "present"}]}),
        Some(ver(&s)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    post(app, meera, &format!("/sessions/{sid}/attendance"), json!({"records": [
        {"studentId": f.aarav_id, "mark": "present"}, {"studentId": f.aarav_id, "mark": "absent"}]}), Some(ver(&s)), StatusCode::UNPROCESSABLE_ENTITY).await;
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [{"studentId": f.aarav_id, "mark": "absent"}]}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["attendanceStatus"], "capturing");
    // re-marking updates the one canonical record
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [{"studentId": f.aarav_id, "mark": "present"}]}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let n: i64 =
        sqlx::query_scalar("SELECT count(*) FROM academic.attendance_record WHERE session_id = $1")
            .bind(Uuid::parse_str(&sid).unwrap())
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(n, 1);
    // incomplete register cannot be submitted or finalised
    post(
        app,
        meera,
        &format!("/sessions/{sid}/submit-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        meera,
        &format!("/sessions/{sid}/finalise-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [{"studentId": f.bina_id, "mark": "absent"}]}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/submit-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["attendanceStatus"], "submitted");
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/complete"),
        json!({"topic": "Sets and relations"}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["topic"], "Sets and relations");
    // locking a completed session needs finalised attendance
    post(
        app,
        meera,
        &format!("/sessions/{sid}/lock"),
        json!({}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
    let s = post(
        app,
        meera,
        &format!("/sessions/{sid}/finalise-attendance"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["attendanceStatus"], "finalised");
    assert_eq!(events(&f.pool, inst, "attendance.finalised").await, 1);
    assert_eq!(events(&f.pool, inst, "teaching_session.completed").await, 1);
    // finalised attendance is immutable (BH-008)
    post(
        app,
        meera,
        &format!("/sessions/{sid}/attendance"),
        json!({"records": [{"studentId": f.bina_id, "mark": "present"}]}),
        Some(ver(&s)),
        StatusCode::CONFLICT,
    )
    .await;
    // one session counted < minSessions 2: no breach yet
    assert_eq!(
        events(&f.pool, inst, "attendance.threshold_breached").await,
        0
    );

    // second session: both absent → Aarav 1/2 = 50 %, Bina 0/2 → two breaches
    let s2 = taught_session(&f, "2026-07-07", "absent", "absent").await;
    assert_eq!(
        events(&f.pool, inst, "attendance.threshold_breached").await,
        2
    );
    let breach: Value = sqlx::query_scalar("SELECT envelope->'data' FROM platform.outbox WHERE institution_id = $1 AND event_type = 'sutraerp.academic.attendance.threshold_breached' AND envelope->'data'->>'studentId' = $2")
        .bind(inst).bind(f.aarav_id.to_string()).fetch_one(&f.pool).await.unwrap();
    assert_eq!(breach["attendancePercent"], 50.0);
    assert_eq!(breach["studentSubjectRef"], b4::AARAV);
    let summary = get_ok(
        app,
        meera,
        &format!("/sections/{}/attendance-summary", f.cs.section_id),
    )
    .await;
    assert_eq!(summary["finalisedSessions"], 2);
    assert_eq!(summary["thresholdPercent"], 75.0);
    // re-evaluation never raises the same breach twice
    post(
        app,
        admin,
        &format!("/sections/{}/attendance-summary/evaluate", f.cs.section_id),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        events(&f.pool, inst, "attendance.threshold_breached").await,
        2
    );
    post(
        app,
        meera,
        &format!("/sections/{}/attendance-summary/evaluate", f.cs.section_id),
        json!({}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let me = get_ok(app, &f.aarav, "/me/attendance").await;
    assert_eq!(me["items"][0]["percent"], 50.0);
    assert_eq!(me["items"][0]["belowThreshold"], true);

    // L4-D-025: governed correction of finalised attendance (auto-approved in Batch 4)
    let s2id = id(&s2);
    let rec = s2["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["studentId"] == json!(f.aarav_id))
        .unwrap()
        .clone();
    let rec_id = rec["id"].as_str().unwrap().to_owned();
    post(
        app,
        meera,
        &format!("/sessions/{s2id}/attendance-corrections"),
        json!({"recordId": rec_id, "toMark": "excused", "reason": " "}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    post(
        app,
        meera,
        &format!("/sessions/{s2id}/attendance-corrections"),
        json!({"recordId": rec_id, "toMark": "absent", "reason": "same"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = post(app, meera, &format!("/sessions/{s2id}/attendance-corrections"),
        json!({"recordId": rec_id, "toMark": "present", "reason": "Was at the lab practical", "evidence": "Lab register p.4"}), None, StatusCode::CREATED).await;
    assert_eq!(c["status"], "applied");
    assert_eq!(c["fromMark"], "absent");
    let s2 = get_ok(app, meera, &format!("/sessions/{s2id}")).await;
    let rec = s2["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["studentId"] == json!(f.aarav_id))
        .unwrap()
        .clone();
    assert_eq!(rec["mark"], "present");
    let audit: (String, String) = sqlx::query_as(
        "SELECT from_mark, to_mark FROM academic.attendance_audit WHERE record_id = $1",
    )
    .bind(Uuid::parse_str(&rec_id).unwrap())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(
        audit,
        ("absent".to_owned(), "present".to_owned()),
        "original fact preserved as history"
    );
    assert!(
        sqlx::query("DELETE FROM academic.attendance_audit")
            .execute(&f.pool)
            .await
            .is_err(),
        "append-only"
    );
    assert_eq!(events(&f.pool, inst, "attendance.corrected").await, 1);
    let me = get_ok(app, &f.aarav, "/me/attendance").await;
    assert_eq!(me["items"][0]["percent"], 100.0);
    let s2 = post(
        app,
        meera,
        &format!("/sessions/{s2id}/lock"),
        json!({}),
        Some(ver(&s2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s2["status"], "locked");

    // exceptional outcomes: cancel needs a reason; abandon is administrators only
    let s3 = new_session(&f, meera, "2026-07-08", "09:00").await;
    post(
        app,
        meera,
        &format!("/sessions/{}/cancel", id(&s3)),
        json!({}),
        Some(ver(&s3)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let s3 = post(
        app,
        meera,
        &format!("/sessions/{}/cancel", id(&s3)),
        json!({"reason": "Holiday"}),
        Some(ver(&s3)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s3["status"], "cancelled");
    post(
        app,
        meera,
        &format!("/sessions/{}/start", id(&s3)),
        json!({}),
        Some(ver(&s3)),
        StatusCode::CONFLICT,
    )
    .await;
    let s4 = new_session(&f, meera, "2026-07-09", "09:00").await;
    let s4 = post(
        app,
        meera,
        &format!("/sessions/{}/start", id(&s4)),
        json!({}),
        Some(ver(&s4)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        meera,
        &format!("/sessions/{}/abandon", id(&s4)),
        json!({"reason": "power cut"}),
        Some(ver(&s4)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let s4 = post(
        app,
        admin,
        &format!("/sessions/{}/abandon", id(&s4)),
        json!({"reason": "power cut"}),
        Some(ver(&s4)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s4["status"], "abandoned");
    let fac = get_ok(app, meera, "/my/attendance").await;
    assert_eq!(fac["items"].as_array().unwrap().len(), 1);
    assert_eq!(fac["items"][0]["sessions"].as_array().unwrap().len(), 4);

    // L4-D-026: syllabus coverage → published (immutable; a change is a new superseding record)
    let cov = post(
        app,
        meera,
        &format!("/sections/{}/syllabus-coverage", f.cs.section_id),
        json!({"summary": "Unit 1", "topics": [
        {"title": "Sets", "status": "completed", "sessionId": sid, "learningOutcomeRef": "CO1"},
        {"title": "Relations", "status": "started", "sessionId": s2id},
        {"title": "Functions", "status": "planned"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(cov["topics"].as_array().unwrap().len(), 3);
    assert!((cov["completedPercent"].as_f64().unwrap() - 33.33).abs() < 0.01);
    post(
        app,
        meera,
        &format!("/sections/{}/syllabus-coverage", f.cs.section_id),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // a topic cannot claim a session of another section
    expect(
        app,
        write(
            meera,
            "PUT",
            &format!("{A}/syllabus-coverage/{}/topics", id(&cov)),
            json!({"topics": [{"title": "X", "status": "completed", "sessionId": Uuid::now_v7()}]}),
            Some(ver(&cov)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let cov = post(
        app,
        meera,
        &format!("/syllabus-coverage/{}/submit", id(&cov)),
        json!({}),
        Some(ver(&cov)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        meera,
        &format!("/syllabus-coverage/{}/publish", id(&cov)),
        json!({}),
        Some(ver(&cov)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let cov = post(
        app,
        admin,
        &format!("/syllabus-coverage/{}/publish", id(&cov)),
        json!({}),
        Some(ver(&cov)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cov["status"], "published");
    assert_eq!(
        events(&f.pool, inst, "syllabus_coverage.published").await,
        1
    );
    expect(
        app,
        write(
            meera,
            "PUT",
            &format!("{A}/syllabus-coverage/{}/topics", id(&cov)),
            json!({"topics": []}),
            Some(ver(&cov)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let next = post(
        app,
        meera,
        &format!("/sections/{}/syllabus-coverage", f.cs.section_id),
        json!({}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(next["supersedesId"], json!(id(&cov)));
    assert_eq!(
        next["topics"].as_array().unwrap().len(),
        3,
        "copied from the published record"
    );
}

// ------------------------------------------------------------------------------- monitoring

/// L4-D-027…033: case → risk (derived from attendance) → outcome → exception → governance review
/// with intervention → certification (blocked until obligations clear) → closure → reopening as
/// a new cycle; repeated facts never create a second outcome.
#[tokio::test]
async fn monitoring_case_lifecycle_and_reopening() {
    let f = fixture(T_MONITORING, "Monitoring University", false).await;
    let (app, admin) = (&f.app, &f.admin);
    let inst = f.inst();
    effective_policy(&f, 75.0, 1).await;
    taught_session(&f, "2026-07-06", "present", "absent").await;

    let body = json!({"scopeKind": "student", "sectionId": f.cs.section_id, "studentId": f.bina_id, "title": "Bina attendance risk",
        "ownerSubject": b4::MEERA, "reviewerSubject": ADMIN, "sourceKey": "attendance:bina:t1"});
    post(
        app,
        &f.meera,
        &format!("/institutions/{inst}/monitoring-cases"),
        body.clone(),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let c = post(
        app,
        admin,
        &format!("/institutions/{inst}/monitoring-cases"),
        body.clone(),
        None,
        StatusCode::CREATED,
    )
    .await;
    let cid = id(&c);
    assert_eq!(c["status"], "open");
    // the same authoritative fact returns the existing case (BH-009)
    let again = post(
        app,
        admin,
        &format!("/institutions/{inst}/monitoring-cases"),
        body,
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(id(&again), cid);
    assert_eq!(events(&f.pool, inst, "monitoring_case.opened").await, 1);
    post(
        app,
        admin,
        &format!("/institutions/{inst}/monitoring-cases"),
        json!({"scopeKind": "student", "sectionId": f.cs.section_id,
        "studentId": Uuid::now_v7(), "title": "Nobody"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // L4-D-028: risk derived from finalised attendance under the D05 rule
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/risk-assessments"),
        json!({"category": "attendance", "level": "low", "sourceKey": "eval-1"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(c["status"], "under_assessment");
    assert_eq!(
        c["riskAssessments"][0]["level"], "critical",
        "0 % attendance"
    );
    assert_eq!(c["riskAssessments"][0]["basis"], "attendance_rule");
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/risk-assessments"),
        json!({"category": "attendance", "sourceKey": "eval-1"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        c["riskAssessments"].as_array().unwrap().len(),
        1,
        "same fact, one assessment"
    );
    assert_eq!(
        events(&f.pool, inst, "monitoring_case.risk_assessed").await,
        1
    );

    // L4-D-027: outcome publication (once per cycle)
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/publish-outcome"),
        json!({"summary": "Attendance critical"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["outcomeSnapshot"]["level"], "critical");
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/publish-outcome"),
        json!({"summary": "again"}),
        Some(ver(&c)),
        StatusCode::CONFLICT,
    )
    .await;

    // L4-D-029: governance exception with an independent reviewer
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/exceptions"),
        json!({"category": "evidence", "severity": "high", "description": "Register gap",
        "ownerSubject": ADMIN, "reviewerSubject": ADMIN}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = post(app, admin, &format!("/monitoring-cases/{cid}/exceptions"), json!({"category": "evidence", "severity": "high",
        "description": "Register gap on 6 July", "ownerSubject": b4::MEERA, "reviewerSubject": ADMIN}), None, StatusCode::CREATED).await;
    let ex = c["exceptions"][0].clone();
    let exid = id(&ex);

    // L4-D-032: certification is blocked while obligations remain
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/certify"),
        json!({}),
        Some(ver(&c)),
        StatusCode::CONFLICT,
    )
    .await;

    // L4-D-031/030: governance review (independent reviewer) → approved decision with intervention
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/submit-review"),
        json!({"requiresIntervention": true}),
        Some(ver(&c)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/submit-review"),
        json!({"requiresIntervention": true,
        "interventionTitle": "Remedial attendance plan", "interventionPlan": "Weekly check-ins"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "intervention");
    assert_eq!(c["decisions"][0]["decision"], "approve");
    assert_eq!(c["decisions"][0]["requiresIntervention"], true);
    let iv = c["interventions"][0].clone();
    let ivid = id(&iv);
    assert_eq!(iv["status"], "planned");
    // owner required before triggering (BH-005)
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/interventions/{ivid}/trigger"),
        json!({}),
        Some(ver(&iv)),
        StatusCode::CONFLICT,
    )
    .await;
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/interventions/{ivid}/assign"),
        json!({"ownerSubject": b4::MEERA, "dueOn": "2026-08-01"}),
        Some(ver(&iv)),
        StatusCode::OK,
    )
    .await;
    let iv = c["interventions"][0].clone();
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/interventions/{ivid}/start"),
        json!({}),
        Some(ver(&iv)),
        StatusCode::CONFLICT,
    )
    .await;
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/interventions/{ivid}/trigger"),
        json!({}),
        Some(ver(&iv)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        events(&f.pool, inst, "monitoring_intervention.triggered").await,
        1
    );
    let iv = c["interventions"][0].clone();
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/interventions/{ivid}/start"),
        json!({}),
        Some(ver(&iv)),
        StatusCode::OK,
    )
    .await;
    let iv = c["interventions"][0].clone();
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/interventions/{ivid}/complete"),
        json!({}),
        Some(ver(&iv)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/interventions/{ivid}/complete"),
        json!({"outcome": "Attendance recovered"}),
        Some(ver(&iv)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["interventions"][0]["status"], "completed");
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/certify"),
        json!({}),
        Some(ver(&c)),
        StatusCode::CONFLICT,
    )
    .await;

    // exception: plan → resolve → independent validation
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/exceptions/{exid}/validate"),
        json!({}),
        Some(ver(&ex)),
        StatusCode::CONFLICT,
    )
    .await;
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/exceptions/{exid}/plan"),
        json!({"resolutionPlan": "Re-verify register"}),
        Some(ver(&ex)),
        StatusCode::OK,
    )
    .await;
    let ex = c["exceptions"][0].clone();
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/exceptions/{exid}/resolve"),
        json!({"resolutionNotes": "Register verified"}),
        Some(ver(&ex)),
        StatusCode::OK,
    )
    .await;
    let ex = c["exceptions"][0].clone();
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/exceptions/{exid}/validate"),
        json!({}),
        Some(ver(&ex)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["exceptions"][0]["status"], "closed");
    assert_eq!(c["exceptions"][0]["validatedBy"], ADMIN);

    // L4-D-032: certification → closure with the final snapshot
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/certify"),
        json!({}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "certified");
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/close"),
        json!({"reason": "Monitoring complete"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "closed");
    assert!(c["closureSnapshot"]["riskAssessments"].is_array());
    assert_eq!(events(&f.pool, inst, "monitoring_case.closed").await, 1);
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/risk-assessments"),
        json!({"category": "attendance"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;

    // L4-D-033: reopening creates a distinct new cycle; the closed case is preserved
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/reopen"),
        json!({"reason": " "}),
        Some(ver(&c)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/reopen"),
        json!({"reason": "Attendance dropped again", "evidence": "Week 6 register"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "closed");
    let new_id = c["reopenedAsId"].as_str().unwrap().to_owned();
    let n = get_ok(app, admin, &format!("/monitoring-cases/{new_id}")).await;
    assert_eq!(n["status"], "reopened");
    assert_eq!(n["cycleNo"], 2);
    assert_eq!(n["reopenedFromId"], json!(cid));
    assert_eq!(events(&f.pool, inst, "monitoring_case.reopened").await, 1);
    post(
        app,
        admin,
        &format!("/monitoring-cases/{cid}/reopen"),
        json!({"reason": "again"}),
        Some(ver(&c)),
        StatusCode::CONFLICT,
    )
    .await;
    let list = get_ok(
        app,
        admin,
        &format!("/institutions/{inst}/monitoring-cases"),
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
}

// ------------------------------------------------------------------------------- assessment

/// L4-D-051…057: blueprint → windows/assignments → submission → marking → moderation →
/// finalisation (`internal_assessment.finalized`) → correction → archive.
#[tokio::test]
async fn internal_assessment_and_coursework() {
    let f = fixture(T_ASSESSMENT, "Assessment University", false).await;
    let (app, admin, meera, aarav) = (&f.app, &f.admin, &f.meera, &f.aarav);
    let inst = f.inst();
    let sec = f.cs_section();

    // L4-D-051: draft → validated → published (institutional authority)
    let bp = post(app, meera, &format!("/sections/{sec}/assessment-blueprints"), json!({"title": "CS101 internal", "totalMarks": 50,
        "components": [{"key": "a1", "title": "Assignment 1", "kind": "assignment", "weight": 40}, {"key": "t1", "title": "Test 1", "kind": "test", "weight": 50}],
        "policy": {"moderationRequired": false}}), None, StatusCode::CREATED).await;
    let bpid = id(&bp);
    post(
        app,
        admin,
        &format!("/assessment-blueprints/{bpid}/publish"),
        json!({}),
        Some(ver(&bp)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let bp = expect(app, write(meera, "PATCH", &format!("{A}/assessment-blueprints/{bpid}"), json!({"components": [
        {"key": "a1", "title": "Assignment 1", "kind": "assignment", "weight": 40}, {"key": "t1", "title": "Test 1", "kind": "test", "weight": 60}]}), Some(ver(&bp))), StatusCode::OK).await;
    // a window cannot open before the blueprint is published (BH-008)
    let now = Utc::now();
    post(
        app,
        meera,
        &format!("/sections/{sec}/assessment-windows"),
        json!({"blueprintId": bpid, "componentKey": "a1", "title": "Essay",
        "opensAt": now - Duration::hours(1), "dueAt": now + Duration::days(2), "maxMarks": 10}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        meera,
        &format!("/assessment-blueprints/{bpid}/publish"),
        json!({}),
        Some(ver(&bp)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let bp = post(
        app,
        admin,
        &format!("/assessment-blueprints/{bpid}/publish"),
        json!({}),
        Some(ver(&bp)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(bp["status"], "published");
    assert_eq!(
        events(&f.pool, inst, "assessment_blueprint.published").await,
        1
    );
    expect(
        app,
        write(
            meera,
            "PATCH",
            &format!("{A}/assessment-blueprints/{bpid}"),
            json!({"title": "x"}),
            Some(ver(&bp)),
        ),
        StatusCode::CONFLICT,
    )
    .await;

    // L4-D-052: coursework window opens; students see it
    post(
        app,
        meera,
        &format!("/sections/{sec}/assessment-windows"),
        json!({"blueprintId": bpid, "componentKey": "zz", "title": "Essay",
        "opensAt": now - Duration::hours(1), "dueAt": now + Duration::days(2), "maxMarks": 10}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let a1 = post(app, meera, &format!("/sections/{sec}/assessment-windows"), json!({"blueprintId": bpid, "componentKey": "a1", "title": "Essay on sets",
        "instructions": "500 words", "opensAt": now - Duration::hours(1), "dueAt": now + Duration::days(2), "maxMarks": 10}), None, StatusCode::CREATED).await;
    let a1id = id(&a1);
    assert_eq!(a1["kind"], "assignment");
    assert_eq!(
        get_ok(app, aarav, "/me/assignments").await["items"]
            .as_array()
            .unwrap()
            .len(),
        0,
        "scheduled windows are hidden"
    );
    post(
        app,
        aarav,
        &format!("/me/assignments/{a1id}/submission"),
        json!({"content": "early"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let a1 = post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/open"),
        json!({}),
        Some(ver(&a1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(events(&f.pool, inst, "assessment_window.opened").await, 1);
    let mine = get_ok(app, aarav, "/me/assignments").await;
    assert_eq!(mine["items"][0]["title"], "Essay on sets");
    assert!(mine["items"][0]["submission"].is_null());

    // L4-D-053: one governed submission per student
    post(
        app,
        aarav,
        &format!("/me/assignments/{a1id}/submission"),
        json!({}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let mine = post(
        app,
        aarav,
        &format!("/me/assignments/{a1id}/submission"),
        json!({"content": "Sets are collections of objects"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(mine["submission"]["status"], "submitted");
    post(
        app,
        aarav,
        &format!("/me/assignments/{a1id}/submission"),
        json!({"content": "again"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        events(&f.pool, inst, "assessment_submission.received").await,
        1
    );
    // extension needs authority and a reason
    post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/extend"),
        json!({"dueAt": now + Duration::days(4), "reason": "Fest"}),
        Some(ver(&a1)),
        StatusCode::FORBIDDEN,
    )
    .await;
    let a1 = post(
        app,
        admin,
        &format!("/assessment-windows/{a1id}/extend"),
        json!({"dueAt": now + Duration::days(4), "reason": "College fest"}),
        Some(ver(&a1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(a1["status"], "extended");

    // marking: Aarav 7 then a reasoned re-mark to 8; Bina's outcome recorded by faculty
    let subs = get_ok(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions"),
    )
    .await;
    assert_eq!(subs["items"].as_array().unwrap().len(), 2);
    let asub = subs["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["studentId"] == json!(f.aarav_id))
        .unwrap()
        .clone();
    let bsub = subs["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["studentId"] == json!(f.bina_id))
        .unwrap()
        .clone();
    assert_eq!(bsub["status"], "pending");
    let asid = id(&asub);
    post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions/{asid}/mark"),
        json!({"marks": 11}),
        Some(ver(&asub)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let asub = post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions/{asid}/mark"),
        json!({"marks": 7, "feedback": "Good"}),
        Some(ver(&asub)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions/{asid}/mark"),
        json!({"marks": 8}),
        Some(ver(&asub)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let asub = post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions/{asid}/mark"),
        json!({"marks": 8, "reason": "Missed a page"}),
        Some(ver(&asub)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(asub["marks"], 8.0);
    let hist: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.assessment_mark_history WHERE submission_id = $1",
    )
    .bind(Uuid::parse_str(&asid).unwrap())
    .fetch_one(&f.pool)
    .await
    .unwrap();
    assert_eq!(hist, 2);
    post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions"),
        json!({"studentId": f.bina_id, "marks": 6}),
        None,
        StatusCode::CREATED,
    )
    .await;

    // a test component, held offline: marks recorded directly, Bina absent
    let t1 = post(
        app,
        meera,
        &format!("/sections/{sec}/assessment-windows"),
        json!({"blueprintId": bpid, "componentKey": "t1", "title": "Unit test",
        "opensAt": now - Duration::hours(2), "dueAt": now - Duration::hours(1), "maxMarks": 50}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let t1id = id(&t1);
    let t1 = post(
        app,
        meera,
        &format!("/assessment-windows/{t1id}/open"),
        json!({}),
        Some(ver(&t1)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        aarav,
        &format!("/me/assignments/{t1id}/submission"),
        json!({"content": "late"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        meera,
        &format!("/assessment-windows/{t1id}/submissions"),
        json!({"studentId": f.aarav_id, "marks": 45}),
        None,
        StatusCode::CREATED,
    )
    .await;
    post(
        app,
        meera,
        &format!("/assessment-windows/{t1id}/submissions"),
        json!({"studentId": f.bina_id, "absent": true}),
        None,
        StatusCode::CREATED,
    )
    .await;

    // L4-D-055: finalisation blocked while windows are open
    post(
        app,
        admin,
        &format!("/sections/{sec}/internal-assessment/finalize"),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    post(
        app,
        meera,
        &format!("/assessment-windows/{t1id}/close"),
        json!({}),
        Some(ver(&t1)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/close"),
        json!({}),
        Some(ver(&a1)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(events(&f.pool, inst, "assessment_window.closed").await, 2);

    // L4-D-054: moderation is a new governed fact; the original marks stay
    let m = post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/moderation"),
        json!({"submissionId": asid, "proposedMarks": 9, "reason": "Moderator review"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(m["status"], "applied");
    assert_eq!(m["originalMarks"], 8.0);
    let subs = get_ok(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions"),
    )
    .await;
    let asub = subs["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["studentId"] == json!(f.aarav_id))
        .unwrap()
        .clone();
    assert_eq!(asub["status"], "moderated");
    assert_eq!(asub["marks"], 8.0);
    assert_eq!(asub["moderatedMarks"], 9.0);
    assert_eq!(
        events(&f.pool, inst, "assessment_outcome.moderated").await,
        1
    );

    // L4-D-055: finalise (assessment.finalize) → results + internal_assessment.finalized
    post(
        app,
        meera,
        &format!("/sections/{sec}/internal-assessment/finalize"),
        json!({}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;
    let res = post(
        app,
        admin,
        &format!("/sections/{sec}/internal-assessment/finalize"),
        json!({}),
        None,
        StatusCode::OK,
    )
    .await;
    let items = res["items"].as_array().unwrap().clone();
    assert_eq!(items.len(), 2);
    let ar = items
        .iter()
        .find(|r| r["studentId"] == json!(f.aarav_id))
        .unwrap()
        .clone();
    let br = items
        .iter()
        .find(|r| r["studentId"] == json!(f.bina_id))
        .unwrap()
        .clone();
    // a1: 9/10 × 40 × 0.5 = 18; t1: 45/50 × 60 × 0.5 = 27
    assert_eq!(ar["marksObtained"], 45.0, "{ar}");
    assert_eq!(ar["status"], "finalized");
    // Bina: 6/10 × 40 × 0.5 = 12; absent test = 0
    assert_eq!(br["marksObtained"], 12.0);
    assert_eq!(
        events(&f.pool, inst, "internal_assessment.finalized").await,
        2
    );
    post(
        app,
        admin,
        &format!("/sections/{sec}/internal-assessment/finalize"),
        json!({}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // finalised marks are immutable except through correction
    let asub = get_ok(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions"),
    )
    .await["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["studentId"] == json!(f.aarav_id))
        .unwrap()
        .clone();
    assert_eq!(asub["status"], "finalized");
    post(
        app,
        meera,
        &format!("/assessment-windows/{a1id}/submissions/{asid}/mark"),
        json!({"marks": 10, "reason": "x"}),
        Some(ver(&asub)),
        StatusCode::CONFLICT,
    )
    .await;

    // L4-D-057: governed correction → new result version, original kept as `corrected`
    let brid = id(&br);
    post(
        app,
        admin,
        &format!("/internal-assessment-results/{brid}/corrections"),
        json!({"marksObtained": 51, "reason": "x"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let corr = post(app, admin, &format!("/internal-assessment-results/{brid}/corrections"), json!({"marksObtained": 15, "reason": "Re-evaluated essay", "evidence": "Moderation sheet"}), None, StatusCode::CREATED).await;
    assert_eq!(corr["status"], "applied");
    let res = get_ok(app, meera, &format!("/sections/{sec}/internal-assessment")).await;
    let bina: Vec<Value> = res["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["studentId"] == json!(f.bina_id))
        .cloned()
        .collect();
    assert_eq!(bina.len(), 2);
    assert_eq!(bina[0]["status"], "corrected");
    assert_eq!(bina[0]["marksObtained"], 12.0);
    assert_eq!(bina[1]["status"], "finalized");
    assert_eq!(bina[1]["resultVersion"], 2);
    assert_eq!(bina[1]["marksObtained"], 15.0);
    assert_eq!(
        events(&f.pool, inst, "internal_assessment.corrected").await,
        1
    );
    post(
        app,
        admin,
        &format!("/internal-assessment-results/{brid}/corrections"),
        json!({"marksObtained": 20, "reason": "again"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;

    // L4-D-056: archive a finalised result; never a corrected one
    let ar = post(
        app,
        admin,
        &format!("/internal-assessment-results/{}/archive", id(&ar)),
        json!({}),
        Some(ver(&ar)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(ar["status"], "archived");
    assert_eq!(
        events(&f.pool, inst, "internal_assessment.archived").await,
        1
    );
    post(
        app,
        admin,
        &format!("/internal-assessment-results/{brid}/archive"),
        json!({}),
        Some(ver(&bina[0])),
        StatusCode::CONFLICT,
    )
    .await;
    let mine = get_ok(app, aarav, "/me/assignments").await;
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);
}

// ------------------------------------------------------------------------------- AwaitDecision

fn await_state(pool: &sqlx::PgPool) -> AcademicState {
    let mut st = sutra_api::app::academic_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    st
}

/// Every 4B approval step stays pending under `ApprovalMode::AwaitDecision` until the explicit
/// decision runs (Flowable, Batch 9): timetable approval, attendance correction, monitoring
/// governance decision, internal-assessment finalisation.
#[tokio::test]
async fn approval_steps_await_a_decision() {
    use sutra_academic::{assessment, attendance, monitoring, timetable};
    let f = fixture(T_AWAIT, "Await University", false).await;
    let st = await_state(&f.pool);
    let tenant = f.setup.tenant();
    let admin = auth(ADMIN, Role::Administrator, tenant);
    let meera = auth(b4::MEERA, Role::Faculty, tenant);
    let inst = f.inst();
    let (app, adm) = (&f.app, &f.admin);

    // timetable: submit → pending (validated), reject → draft, resubmit, approve → approved
    let tt = post(
        app,
        adm,
        &format!("/institutions/{inst}/timetables"),
        json!({"academicTermId": f.setup.term_id, "name": "Await"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let tid = Uuid::parse_str(&id(&tt)).unwrap();
    let tt = post(app, adm, &format!("/timetables/{tid}/slots"), json!({"sectionId": f.cs.section_id, "dayOfWeek": 1, "startsAt": "09:00", "endsAt": "10:00"}), Some(ver(&tt)), StatusCode::OK).await;
    let tt = post(
        app,
        adm,
        &format!("/timetables/{tid}/validate"),
        json!({}),
        Some(ver(&tt)),
        StatusCode::OK,
    )
    .await;
    let t = timetable::repo::submit(&st, &admin, tid, Version(ver(&tt)), &actor(ADMIN))
        .await
        .unwrap();
    assert_eq!(t.status.as_str(), "validated");
    assert!(t.pending_decision);
    assert!(t.process_instance_id.is_some());
    assert!(
        timetable::repo::publish(&st, &admin, tid, Version(t.version), &actor(ADMIN))
            .await
            .is_err(),
        "not approved yet"
    );
    assert!(
        timetable::repo::decide(
            &st,
            &admin,
            tid,
            Version(t.version),
            false,
            None,
            &actor(ADMIN)
        )
        .await
        .is_err(),
        "rejection needs a reason"
    );
    let t = timetable::repo::decide(
        &st,
        &admin,
        tid,
        Version(t.version),
        false,
        Some("Clash with exams".into()),
        &actor(ADMIN),
    )
    .await
    .unwrap();
    assert_eq!(t.status.as_str(), "draft");
    assert!(!t.pending_decision);
    let t = timetable::repo::validate(&st, &admin, tid, Version(t.version), &actor(ADMIN))
        .await
        .unwrap();
    let t = timetable::repo::submit(&st, &admin, tid, Version(t.version), &actor(ADMIN))
        .await
        .unwrap();
    let t = timetable::repo::decide(
        &st,
        &admin,
        tid,
        Version(t.version),
        true,
        None,
        &actor(ADMIN),
    )
    .await
    .unwrap();
    assert_eq!(t.status.as_str(), "approved");

    // attendance correction: pending → rejected; the finalised fact stays
    effective_policy(&f, 75.0, 1).await;
    let s = taught_session(&f, "2026-07-06", "absent", "present").await;
    let sid = Uuid::parse_str(&id(&s)).unwrap();
    let rec: Uuid = s["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["studentId"] == json!(f.aarav_id))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    let cmd: attendance::repo::NewAttendanceCorrection =
        serde_json::from_value(json!({"recordId": rec, "toMark": "present", "reason": "Medical"}))
            .unwrap();
    let c = attendance::repo::request_correction(&st, &meera, sid, cmd, &actor(b4::MEERA))
        .await
        .unwrap();
    assert_eq!(c.status.as_str(), "requested");
    assert!(
        attendance::repo::decide_correction(
            &st,
            &meera,
            c.id,
            Version(c.version),
            true,
            None,
            &actor(b4::MEERA)
        )
        .await
        .is_err(),
        "faculty cannot decide"
    );
    let c = attendance::repo::decide_correction(
        &st,
        &admin,
        c.id,
        Version(c.version),
        false,
        Some("No certificate".into()),
        &actor(ADMIN),
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "rejected");
    let mark: String =
        sqlx::query_scalar("SELECT mark FROM academic.attendance_record WHERE id = $1")
            .bind(rec)
            .fetch_one(&f.pool)
            .await
            .unwrap();
    assert_eq!(mark, "absent");
    assert_eq!(events(&f.pool, inst, "attendance.corrected").await, 0);

    // monitoring: governance review stays pending; the owner cannot decide; the authority approves
    let open: monitoring::repo::NewMonitoringCase =
        serde_json::from_value(json!({"scopeKind": "section", "sectionId": f.cs.section_id,
        "title": "Section watch", "ownerSubject": b4::MEERA, "reviewerSubject": ADMIN}))
        .unwrap();
    let (c, _) = monitoring::repo::open(&st, &admin, inst, open, &actor(ADMIN))
        .await
        .unwrap();
    let ra: monitoring::repo::NewRiskAssessment =
        serde_json::from_value(json!({"category": "attendance"})).unwrap();
    let c = monitoring::repo::assess_risk(&st, &admin, c.id, ra, &actor(ADMIN))
        .await
        .unwrap();
    let review: monitoring::repo::GovernanceReview =
        serde_json::from_value(json!({"requiresIntervention": false})).unwrap();
    let c = monitoring::repo::submit_review(
        &st,
        &admin,
        c.id,
        Version(c.version),
        review,
        &actor(ADMIN),
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "under_review");
    assert_eq!(c.pending_decision.as_deref(), Some("governance"));
    assert!(
        monitoring::repo::decide(
            &st,
            &admin,
            c.id,
            Version(c.version),
            true,
            None,
            &actor(b4::MEERA)
        )
        .await
        .is_err(),
        "owner is not the authority"
    );
    let c = monitoring::repo::decide(
        &st,
        &admin,
        c.id,
        Version(c.version),
        true,
        Some("Noted".into()),
        &actor(ADMIN),
    )
    .await
    .unwrap();
    assert_eq!(c.status.as_str(), "under_assessment");
    assert!(c.pending_decision.is_none());
    assert_eq!(c.decisions.len(), 1);

    // internal assessment: finalisation stays pending until decided
    let sec = f.cs_section();
    let bp = post(
        app,
        adm,
        &format!("/sections/{sec}/assessment-blueprints"),
        json!({"title": "IA", "totalMarks": 20,
        "components": [{"key": "q1", "title": "Quiz", "kind": "quiz", "weight": 100}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let bp = post(
        app,
        adm,
        &format!("/assessment-blueprints/{}/publish", id(&bp)),
        json!({}),
        Some(ver(&bp)),
        StatusCode::OK,
    )
    .await;
    let now = Utc::now();
    let w = post(
        app,
        adm,
        &format!("/sections/{sec}/assessment-windows"),
        json!({"blueprintId": id(&bp), "componentKey": "q1", "title": "Quiz 1",
        "opensAt": now - Duration::hours(2), "dueAt": now - Duration::hours(1), "maxMarks": 10}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let w = post(
        app,
        adm,
        &format!("/assessment-windows/{}/open", id(&w)),
        json!({}),
        Some(ver(&w)),
        StatusCode::OK,
    )
    .await;
    post(
        app,
        adm,
        &format!("/assessment-windows/{}/submissions", id(&w)),
        json!({"studentId": f.aarav_id, "marks": 10}),
        None,
        StatusCode::CREATED,
    )
    .await;
    post(
        app,
        adm,
        &format!("/assessment-windows/{}/submissions", id(&w)),
        json!({"studentId": f.bina_id, "marks": 5}),
        None,
        StatusCode::CREATED,
    )
    .await;
    post(
        app,
        adm,
        &format!("/assessment-windows/{}/close", id(&w)),
        json!({}),
        Some(ver(&w)),
        StatusCode::OK,
    )
    .await;
    let rows = assessment::repo::finalize(&st, &admin, f.cs.section_id, &actor(ADMIN))
        .await
        .unwrap();
    assert!(rows.iter().all(|r| r.status.as_str() == "in_progress"));
    assert_eq!(
        events(&f.pool, inst, "internal_assessment.finalized").await,
        0
    );
    let rows = assessment::repo::decide_finalization(
        &st,
        &admin,
        f.cs.section_id,
        true,
        None,
        &actor(ADMIN),
    )
    .await
    .unwrap();
    assert!(rows.iter().all(|r| r.status.as_str() == "finalized"));
    let aarav = rows.iter().find(|r| r.student_id == f.aarav_id).unwrap();
    assert_eq!(aarav.marks_obtained, 20.0);
    assert_eq!(
        events(&f.pool, inst, "internal_assessment.finalized").await,
        2
    );
    let _ = (&f.md, &f.meera);
}
