//! **3C** student exits (L3-C-042 … C-048) and institutional clearance (L3-C-046).
//!
//! Exit flow: initiate (→ `clearance_pending`, `student_exit.initiated`) → resolve clearance
//! items (finance from `FinanceClearancePort`, never manually) → all cleared/waived →
//! `approval_pending` + `exit_clearance.completed` → WorkflowPort `STUDENT_EXIT` approval
//! (auto-approve in Batch 3; `/decision` otherwise) → `approved` → `/complete` closes the
//! StudentContext through `shared::lifecycle` (→ withdrawn / transferred / discontinued /
//! terminated) and emits `student_exit.completed`. Deceased closure also archives the persona
//! when no live context remains (contract §4.2).

use super::domain::*;
use super::kit::{self, StatusEvent};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::lifecycle::{change_context_status, change_student_status, StatusChange};
use crate::shared::student::{lock_context, lock_student, StudentRow};
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use chrono::Utc;
use serde_json::json;
use sqlx::{PgConnection, PgExecutor};
use std::collections::BTreeMap;
use sutra_contracts::finance::{FinanceEvidence, StudentFinanceQuery};
use sutra_contracts::student::events::completion::*;
use sutra_contracts::student::ids::{ExitClearanceId, StudentExitId, StudentId};
use sutra_contracts::student::status::{
    ExitClearanceStatus, StudentExitKind, StudentExitStatus, StudentStatus,
};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use uuid::Uuid;

const TABLE: &str = "student_exit";
const ENTITY: &str = "student exit";
pub(crate) const CLEARANCE_TABLE: &str = "exit_clearance";

/// Owner of a set of clearance items.
#[derive(Clone, Copy, Debug)]
pub enum ClearanceOwner {
    Exit(Uuid),
    Completion(Uuid),
}

impl ClearanceOwner {
    fn column(self) -> (&'static str, Uuid) {
        match self {
            Self::Exit(id) => ("exit_id", id),
            Self::Completion(id) => ("completion_id", id),
        }
    }
}

/// Clearance items of an exit or a completion, in creation order.
pub async fn clearances_of<'e, E: PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    owner: ClearanceOwner,
) -> Result<Vec<ExitClearance>, KernelError> {
    let (col, id) = owner.column();
    let mut items: Vec<ExitClearance> =
        kit::list_by(executor, CLEARANCE_TABLE, tenant, col, id, None).await?;
    items.reverse();
    Ok(items)
}

/// Finance evidence for the clearance `finance` item (stub: `NotApplicable`).
pub async fn finance_evidence(
    state: &StudentState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    student_id: StudentId,
) -> Result<FinanceEvidence, KernelError> {
    state
        .finance
        .student_dues(
            scope,
            StudentFinanceQuery {
                institution_id,
                student_id: student_id.as_uuid(),
            },
        )
        .await
}

/// Status + note of the finance item from Finance's evidence (Finance owns the fact).
pub fn finance_resolution(e: &FinanceEvidence) -> (ExitClearanceStatus, String) {
    match e {
        FinanceEvidence::NotApplicable => (
            ExitClearanceStatus::Cleared,
            "no Finance obligation configured".into(),
        ),
        FinanceEvidence::Cleared { reference } => (
            ExitClearanceStatus::Cleared,
            format!("cleared by Finance ({reference})"),
        ),
        FinanceEvidence::Outstanding { detail } => (
            ExitClearanceStatus::Blocked,
            format!("outstanding dues: {detail}"),
        ),
        FinanceEvidence::Pending { detail } => (
            ExitClearanceStatus::Pending,
            format!("Finance evidence pending: {detail}"),
        ),
    }
}

/// Normalised clearance list (finance always first when requested, duplicates removed).
pub fn clearance_items(requested: Option<&[ClearanceItem]>) -> Vec<ClearanceItem> {
    let mut out: Vec<ClearanceItem> = Vec::new();
    for i in requested.unwrap_or(DEFAULT_CLEARANCES) {
        if !out.contains(i) {
            out.push(*i);
        }
    }
    out
}

