//! Helpers shared by the 7C payroll and payslip code: command context, validation errors,
//! version checks, status-history + event payload builders and decimal (de)serialisation.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::{Actor, PeopleState};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use sutra_contracts::people::events::StatusChangedV1;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use utoipa::ToSchema;
use uuid::Uuid;

/// Who runs a command, in which tenant.
#[derive(Clone, Copy)]
pub struct Cmd<'a> {
    pub st: &'a PeopleState,
    pub tenant: TenantId,
    pub actor: &'a Actor,
}

impl Cmd<'_> {
    pub fn scope(&self, institution: InstitutionId) -> ExecutionScope {
        ExecutionScope::institution(self.tenant, institution)
    }

    /// The system actor for automatic decisions in this command.
    pub fn auto(&self) -> Actor {
        Actor::system(self.actor.correlation_id.clone())
    }
}

/// 422 on one field.
pub fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message.into())])
}

/// 412 unless the caller's `If-Match` version is current.
pub fn ensure_version(expected: i64, actual: i64) -> Result<(), KernelError> {
    if expected == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict { expected, actual })
    }
}

/// 409 for an illegal transition.
pub fn illegal(what: &str, label: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{what} {label} is {from}; it cannot become {to}"))
}

/// A required, trimmed, non-empty reason (≤ 500 characters).
pub fn required_reason(r: Option<&str>) -> Result<String, KernelError> {
    match r.map(str::trim).filter(|r| !r.is_empty()) {
        Some(r) if r.chars().count() <= 500 => Ok(r.to_owned()),
        Some(_) => Err(invalid("reason", "too_long", "at most 500 characters")),
        None => Err(invalid("reason", "required", "a reason is required")),
    }
}

/// Optional trimmed reason.
pub fn opt_reason(r: Option<&str>) -> Option<String> {
    r.map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| r.chars().take(500).collect())
}

/// Parses a money string field.
pub fn money(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    crate::shared::decimal::parse_decimal(field, raw)
}

/// Parses an optional decimal string field.
pub fn opt_money(field: &str, raw: Option<&str>) -> Result<Option<Decimal>, KernelError> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| money(field, s))
        .transpose()
}

/// Parses a percentage (0 … 100, at most four decimals).
pub fn rate(field: &str, raw: Option<&str>) -> Result<Option<Decimal>, KernelError> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let v: Decimal = raw
        .parse()
        .map_err(|_| invalid(field, "invalid_number", "must be a decimal string"))?;
    if v.normalize().scale() > 4 || v < Decimal::ZERO || v > Decimal::from(100) {
        return Err(invalid(
            field,
            "invalid_rate",
            "a percentage between 0 and 100 with at most four decimals",
        ));
    }
    Ok(Some(v))
}

/// Two-place string of an optional decimal.
pub fn opt2(v: Option<Decimal>) -> Option<String> {
    v.map(crate::shared::decimal::fmt2)
}

/// Four-place string of a rate.
pub fn fmt4(v: Decimal) -> String {
    let mut r = v.round_dp(4);
    r.rescale(4);
    r.to_string()
}

/// Decision body (`POST …/decision`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct PayrollDecisionBody {
    pub decision: PayrollDecision,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PayrollDecision {
    Approve,
    Reject,
}

/// Optional reason body of state commands.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct PayrollReasonBody {
    pub reason: Option<String>,
}

/// Summary of a bulk command (`{processed, skipped, failed[]}`).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
pub struct PayrollBulkSummary {
    pub processed: i64,
    pub skipped: i64,
    pub failed: Vec<PayrollBulkFailure>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PayrollBulkFailure {
    pub id: Uuid,
    pub reason: String,
}

/// Fields of a [`StatusChangedV1`] payload.
#[derive(Default)]
pub struct Change<'a> {
    pub aggregate_id: Uuid,
    pub institution: Option<InstitutionId>,
    pub staff_id: Option<Uuid>,
    pub engagement_id: Option<Uuid>,
    pub from: Option<&'a str>,
    pub to: &'a str,
    pub version: i64,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
    pub staff_subject_ref: Option<&'a str>,
}

/// Builds the shared lifecycle payload.
pub fn payload(c: &Change<'_>) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id: c.aggregate_id,
        institution_id: c.institution.map(|i| i.as_uuid()).unwrap_or_default(),
        staff_id: c.staff_id,
        engagement_id: c.engagement_id,
        from_status: c.from.map(str::to_owned),
        to_status: c.to.to_owned(),
        version: c.version,
        reason: c.reason.map(str::to_owned),
        process_instance_id: c.process_instance_id.map(str::to_owned),
        staff_subject_ref: c.staff_subject_ref.map(str::to_owned),
        manager_subject_ref: None,
    }
}

/// Appends one `people.status_history` row of a 7C aggregate.
#[allow(clippy::too_many_arguments)]
pub async fn history(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    aggregate_type: &'static str,
    aggregate_id: Uuid,
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
            aggregate_id,
            from,
            to,
        },
        change,
        actor,
    )
    .await
}

/// A `StatusChange` with a cause and optional reason / process.
pub fn cause<'a>(
    cause_event_type: &'static str,
    reason: Option<&'a str>,
    process_instance_id: Option<&'a str>,
) -> StatusChange<'a> {
    StatusChange {
        cause_event_type,
        reason,
        process_instance_id,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_rates_and_versions() {
        assert!(required_reason(None).is_err());
        assert!(required_reason(Some("  ")).is_err());
        assert_eq!(required_reason(Some(" ok ")).unwrap_or_default(), "ok");
        assert!(rate("rate", Some("12.5")).is_ok());
        assert!(rate("rate", Some("120")).is_err());
        assert!(rate("rate", Some("1.23456")).is_err());
        assert_eq!(rate("rate", None).ok().flatten(), None);
        assert!(ensure_version(2, 2).is_ok());
        assert!(matches!(
            ensure_version(1, 2),
            Err(KernelError::VersionConflict { .. })
        ));
        assert_eq!(fmt4(Decimal::new(125, 1)), "12.5000");
    }
}
