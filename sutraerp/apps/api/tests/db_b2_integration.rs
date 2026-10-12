//! Batch 2 cross-slice integration test (integration-owned): one institution walks the whole
//! setup path across 2A (lifecycle, profile, blueprint, org unit), 2B (campus, structure,
//! programme, curriculum) and 2C (calendar, open year, assignment) until every required
//! readiness item is satisfied, then submits and becomes active. Also checks the 2B × 2C
//! wiring: calendar assignments validate scopes against the real 2A/2B tables, and a campus
//! still governed by a calendar assignment cannot be decommissioned.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use support::*;

const API: &str = "/api/v1/institution";

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
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, v)).await;
    assert_eq!(s, want, "{method} {path}: {b}");
    b
}

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

fn ver(b: &Value) -> Option<i64> {
    b["version"].as_i64()
}

fn id(b: &Value) -> String {
    b["id"].as_str().unwrap().to_owned()
}

fn blocking(r: &Value) -> Vec<String> {
    r["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["required"] == true && i["status"] == "unmet")
        .map(|i| i["key"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn register_configure_set_up_all_slices_then_activate() {
    let (app, pool) = app(tenant()).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Integrated University").await;
    let readiness = format!("{API}/institutions/{inst}/activation-readiness");

    // Fresh draft: every provider reports its keys, and every slice blocks.
    let r = read(&app, &admin, &readiness).await;
    assert_eq!(r["institutionStatus"], "draft");
    assert_eq!(r["ready"], false);
    let keys: Vec<&str> = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["key"].as_str().unwrap())
        .collect();
    for k in [
        "institution.profile_complete",
        "institution.timezone_set",
        "institution.policies_effective",
        "organisation.root_unit_active",
        "organisation.governance_active",
        "position.definitions_active",
        "campus.active_campus",
        "curriculum.academic_structure_published",
        "curriculum.programme_published",
        "curriculum.curriculum_published",
        "curriculum.school_model_published",
        "calendar.version_published",
        "calendar.academic_year_open",
        "calendar.assignment_effective",
    ] {
        assert!(keys.contains(&k), "missing {k} in {keys:?}");
    }
    let b = blocking(&r);
    for k in [
        "institution.profile_complete",
        "organisation.root_unit_active",
        "campus.active_campus",
        "curriculum.curriculum_published",
        "calendar.version_published",
    ] {
        assert!(b.contains(&k.to_owned()), "{k} should block: {b:?}");
    }

    // 2A: profile → configure; blueprint + active root unit.
    let cur = read(&app, &admin, &format!("{API}/institutions/{inst}")).await;
    let cur = call(
        &app,
        &admin,
        "PUT",
        &format!("{API}/institutions/{inst}/profile"),
        json!({
            "shortName": "IU", "institutionType": "university", "registrationNumber": "REG-IU",
            "address": "1 Campus Road, Pune", "contactPhone": "+91 20 1234 5678",
            "officialEmail": "office@iu.edu", "academicPattern": "semester",
            "workingDays": ["mon", "tue", "wed", "thu", "fri"], "defaultLanguage": "en-IN",
            "administratorEmail": "admin@iu.edu"
        }),
        ver(&cur),
        StatusCode::OK,
    )
    .await;
    let cur = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/institutions/{inst}/configure"),
        json!({}),
        ver(&cur),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cur["status"], "configured");
    let today: chrono::NaiveDate = sqlx::query_scalar(
        "SELECT (now() AT TIME ZONE timezone)::date FROM institution.institution WHERE id = $1::uuid",
    )
    .bind(&inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    let bp = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/institutions/{inst}/org-type-blueprints"),
        json!({"effectiveFrom": today.to_string(), "unitTypes": [
            {"key": "university", "label": "University", "allowRoot": true},
            {"key": "department", "label": "Department", "allowedParentTypes": ["university"]},
        ]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{API}/org-type-blueprints/{}/publish", id(&bp)),
        json!({}),
        ver(&bp),
        StatusCode::OK,
    )
    .await;
    let root = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/institutions/{inst}/org-units"),
        json!({"unitType": "university", "code": "ROOT", "name": "Integrated University"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let root = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/org-units/{}/activate", id(&root)),
        json!({}),
        ver(&root),
        StatusCode::OK,
    )
    .await;
    assert_eq!(root["status"], "active");

    // Still blocked by 2B/2C: submit refuses with 409 naming the open keys.
    let (s, _, err) = send(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/institutions/{inst}/submit"),
            json!({}),
            ver(&cur),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{err}");
    assert!(err.to_string().contains("campus.active_campus"), "{err}");

    // 2B: main campus, structure → programme → curriculum version.
    let main = b2::active_campus(&app, &admin, &inst, "MAIN", true).await;
    let (programme, _) = b2::published_curriculum(&app, &admin, &inst).await;
    // 2C: published calendar, open year, effective institution assignment.
    let (cal, version) = b2::published_calendar(&app, &admin, &inst).await;

    // 2C scope checks run against the real 2A/2B tables.
    for (kind, scope) in [
        ("campus", id(&main)),
        ("org_unit", id(&root)),
        ("programme", id(&programme)),
    ] {
        let a = call(
            &app,
            &admin,
            "POST",
            &format!("{API}/calendars/{cal}/assignments"),
            json!({"calendarVersionId": id(&version), "scopeKind": kind, "scopeId": scope, "effectiveFrom": "2026-06-01"}),
            None,
            StatusCode::CREATED,
        )
        .await;
        assert_eq!(a["scopeId"], scope.as_str());
    }
    // A programme id is not a campus.
    call(
        &app,
        &admin,
        "POST",
        &format!("{API}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": id(&version), "scopeKind": "campus", "scopeId": id(&programme), "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // 2B × 2C: a satellite campus governed by a calendar assignment cannot be decommissioned.
    let sat = b2::active_campus(&app, &admin, &inst, "SAT", false).await;
    call(
        &app,
        &admin,
        "POST",
        &format!("{API}/calendars/{cal}/assignments"),
        json!({"calendarVersionId": id(&version), "scopeKind": "campus", "scopeId": id(&sat), "effectiveFrom": "2026-06-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let err = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/campuses/{}/decommission", id(&sat)),
        json!({"reason": "Lease ends", "effectiveOn": "2027-03-31"}),
        ver(&sat),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(
        err["detail"]
            .as_str()
            .unwrap_or_default()
            .contains("calendar assignment"),
        "{err}"
    );

    // Readiness: every required item satisfied; the school model does not apply.
    let r = read(&app, &admin, &readiness).await;
    assert_eq!(blocking(&r), Vec::<String>::new(), "{r}");
    assert_eq!(r["ready"], true, "{r}");
    assert_eq!(r["institutionStatus"], "configured");
    let school = r["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["key"] == "curriculum.school_model_published")
        .unwrap();
    assert_eq!(school["status"], "not_applicable");

    // Submit → pending_approval → (auto-approval) → active.
    let cur = read(&app, &admin, &format!("{API}/institutions/{inst}")).await;
    let done = call(
        &app,
        &admin,
        "POST",
        &format!("{API}/institutions/{inst}/submit"),
        json!({}),
        ver(&cur),
        StatusCode::OK,
    )
    .await;
    assert_eq!(done["status"], "active", "{done}");
    let r = read(&app, &admin, &readiness).await;
    assert_eq!(r["institutionStatus"], "active");
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM platform.outbox WHERE subject = $1 ORDER BY created_at, id",
    )
    .bind(&inst)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert!(
        events.iter().any(|e| e.ends_with("institution.activated")),
        "{events:?}"
    );
}
