//! Batch 3 foundation smoke test: the shared Student tables, the lifecycle helper, the student
//! directory port and the Domain B reference port.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::*;
use sutra_contracts::institution::reference::InstitutionReferencePort;
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_contracts::student::ports::StudentDirectory;
use sutra_contracts::student::status::{StudentContextStatus, StudentStatus};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_student::shared::lifecycle::{
    change_context_status, change_student_status, StatusChange,
};
use sutra_student::shared::student::{live_context, lock_context, lock_student};
use sutra_student::shared::Actor;

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b3f00";

#[tokio::test]
async fn shared_student_aggregate_lifecycle_and_ports() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let setup = b3::academic_setup(&app, &admin, TENANT, "Foundation University").await;
    let tenant: TenantId = TENANT.parse().unwrap();
    let inst = InstitutionId(setup.institution_id);
    let scope = ExecutionScope::institution(tenant, inst);

    // Domain B reference port.
    let refs = sutra_institution::PgInstitutionReferences::new(pool.clone());
    let i = refs.institution(&scope, inst).await.unwrap().unwrap();
    assert_eq!(i.timezone, "Asia/Kolkata");
    let cv = refs
        .curriculum_version(&scope, setup.curriculum_version_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cv.programme_id, setup.programme_id);
    assert!(!refs
        .curriculum_components(&scope, cv.id)
        .await
        .unwrap()
        .is_empty());
    let term = refs
        .academic_term(&scope, setup.term_ids[0])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(term.sequence, 1);
    let foreign = ExecutionScope::tenant(TenantId::new());
    assert!(refs.institution(&foreign, inst).await.unwrap().is_none());

    // Seeded student + directory port.
    let s = b3::seed_student(&pool, &setup, "Asha", None).await;
    let dir = sutra_student::PgStudentDirectory::new(pool.clone());
    let sum = dir
        .student(&scope, StudentId(s.student_id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sum.status, StudentStatus::Active);
    let live = dir
        .live_context(&scope, StudentId(s.student_id), inst)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(live.status, StudentContextStatus::Active);

    // Lifecycle helper: active → suspended → active → withdrawn, then persona archived.
    let actor = Actor {
        id: Some("test".into()),
        correlation_id: None,
    };
    let mut tx = pool.begin().await.unwrap();
    let ctx = lock_context(&mut tx, tenant, StudentContextId(s.context_id))
        .await
        .unwrap();
    let change = StatusChange {
        cause_event_type: "sutraerp.student.student.suspended",
        reason: Some("test"),
        ..Default::default()
    };
    let ctx = change_context_status(
        &mut tx,
        &ctx,
        StudentContextStatus::Suspended,
        &change,
        &actor,
    )
    .await
    .unwrap();
    // Suspended → qualification_completed is illegal.
    assert!(change_context_status(
        &mut tx,
        &ctx,
        StudentContextStatus::QualificationCompleted,
        &change,
        &actor
    )
    .await
    .is_err());
    let ctx = change_context_status(&mut tx, &ctx, StudentContextStatus::Active, &change, &actor)
        .await
        .unwrap();
    let student = lock_student(&mut tx, tenant, StudentId(s.student_id))
        .await
        .unwrap();
    // Archiving with a live context is refused.
    assert!(change_student_status(
        &mut tx,
        &student,
        inst,
        StudentStatus::Archived,
        &change,
        &actor
    )
    .await
    .is_err());
    let ctx = change_context_status(
        &mut tx,
        &ctx,
        StudentContextStatus::Withdrawn,
        &change,
        &actor,
    )
    .await
    .unwrap();
    assert!(ctx.closed_on.is_some());
    let student = change_student_status(
        &mut tx,
        &student,
        inst,
        StudentStatus::Archived,
        &change,
        &actor,
    )
    .await
    .unwrap();
    assert_eq!(student.status, StudentStatus::Archived);
    tx.commit().await.unwrap();

    assert!(live_context(&pool, tenant, StudentId(s.student_id), inst)
        .await
        .unwrap()
        .is_none());
    // Terminal contexts are immutable; history is append-only.
    assert!(
        sqlx::query("UPDATE student.student_context SET roll_number = 'X' WHERE id = $1")
            .bind(s.context_id)
            .execute(&pool)
            .await
            .is_err()
    );
    assert!(
        sqlx::query("DELETE FROM student.student_status_history WHERE student_id = $1")
            .bind(s.student_id)
            .execute(&pool)
            .await
            .is_err()
    );
    let history: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student_status_history WHERE student_id = $1",
    )
    .bind(s.student_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(history, 4);
    let events: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM platform.outbox WHERE event_type = 'sutraerp.student.student.status_changed' \
         AND subject = $1",
    )
    .bind(s.student_id.to_string())
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(events, 4);

    // One live context per student and institution.
    let s2 = b3::seed_student(&pool, &setup, "Bina", None).await;
    let dup = sqlx::query(
        "INSERT INTO student.student_context (tenant_id, student_id, institution_id, status) \
         VALUES ($1, $2, $3, 'pending_activation')",
    )
    .bind(setup.tenant_id)
    .bind(s2.student_id)
    .bind(setup.institution_id)
    .execute(&pool)
    .await;
    assert!(dup.is_err());
}
