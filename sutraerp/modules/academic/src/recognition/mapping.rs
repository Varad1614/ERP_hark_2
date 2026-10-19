//! Credit mapping rule configuration (L3-D-046): a distinct mapping version per opportunity
//! against authoritative Section B curriculum definitions (read through the reference port,
//! never re-owned), equivalency + credits + effective period, duplicate / curriculum / credit
//! limit validation, publication through `CREDIT_MAPPING_RULE`, predecessors kept (superseded),
//! completed recognitions never altered.

use crate::flexible::domain::version_can_transition;
use crate::flexible::kit::{
    self, begin, check_version, commit, history, illegal, invalid, require, row_scope,
    FlexibleHistoryEntry, StatusEvent,
};
use crate::flexible::opportunity::{self, LearningOpportunity};
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
use sutra_contracts::academic::status::{LearningOpportunityStatus, PolicyVersionStatus as V};
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "credit_mapping_rule";
const ENTITY: &str = "credit mapping rule";
const EQUIVALENCIES: &[&str] = &[
    "component_equivalent",
    "elective_credit",
    "additional_credit",
];

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreditMappingRule {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub opportunity_id: Uuid,
    pub curriculum_version_id: Option<Uuid>,
    pub curriculum_component_id: Option<Uuid>,
    /// `component_equivalent` | `elective_credit` | `additional_credit`.
    pub equivalency: String,
    pub credits: f64,
    pub version_no: i32,
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
pub struct CreditMappingRuleList {
    pub items: Vec<CreditMappingRule>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewCreditMappingRule {
    pub opportunity_id: Uuid,
    pub curriculum_version_id: Option<Uuid>,
    pub curriculum_component_id: Option<Uuid>,
    pub equivalency: String,
    pub credits: f64,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct MappingFilter {
    pub status: Option<V>,
    pub opportunity_id: Option<Uuid>,
}

pub async fn list(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    f: &MappingFilter,
) -> Result<Vec<CreditMappingRule>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.credit_mapping_rule t WHERE t.tenant_id = $1 AND t.institution_id = $2 \
         AND ($3::text IS NULL OR t.status = $3) AND ($4::uuid IS NULL OR t.opportunity_id = $4) \
         ORDER BY t.opportunity_id, t.version_no DESC LIMIT 500",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(f.status.map(|s| s.as_str()))
    .bind(f.opportunity_id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<CreditMappingRule, KernelError> {
    let mut r: CreditMappingRule = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    r.history = kit::history_of(&st.pool, tenant, id).await?;
    Ok(r)
}

/// The effective mapping rule of an opportunity, if any.
pub async fn effective_for(
    conn: &mut PgConnection,
    opportunity: Uuid,
) -> Result<Option<CreditMappingRule>, KernelError> {
    let v: Option<serde_json::Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM academic.credit_mapping_rule t WHERE t.opportunity_id = $1 AND t.status = 'effective'")
        .bind(opportunity)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    v.map(kit::decode).transpose()
}

/// BH-002/004: Section B definitions, credit limits, dates.
#[allow(clippy::too_many_arguments)]
async fn validate(
    st: &AcademicState,
    scope: &ExecutionScope,
    o: &LearningOpportunity,
    version_id: Option<Uuid>,
    component_id: Option<Uuid>,
    equivalency: &str,
    credits: f64,
    from: NaiveDate,
    to: Option<NaiveDate>,
) -> Result<(), KernelError> {
    if !EQUIVALENCIES.contains(&equivalency) {
        return Err(invalid(
            "equivalency",
            "invalid",
            format!("equivalency must be one of {}", EQUIVALENCIES.join(", ")),
        ));
    }
    if !(credits > 0.0 && credits <= o.credits.max(0.0)) {
        return Err(invalid(
            "credits",
            "limit",
            format!(
                "credits must be above 0 and at most the opportunity's {} credits",
                o.credits
            ),
        ));
    }
    if to.is_some_and(|t| t < from) {
        return Err(invalid(
            "effectiveTo",
            "invalid",
            "effectiveTo must not be before effectiveFrom",
        ));
    }
    if equivalency == "component_equivalent" && component_id.is_none() {
        return Err(invalid(
            "curriculumComponentId",
            "required",
            "a component equivalence needs a curriculum component",
        ));
    }
    if let Some(cid) = component_id {
        let vid = version_id.ok_or_else(|| {
            invalid(
                "curriculumVersionId",
                "required",
                "the curriculum version of the component is required",
            )
        })?;
        let v = st
            .references
            .curriculum_version(scope, vid)
            .await?
            .filter(|v| v.institution_id.as_uuid() == o.institution_id)
            .ok_or_else(|| {
                invalid(
                    "curriculumVersionId",
                    "not_found",
                    "curriculum version not found in this institution",
                )
            })?;
        if v.status != PublicationStatus::Published {
            return Err(invalid(
                "curriculumVersionId",
                "unpublished",
                "the curriculum version is not published",
            ));
        }
        let comps = st.references.curriculum_components(scope, vid).await?;
        let c = comps.iter().find(|c| c.id == cid).ok_or_else(|| {
            invalid(
                "curriculumComponentId",
                "not_found",
                "the component is not part of this curriculum version",
            )
        })?;
        if credits > c.credits {
            return Err(invalid(
                "credits",
                "limit",
                format!("credits exceed the component's {} credits", c.credits),
            ));
        }
    }
    Ok(())
}

pub async fn create(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: Uuid,
    cmd: NewCreditMappingRule,
    actor: &Actor,
) -> Result<CreditMappingRule, KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution));
    require_institution_writable(st, &scope, InstitutionId(institution)).await?;
    let tenant = ctx.scope.tenant_id;
    let o: LearningOpportunity = kit::fetch(
        &st.pool,
        opportunity::TABLE,
        opportunity::ENTITY,
        tenant,
        cmd.opportunity_id,
    )
    .await
    .map_err(|_| {
        invalid(
            "opportunityId",
            "not_found",
            "learning opportunity not found",
        )
    })?;
    if o.institution_id != institution {
        return Err(invalid(
            "opportunityId",
            "not_found",
            "learning opportunity not found in this institution",
        ));
    }
    require(
        o.status != LearningOpportunityStatus::Archived,
        "the learning opportunity is archived",
    )?;
    validate(
        st,
        &scope,
        &o,
        cmd.curriculum_version_id,
        cmd.curriculum_component_id,
        &cmd.equivalency,
        cmd.credits,
        cmd.effective_from,
        cmd.effective_to,
    )
    .await?;
    let mut tx = begin(st).await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('credit_mapping_rule:' || $1::text))")
        .bind(o.id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let (next, pending): (i32, i64) = sqlx::query_as(
        "SELECT COALESCE(max(version_no), 0) + 1, count(*) FILTER (WHERE status IN ('draft', 'in_review')) \
         FROM academic.credit_mapping_rule WHERE opportunity_id = $1",
    )
    .bind(o.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    require(
        pending == 0,
        "a mapping version for this opportunity is already being prepared",
    )?;
    let r: CreditMappingRule = kit::insert(&mut tx, TABLE, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": institution, "opportunity_id": o.id,
        "curriculum_version_id": cmd.curriculum_version_id, "curriculum_component_id": cmd.curriculum_component_id,
        "equivalency": cmd.equivalency, "credits": cmd.credits, "version_no": next,
        "effective_from": cmd.effective_from, "effective_to": cmd.effective_to,
    })).await?;
    history(
        &mut tx, tenant, TABLE, r.id, None, "draft", None, None, actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "create",
        r.id,
        json!({"opportunityId": o.id, "versionNo": next}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, r.id).await
}

#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: &CreditMappingRule,
    to: V,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<CreditMappingRule, KernelError> {
    if !version_can_transition(r.status, to) {
        return Err(illegal(ENTITY, r.status, to));
    }
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": kit::now()});
    if let Some(pid) = pid {
        changes["process_instance_id"] = json!(pid);
    }
    if to == V::Effective {
        if let Some(old) = effective_for(conn, r.opportunity_id).await? {
            let _: CreditMappingRule = kit::lock(conn, TABLE, ENTITY, tenant, old.id).await?;
            let end = (r.effective_from - Duration::days(1)).max(old.effective_from);
            kit::patch::<CreditMappingRule>(conn, TABLE, tenant, old.id, json!({
                "status": "superseded", "status_reason": format!("superseded by version {}", r.version_no),
                "status_changed_at": kit::now(), "effective_to": old.effective_to.map_or(end, |e| e.min(end)),
            })).await?;
            history(
                conn,
                tenant,
                TABLE,
                old.id,
                Some("effective"),
                "superseded",
                Some("superseded by a new version"),
                pid,
                actor,
            )
            .await?;
            changes["supersedes_id"] = json!(old.id);
        }
    }
    let n: CreditMappingRule = kit::patch(conn, TABLE, tenant, r.id, changes).await?;
    history(
        conn,
        tenant,
        TABLE,
        r.id,
        Some(r.status.as_str()),
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
        r.id,
        json!({"from": r.status, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    if to == V::Effective {
        StatusEvent {
            aggregate_id: r.id,
            institution_id: r.institution_id,
            from: Some(r.status.as_str()),
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
                CREDIT_MAPPING_RULE_EFFECTIVE,
                CREDIT_MAPPING_RULE_EFFECTIVE_SCHEMA,
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
) -> Result<(CreditMappingRule, ExecutionScope), KernelError> {
    let r: CreditMappingRule = kit::fetch(&st.pool, TABLE, ENTITY, ctx.scope.tenant_id, id).await?;
    check_version(r.version, expected)?;
    let scope = row_scope(ctx, r.institution_id);
    require_institution_writable(st, &scope, InstitutionId(r.institution_id)).await?;
    Ok((r, scope))
}

/// Submit for approval (`CREDIT_MAPPING_RULE`); auto-approval publishes it (effective).
pub async fn submit(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<CreditMappingRule, KernelError> {
    let (r, scope) = load(st, ctx, id, expected).await?;
    if !version_can_transition(r.status, V::InReview) {
        return Err(illegal(ENTITY, r.status, V::InReview));
    }
    let tenant = ctx.scope.tenant_id;
    let o: LearningOpportunity = kit::fetch(
        &st.pool,
        opportunity::TABLE,
        opportunity::ENTITY,
        tenant,
        r.opportunity_id,
    )
    .await?;
    require(
        o.status != LearningOpportunityStatus::Archived,
        "the learning opportunity is archived",
    )?;
    validate(
        st,
        &scope,
        &o,
        r.curriculum_version_id,
        r.curriculum_component_id,
        &r.equivalency,
        r.credits,
        r.effective_from,
        r.effective_to,
    )
    .await?;
    let today = business_today(st, &scope, InstitutionId(r.institution_id)).await?;
    require(
        r.effective_to.is_none_or(|t| t >= today),
        "the mapping's effective period has already ended",
    )?;
    let mut vars = BTreeMap::new();
    vars.insert("opportunityId".into(), json!(r.opportunity_id));
    vars.insert("credits".into(), json!(r.credits));
    let outcome = approval::request_approval(
        st,
        &scope,
        process::CREDIT_MAPPING_RULE,
        business_key(TABLE, r.id, r.version),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = begin(st).await?;
    let locked: CreditMappingRule = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
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
            Some("mapping approved"),
            Some(&pid),
            &sys,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Explicit mapping-approval decision.
#[allow(clippy::too_many_arguments)]
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<CreditMappingRule, KernelError> {
    let (r, scope) = load(st, ctx, id, expected).await?;
    require(
        r.status == V::InReview,
        format!(
            "the mapping rule is {}; there is no pending approval",
            r.status
        ),
    )?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: CreditMappingRule = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
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

/// Retire a mapping version (history kept; posted recognitions are never altered).
pub async fn retire(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<CreditMappingRule, KernelError> {
    let (_, scope) = load(st, ctx, id, expected).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: CreditMappingRule = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
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
