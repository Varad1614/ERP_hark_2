//! The **only** writer of `exams.paper_mark` (contract doc §3.3).
//!
//! * [`update`] — 6B: enter / import marks and move the row through the shared [`MarkStatus`]
//!   machine (`pending → entered → submitted → verified → [moderated] → approved → finalised`,
//!   `withheld` from any review step). Values may change only while the target status allows it
//!   (entry while editable, moderation / grace on `verified → moderated`, internal import
//!   while not final).
//! * [`revise_final`] — 6C: the governed change of **final** marks after an approved
//!   revaluation / rechecking outcome or an approved correction (L3-I-088/118/132). Keeps
//!   `finalised`, bumps `revision`.
//!
//! Every write appends an `exams.paper_mark_history` row (append-only) with the cause, reason
//! and actor, and validates marks against their maxima (`422`).

use crate::shared::rows::{mark_from_row, MARK_COLUMNS};
use crate::shared::Actor;
use rust_decimal::Decimal;
use sqlx::PgConnection;
use sutra_contracts::exams::ids::PaperMarkId;
use sutra_contracts::exams::ports::PaperMarkSummary;
use sutra_contracts::exams::status::MarkStatus;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Mark values to store (the full set; copy the current ones for fields that do not change).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MarkValues {
    pub internal_marks: Option<Decimal>,
    pub internal_max: Option<Decimal>,
    pub internal_source_id: Option<Uuid>,
    pub external_marks: Option<Decimal>,
    pub grace_marks: Decimal,
    pub absent: bool,
}

impl MarkValues {
    /// The values currently stored on `m`.
    pub fn of(m: &PaperMarkSummary) -> Self {
        Self {
            internal_marks: m.internal_marks,
            internal_max: m.internal_max,
            internal_source_id: m.internal_source_id,
            external_marks: m.external_marks,
            grace_marks: m.grace_marks,
            absent: m.absent,
        }
    }
}

fn violation(field: &str, code: &str, msg: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, msg)])
}

fn two_places(v: Decimal) -> bool {
    v.normalize().scale() <= 2
}

/// Validates values against the row's external maximum → 422 on the offending field.
pub fn validate(v: &MarkValues, external_max: Decimal) -> Result<(), KernelError> {
    if let Some(e) = v.external_marks {
        if e < Decimal::ZERO || e > external_max || !two_places(e) {
            return Err(violation(
                "externalMarks",
                "out_of_range",
                "must be between 0 and the paper's maximum, with at most two decimals",
            ));
        }
    }
    match (v.internal_marks, v.internal_max) {
        (Some(m), Some(max)) => {
            if max <= Decimal::ZERO || m < Decimal::ZERO || m > max || !two_places(m) {
                return Err(violation(
                    "internalMarks",
                    "out_of_range",
                    "must be between 0 and the internal maximum, with at most two decimals",
                ));
            }
        }
        (Some(_), None) => {
            return Err(violation(
                "internalMax",
                "required",
                "internal marks need their maximum",
            ))
        }
        _ => {}
    }
    if v.grace_marks < Decimal::ZERO || !two_places(v.grace_marks) {
        return Err(violation(
            "graceMarks",
            "out_of_range",
            "must be zero or positive, with at most two decimals",
        ));
    }
    if v.absent && v.external_marks.is_some() {
        return Err(violation(
            "absent",
            "conflict",
            "an absent candidate has no external marks",
        ));
    }
    Ok(())
}

