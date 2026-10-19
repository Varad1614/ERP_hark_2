//! **8D** building blocks shared by every 8D area (suppliers, requisitions, sourcing, orders,
//! receipts, inventory, assets, payables): request bodies, validation helpers, `If-Match`
//! handling, status enums, event payloads, history / audit / outbox helpers and the reference
//! checks through the Domain B / E ports. Lives in `suppliers/` because 8D owns no file at the
//! crate root (`docs/batch8-contracts.md` §2.1).

use crate::shared::decimal::parse_decimal;
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::publish;
use crate::shared::{Actor, ResourcesState};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::str::FromStr;
use sutra_contracts::institution::status::OrgUnitStatus;
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::ports::StaffSummary;
use sutra_contracts::people::status::StaffStatus;
use sutra_contracts::resources::events::StatusChangedV1;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use utoipa::{IntoParams, ToSchema};
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

/// Declares a slice status enum (`ALL`, `as_str`, `parse`, `Display`); transitions live next to
/// the aggregate. Views carry the stored `snake_case` string.
macro_rules! slice_status {
    ($(#[$meta:meta])* $name:ident { $($variant:ident => $wire:literal),+ $(,)? }) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum $name { $($variant),+ }

        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            /// Stored / wire name.
            pub fn as_str(&self) -> &'static str {
                match self { $($name::$variant => $wire),+ }
            }

            /// Strict parse of the stored name.
            pub fn parse(s: &str) -> Option<Self> {
                Self::ALL.iter().copied().find(|v| v.as_str() == s)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}
pub(crate) use slice_status;

/// Optional reason of a state command.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesReasonBody {
    pub reason: Option<String>,
}

/// Approve / reject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourcesDecision {
    Approve,
    Reject,
}

/// Decision of a governed step (`resources.approve`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesDecisionBody {
    pub decision: ResourcesDecision,
    pub reason: Option<String>,
}

/// One failed element of a bulk command.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesBulkFailure {
    pub id: Option<Uuid>,
    pub message: String,
}

/// Result of a bulk command (`{processed, skipped, failed[]}`).
#[derive(Debug, Clone, Default, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesBulkResult {
    pub processed: i64,
    pub skipped: i64,
    pub failed: Vec<ResourcesBulkFailure>,
    /// Records created by the command (needs, requisitions …).
    pub created_ids: Vec<Uuid>,
}

