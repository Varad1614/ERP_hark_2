//! Qualification eligibility determination (L3-C-036) and credit-ledger synchronisation to the
//! Academic Bank of Credits (L3-C-037).
//!
//! **Eligibility contract (read by 3C, `docs/batch3-contracts.md` §5.3/§7):** the newest row of
//! `student.qualification_eligibility` per student context is the current determination.
//! `eligible = true` when the curriculum's `min_total_credits` is met by the 3B credit ledger
//! (signed sum of all entries) **and** every mandatory (`*_core`) component has earned credits;
//! `outcomes` lists the `QualificationOutcome`s currently reachable (with NEP MEME the
//! intermediate certificate/diploma/advanced-diploma exits at ¼, ½, ¾ of the minimum, so
//! `outcomes` may be non-empty while `eligible` is false). When the curriculum defines no
//! minimum credits the determination is manual (`basis = manual`). Every evaluation emits
//! `qualification_eligibility.updated` ([`QualificationEligibilityV1`]).
//!
//! **ABC:** the no-op adapter answers `NotConfigured`; 3B records that attempt as an `abc_sync`
//! row in status `failed` with `failureReason = "not_configured"` and emits `abc_sync.failed`
//! (the institutional ledger stays authoritative and unchanged). `Accepted` → `acknowledged` +
//! `abc_sync.completed`; `Rejected` → `failed` with the registry's reason.

use super::domain::*;
use super::repo::ledger_total;
use crate::registration::support::{
    audit, conflict, emit, invalid, parse_enum, status_changed, write_student,
};
use crate::shared::student::lock_context;
use crate::shared::{Actor, StudentState};
use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgExecutor, PgPool};
use sutra_contracts::student::events::progression::*;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::{AbcCreditSubmission, ExternalSyncOutcome};
use sutra_contracts::student::status::{AbcSyncStatus, QualificationOutcome};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::TenantId;
use sutra_persistence::map_db_error;
use uuid::Uuid;

const ELIGIBILITY_COLUMNS: &str = "id, student_id, student_context_id, institution_id, programme_id, curriculum_version_id, \
    eligible, outcomes, total_credits::float8 AS total_credits, required_credits::float8 AS required_credits, outstanding, \
    basis, note, evaluated_by, evaluated_at";

#[derive(sqlx::FromRow)]
struct EligibilityRow {
    id: Uuid,
    student_id: Uuid,
    student_context_id: Uuid,
    institution_id: Uuid,
    programme_id: Option<Uuid>,
    curriculum_version_id: Option<Uuid>,
    eligible: bool,
    outcomes: Vec<String>,
    total_credits: f64,
    required_credits: Option<f64>,
    outstanding: Vec<String>,
    basis: String,
    note: Option<String>,
    evaluated_by: Option<String>,
    evaluated_at: DateTime<Utc>,
}

impl EligibilityRow {
    fn dto(self) -> Result<QualificationEligibility, KernelError> {
        Ok(QualificationEligibility {
            outcomes: self
                .outcomes
                .iter()
                .map(|o| parse_enum(o, QualificationOutcome::parse, "qualification outcome"))
                .collect::<Result<_, _>>()?,
            id: self.id,
            student_id: self.student_id,
            student_context_id: self.student_context_id,
            institution_id: self.institution_id,
            programme_id: self.programme_id,
            curriculum_version_id: self.curriculum_version_id,
            eligible: self.eligible,
            total_credits: self.total_credits,
            required_credits: self.required_credits,
            outstanding: self.outstanding,
            basis: self.basis,
            note: self.note,
            evaluated_by: self.evaluated_by,
            evaluated_at: self.evaluated_at,
        })
    }
}

/// Current determination of a student (newest evaluation of any context), if any.
pub async fn current<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Option<QualificationEligibility>, KernelError> {
    let sql = format!(
        "SELECT {ELIGIBILITY_COLUMNS} FROM student.qualification_eligibility WHERE tenant_id = $1 AND student_id = $2 \
         ORDER BY evaluated_at DESC, id DESC LIMIT 1"
    );
    sqlx::query_as::<_, EligibilityRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .map(EligibilityRow::dto)
        .transpose()
}

