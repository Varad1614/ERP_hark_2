//! Flexible-learning policy and approval configuration (L3-D-045): distinct versions with an
//! effective period, eligibility / enrolment / withdrawal / waitlist rules and the approval path,
//! publication through `FLEXIBLE_LEARNING_POLICY`, prior versions retained (superseded), no
//! retroactive rewrite (each enrolment keeps the policy version it was decided under).

use super::domain::{version_can_transition, FlexiblePolicyRules};
use super::kit::{
    self, begin, check_version, commit, history, illegal, invalid, require, row_scope,
    FlexibleHistoryEntry, StatusEvent,
};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::flexible::*;
use sutra_contracts::academic::status::PolicyVersionStatus as V;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "flexible_learning_policy";
const ENTITY: &str = "flexible-learning policy";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlexibleLearningPolicy {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub name: String,
    pub version_no: i32,
    pub rules: FlexiblePolicyRules,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub status: V,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub supersedes_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub history: Vec<FlexibleHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FlexibleLearningPolicyList {
    pub items: Vec<FlexibleLearningPolicy>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewFlexibleLearningPolicy {
    pub name: String,
    pub rules: FlexiblePolicyRules,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
}

pub async fn list(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<FlexibleLearningPolicy>, KernelError> {
    kit::list_by(
        &st.pool,
        TABLE,
        tenant,
        "institution_id",
        institution,
        status,
        "t.version_no DESC",
        200,
    )
    .await
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<FlexibleLearningPolicy, KernelError> {
    let mut p: FlexibleLearningPolicy = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    p.history = kit::history_of(&st.pool, tenant, id).await?;
    Ok(p)
}

/// The policy version in force for an institution (`effective`), if any.
pub async fn effective(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Uuid,
) -> Result<Option<FlexibleLearningPolicy>, KernelError> {
    let v: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.flexible_learning_policy t WHERE t.tenant_id = $1 \
         AND t.institution_id = $2 AND t.status = 'effective'",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    v.map(kit::decode).transpose()
}

/// Rules of a policy version (any status).
pub async fn rules_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Option<Uuid>,
) -> Result<Option<FlexiblePolicyRules>, KernelError> {
    let Some(id) = id else { return Ok(None) };
    let p: FlexibleLearningPolicy = kit::fetch(&mut *conn, TABLE, ENTITY, tenant, id).await?;
    Ok(Some(p.rules))
}

fn validate(
    rules: &FlexiblePolicyRules,
    from: NaiveDate,
    to: Option<NaiveDate>,
    today: NaiveDate,
) -> Result<(), KernelError> {
    let mut v: Vec<FieldViolation> = rules
        .problems()
        .into_iter()
        .map(|(f, m)| FieldViolation::new(f, "invalid", m))
        .collect();
    if from < today {
        v.push(FieldViolation::new(
            "effectiveFrom",
            "past",
            "a new policy version cannot take effect in the past",
        ));
    }
    if to.is_some_and(|t| t < from) {
        v.push(FieldViolation::new(
            "effectiveTo",
            "invalid",
            "effectiveTo must not be before effectiveFrom",
        ));
    }
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

pub async fn create(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: Uuid,
    cmd: NewFlexibleLearningPolicy,
    actor: &Actor,
) -> Result<FlexibleLearningPolicy, KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution));
    require_institution_writable(st, &scope, InstitutionId(institution)).await?;
    let name = kit::required_text("name", Some(&cmd.name))?;
    let today = business_today(st, &scope, InstitutionId(institution)).await?;
    validate(&cmd.rules, cmd.effective_from, cmd.effective_to, today)?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    // serialise version numbering per institution
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('flexible_learning_policy:' || $1::text))")
        .bind(institution)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM academic.flexible_learning_policy WHERE tenant_id = $1 AND institution_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let p: FlexibleLearningPolicy = kit::insert(&mut tx, TABLE, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": institution, "name": name, "version_no": next,
        "rules": cmd.rules, "effective_from": cmd.effective_from, "effective_to": cmd.effective_to,
    })).await?;
    history(
        &mut tx, tenant, TABLE, p.id, None, "draft", None, None, actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "create",
        p.id,
        json!({"versionNo": next}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, p.id).await
}

