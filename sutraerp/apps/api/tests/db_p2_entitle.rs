//! Part 2 · Stage 7 (S7-entitle) — module entitlements and licence state on real PostgreSQL,
//! through the assembled application (`docs/readiness/ENTITLEMENT_IMPLEMENTATION.md`):
//!
//! * `module_gate_is_server_side_whatever_the_role` — tenants without a stored entitlement keep
//!   every module (`complete_erp` default); after `core_academic` is approved a **direct API call**
//!   to payroll, hostel or fees answers `403 entitlement.module_inactive` even for the
//!   administrator and the payroll checker who hold the permissions; core (institution, campus
//!   facilities) stays reachable; add-on modules open their routes at once.
//! * `invalid_activations_are_rejected` — dependency validation (`payroll` needs `hr`,
//!   `inventory` needs `procurement`), unknown package / module, validity window, grace range,
//!   kind / status mismatch, permission, one pending request per tenant.
//! * `expired_licence_is_read_only_but_exportable_and_renewal_restores` — injectable clock:
//!   active → grace (full access + `Sutra-Licence-Status: grace`) → expired (reads and the data
//!   export work, writes `403 entitlement.licence_expired`) → renewal restores writes immediately.
//! * `suspended_licence_leaves_platform_admin_and_export` — suspension leaves only the platform
//!   administration area for administrators and the export; reinstatement restores access.
//! * `entitlements_are_isolated_per_tenant` — one tenant's entitlement never applies to, is never
//!   visible to and can never be decided by another; the new tables carry forced RLS.
//! * `separation_of_duties_and_audit` — the requester can never decide (API and database CHECK),
//!   only the requester withdraws, a request made against an older entitlement version is refused,
//!   every step is audited.
//! * `core_academic_without_fees_admits_and_issues_hall_tickets` — commercial-review gap (a):
//!   with outstanding Finance charges, clearance is refused and the hall ticket withheld while
//!   `fees` is active; on `core_academic` (no fees) admission clears (`notApplicable`), is
//!   confirmed and creates the student, and the hall ticket is released.
//! * `import_jobs_are_gated_by_their_target_module` — commercial-review gap (b): staff / fee-head /
//!   opening-balance imports are refused (`entitlement.module_inactive`) and hidden on
//!   `core_academic`, a job created before the downgrade cannot run, student / set-up imports and
//!   every export stay available, the HR add-on reopens staff imports.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::{HeaderMap, StatusCode};
use axum::Router;
use chrono::{Duration, Timelike, Utc};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_kernel::entitlement::{Clock, ManualClock};

const ENT: &str = "/api/v1/platform/entitlement";
const REQS: &str = "/api/v1/platform/entitlement-requests";

/// A tenant of its own for every test (reruns on the same database never collide).
fn fresh_tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

/// Postgres-backed app (runtime role) in `tenant`; the entitlement gate runs on `clock` when
/// given (wall clock otherwise).
async fn ent_app(tenant: &'static str, clock: Option<Arc<ManualClock>>) -> (Router, sqlx::PgPool) {
    let owner = owner_pool().await;
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let runtime: &'static str =
        Box::leak(format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}").into_boxed_str());
    let src: HashMap<&'static str, &'static str> = HashMap::from([
        ("SUTRA_ENV", "development"),
        ("SUTRA_DEV_AUTH_PASSWORD", sutra_api::app::SPEC_DEV_PASSWORD),
        ("DATABASE_URL", runtime),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_APPROVAL_MODE", "auto"),
    ]);
    let cfg = Config::load(&src).expect("config");
    let mut ctx = AppContext::from_config(&cfg).expect("context");
    if let Some(c) = clock {
        ctx = ctx.with_entitlement_clock(c);
    }
    (router(ctx), owner)
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    if method == "GET" {
        send(app, get(who, path)).await
    } else {
        send(app, write(who, method, path, body, None)).await
    }
}

fn code(v: &Value) -> &str {
    v["errorCode"].as_str().unwrap_or_default()
}

/// The request reached its handler (whatever the handler answered).
fn not_gated(what: &str, r: &(StatusCode, HeaderMap, Value)) {
    assert!(
        !code(&r.2).starts_with("entitlement."),
        "{what} was refused by the entitlement gate: {} {}",
        r.0,
        r.2
    );
}

fn refused(what: &str, r: &(StatusCode, HeaderMap, Value), want: &str) {
    assert_eq!(r.0, StatusCode::FORBIDDEN, "{what}: {}", r.2);
    assert_eq!(code(&r.2), want, "{what}: {}", r.2);
}

