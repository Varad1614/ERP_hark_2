//! Execution scope of institution-bound Domain B operations.
//!
//! Tenant always comes from the session (ADR-L5-0004). Domain B administration is tenant-wide in
//! Batch 2 (as for the Batch 0 reference aggregate): an administrator acts on any institution of
//! the session tenant and the path's institution id becomes the event/audit scope. Every repo
//! query must still filter on `tenant_id` and verify the institution row exists in that tenant
//! ([`crate::shared::guard`]) — other tenants' institutions are indistinguishable from missing.
//! Pinning a session to its own institution is a later-batch hardening (see contract doc §9).

use sutra_kernel::auth::AuthContext;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};

/// Scope for an operation on `institution_id` by the authenticated caller.
pub fn institution_scope(ctx: &AuthContext, institution_id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(ctx.scope.tenant_id, institution_id)
}
