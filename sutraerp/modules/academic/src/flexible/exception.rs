//! Flexible-learning exception case → governed closure (L3-D-050): classified case with an
//! accountable owner; corrective action only after an approved plan (`FLEXIBLE_EXCEPTION`,
//! overrides need a justification); a failed correction keeps the case open; holds keep it
//! open; closure only with resolution evidence; the original failure history is never deleted;
//! a repeated authoritative failure fact (`sourceKey`) returns the existing case.

use super::domain::exception_can_transition;
use super::kit::{
    self, begin, check_version, commit, history, illegal, invalid, require, row_scope,
    FlexibleHistoryEntry,
};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::institution_scope;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "flexible_exception_case";
const ENTITY: &str = "flexible exception case";
const CATEGORIES: &[&str] = &[
    "allocation",
    "registration",
    "completion",
    "certificate",
    "recognition",
    "reconciliation",
    "other",
];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlexibleExceptionCase {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub opportunity_id: Option<Uuid>,
    pub enrolment_id: Option<Uuid>,
    pub category: String,
    pub title: String,
    pub description: Option<String>,
    pub root_cause: Option<String>,
    pub owner_subject: String,
    pub corrective_plan: Option<String>,
    pub override_required: bool,
    pub override_justification: Option<String>,
    pub corrective_action: Option<String>,
    pub resolution_evidence: Option<String>,
    pub on_hold: bool,
    pub hold_reason: Option<String>,
    /// `open` | `plan_pending` | `plan_approved` | `resolved` | `closed`.
    pub status: String,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub source_key: Option<String>,
    pub process_instance_id: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub history: Vec<FlexibleHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FlexibleExceptionCaseList {
    pub items: Vec<FlexibleExceptionCase>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewFlexibleExceptionCase {
    pub category: String,
    pub title: String,
    pub description: Option<String>,
    pub root_cause: Option<String>,
    /// Defaults to the caller.
    pub owner_subject: Option<String>,
    pub opportunity_id: Option<Uuid>,
    pub enrolment_id: Option<Uuid>,
    pub source_key: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionPlan {
    pub corrective_plan: String,
    #[serde(default)]
    pub override_required: bool,
    pub override_justification: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionResolution {
    pub corrective_action: String,
    pub resolution_evidence: String,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExceptionHold {
    pub on_hold: bool,
    pub reason: Option<String>,
}

pub async fn list(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<FlexibleExceptionCase>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "institution_id",
        institution,
        status,
        "t.created_at DESC",
        200,
    )
    .await
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<FlexibleExceptionCase, KernelError> {
    let mut c: FlexibleExceptionCase = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    c.history = kit::history_of(&st.pool, tenant, id).await?;
    Ok(c)
}

/// Raises a case inside an existing transaction (used by reconciliation escalation too).
/// Returns the case id and whether it is new.
pub async fn raise_in(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    institution: Uuid,
    cmd: &NewFlexibleExceptionCase,
    owner: &str,
    actor: &Actor,
) -> Result<(Uuid, bool), KernelError> {
    let key = kit::opt_text(cmd.source_key.as_deref());
    if let Some(k) = &key {
        let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM academic.flexible_exception_case WHERE tenant_id = $1 AND source_key = $2")
            .bind(tenant.as_uuid())
            .bind(k)
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
        if let Some(id) = existing {
            return Ok((id, false));
        }
    }
    let c: FlexibleExceptionCase = kit::insert(conn, TABLE, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": institution, "opportunity_id": cmd.opportunity_id,
        "enrolment_id": cmd.enrolment_id, "category": cmd.category, "title": cmd.title.trim(),
        "description": kit::opt_text(cmd.description.as_deref()), "root_cause": kit::opt_text(cmd.root_cause.as_deref()),
        "owner_subject": owner, "source_key": key,
    })).await?;
    history(conn, tenant, TABLE, c.id, None, "open", None, None, actor).await?;
    kit::audit(
        conn,
        scope,
        TABLE,
        "raise",
        c.id,
        json!({"category": c.category}),
        actor,
    )
    .await?;
    Ok((c.id, true))
}

pub async fn raise(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: Uuid,
    cmd: NewFlexibleExceptionCase,
    actor: &Actor,
) -> Result<(FlexibleExceptionCase, bool), KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution));
    require_institution_writable(st, &scope, InstitutionId(institution)).await?;
    if !CATEGORIES.contains(&cmd.category.as_str()) {
        return Err(invalid(
            "category",
            "invalid",
            format!("category must be one of {}", CATEGORIES.join(", ")),
        ));
    }
    kit::required_text("title", Some(&cmd.title))?;
    let tenant = ctx.scope.tenant_id;
    for (field, table, id) in [
        ("opportunityId", "learning_opportunity", cmd.opportunity_id),
        ("enrolmentId", "flexible_enrolment", cmd.enrolment_id),
    ] {
        if let Some(id) = id {
            let inst: Option<Uuid> = sqlx::query_scalar(&format!(
                "SELECT institution_id FROM academic.{table} WHERE id = $1 AND tenant_id = $2"
            ))
            .bind(id)
            .bind(tenant.as_uuid())
            .fetch_optional(&st.pool)
            .await
            .map_err(map_db_error)?;
            if inst != Some(institution) {
                return Err(invalid(
                    field,
                    "not_found",
                    format!("{field} not found in this institution"),
                ));
            }
        }
    }
    let owner = kit::opt_text(cmd.owner_subject.as_deref()).unwrap_or_else(|| ctx.subject.clone());
    let mut tx = begin(st).await?;
    let (id, new) = raise_in(&mut tx, &scope, tenant, institution, &cmd, &owner, actor).await?;
    commit(tx).await?;
    Ok((get(st, tenant, id).await?, new))
}