/// `true` when every item is cleared or waived (an empty set is cleared).
pub fn all_cleared<'a>(statuses: impl IntoIterator<Item = &'a ExitClearanceStatus>) -> bool {
    statuses.into_iter().all(|s| {
        matches!(
            s,
            ExitClearanceStatus::Cleared | ExitClearanceStatus::Waived
        )
    })
}

/// Inserts the clearance items of a new exit / completion.
#[allow(clippy::too_many_arguments)]
pub async fn create_clearances(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution_id: InstitutionId,
    student_id: StudentId,
    owner: ClearanceOwner,
    items: &[ClearanceItem],
    finance: &FinanceEvidence,
) -> Result<Vec<ExitClearance>, KernelError> {
    let (col, owner_id) = owner.column();
    let mut out = Vec::new();
    for item in items {
        let (status, note, resolved) = if *item == ClearanceItem::Finance {
            let (s, n) = finance_resolution(finance);
            let done = s != ExitClearanceStatus::Pending;
            (s, Some(n), done)
        } else {
            (ExitClearanceStatus::Pending, None, false)
        };
        out.push(
            kit::insert::<ExitClearance>(
                conn,
                CLEARANCE_TABLE,
                json!({
                    "tenant_id": tenant.as_uuid(),
                    "institution_id": institution_id.as_uuid(),
                    "student_id": student_id.as_uuid(),
                    (col): owner_id,
                    "item": item.as_str(),
                    "status": status.as_str(),
                    "note": note,
                    "resolved_by": if resolved { Some("system:finance") } else { None },
                    "resolved_at": if resolved { Some(Utc::now()) } else { None },
                }),
            )
            .await?,
        );
    }
    Ok(out)
}

/// Validates a manual resolution of one item (finance is never resolved by hand: L4-C-046
/// "shall not substitute another authority").
pub fn check_resolution(
    item: &ExitClearance,
    cmd: &ResolveClearance,
) -> Result<ExitClearanceStatus, KernelError> {
    if item.item == ClearanceItem::Finance {
        return Err(KernelError::Conflict(
            "the finance clearance is owned by Finance; refresh it instead".into(),
        ));
    }
    let to = match cmd.status {
        ClearanceResolution::Cleared => ExitClearanceStatus::Cleared,
        ClearanceResolution::Waived => ExitClearanceStatus::Waived,
        ClearanceResolution::Blocked => ExitClearanceStatus::Blocked,
    };
    if to != ExitClearanceStatus::Cleared
        && kit::required_text("note", cmd.note.as_deref()).is_err()
    {
        return Err(kit::invalid(
            "note",
            "required",
            "a note is required to waive or block a clearance item",
        ));
    }
    if !clearance_can_transition(item.status, to) {
        return Err(kit::illegal("the clearance item", item.status, to));
    }
    Ok(to)
}

/// Applies one item resolution inside the caller's tx.
pub async fn apply_resolution(
    conn: &mut PgConnection,
    tenant: TenantId,
    item: &ExitClearance,
    to: ExitClearanceStatus,
    note: Option<String>,
    actor: &Actor,
) -> Result<ExitClearance, KernelError> {
    let done = to != ExitClearanceStatus::Pending;
    kit::patch(
        conn,
        CLEARANCE_TABLE,
        tenant,
        item.id.as_uuid(),
        json!({
            "status": to.as_str(),
            "note": note,
            "resolved_by": if done { actor.id.clone() } else { None },
            "resolved_at": if done { Some(Utc::now()) } else { None },
        }),
    )
    .await
}

fn with_clearances(mut e: StudentExit, items: Vec<ExitClearance>) -> StudentExit {
    e.clearances = items;
    e
}

