//! **3C** alumni transition persistence + commands (L3-C-072 … C-074).

use super::domain::*;
use crate::completion::domain::{ConferredQualification, StudentExit};
use crate::completion::kit::{self, StatusEvent};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_student_writable;
use crate::shared::lifecycle::{change_student_status, StatusChange};
use crate::shared::student::{contexts_of, load_context, load_student, lock_student, StudentRow};
use crate::shared::{Actor, StudentState};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::student::events::alumni::*;
use sutra_contracts::student::ids::{AlumniTransitionId, StudentId};
use sutra_contracts::student::status::{
    AlumniTransitionStatus, ConferredQualificationStatus, StudentContextStatus, StudentExitKind,
    StudentExitStatus, StudentStatus,
};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const TABLE: &str = "alumni_transition";
const ENTITY: &str = "alumni transition";
const CORRECTION: &str = "alumni_transition_correction";

type Status = AlumniTransitionStatus;

macro_rules! tx {
    ($state:expr) => {
        $state.pool.begin().await.map_err(map_db_error)?
    };
}

async fn hydrate(
    state: &StudentState,
    tenant: TenantId,
    mut t: AlumniTransition,
) -> Result<AlumniTransition, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(c) FROM student.alumni_transition_correction c WHERE c.tenant_id = $1 AND c.transition_id = $2 ORDER BY c.recorded_at, c.id",
    )
    .bind(tenant.as_uuid())
    .bind(t.id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    t.corrections = rows
        .into_iter()
        .map(kit::decode)
        .collect::<Result<_, _>>()?;
    Ok(t)
}

pub async fn get(
    state: &StudentState,
    tenant: TenantId,
    id: AlumniTransitionId,
) -> Result<AlumniTransition, KernelError> {
    let t = kit::fetch(&state.pool, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    hydrate(state, tenant, t).await
}

pub async fn list(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    status: Option<Status>,
) -> Result<Vec<AlumniTransition>, KernelError> {
    let list: Vec<AlumniTransition> = kit::list_by(
        &state.pool,
        TABLE,
        tenant,
        "student_id",
        student_id.as_uuid(),
        status.map(|s| s.as_str()),
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for t in list {
        out.push(hydrate(state, tenant, t).await?);
    }
    Ok(out)
}

fn eligible_context_status(s: StudentContextStatus) -> bool {
    matches!(
        s,
        StudentContextStatus::QualificationCompleted
            | StudentContextStatus::Withdrawn
            | StudentContextStatus::Transferred
    )
}

fn event<'a>(
    (event_type, schema): (&'static str, &'static str),
    t: &'a AlumniTransition,
    student: &'a StudentRow,
    from: Status,
    reason: Option<&'a str>,
) -> StatusEvent<'a> {
    StatusEvent {
        event_type,
        schema,
        aggregate_id: t.id.as_uuid(),
        institution_id: t.institution_id,
        student: Some(student),
        from: Some(from.as_str()),
        to: t.status.as_str(),
        version: t.version,
        reason,
        process_instance_id: t.process_instance_id.as_deref(),
    }
}

async fn move_to(
    conn: &mut PgConnection,
    tenant: TenantId,
    t: &AlumniTransition,
    to: Status,
    extra: serde_json::Value,
) -> Result<AlumniTransition, KernelError> {
    if !can_transition(t.status, to) {
        return Err(kit::illegal("the alumni transition", t.status, to));
    }
    let mut changes = if extra.is_object() { extra } else { json!({}) };
    changes["status"] = json!(to.as_str());
    kit::patch(conn, TABLE, tenant, t.id.as_uuid(), changes).await
}

