//! **7B** — Faculty availability + workload plans vs the Domain D teaching load (`state.workload`, L3-E-030); `/me/workload`.
//!
//! Tag `people-attendance`; operation id prefixes `people_workload_` (contract §5.2). The router mount point is
//! already registered in `crate::router_with_state`; 7B fills in this `router` (and its own
//! files under this folder) and never edits `lib.rs`.
//!
//! A plan (draft) states the faculty member's availability and planned weekly hours for a planning
//! period; submitting it evaluates the Domain D teaching load (`FacultyWorkloadPort`, no
//! cross-schema SQL) against the maximum (plan maximum, else the Domain D faculty assignment
//! policy): an over-allocation is refused unless an exception reason is given for approval
//! (L4-E-030-BH-004/005). `WORKLOAD_PLAN_APPROVAL` → approved → active (the previous active plan of
//! the period is superseded).

use crate::leave::common::{
    audit, clean, emit_status, ensure_version, expected, history, illegal, invalid, parse_date,
    required, subject_of, EventParties, LeaveDecisionBody, Transition,
};
use crate::leave::domain::{over_allocated, workload_can_transition};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::decimal::{fmt2, parse_decimal};
use crate::shared::guard::{require_institution_writable, require_live_engagement};
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_staff;
use crate::shared::rows::lock_staff;
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::academic::ports::FacultyWorkload;
use sutra_contracts::people::events::leave::{
    WORKLOAD_PLAN_APPROVED, WORKLOAD_PLAN_APPROVED_SCHEMA,
};
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::ports::StaffSummary;
use sutra_contracts::people::status::{EngagementStatus, StaffCategory, WorkloadPlanStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Faculty availability in a planning period.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FacultyAvailability {
    Available,
    PartiallyAvailable,
    Unavailable,
}

impl FacultyAvailability {
    pub const ALL: &'static [FacultyAvailability] =
        &[Self::Available, Self::PartiallyAvailable, Self::Unavailable];

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::PartiallyAvailable => "partially_available",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|v| v.as_str() == s)
    }
}

/// A workload plan.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadPlanView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub academic_term_id: Option<Uuid>,
    pub term_label: String,
    pub availability: FacultyAvailability,
    pub available_from: Option<NaiveDate>,
    pub available_to: Option<NaiveDate>,
    #[schema(example = "12.00")]
    pub planned_weekly_hours: String,
    #[schema(example = "16.00")]
    pub max_weekly_hours: Option<String>,
    /// Domain D teaching load at submission.
    #[schema(example = "8.00")]
    pub teaching_weekly_hours: Option<String>,
    /// Maximum applied at submission (plan maximum, else the Domain D policy maximum).
    #[schema(example = "16.00")]
    pub effective_max_hours: Option<String>,
    pub over_allocated: bool,
    pub exception_reason: Option<String>,
    pub notes: Option<String>,
    pub status: WorkloadPlanStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WorkloadPlanList {
    pub items: Vec<WorkloadPlanView>,
}

/// New workload plan (draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadPlanCreate {
    /// Domain D academic term (optional: the plan applies to every term otherwise).
    pub academic_term_id: Option<Uuid>,
    pub term_label: Option<String>,
    pub availability: FacultyAvailability,
    pub available_from: Option<String>,
    pub available_to: Option<String>,
    #[schema(example = "12.00")]
    pub planned_weekly_hours: String,
    #[schema(example = "16.00")]
    pub max_weekly_hours: Option<String>,
    pub notes: Option<String>,
}

/// Submit a plan for approval.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadPlanSubmit {
    /// Required when the load exceeds the maximum (exception for approval).
    pub exception_reason: Option<String>,
}

/// One teaching assignment counted in the load.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeachingLoadItemView {
    pub assignment_id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub academic_term_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub role: String,
    pub status: String,
    #[schema(example = "4.00")]
    pub weekly_hours: Option<String>,
}

