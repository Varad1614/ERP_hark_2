//! Part 2 · Stage 6 (S6-pilot) — regressions for defects found while preparing the UAT
//! (DEF-P2-PILOT-001: by code review while designing dry-run case UAT-EXM-01, not by the dry
//! run itself; `apps/web/e2e/uat-dryrun.e2e.mjs` then verifies it with synthetic data), real
//! PostgreSQL (`db-tests`):
//!
//! * `malpractice_decision_is_approved_by_an_independent_controller` — DEF-P2-PILOT-001: the
//!   governed malpractice approval (`POST /exams/malpractice-cases/{id}/approval`) had no
//!   separation-of-duties guard (its path ends in `/approval`, so the approval-matrix fitness
//!   test never enumerated it), and the guard sat on the maker's proposal step instead. Through
//!   the API composed with the DEFAULT (human) approval mode: the exam official who proposed the
//!   committee decision cannot approve it (403, nothing applied); faculty cannot decide (403);
//!   the controller of examinations can reject it (reason required) and approve it; after a
//!   rejection the same official may propose again (the maker step is not a decision).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use serde_json::{json, Value};
use std::collections::HashMap;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_contracts::exams::status::ExamRegistrationStatus;
use sutra_kernel::approval::ApprovalMode;

const T_MAL: &str = "0192f5a0-7c3e-7000-8000-00000006b601";
const E: &str = "/api/v1/exams";

const ADMIN: &str = "admin@sutra.dev";
const FACULTY: &str = "meera.shah@sutra.dev";
const EXAM_CONTROLLER: &str = "exam.controller@sutra.dev";
const MAKER_ID: &str = "dev|admin@sutra.dev";

/// The API exactly as production composes it, with the **default** approval mode (human), in
/// development auth so the dev personas can sign in (same database / runtime role as
/// `support::app`).
async fn human_app(tenant: &'static str) -> Router {
    let _ = owner_pool().await; // migrations + runtime role
    let url = database_url();
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
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
    ]);
    let cfg = Config::load(&src).expect("config");
    assert_eq!(
        cfg.hardening.workflow.approvals,
        ApprovalMode::AwaitDecision,
        "the default approval mode is a human decision"
    );
    router(AppContext::from_config(&cfg).expect("context"))
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

async fn call(
    app: &Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
    expect: StatusCode,
) -> Value {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    assert_eq!(s, expect, "{method} {path}: {b}");
    b
}

/// `(status, proposal_kind, penalty, version)` of the case in the database.
async fn case_row(pool: &sqlx::PgPool, id: &str) -> (String, Option<String>, Option<String>, i64) {
    sqlx::query_as(
        "SELECT status, proposal_kind, penalty, version FROM exams.malpractice_case \
         WHERE id = $1::uuid",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn malpractice_decision_is_approved_by_an_independent_controller() {
    let (setup_app, pool) = app(T_MAL).await;
    let setup_admin = login(&setup_app, ADMIN).await;
    let setup = b6::exam_setup(
        &setup_app,
        &setup_admin,
        &pool,
        T_MAL,
        "Malpractice Control College",
        Some(b4::AARAV),
        "active",
        "conducted",
    )
    .await;
    let reg = b6::seed_registration(
        &pool,
        &setup,
        setup.session_id,
        &[setup.paper_id],
        ExamRegistrationStatus::Confirmed,
    )
    .await;
    let session = setup.session_id.as_uuid();

    let api = human_app(T_MAL).await;
    let maker = login(&api, ADMIN).await;
    let controller = login(&api, EXAM_CONTROLLER).await;
    let faculty = login(&api, FACULTY).await;

    // The exam cell reports, investigates and proposes the committee decision (maker steps).
    let c = call(
        &api,
        &maker,
        "POST",
        &format!("{E}/exam-sessions/{session}/malpractice-cases"),
        json!({"registrationId": reg.registration_id.as_uuid(),
               "paperId": setup.paper_id.as_uuid(),
               "allegation": "unauthorised notes found during the sitting"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let id = c["id"].as_str().unwrap().to_owned();
    let c = call(
        &api,
        &maker,
        "POST",
        &format!("{E}/malpractice-cases/{id}/investigate"),
        json!({}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    let c = call(
        &api,
        &maker,
        "POST",
        &format!("{E}/malpractice-cases/{id}/decision"),
        json!({"penalty": "warning", "reason": "first offence"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "under_investigation", "{c}");
    assert_eq!(c["pending"]["kind"], "decision", "awaits approval: {c}");

    // The proposer approving their own committee decision → 403, and faculty (no exam.approve)
    // → 403; nothing is applied.
    for who in [&maker, &faculty] {
        call(
            &api,
            who,
            "POST",
            &format!("{E}/malpractice-cases/{id}/approval"),
            json!({"decision": "approve", "reason": "not mine to decide"}),
            Some(ver(&c)),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    let (status, proposal, penalty, version) = case_row(&pool, &id).await;
    assert_eq!(
        (status.as_str(), proposal.as_deref(), penalty, version),
        ("under_investigation", Some("decision"), None, ver(&c)),
        "refused decisions change nothing"
    );

    // The controller of examinations rejects (reason required), clearing the proposal.
    call(
        &api,
        &controller,
        "POST",
        &format!("{E}/malpractice-cases/{id}/approval"),
        json!({"decision": "reject"}),
        Some(ver(&c)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let c = call(
        &api,
        &controller,
        "POST",
        &format!("{E}/malpractice-cases/{id}/approval"),
        json!({"decision": "reject", "reason": "hearing first"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert!(c["pending"].is_null(), "proposal cleared: {c}");

    // The same exam official proposes again: a proposal is the maker step, not a decision.
    let c = call(
        &api,
        &maker,
        "POST",
        &format!("{E}/malpractice-cases/{id}/decision"),
        json!({"penalty": "warning", "reason": "first offence, after review"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["pending"]["kind"], "decision", "{c}");
    // Still not decidable by its proposer …
    call(
        &api,
        &maker,
        "POST",
        &format!("{E}/malpractice-cases/{id}/approval"),
        json!({"decision": "approve"}),
        Some(ver(&c)),
        StatusCode::FORBIDDEN,
    )
    .await;
    // … and the independent controller of examinations applies it.
    let c = call(
        &api,
        &controller,
        "POST",
        &format!("{E}/malpractice-cases/{id}/approval"),
        json!({"decision": "approve", "reason": "committee decision confirmed"}),
        Some(ver(&c)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(c["status"], "decided", "{c}");
    assert_eq!(c["penalty"], "warning", "{c}");
    let (status, proposal, penalty, _) = case_row(&pool, &id).await;
    assert_eq!(
        (status.as_str(), proposal, penalty.as_deref()),
        ("decided", None, Some("warning"))
    );

    // Both proposals were recorded in the approval ledger with their requester.
    let requesters: Vec<Option<String>> = sqlx::query_scalar(
        "SELECT requested_by FROM platform.approval_request WHERE tenant_id = $1::uuid \
         AND subject_id = $2 ORDER BY requested_at",
    )
    .bind(T_MAL)
    .bind(&id)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(requesters.len(), 2, "{requesters:?}");
    let all_maker = requesters.iter().all(|r| r.as_deref() == Some(MAKER_ID));
    assert!(all_maker, "{requesters:?}");
}
