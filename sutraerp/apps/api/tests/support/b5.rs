//! Batch 5 foundation test helpers (foundation-owned; slices add their own helpers in their own
//! test file, see `docs/batch5-contracts.md` §11).
//!
//! * [`finance_setup`] — Batch 4's Domain B setup (institution + published programme/curriculum
//!   + published calendar, term-1 components) with the institution set **active**, one seeded
//!   active student (optionally linked to a login, e.g. [`super::b4::AARAV`]), the default chart
//!   of accounts, a `TUITION` fee head and the student's finance account.
//! * [`seed_charge`] — a charge of the student (one TUITION line) in any status; for
//!   `published | active | disputed | closed` the publication entry (Dr receivables / Cr fee
//!   income) is posted through `shared::posting`. 5B/5C use it so they never wait for 5A's API.
//! * [`seed_payment`] — settles an amount on a charge exactly like 5B will (through
//!   `shared::charge::apply_settlement` + Dr bank / Cr receivables posting) without 5B's payment
//!   tables. 5A/5C use it so they never wait for 5B.

use super::b3::SeededStudent;
use super::b4::{self, DeliverySetup};
use super::Login;
use axum::Router;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::str::FromStr;
use sutra_contracts::finance::events::JOURNAL_ENTRY_POSTED;
use sutra_contracts::finance::ids::{ChargeId, FeeHeadId, JournalEntryId, StudentAccountId};
use sutra_contracts::finance::ports::{
    source, AccountRef, PostingLine, PostingRequest, SystemAccount,
};
use sutra_contracts::finance::status::{ChargeSourceType, JournalEntryKind};
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::StudentSummary;
use sutra_contracts::student::status::StudentStatus;
use sutra_finance::shared::account::ensure_student_account;
use sutra_finance::shared::charge::{
    apply_settlement, insert_charge, lock_charge, NewCharge, NewChargeLine,
};
use sutra_finance::shared::coa::{ensure_default_chart, resolve};
use sutra_finance::shared::{posting, Actor};
use sutra_kernel::money::CurrencyCode;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use uuid::Uuid;

/// Decimal from a literal (`"1500.00"`).
pub fn amount(s: &str) -> Decimal {
    Decimal::from_str(s).expect("decimal literal")
}

/// Domain B/C/F context a Domain F test needs.
#[derive(Clone, Debug)]
pub struct FinanceSetup {
    pub delivery: DeliverySetup,
    pub student: SeededStudent,
    pub student_account_id: StudentAccountId,
    pub fee_head_id: FeeHeadId,
}

impl FinanceSetup {
    pub fn tenant(&self) -> TenantId {
        self.delivery.tenant()
    }
    pub fn institution(&self) -> InstitutionId {
        self.delivery.institution()
    }
    pub fn scope(&self) -> ExecutionScope {
        self.delivery.scope()
    }
}

/// The actor recorded by seeds.
pub fn seed_actor() -> Actor {
    Actor {
        id: Some("test:b5-seed".into()),
        correlation_id: None,
    }
}