/// Capacity position of a faculty member: teaching load (Domain D) vs plan / policy maximum.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaffWorkloadView {
    pub staff_id: StaffMemberId,
    pub engagement_status: Option<EngagementStatus>,
    /// Operationally available (working engagement, not on long leave).
    pub available: bool,
    #[schema(example = "8.00")]
    pub total_weekly_hours: String,
    #[schema(example = "16.00")]
    pub policy_max_weekly_hours: Option<String>,
    #[schema(example = "16.00")]
    pub effective_max_hours: Option<String>,
    pub over_allocated: bool,
    pub items: Vec<TeachingLoadItemView>,
    pub active_plan: Option<WorkloadPlanView>,
}

/// `?academicTermId=`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct WorkloadQuery {
    pub academic_term_id: Option<Uuid>,
}

struct PlanRow {
    view: WorkloadPlanView,
    institution: InstitutionId,
    planned: Decimal,
    max: Option<Decimal>,
}

const PLAN_COLUMNS: &str = "id, institution_id, staff_id, engagement_id, academic_term_id, \
    term_label, availability, available_from, available_to, planned_weekly_hours, \
    max_weekly_hours, teaching_weekly_hours, effective_max_hours, over_allocated, \
    exception_reason, notes, status, status_reason, process_instance_id, created_at, version";

fn opt2(v: Option<Decimal>) -> Option<String> {
    v.map(fmt2)
}

fn plan_from_row(r: &PgRow) -> Result<PlanRow, sqlx::Error> {
    let planned: Decimal = r.try_get("planned_weekly_hours")?;
    let max: Option<Decimal> = r.try_get("max_weekly_hours")?;
    let institution: Uuid = r.try_get("institution_id")?;
    Ok(PlanRow {
        view: WorkloadPlanView {
            id: r.try_get("id")?,
            institution_id: institution,
            staff_id: r.try_get("staff_id")?,
            engagement_id: r.try_get("engagement_id")?,
            academic_term_id: r.try_get("academic_term_id")?,
            term_label: r.try_get("term_label")?,
            availability: crate::shared::rows::decode(
                r.try_get("availability")?,
                FacultyAvailability::parse,
            )?,
            available_from: r.try_get("available_from")?,
            available_to: r.try_get("available_to")?,
            planned_weekly_hours: fmt2(planned),
            max_weekly_hours: opt2(max),
            teaching_weekly_hours: opt2(r.try_get("teaching_weekly_hours")?),
            effective_max_hours: opt2(r.try_get("effective_max_hours")?),
            over_allocated: r.try_get("over_allocated")?,
            exception_reason: r.try_get("exception_reason")?,
            notes: r.try_get("notes")?,
            status: crate::shared::rows::decode(r.try_get("status")?, WorkloadPlanStatus::parse)?,
            status_reason: r.try_get("status_reason")?,
            process_instance_id: r.try_get("process_instance_id")?,
            created_at: r.try_get("created_at")?,
            version: r.try_get("version")?,
        },
        institution: InstitutionId(institution),
        planned,
        max,
    })
}

async fn load_plan(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PlanRow, KernelError> {
    let sql = format!(
        "SELECT {PLAN_COLUMNS} FROM people.workload_plan WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "workload plan",
        })?;
    plan_from_row(&r).map_err(map_db_error)
}

fn plan_response(status: StatusCode, p: &PlanRow) -> Response {
    with_etag(status, &p.view, Version(p.view.version))
}

fn hours(field: &str, raw: &str) -> Result<Decimal, KernelError> {
    let v = parse_decimal(field, raw)?;
    if v < Decimal::ZERO || v > Decimal::from(168) {
        return Err(invalid(field, "out_of_range", "0 … 168 hours per week"));
    }
    Ok(v)
}

