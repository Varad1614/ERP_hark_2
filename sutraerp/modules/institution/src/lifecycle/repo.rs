//! Institution lifecycle, profile and timezone persistence (owner-local transactions with
//! outbox + audit, ADR-L5-0006/0007).

use super::common::{business_today, check_version, invalid, status_changed};
use super::domain::{
    ActivationDecision, ActivationRequest, DecideActivation, InstitutionProfile, ProfileRevision,
    ReviseProfile,
};
use crate::domain::{can_transition, illegal_transition, Institution};
use crate::repo::{from_row, COLUMNS};
use crate::shared::approval::{self, process, ApprovalOutcome};
use crate::shared::publish::{audit, event};
use crate::shared::readiness::{self, ReadinessReport};
use crate::shared::{Actor, InstitutionState};
use sqlx::{PgConnection, PgExecutor, PgPool, Row};
use std::collections::BTreeMap;
use sutra_contracts::institution::events::lifecycle::*;
use sutra_contracts::institution::events::ModifiedV1;
use sutra_contracts::institution::ids::ActivationRequestId;
use sutra_contracts::institution::status::InstitutionStatus;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

/// 422 unless `tz` is a zone of PostgreSQL's IANA catalogue (`pg_timezone_names`).
pub async fn require_known_timezone<'e, E: PgExecutor<'e>>(
    executor: E,
    tz: &str,
) -> Result<(), KernelError> {
    let tz = tz.trim();
    let known = super::domain::timezone_format_ok(tz) && timezone_known(executor, tz).await?;
    if known {
        Ok(())
    } else {
        Err(invalid(
            "timezone",
            "unknown-zone",
            format!("`{tz}` is not an IANA timezone such as Asia/Kolkata"),
        ))
    }
}

/// `true` when PostgreSQL knows the zone name.
pub async fn timezone_known<'e, E: PgExecutor<'e>>(
    executor: E,
    tz: &str,
) -> Result<bool, KernelError> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_timezone_names WHERE name = $1 \
           AND name NOT LIKE 'posix/%' AND name NOT LIKE 'right/%')",
    )
    .bind(tz)
    .fetch_one(executor)
    .await
    .map_err(map_db_error)
}

/// Locks one institution of the scope's tenant `FOR UPDATE` (404 when missing / foreign).
pub(crate) async fn lock(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: InstitutionId,
) -> Result<Institution, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.institution WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "institution",
    })?;
    from_row(&row).map_err(map_db_error)
}

fn event_scope(scope: &ExecutionScope, id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(scope.tenant_id, id)
}

/// Moves a locked institution to `to` with its transition event and audit entry.
#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    inst: &Institution,
    to: InstitutionStatus,
    event_type: &'static str,
    schema: &'static str,
    command: &str,
    reason: Option<String>,
    process_instance_id: Option<String>,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    if !can_transition(inst.status, to) {
        return Err(illegal_transition("institution", inst.status, to));
    }
    let row = sqlx::query(&format!(
        "UPDATE institution.institution SET status = $1, status_changed_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $2 AND tenant_id = $3 RETURNING {COLUMNS}"
    ))
    .bind(to.as_str())
    .bind(inst.id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row).map_err(map_db_error)?;
    let es = event_scope(scope, inst.id);
    event(
        conn,
        &es,
        event_type,
        schema,
        inst.id.to_string(),
        &status_changed(
            inst.id.as_uuid(),
            inst.id,
            inst.status.as_str(),
            to.as_str(),
            next.version.0,
            reason.clone(),
            process_instance_id.clone(),
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        &es,
        &format!("institution.institution.{command}"),
        "institution",
        inst.id.to_string(),
        serde_json::json!({
            "fromStatus": inst.status, "toStatus": to, "reason": reason,
            "processInstanceId": process_instance_id, "version": next.version.0,
        }),
        actor,
    )
    .await?;
    Ok(next)
}

