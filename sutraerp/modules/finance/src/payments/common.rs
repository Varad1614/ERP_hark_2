//! Helpers shared by the 5B areas (payments, refunds, receivables): wire amount parsing,
//! status-history + event payload builders, party lookups.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::money::require_positive;
use crate::shared::Actor;
use rust_decimal::Decimal;
use sqlx::PgConnection;
use std::str::FromStr;
use sutra_contracts::finance::events::StatusChangedV1;
use sutra_contracts::finance::ids::StudentAccountId;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Parses a positive wire amount (`"1250.00"`) → 422 on the field otherwise.
pub fn parse_amount(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let d = Decimal::from_str(raw.trim()).map_err(|_| {
        KernelError::Validation(vec![FieldViolation::new(
            field,
            "invalid_amount",
            "must be a decimal string such as \"1250.00\"",
        )])
    })?;
    require_positive(field, d)
}

/// 422 when a free-text reason is blank.
pub fn require_text(field: &str, raw: Option<&str>) -> Result<String, KernelError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.len() <= 1000 => Ok(s.to_owned()),
        _ => Err(KernelError::Validation(vec![FieldViolation::new(
            field,
            "required",
            "is required (1-1000 characters)",
        )])),
    }
}

/// 412 unless the `If-Match` version is the current one.
pub fn require_version(
    if_match: sutra_kernel::concurrency::Version,
    current: i64,
) -> Result<(), KernelError> {
    if if_match.0 == current {
        Ok(())
    } else {
        Err(KernelError::VersionConflict {
            expected: if_match.0,
            actual: current,
        })
    }
}

/// 409 for an illegal transition of a 5B aggregate.
pub fn illegal(aggregate: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{aggregate} cannot move from {from} to {to}"))
}

/// Appends the status history row of a 5B aggregate.
#[allow(clippy::too_many_arguments)]
pub async fn history(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    aggregate_type: &'static str,
    id: Uuid,
    from: Option<&str>,
    to: &str,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution,
            aggregate_type,
            aggregate_id: id,
            from,
            to,
        },
        change,
        actor,
    )
    .await
}

/// The login subject of a party account (so pure inbox rules can notify the student).
pub async fn subject_ref(
    conn: &mut PgConnection,
    tenant: TenantId,
    account: StudentAccountId,
) -> Result<Option<String>, KernelError> {
    sqlx::query_scalar::<_, Option<String>>(
        "SELECT subject_ref FROM finance.student_account WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(account.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)
    .map(Option::flatten)
}

/// Builder for the shared `StatusChangedV1` payload.
pub struct Changed {
    pub v: StatusChangedV1,
}

impl Changed {
    pub fn new(id: Uuid, institution: InstitutionId, to: &str, version: i64) -> Self {
        Self {
            v: StatusChangedV1 {
                aggregate_id: id,
                institution_id: institution.as_uuid(),
                student_account_id: None,
                student_id: None,
                charge_id: None,
                from_status: None,
                to_status: to.to_owned(),
                version,
                amount: None,
                currency: None,
                reason: None,
                process_instance_id: None,
                journal_entry_id: None,
                student_subject_ref: None,
            },
        }
    }
    pub fn from(mut self, from: &str) -> Self {
        self.v.from_status = Some(from.to_owned());
        self
    }
    pub fn account(mut self, a: StudentAccountId, subject: Option<String>) -> Self {
        self.v.student_account_id = Some(a.as_uuid());
        self.v.student_subject_ref = subject;
        self
    }
    pub fn charge(mut self, c: Uuid) -> Self {
        self.v.charge_id = Some(c);
        self
    }
    pub fn amount(mut self, a: Decimal, currency: &str) -> Self {
        self.v.amount = Some(crate::shared::money::fmt(a));
        self.v.currency = Some(currency.to_owned());
        self
    }
    pub fn reason(mut self, r: Option<&str>) -> Self {
        self.v.reason = r.map(str::to_owned);
        self
    }
    pub fn process(mut self, p: Option<&str>) -> Self {
        self.v.process_instance_id = p.map(str::to_owned);
        self
    }
    pub fn journal(mut self, j: Option<Uuid>) -> Self {
        self.v.journal_entry_id = j;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_and_reasons_are_validated() {
        assert_eq!(
            parse_amount("amount", "1250.5").ok(),
            Decimal::from_str("1250.50").ok()
        );
        assert!(parse_amount("amount", "abc").is_err());
        assert!(parse_amount("amount", "-1").is_err());
        assert!(parse_amount("amount", "1.001").is_err());
        assert!(require_text("reason", Some("  ")).is_err());
        assert_eq!(
            require_text("reason", Some(" ok ")).ok().as_deref(),
            Some("ok")
        );
    }
}
