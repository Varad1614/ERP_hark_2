//! Learning opportunities (L3-D-040…042, 047): establishment against authoritative references
//! (Domain B term through the reference port, 4A offering through the offering read port, the
//! approved provider), governed capacity / reservation / waitlist / window, activation through
//! `LEARNING_OPPORTUNITY_APPROVAL`, suspension (no new enrolments, existing ones kept),
//! archival only without open obligations, and the D06 academic session archival.

use super::domain::opportunity_can_transition;
use super::kit::{
    self, begin, check_version, commit, history, illegal, invalid, require, row_scope,
    FlexibleHistoryEntry, StatusEvent,
};
use super::provider::{self, LearningProvider};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, NaiveDate, Utc};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::flexible::*;
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::academic::status::{
    CourseOfferingStatus, LearningMode, LearningOpportunityStatus as O, LearningProviderStatus,
};
use sutra_contracts::institution::status::PublicationStatus;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "learning_opportunity";
pub const ENTITY: &str = "learning opportunity";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LearningOpportunity {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub code: String,
    pub title: String,
    pub mode: LearningMode,
    pub provider_id: Option<Uuid>,
    pub offering_id: Option<Uuid>,
    pub academic_term_id: Uuid,
    pub credits: f64,
    pub min_hours: Option<f64>,
    pub capacity: i32,
    pub reserved_seats: i32,
    pub waitlist_enabled: bool,
    pub requires_approval: bool,
    pub audience: Option<String>,
    pub enrolment_opens_on: Option<NaiveDate>,
    pub enrolment_closes_on: Option<NaiveDate>,
    pub status: O,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub suspended_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub session_archived_at: Option<DateTime<Utc>>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Confirmed seat allocations.
    #[serde(default)]
    pub seats_allocated: i64,
    /// Queued (waitlisted) demand.
    #[serde(default)]
    pub waitlist_count: i64,
    /// Confirmed allocations exceed governed capacity (resolve through `resolve-conflicts`).
    #[serde(default)]
    pub over_allocated: bool,
    /// Provider name / offering code for display.
    #[serde(default)]
    pub provider_name: Option<String>,
    #[serde(default)]
    pub offering_code: Option<String>,
    #[serde(default)]
    pub history: Vec<FlexibleHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct LearningOpportunityList {
    pub items: Vec<LearningOpportunity>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewLearningOpportunity {
    pub code: String,
    pub title: String,
    pub mode: LearningMode,
    /// Required for every mode except `open_elective`.
    pub provider_id: Option<Uuid>,
    /// Required for `open_elective` (a 4A offering of the same institution and term).
    pub offering_id: Option<Uuid>,
    pub academic_term_id: Uuid,
    /// Defaults to the offering's credits for open electives; required otherwise.
    pub credits: Option<f64>,
    pub min_hours: Option<f64>,
    pub capacity: i32,
    #[serde(default)]
    pub reserved_seats: i32,
    pub waitlist_enabled: Option<bool>,
    #[serde(default)]
    pub requires_approval: bool,
    pub audience: Option<String>,
    pub enrolment_opens_on: Option<NaiveDate>,
    pub enrolment_closes_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OpportunityCapacity {
    pub capacity: i32,
    pub reserved_seats: Option<i32>,
    pub waitlist_enabled: Option<bool>,
    pub enrolment_opens_on: Option<NaiveDate>,
    pub enrolment_closes_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionArchival {
    pub academic_term_id: Uuid,
    /// Reference to the authoritative session-closure decision when the term has not ended yet.
    pub closure_evidence: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionArchivalResult {
    pub academic_term_id: Uuid,
    pub archived: Vec<Uuid>,
    pub already_archived: i64,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct OpportunityFilter {
    pub status: Option<O>,
    pub academic_term_id: Option<Uuid>,
}

async fn decorate(
    st: &AcademicState,
    tenant: TenantId,
    mut o: LearningOpportunity,
) -> Result<LearningOpportunity, KernelError> {
    let (alloc, wait): (i64, i64) = sqlx::query_as(
        "SELECT (SELECT count(*) FROM academic.flexible_seat_allocation a WHERE a.opportunity_id = $1 AND a.status = 'confirmed'), \
                (SELECT count(*) FROM academic.flexible_enrolment e WHERE e.opportunity_id = $1 AND e.status = 'waitlisted')",
    )
    .bind(o.id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    o.seats_allocated = alloc;
    o.waitlist_count = wait;
    o.over_allocated = alloc > i64::from(o.capacity - o.reserved_seats);
    if let Some(p) = o.provider_id {
        o.provider_name = sqlx::query_scalar(
            "SELECT name FROM academic.learning_provider WHERE id = $1 AND tenant_id = $2",
        )
        .bind(p)
        .bind(tenant.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
    }
    if let Some(off) = o.offering_id {
        o.offering_code = st
            .offerings
            .offering(&ExecutionScope::tenant(tenant), CourseOfferingId(off))
            .await?
            .map(|x| x.offering_code);
    }
    Ok(o)
}

pub async fn list(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    f: &OpportunityFilter,
) -> Result<Vec<LearningOpportunity>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.learning_opportunity t WHERE t.tenant_id = $1 AND t.institution_id = $2 \
         AND ($3::text IS NULL OR t.status = $3) AND ($4::uuid IS NULL OR t.academic_term_id = $4) ORDER BY t.code LIMIT 500",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(f.status.map(|s| s.as_str()))
    .bind(f.academic_term_id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(decorate(st, tenant, kit::decode(r)?).await?);
    }
    Ok(out)
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<LearningOpportunity, KernelError> {
    let o: LearningOpportunity = kit::fetch(&st.pool, TABLE, ENTITY, tenant, id).await?;
    let mut o = decorate(st, tenant, o).await?;
    o.history = kit::history_of(&st.pool, tenant, id).await?;
    Ok(o)
}

/// Active learning opportunities of the tenant (student self-service discovery).
pub async fn active_in_tenant(
    st: &AcademicState,
    tenant: TenantId,
) -> Result<Vec<LearningOpportunity>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.learning_opportunity t WHERE t.tenant_id = $1 AND t.status = 'active' ORDER BY t.code LIMIT 200",
    )
    .bind(tenant.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        out.push(decorate(st, tenant, kit::decode(r)?).await?);
    }
    Ok(out)
}

/// BH-002/003: the authoritative references of an opportunity (term of a published calendar,
/// the 4A offering for open electives, a non-offboarded provider whose scope covers the mode).
#[allow(clippy::too_many_arguments)]
async fn validate_refs(
    st: &AcademicState,
    ctx: &AuthContext,
    scope: &ExecutionScope,
    institution: Uuid,
    mode: LearningMode,
    provider_id: Option<Uuid>,
    offering_id: Option<Uuid>,
    term_id: Uuid,
    for_activation: bool,
) -> Result<Option<f64>, KernelError> {
    let term = st
        .references
        .academic_term(scope, term_id)
        .await?
        .filter(|t| t.institution_id.as_uuid() == institution)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "not_found",
                "academic term not found in this institution",
            )
        })?;
    if term.calendar_version_status != PublicationStatus::Published {
        return Err(invalid(
            "academicTermId",
            "unpublished",
            "the academic term belongs to an unpublished calendar version",
        ));
    }
    if mode == LearningMode::OpenElective {
        let off_id = offering_id.ok_or_else(|| {
            invalid(
                "offeringId",
                "required",
                "an open elective needs a course offering",
            )
        })?;
        let off = st
            .offerings
            .offering(&tenant_scope(ctx), CourseOfferingId(off_id))
            .await?
            .filter(|o| o.institution_id.as_uuid() == institution)
            .ok_or_else(|| {
                invalid(
                    "offeringId",
                    "not_found",
                    "course offering not found in this institution",
                )
            })?;
        if off.academic_term_id != term_id {
            return Err(invalid(
                "offeringId",
                "term_mismatch",
                "the course offering belongs to another academic term",
            ));
        }
        if off.status.is_terminal()
            || matches!(
                off.status,
                CourseOfferingStatus::Cancelled | CourseOfferingStatus::Completed
            )
        {
            return Err(KernelError::Conflict(format!(
                "the course offering is {}",
                off.status
            )));
        }
        if for_activation
            && !off.status.is_deliverable()
            && off.status != CourseOfferingStatus::Published
        {
            return Err(KernelError::Conflict(format!(
                "the course offering is {}; publish it before activating the elective",
                off.status
            )));
        }
        if provider_id.is_some() {
            return Err(invalid(
                "providerId",
                "invalid",
                "an open elective has no external provider",
            ));
        }
        Ok(Some(off.credits))
    } else {
        let pid = provider_id.ok_or_else(|| {
            invalid(
                "providerId",
                "required",
                "an external opportunity needs a learning provider",
            )
        })?;
        let p: LearningProvider = kit::fetch(
            &st.pool,
            provider::TABLE,
            "learning provider",
            ctx.scope.tenant_id,
            pid,
        )
        .await
        .map_err(|_| invalid("providerId", "not_found", "learning provider not found"))?;
        if p.institution_id != institution {
            return Err(invalid(
                "providerId",
                "not_found",
                "learning provider not found in this institution",
            ));
        }
        if matches!(
            p.status,
            LearningProviderStatus::Offboarded | LearningProviderStatus::Suspended
        ) || (for_activation && p.status != LearningProviderStatus::Active)
        {
            return Err(KernelError::Conflict(format!(
                "the learning provider is {}",
                p.status
            )));
        }
        if !p.modes.contains(&mode) {
            return Err(invalid(
                "mode",
                "out_of_scope",
                format!("the provider is not approved for {mode}"),
            ));
        }
        if offering_id.is_some() {
            return Err(invalid(
                "offeringId",
                "invalid",
                "only open electives reference a course offering",
            ));
        }
        Ok(None)
    }
}

pub async fn create(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: Uuid,
    cmd: NewLearningOpportunity,
    actor: &Actor,
) -> Result<LearningOpportunity, KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution));
    require_institution_writable(st, &scope, InstitutionId(institution)).await?;
    let code = kit::required_text("code", Some(&cmd.code))?;
    let title = kit::required_text("title", Some(&cmd.title))?;
    let off_credits = validate_refs(
        st,
        ctx,
        &scope,
        institution,
        cmd.mode,
        cmd.provider_id,
        cmd.offering_id,
        cmd.academic_term_id,
        false,
    )
    .await?;
    let credits = match (cmd.credits, off_credits) {
        (Some(c), _) => c,
        (None, Some(c)) => c,
        (None, None) => {
            return Err(invalid(
                "credits",
                "required",
                "credits are required for external opportunities",
            ))
        }
    };
    if !(0.0..=100.0).contains(&credits) {
        return Err(invalid(
            "credits",
            "range",
            "credits must be between 0 and 100",
        ));
    }
    if cmd.capacity < 0 || cmd.reserved_seats < 0 || cmd.reserved_seats > cmd.capacity {
        return Err(invalid(
            "reservedSeats",
            "range",
            "capacity and reserved seats must be ≥ 0 and reserved ≤ capacity",
        ));
    }
    if let (Some(a), Some(b)) = (cmd.enrolment_opens_on, cmd.enrolment_closes_on) {
        if a > b {
            return Err(invalid(
                "enrolmentClosesOn",
                "invalid",
                "the enrolment window closes before it opens",
            ));
        }
    }
    let dup: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM academic.learning_opportunity WHERE tenant_id = $1 AND institution_id = $2 \
         AND academic_term_id = $3 AND lower(code) = lower($4) AND status <> 'archived')",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(institution)
    .bind(cmd.academic_term_id)
    .bind(&code)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    require(
        !dup,
        format!("a learning opportunity {code} already exists in this term"),
    )?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let o: LearningOpportunity = kit::insert(&mut tx, TABLE, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": institution, "code": code, "title": title,
        "mode": cmd.mode.as_str(), "provider_id": cmd.provider_id, "offering_id": cmd.offering_id,
        "academic_term_id": cmd.academic_term_id, "credits": credits, "min_hours": cmd.min_hours,
        "capacity": cmd.capacity, "reserved_seats": cmd.reserved_seats,
        "waitlist_enabled": cmd.waitlist_enabled.unwrap_or(true), "requires_approval": cmd.requires_approval,
        "audience": kit::opt_text(cmd.audience.as_deref()),
        "enrolment_opens_on": cmd.enrolment_opens_on, "enrolment_closes_on": cmd.enrolment_closes_on,
    })).await?;
    history(
        &mut tx, tenant, TABLE, o.id, None, "draft", None, None, actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "create",
        o.id,
        json!({"code": o.code, "mode": o.mode}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, o.id).await
}