/// One exit with its clearance items.
pub async fn get(
    state: &StudentState,
    tenant: TenantId,
    id: StudentExitId,
) -> Result<StudentExit, KernelError> {
    let e: StudentExit = kit::fetch(&state.pool, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    let items = clearances_of(&state.pool, tenant, ClearanceOwner::Exit(id.as_uuid())).await?;
    Ok(with_clearances(e, items))
}

async fn with_items(
    state: &StudentState,
    tenant: TenantId,
    list: Vec<StudentExit>,
) -> Result<Vec<StudentExit>, KernelError> {
    let mut out = Vec::with_capacity(list.len());
    for e in list {
        let items =
            clearances_of(&state.pool, tenant, ClearanceOwner::Exit(e.id.as_uuid())).await?;
        out.push(with_clearances(e, items));
    }
    Ok(out)
}

pub async fn list_for_student(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    status: Option<StudentExitStatus>,
) -> Result<Vec<StudentExit>, KernelError> {
    let list = kit::list_by(
        &state.pool,
        TABLE,
        tenant,
        "student_id",
        student_id.as_uuid(),
        status.map(|s| s.as_str()),
    )
    .await?;
    with_items(state, tenant, list).await
}

pub async fn list_for_institution(
    state: &StudentState,
    tenant: TenantId,
    institution_id: InstitutionId,
    status: Option<StudentExitStatus>,
) -> Result<Vec<StudentExit>, KernelError> {
    let list = kit::list_by(
        &state.pool,
        TABLE,
        tenant,
        "institution_id",
        institution_id.as_uuid(),
        status.map(|s| s.as_str()),
    )
    .await?;
    with_items(state, tenant, list).await
}

fn exit_payload(e: &StudentExit, student: Option<&StudentRow>) -> StudentExitV1 {
    StudentExitV1 {
        exit_id: e.id.as_uuid(),
        student_id: e.student_id.as_uuid(),
        student_context_id: e.student_context_id.as_uuid(),
        institution_id: e.institution_id,
        kind: e.kind.as_str().into(),
        status: e.status.as_str().into(),
        effective_on: e.effective_on.or(e.requested_effective_on),
        reason: Some(e.reason.clone()),
        process_instance_id: e.process_instance_id.clone(),
        student_subject_ref: student.and_then(|s| s.subject_ref.clone()),
    }
}

/// Emits a [`StudentExitV1`] event.
pub async fn exit_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    (event_type, schema): (&'static str, &'static str),
    e: &StudentExit,
    student: Option<&StudentRow>,
    actor: &Actor,
) -> Result<(), KernelError> {
    crate::shared::publish::event(
        conn,
        scope,
        event_type,
        schema,
        e.id.to_string(),
        &exit_payload(e, student),
        actor,
    )
    .await
}

fn validate_new(cmd: &NewStudentExit) -> Result<(), KernelError> {
    kit::required_text("reason", Some(&cmd.reason))?;
    match cmd.kind {
        StudentExitKind::Completion | StudentExitKind::MemeExit => Err(kit::invalid(
            "kind",
            "completion_flow",
            "completion and MEME exits are completed through the qualification completion",
        )),
        StudentExitKind::Transfer if kit::required_text("destinationInstitution", cmd.destination_institution.as_deref()).is_err() => {
            Err(kit::invalid("destinationInstitution", "required", "a transfer-out exit names the receiving institution"))
        }
        StudentExitKind::Deceased | StudentExitKind::Termination
            if kit::required_text("evidenceRef", cmd.evidence_ref.as_deref()).is_err() =>
        {
            Err(kit::invalid(
                "evidenceRef",
                "required",
                "deceased closure needs the verified notification reference; termination the decision reference",
            ))
        }
        _ => Ok(()),
    }
}

/// Decision applied after clearance or through `/decision`.
#[allow(clippy::too_many_arguments)]
async fn apply_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    e: &StudentExit,
    student: &StudentRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    let to = if approve {
        StudentExitStatus::Approved
    } else {
        StudentExitStatus::Rejected
    };
    if !exit_can_transition(e.status, to) {
        return Err(kit::illegal("the exit", e.status, to));
    }
    let updated: StudentExit = kit::patch(
        conn,
        TABLE,
        tenant,
        e.id.as_uuid(),
        json!({
            "status": to.as_str(), "decision_reason": reason,
        }),
    )
    .await?;
    kit::audit(conn, scope, TABLE, if approve { "approve" } else { "reject" }, e.id.as_uuid(),
        json!({"fromStatus": e.status, "toStatus": to, "reason": reason, "processInstanceId": e.process_instance_id}), actor).await?;
    if !approve {
        exit_event(
            conn,
            scope,
            (STUDENT_EXIT_CANCELLED, STUDENT_EXIT_CANCELLED_SCHEMA),
            &updated,
            Some(student),
            actor,
        )
        .await?;
    }
    Ok(updated)
}

