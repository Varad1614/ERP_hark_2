//! Cross-slice write rules of Domain F (contract doc §6.1).
//!
//! * **Institution** (Domain B, read through `InstitutionReferencePort`): finance records of an
//!   institution may change while it is not `suspended | archived` (same rule as Domains C/D);
//!   missing / foreign is 404. No cross-schema row lock: the check runs just before the
//!   owner-local transaction.
//! * **Charge**: amounts move only while the charge accepts them (`ChargeStatus::
//!   accepts_settlement` / `accepts_adjustment`, enforced by `shared::charge`).

use crate::shared::FinanceState;
use sutra_contracts::institution::reference::InstitutionRef;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// `true` when Domain F data of an institution in this status may change.
pub fn institution_writable(status: InstitutionStatus) -> bool {
    !matches!(
        status,
        InstitutionStatus::Suspended | InstitutionStatus::Archived
    )
}

/// Resolves the institution (404) and enforces [`institution_writable`] (409).
pub async fn require_institution_writable(
    state: &FinanceState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<InstitutionRef, KernelError> {
    let inst = state
        .references
        .institution(scope, institution_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "institution",
        })?;
    if !institution_writable(inst.status) {
        return Err(KernelError::Conflict(format!(
            "institution is {}; its finance records are read-only",
            inst.status
        )));
    }
    Ok(inst)
}

/// Resolves the institution for reads (404 when missing / foreign).
pub async fn require_institution(
    state: &FinanceState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<InstitutionRef, KernelError> {
    state
        .references
        .institution(scope, institution_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "institution",
        })
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
            assert_eq!(institution_writable(*s), expect, "{s}");
        }
    }
}
