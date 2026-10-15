//! Part 1 (DEF-X-002): who requested a governed decision, read from a module's append-only
//! status history (`<schema>.status_history` — `actor` of the row that moved the aggregate into
//! its pending state, plus the actor of its creation row). Modules pass the result to
//! [`sutra_kernel::approval::ensure_independent_decider`] on their decision path.

use crate::map_db_error;
use sqlx::PgConnection;
use sutra_kernel::error::KernelError;
use uuid::Uuid;

/// A module status history table with the common shape
/// `(tenant_id, aggregate_type, aggregate_id, from_status, to_status, actor)`.
#[derive(Clone, Copy, Debug)]
pub enum HistoryTable {
    Finance,
    Exams,
    People,
    Resources,
    Campus,
    Services,
    Academic,
}

impl HistoryTable {
    fn name(self) -> &'static str {
        match self {
            Self::Finance => "finance.status_history",
            Self::Exams => "exams.status_history",
            Self::People => "people.status_history",
            Self::Resources => "resources.status_history",
            Self::Campus => "campus.status_history",
            Self::Services => "services.status_history",
            Self::Academic => "academic.status_history",
        }
    }
}

/// Distinct non-empty actors that created the aggregate (`from_status IS NULL`) or moved it into
/// one of `pending` statuses — the request's makers.
pub async fn requesters(
    conn: &mut PgConnection,
    table: HistoryTable,
    tenant_id: Uuid,
    aggregate_type: &str,
    aggregate_id: Uuid,
    pending: &[&str],
) -> Result<Vec<String>, KernelError> {
    let pending: Vec<String> = pending.iter().map(|s| (*s).to_owned()).collect();
    let sql = format!(
        "SELECT DISTINCT actor FROM {} WHERE tenant_id = $1 AND aggregate_type = $2 \
         AND aggregate_id = $3 AND actor IS NOT NULL AND actor <> '' \
         AND (from_status IS NULL OR to_status = ANY($4))",
        table.name()
    );
    sqlx::query_scalar::<_, String>(&sql)
        .bind(tenant_id)
        .bind(aggregate_type)
        .bind(aggregate_id)
        .bind(&pending)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)
}
