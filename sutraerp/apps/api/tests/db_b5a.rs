//! Batch 5 slice 5A (Finance: charges, adjustments, aid, configuration) against PostgreSQL.
//!
//! * `charges_structures_adjustments_and_configuration` — L3/L4-F-001 … F-006 (fee heads,
//!   versioned fee structures → approval → publication → charge generation with the publication
//!   posting, manual charge validate / publish, instalments, governed revision, disputes,
//!   suspend / resume / cancel, settlement closure, archival), F-020 … F-024 (credit / debit
//!   notes, write-off, reversal, corrected successor) and F-084 / F-085 (configuration and rule
//!   versions with rollback), plus the receivable sub-ledger invariant.
//! * `financial_aid_from_handed_off_decisions` — F-062 … F-066 through the real
//!   `AidDecisionSource` (Domain C decisions seeded in 3C's tables).
//! * `await_decision_keeps_governed_steps_pending` — WorkflowPort `AwaitDecision` (contract §9).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde_json::{json, Value};
use std::sync::Arc;
use support::b5::amount;
use support::*;
use sutra_contracts::finance::ids::{ChargeId, FeeStructureId};
use sutra_contracts::finance::status::{AdjustmentKind, AdjustmentStatus, FeeStructureStatus};
use sutra_finance::shared::approval::ApprovalMode;
use sutra_finance::shared::Actor;
use sutra_kernel::error::KernelError;
use uuid::Uuid;

const TENANT_A: &str = "0192f5a0-7c3e-7000-8000-0000000b5a01";
const TENANT_B: &str = "0192f5a0-7c3e-7000-8000-0000000b5a02";
const TENANT_C: &str = "0192f5a0-7c3e-7000-8000-0000000b5a03";

async fn call(
    app: &axum::Router,
    who: &Login,
    method: &str,
    path: &str,
    body: Value,
    if_match: Option<i64>,
) -> (StatusCode, Value) {
    let (s, _, b) = send(app, write(who, method, path, body, if_match)).await;
    (s, b)
}

async fn read(app: &axum::Router, who: &Login, path: &str) -> (StatusCode, Value) {
    let (s, _, b) = send(app, get(who, path)).await;
    (s, b)
}

fn v(body: &Value) -> i64 {
    body["version"].as_i64().unwrap()
}