/// Initiates the transition for a terminal context (L3-C-072).
pub async fn initiate(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewAlumniTransition,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let student = load_student(&state.pool, tenant, student_id).await?;
    require_student_writable(&student)?;
    let context = match cmd.student_context_id {
        Some(id) => {
            let c = load_context(&state.pool, tenant, id).await?;
            if c.student_id != student_id {
                return Err(kit::invalid(
                    "studentContextId",
                    "other_student",
                    "the context belongs to another student",
                ));
            }
            c
        }
        None => contexts_of(&state.pool, tenant, student_id)
            .await?
            .into_iter()
            .find(|c| eligible_context_status(c.status))
            .ok_or_else(|| {
                KernelError::Conflict(
                    "the student has no completed or eligible-exit student context".into(),
                )
            })?,
    };
    if !context.status.is_terminal() {
        return Err(KernelError::Conflict(format!(
            "the student context is {}; the student relationship has not concluded",
            context.status
        )));
    }
    let scope = kit::writable_institution(state, ctx, context.institution_id).await?;
    let mut tx = tx!(state);
    let created: AlumniTransition = kit::insert(&mut tx, TABLE, json!({
        "id": Uuid::now_v7(), "tenant_id": tenant.as_uuid(), "institution_id": context.institution_id.as_uuid(),
        "student_id": student_id.as_uuid(), "student_context_id": context.id.as_uuid(),
        "status": Status::Created.as_str(),
    })).await.map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("an alumni transition already exists for this student context".into()),
        other => other,
    })?;
    let t = move_to(
        &mut tx,
        tenant,
        &created,
        Status::PendingVerification,
        json!({}),
    )
    .await?;
    event(
        (
            ALUMNI_TRANSITION_INITIATED,
            ALUMNI_TRANSITION_INITIATED_SCHEMA,
        ),
        &t,
        &student,
        Status::Created,
        None,
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "initiate",
        t.id.as_uuid(),
        json!({"studentContextId": context.id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, t.id).await
}

struct Cmd {
    t: AlumniTransition,
    student: StudentRow,
    scope: ExecutionScope,
}

/// Loads for a command. `historical` commands (correction, archival) are allowed on an archived persona.
async fn load(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    historical: bool,
) -> Result<Cmd, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let t: AlumniTransition = kit::fetch(&state.pool, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    let student = load_student(&state.pool, tenant, t.student_id).await?;
    if !historical {
        require_student_writable(&student)?;
    }
    let scope = kit::writable_institution(state, ctx, InstitutionId(t.institution_id)).await?;
    Ok(Cmd { t, student, scope })
}

async fn lock_checked(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AlumniTransitionId,
    expected: Version,
) -> Result<AlumniTransition, KernelError> {
    let t: AlumniTransition = kit::lock(conn, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(t.version, expected)?;
    Ok(t)
}

/// Evaluates eligibility from the completion / exit facts (Domain C-owned, read-only here).
async fn evaluate(
    state: &StudentState,
    tenant: TenantId,
    t: &AlumniTransition,
) -> Result<(AlumniEligibility, Option<Uuid>, Option<Uuid>), KernelError> {
    let context = load_context(&state.pool, tenant, t.student_context_id).await?;
    let mut unmet = Vec::new();
    let exits: Vec<StudentExit> = kit::list_by(
        &state.pool,
        "student_exit",
        tenant,
        "student_context_id",
        context.id.as_uuid(),
        Some(StudentExitStatus::Completed.as_str()),
    )
    .await?;
    let exit = exits.first();
    let mut basis = None;
    let mut qualification = None;
    match context.status {
        StudentContextStatus::QualificationCompleted => {
            let quals: Vec<ConferredQualification> = kit::list_by(
                &state.pool,
                "conferred_qualification",
                tenant,
                "student_context_id",
                context.id.as_uuid(),
                Some(ConferredQualificationStatus::Conferred.as_str()),
            )
            .await?;
            match quals.first() {
                Some(q) => {
                    basis = Some(AlumniBasis::Qualification);
                    qualification = Some(q.id);
                }
                None => unmet.push(
                    "no current conferred qualification (it may have been revoked)".to_owned(),
                ),
            }
        }
        StudentContextStatus::Withdrawn | StudentContextStatus::Transferred => {
            if exit.is_some_and(|e| {
                matches!(
                    e.kind,
                    StudentExitKind::Withdrawal | StudentExitKind::Transfer
                )
            }) {
                basis = Some(AlumniBasis::EligibleExit);
            } else {
                unmet.push("no completed withdrawal or transfer exit".to_owned());
            }
        }
        other => unmet.push(format!(
            "a {other} student context is not eligible for alumni transition"
        )),
    }
    if exit.is_none() {
        unmet.push("the student exit is not completed".to_owned());
    }
    Ok((
        AlumniEligibility {
            eligible: unmet.is_empty(),
            basis,
            unmet,
            evaluated_at: chrono::Utc::now(),
        },
        qualification,
        exit.map(|e| e.id.as_uuid()),
    ))
}

pub async fn validate_eligibility(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    expected: Version,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let Cmd { t, student, scope } = load(state, ctx, id, false).await?;
    if t.status != Status::PendingVerification {
        return Err(kit::illegal(
            "the alumni transition",
            t.status,
            Status::Verified,
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let (result, qualification, exit) = evaluate(state, tenant, &t).await?;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    let extra = json!({"eligibility_result": result, "basis": result.basis, "conferred_qualification_id": qualification, "exit_id": exit});
    if result.eligible {
        let v = move_to(&mut tx, tenant, &locked, Status::Verified, extra).await?;
        event(
            (
                ALUMNI_TRANSITION_ELIGIBILITY_VALIDATED,
                ALUMNI_TRANSITION_ELIGIBILITY_VALIDATED_SCHEMA,
            ),
            &v,
            &student,
            locked.status,
            None,
        )
        .emit(&mut tx, &scope, actor)
        .await?;
    } else {
        let _: AlumniTransition = kit::patch(
            &mut tx,
            TABLE,
            tenant,
            id.as_uuid(),
            json!({"eligibility_result": result}),
        )
        .await?;
    }
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "validate_eligibility",
        id.as_uuid(),
        json!(result),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

#[allow(clippy::too_many_arguments)]
async fn apply_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    t: &AlumniTransition,
    student: &StudentRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let to = if approve {
        Status::Approved
    } else {
        Status::Rejected
    };
    let r = move_to(conn, tenant, t, to, json!({"decision_reason": reason})).await?;
    if approve {
        event(
            (
                ALUMNI_TRANSITION_APPROVED,
                ALUMNI_TRANSITION_APPROVED_SCHEMA,
            ),
            &r,
            student,
            t.status,
            reason,
        )
        .emit(conn, scope, actor)
        .await?;
    }
    kit::audit(
        conn,
        scope,
        TABLE,
        if approve { "approve" } else { "reject" },
        t.id.as_uuid(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    Ok(r)
}

/// Policy validation → approval (`ALUMNI_TRANSITION`).
pub async fn validate_policy(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    expected: Version,
    cmd: ValidateAlumniPolicy,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    if !(1900..=2200).contains(&cmd.graduation_year) {
        return Err(kit::invalid(
            "graduationYear",
            "range",
            "graduation year out of range",
        ));
    }
    let Cmd { t, student, scope } = load(state, ctx, id, false).await?;
    if t.status != Status::Verified {
        return Err(kit::illegal(
            "the alumni transition",
            t.status,
            Status::PendingApproval,
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let mut vars = BTreeMap::new();
    vars.insert("basis".to_owned(), json!(t.basis));
    let outcome = approval::request_approval(
        state,
        &scope,
        process::ALUMNI_TRANSITION,
        business_key(TABLE, id, t.version),
        vars,
    )
    .await?;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    let pending = move_to(
        &mut tx,
        tenant,
        &locked,
        Status::PendingApproval,
        json!({
            "graduation_year": cmd.graduation_year,
            "alumni_email": cmd.alumni_email.map(|e| e.trim().to_owned()).filter(|e| !e.is_empty()),
            "directory_consent": cmd.directory_consent,
            "process_instance_id": outcome.process_instance_id(),
        }),
    )
    .await?;
    event(
        (
            ALUMNI_TRANSITION_POLICY_VALIDATED,
            ALUMNI_TRANSITION_POLICY_VALIDATED_SCHEMA,
        ),
        &pending,
        &student,
        locked.status,
        None,
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "validate_policy",
        id.as_uuid(),
        json!({"graduationYear": cmd.graduation_year}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        apply_decision(
            &mut tx, &scope, tenant, &pending, &student, true, None, &system,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn decide(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    expected: Version,
    cmd: kit::LifecycleDecision,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let approve = cmd.decision == kit::LifecycleDecisionValue::Approve;
    if !approve {
        kit::required_text("reason", cmd.reason.as_deref())?;
    }
    let Cmd { student, scope, .. } = load(state, ctx, id, false).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    if locked.status != Status::PendingApproval {
        return Err(KernelError::Conflict(format!(
            "the alumni transition is {}; no decision is pending",
            locked.status
        )));
    }
    apply_decision(
        &mut tx,
        &scope,
        tenant,
        &locked,
        &student,
        approve,
        cmd.reason.as_deref(),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Executes the approved transition: student relationship closed, alumni relationship activated.
pub async fn execute(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    expected: Version,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let Cmd { student, scope, .. } = load(state, ctx, id, false).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    // `transitioning` is internal (no event, L2-C08 §12.4).
    let running = move_to(&mut tx, tenant, &locked, Status::Transitioning, json!({})).await?;
    let activated = move_to(
        &mut tx,
        tenant,
        &running,
        Status::Activated,
        json!({"activated_at": chrono::Utc::now()}),
    )
    .await?;
    event(
        (
            ALUMNI_TRANSITION_STUDENT_RELATIONSHIP_CLOSED,
            ALUMNI_TRANSITION_STUDENT_RELATIONSHIP_CLOSED_SCHEMA,
        ),
        &activated,
        &student,
        Status::Transitioning,
        None,
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    event(
        (
            ALUMNI_TRANSITION_ALUMNI_RELATIONSHIP_ACTIVATED,
            ALUMNI_TRANSITION_ALUMNI_RELATIONSHIP_ACTIVATED_SCHEMA,
        ),
        &activated,
        &student,
        Status::Transitioning,
        None,
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "execute",
        id.as_uuid(),
        json!({}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Completes the transition (hand-off to H06) and archives the persona when no live context remains.
pub async fn complete(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    expected: Version,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let Cmd { student, scope, .. } = load(state, ctx, id, false).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    let done = move_to(
        &mut tx,
        tenant,
        &locked,
        Status::Completed,
        json!({"completed_at": chrono::Utc::now()}),
    )
    .await?;
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student_context WHERE tenant_id = $1 AND student_id = $2 \
         AND status IN ('pending_activation', 'active', 'suspended')",
    )
    .bind(tenant.as_uuid())
    .bind(locked.student_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let archived = if live == 0 {
        let persona = lock_student(&mut tx, tenant, locked.student_id).await?;
        if persona.status != StudentStatus::Archived {
            let change = StatusChange {
                cause_event_type: ALUMNI_TRANSITION_COMPLETED,
                reason: Some("alumni transition completed"),
                ..Default::default()
            };
            change_student_status(
                &mut tx,
                &persona,
                InstitutionId(locked.institution_id),
                StudentStatus::Archived,
                &change,
                actor,
            )
            .await?;
            true
        } else {
            false
        }
    } else {
        false
    };
    event(
        (
            ALUMNI_TRANSITION_COMPLETED,
            ALUMNI_TRANSITION_COMPLETED_SCHEMA,
        ),
        &done,
        &student,
        locked.status,
        None,
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "complete",
        id.as_uuid(),
        json!({"personaArchived": archived}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Governed correction (L3-C-073): appends a correction record and a new version.
pub async fn correct(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    expected: Version,
    cmd: CorrectAlumniTransition,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let reason = kit::required_text("reason", Some(&cmd.reason))?;
    let Cmd { t, student, scope } = load(state, ctx, id, true).await?;
    if !matches!(t.status, Status::Activated | Status::Completed) {
        return Err(KernelError::Conflict(format!(
            "the alumni transition is {}; only an executed transition is corrected",
            t.status
        )));
    }
    let mut previous = serde_json::Map::new();
    let mut corrected = serde_json::Map::new();
    if let Some(y) = cmd
        .graduation_year
        .filter(|y| Some(*y) != t.graduation_year)
    {
        if !(1900..=2200).contains(&y) {
            return Err(kit::invalid(
                "graduationYear",
                "range",
                "graduation year out of range",
            ));
        }
        previous.insert("graduation_year".into(), json!(t.graduation_year));
        corrected.insert("graduation_year".into(), json!(y));
    }
    if let Some(e) = cmd
        .alumni_email
        .map(|e| e.trim().to_owned())
        .filter(|e| Some(e) != t.alumni_email.as_ref())
    {
        previous.insert("alumni_email".into(), json!(t.alumni_email));
        corrected.insert(
            "alumni_email".into(),
            json!(if e.is_empty() { None } else { Some(e) }),
        );
    }
    if let Some(c) = cmd.directory_consent.filter(|c| *c != t.directory_consent) {
        previous.insert("directory_consent".into(), json!(t.directory_consent));
        corrected.insert("directory_consent".into(), json!(c));
    }
    if corrected.is_empty() {
        return Err(kit::invalid(
            "body",
            "no_change",
            "the correction changes nothing",
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let fields: Vec<String> = corrected.keys().map(|k| kit::camel_key(k)).collect();
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    let mut changes = corrected.clone();
    changes.insert(
        "correction_count".into(),
        json!(locked.correction_count + 1),
    );
    let updated: AlumniTransition = kit::patch(
        &mut tx,
        TABLE,
        tenant,
        id.as_uuid(),
        serde_json::Value::Object(changes),
    )
    .await?;
    kit::append(&mut tx, CORRECTION, json!({
        "tenant_id": tenant.as_uuid(), "transition_id": id.as_uuid(), "transition_version": updated.version,
        "changed_fields": fields, "previous": kit::camelize(serde_json::Value::Object(previous)),
        "corrected": kit::camelize(serde_json::Value::Object(corrected)), "reason": reason, "actor": actor.id,
    })).await?;
    kit::modified_event(
        &mut tx,
        &scope,
        (
            ALUMNI_TRANSITION_CORRECTED,
            ALUMNI_TRANSITION_CORRECTED_SCHEMA,
        ),
        id.as_uuid(),
        Some(&student),
        updated.version,
        fields.clone(),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "correct",
        id.as_uuid(),
        json!({"changedFields": fields, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Archival of a completed transition (L3-C-074).
pub async fn archive(
    state: &StudentState,
    ctx: &AuthContext,
    id: AlumniTransitionId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AlumniTransition, KernelError> {
    let Cmd { student, scope, .. } = load(state, ctx, id, true).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    let done = move_to(
        &mut tx,
        tenant,
        &locked,
        Status::Archived,
        json!({"archived_at": chrono::Utc::now(), "status_reason": reason}),
    )
    .await?;
    event(
        (
            ALUMNI_TRANSITION_ARCHIVED,
            ALUMNI_TRANSITION_ARCHIVED_SCHEMA,
        ),
        &done,
        &student,
        locked.status,
        reason.as_deref(),
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "archive",
        id.as_uuid(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}
