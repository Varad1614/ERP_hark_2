//! Batch 7 foundation smoke test: migrations + the shared People tables (append-only leave
//! ledger at trigger level), the shared staff / lifecycle / leave / attendance helpers (one live
//! staff record per login, one live engagement per staff member, legal transitions only,
//! idempotent signed ledger entries, reversals, balance projection), the real
//! `WorkforceDirectory` / `LeaveReader` / `StaffDirectory` adapters, the staff-directory API
//! (permissions, self-service, search), and the cross-domain ports wired for Batch 7
//! (`FacultyWorkloadPort` over 4A, `PayrollPostingPort` → a balanced `payroll_run` journal entry
//! in Finance).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use axum::http::StatusCode;
use std::sync::Arc;
use support::b7::{self, date, dec, seed_actor};
use support::*;
use sutra_contracts::finance::ports::PayrollPostingRequest;
use sutra_contracts::people::status::{
    AttendanceDayStatus, AttendanceSource, EmploymentType, EngagementStatus, LeaveEntryKind,
    StaffCategory, StaffStatus,
};
use sutra_contracts::people::StaffDirectory;
use sutra_kernel::error::KernelError;
use sutra_people::shared::attendance::{record_day, DayRecord};
use sutra_people::shared::leave::{balance, post_entry, reverse_entry, NewLeaveEntry};
use sutra_people::shared::lifecycle::{
    change_engagement_status, change_staff_status, StatusChange,
};
use sutra_people::shared::rows::{lock_engagement, lock_staff};
use sutra_people::shared::staff::{insert_engagement, insert_staff, NewEngagement, NewStaff};
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b7f00";
const PEOPLE: &str = "/api/v1/people";

