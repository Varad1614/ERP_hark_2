//! Slice 2B DB tests (owned by 2B). Run against this slice's own database:
//! `DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra_2b cargo test -p sutra-api --features db-tests --test db_b2b`.
//!
//! Covers L3/L4-B-017 … B-026: campus lifecycle (register → activation decision → modify →
//! suspend/restore → decommission (step-up) → archive), academic structure / programme /
//! curriculum publication with immutable successor versions and retirement, school academic
//! model, readiness keys, tenant isolation, permissions, ETag / idempotency, audit + outbox, and
//! the institution status guard.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]

mod support;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_contracts::institution::ids::CampusId;
use sutra_contracts::institution::status::{CampusStatus, PublicationStatus};
use sutra_institution::shared::approval::ApprovalMode;
use sutra_institution::shared::{Actor, InstitutionState};
use sutra_kernel::cache::AdvisoryCache;
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};

const API: &str = "/api/v1/institution";

fn fresh_tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

/// App + owner pool + administrator session + a fresh draft institution.
async fn setup() -> (Router, sqlx::PgPool, Login, String, &'static str) {
    let tenant = fresh_tenant();
    let (app, pool) = app(tenant).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Slice 2B College").await;
    (app, pool, admin, inst, tenant)
}

fn etag(h: &axum::http::HeaderMap) -> i64 {
    h[header::ETAG]
        .to_str()
        .unwrap()
        .trim_matches('"')
        .parse()
        .unwrap()
}

/// Sends and asserts the status, returning the body (and checks `ETag` = `version` on 2xx
/// resources that carry one).
async fn expect(app: &Router, req: Request<Body>, status: StatusCode) -> Value {
    let (s, h, body) = send(app, req).await;
    assert_eq!(s, status, "{body}");
    if s.is_success() && body.get("version").is_some() && h.get(header::ETAG).is_some() {
        assert_eq!(
            body["version"].as_i64().unwrap(),
            etag(&h),
            "ETag = version"
        );
    }
    body
}

fn v(body: &Value) -> i64 {
    body["version"].as_i64().unwrap()
}

fn id(body: &Value) -> String {
    body["id"].as_str().unwrap().to_owned()
}

async fn outbox(pool: &sqlx::PgPool, subject: &str, suffix: &str) -> Vec<Value> {
    sqlx::query_scalar(
        "SELECT envelope FROM platform.outbox WHERE subject = $1 AND event_type = $2 ORDER BY created_at, id",
    )
    .bind(subject)
    .bind(format!("sutraerp.institution.{suffix}"))
    .fetch_all(pool)
    .await
    .unwrap()
}

async fn audits(
    pool: &sqlx::PgPool,
    target: &str,
    action: &str,
) -> Vec<(String, Option<String>, Value)> {
    sqlx::query_as(
        "SELECT actor_kind, actor_id, details FROM platform.audit_log WHERE target_id = $1 AND action = $2 ORDER BY occurred_at, id",
    )
    .bind(target)
    .bind(action)
    .fetch_all(pool)
    .await
    .unwrap()
}

fn campus_body(code: &str, is_main: bool, with_address: bool) -> Value {
    let mut b = json!({
        "code": code,
        "name": format!("{code} Campus"),
        "campusType": if is_main { "main" } else { "satellite" },
        "isMain": is_main,
        "geo": {"latitude": 18.52, "longitude": 73.85},
        "timezone": "Asia/Kolkata",
        "establishedOn": "1999-06-01",
        "declaredCapacity": 4000,
        "contactEmail": "campus@sutra.dev",
        "contactPhone": "+91 20 1234 5678",
    });
    if with_address {
        b["address"] = json!({"line1": "1 University Road", "city": "Pune", "state": "Maharashtra", "postalCode": "411007", "country": "IN"});
    }
    b
}

