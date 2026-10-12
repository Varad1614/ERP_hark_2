//! **3C** qualification completion and conferment (L3-C-039 … C-041, C-049, C-050).
//!
//! `eligibility_evaluation → degree_audit → institutional_clearance → approval_pending →
//! approved → awarded → record_finalized → exit_completed` (L2-C05 §9.4; MEME uses the same
//! lifecycle, amendment §4). Failed evaluations / audits keep the state and record the unmet
//! requirement; `deferred` interrupts evaluation, audit or clearance until remediation
//! (L3-C-049). Approval runs through the WorkflowPort (`QUALIFICATION_COMPLETION`); a rejected
//! approval returns to `degree_audit`. Award creates the conferred qualification
//! (`qualification_completion.qualification_awarded`); the exit closes the StudentContext as
//! `qualification_completed` and the persona too when no other live context remains.
//! Conferred qualifications are corrected by appending a new version (the old row becomes
//! `corrected`) or revoked through `QUALIFICATION_REVOCATION`; history is never rewritten.

use super::domain::*;
use super::eligibility;
use super::exit::{
    self, all_cleared, apply_resolution, check_resolution, clearances_of, create_clearances,
    finance_evidence, finance_resolution, ClearanceOwner, CLEARANCE_TABLE,
};
use super::kit::{self, StatusEvent};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::{require_live_context, require_student_writable};
use crate::shared::lifecycle::{change_context_status, change_student_status, StatusChange};
use crate::shared::student::{load_student, lock_context, lock_student, StudentRow};
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::student::events::completion::*;
use sutra_contracts::student::ids::{
    ExitClearanceId, QualificationCompletionId, QualificationCorrectionId, StudentExitId, StudentId,
};
use sutra_contracts::student::status::{
    CaseStatus, ConferredQualificationStatus, ExitClearanceStatus, QualificationCompletionStatus,
    StudentContextStatus, StudentExitStatus, StudentStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const TABLE: &str = "qualification_completion";
const ENTITY: &str = "qualification completion";
const CONFERRED: &str = "conferred_qualification";
const CONFERRED_ENTITY: &str = "conferred qualification";
const CORRECTION: &str = "qualification_correction";
const DEFERRAL: &str = "completion_deferral";

type Status = QualificationCompletionStatus;

async fn hydrate(
    state: &StudentState,
    tenant: TenantId,
    mut c: QualificationCompletion,
) -> Result<QualificationCompletion, KernelError> {
    c.clearances = clearances_of(
        &state.pool,
        tenant,
        ClearanceOwner::Completion(c.id.as_uuid()),
    )
    .await?;
    c.deferrals = kit::list_by(
        &state.pool,
        DEFERRAL,
        tenant,
        "completion_id",
        c.id.as_uuid(),
        None,
    )
    .await?;
    Ok(c)
}

pub async fn get(
    state: &StudentState,
    tenant: TenantId,
    id: QualificationCompletionId,
) -> Result<QualificationCompletion, KernelError> {
    let c = kit::fetch(&state.pool, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    hydrate(state, tenant, c).await
}

pub async fn list_for_student(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    status: Option<Status>,
) -> Result<Vec<QualificationCompletion>, KernelError> {
    let list: Vec<QualificationCompletion> = kit::list_by(
        &state.pool,
        TABLE,
        tenant,
        "student_id",
        student_id.as_uuid(),
        status.map(|s| s.as_str()),
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for c in list {
        out.push(hydrate(state, tenant, c).await?);
    }
    Ok(out)
}

fn status_event<'a>(
    (event_type, schema): (&'static str, &'static str),
    c: &'a QualificationCompletion,
    student: &'a StudentRow,
    from: Status,
    reason: Option<&'a str>,
) -> StatusEvent<'a> {
    StatusEvent {
        event_type,
        schema,
        aggregate_id: c.id.as_uuid(),
        institution_id: c.institution_id,
        student: Some(student),
        from: Some(from.as_str()),
        to: c.status.as_str(),
        version: c.version,
        reason,
        process_instance_id: c.process_instance_id.as_deref(),
    }
}

/// Moves a locked completion, checking the machine; `extra` columns are set alongside.
async fn transition(
    conn: &mut PgConnection,
    tenant: TenantId,
    c: &QualificationCompletion,
    to: Status,
    extra: serde_json::Value,
) -> Result<QualificationCompletion, KernelError> {
    if !completion_can_transition(c.status, to) {
        return Err(kit::illegal("the qualification completion", c.status, to));
    }
    let mut changes = if extra.is_object() { extra } else { json!({}) };
    changes["status"] = json!(to.as_str());
    kit::patch(conn, TABLE, tenant, c.id.as_uuid(), changes).await
}

/// Write context of a completion command.
struct Cmd {
    c: QualificationCompletion,
    student: StudentRow,
    scope: ExecutionScope,
}

async fn load_for_write(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
) -> Result<Cmd, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let c: QualificationCompletion =
        kit::fetch(&state.pool, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    let student = load_student(&state.pool, tenant, c.student_id).await?;
    require_student_writable(&student)?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(c.institution_id)).await?;
    Ok(Cmd { c, student, scope })
}

