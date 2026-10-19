//! Helpers shared by the 7B areas (leave, attendance, workload): request bodies, the `If-Match`
//! macro, transition history + `StatusChangedV1` events, row lookups of the 7B tables and of the
//! foundation staff rows.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::publish;
use crate::shared::Actor;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use sutra_contracts::people::events::StatusChangedV1;
use sutra_contracts::people::ids::StaffMemberId;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// Reads the required `If-Match` version; returns the ready 428 response when it is missing.
macro_rules! expected {
    ($headers:expr) => {
        match $crate::shared::http::if_match(&$headers)? {
            Ok(v) => v.0,
            Err(resp) => return Ok(resp),
        }
    };
}
pub(crate) use expected;

/// Approve or reject (`POST …/decision`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum LeaveDecision {
    Approve,
    Reject,
}

/// Decision body of every 7B governed decision.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct LeaveDecisionBody {
    pub decision: LeaveDecision,
    pub reason: Option<String>,
}

impl LeaveDecisionBody {
    pub fn approve(&self) -> bool {
        self.decision == LeaveDecision::Approve
    }
}

/// Optional reason of a state command (`{}` allowed).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct LeaveReasonBody {
    pub reason: Option<String>,
}

/// One row a bulk command could not process.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveBulkFailure {
    /// Staff member / request concerned.
    pub id: Uuid,
    pub reason: String,
}

/// Summary of a bulk command (`{processed, skipped, failed[]}`, contract §5.1).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveBulkSummary {
    pub processed: i64,
    pub skipped: i64,
    pub failed: Vec<LeaveBulkFailure>,
}

/// 422 on one field.
pub fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// 412 unless the caller's `If-Match` version is the current one.
pub fn ensure_version(expected: i64, actual: i64) -> Result<(), KernelError> {
    if expected == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict { expected, actual })
    }
}

/// 409 for an illegal transition of a slice aggregate.
pub fn illegal(what: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{what} is {from}; it cannot become {to}"))
}

/// Trimmed non-empty text or `None`.
pub fn clean(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

/// Required text field (422 `required`).
pub fn required(field: &str, s: Option<String>) -> Result<String, KernelError> {
    clean(s).ok_or_else(|| invalid(field, "required", "this field is required"))
}

/// Parses a `YYYY-MM-DD` date field.
pub fn parse_date(field: &str, raw: &str) -> Result<NaiveDate, KernelError> {
    NaiveDate::parse_from_str(raw.trim(), "%Y-%m-%d")
        .map_err(|_| invalid(field, "invalid_date", "must be a date YYYY-MM-DD"))
}

/// Parses a non-negative two-place decimal string field.
pub fn non_negative(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = crate::shared::decimal::parse_decimal(field, raw)?;
    if v < Decimal::ZERO {
        return Err(invalid(field, "negative", "must not be negative"));
    }
    Ok(v)
}

/// One status transition of a 7B aggregate (history row + optional event).
pub struct Transition<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub aggregate_type: &'static str,
    pub aggregate_id: Uuid,
    pub from: Option<&'a str>,
    pub to: &'a str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
    /// Business event type behind the change (history `cause_event_type`).
    pub cause: &'static str,
}

/// Appends the `people.status_history` row of a transition.
pub async fn history(
    conn: &mut PgConnection,
    t: &Transition<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    record_history(
        conn,
        HistoryEntry {
            tenant: t.tenant,
            institution: t.institution,
            aggregate_type: t.aggregate_type,
            aggregate_id: t.aggregate_id,
            from: t.from,
            to: t.to,
        },
        &StatusChange {
            cause_event_type: t.cause,
            reason: t.reason,
            process_instance_id: t.process_instance_id,
        },
        actor,
    )
    .await
}

/// Who the event concerns.
#[derive(Clone, Debug, Default)]
pub struct EventParties {
    pub staff_id: Option<Uuid>,
    pub engagement_id: Option<Uuid>,
    pub staff_subject_ref: Option<String>,
    pub manager_subject_ref: Option<String>,
}

/// Emits a `StatusChangedV1` event (outbox, same transaction).
#[allow(clippy::too_many_arguments)]
pub async fn emit_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event_type: &'static str,
    schema: &'static str,
    t: &Transition<'_>,
    version: i64,
    parties: &EventParties,
    actor: &Actor,
) -> Result<(), KernelError> {
    let payload = StatusChangedV1 {
        aggregate_id: t.aggregate_id,
        institution_id: t.institution.as_uuid(),
        staff_id: parties.staff_id,
        engagement_id: parties.engagement_id,
        from_status: t.from.map(str::to_owned),
        to_status: t.to.to_owned(),
        version,
        reason: t.reason.map(str::to_owned),
        process_instance_id: t.process_instance_id.map(str::to_owned),
        staff_subject_ref: parties.staff_subject_ref.clone(),
        manager_subject_ref: parties.manager_subject_ref.clone(),
    };
    publish::event(
        conn,
        scope,
        event_type,
        schema,
        t.aggregate_id.to_string(),
        &payload,
        actor,
    )
    .await
}

/// Writes the audit entry `people.<aggregate>.<command>`.
pub async fn audit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    action: &str,
    target_type: &str,
    target_id: Uuid,
    details: serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::audit(
        conn,
        scope,
        action,
        target_type,
        target_id.to_string(),
        details,
        actor,
    )
    .await
}

/// Login subject of a staff record (for pure inbox rules).
pub async fn subject_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: Option<StaffMemberId>,
) -> Result<Option<String>, KernelError> {
    let Some(staff) = staff else {
        return Ok(None);
    };
    let s: Option<Option<String>> = sqlx::query_scalar(
        "SELECT subject_ref FROM people.staff_member WHERE id = $1 AND tenant_id = $2",
    )
    .bind(staff.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(s.flatten())
}

/// List limit (`?limit=` ≤ 200, default 50).
pub fn limit(raw: Option<i64>) -> i64 {
    raw.unwrap_or(50).clamp(1, 200)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_helpers() {
        assert_eq!(clean(Some("  ".into())), None);
        assert_eq!(clean(Some(" a ".into())), Some("a".into()));
        assert!(required("reason", None).is_err());
        assert!(parse_date("startDate", "2026-07-30").is_ok());
        assert!(parse_date("startDate", "30/07/2026").is_err());
        assert!(non_negative("days", "-1").is_err());
        assert!(ensure_version(2, 3).is_err());
        assert_eq!(limit(Some(500)), 200);
        assert_eq!(limit(None), 50);
    }
}
