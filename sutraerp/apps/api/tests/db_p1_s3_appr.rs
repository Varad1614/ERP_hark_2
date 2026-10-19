//! Part 1 · Stage 3 (S3-appr) — maker-checker regressions on real PostgreSQL for the governed
//! money decisions and the DEF-P1-DEMO-006 eligibility override:
//!
//! * `finance_adjustment_*`, `payroll_adjustment_and_final_settlement_*`, `vendor_invoice_*` —
//!   for each decision: the maker deciding it → 403 (`Forbidden`), a system actor → 403, an
//!   independent authorised checker → OK; the posting / finalising / applying step of a pending
//!   record is refused (409); a content change after approval is refused or creates a new
//!   pending (draft) record that needs a fresh decision.
//! * `enrolment_override_*` (DEF-P1-DEMO-006) — under the default human approval mode a direct
//!   override seat is refused (no seat without a decision); the governed request is decided by a
//!   second administrator (never the requester), seats the student, and a rejection needs a
//!   reason and seats nobody.
//! * `approval_ledger_refuses_system_and_requesters` — the shared ledger guard
//!   (`sutra_persistence::approval::guard_decision_by`) used by the ~85 guarded handlers.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use axum::Router;
use chrono::{Duration, NaiveDate};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use support::b7::{self, dec};
use support::*;
use sutra_api::app::{router, AppContext};
use sutra_api::config::Config;
use sutra_contracts::finance::status::{AdjustmentKind, AdjustmentStatus};
use sutra_contracts::people::status::{EngagementStatus, PayrollRunKind, StaffCategory};
use sutra_contracts::resources::status::ItemKind;
use sutra_kernel::approval::ApprovalMode;
use sutra_kernel::error::KernelError;
use uuid::Uuid;

const T_FADJ: &str = "0192f5a0-7c3e-7000-8000-0000000c5a01";
const T_PAYROLL: &str = "0192f5a0-7c3e-7000-8000-0000000c5a02";
const T_INVOICE: &str = "0192f5a0-7c3e-7000-8000-0000000c5a03";
const T_OVERRIDE: &str = "0192f5a0-7c3e-7000-8000-0000000c5a04";
const T_LEDGER: &str = "0192f5a0-7c3e-7000-8000-0000000c5a05";

const R: &str = "/api/v1/resources";
const A: &str = "/api/v1/academic";
const P: &str = "/api/v1/people";

const ADMIN: &str = "admin@sutra.dev";
const DEAN: &str = "dean@sutra.dev";
const FACULTY: &str = "meera.shah@sutra.dev";
const PROCUREMENT_APPROVER: &str = "procurement.approver@sutra.dev";

const MAKER_ID: &str = "dev|admin@sutra.dev";
const CHECKER_ID: &str = "dev|approver@sutra.dev";
const SYSTEM_ID: &str = "system:auto-approval";

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

