//! Small helpers shared by the 5A areas (charges, adjustments, aid, configuration): request
//! parsing, decisions, status events and list paging. Slice-owned (5A).

use crate::shared::publish::event;
use crate::shared::Actor;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::str::FromStr;
use sutra_contracts::finance::events::StatusChangedV1;
use sutra_contracts::finance::ids::StudentAccountId;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// 422 on one field.
pub fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Parses a decimal string amount (positive, ≤ 2 decimals) → 422 on `field`.
pub fn parse_amount(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let d = Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_amount",
            "must be a decimal string such as \"1250.00\"",
        )
    })?;
    crate::shared::money::require_positive(field, d)
}

/// Parses a decimal string that may be zero or positive (percentages, totals).
pub fn parse_decimal(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    Decimal::from_str(raw.trim())
        .map_err(|_| invalid(field, "invalid_number", "must be a decimal string"))
}

/// A required, non-blank reason.
pub fn require_text(field: &str, raw: Option<&str>) -> Result<String, KernelError> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(field, "required", "is required"))
}

/// Trimmed optional text (`None` when blank).
pub fn opt_text(raw: Option<&str>) -> Option<String> {
    raw.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// `412` unless the aggregate is still at the version the caller saw.
pub fn require_version(current: i64, expected: i64) -> Result<(), KernelError> {
    if current == expected {
        Ok(())
    } else {
        Err(KernelError::VersionConflict {
            expected,
            actual: current,
        })
    }
}

/// Optional reason of a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonBody {
    pub reason: Option<String>,
}

/// Decision of a governed step (`finance.approve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Approve,
    Reject,
}

/// Body of every `…/decision` endpoint.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionBody {
    pub decision: DecisionKind,
    pub reason: Option<String>,
}

/// `?status=&limit=` of a collection.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ListQuery {
    /// Wire status filter.
    pub status: Option<String>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
}

impl ListQuery {
    pub fn limit(&self) -> i64 {
        self.limit.unwrap_or(50).clamp(1, 200)
    }
}

/// The login subject of a party account (for pure student notifications).
pub async fn subject_of(
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
    .map(Option::flatten)
    .map_err(map_db_error)
}

/// Fields of a [`StatusChangedV1`] payload.
#[derive(Debug, Clone, Default)]
pub struct StatusEvent {
    pub aggregate_id: Uuid,
    pub student_account_id: Option<Uuid>,
    pub student_id: Option<Uuid>,
    pub charge_id: Option<Uuid>,
    pub from: Option<String>,
    pub to: String,
    pub version: i64,
    pub amount: Option<Decimal>,
    pub reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub journal_entry_id: Option<Uuid>,
    pub subject: Option<String>,
}

/// Emits a `StatusChangedV1` event of this slice in the caller's transaction.
pub async fn status_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    (event_type, schema): (&'static str, &'static str),
    e: StatusEvent,
    actor: &Actor,
) -> Result<(), KernelError> {
    let institution = scope
        .institution_id
        .map(|i| i.as_uuid())
        .unwrap_or_default();
    let payload = StatusChangedV1 {
        aggregate_id: e.aggregate_id,
        institution_id: institution,
        student_account_id: e.student_account_id,
        student_id: e.student_id,
        charge_id: e.charge_id,
        from_status: e.from,
        to_status: e.to,
        version: e.version,
        amount: e.amount.map(crate::shared::money::fmt),
        currency: e.amount.map(|_| "INR".to_owned()),
        reason: e.reason,
        process_instance_id: e.process_instance_id,
        journal_entry_id: e.journal_entry_id,
        student_subject_ref: e.subject,
    };
    event(
        conn,
        scope,
        event_type,
        schema,
        e.aggregate_id.to_string(),
        &payload,
        actor,
    )
    .await
}

/// Reason of an optional `{ "reason": … }` body.
pub fn reason(body: Option<axum::Json<ReasonBody>>) -> Option<String> {
    body.and_then(|b| opt_text(b.0.reason.as_deref()))
}

/// Reads the required `If-Match` version inside a handler: returns the ready `428` response
/// from the handler when it is missing.
macro_rules! if_match {
    ($headers:expr) => {
        match $crate::shared::http::if_match(&$headers)? {
            Ok(v) => v.0,
            Err(r) => return Ok(r),
        }
    };
}
pub(crate) use if_match;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amounts_parse_strictly() {
        assert_eq!(
            parse_amount("amount", "12.5").ok(),
            Decimal::from_str("12.50").ok()
        );
        assert!(parse_amount("amount", "12.555").is_err());
        assert!(parse_amount("amount", "abc").is_err());
        assert!(parse_amount("amount", "-1").is_err());
        assert!(require_text("reason", Some("  ")).is_err());
        assert_eq!(opt_text(Some(" x ")).as_deref(), Some("x"));
        assert!(require_version(2, 2).is_ok());
        assert!(matches!(
            require_version(3, 2),
            Err(KernelError::VersionConflict { .. })
        ));
        assert_eq!(ListQuery::default().limit(), 50);
    }
}
