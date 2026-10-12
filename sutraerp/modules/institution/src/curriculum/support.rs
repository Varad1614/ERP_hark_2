//! Helpers shared by the 2B areas (`campus`, `curriculum`): validation, owner-local transaction
//! set-up, optimistic-concurrency checks, command bodies and lifecycle event emission.

use crate::shared::{guard, publish, Actor};
use sqlx::{PgConnection, PgPool, Postgres, Transaction};
use sutra_contracts::institution::events::StatusChangedV1;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub(crate) fn violation(field: &str, code: &str, message: impl Into<String>) -> FieldViolation {
    FieldViolation::new(field, code, message.into())
}

pub(crate) fn finish(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

pub(crate) fn required_text(field: &str, value: &str, max: usize, v: &mut Vec<FieldViolation>) {
    let len = value.trim().chars().count();
    if len == 0 {
        v.push(violation(field, "required", format!("{field} is required")));
    } else if len > max {
        v.push(violation(
            field,
            "too-long",
            format!("{field} must be at most {max} characters"),
        ));
    }
}

pub(crate) fn optional_text(
    field: &str,
    value: Option<&String>,
    max: usize,
    v: &mut Vec<FieldViolation>,
) {
    if let Some(s) = value {
        if s.chars().count() > max {
            v.push(violation(
                field,
                "too-long",
                format!("{field} must be at most {max} characters"),
            ));
        }
    }
}

/// Business code: 2–32 characters of `A-Z`, `0-9`, `-`.
pub(crate) fn code_format(field: &str, code: &str, v: &mut Vec<FieldViolation>) {
    let c = code.trim();
    if !(2..=32).contains(&c.len())
        || !c
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    {
        v.push(violation(
            field,
            "format",
            format!("{field} must be 2-32 characters of A-Z, 0-9 or '-'"),
        ));
    }
}

/// Structure key: lowercase slug (`a-z0-9`, then also `_ . -`), 1–40 characters.
pub(crate) fn key_ok(key: &str, max: usize) -> bool {
    let b = key.as_bytes();
    !b.is_empty()
        && b.len() <= max
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
        })
}

/// IANA zone shape (`Area/Location`), same rule as the institution aggregate.
pub(crate) fn timezone_ok(tz: &str) -> bool {
    let tz = tz.trim();
    !tz.is_empty()
        && tz.contains('/')
        && tz
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'_' | b'-' | b'+'))
}

/// Credits are recorded in half-credit steps (0.5); returns the value in half-credits.
pub(crate) fn half_credits(value: f64) -> Option<i64> {
    let doubled = value * 2.0;
    if value.is_finite() && value >= 0.0 && (doubled - doubled.round()).abs() < 1e-9 {
        #[allow(clippy::cast_possible_truncation)]
        Some(doubled.round() as i64)
    } else {
        None
    }
}

/// Institution of a 2B row of the session tenant (404 when missing or foreign).
pub(crate) async fn institution_of(
    pool: &PgPool,
    tenant: &ExecutionScope,
    table: &'static str,
    entity: &'static str,
    id: Uuid,
) -> Result<InstitutionId, KernelError> {
    let found: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT institution_id FROM institution.{table} WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(map_db_error)?;
    found
        .map(InstitutionId)
        .ok_or(KernelError::NotFound { entity })
}

/// Opens the owner-local transaction and takes the institution guard (`FOR SHARE`; suspended /
/// archived institution → 409).
pub(crate) async fn begin(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Transaction<'static, Postgres>, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    guard::lock_institution_for_change(&mut tx, scope, institution_id).await?;
    Ok(tx)
}

/// Optimistic concurrency: 412 when the stored version differs from `If-Match`.
pub(crate) fn expect_version(expected: Version, actual: i64) -> Result<(), KernelError> {
    Version(actual).expect(expected)
}

/// Optional JSON command body (`{}` or empty → default).
pub(crate) fn body<T: serde::de::DeserializeOwned + Default>(
    bytes: &[u8],
) -> Result<T, KernelError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(bytes)
        .map_err(|e| KernelError::BadRequest(format!("invalid JSON body: {e}")))
}

/// Body of a state command: optional business reason.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReasonBody {
    /// Business reason / decision note (max 2000 characters).
    pub reason: Option<String>,
}

