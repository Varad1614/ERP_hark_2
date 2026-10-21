//! Slice 2A DB tests (owned by 2A). Run against this slice's own database:
//! `DATABASE_URL=postgres://sutra:sutra@localhost:5432/sutra_2a cargo test -p sutra-api --features db-tests --test db_b2a`.
//!
//! Coverage (L3/L4 B-001 … B-016): institution lifecycle incl. the readiness-gated activation
//! request and the Activation Authority decision (auto-approval and awaited decision), suspend /
//! reactivate / archive with step-up, profile revision history, business timezone, versioned
//! policies, type blueprints and type migration, the organisation unit tree (placement rules,
//! cycles, restructure move / merge, stale baseline), governance effective dating, effective-dated
//! position definitions; plus tenant isolation, permission denials, ETag / If-Match, idempotency,
//! and the audit + outbox rows of every command.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::{Days, NaiveDate};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use support::*;
use sutra_institution::shared::approval::ApprovalMode;
use sutra_institution::shared::{Actor, InstitutionState};
use sutra_kernel::cache::AdvisoryCache;
use sutra_kernel::concurrency::Version;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};

const API: &str = "/api/v1/institution";

// ------------------------------------------------------------------------------- helpers

fn tenant() -> &'static str {
    Box::leak(uuid::Uuid::now_v7().to_string().into_boxed_str())
}

async fn post(
    app: &Router,
    who: &Login,
    path: &str,
    body: Value,
    v: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, "POST", path, body, v)).await;
    (s, b)
}

async fn put(app: &Router, who: &Login, path: &str, body: Value, v: i64) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, "PUT", path, body, Some(v))).await;
    (s, b)
}

async fn patch(app: &Router, who: &Login, path: &str, body: Value, v: i64) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, "PATCH", path, body, Some(v))).await;
    (s, b)
}

