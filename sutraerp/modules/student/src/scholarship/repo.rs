//! **3C** scholarship persistence + commands (L3-C-062 … C-071).
//!
//! Definitions `draft → published → active ⇄ suspended → expired → archived`; policy versions
//! `draft → in_review → effective → superseded | retired` (WorkflowPort `SCHOLARSHIP_POLICY`);
//! eligibility determinations evaluate the effective policy against the applicant facts and
//! append every assessment to `scholarship_eligibility_assessment`. A recommendation goes
//! through `SCHOLARSHIP_RECOMMENDATION`; the hand-off to Financial Aid (Domain F, Batch 5) is
//! **only** the `scholarship_eligibility.handed_off` event. External ecosystems are called
//! through `ExternalScholarshipPort` (no-op adapter) and never grant eligibility by themselves.

use super::domain::*;
use crate::completion::kit::{self, StatusEvent};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::student::{load_context, load_student, StudentRow};
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use chrono::{Duration, NaiveDate};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::student::events::scholarship::*;
use sutra_contracts::student::ids::{
    ScholarshipDefinitionId, ScholarshipEligibilityId, ScholarshipPolicyId,
    ScholarshipRecalculationId, StudentId,
};
use sutra_contracts::student::ports::{ExternalScholarshipSync, ExternalSyncOutcome};
use sutra_contracts::student::status::{
    PolicyVersionStatus, ScholarshipDefinitionStatus, ScholarshipEligibilityStatus,
};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const DEF: &str = "scholarship";
const DEF_ENTITY: &str = "scholarship";
const POLICY: &str = "scholarship_policy";
const POLICY_ENTITY: &str = "scholarship policy";
const ELIG: &str = "scholarship_eligibility";
const ELIG_ENTITY: &str = "scholarship eligibility";
const ASSESSMENT: &str = "scholarship_eligibility_assessment";
const SYNC: &str = "scholarship_external_sync";
const RECALC: &str = "scholarship_recalculation";

type DefStatus = ScholarshipDefinitionStatus;
type ElStatus = ScholarshipEligibilityStatus;

macro_rules! tx {
    ($state:expr) => {
        $state.pool.begin().await.map_err(map_db_error)?
    };
}

fn trimmed(v: Option<String>) -> Option<String> {
    v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

// ---- definitions -------------------------------------------------------------------------------

async fn effective_policy<'e, E: sqlx::PgExecutor<'e>>(
    executor: E,
    tenant: TenantId,
    scholarship_id: Uuid,
) -> Result<Option<ScholarshipPolicy>, KernelError> {
    let v: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM student.scholarship_policy p WHERE p.tenant_id = $1 AND p.scholarship_id = $2 AND p.status = 'effective'",
    )
    .bind(tenant.as_uuid())
    .bind(scholarship_id)
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?;
    v.map(kit::decode).transpose()
}

async fn hydrate(
    state: &StudentState,
    tenant: TenantId,
    mut s: Scholarship,
) -> Result<Scholarship, KernelError> {
    s.effective_policy = effective_policy(&state.pool, tenant, s.id.as_uuid()).await?;
    Ok(s)
}

pub async fn get(
    state: &StudentState,
    tenant: TenantId,
    id: ScholarshipDefinitionId,
) -> Result<Scholarship, KernelError> {
    let s = kit::fetch(&state.pool, DEF, DEF_ENTITY, tenant, id.as_uuid()).await?;
    hydrate(state, tenant, s).await
}

pub async fn list(
    state: &StudentState,
    tenant: TenantId,
    institution_id: InstitutionId,
    status: Option<DefStatus>,
) -> Result<Vec<Scholarship>, KernelError> {
    let list: Vec<Scholarship> = kit::list_by(
        &state.pool,
        DEF,
        tenant,
        "institution_id",
        institution_id.as_uuid(),
        status.map(|s| s.as_str()),
    )
    .await?;
    let mut out = Vec::with_capacity(list.len());
    for s in list {
        out.push(hydrate(state, tenant, s).await?);
    }
    Ok(out)
}

fn def_event<'a>(
    (event_type, schema): (&'static str, &'static str),
    s: &'a Scholarship,
    from: DefStatus,
    reason: Option<&'a str>,
) -> StatusEvent<'a> {
    StatusEvent {
        event_type,
        schema,
        aggregate_id: s.id.as_uuid(),
        institution_id: s.institution_id,
        student: None,
        from: Some(from.as_str()),
        to: s.status.as_str(),
        version: s.version,
        reason,
        process_instance_id: None,
    }
}

pub async fn create(
    state: &StudentState,
    ctx: &AuthContext,
    institution_id: InstitutionId,
    cmd: NewScholarship,
    actor: &Actor,
) -> Result<Scholarship, KernelError> {
    let code = cmd.code.trim().to_uppercase();
    if !valid_code(&code) {
        return Err(kit::invalid(
            "code",
            "format",
            "2–32 characters A–Z, 0–9, '_' or '-'",
        ));
    }
    let name = kit::required_text("name", Some(&cmd.name))?;
    if cmd.valid_until.is_some_and(|u| u < cmd.valid_from) {
        return Err(kit::invalid(
            "validUntil",
            "before_start",
            "the end date is before the start date",
        ));
    }
    if cmd.benefit_value.is_some_and(|v| v < 0.0)
        || (cmd.benefit_type == BenefitType::FeePercentage
            && cmd.benefit_value.is_some_and(|v| v > 100.0))
    {
        return Err(kit::invalid(
            "benefitValue",
            "range",
            "the benefit value is out of range",
        ));
    }
    let scope = kit::writable_institution(state, ctx, institution_id).await?;
    let mut tx = tx!(state);
    let s: Scholarship = kit::insert(&mut tx, DEF, json!({
        "id": Uuid::now_v7(), "tenant_id": ctx.scope.tenant_id.as_uuid(), "institution_id": institution_id.as_uuid(),
        "code": code, "name": name, "description": trimmed(cmd.description), "sponsor_type": cmd.sponsor_type,
        "external_scheme": trimmed(cmd.external_scheme), "benefit_type": cmd.benefit_type, "benefit_value": cmd.benefit_value,
        "valid_from": cmd.valid_from, "valid_until": cmd.valid_until, "renewable": cmd.renewable,
        "status": DefStatus::Draft.as_str(),
    })).await.map_err(|e| match e {
        KernelError::Conflict(_) => kit::invalid("code", "duplicate", "a scholarship with this code exists"),
        other => other,
    })?;
    kit::audit(
        &mut tx,
        &scope,
        DEF,
        "create",
        s.id.as_uuid(),
        json!({"code": s.code}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, ctx.scope.tenant_id, s.id).await
}

async fn load_def(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipDefinitionId,
) -> Result<(Scholarship, ExecutionScope), KernelError> {
    let s: Scholarship = kit::fetch(
        &state.pool,
        DEF,
        DEF_ENTITY,
        ctx.scope.tenant_id,
        id.as_uuid(),
    )
    .await?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(s.institution_id)).await?;
    Ok((s, scope))
}

