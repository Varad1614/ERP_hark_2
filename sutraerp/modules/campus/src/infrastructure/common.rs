//! Helpers shared by the 8A areas (infrastructure, reservations, service desk, maintenance,
//! labs): request bodies, the `If-Match` macro, transition history + `StatusChangedV1` events,
//! audit, small parsers. Owned by slice 8A.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::publish;
use crate::shared::Actor;
use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use sutra_contracts::campus::events::{ModifiedV1, StatusChangedV1};
use sutra_contracts::campus::status::PersonKind;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
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
pub enum FacilityDecision {
    Approve,
    Reject,
}

/// Decision body of every 8A governed decision.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct FacilityDecisionBody {
    pub decision: FacilityDecision,
    pub reason: Option<String>,
}

impl FacilityDecisionBody {
    pub fn approve(&self) -> bool {
        self.decision == FacilityDecision::Approve
    }
}

/// Optional reason of a state command (`{}` allowed).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct FacilityReasonBody {
    pub reason: Option<String>,
}

/// One row a bulk command could not process.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacilityBulkFailure {
    pub id: Uuid,
    pub reason: String,
}

/// Summary of a bulk command (`{processed, skipped, failed[]}`, contract §5.1).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacilityBulkSummary {
    pub processed: i64,
    pub skipped: i64,
    pub failed: Vec<FacilityBulkFailure>,
}

/// A student or staff member named by the office (validated through the directories).
#[derive(Debug, Clone, Copy, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacilityPersonInput {
    pub kind: PersonKind,
    /// Domain C student id or Domain E staff id.
    pub id: Uuid,
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

/// 409 for an illegal transition of an 8A aggregate.
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

/// Parses an `HH:MM` time field (business-local).
pub fn parse_time(field: &str, raw: &str) -> Result<NaiveTime, KernelError> {
    NaiveTime::parse_from_str(raw.trim(), "%H:%M")
        .map_err(|_| invalid(field, "invalid_time", "must be a time HH:MM"))
}

/// `HH:MM` wire form of a time.
pub fn fmt_time(t: NaiveTime) -> String {
    t.format("%H:%M").to_string()
}

/// List limit (`?limit=` ≤ 200, default 50).
pub fn limit(raw: Option<i64>) -> i64 {
    raw.unwrap_or(50).clamp(1, 200)
}

/// Parses an optional status filter (422 on an unknown value).
pub fn parse_filter<T>(
    field: &str,
    raw: Option<&str>,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, KernelError> {
    raw.filter(|r| !r.is_empty())
        .map(|r| parse(r).ok_or_else(|| invalid(field, "invalid", "unknown value")))
        .transpose()
}

/// One status transition of an 8A aggregate (history row + optional event).
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

/// Appends the `campus.status_history` row of a transition.
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

/// Extra payload members of a `StatusChangedV1` event.
#[derive(Clone, Debug, Default)]
pub struct EventExtras {
    pub number: Option<String>,
    pub subject_ref: Option<String>,
}

/// Emits a `StatusChangedV1` event for a transition (outbox, same transaction).
pub async fn emit_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event: (&'static str, &'static str),
    t: &Transition<'_>,
    version: i64,
    extras: &EventExtras,
    actor: &Actor,
) -> Result<(), KernelError> {
    let payload = StatusChangedV1 {
        aggregate_id: t.aggregate_id,
        institution_id: t.institution.as_uuid(),
        from_status: t.from.map(str::to_owned),
        to_status: t.to.to_owned(),
        version,
        reason: t.reason.map(str::to_owned),
        process_instance_id: t.process_instance_id.map(str::to_owned),
        number: extras.number.clone(),
        subject_ref: extras.subject_ref.clone(),
    };
    publish::event(
        conn,
        scope,
        event.0,
        event.1,
        t.aggregate_id.to_string(),
        &payload,
        actor,
    )
    .await
}

/// Emits a `ModifiedV1` event.
pub async fn emit_modified(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event: (&'static str, &'static str),
    aggregate_id: Uuid,
    version: i64,
    changed_fields: Vec<String>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let institution_id = scope
        .institution_id
        .map(|i| i.as_uuid())
        .unwrap_or_default();
    publish::event(
        conn,
        scope,
        event.0,
        event.1,
        aggregate_id.to_string(),
        &ModifiedV1 {
            aggregate_id,
            institution_id,
            version,
            changed_fields,
        },
        actor,
    )
    .await
}

/// Writes the audit entry `campus.<aggregate>.<command>`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_helpers() {
        assert_eq!(clean(Some("  ".into())), None);
        assert_eq!(clean(Some(" a ".into())), Some("a".into()));
        assert!(required("reason", None).is_err());
        assert!(parse_date("date", "2026-11-30").is_ok());
        assert!(parse_date("date", "30/11/2026").is_err());
        assert_eq!(
            parse_time("startTime", "09:30").map(fmt_time).ok(),
            Some("09:30".to_owned())
        );
        assert!(parse_time("startTime", "9h").is_err());
        assert!(ensure_version(2, 3).is_err());
        assert_eq!(limit(Some(500)), 200);
        assert_eq!(limit(None), 50);
        assert!(matches!(
            parse_filter("status", Some(""), PersonKind::parse),
            Ok(None)
        ));
        assert!(parse_filter("status", Some("x"), PersonKind::parse).is_err());
    }
}
