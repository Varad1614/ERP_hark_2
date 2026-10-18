//! The shared charge (obligation) tables (contract §3.3): row types, loaders, creation, and the
//! **only** writers of the two running amounts:
//!
//! * [`apply_settlement`] — `settled_amount` (5B: allocations `+`, allocation reversals `−`);
//!   moves `published → active` on the first settlement and `closed → active` when a reversal
//!   re-opens a closed charge (both through `shared::lifecycle`).
//! * [`apply_adjustment`] — `adjustment_amount` (5A: credit notes / write-offs / waivers / aid
//!   `−`, debit notes `+`, reversals of those the opposite way).
//!
//! `outstanding_amount = total + adjustment − settled` is a generated column. Callers lock the
//! charge ([`lock_charge`]) inside their owner-local transaction, then post the matching ledger
//! entry with `shared::posting::post` in the **same** transaction.

use crate::shared::lifecycle::{change_charge_status, StatusChange};
use crate::shared::Actor;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::ids::{
    ChargeId, ChargeLineId, FeeHeadId, LedgerAccountId, StudentAccountId,
};
use sutra_contracts::finance::ports::ChargeSummary;
use sutra_contracts::finance::status::{ChargeSourceType, ChargeStatus};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Columns of [`charge_from_row`].
pub const CHARGE_COLUMNS: &str = "id, tenant_id, institution_id, student_account_id, student_id, \
    admission_id, charge_number, academic_year_id, academic_term_id, description, currency, \
    total_amount, adjustment_amount, settled_amount, outstanding_amount, due_date, source_type, \
    source_id, status, status_reason, status_changed_at, process_instance_id, published_at, \
    version, created_at, updated_at";

/// A `finance.charge` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeRow {
    pub id: ChargeId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub student_account_id: StudentAccountId,
    pub student_id: Option<Uuid>,
    pub admission_id: Option<Uuid>,
    pub charge_number: String,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub description: String,
    pub currency: String,
    pub total_amount: Decimal,
    pub adjustment_amount: Decimal,
    pub settled_amount: Decimal,
    pub outstanding_amount: Decimal,
    pub due_date: Option<NaiveDate>,
    pub source_type: ChargeSourceType,
    pub source_id: Option<Uuid>,
    pub status: ChargeStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub process_instance_id: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl ChargeRow {
    /// The port view of the row.
    pub fn summary(&self) -> ChargeSummary {
        ChargeSummary {
            id: self.id,
            institution_id: self.institution_id,
            student_account_id: self.student_account_id,
            student_id: self.student_id,
            admission_id: self.admission_id,
            charge_number: self.charge_number.clone(),
            description: self.description.clone(),
            currency: self.currency.clone(),
            total_amount: self.total_amount,
            adjustment_amount: self.adjustment_amount,
            settled_amount: self.settled_amount,
            outstanding_amount: self.outstanding_amount,
            due_date: self.due_date,
            academic_term_id: self.academic_term_id,
            status: self.status,
            version: self.version,
        }
    }
}

fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown value `{raw}`").into()))
}