fn id(v: &Value) -> String {
    v["id"].as_str().unwrap().to_owned()
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

fn forbidden<T: std::fmt::Debug>(r: Result<T, KernelError>, what: &str) {
    assert!(
        matches!(r, Err(KernelError::Forbidden(_))),
        "{what}: expected 403, got {r:?}"
    );
}

fn refused<T: std::fmt::Debug>(r: Result<T, KernelError>, what: &str) {
    assert!(
        matches!(r, Err(KernelError::Conflict(_))),
        "{what}: expected 409, got {r:?}"
    );
}

// ============================================================================ finance adjustment

#[tokio::test]
async fn finance_adjustment_is_decided_by_an_independent_human_only() {
    use sutra_finance::adjustments::repo as adj;
    use sutra_finance::shared::Actor;
    let (app, pool) = app(T_FADJ).await;
    let admin = login(&app, ADMIN).await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        T_FADJ,
        "Adjustment Control University",
        Some(b4::AARAV),
    )
    .await;
    let mut st = sutra_api::app::finance_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let t = setup.tenant();
    let maker = Actor {
        id: Some(MAKER_ID.into()),
        correlation_id: None,
    };
    let checker = Actor {
        id: Some(CHECKER_ID.into()),
        correlation_id: None,
    };
    let system = Actor {
        id: Some(SYSTEM_ID.into()),
        correlation_id: None,
    };
    let anonymous = Actor {
        id: None,
        correlation_id: None,
    };

    let c = b5::seed_charge(&pool, &setup, "20000.00", "published", None).await;
    let a = adj::create(
        &st,
        t,
        c.charge_id,
        adj::NewAdjustment {
            kind: AdjustmentKind::Concession,
            amount: b5::amount("2500"),
            reason: "Sibling concession",
            origin: "manual",
            supersedes: None,
        },
        &maker,
    )
    .await
    .unwrap();
    let a = adj::submit(&st, t, a.id, a.version, &maker).await.unwrap();
    assert_eq!(a.status, AdjustmentStatus::PendingApproval);
    assert!(a.journal_entry_id.is_none());

    // Self-approval, a system actor and an unidentified actor are refused; nothing posts.
    forbidden(
        adj::decide(&st, t, a.id, a.version, true, None, &maker).await,
        "maker approving their own adjustment",
    );
    forbidden(
        adj::decide(&st, t, a.id, a.version, true, None, &system).await,
        "system actor deciding an adjustment",
    );
    forbidden(
        adj::decide(&st, t, a.id, a.version, true, None, &anonymous).await,
        "unidentified actor deciding an adjustment",
    );
    // Bypass attempts on the pending record: reversing (a posting step) and correcting it.
    refused(
        adj::reverse(&st, t, a.id, a.version, "undo", &maker).await,
        "reversing a pending adjustment",
    );
    assert!(
        adj::correct(
            &st,
            t,
            a.id,
            a.version,
            None,
            b5::amount("9000"),
            "bigger",
            &maker
        )
        .await
        .is_err(),
        "a pending adjustment is not edited behind the checker's back"
    );
    let still = adj::get(&st, t, a.id).await.unwrap();
    assert_eq!(still.status, AdjustmentStatus::PendingApproval);
    assert!(still.journal_entry_id.is_none(), "nothing posted");
    assert_eq!(still.amount, b5::amount("2500"), "amount unchanged");

    // An independent checker approves → posted.
    let a = adj::decide(&st, t, a.id, still.version, true, None, &checker)
        .await
        .unwrap();
    assert_eq!(a.status, AdjustmentStatus::Posted);
    assert_eq!(a.decided_by.as_deref(), Some(CHECKER_ID));
    assert!(a.journal_entry_id.is_some());

    // Changing the amount after approval never edits the approved record: the correction
    // supersedes it and returns a new draft that needs its own submission + decision.
    let successor = adj::correct(
        &st,
        t,
        a.id,
        a.version,
        None,
        b5::amount("4000"),
        "wrong amount",
        &checker,
    )
    .await
    .unwrap();
    assert_eq!(successor.status, AdjustmentStatus::Draft);
    assert_ne!(successor.id, a.id);
    assert!(successor.journal_entry_id.is_none());
    let successor = adj::submit(&st, t, successor.id, successor.version, &maker)
        .await
        .unwrap();
    assert_eq!(successor.status, AdjustmentStatus::PendingApproval);
    // The submitter of the corrected adjustment cannot decide it either.
    forbidden(
        adj::decide(&st, t, successor.id, successor.version, true, None, &maker).await,
        "the submitter of the corrected adjustment",
    );
}

// ============================================================================ payroll

