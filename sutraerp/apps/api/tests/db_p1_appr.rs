//! Part 1 · Stage 2 — approval control (DEF-X-001 human approval by default, DEF-X-002 separation
//! of duties). Runs the **real composition root with the default configuration** (no
//! `SUTRA_APPROVAL_MODE`, so governed decisions wait for a human) next to the usual auto-approving
//! setup app on the same database and tenant:
//!
//! * (a) a pending refund / result batch is not applied until it is decided;
//! * (b) the requester (maker) deciding their own request → 403;
//! * (c) a user without the domain's approve permission (faculty; a checker of another domain)
//!   → 403, and a checker cannot raise the requests it decides;
//! * (d) the decision is recorded with approver, timestamp and reason, in the status history and
//!   as an audit event;
//! * (e) production configuration with noop workflow / auto-approval refuses to load.
//!
//! The admission decision (registrar checker, requester 403) is covered in
//! `db_b3a.rs::admission_approval_await_decision_and_finance_evidence`; the matrix of every
//! governed decision is `docs/readiness/APPROVAL_CONTROL_MATRIX.md`.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::{Config, ConfigError};
use sutra_contracts::exams::status::{ExamRegistrationStatus, ResultBatchStatus, ResultStatus};
use sutra_exams::results::repo as rr;
use sutra_exams::shared::Actor;
use sutra_kernel::approval::ApprovalMode;
use sutra_kernel::error::KernelError;

const T_REFUND: &str = "0192f5a0-7c3e-7000-8000-00000001a001";
const T_RESULT: &str = "0192f5a0-7c3e-7000-8000-00000001a002";

const F: &str = "/api/v1/finance";
const E: &str = "/api/v1/exams";

const ADMIN: &str = "admin@sutra.dev";
const FACULTY: &str = "meera.shah@sutra.dev";
const FINANCE_APPROVER: &str = "finance.approver@sutra.dev";
const EXAM_CONTROLLER: &str = "exam.controller@sutra.dev";

/// The API exactly as production composes it, with the **default** approval mode (human), in
/// development auth so the dev personas can sign in. Same database / runtime role as
/// `support::app`.
async fn human_app(tenant: &'static str) -> Router {
    let _ = owner_pool().await; // migrations + runtime role
    let url = database_url();
    let (scheme, rest) = url.split_once("://").expect("postgres URL");
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
    ]);
    let cfg = Config::load(&src).expect("config");
    assert_eq!(
        cfg.hardening.workflow.approvals,
        ApprovalMode::AwaitDecision,
        "the default approval mode is a human decision"
    );
    assert!(!cfg.hardening.workflow.flowable_auto_approve);
    router(AppContext::from_config(&cfg).expect("context"))
}

fn day(m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, m, d).unwrap()
}

fn ver(v: &Value) -> i64 {
    v["version"].as_i64().unwrap()
}

