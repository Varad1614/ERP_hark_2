//! Batch 5 foundation smoke test: migrations + the shared finance tables (append-only ledger at
//! trigger and privilege level), the posting helper (balanced, idempotent, reversible, refuses
//! unbalanced entries), the settlement / adjustment helpers, the charge directory and receivable
//! reader, the real `FinanceClearancePort` (NotApplicable → Outstanding → Cleared / Pending) and
//! the foundation account endpoints.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use chrono::NaiveDate;
use support::b5::{amount, seed_actor};
use support::*;
use sutra_contracts::finance::events::{JOURNAL_ENTRY_POSTED, JOURNAL_ENTRY_REVERSED};
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{ChargeStatus, JournalEntryKind};
use sutra_contracts::finance::{
    AdmissionFinanceQuery, FinanceClearancePort, FinanceEvidence, StudentFinanceQuery,
};
use sutra_finance::shared::charge::{apply_adjustment, apply_settlement, load_charge, lock_charge};
use sutra_finance::shared::posting;
use sutra_kernel::error::KernelError;
use sutra_kernel::money::CurrencyCode;
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b5f00";

fn date(m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, m, d).unwrap()
}

fn request(setup: &b5::FinanceSetup, key: &str, debit: &str, credit: &str) -> PostingRequest {
    PostingRequest {
        institution_id: setup.institution(),
        posting_date: date(7, 10),
        kind: JournalEntryKind::Standard,
        source_type: source::PAYMENT.into(),
        source_id: Uuid::from_u128(0x5f00),
        posting_key: key.into(),
        description: "Counter receipt".into(),
        currency: CurrencyCode::INR,
        lines: vec![
            PostingLine::debit(AccountRef::System(SystemAccount::CashOnHand), amount(debit)),
            PostingLine::credit(
                AccountRef::System(SystemAccount::UnappliedReceipts),
                amount(credit),
            ),
        ],
    }
}

