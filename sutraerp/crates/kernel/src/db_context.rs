//! Per-task database security context (Part 1 Stage 3, DEF-X-003 / SEC-9D-004).
//!
//! PostgreSQL row-level security on every tenant-scoped table (migration
//! `20261230000100_platform_tenant_rls.sql`) reads three settings of the database session:
//!
//! | setting | value |
//! |---|---|
//! | `sutra.tenant_id` | tenant of the authenticated session (empty = none) |
//! | `sutra.institution_id` | institution the session is pinned to (empty = tenant-wide) |
//! | `sutra.rls_scope` | `tenant`, `system` or empty |
//!
//! The values are **never** taken from anything the client sends. The session middleware
//! (`sutra_web::auth::authenticate`) runs every authenticated request inside [`scoped`] with the
//! scope of the server-side session; server-internal work that is cross-tenant by design
//! (session resolution and sign-in before a tenant is known, the outbox relay, sweepers, the
//! payment-gateway webhook lookup, migrations) runs inside [`system`]. The pool
//! (`sutra_persistence::pool`) copies [`current`] onto every connection it hands out
//! (`before_acquire` / `after_connect`) and clears it again when the connection returns
//! (`after_release`), so a pooled connection never carries one request's tenant into the next.
//! Code that runs outside both (a task spawned without the context, an unauthenticated route)
//! gets **no** context: row-level security then shows no tenant rows and refuses every write —
//! it fails closed.

use crate::scope::ExecutionScope;
use std::future::Future;

/// Database security context of the running task.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DbContext {
    /// Rows of the session's tenant only (and of its institution when the session is pinned).
    Scoped(ExecutionScope),
    /// Server-internal, cross-tenant work (never selected by a client).
    System,
}

tokio::task_local! {
    static DB_CONTEXT: DbContext;
}

/// The context of the running task, `None` outside [`scoped`] / [`system`].
pub fn current() -> Option<DbContext> {
    DB_CONTEXT.try_with(|c| *c).ok()
}

/// Runs `fut` with row-level security bound to `scope` (the authenticated session's scope).
pub async fn scoped<F: Future>(scope: ExecutionScope, fut: F) -> F::Output {
    DB_CONTEXT.scope(DbContext::Scoped(scope), fut).await
}

/// Runs `fut` as server-internal, cross-tenant work.
pub async fn system<F: Future>(fut: F) -> F::Output {
    DB_CONTEXT.scope(DbContext::System, fut).await
}

/// Values of (`sutra.tenant_id`, `sutra.institution_id`, `sutra.rls_scope`) for `ctx`.
pub fn settings(ctx: Option<DbContext>) -> (String, String, &'static str) {
    match ctx {
        Some(DbContext::Scoped(s)) => (
            s.tenant_id.as_uuid().to_string(),
            s.institution_id
                .map(|i| i.as_uuid().to_string())
                .unwrap_or_default(),
            "tenant",
        ),
        Some(DbContext::System) => (String::new(), String::new(), "system"),
        None => (String::new(), String::new(), ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{InstitutionId, TenantId};

    #[tokio::test]
    async fn no_context_outside_a_scope() {
        assert_eq!(current(), None);
        assert_eq!(settings(None), (String::new(), String::new(), ""));
    }

    #[tokio::test]
    async fn scoped_and_system_nest_and_restore() {
        let s = ExecutionScope::institution(TenantId::new(), InstitutionId::new());
        scoped(s, async {
            assert_eq!(current(), Some(DbContext::Scoped(s)));
            system(async { assert_eq!(current(), Some(DbContext::System)) }).await;
            assert_eq!(current(), Some(DbContext::Scoped(s)));
            let (t, i, k) = settings(current());
            assert_eq!(t, s.tenant_id.as_uuid().to_string());
            assert_eq!(i, s.institution_id.unwrap().as_uuid().to_string());
            assert_eq!(k, "tenant");
        })
        .await;
        assert_eq!(current(), None);
    }

    #[tokio::test]
    async fn spawned_tasks_do_not_inherit_the_context() {
        let s = ExecutionScope::tenant(TenantId::new());
        let inner = scoped(s, async {
            tokio::spawn(async { current() }).await.unwrap()
        })
        .await;
        assert_eq!(
            inner, None,
            "a spawned task must start without a tenant (fail closed)"
        );
    }
}