impl ReasonBody {
    pub(crate) fn validated(self, required: bool) -> Result<Option<String>, KernelError> {
        let reason = self
            .reason
            .map(|r| r.trim().to_owned())
            .filter(|r| !r.is_empty());
        let mut v = vec![];
        match &reason {
            None if required => v.push(violation("reason", "required", "a reason is required")),
            Some(r) if r.chars().count() > 2000 => v.push(violation(
                "reason",
                "too-long",
                "reason must be at most 2000 characters",
            )),
            _ => {}
        }
        finish(v)?;
        Ok(reason)
    }
}

/// Outcome chosen by a decision-maker on a pending governed step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
    /// Back to draft for correction (publication decisions only).
    Return,
}

/// Body of `POST …/decision`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionBody {
    pub decision: Decision,
    pub reason: Option<String>,
}

impl DecisionBody {
    pub(crate) fn parse(bytes: &[u8]) -> Result<(Decision, Option<String>), KernelError> {
        let b: DecisionBody = serde_json::from_slice(bytes)
            .map_err(|e| KernelError::BadRequest(format!("invalid decision body: {e}")))?;
        let reason = ReasonBody { reason: b.reason }.validated(b.decision != Decision::Approve)?;
        Ok((b.decision, reason))
    }
}

/// One lifecycle transition: `StatusChangedV1` event + audit entry in the caller's transaction.
pub(crate) struct Transition<'a> {
    pub event_type: &'static str,
    pub dataschema: &'static str,
    pub audit_action: &'static str,
    pub target_type: &'static str,
    pub aggregate_id: Uuid,
    pub from: &'a str,
    pub to: &'a str,
    pub version: i64,
    pub reason: Option<String>,
    pub process_instance_id: Option<String>,
}

impl Transition<'_> {
    pub(crate) async fn emit(
        self,
        conn: &mut PgConnection,
        scope: &ExecutionScope,
        actor: &Actor,
    ) -> Result<(), KernelError> {
        let institution_id = scope
            .institution_id
            .map(|i| i.as_uuid())
            .ok_or_else(|| KernelError::internal_msg("institution scope required"))?;
        let data = StatusChangedV1 {
            aggregate_id: self.aggregate_id,
            institution_id,
            from_status: self.from.to_owned(),
            to_status: self.to.to_owned(),
            version: self.version,
            reason: self.reason.clone(),
            process_instance_id: self.process_instance_id.clone(),
        };
        publish::event(
            conn,
            scope,
            self.event_type,
            self.dataschema,
            self.aggregate_id.to_string(),
            &data,
            actor,
        )
        .await?;
        audit_transition(
            conn,
            scope,
            self.audit_action,
            self.target_type,
            self.aggregate_id,
            self.from,
            self.to,
            self.version,
            self.reason.as_deref(),
            self.process_instance_id.as_deref(),
            actor,
        )
        .await
    }
}

/// Audit entry of a transition that has no public event of its own (e.g. submitted for
/// approval, returned, rejected).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn audit_transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    action: &str,
    target_type: &str,
    id: Uuid,
    from: &str,
    to: &str,
    version: i64,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::audit(
        conn,
        scope,
        action,
        target_type,
        id.to_string(),
        serde_json::json!({
            "fromStatus": from,
            "toStatus": to,
            "version": version,
            "reason": reason,
            "processInstanceId": process_instance_id,
        }),
        actor,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validators() {
        assert!(key_ok("ug", 40) && key_ok("b.tech", 40) && !key_ok("UG", 40) && !key_ok("", 4));
        assert!(timezone_ok("Asia/Kolkata") && !timezone_ok("IST"));
        assert_eq!(half_credits(3.5), Some(7));
        assert_eq!(half_credits(3.25), None);
        assert_eq!(half_credits(-1.0), None);
        let mut v = vec![];
        code_format("code", "MAIN-1", &mut v);
        code_format("code", "x", &mut v);
        required_text("name", " ", 10, &mut v);
        assert_eq!(v.len(), 2);
        assert!(body::<ReasonBody>(b"").is_ok());
        assert!(body::<ReasonBody>(b"{\"bogus\":1}").is_err());
        assert!(ReasonBody { reason: None }.validated(true).is_err());
        assert_eq!(
            ReasonBody {
                reason: Some(" ok ".into())
            }
            .validated(true)
            .ok()
            .flatten()
            .as_deref(),
            Some("ok")
        );
        let (d, r) =
            DecisionBody::parse(br#"{"decision":"approve"}"#).unwrap_or((Decision::Reject, None));
        assert_eq!((d, r), (Decision::Approve, None));
        assert!(DecisionBody::parse(br#"{"decision":"reject"}"#).is_err());
    }
}
