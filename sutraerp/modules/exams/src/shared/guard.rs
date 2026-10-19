//! Cross-slice write rules of Domain I (contract doc §6.1).
//!
//! * **Institution** (Domain B, read through `InstitutionReferencePort`): examination records of
//!   an institution may change while it is not `suspended | archived` (same rule as Domains C/D/F);
//!   missing / foreign is 404. No cross-schema row lock: the check runs just before the
//!   owner-local transaction.
//! * **Session**: operational workflows (registration, conduct, marks, results) consume only an
//!   `active | reopened` session ([`require_operational`], L2-I02 locked decision 4).

use crate::shared::ExamsState;
use sutra_contracts::exams::ports::ExamSessionSummary;
use sutra_contracts::institution::reference::InstitutionRef;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// `true` when Domain I data of an institution in this status may change.
pub fn institution_writable(status: InstitutionStatus) -> bool {
    !matches!(
        status,
        InstitutionStatus::Suspended | InstitutionStatus::Archived
    )
}

/// Resolves the institution (404) and enforces [`institution_writable`] (409).
pub async fn require_institution_writable(
    state: &ExamsState,
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
            "institution is {}; its examination records are read-only",
            inst.status
        )));
    }
    Ok(inst)
}

/// Resolves the institution for reads (404 when missing / foreign).
pub async fn require_institution(
    state: &ExamsState,
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

/// 409 unless the session may be consumed by operational workflows (`active | reopened`).
pub fn require_operational(session: &ExamSessionSummary) -> Result<(), KernelError> {
    if session.status.is_operational() {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "examination session {} is {}; only an active session accepts this operation",
            session.code, session.status
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