/// `?status=` filter of the 8D lists.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ResourcesStatusQuery {
    pub status: Option<String>,
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
pub fn reason_of(body: Option<axum::Json<ResourcesReasonBody>>) -> Option<String> {
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

/// `412` unless the stored version is the expected one.
pub fn check_version(expected: i64, actual: i64) -> Result<(), KernelError> {
    if expected == actual {
        Ok(())
    } else {
        Err(KernelError::VersionConflict { expected, actual })
    }
}

/// 409 for an illegal transition of an 8D aggregate.
pub fn illegal(what: &str, label: &str, from: &str, to: &str) -> KernelError {
    KernelError::Conflict(format!("{what} {label} is {from}; it cannot become {to}"))
}

/// Money (`"1250.00"`): ≥ 0, at most two decimals.
pub fn parse_money(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = parse_decimal(field, raw)?;
    if v < Decimal::ZERO {
        return Err(invalid(field, "negative", "must not be negative"));
    }
    Ok(v)
}

/// Quantity (`"12.500"`): > 0, at most three decimals.
pub fn parse_qty(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = parse_signed_qty(field, raw)?;
    if v <= Decimal::ZERO {
        return Err(invalid(field, "not_positive", "must be greater than zero"));
    }
    Ok(v)
}

/// Signed quantity (adjustments): non-zero, at most three decimals.
pub fn parse_signed_qty(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = Decimal::from_str(raw.trim()).map_err(|_| {
        invalid(
            field,
            "invalid_number",
            "must be a decimal string such as \"12.500\"",
        )
    })?;
    if v.normalize().scale() > 3 {
        return Err(invalid(field, "too_precise", "at most three decimals"));
    }
    if v.is_zero() {
        return Err(invalid(field, "zero", "must not be zero"));
    }
    Ok(v)
}

/// Line amount = quantity × unit price, rounded once (half away from zero).
pub fn line_amount(quantity: Decimal, unit_price: Decimal) -> Decimal {
    crate::shared::decimal::round2(quantity * unit_price)
}

/// Lifecycle payload of an 8D aggregate.
#[allow(clippy::too_many_arguments)]
pub fn status_payload(
    aggregate_id: Uuid,
    institution: Uuid,
    from: Option<&str>,
    to: &str,
    version: i64,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    number: Option<&str>,
    subject_ref: Option<&str>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id,
        institution_id: institution,
        from_status: from.map(str::to_owned),
        to_status: to.to_owned(),
        version,
        reason: reason.map(str::to_owned),
        process_instance_id: process_instance_id.map(str::to_owned),
        number: number.map(str::to_owned),
        subject_ref: subject_ref.map(str::to_owned),
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

/// Appends a status history row of an 8D aggregate.
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

/// Validates a Domain B org unit (422 on the field when unknown / foreign / not usable).
pub async fn check_org_unit(
    st: &ResourcesState,
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

/// Validates a Domain E staff member of the institution that is not merged / archived (422 on
/// the field otherwise).
pub async fn check_staff(
    st: &ResourcesState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    field: &str,
    id: Option<Uuid>,
) -> Result<Option<StaffSummary>, KernelError> {
    let Some(id) = id else { return Ok(None) };
    match st.workforce.staff(scope, StaffMemberId(id)).await? {
        Some(s) if s.institution_id == institution && s.status.is_live() => {
            if s.status == StaffStatus::Suspended {
                return Err(invalid(
                    field,
                    "not_usable",
                    "the staff member is suspended",
                ));
            }
            Ok(Some(s))
        }
        Some(_) => Err(invalid(
            field,
            "not_usable",
            "the staff member is not a current staff member of this institution",
        )),
        None => Err(invalid(field, "not_found", "unknown staff member")),
    }
}

/// The parts of `resources.item` the 8D documents need.
#[derive(Clone, Debug)]
pub struct ItemRef {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    pub kind: String,
    pub unit: String,
    pub status: String,
    pub reorder_level: Option<Decimal>,
    pub reorder_quantity: Option<Decimal>,
    pub standard_cost: Option<Decimal>,
}

impl ItemRef {
    /// Purchase line kind of the item (`stock` | `asset` | `service`).
    pub fn line_kind(&self) -> &'static str {
        match self.kind.as_str() {
            "consumable" => "stock",
            "asset" => "asset",
            _ => "service",
        }
    }
}

fn item_from_row(r: &PgRow) -> Result<ItemRef, sqlx::Error> {
    Ok(ItemRef {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        kind: r.try_get("kind")?,
        unit: r.try_get("unit")?,
        status: r.try_get("status")?,
        reorder_level: r.try_get("reorder_level")?,
        reorder_quantity: r.try_get("reorder_quantity")?,
        standard_cost: r.try_get("standard_cost")?,
    })
}

/// An item of the tenant (`None` when missing).
pub async fn load_item(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<Option<ItemRef>, KernelError> {
    let row = sqlx::query(
        "SELECT id, institution_id, code, name, kind, unit, status, reorder_level, \
         reorder_quantity, standard_cost FROM resources.item WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.as_ref()
        .map(item_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// An **active** item of the institution, else 422 on `field`.
pub async fn active_item(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    field: &str,
    id: Uuid,
) -> Result<ItemRef, KernelError> {
    match load_item(conn, tenant, id).await? {
        Some(i) if i.institution_id == institution.as_uuid() && i.status == "active" => Ok(i),
        Some(i) if i.institution_id == institution.as_uuid() => Err(invalid(
            field,
            "not_active",
            &format!("item {} is {}", i.code, i.status),
        )),
        _ => Err(invalid(
            field,
            "unknown_item",
            "no such item in the institution",
        )),
    }
}

/// An **active** store location of the institution, else 422 on `field`. Returns its code.
pub async fn active_location(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    field: &str,
    id: Uuid,
) -> Result<String, KernelError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "SELECT code, status FROM resources.store_location WHERE id = $1 AND tenant_id = $2 \
         AND institution_id = $3",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match row {
        Some((code, status)) if status == "active" => Ok(code),
        Some((code, status)) => Err(invalid(
            field,
            "not_active",
            &format!("store location {code} is {status}"),
        )),
        None => Err(invalid(
            field,
            "unknown_location",
            "no such store location in the institution",
        )),
    }
}

/// Quantity held by active reservations of an item at a location (optionally excluding one).
pub async fn reserved(
    conn: &mut PgConnection,
    tenant: TenantId,
    item: Uuid,
    location: Option<Uuid>,
    excluding: Option<Uuid>,
) -> Result<Decimal, KernelError> {
    let v: Option<Decimal> = sqlx::query_scalar(
        "SELECT sum(quantity) FROM resources.stock_reservation WHERE tenant_id = $1 \
         AND item_id = $2 AND ($3::uuid IS NULL OR location_id = $3) AND status = 'active' \
         AND ($4::uuid IS NULL OR id <> $4)",
    )
    .bind(tenant.as_uuid())
    .bind(item)
    .bind(location)
    .bind(excluding)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(v.unwrap_or(Decimal::ZERO))
}

/// Institution of an 8D aggregate row (`table` is a fixed 8D table name), 404 when missing.
pub async fn institution_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    table: &'static str,
    entity: &'static str,
    id: Uuid,
) -> Result<InstitutionId, KernelError> {
    let v: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT institution_id FROM resources.{table} WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    v.map(InstitutionId).ok_or(KernelError::NotFound { entity })
}

/// Bank account masked for reads (`XXXXXX1234`).
pub fn mask_account(v: Option<String>) -> Option<String> {
    v.map(|s| {
        let n = s.chars().count();
        if n <= 4 {
            "X".repeat(n)
        } else {
            let tail: String = s.chars().skip(n - 4).collect();
            format!("{}{tail}", "X".repeat(n - 4))
        }
    })
}

/// GSTIN format (`27ABCDE1234F1Z5`; state code, PAN, entity, `Z`, check character).
pub fn valid_gstin(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 15
        && v.is_ascii()
        && b[0..2].iter().all(u8::is_ascii_digit)
        && valid_pan(&v[2..12])
        && (b[12].is_ascii_uppercase() || (b'1'..=b'9').contains(&b[12]))
        && b[13] == b'Z'
        && (b[14].is_ascii_uppercase() || b[14].is_ascii_digit())
}

/// PAN format (`ABCDE1234F`).
pub fn valid_pan(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 10
        && b[0..5].iter().all(u8::is_ascii_uppercase)
        && b[5..9].iter().all(u8::is_ascii_digit)
        && b[9].is_ascii_uppercase()
}

/// IFSC format (`HDFC0001234`).
pub fn valid_ifsc(v: &str) -> bool {
    let b = v.as_bytes();
    b.len() == 11
        && b[0..4].iter().all(u8::is_ascii_uppercase)
        && b[4] == b'0'
        && b[5..]
            .iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Item / store code rule (same as the foundation table check).
pub fn valid_code(v: &str) -> bool {
    let b = v.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| {
            c.is_ascii_uppercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    #[test]
    fn validation_helpers() {
        assert_eq!(clean(Some("  a ".into())), Some("a".into()));
        assert!(required_text("x", " ", 10).is_err());
        assert!(required_reason(None).is_err());
        assert!(check_version(2, 3).is_err() && check_version(3, 3).is_ok());
        assert!(parse_money("m", "12.50").is_ok());
        assert!(parse_money("m", "-1").is_err());
        assert!(parse_money("m", "1.005").is_err());
        assert_eq!(parse_qty("q", "2.125").ok(), Some(d("2.125")));
        assert!(parse_qty("q", "0").is_err());
        assert!(parse_qty("q", "1.0001").is_err());
        assert!(parse_signed_qty("q", "-2").is_ok());
        assert_eq!(line_amount(d("3"), d("33.335")), d("100.01"));
    }

    #[test]
    fn identity_formats() {
        assert!(valid_pan("ABCDE1234F"));
        assert!(!valid_pan("ABCDE1234"));
        assert!(valid_gstin("27ABCDE1234F1Z5"));
        assert!(!valid_gstin("27ABCDE1234F1X5"));
        assert!(!valid_gstin("2AABCDE1234F1Z5"));
        assert!(valid_ifsc("HDFC0001234"));
        assert!(!valid_ifsc("HDFC1001234"));
        assert_eq!(
            mask_account(Some("1234567890".into())).as_deref(),
            Some("XXXXXX7890")
        );
        assert!(valid_code("PAPER-A4") && !valid_code("paper"));
    }
}