/// Teaching load of a staff member through the Domain D port (empty without a login subject).
async fn load_of(
    st: &PeopleState,
    scope: &ExecutionScope,
    s: &StaffSummary,
    term: Option<Uuid>,
) -> Result<FacultyWorkload, KernelError> {
    match s.subject_ref.as_deref() {
        Some(subject) => {
            st.workload
                .teaching_load(scope, s.institution_id, subject, term)
                .await
        }
        None => Ok(FacultyWorkload {
            faculty_subject_ref: String::new(),
            items: vec![],
            total_weekly_hours: Decimal::ZERO,
            policy_max_weekly_hours: None,
        }),
    }
}

fn plan_transition<'a>(
    t: TenantId,
    p: &'a PlanRow,
    from: &'a str,
    to: &'a str,
    reason: Option<&'a str>,
) -> Transition<'a> {
    Transition {
        tenant: t,
        institution: p.institution,
        aggregate_type: "workload_plan",
        aggregate_id: p.view.id,
        from: Some(from),
        to,
        reason,
        process_instance_id: p.view.process_instance_id.as_deref(),
        cause: WORKLOAD_PLAN_APPROVED,
    }
}

/// Approval outcome applied inside the caller's transaction (plan locked, status `submitted`):
/// approved → active, superseding the previous active plan of the period
/// (`workload_plan.approved`).
async fn approve_plan(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &PlanRow,
    reason: Option<&str>,
    a: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    if !workload_can_transition(p.view.status, WorkloadPlanStatus::Approved) {
        return Err(illegal(
            "the workload plan",
            p.view.status.as_str(),
            "approved",
        ));
    }
    let previous: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM people.workload_plan WHERE tenant_id = $1 AND staff_id = $2 \
         AND status = 'active' AND academic_term_id IS NOT DISTINCT FROM $3 AND id <> $4 \
         FOR UPDATE",
    )
    .bind(t.as_uuid())
    .bind(p.view.staff_id)
    .bind(p.view.academic_term_id)
    .bind(p.view.id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for prev in previous {
        sqlx::query(
            "UPDATE people.workload_plan SET status = 'superseded', status_reason = $2, \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(prev)
        .bind(format!("superseded by plan {}", p.view.id))
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        history(
            conn,
            &Transition {
                tenant: t,
                institution: p.institution,
                aggregate_type: "workload_plan",
                aggregate_id: prev,
                from: Some("active"),
                to: "superseded",
                reason: None,
                process_instance_id: None,
                cause: WORKLOAD_PLAN_APPROVED,
            },
            a,
        )
        .await?;
    }
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.workload_plan SET status = 'active', status_reason = $2, decided_by = $3, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(p.view.id)
    .bind(reason)
    .bind(a.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        &plan_transition(t, p, "submitted", "approved", reason),
        a,
    )
    .await?;
    history(
        conn,
        &plan_transition(t, p, "approved", "active", reason),
        a,
    )
    .await?;
    let subject = subject_of(conn, t, Some(StaffMemberId(p.view.staff_id))).await?;
    emit_status(
        conn,
        scope,
        WORKLOAD_PLAN_APPROVED,
        WORKLOAD_PLAN_APPROVED_SCHEMA,
        &plan_transition(t, p, "submitted", "approved", reason),
        version,
        &EventParties {
            staff_id: Some(p.view.staff_id),
            engagement_id: Some(p.view.engagement_id),
            staff_subject_ref: subject,
            manager_subject_ref: None,
        },
        a,
    )
    .await
}

/// Create a draft workload plan for a faculty member (`people.leave.manage`): teaching staff with
/// a working engagement only; a second open plan for the same period is a conflict.
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/workload-plans", operation_id = "people_workload_plan_create", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = WorkloadPlanCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = WorkloadPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<WorkloadPlanCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let s = st
        .directory
        .staff(&tenant_scope(ctx), StaffMemberId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    let e = st
        .directory
        .current_engagement(&scope, s.id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    if e.staff_category != StaffCategory::Teaching {
        return Err(invalid(
            "staffId",
            "not_faculty",
            "workload plans are kept for teaching staff",
        )
        .into());
    }
    require_live_engagement(&e)?;
    if !e.status.is_working() {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {}; workload is planned for an active faculty member",
            e.engagement_number, e.status
        ))
        .into());
    }
    let term_label = required("termLabel", b.term_label)?;
    let planned = hours("plannedWeeklyHours", &b.planned_weekly_hours)?;
    let max = match b.max_weekly_hours.as_deref() {
        None => None,
        Some(raw) => {
            let m = hours("maxWeeklyHours", raw)?;
            if m.is_zero() {
                return Err(invalid("maxWeeklyHours", "out_of_range", "a positive maximum").into());
            }
            Some(m)
        }
    };
    let from = match b.available_from.as_deref() {
        None => None,
        Some(raw) => Some(parse_date("availableFrom", raw)?),
    };
    let to = match b.available_to.as_deref() {
        None => None,
        Some(raw) => Some(parse_date("availableTo", raw)?),
    };
    if let (Some(f), Some(x)) = (from, to) {
        if x < f {
            return Err(invalid(
                "availableTo",
                "end_before_start",
                "availableTo is before availableFrom",
            )
            .into());
        }
    }
    if b.availability == FacultyAvailability::Unavailable && planned > Decimal::ZERO {
        return Err(invalid(
            "plannedWeeklyHours",
            "unavailable",
            "an unavailable faculty member has no planned hours",
        )
        .into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_staff(&mut tx, t, s.id).await?;
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.workload_plan WHERE tenant_id = $1 AND staff_id = $2 \
         AND academic_term_id IS NOT DISTINCT FROM $3 AND status IN ('draft', 'submitted'))",
    )
    .bind(t.as_uuid())
    .bind(s.id.as_uuid())
    .bind(b.academic_term_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open {
        return Err(KernelError::Conflict(
            "an open workload plan for this period already exists; finish it first".into(),
        )
        .into());
    }
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.workload_plan (tenant_id, institution_id, staff_id, engagement_id, \
         academic_term_id, term_label, availability, available_from, available_to, \
         planned_weekly_hours, max_weekly_hours, notes, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(s.id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(b.academic_term_id)
    .bind(&term_label)
    .bind(b.availability.as_str())
    .bind(from)
    .bind(to)
    .bind(planned)
    .bind(max)
    .bind(clean(b.notes).as_deref())
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution: s.institution_id,
            aggregate_type: "workload_plan",
            aggregate_id: pid,
            from: None,
            to: "draft",
            reason: Some(&term_label),
            process_instance_id: None,
            cause: "sutraerp.people.workload_plan.created",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.workload_plan.create",
        "workload_plan",
        pid,
        serde_json::json!({"termLabel": term_label, "plannedWeeklyHours": fmt2(planned)}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_plan(&mut conn, t, pid, false).await?;
    Ok(plan_response(StatusCode::CREATED, &p))
}

/// Workload plans of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/workload-plans", operation_id = "people_workload_plan_list", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = WorkloadPlanList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<WorkloadPlanList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let rows = sqlx::query(&format!(
        "SELECT {PLAN_COLUMNS} FROM people.workload_plan WHERE tenant_id = $1 AND staff_id = $2 \
         ORDER BY created_at DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| plan_from_row(r).map(|p| p.view))
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(WorkloadPlanList { items }))
}

/// Submit a draft (`people.leave.manage`): evaluates the Domain D teaching load against the
/// maximum; over-allocation needs `exceptionReason` (409 otherwise); `WORKLOAD_PLAN_APPROVAL` →
/// submitted (auto-approval: active).
#[utoipa::path(post, path = "/api/v1/people/workload-plans/{id}/submit", operation_id = "people_workload_plan_submit", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body = WorkloadPlanSubmit, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = WorkloadPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_submit(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<WorkloadPlanSubmit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let p = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_plan(&mut conn, t, id, false).await?
    };
    ensure_version(v, p.view.version)?;
    if !workload_can_transition(p.view.status, WorkloadPlanStatus::Submitted) {
        return Err(illegal("the workload plan", p.view.status.as_str(), "submitted").into());
    }
    let scope = institution_scope(ctx, p.institution);
    require_institution_writable(&st, &scope, p.institution).await?;
    let s = st
        .directory
        .staff(&scope, StaffMemberId(p.view.staff_id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let e = st.directory.current_engagement(&scope, s.id).await?;
    if !e.as_ref().is_some_and(|e| e.status.is_working()) {
        return Err(KernelError::Conflict(
            "the faculty member is not active; workload cannot be accepted".into(),
        )
        .into());
    }
    let load = load_of(&st, &scope, &s, p.view.academic_term_id).await?;
    let effective_max = p.max.or(load.policy_max_weekly_hours);
    let over = over_allocated(load.total_weekly_hours, p.planned, effective_max);
    let exception = clean(b.exception_reason);
    if over && exception.is_none() {
        return Err(KernelError::Conflict(format!(
            "the load ({} planned, {} assigned) exceeds the maximum of {} weekly hours; \
             give an exceptionReason to request an approved exception",
            fmt2(p.planned),
            fmt2(load.total_weekly_hours),
            effective_max.map(fmt2).unwrap_or_default()
        ))
        .into());
    }
    let outcome = request_approval(
        &st,
        &scope,
        process::WORKLOAD_PLAN_APPROVAL,
        business_key("workload_plan", id, p.view.version),
        BTreeMap::from([
            ("staffId".to_owned(), serde_json::json!(p.view.staff_id)),
            ("overAllocated".to_owned(), serde_json::json!(over)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_plan(&mut tx, t, id, true).await?;
    ensure_version(v, cur.view.version)?;
    sqlx::query(
        "UPDATE people.workload_plan SET status = 'submitted', teaching_weekly_hours = $2, \
         effective_max_hours = $3, over_allocated = $4, exception_reason = $5, \
         process_instance_id = $6, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(load.total_weekly_hours.min(Decimal::from(999)))
    .bind(effective_max)
    .bind(over)
    .bind(exception.as_deref())
    .bind(&pid)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution: p.institution,
            aggregate_type: "workload_plan",
            aggregate_id: id,
            from: Some("draft"),
            to: "submitted",
            reason: exception.as_deref(),
            process_instance_id: Some(&pid),
            cause: "sutraerp.people.workload_plan.submitted",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.workload_plan.submit",
        "workload_plan",
        id,
        serde_json::json!({"teachingWeeklyHours": fmt2(load.total_weekly_hours),
            "effectiveMaxHours": effective_max.map(fmt2), "overAllocated": over,
            "exceptionReason": exception}),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let cur = load_plan(&mut tx, t, id, true).await?;
        approve_plan(
            &mut tx,
            &scope,
            &cur,
            Some("auto-approved"),
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_plan(&mut conn, t, id, false).await?;
    Ok(plan_response(StatusCode::OK, &p))
}

/// Decide a submitted plan (`people.approve`, step-up): approve → active, reject → back to draft.
#[utoipa::path(post, path = "/api/v1/people/workload-plans/{id}/decision", operation_id = "people_workload_plan_decide", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = WorkloadPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn plan_decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = clean(b.reason.clone());
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_plan(&mut tx, t, id, true).await?;
    ensure_version(v, p.view.version)?;
    let scope = institution_scope(ctx, p.institution);
    if b.approve() {
        approve_plan(&mut tx, &scope, &p, reason.as_deref(), &a).await?;
    } else {
        let reason = reason
            .clone()
            .ok_or_else(|| invalid("reason", "required", "a rejection needs a reason"))?;
        if !workload_can_transition(p.view.status, WorkloadPlanStatus::Draft) {
            return Err(illegal("the workload plan", p.view.status.as_str(), "draft").into());
        }
        sqlx::query(
            "UPDATE people.workload_plan SET status = 'draft', status_reason = $2, decided_by = $3, \
             version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(&reason)
        .bind(a.id.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            &Transition {
                tenant: t,
                institution: p.institution,
                aggregate_type: "workload_plan",
                aggregate_id: id,
                from: Some("submitted"),
                to: "draft",
                reason: Some(&reason),
                process_instance_id: p.view.process_instance_id.as_deref(),
                cause: "sutraerp.people.workload_plan.rejected",
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.workload_plan.decide",
        "workload_plan",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_plan(&mut conn, t, id, false).await?;
    Ok(plan_response(StatusCode::OK, &p))
}

async fn workload_view(
    st: &PeopleState,
    ctx: &AuthContext,
    s: &StaffSummary,
    term: Option<Uuid>,
) -> Result<StaffWorkloadView, KernelError> {
    let scope = institution_scope(ctx, s.institution_id);
    let e = st.directory.current_engagement(&scope, s.id).await?;
    let load = load_of(st, &scope, s, term).await?;
    let row = sqlx::query(&format!(
        "SELECT {PLAN_COLUMNS} FROM people.workload_plan WHERE tenant_id = $1 AND staff_id = $2 \
         AND status = 'active' AND ($3::uuid IS NULL OR academic_term_id IS NOT DISTINCT FROM $3) \
         ORDER BY updated_at DESC LIMIT 1"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(s.id.as_uuid())
    .bind(term)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    let plan = row
        .as_ref()
        .map(plan_from_row)
        .transpose()
        .map_err(map_db_error)?;
    let effective_max = plan
        .as_ref()
        .and_then(|p| p.max)
        .or(load.policy_max_weekly_hours);
    let planned = plan.as_ref().map(|p| p.planned).unwrap_or(Decimal::ZERO);
    Ok(StaffWorkloadView {
        staff_id: s.id,
        engagement_status: e.as_ref().map(|e| e.status),
        available: e.as_ref().is_some_and(|e| e.status.is_working()),
        total_weekly_hours: fmt2(load.total_weekly_hours),
        policy_max_weekly_hours: opt2(load.policy_max_weekly_hours),
        effective_max_hours: opt2(effective_max),
        over_allocated: over_allocated(load.total_weekly_hours, planned, effective_max),
        items: load
            .items
            .iter()
            .map(|i| TeachingLoadItemView {
                assignment_id: i.assignment_id.as_uuid(),
                offering_id: i.offering_id.as_uuid(),
                section_id: i.section_id.as_uuid(),
                academic_term_id: i.academic_term_id,
                offering_code: i.offering_code.clone(),
                title: i.title.clone(),
                role: i.role.as_str().to_owned(),
                status: i.status.as_str().to_owned(),
                weekly_hours: opt2(i.weekly_hours),
            })
            .collect(),
        active_plan: plan.map(|p| p.view),
    })
}

/// Capacity position of a faculty member (`people.read`): Domain D teaching load vs the active
/// plan / policy maximum, availability.
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/workload", operation_id = "people_workload_get", tag = "people-attendance",
    params(("id" = String, Path, format = Uuid), WorkloadQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StaffWorkloadView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn staff_workload(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<WorkloadQuery>,
) -> ApiResult<Json<StaffWorkloadView>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let s = st
        .directory
        .staff(&tenant_scope(ctx), StaffMemberId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    Ok(Json(workload_view(&st, ctx, &s, q.academic_term_id).await?))
}

/// My capacity position (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/workload", operation_id = "people_workload_self_get", tag = "people-attendance",
    params(WorkloadQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StaffWorkloadView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_workload(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Query(q): Query<WorkloadQuery>,
) -> ApiResult<Json<StaffWorkloadView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let me = my_staff(&st, ctx).await?;
    Ok(Json(
        workload_view(&st, ctx, &me, q.academic_term_id).await?,
    ))
}

/// Routes of this area.
pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(plan_create, plan_list))
        .routes(routes!(plan_submit))
        .routes(routes!(plan_decide))
        .routes(routes!(staff_workload))
        .routes(routes!(my_workload))
        .with_state(state)
}