/// Registers + activates a campus (auto-approval) and returns its body.
async fn active_campus(app: &Router, who: &Login, inst: &str, code: &str, is_main: bool) -> Value {
    let c = expect(
        app,
        write(
            who,
            "POST",
            &format!("{API}/institutions/{inst}/campuses"),
            campus_body(code, is_main, true),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    expect(
        app,
        write(
            who,
            "POST",
            &format!("{API}/campuses/{}/activate", id(&c)),
            json!({}),
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await
}

// ---------------------------------------------------------------------------------------------
// Campuses
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn campus_lifecycle_end_to_end() {
    let (app, pool, admin, inst, _) = setup().await;
    let campuses = format!("{API}/institutions/{inst}/campuses");

    // L3-B-017 registration → registered (Proposed), event + audit + first revision.
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &campuses,
            campus_body("MAIN", true, false),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let cid = id(&c);
    assert_eq!(c["status"], "registered");
    assert_eq!(c["institutionId"], inst.as_str());
    assert_eq!(c["isMain"], true);
    assert_eq!(c["declaredCapacity"], 4000);
    assert_eq!(c["address"]["country"], "IN");
    assert_eq!(v(&c), 1);
    let ev = outbox(&pool, &cid, "campus.registered").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["data"]["code"], "MAIN");
    assert_eq!(ev[0]["data"]["isMain"], true);
    assert_eq!(ev[0]["institutionid"], inst.as_str());
    assert_eq!(
        audits(&pool, &cid, "institution.campus.register")
            .await
            .len(),
        1
    );

    // Code unique per institution; one live main campus; validation.
    expect(
        &app,
        write(
            &admin,
            "POST",
            &campuses,
            campus_body("MAIN", false, false),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let body = expect(
        &app,
        write(
            &admin,
            "POST",
            &campuses,
            campus_body("SECOND", true, false),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(
        body["detail"].as_str().unwrap().contains("main campus"),
        "{body}"
    );
    let mut bad = campus_body("x", false, false);
    bad["timezone"] = json!("IST");
    let body = expect(
        &app,
        write(&admin, "POST", &campuses, bad, None),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(body["errorCode"], "validation-failed");

    // L3-B-018: activation needs the mandatory address; the state is preserved on failure.
    let body = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/activate"),
            json!({}),
            Some(1),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert!(body.to_string().contains("address.line1"), "{body}");
    let got = expect(
        &app,
        get(&admin, &format!("{API}/campuses/{cid}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!((got["status"].as_str(), v(&got)), (Some("registered"), 1));

    // L3-B-019 governed modification: If-Match required, stale rejected, immutables refused.
    let patch = json!({"address": {"line1": "1 University Road", "city": "Pune", "state": "Maharashtra", "postalCode": "411007"}, "description": "Founding campus", "workingHours": "Mon-Sat 08:00-18:00"});
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            patch.clone(),
            None,
        ),
        StatusCode::PRECONDITION_REQUIRED,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            patch.clone(),
            Some(7),
        ),
        StatusCode::PRECONDITION_FAILED,
    )
    .await;
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            json!({"code": "NEW"}),
            Some(1),
        ),
    )
    .await;
    assert!(
        s.is_client_error() && s != StatusCode::OK,
        "code is immutable"
    );
    let (s, _, _) = send(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            json!({"declaredCapacity": 9}),
            Some(1),
        ),
    )
    .await;
    assert!(s.is_client_error(), "capacity is G10's, not modifiable");
    let c = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            patch,
            Some(1),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v(&c), 2);
    assert_eq!(c["code"], "MAIN");
    assert_eq!(c["declaredCapacity"], 4000);
    let ev = outbox(&pool, &cid, "campus.modified").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(
        ev[0]["data"]["changedFields"],
        json!(["address", "description", "workingHours"])
    );
    let a = audits(&pool, &cid, "institution.campus.modify").await;
    assert_eq!(
        a[0].2["before"]["address"]["line1"],
        Value::Null,
        "prior metadata kept"
    );

    // Activation decision (auto-approved through the WorkflowPort) → active.
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/activate"),
            json!({"reason": "Ready"}),
            Some(2),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "active");
    assert_eq!(v(&c), 4, "pending_activation then active");
    assert!(c["processInstanceId"]
        .as_str()
        .unwrap()
        .starts_with("noop-"));
    assert!(c["pendingAction"].is_null());
    let ev = outbox(&pool, &cid, "campus.activated").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["data"]["fromStatus"], "pending_activation");
    assert_eq!(ev[0]["data"]["toStatus"], "active");
    assert_eq!(ev[0]["data"]["processInstanceId"], c["processInstanceId"]);
    assert_eq!(ev[0]["actorref"], "system:auto-approval");
    let a = audits(&pool, &cid, "institution.campus.activate").await;
    assert_eq!(
        (a[0].0.as_str(), a[0].1.as_deref()),
        ("system", Some("system:auto-approval"))
    );
    let req = audits(&pool, &cid, "institution.campus.request_activation").await;
    assert_eq!(req[0].0, "user");

    // Illegal transitions (409) keep the state.
    let cv = v(&c);
    for (verb, body) in [
        ("activate", json!({})),
        ("restore", json!({})),
        ("archive", json!({})),
    ] {
        expect(
            &app,
            write(
                &admin,
                "POST",
                &format!("{API}/campuses/{cid}/{verb}"),
                body,
                Some(cv),
            ),
            StatusCode::CONFLICT,
        )
        .await;
    }
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/decision"),
            json!({"decision": "approve"}),
            Some(cv),
        ),
        StatusCode::CONFLICT,
    )
    .await;

    // L3-B-020 temporary closure / restoration.
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/suspend"),
            json!({}),
            Some(cv),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/suspend"),
            json!({"reason": "Flood repairs"}),
            Some(cv),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "suspended");
    assert_eq!(c["code"], "MAIN", "identity preserved");
    let c = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            json!({"contactPhone": null}),
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await;
    assert!(c["contactPhone"].is_null());
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/decommission"),
            json!({"reason": "x", "effectiveOn": "2026-12-31"}),
            Some(v(&c)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/archive"),
            json!({}),
            Some(v(&c)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/restore"),
            json!({}),
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "active");
    assert_eq!(outbox(&pool, &cid, "campus.suspended").await.len(), 1);
    assert_eq!(outbox(&pool, &cid, "campus.restored").await.len(), 1);

    // L3-B-021: unresolved programme usage blocks decommissioning.
    let p = expect(
        &app,
        write(&admin, "POST", &format!("{API}/institutions/{inst}/programmes"), json!({"code": "BSC-PHY", "name": "B.Sc Physics", "level": "ug", "durationTerms": 6, "campusIds": [cid]}), None),
        StatusCode::CREATED,
    )
    .await;
    let decommission =
        json!({"reason": "Consolidated into the city campus", "effectiveOn": "2026-12-31"});
    let body = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/decommission"),
            decommission.clone(),
            Some(v(&c)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(
        body["detail"].as_str().unwrap().contains("BSC-PHY"),
        "{body}"
    );
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/decommission"),
            json!({"reason": " ", "effectiveOn": "2026-12-31"}),
            Some(v(&c)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/programmes/{}", id(&p)),
            json!({"name": "B.Sc Physics", "level": "ug", "durationTerms": 6, "campusIds": []}),
            Some(v(&p)),
        ),
        StatusCode::OK,
    )
    .await;
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/decommission"),
            decommission,
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "decommissioned");
    assert_eq!(c["decommissionEffectiveOn"], "2026-12-31");
    let ev = outbox(&pool, &cid, "campus.decommissioned").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["data"]["reason"], "Consolidated into the city campus");
    // A decommissioned campus is not a valid offering campus and is read-only.
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/programmes/{}", id(&p)),
            json!({"name": "B.Sc Physics", "level": "ug", "durationTerms": 6, "campusIds": [cid]}),
            Some(v(&p) + 1),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let dv = v(&c);
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            json!({"name": "Back"}),
            Some(dv),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/restore"),
            json!({}),
            Some(dv),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/activate"),
            json!({}),
            Some(dv),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    // A new main campus may be designated once the old one is decommissioned.
    active_campus(&app, &admin, &inst, "CITY", true).await;

    // L3-B-022 archive: terminal; the code is never reused.
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/archive"),
            json!({"reason": "History"}),
            Some(dv),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "archived");
    for (verb, body) in [
        ("restore", json!({})),
        ("suspend", json!({"reason": "r"})),
        ("archive", json!({})),
    ] {
        expect(
            &app,
            write(
                &admin,
                "POST",
                &format!("{API}/campuses/{cid}/{verb}"),
                body,
                Some(v(&c)),
            ),
            StatusCode::CONFLICT,
        )
        .await;
    }
    expect(
        &app,
        write(
            &admin,
            "POST",
            &campuses,
            campus_body("MAIN", false, false),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(outbox(&pool, &cid, "campus.archived").await.len(), 1);

    // History: every accepted change is a snapshot.
    let revs = expect(
        &app,
        get(&admin, &format!("{API}/campuses/{cid}/revisions")),
        StatusCode::OK,
    )
    .await;
    let kinds: Vec<_> = revs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["kind"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(kinds.first().map(String::as_str), Some("registered"));
    assert!(kinds.iter().filter(|k| *k == "modified").count() == 2);
    let last = revs["items"].as_array().unwrap().last().unwrap();
    assert_eq!(last["toStatus"], "archived");
    assert_eq!(last["snapshot"]["status"], "archived");

    // List + filter.
    let list = expect(
        &app,
        get(&admin, &format!("{campuses}?status=active")),
        StatusCode::OK,
    )
    .await;
    let codes: Vec<_> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["code"].as_str().unwrap())
        .collect();
    assert_eq!(codes, vec!["CITY"]);
}

#[tokio::test]
async fn campus_tenant_isolation_permissions_and_idempotency() {
    let (app, pool, admin, inst, _) = setup().await;
    let campuses = format!("{API}/institutions/{inst}/campuses");
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &campuses,
            campus_body("NORTH", false, true),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let cid = id(&c);

    // Another tenant cannot see or touch it (404, indistinguishable from missing).
    let (other, _) = support::app(fresh_tenant()).await;
    let intruder = login(&other, "admin@sutra.dev").await;
    expect(
        &other,
        get(&intruder, &format!("{API}/campuses/{cid}")),
        StatusCode::NOT_FOUND,
    )
    .await;
    expect(&other, get(&intruder, &campuses), StatusCode::NOT_FOUND).await;
    expect(
        &other,
        write(
            &intruder,
            "PATCH",
            &format!("{API}/campuses/{cid}"),
            json!({"name": "Mine"}),
            Some(1),
        ),
        StatusCode::NOT_FOUND,
    )
    .await;
    expect(
        &other,
        write(
            &intruder,
            "POST",
            &format!("{API}/campuses/{cid}/activate"),
            json!({}),
            Some(1),
        ),
        StatusCode::NOT_FOUND,
    )
    .await;
    expect(
        &other,
        write(
            &intruder,
            "POST",
            &campuses,
            campus_body("EVIL", false, false),
            None,
        ),
        StatusCode::NOT_FOUND,
    )
    .await;
    let structures = format!("{API}/institutions/{inst}/academic-structures");
    expect(
        &other,
        write(
            &intruder,
            "POST",
            &structures,
            json!({"name": "S", "model": "higher_education"}),
            None,
        ),
        StatusCode::NOT_FOUND,
    )
    .await;

    // Reads need institution.read (every role); writes are administrator-only.
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    expect(
        &app,
        get(&faculty, &format!("{API}/campuses/{cid}")),
        StatusCode::OK,
    )
    .await;
    expect(&app, get(&student, &campuses), StatusCode::OK).await;
    for who in [&faculty, &student] {
        let b = expect(
            &app,
            write(
                who,
                "POST",
                &campuses,
                campus_body("NOPE", false, false),
                None,
            ),
            StatusCode::FORBIDDEN,
        )
        .await;
        assert_eq!(b["errorCode"], "forbidden");
        expect(
            &app,
            write(
                who,
                "POST",
                &format!("{API}/campuses/{cid}/activate"),
                json!({}),
                Some(1),
            ),
            StatusCode::FORBIDDEN,
        )
        .await;
        expect(
            &app,
            write(
                who,
                "POST",
                &format!("{API}/campuses/{cid}/decommission"),
                json!({"reason": "r", "effectiveOn": "2027-01-01"}),
                Some(1),
            ),
            StatusCode::FORBIDDEN,
        )
        .await;
        expect(
            &app,
            write(
                who,
                "POST",
                &structures,
                json!({"name": "S", "model": "higher_education"}),
                None,
            ),
            StatusCode::FORBIDDEN,
        )
        .await;
    }

    // Idempotency-Key is required on create; a retry replays; a different body is refused.
    let no_key = Request::post(&campuses)
        .header("content-type", "application/json")
        .header("cookie", &admin.cookie)
        .header("x-csrf-token", &admin.csrf)
        .body(Body::from(campus_body("SOUTH", false, false).to_string()))
        .unwrap();
    expect(&app, no_key, StatusCode::BAD_REQUEST).await;
    let keyed = |body: Value| {
        Request::post(&campuses)
            .header("content-type", "application/json")
            .header("cookie", &admin.cookie)
            .header("x-csrf-token", &admin.csrf)
            .header("idempotency-key", "campus-south-1")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let first = expect(
        &app,
        keyed(campus_body("SOUTH", false, false)),
        StatusCode::CREATED,
    )
    .await;
    let (s, h, again) = send(&app, keyed(campus_body("SOUTH", false, false))).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(h["idempotency-replayed"], "true");
    assert_eq!(again["id"], first["id"]);
    let (s, _, _) = send(&app, keyed(campus_body("WEST", false, false))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM institution.campus WHERE institution_id = $1::uuid AND code = 'SOUTH'")
        .bind(&inst)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        outbox(&pool, &id(&first), "campus.registered").await.len(),
        1
    );
}

/// App whose deployment enforces step-up (dev TOTP).
async fn step_up_app(tenant: &'static str) -> (Router, sqlx::PgPool) {
    let owner = owner_pool().await;
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let runtime: &'static str =
        Box::leak(format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}").into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("DATABASE_URL", runtime),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_DEV_MFA", "true"),
        ("SUTRA_APPROVAL_MODE", "auto"), // Part 1: explicit dev opt-in (subject is step-up)
    ]);
    let cfg = Config::load(&src).expect("config");
    (
        router(AppContext::from_config(&cfg).expect("context")),
        owner,
    )
}

