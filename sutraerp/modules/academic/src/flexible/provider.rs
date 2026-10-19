//! Learning provider onboarding → governed active provider (L3-D-044): uniqueness, accreditation
//! evidence (referenced, never re-owned), approved scope (modes), readiness before review, the
//! `LEARNING_PROVIDER_ONBOARDING` approval, suspension (no new enrolments) and offboarding.

use super::domain::provider_can_transition;
use super::kit::{
    self, begin, check_version, commit, history, illegal, invalid, require, required_text,
    row_scope, FlexibleHistoryEntry, StatusEvent,
};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::flexible::*;
use sutra_contracts::academic::status::{LearningMode, LearningProviderStatus as P};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "learning_provider";
const ENTITY: &str = "learning provider";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LearningProvider {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub name: String,
    /// `mooc_platform` | `government_platform` | `university` | `industry` | `other`.
    pub provider_kind: String,
    pub accreditation_ref: Option<String>,
    pub accreditation_valid_until: Option<NaiveDate>,
    pub website: Option<String>,
    pub contact_email: Option<String>,
    /// Approved institutional scope.
    pub modes: Vec<LearningMode>,
    pub scope_note: Option<String>,
    pub status: P,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub history: Vec<FlexibleHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct LearningProviderList {
    pub items: Vec<LearningProvider>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewLearningProvider {
    pub code: String,
    pub name: String,
    pub provider_kind: String,
    pub accreditation_ref: Option<String>,
    pub accreditation_valid_until: Option<NaiveDate>,
    pub website: Option<String>,
    pub contact_email: Option<String>,
    #[serde(default)]
    pub modes: Vec<LearningMode>,
    pub scope_note: Option<String>,
}

const KINDS: &[&str] = &[
    "mooc_platform",
    "government_platform",
    "university",
    "industry",
    "other",
];

pub async fn list(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<LearningProvider>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "institution_id",
        institution,
        status,
        "t.code",
        500,
    )
    .await
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<LearningProvider, KernelError> {
    let mut p: LearningProvider = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    p.history = kit::history_of(&st.pool, tenant, id).await?;
    Ok(p)
}

pub async fn create(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: Uuid,
    cmd: NewLearningProvider,
    actor: &Actor,
) -> Result<LearningProvider, KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution));
    require_institution_writable(st, &scope, InstitutionId(institution)).await?;
    let code = required_text("code", Some(&cmd.code))?;
    let name = required_text("name", Some(&cmd.name))?;
    if !KINDS.contains(&cmd.provider_kind.as_str()) {
        return Err(invalid(
            "providerKind",
            "invalid",
            format!("providerKind must be one of {}", KINDS.join(", ")),
        ));
    }
    if cmd.modes.contains(&LearningMode::OpenElective) {
        return Err(invalid(
            "modes",
            "invalid",
            "open electives are internal offerings, not provider modes",
        ));
    }
    // BH-001: provider uniqueness within the institution.
    let dup: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM academic.learning_provider WHERE tenant_id = $1 \
         AND institution_id = $2 AND lower(code) = lower($3) AND status <> 'offboarded')",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(institution)
    .bind(&code)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    require(
        !dup,
        format!("a learning provider with code {code} already exists"),
    )?;
    let mut tx = begin(st).await?;
    let p: LearningProvider = kit::insert(
        &mut tx,
        TABLE,
        json!({
            "tenant_id": ctx.scope.tenant_id.as_uuid(), "institution_id": institution, "code": code,
            "name": name, "provider_kind": cmd.provider_kind,
            "accreditation_ref": kit::opt_text(cmd.accreditation_ref.as_deref()),
            "accreditation_valid_until": cmd.accreditation_valid_until,
            "website": kit::opt_text(cmd.website.as_deref()),
            "contact_email": kit::opt_text(cmd.contact_email.as_deref()),
            "modes": cmd.modes.iter().map(|m| m.as_str()).collect::<Vec<_>>(),
            "scope_note": kit::opt_text(cmd.scope_note.as_deref()),
        }),
    )
    .await?;
    history(
        &mut tx,
        ctx.scope.tenant_id,
        TABLE,
        p.id,
        None,
        "proposed",
        None,
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "create",
        p.id,
        json!({"code": p.code}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, ctx.scope.tenant_id, p.id).await
}

