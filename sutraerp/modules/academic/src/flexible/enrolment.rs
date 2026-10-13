//! Flexible enrolments (L3-D-034…038, 048): governed request → eligibility (policy path or
//! `FLEXIBLE_ENROLMENT_APPROVAL`) → seat allocation or waitlist (never above governed capacity)
//! → external registration (provider acknowledgement is evidence) → learning progress →
//! recorded learning completion (no automatic credit). Section C registration is never touched.

use super::domain::{
    self, allocate, enrolment_can_transition, over_allocated, AllocationOutcome, Demand,
    FlexiblePolicyRules,
};
use super::kit::{
    self, begin, check_version, commit, history, illegal, invalid, require, row_scope,
    FlexibleHistoryEntry, StatusEvent,
};
use super::opportunity::{self, LearningOpportunity};
use super::policy;
use super::provider::{self, LearningProvider};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::time::business_today;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde_json::{json, Value};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::flexible::*;
use sutra_contracts::academic::status::{
    FlexibleEnrolmentStatus as E, LearningMode, LearningOpportunityStatus, LearningProviderStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::StudentContextStatus;
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

pub const TABLE: &str = "flexible_enrolment";
pub const ENTITY: &str = "flexible enrolment";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExternalRegistration {
    pub id: Uuid,
    pub enrolment_id: Uuid,
    pub provider_id: Uuid,
    /// `requested` | `accepted` | `rejected` | `recovery_required`.
    pub status: String,
    pub provider_reference: Option<String>,
    pub evidence_key: Option<String>,
    pub outcome_reason: Option<String>,
    pub requested_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LearningCompletion {
    pub id: Uuid,
    pub enrolment_id: Uuid,
    pub hours_completed: f64,
    /// `passed` | `failed` | `not_applicable`.
    pub assessment_status: String,
    pub grade: Option<String>,
    pub provider_evidence_ref: Option<String>,
    pub completed_on: NaiveDate,
    pub source_key: Option<String>,
    pub recorded_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeatAllocation {
    pub id: Uuid,
    pub opportunity_id: Uuid,
    pub enrolment_id: Uuid,
    /// `allocation` | `waitlist_promotion` | `conflict_resolution`.
    pub source: String,
    /// `confirmed` | `released`.
    pub status: String,
    pub release_reason: Option<String>,
    pub released_at: Option<DateTime<Utc>>,
    pub allocated_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlexibleEnrolment {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub opportunity_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub policy_id: Option<Uuid>,
    pub priority: i32,
    pub status: E,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    #[schema(value_type = Object)]
    pub eligibility: Value,
    pub waitlisted_at: Option<DateTime<Utc>>,
    pub waitlist_expires_on: Option<NaiveDate>,
    pub allocated_at: Option<DateTime<Utc>>,
    pub hours_completed: Option<f64>,
    pub source_key: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// Queue position among waitlisted demand (1 = next), when waitlisted.
    #[serde(default)]
    pub waitlist_position: Option<i64>,
    #[serde(default)]
    pub opportunity_code: Option<String>,
    #[serde(default)]
    pub opportunity_title: Option<String>,
    #[serde(default)]
    pub registrations: Vec<ExternalRegistration>,
    #[serde(default)]
    pub allocations: Vec<SeatAllocation>,
    #[serde(default)]
    pub completion: Option<LearningCompletion>,
    #[serde(default)]
    pub history: Vec<FlexibleHistoryEntry>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FlexibleEnrolmentList {
    pub items: Vec<FlexibleEnrolment>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewFlexibleEnrolment {
    pub student_id: Uuid,
    /// Lower = higher priority (default 100).
    pub priority: Option<i32>,
    /// Authoritative fact behind the request; a repeat returns the existing enrolment.
    pub source_key: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelfFlexibleRequest {
    pub opportunity_id: Uuid,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationOutcome {
    /// `accepted` | `rejected` | `recovery_required`.
    pub outcome: String,
    pub provider_reference: Option<String>,
    /// Provider acknowledgement id (a repeat is the same fact).
    pub evidence_key: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LearningProgress {
    pub hours_completed: f64,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewLearningCompletion {
    pub hours_completed: f64,
    /// `passed` | `failed` | `not_applicable`.
    pub assessment_status: String,
    pub grade: Option<String>,
    /// Provider completion evidence (required for external opportunities).
    pub provider_evidence_ref: Option<String>,
    pub completed_on: Option<NaiveDate>,
    pub source_key: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AllocationRun {
    pub opportunity: Option<LearningOpportunity>,
    pub allocated: Vec<Uuid>,
    pub waitlisted: Vec<Uuid>,
    pub unallocated: Vec<Uuid>,
    /// Waitlist entries whose validity ended (no allocation).
    pub expired: Vec<Uuid>,
    /// Waitlist entries no longer eligible on revalidation.
    pub ineligible: Vec<Uuid>,
    /// Allocations released by conflict resolution.
    pub released: Vec<Uuid>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct EnrolmentFilter {
    pub status: Option<E>,
}

// ------------------------------------------------------------------ reads

async fn decorate(
    conn: &mut PgConnection,
    tenant: TenantId,
    mut e: FlexibleEnrolment,
    full: bool,
) -> Result<FlexibleEnrolment, KernelError> {
    let (code, title): (String, String) =
        sqlx::query_as("SELECT code, title FROM academic.learning_opportunity WHERE id = $1")
            .bind(e.opportunity_id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
    e.opportunity_code = Some(code);
    e.opportunity_title = Some(title);
    if e.status == E::Waitlisted {
        e.waitlist_position = Some(
            sqlx::query_scalar(
                "SELECT count(*) + 1 FROM academic.flexible_enrolment o WHERE o.opportunity_id = $1 AND o.status = 'waitlisted' \
                 AND (o.priority, o.waitlisted_at, o.id) < ($2, $3, $4)",
            )
            .bind(e.opportunity_id)
            .bind(e.priority)
            .bind(e.waitlisted_at)
            .bind(e.id)
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?,
        );
    }
    if full {
        e.registrations = kit::list_by(
            &mut *conn,
            "external_registration",
            tenant,
            "enrolment_id",
            e.id,
            None,
            "t.created_at",
            50,
        )
        .await?;
        e.allocations = kit::list_by(
            &mut *conn,
            "flexible_seat_allocation",
            tenant,
            "enrolment_id",
            e.id,
            None,
            "t.created_at",
            50,
        )
        .await?;
        e.completion = kit::list_by::<LearningCompletion, _>(
            &mut *conn,
            "learning_completion",
            tenant,
            "enrolment_id",
            e.id,
            None,
            "t.created_at",
            1,
        )
        .await?
        .into_iter()
        .next();
        e.history = kit::history_of(&mut *conn, tenant, e.id).await?;
    }
    Ok(e)
}

pub async fn get(
    st: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<FlexibleEnrolment, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let e: FlexibleEnrolment = kit::fetch(&mut *conn, TABLE, ENTITY, tenant, id).await?;
    decorate(&mut conn, tenant, e, true).await
}

pub async fn list_for_opportunity(
    st: &AcademicState,
    tenant: TenantId,
    opportunity: Uuid,
    status: Option<&str>,
) -> Result<Vec<FlexibleEnrolment>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<FlexibleEnrolment> = kit::list_by(
        &mut *conn,
        TABLE,
        tenant,
        "opportunity_id",
        opportunity,
        status,
        "t.priority, t.created_at",
        500,
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for e in rows {
        out.push(decorate(&mut conn, tenant, e, false).await?);
    }
    Ok(out)
}

pub async fn list_for_student(
    st: &AcademicState,
    tenant: TenantId,
    student: Uuid,
) -> Result<Vec<FlexibleEnrolment>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows: Vec<FlexibleEnrolment> = kit::list_by(
        &mut *conn,
        TABLE,
        tenant,
        "student_id",
        student,
        None,
        "t.created_at DESC",
        200,
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for e in rows {
        out.push(decorate(&mut conn, tenant, e, true).await?);
    }
    Ok(out)
}

// ------------------------------------------------------------------ transitions

/// Moves a locked enrolment; emits the contract event for requested / allocated / waitlisted /
/// registered / completed.
#[allow(clippy::too_many_arguments)]
pub async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    e: &FlexibleEnrolment,
    to: E,
    reason: Option<&str>,
    pid: Option<&str>,
    extra: Value,
    actor: &Actor,
) -> Result<FlexibleEnrolment, KernelError> {
    if !enrolment_can_transition(e.status, to) {
        return Err(illegal(ENTITY, e.status, to));
    }
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": kit::now()});
    if let Some(pid) = pid {
        changes["process_instance_id"] = json!(pid);
    }
    if let (Value::Object(c), Value::Object(x)) = (&mut changes, extra) {
        c.extend(x);
    }
    let n: FlexibleEnrolment = kit::patch(conn, TABLE, tenant, e.id, changes).await?;
    history(
        conn,
        tenant,
        TABLE,
        e.id,
        Some(e.status.as_str()),
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
        e.id,
        json!({"from": e.status, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    emit(conn, scope, &n, Some(e.status.as_str()), reason, pid, actor).await?;
    Ok(n)
}

async fn emit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    n: &FlexibleEnrolment,
    from: Option<&str>,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let ev = match n.status {
        E::Requested => Some((
            FLEXIBLE_ENROLMENT_REQUESTED,
            FLEXIBLE_ENROLMENT_REQUESTED_SCHEMA,
        )),
        E::Allocated => Some((
            FLEXIBLE_ENROLMENT_ALLOCATED,
            FLEXIBLE_ENROLMENT_ALLOCATED_SCHEMA,
        )),
        E::Waitlisted => Some((
            FLEXIBLE_ENROLMENT_WAITLISTED,
            FLEXIBLE_ENROLMENT_WAITLISTED_SCHEMA,
        )),
        E::Registered => Some((
            FLEXIBLE_ENROLMENT_REGISTERED,
            FLEXIBLE_ENROLMENT_REGISTERED_SCHEMA,
        )),
        E::Completed => Some((
            FLEXIBLE_ENROLMENT_COMPLETED,
            FLEXIBLE_ENROLMENT_COMPLETED_SCHEMA,
        )),
        _ => None,
    };
    if let Some(ev) = ev {
        StatusEvent {
            aggregate_id: n.id,
            institution_id: n.institution_id,
            student_id: Some(n.student_id),
            from,
            to: n.status.as_str(),
            version: n.version,
            reason,
            process_instance_id: pid.or(n.process_instance_id.as_deref()),
            student_subject_ref: n.subject_ref.as_deref(),
            ..Default::default()
        }
        .emit(conn, scope, ev, actor)
        .await?;
    }
    Ok(())
}

async fn load(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<(FlexibleEnrolment, LearningOpportunity, ExecutionScope), KernelError> {
    let e: FlexibleEnrolment = kit::fetch(&st.pool, TABLE, ENTITY, ctx.scope.tenant_id, id).await?;
    let o: LearningOpportunity = kit::fetch(
        &st.pool,
        opportunity::TABLE,
        opportunity::ENTITY,
        ctx.scope.tenant_id,
        e.opportunity_id,
    )
    .await?;
    let scope = row_scope(ctx, e.institution_id);
    require_institution_writable(st, &scope, InstitutionId(e.institution_id)).await?;
    Ok((e, o, scope))
}

// ------------------------------------------------------------------ request (L3-D-034)

/// Establishes a request for `student_id` (admin on behalf, or the student via self-service):
/// validates the opportunity (active, window, provider), the student's live context, the
/// effective policy (concurrency + credit limits) and duplicates; then the governed approval:
/// the policy path (no human decision) or `FLEXIBLE_ENROLMENT_APPROVAL` when review is required.
pub async fn request(
    st: &AcademicState,
    ctx: &AuthContext,
    opportunity_id: Uuid,
    student_id: Uuid,
    priority: Option<i32>,
    source_key: Option<String>,
    actor: &Actor,
) -> Result<(FlexibleEnrolment, bool), KernelError> {
    let tenant = ctx.scope.tenant_id;
    let source_key = kit::opt_text(source_key.as_deref());
    if let Some(k) = &source_key {
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM academic.flexible_enrolment WHERE tenant_id = $1 AND source_key = $2",
        )
        .bind(tenant.as_uuid())
        .bind(k)
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
        if let Some(id) = existing {
            return Ok((get(st, tenant, id).await?, false));
        }
    }
    let o: LearningOpportunity = kit::fetch(
        &st.pool,
        opportunity::TABLE,
        opportunity::ENTITY,
        tenant,
        opportunity_id,
    )
    .await?;
    let scope = row_scope(ctx, o.institution_id);
    require_institution_writable(st, &scope, InstitutionId(o.institution_id)).await?;
    // BH-002 window / availability (L4-D-041-BH-004: none while suspended)
    require(
        o.status == LearningOpportunityStatus::Active,
        format!(
            "the learning opportunity is {}; it accepts no new enrolment",
            o.status
        ),
    )?;
    let today = business_today(st, &scope, InstitutionId(o.institution_id)).await?;
    require(
        o.enrolment_opens_on.is_none_or(|d| d <= today)
            && o.enrolment_closes_on.is_none_or(|d| d >= today),
        "the enrolment window of this opportunity is not open",
    )?;
    if let Some(pid) = o.provider_id {
        let p: LearningProvider =
            kit::fetch(&st.pool, provider::TABLE, "learning provider", tenant, pid).await?;
        require(
            p.status == LearningProviderStatus::Active,
            format!(
                "the learning provider is {}; it receives no new enrolments",
                p.status
            ),
        )?;
    }
    let student = st
        .students
        .student(&ExecutionScope::tenant(tenant), StudentId(student_id))
        .await?
        .ok_or_else(|| invalid("studentId", "not_found", "student not found"))?;
    let context = st
        .students
        .live_context(&scope, student.id, InstitutionId(o.institution_id))
        .await?
        .filter(|c| c.status == StudentContextStatus::Active)
        .ok_or_else(|| {
            KernelError::Conflict("the student has no active context in this institution".into())
        })?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let pol = policy::effective(&mut conn, tenant, o.institution_id)
        .await?
        .ok_or_else(|| {
            KernelError::Conflict(
                "no flexible-learning policy is effective for this institution".into(),
            )
        })?;
    let (live, credits): (i64, f64) = sqlx::query_as(
        "SELECT count(*), COALESCE(sum(o.credits) FILTER (WHERE o.academic_term_id = $3), 0)::float8 \
         FROM academic.flexible_enrolment e JOIN academic.learning_opportunity o ON o.id = e.opportunity_id \
         WHERE e.tenant_id = $1 AND e.student_id = $2 AND e.status NOT IN ('completed', 'withdrawn', 'rejected')",
    )
    .bind(tenant.as_uuid())
    .bind(student_id)
    .bind(o.academic_term_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let dup: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM academic.flexible_enrolment WHERE opportunity_id = $1 AND student_id = $2 \
         AND status NOT IN ('withdrawn', 'rejected'))",
    )
    .bind(opportunity_id)
    .bind(student_id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    drop(conn);
    require(
        !dup,
        "the student already has a live request for this opportunity",
    )?;
    require(
        live < i64::from(pol.rules.max_concurrent_enrolments),
        format!(
            "the policy allows at most {} live flexible enrolments",
            pol.rules.max_concurrent_enrolments
        ),
    )?;
    require(
        credits + o.credits <= pol.rules.max_credits_per_term,
        format!(
            "the policy allows at most {} flexible credits per term",
            pol.rules.max_credits_per_term
        ),
    )?;
    let human = o.requires_approval || pol.rules.require_human_approval;
    let id = Uuid::now_v7();
    let outcome = if human {
        let mut vars = BTreeMap::new();
        vars.insert("opportunityCode".into(), json!(o.code));
        vars.insert("studentNumber".into(), json!(student.student_number));
        Some(
            approval::request_approval(
                st,
                &scope,
                process::FLEXIBLE_ENROLMENT_APPROVAL,
                business_key(TABLE, id, 1),
                vars,
            )
            .await?,
        )
    } else {
        None
    };
    let mut tx = begin(st).await?;
    let e: FlexibleEnrolment = kit::insert(&mut tx, TABLE, json!({
        "id": id, "tenant_id": tenant.as_uuid(), "institution_id": o.institution_id, "opportunity_id": o.id,
        "student_id": student_id, "student_context_id": context.id.as_uuid(), "student_number": student.student_number,
        "display_name": student.display_name, "subject_ref": student.subject_ref, "policy_id": pol.id,
        "priority": priority.unwrap_or(100), "source_key": source_key,
        "process_instance_id": outcome.as_ref().map(|o| o.process_instance_id()),
        "eligibility": {"contextId": context.id.as_uuid(), "policyVersion": pol.version_no, "checkedOn": today,
                         "liveEnrolments": live, "termCredits": credits},
    })).await?;
    history(
        &mut tx,
        tenant,
        TABLE,
        id,
        None,
        "requested",
        None,
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TABLE,
        "request",
        id,
        json!({"opportunityId": o.id, "studentId": student_id}),
        actor,
    )
    .await?;
    emit(&mut tx, &scope, &e, None, None, None, actor).await?;
    let sys = Actor::system(actor.correlation_id.clone());
    match &outcome {
        None => {
            transition(
                &mut tx,
                &scope,
                tenant,
                &e,
                E::Eligible,
                Some("approved by policy (no human review required)"),
                None,
                json!({}),
                &sys,
            )
            .await?;
        }
        Some(ApprovalOutcome::Approved {
            process_instance_id,
        }) => {
            transition(
                &mut tx,
                &scope,
                tenant,
                &e,
                E::Eligible,
                Some("enrolment approved"),
                Some(process_instance_id),
                json!({}),
                &sys,
            )
            .await?;
        }
        Some(ApprovalOutcome::Pending { .. }) => {}
    }
    commit(tx).await?;
    Ok((get(st, tenant, id).await?, true))
}

/// Explicit academic decision on a pending request (approve → eligible, reject → rejected).
pub async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<FlexibleEnrolment, KernelError> {
    let (e, _o, scope) = load(st, ctx, id).await?;
    check_version(e.version, expected)?;
    require(
        e.status == E::Requested,
        format!(
            "the flexible enrolment is {}; there is no pending decision",
            e.status
        ),
    )?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleEnrolment = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        if approve { E::Eligible } else { E::Rejected },
        reason.as_deref(),
        None,
        json!({}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

async fn release_allocation(
    conn: &mut PgConnection,
    enrolment: Uuid,
    reason: &str,
) -> Result<(), KernelError> {
    sqlx::query("UPDATE academic.flexible_seat_allocation SET status = 'released', release_reason = $2, released_at = now() WHERE enrolment_id = $1 AND status = 'confirmed'")
        .bind(enrolment)
        .bind(reason)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// Withdrawal by the student (policy-gated) or authorised removal by staff (reason required);
/// a held seat is released, queue history is kept (L4-D-036-BH-007).
#[allow(clippy::too_many_arguments)]
pub async fn withdraw(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    by_student: bool,
    actor: &Actor,
) -> Result<FlexibleEnrolment, KernelError> {
    let (e, _o, scope) = load(st, ctx, id).await?;
    check_version(e.version, expected)?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    if by_student {
        let rules: Option<FlexiblePolicyRules> =
            policy::rules_of(&mut tx, tenant, e.policy_id).await?;
        if let Some(r) = rules {
            require(
                r.allows_withdrawal(e.status),
                format!(
                    "the policy does not allow withdrawal once the enrolment is {}",
                    e.status
                ),
            )?;
        }
    } else if reason.is_none() {
        return Err(invalid("reason", "required", "a removal needs a reason"));
    }
    let locked: FlexibleEnrolment = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let why = reason.unwrap_or_else(|| "withdrawn by the student".into());
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        E::Withdrawn,
        Some(&why),
        None,
        json!({}),
        actor,
    )
    .await?;
    release_allocation(&mut tx, id, &why).await?;
    commit(tx).await?;
    get(st, tenant, id).await
}

// ------------------------------------------------------------------ allocation (L3-D-035/036/048)

async fn eligible_demand(
    conn: &mut PgConnection,
    opportunity: Uuid,
    status: &str,
) -> Result<Vec<FlexibleEnrolment>, KernelError> {
    let rows: Vec<Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.flexible_enrolment t WHERE t.opportunity_id = $1 AND t.status = $2 \
         ORDER BY t.priority, COALESCE(t.waitlisted_at, t.created_at), t.id FOR UPDATE",
    )
    .bind(opportunity)
    .bind(status)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

async fn confirmed(conn: &mut PgConnection, opportunity: Uuid) -> Result<i64, KernelError> {
    sqlx::query_scalar("SELECT count(*) FROM academic.flexible_seat_allocation WHERE opportunity_id = $1 AND status = 'confirmed'")
        .bind(opportunity)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn seat(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    e: &FlexibleEnrolment,
    source: &str,
    reason: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let n = transition(
        conn,
        scope,
        tenant,
        e,
        E::Allocated,
        Some(reason),
        None,
        json!({"allocated_at": kit::now()}),
        actor,
    )
    .await?;
    kit::append(
        conn,
        "flexible_seat_allocation",
        json!({
            "tenant_id": tenant.as_uuid(), "opportunity_id": n.opportunity_id, "enrolment_id": n.id,
            "source": source, "allocated_by": actor.id,
        }),
    )
    .await
}

async fn waitlist_days(
    conn: &mut PgConnection,
    tenant: TenantId,
    e: &FlexibleEnrolment,
) -> Result<i64, KernelError> {
    Ok(policy::rules_of(conn, tenant, e.policy_id)
        .await?
        .map_or(30, |r| i64::from(r.waitlist_validity_days)))
}

/// Locks the opportunity for a seat command (If-Match on the opportunity; bumps its version).
async fn lock_opportunity(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
) -> Result<LearningOpportunity, KernelError> {
    let o: LearningOpportunity =
        kit::lock(conn, opportunity::TABLE, opportunity::ENTITY, tenant, id).await?;
    check_version(o.version, expected)?;
    kit::patch::<LearningOpportunity>(
        conn,
        opportunity::TABLE,
        tenant,
        id,
        json!({"status": o.status.as_str()}),
    )
    .await?;
    Ok(o)
}

/// Seat allocation run over eligible demand (L4-D-035): never above governed capacity, highest
/// priority first, at most one allocation per demand; the rest is waitlisted (or gets the
/// no-seat outcome when the waitlist is disabled).
pub async fn run_allocation(
    st: &AcademicState,
    ctx: &AuthContext,
    opportunity_id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<AllocationRun, KernelError> {
    let (o, scope) = opportunity::load(st, ctx, opportunity_id, expected).await?;
    require(
        o.status == LearningOpportunityStatus::Active,
        format!(
            "the learning opportunity is {}; allocation needs it active",
            o.status
        ),
    )?;
    let today = business_today(st, &scope, InstitutionId(o.institution_id)).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let o = lock_opportunity(&mut tx, tenant, opportunity_id, expected).await?;
    let taken = confirmed(&mut tx, o.id).await?;
    require(
        taken <= i64::from(o.capacity - o.reserved_seats),
        "allocations exceed capacity; resolve the allocation conflict first",
    )?;
    let demand = eligible_demand(&mut tx, o.id, "eligible").await?;
    let by_id: BTreeMap<Uuid, FlexibleEnrolment> =
        demand.iter().map(|e| (e.id, e.clone())).collect();
    let plan = allocate(
        o.capacity,
        o.reserved_seats,
        taken,
        o.waitlist_enabled,
        demand
            .iter()
            .map(|e| Demand {
                enrolment_id: e.id,
                priority: e.priority,
                order_key: e.created_at.timestamp_micros(),
            })
            .collect(),
    );
    let mut run = AllocationRun::default();
    for (id, outcome) in plan {
        let e = &by_id[&id];
        match outcome {
            AllocationOutcome::Allocate => {
                seat(
                    &mut tx,
                    &scope,
                    tenant,
                    e,
                    "allocation",
                    "seat allocated",
                    actor,
                )
                .await?;
                run.allocated.push(id);
            }
            AllocationOutcome::Waitlist => {
                let days = waitlist_days(&mut tx, tenant, e).await?;
                transition(&mut tx, &scope, tenant, e, E::Waitlisted, Some("capacity exhausted: waitlisted"), None,
                    json!({"waitlisted_at": kit::now(), "waitlist_expires_on": today + Duration::days(days)}), actor).await?;
                run.waitlisted.push(id);
            }
            AllocationOutcome::Unallocated => {
                transition(
                    &mut tx,
                    &scope,
                    tenant,
                    e,
                    E::Rejected,
                    Some("unallocated: capacity exhausted and no waitlist"),
                    None,
                    json!({}),
                    actor,
                )
                .await?;
                run.unallocated.push(id);
            }
        }
    }
    kit::audit(&mut tx, &scope, opportunity::TABLE, "allocate", o.id, json!({"allocated": run.allocated.len(), "waitlisted": run.waitlisted.len(), "unallocated": run.unallocated.len()}), actor).await?;
    commit(tx).await?;
    run.opportunity = Some(opportunity::get(st, tenant, opportunity_id).await?);
    Ok(run)
}

/// Waitlist promotion (L4-D-036): expired entries are recorded Expired (withdrawn, no
/// allocation); continuing eligibility is revalidated; only the next eligible entries are
/// promoted into free seats; queued demand never consumes capacity.
pub async fn promote_waitlist(
    st: &AcademicState,
    ctx: &AuthContext,
    opportunity_id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<AllocationRun, KernelError> {
    let (o, scope) = opportunity::load(st, ctx, opportunity_id, expected).await?;
    require(
        o.status == LearningOpportunityStatus::Active,
        format!(
            "the learning opportunity is {}; promotion needs it active",
            o.status
        ),
    )?;
    let today = business_today(st, &scope, InstitutionId(o.institution_id)).await?;
    let tenant = ctx.scope.tenant_id;
    let queue = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        eligible_demand_ro(&mut conn, o.id).await?
    };
    // revalidate continuing eligibility outside the transaction (port reads)
    let mut still_eligible = BTreeMap::new();
    for e in &queue {
        let ok = st
            .students
            .live_context(
                &scope,
                StudentId(e.student_id),
                InstitutionId(o.institution_id),
            )
            .await?
            .is_some_and(|c| c.status == StudentContextStatus::Active);
        still_eligible.insert(e.id, ok);
    }
    let mut tx = begin(st).await?;
    let o = lock_opportunity(&mut tx, tenant, opportunity_id, expected).await?;
    let mut free = domain::free_seats(
        o.capacity,
        o.reserved_seats,
        confirmed(&mut tx, o.id).await?,
    );
    let mut run = AllocationRun::default();
    for e in eligible_demand(&mut tx, o.id, "waitlisted").await? {
        if e.waitlist_expires_on.is_some_and(|d| d < today) {
            transition(
                &mut tx,
                &scope,
                tenant,
                &e,
                E::Withdrawn,
                Some("waitlist entry expired"),
                None,
                json!({}),
                actor,
            )
            .await?;
            run.expired.push(e.id);
            continue;
        }
        if free == 0 {
            continue;
        }
        if !still_eligible.get(&e.id).copied().unwrap_or(false) {
            transition(
                &mut tx,
                &scope,
                tenant,
                &e,
                E::Rejected,
                Some("no longer eligible on promotion"),
                None,
                json!({}),
                actor,
            )
            .await?;
            run.ineligible.push(e.id);
            continue;
        }
        seat(
            &mut tx,
            &scope,
            tenant,
            &e,
            "waitlist_promotion",
            "promoted from the waitlist",
            actor,
        )
        .await?;
        run.allocated.push(e.id);
        free -= 1;
    }
    kit::audit(&mut tx, &scope, opportunity::TABLE, "promote_waitlist", o.id, json!({"allocated": run.allocated.len(), "expired": run.expired.len(), "ineligible": run.ineligible.len()}), actor).await?;
    commit(tx).await?;
    run.opportunity = Some(opportunity::get(st, tenant, opportunity_id).await?);
    Ok(run)
}

async fn eligible_demand_ro(
    conn: &mut PgConnection,
    opportunity: Uuid,
) -> Result<Vec<FlexibleEnrolment>, KernelError> {
    let rows: Vec<Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM academic.flexible_enrolment t WHERE t.opportunity_id = $1 AND t.status = 'waitlisted'")
        .bind(opportunity)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

/// Allocation conflict resolution (L4-D-048): reassesses confirmed allocations against
/// governed capacity; releases the lowest-priority / latest ones (duplicates first) back to the
/// waitlist (or the unallocated outcome without a waitlist); the released allocation rows stay
/// as conflict evidence. Registered or further-progressed participations are never released.
pub async fn resolve_conflicts(
    st: &AcademicState,
    ctx: &AuthContext,
    opportunity_id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<AllocationRun, KernelError> {
    let (o0, scope) = opportunity::load(st, ctx, opportunity_id, expected).await?;
    let today = business_today(st, &scope, InstitutionId(o0.institution_id)).await?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let o = lock_opportunity(&mut tx, tenant, opportunity_id, expected).await?;
    let rows: Vec<(Uuid, i32, DateTime<Utc>, String)> = sqlx::query_as(
        "SELECT a.enrolment_id, e.priority, a.created_at, e.status FROM academic.flexible_seat_allocation a \
         JOIN academic.flexible_enrolment e ON e.id = a.enrolment_id WHERE a.opportunity_id = $1 AND a.status = 'confirmed' FOR UPDATE OF a",
    )
    .bind(o.id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    // progressed participations (registered and beyond) are protected: they always keep their seat
    let protected = rows.iter().filter(|r| r.3 != "allocated").count() as i32;
    let candidates: Vec<Demand> = rows
        .iter()
        .filter(|r| r.3 == "allocated")
        .map(|r| Demand {
            enrolment_id: r.0,
            priority: r.1,
            order_key: r.2.timestamp_micros(),
        })
        .collect();
    let release = over_allocated(
        (o.capacity - protected).max(0),
        o.reserved_seats.min((o.capacity - protected).max(0)),
        candidates,
    );
    let mut run = AllocationRun::default();
    for id in release {
        let e: FlexibleEnrolment = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
        release_allocation(&mut tx, id, "conflict resolution: over capacity").await?;
        if o.waitlist_enabled {
            let days = waitlist_days(&mut tx, tenant, &e).await?;
            transition(&mut tx, &scope, tenant, &e, E::Waitlisted, Some("allocation conflict: returned to the waitlist"), None,
                json!({"waitlisted_at": kit::now(), "waitlist_expires_on": today + Duration::days(days), "allocated_at": null}), actor).await?;
            run.waitlisted.push(id);
        } else {
            transition(
                &mut tx,
                &scope,
                tenant,
                &e,
                E::Rejected,
                Some("allocation conflict: unallocated"),
                None,
                json!({"allocated_at": null}),
                actor,
            )
            .await?;
            run.unallocated.push(id);
        }
        run.released.push(id);
    }
    kit::audit(&mut tx, &scope, opportunity::TABLE, "resolve_conflicts", o.id, json!({"released": run.released.len(), "capacity": o.capacity, "reserved": o.reserved_seats}), actor).await?;
    commit(tx).await?;
    run.opportunity = Some(opportunity::get(st, tenant, opportunity_id).await?);
    Ok(run)
}

// ------------------------------------------------------------------ registration (L3-D-037)

/// Registration hand-off: open electives register institutionally at once; external
/// opportunities create a provider registration request (enrolment stays allocated until the
/// provider accepts).
pub async fn register(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<FlexibleEnrolment, KernelError> {
    let (e, o, scope) = load(st, ctx, id).await?;
    check_version(e.version, expected)?;
    require(
        e.status == E::Allocated,
        format!(
            "the flexible enrolment is {}; registration needs an allocated seat",
            e.status
        ),
    )?;
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleEnrolment = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    if o.mode == LearningMode::OpenElective {
        transition(
            &mut tx,
            &scope,
            tenant,
            &locked,
            E::Registered,
            Some("institutional registration"),
            None,
            json!({}),
            actor,
        )
        .await?;
    } else {
        let pid = o
            .provider_id
            .ok_or_else(|| KernelError::internal_msg("external opportunity without provider"))?;
        let p: LearningProvider =
            kit::fetch(&mut *tx, provider::TABLE, "learning provider", tenant, pid).await?;
        require(
            p.status == LearningProviderStatus::Active,
            format!(
                "the learning provider is {}; registration hand-off is blocked",
                p.status
            ),
        )?;
        let live: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM academic.external_registration WHERE enrolment_id = $1 AND status IN ('requested', 'accepted'))")
            .bind(id)
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
        require(
            !live,
            "a provider registration is already pending for this enrolment",
        )?;
        kit::append(&mut tx, "external_registration", json!({
            "tenant_id": tenant.as_uuid(), "enrolment_id": id, "provider_id": pid, "requested_by": actor.id,
        })).await?;
        kit::patch::<FlexibleEnrolment>(
            &mut tx,
            TABLE,
            tenant,
            id,
            json!({"status_reason": "provider registration requested"}),
        )
        .await?;
        kit::audit(
            &mut tx,
            &scope,
            TABLE,
            "registration_request",
            id,
            json!({"providerId": pid}),
            actor,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Provider acknowledgement (BH-003…008): accepted (provider reference recorded, enrolment
/// registered), rejected / recovery_required (explicit unsuccessful position, enrolment stays
/// allocated). A repeated `evidenceKey` is the same fact and changes nothing.
pub async fn registration_outcome(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: RegistrationOutcome,
    actor: &Actor,
) -> Result<FlexibleEnrolment, KernelError> {
    let (e, _o, scope) = load(st, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let evidence = kit::opt_text(cmd.evidence_key.as_deref());
    if let Some(k) = &evidence {
        let seen: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM academic.external_registration WHERE enrolment_id = $1 AND evidence_key = $2)")
            .bind(id)
            .bind(k)
            .fetch_one(&st.pool)
            .await
            .map_err(map_db_error)?;
        if seen {
            return get(st, tenant, id).await;
        }
    }
    check_version(e.version, expected)?;
    if !matches!(
        cmd.outcome.as_str(),
        "accepted" | "rejected" | "recovery_required"
    ) {
        return Err(invalid(
            "outcome",
            "invalid",
            "outcome must be accepted, rejected or recovery_required",
        ));
    }
    let reference = kit::opt_text(cmd.provider_reference.as_deref());
    if cmd.outcome == "accepted" && reference.is_none() {
        return Err(invalid(
            "providerReference",
            "required",
            "an accepted registration needs the provider enrolment reference",
        ));
    }
    let reason = kit::opt_text(cmd.reason.as_deref());
    if cmd.outcome != "accepted" && reason.is_none() {
        return Err(invalid(
            "reason",
            "required",
            "an unsuccessful registration needs a reason",
        ));
    }
    let mut tx = begin(st).await?;
    let locked: FlexibleEnrolment = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    let reg: Option<ExternalRegistration> = {
        let v: Option<Value> = sqlx::query_scalar("SELECT to_jsonb(t) FROM academic.external_registration t WHERE t.enrolment_id = $1 AND t.status = 'requested' FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?;
        v.map(kit::decode).transpose()?
    };
    let reg = reg.ok_or_else(|| {
        KernelError::Conflict("there is no pending provider registration for this enrolment".into())
    })?;
    kit::patch::<ExternalRegistration>(&mut tx, "external_registration", tenant, reg.id, json!({
        "status": cmd.outcome, "provider_reference": reference, "evidence_key": evidence, "outcome_reason": reason,
    })).await?;
    if cmd.outcome == "accepted" {
        transition(
            &mut tx,
            &scope,
            tenant,
            &locked,
            E::Registered,
            Some("provider registration accepted"),
            None,
            json!({}),
            actor,
        )
        .await?;
    } else {
        let why = format!(
            "provider registration {}: {}",
            cmd.outcome,
            reason.clone().unwrap_or_default()
        );
        kit::patch::<FlexibleEnrolment>(&mut tx, TABLE, tenant, id, json!({"status_reason": why}))
            .await?;
        kit::audit(
            &mut tx,
            &scope,
            TABLE,
            "registration_unsuccessful",
            id,
            json!({"outcome": cmd.outcome, "reason": reason}),
            actor,
        )
        .await?;
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

// ------------------------------------------------------------------ progress + completion (L3-D-038)

pub async fn progress(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: LearningProgress,
    actor: &Actor,
) -> Result<FlexibleEnrolment, KernelError> {
    let (e, _o, scope) = load(st, ctx, id).await?;
    check_version(e.version, expected)?;
    if !(0.0..=10000.0).contains(&cmd.hours_completed) {
        return Err(invalid(
            "hoursCompleted",
            "range",
            "hoursCompleted must be between 0 and 10000",
        ));
    }
    let tenant = ctx.scope.tenant_id;
    let mut tx = begin(st).await?;
    let locked: FlexibleEnrolment = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    match locked.status {
        E::Registered => {
            transition(
                &mut tx,
                &scope,
                tenant,
                &locked,
                E::InProgress,
                Some("learning progress established"),
                None,
                json!({"hours_completed": cmd.hours_completed}),
                actor,
            )
            .await?;
        }
        E::InProgress => {
            kit::patch::<FlexibleEnrolment>(
                &mut tx,
                TABLE,
                tenant,
                id,
                json!({"hours_completed": cmd.hours_completed}),
            )
            .await?;
            kit::audit(
                &mut tx,
                &scope,
                TABLE,
                "progress",
                id,
                json!({"hoursCompleted": cmd.hours_completed}),
                actor,
            )
            .await?;
        }
        s => {
            return Err(KernelError::Conflict(format!(
                "the flexible enrolment is {s}; progress needs an active participation"
            )))
        }
    }
    commit(tx).await?;
    get(st, tenant, id).await
}

/// Records the institutional learning completion after validating hours, assessment and the
/// provider evidence (evidence, not authority). Never grants credit (BH-007). A repeated
/// `sourceKey` returns the recorded completion.
pub async fn complete(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: NewLearningCompletion,
    actor: &Actor,
) -> Result<FlexibleEnrolment, KernelError> {
    let (e, o, scope) = load(st, ctx, id).await?;
    let tenant = ctx.scope.tenant_id;
    let key = kit::opt_text(cmd.source_key.as_deref());
    if e.status == E::Completed {
        let same: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM academic.learning_completion WHERE enrolment_id = $1 AND source_key IS NOT DISTINCT FROM $2)")
            .bind(id)
            .bind(&key)
            .fetch_one(&st.pool)
            .await
            .map_err(map_db_error)?;
        if same && key.is_some() {
            return get(st, tenant, id).await;
        }
        return Err(KernelError::Conflict(
            "the learning completion is already recorded".into(),
        ));
    }
    check_version(e.version, expected)?;
    require(
        matches!(e.status, E::InProgress | E::Registered),
        format!(
            "the flexible enrolment is {}; completion needs an active participation",
            e.status
        ),
    )?;
    let evidence = kit::opt_text(cmd.provider_evidence_ref.as_deref());
    if o.mode != LearningMode::OpenElective && evidence.is_none() {
        return Err(invalid(
            "providerEvidenceRef",
            "required",
            "provider completion evidence is required for external learning",
        ));
    }
    if !matches!(
        cmd.assessment_status.as_str(),
        "passed" | "failed" | "not_applicable"
    ) {
        return Err(invalid(
            "assessmentStatus",
            "invalid",
            "assessmentStatus must be passed, failed or not_applicable",
        ));
    }
    require(
        cmd.assessment_status != "failed",
        "the required assessment was not passed; completion is not recorded",
    )?;
    if let Some(min) = o.min_hours {
        require(
            cmd.hours_completed >= min,
            format!(
                "{} of the required {min} hours completed; completion is not recorded",
                cmd.hours_completed
            ),
        )?;
    }
    let today = business_today(st, &scope, InstitutionId(e.institution_id)).await?;
    let on = cmd.completed_on.unwrap_or(today);
    if on > today {
        return Err(invalid(
            "completedOn",
            "future",
            "completedOn cannot be in the future",
        ));
    }
    let mut tx = begin(st).await?;
    let locked: FlexibleEnrolment = kit::lock(&mut tx, TABLE, ENTITY, tenant, id).await?;
    check_version(locked.version, expected)?;
    kit::append(&mut tx, "learning_completion", json!({
        "tenant_id": tenant.as_uuid(), "enrolment_id": id, "hours_completed": cmd.hours_completed,
        "assessment_status": cmd.assessment_status, "grade": kit::opt_text(cmd.grade.as_deref()),
        "provider_evidence_ref": evidence, "completed_on": on, "source_key": key, "recorded_by": actor.id,
    })).await?;
    transition(
        &mut tx,
        &scope,
        tenant,
        &locked,
        E::Completed,
        Some("learning completion recorded"),
        None,
        json!({"hours_completed": cmd.hours_completed}),
        actor,
    )
    .await?;
    commit(tx).await?;
    get(st, tenant, id).await
}
