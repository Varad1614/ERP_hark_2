//! Domain B reference validation for 3A (through `InstitutionReferencePort` only; contract §6.2).
//! Missing / foreign / wrong-institution references are `422` on the named field.

use super::support::invalid;
use crate::shared::StudentState;
use chrono::NaiveDate;
use sutra_contracts::institution::reference::{CurriculumVersionRef, ProgrammeRef};
use sutra_contracts::institution::status::{CampusStatus, OrgUnitStatus, PublicationStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use uuid::Uuid;

/// A **published** programme of `institution`.
pub async fn programme(
    st: &StudentState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    id: Uuid,
    field: &str,
) -> Result<ProgrammeRef, KernelError> {
    let p = st
        .references
        .programme(scope, id)
        .await?
        .filter(|p| p.institution_id == institution)
        .ok_or_else(|| invalid(field, "unknown", "programme not found in this institution"))?;
    if p.status != PublicationStatus::Published {
        return Err(invalid(
            field,
            "not-published",
            "the programme is not published",
        ));
    }
    Ok(p)
}

/// The curriculum version to admit/enrol on: `id` when given (published, same programme), else
/// the programme's published version in force on `on`.
pub async fn curriculum(
    st: &StudentState,
    scope: &ExecutionScope,
    programme_id: Uuid,
    id: Option<Uuid>,
    on: NaiveDate,
    field: &str,
) -> Result<CurriculumVersionRef, KernelError> {
    let cv = match id {
        Some(id) => st
            .references
            .curriculum_version(scope, id)
            .await?
            .filter(|c| c.programme_id == programme_id)
            .ok_or_else(|| {
                invalid(
                    field,
                    "unknown",
                    "curriculum version not found for this programme",
                )
            })?,
        None => st
            .references
            .current_curriculum_version(scope, programme_id, on)
            .await?
            .ok_or_else(|| {
                invalid(
                    field,
                    "none-in-force",
                    "the programme has no published curriculum version in force",
                )
            })?,
    };
    if cv.status != PublicationStatus::Published {
        return Err(invalid(
            field,
            "not-published",
            "the curriculum version is not published",
        ));
    }
    Ok(cv)
}

/// An **active** campus of `institution` (when given).
pub async fn campus(
    st: &StudentState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    id: Option<Uuid>,
    field: &str,
) -> Result<(), KernelError> {
    let Some(id) = id else { return Ok(()) };
    let c = st
        .references
        .campus(scope, id)
        .await?
        .filter(|c| c.institution_id == institution)
        .ok_or_else(|| invalid(field, "unknown", "campus not found in this institution"))?;
    if c.status != CampusStatus::Active {
        return Err(invalid(field, "not-active", "the campus is not active"));
    }
    Ok(())
}

/// An **active** organisation unit of `institution` (when given).
pub async fn org_unit(
    st: &StudentState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    id: Option<Uuid>,
    field: &str,
) -> Result<(), KernelError> {
    let Some(id) = id else { return Ok(()) };
    let u = st
        .references
        .org_unit(scope, id)
        .await?
        .filter(|u| u.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                field,
                "unknown",
                "organisation unit not found in this institution",
            )
        })?;
    if u.status != OrgUnitStatus::Active {
        return Err(invalid(
            field,
            "not-active",
            "the organisation unit is not active",
        ));
    }
    Ok(())
}

/// An academic term of `institution` (when given); returns its academic year id.
pub async fn term(
    st: &StudentState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    id: Option<Uuid>,
    field: &str,
) -> Result<Option<Uuid>, KernelError> {
    let Some(id) = id else { return Ok(None) };
    let t = st
        .references
        .academic_term(scope, id)
        .await?
        .filter(|t| t.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                field,
                "unknown",
                "academic term not found in this institution",
            )
        })?;
    Ok(Some(t.academic_year_id))
}