async fn read(app: &Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

fn ver(b: &Value) -> i64 {
    b["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("no version in {b}"))
}

fn id(b: &Value) -> String {
    b["id"]
        .as_str()
        .unwrap_or_else(|| panic!("no id in {b}"))
        .to_owned()
}

fn inst_path(inst: &str, rest: &str) -> String {
    format!("{API}/institutions/{inst}{rest}")
}

fn profile() -> Value {
    json!({
        "shortName": "SIT", "institutionType": "college", "registrationNumber": "REG-001",
        "address": "1 Campus Road, Pune", "contactPhone": "+91 20 1234 5678",
        "officialEmail": "office@sit.edu", "academicPattern": "semester",
        "workingDays": ["mon", "tue", "wed", "thu", "fri"], "defaultLanguage": "en-IN",
        "administratorEmail": "admin@sit.edu"
    })
}

fn std_types() -> Value {
    json!([
        {"key": "university", "label": "University", "allowRoot": true},
        {"key": "faculty", "label": "Faculty", "allowedParentTypes": ["university"]},
        {"key": "department", "label": "Department", "allowedParentTypes": ["faculty", "department"]},
    ])
}

/// Registers an institution; returns (id, version).
async fn new_institution(app: &Router, admin: &Login) -> (String, i64) {
    let inst = register_institution(app, admin, "Sutra Institute of Technology").await;
    let (s, b) = read(app, admin, &inst_path(&inst, "")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(b["status"], "draft");
    (inst, ver(&b))
}

async fn business_today(pool: &sqlx::PgPool, inst: &str) -> NaiveDate {
    sqlx::query_scalar("SELECT (now() AT TIME ZONE timezone)::date FROM institution.institution WHERE id = $1::uuid")
        .bind(inst)
        .fetch_one(pool)
        .await
        .unwrap()
}

fn plus(d: NaiveDate, days: u64) -> String {
    d.checked_add_days(Days::new(days)).unwrap().to_string()
}

async fn publish_blueprint(
    app: &Router,
    admin: &Login,
    inst: &str,
    types: Value,
    from: &str,
) -> Value {
    let (s, bp) = post(
        app,
        admin,
        &inst_path(inst, "/org-type-blueprints"),
        json!({"effectiveFrom": from, "unitTypes": types}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{bp}");
    let (s, bp) = post(
        app,
        admin,
        &format!("{API}/org-type-blueprints/{}/publish", id(&bp)),
        json!({}),
        Some(ver(&bp)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{bp}");
    assert_eq!(bp["status"], "published");
    bp
}

async fn unit(
    app: &Router,
    admin: &Login,
    inst: &str,
    parent: Option<&str>,
    ty: &str,
    code: &str,
) -> Value {
    let (s, u) = post(
        app,
        admin,
        &inst_path(inst, "/org-units"),
        json!({"parentId": parent, "unitType": ty, "code": code, "name": format!("Unit {code}")}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{u}");
    assert_eq!(u["status"], "draft");
    u
}

async fn unit_cmd(
    app: &Router,
    admin: &Login,
    u: &Value,
    verb: &str,
    reason: Option<&str>,
) -> (StatusCode, Value) {
    post(
        app,
        admin,
        &format!("{API}/org-units/{}/{verb}", id(u)),
        json!({"reason": reason}),
        Some(ver(u)),
    )
    .await
}

async fn activate_unit(app: &Router, admin: &Login, u: &Value) -> Value {
    let (s, b) = unit_cmd(app, admin, u, "activate", None).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "active");
    b
}

/// Satisfies every required 2A readiness item: complete profile, published blueprint, active
/// root unit. Returns (institution version, root unit, blueprint).
async fn ready_2a(
    app: &Router,
    admin: &Login,
    pool: &sqlx::PgPool,
    inst: &str,
    v: i64,
) -> (i64, Value, Value) {
    let (s, b) = put(app, admin, &inst_path(inst, "/profile"), profile(), v).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let today = business_today(pool, inst).await;
    let bp = publish_blueprint(app, admin, inst, std_types(), &today.to_string()).await;
    let root = unit(app, admin, inst, None, "university", "ROOT").await;
    let root = activate_unit(app, admin, &root).await;
    other_slices_ready(app, admin, inst).await;
    (ver(&b), root, bp)
}

/// Integration hook: the required 2B/2C readiness items (active campus, published structure,
/// programme and curriculum version, published calendar with an open year and an effective
/// institution-level assignment) are satisfied through their real APIs so activation succeeds.
async fn other_slices_ready(app: &Router, admin: &Login, inst: &str) {
    support::b2::satisfy_2b_2c(app, admin, inst).await;
}

/// Moves the institution to `status` directly (setup shortcut for tests that need it).
async fn force_status(pool: &sqlx::PgPool, inst: &str, status: &str) {
    sqlx::query(
        "UPDATE institution.institution SET status = $1, version = version + 1 WHERE id = $2::uuid",
    )
    .bind(status)
    .bind(inst)
    .execute(pool)
    .await
    .unwrap();
}

async fn institution(app: &Router, who: &Login, inst: &str) -> Value {
    let (s, b) = read(app, who, &inst_path(inst, "")).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    b
}

/// Event types (without the `sutraerp.institution.` prefix) published for `subject`, in order.
async fn outbox(pool: &sqlx::PgPool, subject: &str) -> Vec<String> {
    let rows: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM platform.outbox WHERE subject = $1 ORDER BY created_at, id",
    )
    .bind(subject)
    .fetch_all(pool)
    .await
    .unwrap();
    rows.into_iter()
        .map(|t| t.trim_start_matches("sutraerp.institution.").to_owned())
        .collect()
}

async fn outbox_data(pool: &sqlx::PgPool, subject: &str, suffix: &str) -> Value {
    sqlx::query_scalar(
        "SELECT envelope -> 'data' FROM platform.outbox WHERE subject = $1 AND event_type = $2 ORDER BY created_at DESC, id DESC LIMIT 1",
    )
    .bind(subject)
    .bind(format!("sutraerp.institution.{suffix}"))
    .fetch_one(pool)
    .await
    .unwrap()
}

/// (action, actor_kind) audit rows of a target.
async fn audits(pool: &sqlx::PgPool, target: &str) -> Vec<(String, String)> {
    sqlx::query_as("SELECT action, actor_kind FROM platform.audit_log WHERE target_id = $1 ORDER BY occurred_at, id")
        .bind(target)
        .fetch_all(pool)
        .await
        .unwrap()
}

fn actions(a: &[(String, String)]) -> Vec<&str> {
    a.iter().map(|(x, _)| x.as_str()).collect()
}

fn await_state(pool: &sqlx::PgPool) -> InstitutionState {
    let mut st = InstitutionState::new(
        pool.clone(),
        AdvisoryCache::disabled(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    );
    st.approvals = ApprovalMode::AwaitDecision;
    st
}

fn scope(t: &str, inst: &str) -> ExecutionScope {
    ExecutionScope::institution(
        t.parse::<TenantId>().unwrap(),
        inst.parse::<InstitutionId>().unwrap(),
    )
}

fn admin_actor() -> Actor {
    Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    }
}

fn detail(b: &Value) -> String {
    b["detail"].as_str().unwrap_or_default().to_owned()
}

fn violation_fields(b: &Value) -> Vec<String> {
    b["validationErrors"]
        .as_array()
        .map(|v| {
            v.iter()
                .filter_map(|x| x["field"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

// --------------------------------------------------------------------------------- tests

/// L3-B-001 / -002 / -003 in the auto-approval composition: Draft → Configured → (readiness
/// gate) → submitted (pending) → Activation Authority approves → Active → Suspended → Reactivated
/// → Archived, every illegal
/// transition refused, audit and outbox rows in the owner-local transaction.
#[tokio::test]
async fn institution_lifecycle_with_readiness_gate_and_auto_approval() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, v) = new_institution(&app, &admin).await;

    // Draft cannot be submitted or decided; configure needs the mandatory master information.
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/submit"),
        json!({}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/configure"),
        json!({}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let fields = violation_fields(&b);
    for f in [
        "shortName",
        "institutionType",
        "officialEmail",
        "workingDays",
        "administratorEmail",
    ] {
        assert!(fields.contains(&f.to_owned()), "{f} in {fields:?}");
    }
    // If-Match is required (428) and must be current (412).
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/configure"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);
    let (s, _) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        profile(),
        v + 7,
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);

    let (s, b) = put(&app, &admin, &inst_path(&inst, "/profile"), profile(), v).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["profile"]["revision"], 1);
    assert_eq!(b["profile"]["institutionType"], "college");
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/configure"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "configured");

    // Readiness gate: no active root unit yet → 409 naming the blocking key.
    let (s, err) = post(
        &app,
        &admin,
        &inst_path(&inst, "/submit"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{err}");
    assert!(
        detail(&err).contains("organisation.root_unit_active"),
        "{err}"
    );
    assert_eq!(
        institution(&app, &admin, &inst).await["status"],
        "configured"
    );

    let today = business_today(&pool, &inst).await;
    publish_blueprint(&app, &admin, &inst, std_types(), &today.to_string()).await;
    let root = unit(&app, &admin, &inst, None, "university", "ROOT").await;
    activate_unit(&app, &admin, &root).await;
    other_slices_ready(&app, &admin, &inst).await;

    // Submit → pending_approval. DEF-B-002: submitting (institution.manage) never activates,
    // even in the auto-approval mode; the Activation Authority decides.
    let cur = institution(&app, &admin, &inst).await;
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/submit"),
        json!({}),
        Some(ver(&cur)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "pending_approval");
    let (s, reqs) = read(&app, &admin, &inst_path(&inst, "/activation-requests")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(reqs["items"][0]["status"], "pending", "{reqs}");
    // Activation Authority decision (institution.activation.decide) → active.
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve"}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "active");
    let (s, reqs) = read(&app, &admin, &inst_path(&inst, "/activation-requests")).await;
    assert_eq!(s, StatusCode::OK);
    let reqs = reqs["items"].as_array().unwrap().clone();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0]["status"], "approved");
    assert_eq!(reqs[0]["decidedBy"], "dev|admin@sutra.dev");
    assert!(reqs[0]["processInstanceId"]
        .as_str()
        .unwrap()
        .starts_with("noop-"));
    assert_eq!(reqs[0]["readiness"]["ready"], true);
    let activated = outbox_data(&pool, &inst, "institution.activated").await;
    assert_eq!(activated["fromStatus"], "pending_approval");
    assert_eq!(activated["toStatus"], "active");
    assert_eq!(activated["processInstanceId"], reqs[0]["processInstanceId"]);

    // Active cannot be re-submitted or decided again; reactivate only from suspended.
    let v = ver(&b);
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/submit"),
        json!({}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/reactivate"),
        json!({"reason": "x"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);

    // Suspension needs a reason; while suspended everything is read-only.
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/suspend"),
        json!({}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/suspend"),
        json!({"reason": "Regulatory audit"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "suspended");
    let v = ver(&b);
    let (s, _) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        json!({"address": "New"}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = patch(
        &app,
        &admin,
        &inst_path(&inst, ""),
        json!({"name": "Renamed"}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, b2) = post(
        &app,
        &admin,
        &inst_path(&inst, "/org-units"),
        json!({"parentId": id(&root), "unitType": "faculty", "code": "ENG", "name": "Engineering"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b2}");
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/policies"),
        json!({"key": "a.b", "title": "T", "body": "B", "effectiveFrom": today.to_string()}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Reads stay available.
    let (s, _) = read(&app, &admin, &inst_path(&inst, "/org-units")).await;
    assert_eq!(s, StatusCode::OK);

    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/reactivate"),
        json!({"reason": "Audit closed"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "active");
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/archive"),
        json!({"reason": "Merged into another trust"}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "archived");
    let v = ver(&b);
    for verb in ["suspend", "reactivate", "archive", "configure", "submit"] {
        // Audit A3 (D-2): `configure` / `submit` take no request body, so they get `{}`.
        let body = if matches!(verb, "configure" | "submit") {
            json!({})
        } else {
            json!({"reason": "r"})
        };
        let (s, _) = post(
            &app,
            &admin,
            &inst_path(&inst, &format!("/{verb}")),
            body,
            Some(v),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{verb} on archived");
    }
    let (s, _) = put(
        &app,
        &admin,
        &inst_path(&inst, "/timezone"),
        json!({"timezone": "Asia/Dubai"}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Identity and history survive archival.
    let b = institution(&app, &admin, &inst).await;
    assert_eq!(b["profile"]["shortName"], "SIT");

    assert_eq!(
        outbox(&pool, &inst).await,
        vec![
            "institution.registered",
            "institution.profile_revised",
            "institution.configured",
            "institution.submitted_for_approval",
            "institution.activated",
            "institution.suspended",
            "institution.reactivated",
            "institution.archived",
        ]
    );
    let a = audits(&pool, &inst).await;
    let acts = actions(&a);
    for want in [
        "institution.institution.register",
        "institution.institution.revise_profile",
        "institution.institution.configure",
        "institution.institution.submit",
        "institution.institution.activate",
        "institution.institution.suspend",
        "institution.institution.reactivate",
        "institution.institution.archive",
    ] {
        assert!(acts.contains(&want), "{want} in {acts:?}");
    }
    let activate = a
        .iter()
        .find(|(x, _)| x == "institution.institution.activate")
        .unwrap();
    assert_eq!(
        activate.1, "user",
        "activation is the Activation Authority's decision, never a system auto-approval"
    );
    let suspend = a
        .iter()
        .find(|(x, _)| x == "institution.institution.suspend")
        .unwrap();
    assert_eq!(suspend.1, "user");
}

/// L3-B-001 with an awaited Activation Authority decision (WorkflowPort `AwaitDecision`):
/// return → correct → resubmit, reject → new cycle, a stale approval is refused while readiness
/// no longer holds, then approve. Decisions need a reason (return / reject).
#[tokio::test]
async fn activation_authority_decisions_return_reject_and_stale_approval() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let (inst, v) = new_institution(&app, &admin).await;
    let (v, root, _) = ready_2a(&app, &admin, &pool, &inst, v).await;
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/configure"),
        json!({}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let st = await_state(&pool);
    let sc = scope(t, &inst);
    let iid: InstitutionId = inst.parse().unwrap();

    let pending =
        sutra_institution::lifecycle::repo::submit(&st, &sc, iid, Version(ver(&b)), &admin_actor())
            .await
            .unwrap();
    assert_eq!(pending.status.as_str(), "pending_approval");
    let v = pending.version.0;
    // Configuration is locked during review.
    let (s, _) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        json!({"address": "Elsewhere"}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Only the authority decides; return / reject need a reason; If-Match is enforced.
    let (s, _) = post(
        &app,
        &student,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "return"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "return", "reason": "x"}),
        Some(v - 1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "maybe"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "return", "reason": "Fix the registration number"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "returned");

    // Returned: editable, re-configure, resubmit.
    let (s, b) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        json!({"registrationNumber": "REG-002"}),
        ver(&b),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/submit"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "returned must be re-configured first"
    );
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/configure"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let pending =
        sutra_institution::lifecycle::repo::submit(&st, &sc, iid, Version(ver(&b)), &admin_actor())
            .await
            .unwrap();
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "reject", "reason": "Board declined"}),
        Some(pending.version.0),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "rejected");
    // Rejected never goes straight to active.
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve"}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/submit"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/configure"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let pending =
        sutra_institution::lifecycle::repo::submit(&st, &sc, iid, Version(ver(&b)), &admin_actor())
            .await
            .unwrap();

    // Readiness no longer holds (root unit suspended) → approval refused, stays pending.
    let (s, r) = unit_cmd(&app, &admin, &root, "suspend", Some("Restructuring")).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (s, err) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve"}),
        Some(pending.version.0),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{err}");
    assert!(detail(&err).contains("organisation.root_unit_active"));
    assert_eq!(
        institution(&app, &admin, &inst).await["status"],
        "pending_approval"
    );
    let (s, r) = unit_cmd(&app, &admin, &r, "restore", None).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    let (s, b) = post(
        &app,
        &admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve", "reason": "Validated"}),
        Some(pending.version.0),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "active");

    let (_, reqs) = read(&app, &admin, &inst_path(&inst, "/activation-requests")).await;
    let statuses: Vec<_> = reqs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, vec!["approved", "rejected", "returned"]);
    assert_eq!(reqs["items"][0]["decidedBy"], "dev|admin@sutra.dev");
    assert_eq!(
        reqs["items"][2]["decisionReason"],
        "Fix the registration number"
    );
    let types = outbox(&pool, &inst).await;
    for want in [
        "institution.returned_for_correction",
        "institution.rejected",
        "institution.activated",
    ] {
        assert!(types.contains(&want.to_owned()), "{want}");
    }
    assert_eq!(
        types
            .iter()
            .filter(|t| *t == "institution.submitted_for_approval")
            .count(),
        3
    );
    let returned = outbox_data(&pool, &inst, "institution.returned_for_correction").await;
    assert_eq!(returned["reason"], "Fix the registration number");
}

/// Step-up (MFA) applies to `institution.lifecycle` and `institution.activation.decide` only;
/// Faculty and Student hold no 2A write permission but can read.
#[tokio::test]
async fn step_up_and_permission_denials() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, v) = new_institution(&app, &admin).await;
    let (v, root, bp) = ready_2a(&app, &admin, &pool, &inst, v).await;
    force_status(&pool, &inst, "active").await;
    let v = v + 1;

    // Step-up enforced: password-only admin session.
    let (mfa_app, _) = app_with_env(t, &[("SUTRA_DEV_MFA", "true")]).await;
    let mfa_admin = login(&mfa_app, "admin@sutra.dev").await;
    for verb in ["suspend", "archive"] {
        let (s, b) = post(
            &mfa_app,
            &mfa_admin,
            &inst_path(&inst, &format!("/{verb}")),
            json!({"reason": "r"}),
            Some(v),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{verb}: {b}");
        assert_eq!(b["errorCode"], "mfa-step-up-required");
    }
    let (s, b) = post(
        &mfa_app,
        &mfa_admin,
        &inst_path(&inst, "/decision"),
        json!({"decision": "approve"}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(b["errorCode"], "mfa-step-up-required");
    // institution.manage / organisation.manage need no step-up (409 = reached the domain rule).
    let (s, b) = post(
        &mfa_app,
        &mfa_admin,
        &inst_path(&inst, "/configure"),
        json!({}),
        Some(v),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let (s, b) = patch(
        &mfa_app,
        &mfa_admin,
        &format!("{API}/org-units/{}", id(&root)),
        json!({"name": "Sutra University"}),
        ver(&root),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");

    for who in ["meera.shah@sutra.dev", "aarav.desai@sutra.dev"] {
        let user = login(&app, who).await;
        let today = business_today(&pool, &inst).await.to_string();
        let denied: Vec<(String, Value, Option<i64>)> = vec![
            (inst_path(&inst, "/configure"), json!({}), Some(v)),
            (
                inst_path(&inst, "/suspend"),
                json!({"reason": "r"}),
                Some(v),
            ),
            (
                inst_path(&inst, "/decision"),
                json!({"decision": "approve"}),
                Some(v),
            ),
            (
                inst_path(&inst, "/policies"),
                json!({"key": "a.b", "title": "T", "body": "B", "effectiveFrom": today}),
                None,
            ),
            (
                inst_path(&inst, "/org-units"),
                json!({"unitType": "faculty", "code": "X1", "name": "X", "parentId": id(&root)}),
                None,
            ),
            (
                inst_path(&inst, "/org-type-blueprints"),
                json!({"effectiveFrom": today, "unitTypes": std_types()}),
                None,
            ),
            (
                inst_path(&inst, "/positions"),
                json!({"orgUnitId": id(&root), "code": "P1", "title": "T", "effectiveFrom": today}),
                None,
            ),
            (
                format!("{API}/org-units/{}/suspend", id(&root)),
                json!({"reason": "r"}),
                Some(1),
            ),
            (
                format!("{API}/org-type-blueprints/{}/publish", id(&bp)),
                json!({}),
                Some(1),
            ),
        ];
        for (path, body, ifm) in denied {
            let (s, _) = post(&app, &user, &path, body, ifm).await;
            assert_eq!(s, StatusCode::FORBIDDEN, "{who} POST {path}");
        }
        let (s, _) = put(
            &app,
            &user,
            &inst_path(&inst, "/profile"),
            json!({"address": "x"}),
            v,
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        let (s, _) = put(
            &app,
            &user,
            &inst_path(&inst, "/timezone"),
            json!({"timezone": "Asia/Dubai"}),
            v,
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN);
        for path in [
            inst_path(&inst, ""),
            inst_path(&inst, "/org-units"),
            inst_path(&inst, "/policies"),
            format!("{API}/org-units/{}", id(&root)),
        ] {
            let (s, _) = read(&app, &user, &path).await;
            assert_eq!(s, StatusCode::OK, "{who} GET {path}");
        }
    }
    // Nothing was audited for the denied attempts beyond the admin's own work.
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM platform.audit_log WHERE tenant_id = $1::uuid \
         AND action LIKE 'institution.%' AND actor_id NOT IN ('dev|admin@sutra.dev', 'system:auto-approval')")
        .bind(t)
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(n, 0);
}

/// Every 2A resource of one tenant is invisible (404) to another tenant's administrator.
#[tokio::test]
async fn tenant_isolation() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, v) = new_institution(&app, &admin).await;
    let (_, root, bp) = ready_2a(&app, &admin, &pool, &inst, v).await;
    let today = business_today(&pool, &inst).await.to_string();
    let (s, pol) = post(
        &app,
        &admin,
        &inst_path(&inst, "/policies"),
        json!({"key": "fees.late", "title": "Late fees", "body": "2%", "effectiveFrom": today}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{pol}");
    let (s, pos) = post(&app, &admin, &inst_path(&inst, "/positions"), json!({"orgUnitId": id(&root), "code": "VC", "title": "Vice Chancellor", "effectiveFrom": today}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{pos}");
    let (s, gov) = post(&app, &admin, &format!("{API}/org-units/{}/governance-configs", id(&root)), json!({"effectiveFrom": today, "settings": {"headTitle": "Vice Chancellor", "approvalLevels": 2}}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{gov}");

    let other_t = tenant();
    let (other, _) = support::app(other_t).await;
    let intruder = login(&other, "admin@sutra.dev").await;
    for path in [
        inst_path(&inst, ""),
        inst_path(&inst, "/org-units"),
        inst_path(&inst, "/policies"),
        inst_path(&inst, "/positions"),
        inst_path(&inst, "/activation-requests"),
        inst_path(&inst, "/org-type-blueprints"),
        inst_path(&inst, "/org-restructures"),
        format!("{API}/org-units/{}", id(&root)),
        format!("{API}/org-units/{}/history", id(&root)),
        format!("{API}/org-type-blueprints/{}", id(&bp)),
        format!("{API}/policies/{}", id(&pol)),
        format!("{API}/positions/{}", id(&pos)),
        format!("{API}/governance-configs/{}", id(&gov)),
    ] {
        let (s, _) = read(&other, &intruder, &path).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "GET {path}");
    }
    for (path, body, ifm) in [
        (inst_path(&inst, "/configure"), json!({}), Some(1)),
        (
            inst_path(&inst, "/org-units"),
            json!({"unitType": "faculty", "code": "XX", "name": "X", "parentId": id(&root)}),
            None,
        ),
        (
            inst_path(&inst, "/policies"),
            json!({"key": "x.y", "title": "T", "body": "B", "effectiveFrom": today}),
            None,
        ),
        (
            format!("{API}/org-units/{}/suspend", id(&root)),
            json!({"reason": "r"}),
            Some(ver(&root)),
        ),
        (
            format!("{API}/positions/{}/activate", id(&pos)),
            json!({}),
            Some(ver(&pos)),
        ),
        (
            format!("{API}/policies/{}/publish", id(&pol)),
            json!({}),
            Some(ver(&pol)),
        ),
    ] {
        let (s, _) = post(&other, &intruder, &path, body, ifm).await;
        assert_eq!(s, StatusCode::NOT_FOUND, "POST {path}");
    }
    let (s, list) = read(&other, &intruder, &format!("{API}/institutions")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(list["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["id"] != json!(inst)));
}

/// L3-B-004 profile revision history and the business timezone (validated IANA; drives every
/// business-local date). No-op revisions change nothing.
#[tokio::test]
async fn profile_revision_history_and_business_timezone() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    // Registration validates the zone against the IANA catalogue.
    let (s, b) = post(
        &app,
        &admin,
        INSTITUTIONS,
        json!({"code": code("TZ"), "name": "Mars College", "timezone": "Mars/Olympus_Mons"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (inst, v) = new_institution(&app, &admin).await;

    let (s, b) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        json!({"officialEmail": "nope", "workingDays": ["mon", "funday"]}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, b) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        json!({"bogusField": 1, "revision": 99}),
        v,
    )
    .await;
    // Unknown / read-only members are ignored (the web may send the profile back as read).
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(ver(&b), v, "nothing changed");
    let (s, b) = put(&app, &admin, &inst_path(&inst, "/profile"), profile(), v).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let v = ver(&b);
    // Same values again: nothing changes (no version bump, no event).
    let (s, b) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        json!({"shortName": "SIT"}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ver(&b), v);
    let (s, b) = put(
        &app,
        &admin,
        &inst_path(&inst, "/profile"),
        json!({"address": "2 New Road, Pune", "website": "https://sit.edu"}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["profile"]["revision"], 2);
    let v = ver(&b);

    for bad in ["IST", "Mars/Olympus", "posix/Asia/Kolkata", ""] {
        let (s, b) = put(
            &app,
            &admin,
            &inst_path(&inst, "/timezone"),
            json!({"timezone": bad}),
            v,
        )
        .await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {b}");
    }
    let (s, b) = put(
        &app,
        &admin,
        &inst_path(&inst, "/timezone"),
        json!({"timezone": "Pacific/Kiritimati"}),
        v,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["timezone"], "Pacific/Kiritimati");
    let changed = outbox_data(&pool, &inst, "institution.timezone_changed").await;
    assert_eq!(changed["previousTimezone"], "Asia/Kolkata");
    assert_eq!(changed["timezone"], "Pacific/Kiritimati");
    // Business-local "today" now follows the new zone (UTC+14).
    let kiritimati: NaiveDate =
        sqlx::query_scalar("SELECT (now() AT TIME ZONE 'Pacific/Kiritimati')::date")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(business_today(&pool, &inst).await, kiritimati);
    let (s, same) = put(
        &app,
        &admin,
        &inst_path(&inst, "/timezone"),
        json!({"timezone": "Pacific/Kiritimati"}),
        ver(&b),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(ver(&same), ver(&b));

    let (s, revs) = read(&app, &admin, &inst_path(&inst, "/profile/revisions")).await;
    assert_eq!(s, StatusCode::OK);
    let items = revs["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(items[0]["changedFields"], json!(["timezone"]));
    assert_eq!(items[1]["changedFields"], json!(["address", "website"]));
    assert_eq!(items[1]["previousValues"]["address"], "1 Campus Road, Pune");
    assert_eq!(items[1]["newValues"]["address"], "2 New Road, Pune");
    assert_eq!(
        outbox(&pool, &inst).await,
        vec![
            "institution.registered",
            "institution.profile_revised",
            "institution.profile_revised",
            "institution.timezone_changed"
        ]
    );
    let changed_fields = outbox_data(&pool, &inst, "institution.profile_revised").await;
    assert_eq!(
        changed_fields["changedFields"],
        json!(["address", "website"])
    );
    assert!(
        actions(&audits(&pool, &inst).await).contains(&"institution.institution.change_timezone")
    );
}

/// L3-B-005 versioned policies: drafts never govern, publication is forward-dated and never
/// overlaps, the predecessor is superseded but stays in force until the successor's date.
/// Also covers idempotent creation.
#[tokio::test]
async fn institutional_policies_are_versioned() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, _) = new_institution(&app, &admin).await;
    let today = business_today(&pool, &inst).await;
    let path = inst_path(&inst, "/policies");
    let body = json!({"key": "attendance.minimum_percentage", "title": "Minimum attendance", "body": "75%", "effectiveFrom": plus(today, 0)});

    // Idempotency-Key required; the same key replays the same response.
    let (s, _, _) = send(&app, {
        let mut r = write(&admin, "POST", &path, body.clone(), None);
        r.headers_mut().remove("idempotency-key");
        r
    })
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let keyed = |b: Value| {
        let mut r = write(&admin, "POST", &path, b, None);
        r.headers_mut()
            .insert("idempotency-key", "policy-create-1".parse().unwrap());
        r
    };
    let (s, _, p) = send(&app, keyed(body.clone())).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    let (s, h, again) = send(&app, keyed(body.clone())).await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        h.get("idempotency-replayed").map(|v| v.to_str().unwrap()),
        Some("true")
    );
    assert_eq!(again["id"], p["id"]);
    let (s, _, _) = send(
        &app,
        keyed(
            json!({"key": "other.key", "title": "x", "body": "y", "effectiveFrom": plus(today, 0)}),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "same key, different request"
    );
    let (s, _) = post(&app, &admin, &path, body.clone(), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate key");
    let (s, b) = post(
        &app,
        &admin,
        &path,
        json!({"key": "Bad Key", "title": "", "body": "x", "effectiveFrom": plus(today, 0)}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");

    assert_eq!(p["status"], "draft");
    assert_eq!(p["revision"], 0);
    assert!(p["currentRevision"].is_null(), "drafts never govern");
    assert_eq!(p["draftRevision"]["revision"], 1);
    let pid = id(&p);
    let ppath = format!("{API}/policies/{pid}");

    // Retroactive publication is refused; fix the draft date, then publish.
    let (s, b) = post(&app, &admin, &format!("{ppath}/revisions"), json!({"title": "Minimum attendance", "body": "75%", "effectiveFrom": today.checked_sub_days(Days::new(1)).unwrap().to_string()}), Some(ver(&p))).await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (s, err) = post(
        &app,
        &admin,
        &format!("{ppath}/publish"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    let (s, b) = post(
        &app,
        &admin,
        &format!("{ppath}/revisions"),
        json!({"title": "Minimum attendance", "body": "75%", "effectiveFrom": plus(today, 0)}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(
        b["draftRevision"]["revision"], 1,
        "the open draft is replaced, not duplicated"
    );
    let (s, b) = post(
        &app,
        &admin,
        &format!("{ppath}/publish"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["status"], "effective");
    assert_eq!(b["currentRevision"]["revision"], 1);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{ppath}/publish"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing to publish");

    // Successor from +30 days: predecessor superseded but still in force today.
    let (s, b) = post(
        &app,
        &admin,
        &format!("{ppath}/revisions"),
        json!({"title": "Minimum attendance", "body": "80%", "effectiveFrom": plus(today, 30)}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["currentRevision"]["body"], "75%");
    let (s, b) = post(
        &app,
        &admin,
        &format!("{ppath}/publish"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    assert_eq!(b["revision"], 2);
    assert_eq!(
        b["currentRevision"]["revision"], 1,
        "successor governs only from its date"
    );
    let (_, future) = read(&app, &admin, &format!("{ppath}?asOf={}", plus(today, 30))).await;
    assert_eq!(future["currentRevision"]["revision"], 2);
    assert_eq!(future["currentRevision"]["body"], "80%");
    // Same date as the latest revision → ambiguous authority, refused.
    let (s, b) = post(
        &app,
        &admin,
        &format!("{ppath}/revisions"),
        json!({"title": "x", "body": "85%", "effectiveFrom": plus(today, 30)}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{ppath}/publish"),
        json!({}),
        Some(ver(&b)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{ppath}/publish"),
        json!({}),
        Some(1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);

    let (_, revs) = read(&app, &admin, &format!("{ppath}/revisions")).await;
    let statuses: Vec<_> = revs["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["status"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(statuses, vec!["draft", "effective", "superseded"]);
    let (_, list) = read(&app, &admin, &format!("{path}?status=effective")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        outbox(&pool, &pid).await,
        vec!["institution_policy.revised", "institution_policy.revised"]
    );
    let revised = outbox_data(&pool, &pid, "institution_policy.revised").await;
    assert_eq!(revised["revision"], 2);
    assert_eq!(revised["policyKey"], "attendance.minimum_percentage");
    assert_eq!(revised["effectiveFrom"], plus(today, 30));
    let acts = audits(&pool, &pid).await;
    assert_eq!(
        actions(&acts)
            .iter()
            .filter(|a| **a == "institution.institution_policy.publish")
            .count(),
        2
    );
    // Policy revision is not a profile revision (L4-B-005-BH-001).
    assert_eq!(
        institution(&app, &admin, &inst).await["profile"]["revision"],
        0
    );
    // Readiness advisory key reflects the policy in force.
    let r = sutra_institution::shared::readiness::evaluate(
        &pool,
        &scope(t, &inst),
        inst.parse().unwrap(),
    )
    .await
    .unwrap();
    let item = r
        .items
        .iter()
        .find(|i| i.key == "institution.policies_effective")
        .unwrap();
    assert_eq!(serde_json::to_value(item.status).unwrap(), "met");
}

/// L3-B-006/-007/-008/-012/-016 and blueprints (L3-B-010): placement rules, one live root,
/// activation order, governed modification, suspend/restore, decommission preconditions,
/// archive, history and events.
#[tokio::test]
async fn organisation_units_tree_and_lifecycle() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, _) = new_institution(&app, &admin).await;
    let today = business_today(&pool, &inst).await;
    let units = inst_path(&inst, "/org-units");

    let (s, b) = post(
        &app,
        &admin,
        &units,
        json!({"unitType": "university", "code": "UN", "name": "U"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no blueprint yet: {b}");
    let (s, b) = post(&app, &admin, &inst_path(&inst, "/org-type-blueprints"), json!({"effectiveFrom": today.to_string(), "unitTypes": [{"key": "Faculty", "label": "", "allowedParentTypes": ["ghost"]}]}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{b}");
    let (s, draft) = post(&app, &admin, &inst_path(&inst, "/org-type-blueprints"), json!({"effectiveFrom": today.to_string(), "unitTypes": [{"key": "university", "label": "University", "allowRoot": true}]}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{draft}");
    let (s, _) = post(
        &app,
        &admin,
        &inst_path(&inst, "/org-type-blueprints"),
        json!({"effectiveFrom": today.to_string(), "unitTypes": std_types()}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open draft at a time");
    let (s, draft) = put(
        &app,
        &admin,
        &format!("{API}/org-type-blueprints/{}", id(&draft)),
        json!({"effectiveFrom": today.to_string(), "unitTypes": std_types()}),
        ver(&draft),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{draft}");
    assert_eq!(draft["unitTypes"].as_array().unwrap().len(), 3);
    let (s, bp) = post(
        &app,
        &admin,
        &format!("{API}/org-type-blueprints/{}/publish", id(&draft)),
        json!({}),
        Some(ver(&draft)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{bp}");
    assert_eq!(bp["status"], "published");
    let (s, _) = put(
        &app,
        &admin,
        &format!("{API}/org-type-blueprints/{}", id(&bp)),
        json!({"effectiveFrom": today.to_string(), "unitTypes": std_types()}),
        ver(&bp),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "published versions are immutable");
    assert_eq!(
        outbox(&pool, &id(&bp)).await,
        vec!["org_type_blueprint.published"]
    );

    let root = unit(&app, &admin, &inst, None, "university", "SU").await;
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"unitType": "university", "code": "SU2", "name": "Second root"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one live root");
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"unitType": "faculty", "code": "F0", "name": "Rootless faculty"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "faculty cannot be root");
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"parentId": id(&root), "unitType": "department", "code": "D0", "name": "D"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "department not allowed under university"
    );
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"parentId": id(&root), "unitType": "lab", "code": "L0", "name": "L"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown type");
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"parentId": uuid::Uuid::now_v7(), "unitType": "faculty", "code": "F9", "name": "F"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown parent");
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"parentId": id(&root), "unitType": "faculty", "code": "bad code", "name": "F"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let fac = unit(&app, &admin, &inst, Some(&id(&root)), "faculty", "ENG").await;
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"parentId": id(&root), "unitType": "faculty", "code": "ENG", "name": "Dup"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "duplicate code");
    assert_eq!(fac["path"], format!("/{}/{}/", id(&root), id(&fac)));
    assert_eq!(fac["depth"], 1);

    // Activation order: the parent first.
    let (s, b) = unit_cmd(&app, &admin, &fac, "activate", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    let root = activate_unit(&app, &admin, &root).await;
    let fac_draft = fac.clone();
    let fac = activate_unit(&app, &admin, &fac).await;
    let (s, _) = unit_cmd(&app, &admin, &fac_draft, "activate", None).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED, "stale version");
    let fac_v = ver(&fac);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{API}/org-units/{}/activate", id(&fac)),
        json!({}),
        Some(fac_v),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "already active");

    // Governed modification: descriptive attributes only.
    let upath = format!("{API}/org-units/{}", id(&fac));
    for bad in [
        json!({"parentId": id(&root)}),
        json!({"unitType": "department"}),
        json!({"status": "archived"}),
        json!({"code": "X"}),
        json!({}),
    ] {
        let (s, b) = patch(&app, &admin, &upath, bad.clone(), fac_v).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{bad}: {b}");
    }
    let (s, fac) = patch(
        &app,
        &admin,
        &upath,
        json!({"name": "Faculty of Engineering", "description": "Est. 1990"}),
        fac_v,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{fac}");
    assert_eq!(fac["name"], "Faculty of Engineering");
    let (s, _) = patch(&app, &admin, &upath, json!({"name": "Late"}), fac_v).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, _, _) = send(
        &app,
        write(&admin, "PATCH", &upath, json!({"name": "No match"}), None),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_REQUIRED);

    // Suspend / restore (reason required for suspension).
    let (s, _) = unit_cmd(&app, &admin, &fac, "suspend", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, fac) = unit_cmd(&app, &admin, &fac, "suspend", Some("Accreditation review")).await;
    assert_eq!(s, StatusCode::OK, "{fac}");
    assert_eq!(fac["status"], "suspended");
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"parentId": id(&fac), "unitType": "department", "code": "CSE", "name": "CSE"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "suspended units take no new units");
    let (s, _) = patch(
        &app,
        &admin,
        &upath,
        json!({"name": "While suspended"}),
        ver(&fac),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = unit_cmd(&app, &admin, &fac, "activate", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "restoration goes through /restore");
    let (s, fac) = unit_cmd(&app, &admin, &fac, "restore", None).await;
    assert_eq!(s, StatusCode::OK, "{fac}");
    assert_eq!(fac["status"], "active");

    // Decommission preconditions: live children; draft units cannot be decommissioned.
    let dept = unit(&app, &admin, &inst, Some(&id(&fac)), "department", "CSE").await;
    let (s, _) = unit_cmd(&app, &admin, &dept, "decommission", Some("x")).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "draft → decommissioned is not a transition"
    );
    let dept = activate_unit(&app, &admin, &dept).await;
    let (s, b) = unit_cmd(&app, &admin, &fac, "decommission", Some("Closing")).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(detail(&b).contains("1 live child units"));
    let (s, _) = unit_cmd(&app, &admin, &dept, "decommission", None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, dept) = unit_cmd(
        &app,
        &admin,
        &dept,
        "decommission",
        Some("Programme closed"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{dept}");
    assert_eq!(dept["status"], "decommissioned");
    for verb in ["activate", "restore", "suspend"] {
        let (s, _) = unit_cmd(&app, &admin, &dept, verb, Some("r")).await;
        assert_eq!(s, StatusCode::CONFLICT, "{verb} on decommissioned");
    }
    let (s, _) = patch(
        &app,
        &admin,
        &format!("{API}/org-units/{}", id(&dept)),
        json!({"name": "x"}),
        ver(&dept),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, dept) = unit_cmd(
        &app,
        &admin,
        &dept,
        "archive",
        Some("Retention period over"),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{dept}");
    assert_eq!(dept["status"], "archived");
    let (s, _) = unit_cmd(&app, &admin, &dept, "archive", Some("again")).await;
    assert_eq!(s, StatusCode::CONFLICT);
    // The archived code is never reused.
    let (s, _) = post(
        &app,
        &admin,
        &units,
        json!({"parentId": id(&fac), "unitType": "department", "code": "CSE", "name": "CSE again"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    // Now the faculty can go (its only child is archived).
    let (s, fac) = unit_cmd(&app, &admin, &fac, "decommission", Some("Merged elsewhere")).await;
    assert_eq!(s, StatusCode::OK, "{fac}");

    let (_, hist) = read(
        &app,
        &admin,
        &format!("{API}/org-units/{}/history", id(&dept)),
    )
    .await;
    let changes: Vec<_> = hist["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["change"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        changes,
        vec!["registered", "activated", "decommissioned", "archived"]
    );
    assert_eq!(hist["items"][2]["reason"], "Programme closed");
    assert_eq!(
        outbox(&pool, &id(&dept)).await,
        vec![
            "org_unit.registered",
            "org_unit.activated",
            "org_unit.decommissioned",
            "org_unit.archived"
        ]
    );
    assert_eq!(
        outbox(&pool, &id(&fac)).await,
        vec![
            "org_unit.registered",
            "org_unit.activated",
            "org_unit.modified",
            "org_unit.suspended",
            "org_unit.restored",
            "org_unit.decommissioned"
        ]
    );
    let modified = outbox_data(&pool, &id(&fac), "org_unit.modified").await;
    assert_eq!(modified["changedFields"], json!(["name", "description"]));
    assert!(actions(&audits(&pool, &id(&fac)).await).contains(&"institution.org_unit.modify"));

    let (_, list) = read(&app, &admin, &format!("{units}?status=active")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (_, kids) = read(&app, &admin, &format!("{units}?parentId={}", id(&root))).await;
    assert_eq!(kids["items"].as_array().unwrap().len(), 1);
    let _ = root;
}

/// L3-B-010 type migration: a draft unit on a superseded blueprint version cannot activate
/// until migrated; the target must be the published version and compatible with the parent
/// and the children.
#[tokio::test]
async fn organisation_type_migration_to_effective_blueprint() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, _) = new_institution(&app, &admin).await;
    let today = business_today(&pool, &inst).await.to_string();
    let v1 = publish_blueprint(&app, &admin, &inst, std_types(), &today).await;
    let root = activate_unit(
        &app,
        &admin,
        &unit(&app, &admin, &inst, None, "university", "UN").await,
    )
    .await;
    let fac = activate_unit(
        &app,
        &admin,
        &unit(&app, &admin, &inst, Some(&id(&root)), "faculty", "SCI").await,
    )
    .await;
    let _dept = activate_unit(
        &app,
        &admin,
        &unit(&app, &admin, &inst, Some(&id(&fac)), "department", "PHY").await,
    )
    .await;
    let pending = unit(&app, &admin, &inst, Some(&id(&root)), "faculty", "ART").await;

    // v2: faculties become "school"s; departments may sit under schools or faculties.
    let v2_types = json!([
        {"key": "university", "label": "University", "allowRoot": true},
        {"key": "faculty", "label": "Faculty", "allowedParentTypes": ["university"]},
        {"key": "school", "label": "School", "allowedParentTypes": ["university"]},
        {"key": "department", "label": "Department", "allowedParentTypes": ["school", "faculty", "department"]},
    ]);
    let v2 = publish_blueprint(&app, &admin, &inst, v2_types, &today).await;
    assert_eq!(v2["versionNo"], 2);
    assert_eq!(v2["supersedesId"], v1["id"]);
    let (_, old) = read(
        &app,
        &admin,
        &format!("{API}/org-type-blueprints/{}", id(&v1)),
    )
    .await;
    assert_eq!(old["status"], "superseded");

    let (s, b) = unit_cmd(&app, &admin, &pending, "activate", None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(detail(&b).contains("migrate"));
    let mpath = |u: &Value| format!("{API}/org-units/{}/migrate-type", id(u));
    let (s, _) = post(
        &app,
        &admin,
        &mpath(&pending),
        json!({"targetBlueprintId": uuid::Uuid::now_v7()}),
        Some(ver(&pending)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "unknown target blueprint"
    );
    let (s, _) = post(
        &app,
        &admin,
        &mpath(&pending),
        json!({"targetBlueprintId": id(&v1), "unitType": "university"}),
        Some(ver(&pending)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "superseded target");
    let (s, _) = post(
        &app,
        &admin,
        &mpath(&pending),
        json!({"targetBlueprintId": id(&v2), "unitType": "lab"}),
        Some(ver(&pending)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "unknown target type");
    let (s, _) = post(
        &app,
        &admin,
        &mpath(&pending),
        json!({"targetBlueprintId": id(&v2), "unitType": "department"}),
        Some(ver(&pending)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "department not allowed under university"
    );
    let (s, migrated) = post(
        &app,
        &admin,
        &mpath(&pending),
        json!({"targetBlueprintId": id(&v2), "unitType": "school"}),
        Some(ver(&pending)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{migrated}");
    assert_eq!(migrated["blueprintId"], v2["id"]);
    assert_eq!(migrated["unitType"], "school");
    activate_unit(&app, &admin, &migrated).await;

    // Migrating the faculty to "school" keeps PHY compatible (v1 department allows faculty and
    // department parents only) → incompatible child, refused.
    let (s, b) = post(
        &app,
        &admin,
        &mpath(&fac),
        json!({"targetBlueprintId": id(&v2), "unitType": "school"}),
        Some(ver(&fac)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{b}");
    assert!(detail(&b).contains("PHY"));
    // Same key on v2 is fine.
    let (s, b) = post(
        &app,
        &admin,
        &mpath(&fac),
        json!({"targetBlueprintId": id(&v2)}),
        Some(ver(&fac)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{b}");
    let (_, hist) = read(
        &app,
        &admin,
        &format!("{API}/org-units/{}/history", id(&pending)),
    )
    .await;
    let last = hist["items"].as_array().unwrap().last().unwrap().clone();
    let first = hist["items"][0].clone();
    assert_eq!(
        first["blueprintId"], v1["id"],
        "history keeps the source association"
    );
    assert_eq!(hist["items"][1]["change"], "type_migrated");
    assert_eq!(last["change"], "activated");
    let m = outbox_data(&pool, &id(&pending), "org_unit.modified").await;
    assert_eq!(m["changedFields"], json!(["blueprintId", "unitType"]));
    assert!(
        actions(&audits(&pool, &id(&pending)).await).contains(&"institution.org_unit.migrate_type")
    );
}

/// L3-B-009 approved structure transformation: move with subtree paths and preserved history,
/// cycles and self-parenting refused, merge moves children and decommissions sources, a stale
/// baseline is never applied, rejection closes the request.
#[tokio::test]
async fn organisation_restructure_move_merge_cycles_and_stale_baseline() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, _) = new_institution(&app, &admin).await;
    let today = business_today(&pool, &inst).await;
    publish_blueprint(&app, &admin, &inst, json!([
        {"key": "university", "label": "University", "allowRoot": true},
        {"key": "division", "label": "Division", "allowedParentTypes": ["university", "division"]},
    ]), &today.to_string()).await;
    let active = |u: Value| {
        let app = app.clone();
        let admin = Login {
            cookie: admin.cookie.clone(),
            csrf: admin.csrf.clone(),
        };
        async move { activate_unit(&app, &admin, &u).await }
    };
    let root = active(unit(&app, &admin, &inst, None, "university", "UN").await).await;
    let a = active(unit(&app, &admin, &inst, Some(&id(&root)), "division", "DA").await).await;
    let b = active(unit(&app, &admin, &inst, Some(&id(&a)), "division", "DB").await).await;
    let c = active(unit(&app, &admin, &inst, Some(&id(&b)), "division", "DC").await).await;
    let d = active(unit(&app, &admin, &inst, Some(&id(&root)), "division", "DD").await).await;
    let rpath = inst_path(&inst, "/org-restructures");

    // Validation and cycles.
    let (s, e) = post(&app, &admin, &rpath, json!({"kind": "move", "reason": "x", "moves": [{"orgUnitId": id(&a), "toParentId": id(&a)}]}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    let (s, e) = post(&app, &admin, &rpath, json!({"kind": "move", "reason": "x", "moves": [{"orgUnitId": id(&a), "toParentId": id(&c)}]}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert!(detail(&e).contains("cycle"));
    let (s, e) = post(&app, &admin, &rpath, json!({"kind": "move", "reason": "x", "moves": [{"orgUnitId": id(&root), "toParentId": id(&d)}]}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "root cannot move: {e}");
    let (s, e) = post(&app, &admin, &rpath, json!({"kind": "move", "reason": "x", "effectiveFrom": plus(today, 5), "moves": [{"orgUnitId": id(&b), "toParentId": id(&d)}]}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "future-dated: {e}");
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.org_restructure WHERE institution_id = $1::uuid",
    )
    .bind(&inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(n, 0, "refused requests leave no trace");

    // Move B (with C) under D: auto-approved and applied.
    let (s, r) = post(&app, &admin, &rpath, json!({"kind": "move", "reason": "Realign divisions", "moves": [{"orgUnitId": id(&b), "toParentId": id(&d)}]}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["status"], "applied");
    assert_eq!(r["moves"][0]["fromParentId"], a["id"]);
    let (_, b2) = read(&app, &admin, &format!("{API}/org-units/{}", id(&b))).await;
    let (_, c2) = read(&app, &admin, &format!("{API}/org-units/{}", id(&c))).await;
    assert_eq!(b2["parentId"], d["id"]);
    assert_eq!(b2["path"], format!("/{}/{}/{}/", id(&root), id(&d), id(&b)));
    assert_eq!(
        c2["path"],
        format!("/{}/{}/{}/{}/", id(&root), id(&d), id(&b), id(&c))
    );
    assert_eq!(c2["depth"], 3);
    assert_eq!(
        c2["version"], c["version"],
        "descendants keep their parent and version"
    );
    let (_, hist) = read(&app, &admin, &format!("{API}/org-units/{}/history", id(&b))).await;
    let items = hist["items"].as_array().unwrap();
    assert_eq!(
        items[0]["parentId"], a["id"],
        "historical parent is never rewritten"
    );
    assert_eq!(items.last().unwrap()["change"], "moved");
    assert_eq!(items.last().unwrap()["restructureId"], r["id"]);
    let ev = outbox_data(&pool, &id(&r), "org_structure.restructured").await;
    assert_eq!(ev["moves"][0]["orgUnitId"], b["id"]);
    assert_eq!(ev["effectiveFrom"], today.to_string());
    assert!(ev["processInstanceId"]
        .as_str()
        .unwrap()
        .starts_with("noop-"));
    let ra = audits(&pool, &id(&r)).await;
    assert_eq!(
        actions(&ra),
        vec![
            "institution.org_restructure.request",
            "institution.org_restructure.apply"
        ]
    );
    assert_eq!(ra[1].1, "system");

    // Merge A into D: A has no children left; then merge D into ... use B (child C) into A.
    let (s, r) = post(&app, &admin, &rpath, json!({"kind": "merge", "reason": "Consolidate", "mergeSourceIds": [id(&b)], "mergeTargetId": id(&a)}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    assert_eq!(r["status"], "applied");
    let (_, b3) = read(&app, &admin, &format!("{API}/org-units/{}", id(&b))).await;
    let (_, c3) = read(&app, &admin, &format!("{API}/org-units/{}", id(&c))).await;
    assert_eq!(b3["status"], "decommissioned");
    assert_eq!(c3["parentId"], a["id"]);
    let ev = outbox_data(&pool, &id(&r), "org_structure.restructured").await;
    assert_eq!(ev["decommissionedUnitIds"], json!([id(&b)]));
    assert_eq!(ev["moves"][0]["orgUnitId"], c["id"]);
    assert!(outbox(&pool, &id(&b))
        .await
        .contains(&"org_unit.decommissioned".to_owned()));
    // Target inside the source = cycle; decommissioned sources are refused.
    let (s, _) = post(&app, &admin, &rpath, json!({"kind": "merge", "reason": "x", "mergeSourceIds": [id(&a)], "mergeTargetId": id(&c)}), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = post(&app, &admin, &rpath, json!({"kind": "merge", "reason": "x", "mergeSourceIds": [id(&b)], "mergeTargetId": id(&d)}), None).await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "B is no longer a live unit"
    );
    // A merge source with active positions is refused.
    let (s, pos) = post(&app, &admin, &inst_path(&inst, "/positions"), json!({"orgUnitId": id(&d), "code": "DIR-D", "title": "Director", "effectiveFrom": today.to_string()}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{pos}");
    let (s, e) = post(&app, &admin, &rpath, json!({"kind": "merge", "reason": "x", "mergeSourceIds": [id(&d)], "mergeTargetId": id(&a)}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert!(detail(&e).contains("positions"));

    // Awaited approval: the structure changes meanwhile → stale baseline refused; reject closes.
    let st = await_state(&pool);
    let sc = scope(t, &inst);
    let cmd: sutra_institution::org::domain::RequestRestructure = serde_json::from_value(json!({
        "kind": "move", "reason": "Pending review", "moves": [{"orgUnitId": id(&c), "toParentId": id(&d)}]
    }))
    .unwrap();
    let pending = sutra_institution::org::restructure::request(
        &st,
        &sc,
        inst.parse().unwrap(),
        &cmd,
        &admin_actor(),
    )
    .await
    .unwrap();
    assert_eq!(pending.status, "pending_approval");
    let (_, c_now) = read(&app, &admin, &format!("{API}/org-units/{}", id(&c))).await;
    let (s, _) = patch(
        &app,
        &admin,
        &format!("{API}/org-units/{}", id(&c)),
        json!({"name": "Renamed while pending"}),
        ver(&c_now),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let dpath = format!("{API}/org-restructures/{}/decision", pending.id);
    let (s, e) = post(
        &app,
        &admin,
        &dpath,
        json!({"decision": "approve"}),
        Some(pending.version.0),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert!(detail(&e).contains("structure changed"));
    let (_, c_after) = read(&app, &admin, &format!("{API}/org-units/{}", id(&c))).await;
    assert_eq!(
        c_after["parentId"], a["id"],
        "stale transformation not applied"
    );
    let (s, _) = post(
        &app,
        &admin,
        &dpath,
        json!({"decision": "reject"}),
        Some(pending.version.0),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, rej) = post(
        &app,
        &admin,
        &dpath,
        json!({"decision": "reject", "reason": "Superseded by rename"}),
        Some(pending.version.0),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rej}");
    assert_eq!(rej["status"], "rejected");
    let (s, _) = post(
        &app,
        &admin,
        &dpath,
        json!({"decision": "approve"}),
        Some(ver(&rej)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (_, list) = read(&app, &admin, &rpath).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    let (s, one) = read(
        &app,
        &admin,
        &format!("{API}/org-restructures/{}", pending.id),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(one["decisionReason"], "Superseded by rename");

    // A pending move approved while the baseline holds is applied by the decision.
    let cmd: sutra_institution::org::domain::RequestRestructure = serde_json::from_value(json!({
        "kind": "move", "reason": "Second try", "moves": [{"orgUnitId": id(&c), "toParentId": id(&d)}]
    }))
    .unwrap();
    let pending = sutra_institution::org::restructure::request(
        &st,
        &sc,
        inst.parse().unwrap(),
        &cmd,
        &admin_actor(),
    )
    .await
    .unwrap();
    let (s, ok) = post(
        &app,
        &admin,
        &format!("{API}/org-restructures/{}/decision", pending.id),
        json!({"decision": "approve", "reason": "Fine"}),
        Some(pending.version.0),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{ok}");
    assert_eq!(ok["status"], "applied");
    assert_eq!(ok["decidedBy"], "dev|admin@sutra.dev");
}

/// L3-B-011 governance configuration: drafts never govern, activation requires an active unit,
/// no retroactive or overlapping authority, predecessor closed the day before its successor.
#[tokio::test]
async fn governance_configuration_effective_dating() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, _) = new_institution(&app, &admin).await;
    let today = business_today(&pool, &inst).await;
    publish_blueprint(&app, &admin, &inst, std_types(), &today.to_string()).await;
    let root = unit(&app, &admin, &inst, None, "university", "UN").await;
    let gpath = format!("{API}/org-units/{}/governance-configs", id(&root));
    let settings = json!({"headTitle": "Vice Chancellor", "approvalLevels": 2, "quorum": 5, "committees": ["Senate"]});

    let (s, e) = post(&app, &admin, &gpath, json!({"effectiveFrom": today.to_string(), "settings": {"headTitle": "", "approvalLevels": 9}}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    let (s, g1) = post(&app, &admin, &gpath, json!({"effectiveFrom": today.checked_sub_days(Days::new(1)).unwrap().to_string(), "settings": settings}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{g1}");
    assert_eq!(g1["revision"], 1);
    let (s, _) = post(
        &app,
        &admin,
        &gpath,
        json!({"effectiveFrom": today.to_string(), "settings": settings}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "one open draft per unit");
    let (s, e) = read(
        &app,
        &admin,
        &format!("{API}/org-units/{}/effective-governance", id(&root)),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND, "drafts never govern: {e}");
    let g1path = format!("{API}/governance-configs/{}", id(&g1));
    let (s, _) = post(
        &app,
        &admin,
        &format!("{g1path}/activate"),
        json!({}),
        Some(ver(&g1)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "unit not active");
    activate_unit(&app, &admin, &root).await;
    let (s, _) = post(
        &app,
        &admin,
        &format!("{g1path}/activate"),
        json!({}),
        Some(ver(&g1)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "retroactive");
    let (s, g1) = put(
        &app,
        &admin,
        &g1path,
        json!({"effectiveFrom": today.to_string(), "settings": settings}),
        ver(&g1),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{g1}");
    let (s, g1) = post(
        &app,
        &admin,
        &format!("{g1path}/activate"),
        json!({}),
        Some(ver(&g1)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{g1}");
    assert_eq!(g1["status"], "published");
    let (s, _) = put(
        &app,
        &admin,
        &g1path,
        json!({"effectiveFrom": today.to_string(), "settings": settings}),
        ver(&g1),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "published revisions are immutable");

    let (s, g2) = post(&app, &admin, &gpath, json!({"effectiveFrom": plus(today, 10), "settings": {"headTitle": "Vice Chancellor", "approvalLevels": 3}}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{g2}");
    assert_eq!(g2["supersedesId"], g1["id"]);
    let (s, g2) = post(
        &app,
        &admin,
        &format!("{API}/governance-configs/{}/activate", id(&g2)),
        json!({}),
        Some(ver(&g2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{g2}");
    let (_, g1now) = read(&app, &admin, &g1path).await;
    assert_eq!(g1now["status"], "superseded");
    assert_eq!(g1now["effectiveTo"], plus(today, 9));
    let eff = |q: String| {
        let app = app.clone();
        let admin = Login {
            cookie: admin.cookie.clone(),
            csrf: admin.csrf.clone(),
        };
        let path = format!("{API}/org-units/{}/effective-governance{q}", id(&root));
        async move { read(&app, &admin, &path).await }
    };
    let (_, now) = eff(String::new()).await;
    assert_eq!(
        now["revision"], 1,
        "the predecessor governs until the successor's date"
    );
    let (_, later) = eff(format!("?asOf={}", plus(today, 10))).await;
    assert_eq!(later["revision"], 2);
    assert_eq!(later["settings"]["approvalLevels"], 3);
    let (_, day9) = eff(format!("?asOf={}", plus(today, 9))).await;
    assert_eq!(day9["revision"], 1);

    let (s, g3) = post(
        &app,
        &admin,
        &gpath,
        json!({"effectiveFrom": plus(today, 10), "settings": settings}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{API}/governance-configs/{}/activate", id(&g3)),
        json!({}),
        Some(ver(&g3)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "overlapping authority");
    let (_, list) = read(&app, &admin, &gpath).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 3);
    assert_eq!(
        outbox(&pool, &id(&g1)).await,
        vec!["governance_config.activated"]
    );
    let ev = outbox_data(&pool, &id(&g2), "governance_config.activated").await;
    assert_eq!(ev["revision"], 2);
    assert_eq!(ev["effectiveFrom"], plus(today, 10));
    assert_eq!(
        actions(&audits(&pool, &id(&g1)).await),
        vec![
            "institution.governance_config.create",
            "institution.governance_config.update",
            "institution.governance_config.activate"
        ]
    );
}

/// L3-B-013/-014/-015 effective-dated position definitions.
#[tokio::test]
async fn position_definitions_are_effective_dated() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, _) = new_institution(&app, &admin).await;
    let today = business_today(&pool, &inst).await;
    publish_blueprint(&app, &admin, &inst, std_types(), &today.to_string()).await;
    let root = activate_unit(
        &app,
        &admin,
        &unit(&app, &admin, &inst, None, "university", "UN").await,
    )
    .await;
    let fac = unit(&app, &admin, &inst, Some(&id(&root)), "faculty", "ENG").await;
    let ppath = inst_path(&inst, "/positions");

    let (s, e) = post(&app, &admin, &ppath, json!({"orgUnitId": id(&fac), "code": "bad code", "title": "", "headcount": 0, "effectiveFrom": today.to_string()}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{e}");
    let (s, _) = post(&app, &admin, &ppath, json!({"orgUnitId": uuid::Uuid::now_v7(), "code": "XX", "title": "X", "effectiveFrom": today.to_string()}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, p) = post(&app, &admin, &ppath, json!({"orgUnitId": id(&fac), "code": "DEAN-ENG", "title": "Dean", "grade": "P1", "effectiveFrom": today.to_string()}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{p}");
    assert_eq!(p["status"], "draft");
    assert_eq!(
        (p["revision"].as_i64(), p["headcount"].as_i64()),
        (Some(1), Some(1))
    );
    let pp = format!("{API}/positions/{}", id(&p));
    let (s, p) = patch(
        &app,
        &admin,
        &pp,
        json!({"title": "Dean of Engineering", "headcount": 1}),
        ver(&p),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["title"], "Dean of Engineering");
    assert_eq!(p["revision"], 1, "a draft is edited in place");

    // Activation needs an active unit.
    let (s, e) = post(
        &app,
        &admin,
        &format!("{pp}/activate"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    let fac = activate_unit(&app, &admin, &fac).await;
    let (s, p) = post(
        &app,
        &admin,
        &format!("{pp}/activate"),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p}");
    assert_eq!(p["status"], "active");
    let (s, _) = patch(&app, &admin, &pp, json!({"title": "Overwrite"}), ver(&p)).await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "active definitions are never overwritten"
    );

    // Effective-dated change.
    let change = |b: Value, v: i64| {
        let app = app.clone();
        let admin = Login {
            cookie: admin.cookie.clone(),
            csrf: admin.csrf.clone(),
        };
        let path = format!("{pp}/changes");
        async move { post(&app, &admin, &path, b, Some(v)).await }
    };
    let (s, _) = change(json!({"title": "Dean", "effectiveFrom": today.checked_sub_days(Days::new(1)).unwrap().to_string(), "reason": "r"}), ver(&p)).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "retroactive");
    let (s, _) = change(
        json!({"title": "Dean of Engineering", "effectiveFrom": plus(today, 30), "reason": "r"}),
        ver(&p),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "no change");
    let (s, _) = change(
        json!({"title": "Dean", "effectiveFrom": today.to_string(), "reason": "r"}),
        ver(&p),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "must be after the latest revision's date"
    );
    let (s, _) = change(
        json!({"title": "Dean", "effectiveFrom": plus(today, 30)}),
        ver(&p),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, p2) = change(json!({"title": "Dean (Engineering & Technology)", "headcount": 2, "effectiveFrom": plus(today, 30), "reason": "Faculty expansion"}), ver(&p)).await;
    assert_eq!(s, StatusCode::OK, "{p2}");
    assert_eq!(
        p2["revision"], 1,
        "the current revision stays authoritative until the successor's date"
    );
    assert_eq!(p2["title"], "Dean of Engineering");
    assert_eq!(p2["effectiveTo"], plus(today, 29));
    assert_eq!(p2["latestRevision"], 2);
    assert_eq!(p2["scheduledRevision"]["revision"], 2);
    let (_, future) = read(&app, &admin, &format!("{pp}?asOf={}", plus(today, 30))).await;
    assert_eq!(future["revision"], 2);
    assert_eq!(future["headcount"], 2);
    assert!(future["scheduledRevision"].is_null());
    let (_, revs) = read(&app, &admin, &format!("{pp}/revisions")).await;
    assert_eq!(revs["items"].as_array().unwrap().len(), 2);
    assert_eq!(revs["items"][1]["reason"], "Faculty expansion");
    let (_, by_unit) = read(&app, &admin, &format!("{ppath}?orgUnitId={}", id(&fac))).await;
    assert_eq!(by_unit["items"].as_array().unwrap().len(), 1);
    let (s, _) = change(
        json!({"title": "x", "effectiveFrom": plus(today, 40), "reason": "r"}),
        ver(&p),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);

    // The unit cannot be decommissioned while the definition is live.
    let (s, e) = unit_cmd(&app, &admin, &fac, "decommission", Some("Closing")).await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    assert!(detail(&e).contains("positions"));

    // Retirement: not before the scheduled change; reason required; then archive.
    let rp = format!("{pp}/retire");
    let (s, _) = post(&app, &admin, &rp, json!({}), Some(ver(&p2))).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, e) = post(
        &app,
        &admin,
        &rp,
        json!({"reason": "Restructure"}),
        Some(ver(&p2)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{e}");
    let (s, p3) = post(
        &app,
        &admin,
        &rp,
        json!({"reason": "Restructure", "effectiveTo": plus(today, 40)}),
        Some(ver(&p2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p3}");
    assert_eq!(p3["status"], "retired");
    assert_eq!(p3["retiredOn"], plus(today, 40));
    for verb in ["activate", "retire"] {
        let (s, _) = post(
            &app,
            &admin,
            &format!("{pp}/{verb}"),
            json!({"reason": "r"}),
            Some(ver(&p3)),
        )
        .await;
        assert_eq!(s, StatusCode::CONFLICT, "{verb} on retired");
    }
    let (s, _) = change(
        json!({"title": "x", "effectiveFrom": plus(today, 50), "reason": "r"}),
        ver(&p3),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, p4) = post(
        &app,
        &admin,
        &format!("{pp}/archive"),
        json!({}),
        Some(ver(&p3)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{p4}");
    assert_eq!(p4["status"], "archived");
    let (s, _) = post(
        &app,
        &admin,
        &format!("{pp}/activate"),
        json!({}),
        Some(ver(&p4)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "no shortcut reactivation");
    let (s, _) = post(&app, &admin, &ppath, json!({"orgUnitId": id(&fac), "code": "DEAN-ENG", "title": "Dean", "effectiveFrom": today.to_string()}), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "codes are never reused");
    // Positions on a suspended unit cannot be established.
    let (_, fac_now) = read(&app, &admin, &format!("{API}/org-units/{}", id(&fac))).await;
    let (s, fac_s) = unit_cmd(&app, &admin, &fac_now, "suspend", Some("Review")).await;
    assert_eq!(s, StatusCode::OK, "{fac_s}");
    let (s, _) = post(&app, &admin, &ppath, json!({"orgUnitId": id(&fac), "code": "NEW-1", "title": "New", "effectiveFrom": today.to_string()}), None).await;
    assert_eq!(s, StatusCode::CONFLICT);

    assert_eq!(
        outbox(&pool, &id(&p)).await,
        vec![
            "position.established",
            "position.activated",
            "position.changed",
            "position.retired",
            "position.archived"
        ]
    );
    let changed = outbox_data(&pool, &id(&p), "position.changed").await;
    assert_eq!(changed["revision"], 2);
    assert_eq!(changed["effectiveFrom"], plus(today, 30));
    assert_eq!(changed["title"], "Dean (Engineering & Technology)");
    let acts = audits(&pool, &id(&p)).await;
    assert_eq!(
        actions(&acts),
        vec![
            "institution.position.establish",
            "institution.position.update_draft",
            "institution.position.activate",
            "institution.position.change",
            "institution.position.retire",
            "institution.position.archive"
        ]
    );
    // No occupant / assignment is ever created: only definition tables are written.
    let r = sutra_institution::shared::readiness::evaluate(
        &pool,
        &scope(t, &inst),
        inst.parse().unwrap(),
    )
    .await
    .unwrap();
    let item = r
        .items
        .iter()
        .find(|i| i.key == "position.definitions_active")
        .unwrap();
    assert_eq!(
        serde_json::to_value(item.status).unwrap(),
        "unmet",
        "the only definition is archived"
    );
}

/// The 2A readiness provider returns all six keys every time with the contract's required
/// flags, and they turn `met` as setup progresses.
#[tokio::test]
async fn readiness_provider_reports_every_2a_key() {
    let t = tenant();
    let (app, pool) = app(t).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let (inst, v) = new_institution(&app, &admin).await;
    let sc = scope(t, &inst);
    let iid: InstitutionId = inst.parse().unwrap();
    let keys = |r: &sutra_institution::shared::readiness::ReadinessReport| -> HashMap<String, (bool, String)> {
        r.items
            .iter()
            .filter(|i| sutra_institution::lifecycle::readiness::KEYS.contains(&i.key.as_str()))
            .map(|i| (i.key.clone(), (i.required, serde_json::to_value(i.status).unwrap().as_str().unwrap().to_owned())))
            .collect()
    };
    let r = sutra_institution::shared::readiness::evaluate(&pool, &sc, iid)
        .await
        .unwrap();
    let k = keys(&r);
    assert_eq!(k.len(), 6);
    assert!(!r.ready);
    let required: Vec<_> = {
        let mut x: Vec<_> = k
            .iter()
            .filter(|(_, (req, _))| *req)
            .map(|(k, _)| k.as_str())
            .collect();
        x.sort_unstable();
        x
    };
    assert_eq!(
        required,
        vec![
            "institution.profile_complete",
            "institution.timezone_set",
            "organisation.root_unit_active"
        ]
    );
    assert_eq!(
        k["institution.timezone_set"].1, "met",
        "Asia/Kolkata by default"
    );
    assert_eq!(k["institution.profile_complete"].1, "unmet");
    assert_eq!(k["organisation.root_unit_active"].1, "unmet");
    assert!(r.blocking_keys().contains(&"institution.profile_complete"));

    let (_, root, _) = ready_2a(&app, &admin, &pool, &inst, v).await;
    let today = business_today(&pool, &inst).await.to_string();
    let (s, g) = post(
        &app,
        &admin,
        &format!("{API}/org-units/{}/governance-configs", id(&root)),
        json!({"effectiveFrom": today, "settings": {"headTitle": "VC", "approvalLevels": 1}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{API}/governance-configs/{}/activate", id(&g)),
        json!({}),
        Some(ver(&g)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, p) = post(&app, &admin, &inst_path(&inst, "/positions"), json!({"orgUnitId": id(&root), "code": "VC", "title": "Vice Chancellor", "effectiveFrom": today}), None).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = post(
        &app,
        &admin,
        &format!("{API}/positions/{}/activate", id(&p)),
        json!({}),
        Some(ver(&p)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let r = sutra_institution::shared::readiness::evaluate(&pool, &sc, iid)
        .await
        .unwrap();
    let k = keys(&r);
    for (key, (_, status)) in &k {
        if key != "institution.policies_effective" {
            assert_eq!(status, "met", "{key}");
        }
    }
    assert_eq!(k["institution.policies_effective"].1, "unmet");
    assert!(r
        .items
        .iter()
        .filter(|i| sutra_institution::lifecycle::readiness::KEYS.contains(&i.key.as_str()))
        .all(|i| !i.blocking()));
}

/// Same as `support::app`, with extra environment (e.g. `SUTRA_DEV_MFA=true` → step-up enforced).
async fn app_with_env(
    tenant: &'static str,
    extra: &[(&'static str, &'static str)],
) -> (Router, sqlx::PgPool) {
    use std::collections::HashMap as Map;
    let owner = owner_pool().await;
    let url = database_url();
    let (scheme, rest) = url.split_once("://").unwrap();
    let host = rest.rsplit_once('@').map_or(rest, |(_, h)| h);
    let runtime: &'static str =
        Box::leak(format!("{scheme}://{RUNTIME_USER}:{RUNTIME_PASSWORD}@{host}").into_boxed_str());
    let mut src: Map<&'static str, &'static str> = Map::from([
        ("SUTRA_ENV", "development"),
        ("SUTRA_DEV_AUTH_PASSWORD", sutra_api::app::SPEC_DEV_PASSWORD),
        ("DATABASE_URL", runtime),
        ("SUTRA_SESSION_STORE", "postgres"),
        ("SUTRA_IDEMPOTENCY_STORE", "postgres"),
        ("SUTRA_AUDIT_SINK", "postgres"),
        ("SUTRA_DEV_TENANT_ID", tenant),
        ("SUTRA_SWEEPERS", "false"),
        ("SUTRA_INBOX_CONSUMER", "false"),
        ("SUTRA_APPROVAL_MODE", "auto"), // Part 1: explicit dev opt-in (subject is not approval)
    ]);
    src.extend(extra.iter().copied());
    let cfg = sutra_api::config::Config::load(&src).expect("config");
    let ctx = sutra_api::app::AppContext::from_config(&cfg).expect("context");
    (sutra_api::app::router(ctx), owner)
}