async fn lock_checked(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: QualificationCompletionId,
    expected: Version,
) -> Result<QualificationCompletion, KernelError> {
    let c: QualificationCompletion = kit::lock(conn, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(c.version, expected)?;
    Ok(c)
}

fn require_status(
    c: &QualificationCompletion,
    expected: Status,
    what: &str,
) -> Result<(), KernelError> {
    if c.status != expected {
        return Err(KernelError::Conflict(format!(
            "the qualification completion is {}; {what} needs {expected}",
            c.status
        )));
    }
    Ok(())
}

macro_rules! tx {
    ($state:expr) => {
        $state.pool.begin().await.map_err(map_db_error)?
    };
}

/// Initiates qualification completion evaluation for the student's live context.
pub async fn create(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewQualificationCompletion,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let pathway = cmd.pathway.unwrap_or(CompletionPathway::Completion);
    if pathway == CompletionPathway::MemeExit && !meme_outcome_allowed(cmd.outcome) {
        return Err(kit::invalid(
            "outcome",
            "meme_outcome",
            "a MEME exit awards a certificate, diploma or advanced diploma",
        ));
    }
    let target = kit::writable_student(state, ctx, student_id).await?;
    if target.context.status != StudentContextStatus::Active {
        return Err(KernelError::Conflict(format!(
            "the student context is {}; qualification completion needs an active context",
            target.context.status
        )));
    }
    let tenant = ctx.scope.tenant_id;
    let items: Vec<&str> = exit::clearance_items(cmd.clearances.as_deref())
        .into_iter()
        .map(ClearanceItem::as_str)
        .collect();
    let mut tx = tx!(state);
    let context = lock_context(&mut tx, tenant, target.context.id).await?;
    require_live_context(&context)?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.qualification_completion WHERE tenant_id = $1 \
         AND student_context_id = $2 AND status NOT IN ('exit_completed', 'cancelled')",
    )
    .bind(tenant.as_uuid())
    .bind(context.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open > 0 {
        return Err(KernelError::Conflict(
            "an open qualification completion already exists for this student context".into(),
        ));
    }
    let id = QualificationCompletionId::new();
    let c: QualificationCompletion = kit::insert(
        &mut tx,
        TABLE,
        json!({
            "id": id.as_uuid(),
            "tenant_id": tenant.as_uuid(),
            "institution_id": context.institution_id.as_uuid(),
            "student_id": student_id.as_uuid(),
            "student_context_id": context.id.as_uuid(),
            "programme_id": context.programme_id,
            "curriculum_version_id": context.curriculum_version_id,
            "pathway": pathway.as_str(),
            "outcome": cmd.outcome.as_str(),
            "status": Status::EligibilityEvaluation.as_str(),
            "requested_clearances": items,
        }),
    )
    .await?;
    kit::audit(
        &mut tx,
        &target.scope,
        TABLE,
        "initiate",
        id.as_uuid(),
        json!({"pathway": pathway, "outcome": cmd.outcome, "clearances": items}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, c.id).await
}

/// Evaluates qualification eligibility (3B facts or manual evidence). Eligible → `degree_audit`;
/// otherwise the unresolved requirement is recorded and the state is kept.
pub async fn evaluate_eligibility(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    cmd: EligibilityEvaluation,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let Cmd { c, student, scope } = load_for_write(state, ctx, id).await?;
    require_status(
        &c,
        Status::EligibilityEvaluation,
        "an eligibility evaluation",
    )?;
    let tenant = ctx.scope.tenant_id;
    let facts = eligibility::progression_facts(state, tenant, c.student_context_id.as_uuid()).await;
    let (min, mee) = eligibility::curriculum_facts(state, &scope, c.curriculum_version_id).await?;
    let result = eligibility::decide(c.pathway, c.outcome, &facts, &cmd, min, mee);
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    require_status(
        &locked,
        Status::EligibilityEvaluation,
        "an eligibility evaluation",
    )?;
    let extra = json!({"eligibility_result": result, "eligibility_id": facts.eligibility_id});
    let updated = if result.eligible {
        transition(&mut tx, tenant, &locked, Status::DegreeAudit, extra).await?
    } else {
        kit::patch(&mut tx, TABLE, tenant, id.as_uuid(), extra).await?
    };
    let unmet = result.unmet.join("; ");
    status_event(
        (
            QUALIFICATION_COMPLETION_ELIGIBILITY_EVALUATED,
            QUALIFICATION_COMPLETION_ELIGIBILITY_EVALUATED_SCHEMA,
        ),
        &updated,
        &student,
        locked.status,
        (!unmet.is_empty()).then_some(unmet.as_str()),
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "evaluate_eligibility",
        id.as_uuid(),
        json!(result),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

async fn request_completion_approval(
    state: &StudentState,
    scope: &ExecutionScope,
    c: &QualificationCompletion,
    version: i64,
) -> Result<ApprovalOutcome, KernelError> {
    let mut vars = BTreeMap::new();
    vars.insert("outcome".to_owned(), json!(c.outcome));
    vars.insert("pathway".to_owned(), json!(c.pathway));
    approval::request_approval(
        state,
        scope,
        process::QUALIFICATION_COMPLETION,
        business_key(TABLE, c.id, version),
        vars,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn apply_approval(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    c: &QualificationCompletion,
    student: &StudentRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let to = if approve {
        Status::Approved
    } else {
        Status::DegreeAudit
    };
    let updated = transition(conn, tenant, c, to, json!({"decision_reason": reason})).await?;
    if approve {
        status_event(
            (
                QUALIFICATION_COMPLETION_APPROVED,
                QUALIFICATION_COMPLETION_APPROVED_SCHEMA,
            ),
            &updated,
            student,
            c.status,
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
        c.id.as_uuid(),
        json!({"reason": reason, "processInstanceId": c.process_instance_id}),
        actor,
    )
    .await?;
    Ok(updated)
}

/// When every clearance item is cleared/waived: `approval_pending` (+ event) and auto-approval.
async fn advance_after_clearance(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    c: QualificationCompletion,
    student: &StudentRow,
    approval: Option<&ApprovalOutcome>,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let items = clearances_of(
        &mut *conn,
        tenant,
        ClearanceOwner::Completion(c.id.as_uuid()),
    )
    .await?;
    let Some(outcome) = approval else {
        return Ok(c);
    };
    if c.status != Status::InstitutionalClearance || !all_cleared(items.iter().map(|i| &i.status)) {
        return Ok(c);
    }
    let pid = outcome.process_instance_id();
    let pending = transition(
        conn,
        tenant,
        &c,
        Status::ApprovalPending,
        json!({"process_instance_id": pid}),
    )
    .await?;
    status_event(
        (
            QUALIFICATION_COMPLETION_INSTITUTIONAL_CLEARANCE_COMPLETED,
            QUALIFICATION_COMPLETION_INSTITUTIONAL_CLEARANCE_COMPLETED_SCHEMA,
        ),
        &pending,
        student,
        c.status,
        None,
    )
    .emit(conn, scope, actor)
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        return apply_approval(conn, scope, tenant, &pending, student, true, None, &system).await;
    }
    Ok(pending)
}

/// Records the degree / qualification audit. Passed → `institutional_clearance` and the
/// clearance items open (finance read from Finance).
pub async fn record_audit(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    cmd: DegreeAudit,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let Cmd { c, student, scope } = load_for_write(state, ctx, id).await?;
    require_status(&c, Status::DegreeAudit, "a degree audit")?;
    let tenant = ctx.scope.tenant_id;
    let prior = c.eligibility_result.as_ref();
    let credits = cmd.credits_earned.or(prior.and_then(|e| e.credits_earned));
    let required = prior.and_then(|e| e.required_credits);
    let mut deficiencies: Vec<String> = cmd
        .deficiencies
        .iter()
        .map(|d| d.trim().to_owned())
        .filter(|d| !d.is_empty())
        .collect();
    if let (Some(e), Some(r)) = (credits, required) {
        if e < r {
            deficiencies.push(format!("{e} credits audited; {r} required"));
        }
    }
    let result = AuditResult {
        passed: deficiencies.is_empty(),
        deficiencies,
        note: cmd.note.clone(),
        credits_earned: credits,
        required_credits: required,
        audited_at: chrono::Utc::now(),
    };
    let items = c.requested_clearances.clone();
    let items = exit::clearance_items(Some(&items));
    let existing = clearances_of(
        &state.pool,
        tenant,
        ClearanceOwner::Completion(id.as_uuid()),
    )
    .await?;
    let missing: Vec<ClearanceItem> = items
        .iter()
        .copied()
        .filter(|i| !existing.iter().any(|e| e.item == *i))
        .collect();
    let finance =
        finance_evidence(state, &scope, InstitutionId(c.institution_id), c.student_id).await?;
    let fin_status = finance_resolution(&finance).0;
    let would_clear = result.passed
        && all_cleared(
            existing
                .iter()
                .map(|e| &e.status)
                .chain(missing.iter().map(|i| {
                    if *i == ClearanceItem::Finance {
                        &fin_status
                    } else {
                        &ExitClearanceStatus::Pending
                    }
                })),
        );
    let approval = if would_clear {
        Some(request_completion_approval(state, &scope, &c, c.version + 1).await?)
    } else {
        None
    };
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    require_status(&locked, Status::DegreeAudit, "a degree audit")?;
    let extra = json!({"audit_result": result});
    let updated = if result.passed {
        let u = transition(
            &mut tx,
            tenant,
            &locked,
            Status::InstitutionalClearance,
            extra,
        )
        .await?;
        create_clearances(
            &mut tx,
            tenant,
            InstitutionId(c.institution_id),
            c.student_id,
            ClearanceOwner::Completion(id.as_uuid()),
            &missing,
            &finance,
        )
        .await?;
        status_event(
            (
                QUALIFICATION_COMPLETION_DEGREE_AUDIT_COMPLETED,
                QUALIFICATION_COMPLETION_DEGREE_AUDIT_COMPLETED_SCHEMA,
            ),
            &u,
            &student,
            locked.status,
            None,
        )
        .emit(&mut tx, &scope, actor)
        .await?;
        u
    } else {
        kit::patch(&mut tx, TABLE, tenant, id.as_uuid(), extra).await?
    };
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "degree_audit",
        id.as_uuid(),
        json!(result),
        actor,
    )
    .await?;
    advance_after_clearance(
        &mut tx,
        &scope,
        tenant,
        updated,
        &student,
        approval.as_ref(),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Resolves (or, for finance, refreshes) one clearance item of the completion (If-Match = the
/// completion's version).
#[allow(clippy::too_many_arguments)]
pub async fn resolve_clearance(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    clearance_id: ExitClearanceId,
    expected: Version,
    cmd: Option<ResolveClearance>,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let Cmd { c, student, scope } = load_for_write(state, ctx, id).await?;
    require_status(&c, Status::InstitutionalClearance, "a clearance resolution")?;
    let tenant = ctx.scope.tenant_id;
    let items = clearances_of(
        &state.pool,
        tenant,
        ClearanceOwner::Completion(id.as_uuid()),
    )
    .await?;
    let item =
        items
            .iter()
            .find(|i| i.id == clearance_id)
            .cloned()
            .ok_or(KernelError::NotFound {
                entity: "clearance item",
            })?;
    let (to, note) = match &cmd {
        Some(cmd) => (check_resolution(&item, cmd)?, cmd.note.clone()),
        None => {
            if item.item != ClearanceItem::Finance {
                return Err(KernelError::Conflict(
                    "only the finance item is refreshed from Finance".into(),
                ));
            }
            if matches!(
                item.status,
                ExitClearanceStatus::Cleared | ExitClearanceStatus::Waived
            ) {
                return Err(KernelError::Conflict(
                    "the finance clearance is already resolved".into(),
                ));
            }
            let ev = finance_evidence(state, &scope, InstitutionId(c.institution_id), c.student_id)
                .await?;
            let (s, n) = finance_resolution(&ev);
            (s, Some(n))
        }
    };
    let completes =
        all_cleared(
            items
                .iter()
                .map(|i| if i.id == clearance_id { &to } else { &i.status }),
        );
    let approval = if completes {
        Some(request_completion_approval(state, &scope, &c, c.version + 1).await?)
    } else {
        None
    };
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    require_status(
        &locked,
        Status::InstitutionalClearance,
        "a clearance resolution",
    )?;
    let current: ExitClearance = kit::lock(
        &mut tx,
        CLEARANCE_TABLE,
        "clearance item",
        tenant,
        clearance_id.as_uuid(),
    )
    .await?;
    if current.status != item.status {
        return Err(KernelError::Conflict(
            "the clearance item changed concurrently".into(),
        ));
    }
    if current.status != to {
        apply_resolution(&mut tx, tenant, &current, to, note.clone(), actor).await?;
    }
    let bumped: QualificationCompletion = kit::patch(
        &mut tx,
        TABLE,
        tenant,
        id.as_uuid(),
        json!({"status": locked.status.as_str()}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        CLEARANCE_TABLE,
        "resolve",
        clearance_id.as_uuid(),
        json!({"completionId": id, "item": item.item, "fromStatus": item.status, "toStatus": to, "note": note}),
        actor,
    )
    .await?;
    advance_after_clearance(
        &mut tx,
        &scope,
        tenant,
        bumped,
        &student,
        approval.as_ref(),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Explicit approval decision (`AwaitDecision`, `qualification.confer`). Reject → `degree_audit`.
pub async fn decide(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    cmd: kit::LifecycleDecision,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let Cmd { student, scope, .. } = load_for_write(state, ctx, id).await?;
    let approve = cmd.decision == kit::LifecycleDecisionValue::Approve;
    if !approve {
        kit::required_text("reason", cmd.reason.as_deref())?;
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    require_status(&locked, Status::ApprovalPending, "a decision")?;
    apply_approval(
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

fn awarded_payload(
    q: &ConferredQualification,
    c: &QualificationCompletion,
    student: &StudentRow,
    supersedes: Option<Uuid>,
) -> QualificationAwardedV1 {
    QualificationAwardedV1 {
        completion_id: c.id.as_uuid(),
        student_id: c.student_id.as_uuid(),
        student_context_id: c.student_context_id.as_uuid(),
        institution_id: c.institution_id,
        programme_id: c.programme_id.unwrap_or_default(),
        outcome: q.outcome.as_str().into(),
        conferred_on: q.conferred_on,
        supersedes_completion_id: supersedes,
        process_instance_id: c.process_instance_id.clone(),
        student_subject_ref: student.subject_ref.clone(),
    }
}

/// Confers the approved qualification (`qualification.confer`, step-up).
pub async fn award(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    cmd: AwardQualification,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let Cmd { c, student, scope } = load_for_write(state, ctx, id).await?;
    require_status(&c, Status::Approved, "conferment")?;
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(c.institution_id)).await?;
    let programme_name = match c.programme_id {
        Some(p) => state.references.programme(&scope, p).await?.map(|p| p.name),
        None => None,
    };
    let title = match cmd
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(t) => t.to_owned(),
        None => match programme_name {
            Some(n) => format!("{} — {n}", outcome_label(c.outcome)),
            None => outcome_label(c.outcome).to_owned(),
        },
    };
    let conferred_on = cmd.conferred_on.unwrap_or(today);
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    require_status(&locked, Status::Approved, "conferment")?;
    let q: ConferredQualification = kit::insert(
        &mut tx,
        CONFERRED,
        json!({
            "id": Uuid::now_v7(),
            "tenant_id": tenant.as_uuid(),
            "institution_id": c.institution_id,
            "student_id": c.student_id.as_uuid(),
            "student_context_id": c.student_context_id.as_uuid(),
            "completion_id": id.as_uuid(),
            "programme_id": c.programme_id,
            "outcome": c.outcome.as_str(),
            "title": title,
            "conferred_on": conferred_on,
            "status": ConferredQualificationStatus::Conferred.as_str(),
        }),
    )
    .await?;
    let updated = transition(
        &mut tx,
        tenant,
        &locked,
        Status::Awarded,
        json!({"conferred_qualification_id": q.id, "conferred_on": conferred_on}),
    )
    .await?;
    crate::shared::publish::event(
        &mut tx,
        &scope,
        QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED,
        QUALIFICATION_COMPLETION_QUALIFICATION_AWARDED_SCHEMA,
        id.to_string(),
        &awarded_payload(&q, &updated, &student, None),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "award",
        id.as_uuid(),
        json!({"conferredQualificationId": q.id, "title": q.title, "conferredOn": conferred_on}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Finalises the academic record of an awarded qualification (L3-C-040).
pub async fn finalise(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let Cmd { c, student, scope } = load_for_write(state, ctx, id).await?;
    require_status(&c, Status::Awarded, "record finalisation")?;
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(c.institution_id)).await?;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    if let Some(qid) = locked.conferred_qualification_id {
        let q: ConferredQualification =
            kit::fetch(&mut *tx, CONFERRED, CONFERRED_ENTITY, tenant, qid).await?;
        if q.status == ConferredQualificationStatus::Revoked {
            return Err(KernelError::Conflict(
                "the conferred qualification was revoked".into(),
            ));
        }
    }
    let updated = transition(
        &mut tx,
        tenant,
        &locked,
        Status::RecordFinalized,
        json!({"finalized_on": today}),
    )
    .await?;
    status_event(
        (
            QUALIFICATION_COMPLETION_ACADEMIC_RECORD_FINALIZED,
            QUALIFICATION_COMPLETION_ACADEMIC_RECORD_FINALIZED_SCHEMA,
        ),
        &updated,
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
        "finalise",
        id.as_uuid(),
        json!({"finalizedOn": today}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Completes the student exit of a finalised completion: the StudentContext becomes
/// `qualification_completed` (persona too when no other live context remains).
pub async fn complete_exit(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    cmd: CompleteExit,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let Cmd { c, student, scope } = load_for_write(state, ctx, id).await?;
    require_status(&c, Status::RecordFinalized, "exit completion")?;
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(c.institution_id)).await?;
    let effective = cmd.effective_on.unwrap_or(today);
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    require_status(&locked, Status::RecordFinalized, "exit completion")?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student_exit WHERE tenant_id = $1 AND student_context_id = $2 \
         AND status NOT IN ('rejected', 'cancelled')",
    )
    .bind(tenant.as_uuid())
    .bind(c.student_context_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open > 0 {
        return Err(KernelError::Conflict(
            "another exit is open for this student context; a second terminal exit is refused"
                .into(),
        ));
    }
    let context = lock_context(&mut tx, tenant, c.student_context_id).await?;
    let exit_id = StudentExitId::new();
    let kind = c.pathway.exit_kind();
    let reason = format!("qualification completed ({})", c.outcome);
    let change = StatusChange {
        cause_event_type: STUDENT_EXIT_COMPLETED,
        reason: Some(&reason),
        process_instance_id: c.process_instance_id.as_deref(),
        closed_on: Some(effective),
    };
    change_context_status(
        &mut tx,
        &context,
        StudentContextStatus::QualificationCompleted,
        &change,
        actor,
    )
    .await?;
    let e: StudentExit = kit::insert(
        &mut tx,
        "student_exit",
        json!({
            "id": exit_id.as_uuid(),
            "tenant_id": tenant.as_uuid(),
            "institution_id": c.institution_id,
            "student_id": c.student_id.as_uuid(),
            "student_context_id": c.student_context_id.as_uuid(),
            "kind": kind.as_str(),
            "status": StudentExitStatus::Completed.as_str(),
            "reason": reason,
            "effective_on": effective,
            "completion_id": id.as_uuid(),
            "process_instance_id": c.process_instance_id,
        }),
    )
    .await?;
    let other_live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student_context WHERE tenant_id = $1 AND student_id = $2 \
         AND status IN ('pending_activation', 'active', 'suspended')",
    )
    .bind(tenant.as_uuid())
    .bind(c.student_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if other_live == 0 {
        let persona = lock_student(&mut tx, tenant, c.student_id).await?;
        if persona
            .status
            .can_transition(StudentStatus::QualificationCompleted)
        {
            change_student_status(
                &mut tx,
                &persona,
                context.institution_id,
                StudentStatus::QualificationCompleted,
                &change,
                actor,
            )
            .await?;
        }
    }
    let updated = transition(
        &mut tx,
        tenant,
        &locked,
        Status::ExitCompleted,
        json!({"exit_id": exit_id.as_uuid()}),
    )
    .await?;
    exit::exit_event(
        &mut tx,
        &scope,
        (STUDENT_EXIT_COMPLETED, STUDENT_EXIT_COMPLETED_SCHEMA),
        &e,
        Some(&student),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "complete_exit",
        id.as_uuid(),
        json!({"exitId": exit_id, "kind": kind, "effectiveOn": effective}),
        actor,
    )
    .await?;
    let _ = updated;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Defers completion processing pending remediation (L3-C-049).
pub async fn defer(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    cmd: DeferCompletion,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let reason = kit::required_text("reason", Some(&cmd.reason))?;
    let Cmd { student, scope, .. } = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    if !completion_can_transition(locked.status, Status::Deferred) {
        return Err(kit::illegal(
            "the qualification completion",
            locked.status,
            Status::Deferred,
        ));
    }
    let _: CompletionDeferral = kit::insert(
        &mut tx,
        DEFERRAL,
        json!({
            "id": Uuid::now_v7(),
            "tenant_id": tenant.as_uuid(),
            "institution_id": locked.institution_id,
            "student_id": locked.student_id.as_uuid(),
            "completion_id": id.as_uuid(),
            "deferred_from": locked.status.as_str(),
            "reason": reason,
            "remediation": cmd.remediation,
            "review_on": cmd.review_on,
            "status": CaseStatus::Approved.as_str(),
        }),
    )
    .await?;
    let updated = transition(
        &mut tx,
        tenant,
        &locked,
        Status::Deferred,
        json!({"status_reason": reason}),
    )
    .await?;
    status_event(
        (
            QUALIFICATION_COMPLETION_DEFERRED,
            QUALIFICATION_COMPLETION_DEFERRED_SCHEMA,
        ),
        &updated,
        &student,
        locked.status,
        Some(&reason),
    )
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(&mut tx, &scope, TABLE, "defer", id.as_uuid(),
        json!({"from": locked.status, "reason": reason, "remediation": cmd.remediation, "reviewOn": cmd.review_on}), actor)
        .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Resumes a deferred completion once remediation evidence exists: back to eligibility evaluation.
pub async fn resume(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    cmd: ResumeCompletion,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let note = kit::required_text("resolutionNote", Some(&cmd.resolution_note))?;
    let Cmd { scope, .. } = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    require_status(&locked, Status::Deferred, "resumption")?;
    close_deferral(&mut tx, tenant, id, CaseStatus::Completed, &note).await?;
    transition(
        &mut tx,
        tenant,
        &locked,
        Status::EligibilityEvaluation,
        json!({"status_reason": null, "eligibility_result": null, "audit_result": null}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "resume",
        id.as_uuid(),
        json!({"resolutionNote": note}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

async fn close_deferral(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: QualificationCompletionId,
    to: CaseStatus,
    note: &str,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE student.completion_deferral SET status = $3, resolution_note = $4, \
         version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND completion_id = $2 AND status = 'approved'",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .bind(to.as_str())
    .bind(note)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Cancels a completion before approval (the deferral, if any, is withdrawn).
pub async fn cancel(
    state: &StudentState,
    ctx: &AuthContext,
    id: QualificationCompletionId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<QualificationCompletion, KernelError> {
    let reason = kit::required_text("reason", reason.as_deref())?;
    let Cmd { scope, .. } = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_checked(&mut tx, tenant, id, expected).await?;
    if locked.status == Status::Deferred {
        close_deferral(&mut tx, tenant, id, CaseStatus::Withdrawn, &reason).await?;
    }
    transition(
        &mut tx,
        tenant,
        &locked,
        Status::Cancelled,
        json!({"status_reason": reason}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "cancel",
        id.as_uuid(),
        json!({"from": locked.status, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ---- conferred qualifications: correction / revocation (L3-C-050) ---------------------------

async fn hydrate_conferred(
    state: &StudentState,
    tenant: TenantId,
    mut q: ConferredQualification,
) -> Result<ConferredQualification, KernelError> {
    q.corrections = kit::list_by(
        &state.pool,
        CORRECTION,
        tenant,
        "conferred_qualification_id",
        q.id,
        None,
    )
    .await?;
    Ok(q)
}

pub async fn get_conferred(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ConferredQualification, KernelError> {
    let q = kit::fetch(&state.pool, CONFERRED, CONFERRED_ENTITY, tenant, id).await?;
    hydrate_conferred(state, tenant, q).await
}

pub async fn list_conferred(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    status: Option<ConferredQualificationStatus>,
) -> Result<Vec<ConferredQualification>, KernelError> {
    let list: Vec<ConferredQualification> = kit::list_by(
        &state.pool,
        CONFERRED,
        tenant,
        "student_id",
        student_id.as_uuid(),
        status.map(|s| s.as_str()),
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for q in list {
        out.push(hydrate_conferred(state, tenant, q).await?);
    }
    Ok(out)
}

struct ConferredCmd {
    q: ConferredQualification,
    c: QualificationCompletion,
    student: StudentRow,
    scope: ExecutionScope,
}

async fn load_conferred(
    state: &StudentState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ConferredCmd, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let q: ConferredQualification =
        kit::fetch(&state.pool, CONFERRED, CONFERRED_ENTITY, tenant, id).await?;
    let c: QualificationCompletion = kit::fetch(
        &state.pool,
        TABLE,
        ENTITY,
        tenant,
        q.completion_id.as_uuid(),
    )
    .await?;
    // Corrections of a historical record are allowed for archived personas too (governed exception).
    let student = load_student(&state.pool, tenant, q.student_id).await?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(q.institution_id)).await?;
    Ok(ConferredCmd {
        q,
        c,
        student,
        scope,
    })
}

/// Applies a correction / revocation decision in the caller's tx.
#[allow(clippy::too_many_arguments)]
async fn apply_case(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    q: &ConferredQualification,
    c: &QualificationCompletion,
    student: &StudentRow,
    case: &QualificationCorrection,
    decision: CorrectionDecisionValue,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if case.status != CaseStatus::UnderReview {
        return Err(KernelError::Conflict(format!(
            "the case is {}; it is not under review",
            case.status
        )));
    }
    let (status, resolution, result_id) = match (decision, case.kind) {
        (CorrectionDecisionValue::Reject, _) => (CaseStatus::Rejected, None, None),
        (CorrectionDecisionValue::NoChange, _) => (
            CaseStatus::Completed,
            Some(CorrectionResolution::NoChange),
            None,
        ),
        (CorrectionDecisionValue::Apply, CorrectionKind::Correction) => {
            if q.status != ConferredQualificationStatus::Conferred {
                return Err(KernelError::Conflict(format!(
                    "the qualification is {}",
                    q.status
                )));
            }
            let p = &case.proposed;
            let new: ConferredQualification = kit::insert(
                conn,
                CONFERRED,
                json!({
                    "id": Uuid::now_v7(),
                    "tenant_id": tenant.as_uuid(),
                    "institution_id": q.institution_id,
                    "student_id": q.student_id.as_uuid(),
                    "student_context_id": q.student_context_id.as_uuid(),
                    "completion_id": q.completion_id.as_uuid(),
                    "programme_id": q.programme_id,
                    "outcome": p.outcome.unwrap_or(q.outcome).as_str(),
                    "title": p.title.clone().unwrap_or_else(|| q.title.clone()),
                    "conferred_on": p.conferred_on.unwrap_or(q.conferred_on),
                    "status": ConferredQualificationStatus::Conferred.as_str(),
                    "supersedes_id": q.id,
                }),
            )
            .await?;
            let _: ConferredQualification = kit::patch(conn, CONFERRED, tenant, q.id, json!({
                "status": ConferredQualificationStatus::Corrected.as_str(), "status_reason": case.reason,
            })).await?;
            crate::shared::publish::event(
                conn,
                scope,
                QUALIFICATION_CORRECTED,
                QUALIFICATION_CORRECTED_SCHEMA,
                new.id.to_string(),
                &awarded_payload(&new, c, student, Some(c.id.as_uuid())),
                actor,
            )
            .await?;
            (
                CaseStatus::Completed,
                Some(CorrectionResolution::Corrected),
                Some(new.id),
            )
        }
        (CorrectionDecisionValue::Apply, CorrectionKind::Revocation) => {
            if q.status != ConferredQualificationStatus::Conferred {
                return Err(KernelError::Conflict(format!(
                    "the qualification is {}",
                    q.status
                )));
            }
            let revoked: ConferredQualification = kit::patch(conn, CONFERRED, tenant, q.id, json!({
                "status": ConferredQualificationStatus::Revoked.as_str(), "status_reason": case.reason,
            })).await?;
            StatusEvent {
                event_type: QUALIFICATION_REVOKED,
                schema: QUALIFICATION_REVOKED_SCHEMA,
                aggregate_id: q.id,
                institution_id: q.institution_id,
                student: Some(student),
                from: Some(q.status.as_str()),
                to: revoked.status.as_str(),
                version: revoked.version,
                reason: Some(&case.reason),
                process_instance_id: case.process_instance_id.as_deref(),
            }
            .emit(conn, scope, actor)
            .await?;
            (
                CaseStatus::Completed,
                Some(CorrectionResolution::Revoked),
                None,
            )
        }
    };
    if !correction_can_transition(case.status, status) {
        return Err(kit::illegal("the correction case", case.status, status));
    }
    let _: QualificationCorrection = kit::patch(
        conn,
        CORRECTION,
        tenant,
        case.id.as_uuid(),
        json!({
            "status": status.as_str(),
            "resolution": resolution,
            "decision_reason": reason,
            "result_qualification_id": result_id,
        }),
    )
    .await?;
    kit::audit(
        conn,
        scope,
        CORRECTION,
        "decide",
        case.id.as_uuid(),
        json!({"kind": case.kind, "decision": format!("{decision:?}"), "resolution": resolution, "reason": reason}),
        actor,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn open_case(
    state: &StudentState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    kind: CorrectionKind,
    proposed: ProposedQualification,
    reason: String,
    evidence_ref: Option<String>,
    actor: &Actor,
) -> Result<ConferredQualification, KernelError> {
    let reason = kit::required_text("reason", Some(&reason))?;
    let ConferredCmd {
        q,
        c,
        student,
        scope,
    } = load_conferred(state, ctx, id).await?;
    if q.status != ConferredQualificationStatus::Conferred {
        return Err(KernelError::Conflict(format!(
            "the qualification is {}; only the current conferred version can change",
            q.status
        )));
    }
    if kind == CorrectionKind::Correction {
        let changes = proposed.outcome.is_some_and(|o| o != q.outcome)
            || proposed
                .title
                .as_deref()
                .is_some_and(|t| !t.trim().is_empty() && t != q.title)
            || proposed.conferred_on.is_some_and(|d| d != q.conferred_on);
        if !changes {
            return Err(kit::invalid(
                "proposed",
                "no_change",
                "the correction proposes no change",
            ));
        }
    }
    let tenant = ctx.scope.tenant_id;
    let case_id = QualificationCorrectionId::new();
    let key = if kind == CorrectionKind::Revocation {
        process::QUALIFICATION_REVOCATION
    } else {
        process::QUALIFICATION_COMPLETION
    };
    let mut vars = BTreeMap::new();
    vars.insert("kind".to_owned(), json!(kind));
    let outcome = approval::request_approval(
        state,
        &scope,
        key,
        business_key(CORRECTION, case_id, 1),
        vars,
    )
    .await?;
    let mut tx = tx!(state);
    let locked: ConferredQualification =
        kit::lock(&mut tx, CONFERRED, CONFERRED_ENTITY, tenant, id).await?;
    kit::check_version(locked.version, expected)?;
    let case: QualificationCorrection = kit::insert(
        &mut tx,
        CORRECTION,
        json!({
            "id": case_id.as_uuid(),
            "tenant_id": tenant.as_uuid(),
            "institution_id": q.institution_id,
            "student_id": q.student_id.as_uuid(),
            "conferred_qualification_id": id,
            "kind": match kind { CorrectionKind::Correction => "correction", CorrectionKind::Revocation => "revocation" },
            "status": CaseStatus::UnderReview.as_str(),
            "proposed": proposed,
            "reason": reason,
            "evidence_ref": evidence_ref,
            "process_instance_id": outcome.process_instance_id(),
        }),
    )
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("a correction or revocation case is already open".into()),
        other => other,
    })?;
    let bumped: ConferredQualification = kit::patch(
        &mut tx,
        CONFERRED,
        tenant,
        id,
        json!({"status": locked.status.as_str()}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        CORRECTION,
        "request",
        case_id.as_uuid(),
        json!({"kind": kind, "reason": reason}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        apply_case(
            &mut tx,
            &scope,
            tenant,
            &bumped,
            &c,
            &student,
            &case,
            CorrectionDecisionValue::Apply,
            None,
            &system,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get_conferred(state, tenant, id).await
}

/// Requests a correction (`qualification.confer`, step-up).
pub async fn request_correction(
    state: &StudentState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: NewQualificationCorrection,
    actor: &Actor,
) -> Result<ConferredQualification, KernelError> {
    open_case(
        state,
        ctx,
        id,
        expected,
        CorrectionKind::Correction,
        cmd.proposed,
        cmd.reason,
        cmd.evidence_ref,
        actor,
    )
    .await
}

/// Requests revocation (`qualification.revoke`, step-up).
pub async fn request_revocation(
    state: &StudentState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: RevokeQualification,
    actor: &Actor,
) -> Result<ConferredQualification, KernelError> {
    open_case(
        state,
        ctx,
        id,
        expected,
        CorrectionKind::Revocation,
        ProposedQualification::default(),
        cmd.reason,
        cmd.evidence_ref,
        actor,
    )
    .await
}

/// Decides a pending case (`AwaitDecision`). Revocation cases need `qualification.revoke`.
pub async fn decide_case(
    state: &StudentState,
    ctx: &AuthContext,
    id: Uuid,
    case_id: QualificationCorrectionId,
    expected: Version,
    cmd: CorrectionDecision,
    actor: &Actor,
) -> Result<ConferredQualification, KernelError> {
    let ConferredCmd {
        c, student, scope, ..
    } = load_conferred(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let case: QualificationCorrection = kit::fetch(
        &state.pool,
        CORRECTION,
        "qualification correction",
        tenant,
        case_id.as_uuid(),
    )
    .await?;
    if case.conferred_qualification_id != id {
        return Err(KernelError::NotFound {
            entity: "qualification correction",
        });
    }
    if case.kind == CorrectionKind::Revocation {
        ctx.require(Permission::QualificationRevoke)?;
    }
    if cmd.decision != CorrectionDecisionValue::Apply {
        kit::required_text("reason", cmd.reason.as_deref())?;
    }
    let mut tx = tx!(state);
    let locked: ConferredQualification =
        kit::lock(&mut tx, CONFERRED, CONFERRED_ENTITY, tenant, id).await?;
    kit::check_version(locked.version, expected)?;
    let case: QualificationCorrection = kit::lock(
        &mut tx,
        CORRECTION,
        "qualification correction",
        tenant,
        case_id.as_uuid(),
    )
    .await?;
    let bumped: ConferredQualification = kit::patch(
        &mut tx,
        CONFERRED,
        tenant,
        id,
        json!({"status": locked.status.as_str()}),
    )
    .await?;
    apply_case(
        &mut tx,
        &scope,
        tenant,
        &bumped,
        &c,
        &student,
        &case,
        cmd.decision,
        cmd.reason.as_deref(),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_conferred(state, tenant, id).await
}
