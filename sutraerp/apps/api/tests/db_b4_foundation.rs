//! Batch 4 foundation smoke test: the shared delivery tables and their lifecycle helper, the
//! offering read port, faculty-scoped access, student self-service resolution, and the
//! cross-domain ports Domain D calls (RegistrationReader, CreditPostingPort, stubs).
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use chrono::Utc;
use std::sync::Arc;
use support::*;
use sutra_academic::shared::faculty::{require_teaching, TeachingAccess};
use sutra_academic::shared::lifecycle::{
    change_enrolment_status, change_offering_status, StatusChange,
};
use sutra_academic::shared::offering::{lock_enrolment, lock_offering, seated_roster};
use sutra_academic::shared::Actor;
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId, SectionEnrolmentId};
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, SectionEnrolmentStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::{CreditPostingRequest, CREDIT_SOURCE_FLEXIBLE_LEARNING};
use sutra_kernel::auth::{AuthContext, MfaLevel, Permission, Role};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use uuid::Uuid;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b4f00";

fn auth(subject: &str, role: Role, tenant: TenantId) -> AuthContext {
    AuthContext {
        session_id: Uuid::now_v7(),
        subject: subject.into(),
        login_id: subject.into(),
        display_name: subject.into(),
        roles: vec![role],
        scope: ExecutionScope::tenant(tenant),
        csrf_token: "a".repeat(64),
        expires_at: Utc::now(),
        idle_expires_at: Utc::now(),
        mfa_level: MfaLevel::None,
        amr: vec!["pwd".into()],
        step_up_enforced: false,
    }
}