/// Moves a locked opportunity (history, audit, lifecycle event).
#[allow(clippy::too_many_arguments)]
pub async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    o: &LearningOpportunity,
    to: O,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<LearningOpportunity, KernelError> {
    if !opportunity_can_transition(o.status, to) {
        return Err(illegal(ENTITY, o.status, to));
    }
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": kit::now()});
    if let Some(pid) = pid {
        changes["process_instance_id"] = json!(pid);
    }
    match to {
        O::Suspended => changes["suspended_at"] = json!(kit::now()),
        O::Active => changes["suspended_at"] = serde_json::Value::Null,
        O::Archived => changes["archived_at"] = json!(kit::now()),
        _ => {}
    }
    let n: LearningOpportunity = kit::patch(conn, TABLE, tenant, o.id, changes).await?;
    history(
        conn,
        tenant,
        TABLE,
        o.id,
        Some(o.status.as_str()),
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
        o.id,
        json!({"from": o.status, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    let ev = match to {
        O::Active => Some((
            LEARNING_OPPORTUNITY_ACTIVATED,
            LEARNING_OPPORTUNITY_ACTIVATED_SCHEMA,
        )),
        O::Suspended => Some((
            LEARNING_OPPORTUNITY_SUSPENDED,
            LEARNING_OPPORTUNITY_SUSPENDED_SCHEMA,
        )),
        O::Archived => Some((
            LEARNING_OPPORTUNITY_ARCHIVED,
            LEARNING_OPPORTUNITY_ARCHIVED_SCHEMA,
        )),
        _ => None,
    };
    if let Some(ev) = ev {
        StatusEvent {
            aggregate_id: o.id,
            institution_id: o.institution_id,
            offering_id: o.offering_id,
            from: Some(o.status.as_str()),
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

pub async fn load(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
) -> Result<(LearningOpportunity, ExecutionScope), KernelError> {
    let o: LearningOpportunity =
        kit::fetch(&st.pool, TABLE, ENTITY, ctx.scope.tenant_id, id).await?;
    check_version(o.version, expected)?;
    let scope = row_scope(ctx, o.institution_id);
    require_institution_writable(st, &scope, InstitutionId(o.institution_id)).await?;
    Ok((o, scope))
}

/// BH-006/007: business readiness before activation.
async fn require_ready(
    st: &AcademicState,
    ctx: &AuthContext,
    scope: &ExecutionScope,
    o: &LearningOpportunity,
) -> Result<(), KernelError> {
    validate_refs(
        st,
        ctx,
        scope,
        o.institution_id,
        o.mode,
        o.provider_id,
        o.offering_id,
        o.academic_term_id,
        true,
    )
    .await?;
    require(
        o.capacity > o.reserved_seats,
        "the opportunity needs at least one unreserved seat before activation",
    )?;
    require(
        o.enrolment_opens_on.is_some() && o.enrolment_closes_on.is_some(),
        "the enrolment window (opens / closes) must be set before activation",
    )?;
    Ok(())
}

/// Submit for activation (`LEARNING_OPPORTUNITY_APPROVAL`); auto-approval activates.
pub async fn submit(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<LearningOpportunity, KernelError> {
    let (o, scope) = load(st, ctx, id, expected).await?;
    if !opportunity_can_transition(o.status, O::PendingApproval) {
        return Err(illegal(ENTITY, o.status, O::PendingApproval));
    }
    require_ready(st, ctx, &scope, &o).await?;
    let mut vars = BTreeMap::new();
    vars.insert("code".into(), json!(o.code));
    vars.insert("mode".into(), json!(o.mode));
    let outcome = approval::request_approval(
        st,
        &scope,
        process::LEARNING_OPPORTUNITY_APPROVAL,
        business_key(TABLE, o.id, o.version),
        vars,
    )
    .await?;
    let tenant = ctx.scope.tenant_id;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = begin(st).await?;
    let locked: LearningOpportunity = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let n = transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        O::PendingApproval,
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
            O::Active,
            Some("activation approved"),
            Some(&pid),
            &sys,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Explicit activation decision (BH-008: the system is not the decision-maker).
#[allow(clippy::too_many_arguments)]
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<LearningOpportunity, KernelError> {
    let (o, scope) = load(st, ctx, id, expected).await?;
    require(
        o.status == O::PendingApproval,
        format!(
            "the learning opportunity is {}; there is no pending activation decision",
            o.status
        ),
    )?;
    if approve {
        require_ready(st, ctx, &scope, &o).await?;
    } else if reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: LearningOpportunity = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        if approve { O::Active } else { O::Draft },
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Open D06 obligations of opportunities (L4-D-042-BH-002, L4-D-047-BH-002), as messages.
async fn open_obligations(
    conn: &mut PgConnection,
    tenant: TenantId,
    ids: &[Uuid],
) -> Result<Vec<String>, KernelError> {
    let (enr, rec, exc, rcn): (i64, i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM academic.flexible_enrolment WHERE tenant_id = $1 AND opportunity_id = ANY($2) \
              AND status NOT IN ('completed', 'withdrawn', 'rejected')), \
           (SELECT count(*) FROM academic.credit_recognition WHERE tenant_id = $1 AND opportunity_id = ANY($2) \
              AND status IN ('requested', 'under_review', 'approved')), \
           (SELECT count(*) FROM academic.flexible_exception_case WHERE tenant_id = $1 AND status <> 'closed' \
              AND (opportunity_id = ANY($2) OR enrolment_id IN (SELECT id FROM academic.flexible_enrolment WHERE opportunity_id = ANY($2)))), \
           (SELECT count(*) FROM academic.reconciliation_run WHERE tenant_id = $1 AND opportunity_id = ANY($2) \
              AND status IN ('open', 'discrepancies'))",
    )
    .bind(tenant.as_uuid())
    .bind(ids)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = Vec::new();
    if enr > 0 {
        out.push(format!("{enr} open flexible enrolments"));
    }
    if rec > 0 {
        out.push(format!(
            "{rec} credit recognitions not yet posted or decided"
        ));
    }
    if exc > 0 {
        out.push(format!("{exc} exception cases not closed"));
    }
    if rcn > 0 {
        out.push(format!("{rcn} unresolved reconciliation runs"));
    }
    Ok(out)
}

/// Suspend (reason required), reactivate (readiness re-checked) or archive (no open obligations).
#[allow(clippy::too_many_arguments)]
pub async fn command(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: O,
    reason: Option<String>,
    actor: &Actor,
) -> Result<LearningOpportunity, KernelError> {
    let (o, scope) = load(st, ctx, id, expected).await?;
    if !opportunity_can_transition(o.status, to) {
        return Err(illegal(ENTITY, o.status, to));
    }
    match to {
        O::Suspended if reason.is_none() => {
            return Err(invalid("reason", "required", "a suspension needs a reason"))
        }
        O::Active => require_ready(st, ctx, &scope, &o).await?,
        _ => {}
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: LearningOpportunity = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    if to == O::Archived {
        let open = open_obligations(&mut tx, tenant, &[id]).await?;
        require(
            open.is_empty(),
            format!(
                "the opportunity cannot be archived while obligations remain: {}",
                open.join(", ")
            ),
        )?;
    }
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

/// Capacity / reservation / waitlist / window change. Lowering capacity below the confirmed
/// allocations is allowed and surfaces as `overAllocated` until the conflict is resolved
/// (L3-D-048); allocations are never silently dropped.
pub async fn set_capacity(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: OpportunityCapacity,
    actor: &Actor,
) -> Result<LearningOpportunity, KernelError> {
    let (o, scope) = load(st, ctx, id, expected).await?;
    require(
        o.status != O::Archived,
        "an archived opportunity cannot change",
    )?;
    let reserved = cmd.reserved_seats.unwrap_or(o.reserved_seats);
    if cmd.capacity < 0 || reserved < 0 || reserved > cmd.capacity {
        return Err(invalid(
            "reservedSeats",
            "range",
            "capacity and reserved seats must be ≥ 0 and reserved ≤ capacity",
        ));
    }
    let opens = cmd.enrolment_opens_on.or(o.enrolment_opens_on);
    let closes = cmd.enrolment_closes_on.or(o.enrolment_closes_on);
    if let (Some(a), Some(b)) = (opens, closes) {
        if a > b {
            return Err(invalid(
                "enrolmentClosesOn",
                "invalid",
                "the enrolment window closes before it opens",
            ));
        }
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: LearningOpportunity = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    kit::patch::<LearningOpportunity>(
        &mut tx,
        TABLE,
        tenant,
        id,
        json!({
            "capacity": cmd.capacity, "reserved_seats": reserved,
            "waitlist_enabled": cmd.waitlist_enabled.unwrap_or(o.waitlist_enabled),
            "enrolment_opens_on": opens, "enrolment_closes_on": closes,
        }),
    )
    .await?;
    kit::audit(&mut tx, &scope, TABLE, "set_capacity", id, json!({"capacity": cmd.capacity, "reservedSeats": reserved, "previousCapacity": o.capacity}), actor).await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

/// D06 academic session archival (L3-D-047): validates the authoritative session-closure fact
/// (term ended in the business timezone, or a referenced closure decision), refuses while any
/// D06 obligation of the term remains, then moves only D06-owned opportunities of the term to
/// the archived/historical position. The Domain B term itself is never touched.
pub async fn archive_session(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: Uuid,
    cmd: SessionArchival,
    actor: &Actor,
) -> Result<SessionArchivalResult, KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution));
    require_institution_writable(st, &scope, InstitutionId(institution)).await?;
    let term = st
        .references
        .academic_term(&scope, cmd.academic_term_id)
        .await?
        .filter(|t| t.institution_id.as_uuid() == institution)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "not_found",
                "academic term not found in this institution",
            )
        })?;
    let today = business_today(st, &scope, InstitutionId(institution)).await?;
    let evidence = kit::opt_text(cmd.closure_evidence.as_deref());
    require(
        term.ends_on < today || evidence.is_some(),
        format!("the academic session {} ends on {}; archival needs the session-closure fact (closureEvidence)", term.code, term.ends_on),
    )?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.learning_opportunity t WHERE t.tenant_id = $1 AND t.institution_id = $2 \
         AND t.academic_term_id = $3 ORDER BY t.code FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(cmd.academic_term_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let opps: Vec<LearningOpportunity> = rows
        .into_iter()
        .map(kit::decode)
        .collect::<Result<_, _>>()?;
    let ids: Vec<Uuid> = opps.iter().map(|o| o.id).collect();
    let open = open_obligations(&mut tx, tenant, &ids).await?;
    require(
        open.is_empty(),
        format!(
            "the session cannot be archived while D06 obligations remain: {}",
            open.join(", ")
        ),
    )?;
    let reason = cmd
        .reason
        .clone()
        .or_else(|| Some(format!("academic session {} archived", term.code)));
    let mut archived = Vec::new();
    let mut already = 0;
    for o in &opps {
        if o.status == O::Archived {
            already += 1;
            continue;
        }
        // pending approvals are returned first: archival never bypasses a pending decision
        require(
            o.status != O::PendingApproval,
            format!("opportunity {} has a pending activation decision", o.code),
        )?;
        let n = transition(
            &mut tx,
            &scope,
            tenant,
            o,
            O::Archived,
            reason.as_deref(),
            None,
            actor,
        )
        .await?;
        kit::patch::<LearningOpportunity>(
            &mut tx,
            TABLE,
            tenant,
            n.id,
            json!({"session_archived_at": kit::now()}),
        )
        .await?;
        archived.push(o.id);
    }
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "archive_session",
        cmd.academic_term_id,
        json!({"archived": archived.len(), "closureEvidence": evidence}),
        actor,
    )
    .await?;
    commit(tx).await?;
    Ok(SessionArchivalResult {
        academic_term_id: cmd.academic_term_id,
        archived,
        already_archived: already,
    })
}