pub fn charge_from_row(r: &PgRow) -> Result<ChargeRow, sqlx::Error> {
    Ok(ChargeRow {
        id: ChargeId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        student_account_id: StudentAccountId(r.try_get("student_account_id")?),
        student_id: r.try_get("student_id")?,
        admission_id: r.try_get("admission_id")?,
        charge_number: r.try_get("charge_number")?,
        academic_year_id: r.try_get("academic_year_id")?,
        academic_term_id: r.try_get("academic_term_id")?,
        description: r.try_get("description")?,
        currency: r.try_get("currency")?,
        total_amount: r.try_get("total_amount")?,
        adjustment_amount: r.try_get("adjustment_amount")?,
        settled_amount: r.try_get("settled_amount")?,
        outstanding_amount: r.try_get("outstanding_amount")?,
        due_date: r.try_get("due_date")?,
        source_type: decode(r.try_get("source_type")?, ChargeSourceType::parse)?,
        source_id: r.try_get("source_id")?,
        status: decode(r.try_get("status")?, ChargeStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        status_changed_at: r.try_get("status_changed_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        published_at: r.try_get("published_at")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

/// One fee-head line of a charge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChargeLineRow {
    pub id: ChargeLineId,
    pub charge_id: ChargeId,
    pub line_no: i32,
    pub fee_head_id: FeeHeadId,
    pub description: String,
    pub amount: Decimal,
    pub income_account_id: LedgerAccountId,
}

async fn fetch(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ChargeId,
    lock: bool,
) -> Result<Option<ChargeRow>, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {CHARGE_COLUMNS} FROM finance.charge WHERE tenant_id = $1 AND id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    row.as_ref()
        .map(charge_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// Reads a charge (no lock; 404 when missing / foreign).
pub async fn load_charge(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ChargeId,
) -> Result<ChargeRow, KernelError> {
    fetch(conn, tenant, id, false)
        .await?
        .ok_or(KernelError::NotFound { entity: "charge" })
}

/// Locks a charge `FOR UPDATE` in the caller's transaction (404 when missing / foreign).
pub async fn lock_charge(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ChargeId,
) -> Result<ChargeRow, KernelError> {
    fetch(conn, tenant, id, true)
        .await?
        .ok_or(KernelError::NotFound { entity: "charge" })
}

/// The lines of a charge in order.
pub async fn charge_lines(
    conn: &mut PgConnection,
    tenant: TenantId,
    charge: ChargeId,
) -> Result<Vec<ChargeLineRow>, KernelError> {
    let rows = sqlx::query(
        "SELECT id, charge_id, line_no, fee_head_id, description, amount, income_account_id \
         FROM finance.charge_line WHERE tenant_id = $1 AND charge_id = $2 ORDER BY line_no",
    )
    .bind(tenant.as_uuid())
    .bind(charge.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            Ok(ChargeLineRow {
                id: ChargeLineId(r.try_get("id")?),
                charge_id: ChargeId(r.try_get("charge_id")?),
                line_no: r.try_get("line_no")?,
                fee_head_id: FeeHeadId(r.try_get("fee_head_id")?),
                description: r.try_get("description")?,
                amount: r.try_get("amount")?,
                income_account_id: LedgerAccountId(r.try_get("income_account_id")?),
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

/// A line of a new charge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewChargeLine {
    pub fee_head_id: FeeHeadId,
    pub description: String,
    pub amount: Decimal,
}

/// A new charge (inserted as `draft`; 5A moves it through validation → publication).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewCharge {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub student_account_id: StudentAccountId,
    pub student_id: Option<Uuid>,
    pub admission_id: Option<Uuid>,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Option<Uuid>,
    pub description: String,
    pub due_date: Option<NaiveDate>,
    pub source_type: ChargeSourceType,
    pub source_id: Option<Uuid>,
    pub lines: Vec<NewChargeLine>,
}

/// Inserts a `draft` charge with its lines (number `CHG-…`, total = Σ lines, each line's income
/// account snapshotted from its fee head, which must be `active` in the same institution).
/// With a GST profile the lines are re-priced first (`crate::gst::repo::plan_for_insert`).
/// The caller emits its own event / audit.
pub async fn insert_charge(
    conn: &mut PgConnection,
    c: &NewCharge,
) -> Result<ChargeRow, KernelError> {
    if c.lines.is_empty() {
        return Err(KernelError::Conflict(
            "a charge needs at least one line".into(),
        ));
    }
    // ERP_EXTENSIONS 5D: under an enabled GST profile, lines on taxed fee heads are re-priced
    // (taxable value + one line per tax row on the system GST heads) and the snapshot recorded.
    let gst = crate::gst::repo::plan_for_insert(conn, c).await?;
    let lines = gst.as_ref().map_or(&c.lines, |g| &g.lines);
    let mut total = Decimal::ZERO;
    for (i, l) in lines.iter().enumerate() {
        crate::shared::money::require_positive(&format!("lines[{i}].amount"), l.amount)?;
        total += l.amount;
    }
    let number = crate::shared::numbering::next(
        conn,
        c.tenant,
        c.institution,
        crate::shared::numbering::kind::CHARGE,
    )
    .await?;
    let row = sqlx::query(&format!(
        "INSERT INTO finance.charge (tenant_id, institution_id, student_account_id, student_id, \
         admission_id, charge_number, academic_year_id, academic_term_id, description, \
         total_amount, due_date, source_type, source_id, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, 'draft') \
         RETURNING {CHARGE_COLUMNS}"
    ))
    .bind(c.tenant.as_uuid())
    .bind(c.institution.as_uuid())
    .bind(c.student_account_id.as_uuid())
    .bind(c.student_id)
    .bind(c.admission_id)
    .bind(&number)
    .bind(c.academic_year_id)
    .bind(c.academic_term_id)
    .bind(&c.description)
    .bind(total)
    .bind(c.due_date)
    .bind(c.source_type.as_str())
    .bind(c.source_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let charge = charge_from_row(&row).map_err(map_db_error)?;
    for (i, l) in lines.iter().enumerate() {
        let n = sqlx::query(
            "INSERT INTO finance.charge_line (tenant_id, charge_id, line_no, fee_head_id, \
             description, amount, income_account_id) \
             SELECT $1, $2, $3, h.id, $5, $6, h.income_account_id FROM finance.fee_head h \
             WHERE h.tenant_id = $1 AND h.institution_id = $7 AND h.id = $4 AND h.status = 'active'",
        )
        .bind(c.tenant.as_uuid())
        .bind(charge.id.as_uuid())
        .bind(i32::try_from(i + 1).unwrap_or(i32::MAX))
        .bind(l.fee_head_id.as_uuid())
        .bind(&l.description)
        .bind(l.amount)
        .bind(c.institution.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?
        .rows_affected();
        if n != 1 {
            return Err(KernelError::Conflict(format!(
                "fee head {} is not an active fee head of this institution",
                l.fee_head_id
            )));
        }
    }
    if let Some(g) = &gst {
        crate::gst::repo::record_pending(conn, &charge, g).await?;
    }
    Ok(charge)
}

async fn update_amount(
    conn: &mut PgConnection,
    c: &ChargeRow,
    column: &'static str,
    value: Decimal,
) -> Result<ChargeRow, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE finance.charge SET {column} = $3, version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND version = $4 RETURNING {CHARGE_COLUMNS}"
    ))
    .bind(c.tenant_id.as_uuid())
    .bind(c.id.as_uuid())
    .bind(value)
    .bind(c.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("charge changed concurrently".into()))?;
    charge_from_row(&row).map_err(map_db_error)
}

/// Pure check of a settlement delta (`+` allocate, `−` reverse). Returns the new settled amount.
pub fn settlement_after(c: &ChargeRow, delta: Decimal) -> Result<Decimal, KernelError> {
    if delta.is_zero() {
        return Err(KernelError::Conflict(
            "settlement amount must not be zero".into(),
        ));
    }
    if delta > Decimal::ZERO && !c.status.accepts_settlement() {
        return Err(KernelError::Conflict(format!(
            "charge {} is {}; payments cannot be allocated to it",
            c.charge_number, c.status
        )));
    }
    let settled = c.settled_amount + delta;
    if settled < Decimal::ZERO {
        return Err(KernelError::Conflict(format!(
            "charge {}: cannot reverse more than was settled",
            c.charge_number
        )));
    }
    if settled > c.total_amount + c.adjustment_amount {
        return Err(KernelError::Conflict(format!(
            "charge {}: allocation exceeds the outstanding amount; keep the surplus as an unapplied receipt",
            c.charge_number
        )));
    }
    Ok(settled)
}

/// Pure check of an adjustment delta (`−` credit, `+` debit). Returns the new adjustment amount.
pub fn adjustment_after(c: &ChargeRow, delta: Decimal) -> Result<Decimal, KernelError> {
    if delta.is_zero() {
        return Err(KernelError::Conflict(
            "adjustment amount must not be zero".into(),
        ));
    }
    if !c.status.accepts_adjustment() {
        return Err(KernelError::Conflict(format!(
            "charge {} is {}; it cannot be adjusted",
            c.charge_number, c.status
        )));
    }
    let adjustment = c.adjustment_amount + delta;
    if c.total_amount + adjustment < c.settled_amount {
        return Err(KernelError::Conflict(format!(
            "charge {}: the credit exceeds the unsettled amount; refund the settled part instead",
            c.charge_number
        )));
    }
    Ok(adjustment)
}

/// Applies a settlement delta to a **locked** charge (5B only). Moves `published → active` on the
/// first settlement and `closed → active` when a reversal re-opens a closed charge.
pub async fn apply_settlement(
    conn: &mut PgConnection,
    c: &ChargeRow,
    delta: Decimal,
    cause_event_type: &'static str,
    actor: &Actor,
) -> Result<ChargeRow, KernelError> {
    let reopen = delta < Decimal::ZERO && c.status == ChargeStatus::Closed;
    if reopen {
        // A reversal on a closed charge: re-open it first (L3-F-011 / F-043).
        let mut probe = c.clone();
        probe.status = ChargeStatus::Active;
        settlement_after(&probe, delta)?;
    } else {
        settlement_after(c, delta)?;
    }
    let mut current = c.clone();
    if reopen || (delta > Decimal::ZERO && c.status == ChargeStatus::Published) {
        let reason = if reopen {
            "settlement reversed"
        } else {
            "first settlement"
        };
        current = change_charge_status(
            conn,
            &current,
            ChargeStatus::Active,
            &StatusChange {
                cause_event_type,
                reason: Some(reason),
                process_instance_id: None,
            },
            actor,
        )
        .await?;
    }
    let settled = current.settled_amount + delta;
    update_amount(conn, &current, "settled_amount", settled).await
}

/// Applies an adjustment delta to a **locked** charge (5A only).
pub async fn apply_adjustment(
    conn: &mut PgConnection,
    c: &ChargeRow,
    delta: Decimal,
) -> Result<ChargeRow, KernelError> {
    let adjustment = adjustment_after(c, delta)?;
    update_amount(conn, c, "adjustment_amount", adjustment).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap_or_default()
    }

    fn charge(status: ChargeStatus, total: &str, adj: &str, settled: &str) -> ChargeRow {
        let now = Utc::now();
        ChargeRow {
            id: ChargeId(Uuid::nil()),
            tenant_id: TenantId(Uuid::nil()),
            institution_id: InstitutionId(Uuid::nil()),
            student_account_id: StudentAccountId(Uuid::nil()),
            student_id: None,
            admission_id: None,
            charge_number: "CHG-000001".into(),
            academic_year_id: None,
            academic_term_id: None,
            description: "Tuition".into(),
            currency: "INR".into(),
            total_amount: d(total),
            adjustment_amount: d(adj),
            settled_amount: d(settled),
            outstanding_amount: d(total) + d(adj) - d(settled),
            due_date: None,
            source_type: ChargeSourceType::Manual,
            source_id: None,
            status,
            status_reason: None,
            status_changed_at: now,
            process_instance_id: None,
            published_at: None,
            version: 1,
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn settlement_rules() {
        let c = charge(ChargeStatus::Published, "1000", "-100", "0");
        assert_eq!(settlement_after(&c, d("900")).ok(), Some(d("900")));
        assert!(
            settlement_after(&c, d("900.01")).is_err(),
            "over-allocation"
        );
        assert!(settlement_after(&c, d("-1")).is_err(), "nothing to reverse");
        assert!(settlement_after(&c, Decimal::ZERO).is_err());
        let disputed = charge(ChargeStatus::Disputed, "1000", "0", "0");
        assert!(settlement_after(&disputed, d("1")).is_err());
        let draft = charge(ChargeStatus::Draft, "1000", "0", "0");
        assert!(settlement_after(&draft, d("1")).is_err());
    }

    #[test]
    fn adjustment_rules() {
        let c = charge(ChargeStatus::Active, "1000", "0", "600");
        assert_eq!(adjustment_after(&c, d("-400")).ok(), Some(d("-400")));
        assert!(
            adjustment_after(&c, d("-400.01")).is_err(),
            "credit beyond unsettled"
        );
        assert_eq!(adjustment_after(&c, d("250")).ok(), Some(d("250")));
        let closed = charge(ChargeStatus::Closed, "1000", "0", "1000");
        assert!(adjustment_after(&closed, d("-1")).is_err());
    }
}