#[tokio::test]
async fn shared_delivery_tables_ports_and_access() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b4::academic_setup(&app, &admin, &pool, TENANT, "Delivery University").await;
    let tenant = setup.tenant();
    let scope = setup.scope();
    assert_eq!(setup.components.len(), 2, "term-1 components");

    // Seeds: an active offering taught by Meera, Aarav seated in it, a second planned offering.
    let off = b4::seed_offering(&pool, &setup, "CS101", "active").await;
    let planned = b4::seed_offering(&pool, &setup, "MD101", "approved").await;
    b4::seed_faculty(&pool, &setup, &off, b4::MEERA, "active").await;
    b4::seed_faculty(&pool, &setup, &planned, b4::MEERA, "approved").await;
    let aarav = b4::seed_student(&pool, &setup, "Aarav", Some(b4::AARAV)).await;
    let bina = b4::seed_student(&pool, &setup, "Bina", None).await;
    let seat = b4::seed_enrolment(&pool, &setup, &off, &aarav, Some(b4::AARAV)).await;
    b4::seed_enrolment(&pool, &setup, &off, &bina, None).await;

    // ---- the state the composition root builds (ports wired as in production)
    let state = sutra_api::app::academic_deps(
        pool.clone(),
        Arc::new(sutra_platform::workflow::NoopWorkflowAdapter),
    )
    .into_state();

    // ---- offering read port
    let dir = state.offerings.clone();
    let o = dir
        .offering(&scope, CourseOfferingId(off.offering_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(o.status, CourseOfferingStatus::Active);
    assert_eq!(o.curriculum_component_id, off.component_id);
    assert!((o.credits - 12.0).abs() < 1e-9);
    assert_eq!(
        dir.offerings_in_term(&scope, setup.institution(), setup.term_id)
            .await
            .unwrap()
            .len(),
        2
    );
    let sections = dir
        .sections_of_offering(&scope, CourseOfferingId(off.offering_id))
        .await
        .unwrap();
    assert_eq!(sections.len(), 1);
    assert_eq!(sections[0].section_code, "A");
    let section = OfferingSectionId(off.section_id);
    assert_eq!(dir.roster(&scope, section, true).await.unwrap().len(), 2);
    let taught = dir.sections_taught_by(&scope, b4::MEERA).await.unwrap();
    assert_eq!(taught.len(), 1, "only the active assignment counts");
    assert_eq!(taught[0].id, section);
    assert!(dir.teaches(&scope, b4::MEERA, section).await.unwrap());
    assert!(!dir
        .teaches(&scope, b4::MEERA, OfferingSectionId(planned.section_id))
        .await
        .unwrap());
    let faculty = dir.faculty_of_section(&scope, section).await.unwrap();
    assert_eq!(faculty[0].status, FacultyAssignmentStatus::Active);
    let mine = dir
        .sections_of_student(&scope, StudentId(aarav.student_id))
        .await
        .unwrap();
    assert_eq!(mine.len(), 1);
    let foreign = ExecutionScope::tenant(TenantId::new());
    assert!(dir
        .offering(&foreign, CourseOfferingId(off.offering_id))
        .await
        .unwrap()
        .is_none());
    assert!(dir
        .roster(&foreign, section, false)
        .await
        .unwrap()
        .is_empty());

    // ---- faculty-scoped access and student self-service resolution
    let meera = auth(b4::MEERA, Role::Faculty, tenant);
    assert_eq!(
        require_teaching(&state, &meera, section, Permission::AttendanceManage)
            .await
            .unwrap(),
        TeachingAccess::AssignedFaculty
    );
    assert!(matches!(
        require_teaching(
            &state,
            &meera,
            OfferingSectionId(planned.section_id),
            Permission::AttendanceManage
        )
        .await,
        Err(KernelError::Forbidden(_))
    ));
    let admin_ctx = auth("dev|admin@sutra.dev", Role::Administrator, tenant);
    assert_eq!(
        require_teaching(&state, &admin_ctx, section, Permission::AttendanceManage)
            .await
            .unwrap(),
        TeachingAccess::Staff
    );
    let student_ctx = auth(b4::AARAV, Role::Student, tenant);
    assert!(
        require_teaching(&state, &student_ctx, section, Permission::AttendanceManage)
            .await
            .is_err()
    );
    let me = sutra_academic::shared::me::require_seated(&state, &student_ctx, section)
        .await
        .unwrap();
    assert_eq!(me.id, StudentId(aarav.student_id));
    assert!(matches!(
        sutra_academic::shared::me::require_seated(
            &state,
            &student_ctx,
            OfferingSectionId(planned.section_id)
        )
        .await,
        Err(KernelError::NotFound { .. })
    ));

    // ---- lifecycle helper: active → frozen → active; illegal active → draft; history rows
    let actor = Actor {
        id: Some("test".into()),
        correlation_id: None,
    };
    let change = StatusChange {
        cause_event_type: "sutraerp.academic.course_offering.frozen",
        reason: Some("test"),
        ..Default::default()
    };
    let mut tx = pool.begin().await.unwrap();
    let row = lock_offering(&mut tx, tenant, CourseOfferingId(off.offering_id))
        .await
        .unwrap();
    let row = change_offering_status(&mut tx, &row, CourseOfferingStatus::Frozen, &change, &actor)
        .await
        .unwrap();
    assert_eq!(row.version.0, 2);
    assert!(matches!(
        change_offering_status(&mut tx, &row, CourseOfferingStatus::Draft, &change, &actor).await,
        Err(KernelError::Conflict(_))
    ));
    let row = change_offering_status(&mut tx, &row, CourseOfferingStatus::Active, &change, &actor)
        .await
        .unwrap();
    assert_eq!(row.status, CourseOfferingStatus::Active);
    // Seat given up: dropped + ended_at.
    let e = lock_enrolment(&mut tx, tenant, SectionEnrolmentId(seat))
        .await
        .unwrap();
    let e = change_enrolment_status(
        &mut tx,
        &e,
        SectionEnrolmentStatus::Dropped,
        &change,
        &actor,
    )
    .await
    .unwrap();
    assert!(e.ended_at.is_some());
    tx.commit().await.unwrap();
    assert_eq!(
        seated_roster(&pool, tenant, section).await.unwrap().len(),
        1
    );
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.status_history WHERE aggregate_id = ANY($1)",
    )
    .bind(vec![off.offering_id, seat])
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(history, 3);
    assert!(
        sqlx::query("DELETE FROM academic.status_history WHERE aggregate_id = $1")
            .bind(off.offering_id)
            .execute(&pool)
            .await
            .is_err(),
        "history is append-only"
    );
    assert!(
        sqlx::query("DELETE FROM academic.course_offering WHERE id = $1")
            .bind(planned.offering_id)
            .execute(&pool)
            .await
            .is_err(),
        "no physical deletion"
    );
    // One seat per student and offering.
    let reseat = sqlx::query(
        "INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, \
         student_id, student_context_id, student_number, display_name, status) \
         SELECT tenant_id, institution_id, offering_id, section_id, student_id, student_context_id, \
         student_number, display_name, 'enrolled' FROM academic.section_enrolment \
         WHERE student_id = $1 AND status = 'enrolled'",
    )
    .bind(bina.student_id)
    .execute(&pool)
    .await;
    assert!(
        reseat.is_err(),
        "a second live seat in the offering is refused"
    );
    // Terminal offerings are immutable.
    sqlx::query("UPDATE academic.course_offering SET status = 'archived' WHERE id = $1")
        .bind(planned.offering_id)
        .execute(&pool)
        .await
        .unwrap();
    assert!(
        sqlx::query("UPDATE academic.course_offering SET title = 'X' WHERE id = $1")
            .bind(planned.offering_id)
            .execute(&pool)
            .await
            .is_err()
    );

    // ---- RegistrationReader (Domain C, 3B tables) gates 4A enrolment
    let cs = setup.component("CS101").id;
    let md = setup.component("MD101").id;
    assert!(state
        .registrations
        .term_registration(&scope, StudentId(aarav.student_id), setup.term_id)
        .await
        .unwrap()
        .is_none());
    b4::seed_registration(&pool, &setup, &aarav, &[cs], "registered").await;
    let reg = state
        .registrations
        .term_registration(&scope, StudentId(aarav.student_id), setup.term_id)
        .await
        .unwrap()
        .unwrap();
    assert!(reg.is_effective());
    assert!(reg.covers(cs) && !reg.covers(md));
    assert!(state
        .registrations
        .term_registration(&foreign, StudentId(aarav.student_id), setup.term_id)
        .await
        .unwrap()
        .is_none());

    // ---- CreditPostingPort (Domain C, 3B ledger) — idempotent on (source_type, source_id)
    let source = Uuid::now_v7();
    let req = CreditPostingRequest {
        institution_id: setup.institution(),
        student_id: StudentId(aarav.student_id),
        credits: 4.0,
        curriculum_component_id: Some(md),
        academic_term_id: Some(setup.term_id),
        source_type: CREDIT_SOURCE_FLEXIBLE_LEARNING,
        source_id: source,
        note: Some("SWAYAM course recognised".into()),
        actor: Some("dev|admin@sutra.dev".into()),
    };
    let first = state
        .credits
        .post_credits(&scope, req.clone())
        .await
        .unwrap();
    assert!(!first.already_posted);
    let again = state
        .credits
        .post_credits(&scope, req.clone())
        .await
        .unwrap();
    assert!(again.already_posted);
    assert_eq!(again.ledger_entry_id, first.ledger_entry_id);
    let (kind, credits): (String, f64) = sqlx::query_as(
        "SELECT kind, credits::float8 FROM student.credit_ledger_entry WHERE id = $1",
    )
    .bind(first.ledger_entry_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!((kind.as_str(), credits), ("recognised", 4.0));
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = 'sutraerp.student.credit_ledger.updated' \
         AND subject = $1",
    )
    .bind(aarav.student_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 1, "one ledger event for one posting");
    assert!(state
        .credits
        .post_credits(
            &scope,
            CreditPostingRequest {
                credits: 0.0,
                source_id: Uuid::now_v7(),
                ..req.clone()
            }
        )
        .await
        .is_err());

    // ---- stubs until Batches 7/8
    assert!(state
        .spaces
        .space(&scope, Uuid::now_v7())
        .await
        .unwrap()
        .is_none());
    let staff = state
        .staff
        .staff_by_subject(&scope, b4::MEERA)
        .await
        .unwrap()
        .unwrap();
    assert!(staff.active && staff.staff_id.is_none());
    assert!(state
        .staff
        .staff_by_subject(&scope, " ")
        .await
        .unwrap()
        .is_none());
}
