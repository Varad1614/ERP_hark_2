//! Staff self-service resolution (contract doc §4.4): the caller's staff record (and through it
//! their engagement, leave, attendance, payslips) is always resolved from the session subject
//! (`people.staff_member.subject_ref`), never from a client-supplied id.

use crate::shared::PeopleState;
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// The caller's own live staff record (`people.self`); 404 when the login has none.
pub async fn my_staff(state: &PeopleState, ctx: &AuthContext) -> Result<StaffSummary, KernelError> {
    ctx.require(Permission::PeopleSelfService)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    state
        .directory
        .staff_by_subject(&scope, &ctx.subject)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff record",
        })
}

/// The caller's own staff record and current engagement (404 when either is missing).
pub async fn my_engagement(
    state: &PeopleState,
    ctx: &AuthContext,
) -> Result<(StaffSummary, EngagementSummary), KernelError> {
    let staff = my_staff(state, ctx).await?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    let e = state
        .directory
        .current_engagement(&scope, staff.id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    Ok((staff, e))
}
