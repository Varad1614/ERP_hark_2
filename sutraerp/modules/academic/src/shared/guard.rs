//! Cross-slice write rules of Domain D (contract doc §6.1).
//!
//! * **Institution** (Domain B, read through `InstitutionReferencePort`): academic data of an
//!   institution may change while it is `draft | configured | pending_approval | returned |
//!   rejected | active` (the "structure editable" rule of Batches 2/3); it is read-only while
//!   `suspended | archived` (409). Missing / foreign is 404. No cross-schema row lock: the check
//!   runs just before the owner-local transaction.
//! * **Offering**: terminal offerings (`archived | merged | split`) are immutable (also a DB
//!   trigger); a `frozen` offering accepts no operational change except the governed unfreeze;
//!   delivery commands (4B sessions/attendance/coursework, 4C resources) need a deliverable
//!   offering (`enrollment_open | enrollment_closed | active`) and an `active` section.

use crate::shared::AcademicState;
use sutra_contracts::academic::status::{CourseOfferingStatus, SectionStatus};
use sutra_contracts::institution::reference::InstitutionRef;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// `true` when Domain D data of an institution in this status may change.
pub fn institution_writable(status: InstitutionStatus) -> bool {
    !matches!(
        status,
        InstitutionStatus::Suspended | InstitutionStatus::Archived
    )
}

/// Resolves the institution (404) and enforces [`institution_writable`] (409).
pub async fn require_institution_writable(
    state: &AcademicState,
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
            "institution is {}; its academic records are read-only",
            inst.status
        )));
    }
    Ok(inst)
}

/// Terminal and frozen offerings refuse operational changes (4A attribute edits, enrolment).
pub fn require_offering_changeable(status: CourseOfferingStatus) -> Result<(), KernelError> {
    if status.is_terminal()
        || matches!(
            status,
            CourseOfferingStatus::Frozen
                | CourseOfferingStatus::Cancelled
                | CourseOfferingStatus::Completed
        )
    {
        return Err(KernelError::Conflict(format!(
            "the course offering is {status} and cannot change"
        )));
    }
    Ok(())
}

/// Delivery (4B/4C) needs a deliverable offering and an active section.
pub fn require_deliverable(
    offering: CourseOfferingStatus,
    section: SectionStatus,
) -> Result<(), KernelError> {
    if !offering.is_deliverable() {
        return Err(KernelError::Conflict(format!(
            "the course offering is {offering}; teaching and coursework need it open or active"
        )));
    }
    if section != SectionStatus::Active {
        return Err(KernelError::Conflict(format!(
            "the section is {section}; teaching and coursework need an active section"
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

    #[test]
    fn offering_gates() {
        assert!(require_offering_changeable(CourseOfferingStatus::Draft).is_ok());
        assert!(require_offering_changeable(CourseOfferingStatus::Frozen).is_err());
        assert!(require_offering_changeable(CourseOfferingStatus::Archived).is_err());
        assert!(require_deliverable(CourseOfferingStatus::Active, SectionStatus::Active).is_ok());
        assert!(require_deliverable(CourseOfferingStatus::Draft, SectionStatus::Active).is_err());
        assert!(require_deliverable(CourseOfferingStatus::Active, SectionStatus::Planned).is_err());
    }
}