/// A draft payroll configuration (BASIC / DA / HRA / BONUS + PF / PT), completed, submitted
/// (auto-approved on the setup app) and activated.
async fn active_payroll_config(app: &Router, admin: &Login, inst: Uuid) {
    let cfg = call(
        app,
        admin,
        "POST",
        &format!("{P}/institutions/{inst}/payroll-configs"),
        json!({"name": "Payroll 2026", "effectiveFrom": "2026-04-01"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let cid = id(&cfg);
    let mut v = ver(&cfg);
    let comps = [
        json!({"code": "BASIC", "name": "Basic pay", "kind": "earning", "basis": "fixed", "sequence": 10}),
        json!({"code": "DA", "name": "Dearness allowance", "kind": "earning", "basis": "percent_of_basic", "rate": "10", "sequence": 20}),
        json!({"code": "HRA", "name": "House rent allowance", "kind": "earning", "basis": "percent_of_basic", "rate": "40", "sequence": 30}),
        json!({"code": "BONUS", "name": "Bonus", "kind": "earning", "basis": "fixed", "sequence": 40}),
        json!({"code": "PF", "name": "Provident fund", "kind": "deduction", "basis": "statutory", "sequence": 100}),
        json!({"code": "PF_ER", "name": "Employer PF", "kind": "employer_contribution", "basis": "statutory", "sequence": 110}),
        json!({"code": "PT", "name": "Professional tax", "kind": "deduction", "basis": "statutory", "sequence": 120}),
    ];
    for c in comps {
        let b = call(
            app,
            admin,
            "POST",
            &format!("{P}/payroll-configs/{cid}/components"),
            c,
            Some(v),
            StatusCode::OK,
        )
        .await;
        v = ver(&b);
    }
    let b = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/statutory-rules"),
        json!({"kind": "pf", "employeeRate": "12", "employerRate": "12", "wageCeiling": "15000",
               "employeeComponentCode": "PF", "employerComponentCode": "PF_ER"}),
        Some(v),
        StatusCode::OK,
    )
    .await;
    let b = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/statutory-rules"),
        json!({"kind": "professional_tax", "employeeComponentCode": "PT",
               "slabs": [{"upTo": "15000", "amount": "0"}, {"upTo": null, "amount": "200"}]}),
        Some(ver(&b)),
        StatusCode::OK,
    )
    .await;
    let b = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/submit"),
        json!({}),
        Some(ver(&b)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(b["status"], "approved", "{b}");
    let b = call(
        app,
        admin,
        "POST",
        &format!("{P}/payroll-configs/{cid}/activate"),
        json!({}),
        Some(ver(&b)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(b["status"], "active", "{b}");
}

#[tokio::test]
async fn payroll_adjustment_and_final_settlement_need_an_independent_approver() {
    use sutra_people::payroll::adjust::{self, AdjustmentLineInput};
    use sutra_people::payroll::common::Cmd;
    use sutra_people::payroll::{runs, settlement};
    use sutra_people::shared::Actor;
    let (app, pool) = app(T_PAYROLL).await;
    let admin = login(&app, ADMIN).await;
    let setup = b7::people_setup(&app, &admin, &pool, T_PAYROLL, "Payroll Control College").await;
    let inst = setup.institution().as_uuid();
    let asha = b7::seed_staff(
        &pool,
        &setup,
        "Asha",
        None,
        StaffCategory::NonTeaching,
        Some(EngagementStatus::Active),
    )
    .await;
    let ravi = b7::seed_staff(
        &pool,
        &setup,
        "Ravi",
        None,
        StaffCategory::Teaching,
        Some(EngagementStatus::Closed),
    )
    .await;
    active_payroll_config(&app, &admin, inst).await;
    let structure = call(
        &app,
        &admin,
        "POST",
        &format!("{P}/institutions/{inst}/salary-structures"),
        json!({"code": "AP-1", "name": "Assistant Professor 1", "grade": "AP-1",
               "lines": [{"componentCode": "BASIC", "amount": "30000"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    // Ravi's engagement is already closed (last working date 2026-11-15): his assignment
    // predates the exit.
    sqlx::query(
        "INSERT INTO people.salary_assignment (tenant_id, institution_id, staff_id, engagement_id, \
         structure_id, effective_from) VALUES ($1, $2, $3, $4, $5, DATE '2026-06-01')",
    )
    .bind(setup.tenant().as_uuid())
    .bind(inst)
    .bind(ravi.staff_id.as_uuid())
    .bind(ravi.engagement_id.unwrap().as_uuid())
    .bind(Uuid::parse_str(&id(&structure)).unwrap())
    .execute(&pool)
    .await
    .unwrap();
    let mut dec_period = Uuid::nil();
    for month in [11, 12] {
        let period = call(
            &app,
            &admin,
            "POST",
            &format!("{P}/institutions/{inst}/payroll-periods"),
            json!({"year": 2026, "month": month}),
            None,
            StatusCode::CREATED,
        )
        .await;
        dec_period = Uuid::parse_str(&id(&period)).unwrap();
    }

    let mut st = sutra_api::app::people_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();
    st.approvals = ApprovalMode::AwaitDecision;
    let (maker, checker, system) = (
        Actor {
            id: Some(MAKER_ID.into()),
            correlation_id: None,
        },
        Actor {
            id: Some(CHECKER_ID.into()),
            correlation_id: None,
        },
        Actor {
            id: Some(SYSTEM_ID.into()),
            correlation_id: None,
        },
    );
    let t = setup.tenant();
    let c = Cmd {
        st: &st,
        tenant: t,
        actor: &maker,
    };
    let a = Cmd {
        st: &st,
        tenant: t,
        actor: &checker,
    };
    let sys = Cmd {
        st: &st,
        tenant: t,
        actor: &system,
    };

    // ---- a finalised off-cycle result to adjust (run decided by the independent checker).
    let run = runs::create_run(
        &c,
        dec_period,
        runs::NewRun {
            kind: PayrollRunKind::OffCycle,
            description: Some("Festival bonus".into()),
            items: vec![runs::OffCycleItem {
                engagement_id: asha.engagement_id.unwrap().as_uuid(),
                component_code: "BONUS".into(),
                amount: dec("5000"),
                note: None,
            }],
        },
    )
    .await
    .unwrap();
    let run = runs::calculate_run(&c, run.id, run.version).await.unwrap();
    let run = runs::validate_run(&c, run.id, run.version).await.unwrap();
    let run = runs::submit_run(&c, run.id, run.version, None)
        .await
        .unwrap();
    forbidden(
        runs::decide_run(&c, run.id, run.version, true, None).await,
        "maker approving their own payroll run",
    );
    forbidden(
        runs::decide_run(&sys, run.id, run.version, true, None).await,
        "system actor approving a payroll run",
    );
    let run = runs::decide_run(&a, run.id, run.version, true, None)
        .await
        .unwrap();
    let run = runs::finalise_run(&a, run.id, run.version).await.unwrap();
    let (results, _) = runs::list_results(&st, t, run.id, false, 50, None)
        .await
        .unwrap();
    let result_id = results[0].id;

    // ---- payroll adjustment.
    let adj = adjust::create_adjustment(
        &c,
        result_id,
        Some("Bonus under-paid"),
        vec![AdjustmentLineInput {
            code: "BONUS".into(),
            kind: None,
            name: None,
            amount: dec("500"),
        }],
    )
    .await
    .unwrap();
    let adj = adjust::validate_adjustment(&c, adj.id, adj.version)
        .await
        .unwrap();
    assert_eq!(adj.status.as_str(), "pending_approval");
    forbidden(
        adjust::decide_adjustment(&c, adj.id, adj.version, true, None).await,
        "maker approving their own payroll adjustment",
    );
    forbidden(
        adjust::decide_adjustment(&sys, adj.id, adj.version, true, None).await,
        "system actor approving a payroll adjustment",
    );
    // Applying (posting the delta) a pending adjustment is refused.
    assert!(
        adjust::apply_adjustment(&c, adj.id, adj.version)
            .await
            .is_err(),
        "a pending payroll adjustment is never applied"
    );
    let still = adjust::get_adjustment(&st, t, adj.id).await.unwrap();
    assert_eq!(still.status.as_str(), "pending_approval");
    let adj = adjust::decide_adjustment(&a, adj.id, still.version, true, Some("verified"))
        .await
        .unwrap();
    assert_eq!(adj.status.as_str(), "approved");
    // A decided adjustment is not decided again (content is fixed once approved).
    assert!(
        adjust::decide_adjustment(&a, adj.id, adj.version, false, Some("again"))
            .await
            .is_err()
    );

    // ---- final settlement of Ravi.
    let fnf =
        settlement::create_settlement(&c, ravi.engagement_id.unwrap().as_uuid(), vec![], None)
            .await
            .unwrap();
    let fnf = settlement::calculate_settlement(&c, fnf.id, fnf.version)
        .await
        .unwrap();
    assert_eq!(fnf.status.as_str(), "pending_approval");
    forbidden(
        settlement::decide_settlement(&c, fnf.id, fnf.version, true, None).await,
        "preparer approving their own final settlement",
    );
    forbidden(
        settlement::decide_settlement(&sys, fnf.id, fnf.version, true, None).await,
        "system actor approving a final settlement",
    );
    // Completing (posting + finalising) a pending settlement is refused; its run cannot be
    // finalised around the settlement either.
    assert!(
        settlement::complete_settlement(&c, fnf.id, fnf.version)
            .await
            .is_err(),
        "a pending settlement never completes"
    );
    let still = settlement::get_settlement(&st, t, fnf.id).await.unwrap();
    assert_eq!(still.status.as_str(), "pending_approval");
    let fnf_run = runs::get_run(&st, t, still.run_id.unwrap()).await.unwrap();
    refused(
        runs::finalise_run(&a, fnf_run.id, fnf_run.version).await,
        "finalising the settlement's run directly",
    );
    let fnf = settlement::decide_settlement(&a, fnf.id, still.version, true, None)
        .await
        .unwrap();
    assert_eq!(fnf.status.as_str(), "approved");
    // Recalculating (changing the amounts of) an approved settlement is refused.
    assert!(
        settlement::calculate_settlement(&c, fnf.id, fnf.version)
            .await
            .is_err(),
        "an approved settlement's amounts are frozen"
    );
}

// ============================================================================ vendor invoice

#[tokio::test]
async fn vendor_invoice_posts_only_after_an_independent_approval() {
    let (setup_app, pool) = app(T_INVOICE).await;
    let admin = login(&setup_app, ADMIN).await;
    let setup = b8::campus_setup(
        &setup_app,
        &admin,
        &pool,
        T_INVOICE,
        "Invoice Control University",
    )
    .await;
    let inst = setup.institution().as_uuid().to_string();
    let paper = b8::seed_item(&pool, &setup, "PAPER-A4", ItemKind::Consumable, None).await;
    let store = b8::seed_store(&pool, &setup, "CENTRAL", None).await;
    // Supplier + released requisition + issued order + accepted receipt on the setup app.
    let s = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/suppliers"),
        json!({"legalName": "Acme Traders", "pan": "ABCDE1234F", "email": "sales@example.in",
            "bankAccountName": "Acme Traders", "bankAccountNumber": "001234567890", "bankIfsc": "HDFC0001234"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let sup = id(&s);
    let s = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sup}/register"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    let s = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/suppliers/{sup}/submit"),
        json!({}),
        Some(ver(&s)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(s["status"], "active", "{s}");
    let req = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/requisitions"),
        json!({"title": "Department purchase", "justification": "Term supplies",
               "lines": [{"itemId": paper, "quantity": "100", "estimatedUnitCost": "100.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let req = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/requisitions/{}/submit", id(&req)),
        json!({}),
        Some(ver(&req)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(req["status"], "released", "{req}");
    let po = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/purchase-orders"),
        json!({"requisitionId": id(&req), "supplierId": sup, "deliveryLocationId": store}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let po_id = id(&po);
    call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/issue"),
        json!({}),
        Some(ver(&po)),
        StatusCode::OK,
    )
    .await;
    let day = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
    let grn = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/purchase-orders/{po_id}/receipts"),
        json!({"receivedOn": day.to_string(), "deliveryNote": "DN-1", "lines": [{"lineNo": 1, "quantity": "100"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/goods-receipts/{}/accept", id(&grn)),
        json!({}),
        Some(ver(&grn)),
        StatusCode::OK,
    )
    .await;
    let inv = call(
        &setup_app,
        &admin,
        "POST",
        &format!("{R}/institutions/{inst}/vendor-invoices"),
        json!({"purchaseOrderId": po_id, "supplierInvoiceNumber": "ACME-1", "invoiceDate": day.to_string(),
            "dueDate": (day + Duration::days(30)).to_string(),
            "lines": [{"lineNo": 1, "quantity": "100", "unitPrice": "100.00"}]}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let inv_id = id(&inv);

    // ---- the governed part, through the default-configured API.
    let api = human_app(T_INVOICE).await;
    let maker = login(&api, ADMIN).await;
    let checker = login(&api, PROCUREMENT_APPROVER).await;
    let faculty = login(&api, FACULTY).await;
    let inv = call(
        &api,
        &maker,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/match"),
        json!({}),
        Some(ver(&inv)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(inv["status"], "matched", "{inv}");
    assert_eq!(inv["approvalPending"], true, "{inv}");

    // Posting a matched but undecided invoice is refused (no AP journal).
    let (s, _, b) = send(
        &api,
        write(
            &maker,
            "POST",
            &format!("{R}/vendor-invoices/{inv_id}/post"),
            json!({}),
            Some(ver(&inv)),
        ),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a pending invoice never posts: {b}"
    );
    // The maker (recorder + matcher) and a user without resources.approve → 403.
    for who in [&maker, &faculty] {
        let (s, _, b) = send(
            &api,
            write(
                who,
                "POST",
                &format!("{R}/vendor-invoices/{inv_id}/decision"),
                json!({"decision": "approve"}),
                Some(ver(&inv)),
            ),
        )
        .await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{b}");
    }
    // A system actor is never the decider (the same check the handler runs).
    let mut conn = pool.acquire().await.unwrap();
    forbidden(
        sutra_resources::shared::approval::ensure_independent_decider(
            &mut conn,
            setup.tenant(),
            "vendor_invoice",
            Uuid::parse_str(&inv_id).unwrap(),
            &["matched"],
            &[],
            Some(SYSTEM_ID),
            "vendor invoice",
        )
        .await,
        "system actor deciding a vendor invoice",
    );
    drop(conn);
    let still = read(&api, &maker, &format!("{R}/vendor-invoices/{inv_id}")).await;
    assert_eq!(still["status"], "matched");
    assert!(still["journalEntryId"].is_null());

    // The independent procurement approver approves; then (and only then) the maker posts.
    let inv = call(
        &api,
        &checker,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/decision"),
        json!({"decision": "approve", "reason": "three-way match verified"}),
        Some(ver(&still)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(inv["status"], "approved", "{inv}");
    assert_eq!(inv["approvalPending"], false);
    // Re-matching (re-pricing) an approved invoice is refused: its content is frozen.
    call(
        &api,
        &maker,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/match"),
        json!({}),
        Some(ver(&inv)),
        StatusCode::CONFLICT,
    )
    .await;
    let inv = call(
        &api,
        &maker,
        "POST",
        &format!("{R}/vendor-invoices/{inv_id}/post"),
        json!({}),
        Some(ver(&inv)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(inv["status"], "posted", "{inv}");
    assert!(inv["journalEntryId"].is_string());
}

// ============================================================================ DEF-P1-DEMO-006

#[tokio::test]
async fn enrolment_override_is_decided_by_a_second_administrator() {
    let (setup_app, pool) = app(T_OVERRIDE).await;
    let setup_admin = login(&setup_app, ADMIN).await;
    let setup = b4::academic_setup(
        &setup_app,
        &setup_admin,
        &pool,
        T_OVERRIDE,
        "Override Control University",
    )
    .await;
    let off = b4::seed_offering(&pool, &setup, "CS101", "enrollment_open").await;
    let section = off.section_id;
    // Unregistered students: seating them needs an eligibility override.
    let chirag = b4::seed_student(&pool, &setup, "Chirag", None).await;
    let dina = b4::seed_student(&pool, &setup, "Dina", None).await;

    let api = human_app(T_OVERRIDE).await;
    let maker = login(&api, ADMIN).await;
    let dean = login(&api, DEAN).await;
    let faculty = login(&api, FACULTY).await;

    // A direct override seat under human approval is refused (it would need a decision).
    call(
        &api,
        &maker,
        "POST",
        &format!("{A}/sections/{section}/enrolments"),
        json!({"studentId": chirag.student_id, "overrideEligibility": true, "reason": "Dean approved"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    let seats = |pool: sqlx::PgPool, student: Uuid| async move {
        sqlx::query_scalar::<_, i64>(
            "SELECT count(*) FROM academic.section_enrolment WHERE section_id = $1 AND student_id = $2",
        )
        .bind(section)
        .bind(student)
        .fetch_one(&pool)
        .await
        .unwrap()
    };
    assert_eq!(seats(pool.clone(), chirag.student_id).await, 0);

    // The governed request: pending until an independent administrator decides it.
    let r = call(
        &api,
        &maker,
        "POST",
        &format!("{A}/sections/{section}/enrolment-overrides"),
        json!({"studentId": chirag.student_id, "reason": "Prerequisite completed elsewhere"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid = id(&r);
    assert_eq!(r["status"], "pending", "{r}");
    assert!(r["requestedBy"].as_str().unwrap().ends_with(ADMIN));
    // A second request for the same student while one is pending → 409.
    call(
        &api,
        &maker,
        "POST",
        &format!("{A}/sections/{section}/enrolment-overrides"),
        json!({"studentId": chirag.student_id, "reason": "again"}),
        None,
        StatusCode::CONFLICT,
    )
    .await;
    // The requester and a user without academic.offering.decide → 403; still no seat.
    for who in [&maker, &faculty] {
        call(
            &api,
            who,
            "POST",
            &format!("{A}/enrolment-overrides/{rid}/decision"),
            json!({"decision": "approve"}),
            Some(ver(&r)),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    assert_eq!(seats(pool.clone(), chirag.student_id).await, 0);
    let r = read(&api, &dean, &format!("{A}/enrolment-overrides/{rid}")).await;
    assert_eq!(r["status"], "pending");

    // The dean (second administrator) approves → the student is seated by the decision.
    let r = call(
        &api,
        &dean,
        "POST",
        &format!("{A}/enrolment-overrides/{rid}/decision"),
        json!({"decision": "approve", "reason": "evidence checked"}),
        Some(ver(&r)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r["status"], "approved", "{r}");
    assert!(r["decidedBy"].as_str().unwrap().ends_with(DEAN));
    assert!(r["enrolmentId"].is_string());
    assert_eq!(seats(pool.clone(), chirag.student_id).await, 1);
    let origin: String = sqlx::query_scalar(
        "SELECT origin FROM academic.section_enrolment WHERE section_id = $1 AND student_id = $2",
    )
    .bind(section)
    .bind(chirag.student_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(origin, "override");
    // Deciding again → 409 (nothing pending).
    call(
        &api,
        &dean,
        "POST",
        &format!("{A}/enrolment-overrides/{rid}/decision"),
        json!({"decision": "approve"}),
        Some(ver(&r)),
        StatusCode::CONFLICT,
    )
    .await;

    // Rejection needs a reason and seats nobody.
    let r2 = call(
        &api,
        &maker,
        "POST",
        &format!("{A}/sections/{section}/enrolment-overrides"),
        json!({"studentId": dina.student_id, "reason": "Late transfer"}),
        None,
        StatusCode::CREATED,
    )
    .await;
    let rid2 = id(&r2);
    call(
        &api,
        &dean,
        "POST",
        &format!("{A}/enrolment-overrides/{rid2}/decision"),
        json!({"decision": "reject"}),
        Some(ver(&r2)),
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    let r2 = call(
        &api,
        &dean,
        "POST",
        &format!("{A}/enrolment-overrides/{rid2}/decision"),
        json!({"decision": "reject", "reason": "no evidence"}),
        Some(ver(&r2)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(r2["status"], "rejected", "{r2}");
    assert_eq!(seats(pool.clone(), dina.student_id).await, 0);
}

// ============================================================================ shared ledger guard

#[tokio::test]
async fn approval_ledger_refuses_system_and_requesters() {
    use sutra_persistence::approval::{guard_decision_by, record_request};
    let pool = owner_pool().await;
    let tenant = Uuid::parse_str(T_LEDGER).unwrap();
    let subject = Uuid::now_v7();
    record_request(
        &pool,
        tenant,
        "test.approval",
        &format!("thing:{subject}:v1"),
        "proc-1",
        Some(MAKER_ID),
    )
    .await
    .unwrap();
    // A concurrent second submission of the same version by another user: both are makers.
    record_request(
        &pool,
        tenant,
        "test.approval",
        &format!("thing:{subject}:v1"),
        "proc-2",
        Some("dev|other@sutra.dev"),
    )
    .await
    .unwrap();
    for decider in [
        Some(MAKER_ID),
        Some("dev|other@sutra.dev"),
        Some(SYSTEM_ID),
        Some("system"),
        None,
    ] {
        forbidden(
            guard_decision_by(&pool, tenant, decider, subject, &[], "thing").await,
            &format!("decider {decider:?}"),
        );
    }
    guard_decision_by(&pool, tenant, Some(CHECKER_ID), subject, &[], "thing")
        .await
        .expect("an independent checker decides");
    // A creator column passed by the caller counts as a maker too.
    forbidden(
        guard_decision_by(
            &pool,
            tenant,
            Some(CHECKER_ID),
            subject,
            &[Some(CHECKER_ID)],
            "thing",
        )
        .await,
        "creator column",
    );
    // A new version re-submitted by the checker: the latest version's requester is the maker.
    record_request(
        &pool,
        tenant,
        "test.approval",
        &format!("thing:{subject}:v2"),
        "proc-3",
        Some(CHECKER_ID),
    )
    .await
    .unwrap();
    forbidden(
        guard_decision_by(&pool, tenant, Some(CHECKER_ID), subject, &[], "thing").await,
        "requester of the latest version",
    );
    guard_decision_by(&pool, tenant, Some(MAKER_ID), subject, &[], "thing")
        .await
        .expect("the earlier version's maker may decide the new version");
    // The ledger is append-only.
    let upd =
        sqlx::query("UPDATE platform.approval_request SET requested_by = 'x' WHERE tenant_id = $1")
            .bind(tenant)
            .execute(&pool)
            .await;
    assert!(upd.is_err(), "approval requests cannot be rewritten");
}
