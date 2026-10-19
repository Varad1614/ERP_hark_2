//! Execution scope of Domain H operations.
//!
//! Tenant always comes from the session (ADR-L5-0004). Administration is tenant-wide in Batch 8 (as Domains B…F and I):
//! the institution of the aggregate (or of the path) becomes the event/audit scope. Every query filters on `tenant_id`; another tenant's rows are
//! indistinguishable from missing (404).

use sutra_kernel::auth::AuthContext;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};

/// Scope for an operation in `institution_id` by the authenticated caller.
pub fn institution_scope(ctx: &AuthContext, institution_id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(ctx.scope.tenant_id, institution_id)
}

/// Tenant-only scope (lookups before the institution is known).
pub fn tenant_scope(ctx: &AuthContext) -> ExecutionScope {
    ExecutionScope::tenant(ctx.scope.tenant_id)
}

/// Tenant of the caller.
pub fn tenant(ctx: &AuthContext) -> TenantId {
    ctx.scope.tenant_id
}
