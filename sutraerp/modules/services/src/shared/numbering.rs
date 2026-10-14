//! Per-institution document numbers (`services.number_sequence`): gap-free inside committed
//! transactions because the counter row is updated in the caller's transaction (a rollback
//! returns the number). Kinds and prefixes are listed in `docs/batch8-contracts.md` §3.4.

use sqlx::PgConnection;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;

/// Number kinds (`docs/batch8-contracts.md` §3.4). Slices add theirs at the end of their block (`// 8X:`).
pub mod kind {
    // 8C
    pub const REPORTING_CYCLE: (&str, &str) = ("reporting_cycle", "RC");
    pub const SUBMISSION_PACKAGE: (&str, &str) = ("submission_package", "SUB");

    /// Every kind (unit-tested unique names).
    pub const ALL: &[(&str, &str)] = &[REPORTING_CYCLE, SUBMISSION_PACKAGE];
}

/// Formats `prefix-NNNNNN`.
pub fn format_number(prefix: &str, n: i64) -> String {
    format!("{prefix}-{n:06}")
}

/// Takes the next number of `kind` (`(name, prefix)` from [`kind`]) in the caller's transaction.
pub async fn next(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    kind: (&str, &str),
) -> Result<String, KernelError> {
    let n: i64 = sqlx::query_scalar(
        "INSERT INTO services.number_sequence (tenant_id, institution_id, kind, next_value) \
         VALUES ($1, $2, $3, 2) \
         ON CONFLICT (tenant_id, institution_id, kind) \
         DO UPDATE SET next_value = services.number_sequence.next_value + 1 \
         RETURNING next_value - 1",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(kind.0)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(format_number(kind.1, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_zero_padded() {
        assert_eq!(format_number("SR", 7), "SR-000007");
        assert_eq!(format_number("PO", 1_234_567), "PO-1234567");
        let all: std::collections::HashSet<_> = kind::ALL.iter().map(|k| k.0).collect();
        assert_eq!(all.len(), kind::ALL.len());
    }
}