pub async fn update(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipDefinitionId,
    expected: Version,
    cmd: ScholarshipUpdate,
    actor: &Actor,
) -> Result<Scholarship, KernelError> {
    let (s, scope) = load_def(state, ctx, id).await?;
    if s.status != DefStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "the scholarship is {}; only drafts are edited",
            s.status
        )));
    }
    let mut changes = serde_json::Map::new();
    if let Some(n) = cmd.name {
        changes.insert("name".into(), json!(kit::required_text("name", Some(&n))?));
    }
    if let Some(d) = cmd.description {
        changes.insert("description".into(), json!(trimmed(Some(d))));
    }
    if let Some(d) = cmd.external_scheme {
        changes.insert("external_scheme".into(), json!(trimmed(Some(d))));
    }
    if let Some(v) = cmd.benefit_value {
        changes.insert("benefit_value".into(), json!(v));
    }
    if let Some(v) = cmd.valid_from {
        changes.insert("valid_from".into(), json!(v));
    }
    if let Some(v) = cmd.valid_until {
        changes.insert("valid_until".into(), json!(v));
    }
    if let Some(v) = cmd.renewable {
        changes.insert("renewable".into(), json!(v));
    }
    if changes.is_empty() {
        return Err(kit::invalid("body", "no_change", "nothing to update"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked: Scholarship = kit::lock(&mut tx, DEF, DEF_ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    let fields: Vec<String> = changes.keys().map(|k| kit::camel_key(k)).collect();
    let _: Scholarship = kit::patch(
        &mut tx,
        DEF,
        tenant,
        id.as_uuid(),
        serde_json::Value::Object(changes),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        DEF,
        "update",
        id.as_uuid(),
        json!({"changedFields": fields}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Lifecycle command on a definition.
pub async fn move_definition(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipDefinitionId,
    expected: Version,
    to: DefStatus,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Scholarship, KernelError> {
    let (s, scope) = load_def(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    if to == DefStatus::Suspended {
        kit::required_text("reason", reason.as_deref())?;
    }
    let mut tx = tx!(state);
    let locked: Scholarship = kit::lock(&mut tx, DEF, DEF_ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    if !definition_can_transition(locked.status, to) {
        return Err(kit::illegal("the scholarship", locked.status, to));
    }
    if to == DefStatus::Active
        && effective_policy(&mut *tx, tenant, id.as_uuid())
            .await?
            .is_none()
    {
        return Err(KernelError::Conflict(
            "the scholarship needs an effective eligibility policy before it becomes available"
                .into(),
        ));
    }
    let updated: Scholarship = kit::patch(
        &mut tx,
        DEF,
        tenant,
        id.as_uuid(),
        json!({"status": to.as_str(), "status_reason": reason}),
    )
    .await?;
    let event = match to {
        DefStatus::Published => Some((SCHOLARSHIP_PUBLISHED, SCHOLARSHIP_PUBLISHED_SCHEMA)),
        DefStatus::Active => Some((SCHOLARSHIP_ACTIVATED, SCHOLARSHIP_ACTIVATED_SCHEMA)),
        DefStatus::Suspended => Some((SCHOLARSHIP_SUSPENDED, SCHOLARSHIP_SUSPENDED_SCHEMA)),
        DefStatus::Expired => Some((SCHOLARSHIP_EXPIRED, SCHOLARSHIP_EXPIRED_SCHEMA)),
        DefStatus::Archived => Some((SCHOLARSHIP_ARCHIVED, SCHOLARSHIP_ARCHIVED_SCHEMA)),
        DefStatus::Draft => None,
    };
    if let Some(e) = event {
        def_event(e, &updated, locked.status, reason.as_deref())
            .emit(&mut tx, &scope, actor)
            .await?;
    }
    kit::audit(
        &mut tx,
        &scope,
        DEF,
        to.as_str(),
        id.as_uuid(),
        json!({"fromStatus": locked.status, "toStatus": to, "reason": reason}),
        actor,
    )
    .await?;
    let _ = s;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ---- policies -----------------------------------------------------------------------------------

pub async fn list_policies(
    state: &StudentState,
    tenant: TenantId,
    scholarship_id: ScholarshipDefinitionId,
) -> Result<Vec<ScholarshipPolicy>, KernelError> {
    kit::list_by(
        &state.pool,
        POLICY,
        tenant,
        "scholarship_id",
        scholarship_id.as_uuid(),
        None,
    )
    .await
}

pub async fn get_policy(
    state: &StudentState,
    tenant: TenantId,
    id: ScholarshipPolicyId,
) -> Result<ScholarshipPolicy, KernelError> {
    kit::fetch(&state.pool, POLICY, POLICY_ENTITY, tenant, id.as_uuid()).await
}

fn validate_criteria(c: &PolicyCriteria) -> Result<(), KernelError> {
    if c.min_academic_score.is_some_and(|v| v < 0.0) || c.max_family_income.is_some_and(|v| v < 0.0)
    {
        return Err(kit::invalid(
            "criteria",
            "range",
            "criteria values must not be negative",
        ));
    }
    if c.condition_days.is_some_and(|d| d < 1) || c.validity_days.is_some_and(|d| d < 1) {
        return Err(kit::invalid(
            "criteria",
            "range",
            "day counts must be positive",
        ));
    }
    Ok(())
}

pub async fn create_policy(
    state: &StudentState,
    ctx: &AuthContext,
    scholarship_id: ScholarshipDefinitionId,
    cmd: NewScholarshipPolicy,
    actor: &Actor,
) -> Result<ScholarshipPolicy, KernelError> {
    validate_criteria(&cmd.criteria)?;
    let (s, scope) = load_def(state, ctx, scholarship_id).await?;
    if matches!(s.status, DefStatus::Expired | DefStatus::Archived) {
        return Err(KernelError::Conflict(format!(
            "the scholarship is {}",
            s.status
        )));
    }
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(s.institution_id)).await?;
    let mut tx = tx!(state);
    let _: Scholarship =
        kit::lock(&mut tx, DEF, DEF_ENTITY, tenant, scholarship_id.as_uuid()).await?;
    let next: i32 = sqlx::query_scalar("SELECT coalesce(max(version_no), 0) + 1 FROM student.scholarship_policy WHERE scholarship_id = $1")
        .bind(scholarship_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let p: ScholarshipPolicy = kit::insert(&mut tx, POLICY, json!({
        "id": Uuid::now_v7(), "tenant_id": tenant.as_uuid(), "institution_id": s.institution_id,
        "scholarship_id": scholarship_id.as_uuid(), "version_no": next, "status": PolicyVersionStatus::Draft.as_str(),
        "criteria": cmd.criteria, "effective_from": cmd.effective_from.unwrap_or(today),
    })).await.map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("a draft or in-review policy version already exists".into()),
        other => other,
    })?;
    kit::audit(
        &mut tx,
        &scope,
        POLICY,
        "create",
        p.id.as_uuid(),
        json!({"scholarshipId": scholarship_id, "versionNo": next}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

async fn load_policy(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipPolicyId,
) -> Result<(ScholarshipPolicy, Scholarship, ExecutionScope), KernelError> {
    let tenant = ctx.scope.tenant_id;
    let p = get_policy(state, tenant, id).await?;
    let (s, scope) = load_def(state, ctx, p.scholarship_id).await?;
    Ok((p, s, scope))
}

pub async fn update_policy(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipPolicyId,
    expected: Version,
    cmd: ScholarshipPolicyUpdate,
    actor: &Actor,
) -> Result<ScholarshipPolicy, KernelError> {
    let (p, _, scope) = load_policy(state, ctx, id).await?;
    if p.status != PolicyVersionStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "the policy version is {}; only drafts are edited",
            p.status
        )));
    }
    if let Some(c) = &cmd.criteria {
        validate_criteria(c)?;
    }
    let mut changes = serde_json::Map::new();
    if let Some(c) = cmd.criteria {
        changes.insert("criteria".into(), json!(c));
    }
    if let Some(d) = cmd.effective_from {
        changes.insert("effective_from".into(), json!(d));
    }
    if changes.is_empty() {
        return Err(kit::invalid("body", "no_change", "nothing to update"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked: ScholarshipPolicy =
        kit::lock(&mut tx, POLICY, POLICY_ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    let updated: ScholarshipPolicy = kit::patch(
        &mut tx,
        POLICY,
        tenant,
        id.as_uuid(),
        serde_json::Value::Object(changes),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        POLICY,
        "update",
        id.as_uuid(),
        json!({}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

async fn apply_policy_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    p: &ScholarshipPolicy,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ScholarshipPolicy, KernelError> {
    let to = if approve {
        PolicyVersionStatus::Effective
    } else {
        PolicyVersionStatus::Draft
    };
    if !policy_can_transition(p.status, to) {
        return Err(kit::illegal("the policy version", p.status, to));
    }
    if approve {
        // The prior effective version becomes history; existing eligibilities are reassessed
        // through C-066 / C-070, never silently rewritten.
        if let Some(prior) =
            effective_policy(&mut *conn, tenant, p.scholarship_id.as_uuid()).await?
        {
            let superseded: ScholarshipPolicy = kit::patch(
                conn,
                POLICY,
                tenant,
                prior.id.as_uuid(),
                json!({"status": PolicyVersionStatus::Superseded.as_str()}),
            )
            .await?;
            StatusEvent {
                event_type: SCHOLARSHIP_POLICY_RETIRED,
                schema: SCHOLARSHIP_POLICY_RETIRED_SCHEMA,
                aggregate_id: prior.id.as_uuid(),
                institution_id: scope
                    .institution_id
                    .map(|i| i.as_uuid())
                    .unwrap_or_default(),
                student: None,
                from: Some(prior.status.as_str()),
                to: superseded.status.as_str(),
                version: superseded.version,
                reason: Some("superseded by a new version"),
                process_instance_id: None,
            }
            .emit(conn, scope, actor)
            .await?;
        }
    }
    let updated: ScholarshipPolicy = kit::patch(
        conn,
        POLICY,
        tenant,
        p.id.as_uuid(),
        json!({"status": to.as_str(), "decision_reason": reason}),
    )
    .await?;
    if approve {
        StatusEvent {
            event_type: SCHOLARSHIP_POLICY_ACTIVATED,
            schema: SCHOLARSHIP_POLICY_ACTIVATED_SCHEMA,
            aggregate_id: p.id.as_uuid(),
            institution_id: scope
                .institution_id
                .map(|i| i.as_uuid())
                .unwrap_or_default(),
            student: None,
            from: Some(p.status.as_str()),
            to: updated.status.as_str(),
            version: updated.version,
            reason,
            process_instance_id: p.process_instance_id.as_deref(),
        }
        .emit(conn, scope, actor)
        .await?;
    }
    kit::audit(
        conn,
        scope,
        POLICY,
        if approve { "approve" } else { "reject" },
        p.id.as_uuid(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    Ok(updated)
}

/// Submits a draft for approval (`SCHOLARSHIP_POLICY`). Future-dated versions are submitted on
/// their effective date so the prior effective version stays in force until then.
pub async fn submit_policy(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipPolicyId,
    expected: Version,
    actor: &Actor,
) -> Result<ScholarshipPolicy, KernelError> {
    let (p, s, scope) = load_policy(state, ctx, id).await?;
    if p.status != PolicyVersionStatus::Draft {
        return Err(kit::illegal(
            "the policy version",
            p.status,
            PolicyVersionStatus::InReview,
        ));
    }
    let today = business_today(state, &scope, InstitutionId(s.institution_id)).await?;
    if p.effective_from > today {
        return Err(KernelError::Conflict(format!(
            "the policy version is effective from {}; submit it on or after that date",
            p.effective_from
        )));
    }
    let tenant = ctx.scope.tenant_id;
    let mut vars = BTreeMap::new();
    vars.insert("scholarshipId".to_owned(), json!(s.id));
    let outcome = approval::request_approval(
        state,
        &scope,
        process::SCHOLARSHIP_POLICY,
        business_key(POLICY, id, p.version),
        vars,
    )
    .await?;
    let mut tx = tx!(state);
    let locked: ScholarshipPolicy =
        kit::lock(&mut tx, POLICY, POLICY_ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    let pending: ScholarshipPolicy = kit::patch(&mut tx, POLICY, tenant, id.as_uuid(), json!({
        "status": PolicyVersionStatus::InReview.as_str(), "process_instance_id": outcome.process_instance_id(),
    })).await?;
    kit::audit(
        &mut tx,
        &scope,
        POLICY,
        "submit",
        id.as_uuid(),
        json!({"processInstanceId": outcome.process_instance_id()}),
        actor,
    )
    .await?;
    let result = if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        apply_policy_decision(&mut tx, &scope, tenant, &pending, true, None, &system).await?
    } else {
        pending
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(result)
}

pub async fn decide_policy(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipPolicyId,
    expected: Version,
    cmd: kit::LifecycleDecision,
    actor: &Actor,
) -> Result<ScholarshipPolicy, KernelError> {
    let approve = cmd.decision == kit::LifecycleDecisionValue::Approve;
    if !approve {
        kit::required_text("reason", cmd.reason.as_deref())?;
    }
    let (_, _, scope) = load_policy(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked: ScholarshipPolicy =
        kit::lock(&mut tx, POLICY, POLICY_ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    if locked.status != PolicyVersionStatus::InReview {
        return Err(KernelError::Conflict(format!(
            "the policy version is {}; no decision is pending",
            locked.status
        )));
    }
    let r = apply_policy_decision(
        &mut tx,
        &scope,
        tenant,
        &locked,
        approve,
        cmd.reason.as_deref(),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

pub async fn retire_policy(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipPolicyId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ScholarshipPolicy, KernelError> {
    let (_, _, scope) = load_policy(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked: ScholarshipPolicy =
        kit::lock(&mut tx, POLICY, POLICY_ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(locked.version, expected)?;
    if !policy_can_transition(locked.status, PolicyVersionStatus::Retired) {
        return Err(kit::illegal(
            "the policy version",
            locked.status,
            PolicyVersionStatus::Retired,
        ));
    }
    let r: ScholarshipPolicy = kit::patch(
        &mut tx,
        POLICY,
        tenant,
        id.as_uuid(),
        json!({"status": PolicyVersionStatus::Retired.as_str(), "status_reason": reason}),
    )
    .await?;
    StatusEvent {
        event_type: SCHOLARSHIP_POLICY_RETIRED,
        schema: SCHOLARSHIP_POLICY_RETIRED_SCHEMA,
        aggregate_id: id.as_uuid(),
        institution_id: scope
            .institution_id
            .map(|i| i.as_uuid())
            .unwrap_or_default(),
        student: None,
        from: Some(locked.status.as_str()),
        to: r.status.as_str(),
        version: r.version,
        reason: reason.as_deref(),
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        POLICY,
        "retire",
        id.as_uuid(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

// ---- eligibility -------------------------------------------------------------------------------

pub async fn get_eligibility(
    state: &StudentState,
    tenant: TenantId,
    id: ScholarshipEligibilityId,
) -> Result<ScholarshipEligibility, KernelError> {
    let mut e: ScholarshipEligibility =
        kit::fetch(&state.pool, ELIG, ELIG_ENTITY, tenant, id.as_uuid()).await?;
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(a) FROM student.scholarship_eligibility_assessment a \
         WHERE a.tenant_id = $1 AND a.eligibility_id = $2 ORDER BY a.recorded_at, a.id",
    )
    .bind(tenant.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let items: Vec<EligibilityAssessment> = rows
        .into_iter()
        .map(kit::decode)
        .collect::<Result<_, _>>()?;
    e.assessments = items;
    Ok(e)
}

pub async fn list_eligibilities(
    state: &StudentState,
    tenant: TenantId,
    column: &str,
    id: Uuid,
    status: Option<ElStatus>,
) -> Result<Vec<ScholarshipEligibility>, KernelError> {
    kit::list_by(
        &state.pool,
        ELIG,
        tenant,
        column,
        id,
        status.map(|s| s.as_str()),
    )
    .await
}

fn el_payload(e: &ScholarshipEligibility, student: &StudentRow) -> ScholarshipEligibilityV1 {
    ScholarshipEligibilityV1 {
        eligibility_id: e.id.as_uuid(),
        scholarship_id: e.scholarship_id.as_uuid(),
        policy_id: e.policy_id.map(|p| p.as_uuid()),
        student_id: e.student_id.as_uuid(),
        institution_id: e.institution_id,
        status: e.status.as_str().into(),
        academic_year_id: e.academic_year_id,
        valid_until: e.valid_until,
        student_subject_ref: student.subject_ref.clone(),
    }
}

async fn el_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    (t, s): (&'static str, &'static str),
    e: &ScholarshipEligibility,
    student: &StudentRow,
    actor: &Actor,
) -> Result<(), KernelError> {
    crate::shared::publish::event(
        conn,
        scope,
        t,
        s,
        e.id.to_string(),
        &el_payload(e, student),
        actor,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn append_assessment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    kind: &str,
    policy: Option<Uuid>,
    from: Option<ElStatus>,
    to: ElStatus,
    facts: &EligibilityFacts,
    evaluation: Option<&Evaluation>,
    reason: Option<&str>,
    recalculation: Option<Uuid>,
    actor: &Actor,
) -> Result<(), KernelError> {
    kit::append(conn, ASSESSMENT, json!({
        "tenant_id": tenant.as_uuid(), "eligibility_id": id, "kind": kind, "policy_id": policy,
        "from_status": from.map(|f| f.as_str()), "to_status": to.as_str(), "facts": facts, "evaluation": evaluation,
        "reason": reason, "recalculation_id": recalculation, "actor": actor.id,
    })).await
}

/// Moves an eligibility (checked); `extra` columns are set alongside.
async fn move_el(
    conn: &mut PgConnection,
    tenant: TenantId,
    e: &ScholarshipEligibility,
    to: ElStatus,
    extra: serde_json::Value,
) -> Result<ScholarshipEligibility, KernelError> {
    if e.status != to && !eligibility_can_transition(e.status, to) {
        return Err(kit::illegal("the scholarship eligibility", e.status, to));
    }
    let mut changes = if extra.is_object() { extra } else { json!({}) };
    changes["status"] = json!(to.as_str());
    kit::patch(conn, ELIG, tenant, e.id.as_uuid(), changes).await
}

fn evaluation_of(
    policy: &ScholarshipPolicy,
    facts: &EligibilityFacts,
    programme: Option<Uuid>,
    term: Option<i32>,
) -> Evaluation {
    let (outcome, criteria, conditions) = evaluate(&policy.criteria, facts, programme, term);
    Evaluation {
        outcome,
        criteria,
        conditions,
        policy_id: policy.id,
        policy_version_no: policy.version_no,
        evaluated_at: chrono::Utc::now(),
    }
}

fn condition_deadline(policy: &ScholarshipPolicy, today: NaiveDate) -> NaiveDate {
    today + Duration::days(i64::from(policy.criteria.condition_days.unwrap_or(30)))
}

fn validity(policy: &ScholarshipPolicy, s: &Scholarship, from: NaiveDate) -> NaiveDate {
    let by_policy = policy
        .criteria
        .validity_days
        .map(|d| from + Duration::days(i64::from(d)));
    match (by_policy, s.valid_until) {
        (Some(p), Some(u)) => p.min(u),
        (Some(p), None) => p,
        (None, Some(u)) => u,
        (None, None) => from + Duration::days(365),
    }
}

/// The effective policy usable today (L4-C-062: an unavailable definition is no eligibility basis).
async fn usable_policy(
    state: &StudentState,
    tenant: TenantId,
    s: &Scholarship,
    today: NaiveDate,
) -> Result<ScholarshipPolicy, KernelError> {
    if s.status != DefStatus::Active {
        return Err(KernelError::Conflict(format!(
            "the scholarship is {}; it is not available",
            s.status
        )));
    }
    if s.valid_until.is_some_and(|u| u < today) || s.valid_from > today {
        return Err(KernelError::Conflict(
            "the scholarship is outside its validity period".into(),
        ));
    }
    let p = effective_policy(&state.pool, tenant, s.id.as_uuid())
        .await?
        .ok_or_else(|| {
            KernelError::Conflict("the scholarship has no effective eligibility policy".into())
        })?;
    if p.effective_from > today {
        return Err(KernelError::Conflict(
            "the eligibility policy is not yet effective".into(),
        ));
    }
    Ok(p)
}

/// Determination + recommendation input (L3-C-064).
pub async fn determine(
    state: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
    cmd: NewScholarshipEligibility,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let target = kit::writable_student(state, ctx, student_id).await?;
    let tenant = ctx.scope.tenant_id;
    let s: Scholarship = kit::fetch(
        &state.pool,
        DEF,
        DEF_ENTITY,
        tenant,
        cmd.scholarship_id.as_uuid(),
    )
    .await
    .map_err(|_| kit::invalid("scholarshipId", "not_found", "unknown scholarship"))?;
    if s.institution_id != target.institution_id().as_uuid() {
        return Err(kit::invalid(
            "scholarshipId",
            "other_institution",
            "the scholarship belongs to another institution",
        ));
    }
    let today = business_today(state, &target.scope, target.institution_id()).await?;
    let policy = usable_policy(state, tenant, &s, today).await?;
    let ev = evaluation_of(
        &policy,
        &cmd.facts,
        target.context.programme_id,
        target.context.current_term_no,
    );
    let to = ev.outcome.status();
    let mut tx = tx!(state);
    let id = ScholarshipEligibilityId::new();
    let created: ScholarshipEligibility = kit::insert(&mut tx, ELIG, json!({
        "id": id.as_uuid(), "tenant_id": tenant.as_uuid(), "institution_id": s.institution_id,
        "scholarship_id": s.id.as_uuid(), "policy_id": policy.id.as_uuid(), "student_id": student_id.as_uuid(),
        "student_context_id": target.context.id.as_uuid(), "academic_year_id": cmd.academic_year_id,
        "status": ElStatus::Created.as_str(), "facts": cmd.facts,
    })).await.map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("the student already has a current eligibility for this scholarship".into()),
        other => other,
    })?;
    let pending = move_el(
        &mut tx,
        tenant,
        &created,
        ElStatus::AssessmentPending,
        json!({}),
    )
    .await?;
    let evaluating = move_el(
        &mut tx,
        tenant,
        &pending,
        ElStatus::UnderEvaluation,
        json!({}),
    )
    .await?;
    let conditional = to == ElStatus::ConditionallyEligible;
    let done = move_el(&mut tx, tenant, &evaluating, to, json!({
        "evaluation": ev, "conditions": ev.conditions,
        "condition_deadline": if conditional { Some(condition_deadline(&policy, today)) } else { None },
    })).await?;
    append_assessment(
        &mut tx,
        tenant,
        id.as_uuid(),
        "determination",
        Some(policy.id.as_uuid()),
        Some(ElStatus::Created),
        to,
        &cmd.facts,
        Some(&ev),
        None,
        None,
        actor,
    )
    .await?;
    el_event(
        &mut tx,
        &target.scope,
        (
            SCHOLARSHIP_ELIGIBILITY_DETERMINED,
            SCHOLARSHIP_ELIGIBILITY_DETERMINED_SCHEMA,
        ),
        &done,
        &target.student,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &target.scope,
        ELIG,
        "determine",
        id.as_uuid(),
        json!({"scholarshipId": s.id, "outcome": ev.outcome, "policyVersionNo": policy.version_no}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

struct ElCmd {
    e: ScholarshipEligibility,
    s: Scholarship,
    student: StudentRow,
    scope: ExecutionScope,
}

async fn load_el(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
) -> Result<ElCmd, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let e: ScholarshipEligibility =
        kit::fetch(&state.pool, ELIG, ELIG_ENTITY, tenant, id.as_uuid()).await?;
    let s: Scholarship = kit::fetch(
        &state.pool,
        DEF,
        DEF_ENTITY,
        tenant,
        e.scholarship_id.as_uuid(),
    )
    .await?;
    let student = load_student(&state.pool, tenant, e.student_id).await?;
    crate::shared::guard::require_student_writable(&student)?;
    let scope = kit::writable_institution(state, ctx, InstitutionId(e.institution_id)).await?;
    Ok(ElCmd {
        e,
        s,
        student,
        scope,
    })
}

async fn lock_el(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ScholarshipEligibilityId,
    expected: Version,
) -> Result<ScholarshipEligibility, KernelError> {
    let e: ScholarshipEligibility =
        kit::lock(conn, ELIG, ELIG_ENTITY, tenant, id.as_uuid()).await?;
    kit::check_version(e.version, expected)?;
    Ok(e)
}

#[allow(clippy::too_many_arguments)]
async fn apply_recommendation(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    e: &ScholarshipEligibility,
    student: &StudentRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    if approve {
        let r = move_el(
            conn,
            tenant,
            e,
            ElStatus::RecommendationGenerated,
            json!({"recommendation_pending": false}),
        )
        .await?;
        el_event(
            conn,
            scope,
            (
                SCHOLARSHIP_ELIGIBILITY_RECOMMENDED,
                SCHOLARSHIP_ELIGIBILITY_RECOMMENDED_SCHEMA,
            ),
            &r,
            student,
            actor,
        )
        .await?;
        kit::audit(
            conn,
            scope,
            ELIG,
            "recommend",
            e.id.as_uuid(),
            json!({"processInstanceId": e.process_instance_id}),
            actor,
        )
        .await?;
        Ok(r)
    } else {
        let r: ScholarshipEligibility = kit::patch(
            conn,
            ELIG,
            tenant,
            e.id.as_uuid(),
            json!({"recommendation_pending": false, "status_reason": reason}),
        )
        .await?;
        kit::audit(
            conn,
            scope,
            ELIG,
            "recommendation_reject",
            e.id.as_uuid(),
            json!({"reason": reason}),
            actor,
        )
        .await?;
        Ok(r)
    }
}

/// Generates the recommendation of an eligible determination (`SCHOLARSHIP_RECOMMENDATION`).
pub async fn recommend(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let ElCmd {
        e, student, scope, ..
    } = load_el(state, ctx, id).await?;
    if e.status != ElStatus::Eligible || e.recommendation_pending {
        return Err(KernelError::Conflict(format!("the eligibility is {}; a recommendation needs an eligible determination without a pending recommendation", e.status)));
    }
    let tenant = ctx.scope.tenant_id;
    let outcome = approval::request_approval(
        state,
        &scope,
        process::SCHOLARSHIP_RECOMMENDATION,
        business_key(ELIG, id, e.version),
        BTreeMap::new(),
    )
    .await?;
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    let pending: ScholarshipEligibility = kit::patch(
        &mut tx,
        ELIG,
        tenant,
        id.as_uuid(),
        json!({
            "recommendation_pending": true, "process_instance_id": outcome.process_instance_id(),
        }),
    )
    .await?;
    let _ = locked;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        apply_recommendation(
            &mut tx, &scope, tenant, &pending, &student, true, None, &system,
        )
        .await?;
    } else {
        kit::audit(
            &mut tx,
            &scope,
            ELIG,
            "recommendation_request",
            id.as_uuid(),
            json!({"processInstanceId": outcome.process_instance_id()}),
            actor,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

pub async fn recommendation_decision(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    cmd: kit::LifecycleDecision,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let approve = cmd.decision == kit::LifecycleDecisionValue::Approve;
    if !approve {
        kit::required_text("reason", cmd.reason.as_deref())?;
    }
    let ElCmd { student, scope, .. } = load_el(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    if !locked.recommendation_pending {
        return Err(KernelError::Conflict(
            "no recommendation decision is pending".into(),
        ));
    }
    apply_recommendation(
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
    get_eligibility(state, tenant, id).await
}

pub async fn withdraw_recommendation(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let reason = kit::required_text("reason", reason.as_deref())?;
    let ElCmd { scope, .. } = load_el(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    if locked.status != ElStatus::RecommendationGenerated {
        return Err(kit::illegal(
            "the scholarship eligibility",
            locked.status,
            ElStatus::Eligible,
        ));
    }
    move_el(
        &mut tx,
        tenant,
        &locked,
        ElStatus::Eligible,
        json!({"status_reason": reason}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        ELIG,
        "withdraw_recommendation",
        id.as_uuid(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

/// Hand-off to Financial Aid (L3-C-071): **event only**; the eligibility becomes `active`.
pub async fn hand_off(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let ElCmd {
        e,
        s,
        student,
        scope,
    } = load_el(state, ctx, id).await?;
    if e.status != ElStatus::RecommendationGenerated {
        return Err(KernelError::Conflict(format!(
            "the eligibility is {}; only a generated recommendation is handed off",
            e.status
        )));
    }
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(e.institution_id)).await?;
    let policy = match e.policy_id {
        Some(p) => get_policy(state, tenant, p).await?,
        None => {
            return Err(KernelError::Conflict(
                "the eligibility has no policy basis".into(),
            ))
        }
    };
    let until = validity(&policy, &s, today);
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    let active = move_el(
        &mut tx,
        tenant,
        &locked,
        ElStatus::Active,
        json!({
            "valid_from": today, "valid_until": until, "handed_off_at": chrono::Utc::now(),
        }),
    )
    .await?;
    el_event(
        &mut tx,
        &scope,
        (
            SCHOLARSHIP_ELIGIBILITY_HANDED_OFF,
            SCHOLARSHIP_ELIGIBILITY_HANDED_OFF_SCHEMA,
        ),
        &active,
        &student,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        ELIG,
        "hand_off",
        id.as_uuid(),
        json!({"validUntil": until}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

/// Conditional eligibility resolution or expiry (L3-C-065).
pub async fn resolve_condition(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    cmd: ResolveCondition,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let ElCmd {
        e, student, scope, ..
    } = load_el(state, ctx, id).await?;
    if e.status != ElStatus::ConditionallyEligible {
        return Err(KernelError::Conflict(format!(
            "the eligibility is {}; no condition is open",
            e.status
        )));
    }
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(e.institution_id)).await?;
    if cmd.outcome == ConditionOutcome::Expired && e.condition_deadline.is_some_and(|d| d >= today)
    {
        return Err(KernelError::Conflict(
            "the condition deadline has not passed".into(),
        ));
    }
    let facts = e.facts.merged(cmd.facts.as_ref());
    let context = load_context(&state.pool, tenant, e.student_context_id).await?;
    let policy = match e.policy_id {
        Some(p) => get_policy(state, tenant, p).await?,
        None => {
            return Err(KernelError::Conflict(
                "the eligibility has no policy basis".into(),
            ))
        }
    };
    let (to, ev) = match cmd.outcome {
        ConditionOutcome::Fulfilled => {
            let ev = evaluation_of(
                &policy,
                &facts,
                context.programme_id,
                context.current_term_no,
            );
            (ev.outcome.status(), Some(ev))
        }
        ConditionOutcome::Failed | ConditionOutcome::Expired => (ElStatus::NotEligible, None),
    };
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    let evaluating = move_el(
        &mut tx,
        tenant,
        &locked,
        ElStatus::UnderEvaluation,
        json!({}),
    )
    .await?;
    let mut extra = json!({"facts": facts, "status_reason": cmd.note});
    if let Some(ev) = &ev {
        extra["evaluation"] = json!(ev);
        extra["conditions"] = json!(ev.conditions);
    }
    if to != ElStatus::ConditionallyEligible {
        extra["condition_deadline"] = json!(null);
        extra["conditions"] = json!(Vec::<String>::new());
    }
    let done = move_el(&mut tx, tenant, &evaluating, to, extra).await?;
    append_assessment(
        &mut tx,
        tenant,
        id.as_uuid(),
        "condition_resolution",
        Some(policy.id.as_uuid()),
        Some(locked.status),
        to,
        &facts,
        ev.as_ref(),
        cmd.note.as_deref(),
        None,
        actor,
    )
    .await?;
    StatusEvent {
        event_type: SCHOLARSHIP_ELIGIBILITY_CONDITION_RESOLVED,
        schema: SCHOLARSHIP_ELIGIBILITY_CONDITION_RESOLVED_SCHEMA,
        aggregate_id: id.as_uuid(),
        institution_id: e.institution_id,
        student: Some(&student),
        from: Some(locked.status.as_str()),
        to: done.status.as_str(),
        version: done.version,
        reason: cmd.note.as_deref(),
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        ELIG,
        "resolve_condition",
        id.as_uuid(),
        json!({"outcome": format!("{:?}", cmd.outcome), "toStatus": to}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

/// Reassessment (L3-C-066) or renewal (L3-C-067) of an active eligibility.
pub async fn reevaluate(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    cmd: Reevaluation,
    renewal: bool,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let ElCmd {
        e,
        s,
        student,
        scope,
    } = load_el(state, ctx, id).await?;
    if e.status != ElStatus::Active {
        return Err(KernelError::Conflict(format!(
            "the eligibility is {}; only an active eligibility is {}",
            e.status,
            if renewal { "renewed" } else { "reassessed" }
        )));
    }
    if renewal && !s.renewable {
        return Err(KernelError::Conflict(
            "the scholarship is not renewable".into(),
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(e.institution_id)).await?;
    // Reassessment and renewal use the policy effective now (a new version applies here, not by rewrite).
    let policy = usable_policy(state, tenant, &s, today).await?;
    let facts = e.facts.merged(cmd.facts.as_ref());
    let context = load_context(&state.pool, tenant, e.student_context_id).await?;
    let ev = evaluation_of(
        &policy,
        &facts,
        context.programme_id,
        context.current_term_no,
    );
    let (mid, to) = if renewal {
        (
            ElStatus::Renewal,
            match ev.outcome {
                EvaluationOutcome::Eligible => ElStatus::Active,
                EvaluationOutcome::ConditionallyEligible => ElStatus::ConditionallyEligible,
                EvaluationOutcome::NotEligible => ElStatus::Expired,
            },
        )
    } else {
        (
            ElStatus::Reassessment,
            match ev.outcome {
                EvaluationOutcome::Eligible => ElStatus::Active,
                EvaluationOutcome::ConditionallyEligible => ElStatus::ConditionallyEligible,
                EvaluationOutcome::NotEligible => ElStatus::NotEligible,
            },
        )
    };
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    let middle = move_el(&mut tx, tenant, &locked, mid, json!({})).await?;
    let mut extra = json!({
        "facts": facts, "evaluation": ev, "conditions": ev.conditions, "policy_id": policy.id.as_uuid(),
        "status_reason": cmd.reason,
    });
    if to == ElStatus::ConditionallyEligible {
        extra["condition_deadline"] = json!(condition_deadline(&policy, today));
    }
    if renewal && to == ElStatus::Active {
        let from = e.valid_until.map_or(today, |u| u.max(today));
        extra["valid_until"] = json!(validity(&policy, &s, from));
        extra["renewal_count"] = json!(e.renewal_count + 1);
        if let Some(y) = cmd.academic_year_id {
            extra["academic_year_id"] = json!(y);
        }
    }
    let done = move_el(&mut tx, tenant, &middle, to, extra).await?;
    append_assessment(
        &mut tx,
        tenant,
        id.as_uuid(),
        if renewal { "renewal" } else { "reassessment" },
        Some(policy.id.as_uuid()),
        Some(locked.status),
        to,
        &facts,
        Some(&ev),
        cmd.reason.as_deref(),
        None,
        actor,
    )
    .await?;
    let event = if renewal {
        (
            SCHOLARSHIP_ELIGIBILITY_RENEWED,
            SCHOLARSHIP_ELIGIBILITY_RENEWED_SCHEMA,
        )
    } else {
        (
            SCHOLARSHIP_ELIGIBILITY_REASSESSED,
            SCHOLARSHIP_ELIGIBILITY_REASSESSED_SCHEMA,
        )
    };
    el_event(&mut tx, &scope, event, &done, &student, actor).await?;
    kit::audit(
        &mut tx,
        &scope,
        ELIG,
        if renewal { "renew" } else { "reassess" },
        id.as_uuid(),
        json!({"outcome": ev.outcome, "toStatus": to}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

/// Expiry (L3-C-068).
pub async fn expire(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let ElCmd { student, scope, .. } = load_el(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    if locked.status == ElStatus::Expired
        || !eligibility_can_transition(locked.status, ElStatus::Expired)
    {
        return Err(kit::illegal(
            "the scholarship eligibility",
            locked.status,
            ElStatus::Expired,
        ));
    }
    let done = move_el(
        &mut tx,
        tenant,
        &locked,
        ElStatus::Expired,
        json!({"status_reason": reason, "recommendation_pending": false}),
    )
    .await?;
    StatusEvent {
        event_type: SCHOLARSHIP_ELIGIBILITY_EXPIRED,
        schema: SCHOLARSHIP_ELIGIBILITY_EXPIRED_SCHEMA,
        aggregate_id: id.as_uuid(),
        institution_id: locked.institution_id,
        student: Some(&student),
        from: Some(locked.status.as_str()),
        to: done.status.as_str(),
        version: done.version,
        reason: reason.as_deref(),
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        ELIG,
        "expire",
        id.as_uuid(),
        json!({"fromStatus": locked.status, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

/// Archival of an expired / not-eligible record (L3-C-068).
pub async fn archive(
    state: &StudentState,
    ctx: &AuthContext,
    id: ScholarshipEligibilityId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ScholarshipEligibility, KernelError> {
    let ElCmd { scope, .. } = load_el(state, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = tx!(state);
    let locked = lock_el(&mut tx, tenant, id, expected).await?;
    if !eligibility_can_transition(locked.status, ElStatus::Archived) {
        return Err(kit::illegal(
            "the scholarship eligibility",
            locked.status,
            ElStatus::Archived,
        ));
    }
    move_el(
        &mut tx,
        tenant,
        &locked,
        ElStatus::Archived,
        json!({"status_reason": reason}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        ELIG,
        "archive",
        id.as_uuid(),
        json!({"fromStatus": locked.status, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_eligibility(state, tenant, id).await
}

// ---- external sync + bulk recalculation ---------------------------------------------------------

pub async fn external_sync(
    state: &StudentState,
    ctx: &AuthContext,
    scholarship_id: ScholarshipDefinitionId,
    actor: &Actor,
) -> Result<ScholarshipExternalSync, KernelError> {
    let (s, scope) = load_def(state, ctx, scholarship_id).await?;
    let scheme = s.external_scheme.clone().ok_or_else(|| {
        kit::invalid(
            "externalScheme",
            "required",
            "the scholarship has no external scheme",
        )
    })?;
    let tenant = ctx.scope.tenant_id;
    let today = business_today(state, &scope, InstitutionId(s.institution_id)).await?;
    let students: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT student_id FROM student.scholarship_eligibility WHERE tenant_id = $1 AND scholarship_id = $2 AND status = 'active'",
    )
    .bind(tenant.as_uuid())
    .bind(scholarship_id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let outcome = state
        .external_scholarships
        .synchronise(
            &scope,
            ExternalScholarshipSync {
                institution_id: InstitutionId(s.institution_id),
                scholarship_id: scholarship_id.as_uuid(),
                external_scheme: scheme.clone(),
                student_ids: students.iter().copied().map(StudentId).collect(),
                as_of: today,
            },
        )
        .await?;
    let (kind, reference, detail) = match &outcome {
        ExternalSyncOutcome::Accepted { reference } => ("accepted", Some(reference.clone()), None),
        ExternalSyncOutcome::Rejected { reason } => ("rejected", None, Some(reason.clone())),
        ExternalSyncOutcome::NotConfigured => (
            "not_configured",
            None,
            Some("no external scholarship adapter is configured".to_owned()),
        ),
    };
    let mut tx = tx!(state);
    let id = Uuid::now_v7();
    kit::append(&mut tx, SYNC, json!({
        "id": id, "tenant_id": tenant.as_uuid(), "institution_id": s.institution_id, "scholarship_id": scholarship_id.as_uuid(),
        "external_scheme": scheme, "student_count": students.len(), "outcome": kind, "reference": reference,
        "detail": detail, "as_of": today, "actor": actor.id,
    })).await?;
    StatusEvent {
        event_type: SCHOLARSHIP_EXTERNAL_SYNCHRONISED,
        schema: SCHOLARSHIP_EXTERNAL_SYNCHRONISED_SCHEMA,
        aggregate_id: id,
        institution_id: s.institution_id,
        student: None,
        from: None,
        to: kind,
        version: 1,
        reason: detail.as_deref(),
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        SYNC,
        "synchronise",
        id,
        json!({"scholarshipId": scholarship_id, "outcome": kind, "students": students.len()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    kit::fetch(&state.pool, SYNC, "external sync", tenant, id).await
}

pub async fn list_syncs(
    state: &StudentState,
    tenant: TenantId,
    scholarship_id: ScholarshipDefinitionId,
) -> Result<Vec<ScholarshipExternalSync>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(x) FROM student.scholarship_external_sync x WHERE x.tenant_id = $1 AND x.scholarship_id = $2 ORDER BY x.created_at DESC LIMIT 200",
    )
    .bind(tenant.as_uuid())
    .bind(scholarship_id.as_uuid())
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

/// Recalculated status: never grants a recommendation or financial assistance (L4-C-070).
pub fn recalculated(from: ElStatus, outcome: EvaluationOutcome) -> ElStatus {
    match (from, outcome) {
        (_, EvaluationOutcome::NotEligible) => ElStatus::NotEligible,
        (ElStatus::Active, _)
        | (ElStatus::RecommendationGenerated, EvaluationOutcome::Eligible) => from,
        (_, EvaluationOutcome::ConditionallyEligible) => ElStatus::ConditionallyEligible,
        (_, EvaluationOutcome::Eligible) => ElStatus::Eligible,
    }
}

pub async fn recalculate(
    state: &StudentState,
    ctx: &AuthContext,
    institution_id: InstitutionId,
    cmd: NewScholarshipRecalculation,
    actor: &Actor,
) -> Result<ScholarshipRecalculation, KernelError> {
    let scope = kit::writable_institution(state, ctx, institution_id).await?;
    let tenant = ctx.scope.tenant_id;
    if let Some(sid) = cmd.scholarship_id {
        let s: Scholarship = kit::fetch(&state.pool, DEF, DEF_ENTITY, tenant, sid.as_uuid())
            .await
            .map_err(|_| kit::invalid("scholarshipId", "not_found", "unknown scholarship"))?;
        if s.institution_id != institution_id.as_uuid() {
            return Err(kit::invalid(
                "scholarshipId",
                "other_institution",
                "the scholarship belongs to another institution",
            ));
        }
    }
    let today = business_today(state, &scope, institution_id).await?;
    let recalc_id = ScholarshipRecalculationId::new();
    let mut tx = tx!(state);
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(e) FROM student.scholarship_eligibility e WHERE e.tenant_id = $1 AND e.institution_id = $2 \
         AND ($3::uuid IS NULL OR e.scholarship_id = $3) \
         AND e.status IN ('eligible', 'conditionally_eligible', 'recommendation_generated', 'active') \
         ORDER BY e.created_at LIMIT 1000 FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(cmd.scholarship_id.map(|s| s.as_uuid()))
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut evaluated = 0;
    let mut changes = Vec::new();
    let mut policies: BTreeMap<Uuid, Option<ScholarshipPolicy>> = BTreeMap::new();
    for row in rows {
        let e: ScholarshipEligibility = kit::decode(row)?;
        if let std::collections::btree_map::Entry::Vacant(slot) =
            policies.entry(e.scholarship_id.as_uuid())
        {
            let p = effective_policy(&mut *tx, tenant, e.scholarship_id.as_uuid())
                .await?
                .filter(|p| p.effective_from <= today);
            slot.insert(p);
        }
        let Some(Some(policy)) = policies.get(&e.scholarship_id.as_uuid()) else {
            continue;
        };
        let context = load_context(&mut *tx, tenant, e.student_context_id).await?;
        let ev = evaluation_of(
            policy,
            &e.facts,
            context.programme_id,
            context.current_term_no,
        );
        let to = recalculated(e.status, ev.outcome);
        evaluated += 1;
        let mut extra = json!({"evaluation": ev, "policy_id": policy.id.as_uuid()});
        if to == ElStatus::ConditionallyEligible && e.status != to {
            extra["conditions"] = json!(ev.conditions);
            extra["condition_deadline"] = json!(condition_deadline(policy, today));
        }
        let updated = move_el(&mut tx, tenant, &e, to, extra).await?;
        append_assessment(
            &mut tx,
            tenant,
            e.id.as_uuid(),
            "recalculation",
            Some(policy.id.as_uuid()),
            Some(e.status),
            to,
            &e.facts,
            Some(&ev),
            cmd.reason.as_deref(),
            Some(recalc_id.as_uuid()),
            actor,
        )
        .await?;
        if to != e.status {
            let student = load_student(&mut *tx, tenant, e.student_id).await?;
            el_event(
                &mut tx,
                &scope,
                (
                    SCHOLARSHIP_ELIGIBILITY_REASSESSED,
                    SCHOLARSHIP_ELIGIBILITY_REASSESSED_SCHEMA,
                ),
                &updated,
                &student,
                actor,
            )
            .await?;
            changes.push(RecalculationChange {
                eligibility_id: e.id,
                student_id: e.student_id,
                from_status: e.status,
                to_status: to,
            });
        }
    }
    let changed = changes.len();
    kit::append(&mut tx, RECALC, json!({
        "id": recalc_id.as_uuid(), "tenant_id": tenant.as_uuid(), "institution_id": institution_id.as_uuid(),
        "scholarship_id": cmd.scholarship_id.map(|s| s.as_uuid()), "reason": cmd.reason, "evaluated": evaluated,
        "changed": changed, "changes": changes, "actor": actor.id,
    })).await?;
    crate::shared::publish::event(
        &mut tx,
        &scope,
        SCHOLARSHIP_RECALCULATION_COMPLETED,
        SCHOLARSHIP_RECALCULATION_COMPLETED_SCHEMA,
        recalc_id.to_string(),
        &ScholarshipRecalculationV1 {
            recalculation_id: recalc_id.as_uuid(),
            institution_id: institution_id.as_uuid(),
            scholarship_id: cmd.scholarship_id.map(|s| s.as_uuid()),
            evaluated,
            changed: changed as i64,
        },
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        RECALC,
        "run",
        recalc_id.as_uuid(),
        json!({"evaluated": evaluated, "changed": changed}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_recalculation(state, tenant, recalc_id).await
}

pub async fn get_recalculation(
    state: &StudentState,
    tenant: TenantId,
    id: ScholarshipRecalculationId,
) -> Result<ScholarshipRecalculation, KernelError> {
    kit::fetch(
        &state.pool,
        RECALC,
        "scholarship recalculation",
        tenant,
        id.as_uuid(),
    )
    .await
}

pub async fn list_recalculations(
    state: &StudentState,
    tenant: TenantId,
    institution_id: InstitutionId,
) -> Result<Vec<ScholarshipRecalculation>, KernelError> {
    kit::list_by(
        &state.pool,
        RECALC,
        tenant,
        "institution_id",
        institution_id.as_uuid(),
        None,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recalculation_never_grants() {
        assert_eq!(
            recalculated(ElStatus::Active, EvaluationOutcome::NotEligible),
            ElStatus::NotEligible
        );
        assert_eq!(
            recalculated(ElStatus::Active, EvaluationOutcome::Eligible),
            ElStatus::Active
        );
        assert_eq!(
            recalculated(ElStatus::ConditionallyEligible, EvaluationOutcome::Eligible),
            ElStatus::Eligible
        );
        assert_eq!(
            recalculated(ElStatus::Eligible, EvaluationOutcome::Eligible),
            ElStatus::Eligible
        );
        assert_eq!(
            recalculated(
                ElStatus::RecommendationGenerated,
                EvaluationOutcome::Eligible
            ),
            ElStatus::RecommendationGenerated
        );
    }
}
