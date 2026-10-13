//! 7A building blocks shared by `workforce`, `positions`, `hr` and `profile`: request bodies,
//! validation helpers, `If-Match` handling, event payload builders and reference checks.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::publish;
use crate::shared::rows::load_staff;
use crate::shared::{Actor, PeopleState};
use chrono::NaiveDate;
use serde::Deserialize;
use sqlx::{PgConnection, Row};
use sutra_contracts::institution::status::{CampusStatus, OrgUnitStatus};
use sutra_contracts::people::events::{ModifiedV1, StatusChangedV1};
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
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

/// Optional reason of a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReasonBody {
    pub reason: Option<String>,
}

/// Approve / reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Approve,
    Reject,
}

/// Decision of a governed step (`people.approve`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionBody {
    pub decision: Decision,
    pub reason: Option<String>,
}

/// 422 with one field violation.
pub fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Trimmed, `None` when blank.
pub fn clean(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// Reason of an optional body.
pub fn reason_of(body: Option<axum::Json<ReasonBody>>) -> Option<String> {
    clean(body.and_then(|b| b.0.reason))
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

/// A required reason (governed commands).
pub fn required_reason(r: Option<String>) -> Result<String, KernelError> {
    let r = clean(r).ok_or_else(|| invalid("reason", "required", "a reason is required"))?;
    if r.chars().count() > 1000 {
        return Err(invalid("reason", "too_long", "at most 1000 characters"));
    }
    Ok(r)
}

/// Grade / pay level code (same rule as the foundation inserter).
pub fn valid_code(field: &str, v: Option<String>) -> Result<Option<String>, KernelError> {
    let v = clean(v);
    if let Some(g) = &v {
        let b = g.as_bytes();
        let ok = (1..=32).contains(&b.len())
            && b[0].is_ascii_alphanumeric()
            && b.iter()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'));
        if !ok {
            return Err(invalid(
                field,
                "invalid_code",
                "must match ^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}$",
            ));
        }
    }
    Ok(v)
}

/// `412` unless the stored version is the expected one.
pub fn check_version(expected: i64, actual: i64) -> Result<(), KernelError> {
    if expected == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict { expected, actual })
    }
}

/// 409 for an illegal transition of a slice aggregate.
pub fn illegal(what: &str, label: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{what} {label} is {from}; it cannot become {to}"))
}

/// Validates a Domain B org unit (422 on the field when unknown / foreign / not usable).
pub async fn check_org_unit(
    st: &PeopleState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    field: &str,
    id: Option<Uuid>,
) -> Result<(), KernelError> {
    let Some(id) = id else { return Ok(()) };
    match st.references.org_unit(scope, id).await? {
        Some(u)
            if u.institution_id == institution
                && matches!(u.status, OrgUnitStatus::Active | OrgUnitStatus::Draft) =>
        {
            Ok(())
        }
        Some(_) => Err(invalid(
            field,
            "not_usable",
            "the org unit is not an operational unit of this institution",
        )),
        None => Err(invalid(field, "not_found", "unknown org unit")),
    }
}

/// Validates a Domain B campus (422 on the field when unknown / foreign / not active).
pub async fn check_campus(
    st: &PeopleState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    field: &str,
    id: Option<Uuid>,
) -> Result<(), KernelError> {
    let Some(id) = id else { return Ok(()) };
    match st.references.campus(scope, id).await? {
        Some(c) if c.institution_id == institution && c.status == CampusStatus::Active => Ok(()),
        Some(_) => Err(invalid(
            field,
            "not_usable",
            "the campus is not an active campus of this institution",
        )),
        None => Err(invalid(field, "not_found", "unknown campus")),
    }
}

/// Login subject of a staff record (for staff notifications).
pub async fn subject_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: Option<StaffMemberId>,
) -> Result<Option<String>, KernelError> {
    let Some(id) = staff else { return Ok(None) };
    Ok(load_staff(conn, tenant, id)
        .await?
        .and_then(|s| s.subject_ref))
}

/// Staff record → 404 unless it belongs to the caller's tenant.
pub async fn staff_or_404(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<StaffSummary, KernelError> {
    load_staff(conn, tenant, StaffMemberId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })
}

