//! The **only** writer of the append-only leave ledger `people.leave_ledger_entry` (contract doc
//! §3.3; L2-E05 §6.6 "no direct balance mutation"): 7B's accrual, request, consumption,
//! cancellation, adjustment, carry-forward, expiry and encashment flows post here, inside their
//! owner-local transaction.
//!
//! * [`post_entry`] checks the sign of `days` against the kind ([`LeaveEntryKind::sign`]) and
//!   the two-decimal rule, and is **idempotent** on `(source_type, source_id, posting_key)`: a
//!   retried command returns the existing entry with `already_posted`.
//! * [`reverse_entry`] posts the mirror (kind `reversal`, opposite days) of an entry, at most
//!   once per entry.
//! * [`balance`] projects one leave type's balance inside the caller's transaction (7B's
//!   "insufficient balance" check), the same projection `LeaveReader::balances` returns.
//!
//! Consumption convention (contract §7): one `consumption` entry per calendar-month segment of a
//! leave (`posting_key = consume:<YYYY-MM>`, `effective_on` = first leave day of that month), so
//! payroll period sums (`LeaveReader::payroll_inputs`) are exact.

use crate::shared::Actor;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::people::ids::{LeaveLedgerEntryId, StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::LeaveBalance;
use sutra_contracts::people::status::{EntrySign, LeaveEntryKind};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// A new ledger entry.
#[derive(Clone, Debug)]
pub struct NewLeaveEntry<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub staff_id: StaffMemberId,
    pub engagement_id: Option<StaffEngagementId>,
    /// 7B leave type (plain id) + its code snapshot (`^[A-Z0-9][A-Z0-9_-]{0,15}$`, e.g. `CL`, `LOP`).
    pub leave_type_id: Uuid,
    pub leave_type_code: &'a str,
    /// `false` for unpaid leave types (payroll loss of pay).
    pub is_paid: bool,
    pub kind: LeaveEntryKind,
    /// Signed days (two decimals at most); the sign must match [`LeaveEntryKind::sign`].
    pub days: Decimal,
    pub effective_on: NaiveDate,
    pub leave_year: i32,
    /// The 7B aggregate posting it (`leave_request`, `leave_accrual_run`, …; lowercase snake case).
    pub source_type: &'a str,
    pub source_id: Uuid,
    /// Distinguishes several postings of one source (`reserve`, `consume:2026-11`, `release`, …).
    pub posting_key: &'a str,
    pub reason: Option<&'a str>,
}

/// Result of a posting.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaveEntryReceipt {
    pub entry_id: LeaveLedgerEntryId,
    /// `true` when the idempotency key had been posted before (nothing new written).
    pub already_posted: bool,
}

fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

fn valid_code(code: &str) -> bool {
    let b = code.as_bytes();
    (1..=16).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

fn valid_source_type(s: &str) -> bool {
    let b = s.as_bytes();
    (2..=48).contains(&b.len())
        && b[0].is_ascii_lowercase()
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

/// Pure validation of an entry (sign, precision, codes); [`post_entry`] runs it first.
pub fn validate(e: &NewLeaveEntry<'_>) -> Result<(), KernelError> {
    if e.days.is_zero() {
        return Err(invalid(
            "days",
            "zero",
            "a ledger entry moves a non-zero number of days",
        ));
    }
    if e.days.normalize().scale() > 2 {
        return Err(invalid("days", "too_precise", "at most two decimals"));
    }
    let ok = match e.kind.sign() {
        EntrySign::Positive => e.days > Decimal::ZERO,
        EntrySign::Negative => e.days < Decimal::ZERO,
        EntrySign::Either => true,
    };
    if !ok {
        return Err(invalid(
            "days",
            "wrong_sign",
            &format!("a {} entry has the wrong sign", e.kind),
        ));
    }
    if e.kind == LeaveEntryKind::Reversal {
        return Err(invalid(
            "kind",
            "use_reverse_entry",
            "reversals are posted with reverse_entry",
        ));
    }
    if !valid_code(e.leave_type_code) {
        return Err(invalid(
            "leaveTypeCode",
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_-]{0,15}$",
        ));
    }
    if !valid_source_type(e.source_type) || e.posting_key.trim().is_empty() {
        return Err(invalid(
            "sourceType",
            "invalid_source",
            "source type (snake case) and posting key are required",
        ));
    }
    if !(2000..=2200).contains(&e.leave_year) {
        return Err(invalid("leaveYear", "out_of_range", "2000 … 2200"));
    }
    Ok(())
}

async fn existing(
    conn: &mut PgConnection,
    tenant: TenantId,
    source_type: &str,
    source_id: Uuid,
    posting_key: &str,
) -> Result<Option<LeaveLedgerEntryId>, KernelError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM people.leave_ledger_entry WHERE tenant_id = $1 AND source_type = $2 \
         AND source_id = $3 AND posting_key = $4",
    )
    .bind(tenant.as_uuid())
    .bind(source_type)
    .bind(source_id)
    .bind(posting_key)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(id.map(LeaveLedgerEntryId))
}

