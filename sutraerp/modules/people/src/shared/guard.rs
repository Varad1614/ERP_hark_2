//! Cross-slice write rules of Domain E (contract doc §6.1).
//!
//! * **Institution** (Domain B, read through `InstitutionReferencePort`): People records of an
//!   institution may change while it is not `suspended | archived` (same rule as Domains C/D/F/I);
//!   missing / foreign is 404. No cross-schema row lock: the check runs just before the
//!   owner-local transaction.
//! * **Engagement**: leave, attendance, workload and payroll commands act only on a **live**
//!   engagement ([`require_live_engagement`]).

use crate::shared::PeopleState;
use sutra_contracts::institution::reference::InstitutionRef;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_contracts::people::ports::EngagementSummary;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// `true` when Domain E data of an institution in this status may change.
pub fn institution_writable(status: InstitutionStatus) -> bool {
    !matches!(
        status,
        InstitutionStatus::Suspended | InstitutionStatus::Archived
    )
}

/// Resolves the institution (404) and enforces [`institution_writable`] (409).
pub async fn require_institution_writable(
    state: &PeopleState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<InstitutionRef, KernelError> {
    let inst = require_institution(state, scope, institution_id).await?;
    if !institution_writable(inst.status) {
        return Err(KernelError::Conflict(format!(
            "institution is {}; its people records are read-only",
            inst.status
        )));
    }
    Ok(inst)
}

/// Resolves the institution for reads (404 when missing / foreign).
pub async fn require_institution(
    state: &PeopleState,
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

/// 409 unless the engagement is live (not `closed | cancelled`).
pub fn require_live_engagement(e: &EngagementSummary) -> Result<(), KernelError> {
    if e.status.is_live() {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "engagement {} is {}; only a live engagement accepts this operation",
            e.engagement_number, e.status
        )))
    }
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
