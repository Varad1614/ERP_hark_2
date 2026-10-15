//! # sutra-persistence
//!
//! PostgreSQL 18 + SQLx infrastructure shared by module repositories (ADR-L5-0020).
//!
//! * [`pool`] – bounded, lazily-connecting SQLx pool, sessions normalised to UTC (ADR-L5-0036 §3.4/§3.7)
//! * [`migrate`] – embedded, version-controlled migrations run as a separate step (ADR-L5-0008)
//! * [`outbox`] – Durable Publication Intent writer for the owner-local transaction (ADR-L5-0007)
//! * [`outbox_relay`] – claim → publish → mark relay to a `MessagePublisher` (ADR-L5-0010/0022)
//! * [`jobs`] – advisory-locked sweeper scheduler + idempotency/outbox retention jobs
//! * [`backoff`] – exponential backoff with jitter
//! * [`audit`] – append-only audit sink
//! * [`idempotency`] – owner-scoped idempotency store (ADR-L5-0034)
//! * [`readiness`] – database readiness probe
//!
//! All queries are runtime-checked (`sqlx::query`), so the workspace builds with no database.

pub mod approval; // Part 1: DEF-X-002
pub mod audit;
pub mod audit_chain; // 9A
pub mod backoff;
pub mod idempotency;
pub mod jobs;
pub mod migrate;
pub mod outbox;
pub mod outbox_relay;
pub mod pool;
pub mod readiness;

pub use sqlx::PgPool;

use sutra_kernel::error::KernelError;

/// `%…%` pattern for a "contains" search bound to `LIKE` / `ILIKE` (PostgreSQL's default escape
/// character is the backslash): the user's `\`, `%` and `_` match literally, so a search for `%` or
/// `_` is not a wildcard that lists every row (SEC-P1-APPSEC-003).
pub fn like_contains(term: &str) -> String {
    let mut out = String::with_capacity(term.len() + 2);
    out.push('%');
    for ch in term.chars() {
        if matches!(ch, '\\' | '%' | '_') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('%');
    out
}

/// Maps SQLx errors onto the kernel taxonomy without leaking driver details to clients.
pub fn map_db_error(e: sqlx::Error) -> KernelError {
    match &e {
        sqlx::Error::PoolTimedOut
        | sqlx::Error::PoolClosed
        | sqlx::Error::Io(_)
        | sqlx::Error::Tls(_) => KernelError::Unavailable("database unavailable".into()),
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            KernelError::Conflict("a record with the same unique attributes already exists".into())
        }
        _ => KernelError::internal(e),
    }
}

#[cfg(test)]
mod like_tests {
    use super::like_contains;

    #[test]
    fn like_wildcards_and_escape_are_literal() {
        assert_eq!(like_contains("ann"), "%ann%");
        assert_eq!(like_contains("%"), "%\\%%");
        assert_eq!(like_contains("a_b"), "%a\\_b%");
        assert_eq!(like_contains("c:\\x"), "%c:\\\\x%");
        assert_eq!(like_contains("' OR '1'='1"), "%' OR '1'='1%");
    }
}