/// Lifecycle payload of an aggregate that belongs to a staff member / engagement.
#[allow(clippy::too_many_arguments)]
pub fn status_payload(
    aggregate_id: Uuid,
    institution: InstitutionId,
    staff: Option<&StaffSummary>,
    engagement: Option<&EngagementSummary>,
    from: Option<&str>,
    to: &str,
    version: i64,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    manager_subject_ref: Option<String>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id,
        institution_id: institution.as_uuid(),
        staff_id: staff
            .map(|s| s.id.as_uuid())
            .or_else(|| engagement.map(|e| e.staff_id.as_uuid())),
        engagement_id: engagement.map(|e| e.id.as_uuid()),
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        reason: reason.map(str::to_owned),
        process_instance_id: process_instance_id.map(str::to_owned),
        staff_subject_ref: staff.and_then(|s| s.subject_ref.clone()),
        manager_subject_ref,
    }
}

/// Modification payload.
pub fn modified(
    aggregate_id: Uuid,
    institution: InstitutionId,
    version: i64,
    fields: &[&str],
) -> ModifiedV1 {
    ModifiedV1 {
        aggregate_id,
        institution_id: institution.as_uuid(),
        version,
        changed_fields: fields.iter().map(|f| (*f).to_owned()).collect(),
    }
}

/// Emits an event (outbox) and the matching audit entry in the caller's transaction.
#[allow(clippy::too_many_arguments)]
pub async fn emit<T: serde::Serialize>(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event_type: &'static str,
    schema: &'static str,
    target_type: &str,
    aggregate_id: Uuid,
    action: &str,
    data: &T,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::event(
        conn,
        scope,
        event_type,
        schema,
        aggregate_id.to_string(),
        data,
        actor,
    )
    .await?;
    let details = serde_json::to_value(data).map_err(KernelError::internal)?;
    publish::audit(
        conn,
        scope,
        action,
        target_type,
        aggregate_id.to_string(),
        details,
        actor,
    )
    .await
}

/// Audit entry only (commands without a released event).
pub async fn audit_only(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    target_type: &str,
    aggregate_id: Uuid,
    action: &str,
    details: serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::audit(
        conn,
        scope,
        action,
        target_type,
        aggregate_id.to_string(),
        details,
        actor,
    )
    .await
}

/// Appends a status history row of a 7A aggregate.
#[allow(clippy::too_many_arguments)]
pub async fn history(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    aggregate_type: &'static str,
    aggregate_id: Uuid,
    from: Option<&str>,
    to: &str,
    cause: &'static str,
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
            cause_event_type: cause,
            reason,
            process_instance_id,
        },
        actor,
    )
    .await
}

/// Reads the profile columns the foundation summary does not carry.
pub async fn staff_extra(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StaffMemberId,
) -> Result<StaffExtra, KernelError> {
    let row = sqlx::query(
        "SELECT phone, date_of_birth, gender, status_reason, verified_at FROM people.staff_member \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "staff member",
    })?;
    let get = || -> Result<StaffExtra, sqlx::Error> {
        Ok(StaffExtra {
            phone: row.try_get("phone")?,
            date_of_birth: row.try_get("date_of_birth")?,
            gender: row.try_get("gender")?,
            status_reason: row.try_get("status_reason")?,
            verified_at: row.try_get("verified_at")?,
        })
    };
    get().map_err(map_db_error)
}

/// Profile columns beyond [`StaffSummary`].
#[derive(Debug, Clone)]
pub struct StaffExtra {
    pub phone: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub status_reason: Option<String>,
    pub verified_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_helpers() {
        assert_eq!(clean(Some("  a ".into())), Some("a".into()));
        assert_eq!(clean(Some("   ".into())), None);
        assert!(required_text("x", " ", 10).is_err());
        assert!(required_text("x", "abc", 2).is_err());
        assert_eq!(required_text("x", " ab ", 2).ok(), Some("ab".into()));
        assert!(valid_code("grade", Some("AP-1".into())).is_ok());
        assert!(valid_code("grade", Some("-x".into())).is_err());
        assert!(required_reason(None).is_err());
        assert!(check_version(2, 3).is_err() && check_version(3, 3).is_ok());
    }
}
