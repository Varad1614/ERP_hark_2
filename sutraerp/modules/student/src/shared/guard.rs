//! Cross-slice write rules of Domain C (contract doc §6).
//!
//! * **Institution** (Domain B, read through `InstitutionReferencePort`): student data of an
//!   institution may be created or changed while it is `draft | configured | pending_approval |
//!   returned | rejected | active` (the same "structure editable" rule as Batch 2 — setup and
//!   admissions may start before activation). It is read-only while `suspended | archived`
//!   (409). A missing / foreign institution is 404. There is no cross-schema row lock: the check
//!   runs just before the owner-local transaction.
//! * **Student**: an `archived` persona is read-only (409) except for governed readmission (3A).
//! * **Context**: terminal contexts are immutable (also enforced by a DB trigger).

use crate::shared::student::{StudentContextRow, StudentRow};
use crate::shared::StudentState;
use sutra_contracts::institution::reference::InstitutionRef;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_contracts::student::status::StudentStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// `true` when Domain C data of an institution in this status may change.
pub fn institution_writable(status: InstitutionStatus) -> bool {
    !matches!(
        status,
        InstitutionStatus::Suspended | InstitutionStatus::Archived
    )
}

/// Resolves the institution (404) and enforces [`institution_writable`] (409).
pub async fn require_institution_writable(
    state: &StudentState,
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
            "institution is {}; its student records are read-only",
            inst.status
        )));
    }
    Ok(inst)
}

/// Archived personas are read-only.
pub fn require_student_writable(student: &StudentRow) -> Result<(), KernelError> {
    if student.status == StudentStatus::Archived {
        return Err(KernelError::Conflict(
            "the student record is archived and read-only".into(),
        ));
    }
    Ok(())
}

/// Terminal contexts are immutable; 3B/3C commands need a live context.
pub fn require_live_context(ctx: &StudentContextRow) -> Result<(), KernelError> {
    if !ctx.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "student context is {} and read-only",
            ctx.status
        )));
    }
    Ok(())
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