#[tokio::test]
async fn finance_foundation_tables_posting_clearance_and_accounts() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b5::finance_setup(
        &app,
        &admin,
        &pool,
        TENANT,
        "Finance University",
        Some(b4::AARAV),
    )
    .await;
    let (t, scope) = (setup.tenant(), setup.scope());

    // ---- migrations: every foundation table exists; the chart is seeded once.
    for table in [
        "ledger_account",
        "fee_head",
        "student_account",
        "charge",
        "charge_line",
        "journal_entry",
        "journal_line",
        "number_sequence",
        "status_history",
    ] {
        let found: Option<String> =
            sqlx::query_scalar(&format!("SELECT to_regclass('finance.{table}')::text"))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(found.is_some(), "finance.{table} missing");
    }
    let accounts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.ledger_account WHERE institution_id = $1 AND system_key IS NOT NULL",
    )
    .bind(setup.institution().as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(accounts, 14, "default chart of accounts");

    // ---- posting: balanced entry → JE number, lines, event; retry is idempotent.
    let mut tx = pool.begin().await.unwrap();
    let r = posting::post(
        &mut tx,
        t,
        &request(&setup, "receive", "250.50", "250.50"),
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(!r.already_posted);
    assert!(r.entry_number.starts_with("JE-"), "{}", r.entry_number);
    let mut tx = pool.begin().await.unwrap();
    let again = posting::post(
        &mut tx,
        t,
        &request(&setup, "receive", "250.50", "250.50"),
        &seed_actor(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert!(again.already_posted);
    assert_eq!(again.journal_entry_id, r.journal_entry_id);
    let (d, c, n): (rust_decimal::Decimal, rust_decimal::Decimal, i64) = sqlx::query_as(
        "SELECT sum(debit), sum(credit), count(*) FROM finance.journal_line WHERE journal_entry_id = $1",
    )
    .bind(r.journal_entry_id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((d, c, n), (amount("250.50"), amount("250.50"), 2));

    // ---- unbalanced: refused before the database (nothing written) …
    let mut tx = pool.begin().await.unwrap();
    let err = posting::post(
        &mut tx,
        t,
        &request(&setup, "bad", "100.00", "99.99"),
        &seed_actor(),
    )
    .await
    .unwrap_err();
    assert!(
        matches!(&err, KernelError::Conflict(m) if m.contains("unbalanced")),
        "{err:?}"
    );
    tx.rollback().await.unwrap();
    let bad: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.journal_entry WHERE tenant_id = $1 AND posting_key = 'bad'",
    )
    .bind(t.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(bad, 0);
    // … and by the database at commit when written around the helper.
    let cash: Uuid = sqlx::query_scalar(
        "SELECT id FROM finance.ledger_account WHERE institution_id = $1 AND system_key = 'cash_on_hand'",
    )
    .bind(setup.institution().as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    let mut tx = pool.begin().await.unwrap();
    let eid: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.journal_entry (tenant_id, institution_id, entry_number, posting_date, \
         kind, source_type, source_id, posting_key, description, total_debit, total_credit) \
         VALUES ($1, $2, 'JE-RAW', DATE '2026-07-10', 'standard', 'payment', $3, 'raw', 'raw', 10, 10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(setup.institution().as_uuid())
    .bind(Uuid::now_v7())
    .fetch_one(&mut *tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO finance.journal_line (tenant_id, journal_entry_id, line_no, ledger_account_id, debit) \
         VALUES ($1, $2, 1, $3, 10)",
    )
    .bind(t.as_uuid())
    .bind(eid)
    .bind(cash)
    .execute(&mut *tx)
    .await
    .unwrap();
    let commit = tx.commit().await;
    assert!(
        commit
            .as_ref()
            .is_err_and(|e| e.to_string().contains("unbalanced")),
        "{commit:?}"
    );

    // ---- append-only: trigger and privilege (runtime module role).
    let upd = sqlx::query("UPDATE finance.journal_entry SET description = 'x' WHERE id = $1")
        .bind(r.journal_entry_id.as_uuid())
        .execute(&pool)
        .await;
    assert!(upd.is_err_and(|e| e.to_string().contains("append-only")));
    let mut tx = pool.begin().await.unwrap();
    sqlx::query("SET LOCAL ROLE sutra_finance")
        .execute(&mut *tx)
        .await
        .unwrap();
    let denied = sqlx::query("UPDATE finance.journal_line SET memo = 'x' WHERE tenant_id = $1")
        .bind(t.as_uuid())
        .execute(&mut *tx)
        .await;
    assert!(denied.is_err_and(|e| e.to_string().contains("permission denied")));
    tx.rollback().await.unwrap();

    // ---- reversal: mirror entry, once; a reversal is not reversible.
    let mut tx = pool.begin().await.unwrap();
    let rev = posting::reverse(
        &mut tx,
        t,
        r.journal_entry_id,
        date(7, 11),
        "keyed twice",
        &seed_actor(),
    )
    .await
    .unwrap();
    let rev2 = posting::reverse(
        &mut tx,
        t,
        r.journal_entry_id,
        date(7, 11),
        "again",
        &seed_actor(),
    )
    .await
    .unwrap();
    let rr = posting::reverse(
        &mut tx,
        t,
        rev.journal_entry_id,
        date(7, 11),
        "x",
        &seed_actor(),
    )
    .await;
    tx.commit().await.unwrap();
    assert!(!rev.already_posted && rev2.already_posted);
    assert_eq!(rev2.journal_entry_id, rev.journal_entry_id);
    assert!(matches!(rr, Err(KernelError::Conflict(_))));
    let cash_balance: rust_decimal::Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(debit - credit), 0) FROM finance.journal_line WHERE ledger_account_id = $1",
    )
    .bind(cash)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        cash_balance,
        rust_decimal::Decimal::ZERO,
        "posting + reversal net to zero"
    );
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM platform.outbox WHERE subject = ANY($1) ORDER BY created_at, id",
    )
    .bind(vec![
        r.journal_entry_id.to_string(),
        rev.journal_entry_id.to_string(),
    ])
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        events,
        vec![
            JOURNAL_ENTRY_POSTED.to_owned(),
            JOURNAL_ENTRY_REVERSED.to_owned()
        ]
    );

    // ---- clearance: no charges → NotApplicable (Batch 3 behaviour unchanged).
    let clearance = sutra_finance::PgFinanceClearance::new(pool.clone());
    let dues = || StudentFinanceQuery {
        institution_id: setup.institution(),
        student_id: setup.student.student_id,
    };
    assert_eq!(
        clearance.student_dues(&scope, dues()).await.unwrap(),
        FinanceEvidence::NotApplicable
    );
    assert_eq!(
        clearance
            .admission_clearance(
                &scope,
                AdmissionFinanceQuery {
                    institution_id: setup.institution(),
                    admission_id: Uuid::now_v7(),
                    programme_id: setup.delivery.base.programme_id,
                    student_id: None,
                },
            )
            .await
            .unwrap(),
        FinanceEvidence::NotApplicable
    );
    // A draft charge is not a due yet.
    let draft = b5::seed_charge(&pool, &setup, "999.00", "draft", None).await;
    assert_eq!(
        clearance.student_dues(&scope, dues()).await.unwrap(),
        FinanceEvidence::NotApplicable
    );

    // ---- an unpaid published charge → Outstanding.
    let tuition = b5::seed_charge(&pool, &setup, "125000.00", "published", Some(date(7, 15))).await;
    assert!(tuition.charge_number.starts_with("CHG-"));
    assert_eq!(
        clearance.student_dues(&scope, dues()).await.unwrap(),
        FinanceEvidence::Outstanding {
            detail: "₹1,25,000.00 outstanding on 1 charge".into()
        }
    );

    // ---- directory + receivable reader.
    let state = sutra_api::app::finance_deps_with(
        pool.clone(),
        std::sync::Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
        // Part 1: explicit dev/test auto-approval opt-in (the subject here is not approval).
        sutra_kernel::approval::ApprovalMode::AutoApprove,
    )
    .into_state();
    let open = state
        .charges
        .open_charges_of_account(&scope, setup.student_account_id)
        .await
        .unwrap();
    assert_eq!(
        open.iter().map(|c| c.id).collect::<Vec<_>>(),
        vec![tuition.charge_id]
    );
    assert_eq!(
        state
            .charges
            .charges_of_account(&scope, setup.student_account_id)
            .await
            .unwrap()
            .len(),
        2
    );
    let bal = state
        .receivables
        .account_balance(&scope, setup.student_account_id, date(8, 1))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (bal.outstanding, bal.overdue, bal.open_charges),
        (amount("125000"), amount("125000"), 1)
    );
    let acc = state
        .charges
        .account_of_student(&scope, setup.institution(), setup.student.student_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(acc.id, setup.student_account_id);
    assert_eq!(
        state
            .charges
            .accounts_of_subject(&scope, b4::AARAV)
            .await
            .unwrap()
            .len(),
        1
    );

    // ---- settlement helper: over-allocation refused; partial → still Outstanding; the first
    // settlement activates the charge; full → Cleared.
    let mut tx = pool.begin().await.unwrap();
    let c = lock_charge(&mut tx, t, tuition.charge_id).await.unwrap();
    let over = apply_settlement(
        &mut tx,
        &c,
        amount("125000.01"),
        JOURNAL_ENTRY_POSTED,
        &seed_actor(),
    )
    .await;
    assert!(matches!(over, Err(KernelError::Conflict(_))));
    tx.rollback().await.unwrap();
    b5::seed_payment(&pool, &setup, tuition.charge_id, "100000.00").await;
    let mut conn = pool.acquire().await.unwrap();
    let c = load_charge(&mut conn, t, tuition.charge_id).await.unwrap();
    assert_eq!(
        (c.status, c.outstanding_amount),
        (ChargeStatus::Active, amount("25000"))
    );
    drop(conn);
    assert!(matches!(
        clearance.student_dues(&scope, dues()).await.unwrap(),
        FinanceEvidence::Outstanding { .. }
    ));
    // A 5000 credit (5A adjustment helper) then the rest paid.
    let mut tx = pool.begin().await.unwrap();
    let c = lock_charge(&mut tx, t, tuition.charge_id).await.unwrap();
    let c = apply_adjustment(&mut tx, &c, amount("-5000"))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(c.outstanding_amount, amount("20000"));
    b5::seed_payment(&pool, &setup, tuition.charge_id, "20000.00").await;
    let ev = clearance.student_dues(&scope, dues()).await.unwrap();
    assert!(
        matches!(&ev, FinanceEvidence::Cleared { reference } if reference.contains(&setup.student_account_id.to_string())),
        "{ev:?}"
    );

    // ---- disputed dues only → Pending.
    b5::seed_charge(&pool, &setup, "500.00", "disputed", None).await;
    assert!(matches!(
        clearance.student_dues(&scope, dues()).await.unwrap(),
        FinanceEvidence::Pending { .. }
    ));

    // ---- the receivable sub-ledger agrees with the charges (published 125000 + 500 − paid 120000).
    let ar: rust_decimal::Decimal = sqlx::query_scalar(
        "SELECT COALESCE(sum(l.debit - l.credit), 0) FROM finance.journal_line l \
         JOIN finance.ledger_account a ON a.id = l.ledger_account_id \
         WHERE a.institution_id = $1 AND a.system_key = 'student_receivables' AND l.student_account_id = $2",
    )
    .bind(setup.institution().as_uuid())
    .bind(setup.student_account_id.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        ar,
        amount("5500"),
        "125000 + 500 − 120000 (the 5000 credit is 5A's posting)"
    );
    let _ = ChargeId(draft.charge_id.as_uuid());

    // ---- foundation endpoints.
    let (s, h, body) = send(
        &app,
        get(
            &admin,
            &format!(
                "/api/v1/finance/student-accounts/{}",
                setup.student_account_id
            ),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert!(h.get("etag").is_some());
    assert_eq!(
        body["account"]["balance"]["disputedOutstanding"], "500.00",
        "{body}"
    );
    assert_eq!(body["charges"].as_array().unwrap().len(), 3, "{body}");
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!(
                "/api/v1/finance/institutions/{}/student-accounts?withDues=false",
                setup.institution()
            ),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    let student = login(&app, "aarav.desai@sutra.dev").await;
    let (s, _, body) = send(&app, get(&student, "/api/v1/finance/me/accounts")).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let mine = &body["items"][0];
    assert_eq!(mine["account"]["id"], setup.student_account_id.to_string());
    assert_eq!(
        mine["charges"].as_array().unwrap().len(),
        2,
        "the draft charge is hidden: {body}"
    );
    let (s, _, _) = send(
        &app,
        get(
            &student,
            &format!(
                "/api/v1/finance/student-accounts/{}",
                setup.student_account_id
            ),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _, _) = send(&app, get(&admin, "/api/v1/finance/me/accounts")).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
}