/// Moves a locked provider, records history + audit and the lifecycle event when there is one.
#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    p: &LearningProvider,
    to: P,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<LearningProvider, KernelError> {
    if !provider_can_transition(p.status, to) {
        return Err(illegal(ENTITY, p.status, to));
    }
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": kit::now()});
    if let Some(pid) = pid {
        changes["process_instance_id"] = json!(pid);
    }
    let n: LearningProvider = kit::patch(conn, TABLE, tenant, p.id, changes).await?;
    history(
        conn,
        tenant,
        TABLE,
        p.id,
        Some(p.status.as_str()),
        to.as_str(),
        reason,
        pid,
        actor,
    )
    .await?;
    kit::audit(
        conn,
        scope,
        TABLE,
        to.as_str(),
        p.id,
        json!({"from": p.status, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    let ev = match to {
        P::Active => Some((
            LEARNING_PROVIDER_ACTIVATED,
            LEARNING_PROVIDER_ACTIVATED_SCHEMA,
        )),
        P::Suspended => Some((
            LEARNING_PROVIDER_SUSPENDED,
            LEARNING_PROVIDER_SUSPENDED_SCHEMA,
        )),
        _ => None,
    };
    if let Some(ev) = ev {
        StatusEvent {
            aggregate_id: p.id,
            institution_id: p.institution_id,
            from: Some(p.status.as_str()),
            to: to.as_str(),
            version: n.version,
            reason,
            process_instance_id: pid.or(n.process_instance_id.as_deref()),
            ..Default::default()
        }
        .emit(conn, scope, ev, actor)
        .await?;
    }
    Ok(n)
}

/// BH-004/005: readiness before review / activation.
async fn require_ready(
    st: &AcademicState,
    scope: &ExecutionScope,
    p: &LearningProvider,
) -> Result<(), KernelError> {
    let today = business_today(st, scope, InstitutionId(p.institution_id)).await?;
    if p.accreditation_ref
        .as_deref()
        .is_none_or(|s| s.trim().is_empty())
    {
        return Err(invalid(
            "accreditationRef",
            "required",
            "approval/accreditation evidence is required before review",
        ));
    }
    match p.accreditation_valid_until {
        Some(d) if d >= today => {}
        _ => {
            return Err(KernelError::Conflict(
                "the provider's accreditation evidence is missing or expired".into(),
            ))
        }
    }
    if p.modes.is_empty() {
        return Err(invalid(
            "modes",
            "required",
            "the approved institutional scope (modes) is required",
        ));
    }
    Ok(())
}

async fn load_for_command(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
) -> Result<(LearningProvider, ExecutionScope), KernelError> {
    let p: LearningProvider = kit::fetch(&st.pool, TABLE, ENTITY, ctx.scope.tenant_id, id).await?;
    check_version(p.version, expected)?;
    let scope = row_scope(ctx, p.institution_id);
    require_institution_writable(st, &scope, InstitutionId(p.institution_id)).await?;
    Ok((p, scope))
}

/// Submit for onboarding review (`LEARNING_PROVIDER_ONBOARDING`); auto-approval activates.
pub async fn submit(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<LearningProvider, KernelError> {
    let (p, scope) = load_for_command(st, ctx, id, expected).await?;
    if !provider_can_transition(p.status, P::UnderReview) {
        return Err(illegal(ENTITY, p.status, P::UnderReview));
    }
    require_ready(st, &scope, &p).await?;
    let mut vars = BTreeMap::new();
    vars.insert("providerCode".into(), json!(p.code));
    let outcome = approval::request_approval(
        st,
        &scope,
        process::LEARNING_PROVIDER_ONBOARDING,
        business_key(TABLE, p.id, p.version),
        vars,
    )
    .await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: LearningProvider = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let pid = outcome.process_instance_id().to_owned();
    let n = transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        P::UnderReview,
        None,
        Some(&pid),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(actor.correlation_id.clone());
        transition(
            &mut tx,
            &scope,
            tenant,
            &n,
            P::Active,
            Some("onboarding approved"),
            Some(&pid),
            &sys,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Explicit onboarding decision (`AwaitDecision`): approve → active, reject → proposed.
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<LearningProvider, KernelError> {
    let (p, scope) = load_for_command(st, ctx, id, expected).await?;
    require(
        p.status == P::UnderReview,
        format!(
            "the learning provider is {}; there is no pending onboarding decision",
            p.status
        ),
    )?;
    if approve {
        require_ready(st, &scope, &p).await?;
    } else if reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: LearningProvider = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let to = if approve { P::Active } else { P::Proposed };
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        to,
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Suspend (reason required; BH-008 no new enrolments), reactivate (readiness re-checked) or
/// offboard (blocked while live opportunities use the provider).
pub async fn command(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: P,
    reason: Option<String>,
    actor: &Actor,
) -> Result<LearningProvider, KernelError> {
    let (p, scope) = load_for_command(st, ctx, id, expected).await?;
    if !provider_can_transition(p.status, to) {
        return Err(illegal(ENTITY, p.status, to));
    }
    match to {
        P::Suspended | P::Offboarded if reason.is_none() => {
            return Err(invalid(
                "reason",
                "required",
                format!("a reason is required to make the provider {to}"),
            ));
        }
        P::Active => require_ready(st, &scope, &p).await?,
        _ => {}
    }
    if to == P::Offboarded {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM academic.learning_opportunity WHERE tenant_id = $1 AND provider_id = $2 \
             AND status IN ('pending_approval', 'active', 'suspended')",
        )
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .fetch_one(&st.pool)
        .await
        .map_err(map_db_error)?;
        require(
            live == 0,
            format!(
                "{live} live learning opportunities still use this provider; archive them first"
            ),
        )?;
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: LearningProvider = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        to,
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}