fn dec(s: &str) -> Decimal {
    s.parse().unwrap()
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

async fn read(app: &Router, who: &Login, path: &str) -> Value {
    let (s, _, b) = send(app, get(who, path)).await;
    assert_eq!(s, StatusCode::OK, "GET {path}: {b}");
    b
}

/// Σ debit − Σ credit of a seeded system account of the institution.
async fn balance(pool: &sqlx::PgPool, institution: uuid::Uuid, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(COALESCE(l.debit, 0) - COALESCE(l.credit, 0)), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id WHERE a.institution_id = $1 AND a.system_key = $2",
    )
    .bind(institution)
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// `(actor_id, details)` of the succeeded audit events `action` on `target_id`.
async fn audit_events(
    pool: &sqlx::PgPool,
    tenant: &str,
    action: &str,
    target_id: &str,
) -> Vec<(Option<String>, Value)> {
    sqlx::query_as(
        "SELECT actor_id, details FROM platform.audit_log WHERE tenant_id = $1::uuid AND action = $2 \
         AND target_id = $3 AND outcome = 'succeeded' ORDER BY occurred_at",
    )
    .bind(tenant)
    .bind(action)
    .bind(target_id)
    .fetch_all(pool)
    .await
    .unwrap()
}

// ------------------------------------------------------------------ refunds (finance)

/// (a)–(d) on the refund decision (Batch 5 reference approval) through the default-configured API.
#[tokio::test]
async fn refund_waits_for_an_independent_finance_approver() {
    let (setup_app, pool) = app(T_REFUND).await;
    let setup_admin = login(&setup_app, ADMIN).await;
    let setup = b5::finance_setup(
        &setup_app,
        &setup_admin,
        &pool,
        T_REFUND,
        "Approval Control University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().as_uuid();
    let acc = setup.student_account_id;
    let charge = b5::seed_charge(&pool, &setup, "10000.00", "published", Some(day(6, 1))).await;

    let api = human_app(T_REFUND).await;
    let maker = login(&api, ADMIN).await;
    let checker = login(&api, FINANCE_APPROVER).await;
    let faculty = login(&api, FACULTY).await;
    let other_checker = login(&api, EXAM_CONTROLLER).await;

    call(
        &api,
        &maker,
        "POST",
        &format!("{F}/student-accounts/{acc}/payments"),
        json!({"channel": "cash", "amount": "4000.00",
               "allocations": [{"chargeId": charge.charge_id, "amount": "1000.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;

    // (c) a checker holds the approve permission only: it cannot raise the refund it decides.
    call(
        &api,
        &checker,
        "POST",
        &format!("{F}/student-accounts/{acc}/refunds"),
        json!({"amount": "1200.00", "reason": "overpaid"}),
        None,
        StatusCode::FORBIDDEN,
    )
    .await;

    // (a) the maker's request stays pending: nothing is posted until a decision.
    let r = call(
        &api,
        &maker,
        "POST",
        &format!("{F}/student-accounts/{acc}/refunds"),
        json!({"amount": "1200.00", "reason": "overpaid"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid = r["id"].as_str().unwrap().to_owned();
    assert_eq!(r["status"], "under_review", "{r}");
    assert!(
        r["journalEntryId"].is_null(),
        "nothing posted before the decision"
    );
    assert!(r["decidedBy"].is_null());
    assert_eq!(balance(&pool, inst, "refunds_payable").await, dec("0"));
    call(
        &api,
        &maker,
        "POST",
        &format!("{F}/refunds/{rid}/process"),
        json!({}),
        Some(ver(&r)),
        StatusCode::CONFLICT,
    )
    .await;

    // (b) self-approval is refused.
    call(
        &api,
        &maker,
        "POST",
        &format!("{F}/refunds/{rid}/decision"),
        json!({"decision": "approve", "reason": "my own"}),
        Some(ver(&r)),
        StatusCode::FORBIDDEN,
    )
    .await;
    // (c) no finance.approve permission → 403: faculty, and the exam controller (a checker of
    // another domain).
    for who in [&faculty, &other_checker] {
        call(
            &api,
            who,
            "POST",
            &format!("{F}/refunds/{rid}/decision"),
            json!({"decision": "approve", "reason": "not mine to decide"}),
            Some(ver(&r)),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    // Still pending, still unposted after the refused attempts.
    let still = read(&api, &checker, &format!("{F}/refunds/{rid}")).await;
    assert_eq!(still["status"], "under_review");
    assert_eq!(ver(&still), ver(&r));
    assert_eq!(balance(&pool, inst, "refunds_payable").await, dec("0"));
    assert!(
        audit_events(&pool, T_REFUND, "finance.refund.approve", &rid)
            .await
            .is_empty()
    );

    // (d) the independent finance approver decides; approver, time and reason are recorded.
    let before = chrono::Utc::now() - chrono::Duration::seconds(5);
    let d = call(
        &api,
        &checker,
        "POST",
        &format!("{F}/refunds/{rid}/decision"),
        json!({"decision": "approve", "reason": "verified against the bank statement"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "approved", "{d}");
    assert_eq!(d["decidedBy"], "dev|finance.approver@sutra.dev");
    assert_eq!(d["decisionReason"], "verified against the bank statement");
    let at: chrono::DateTime<chrono::Utc> = d["decidedAt"].as_str().unwrap().parse().unwrap();
    assert!(at >= before && at <= chrono::Utc::now() + chrono::Duration::seconds(5));
    // (a) only now is the refund applied (refunds payable credited).
    assert_eq!(balance(&pool, inst, "refunds_payable").await, dec("-1200"));
    let row: (
        Option<String>,
        Option<chrono::DateTime<chrono::Utc>>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT decided_by, decided_at, decision_reason FROM finance.refund WHERE id = $1::uuid",
    )
    .bind(&rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.0.as_deref(), Some("dev|finance.approver@sutra.dev"));
    assert!(row.1.is_some());
    assert_eq!(
        row.2.as_deref(),
        Some("verified against the bank statement")
    );
    let hist: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.status_history WHERE aggregate_type = 'refund' \
         AND aggregate_id = $1::uuid AND to_status = 'approved' AND actor = 'dev|finance.approver@sutra.dev'",
    )
    .bind(&rid)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(hist, 1, "status history names the approver");
    let audit = audit_events(&pool, T_REFUND, "finance.refund.approve", &rid).await;
    assert_eq!(audit.len(), 1, "one audit event for the decision");
    assert_eq!(
        audit[0].0.as_deref(),
        Some("dev|finance.approver@sutra.dev")
    );
    assert_eq!(audit[0].1["reason"], "verified against the bank statement");
    assert_eq!(audit[0].1["to"], "approved");

    // A decided refund cannot be decided again.
    call(
        &api,
        &checker,
        "POST",
        &format!("{F}/refunds/{rid}/decision"),
        json!({"decision": "reject", "reason": "again"}),
        Some(ver(&d)),
        StatusCode::CONFLICT,
    )
    .await;
}

// ------------------------------------------------------------------ result batches (exams)

/// (a)–(d) on result approval: validation leaves the batch pending under the default mode; the
/// maker and a user without `exam.result.publish` are refused; the exam controller decides.
#[tokio::test]
async fn result_batch_waits_for_an_independent_exam_controller() {
    let (setup_app, pool) = app(T_RESULT).await;
    let setup_admin = login(&setup_app, ADMIN).await;
    let setup = b6::exam_setup(
        &setup_app,
        &setup_admin,
        &pool,
        T_RESULT,
        "Approval Control Institute",
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
        ExamRegistrationStatus::Locked,
    )
    .await;
    b6::seed_final_marks(
        &pool,
        &setup,
        reg.papers[0].registration_paper_id,
        Some(("30", "40")),
        Some("60"),
    )
    .await;

    // Exams state exactly as the composition root builds it by default (no override).
    let st = sutra_api::app::exams_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    assert_eq!(st.approvals, ApprovalMode::AwaitDecision);
    let t = setup.tenant();
    let sid = setup.session_id;
    let maker = Actor {
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };

    let b = rr::create_batch(&st, t, sid, &maker).await.unwrap();
    let (b, sum) = rr::compute_batch(&st, t, b.id, b.version, &maker)
        .await
        .unwrap();
    assert_eq!(sum.computed, 1);
    let b = rr::validate_batch(&st, t, b.id, b.version, None, &maker)
        .await
        .unwrap();
    // (a) pending: validated, not approved; results not approved; finalising is refused.
    assert_eq!(b.status, ResultBatchStatus::Validated);
    assert!(b.process_instance_id.is_some());
    let status = |rows: Vec<sutra_exams::shared::results::ResultRow>| rows[0].status;
    assert_eq!(
        status(rr::list_results(&st, t, sid, None, false).await.unwrap()),
        ResultStatus::Validated
    );
    assert!(matches!(
        rr::finalise_batch(&st, t, b.id, b.version, &maker).await,
        Err(KernelError::Conflict(_))
    ));
    // (b) at the repository: the maker cannot approve, a system actor cannot approve.
    assert!(matches!(
        rr::decide_batch(&st, t, b.id, b.version, true, None, &maker).await,
        Err(KernelError::Forbidden(_))
    ));
    let system = Actor {
        id: Some("system:auto-approval".into()),
        correlation_id: None,
    };
    assert!(matches!(
        rr::decide_batch(&st, t, b.id, b.version, true, None, &system).await,
        Err(KernelError::Forbidden(_))
    ));

    // Through the default-configured API.
    let api = human_app(T_RESULT).await;
    let admin = login(&api, ADMIN).await;
    let faculty = login(&api, FACULTY).await;
    let other_checker = login(&api, FINANCE_APPROVER).await;
    let controller = login(&api, EXAM_CONTROLLER).await;
    let path = format!("{E}/result-batches/{}/decision", b.id);
    // (b) the maker (admin created / validated the batch) → 403.
    call(
        &api,
        &admin,
        "POST",
        &path,
        json!({"decision": "approve"}),
        Some(b.version),
        StatusCode::FORBIDDEN,
    )
    .await;
    // (c) no exam.result.publish → 403 (faculty; the finance approver).
    for who in [&faculty, &other_checker] {
        call(
            &api,
            who,
            "POST",
            &path,
            json!({"decision": "approve"}),
            Some(b.version),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    assert_eq!(
        status(rr::list_results(&st, t, sid, None, false).await.unwrap()),
        ResultStatus::Validated,
        "nothing applied by the refused attempts"
    );
    let bid = b.id.to_string();
    assert!(
        audit_events(&pool, T_RESULT, "exams.result_batch.approve", &bid)
            .await
            .is_empty()
    );

    // (d) the exam controller approves; recorded with approver, time, reason and an audit event.
    let d = call(
        &api,
        &controller,
        "POST",
        &path,
        json!({"decision": "approve", "reason": "moderation complete"}),
        Some(b.version),
        StatusCode::OK,
    )
    .await;
    assert_eq!(d["status"], "approved", "{d}");
    assert_eq!(
        status(rr::list_results(&st, t, sid, None, false).await.unwrap()),
        ResultStatus::Approved
    );
    let hist: Vec<(Option<String>, Option<String>, chrono::DateTime<chrono::Utc>)> =
        sqlx::query_as(
            "SELECT actor, reason, created_at FROM exams.status_history WHERE aggregate_type = 'result_batch' \
             AND aggregate_id = $1 AND to_status = 'approved'",
        )
        .bind(b.id)
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(hist.len(), 1, "one approval transition: {hist:?}");
    assert_eq!(hist[0].0.as_deref(), Some("dev|exam.controller@sutra.dev"));
    assert_eq!(hist[0].1.as_deref(), Some("moderation complete"));
    let audit = audit_events(&pool, T_RESULT, "exams.result_batch.approve", &bid).await;
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].0.as_deref(), Some("dev|exam.controller@sutra.dev"));
    assert_eq!(audit[0].1["reason"], "moderation complete");
}

// ------------------------------------------------------------------ (e) production refusals

/// A production environment that passes the Part 1 · Stage 3 start-up gate (TLS URLs, sslmode,
/// distinct random keys, non-default secrets — `apps/api/src/config_http.rs`), so these tests
/// isolate the approval rules.
fn production(extra: &[(&'static str, &'static str)]) -> HashMap<&'static str, &'static str> {
    let mut m = HashMap::from([
        ("SUTRA_ENV", "production"),
        (
            "DATABASE_URL",
            "postgres://sutra_api:Zq8w3Lr0Vb6Xn2Ty@pg.internal:5432/sutra?sslmode=verify-full",
        ),
        ("SUTRA_OIDC_ISSUER", "https://idp.example.edu/realms/sutra"),
        (
            "SUTRA_OIDC_REDIRECT_URI",
            "https://erp.example.edu/api/v1/auth/oidc/callback",
        ),
        (
            "SUTRA_OIDC_POST_LOGOUT_REDIRECT_URI",
            "https://erp.example.edu/sign-in",
        ),
        (
            "SUTRA_OIDC_CLIENT_SECRET",
            "unit-test-only-oidc-client-secret",
        ),
        ("SUTRA_PUBLIC_URL", "https://erp.example.edu"),
        ("SUTRA_WEBAUTHN_ORIGINS", "https://erp.example.edu"),
        (
            "SUTRA_CSRF_KEY",
            "3f9a1c7e5b2d8046a1e9c3b7d5f20864e1a3c5b7d9f1e2a4c6b8d0f2e4a6c8b1",
        ),
        (
            "SUTRA_MFA_ENCRYPTION_KEY",
            "c1d2e3f405162738495a6b7c8d9eafb0c1d2e3f405162738495a6b7c8d9eafb1",
        ),
        (
            "SUTRA_AUDIT_ANCHOR_KEY",
            "8e7d6c5b4a39281706f5e4d3c2b1a0998e7d6c5b4a39281706f5e4d3c2b1a092",
        ),
        ("SUTRA_WORKFLOW", "flowable"),
        (
            "SUTRA_FLOWABLE_PASSWORD",
            "unit-test-only-flowable-password",
        ),
        // Part 2 · S4-docs: production refuses to start without S3 document storage.
        ("SUTRA_DOCUMENT_STORAGE", "s3"),
        ("SUTRA_S3_ENDPOINT", "https://objects.example.edu"),
        ("SUTRA_S3_BUCKET", "sutraerp-documents"),
        ("SUTRA_S3_ACCESS_KEY_ID", "sutraerp-api"),
        (
            "SUTRA_S3_SECRET_ACCESS_KEY",
            "unit-test-only-object-storage-secret",
        ),
    ]);
    m.extend(extra.iter().copied());
    m
}

/// (e) The config loader refuses every auto-approving production configuration, and accepts
/// production only with Flowable + human decisions.
#[test]
fn production_config_refuses_noop_workflow_and_auto_approval() {
    let ok = Config::load(&production(&[])).expect("flowable + human is a valid production config");
    assert_eq!(ok.hardening.workflow.approvals, ApprovalMode::AwaitDecision);
    assert!(!ok.hardening.workflow.flowable_auto_approve);
    let ok = Config::load(&production(&[("SUTRA_APPROVAL_MODE", "human")])).unwrap();
    assert_eq!(ok.hardening.workflow.approvals, ApprovalMode::AwaitDecision);

    for (bad, why) in [
        (vec![("SUTRA_APPROVAL_MODE", "auto")], "auto-approval"),
        (vec![("SUTRA_WORKFLOW", "noop")], "noop workflow"),
        (
            vec![("SUTRA_FLOWABLE_AUTO_APPROVE", "true")],
            "Flowable autoApprove",
        ),
        (
            vec![("SUTRA_WORKFLOW", "noop"), ("SUTRA_APPROVAL_MODE", "auto")],
            "noop + auto",
        ),
    ] {
        assert!(
            matches!(
                Config::load(&production(&bad)),
                Err(ConfigError::Forbidden(_))
            ),
            "production must refuse {why}"
        );
    }
    // No SUTRA_WORKFLOW at all = the noop default → refused in production.
    let mut unset = production(&[]);
    unset.remove("SUTRA_WORKFLOW");
    assert!(matches!(
        Config::load(&unset),
        Err(ConfigError::Forbidden(_))
    ));
    // An unknown mode is a configuration error everywhere.
    assert!(matches!(
        Config::load(&production(&[("SUTRA_APPROVAL_MODE", "sometimes")])),
        Err(ConfigError::Invalid { .. })
    ));
    // Development may opt in explicitly (tests / demos); the default stays human.
    let dev = Config::load(&HashMap::from([("SUTRA_APPROVAL_MODE", "auto")])).unwrap();
    assert_eq!(dev.hardening.workflow.approvals, ApprovalMode::AutoApprove);
    let dev = Config::load(&HashMap::<&str, &str>::new()).unwrap();
    assert_eq!(
        dev.hardening.workflow.approvals,
        ApprovalMode::AwaitDecision
    );
}
