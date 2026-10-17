//! The **only** writer of the append-only `resources.stock_ledger_entry` (contract
//! `docs/batch8-contracts.md` §3.3) and the on-hand projection over it.
//!
//! [`post_movement`] runs in the caller's owner-local transaction: it checks the sign rule of the
//! kind (`StockMovementKind::sign`), the scale (three decimals), that item and location exist in
//! the institution and are active, and — for outgoing movements — that enough stock is on hand
//! at the location (`409` otherwise). It is **idempotent** on `(source_type, source_id,
//! posting_key)`: a second call returns the entry already posted (`already_posted = true`).
//! [`reverse_movement`] posts the opposite quantity of an entry once (`posting_key =
//! reverse:<entry>`).

use crate::shared::Actor;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::{PgConnection, Row};
use sutra_contracts::resources::ids::{ItemId, StockLedgerEntryId, StoreLocationId};
use sutra_contracts::resources::status::StockMovementKind;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// One movement to post.
#[derive(Clone, Debug)]
pub struct NewMovement<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub item_id: ItemId,
    pub location_id: StoreLocationId,
    pub kind: StockMovementKind,
    /// Signed quantity (sign fixed by the kind; three decimals at most).
    pub quantity: Decimal,
    pub unit_cost: Option<Decimal>,
    pub effective_on: NaiveDate,
    /// The 8D aggregate (`goods_receipt`, `stock_issue`, `stock_return`, `stock_adjustment` …).
    pub source_type: &'a str,
    pub source_id: Uuid,
    /// Distinguishes several movements of one source (`line:<n>`, `reverse:<entry>`).
    pub posting_key: &'a str,
    pub reverses_entry_id: Option<StockLedgerEntryId>,
    pub reason: Option<&'a str>,
    /// Expiry date of an incoming lot (items that track expiry; ERP_EXTENSIONS 2C).
    pub expiry_on: Option<NaiveDate>,
}

/// What [`post_movement`] recorded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MovementReceipt {
    pub entry_id: StockLedgerEntryId,
    pub quantity: Decimal,
    /// On hand at the location after the movement.
    pub on_hand_after: Decimal,
    /// `true` when the movement had been posted before (nothing new written).
    pub already_posted: bool,
}

fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

/// Pure checks of [`post_movement`] (unit-tested).
pub fn validate_movement(m: &NewMovement<'_>) -> Result<(), KernelError> {
    if m.quantity.is_zero() {
        return Err(invalid("quantity", "zero", "a movement moves something"));
    }
    if m.quantity.normalize().scale() > 3 {
        return Err(invalid("quantity", "too_precise", "at most three decimals"));
    }
    let sign = m.kind.sign();
    if (sign > 0 && m.quantity < Decimal::ZERO) || (sign < 0 && m.quantity > Decimal::ZERO) {
        return Err(invalid(
            "quantity",
            "wrong_sign",
            &format!("a {} movement has the wrong sign", m.kind),
        ));
    }
    if m.unit_cost
        .is_some_and(|c| c < Decimal::ZERO || c.normalize().scale() > 2)
    {
        return Err(invalid(
            "unitCost",
            "invalid",
            "≥ 0 with at most two decimals",
        ));
    }
    if m.posting_key.trim().is_empty() || m.source_type.trim().is_empty() {
        return Err(invalid(
            "postingKey",
            "required",
            "source type and posting key are required",
        ));
    }
    Ok(())
}

/// On hand of an item: at one location, or across every location of the institution.
pub async fn on_hand(
    conn: &mut PgConnection,
    tenant: TenantId,
    item_id: ItemId,
    location_id: Option<StoreLocationId>,
) -> Result<Decimal, KernelError> {
    let v: Option<Decimal> = sqlx::query_scalar(
        "SELECT sum(quantity) FROM resources.stock_ledger_entry WHERE tenant_id = $1 \
         AND item_id = $2 AND ($3::uuid IS NULL OR location_id = $3)",
    )
    .bind(tenant.as_uuid())
    .bind(item_id.as_uuid())
    .bind(location_id.map(|l| l.as_uuid()))
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(v.unwrap_or(Decimal::ZERO))
}

