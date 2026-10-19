//! Helpers shared by the 5C areas (ledger, periods, treasury): wire parsing, version checks,
//! status history and the shared `StatusChangedV1` payload.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::money::require_positive;
use crate::shared::publish::{audit, event};
use crate::shared::Actor;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::Serialize;
use sqlx::{PgConnection, Row};
use std::str::FromStr;
use sutra_contracts::finance::events::StatusChangedV1;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// 422 on one field.
pub fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message.into())])
}

/// Parses a positive wire amount (`"1250.00"`) → 422 on the field otherwise.
pub fn parse_amount(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let d = Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_amount",
            "must be a decimal string such as \"1250.00\"",
        )
    })?;
    require_positive(field, d)
}

/// Parses a non-negative wire amount (`"0.00"` allowed).
pub fn parse_amount_or_zero(field: &str, raw: Option<&str>) -> Result<Decimal, KernelError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(Decimal::ZERO),
        Some(s) => {
            let d = Decimal::from_str(s).map_err(|_| {
                invalid(
                    field,
                    "invalid_amount",
                    "must be a decimal string such as \"12.50\"",
                )
            })?;
            if d.is_zero() {
                Ok(Decimal::ZERO)
            } else {
                require_positive(field, d)
            }
        }
    }
}

/// Parses a signed wire amount (opening / closing balances may be negative = overdraft).
pub fn parse_signed(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let d = Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_amount",
            "must be a decimal string such as \"-12.50\"",
        )
    })?;
    if d.scale() > 2 || d.abs() >= Decimal::from(10_000_000_000_000_000_i64) {
        return Err(invalid(
            field,
            "invalid_amount",
            "at most 2 decimals and 16 integer digits",
        ));
    }
    Ok(d.round_dp(2))
}

/// 422 when a required text is blank (1–1000 characters).
pub fn require_text(field: &str, raw: Option<&str>) -> Result<String, KernelError> {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) if s.chars().count() <= 1000 => Ok(s.to_owned()),
        _ => Err(invalid(
            field,
            "required",
            "is required (1-1000 characters)",
        )),
    }
}

/// Optional trimmed text (`None` when blank).
pub fn opt_text(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// 412 unless the `If-Match` version is the current one.
pub fn require_version(if_match: Version, current: i64) -> Result<(), KernelError> {
    if if_match.0 == current {
        Ok(())
    } else {
        Err(KernelError::VersionConflict {
            expected: if_match.0,
            actual: current,
        })
    }
}

/// 409 for an illegal transition of a 5C aggregate.
pub fn illegal(aggregate: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{aggregate} cannot move from {from} to {to}"))
}

/// `approve` → true, `reject` → false, otherwise 422.
pub fn parse_decision(raw: &str) -> Result<bool, KernelError> {
    match raw {
        "approve" => Ok(true),
        "reject" => Ok(false),
        _ => Err(invalid("decision", "invalid", "approve or reject")),
    }
}

/// Appends the status history row of a 5C aggregate.
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

/// One status history entry (auditable history, L3-F-032).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HistoryView {
    pub from_status: Option<String>,
    pub to_status: String,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub process_instance_id: Option<String>,
    pub at: DateTime<Utc>,
}

/// The status history of one aggregate, oldest first.
pub async fn load_history(
    conn: &mut PgConnection,
    tenant: TenantId,
    aggregate_type: &str,
    id: Uuid,
) -> Result<Vec<HistoryView>, KernelError> {
    sqlx::query(
        "SELECT from_status, to_status, reason, actor, process_instance_id, created_at \
         FROM finance.status_history WHERE tenant_id = $1 AND aggregate_type = $2 \
         AND aggregate_id = $3 ORDER BY created_at, id",
    )
    .bind(tenant.as_uuid())
    .bind(aggregate_type)
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|h| -> Result<HistoryView, sqlx::Error> {
        Ok(HistoryView {
            from_status: h.try_get("from_status")?,
            to_status: h.try_get("to_status")?,
            reason: h.try_get("reason")?,
            actor: h.try_get("actor")?,
            process_instance_id: h.try_get("process_instance_id")?,
            at: h.try_get("created_at")?,
        })
    })
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

/// Builds the shared `StatusChangedV1` payload.
#[allow(clippy::too_many_arguments)]
pub fn changed(
    id: Uuid,
    institution: InstitutionId,
    from: Option<&str>,
    to: &str,
    version: i64,
    amount: Option<Decimal>,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    journal_entry_id: Option<Uuid>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id: id,
        institution_id: institution.as_uuid(),
        student_account_id: None,
        student_id: None,
        charge_id: None,
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        amount: amount.map(crate::shared::money::fmt),
        currency: amount.map(|_| "INR".to_owned()),
        reason: reason.map(str::to_owned),
        process_instance_id: process_instance_id.map(str::to_owned),
        journal_entry_id,
        student_subject_ref: None,
    }
}

/// Emits one 5C event + its audit entry in the caller's transaction.
#[allow(clippy::too_many_arguments)]
pub async fn emit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event_type: Option<(&'static str, &'static str)>,
    payload: &StatusChangedV1,
    action: &str,
    target_type: &str,
    details: serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    if let Some((t, schema)) = event_type {
        event(
            conn,
            scope,
            t,
            schema,
            payload.aggregate_id.to_string(),
            payload,
            actor,
        )
        .await?;
    }
    audit(
        conn,
        scope,
        action,
        target_type,
        payload.aggregate_id.to_string(),
        details,
        actor,
    )
    .await
}

/// Parses `YYYY-MM-DD` → 422 on the field.
pub fn parse_date(field: &str, raw: &str) -> Result<NaiveDate, KernelError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| invalid(field, "invalid_date", "must be a date YYYY-MM-DD"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_amounts_and_decisions() {
        assert_eq!(parse_amount("a", "12.50").ok(), Some(Decimal::new(1250, 2)));
        assert!(parse_amount("a", "0").is_err());
        assert!(parse_amount("a", "-1").is_err());
        assert!(parse_amount("a", "1.234").is_err());
        assert!(parse_amount("a", "abc").is_err());
        assert_eq!(parse_amount_or_zero("f", None).ok(), Some(Decimal::ZERO));
        assert_eq!(
            parse_amount_or_zero("f", Some("0.00")).ok(),
            Some(Decimal::ZERO)
        );
        assert_eq!(
            parse_signed("b", "-10.5").ok(),
            Some(Decimal::new(-1050, 2))
        );
        assert!(parse_signed("b", "1.001").is_err());
        assert_eq!(parse_decision("approve").ok(), Some(true));
        assert_eq!(parse_decision("reject").ok(), Some(false));
        assert!(parse_decision("maybe").is_err());
        assert!(require_text("r", Some("  ")).is_err());
        assert_eq!(opt_text(Some(" x ")), Some("x".into()));
        assert!(parse_date("d", "2026-02-30").is_err());
        assert!(require_version(Version(2), 3).is_err());
    }
}
