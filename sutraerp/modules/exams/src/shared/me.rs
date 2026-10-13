//! Student self-service resolution (contract doc §4.4): the caller's registrations (and through
//! them hall tickets, results, grade cards, revaluation requests) are always resolved from the
//! session subject (`exams.exam_registration.subject_ref`, snapshotted from Domain C at
//! registration), never from a client-supplied id.

use crate::shared::ExamsState;
use sutra_contracts::exams::ids::ExamRegistrationId;
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// The caller's own registrations (`exam.self`); empty when never registered.
pub async fn my_registrations(
    state: &ExamsState,
    ctx: &AuthContext,
) -> Result<Vec<CandidateSummary>, KernelError> {
    ctx.require(Permission::ExamSelfService)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    state
        .directory
        .registrations_of_subject(&scope, &ctx.subject)
        .await
}

/// 404 unless `registration` is one of the caller's own registrations (a student never learns
/// about other candidates).
pub async fn require_own_registration(
    state: &ExamsState,
    ctx: &AuthContext,
    registration: ExamRegistrationId,
) -> Result<CandidateSummary, KernelError> {
    my_registrations(state, ctx)
        .await?
        .into_iter()
        .find(|r| r.registration_id == registration)
        .ok_or(KernelError::NotFound {
            entity: "examination registration",
        })
}
