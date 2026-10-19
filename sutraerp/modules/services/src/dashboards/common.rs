//! 8C reporting building blocks (dashboards + regulatory): request bodies, validation helpers, `If-Match` handling and the
//! one transition helper (status history + `StatusChangedV1` event + audit in the caller's
//! owner-local transaction).

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::publish;
use crate::shared::Actor;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use sutra_contracts::services::events::StatusChangedV1;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use utoipa::ToSchema;
use uuid::Uuid;

/// `If-Match` → expected version (`428` response returned early when missing).
macro_rules! expected {
    ($headers:expr) => {
        match $crate::shared::http::if_match(&$headers)? {
            Ok(v) => v.0,
            Err(resp) => return Ok(resp),
        }
    };
}
pub(crate) use expected;

/// Optional reason of a reporting state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportReasonBody {
    pub reason: Option<String>,
}

/// Approve / reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReportDecision {
    Approve,
    Reject,
}

/// Decision of a governed reporting step (`services.report.approve`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportDecisionBody {
    pub decision: ReportDecision,
    pub reason: Option<String>,
}

/// Result of a bulk command (`{processed, skipped, failed[]}`).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportBulkResult {
    pub processed: i64,
    pub skipped: i64,
    pub failed: Vec<ReportBulkFailure>,
}

/// One item a bulk command could not process.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReportBulkFailure {
    pub id: Uuid,
    pub reason: String,
}

/// 422 with one field violation.
pub fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Trimmed, `None` when blank.
pub fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Required trimmed text of 1..=`max` characters.
pub fn required_text(field: &str, v: &str, max: usize) -> Result<String, KernelError> {
    let t = v.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(invalid(
            field,
            "required",
            &format!("1 to {max} characters"),
        ));
    }
    Ok(t.to_owned())
}

/// Optional trimmed text of at most `max` characters.
pub fn optional_text(
    field: &str,
    v: Option<String>,
    max: usize,
) -> Result<Option<String>, KernelError> {
    let v = clean(v);
    if v.as_deref().is_some_and(|s| s.chars().count() > max) {
        return Err(invalid(
            field,
            "too_long",
            &format!("at most {max} characters"),
        ));
    }
    Ok(v)
}

/// A required reason (governed or restrictive commands).
pub fn required_reason(r: Option<String>) -> Result<String, KernelError> {
    let r = clean(r).ok_or_else(|| invalid("reason", "required", "a reason is required"))?;
    if r.chars().count() > 1000 {
        return Err(invalid("reason", "too_long", "at most 1000 characters"));
    }
    Ok(r)
}

/// Upper-case code `^[A-Z0-9][A-Z0-9_-]{1,31}$` (dashboards, widgets, frameworks).
pub fn valid_code(field: &str, raw: &str) -> Result<String, KernelError> {
    let c = raw.trim().to_uppercase();
    let b = c.as_bytes();
    let ok = (2..=32).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|x| x.is_ascii_uppercase() || x.is_ascii_digit() || matches!(x, b'_' | b'-'));
    if ok {
        Ok(c)
    } else {
        Err(invalid(
            field,
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_-]{1,31}$",
        ))
    }
}

/// Strict decode of a stored status / classification name.
pub fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown value `{raw}`").into()))
}

/// `412` unless the stored version is the expected one.
pub fn check_version(expected: i64, actual: i64) -> Result<(), KernelError> {
    if expected == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict { expected, actual })
    }
}

/// `?limit=` (default 50, at most 200).
pub fn page_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(50).clamp(1, 200)
}

/// One status transition of a reporting aggregate.
#[derive(Clone, Debug)]
pub struct Transition<'a> {
    pub aggregate_type: &'static str,
    pub id: Uuid,
    pub institution: InstitutionId,
    pub from: Option<&'a str>,
    pub to: &'a str,
    /// Aggregate version after the transition.
    pub version: i64,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
    pub number: Option<&'a str>,
    pub subject_ref: Option<&'a str>,
}

/// Records the transition: `services.status_history`, the business event when one is released
/// (`event` = `(type, dataschema)`), and the audit entry `action`.
pub async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: &Transition<'_>,
    event: Option<(&'static str, &'static str)>,
    action: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    record_history(
        conn,
        HistoryEntry {
            tenant: scope.tenant_id,
            institution: t.institution,
            aggregate_type: t.aggregate_type,
            aggregate_id: t.id,
            from: t.from,
            to: t.to,
        },
        &StatusChange {
            cause_event_type: event.map_or(action, |e| e.0),
            reason: t.reason,
            process_instance_id: t.process_instance_id,
        },
        actor,
    )
    .await?;
    if let Some((event_type, schema)) = event {
        publish::event(
            conn,
            scope,
            event_type,
            schema,
            t.id.to_string(),
            &StatusChangedV1 {
                aggregate_id: t.id,
                institution_id: t.institution.as_uuid(),
                from_status: t.from.map(str::to_owned),
                to_status: t.to.to_owned(),
                version: t.version,
                reason: t.reason.map(str::to_owned),
                process_instance_id: t.process_instance_id.map(str::to_owned),
                number: t.number.map(str::to_owned),
                subject_ref: t.subject_ref.map(str::to_owned),
            },
            actor,
        )
        .await?;
    }
    publish::audit(
        conn,
        scope,
        action,
        t.aggregate_type,
        t.id.to_string(),
        serde_json::json!({
            "from": t.from,
            "to": t.to,
            "reason": t.reason,
            "number": t.number,
            "processInstanceId": t.process_instance_id,
        }),
        actor,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(page_limit(None), 50);
        assert_eq!(page_limit(Some(1000)), 200);
        assert_eq!(page_limit(Some(0)), 1);
        assert_eq!(clean(Some("  ".into())), None);
        assert!(required_text("title", " ", 10).is_err());
        assert!(optional_text("note", Some("abc".into()), 2).is_err());
        assert!(check_version(2, 3).is_err());
        assert!(required_reason(None).is_err());
        assert_eq!(valid_code("code", " aishe ").unwrap_or_default(), "AISHE");
        assert!(valid_code("code", "x").is_err());
        assert!(valid_code("code", "BAD CODE").is_err());
    }
}