fn session_cookie(h: &axum::http::HeaderMap) -> String {
    h[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn campus_decommission_and_archive_need_step_up() {
    use sutra_platform::auth::mfa::{base32_decode, totp_code, totp_step};
    let tenant = fresh_tenant();
    let (app, pool) = step_up_app(tenant).await;
    // Institution registration itself needs step-up: seed it directly.
    let inst: uuid::Uuid = sqlx::query_scalar("INSERT INTO institution.institution (tenant_id, code, name) VALUES ($1::uuid, $2, 'Step-up College') RETURNING id")
        .bind(tenant)
        .bind(code("SU"))
        .fetch_one(&pool)
        .await
        .unwrap();
    let admin = login(&app, "admin@sutra.dev").await;
    // campus.manage does not need step-up.
    let c = active_campus(&app, &admin, &inst.to_string(), "MAIN", true).await;
    let cid = id(&c);
    let decommission = json!({"reason": "Closing", "effectiveOn": "2027-03-31"});
    let b = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/campuses/{cid}/decommission"),
            decommission.clone(),
            Some(v(&c)),
        ),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(b["errorCode"], "mfa-step-up-required");
    assert!(b["detail"]
        .as_str()
        .unwrap()
        .contains("campus.decommission"));

    // Enrol + confirm TOTP: the session is upgraded to MFA.
    let (s, _, enrol) = send(
        &app,
        write(
            &admin,
            "POST",
            "/api/v1/auth/mfa/totp/enrolment",
            json!({}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{enrol}");
    let secret = base32_decode(enrol["secret"].as_str().unwrap()).unwrap();
    let totp = totp_code(&secret, totp_step(chrono::Utc::now().timestamp()));
    let (s, h, body) = send(
        &app,
        write(
            &admin,
            "POST",
            "/api/v1/auth/mfa/totp/confirm",
            json!({"code": totp}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let elevated = Login {
        cookie: session_cookie(&h),
        csrf: body["csrfToken"].as_str().unwrap().to_owned(),
    };
    let c = expect(
        &app,
        write(
            &elevated,
            "POST",
            &format!("{API}/campuses/{cid}/decommission"),
            decommission,
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "decommissioned");
    let c = expect(
        &app,
        write(
            &elevated,
            "POST",
            &format!("{API}/campuses/{cid}/archive"),
            json!({}),
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "archived");
}

fn await_decision_state(pool: &sqlx::PgPool) -> InstitutionState {
    let mut st = InstitutionState::new(
        pool.clone(),
        AdvisoryCache::disabled(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    );
    st.approvals = ApprovalMode::AwaitDecision;
    st
}

fn user() -> Actor {
    Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    }
}

#[tokio::test]
async fn campus_decisions_await_a_decision_maker() {
    use sutra_institution::campus::{domain::RegisterCampus, repo};
    use sutra_institution::curriculum::support::Decision;
    let (_app, pool, _admin, inst, tenant) = setup().await;
    let st = await_decision_state(&pool);
    let inst_id: InstitutionId = inst.parse().unwrap();
    let tenant_scope = ExecutionScope::tenant(tenant.parse::<TenantId>().unwrap());
    let scope = ExecutionScope::institution(tenant_scope.tenant_id, inst_id);
    let cmd: RegisterCampus = serde_json::from_value(campus_body("AWAIT", true, true)).unwrap();
    let c = repo::register(&pool, &scope, inst_id, cmd, &user())
        .await
        .unwrap();
    let cid: CampusId = c.id;

    // Activation stays pending until decided; a rejection returns it to registered.
    let c = repo::activate(&st, &tenant_scope, cid, c.version, None, &user())
        .await
        .unwrap();
    assert_eq!(c.status, CampusStatus::PendingActivation);
    assert_eq!(c.pending_action.as_deref(), Some("activation"));
    assert!(c.process_instance_id.is_some());
    assert!(
        repo::transition(
            &pool,
            &tenant_scope,
            cid,
            c.version,
            CampusStatus::Active,
            None,
            &user()
        )
        .await
        .is_err(),
        "no restore shortcut"
    );
    let c = repo::decide(
        &pool,
        &tenant_scope,
        cid,
        c.version,
        Decision::Reject,
        Some("Missing fire NOC".into()),
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(c.status, CampusStatus::Registered);
    assert!(outbox(&pool, &cid.to_string(), "campus.activated")
        .await
        .is_empty());
    let c = repo::activate(&st, &tenant_scope, cid, c.version, None, &user())
        .await
        .unwrap();
    let c = repo::decide(
        &pool,
        &tenant_scope,
        cid,
        c.version,
        Decision::Approve,
        None,
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(c.status, CampusStatus::Active);
    let ev = outbox(&pool, &cid.to_string(), "campus.activated").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["actorref"], "dev|admin@sutra.dev");

    // Decommissioning awaits too; other transitions are blocked meanwhile.
    let cmd = serde_json::from_value(json!({"reason": "Lease ends", "effectiveOn": "2027-06-30"}))
        .unwrap();
    let c = repo::decommission(&st, &tenant_scope, cid, c.version, cmd, &user())
        .await
        .unwrap();
    assert_eq!(
        (c.status, c.pending_action.as_deref()),
        (CampusStatus::Active, Some("decommission"))
    );
    assert!(repo::transition(
        &pool,
        &tenant_scope,
        cid,
        c.version,
        CampusStatus::Suspended,
        Some("x".into()),
        &user()
    )
    .await
    .is_err());
    assert!(
        repo::decide(
            &pool,
            &tenant_scope,
            cid,
            Version(c.version.0 - 1),
            Decision::Approve,
            None,
            &user()
        )
        .await
        .is_err(),
        "stale"
    );
    let c = repo::decide(
        &pool,
        &tenant_scope,
        cid,
        c.version,
        Decision::Approve,
        None,
        &user(),
    )
    .await
    .unwrap();
    assert_eq!(c.status, CampusStatus::Decommissioned);
    assert_eq!(
        c.decommission_effective_on
            .map(|d| d.to_string())
            .as_deref(),
        Some("2027-06-30")
    );
    assert_eq!(
        outbox(&pool, &cid.to_string(), "campus.decommissioned")
            .await
            .len(),
        1
    );
}

// ---------------------------------------------------------------------------------------------
// Academic structure, programmes, curriculum framework
// ---------------------------------------------------------------------------------------------

fn structure_body(model: &str) -> Value {
    json!({
        "name": "University structure",
        "model": model,
        "nepAligned": true,
        "effectiveFrom": "2026-06-01",
        "nodes": [
            {"kind": "level", "key": "ug", "name": "Undergraduate", "nheqfLevel": 5},
            {"kind": "level", "key": "pg", "name": "Postgraduate", "nheqfLevel": 6.5},
            {"kind": "degree_type", "key": "btech", "name": "B.Tech", "parentKey": "ug", "nheqfLevel": 6},
            {"kind": "stream", "key": "engineering", "name": "Engineering", "parentKey": "btech"},
            {"kind": "discipline", "key": "cse", "name": "Computer Science", "parentKey": "engineering"},
        ]
    })
}

fn slot(term: i64, code: &str, credits: f64, category: &str) -> Value {
    json!({"termNo": term, "slotCode": code, "title": code, "category": category, "credits": credits, "lectureHours": 3, "tutorialHours": 1})
}

fn version_body(term_count: i64, effective_from: &str) -> Value {
    let mut components = vec![];
    for t in 1..=term_count {
        components.push(slot(t, &format!("CS{t}01"), 12.0, "major_core"));
        components.push(slot(t, &format!("MD{t}01"), 8.0, "multidisciplinary"));
    }
    json!({
        "effectiveFrom": effective_from,
        "appliesFromCohort": "2026-27",
        "termStructure": "semester",
        "termCount": term_count,
        "creditFramework": "nep2020",
        "minTotalCredits": 20 * term_count,
        "minCreditsPerTerm": 16,
        "maxCreditsPerTerm": 28,
        "nepAligned": true,
        "nheqfLevel": 6,
        "multipleEntryExit": true,
        "abcEnabled": true,
        "exitAwards": [{"afterTerms": 2, "award": "UG Certificate", "minCredits": 40}],
        "components": components,
    })
}

async fn published_structure(app: &Router, who: &Login, inst: &str, model: &str) -> Value {
    let s = expect(
        app,
        write(
            who,
            "POST",
            &format!("{API}/institutions/{inst}/academic-structures"),
            structure_body(model),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    expect(
        app,
        write(
            who,
            "POST",
            &format!("{API}/academic-structures/{}/publish", id(&s)),
            json!({}),
            Some(v(&s)),
        ),
        StatusCode::OK,
    )
    .await
}

#[tokio::test]
async fn academic_structure_programme_and_curriculum_publication() {
    let (app, pool, admin, inst, _) = setup().await;
    let structures = format!("{API}/institutions/{inst}/academic-structures");

    // Draft validation, then a draft without levels cannot be published.
    let mut bad = structure_body("higher_education");
    bad["nodes"][2]["parentKey"] = json!("nope");
    expect(
        &app,
        write(&admin, "POST", &structures, bad, None),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let s = expect(
        &app,
        write(
            &admin,
            "POST",
            &structures,
            json!({"name": "Empty", "model": "higher_education"}),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let sid = id(&s);
    assert_eq!(
        (s["status"].as_str(), s["versionNo"].as_i64()),
        (Some("draft"), Some(1))
    );
    expect(
        &app,
        write(
            &admin,
            "POST",
            &structures,
            structure_body("higher_education"),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await; // one open draft
    let b = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/publish"),
            json!({}),
            Some(v(&s)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert!(b.to_string().contains("required-for-publication"));
    let s = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/academic-structures/{sid}"),
            structure_body("higher_education"),
            Some(v(&s)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["nodes"].as_array().unwrap().len(), 5);

    // Programme cannot publish before the structure.
    let p = expect(
        &app,
        write(&admin, "POST", &format!("{API}/institutions/{inst}/programmes"), json!({"code": "BTECH-CSE", "name": "B.Tech CSE", "level": "ug", "degreeType": "btech", "durationTerms": 4}), None),
        StatusCode::CREATED,
    )
    .await;
    let pid = id(&p);
    assert_eq!(outbox(&pool, &pid, "programme.registered").await.len(), 1);
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/programmes/{pid}/publish"),
            json!({}),
            Some(v(&p)),
        ),
        StatusCode::CONFLICT,
    )
    .await;

    // Publish the structure (WorkflowPort auto-approval): draft → in_review → published.
    let s = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/publish"),
            json!({}),
            Some(v(&s)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "published");
    assert!(s["processInstanceId"].as_str().is_some());
    let ev = outbox(&pool, &sid, "academic_structure.published").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["data"]["nodeCount"], 5);
    assert_eq!(
        audits(&pool, &sid, "institution.academic_structure.submit").await[0].0,
        "user"
    );
    assert_eq!(
        audits(&pool, &sid, "institution.academic_structure.publish").await[0].0,
        "system"
    );
    // Published structures are immutable (API 409, database trigger), and a new one goes through revise.
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/academic-structures/{sid}"),
            structure_body("mixed"),
            Some(v(&s)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/publish"),
            json!({}),
            Some(v(&s)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &structures,
            structure_body("higher_education"),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(sqlx::query(
        "UPDATE institution.academic_structure SET name = 'hacked' WHERE id = $1::uuid"
    )
    .bind(&sid)
    .execute(&pool)
    .await
    .is_err());
    assert!(sqlx::query(
        "DELETE FROM institution.academic_structure_node WHERE academic_structure_id = $1::uuid"
    )
    .bind(&sid)
    .execute(&pool)
    .await
    .is_err());

    // Programme publication checks the published structure's keys (coherent set).
    let p = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/programmes/{pid}"),
            json!({"name": "B.Tech CSE", "level": "ug", "degreeType": "mtech", "durationTerms": 4}),
            Some(v(&p)),
        ),
        StatusCode::OK,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/programmes/{pid}/publish"),
            json!({}),
            Some(v(&p)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let p = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/programmes/{pid}"),
            json!({"name": "B.Tech CSE", "level": "ug", "degreeType": "btech", "durationTerms": 4}),
            Some(v(&p)),
        ),
        StatusCode::OK,
    )
    .await;

    // Curriculum: first draft version; cannot publish while the programme is a draft.
    let curricula = format!("{API}/programmes/{pid}/curricula");
    let mut bad = json!({"code": "CSE-2026", "title": "B.Tech CSE 2026", "initialVersion": version_body(4, "2026-07-01")});
    bad["initialVersion"]["components"][0]["credits"] = json!(3.25);
    expect(
        &app,
        write(&admin, "POST", &curricula, bad, None),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let mut wrong_terms = version_body(3, "2026-07-01");
    wrong_terms["exitAwards"] =
        json!([{"afterTerms": 2, "award": "UG Certificate", "minCredits": 40}]);
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &curricula,
            json!({"code": "CSE-2026", "title": "B.Tech CSE 2026", "initialVersion": wrong_terms}),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let cid = id(&c);
    assert_eq!(c["status"], "draft");
    let v1_id = c["openVersionId"].as_str().unwrap().to_owned();
    let v1 = expect(
        &app,
        get(&admin, &format!("{API}/curriculum-versions/{v1_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (v1["versionNo"].as_i64(), v1["status"].as_str()),
        (Some(1), Some("draft"))
    );
    assert!((v1["totalCredits"].as_f64().unwrap() - 60.0).abs() < 1e-9);
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/publish"),
            json!({}),
            Some(v(&v1)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let p = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/programmes/{pid}/publish"),
            json!({}),
            Some(v(&p)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "published");
    assert_eq!(outbox(&pool, &pid, "programme.published").await.len(), 1);
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/programmes/{pid}"),
            json!({"name": "X", "level": "ug", "durationTerms": 4}),
            Some(v(&p)),
        ),
        StatusCode::CONFLICT,
    )
    .await;

    // Credit rules: term count must match the programme duration (422), fix via PATCH.
    let b = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/publish"),
            json!({}),
            Some(v(&v1)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert!(b.to_string().contains("programme-mismatch"), "{b}");
    let v1 = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/curriculum-versions/{v1_id}"),
            version_body(4, "2026-07-01"),
            Some(v(&v1)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v1["components"].as_array().unwrap().len(), 8);
    let v1 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/publish"),
            json!({}),
            Some(v(&v1)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v1["status"], "published");
    let ev = outbox(&pool, &v1_id, "curriculum_version.published").await;
    assert_eq!(ev.len(), 1);
    assert_eq!(ev[0]["data"]["versionNo"], 1);
    assert!(ev[0]["data"]["supersedesVersionId"].is_null());
    assert_eq!(ev[0]["data"]["totalCredits"], "80");
    assert_eq!(ev[0]["data"]["effectiveFrom"], "2026-07-01");
    let c = expect(
        &app,
        get(&admin, &format!("{API}/curricula/{cid}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (c["status"].as_str(), c["currentVersionId"].as_str()),
        (Some("published"), Some(v1_id.as_str()))
    );

    // L3-B-024: published versions are immutable; a revision creates a linked successor.
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/curriculum-versions/{v1_id}"),
            version_body(4, "2026-07-01"),
            Some(v(&v1)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(sqlx::query(
        "UPDATE institution.curriculum_version SET term_count = 6 WHERE id = $1::uuid"
    )
    .bind(&v1_id)
    .execute(&pool)
    .await
    .is_err());
    assert!(sqlx::query("UPDATE institution.curriculum_component SET credits = 1 WHERE curriculum_version_id = $1::uuid").bind(&v1_id).execute(&pool).await.is_err());
    let revisions = format!("{API}/curricula/{cid}/revisions");
    expect(
        &app,
        write(
            &admin,
            "POST",
            &revisions,
            json!({"changeSummary": "Too early", "effectiveFrom": "2025-01-01"}),
            None,
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let v2 = expect(&app, write(&admin, "POST", &revisions, json!({"changeSummary": "BoS 2027: AI elective", "effectiveFrom": "2027-07-01", "appliesFromCohort": "2027-28"}), None), StatusCode::CREATED).await;
    let v2_id = id(&v2);
    assert_eq!(
        (v2["versionNo"].as_i64(), v2["status"].as_str()),
        (Some(2), Some("draft"))
    );
    assert_eq!(v2["supersedesVersionId"], v1_id.as_str());
    assert_eq!(v2["components"], v1["components"], "content copied");
    expect(
        &app,
        write(
            &admin,
            "POST",
            &revisions,
            json!({"changeSummary": "Second", "effectiveFrom": "2027-07-01"}),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let mut changed = version_body(4, "2027-07-01");
    changed["changeSummary"] = json!("BoS 2027: AI elective");
    changed["components"]
        .as_array_mut()
        .unwrap()
        .push(slot(4, "AI401", 4.0, "major_elective"));
    let v2 = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/curriculum-versions/{v2_id}"),
            changed,
            Some(v(&v2)),
        ),
        StatusCode::OK,
    )
    .await;
    // The predecessor stays authoritative until the successor publishes.
    let c = expect(
        &app,
        get(&admin, &format!("{API}/curricula/{cid}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (c["currentVersionId"].as_str(), c["openVersionId"].as_str()),
        (Some(v1_id.as_str()), Some(v2_id.as_str()))
    );
    let v2 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v2_id}/publish"),
            json!({}),
            Some(v(&v2)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v2["status"], "published");
    let v1_now = expect(
        &app,
        get(&admin, &format!("{API}/curriculum-versions/{v1_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v1_now["status"], "superseded");
    assert_eq!(
        v1_now["components"], v1["components"],
        "predecessor content untouched"
    );
    let ev = outbox(&pool, &v2_id, "curriculum_version.published").await;
    assert_eq!(ev[0]["data"]["supersedesVersionId"], v1_id.as_str());
    assert_eq!(ev[0]["data"]["versionNo"], 2);
    let chain = expect(
        &app,
        get(&admin, &format!("{API}/curricula/{cid}/versions")),
        StatusCode::OK,
    )
    .await;
    let statuses: Vec<_> = chain["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| {
            (
                x["versionNo"].as_i64().unwrap(),
                x["status"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        statuses,
        vec![(2, "published".to_owned()), (1, "superseded".to_owned())]
    );
    // A superseded version cannot be published again or retired; nothing reactivates.
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/publish"),
            json!({}),
            Some(v(&v1_now)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/retire"),
            json!({"reason": "r"}),
            Some(v(&v1_now)),
        ),
        StatusCode::CONFLICT,
    )
    .await;

    // L3-B-025 retirement: dependencies block, explicit cascade, CurriculumRetired only.
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/programmes/{pid}/retire"),
            json!({"reason": "Closing"}),
            Some(v(&p)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/retire"),
            json!({"reason": "Closing"}),
            Some(v(&s)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curricula/{cid}/retire"),
            json!({}),
            Some(v(&c)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = expect(
        &app,
        get(&admin, &format!("{API}/curricula/{cid}")),
        StatusCode::OK,
    )
    .await;
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curricula/{cid}/retire"),
            json!({"reason": "Programme discontinued"}),
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "retired");
    assert!(c["currentVersionId"].is_null());
    let v2_now = expect(
        &app,
        get(&admin, &format!("{API}/curriculum-versions/{v2_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v2_now["status"], "retired");
    assert_eq!(outbox(&pool, &cid, "curriculum.retired").await.len(), 1);
    assert!(
        outbox(&pool, &v2_id, "curriculum_version.retired")
            .await
            .is_empty(),
        "no legacy CurriculumVersionRetired"
    );
    expect(
        &app,
        write(
            &admin,
            "POST",
            &revisions,
            json!({"changeSummary": "Revive", "effectiveFrom": "2028-07-01"}),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curricula/{cid}/retire"),
            json!({"reason": "again"}),
            Some(v(&c)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v2_id}/publish"),
            json!({}),
            Some(v(&v2_now)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    let p = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/programmes/{pid}/retire"),
            json!({"reason": "Closing"}),
            Some(v(&p)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(p["status"], "retired");
    assert_eq!(outbox(&pool, &pid, "programme.retired").await.len(), 1);
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/programmes/{pid}/publish"),
            json!({}),
            Some(v(&p)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(&app, write(&admin, "POST", &curricula, json!({"code": "CSE-2030", "title": "New", "initialVersion": version_body(4, "2030-07-01")}), None), StatusCode::CONFLICT).await;
    let s = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/retire"),
            json!({"reason": "Replaced"}),
            Some(v(&s)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "retired");
    let ev = outbox(&pool, &sid, "academic_structure.retired").await;
    assert_eq!(ev[0]["data"]["fromStatus"], "published");
    assert_eq!(
        audits(&pool, &cid, "institution.curriculum.retire")
            .await
            .len(),
        1
    );
}

#[tokio::test]
async fn stale_successor_and_structure_revision_chain() {
    let (app, pool, admin, inst, _) = setup().await;
    let s1 = published_structure(&app, &admin, &inst, "higher_education").await;
    let s1_id = id(&s1);
    // Structure successor: revise → edit → publish supersedes v1.
    let s2 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{s1_id}/revise"),
            json!({"effectiveFrom": "2027-06-01"}),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (s2["versionNo"].as_i64(), s2["supersedesId"].as_str()),
        (Some(2), Some(s1_id.as_str()))
    );
    let mut body = structure_body("higher_education");
    body["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"kind": "level", "key": "doctoral", "name": "Doctoral", "nheqfLevel": 8}));
    let s2 = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/academic-structures/{}", id(&s2)),
            body,
            Some(v(&s2)),
        ),
        StatusCode::OK,
    )
    .await;
    let s2 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{}/publish", id(&s2)),
            json!({}),
            Some(v(&s2)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s2["status"], "published");
    let s1 = expect(
        &app,
        get(&admin, &format!("{API}/academic-structures/{s1_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s1["status"], "superseded");
    assert_eq!(s1["nodes"].as_array().unwrap().len(), 5);
    let ev = outbox(&pool, &id(&s2), "academic_structure.published").await;
    assert_eq!(ev[0]["data"]["supersedesId"], s1_id.as_str());
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{s1_id}/revise"),
            json!({}),
            None,
        ),
        StatusCode::CONFLICT,
    )
    .await;

    // Stale successor: v2 draft was based on v1; v1 is retired meanwhile → v2 cannot publish.
    let p = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/institutions/{inst}/programmes"),
            json!({"code": "BTECH", "name": "B.Tech", "level": "ug", "durationTerms": 4}),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let p = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/programmes/{}/publish", id(&p)),
            json!({}),
            Some(v(&p)),
        ),
        StatusCode::OK,
    )
    .await;
    let c = expect(&app, write(&admin, "POST", &format!("{API}/programmes/{}/curricula", id(&p)), json!({"code": "BT-2026", "title": "B.Tech 2026", "initialVersion": version_body(4, "2026-07-01")}), None), StatusCode::CREATED).await;
    let v1_id = c["openVersionId"].as_str().unwrap().to_owned();
    let v1 = expect(
        &app,
        get(&admin, &format!("{API}/curriculum-versions/{v1_id}")),
        StatusCode::OK,
    )
    .await;
    let v1 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/publish"),
            json!({}),
            Some(v(&v1)),
        ),
        StatusCode::OK,
    )
    .await;
    let v2 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curricula/{}/revisions", id(&c)),
            json!({"changeSummary": "Revision", "effectiveFrom": "2027-07-01"}),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/retire"),
            json!({}),
            Some(v(&v1)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let v1 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{v1_id}/retire"),
            json!({"reason": "Regulator withdrew approval"}),
            Some(v(&v1)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(v1["status"], "retired");
    assert_eq!(
        outbox(&pool, &v1_id, "curriculum_version.retired")
            .await
            .len(),
        1
    );
    let b = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/curriculum-versions/{}/publish", id(&v2)),
            json!({}),
            Some(v(&v2)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(b["detail"].as_str().unwrap().contains("stale"), "{b}");
    let v2 = expect(
        &app,
        get(&admin, &format!("{API}/curriculum-versions/{}", id(&v2))),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        v2["status"], "draft",
        "a refused publication changes nothing"
    );
}

#[tokio::test]
async fn publication_awaiting_a_decision_can_be_returned_and_approved() {
    use sutra_institution::curriculum::publication::{decide, submit, Kind};
    use sutra_institution::curriculum::support::Decision;
    let (app, pool, admin, inst, tenant) = setup().await;
    let st = await_decision_state(&pool);
    let scope = ExecutionScope::tenant(tenant.parse::<TenantId>().unwrap());
    let s = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/institutions/{inst}/academic-structures"),
            structure_body("school"),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let sid: uuid::Uuid = id(&s).parse().unwrap();
    submit(&st, &scope, Kind::Structure, sid, Version(v(&s)), &user())
        .await
        .unwrap();
    let s = expect(
        &app,
        get(&admin, &format!("{API}/academic-structures/{sid}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "in_review");
    // In review: content is frozen (API and trigger) and not yet authoritative.
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/academic-structures/{sid}"),
            structure_body("school"),
            Some(v(&s)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(
        outbox(&pool, &sid.to_string(), "academic_structure.published")
            .await
            .is_empty()
    );
    // Return for correction (reason required) → draft; decide on a draft → 409.
    let b = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/decision"),
            json!({"decision": "return"}),
            Some(v(&s)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(b["errorCode"], "validation-failed");
    let s = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/decision"),
            json!({"decision": "return", "reason": "Add NHEQF levels"}),
            Some(v(&s)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        (s["status"].as_str(), s["statusReason"].as_str()),
        (Some("draft"), Some("Add NHEQF levels"))
    );
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/decision"),
            json!({"decision": "approve"}),
            Some(v(&s)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(
        audits(
            &pool,
            &sid.to_string(),
            "institution.academic_structure.return"
        )
        .await
        .len(),
        1
    );
    // Resubmit and approve as the decision-maker.
    submit(&st, &scope, Kind::Structure, sid, Version(v(&s)), &user())
        .await
        .unwrap();
    let s = expect(
        &app,
        get(&admin, &format!("{API}/academic-structures/{sid}")),
        StatusCode::OK,
    )
    .await;
    let s = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/academic-structures/{sid}/decision"),
            json!({"decision": "approve", "reason": "BoS resolution 12"}),
            Some(v(&s)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "published");
    let ev = outbox(&pool, &sid.to_string(), "academic_structure.published").await;
    assert_eq!(ev[0]["actorref"], "dev|admin@sutra.dev");

    // School model: a rejected successor leaves the published model authoritative (L4-B-026-BH-006).
    let m = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/institutions/{inst}/school-models"),
            school_body(),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let m = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/school-models/{}/publish", id(&m)),
            json!({}),
            Some(v(&m)),
        ),
        StatusCode::OK,
    )
    .await;
    let m2 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/school-models/{}/revise", id(&m)),
            json!({}),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let m2_id: uuid::Uuid = id(&m2).parse().unwrap();
    submit(
        &st,
        &scope,
        Kind::SchoolModel,
        m2_id,
        Version(v(&m2)),
        &user(),
    )
    .await
    .unwrap();
    let m2 = expect(
        &app,
        get(&admin, &format!("{API}/school-models/{m2_id}")),
        StatusCode::OK,
    )
    .await;
    decide(
        &pool,
        &scope,
        Kind::SchoolModel,
        m2_id,
        Version(v(&m2)),
        Decision::Reject,
        Some("Not approved".into()),
        &user(),
    )
    .await
    .unwrap();
    let m2 = expect(
        &app,
        get(&admin, &format!("{API}/school-models/{m2_id}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m2["status"], "draft");
    let m1 = expect(
        &app,
        get(&admin, &format!("{API}/school-models/{}", id(&m))),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m1["status"], "published");
    assert_eq!(
        audits(&pool, &m2_id.to_string(), "institution.school_model.reject")
            .await
            .len(),
        1
    );
}

fn school_body() -> Value {
    json!({
        "name": "CBSE school model",
        "board": "cbse",
        "nepStageAligned": true,
        "effectiveFrom": "2026-04-01",
        "gradeLevels": [
            {"key": "balvatika", "name": "Balvatika", "nepStage": "foundational", "sections": ["A"], "subjects": [{"code": "PLAY", "name": "Play-based learning", "kind": "core"}]},
            {"key": "1", "name": "Class 1", "nepStage": "foundational", "sections": ["A", "B"], "subjects": [{"code": "LANG1", "name": "Language R1", "kind": "language"}, {"code": "MATH", "name": "Mathematics", "kind": "core"}]},
            {"key": "6", "name": "Class 6", "nepStage": "middle", "sections": ["A", "B", "C"], "subjects": [{"code": "SCI", "name": "Science", "kind": "core"}, {"code": "ART", "name": "Art education", "kind": "co_scholastic"}]},
        ]
    })
}

#[tokio::test]
async fn school_academic_model_establishment_and_readiness() {
    let (app, pool, admin, inst, tenant) = setup().await;
    let scope = ExecutionScope::tenant(tenant.parse::<TenantId>().unwrap());
    let inst_id: InstitutionId = inst.parse().unwrap();
    let models = format!("{API}/institutions/{inst}/school-models");

    // Before anything: every 2B key is present; school model does not apply yet.
    let r = sutra_institution::shared::readiness::evaluate(&pool, &scope, inst_id)
        .await
        .unwrap();
    let status_of = |r: &sutra_institution::shared::readiness::ReadinessReport, k: &str| {
        serde_json::to_value(
            r.items
                .iter()
                .find(|i| i.key == k)
                .unwrap_or_else(|| panic!("{k}"))
                .status,
        )
        .unwrap()
    };
    for k in [
        "campus.active_campus",
        "curriculum.academic_structure_published",
        "curriculum.programme_published",
        "curriculum.curriculum_published",
    ] {
        assert_eq!(status_of(&r, k), "unmet", "{k}");
    }
    assert_eq!(
        status_of(&r, "curriculum.school_model_published"),
        "not_applicable"
    );
    assert!(!r.ready);

    // Validation and publication rules.
    let mut bad = school_body();
    bad["gradeLevels"][1]["key"] = json!("balvatika");
    expect(
        &app,
        write(&admin, "POST", &models, bad, None),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let mut no_sections = school_body();
    no_sections["gradeLevels"][2]["sections"] = json!([]);
    let m = expect(
        &app,
        write(&admin, "POST", &models, no_sections, None),
        StatusCode::CREATED,
    )
    .await;
    let mid = id(&m);
    // A school model needs a published school / mixed structure first.
    expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/school-models/{mid}/publish"),
            json!({}),
            Some(v(&m)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    published_structure(&app, &admin, &inst, "school").await;
    let r = sutra_institution::shared::readiness::evaluate(&pool, &scope, inst_id)
        .await
        .unwrap();
    assert_eq!(
        status_of(&r, "curriculum.programme_published"),
        "not_applicable",
        "not a programme model"
    );
    assert_eq!(
        status_of(&r, "curriculum.curriculum_published"),
        "not_applicable"
    );
    assert_eq!(status_of(&r, "curriculum.school_model_published"), "unmet");
    let b = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/school-models/{mid}/publish"),
            json!({}),
            Some(v(&m)),
        ),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert!(b.to_string().contains("sections"), "{b}");
    let m = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/school-models/{mid}"),
            school_body(),
            Some(v(&m)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m["gradeLevels"][2]["sections"], json!(["A", "B", "C"]));
    let m = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/school-models/{mid}/publish"),
            json!({}),
            Some(v(&m)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m["status"], "published");
    let ev = outbox(&pool, &mid, "school_model.published").await;
    assert_eq!(ev[0]["data"]["gradeLevels"], json!(["balvatika", "1", "6"]));
    assert_eq!(ev[0]["data"]["board"], "cbse");
    expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/school-models/{mid}"),
            school_body(),
            Some(v(&m)),
        ),
        StatusCode::CONFLICT,
    )
    .await;
    expect(
        &app,
        write(&admin, "POST", &models, school_body(), None),
        StatusCode::CONFLICT,
    )
    .await;
    assert!(sqlx::query(
        "UPDATE institution.school_grade_level SET name = 'x' WHERE school_model_id = $1::uuid"
    )
    .bind(&mid)
    .execute(&pool)
    .await
    .is_err());

    // Successor model supersedes.
    let m2 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/school-models/{mid}/revise"),
            json!({"effectiveFrom": "2027-04-01"}),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        (m2["versionNo"].as_i64(), m2["supersedesId"].as_str()),
        (Some(2), Some(mid.as_str()))
    );
    let m2 = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/school-models/{}/publish", id(&m2)),
            json!({}),
            Some(v(&m2)),
        ),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m2["status"], "published");
    let m1 = expect(
        &app,
        get(&admin, &format!("{API}/school-models/{mid}")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(m1["status"], "superseded");
    let list = expect(
        &app,
        get(&admin, &format!("{models}?status=published")),
        StatusCode::OK,
    )
    .await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);

    // With an active campus every 2B key is met / not applicable.
    active_campus(&app, &admin, &inst, "SCHOOL", true).await;
    let r = sutra_institution::shared::readiness::evaluate(&pool, &scope, inst_id)
        .await
        .unwrap();
    assert_eq!(status_of(&r, "campus.active_campus"), "met");
    assert_eq!(
        status_of(&r, "curriculum.academic_structure_published"),
        "met"
    );
    assert_eq!(status_of(&r, "curriculum.school_model_published"), "met");
    assert_eq!(
        r.items
            .iter()
            .filter(|i| i.key.starts_with("campus.") || i.key.starts_with("curriculum."))
            .count(),
        5
    );
}

#[tokio::test]
async fn setup_is_allowed_before_activation_and_frozen_when_suspended_or_archived() {
    let (app, pool, admin, inst, _) = setup().await;
    let campuses = format!("{API}/institutions/{inst}/campuses");
    let c = expect(
        &app,
        write(
            &admin,
            "POST",
            &campuses,
            campus_body("BASE", false, true),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let s = expect(
        &app,
        write(
            &admin,
            "POST",
            &format!("{API}/institutions/{inst}/academic-structures"),
            structure_body("higher_education"),
            None,
        ),
        StatusCode::CREATED,
    )
    .await;
    let set_status = |status: &'static str| {
        let pool = pool.clone();
        let inst = inst.clone();
        async move {
            sqlx::query("UPDATE institution.institution SET status = $1 WHERE id = $2::uuid")
                .bind(status)
                .bind(inst)
                .execute(&pool)
                .await
                .unwrap();
        }
    };
    for (i, status) in [
        "draft",
        "configured",
        "pending_approval",
        "returned",
        "rejected",
        "active",
    ]
    .into_iter()
    .enumerate()
    {
        set_status(status).await;
        expect(
            &app,
            write(
                &admin,
                "POST",
                &campuses,
                campus_body(&format!("OK-{i}"), false, false),
                None,
            ),
            StatusCode::CREATED,
        )
        .await;
    }
    let c = expect(
        &app,
        write(
            &admin,
            "PATCH",
            &format!("{API}/campuses/{}", id(&c)),
            json!({"name": "Base campus"}),
            Some(v(&c)),
        ),
        StatusCode::OK,
    )
    .await;
    for status in ["suspended", "archived"] {
        set_status(status).await;
        let b = expect(
            &app,
            write(
                &admin,
                "POST",
                &campuses,
                campus_body("FROZEN", false, false),
                None,
            ),
            StatusCode::CONFLICT,
        )
        .await;
        assert!(b["detail"].as_str().unwrap().contains("read-only"), "{b}");
        expect(
            &app,
            write(
                &admin,
                "PATCH",
                &format!("{API}/campuses/{}", id(&c)),
                json!({"name": "No"}),
                Some(v(&c)),
            ),
            StatusCode::CONFLICT,
        )
        .await;
        expect(
            &app,
            write(
                &admin,
                "POST",
                &format!("{API}/campuses/{}/activate", id(&c)),
                json!({}),
                Some(v(&c)),
            ),
            StatusCode::CONFLICT,
        )
        .await;
        expect(
            &app,
            write(
                &admin,
                "POST",
                &format!("{API}/academic-structures/{}/publish", id(&s)),
                json!({}),
                Some(v(&s)),
            ),
            StatusCode::CONFLICT,
        )
        .await;
        expect(
            &app,
            write(
                &admin,
                "POST",
                &format!("{API}/institutions/{inst}/programmes"),
                json!({"code": "PX", "name": "P", "level": "ug", "durationTerms": 2}),
                None,
            ),
            StatusCode::CONFLICT,
        )
        .await;
        expect(
            &app,
            write(
                &admin,
                "POST",
                &format!("{API}/institutions/{inst}/school-models"),
                school_body(),
                None,
            ),
            StatusCode::CONFLICT,
        )
        .await;
        // Reads stay available.
        expect(&app, get(&admin, &campuses), StatusCode::OK).await;
        expect(
            &app,
            get(&admin, &format!("{API}/academic-structures/{}", id(&s))),
            StatusCode::OK,
        )
        .await;
    }
    let st: String =
        sqlx::query_scalar("SELECT status FROM institution.academic_structure WHERE id = $1::uuid")
            .bind(id(&s))
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(st, PublicationStatus::Draft.as_str());
}