pub async fn history(
    pool: &PgPool,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Vec<QualificationEligibility>, KernelError> {
    crate::shared::student::load_student(pool, tenant, student_id).await?;
    let sql = format!(
        "SELECT {ELIGIBILITY_COLUMNS} FROM student.qualification_eligibility WHERE tenant_id = $1 AND student_id = $2 \
         ORDER BY evaluated_at DESC, id DESC"
    );
    sqlx::query_as::<_, EligibilityRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(EligibilityRow::dto)
        .collect()
}

/// Evaluates qualification eligibility from the ledger and the curriculum (L3-C-036).
pub async fn evaluate(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: EvaluateEligibility,
    actor: &Actor,
) -> Result<QualificationEligibility, KernelError> {
    let ss = write_student(state, tenant, student_id, false).await?;
    let cv_id = ss
        .context
        .curriculum_version_id
        .ok_or_else(|| conflict("the student context has no curriculum version"))?;
    let cv = state
        .references
        .curriculum_version(&ss.scope, cv_id)
        .await?
        .ok_or_else(|| conflict("the student's curriculum version no longer exists"))?;
    let comps = state
        .references
        .curriculum_components(&ss.scope, cv_id)
        .await?;
    let note = super::repo::text(cmd.note.clone(), "note", 2000)?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let ctx = lock_context(&mut tx, tenant, ss.context.id).await?;
    let total = ledger_total(&mut *tx, tenant, ctx.id.as_uuid()).await?;
    let earned: Vec<Uuid> = sqlx::query_scalar(
        "SELECT curriculum_component_id FROM student.credit_ledger_entry WHERE tenant_id = $1 AND student_context_id = $2 \
         AND curriculum_component_id IS NOT NULL GROUP BY 1 \
         HAVING sum(CASE WHEN kind IN ('revoked', 'expired') THEN -credits ELSE credits END) > 0",
    )
    .bind(tenant.as_uuid())
    .bind(ctx.id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let (eligible, outcomes, outstanding, basis, required) = match cv.min_total_credits {
        Some(required) => {
            let mut outstanding = vec![];
            if total + 1e-9 < required {
                outstanding.push(format!(
                    "minimum programme credits {required} not met ({total} recorded)"
                ));
            }
            let missing: Vec<String> = comps
                .iter()
                .filter(|c| is_mandatory_category(&c.category) && !earned.contains(&c.id))
                .map(|c| {
                    format!(
                        "mandatory component {} ({}) not earned",
                        c.slot_code, c.title
                    )
                })
                .collect();
            let mandatory_met = missing.is_empty();
            outstanding.extend(missing);
            let outcomes =
                reachable_outcomes(total, required, cv.multiple_entry_exit, mandatory_met);
            let eligible = outstanding.is_empty();
            (eligible, outcomes, outstanding, "ledger", Some(required))
        }
        None => {
            let manual = cmd.manual_eligible.ok_or_else(|| {
                invalid(
                    "manualEligible",
                    "required",
                    "the curriculum defines no minimum credits: decide manually",
                )
            })?;
            if note.is_none() {
                return Err(invalid(
                    "note",
                    "required",
                    "a manual determination needs a note",
                ));
            }
            let outcomes = if manual {
                cmd.manual_outcomes
                    .clone()
                    .filter(|o| !o.is_empty())
                    .unwrap_or_else(|| vec![QualificationOutcome::Degree])
            } else {
                vec![]
            };
            let outstanding = if manual {
                vec![]
            } else {
                vec!["determined not eligible".to_owned()]
            };
            (manual, outcomes, outstanding, "manual", None)
        }
    };
    let outcome_names: Vec<String> = outcomes.iter().map(|o| o.as_str().to_owned()).collect();
    let sql = format!(
        "INSERT INTO student.qualification_eligibility (tenant_id, institution_id, student_id, student_context_id, programme_id, \
         curriculum_version_id, eligible, outcomes, total_credits, required_credits, outstanding, basis, note, evaluated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING {ELIGIBILITY_COLUMNS}"
    );
    let e = sqlx::query_as::<_, EligibilityRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ctx.institution_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(ctx.id.as_uuid())
        .bind(ctx.programme_id)
        .bind(cv_id)
        .bind(eligible)
        .bind(&outcome_names)
        .bind(total)
        .bind(required)
        .bind(&outstanding)
        .bind(basis)
        .bind(note.as_deref())
        .bind(actor.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto()?;
    emit(
        &mut tx,
        &ss.scope,
        QUALIFICATION_ELIGIBILITY_UPDATED,
        QUALIFICATION_ELIGIBILITY_UPDATED_SCHEMA,
        e.id,
        &QualificationEligibilityV1 {
            eligibility_id: e.id,
            student_id: e.student_id,
            student_context_id: e.student_context_id,
            institution_id: e.institution_id,
            eligible,
            outcomes: outcome_names.clone(),
            total_credits: total,
        },
        actor,
    )
    .await?;
    audit(&mut tx, &ss.scope, "student.qualification_eligibility.evaluate", "qualification_eligibility", e.id,
        json!({"eligible": eligible, "outcomes": outcome_names, "totalCredits": total, "basis": basis}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(e)
}

// ================================================================== ABC sync

const ABC_COLUMNS: &str =
    "id, student_id, institution_id, entry_ids, total_credits::float8 AS total_credits, status, \
    failure_reason, external_reference, attempted_at, version";

#[derive(sqlx::FromRow)]
struct AbcRow {
    id: Uuid,
    student_id: Uuid,
    institution_id: Uuid,
    entry_ids: Vec<Uuid>,
    total_credits: f64,
    status: String,
    failure_reason: Option<String>,
    external_reference: Option<String>,
    attempted_at: DateTime<Utc>,
    version: i64,
}

impl AbcRow {
    fn dto(self) -> Result<AbcSync, KernelError> {
        Ok(AbcSync {
            status: parse_enum(&self.status, AbcSyncStatus::parse, "abc sync status")?,
            id: self.id,
            student_id: self.student_id,
            institution_id: self.institution_id,
            entry_ids: self.entry_ids,
            total_credits: self.total_credits,
            failure_reason: self.failure_reason,
            external_reference: self.external_reference,
            attempted_at: self.attempted_at,
            version: self.version,
        })
    }
}

pub async fn get_abc(pool: &PgPool, tenant: TenantId, id: Uuid) -> Result<AbcSync, KernelError> {
    let sql =
        format!("SELECT {ABC_COLUMNS} FROM student.abc_sync WHERE id = $1 AND tenant_id = $2");
    sqlx::query_as::<_, AbcRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(pool)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "abc sync" })?
        .dto()
}

pub async fn list_abc(
    pool: &PgPool,
    tenant: TenantId,
    student_id: StudentId,
) -> Result<Vec<AbcSync>, KernelError> {
    crate::shared::student::load_student(pool, tenant, student_id).await?;
    let sql = format!("SELECT {ABC_COLUMNS} FROM student.abc_sync WHERE tenant_id = $1 AND student_id = $2 ORDER BY created_at DESC, id DESC");
    sqlx::query_as::<_, AbcRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(AbcRow::dto)
        .collect()
}

/// Submits the institution-approved ledger entries not yet acknowledged by ABC (L3-C-037).
pub async fn synchronise(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    actor: &Actor,
) -> Result<AbcSync, KernelError> {
    let ss = write_student(state, tenant, student_id, false).await?;
    let cv_id = ss
        .context
        .curriculum_version_id
        .ok_or_else(|| conflict("the student context has no curriculum version"))?;
    let cv = state
        .references
        .curriculum_version(&ss.scope, cv_id)
        .await?
        .ok_or_else(|| conflict("the student's curriculum version no longer exists"))?;
    if !cv.abc_enabled {
        return Err(conflict(
            "ABC synchronisation is not enabled for the student's curriculum",
        ));
    }
    let pending: Vec<(Uuid, String, f64)> = sqlx::query_as(
        "SELECT e.id, e.kind, e.credits::float8 FROM student.credit_ledger_entry e WHERE e.tenant_id = $1 \
         AND e.student_context_id = $2 AND e.institution_approved AND NOT EXISTS (SELECT 1 FROM student.abc_sync s \
         WHERE s.student_context_id = e.student_context_id AND s.status = 'acknowledged' AND e.id = ANY(s.entry_ids)) \
         ORDER BY e.recorded_at, e.id",
    )
    .bind(tenant.as_uuid())
    .bind(ss.context.id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    if pending.is_empty() {
        return Err(conflict(
            "no institution-approved credits are waiting for ABC synchronisation",
        ));
    }
    let entry_ids: Vec<Uuid> = pending.iter().map(|p| p.0).collect();
    let total: f64 = pending
        .iter()
        .map(|(_, k, c)| {
            if k == "revoked" || k == "expired" {
                -c
            } else {
                *c
            }
        })
        .sum();
    let outcome = state
        .abc
        .submit_credits(
            &ss.scope,
            AbcCreditSubmission {
                institution_id: ss.context.institution_id,
                student_id,
                abc_account_id: None,
                ledger_entry_ids: entry_ids.clone(),
                total_credits: total,
            },
        )
        .await?;
    let (status, reason, reference) = match &outcome {
        ExternalSyncOutcome::Accepted { reference } => {
            (AbcSyncStatus::Acknowledged, None, Some(reference.clone()))
        }
        ExternalSyncOutcome::Rejected { reason } => {
            (AbcSyncStatus::Failed, Some(reason.clone()), None)
        }
        ExternalSyncOutcome::NotConfigured => (
            AbcSyncStatus::Failed,
            Some("not_configured".to_owned()),
            None,
        ),
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    lock_context(&mut tx, tenant, ss.context.id).await?;
    let sql = format!(
        "INSERT INTO student.abc_sync (tenant_id, institution_id, student_id, student_context_id, entry_ids, total_credits, status, \
         failure_reason, external_reference) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {ABC_COLUMNS}"
    );
    let s = sqlx::query_as::<_, AbcRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(&entry_ids)
        .bind(total)
        .bind(status.as_str())
        .bind(reason.as_deref())
        .bind(reference.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .dto()?;
    let payload = status_changed(
        s.id,
        &ss.scope,
        Some(student_id.as_uuid()),
        Some("pending"),
        status.as_str(),
        s.version,
        reason.as_deref(),
        None,
        ss.subject_ref(),
    );
    if status == AbcSyncStatus::Acknowledged {
        emit(
            &mut tx,
            &ss.scope,
            ABC_SYNC_COMPLETED,
            ABC_SYNC_COMPLETED_SCHEMA,
            s.id,
            &payload,
            actor,
        )
        .await?;
    } else {
        emit(
            &mut tx,
            &ss.scope,
            ABC_SYNC_FAILED,
            ABC_SYNC_FAILED_SCHEMA,
            s.id,
            &payload,
            actor,
        )
        .await?;
    }
    audit(&mut tx, &ss.scope, "student.abc_sync.submit", "abc_sync", s.id,
        json!({"entries": entry_ids.len(), "totalCredits": total, "status": status, "reason": reason}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(s)
}
