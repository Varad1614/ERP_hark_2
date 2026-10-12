//! Institution status lookup and the cross-slice rule for downstream configuration.
//!
//! Rule (contract doc §6): campuses, organisation units, positions, academic structure,
//! programmes, curricula and calendars may be **created or changed** while the institution is
//! `draft`, `configured`, `returned`, `pending_approval`, `rejected` or `active` (setup happens
//! before activation; readiness aggregates it). They are **read-only** while the institution is
//! `suspended` or `archived` → 409 `conflict`.
//!
//! Call [`lock_institution_for_change`] inside the owner-local transaction *before* writing a
//! downstream row: it takes `FOR SHARE` on the institution row, so a concurrent suspend/archive
//! (which updates that row) serialises against it.

use sqlx::{PgConnection, PgExecutor};
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

fn parse(raw: Option<String>) -> Result<InstitutionStatus, KernelError> {
    let raw = raw.ok_or(KernelError::NotFound {
        entity: "institution",
    })?;
    InstitutionStatus::parse(&raw)
        .ok_or_else(|| KernelError::internal_msg(format!("unknown institution status `{raw}`")))
}

/// Current status of an institution of the scope's tenant (404 when missing / foreign).
pub async fn institution_status<'e, E: PgExecutor<'e>>(
    executor: E,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<InstitutionStatus, KernelError> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT status FROM institution.institution WHERE id = $1 AND tenant_id = $2",
    )
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?;
    parse(raw)
}

/// `true` when downstream structure may be created or changed in this institution status.
pub fn structure_editable(status: InstitutionStatus) -> bool {
    !matches!(
        status,
        InstitutionStatus::Suspended | InstitutionStatus::Archived
    )
}

/// Locks the institution row `FOR SHARE` and enforces [`structure_editable`].
pub async fn lock_institution_for_change(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<InstitutionStatus, KernelError> {
    let raw: Option<String> = sqlx::query_scalar(
        "SELECT status FROM institution.institution WHERE id = $1 AND tenant_id = $2 FOR SHARE",
    )
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let status = parse(raw)?;
    if !structure_editable(status) {
        return Err(KernelError::Conflict(format!(
            "institution is {status}; its structure is read-only"
        )));
    }
    Ok(status)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suspended_and_archived_institutions_are_read_only() {
        for s in InstitutionStatus::ALL {
            let expect = !matches!(
                s,
                InstitutionStatus::Suspended | InstitutionStatus::Archived
            );
            assert_eq!(structure_editable(*s), expect, "{s}");
        }
        assert!(matches!(
            parse(None),
            Err(KernelError::NotFound {
                entity: "institution"
            })
        ));
    }
}