/// Posts one ledger entry in the caller's transaction (see the module docs).
pub async fn post_entry(
    conn: &mut PgConnection,
    e: &NewLeaveEntry<'_>,
    actor: &Actor,
) -> Result<LeaveEntryReceipt, KernelError> {
    validate(e)?;
    if let Some(id) = existing(conn, e.tenant, e.source_type, e.source_id, e.posting_key).await? {
        return Ok(LeaveEntryReceipt {
            entry_id: id,
            already_posted: true,
        });
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.leave_ledger_entry (tenant_id, institution_id, staff_id, \
         engagement_id, leave_type_id, leave_type_code, is_paid, kind, days, effective_on, \
         leave_year, source_type, source_id, posting_key, reason, actor, correlation_id) \
         SELECT $1, $2, s.id, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17 \
         FROM people.staff_member s WHERE s.id = $3 AND s.tenant_id = $1 AND s.institution_id = $2 \
         RETURNING id",
    )
    .bind(e.tenant.as_uuid())
    .bind(e.institution.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(e.engagement_id.map(|x| x.as_uuid()))
    .bind(e.leave_type_id)
    .bind(e.leave_type_code)
    .bind(e.is_paid)
    .bind(e.kind.as_str())
    .bind(e.days)
    .bind(e.effective_on)
    .bind(e.leave_year)
    .bind(e.source_type)
    .bind(e.source_id)
    .bind(e.posting_key.trim())
    .bind(e.reason)
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "staff member",
    })?;
    Ok(LeaveEntryReceipt {
        entry_id: LeaveLedgerEntryId(id),
        already_posted: false,
    })
}

/// Posts the mirror of `entry` (kind `reversal`, opposite days, same leave type / year / source)
/// with `posting_key` (e.g. `reverse:<entry id>`); idempotent like [`post_entry`]; a reversal is
/// itself never reversed and an entry is reversed at most once (`409`).
pub async fn reverse_entry(
    conn: &mut PgConnection,
    tenant: TenantId,
    entry: LeaveLedgerEntryId,
    posting_key: &str,
    effective_on: NaiveDate,
    reason: &str,
    actor: &Actor,
) -> Result<LeaveEntryReceipt, KernelError> {
    let row = sqlx::query(
        "SELECT kind, source_type, source_id FROM people.leave_ledger_entry \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(entry.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "leave ledger entry",
    })?;
    let kind: String = row.try_get("kind").map_err(map_db_error)?;
    let source_type: String = row.try_get("source_type").map_err(map_db_error)?;
    let source_id: Uuid = row.try_get("source_id").map_err(map_db_error)?;
    if let Some(id) = existing(conn, tenant, &source_type, source_id, posting_key).await? {
        return Ok(LeaveEntryReceipt {
            entry_id: id,
            already_posted: true,
        });
    }
    if kind == LeaveEntryKind::Reversal.as_str() {
        return Err(KernelError::Conflict(
            "a reversal entry cannot be reversed".into(),
        ));
    }
    if posting_key.trim().is_empty() || reason.trim().is_empty() {
        return Err(invalid(
            "reason",
            "required",
            "a posting key and a reason are required",
        ));
    }
    let reversed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.leave_ledger_entry WHERE reverses_entry_id = $1)",
    )
    .bind(entry.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if reversed {
        return Err(KernelError::Conflict(
            "the leave ledger entry is already reversed".into(),
        ));
    }
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.leave_ledger_entry (tenant_id, institution_id, staff_id, \
         engagement_id, leave_type_id, leave_type_code, is_paid, kind, days, effective_on, \
         leave_year, source_type, source_id, posting_key, reverses_entry_id, reason, actor, \
         correlation_id) \
         SELECT tenant_id, institution_id, staff_id, engagement_id, leave_type_id, leave_type_code, \
         is_paid, 'reversal', -days, $3, leave_year, source_type, source_id, $4, id, $5, $6, $7 \
         FROM people.leave_ledger_entry WHERE id = $1 AND tenant_id = $2 RETURNING id",
    )
    .bind(entry.as_uuid())
    .bind(tenant.as_uuid())
    .bind(effective_on)
    .bind(posting_key.trim())
    .bind(reason.trim())
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(LeaveEntryReceipt {
        entry_id: LeaveLedgerEntryId(id),
        already_posted: false,
    })
}