/// After a clearance change: when every item is cleared/waived, move to `approval_pending`
/// (+ `exit_clearance.completed`) and apply an auto-approval.
async fn advance_after_clearance(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    e: StudentExit,
    student: &StudentRow,
    approval: Option<&ApprovalOutcome>,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    let items = clearances_of(&mut *conn, tenant, ClearanceOwner::Exit(e.id.as_uuid())).await?;
    let (Some(outcome), true) = (approval, all_cleared(items.iter().map(|i| &i.status))) else {
        return Ok(e);
    };
    if e.status != StudentExitStatus::ClearancePending {
        return Ok(e);
    }
    let pid = outcome.process_instance_id();
    let pending: StudentExit = kit::patch(
        conn,
        TABLE,
        tenant,
        e.id.as_uuid(),
        json!({
            "status": StudentExitStatus::ApprovalPending.as_str(), "process_instance_id": pid,
        }),
    )
    .await?;
    StatusEvent {
        event_type: EXIT_CLEARANCE_COMPLETED,
        schema: EXIT_CLEARANCE_COMPLETED_SCHEMA,
        aggregate_id: e.id.as_uuid(),
        institution_id: e.institution_id,
        student: Some(student),
        from: Some(e.status.as_str()),
        to: pending.status.as_str(),
        version: pending.version,
        reason: None,
        process_instance_id: Some(pid),
    }
    .emit(conn, scope, actor)
    .await?;
    kit::audit(
        conn,
        scope,
        TABLE,
        "clearance_complete",
        e.id.as_uuid(),
        json!({"items": items.len(), "processInstanceId": pid}),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        return apply_decision(conn, scope, tenant, &pending, student, true, None, &system).await;
    }
    Ok(pending)
}

async fn request_exit_approval(
    state: &StudentState,
    scope: &ExecutionScope,
    id: Uuid,
    version: i64,
    kind: StudentExitKind,
) -> Result<ApprovalOutcome, KernelError> {
    let mut vars = BTreeMap::new();
    vars.insert("kind".to_owned(), json!(kind));
    approval::request_approval(
        state,
        scope,
        process::STUDENT_EXIT,
        business_key(TABLE, id, version),
        vars,
    )
    .await
}

