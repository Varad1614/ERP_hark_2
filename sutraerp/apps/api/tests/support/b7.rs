//! Batch 7 foundation test helpers (foundation-owned; slices add their own helpers in their own
//! test file, see `docs/batch7-contracts.md` §11).
//!
//! * [`people_setup`] — Batch 4's Domain B setup (institution + published programme / curriculum
//!   + published calendar) with the institution set **active** and the default chart of accounts
//!   (payroll postings land in its ledger).
//! * [`seed_staff`] — a staff record (`active`) through `sutra_people::shared::staff::insert_staff`
//!   and, optionally, an engagement in any [`EngagementStatus`] through `insert_engagement` +
//!   the shared lifecycle (exactly the shape 7A's API writes), optionally linked to a login
//!   (`Some(b4::MEERA)`), so 7B / 7C never wait for 7A.
//! * [`set_manager`] — sets the engagement's reporting manager (what 7A's reporting-line command
//!   does), for 7B manager approvals.
//! * [`seed_leave_entry`] — one leave ledger entry through `shared::leave::post_entry`, exactly
//!   like 7B will (opening balances, unpaid consumption, encashment), so 7C can read payroll
//!   inputs without 7B's API.
//! * [`seed_attendance`] — one attendance day through `shared::attendance::record_day`.

use super::b4::{self, DeliverySetup};
use super::Login;
use axum::Router;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::PgPool;
use std::str::FromStr;
use sutra_contracts::people::ids::{LeaveLedgerEntryId, StaffEngagementId, StaffMemberId};
use sutra_contracts::people::status::{
    AttendanceDayStatus, AttendanceSource, EmploymentType, EngagementStatus, LeaveEntryKind,
    StaffCategory, StaffStatus,
};
use sutra_finance::shared::coa::ensure_default_chart;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_people::shared::attendance::{record_day, DayRecord};
use sutra_people::shared::leave::{post_entry, NewLeaveEntry};
use sutra_people::shared::lifecycle::{change_engagement_status, StatusChange};
use sutra_people::shared::rows::lock_engagement;
use sutra_people::shared::staff::{insert_engagement, insert_staff, NewEngagement, NewStaff};
use sutra_people::shared::Actor;
use uuid::Uuid;

/// Decimal from a literal (`"12.50"`).
pub fn dec(s: &str) -> Decimal {
    Decimal::from_str(s).expect("decimal literal")
}

/// `YYYY-MM-DD` date literal.
pub fn date(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("date literal")
}

/// The actor recorded by seeds.
pub fn seed_actor() -> Actor {
    Actor {
        id: Some("test:b7-seed".into()),
        correlation_id: None,
    }
}

/// Domain B / F context a Domain E test needs.
#[derive(Clone, Debug)]
pub struct PeopleSetup {
    pub delivery: DeliverySetup,
}