/// Balance projection SQL (shared with `PgLeaveReader`): `$1` tenant, `$2` staff, `$3` year,
/// `$4` leave type (NULL = every type). Reversals count under the kind they reverse.
pub(crate) const BALANCE_SQL: &str = "SELECT e.leave_type_id, max(e.leave_type_code) AS leave_type_code, \
    e.leave_year, \
    COALESCE(sum(e.days) FILTER (WHERE COALESCE(r.kind, e.kind) IN ('opening', 'accrual', 'carry_forward', 'adjustment')), 0) AS entitled, \
    COALESCE(-sum(e.days) FILTER (WHERE COALESCE(r.kind, e.kind) IN ('reservation', 'release')), 0) AS reserved, \
    COALESCE(-sum(e.days) FILTER (WHERE COALESCE(r.kind, e.kind) = 'consumption'), 0) AS consumed, \
    COALESCE(sum(e.days), 0) AS available \
    FROM people.leave_ledger_entry e \
    LEFT JOIN people.leave_ledger_entry r ON r.id = e.reverses_entry_id \
    WHERE e.tenant_id = $1 AND e.staff_id = $2 AND e.leave_year = $3 \
    AND ($4::uuid IS NULL OR e.leave_type_id = $4) \
    GROUP BY e.leave_type_id, e.leave_year ORDER BY max(e.leave_type_code)";

pub(crate) fn balance_from_row(r: &PgRow) -> Result<LeaveBalance, sqlx::Error> {
    Ok(LeaveBalance {
        leave_type_id: r.try_get("leave_type_id")?,
        leave_type_code: r.try_get("leave_type_code")?,
        leave_year: r.try_get("leave_year")?,
        entitled: r.try_get("entitled")?,
        reserved: r.try_get("reserved")?,
        consumed: r.try_get("consumed")?,
        available: r.try_get("available")?,
    })
}

/// Balance of one leave type and year inside the caller's transaction (zero when no entries).
pub async fn balance(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff_id: StaffMemberId,
    leave_type_id: Uuid,
    leave_year: i32,
) -> Result<LeaveBalance, KernelError> {
    let row = sqlx::query(BALANCE_SQL)
        .bind(tenant.as_uuid())
        .bind(staff_id.as_uuid())
        .bind(leave_year)
        .bind(Some(leave_type_id))
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    match row {
        Some(r) => balance_from_row(&r).map_err(map_db_error),
        None => Ok(LeaveBalance {
            leave_type_id,
            leave_type_code: String::new(),
            leave_year,
            entitled: Decimal::ZERO,
            reserved: Decimal::ZERO,
            consumed: Decimal::ZERO,
            available: Decimal::ZERO,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn entry(kind: LeaveEntryKind, days: &str) -> NewLeaveEntry<'static> {
        NewLeaveEntry {
            tenant: TenantId(Uuid::nil()),
            institution: InstitutionId(Uuid::nil()),
            staff_id: StaffMemberId(Uuid::nil()),
            engagement_id: None,
            leave_type_id: Uuid::nil(),
            leave_type_code: "CL",
            is_paid: true,
            kind,
            days: Decimal::from_str(days).unwrap_or_default(),
            effective_on: NaiveDate::default(),
            leave_year: 2026,
            source_type: "leave_request",
            source_id: Uuid::nil(),
            posting_key: "reserve",
            reason: None,
        }
    }

    #[test]
    fn signs_and_precision_are_checked() {
        assert!(validate(&entry(LeaveEntryKind::Accrual, "1.50")).is_ok());
        assert!(validate(&entry(LeaveEntryKind::Accrual, "-1.50")).is_err());
        assert!(validate(&entry(LeaveEntryKind::Reservation, "-2")).is_ok());
        assert!(validate(&entry(LeaveEntryKind::Reservation, "2")).is_err());
        assert!(validate(&entry(LeaveEntryKind::Adjustment, "-0.5")).is_ok());
        assert!(validate(&entry(LeaveEntryKind::Consumption, "-0.125")).is_err());
        assert!(validate(&entry(LeaveEntryKind::Reversal, "1")).is_err());
        assert!(validate(&entry(LeaveEntryKind::Accrual, "0")).is_err());
        let mut e = entry(LeaveEntryKind::Accrual, "1");
        e.leave_type_code = "cl";
        assert!(validate(&e).is_err());
        assert!(valid_source_type("leave_accrual_run") && !valid_source_type("Leave"));
    }
}