/// See the module docs. `subject_ref` links the student to a login (`Some(b4::AARAV)`).
pub async fn finance_setup(
    app: &Router,
    admin: &Login,
    owner: &PgPool,
    tenant: &str,
    name: &str,
    subject_ref: Option<&str>,
) -> FinanceSetup {
    let delivery = b4::academic_setup(app, admin, owner, tenant, name).await;
    sqlx::query("UPDATE institution.institution SET status = 'active' WHERE id = $1")
        .bind(delivery.base.institution_id)
        .execute(owner)
        .await
        .expect("activate institution");
    let student = b4::seed_student(owner, &delivery, "Aarav", subject_ref).await;
    let (t, i) = (delivery.tenant(), delivery.institution());
    let mut tx = owner.begin().await.expect("tx");
    ensure_default_chart(&mut tx, t, i).await.expect("chart");
    let income = resolve(&mut tx, t, i, AccountRef::System(SystemAccount::FeeIncome))
        .await
        .expect("fee income account");
    let fee_head_id: Uuid = sqlx::query_scalar(
        "INSERT INTO finance.fee_head (tenant_id, institution_id, code, name, category, \
         income_account_id) VALUES ($1, $2, 'TUITION', 'Tuition fee', 'tuition', $3) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(i.as_uuid())
    .bind(income.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .expect("fee head");
    let account = ensure_student_account(
        &mut tx,
        t,
        i,
        &StudentSummary {
            id: StudentId(student.student_id),
            student_number: student.student_number.clone(),
            display_name: "Aarav Seeded".into(),
            status: StudentStatus::Active,
            subject_ref: subject_ref.map(str::to_owned),
        },
        &seed_actor(),
    )
    .await
    .expect("student account");
    tx.commit().await.expect("commit");
    FinanceSetup {
        delivery,
        student,
        student_account_id: account.id(),
        fee_head_id: FeeHeadId(fee_head_id),
    }
}

/// A seeded charge.
#[derive(Clone, Debug)]
pub struct SeededCharge {
    pub charge_id: ChargeId,
    pub charge_number: String,
    pub publication_entry: Option<JournalEntryId>,
}

/// See the module docs. `status` is any `ChargeStatus` wire name; `due` defaults to none.
pub async fn seed_charge(
    owner: &PgPool,
    setup: &FinanceSetup,
    total: &str,
    status: &str,
    due: Option<NaiveDate>,
) -> SeededCharge {
    let (t, i) = (setup.tenant(), setup.institution());
    let mut tx = owner.begin().await.expect("tx");
    let c = insert_charge(
        &mut tx,
        &NewCharge {
            tenant: t,
            institution: i,
            student_account_id: setup.student_account_id,
            student_id: Some(setup.student.student_id),
            admission_id: None,
            academic_year_id: Some(setup.delivery.base.academic_year_id),
            academic_term_id: Some(setup.delivery.term_id),
            description: "Tuition fee — term 1".into(),
            due_date: due,
            source_type: ChargeSourceType::Manual,
            source_id: None,
            lines: vec![NewChargeLine {
                fee_head_id: setup.fee_head_id,
                description: "Tuition".into(),
                amount: amount(total),
            }],
        },
    )
    .await
    .expect("insert charge");
    let mut entry = None;
    if status != "draft" {
        sqlx::query(
            "UPDATE finance.charge SET status = $2, published_at = CASE WHEN $2 IN \
             ('published', 'active', 'disputed', 'closed') THEN now() END WHERE id = $1",
        )
        .bind(c.id.as_uuid())
        .bind(status)
        .execute(&mut *tx)
        .await
        .expect("seed charge status");
    }
    if matches!(status, "published" | "active" | "disputed" | "closed") {
        let r = posting::post(
            &mut tx,
            t,
            &PostingRequest {
                institution_id: i,
                posting_date: NaiveDate::from_ymd_opt(2026, 7, 1).expect("date"),
                kind: JournalEntryKind::Standard,
                source_type: source::CHARGE.into(),
                source_id: c.id.as_uuid(),
                posting_key: "publish".into(),
                description: format!("Charge {} published", c.charge_number),
                currency: CurrencyCode::INR,
                lines: vec![
                    PostingLine::debit(
                        AccountRef::System(SystemAccount::StudentReceivables),
                        amount(total),
                    )
                    .for_charge(setup.student_account_id, Some(c.id)),
                    PostingLine::credit(
                        AccountRef::System(SystemAccount::FeeIncome),
                        amount(total),
                    ),
                ],
            },
            &seed_actor(),
        )
        .await
        .expect("publication posting");
        entry = Some(r.journal_entry_id);
    }
    tx.commit().await.expect("commit");
    SeededCharge {
        charge_id: c.id,
        charge_number: c.charge_number,
        publication_entry: entry,
    }
}

/// See the module docs. Returns the settlement journal entry.
pub async fn seed_payment(
    owner: &PgPool,
    setup: &FinanceSetup,
    charge: ChargeId,
    paid: &str,
) -> JournalEntryId {
    let (t, i) = (setup.tenant(), setup.institution());
    let actor = seed_actor();
    let mut tx = owner.begin().await.expect("tx");
    let c = lock_charge(&mut tx, t, charge).await.expect("lock charge");
    apply_settlement(&mut tx, &c, amount(paid), JOURNAL_ENTRY_POSTED, &actor)
        .await
        .expect("settlement");
    let r = posting::post(
        &mut tx,
        t,
        &PostingRequest {
            institution_id: i,
            posting_date: NaiveDate::from_ymd_opt(2026, 7, 2).expect("date"),
            kind: JournalEntryKind::Standard,
            source_type: source::PAYMENT.into(),
            source_id: Uuid::now_v7(),
            posting_key: format!("settle:{charge}"),
            description: format!("Payment for {}", c.charge_number),
            currency: CurrencyCode::INR,
            lines: vec![
                PostingLine::debit(AccountRef::System(SystemAccount::BankMain), amount(paid)),
                PostingLine::credit(
                    AccountRef::System(SystemAccount::StudentReceivables),
                    amount(paid),
                )
                .for_charge(setup.student_account_id, Some(charge)),
            ],
        },
        &actor,
    )
    .await
    .expect("settlement posting");
    tx.commit().await.expect("commit");
    r.journal_entry_id
}