#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    p: &FlexibleLearningPolicy,
    to: V,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<FlexibleLearningPolicy, KernelError> {
    if !version_can_transition(p.status, to) {
        return Err(illegal(ENTITY, p.status, to));
    }
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": kit::now()});
    if let Some(pid) = pid {
        changes["process_instance_id"] = json!(pid);
    }
    if to == V::Effective {
        // BH-007: publish the new version, keep the predecessor as superseded history.
        let prior: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM academic.flexible_learning_policy WHERE tenant_id = $1 AND institution_id = $2 AND status = 'effective' FOR UPDATE",
        )
        .bind(tenant.as_uuid())
        .bind(p.institution_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if let Some(prior) = prior {
            let old: FlexibleLearningPolicy = kit::lock(conn, TABLE, ENTITY, tenant, prior).await?;
            let end = (p.effective_from - Duration::days(1)).max(old.effective_from);
            kit::patch::<FlexibleLearningPolicy>(conn, TABLE, tenant, prior, json!({
                "status": "superseded", "status_reason": format!("superseded by version {}", p.version_no),
                "status_changed_at": kit::now(), "effective_to": old.effective_to.map_or(end, |e| e.min(end)),
            })).await?;
            history(
                conn,
                tenant,
                TABLE,
                prior,
                Some("effective"),
                "superseded",
                Some("superseded by a new version"),
                pid,
                actor,
            )
            .await?;
            changes["supersedes_id"] = json!(prior);
        }
    }
    let n: FlexibleLearningPolicy = kit::patch(conn, TABLE, tenant, p.id, changes).await?;
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
    if to == V::Effective {
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
        .emit(
            conn,
            scope,
            (
                FLEXIBLE_LEARNING_POLICY_EFFECTIVE,
                FLEXIBLE_LEARNING_POLICY_EFFECTIVE_SCHEMA,
            ),
            actor,
        )
        .await?;
    }
    Ok(n)
}

async fn load(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
) -> Result<(FlexibleLearningPolicy, ExecutionScope), KernelError> {
    let p: FlexibleLearningPolicy =
        kit::fetch(&st.pool, TABLE, ENTITY, ctx.scope.tenant_id, id).await?;
    check_version(p.version, expected)?;
    let scope = row_scope(ctx, p.institution_id);
    require_institution_writable(st, &scope, InstitutionId(p.institution_id)).await?;
    Ok((p, scope))
}

/// Submit for publication (`FLEXIBLE_LEARNING_POLICY`); auto-approval makes it effective.
pub async fn submit(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<FlexibleLearningPolicy, KernelError> {
    let (p, scope) = load(st, ctx, id, expected).await?;
    if !version_can_transition(p.status, V::InReview) {
        return Err(illegal(ENTITY, p.status, V::InReview));
    }
    let today = business_today(st, &scope, InstitutionId(p.institution_id)).await?;
    validate(&p.rules, p.effective_from, p.effective_to, today)?;
    let mut vars = BTreeMap::new();
    vars.insert("versionNo".into(), json!(p.version_no));
    let outcome = approval::request_approval(
        st,
        &scope,
        process::FLEXIBLE_LEARNING_POLICY,
        business_key(TABLE, p.id, p.version),
        vars,
    )
    .await?;
    let tenant = ctx.scope.tenant_id;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = begin(st).await?;
    let locked: FlexibleLearningPolicy = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let n = transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        V::InReview,
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
            V::Effective,
            Some("publication approved"),
            Some(&pid),
            &sys,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Explicit publication decision: approve → effective (supersedes the prior version), reject → draft.
#[allow(clippy::too_many_arguments)]
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FlexibleLearningPolicy, KernelError> {
    let (p, scope) = load(st, ctx, id, expected).await?;
    require(
        p.status == V::InReview,
        format!(
            "the policy version is {}; there is no pending publication decision",
            p.status
        ),
    )?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleLearningPolicy = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        if approve { V::Effective } else { V::Draft },
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Retire a draft or effective version (history kept).
pub async fn retire(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FlexibleLearningPolicy, KernelError> {
    let (_, scope) = load(st, ctx, id, expected).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleLearningPolicy = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        V::Retired,
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}