#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    c: &FlexibleExceptionCase,
    to: &str,
    reason: Option<&str>,
    pid: Option<&str>,
    extra: Value,
    actor: &Actor,
) -> Result<FlexibleExceptionCase, KernelError> {
    if !exception_can_transition(&c.status, to) {
        return Err(illegal(ENTITY, &c.status, to));
    }
    let mut changes =
        json!({"status": to, "status_reason": reason, "status_changed_at": kit::now()});
    if let Some(pid) = pid {
        changes["process_instance_id"] = json!(pid);
    }
    if to == "closed" {
        changes["closed_at"] = json!(kit::now());
    }
    if let (Value::Object(m), Value::Object(x)) = (&mut changes, extra) {
        m.extend(x);
    }
    let n: FlexibleExceptionCase = kit::patch(conn, TABLE, tenant, c.id, changes).await?;
    history(
        conn,
        tenant,
        TABLE,
        c.id,
        Some(&c.status),
        to,
        reason,
        pid,
        actor,
    )
    .await?;
    kit::audit(
        conn,
        scope,
        TABLE,
        to,
        c.id,
        json!({"from": c.status, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    Ok(n)
}

async fn load(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
) -> Result<(FlexibleExceptionCase, ExecutionScope), KernelError> {
    let c: FlexibleExceptionCase =
        kit::fetch(&st.pool, TABLE, ENTITY, ctx.scope.tenant_id, id).await?;
    check_version(c.version, expected)?;
    let scope = row_scope(ctx, c.institution_id);
    require_institution_writable(st, &scope, InstitutionId(c.institution_id)).await?;
    Ok((c, scope))
}

/// Proposes the corrective / recovery plan (`FLEXIBLE_EXCEPTION`); auto-approval approves it.
pub async fn plan(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: ExceptionPlan,
    actor: &Actor,
) -> Result<FlexibleExceptionCase, KernelError> {
    let (c, scope) = load(st, ctx, id, expected).await?;
    if !exception_can_transition(&c.status, "plan_pending") {
        return Err(illegal(ENTITY, &c.status, "plan_pending"));
    }
    let plan = kit::required_text("correctivePlan", Some(&cmd.corrective_plan))?;
    let justification = kit::opt_text(cmd.override_justification.as_deref());
    if cmd.override_required && justification.is_none() {
        return Err(invalid(
            "overrideJustification",
            "required",
            "an override needs a prior justification and scope",
        ));
    }
    let mut vars = BTreeMap::new();
    vars.insert("category".into(), json!(c.category));
    vars.insert("overrideRequired".into(), json!(cmd.override_required));
    let outcome = approval::request_approval(
        st,
        &scope,
        process::FLEXIBLE_EXCEPTION,
        business_key(TABLE, c.id, c.version),
        vars,
    )
    .await?;
    let tenant = ctx.scope.tenant_id;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = begin(st).await?;
    let locked: FlexibleExceptionCase = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let n = transition(&mut tx, &scope, tenant, &locked, "plan_pending", None, Some(&pid),
        json!({"corrective_plan": plan, "override_required": cmd.override_required, "override_justification": justification}), actor).await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(actor.correlation_id.clone());
        transition(
            &mut tx,
            &scope,
            tenant,
            &n,
            "plan_approved",
            Some("corrective plan approved"),
            Some(&pid),
            json!({}),
            &sys,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Explicit plan decision: approve → plan_approved, reject → open.
#[allow(clippy::too_many_arguments)]
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FlexibleExceptionCase, KernelError> {
    let (c, scope) = load(st, ctx, id, expected).await?;
    require(
        c.status == "plan_pending",
        format!(
            "the exception case is {}; there is no pending plan decision",
            c.status
        ),
    )?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleExceptionCase = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        if approve { "plan_approved" } else { "open" },
        reason.as_deref(),
        None,
        json!({}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Records the completed corrective action with its evidence (needs the approved plan).
pub async fn resolve(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: ExceptionResolution,
    actor: &Actor,
) -> Result<FlexibleExceptionCase, KernelError> {
    let (c, scope) = load(st, ctx, id, expected).await?;
    require(
        c.status == "plan_approved",
        format!(
            "the exception case is {}; corrective action needs an approved plan",
            c.status
        ),
    )?;
    let action = kit::required_text("correctiveAction", Some(&cmd.corrective_action))?;
    let evidence = kit::required_text("resolutionEvidence", Some(&cmd.resolution_evidence))?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleExceptionCase = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        "resolved",
        None,
        None,
        json!({"corrective_action": action, "resolution_evidence": evidence}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// A failed correction: back to the approved plan, evidence of the attempt kept in history.
pub async fn reopen_correction(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FlexibleExceptionCase, KernelError> {
    let (_, scope) = load(st, ctx, id, expected).await?;
    let reason =
        reason.ok_or_else(|| invalid("reason", "required", "say why the correction failed"))?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleExceptionCase = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        "plan_approved",
        Some(&reason),
        None,
        json!({}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Places or lifts an investigation / regulatory hold (BH-006).
pub async fn hold(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: ExceptionHold,
    actor: &Actor,
) -> Result<FlexibleExceptionCase, KernelError> {
    let (c, scope) = load(st, ctx, id, expected).await?;
    require(
        c.status != "closed",
        "a closed exception case cannot be held",
    )?;
    let reason = kit::opt_text(cmd.reason.as_deref());
    if cmd.on_hold && reason.is_none() {
        return Err(invalid("reason", "required", "a hold needs a reason"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleExceptionCase = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    kit::patch::<FlexibleExceptionCase>(
        &mut tx,
        TABLE,
        tenant,
        id,
        json!({"on_hold": cmd.on_hold, "hold_reason": reason}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        if cmd.on_hold { "hold" } else { "release_hold" },
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Governed closure: resolved, evidence present, no hold.
pub async fn close(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FlexibleExceptionCase, KernelError> {
    let (c, scope) = load(st, ctx, id, expected).await?;
    require(
        !c.on_hold,
        "the exception case is on hold; it stays open until the hold is lifted",
    )?;
    require(
        c.status == "resolved",
        format!(
            "the exception case is {}; closure needs a resolved case",
            c.status
        ),
    )?;
    require(
        c.resolution_evidence
            .as_deref()
            .is_some_and(|s| !s.trim().is_empty()),
        "closure needs complete resolution evidence",
    )?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleExceptionCase = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        "closed",
        reason.as_deref(),
        None,
        json!({}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}
