//! Faculty-scoped access (contract doc §4.4).
//!
//! A faculty member acts on a section only while holding an **active** faculty assignment on it
//! (`academic.faculty_assignment`, read through `AcademicState::offerings`). Staff with the
//! slice's manage permission act on any section. Every 4B/4C teaching handler starts with
//! [`require_teaching`]; a faculty member who is not assigned gets 403.

use crate::shared::AcademicState;
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::ports::SectionSummary;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// How the caller was allowed in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TeachingAccess {
    /// Holds the slice's manage permission (administrator): any section.
    Staff,
    /// Holds `academic.teach` and an active assignment on this section.
    AssignedFaculty,
}

/// Allows `manage` holders on any section, and `academic.teach` holders only on sections they
/// actively teach (403 otherwise). Step-up of `manage` is enforced by `AuthContext::require`.
pub async fn require_teaching(
    state: &AcademicState,
    ctx: &AuthContext,
    section_id: OfferingSectionId,
    manage: Permission,
) -> Result<TeachingAccess, KernelError> {
    if ctx.has_permission(manage) {
        ctx.require(manage)?;
        return Ok(TeachingAccess::Staff);
    }
    ctx.require(Permission::AcademicTeach)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    if state
        .offerings
        .teaches(&scope, &ctx.subject, section_id)
        .await?
    {
        Ok(TeachingAccess::AssignedFaculty)
    } else {
        Err(KernelError::Forbidden(
            "you are not assigned to teach this section".into(),
        ))
    }
}

/// Sections the caller actively teaches ("My classes"); requires `academic.teach`.
pub async fn my_sections(
    state: &AcademicState,
    ctx: &AuthContext,
) -> Result<Vec<SectionSummary>, KernelError> {
    ctx.require(Permission::AcademicTeach)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    state
        .offerings
        .sections_taught_by(&scope, &ctx.subject)
        .await
}
