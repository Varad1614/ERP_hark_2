//! Per-institution document numbers (`finance.number_sequence`): gap-free inside committed
//! transactions because the counter row is updated in the caller's transaction (a rollback
//! returns the number). Kinds and prefixes are listed in contract §3.4.

use sqlx::PgConnection;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;

/// Number kinds (contract §3.4). Slices add theirs at the end of their block (`// 5X:`).
pub mod kind {
    // foundation
    pub const CHARGE: (&str, &str) = ("charge", "CHG");
    pub const JOURNAL: (&str, &str) = ("journal", "JE");
    // 5A
    pub const ADJUSTMENT: (&str, &str) = ("adjustment", "ADJ");
    pub const CREDIT_NOTE: (&str, &str) = ("credit_note", "CN");
    pub const DEBIT_NOTE: (&str, &str) = ("debit_note", "DN");
    pub const AID_ALLOCATION: (&str, &str) = ("aid_allocation", "AID");
    // 5B
    pub const PAYMENT: (&str, &str) = ("payment", "PAY");
    pub const RECEIPT: (&str, &str) = ("receipt", "RCT");
    pub const REFUND: (&str, &str) = ("refund", "RFD");
    // 5C
    pub const MANUAL_JOURNAL: (&str, &str) = ("manual_journal", "MJ");
    pub const FUND_TRANSFER: (&str, &str) = ("fund_transfer", "FT");
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
        "INSERT INTO finance.number_sequence (tenant_id, institution_id, kind, next_value) \
         VALUES ($1, $2, $3, 2) \
         ON CONFLICT (tenant_id, institution_id, kind) \
         DO UPDATE SET next_value = finance.number_sequence.next_value + 1 \
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
        assert_eq!(format_number(kind::CHARGE.1, 7), "CHG-000007");
        assert_eq!(format_number(kind::JOURNAL.1, 1_234_567), "JE-1234567");
    }
}