impl PeopleSetup {
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

/// See the module docs.
pub async fn people_setup(
    app: &Router,
    admin: &Login,
    owner: &PgPool,
    tenant: &str,
    name: &str,
) -> PeopleSetup {
    let delivery = b4::academic_setup(app, admin, owner, tenant, name).await;
    sqlx::query("UPDATE institution.institution SET status = 'active' WHERE id = $1")
        .bind(delivery.base.institution_id)
        .execute(owner)
        .await
        .expect("activate institution");
    let mut tx = owner.begin().await.expect("tx");
    ensure_default_chart(&mut tx, delivery.tenant(), delivery.institution())
        .await
        .expect("chart");
    tx.commit().await.expect("commit");
    PeopleSetup { delivery }
}

/// A seeded staff member.
#[derive(Clone, Debug)]
pub struct SeededStaff {
    pub staff_id: StaffMemberId,
    pub staff_number: String,
    pub engagement_id: Option<StaffEngagementId>,
    pub subject_ref: Option<String>,
}

/// An active staff record (`given_name` + "Seeded"), optionally linked to `subject_ref`, with an
/// engagement in `engagement` status (`None` = no engagement; `cancelled` is not seedable from
/// `active` — seed `pending` and cancel it through the lifecycle). `closed` engagements get
/// `last_working_date = 2026-11-15`; every engagement starts on 2026-06-01 (`Assistant
/// Professor`, grade `AP-1`, permanent).
pub async fn seed_staff(
    owner: &PgPool,
    setup: &PeopleSetup,
    given_name: &str,
    subject_ref: Option<&str>,
    category: StaffCategory,
    engagement: Option<EngagementStatus>,
) -> SeededStaff {
    let (t, i) = (setup.tenant(), setup.institution());
    let actor = seed_actor();
    let email = format!("{}@staff.sutra.dev", given_name.to_lowercase());
    let mut tx = owner.begin().await.expect("tx");
    let staff = insert_staff(
        &mut tx,
        &NewStaff {
            tenant: t,
            institution: i,
            subject_ref,
            given_name,
            family_name: Some("Seeded"),
            display_name: None,
            work_email: Some(&email),
            phone: None,
            date_of_birth: None,
            gender: None,
            staff_category: category,
            status: StaffStatus::Active,
        },
        &actor,
    )
    .await
    .expect("seed staff");
    let mut engagement_id = None;
    if let Some(target) = engagement {
        let initial = if target == EngagementStatus::Pending {
            EngagementStatus::Pending
        } else {
            EngagementStatus::Active
        };
        let e = insert_engagement(
            &mut tx,
            &NewEngagement {
                tenant: t,
                institution: i,
                staff_id: staff.id,
                org_unit_id: None,
                campus_id: None,
                staff_category: category,
                employment_type: EmploymentType::Permanent,
                designation: "Assistant Professor",
                grade: Some("AP-1"),
                start_date: date("2026-06-01"),
                end_date: None,
                probation_end_date: None,
                reports_to_staff_id: None,
                rejoin_of: None,
                status: initial,
            },
            &actor,
        )
        .await
        .expect("seed engagement");
        if !matches!(target, EngagementStatus::Pending | EngagementStatus::Active) {
            if target == EngagementStatus::Closed {
                sqlx::query(
                    "UPDATE people.staff_engagement SET last_working_date = DATE '2026-11-15' \
                     WHERE id = $1",
                )
                .bind(e.id.as_uuid())
                .execute(&mut *tx)
                .await
                .expect("last working date");
            }
            let locked = lock_engagement(&mut tx, t, e.id).await.expect("lock");
            change_engagement_status(
                &mut tx,
                t,
                &locked,
                target,
                &StatusChange {
                    cause_event_type: "test.seed",
                    reason: Some("seeded"),
                    process_instance_id: None,
                },
                &actor,
            )
            .await
            .expect("seed engagement status");
        }
        engagement_id = Some(e.id);
    }
    tx.commit().await.expect("commit");
    SeededStaff {
        staff_id: staff.id,
        staff_number: staff.staff_number,
        engagement_id,
        subject_ref: subject_ref.map(str::to_owned),
    }
}

/// Sets `staff`'s engagement reporting manager to `manager` (what 7A's reporting-line command
/// writes).
pub async fn set_manager(owner: &PgPool, staff: &SeededStaff, manager: &SeededStaff) {
    sqlx::query(
        "UPDATE people.staff_engagement SET reports_to_staff_id = $2, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(staff.engagement_id.expect("engagement").as_uuid())
    .bind(manager.staff_id.as_uuid())
    .execute(owner)
    .await
    .expect("set manager");
}

/// One leave ledger entry of `staff` (`days` signed, e.g. `"12.00"` opening, `"-2.00"`
/// consumption) for leave type `code` (`leave_type_id` any stable uuid per code in the test).
#[allow(clippy::too_many_arguments)]
pub async fn seed_leave_entry(
    owner: &PgPool,
    setup: &PeopleSetup,
    staff: &SeededStaff,
    leave_type_id: Uuid,
    code: &str,
    is_paid: bool,
    kind: LeaveEntryKind,
    days: &str,
    effective_on: &str,
    posting_key: &str,
) -> LeaveLedgerEntryId {
    let on = date(effective_on);
    let mut tx = owner.begin().await.expect("tx");
    let r = post_entry(
        &mut tx,
        &NewLeaveEntry {
            tenant: setup.tenant(),
            institution: setup.institution(),
            staff_id: staff.staff_id,
            engagement_id: staff.engagement_id,
            leave_type_id,
            leave_type_code: code,
            is_paid,
            kind,
            days: dec(days),
            effective_on: on,
            leave_year: chrono::Datelike::year(&on),
            source_type: "test_seed",
            source_id: staff.staff_id.as_uuid(),
            posting_key,
            reason: Some("seeded"),
        },
        &seed_actor(),
    )
    .await
    .expect("seed leave entry");
    tx.commit().await.expect("commit");
    r.entry_id
}

/// One attendance day of `staff`.
pub async fn seed_attendance(
    owner: &PgPool,
    setup: &PeopleSetup,
    staff: &SeededStaff,
    work_date: &str,
    status: AttendanceDayStatus,
) {
    let mut tx = owner.begin().await.expect("tx");
    record_day(
        &mut tx,
        &DayRecord {
            tenant: setup.tenant(),
            institution: setup.institution(),
            staff_id: staff.staff_id,
            work_date: date(work_date),
            status,
            source: AttendanceSource::Manual,
            leave_request_id: None,
            remark: None,
        },
        &seed_actor(),
    )
    .await
    .expect("seed attendance");
    tx.commit().await.expect("commit");
}
