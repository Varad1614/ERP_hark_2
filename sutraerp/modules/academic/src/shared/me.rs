//! Student self-service resolution (contract doc §4.4): the student is always resolved from the
//! session subject through `StudentDirectory::by_subject`, never taken from the client.

use crate::shared::AcademicState;
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::ports::SectionSummary;
use sutra_contracts::student::ports::StudentSummary;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// The caller's own student record (`academic.self`; 404 when the login has no linked student).
pub async fn me(state: &AcademicState, ctx: &AuthContext) -> Result<StudentSummary, KernelError> {
    ctx.require(Permission::AcademicSelfService)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    state
        .students
        .by_subject(&scope, &ctx.subject)
        .await?
        .ok_or(KernelError::NotFound { entity: "student" })
}

/// Sections where the caller (a student) holds an `enrolled` seat.
pub async fn my_sections(
    state: &AcademicState,
    ctx: &AuthContext,
) -> Result<(StudentSummary, Vec<SectionSummary>), KernelError> {
    let s = me(state, ctx).await?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    let sections = state.offerings.sections_of_student(&scope, s.id).await?;
    Ok((s, sections))
}

/// 404 unless the caller is seated in `section_id` (a student never learns about other sections).
pub async fn require_seated(
    state: &AcademicState,
    ctx: &AuthContext,
    section_id: OfferingSectionId,
) -> Result<StudentSummary, KernelError> {
    let (s, sections) = my_sections(state, ctx).await?;
    if sections.iter().any(|x| x.id == section_id) {
        Ok(s)
    } else {
        Err(KernelError::NotFound { entity: "section" })
    }
}