/// Locks a mark row (`FOR UPDATE`) in the caller's transaction.
pub async fn lock_mark(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: PaperMarkId,
) -> Result<PaperMarkSummary, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {MARK_COLUMNS} FROM exams.paper_mark WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "marks" })?;
    mark_from_row(&row).map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn write(
    conn: &mut PgConnection,
    tenant: TenantId,
    m: &PaperMarkSummary,
    v: &MarkValues,
    to: MarkStatus,
    revision_bump: bool,
    cause: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<PaperMarkSummary, KernelError> {
    validate(v, m.external_max)?;
    let row = sqlx::query(&format!(
        "UPDATE exams.paper_mark SET internal_marks = $3, internal_max = $4, \
         internal_source_id = $5, external_marks = $6, grace_marks = $7, absent = $8, \
         status = $9, status_reason = $10, entered_by = COALESCE($11, entered_by), \
         finalised_at = CASE WHEN $9 = 'finalised' THEN COALESCE(finalised_at, now()) ELSE NULL END, \
         revision = revision + $12, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $13 RETURNING {MARK_COLUMNS}"
    ))
    .bind(m.id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(v.internal_marks)
    .bind(v.internal_max)
    .bind(v.internal_source_id)
    .bind(v.external_marks)
    .bind(v.grace_marks)
    .bind(v.absent)
    .bind(to.as_str())
    .bind(reason)
    .bind(actor.id.as_deref())
    .bind(i32::from(revision_bump))
    .bind(m.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("marks changed concurrently".into()))?;
    sqlx::query(
        "INSERT INTO exams.paper_mark_history (tenant_id, paper_mark_id, from_status, to_status, \
         internal_marks, external_marks, grace_marks, absent, cause, reason, actor, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(tenant.as_uuid())
    .bind(m.id.as_uuid())
    .bind(m.status.as_str())
    .bind(to.as_str())
    .bind(v.internal_marks)
    .bind(v.external_marks)
    .bind(v.grace_marks)
    .bind(v.absent)
    .bind(cause)
    .bind(reason)
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    mark_from_row(&row).map_err(map_db_error)
}

/// 6B: stores `values` (or keeps the current ones when `None`) and moves a **locked** row to
/// `to`. `cause` names the step for the history (`entry`, `import`, `submit`, `verify`,
/// `moderation`, `approve`, `withhold`, `finalise`, …). Values may change only when entering
/// (`to = entered`), moderating (`to = moderated`) or on the internal import of a non-final row
/// (`cause = "import"`); final marks change only through [`revise_final`].
#[allow(clippy::too_many_arguments)]
pub async fn update(
    conn: &mut PgConnection,
    tenant: TenantId,
    m: &PaperMarkSummary,
    values: Option<MarkValues>,
    to: MarkStatus,
    cause: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<PaperMarkSummary, KernelError> {
    let import = cause == "import" && !m.status.is_final() && to == m.status;
    if !import && (m.status.is_final() || !m.status.can_transition(to)) {
        return Err(KernelError::Conflict(format!(
            "marks are {}; they cannot become {to}",
            m.status
        )));
    }
    let current = MarkValues::of(m);
    let v = values.unwrap_or_else(|| current.clone());
    if v != current && !(import || matches!(to, MarkStatus::Entered | MarkStatus::Moderated)) {
        return Err(KernelError::Conflict(format!(
            "marks cannot be changed while moving from {} to {to}",
            m.status
        )));
    }
    write(conn, tenant, m, &v, to, false, cause, reason, actor).await
}

/// 6C: governed revision of **final** marks (revaluation / rechecking outcome, approved
/// correction). `reason` is mandatory; the row stays `finalised` and its `revision` increments.
pub async fn revise_final(
    conn: &mut PgConnection,
    tenant: TenantId,
    m: &PaperMarkSummary,
    values: MarkValues,
    cause: &str,
    reason: &str,
    actor: &Actor,
) -> Result<PaperMarkSummary, KernelError> {
    if !m.status.is_final() {
        return Err(KernelError::Conflict(format!(
            "marks are {}; only final marks are revised",
            m.status
        )));
    }
    if reason.trim().is_empty() {
        return Err(violation("reason", "required", "a revision needs a reason"));
    }
    write(
        conn,
        tenant,
        m,
        &values,
        MarkStatus::Finalised,
        true,
        cause,
        Some(reason),
        actor,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn values() -> MarkValues {
        MarkValues {
            internal_marks: Some(d("30")),
            internal_max: Some(d("40")),
            internal_source_id: None,
            external_marks: Some(d("55.5")),
            grace_marks: Decimal::ZERO,
            absent: false,
        }
    }

    #[test]
    fn marks_are_validated_against_their_maxima() {
        assert!(validate(&values(), d("100")).is_ok());
        let mut over = values();
        over.external_marks = Some(d("101"));
        assert!(validate(&over, d("100")).is_err());
        let mut scale = values();
        scale.external_marks = Some(d("55.555"));
        assert!(validate(&scale, d("100")).is_err());
        let mut no_max = values();
        no_max.internal_max = None;
        assert!(validate(&no_max, d("100")).is_err());
        let mut absent = values();
        absent.absent = true;
        assert!(validate(&absent, d("100")).is_err());
        absent.external_marks = None;
        assert!(validate(&absent, d("100")).is_ok());
    }
}