fn change(cause: &'static str) -> StatusChange<'static> {
    StatusChange {
        cause_event_type: cause,
        reason: Some("db_b7_foundation"),
        process_instance_id: None,
    }
}

#[tokio::test]
async fn people_foundation_tables_helpers_ports_and_directory_api() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let meera = login(&app, "meera.shah@sutra.dev").await;
    let aarav = login(&app, "aarav.desai@sutra.dev").await;
    let setup = b7::people_setup(&app, &admin, &pool, TENANT, "People University").await;
    let (t, scope) = (setup.tenant(), setup.scope());
    let inst = setup.institution().as_uuid();
    let state = sutra_api::app::people_deps_with(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
        // Part 1: explicit dev/test auto-approval opt-in (the subject here is not approval).
        sutra_kernel::approval::ApprovalMode::AutoApprove,
    )
    .into_state();

    // ---- migrations: every foundation table exists.
    for table in [
        "staff_member",
        "staff_engagement",
        "leave_ledger_entry",
        "attendance_day",
        "number_sequence",
        "status_history",
    ] {
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM information_schema.tables WHERE table_schema = 'people' \
             AND table_name = $1",
        )
        .bind(table)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(n, 1, "people.{table}");
    }

    // ---- staff records + engagements through the shared inserters.
    let meera_staff = b7::seed_staff(
        &pool,
        &setup,
        "Meera",
        Some(b4::MEERA),
        StaffCategory::Teaching,
        Some(EngagementStatus::Active),
    )
    .await;
    assert!(meera_staff.staff_number.starts_with("EMP-"));
    let meera_engagement = meera_staff.engagement_id.unwrap();

    let mut tx = pool.begin().await.unwrap();
    // One live staff record per login subject.
    let dup = insert_staff(
        &mut tx,
        &NewStaff {
            tenant: t,
            institution: setup.institution(),
            subject_ref: Some(b4::MEERA),
            given_name: "Meera",
            family_name: Some("Duplicate"),
            display_name: None,
            work_email: None,
            phone: None,
            date_of_birth: None,
            gender: None,
            staff_category: StaffCategory::Teaching,
            status: StaffStatus::Draft,
        },
        &seed_actor(),
    )
    .await;
    assert!(matches!(dup, Err(KernelError::Conflict(_))), "{dup:?}");
    // One live engagement per staff member.
    let second = insert_engagement(
        &mut tx,
        &NewEngagement {
            tenant: t,
            institution: setup.institution(),
            staff_id: meera_staff.staff_id,
            org_unit_id: None,
            campus_id: None,
            staff_category: StaffCategory::Teaching,
            employment_type: EmploymentType::Contract,
            designation: "Visiting Professor",
            grade: None,
            start_date: date("2026-07-01"),
            end_date: None,
            probation_end_date: None,
            reports_to_staff_id: None,
            rejoin_of: None,
            status: EngagementStatus::Pending,
        },
        &seed_actor(),
    )
    .await;
    assert!(
        matches!(second, Err(KernelError::Conflict(_))),
        "{second:?}"
    );
    // Invalid initial status / dates are validation errors.
    let bad = insert_engagement(
        &mut tx,
        &NewEngagement {
            tenant: t,
            institution: setup.institution(),
            staff_id: meera_staff.staff_id,
            org_unit_id: None,
            campus_id: None,
            staff_category: StaffCategory::Teaching,
            employment_type: EmploymentType::Contract,
            designation: "Visiting Professor",
            grade: None,
            start_date: date("2026-07-01"),
            end_date: Some(date("2026-06-01")),
            probation_end_date: None,
            reports_to_staff_id: None,
            rejoin_of: None,
            status: EngagementStatus::Pending,
        },
        &seed_actor(),
    )
    .await;
    assert!(matches!(bad, Err(KernelError::Validation(_))), "{bad:?}");

    // ---- shared lifecycle: illegal moves are 409, legal ones bump the version + history.
    let e = lock_engagement(&mut tx, t, meera_engagement).await.unwrap();
    assert_eq!(e.status, EngagementStatus::Active);
    let illegal = change_engagement_status(
        &mut tx,
        t,
        &e,
        EngagementStatus::Pending,
        &change("test.illegal"),
        &seed_actor(),
    )
    .await;
    assert!(matches!(illegal, Err(KernelError::Conflict(_))));
    let on_leave = change_engagement_status(
        &mut tx,
        t,
        &e,
        EngagementStatus::LongLeave,
        &change("test.long_leave"),
        &seed_actor(),
    )
    .await
    .unwrap();
    assert_eq!(on_leave.version, e.version + 1);
    let back = change_engagement_status(
        &mut tx,
        t,
        &on_leave,
        EngagementStatus::Active,
        &change("test.long_leave_end"),
        &seed_actor(),
    )
    .await
    .unwrap();
    assert_eq!(back.status, EngagementStatus::Active);
    let s = lock_staff(&mut tx, t, meera_staff.staff_id).await.unwrap();
    let illegal = change_staff_status(
        &mut tx,
        t,
        &s,
        StaffStatus::Draft,
        &change("test.illegal"),
        &seed_actor(),
    )
    .await;
    assert!(matches!(illegal, Err(KernelError::Conflict(_))));
    tx.commit().await.unwrap();
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM people.status_history WHERE aggregate_type = 'staff_engagement' \
         AND aggregate_id = $1",
    )
    .bind(meera_engagement.as_uuid())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(history, 3, "created + long_leave + active");

    // ---- more staff for the directory / payroll population.
    let ravi = b7::seed_staff(
        &pool,
        &setup,
        "Ravi",
        Some("dev|ravi.closed@sutra.dev"),
        StaffCategory::NonTeaching,
        Some(EngagementStatus::Closed),
    )
    .await;
    let sana = b7::seed_staff(
        &pool,
        &setup,
        "Sana",
        Some("dev|sana.suspended@sutra.dev"),
        StaffCategory::Teaching,
        Some(EngagementStatus::Suspended),
    )
    .await;
    let kiran = b7::seed_staff(
        &pool,
        &setup,
        "Kiran",
        None,
        StaffCategory::Administrative,
        Some(EngagementStatus::Active),
    )
    .await;
    let pending = b7::seed_staff(
        &pool,
        &setup,
        "Pending",
        None,
        StaffCategory::Administrative,
        Some(EngagementStatus::Pending),
    )
    .await;
    b7::set_manager(&pool, &kiran, &meera_staff).await;

    // ---- leave ledger: idempotent, signed, reversible, append-only; balance projection.
    let cl = Uuid::now_v7();
    let lop = Uuid::now_v7();
    let opening = b7::seed_leave_entry(
        &pool,
        &setup,
        &meera_staff,
        cl,
        "CL",
        true,
        LeaveEntryKind::Opening,
        "12.00",
        "2026-01-01",
        "opening",
    )
    .await;
    let again = b7::seed_leave_entry(
        &pool,
        &setup,
        &meera_staff,
        cl,
        "CL",
        true,
        LeaveEntryKind::Opening,
        "12.00",
        "2026-01-01",
        "opening",
    )
    .await;
    assert_eq!(opening, again, "same idempotency key → same entry");
    for (kind, days, on, key) in [
        (
            LeaveEntryKind::Reservation,
            "-2.00",
            "2026-11-02",
            "reserve",
        ),
        (LeaveEntryKind::Release, "2.00", "2026-11-03", "release"),
    ] {
        b7::seed_leave_entry(
            &pool,
            &setup,
            &meera_staff,
            cl,
            "CL",
            true,
            kind,
            days,
            on,
            key,
        )
        .await;
    }
    let consumed = b7::seed_leave_entry(
        &pool,
        &setup,
        &meera_staff,
        cl,
        "CL",
        true,
        LeaveEntryKind::Consumption,
        "-2.00",
        "2026-11-03",
        "consume:2026-11",
    )
    .await;
    let mut tx = pool.begin().await.unwrap();
    let wrong_sign = post_entry(
        &mut tx,
        &NewLeaveEntry {
            tenant: t,
            institution: setup.institution(),
            staff_id: meera_staff.staff_id,
            engagement_id: Some(meera_engagement),
            leave_type_id: cl,
            leave_type_code: "CL",
            is_paid: true,
            kind: LeaveEntryKind::Reservation,
            days: dec("1.00"),
            effective_on: date("2026-11-05"),
            leave_year: 2026,
            source_type: "test_seed",
            source_id: Uuid::now_v7(),
            posting_key: "reserve",
            reason: None,
        },
        &seed_actor(),
    )
    .await;
    assert!(matches!(wrong_sign, Err(KernelError::Validation(_))));
    let b = balance(&mut tx, t, meera_staff.staff_id, cl, 2026)
        .await
        .unwrap();
    assert_eq!(
        (b.entitled, b.reserved, b.consumed, b.available),
        (dec("12.00"), dec("0.00"), dec("2.00"), dec("10.00"))
    );
    let rev = reverse_entry(
        &mut tx,
        t,
        consumed,
        "reverse:consume:2026-11",
        date("2026-11-04"),
        "leave cancelled",
        &seed_actor(),
    )
    .await
    .unwrap();
    assert!(!rev.already_posted);
    let rev2 = reverse_entry(
        &mut tx,
        t,
        consumed,
        "reverse:consume:2026-11",
        date("2026-11-04"),
        "leave cancelled",
        &seed_actor(),
    )
    .await
    .unwrap();
    assert!(rev2.already_posted && rev2.entry_id == rev.entry_id);
    let twice = reverse_entry(
        &mut tx,
        t,
        consumed,
        "reverse-again",
        date("2026-11-04"),
        "again",
        &seed_actor(),
    )
    .await;
    assert!(matches!(twice, Err(KernelError::Conflict(_))));
    tx.commit().await.unwrap();
    let balances = state
        .leave
        .balances(&scope, meera_staff.staff_id, 2026)
        .await
        .unwrap();
    assert_eq!(balances.len(), 1);
    assert_eq!(balances[0].leave_type_code, "CL");
    assert_eq!(
        (balances[0].consumed, balances[0].available),
        (dec("0.00"), dec("12.00"))
    );
    let rewrite = sqlx::query("UPDATE people.leave_ledger_entry SET days = 1 WHERE id = $1")
        .bind(opening.as_uuid())
        .execute(&pool)
        .await;
    assert!(rewrite.is_err(), "the leave ledger is append-only");
    let delete = sqlx::query("DELETE FROM people.leave_ledger_entry WHERE id = $1")
        .bind(opening.as_uuid())
        .execute(&pool)
        .await;
    assert!(delete.is_err(), "the leave ledger is append-only");

    // ---- payroll inputs: unpaid leave + attendance absence + encashment of the period.
    b7::seed_leave_entry(
        &pool,
        &setup,
        &meera_staff,
        lop,
        "LOP",
        false,
        LeaveEntryKind::Consumption,
        "-1.00",
        "2026-11-10",
        "lop:consume:2026-11",
    )
    .await;
    b7::seed_leave_entry(
        &pool,
        &setup,
        &meera_staff,
        cl,
        "CL",
        true,
        LeaveEntryKind::Encashment,
        "-3.00",
        "2026-11-20",
        "encash",
    )
    .await;
    b7::seed_attendance(
        &pool,
        &setup,
        &meera_staff,
        "2026-11-12",
        AttendanceDayStatus::Absent,
    )
    .await;
    b7::seed_attendance(
        &pool,
        &setup,
        &meera_staff,
        "2026-11-13",
        AttendanceDayStatus::HalfDay,
    )
    .await;
    b7::seed_attendance(
        &pool,
        &setup,
        &meera_staff,
        "2026-11-14",
        AttendanceDayStatus::Present,
    )
    .await;
    // A correction keeps one row per day and records history.
    let mut tx = pool.begin().await.unwrap();
    let corrected = record_day(
        &mut tx,
        &DayRecord {
            tenant: t,
            institution: setup.institution(),
            staff_id: meera_staff.staff_id,
            work_date: date("2026-11-14"),
            status: AttendanceDayStatus::Absent,
            source: AttendanceSource::Correction,
            leave_request_id: None,
            remark: Some("marked absent by mistake as present"),
        },
        &seed_actor(),
    )
    .await
    .unwrap();
    assert_eq!(corrected.version, 2);
    let restored = record_day(
        &mut tx,
        &DayRecord {
            tenant: t,
            institution: setup.institution(),
            staff_id: meera_staff.staff_id,
            work_date: date("2026-11-14"),
            status: AttendanceDayStatus::Present,
            source: AttendanceSource::Correction,
            leave_request_id: None,
            remark: Some("present after all"),
        },
        &seed_actor(),
    )
    .await
    .unwrap();
    assert_eq!((restored.id, restored.version), (corrected.id, 3));
    tx.commit().await.unwrap();
    let inputs = state
        .leave
        .payroll_inputs(
            &scope,
            meera_staff.staff_id,
            date("2026-11-01"),
            date("2026-11-30"),
        )
        .await
        .unwrap();
    assert_eq!(inputs.unpaid_leave_days, dec("1.00"));
    assert_eq!(inputs.absent_days, dec("1.50"));
    assert_eq!(inputs.lop_days(), dec("2.50"));
    assert_eq!(inputs.encashments.len(), 1);
    assert_eq!(inputs.encashments[0].days, dec("3.00"));
    let december = state
        .leave
        .payroll_inputs(
            &scope,
            meera_staff.staff_id,
            date("2026-12-01"),
            date("2026-12-31"),
        )
        .await
        .unwrap();
    assert_eq!(december.lop_days(), dec("0"));
    assert!(december.encashments.is_empty());

    // ---- WorkforceDirectory: subject resolution, payroll population, reports.
    let by_subject = state
        .directory
        .staff_by_subject(&scope, b4::MEERA)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_subject.id, meera_staff.staff_id);
    let november: Vec<_> = state
        .directory
        .payable_engagements(
            &scope,
            setup.institution(),
            date("2026-11-01"),
            date("2026-11-30"),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.staff_id)
        .collect();
    for s in [&meera_staff, &ravi, &sana, &kiran] {
        assert!(
            november.contains(&s.staff_id),
            "{} payable in November",
            s.staff_number
        );
    }
    assert!(
        !november.contains(&pending.staff_id),
        "pending engagements are not payable"
    );
    let december: Vec<_> = state
        .directory
        .payable_engagements(
            &scope,
            setup.institution(),
            date("2026-12-01"),
            date("2026-12-31"),
        )
        .await
        .unwrap()
        .into_iter()
        .map(|e| e.staff_id)
        .collect();
    assert!(!december.contains(&ravi.staff_id), "closed in November");
    let reports = state
        .directory
        .reports_of(&scope, meera_staff.staff_id)
        .await
        .unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].staff_id, kiran.staff_id);

    // ---- StaffDirectory for Domain D: real record, active only while working, fallback.
    let staff_port = sutra_people::PgStaffDirectory::new(pool.clone());
    let m = staff_port
        .staff_by_subject(&scope, b4::MEERA)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(m.staff_id, Some(meera_staff.staff_id.as_uuid()));
    assert!(m.active);
    assert_eq!(m.display_name.as_deref(), Some("Meera Seeded"));
    let s = staff_port
        .staff_by_subject(&scope, "dev|sana.suspended@sutra.dev")
        .await
        .unwrap()
        .unwrap();
    assert!(
        s.staff_id.is_some() && !s.active,
        "suspended staff get no new assignments"
    );
    let unknown = staff_port
        .staff_by_subject(&scope, "dev|nobody@sutra.dev")
        .await
        .unwrap()
        .unwrap();
    assert!(
        unknown.staff_id.is_none() && unknown.active,
        "Batch 4 fallback"
    );
    assert!(staff_port
        .staff_by_subject(&scope, " ")
        .await
        .unwrap()
        .is_none());

    // ---- staff-directory API.
    let (s, _, body) = send(
        &app,
        get(&admin, &format!("{PEOPLE}/institutions/{inst}/staff")),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let me = items
        .iter()
        .find(|i| i["staffNumber"] == meera_staff.staff_number.as_str())
        .unwrap();
    assert_eq!(me["engagement"]["status"], "active");
    assert_eq!(me["staffCategory"], "teaching");
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!("{PEOPLE}/institutions/{inst}/staff?q=meera&limit=1"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 1);
    assert!(body["nextCursor"].is_null());
    let (s, _, body) = send(
        &app,
        get(
            &admin,
            &format!("{PEOPLE}/institutions/{inst}/staff?limit=2"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["items"].as_array().unwrap().len(), 2);
    assert!(body["nextCursor"].is_string());
    let (s, _, _) = send(
        &app,
        get(
            &admin,
            &format!("{PEOPLE}/institutions/{inst}/staff?status=bogus"),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY);
    let (s, _, _) = send(
        &app,
        get(&meera, &format!("{PEOPLE}/institutions/{inst}/staff")),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "faculty do not read HR records");
    let (s, _, body) = send(
        &app,
        get(&admin, &format!("{PEOPLE}/staff/{}", meera_staff.staff_id)),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["displayName"], "Meera Seeded");
    let (s, _, _) = send(
        &app,
        get(&admin, &format!("{PEOPLE}/staff/{}", Uuid::now_v7())),
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _, body) = send(&app, get(&meera, &format!("{PEOPLE}/me/staff"))).await;
    assert_eq!(s, StatusCode::OK, "{body}");
    assert_eq!(body["staffNumber"], meera_staff.staff_number.as_str());
    assert_eq!(body["engagement"]["designation"], "Assistant Professor");
    let (s, _, _) = send(&app, get(&admin, &format!("{PEOPLE}/me/staff"))).await;
    assert_eq!(
        s,
        StatusCode::NOT_FOUND,
        "the admin login has no staff record"
    );
    let (s, _, _) = send(&app, get(&aarav, &format!("{PEOPLE}/me/staff"))).await;
    assert_eq!(s, StatusCode::FORBIDDEN, "students have no HR self-service");
    let (s, _, body) = send(
        &app,
        get(&meera, &format!("{PEOPLE}/institutions/{inst}/directory")),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{body}");
    let names: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["staffNumber"].as_str().unwrap())
        .collect();
    assert!(names.contains(&meera_staff.staff_number.as_str()));
    assert!(names.contains(&kiran.staff_number.as_str()));
    assert!(
        !names.contains(&ravi.staff_number.as_str()),
        "closed engagement"
    );
    assert!(
        !names.contains(&sana.staff_number.as_str()),
        "suspended engagement"
    );
    assert!(
        !names.contains(&pending.staff_number.as_str()),
        "not started"
    );

    // ---- FacultyWorkloadPort over 4A faculty assignments.
    let offering = b4::seed_offering(&pool, &setup.delivery, "CS101", "active").await;
    b4::seed_faculty(&pool, &setup.delivery, &offering, b4::MEERA, "active").await;
    let w = state
        .workload
        .teaching_load(&scope, setup.institution(), b4::MEERA, None)
        .await
        .unwrap();
    assert_eq!(w.items.len(), 1);
    assert_eq!(w.items[0].section_id.as_uuid(), offering.section_id);
    assert_eq!(w.total_weekly_hours, dec("4"));
    let other_term = state
        .workload
        .teaching_load(
            &scope,
            setup.institution(),
            b4::MEERA,
            Some(setup.delivery.base.term_ids[1]),
        )
        .await
        .unwrap();
    assert!(other_term.items.is_empty());
    assert_eq!(other_term.total_weekly_hours, dec("0"));

    // ---- PayrollPostingPort → one balanced, idempotent payroll_run journal entry in Finance.
    let run = Uuid::now_v7();
    let req = PayrollPostingRequest {
        institution_id: setup.institution(),
        payroll_run_id: run,
        posting_key: "finalise".into(),
        posting_date: date("2026-11-30"),
        description: "Payroll November 2026".into(),
        gross_earnings: dec("50000.00"),
        employee_deductions: dec("6200.00"),
        employer_contributions: dec("1800.00"),
        net_pay: dec("43800.00"),
        actor: Some("test:b7".into()),
    };
    let posted = state
        .postings
        .post_payroll(&scope, req.clone())
        .await
        .unwrap();
    assert!(!posted.already_posted);
    assert!(
        posted.entry_number.starts_with("JE-"),
        "{}",
        posted.entry_number
    );
    let replay = state
        .postings
        .post_payroll(&scope, req.clone())
        .await
        .unwrap();
    assert!(replay.already_posted);
    assert_eq!(replay.journal_entry_id, posted.journal_entry_id);
    let (debit, credit, lines): (rust_decimal::Decimal, rust_decimal::Decimal, i64) =
        sqlx::query_as(
            "SELECT sum(debit), sum(credit), count(*) FROM finance.journal_line \
             WHERE journal_entry_id = $1",
        )
        .bind(posted.journal_entry_id.as_uuid())
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(
        (debit, credit, lines),
        (dec("51800.00"), dec("51800.00"), 3)
    );
    let source: String =
        sqlx::query_scalar("SELECT source_type FROM finance.journal_entry WHERE id = $1")
            .bind(posted.journal_entry_id.as_uuid())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(source, "payroll_run");
    let payroll_accounts: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM finance.ledger_account WHERE institution_id = $1 AND system_key IN \
         ('salary_expense', 'payroll_payable', 'payroll_deductions_payable')",
    )
    .bind(inst)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(payroll_accounts, 3);
    // A corrective (negative) posting flips the sides.
    let recovery = state
        .postings
        .post_payroll(
            &scope,
            PayrollPostingRequest {
                posting_key: "adjustment:recovery".into(),
                gross_earnings: dec("-500.00"),
                employee_deductions: dec("0"),
                employer_contributions: dec("0"),
                net_pay: dec("-500.00"),
                ..req.clone()
            },
        )
        .await
        .unwrap();
    assert!(!recovery.already_posted);
    let invalid = state
        .postings
        .post_payroll(
            &scope,
            PayrollPostingRequest {
                posting_key: "bad".into(),
                net_pay: dec("1.00"),
                ..req.clone()
            },
        )
        .await;
    assert!(
        matches!(invalid, Err(KernelError::Validation(_))),
        "{invalid:?}"
    );
}
