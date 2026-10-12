//! Batch 2 foundation smoke test: the institution status column + shared guard/readiness.
#![cfg(feature = "db-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::*;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};

const TENANT: &str = "0192f5a0-7c3e-7000-8000-0000000b2f00";

#[tokio::test]
async fn new_institutions_start_in_draft_and_are_structure_editable() {
    let (app, pool) = app(TENANT).await;
    let admin = login(&app, "admin@sutra.dev").await;
    let id: InstitutionId = register_institution(&app, &admin, "Foundation College")
        .await
        .parse()
        .unwrap();
    let scope = ExecutionScope::tenant(TENANT.parse::<TenantId>().unwrap());
    let status = sutra_institution::shared::guard::institution_status(&pool, &scope, id)
        .await
        .unwrap();
    assert_eq!(status, InstitutionStatus::Draft);
    let mut tx = pool.begin().await.unwrap();
    sutra_institution::shared::guard::lock_institution_for_change(&mut tx, &scope, id)
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    // Foreign tenant: indistinguishable from missing.
    let other = ExecutionScope::tenant(TenantId::new());
    assert!(
        sutra_institution::shared::guard::institution_status(&pool, &other, id)
            .await
            .is_err()
    );
    // Readiness aggregates the (stub) providers.
    let r = sutra_institution::shared::readiness::evaluate(&pool, &scope, id)
        .await
        .unwrap();
    assert_eq!(r.institution_status, InstitutionStatus::Draft);
}