/// Initiates an exit of the student's live context.
pub async fn create(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewStudentExit,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    validate_new(&cmd)?;
    let target = kit::writable_student(state, ctx, student_id).await?;
    let tenant = ctx.scope.tenant_id;
    let items = clearance_items(cmd.clearances.as_deref());
    let finance = if items.contains(&ClearanceItem::Finance) {
        finance_evidence(state, &target.scope, target.institution_id(), student_id).await?
    } else {
        FinanceEvidence::NotApplicable
    };
    let id = StudentExitId::new();
    // When the initial items already clear (e.g. finance only), start the approval at once.
    let pre_cleared = items.iter().all(|i| *i == ClearanceItem::Finance)
        && finance_resolution(&finance).0 == ExitClearanceStatus::Cleared;
    let approval = if pre_cleared {
        Some(request_exit_approval(state, &target.scope, id.as_uuid(), 2, cmd.kind).await?)
    } else {
        None
    };
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(sutra_persistence::map_db_error)?;
    let context = lock_context(&mut tx, tenant, target.context.id).await?;
    crate::shared::guard::require_live_context(&context)?;
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student_exit WHERE tenant_id = $1 AND student_context_id = $2 \
         AND status NOT IN ('rejected', 'cancelled')",
    )
    .bind(tenant.as_uuid())
    .bind(context.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(sutra_persistence::map_db_error)?;
    let open_completion: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.qualification_completion WHERE tenant_id = $1 \
         AND student_context_id = $2 AND status IN ('approved', 'awarded', 'record_finalized')",
    )
    .bind(tenant.as_uuid())
    .bind(context.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(sutra_persistence::map_db_error)?;
    if open > 0 || open_completion > 0 {
        return Err(KernelError::Conflict(
            "a competing exit or an approved qualification completion already exists for this student context".into(),
        ));
    }
    let e: StudentExit = kit::insert(
        &mut tx,
        TABLE,
        json!({
            "id": id.as_uuid(),
            "tenant_id": tenant.as_uuid(),
            "institution_id": context.institution_id.as_uuid(),
            "student_id": student_id.as_uuid(),
            "student_context_id": context.id.as_uuid(),
            "kind": cmd.kind.as_str(),
            "status": StudentExitStatus::ClearancePending.as_str(),
            "reason": cmd.reason.trim(),
            "requested_effective_on": cmd.requested_effective_on,
            "destination_institution": cmd.destination_institution.as_deref().map(str::trim),
            "evidence_ref": cmd.evidence_ref.as_deref().map(str::trim),
        }),
    )
    .await?;
    create_clearances(
        &mut tx,
        tenant,
        context.institution_id,
        student_id,
        ClearanceOwner::Exit(id.as_uuid()),
        &items,
        &finance,
    )
    .await?;
    exit_event(
        &mut tx,
        &target.scope,
        (STUDENT_EXIT_INITIATED, STUDENT_EXIT_INITIATED_SCHEMA),
        &e,
        Some(&target.student),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &target.scope,
        TABLE,
        "initiate",
        id.as_uuid(),
        json!({"kind": cmd.kind, "items": items}),
        actor,
    )
    .await?;
    // Bump to v2 so the business key of an early approval matches.
    let e = if approval.is_some() {
        advance_after_clearance(
            &mut tx,
            &target.scope,
            tenant,
            e,
            &target.student,
            approval.as_ref(),
            actor,
        )
        .await?
    } else {
        e
    };
    tx.commit().await.map_err(sutra_persistence::map_db_error)?;
    get(state, tenant, e.id).await
}

async fn load_for_write(
    state: &StudentState,
    ctx: &AuthContext,
    id: StudentExitId,
) -> Result<(StudentExit, StudentRow, ExecutionScope), KernelError> {
    let tenant = ctx.scope.tenant_id;
    let e: StudentExit = kit::fetch(&state.pool, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    let student = crate::shared::student::load_student(&state.pool, tenant, e.student_id).await?;
    crate::shared::guard::require_student_writable(&student)?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(e.institution_id)).await?;
    Ok((e, student, scope))
}