/// Receivable sub-ledger balance of a party account (Dr − Cr on student receivables).
async fn ar(pool: &sqlx::PgPool, setup: &b5::FinanceSetup) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(l.debit - l.credit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE a.institution_id = $1 AND a.system_key = 'student_receivables' AND l.student_account_id = $2",
    )
    .bind(setup.institution().as_uuid())
    .bind(setup.student_account_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Σ outstanding of the charges that are recognised in the ledger (published and not
/// cancelled); must equal [`ar`].
async fn recognised_outstanding(pool: &sqlx::PgPool, setup: &b5::FinanceSetup) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(outstanding_amount), 0) FROM finance.charge WHERE student_account_id = $1 \
         AND published_at IS NOT NULL AND status <> 'cancelled'",
    )
    .bind(setup.student_account_id.as_uuid())
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Balance of a seeded system account (Dr − Cr).
async fn account_balance(pool: &sqlx::PgPool, setup: &b5::FinanceSetup, key: &str) -> Decimal {
    sqlx::query_scalar(
        "SELECT COALESCE(sum(l.debit - l.credit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE a.institution_id = $1 AND a.system_key = $2",
    )
    .bind(setup.institution().as_uuid())
    .bind(key)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn outbox_count(pool: &sqlx::PgPool, event_type: &str, subject: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = $1 AND subject = $2",
    )
    .bind(event_type)
    .bind(subject)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn d(m: u32, day: u32) -> String {
    NaiveDate::from_ymd_opt(2026, m, day).unwrap().to_string()
}

#[tokio::test]
async fn charges_structures_adjustments_and_configuration() {
    let (app, pool) = app(TENANT_A).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        TENANT_A,
        "Charges University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().to_string();
    let base = format!("/api/v1/finance/institutions/{inst}");

    // ---- F-001 fee heads: category → default income account; duplicates refused.
    let (s, hostel) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/fee-heads"),
        json!({"code": "hostel", "name": "Hostel fee", "category": "hostel"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{hostel}");
    assert_eq!(hostel["code"], "HOSTEL");
    assert_eq!(hostel["incomeAccountCode"], "4900");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/fee-heads"),
        json!({"code": "HOSTEL", "name": "Again", "category": "hostel"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, heads) = read(&app, &admin, &format!("{base}/fee-heads")).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(heads["items"].as_array().unwrap().len(), 2);
    let tuition = setup.fee_head_id.to_string();
    let hostel_id = hostel["id"].as_str().unwrap().to_owned();

    // ---- F-001 fee structure: draft → submit (auto-approved) → publish → generate charges.
    let year = setup.delivery.base.academic_year_id;
    let (s, fs) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/fee-structures"),
        json!({
        "code": "BTECH-Y1", "name": "B.Tech year 1", "academicYearId": year,
        "academicTermId": setup.delivery.term_id, "dueDate": d(7, 15),
        "lines": [{"feeHeadId": tuition, "description": "Tuition", "amount": "100000.00"},
                  {"feeHeadId": hostel_id, "description": "Hostel", "amount": "25000"}]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{fs}");
    assert_eq!(
        (fs["versionNo"].as_i64(), fs["totalAmount"].as_str()),
        (Some(1), Some("125000.00"))
    );
    let fs_id = fs["id"].as_str().unwrap().to_owned();
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/fee-structures/{fs_id}/publish"),
        json!({}),
        Some(v(&fs)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "approval first");
    let (s, fs) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/fee-structures/{fs_id}/submit"),
        json!({}),
        Some(v(&fs)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{fs}");
    assert_eq!(fs["status"], "approved");
    assert!(fs["processInstanceId"].is_string());
    let (s, fs) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/fee-structures/{fs_id}/publish"),
        json!({}),
        Some(v(&fs)),
    )
    .await;
    assert_eq!(
        (s, fs["status"].as_str()),
        (StatusCode::OK, Some("published")),
        "{fs}"
    );
    let student = setup.student.student_id;
    let (s, gen) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/fee-structures/{fs_id}/generate-charges"),
        json!({"studentIds": [student]}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{gen}");
    assert_eq!(gen["created"].as_array().unwrap().len(), 1);
    assert_eq!(gen["created"][0]["status"], "published");
    let charge = gen["created"][0]["chargeId"].as_str().unwrap().to_owned();
    let (_, again) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/fee-structures/{fs_id}/generate-charges"),
        json!({"studentIds": [student]}),
        None,
    )
    .await;
    assert_eq!(
        again["skipped"].as_array().unwrap().len(),
        1,
        "idempotent per student: {again}"
    );
    // Publication posting: Dr receivables 125000 / Cr 4100 100000 + Cr 4900 25000.
    let (s, c) = read(&app, &admin, &format!("/api/v1/finance/charges/{charge}")).await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(
        (c["status"].as_str(), c["sourceType"].as_str()),
        (Some("published"), Some("fee_structure"))
    );
    assert_eq!(c["lines"].as_array().unwrap().len(), 2);
    let je: Uuid = c["journalEntryId"].as_str().unwrap().parse().unwrap();
    let lines: Vec<(String, Decimal, Decimal)> = sqlx::query_as(
        "SELECT a.code, l.debit, l.credit FROM finance.journal_line l JOIN finance.ledger_account a \
         ON a.id = l.ledger_account_id WHERE l.journal_entry_id = $1 ORDER BY l.line_no",
    ).bind(je).fetch_all(&pool).await.unwrap();
    assert_eq!(
        lines,
        vec![
            ("1300".into(), amount("125000"), Decimal::ZERO),
            ("4100".into(), Decimal::ZERO, amount("100000")),
            ("4900".into(), Decimal::ZERO, amount("25000")),
        ]
    );
    assert_eq!(
        outbox_count(&pool, "sutraerp.finance.charge.published", &charge).await,
        1
    );
    let subject: Option<String> = sqlx::query_scalar(
        "SELECT envelope->'data'->>'studentSubjectRef' FROM platform.outbox WHERE event_type = 'sutraerp.finance.charge.published' AND subject = $1",
    ).bind(&charge).fetch_one(&pool).await.unwrap();
    assert_eq!(subject.as_deref(), Some(b4::AARAV));
    // Version 2 supersedes version 1 on publication.
    let (_, fs2) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/fee-structures"),
        json!({
        "code": "BTECH-Y1", "name": "B.Tech year 1 (revised)",
        "lines": [{"feeHeadId": tuition, "amount": "110000.00"}]}),
        None,
    )
    .await;
    assert_eq!(fs2["versionNo"], 2);
    let fs2_id = fs2["id"].as_str().unwrap().to_owned();
    let (_, fs2) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/fee-structures/{fs2_id}/submit"),
        json!({}),
        Some(v(&fs2)),
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/fee-structures/{fs2_id}/publish"),
        json!({}),
        Some(v(&fs2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (_, fs1) = read(
        &app,
        &admin,
        &format!("/api/v1/finance/fee-structures/{fs_id}"),
    )
    .await;
    assert_eq!(fs1["status"], "superseded");
    assert_eq!(fs1["supersededById"], fs2_id.as_str());
    assert_eq!(fs1["chargesGenerated"], 1);

    // ---- F-001 manual charge: draft (invisible to the student) → validate → publish.
    let (s, fine) = call(&app, &admin, "POST", &format!("{base}/charges"), json!({
        "studentId": student, "description": "Library fine", "sourceType": "fine", "dueDate": d(8, 1),
        "lines": [{"feeHeadId": tuition, "description": "Late return", "amount": "500.00"}]}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{fine}");
    assert_eq!(fine["status"], "draft");
    let fine_id = fine["id"].as_str().unwrap().to_owned();
    let (s, _) = call(&app, &admin, "POST", &format!("{base}/charges"), json!({
        "studentId": student, "description": "Bad", "lines": [{"feeHeadId": tuition, "amount": "1.005"}]}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "three decimals");
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let (s, mine) = read(&app, &aarav, "/api/v1/finance/me/charges").await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(
        mine["items"].as_array().unwrap().len(),
        1,
        "draft hidden: {mine}"
    );
    let (s, _) = read(&app, &aarav, &format!("/api/v1/finance/charges/{fine_id}")).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "students never see drafts");
    let (s, fine) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/validate"),
        json!({}),
        Some(v(&fine)),
    )
    .await;
    assert_eq!(
        (s, fine["status"].as_str()),
        (StatusCode::OK, Some("validated")),
        "{fine}"
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/publish"),
        json!({}),
        Some(v(&fine) - 1),
    )
    .await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    let (s, fine) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/publish"),
        json!({}),
        Some(v(&fine)),
    )
    .await;
    assert_eq!(
        (s, fine["status"].as_str()),
        (StatusCode::OK, Some("published")),
        "{fine}"
    );
    assert!(fine["journalEntryId"].is_string());

    // ---- F-005 instalments: equal split (remainder last), explicit schedule must add up.
    let (s, sched) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{charge}/instalments"),
        json!({"count": 3, "firstDueDate": d(7, 15)}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{sched}");
    let amounts: Vec<&str> = sched["instalments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["amount"].as_str().unwrap())
        .collect();
    assert_eq!(amounts, vec!["41666.67", "41666.67", "41666.66"]);
    let (s, _) = call(&app, &admin, "POST", &format!("/api/v1/finance/charges/{charge}/instalments"),
        json!({"instalments": [{"dueDate": d(7, 15), "amount": "100000"}, {"dueDate": d(8, 15), "amount": "1"}]}), None).await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, sched) = call(&app, &admin, "POST", &format!("/api/v1/finance/charges/{charge}/instalments"),
        json!({"instalments": [{"dueDate": d(7, 15), "amount": "75000"}, {"dueDate": d(9, 15), "amount": "50000"}]}), None).await;
    assert_eq!(s, StatusCode::CREATED, "{sched}");
    assert_eq!(
        sched["instalments"].as_array().unwrap().len(),
        2,
        "the plan is replaced"
    );
    let second = sched["instalments"][1].clone();
    let (s, moved) = call(
        &app,
        &admin,
        "PATCH",
        &format!(
            "/api/v1/finance/charges/{charge}/instalments/{}",
            second["id"].as_str().unwrap()
        ),
        json!({"dueDate": d(10, 15)}),
        Some(v(&second)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{moved}");
    assert_eq!(moved["instalments"][1]["dueDate"], d(10, 15));

    // ---- F-002 governed revision: due date in place; amount → posted credit note.
    let (_, c) = read(&app, &admin, &format!("/api/v1/finance/charges/{charge}")).await;
    let (s, c) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{charge}/revise"),
        json!({"reason": "Hostel discount agreed", "newAmount": "120000.00", "dueDate": d(7, 31)}),
        Some(v(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{c}");
    assert_eq!(
        (
            c["adjustmentAmount"].as_str(),
            c["outstandingAmount"].as_str()
        ),
        (Some("-5000.00"), Some("120000.00"))
    );
    assert_eq!(c["dueDate"], d(7, 31));
    let (_, revs) = read(
        &app,
        &admin,
        &format!("/api/v1/finance/charges/{charge}/revisions"),
    )
    .await;
    assert_eq!(revs["items"].as_array().unwrap().len(), 1);
    let rev_adj = revs["items"][0]["adjustmentId"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_, ra) = read(
        &app,
        &admin,
        &format!("/api/v1/finance/adjustments/{rev_adj}"),
    )
    .await;
    assert_eq!(
        (
            ra["kind"].as_str(),
            ra["status"].as_str(),
            ra["origin"].as_str()
        ),
        (Some("credit_note"), Some("posted"), Some("charge_revision"))
    );
    assert_eq!(
        outbox_count(&pool, "sutraerp.finance.charge.revised", &charge).await,
        1
    );

    // ---- F-004 dispute raised by the student → upheld with a credit; another rejected.
    let (s, dsp) = call(
        &app,
        &aarav,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/disputes"),
        json!({"reason": "Book was returned on time"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{dsp}");
    assert_eq!(
        (dsp["status"].as_str(), dsp["raisedByStudent"].as_bool()),
        (Some("raised"), Some(true))
    );
    let (_, f) = read(&app, &admin, &format!("/api/v1/finance/charges/{fine_id}")).await;
    assert_eq!(f["status"], "disputed");
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!(
            "/api/v1/finance/disputes/{}/decision",
            dsp["id"].as_str().unwrap()
        ),
        json!({"decision": "approve"}),
        Some(v(&dsp)),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students do not decide");
    let (s, dsp) = call(&app, &admin, "POST", &format!("/api/v1/finance/disputes/{}/decision", dsp["id"].as_str().unwrap()),
        json!({"decision": "approve", "resolution": "credit", "creditAmount": "200.00", "reason": "Partly late"}), Some(v(&dsp))).await;
    assert_eq!(s, StatusCode::OK, "{dsp}");
    assert_eq!(
        (dsp["status"].as_str(), dsp["resolution"].as_str()),
        (Some("upheld"), Some("credit"))
    );
    let (_, f) = read(&app, &admin, &format!("/api/v1/finance/charges/{fine_id}")).await;
    assert_eq!(
        (f["status"].as_str(), f["outstandingAmount"].as_str()),
        (Some("active"), Some("300.00")),
        "{f}"
    );
    let (_, dsp2) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/disputes"),
        json!({"reason": "Second look"}),
        None,
    )
    .await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!(
            "/api/v1/finance/disputes/{}/decision",
            dsp2["id"].as_str().unwrap()
        ),
        json!({"decision": "reject"}),
        Some(v(&dsp2)),
    )
    .await;
    assert_eq!(
        s,
        StatusCode::UNPROCESSABLE_ENTITY,
        "rejection needs a reason"
    );
    let (s, dsp2) = call(
        &app,
        &admin,
        "POST",
        &format!(
            "/api/v1/finance/disputes/{}/decision",
            dsp2["id"].as_str().unwrap()
        ),
        json!({"decision": "reject", "reason": "Fine stands"}),
        Some(v(&dsp2)),
    )
    .await;
    assert_eq!(
        (s, dsp2["status"].as_str()),
        (StatusCode::OK, Some("rejected"))
    );

    // ---- F-003 suspend / resume / cancel (cancel posts the remaining outstanding back).
    let (_, f) = read(&app, &admin, &format!("/api/v1/finance/charges/{fine_id}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/suspend"),
        json!({}),
        Some(v(&f)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, f) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/suspend"),
        json!({"reason": "Under review"}),
        Some(v(&f)),
    )
    .await;
    assert_eq!(
        (s, f["status"].as_str()),
        (StatusCode::OK, Some("suspended"))
    );
    let (s, f) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/resume"),
        json!({}),
        Some(v(&f)),
    )
    .await;
    assert_eq!((s, f["status"].as_str()), (StatusCode::OK, Some("active")));
    let (s, f) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{fine_id}/cancel"),
        json!({"reason": "Waived by librarian"}),
        Some(v(&f)),
    )
    .await;
    assert_eq!(
        (s, f["status"].as_str()),
        (StatusCode::OK, Some("cancelled")),
        "{f}"
    );
    let fine_ar: Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(debit - credit), 0) FROM finance.journal_line WHERE charge_id = $1",
    )
    .bind(fine_id.parse::<Uuid>().unwrap())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        fine_ar,
        Decimal::ZERO,
        "the cancelled charge is cleared in the ledger"
    );

    // ---- F-020 / F-021 credit and debit notes through submit (auto-approval posts them).
    let (s, dn) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{charge}/adjustments"),
        json!({"kind": "debit_note", "amount": "1000.00", "reason": "Lab breakage"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{dn}");
    assert!(dn["adjustmentNumber"].as_str().unwrap().starts_with("DN-"));
    let dn_id = dn["id"].as_str().unwrap().to_owned();
    let (s, dn) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/adjustments/{dn_id}/submit"),
        json!({}),
        Some(v(&dn)),
    )
    .await;
    assert_eq!(
        (s, dn["status"].as_str()),
        (StatusCode::OK, Some("posted")),
        "{dn}"
    );
    assert_eq!(dn["decidedBy"], "system:auto-approval");
    let (_, cn) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{charge}/adjustments"),
        json!({"kind": "credit_note", "amount": "3000", "reason": "Merit concession"}),
        None,
    )
    .await;
    let cn_id = cn["id"].as_str().unwrap().to_owned();
    let (_, cn) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/adjustments/{cn_id}/submit"),
        json!({}),
        Some(v(&cn)),
    )
    .await;
    assert_eq!(cn["status"], "posted");
    let (_, c) = read(&app, &admin, &format!("/api/v1/finance/charges/{charge}")).await;
    assert_eq!(c["outstandingAmount"], "118000.00", "120000 + 1000 − 3000");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{charge}/adjustments"),
        json!({"kind": "credit_note", "amount": "118000.01", "reason": "Too much"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "a credit never exceeds the unsettled amount"
    );
    let (s, _) = call(
        &app,
        &aarav,
        "POST",
        &format!("/api/v1/finance/charges/{charge}/adjustments"),
        json!({"kind": "credit_note", "amount": "1", "reason": "x"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // ---- F-023 reversal of the credit note (mirror entry, charge effect undone).
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/adjustments/{cn_id}/reverse"),
        json!({}),
        Some(v(&cn)),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "reason required");
    let (s, cn) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/adjustments/{cn_id}/reverse"),
        json!({"reason": "Concession withdrawn"}),
        Some(v(&cn)),
    )
    .await;
    assert_eq!(
        (s, cn["status"].as_str()),
        (StatusCode::OK, Some("reversed")),
        "{cn}"
    );
    assert!(cn["reversalEntryId"].is_string());
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/adjustments/{cn_id}/reverse"),
        json!({"reason": "again"}),
        Some(v(&cn)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "a reversal is final");

    // ---- F-024 corrected successor of the debit note (1000 → 1500).
    let (s, succ) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/adjustments/{dn_id}/correct"),
        json!({"amount": "1500.00", "reason": "Breakage valued at 1500"}),
        Some(v(&dn)),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{succ}");
    assert_eq!(
        (succ["status"].as_str(), succ["supersedesId"].as_str()),
        (Some("draft"), Some(dn_id.as_str()))
    );
    let (_, old) = read(
        &app,
        &admin,
        &format!("/api/v1/finance/adjustments/{dn_id}"),
    )
    .await;
    assert_eq!(old["status"], "superseded");
    assert_eq!(old["supersededById"], succ["id"]);
    let (_, succ) = call(
        &app,
        &admin,
        "POST",
        &format!(
            "/api/v1/finance/adjustments/{}/submit",
            succ["id"].as_str().unwrap()
        ),
        json!({}),
        Some(v(&succ)),
    )
    .await;
    assert_eq!(succ["status"], "posted");
    let (_, c) = read(&app, &admin, &format!("/api/v1/finance/charges/{charge}")).await;
    assert_eq!(c["outstandingAmount"], "121500.00", "120000 + 1500");
    let (_, list) = read(&app, &admin, &format!("{base}/adjustments?status=posted")).await;
    assert!(list["items"].as_array().unwrap().len() >= 2);

    // ---- F-022 write-off of an uncollectable remainder → written_off → archived.
    let w = b5::seed_charge(&pool, &setup, "10000.00", "published", None).await;
    b5::seed_payment(&pool, &setup, w.charge_id, "8000.00").await;
    let (_, wo) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{}/adjustments", w.charge_id),
        json!({"kind": "write_off", "amount": "2000.00", "reason": "Student left; uncollectable"}),
        None,
    )
    .await;
    let (_, wo) = call(
        &app,
        &admin,
        "POST",
        &format!(
            "/api/v1/finance/adjustments/{}/submit",
            wo["id"].as_str().unwrap()
        ),
        json!({}),
        Some(v(&wo)),
    )
    .await;
    assert_eq!(wo["status"], "posted", "{wo}");
    let (_, wc) = read(
        &app,
        &admin,
        &format!("/api/v1/finance/charges/{}", w.charge_id),
    )
    .await;
    assert_eq!(
        (wc["status"].as_str(), wc["outstandingAmount"].as_str()),
        (Some("written_off"), Some("0.00"))
    );
    let (s, wc) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{}/archive", w.charge_id),
        json!({}),
        Some(v(&wc)),
    )
    .await;
    assert_eq!(
        (s, wc["status"].as_str()),
        (StatusCode::OK, Some("archived"))
    );
    assert_eq!(
        account_balance(&pool, &setup, "bad_debt_expense").await,
        amount("2000")
    );

    // ---- F-006 settlement → closure → archival; closing with dues is refused.
    let (_, c) = read(&app, &admin, &format!("/api/v1/finance/charges/{charge}")).await;
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{charge}/close"),
        json!({}),
        Some(v(&c)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let paid = b5::seed_charge(&pool, &setup, "5000.00", "published", None).await;
    b5::seed_payment(&pool, &setup, paid.charge_id, "5000.00").await;
    let (_, pc) = read(
        &app,
        &admin,
        &format!("/api/v1/finance/charges/{}", paid.charge_id),
    )
    .await;
    assert_eq!(pc["status"], "active");
    let (s, pc) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{}/close", paid.charge_id),
        json!({}),
        Some(v(&pc)),
    )
    .await;
    assert_eq!((s, pc["status"].as_str()), (StatusCode::OK, Some("closed")));
    let (s, pc) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{}/archive", paid.charge_id),
        json!({}),
        Some(v(&pc)),
    )
    .await;
    assert_eq!(
        (s, pc["status"].as_str()),
        (StatusCode::OK, Some("archived"))
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/charges/{}/revise", paid.charge_id),
        json!({"reason": "late", "description": "x"}),
        Some(v(&pc)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "archived charges are frozen");

    // ---- the receivable sub-ledger agrees with the recognised charges.
    assert_eq!(
        ar(&pool, &setup).await,
        recognised_outstanding(&pool, &setup).await
    );
    assert_eq!(ar(&pool, &setup).await, amount("121500"));
    let (_, list) = read(&app, &admin, &format!("{base}/charges?limit=2")).await;
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let cursor = list["nextCursor"].as_str().unwrap().to_owned();
    let (_, page2) = read(
        &app,
        &admin,
        &format!("{base}/charges?limit=50&cursor={cursor}"),
    )
    .await;
    assert_eq!(
        page2["items"].as_array().unwrap().len(),
        2,
        "4 charges in all"
    );

    // ---- F-084 configuration: v1 effective → v2 supersedes → rollback to v1 creates v3.
    let cfg_base = format!("{base}/finance-config");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &cfg_base,
        json!({"values": {"defaultDueDays": 500}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let mut ids = Vec::new();
    for days in [30, 45] {
        let (s, c1) = call(
            &app,
            &admin,
            "POST",
            &cfg_base,
            json!({"values": {"defaultDueDays": days, "allowPartialPayments": true}}),
            None,
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{c1}");
        let id = c1["id"].as_str().unwrap().to_owned();
        let (s, c1) = call(
            &app,
            &admin,
            "POST",
            &format!("/api/v1/finance/finance-config/{id}/submit"),
            json!({}),
            Some(v(&c1)),
        )
        .await;
        assert_eq!(
            (s, c1["status"].as_str()),
            (StatusCode::OK, Some("in_review"))
        );
        assert!(c1["approvedAt"].is_string(), "auto-approved");
        let (s, c1) = call(
            &app,
            &admin,
            "POST",
            &format!("/api/v1/finance/finance-config/{id}/activate"),
            json!({}),
            Some(v(&c1)),
        )
        .await;
        assert_eq!(
            (s, c1["status"].as_str()),
            (StatusCode::OK, Some("effective")),
            "{c1}"
        );
        ids.push(id);
    }
    let (_, v1) = read(
        &app,
        &admin,
        &format!("/api/v1/finance/finance-config/{}", ids[0]),
    )
    .await;
    assert_eq!(v1["status"], "superseded");
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/finance-config/{}/rollback", ids[1]),
        json!({"reason": "x"}),
        None,
    )
    .await;
    assert_eq!(
        s,
        StatusCode::CONFLICT,
        "the effective version is no rollback basis"
    );
    let (s, v3) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/finance-config/{}/rollback", ids[0]),
        json!({"reason": "45 days hurt collections"}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v3}");
    assert_eq!(
        (
            v3["versionNo"].as_i64(),
            v3["basedOnId"].as_str(),
            v3["status"].as_str()
        ),
        (Some(3), Some(ids[0].as_str()), Some("draft"))
    );
    assert_eq!(v3["values"]["defaultDueDays"], 30);
    let v3_id = v3["id"].as_str().unwrap().to_owned();
    let (_, v3) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/finance-config/{v3_id}/submit"),
        json!({}),
        Some(v(&v3)),
    )
    .await;
    let (_, v3) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/finance-config/{v3_id}/activate"),
        json!({}),
        Some(v(&v3)),
    )
    .await;
    assert_eq!(v3["status"], "effective");
    let (_, all) = read(&app, &admin, &cfg_base).await;
    let statuses: Vec<&str> = all["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        statuses,
        vec!["effective", "rolled_back", "superseded"],
        "v1 itself is never reactivated"
    );
    assert_eq!(
        outbox_count(&pool, "sutraerp.finance.finance_config.effective", &v3_id).await,
        1
    );

    // ---- F-085 financial rule (late fee) with type checks.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/finance-rules"),
        json!({"ruleKey": "late_fee", "values": {"percent": 2.5}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "no floats");
    let (s, r) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/finance-rules"),
        json!({"ruleKey": "late_fee", "values": {"percent": "2.50", "graceDays": 7}}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r}");
    let rid = r["id"].as_str().unwrap().to_owned();
    let (_, r) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/finance-rules/{rid}/submit"),
        json!({}),
        Some(v(&r)),
    )
    .await;
    let (s, r) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/finance-rules/{rid}/activate"),
        json!({}),
        Some(v(&r)),
    )
    .await;
    assert_eq!(
        (s, r["status"].as_str(), r["ruleKey"].as_str()),
        (StatusCode::OK, Some("effective"), Some("late_fee"))
    );
    let (_, rules) = read(
        &app,
        &admin,
        &format!("{base}/finance-rules?ruleKey=late_fee"),
    )
    .await;
    assert_eq!(rules["items"].as_array().unwrap().len(), 1);
}

/// Seeds a Domain C scholarship + an eligibility handed off to Financial Aid (3C's tables).
async fn seed_decision(
    pool: &sqlx::PgPool,
    setup: &b5::FinanceSetup,
    code: &str,
    benefit_type: &str,
    value: Option<&str>,
) -> Uuid {
    let scholarship: Uuid = sqlx::query_scalar(
        "INSERT INTO student.scholarship (tenant_id, institution_id, code, name, sponsor_type, benefit_type, \
         benefit_value, valid_from, status) VALUES ($1, $2, $3, $4, 'institutional', $5, $6::numeric, DATE '2026-06-01', 'active') RETURNING id",
    )
    .bind(setup.tenant().as_uuid())
    .bind(setup.institution().as_uuid())
    .bind(code)
    .bind(format!("{code} scholarship"))
    .bind(benefit_type)
    .bind(value)
    .fetch_one(pool)
    .await
    .unwrap();
    sqlx::query_scalar(
        "INSERT INTO student.scholarship_eligibility (tenant_id, institution_id, scholarship_id, student_id, \
         student_context_id, academic_year_id, status, valid_from, valid_until, handed_off_at) \
         VALUES ($1, $2, $3, $4, $5, $6, 'active', DATE '2026-06-01', DATE '2027-05-31', now()) RETURNING id",
    )
    .bind(setup.tenant().as_uuid())
    .bind(setup.institution().as_uuid())
    .bind(scholarship)
    .bind(setup.student.student_id)
    .bind(setup.student.context_id)
    .bind(setup.delivery.base.academic_year_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn financial_aid_from_handed_off_decisions() {
    let (app, pool) = app(TENANT_B).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        TENANT_B,
        "Aid University",
        Some(b4::AARAV),
    )
    .await;
    let inst = setup.institution().to_string();
    let base = format!("/api/v1/finance/institutions/{inst}");
    let tuition = b5::seed_charge(&pool, &setup, "125000.00", "published", None).await;
    let merit = seed_decision(&pool, &setup, "MERIT", "fixed_amount", Some("30000.00")).await;

    // ---- F-062 handed-off decision → allocation (auto-approved), idempotent per decision.
    let (s, decisions) = read(&app, &admin, &format!("{base}/aid-decisions")).await;
    assert_eq!(s, StatusCode::OK, "{decisions}");
    assert_eq!(decisions["items"][0]["eligibilityId"], merit.to_string());
    assert_eq!(decisions["items"][0]["live"], true);
    let (s, al) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/aid-allocations"),
        json!({"eligibilityId": merit}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{al}");
    assert_eq!(
        (al["status"].as_str(), al["allocatedAmount"].as_str()),
        (Some("allocated"), Some("30000.00"))
    );
    assert!(al["allocationNumber"].as_str().unwrap().starts_with("AID-"));
    let al_id = al["id"].as_str().unwrap().to_owned();
    let (s, same) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/aid-allocations"),
        json!({"eligibilityId": merit}),
        None,
    )
    .await;
    assert_eq!(
        (s, same["id"].as_str()),
        (StatusCode::OK, Some(al_id.as_str()))
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/aid-allocations"),
        json!({"eligibilityId": Uuid::now_v7()}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);

    // ---- F-063 applied to the tuition charge (Dr aid expense / Cr receivables).
    let (s, al) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/aid-allocations/{al_id}/disburse"),
        json!({"mode": "charge_adjustment", "chargeId": tuition.charge_id}),
        Some(v(&al)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{al}");
    assert_eq!(
        (
            al["status"].as_str(),
            al["appliedAmount"].as_str(),
            al["remainingAmount"].as_str()
        ),
        (Some("disbursed"), Some("30000.00"), Some("0.00"))
    );
    let mut conn = pool.acquire().await.unwrap();
    let c =
        sutra_finance::shared::charge::load_charge(&mut conn, setup.tenant(), tuition.charge_id)
            .await
            .unwrap();
    drop(conn);
    assert_eq!(
        (c.adjustment_amount, c.outstanding_amount),
        (amount("-30000"), amount("95000"))
    );
    assert_eq!(
        account_balance(&pool, &setup, "aid_expense").await,
        amount("30000")
    );
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/aid-allocations/{al_id}/disburse"),
        json!({"mode": "charge_adjustment", "chargeId": tuition.charge_id}),
        Some(v(&al)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "nothing left of the fixed benefit");

    // ---- F-064 renewal needs a renewed decision; then the ceiling grows; stipend-like cash.
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/aid-allocations/{al_id}/renew"),
        json!({}),
        Some(v(&al)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    sqlx::query("UPDATE student.scholarship_eligibility SET renewal_count = 1, status = 'renewal', valid_until = DATE '2028-05-31' WHERE id = $1")
        .bind(merit).execute(&pool).await.unwrap();
    let (s, al) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/aid-allocations/{al_id}/renew"),
        json!({}),
        Some(v(&al)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{al}");
    assert_eq!(
        (al["allocatedAmount"].as_str(), al["renewalCount"].as_i64()),
        (Some("60000.00"), Some(1))
    );
    assert_eq!(
        outbox_count(&pool, "sutraerp.finance.aid_allocation.renewed", &al_id).await,
        1
    );
    let (s, al) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/aid-allocations/{al_id}/disburse"),
        json!({"mode": "cash", "amount": "10000.00"}),
        Some(v(&al)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{al}");
    assert_eq!(al["disbursedAmount"], "10000.00");
    assert_eq!(
        account_balance(&pool, &setup, "refunds_payable").await,
        amount("-10000")
    );

    // ---- F-066 reconciliation: matched while consistent and the decision is live.
    let (s, rec) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/aid-reconciliations"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{rec}");
    assert_eq!(
        (
            rec["status"].as_str(),
            rec["utilisedTotal"].as_str(),
            rec["ledgerTotal"].as_str()
        ),
        (Some("matched"), Some("40000.00"), Some("40000.00")),
        "{rec}"
    );

    // ---- F-065 the decision expires → no more disbursement; revoke with recovery.
    sqlx::query("UPDATE student.scholarship_eligibility SET status = 'expired' WHERE id = $1")
        .bind(merit)
        .execute(&pool)
        .await
        .unwrap();
    let (s, _) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/aid-allocations/{al_id}/disburse"),
        json!({"mode": "cash", "amount": "1.00"}),
        Some(v(&al)),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (_, rec2) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/aid-reconciliations"),
        json!({}),
        None,
    )
    .await;
    assert_eq!(rec2["status"], "exceptions");
    assert_eq!(rec2["exceptions"][0]["issue"], "decision_not_active");
    let ar_before = ar(&pool, &setup).await;
    let (s, al) = call(
        &app,
        &admin,
        "POST",
        &format!("/api/v1/finance/aid-allocations/{al_id}/revoke"),
        json!({"reason": "Attendance below the scholarship condition", "recover": true}),
        Some(v(&al)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{al}");
    assert_eq!(
        (al["status"].as_str(), al["recoveredAmount"].as_str()),
        (Some("recovered"), Some("40000.00"))
    );
    let recovery = al["recoveryChargeId"].as_str().unwrap().to_owned();
    let (_, rc) = read(&app, &admin, &format!("/api/v1/finance/charges/{recovery}")).await;
    assert_eq!(
        (
            rc["sourceType"].as_str(),
            rc["status"].as_str(),
            rc["totalAmount"].as_str()
        ),
        (Some("aid_recovery"), Some("published"), Some("40000.00"))
    );
    assert_eq!(ar(&pool, &setup).await - ar_before, amount("40000"));
    assert_eq!(
        ar(&pool, &setup).await,
        recognised_outstanding(&pool, &setup).await
    );
    let (_, recs) = read(&app, &admin, &format!("{base}/aid-reconciliations")).await;
    assert_eq!(
        recs["items"].as_array().unwrap().len(),
        2,
        "each run is a new fact"
    );

    // ---- percentage aid (10 %) on a new charge; the student sees their aid.
    let fee2 = b5::seed_charge(&pool, &setup, "50000.00", "published", None).await;
    let pct = seed_decision(&pool, &setup, "SPORTS", "fee_percentage", Some("10")).await;
    let (_, al2) = call(
        &app,
        &admin,
        "POST",
        &format!("{base}/aid-allocations"),
        json!({"eligibilityId": pct}),
        None,
    )
    .await;
    let (s, al2) = call(
        &app,
        &admin,
        "POST",
        &format!(
            "/api/v1/finance/aid-allocations/{}/disburse",
            al2["id"].as_str().unwrap()
        ),
        json!({"mode": "charge_adjustment", "chargeId": fee2.charge_id}),
        Some(v(&al2)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{al2}");
    assert_eq!(al2["appliedAmount"], "5000.00");
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let (s, mine) = read(&app, &aarav, "/api/v1/finance/me/aid").await;
    assert_eq!(s, StatusCode::OK, "{mine}");
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);
    let (s, _) = read(&app, &aarav, &format!("{base}/aid-allocations")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn await_decision_keeps_governed_steps_pending() {
    let (app, pool) = app(TENANT_C).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        TENANT_C,
        "Awaiting University",
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
        id: Some("dev|admin@sutra.dev".into()),
        correlation_id: None,
    };
    let approver = Actor {
        id: Some("dev|approver@sutra.dev".into()),
        correlation_id: None,
    };
    use sutra_finance::adjustments::repo as adj;

    // ---- adjustment: submit leaves it pending (no posting) until the explicit decision.
    let c = b5::seed_charge(&pool, &setup, "20000.00", "published", None).await;
    let a = adj::create(
        &st,
        t,
        c.charge_id,
        adj::NewAdjustment {
            kind: AdjustmentKind::Concession,
            amount: amount("2500"),
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
    assert!(a.process_instance_id.is_some());
    assert!(a.journal_entry_id.is_none());
    assert_eq!(
        outbox_count(
            &pool,
            "sutraerp.finance.adjustment.submitted",
            &a.id.to_string()
        )
        .await,
        1
    );
    let mut conn = pool.acquire().await.unwrap();
    let row = sutra_finance::shared::charge::load_charge(&mut conn, t, c.charge_id)
        .await
        .unwrap();
    assert_eq!(
        row.outstanding_amount,
        amount("20000"),
        "nothing moves while pending"
    );
    drop(conn);
    let err = adj::decide(&st, t, a.id, a.version, false, None, &approver)
        .await
        .unwrap_err();
    assert!(
        matches!(err, KernelError::Validation(_)),
        "a rejection needs a reason"
    );
    let a = adj::decide(&st, t, a.id, a.version, true, None, &approver)
        .await
        .unwrap();
    assert_eq!(a.status, AdjustmentStatus::Posted);
    assert_eq!(a.decided_by.as_deref(), Some("dev|approver@sutra.dev"));
    let b = adj::create(
        &st,
        t,
        c.charge_id,
        adj::NewAdjustment {
            kind: AdjustmentKind::WriteOff,
            amount: amount("100"),
            reason: "Rounding",
            origin: "manual",
            supersedes: None,
        },
        &maker,
    )
    .await
    .unwrap();
    let b = adj::submit(&st, t, b.id, b.version, &maker).await.unwrap();
    let b = adj::decide(
        &st,
        t,
        b.id,
        b.version,
        false,
        Some("not justified"),
        &approver,
    )
    .await
    .unwrap();
    assert_eq!(b.status, AdjustmentStatus::Rejected);
    let mut conn = pool.acquire().await.unwrap();
    let row = sutra_finance::shared::charge::load_charge(&mut conn, t, c.charge_id)
        .await
        .unwrap();
    assert_eq!(row.outstanding_amount, amount("17500"));
    drop(conn);

    // ---- fee structure stays in review until approved.
    use sutra_finance::charges::fees;
    let fs = fees::create_structure(
        &st,
        &setup.scope(),
        setup.institution(),
        fees::NewStructure {
            code: "MBA".into(),
            name: "MBA".into(),
            academic_year_id: None,
            academic_term_id: None,
            programme_id: None,
            due_date: None,
            lines: vec![sutra_finance::charges::repo::line(
                setup.fee_head_id.as_uuid(),
                "Tuition".into(),
                amount("90000"),
            )],
        },
        &maker,
    )
    .await
    .unwrap();
    let fs = fees::submit_structure(&st, t, FeeStructureId(fs.id), fs.version, &maker)
        .await
        .unwrap();
    assert_eq!(fs.status, FeeStructureStatus::InReview);
    let err = fees::publish_structure(&st, t, FeeStructureId(fs.id), fs.version, &maker)
        .await
        .unwrap_err();
    assert!(matches!(err, KernelError::Conflict(_)));
    let fs = fees::decide_structure(
        &st,
        t,
        FeeStructureId(fs.id),
        fs.version,
        true,
        None,
        &approver,
    )
    .await
    .unwrap();
    assert_eq!(
        (fs.status, fs.approved_by.as_deref()),
        (FeeStructureStatus::Approved, Some("dev|approver@sutra.dev"))
    );

    // ---- charge publication awaits the publication authority; the approver completes it.
    let draft = b5::seed_charge(&pool, &setup, "700.00", "draft", None).await;
    let charges = sutra_finance::charges::repo::publish(&st, t, draft.charge_id, 1, false, &maker)
        .await
        .unwrap();
    assert_eq!(
        charges.status,
        sutra_contracts::finance::status::ChargeStatus::Validated
    );
    assert!(charges.process_instance_id.is_some() && charges.journal_entry_id.is_none());
    let done = sutra_finance::charges::repo::publish(
        &st,
        t,
        ChargeId(charges.id),
        charges.version,
        true,
        &approver,
    )
    .await
    .unwrap();
    assert_eq!(
        done.status,
        sutra_contracts::finance::status::ChargeStatus::Published
    );
    assert!(done.journal_entry_id.is_some());

    // ---- aid allocation stays proposed until approved.
    let e = seed_decision(&pool, &setup, "NEED", "fee_waiver", None).await;
    let (al, created) =
        sutra_finance::aid::repo::allocate(&st, &setup.scope(), setup.institution(), e, &maker)
            .await
            .unwrap();
    assert!(created);
    assert_eq!(
        al.status,
        sutra_contracts::finance::status::AidAllocationStatus::Proposed
    );
    let al = sutra_finance::aid::repo::decide(
        &st,
        t,
        sutra_contracts::finance::ids::AidAllocationId(al.id),
        al.version,
        true,
        None,
        &approver,
    )
    .await
    .unwrap();
    assert_eq!(
        al.status,
        sutra_contracts::finance::status::AidAllocationStatus::Allocated
    );

    // ---- configuration: activation requires the approval decision.
    use sutra_finance::config::repo as cfg;
    let v1 = cfg::create(
        &st,
        cfg::Kind::Config,
        &setup.scope(),
        setup.institution(),
        None,
        json!({"maxInstalments": 6}),
        None,
        &maker,
    )
    .await
    .unwrap();
    let v1 = cfg::submit(&st, cfg::Kind::Config, t, v1.id, v1.version, &maker)
        .await
        .unwrap();
    assert!(v1.approved_at.is_none());
    assert!(matches!(
        cfg::activate(&st, cfg::Kind::Config, t, v1.id, v1.version, &maker).await,
        Err(KernelError::Conflict(_))
    ));
    let v1 = cfg::decide(
        &st,
        cfg::Kind::Config,
        t,
        v1.id,
        v1.version,
        true,
        None,
        &approver,
    )
    .await
    .unwrap();
    let v1 = cfg::activate(&st, cfg::Kind::Config, t, v1.id, v1.version, &maker)
        .await
        .unwrap();
    assert_eq!(
        v1.status,
        sutra_contracts::finance::status::FinancePolicyStatus::Effective
    );
}