/// Posts one movement (see the module docs).
pub async fn post_movement(
    conn: &mut PgConnection,
    m: NewMovement<'_>,
    actor: &Actor,
) -> Result<MovementReceipt, KernelError> {
    validate_movement(&m)?;
    if let Some(r) = sqlx::query(
        "SELECT id, quantity FROM resources.stock_ledger_entry WHERE tenant_id = $1 \
         AND source_type = $2 AND source_id = $3 AND posting_key = $4",
    )
    .bind(m.tenant.as_uuid())
    .bind(m.source_type)
    .bind(m.source_id)
    .bind(m.posting_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    {
        let after = on_hand(conn, m.tenant, m.item_id, Some(m.location_id)).await?;
        return Ok(MovementReceipt {
            entry_id: StockLedgerEntryId(r.try_get("id").map_err(map_db_error)?),
            quantity: r.try_get("quantity").map_err(map_db_error)?,
            on_hand_after: after,
            already_posted: true,
        });
    }
    let item_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM resources.item WHERE id = $1 AND tenant_id = $2 AND institution_id = $3 \
         FOR SHARE",
    )
    .bind(m.item_id.as_uuid())
    .bind(m.tenant.as_uuid())
    .bind(m.institution.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match item_status.as_deref() {
        None => {
            return Err(invalid(
                "itemId",
                "unknown_item",
                "no such item in the institution",
            ))
        }
        Some("active") => {}
        Some(other) => {
            return Err(KernelError::Conflict(format!(
                "item is {other}; only active items move stock"
            )))
        }
    }
    // Serialises movements of one location (on-hand checks stay exact under concurrency).
    let loc_status: Option<String> = sqlx::query_scalar(
        "SELECT status FROM resources.store_location WHERE id = $1 AND tenant_id = $2 \
         AND institution_id = $3 FOR UPDATE",
    )
    .bind(m.location_id.as_uuid())
    .bind(m.tenant.as_uuid())
    .bind(m.institution.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match loc_status.as_deref() {
        None => {
            return Err(invalid(
                "locationId",
                "unknown_location",
                "no such store location in the institution",
            ))
        }
        Some("active") => {}
        Some(_) if m.quantity < Decimal::ZERO => {}
        Some(other) => {
            return Err(KernelError::Conflict(format!(
                "store location is {other}; it receives no stock"
            )))
        }
    }
    let before = on_hand(conn, m.tenant, m.item_id, Some(m.location_id)).await?;
    let after = before + m.quantity;
    if after < Decimal::ZERO {
        return Err(KernelError::Conflict(format!(
            "insufficient stock: {before} on hand, {} requested",
            -m.quantity
        )));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.stock_ledger_entry (tenant_id, institution_id, item_id, location_id, \
         kind, quantity, unit_cost, effective_on, source_type, source_id, posting_key, \
         reverses_entry_id, reason, actor, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) RETURNING id",
    )
    .bind(m.tenant.as_uuid())
    .bind(m.institution.as_uuid())
    .bind(m.item_id.as_uuid())
    .bind(m.location_id.as_uuid())
    .bind(m.kind.as_str())
    .bind(m.quantity)
    .bind(m.unit_cost)
    .bind(m.effective_on)
    .bind(m.source_type)
    .bind(m.source_id)
    .bind(m.posting_key)
    .bind(m.reverses_entry_id.map(|e| e.as_uuid()))
    .bind(m.reason)
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    // ERP_EXTENSIONS 2C: dated lots of items that track expiry follow the ledger.
    crate::inventory::lotstore::follow(conn, &m, id, before).await?;
    Ok(MovementReceipt {
        entry_id: StockLedgerEntryId(id),
        quantity: m.quantity,
        on_hand_after: after,
        already_posted: false,
    })
}

/// Reverses one ledger entry (opposite quantity, kind `reversal`, `posting_key =
/// reverse:<entry>`, same source); idempotent.
pub async fn reverse_movement(
    conn: &mut PgConnection,
    tenant: TenantId,
    entry_id: StockLedgerEntryId,
    effective_on: NaiveDate,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<MovementReceipt, KernelError> {
    let r = sqlx::query(
        "SELECT institution_id, item_id, location_id, quantity, unit_cost, source_type, source_id \
         FROM resources.stock_ledger_entry WHERE id = $1 AND tenant_id = $2",
    )
    .bind(entry_id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "stock ledger entry",
    })?;
    let quantity: Decimal = r.try_get("quantity").map_err(map_db_error)?;
    let source_type: String = r.try_get("source_type").map_err(map_db_error)?;
    let key = format!("reverse:{entry_id}");
    post_movement(
        conn,
        NewMovement {
            tenant,
            institution: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
            item_id: ItemId(r.try_get("item_id").map_err(map_db_error)?),
            location_id: StoreLocationId(r.try_get("location_id").map_err(map_db_error)?),
            kind: StockMovementKind::Reversal,
            quantity: -quantity,
            unit_cost: r.try_get("unit_cost").map_err(map_db_error)?,
            effective_on,
            source_type: &source_type,
            source_id: r.try_get("source_id").map_err(map_db_error)?,
            posting_key: &key,
            reverses_entry_id: Some(entry_id),
            reason,
            expiry_on: None,
        },
        actor,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn m(kind: StockMovementKind, q: &str) -> NewMovement<'static> {
        NewMovement {
            tenant: TenantId(Uuid::nil()),
            institution: InstitutionId(Uuid::nil()),
            item_id: ItemId(Uuid::nil()),
            location_id: StoreLocationId(Uuid::nil()),
            kind,
            quantity: d(q),
            unit_cost: None,
            effective_on: NaiveDate::default(),
            source_type: "goods_receipt",
            source_id: Uuid::nil(),
            posting_key: "line:1",
            reverses_entry_id: None,
            reason: None,
            expiry_on: None,
        }
    }

    #[test]
    fn sign_and_scale_rules() {
        use StockMovementKind::*;
        assert!(validate_movement(&m(Receipt, "10")).is_ok());
        assert!(validate_movement(&m(Receipt, "-1")).is_err());
        assert!(validate_movement(&m(Issue, "-2.5")).is_ok());
        assert!(validate_movement(&m(Issue, "2")).is_err());
        assert!(validate_movement(&m(Adjustment, "-0.125")).is_ok());
        assert!(validate_movement(&m(Adjustment, "0.0001")).is_err());
        assert!(validate_movement(&m(Reversal, "0")).is_err());
    }
}