/// Adds a clearance item to an exit still in clearance.
pub async fn add_clearance(
    state: &StudentState,
    ctx: &AuthContext,
    id: StudentExitId,
    expected: Version,
    cmd: NewExitClearance,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    let (e, _student, scope) = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    if e.status != StudentExitStatus::ClearancePending {
        return Err(KernelError::Conflict(format!(
            "the exit is {}; clearance items can no longer be added",
            e.status
        )));
    }
    let finance = if cmd.item == ClearanceItem::Finance {
        finance_evidence(state, &scope, InstitutionId(e.institution_id), e.student_id).await?
    } else {
        FinanceEvidence::NotApplicable
    };
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(sutra_persistence::map_db_error)?;
    let locked: StudentExit = kit::lock(&mut tx, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    if locked.status != StudentExitStatus::ClearancePending {
        return Err(KernelError::Conflict(
            "the exit left clearance concurrently".into(),
        ));
    }
    let mut created = create_clearances(
        &mut tx,
        tenant,
        InstitutionId(e.institution_id),
        e.student_id,
        ClearanceOwner::Exit(id.as_uuid()),
        &[cmd.item],
        &finance,
    )
    .await?;
    if let (Some(note), Some(c)) = (cmd.note.as_deref(), created.first_mut()) {
        if c.note.is_none() {
            *c = kit::patch(
                &mut tx,
                CLEARANCE_TABLE,
                tenant,
                c.id.as_uuid(),
                json!({"note": note}),
            )
            .await?;
        }
    }
    let _: StudentExit = kit::patch(
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
        TABLE,
        "clearance_add",
        id.as_uuid(),
        json!({"item": cmd.item}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(sutra_persistence::map_db_error)?;
    get(state, tenant, id).await
}

/// Resolves one clearance item (If-Match = the exit's version).
#[allow(clippy::too_many_arguments)]
pub async fn resolve_clearance(
    state: &StudentState,
    ctx: &AuthContext,
    id: StudentExitId,
    clearance_id: ExitClearanceId,
    expected: Version,
    cmd: ResolveClearance,
    refresh_finance: bool,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    let (e, student, scope) = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    if e.status != StudentExitStatus::ClearancePending {
        return Err(KernelError::Conflict(format!(
            "the exit is {}; its clearance is closed",
            e.status
        )));
    }
    let items = clearances_of(&state.pool, tenant, ClearanceOwner::Exit(id.as_uuid())).await?;
    let item =
        items
            .iter()
            .find(|i| i.id == clearance_id)
            .cloned()
            .ok_or(KernelError::NotFound {
                entity: "exit clearance",
            })?;
    let (to, note) = if refresh_finance {
        if item.item != ClearanceItem::Finance {
            return Err(KernelError::Conflict(
                "only the finance item is refreshed from Finance".into(),
            ));
        }
        let ev =
            finance_evidence(state, &scope, InstitutionId(e.institution_id), e.student_id).await?;
        let (s, n) = finance_resolution(&ev);
        if matches!(
            item.status,
            ExitClearanceStatus::Cleared | ExitClearanceStatus::Waived
        ) {
            return Err(KernelError::Conflict(
                "the finance clearance is already resolved".into(),
            ));
        }
        (s, Some(n))
    } else {
        (check_resolution(&item, &cmd)?, cmd.note.clone())
    };
    let completes =
        all_cleared(
            items
                .iter()
                .map(|i| if i.id == clearance_id { &to } else { &i.status }),
        );
    let approval = if completes {
        Some(request_exit_approval(state, &scope, id.as_uuid(), e.version + 1, e.kind).await?)
    } else {
        None
    };
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(sutra_persistence::map_db_error)?;
    let locked: StudentExit = kit::lock(&mut tx, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    let current: ExitClearance = kit::lock(
        &mut tx,
        CLEARANCE_TABLE,
        "exit clearance",
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
    let bumped: StudentExit = kit::patch(
        &mut tx,
        TABLE,
        tenant,
        id.as_uuid(),
        json!({"status": locked.status.as_str()}),
    )
    .await?;
    kit::audit(&mut tx, &scope, CLEARANCE_TABLE, "resolve", clearance_id.as_uuid(),
        json!({"exitId": id, "item": item.item, "fromStatus": item.status, "toStatus": to, "note": note}), actor).await?;
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
    tx.commit().await.map_err(sutra_persistence::map_db_error)?;
    get(state, tenant, id).await
}

/// Explicit approval decision (`AwaitDecision`; `exit.decide`).
pub async fn decide(
    state: &StudentState,
    ctx: &AuthContext,
    id: StudentExitId,
    expected: Version,
    cmd: kit::LifecycleDecision,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    let (_e, student, scope) = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let approve = cmd.decision == kit::LifecycleDecisionValue::Approve;
    if !approve {
        kit::required_text("reason", cmd.reason.as_deref())?;
    }
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(sutra_persistence::map_db_error)?;
    let locked: StudentExit = kit::lock(&mut tx, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    if locked.status != StudentExitStatus::ApprovalPending {
        return Err(KernelError::Conflict(format!(
            "the exit is {}; there is no pending decision",
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
    tx.commit().await.map_err(sutra_persistence::map_db_error)?;
    get(state, tenant, id).await
}

/// Closes the context of an approved exit (and archives the persona on deceased closure).
pub async fn complete(
    state: &StudentState,
    ctx: &AuthContext,
    id: StudentExitId,
    expected: Version,
    cmd: CompleteExit,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    let (e, student, scope) = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(e.institution_id)).await?;
    let effective = cmd
        .effective_on
        .or(e.requested_effective_on)
        .unwrap_or(today);
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(sutra_persistence::map_db_error)?;
    let locked: StudentExit = kit::lock(&mut tx, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    if !exit_can_transition(locked.status, StudentExitStatus::Completed) {
        return Err(kit::illegal(
            "the exit",
            locked.status,
            StudentExitStatus::Completed,
        ));
    }
    let context = lock_context(&mut tx, tenant, locked.student_context_id).await?;
    let change = StatusChange {
        cause_event_type: STUDENT_EXIT_COMPLETED,
        reason: Some(&locked.reason),
        process_instance_id: locked.process_instance_id.as_deref(),
        closed_on: Some(effective),
    };
    change_context_status(
        &mut tx,
        &context,
        terminal_context_status(locked.kind),
        &change,
        actor,
    )
    .await?;
    let done: StudentExit = kit::patch(
        &mut tx,
        TABLE,
        tenant,
        id.as_uuid(),
        json!({
            "status": StudentExitStatus::Completed.as_str(), "effective_on": effective,
        }),
    )
    .await?;
    if locked.kind == StudentExitKind::Deceased {
        let persona = lock_student(&mut tx, tenant, locked.student_id).await?;
        if persona.status != StudentStatus::Archived {
            change_student_status(
                &mut tx,
                &persona,
                context.institution_id,
                StudentStatus::Archived,
                &change,
                actor,
            )
            .await?;
        }
    }
    exit_event(
        &mut tx,
        &scope,
        (STUDENT_EXIT_COMPLETED, STUDENT_EXIT_COMPLETED_SCHEMA),
        &done,
        Some(&student),
        actor,
    )
    .await?;
    kit::audit(&mut tx, &scope, TABLE, "complete", id.as_uuid(),
        json!({"kind": locked.kind, "effectiveOn": effective, "contextStatus": terminal_context_status(locked.kind)}), actor).await?;
    tx.commit().await.map_err(sutra_persistence::map_db_error)?;
    get(state, tenant, id).await
}

/// Cancels an exit that has not completed.
pub async fn cancel(
    state: &StudentState,
    ctx: &AuthContext,
    id: StudentExitId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<StudentExit, KernelError> {
    let (_e, student, scope) = load_for_write(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = state
        .pool
        .begin()
        .await
        .map_err(sutra_persistence::map_db_error)?;
    let locked: StudentExit = kit::lock(&mut tx, TABLE, ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    if !exit_can_transition(locked.status, StudentExitStatus::Cancelled) {
        return Err(kit::illegal(
            "the exit",
            locked.status,
            StudentExitStatus::Cancelled,
        ));
    }
    let done: StudentExit = kit::patch(
        &mut tx,
        TABLE,
        tenant,
        id.as_uuid(),
        json!({
            "status": StudentExitStatus::Cancelled.as_str(), "status_reason": reason,
        }),
    )
    .await?;
    exit_event(
        &mut tx,
        &scope,
        (STUDENT_EXIT_CANCELLED, STUDENT_EXIT_CANCELLED_SCHEMA),
        &done,
        Some(&student),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "cancel",
        id.as_uuid(),
        json!({"fromStatus": locked.status, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(sutra_persistence::map_db_error)?;
    get(state, tenant, id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finance_evidence_maps_to_clearance() {
        assert_eq!(
            finance_resolution(&FinanceEvidence::NotApplicable).0,
            ExitClearanceStatus::Cleared
        );
        assert_eq!(
            finance_resolution(&FinanceEvidence::Outstanding {
                detail: "fees".into()
            })
            .0,
            ExitClearanceStatus::Blocked
        );
        assert_eq!(clearance_items(None), DEFAULT_CLEARANCES.to_vec());
        assert_eq!(
            clearance_items(Some(&[ClearanceItem::Library, ClearanceItem::Library])),
            vec![ClearanceItem::Library]
        );
        assert!(all_cleared(&[
            ExitClearanceStatus::Cleared,
            ExitClearanceStatus::Waived
        ]));
        assert!(!all_cleared(&[
            ExitClearanceStatus::Cleared,
            ExitClearanceStatus::Blocked
        ]));
    }
}
