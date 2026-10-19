//! Small helpers shared by the 6C areas (`results`, `revaluation`, `completion`).

use crate::shared::grading::fmt2;
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::Actor;
use rust_decimal::Decimal;
use serde::Deserialize;
use sqlx::PgConnection;
use std::str::FromStr;
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use utoipa::ToSchema;
use uuid::Uuid;

/// Optional reason of a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
pub struct ReasonBody {
    pub reason: Option<String>,
}

impl ReasonBody {
    /// Trimmed, non-empty reason.
    pub fn text(body: Option<axum::Json<ReasonBody>>) -> Option<String> {
        body.and_then(|axum::Json(b)| b.reason)
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty())
    }
}

/// Approve / reject decision of a governed step (§9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
}

/// Decision body (`POST …/decision`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct DecisionBody {
    pub decision: Decision,
    pub reason: Option<String>,
}

/// 422 on one field.
pub fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Parses a decimal string (`"72.50"`) → 422 on `field`.
pub fn parse_dec(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_number",
            "must be a decimal string such as \"72.50\"",
        )
    })
}

/// Optional decimal string.
pub fn parse_opt(field: &str, raw: Option<&str>) -> Result<Option<Decimal>, KernelError> {
    raw.map(|r| parse_dec(field, r)).transpose()
}

/// Two-place wire string of an optional decimal.
pub fn opt2(v: Option<Decimal>) -> Option<String> {
    v.map(fmt2)
}

/// 412 unless the stored version matches `If-Match`.
pub fn ensure_version(expected: i64, actual: i64) -> Result<(), KernelError> {
    if expected == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict { expected, actual })
    }
}

/// `StatusChangedV1` payload of a 6C aggregate transition.
#[allow(clippy::too_many_arguments)]
pub fn status_payload(
    aggregate_id: Uuid,
    institution_id: InstitutionId,
    session_id: Option<Uuid>,
    registration_id: Option<Uuid>,
    student_id: Option<Uuid>,
    from: Option<&str>,
    to: &str,
    version: i64,
    reason: Option<String>,
    process_instance_id: Option<String>,
    subject_ref: Option<String>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id,
        institution_id: institution_id.as_uuid(),
        session_id,
        paper_id: None,
        registration_id,
        student_id,
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        reason,
        process_instance_id,
        student_subject_ref: subject_ref,
    }
}

/// Appends a status history row of a 6C aggregate.
#[allow(clippy::too_many_arguments)]
pub async fn history(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    aggregate_type: &'static str,
    aggregate_id: Uuid,
    from: Option<&str>,
    to: &str,
    cause_event_type: &'static str,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
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
        &StatusChange {
            cause_event_type,
            reason,
            process_instance_id,
        },
        actor,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals_and_versions_are_checked() {
        assert_eq!(
            parse_dec("x", " 72.5 ").ok(),
            Decimal::from_str("72.5").ok()
        );
        assert!(parse_dec("x", "abc").is_err());
        assert_eq!(parse_opt("x", None).ok(), Some(None));
        assert!(ensure_version(2, 2).is_ok());
        assert!(matches!(
            ensure_version(1, 2),
            Err(KernelError::VersionConflict { .. })
        ));
        assert_eq!(opt2(Decimal::from_str("8").ok()), Some("8.00".into()));
    }
}