/// `draft | returned | rejected → configured` once the mandatory master information is
/// complete and the timezone is valid (L2-B01 §8 "System validation" → "Mark as Configured").
pub async fn configure(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
    expected: Version,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let inst = lock(&mut tx, scope, id).await?;
    check_version(expected, inst.version.0)?;
    if !can_transition(inst.status, InstitutionStatus::Configured) {
        return Err(illegal_transition(
            "institution",
            inst.status,
            InstitutionStatus::Configured,
        ));
    }
    let mut v: Vec<_> = inst
        .profile
        .missing_fields()
        .into_iter()
        .map(|f| {
            sutra_kernel::error::FieldViolation::new(
                f,
                "required",
                format!("{f} is mandatory master information"),
            )
        })
        .collect();
    if !timezone_known(&mut *tx, &inst.timezone).await? {
        v.push(sutra_kernel::error::FieldViolation::new(
            "timezone",
            "unknown-zone",
            "timezone must be an IANA zone",
        ));
    }
    if !v.is_empty() {
        return Err(KernelError::Validation(v));
    }
    let next = transition(
        &mut tx,
        scope,
        &inst,
        InstitutionStatus::Configured,
        INSTITUTION_CONFIGURED,
        INSTITUTION_CONFIGURED_SCHEMA,
        "configure",
        None,
        None,
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

fn not_ready(report: &ReadinessReport) -> KernelError {
    KernelError::Conflict(format!(
        "activation readiness is not met; blocking items: {}",
        report.blocking_keys().join(", ")
    ))
}

/// `configured → pending_approval` (activation request by the Institution Administrator). Refused
/// with 409 while a required readiness item is unmet. The approval step goes through the
/// WorkflowPort; with auto-approval the decision (`→ active`) is applied in the same transaction.
pub async fn submit(
    state: &InstitutionState,
    scope: &ExecutionScope,
    id: InstitutionId,
    expected: Version,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    // 1. Validate (read only).
    let current = crate::repo::get(&state.pool, scope, id).await?;
    check_version(expected, current.version.0)?;
    if !can_transition(current.status, InstitutionStatus::PendingApproval) {
        return Err(illegal_transition(
            "institution",
            current.status,
            InstitutionStatus::PendingApproval,
        ));
    }
    let report = readiness::evaluate(&state.pool, scope, id).await?;
    if !report.ready {
        return Err(not_ready(&report));
    }
    // 2. Approval step before the transaction (deterministic business key → retry-safe).
    let es = event_scope(scope, id);
    let outcome = approval::request_approval(
        state,
        &es,
        process::INSTITUTION_ACTIVATION,
        approval::business_key("institution", id, current.version.0),
        BTreeMap::from([
            (
                "institutionCode".to_owned(),
                serde_json::json!(current.code),
            ),
            (
                "institutionName".to_owned(),
                serde_json::json!(current.name),
            ),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    // 3. One owner-local transaction.
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let inst = lock(&mut tx, scope, id).await?;
    check_version(expected, inst.version.0)?;
    let request_id = ActivationRequestId::new();
    sqlx::query(
        "INSERT INTO institution.activation_request \
           (id, tenant_id, institution_id, status, submitted_version, process_instance_id, readiness, submitted_by) \
         VALUES ($1, $2, $3, 'pending', $4, $5, $6, $7)",
    )
    .bind(request_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(inst.version.0)
    .bind(&pid)
    .bind(serde_json::to_value(&report).map_err(KernelError::internal)?)
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut next = transition(
        &mut tx,
        scope,
        &inst,
        InstitutionStatus::PendingApproval,
        INSTITUTION_SUBMITTED_FOR_APPROVAL,
        INSTITUTION_SUBMITTED_FOR_APPROVAL_SCHEMA,
        "submit",
        None,
        Some(pid.clone()),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        next = apply_decision(
            &mut tx,
            scope,
            &next,
            ActivationDecision::Approve,
            Some("auto-approved (Batch 2 approval mode)".into()),
            &system,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

async fn apply_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    inst: &Institution,
    decision: ActivationDecision,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    let req: Option<(uuid::Uuid, Option<String>)> = sqlx::query_as(
        "SELECT id, process_instance_id FROM institution.activation_request \
         WHERE institution_id = $1 AND tenant_id = $2 AND status = 'pending' FOR UPDATE",
    )
    .bind(inst.id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let Some((req_id, pid)) = req else {
        return Err(KernelError::Conflict(
            "the institution has no pending activation request".into(),
        ));
    };
    let (event_type, schema, command) = match decision {
        ActivationDecision::Approve => (
            INSTITUTION_ACTIVATED,
            INSTITUTION_ACTIVATED_SCHEMA,
            "activate",
        ),
        ActivationDecision::Return => (
            INSTITUTION_RETURNED_FOR_CORRECTION,
            INSTITUTION_RETURNED_FOR_CORRECTION_SCHEMA,
            "return_for_correction",
        ),
        ActivationDecision::Reject => (INSTITUTION_REJECTED, INSTITUTION_REJECTED_SCHEMA, "reject"),
    };
    let next = transition(
        conn,
        scope,
        inst,
        decision.target(),
        event_type,
        schema,
        command,
        reason.clone(),
        pid,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE institution.activation_request SET status = $1, decided_by = $2, decided_at = now(), \
         decision_reason = $3, version = version + 1, updated_at = now() WHERE id = $4",
    )
    .bind(decision.request_status())
    .bind(actor.id.as_deref())
    .bind(reason)
    .bind(req_id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(next)
}

/// Decision of the Institution Activation Authority on the pending request. An approval is
/// re-validated against the current readiness (a stale approval is never silently applied,
/// L4-B-001-BH-005/006); `return` and `reject` need a reason.
pub async fn decide(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
    expected: Version,
    cmd: &DecideActivation,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    let body = super::domain::CommandBody {
        reason: cmd.reason.clone(),
    };
    let reason = match cmd.decision {
        ActivationDecision::Approve => body.optional_reason()?,
        _ => Some(body.required_reason()?),
    };
    let current = crate::repo::get(pool, scope, id).await?;
    check_version(expected, current.version.0)?;
    if current.status != InstitutionStatus::PendingApproval {
        return Err(illegal_transition(
            "institution",
            current.status,
            cmd.decision.target(),
        ));
    }
    if cmd.decision == ActivationDecision::Approve {
        let report = readiness::evaluate(pool, scope, id).await?;
        if !report.ready {
            return Err(not_ready(&report));
        }
    }
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let inst = lock(&mut tx, scope, id).await?;
    check_version(expected, inst.version.0)?;
    let next = apply_decision(&mut tx, scope, &inst, cmd.decision, reason, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Operational lifecycle commands (step-up permission `institution.lifecycle`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleCommand {
    Suspend,
    Reactivate,
    Archive,
}

/// `active → suspended`, `suspended → active`, `active | suspended → archived`; a reason is
/// mandatory evidence for each.
pub async fn lifecycle_command(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
    expected: Version,
    command: LifecycleCommand,
    reason: String,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    let (to, event_type, schema, name) = match command {
        LifecycleCommand::Suspend => (
            InstitutionStatus::Suspended,
            INSTITUTION_SUSPENDED,
            INSTITUTION_SUSPENDED_SCHEMA,
            "suspend",
        ),
        LifecycleCommand::Reactivate => (
            InstitutionStatus::Active,
            INSTITUTION_REACTIVATED,
            INSTITUTION_REACTIVATED_SCHEMA,
            "reactivate",
        ),
        LifecycleCommand::Archive => (
            InstitutionStatus::Archived,
            INSTITUTION_ARCHIVED,
            INSTITUTION_ARCHIVED_SCHEMA,
            "archive",
        ),
    };
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let inst = lock(&mut tx, scope, id).await?;
    check_version(expected, inst.version.0)?;
    // Reactivation is a Suspended → Active restoration only (never Pending → Active).
    if command == LifecycleCommand::Reactivate && inst.status != InstitutionStatus::Suspended {
        return Err(illegal_transition("institution", inst.status, to));
    }
    let next = transition(
        &mut tx,
        scope,
        &inst,
        to,
        event_type,
        schema,
        name,
        Some(reason),
        None,
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

fn require_editable(inst: &Institution) -> Result<(), KernelError> {
    if super::domain::profile_editable(inst.status) {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "institution is {}; its configuration cannot be changed now",
            inst.status
        )))
    }
}

#[allow(clippy::too_many_arguments)]
async fn record_profile_revision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    inst: &Institution,
    revision: i32,
    changed: &[&str],
    previous: serde_json::Value,
    new: serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    let today = business_today(&mut *conn, scope, inst.id).await?;
    sqlx::query(
        "INSERT INTO institution.institution_profile_revision \
           (tenant_id, institution_id, revision, changed_fields, previous_values, new_values, effective_from, revised_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(inst.id.as_uuid())
    .bind(revision)
    .bind(changed.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    .bind(previous)
    .bind(new)
    .bind(today)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Profile revision (L3-B-004): binds to the canonical identity, keeps previous values in the
/// revision history, and emits `institution.profile_revised`. A no-op revision changes nothing.
pub async fn revise_profile(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
    expected: Version,
    cmd: &ReviseProfile,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let inst = lock(&mut tx, scope, id).await?;
    check_version(expected, inst.version.0)?;
    require_editable(&inst)?;
    let (next_profile, changed) = cmd.apply(&inst.profile);
    if changed.is_empty() {
        tx.rollback().await.map_err(map_db_error)?;
        return Ok(inst);
    }
    let p: &InstitutionProfile = &next_profile;
    let row = sqlx::query(&format!(
        "UPDATE institution.institution SET short_name = $1, institution_type = $2, \
           affiliated_university = $3, regulatory_bodies = $4, registration_number = $5, address = $6, \
           contact_phone = $7, official_email = $8, website = $9, academic_pattern = $10, \
           working_days = $11, default_language = $12, administrator_email = $13, \
           profile_revision = profile_revision + 1, version = version + 1, updated_at = now() \
         WHERE id = $14 AND tenant_id = $15 RETURNING {COLUMNS}"
    ))
    .bind(&p.short_name)
    .bind(p.institution_type.map(|t| t.as_str()))
    .bind(&p.affiliated_university)
    .bind(&p.regulatory_bodies)
    .bind(&p.registration_number)
    .bind(&p.address)
    .bind(&p.contact_phone)
    .bind(&p.official_email)
    .bind(&p.website)
    .bind(p.academic_pattern.map(|t| t.as_str()))
    .bind(&p.working_days)
    .bind(&p.default_language)
    .bind(&p.administrator_email)
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row).map_err(map_db_error)?;
    let previous: serde_json::Map<_, _> = changed
        .iter()
        .map(|f| ((*f).to_owned(), inst.profile.field_value(f)))
        .collect();
    let new: serde_json::Map<_, _> = changed
        .iter()
        .map(|f| ((*f).to_owned(), next.profile.field_value(f)))
        .collect();
    record_profile_revision(
        &mut tx,
        scope,
        &next,
        next.profile.revision,
        &changed,
        previous.into(),
        new.into(),
        actor,
    )
    .await?;
    let es = event_scope(scope, id);
    event(
        &mut tx,
        &es,
        INSTITUTION_PROFILE_REVISED,
        INSTITUTION_PROFILE_REVISED_SCHEMA,
        id.to_string(),
        &ModifiedV1 {
            aggregate_id: id.as_uuid(),
            institution_id: id.as_uuid(),
            version: next.version.0,
            changed_fields: changed.iter().map(|s| (*s).to_owned()).collect(),
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.institution.revise_profile",
        "institution",
        id.to_string(),
        serde_json::json!({ "changedFields": changed, "profileRevision": next.profile.revision, "fromVersion": expected.0 }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Business timezone change (validated IANA). Emits `institution.timezone_changed` and records a
/// profile revision; every later business-local date of the institution uses the new zone.
pub async fn change_timezone(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
    expected: Version,
    timezone: &str,
    actor: &Actor,
) -> Result<Institution, KernelError> {
    let tz = timezone.trim().to_owned();
    require_known_timezone(pool, &tz).await?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let inst = lock(&mut tx, scope, id).await?;
    check_version(expected, inst.version.0)?;
    require_editable(&inst)?;
    if inst.timezone == tz {
        tx.rollback().await.map_err(map_db_error)?;
        return Ok(inst);
    }
    let row = sqlx::query(&format!(
        "UPDATE institution.institution SET timezone = $1, profile_revision = profile_revision + 1, \
         version = version + 1, updated_at = now() WHERE id = $2 AND tenant_id = $3 RETURNING {COLUMNS}"
    ))
    .bind(&tz)
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row).map_err(map_db_error)?;
    record_profile_revision(
        &mut tx,
        scope,
        &next,
        next.profile.revision,
        &["timezone"],
        serde_json::json!({ "timezone": inst.timezone }),
        serde_json::json!({ "timezone": tz }),
        actor,
    )
    .await?;
    let es = event_scope(scope, id);
    event(
        &mut tx,
        &es,
        INSTITUTION_TIMEZONE_CHANGED,
        INSTITUTION_TIMEZONE_CHANGED_SCHEMA,
        id.to_string(),
        &InstitutionTimezoneChangedV1 {
            institution_id: id.as_uuid(),
            timezone: tz.clone(),
            previous_timezone: inst.timezone.clone(),
            version: next.version.0,
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.institution.change_timezone",
        "institution",
        id.to_string(),
        serde_json::json!({ "timezone": tz, "previousTimezone": inst.timezone }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Activation request cycles, newest first.
pub async fn list_activation_requests(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
) -> Result<Vec<ActivationRequest>, KernelError> {
    crate::shared::guard::institution_status(pool, scope, id).await?;
    let rows = sqlx::query(
        "SELECT id, institution_id, status, submitted_version, process_instance_id, readiness, \
           submitted_by, submitted_at, decided_by, decided_at, decision_reason \
         FROM institution.activation_request WHERE institution_id = $1 AND tenant_id = $2 \
         ORDER BY submitted_at DESC, id DESC",
    )
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| -> Result<ActivationRequest, KernelError> {
            let readiness: serde_json::Value = r.try_get("readiness").map_err(map_db_error)?;
            Ok(ActivationRequest {
                id: ActivationRequestId(r.try_get("id").map_err(map_db_error)?),
                institution_id: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
                status: r.try_get("status").map_err(map_db_error)?,
                submitted_version: r.try_get("submitted_version").map_err(map_db_error)?,
                process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
                readiness: serde_json::from_value(readiness).map_err(KernelError::internal)?,
                submitted_by: r.try_get("submitted_by").map_err(map_db_error)?,
                submitted_at: r.try_get("submitted_at").map_err(map_db_error)?,
                decided_by: r.try_get("decided_by").map_err(map_db_error)?,
                decided_at: r.try_get("decided_at").map_err(map_db_error)?,
                decision_reason: r.try_get("decision_reason").map_err(map_db_error)?,
            })
        })
        .collect()
}

/// Applied profile revisions, newest first.
pub async fn list_profile_revisions(
    pool: &PgPool,
    scope: &ExecutionScope,
    id: InstitutionId,
) -> Result<Vec<ProfileRevision>, KernelError> {
    crate::shared::guard::institution_status(pool, scope, id).await?;
    let rows = sqlx::query(
        "SELECT revision, changed_fields, previous_values, new_values, effective_from, revised_by, created_at \
         FROM institution.institution_profile_revision WHERE institution_id = $1 AND tenant_id = $2 \
         ORDER BY revision DESC",
    )
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            Ok(ProfileRevision {
                revision: r.try_get("revision")?,
                changed_fields: r.try_get("changed_fields")?,
                previous_values: r.try_get("previous_values")?,
                new_values: r.try_get("new_values")?,
                effective_from: r.try_get("effective_from")?,
                revised_by: r.try_get("revised_by")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}
