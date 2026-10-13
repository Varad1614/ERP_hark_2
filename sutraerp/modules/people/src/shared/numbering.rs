//! Per-institution document numbers (`people.number_sequence`): gap-free inside committed
//! transactions because the counter row is updated in the caller's transaction (a rollback
//! returns the number). Kinds and prefixes are listed in contract §3.4.

use sqlx::PgConnection;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;

/// Number kinds (contract §3.4). Slices add theirs at the end of their block (`// 7X:`).
pub mod kind {
    // foundation
    pub const STAFF: (&str, &str) = ("staff", "EMP");
    pub const ENGAGEMENT: (&str, &str) = ("engagement", "ENG");
    // 7A
    pub const REQUISITION: (&str, &str) = ("requisition", "REQ");
    pub const ONBOARDING: (&str, &str) = ("onboarding", "ONB");
    pub const SEPARATION: (&str, &str) = ("separation", "SEP");
    pub const CERTIFICATE: (&str, &str) = ("certificate", "CERT");
    // 7B
    pub const LEAVE_REQUEST: (&str, &str) = ("leave_request", "LV");
    pub const LEAVE_EXCEPTION: (&str, &str) = ("leave_exception", "LEX");
    // 7C
    pub const PAYROLL_RUN: (&str, &str) = ("payroll_run", "PR");
    pub const PAYSLIP: (&str, &str) = ("payslip", "PS");
    pub const SETTLEMENT: (&str, &str) = ("settlement", "FNF");
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
        "INSERT INTO people.number_sequence (tenant_id, institution_id, kind, next_value) \
         VALUES ($1, $2, $3, 2) \
         ON CONFLICT (tenant_id, institution_id, kind) \
         DO UPDATE SET next_value = people.number_sequence.next_value + 1 \
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
        assert_eq!(format_number(kind::STAFF.1, 7), "EMP-000007");
        assert_eq!(format_number(kind::PAYSLIP.1, 1_234_567), "PS-1234567");
    }
}
