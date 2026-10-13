//! Student self-service resolution (contract doc §4.4): the caller's party accounts are always
//! resolved from the session subject (`finance.student_account.subject_ref`, snapshotted from
//! Domain C when the account is opened), never from a client-supplied id.

use crate::shared::FinanceState;
use sutra_contracts::finance::ids::StudentAccountId;
use sutra_contracts::finance::ports::StudentAccountSummary;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;

/// The caller's own party accounts (`finance.self`); empty when no charge was ever raised.
pub async fn my_accounts(
    state: &FinanceState,
    ctx: &AuthContext,
) -> Result<Vec<StudentAccountSummary>, KernelError> {
    ctx.require(Permission::FinanceSelfService)?;
    let scope = ExecutionScope::tenant(ctx.scope.tenant_id);
    state
        .charges
        .accounts_of_subject(&scope, &ctx.subject)
        .await
}

/// 404 unless `account` is one of the caller's own accounts (a student never learns about
/// other parties).
pub async fn require_own_account(
    state: &FinanceState,
    ctx: &AuthContext,
    account: StudentAccountId,
) -> Result<StudentAccountSummary, KernelError> {
    my_accounts(state, ctx)
        .await?
        .into_iter()
        .find(|a| a.id == account)
        .ok_or(KernelError::NotFound {
            entity: "student account",
        })
}