/// `maker` requests `body`, `checker` approves it; returns the decision result.
async fn change(app: &Router, maker: &Login, checker: &Login, body: Value) -> Value {
    let (s, _, req) = call(app, maker, "POST", REQS, body).await;
    assert_eq!(s, StatusCode::CREATED, "{req}");
    let id = req["id"].as_str().unwrap().to_owned();
    let (s, _, decided) = call(
        app,
        checker,
        "POST",
        &format!("{REQS}/{id}/decision"),
        json!({"decision": "approve", "reason": "contract signed"}),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{decided}");
    assert_eq!(decided["request"]["status"], "approved");
    decided
}

fn modules(v: &Value) -> Vec<String> {
    v["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m.as_str().unwrap().to_owned())
        .collect()
}

// ------------------------------------------------------------------ module gate

#[tokio::test]
async fn module_gate_is_server_side_whatever_the_role() {
    let tenant = fresh_tenant();
    let (app, _owner) = ent_app(tenant, None).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let payroll = login(&app, "payroll.approver@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Entitlement College").await;

    // No stored entitlement: the default (complete_erp, perpetual) — existing tenants keep all.
    let (s, _, e) = call(&app, &student, "GET", ENT, Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{e}");
    assert_eq!(e["origin"], "default");
    assert_eq!(e["package"], "complete_erp");
    assert_eq!(e["status"], "active");
    assert_eq!(modules(&e).len(), 15);
    let periods = format!("/api/v1/people/institutions/{inst}/payroll-periods");
    not_gated(
        "payroll before",
        &call(&app, &admin, "GET", &periods, Value::Null).await,
    );

    // Catalogue: modules with dependencies and the four packages.
    let (s, _, cat) = call(
        &app,
        &student,
        "GET",
        &format!("{ENT}/catalogue"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cat}");
    assert_eq!(cat["packages"].as_array().unwrap().len(), 4);
    let payroll_spec = cat["modules"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["key"] == "payroll")
        .unwrap()
        .clone();
    assert_eq!(payroll_spec["requires"], json!(["hr"]));
    assert_eq!(payroll_spec["crate"], "sutra-people");

    // core_academic approved by a second administrator.
    let decided = change(
        &app,
        &admin,
        &dean,
        json!({"kind": "activate", "package": "core_academic", "reason": "pilot contract"}),
    )
    .await;
    let mut want = vec![
        "academics",
        "admissions",
        "attendance",
        "examinations",
        "students",
    ];
    want.sort_unstable();
    assert_eq!(modules(&decided["entitlement"]), want);
    assert_eq!(decided["entitlement"]["origin"], "stored");

    // Direct API calls to inactive modules: 403 whatever the caller's roles.
    refused(
        "payroll list (administrator)",
        &call(&app, &admin, "GET", &periods, Value::Null).await,
        "entitlement.module_inactive",
    );
    refused(
        "payroll list (payroll checker)",
        &call(&app, &payroll, "GET", &periods, Value::Null).await,
        "entitlement.module_inactive",
    );
    refused(
        "payroll config create",
        &call(
            &app,
            &admin,
            "POST",
            &format!("/api/v1/people/institutions/{inst}/payroll-configs"),
            json!({}),
        )
        .await,
        "entitlement.module_inactive",
    );
    refused(
        "hostel policies",
        &call(
            &app,
            &admin,
            "GET",
            &format!("/api/v1/campus/institutions/{inst}/hostel-policies"),
            Value::Null,
        )
        .await,
        "entitlement.module_inactive",
    );
    refused(
        "student fees",
        &call(
            &app,
            &student,
            "GET",
            "/api/v1/finance/me/payments",
            Value::Null,
        )
        .await,
        "entitlement.module_inactive",
    );
    refused(
        "fee heads",
        &call(
            &app,
            &admin,
            "GET",
            &format!("/api/v1/finance/institutions/{inst}/fee-heads"),
            Value::Null,
        )
        .await,
        "entitlement.module_inactive",
    );
    // Active modules and the core stay reachable.
    not_gated(
        "exam policies",
        &call(
            &app,
            &admin,
            "GET",
            &format!("/api/v1/exams/institutions/{inst}/exam-policies"),
            Value::Null,
        )
        .await,
    );
    not_gated(
        "institution",
        &call(
            &app,
            &admin,
            "GET",
            &format!("{INSTITUTIONS}/{inst}"),
            Value::Null,
        )
        .await,
    );
    not_gated(
        "campus service desk (core facilities)",
        &call(
            &app,
            &admin,
            "GET",
            &format!("/api/v1/campus/institutions/{inst}/tickets"),
            Value::Null,
        )
        .await,
    );
    not_gated(
        "own student record",
        &call(
            &app,
            &student,
            "GET",
            "/api/v1/student/me/student",
            Value::Null,
        )
        .await,
    );

    // Add-on modules (hr + payroll) open payroll at once.
    let decided = change(
        &app,
        &admin,
        &dean,
        json!({"kind": "change", "package": "core_academic", "addOnModules": ["payroll", "hr"],
               "reason": "payroll add-on"}),
    )
    .await;
    assert_eq!(
        decided["entitlement"]["addOnModules"],
        json!(["hr", "payroll"])
    );
    not_gated(
        "payroll after add-on",
        &call(&app, &payroll, "GET", &periods, Value::Null).await,
    );
    refused(
        "hostel still inactive",
        &call(
            &app,
            &admin,
            "GET",
            &format!("/api/v1/campus/institutions/{inst}/hostel-policies"),
            Value::Null,
        )
        .await,
        "entitlement.module_inactive",
    );
}

// ------------------------------------------------------------------ validation

#[tokio::test]
async fn invalid_activations_are_rejected() {
    let tenant = fresh_tenant();
    let (app, _owner) = ent_app(tenant, None).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let now = Utc::now();
    for (body, field_code) in [
        (
            json!({"kind": "change", "package": "core_academic", "addOnModules": ["payroll"], "reason": "x"}),
            "dependency_missing",
        ),
        (
            json!({"kind": "change", "package": "core_academic", "addOnModules": ["inventory"], "reason": "x"}),
            "dependency_missing",
        ),
        (
            json!({"kind": "activate", "package": "gold", "reason": "x"}),
            "unknown_package",
        ),
        (
            json!({"kind": "change", "package": "core_academic", "addOnModules": ["teleport"], "reason": "x"}),
            "unknown_module",
        ),
        (
            json!({"kind": "renew", "package": "complete_erp", "validFrom": now,
                   "validUntil": now - Duration::days(1), "reason": "x"}),
            "after_valid_from",
        ),
        (
            json!({"kind": "renew", "package": "complete_erp", "graceDays": 400, "reason": "x"}),
            "range",
        ),
        (
            json!({"kind": "suspend", "package": "complete_erp", "licenceStatus": "active", "reason": "x"}),
            "kind_mismatch",
        ),
        (
            json!({"kind": "upgrade", "package": "complete_erp", "reason": "x"}),
            "enum",
        ),
        (
            json!({"kind": "activate", "package": "complete_erp", "reason": "  "}),
            "length",
        ),
    ] {
        let (s, _, v) = call(&app, &admin, "POST", REQS, body.clone()).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{body} → {v}");
        assert_eq!(v["validationErrors"][0]["code"], field_code, "{body} → {v}");
    }
    let (s, _, v) = call(
        &app,
        &admin,
        "POST",
        REQS,
        json!({"kind": "change", "package": "core_academic", "addOnModules": ["payroll"], "reason": "x"}),
    )
    .await;
    assert!(
        v["validationErrors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("`payroll` requires `hr`"),
        "{s} {v}"
    );
    // Only platform administrators request changes.
    let (s, _, _) = call(
        &app,
        &faculty,
        "POST",
        REQS,
        json!({"kind": "activate", "package": "complete_erp", "reason": "x"}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = call(&app, &faculty, "GET", REQS, Value::Null).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    // One pending request per tenant.
    let ok = json!({"kind": "activate", "package": "institution_operations", "graceDays": 30,
                    "validUntil": now + Duration::days(365), "reason": "annual licence"});
    let (s, _, v) = call(&app, &admin, "POST", REQS, ok.clone()).await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["status"], "pending");
    assert_eq!(v["baseVersion"], 0);
    let (s, _, v) = call(&app, &admin, "POST", REQS, ok).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    // Nothing applied while pending.
    let (_, _, e) = call(&app, &admin, "GET", ENT, Value::Null).await;
    assert_eq!(e["origin"], "default");
}

// ------------------------------------------------------------------ expiry + renewal

#[tokio::test]
async fn expired_licence_is_read_only_but_exportable_and_renewal_restores() {
    let tenant = fresh_tenant();
    // Whole seconds: PostgreSQL keeps microseconds, so day arithmetic stays exact.
    let clock = Arc::new(ManualClock::new(Utc::now().with_nanosecond(0).unwrap()));
    let (app, _owner) = ent_app(tenant, Some(clock.clone())).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Expiry College").await;
    let start = clock.now();
    change(
        &app,
        &admin,
        &dean,
        json!({"kind": "activate", "package": "complete_erp",
               "validFrom": start - Duration::days(10), "validUntil": start + Duration::days(5),
               "graceDays": 10, "reason": "term licence"}),
    )
    .await;
    let (_, h, e) = call(&app, &faculty, "GET", ENT, Value::Null).await;
    assert_eq!(e["status"], "active", "{e}");
    assert_eq!(e["readOnly"], false);
    assert_eq!(e["daysRemaining"], 5, "{e}");
    assert!(h.get("sutra-licence-status").is_none());

    // Grace: past validUntil, inside the 10 grace days — full access plus the banner header.
    clock.advance(Duration::days(8));
    let (_, _, e) = call(&app, &faculty, "GET", ENT, Value::Null).await;
    assert_eq!(e["status"], "grace", "{e}");
    assert_eq!(e["readOnly"], false);
    let (s, h, v) = send(
        &app,
        write(
            &admin,
            "POST",
            INSTITUTIONS,
            json!({"code": code_of("GR"), "name": "Grace write"}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(h["sutra-licence-status"], "grace");

    // Expired: reads, own data, export — yes; writes — no.
    clock.advance(Duration::days(10));
    let (_, _, e) = call(&app, &faculty, "GET", ENT, Value::Null).await;
    assert_eq!(e["status"], "expired", "{e}");
    assert_eq!(e["readOnly"], true);
    let r = call(
        &app,
        &admin,
        "GET",
        &format!("{INSTITUTIONS}/{inst}"),
        Value::Null,
    )
    .await;
    assert_eq!(r.0, StatusCode::OK, "{}", r.2);
    assert_eq!(r.1["sutra-licence-status"], "expired");
    not_gated(
        "exam read",
        &call(
            &app,
            &admin,
            "GET",
            &format!("/api/v1/exams/institutions/{inst}/exam-policies"),
            Value::Null,
        )
        .await,
    );
    refused(
        "institution write",
        &call(
            &app,
            &admin,
            "POST",
            INSTITUTIONS,
            json!({"code": code_of("EX"), "name": "Expired write"}),
        )
        .await,
        "entitlement.licence_expired",
    );
    refused(
        "exam write",
        &call(
            &app,
            &admin,
            "POST",
            &format!("/api/v1/exams/institutions/{inst}/grading-schemes"),
            json!({}),
        )
        .await,
        "entitlement.licence_expired",
    );
    refused(
        "import upload",
        &call(&app, &admin, "POST", "/api/v1/platform/imports", json!({})).await,
        "entitlement.licence_expired",
    );
    let export = format!(
        "/api/v1/platform/imports/entities/students/export?institutionId={inst}&format=csv"
    );
    let r = call(&app, &admin, "GET", &export, Value::Null).await;
    assert_eq!(r.0, StatusCode::OK, "export after expiry: {}", r.2);

    // Renewal (approved while expired) restores writes on the very next request.
    let now = clock.now();
    let decided = change(
        &app,
        &admin,
        &dean,
        json!({"kind": "renew", "package": "complete_erp", "validFrom": now - Duration::days(1),
               "validUntil": now + Duration::days(365), "graceDays": 15, "reason": "renewed"}),
    )
    .await;
    assert_eq!(decided["entitlement"]["status"], "active");
    let (s, h, v) = send(
        &app,
        write(
            &admin,
            "POST",
            INSTITUTIONS,
            json!({"code": code_of("RN"), "name": "Renewed write"}),
            None,
        ),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert!(h.get("sutra-licence-status").is_none());
}

fn code_of(prefix: &str) -> String {
    support::code(prefix)
}

// ------------------------------------------------------------------ suspension

#[tokio::test]
async fn suspended_licence_leaves_platform_admin_and_export() {
    let tenant = fresh_tenant();
    let (app, _owner) = ent_app(tenant, None).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Suspension College").await;
    change(
        &app,
        &admin,
        &dean,
        json!({"kind": "suspend", "package": "complete_erp", "reason": "unpaid invoice"}),
    )
    .await;
    let (_, _, e) = call(&app, &student, "GET", ENT, Value::Null).await;
    assert_eq!(e["status"], "suspended", "{e}");
    assert_eq!(e["readOnly"], true);
    refused(
        "student record",
        &call(
            &app,
            &student,
            "GET",
            "/api/v1/student/me/student",
            Value::Null,
        )
        .await,
        "entitlement.licence_suspended",
    );
    refused(
        "institution (administrator)",
        &call(
            &app,
            &admin,
            "GET",
            &format!("{INSTITUTIONS}/{inst}"),
            Value::Null,
        )
        .await,
        "entitlement.licence_suspended",
    );
    refused(
        "platform area (student)",
        &call(
            &app,
            &student,
            "GET",
            "/api/v1/platform/workspaces",
            Value::Null,
        )
        .await,
        "entitlement.licence_suspended",
    );
    let r = call(
        &app,
        &admin,
        "GET",
        "/api/v1/platform/workspaces",
        Value::Null,
    )
    .await;
    assert_eq!(
        r.0,
        StatusCode::OK,
        "platform administration stays: {}",
        r.2
    );
    let export = format!("/api/v1/platform/imports/entities/students/export?institutionId={inst}");
    let r = call(&app, &admin, "GET", &export, Value::Null).await;
    assert_eq!(r.0, StatusCode::OK, "export while suspended: {}", r.2);

    change(
        &app,
        &admin,
        &dean,
        json!({"kind": "reinstate", "package": "complete_erp", "reason": "invoice paid"}),
    )
    .await;
    not_gated(
        "student record after reinstatement",
        &call(
            &app,
            &student,
            "GET",
            "/api/v1/student/me/student",
            Value::Null,
        )
        .await,
    );
}

// ------------------------------------------------------------------ tenancy

#[tokio::test]
async fn entitlements_are_isolated_per_tenant() {
    let (ta, tb) = (fresh_tenant(), fresh_tenant());
    let (app_a, owner) = ent_app(ta, None).await;
    let (app_b, _) = ent_app(tb, None).await;
    let admin_a = login(&app_a, "admin@sutra.dev").await;
    let dean_a = login(&app_a, "dean@sutra.dev").await;
    let admin_b = login(&app_b, "admin@sutra.dev").await;
    let dean_b = login(&app_b, "dean@sutra.dev").await;
    let inst_b = register_institution(&app_b, &admin_b, "Tenant B College").await;
    change(
        &app_a,
        &admin_a,
        &dean_a,
        json!({"kind": "activate", "package": "core_academic", "reason": "A only"}),
    )
    .await;
    // B is untouched.
    let (_, _, e) = call(&app_b, &admin_b, "GET", ENT, Value::Null).await;
    assert_eq!(e["origin"], "default", "{e}");
    assert_eq!(e["package"], "complete_erp");
    not_gated(
        "B payroll",
        &call(
            &app_b,
            &admin_b,
            "GET",
            &format!("/api/v1/people/institutions/{inst_b}/payroll-periods"),
            Value::Null,
        )
        .await,
    );
    // A's pending request is invisible to B and cannot be decided from B.
    let (s, _, pending) = call(
        &app_a,
        &admin_a,
        "POST",
        REQS,
        json!({"kind": "change", "package": "academic_plus", "reason": "upgrade"}),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{pending}");
    let id = pending["id"].as_str().unwrap();
    let (_, _, list_b) = call(&app_b, &admin_b, "GET", REQS, Value::Null).await;
    assert_eq!(list_b["total"], 0, "{list_b}");
    let (s, _, _) = call(
        &app_b,
        &dean_b,
        "POST",
        &format!("{REQS}/{id}/decision"),
        json!({"decision": "approve", "reason": "cross-tenant"}),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    // Stored rows: A only; forced RLS on both new tables.
    let rows: Vec<(uuid::Uuid, Option<String>)> = sqlx::query_as(
        "SELECT tenant_id, package FROM platform.tenant_entitlement WHERE tenant_id = ANY($1)",
    )
    .bind(vec![
        ta.parse::<uuid::Uuid>().unwrap(),
        tb.parse::<uuid::Uuid>().unwrap(),
    ])
    .fetch_all(&owner)
    .await
    .unwrap();
    assert_eq!(
        rows,
        vec![(ta.parse().unwrap(), Some("core_academic".to_owned()))]
    );
    let gaps: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.tenant_rls_gaps WHERE table_name LIKE 'tenant_entitlement%'",
    )
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(gaps, 0);
    let forced: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
         WHERE n.nspname = 'platform' AND c.relname IN ('tenant_entitlement', 'tenant_entitlement_request') \
           AND c.relrowsecurity AND c.relforcerowsecurity",
    )
    .fetch_one(&owner)
    .await
    .unwrap();
    assert_eq!(forced, 2);
}

// ------------------------------------------------------------------ SoD + audit

#[tokio::test]
async fn separation_of_duties_and_audit() {
    let tenant = fresh_tenant();
    let (app, owner) = ent_app(tenant, None).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let faculty = login(&app, "meera.shah@sutra.dev").await;
    let finance = login(&app, "finance.approver@sutra.dev").await;
    let body = json!({"kind": "activate", "package": "academic_plus", "reason": "SoD"});

    let (_, _, req) = call(&app, &admin, "POST", REQS, body.clone()).await;
    let id = req["id"].as_str().unwrap().to_owned();
    let decide = |d: &str| json!({"decision": d, "reason": "checked"});
    let path = format!("{REQS}/{id}/decision");
    // The requester never decides; neither do roles without platform.entitlement.approve.
    let (s, _, v) = call(&app, &admin, "POST", &path, decide("approve")).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{v}");
    assert!(
        v["detail"]
            .as_str()
            .unwrap()
            .contains("separation of duties"),
        "{v}"
    );
    for who in [&faculty, &finance] {
        let (s, _, _) = call(&app, who, "POST", &path, decide("approve")).await;
        assert_eq!(s, StatusCode::FORBIDDEN);
    }
    // A second administrator rejects: nothing changes.
    let (s, _, v) = call(&app, &dean, "POST", &path, decide("reject")).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["request"]["status"], "rejected");
    assert_eq!(v["entitlement"]["origin"], "default");
    let (s, _, _) = call(&app, &dean, "POST", &path, decide("approve")).await;
    assert_eq!(s, StatusCode::CONFLICT, "decided once");

    // Only the requester withdraws.
    let (_, _, req) = call(&app, &admin, "POST", REQS, body.clone()).await;
    let id = req["id"].as_str().unwrap().to_owned();
    let (s, _, _) = call(
        &app,
        &dean,
        "POST",
        &format!("{REQS}/{id}/withdraw"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, v) = call(
        &app,
        &admin,
        "POST",
        &format!("{REQS}/{id}/withdraw"),
        Value::Null,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "withdrawn");

    // A request made against an older entitlement version is refused.
    let (_, _, req) = call(&app, &admin, "POST", REQS, body.clone()).await;
    let id = req["id"].as_str().unwrap().to_owned();
    sqlx::query(
        "INSERT INTO platform.tenant_entitlement (tenant_id, package, valid_from, request_id, updated_by) \
         VALUES ($1, 'complete_erp', now(), $2, 'test:concurrent')",
    )
    .bind(tenant.parse::<uuid::Uuid>().unwrap())
    .bind(uuid::Uuid::now_v7())
    .execute(&owner)
    .await
    .unwrap();
    let (s, _, v) = call(
        &app,
        &dean,
        "POST",
        &format!("{REQS}/{id}/decision"),
        decide("approve"),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (s, _, v) = call(
        &app,
        &dean,
        "POST",
        &format!("{REQS}/{id}/decision"),
        decide("reject"),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::OK,
        "a stale request can still be rejected: {v}"
    );
    // Approve a fresh one.
    let (s, _, req) = call(&app, &admin, "POST", REQS, body).await;
    assert_eq!(s, StatusCode::CREATED, "{req}");
    assert_eq!(req["baseVersion"], 1);
    let id = req["id"].as_str().unwrap().to_owned();
    let (s, _, v) = call(
        &app,
        &dean,
        "POST",
        &format!("{REQS}/{id}/decision"),
        decide("approve"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["entitlement"]["package"], "academic_plus");
    assert_eq!(v["entitlement"]["version"], 2);

    // Database CHECK: the requester can never be the decider, whatever the writer.
    let err = sqlx::query(
        "UPDATE platform.tenant_entitlement_request SET status = 'approved', \
           decided_by = requested_by, decided_at = now() WHERE id = $1",
    )
    .bind(id.parse::<uuid::Uuid>().unwrap())
    .execute(&owner)
    .await;
    assert!(err.is_err(), "CHECK decided_by <> requested_by");

    // Every step is audited.
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM platform.audit_log WHERE tenant_id = $1 \
           AND action LIKE 'platform.entitlement.%' ORDER BY occurred_at",
    )
    .bind(tenant.parse::<uuid::Uuid>().unwrap())
    .fetch_all(&owner)
    .await
    .unwrap();
    for a in [
        "platform.entitlement.change_requested",
        "platform.entitlement.change_rejected",
        "platform.entitlement.change_withdrawn",
        "platform.entitlement.change_approved",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} missing: {actions:?}");
    }
}

// ------------------------------------------------------------------ cross-module facts (fees)

/// Unsafe request with `If-Match` (admission / hall-ticket commands).
async fn cmd(app: &Router, who: &Login, path: &str, if_match: Option<i64>) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, "POST", path, json!({}), if_match)).await;
    (s, b)
}

fn ver(v: &Value) -> i64 {
    v["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no version in {v}"))
}

fn id_of(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {v}"))
        .to_owned()
}

/// `POST path` (optionally `If-Match`) answering `want`; returns the body.
async fn post_ok(
    app: &Router,
    who: &Login,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    want: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, "POST", path, body, if_match)).await;
    assert_eq!(s, want, "POST {path}: {b}");
    b
}

/// Part 2 · S7 gap (commercial review): admission financial clearance and the examination
/// hall-ticket entry condition read Finance facts through ports the HTTP gate never sees. A
/// `core_academic` tenant (no `fees`) must complete admission → confirmation → student, and get
/// its hall ticket, even when Finance holds outstanding charges (e.g. left from before a
/// downgrade) that it can no longer see or settle: no Finance obligation applies without `fees`.
/// Control: the same charges block both while `fees` is active (complete_erp default).
#[tokio::test]
async fn core_academic_without_fees_admits_and_issues_hall_tickets() {
    let tenant = fresh_tenant();
    let (app, owner) = ent_app(tenant, None).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let setup = b6::exam_setup(
        &app,
        &admin,
        &owner,
        tenant,
        "Core Academic College",
        Some(b4::AARAV),
        "active",
        "scheduled",
    )
    .await;
    let base = &setup.finance.delivery.base;
    let inst = base.institution_id;

    // An applicant through to approval (auto-approval in this harness).
    let st = "/api/v1/student";
    let app_body = json!({
        "programmeId": base.programme_id, "givenName": "Meenal", "familyName": "Kulkarni",
        "dateOfBirth": "2007-03-14", "email": "meenal.kulkarni@example.in",
        "phone": "+91 98765 43210", "admissionTermId": base.term_ids[0],
    });
    let mut a = post_ok(
        &app,
        &admin,
        &format!("{st}/institutions/{inst}/admission-applications"),
        app_body,
        None,
        StatusCode::CREATED,
    )
    .await;
    for step in ["submit", "review", "offer"] {
        let path = format!("{st}/admission-applications/{}/{step}", id_of(&a));
        let (s, b) = cmd(&app, &admin, &path, Some(ver(&a))).await;
        assert_eq!(s, StatusCode::OK, "{step}: {b}");
        a = b;
    }
    assert_eq!(a["status"], "offered", "{a}");
    let aid = a["admissionId"].as_str().unwrap().to_owned();
    let (_, _, adm) = send(&app, get(&admin, &format!("{st}/admissions/{aid}"))).await;
    let adm = post_ok(
        &app,
        &admin,
        &format!("{st}/admissions/{aid}/accept"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    let mut docs = vec![];
    for t in adm["requiredDocuments"].as_array().unwrap().clone() {
        let d = post_ok(
            &app,
            &admin,
            &format!("{st}/admissions/{aid}/documents"),
            json!({"documentType": t, "title": format!("{t} scan"), "fileRef": "dms://x"}),
            None,
            StatusCode::CREATED,
        )
        .await;
        docs.push(d);
    }
    let adm = post_ok(
        &app,
        &admin,
        &format!("{st}/admissions/{aid}/submit-documents"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "documents_submitted", "{adm}");
    for d in &docs {
        post_ok(
            &app,
            &admin,
            &format!("{st}/admissions/{aid}/documents/{}/verify", id_of(d)),
            json!({"outcome": "verified"}),
            Some(ver(d)),
            StatusCode::OK,
        )
        .await;
    }
    let (_, _, adm) = send(&app, get(&admin, &format!("{st}/admissions/{aid}"))).await;
    let adm = post_ok(
        &app,
        &admin,
        &format!("{st}/admissions/{aid}/submit-for-approval"),
        json!({}),
        Some(ver(&adm)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(adm["status"], "approved", "{adm}");

    // Finance holds an unpaid admission charge and an unpaid student charge.
    let adm_charge = b5::seed_charge(&owner, &setup.finance, "5000", "published", None).await;
    sqlx::query("UPDATE finance.charge SET admission_id = $2 WHERE id = $1")
        .bind(adm_charge.charge_id.as_uuid())
        .bind(aid.parse::<uuid::Uuid>().unwrap())
        .execute(&owner)
        .await
        .unwrap();
    b5::seed_charge(&owner, &setup.finance, "1000", "published", None).await;
    let papers = [setup.paper_id];
    b6::seed_registration(
        &owner,
        &setup,
        setup.session_id,
        &papers,
        sutra_contracts::exams::status::ExamRegistrationStatus::Confirmed,
    )
    .await;
    let clearance = format!("{st}/admissions/{aid}/financial-clearance");
    let sid = setup.session_id.as_uuid();
    let tickets = format!("/api/v1/exams/exam-sessions/{sid}/hall-tickets");

    // Control — fees active (complete_erp default): the dues block clearance and the ticket.
    let (s, b) = cmd(&app, &admin, &clearance, Some(ver(&adm))).await;
    assert_eq!(s, StatusCode::CONFLICT, "outstanding admission dues: {b}");
    assert!(b["detail"].as_str().unwrap().contains("outstanding"), "{b}");
    let (s, b) = cmd(&app, &admin, &format!("{tickets}/issue"), None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (_, _, list) = send(&app, get(&admin, &tickets)).await;
    let ticket = list["items"][0].clone();
    assert_eq!(ticket["status"], "withheld", "{list}");
    assert_eq!(
        ticket["conditions"]["dues"]["kind"], "outstanding",
        "{ticket}"
    );

    // core_academic: fees is not active.
    change(
        &app,
        &admin,
        &dean,
        json!({"kind": "activate", "package": "core_academic", "reason": "offline fees"}),
    )
    .await;
    refused(
        "fees API",
        &call(
            &app,
            &admin,
            "GET",
            &format!("/api/v1/finance/institutions/{inst}/student-accounts"),
            Value::Null,
        )
        .await,
        "entitlement.module_inactive",
    );

    // Admission → financial clearance → confirmation → student.
    let (s, adm) = cmd(&app, &admin, &clearance, Some(ver(&adm))).await;
    assert_eq!(s, StatusCode::OK, "{adm}");
    assert_eq!(adm["status"], "financially_cleared", "{adm}");
    assert_eq!(adm["financeEvidence"]["kind"], "notApplicable", "{adm}");
    let confirm = format!("{st}/admissions/{aid}/confirm");
    let (s, adm) = cmd(&app, &admin, &confirm, Some(ver(&adm))).await;
    assert_eq!(s, StatusCode::OK, "{adm}");
    assert_eq!(adm["status"], "confirmed", "{adm}");
    let create = format!("{st}/admissions/{aid}/create-student");
    let (s, adm) = cmd(&app, &admin, &create, Some(ver(&adm))).await;
    assert_eq!(s, StatusCode::OK, "{adm}");
    assert_eq!(adm["status"], "student_created", "{adm}");
    assert!(adm["studentId"].is_string(), "{adm}");

    // Examination eligibility: the withheld ticket is released (no Finance obligation applies).
    let (s, t) = cmd(
        &app,
        &admin,
        &format!("/api/v1/exams/hall-tickets/{}/release", id_of(&ticket)),
        Some(ver(&ticket)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["status"], "active", "{t}");
    assert_eq!(t["conditions"]["dues"]["kind"], "notApplicable", "{t}");
}

// ------------------------------------------------------------------ data import per module

const IMP: &str = "/api/v1/platform/imports";

fn b64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18u32, 12, 6, 0].into_iter().enumerate() {
            if i <= chunk.len() {
                out.push(A[(n >> shift) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

async fn upload(app: &Router, who: &Login, entity: &str, inst: &str) -> (StatusCode, Value) {
    let body = json!({
        "entity": entity,
        "institutionId": inst,
        "fileName": format!("{entity}.csv"),
        "contentBase64": b64(b"code,name\nX1,Legacy row\n"),
    });
    let (s, _, b) = call(app, who, "POST", IMP, body).await;
    (s, b)
}

async fn entities(app: &Router, who: &Login) -> Vec<String> {
    let (s, _, v) = call(app, who, "GET", &format!("{IMP}/entities"), Value::Null).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["entity"].as_str().unwrap().to_owned())
        .collect()
}

/// Part 2 · S7 gap (commercial review): the import endpoints live under the always-available
/// platform area, so the path gate alone would let a `core_academic` tenant load staff, fee
/// heads or opening balances. Each entity is gated on the module it writes into (registry
/// `imports`): upload, mapping, dry run, commit and the commit decision answer
/// `403 entitlement.module_inactive`; the entity list hides those entities; student and
/// institution set-up imports stay available; export is never gated; an add-on reopens at once.
#[tokio::test]
async fn import_jobs_are_gated_by_their_target_module() {
    let tenant = fresh_tenant();
    let (app, _owner) = ent_app(tenant, None).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let dean = login(&app, "dean@sutra.dev").await;
    let inst = register_institution(&app, &admin, "Import Entitlement College").await;

    // complete_erp default: every entity is offered and a staff job can be created.
    let all = entities(&app, &admin).await;
    for e in [
        "students",
        "staff",
        "fee_heads",
        "opening_balances",
        "campuses",
    ] {
        assert!(all.iter().any(|x| x == e), "{e} missing: {all:?}");
    }
    let (s, staff_job) = upload(&app, &admin, "staff", &inst).await;
    assert_eq!(s, StatusCode::CREATED, "{staff_job}");
    let staff_job = id_of(&staff_job);

    change(
        &app,
        &admin,
        &dean,
        json!({"kind": "activate", "package": "core_academic", "reason": "pilot contract"}),
    )
    .await;

    let offered = entities(&app, &admin).await;
    for e in ["staff", "fee_heads", "opening_balances"] {
        assert!(!offered.iter().any(|x| x == e), "{e} offered: {offered:?}");
    }
    for e in ["students", "historical_results", "campuses", "programmes"] {
        assert!(offered.iter().any(|x| x == e), "{e} hidden: {offered:?}");
    }
    for e in ["staff", "fee_heads", "opening_balances"] {
        let (s, b) = upload(&app, &admin, e, &inst).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{e}: {b}");
        assert_eq!(code(&b), "entitlement.module_inactive", "{e}: {b}");
    }
    for e in ["students", "campuses"] {
        let (s, b) = upload(&app, &admin, e, &inst).await;
        assert_eq!(s, StatusCode::CREATED, "{e}: {b}");
    }
    // A staff job created before the downgrade can no longer be mapped, run or committed.
    for (method, path, body) in [
        ("POST", format!("{IMP}/{staff_job}/dry-run"), json!({})),
        ("POST", format!("{IMP}/{staff_job}/commit"), json!({})),
        (
            "PUT",
            format!("{IMP}/{staff_job}/mapping"),
            json!({"mapping": {"code": "employee_code"}}),
        ),
        (
            "POST",
            format!("{IMP}/{staff_job}/decision"),
            json!({"approve": true}),
        ),
    ] {
        let r = call(&app, &dean, method, &path, body).await;
        let what = format!("{method} {path}");
        refused(&what, &r, "entitlement.module_inactive");
    }
    let r = call(
        &app,
        &admin,
        "POST",
        &format!("{IMP}/mapping-profiles"),
        json!({"entity": "fee_heads", "name": "legacy", "mapping": {"code": "code"}}),
    )
    .await;
    refused("fee-head profile", &r, "entitlement.module_inactive");
    // The job itself stays readable and every entity stays exportable (no data hostage).
    let job_path = format!("{IMP}/{staff_job}");
    not_gated(
        "staff job read",
        &call(&app, &admin, "GET", &job_path, Value::Null).await,
    );
    let export = format!("{IMP}/entities/staff/export?institutionId={inst}");
    let r = call(&app, &admin, "GET", &export, Value::Null).await;
    assert_eq!(r.0, StatusCode::OK, "staff export: {}", r.2);

    // The HR add-on reopens staff imports at once (fees stays closed).
    change(
        &app,
        &admin,
        &dean,
        json!({"kind": "change", "package": "core_academic", "addOnModules": ["hr"],
               "reason": "hr add-on"}),
    )
    .await;
    // Same file, same institution: the upload answers the existing job (200), not a refusal.
    let (s, b) = upload(&app, &admin, "staff", &inst).await;
    assert!(s.is_success(), "{s} {b}");
    assert_eq!(id_of(&b), staff_job);
    not_gated(
        "staff dry run after add-on",
        &call(
            &app,
            &admin,
            "POST",
            &format!("{IMP}/{staff_job}/dry-run"),
            json!({}),
        )
        .await,
    );
    let (s, b) = upload(&app, &admin, "fee_heads", &inst).await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, "entitlement.module_inactive"),
        "{b}"
    );
}
