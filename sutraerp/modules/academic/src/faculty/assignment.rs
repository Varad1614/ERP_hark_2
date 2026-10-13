//! Faculty academic assignment (L3-D-017/018): establishment → approval through the
//! WorkflowPort → activation, end (complete / revoke / withdraw), and governed operational
//! changes (role, weekly hours, period, reassignment).

use crate::offering::change::change_can_transition;
use crate::offering::lifecycle::{
    assignment_v1, effective_policy, emit_assignment_ended, expect_version, unique_code,
};
use crate::offering::support::*;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_assignment_status, StatusChange};
use crate::shared::offering::{
    assignment_from_row, load_assignment, load_offering, load_section, lock_assignment,
    lock_offering, FacultyAssignmentRow, OfferingRow, ASSIGNMENT_COLUMNS,
};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::{publish, AcademicState, Actor};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Postgres, Row, Transaction};
use std::collections::BTreeMap;
use sutra_contracts::academic::events::offering as ev;
use sutra_contracts::academic::events::StatusChangedV1;
use sutra_contracts::academic::ids::{FacultyAssignmentId, OfferingSectionId};
use sutra_contracts::academic::status::{
    ChangeRequestStatus, CourseOfferingStatus, FacultyAssignmentStatus, FacultyRole, SectionStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::ExecutionScope;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

// ================================================================== views / requests

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyAssignment {
    pub id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub faculty_subject_ref: String,
    pub faculty_name: Option<String>,
    pub staff_id: Option<Uuid>,
    pub role: FacultyRole,
    pub weekly_hours: Option<f64>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub status: FacultyAssignmentStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FacultyAssignmentList {
    pub items: Vec<FacultyAssignment>,
}

pub fn view(a: &FacultyAssignmentRow) -> FacultyAssignment {
    FacultyAssignment {
        id: a.id.as_uuid(),
        offering_id: a.offering_id.as_uuid(),
        section_id: a.section_id.as_uuid(),
        faculty_subject_ref: a.faculty_subject_ref.clone(),
        faculty_name: a.faculty_name.clone(),
        staff_id: a.staff_id,
        role: a.role,
        weekly_hours: a.weekly_hours,
        effective_from: a.effective_from,
        effective_to: a.effective_to,
        status: a.status,
        status_reason: a.status_reason.clone(),
        process_instance_id: a.process_instance_id.clone(),
        version: a.version.0,
        created_at: a.created_at,
    }
}

fn etag(status: StatusCode, a: &FacultyAssignment) -> Response {
    with_etag(status, a, Version(a.version))
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewFacultyAssignment {
    /// Login subject of the faculty member (e.g. `dev|meera.shah@sutra.dev`).
    pub faculty_subject_ref: String,
    /// Display name typed by the administrator until People (Batch 7) provides it.
    pub faculty_name: Option<String>,
    /// Defaults to `primary_instructor`.
    pub role: Option<FacultyRole>,
    pub weekly_hours: Option<f64>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFacultyAssignment {
    pub faculty_name: Option<String>,
    pub role: Option<FacultyRole>,
    pub weekly_hours: Option<f64>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct AssignmentQuery {
    #[param(inline)]
    pub status: Option<FacultyAssignmentStatus>,
}

fn check_hours(field: &str, h: Option<f64>) -> Result<Option<f64>, KernelError> {
    if h.is_some_and(|h| !(0.0..=60.0).contains(&h) || h.is_nan()) {
        return Err(invalid(field, "out_of_range", "0 to 60 hours a week"));
    }
    Ok(h)
}

fn check_period(from: Option<NaiveDate>, to: Option<NaiveDate>) -> Result<(), KernelError> {
    if let (Some(f), Some(t)) = (from, to) {
        if t < f {
            return Err(invalid(
                "effectiveTo",
                "before_from",
                "must not precede effectiveFrom",
            ));
        }
    }
    Ok(())
}

/// Policy checks (L3-D-019 configuration): allowed roles and the weekly-hours ceiling over the
/// faculty member's live assignments in the same term.
async fn check_policy(
    conn: &mut PgConnection,
    o: &OfferingRow,
    subject: &str,
    role: FacultyRole,
    hours: Option<f64>,
    exclude: Option<FacultyAssignmentId>,
) -> Result<(), KernelError> {
    let Some((max, roles, _)) = effective_policy(&mut *conn, o.tenant_id, o.institution_id).await?
    else {
        return Ok(());
    };
    if !roles.is_empty() && !roles.iter().any(|r| r == role.as_str()) {
        return Err(conflict(format!(
            "the effective assignment policy does not allow the role {role}"
        )));
    }
    if let Some(max) = max {
        let used: f64 = sqlx::query_scalar(
            "SELECT COALESCE(sum(a.weekly_hours), 0)::float8 FROM academic.faculty_assignment a \
             JOIN academic.course_offering o ON o.id = a.offering_id \
             WHERE a.tenant_id = $1 AND a.faculty_subject_ref = $2 AND o.academic_term_id = $3 \
             AND a.status IN ('submitted', 'under_review', 'approved', 'active') AND ($4::uuid IS NULL OR a.id <> $4)",
        )
        .bind(o.tenant_id.as_uuid())
        .bind(subject)
        .bind(o.academic_term_id)
        .bind(exclude.map(|x| x.as_uuid()))
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let total = used + hours.unwrap_or(0.0);
        if total > max + 1e-9 {
            return Err(conflict(format!(
                "the workload would reach {total} h/week; the effective policy allows {max}"
            )));
        }
    }
    Ok(())
}

/// Assignment command context: offering + assignment locked.
struct ACmd {
    tx: Transaction<'static, Postgres>,
    o: OfferingRow,
    a: FacultyAssignmentRow,
    scope: ExecutionScope,
    actor: Actor,
}

async fn begin_assignment(
    st: &AcademicState,
    ctx: &AuthContext,
    id: FacultyAssignmentId,
    expected: Option<Version>,
) -> Result<ACmd, KernelError> {
    let t = tenant(ctx);
    let a = load_assignment(&st.pool, t, id).await?;
    let o = load_offering(&st.pool, t, a.offering_id).await?;
    let scope = institution_scope(ctx, o.institution_id);
    require_institution_writable(st, &scope, o.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = lock_offering(&mut tx, t, a.offering_id).await?;
    let a = lock_assignment(&mut tx, t, id).await?;
    if let Some(v) = expected {
        check_version(a.version, v)?;
    }
    Ok(ACmd {
        tx,
        o,
        a,
        scope,
        actor: actor(ctx),
    })
}

impl ACmd {
    async fn set(
        &mut self,
        to: FacultyAssignmentStatus,
        cause: &'static str,
        reason: Option<&str>,
        pid: Option<&str>,
    ) -> Result<FacultyAssignmentStatus, KernelError> {
        let from = self.a.status;
        self.a = change_assignment_status(
            &mut self.tx,
            &self.a,
            to,
            &StatusChange {
                cause_event_type: cause,
                reason,
                process_instance_id: pid,
            },
            &self.actor,
        )
        .await?;
        Ok(from)
    }

    async fn status_event(
        &mut self,
        from: FacultyAssignmentStatus,
        ty: &'static str,
        schema: &'static str,
        reason: Option<&str>,
    ) -> Result<(), KernelError> {
        let data = StatusChangedV1 {
            aggregate_id: self.a.id.as_uuid(),
            institution_id: self.o.institution_id.as_uuid(),
            offering_id: Some(self.o.id.as_uuid()),
            section_id: Some(self.a.section_id.as_uuid()),
            student_id: None,
            from_status: Some(from.as_str().to_owned()),
            to_status: self.a.status.as_str().to_owned(),
            version: self.a.version.0,
            reason: reason.map(str::to_owned),
            process_instance_id: self.a.process_instance_id.clone(),
            student_subject_ref: None,
            faculty_subject_ref: Some(self.a.faculty_subject_ref.clone()),
        };
        publish::event(
            &mut self.tx,
            &self.scope,
            ty,
            schema,
            self.a.id.to_string(),
            &data,
            &self.actor,
        )
        .await
    }

    async fn audit(&mut self, action: &str, details: serde_json::Value) -> Result<(), KernelError> {
        publish::audit(
            &mut self.tx,
            &self.scope,
            action,
            "faculty_assignment",
            self.a.id.to_string(),
            details,
            &self.actor,
        )
        .await
    }

    async fn finish(self) -> Result<FacultyAssignment, KernelError> {
        let v = view(&self.a);
        self.tx.commit().await.map_err(map_db_error)?;
        Ok(v)
    }
}

fn assignable(o: &OfferingRow) -> Result<(), KernelError> {
    if o.status.is_terminal()
        || matches!(
            o.status,
            CourseOfferingStatus::Cancelled
                | CourseOfferingStatus::Completed
                | CourseOfferingStatus::Frozen
        )
    {
        return Err(conflict(format!(
            "the course offering is {}; faculty assignments cannot change",
            o.status
        )));
    }
    Ok(())
}

// ================================================================== create / read / update

/// Establish a faculty assignment on a section in `draft` (L3-D-017, `faculty_assignment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/sections/{id}/faculty-assignments",
    operation_id = "faculty_assignment_create",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid, description = "Section id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewFacultyAssignment,
    responses(
        (status = 201, body = FacultyAssignment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Offering closed, duplicate live assignment or policy breach", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<OfferingSectionId>,
    headers: HeaderMap,
    Json(cmd): Json<NewFacultyAssignment>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    require_idempotency_key(&headers)?;
    let t = tenant(ctx);
    let s = load_section(&st.pool, t, id).await?;
    let o = load_offering(&st.pool, t, s.offering_id).await?;
    let scope = institution_scope(ctx, o.institution_id);
    require_institution_writable(&st, &scope, o.institution_id).await?;
    assignable(&o)?;
    if !matches!(s.status, SectionStatus::Planned | SectionStatus::Active) {
        return Err(conflict(format!("the section is {}", s.status)).into());
    }
    let subject = check_text("facultySubjectRef", &cmd.faculty_subject_ref, 200)?;
    let staff = st
        .staff
        .staff_by_subject(&scope, &subject)
        .await?
        .filter(|x| x.active)
        .ok_or_else(|| {
            invalid(
                "facultySubjectRef",
                "not_found",
                "no active staff member has this login",
            )
        })?;
    let name = match cmd
        .faculty_name
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty())
    {
        Some(n) => Some(check_text("facultyName", n, 200)?),
        None => staff.display_name.clone(),
    };
    let role = cmd.role.unwrap_or(FacultyRole::PrimaryInstructor);
    let hours = check_hours("weeklyHours", cmd.weekly_hours)?;
    check_period(cmd.effective_from, cmd.effective_to)?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = lock_offering(&mut tx, t, o.id).await?;
    check_policy(&mut tx, &o, &subject, role, None, None).await?;
    let row = sqlx::query(&format!(
        "INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, \
         faculty_name, staff_id, role, weekly_hours, effective_from, effective_to) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {ASSIGNMENT_COLUMNS}"
    ))
    .bind(t.as_uuid()).bind(o.institution_id.as_uuid()).bind(o.id.as_uuid()).bind(s.id.as_uuid())
    .bind(&subject).bind(&name).bind(staff.staff_id).bind(role.as_str()).bind(hours)
    .bind(cmd.effective_from).bind(cmd.effective_to)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| unique_code(e, "facultySubjectRef", "this faculty member already holds a live assignment in this role on the section"))?;
    let asg = assignment_from_row(&row).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "academic.faculty_assignment.create",
        "faculty_assignment",
        asg.id.to_string(),
        serde_json::json!({"sectionId": s.id, "facultySubjectRef": subject, "role": role.as_str()}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(etag(StatusCode::CREATED, &view(&asg)))
}

/// Faculty assignments of a section (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/sections/{id}/faculty-assignments",
    operation_id = "faculty_assignment_list",
    tag = "faculty-assignments",
    params(("id" = String, Path, format = Uuid, description = "Section id"), AssignmentQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = FacultyAssignmentList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<OfferingSectionId>,
    Query(q): Query<AssignmentQuery>,
) -> ApiResult<Json<FacultyAssignmentList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let s = load_section(&st.pool, tenant(ctx), id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM academic.faculty_assignment WHERE section_id = $1 AND ($2::text IS NULL OR status = $2) ORDER BY created_at"
    ))
    .bind(s.id.as_uuid()).bind(q.status.map(|x| x.as_str())).fetch_all(&st.pool).await.map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| assignment_from_row(r).map(|a| view(&a)))
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    Ok(Json(FacultyAssignmentList { items }))
}

/// One faculty assignment (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/faculty-assignments/{id}",
    operation_id = "faculty_assignment_get",
    tag = "faculty-assignments",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = FacultyAssignment, headers(("ETag" = String))), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<FacultyAssignmentId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let a = load_assignment(&st.pool, tenant(ctx), id).await?;
    Ok(etag(StatusCode::OK, &view(&a)))
}

/// Edit a draft assignment (`faculty_assignment.manage`).
#[utoipa::path(
    patch,
    path = "/api/v1/academic/faculty-assignments/{id}",
    operation_id = "faculty_assignment_update",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the assignment"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateFacultyAssignment,
    responses(
        (status = 200, body = FacultyAssignment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<FacultyAssignmentId>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateFacultyAssignment>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    let v = expect_version!(headers);
    let mut c = begin_assignment(&st, ctx, id, Some(v)).await?;
    if c.a.status != FacultyAssignmentStatus::Draft {
        return Err(conflict(format!(
            "the assignment is {}; request an operational change instead",
            c.a.status
        ))
        .into());
    }
    let hours = match cmd.weekly_hours {
        Some(h) => check_hours("weeklyHours", Some(h))?,
        None => c.a.weekly_hours,
    };
    let from = cmd.effective_from.or(c.a.effective_from);
    let to = cmd.effective_to.or(c.a.effective_to);
    check_period(from, to)?;
    let role = cmd.role.unwrap_or(c.a.role);
    let name = match cmd.faculty_name.as_deref() {
        Some(n) => Some(check_text("facultyName", n, 200)?),
        None => c.a.faculty_name.clone(),
    };
    let row = sqlx::query(&format!(
        "UPDATE academic.faculty_assignment SET faculty_name = $2, role = $3, weekly_hours = $4, effective_from = $5, effective_to = $6, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {ASSIGNMENT_COLUMNS}"
    ))
    .bind(c.a.id.as_uuid()).bind(&name).bind(role.as_str()).bind(hours).bind(from).bind(to)
    .fetch_one(&mut *c.tx).await
    .map_err(|e| unique_code(e, "role", "this faculty member already holds a live assignment in this role on the section"))?;
    c.a = assignment_from_row(&row).map_err(map_db_error)?;
    c.audit(
        "academic.faculty_assignment.update",
        serde_json::json!({"role": role.as_str(), "weeklyHours": hours}),
    )
    .await?;
    Ok(etag(StatusCode::OK, &c.finish().await?))
}

// ================================================================== lifecycle commands

/// Submit a draft through the WorkflowPort (`faculty_assignment_approval`): draft → submitted →
/// approved (auto) or under_review (awaiting a decision).
pub async fn submit_assignment(
    st: &AcademicState,
    ctx: &AuthContext,
    id: FacultyAssignmentId,
    v: Version,
) -> Result<FacultyAssignment, KernelError> {
    let t = tenant(ctx);
    let a = load_assignment(&st.pool, t, id).await?;
    check_version(a.version, v)?;
    if a.status != FacultyAssignmentStatus::Draft {
        return Err(conflict(format!(
            "the assignment is {}; only drafts are submitted",
            a.status
        )));
    }
    let o = load_offering(&st.pool, t, a.offering_id).await?;
    assignable(&o)?;
    let outcome = request_approval(
        st,
        &institution_scope(ctx, o.institution_id),
        process::FACULTY_ASSIGNMENT_APPROVAL,
        business_key("faculty_assignment", a.id, a.version.0),
        BTreeMap::from([
            (
                "facultySubjectRef".to_owned(),
                serde_json::json!(a.faculty_subject_ref),
            ),
            ("sectionId".to_owned(), serde_json::json!(a.section_id)),
        ]),
    )
    .await?;
    let mut c = begin_assignment(st, ctx, id, Some(v)).await?;
    check_policy(
        &mut c.tx,
        &c.o,
        &c.a.faculty_subject_ref,
        c.a.role,
        c.a.weekly_hours,
        Some(c.a.id),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let from = c
        .set(
            FacultyAssignmentStatus::Submitted,
            ev::FACULTY_ASSIGNMENT_SUBMITTED,
            None,
            Some(&pid),
        )
        .await?;
    c.status_event(
        from,
        ev::FACULTY_ASSIGNMENT_SUBMITTED,
        ev::FACULTY_ASSIGNMENT_SUBMITTED_SCHEMA,
        None,
    )
    .await?;
    c.audit(
        "academic.faculty_assignment.submit",
        serde_json::json!({"processInstanceId": pid}),
    )
    .await?;
    match outcome {
        ApprovalOutcome::Approved { .. } => {
            let system = Actor::system(c.actor.correlation_id.clone());
            let user = std::mem::replace(&mut c.actor, system);
            let from = c
                .set(
                    FacultyAssignmentStatus::Approved,
                    ev::FACULTY_ASSIGNMENT_APPROVED,
                    Some("auto-approved"),
                    Some(&pid),
                )
                .await?;
            c.status_event(
                from,
                ev::FACULTY_ASSIGNMENT_APPROVED,
                ev::FACULTY_ASSIGNMENT_APPROVED_SCHEMA,
                Some("auto-approved"),
            )
            .await?;
            c.audit(
                "academic.faculty_assignment.approve",
                serde_json::json!({"processInstanceId": pid}),
            )
            .await?;
            c.actor = user;
        }
        ApprovalOutcome::Pending { .. } => {
            c.set(
                FacultyAssignmentStatus::UnderReview,
                ev::FACULTY_ASSIGNMENT_SUBMITTED,
                None,
                Some(&pid),
            )
            .await?;
        }
    }
    c.finish().await
}

async fn decide(
    st: &AcademicState,
    ctx: &AuthContext,
    id: FacultyAssignmentId,
    v: Version,
    d: StepDecisionBody,
) -> Result<FacultyAssignment, KernelError> {
    let reason = clean_reason(d.reason, d.decision == StepDecision::Reject)?;
    let mut c = begin_assignment(st, ctx, id, Some(v)).await?;
    if !matches!(
        c.a.status,
        FacultyAssignmentStatus::Submitted | FacultyAssignmentStatus::UnderReview
    ) {
        return Err(conflict(format!(
            "the assignment is {}; nothing awaits a decision",
            c.a.status
        )));
    }
    let pid = c.a.process_instance_id.clone();
    match d.decision {
        StepDecision::Approve => {
            assignable(&c.o)?;
            let from = c
                .set(
                    FacultyAssignmentStatus::Approved,
                    ev::FACULTY_ASSIGNMENT_APPROVED,
                    reason.as_deref(),
                    pid.as_deref(),
                )
                .await?;
            c.status_event(
                from,
                ev::FACULTY_ASSIGNMENT_APPROVED,
                ev::FACULTY_ASSIGNMENT_APPROVED_SCHEMA,
                reason.as_deref(),
            )
            .await?;
            c.audit(
                "academic.faculty_assignment.approve",
                serde_json::json!({"reason": reason}),
            )
            .await?;
        }
        StepDecision::Reject => {
            c.set(
                FacultyAssignmentStatus::Rejected,
                ev::FACULTY_ASSIGNMENT_SUBMITTED,
                reason.as_deref(),
                pid.as_deref(),
            )
            .await?;
            c.audit(
                "academic.faculty_assignment.reject",
                serde_json::json!({"reason": reason}),
            )
            .await?;
        }
    }
    c.finish().await
}

async fn transition(
    st: &AcademicState,
    ctx: &AuthContext,
    id: FacultyAssignmentId,
    v: Version,
    to: FacultyAssignmentStatus,
    reason: Option<String>,
) -> Result<FacultyAssignment, KernelError> {
    let mut c = begin_assignment(st, ctx, id, Some(v)).await?;
    let r = reason.as_deref();
    match to {
        FacultyAssignmentStatus::Active => {
            assignable(&c.o)?;
            if matches!(
                c.o.status,
                CourseOfferingStatus::Draft | CourseOfferingStatus::PendingApproval
            ) {
                return Err(conflict(format!(
                    "the course offering is {}; assignments activate once it is approved",
                    c.o.status
                )));
            }
            let s = load_section(&mut *c.tx, c.o.tenant_id, c.a.section_id).await?;
            if !matches!(s.status, SectionStatus::Planned | SectionStatus::Active) {
                return Err(conflict(format!("the section is {}", s.status)));
            }
            check_policy(
                &mut c.tx,
                &c.o,
                &c.a.faculty_subject_ref,
                c.a.role,
                c.a.weekly_hours,
                Some(c.a.id),
            )
            .await?;
            c.set(to, ev::FACULTY_ASSIGNMENT_ACTIVATED, r, None).await?;
            let data = assignment_v1(&c.a);
            publish::event(
                &mut c.tx,
                &c.scope,
                ev::FACULTY_ASSIGNMENT_ACTIVATED,
                ev::FACULTY_ASSIGNMENT_ACTIVATED_SCHEMA,
                c.a.id.to_string(),
                &data,
                &c.actor,
            )
            .await?;
            c.audit(
                "academic.faculty_assignment.activate",
                serde_json::json!({}),
            )
            .await?;
        }
        FacultyAssignmentStatus::Revoked | FacultyAssignmentStatus::Completed => {
            if c.a.status != FacultyAssignmentStatus::Active {
                return Err(conflict(format!(
                    "the assignment is {}; only active assignments end",
                    c.a.status
                )));
            }
            c.set(to, ev::FACULTY_ASSIGNMENT_ENDED, r, None).await?;
            let (scope, a, actor) = (c.scope, c.a.clone(), c.actor.clone());
            emit_assignment_ended(&mut c.tx, &scope, &a, &actor).await?;
            let verb = if to == FacultyAssignmentStatus::Revoked {
                "revoke"
            } else {
                "complete"
            };
            c.audit(
                &format!("academic.faculty_assignment.{verb}"),
                serde_json::json!({"reason": reason}),
            )
            .await?;
        }
        FacultyAssignmentStatus::Withdrawn => {
            c.set(to, ev::FACULTY_ASSIGNMENT_ENDED, r, None).await?;
            c.audit(
                "academic.faculty_assignment.withdraw",
                serde_json::json!({"reason": reason}),
            )
            .await?;
        }
        FacultyAssignmentStatus::Archived => {
            c.set(to, ev::FACULTY_ASSIGNMENT_ENDED, r, None).await?;
            c.audit("academic.faculty_assignment.archive", serde_json::json!({}))
                .await?;
        }
        _ => return Err(conflict("unsupported transition")),
    }
    c.finish().await
}

/// Submit a draft assignment for approval (`faculty_assignment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignments/{id}/submit",
    operation_id = "faculty_assignment_submit",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the assignment"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = FacultyAssignment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<FacultyAssignmentId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    let v = expect_version!(headers);
    Ok(etag(
        StatusCode::OK,
        &submit_assignment(&st, ctx, id, v).await?,
    ))
}

/// Approve or reject a submitted assignment (`faculty_assignment.decide`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignments/{id}/decision",
    operation_id = "faculty_assignment_decision",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the assignment"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = FacultyAssignment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<FacultyAssignmentId>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentDecide)?;
    let v = expect_version!(headers);
    Ok(etag(StatusCode::OK, &decide(&st, ctx, id, v, d).await?))
}

macro_rules! assignment_command {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $perm:expr, $to:expr, $reason_required:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "faculty-assignments",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the assignment"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<StepReason>),
            responses(
                (status = 200, body = FacultyAssignment, headers(("ETag" = String))),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(State(st): State<AcademicState>, auth: Authenticated, Path(id): Path<FacultyAssignmentId>, headers: HeaderMap, raw: Bytes) -> ApiResult<Response> {
            let ctx = auth.require($perm)?;
            let v = expect_version!(headers);
            let reason = body::<StepReason>(&raw)?.validated($reason_required)?;
            Ok(etag(StatusCode::OK, &transition(&st, ctx, id, v, $to, reason).await?))
        }
    };
}

assignment_command!(activate, "/api/v1/academic/faculty-assignments/{id}/activate", "faculty_assignment_activate",
    "Activate an approved assignment: the faculty member now teaches the section (`faculty_assignment.decide`).",
    Permission::FacultyAssignmentDecide, FacultyAssignmentStatus::Active, false);
assignment_command!(
    revoke,
    "/api/v1/academic/faculty-assignments/{id}/revoke",
    "faculty_assignment_revoke",
    "Revoke an active assignment (reason required, `faculty_assignment.decide`).",
    Permission::FacultyAssignmentDecide,
    FacultyAssignmentStatus::Revoked,
    true
);
assignment_command!(
    complete,
    "/api/v1/academic/faculty-assignments/{id}/complete",
    "faculty_assignment_complete",
    "Complete an active assignment (`faculty_assignment.decide`).",
    Permission::FacultyAssignmentDecide,
    FacultyAssignmentStatus::Completed,
    false
);
assignment_command!(
    withdraw,
    "/api/v1/academic/faculty-assignments/{id}/withdraw",
    "faculty_assignment_withdraw",
    "Withdraw an assignment that is not yet active (`faculty_assignment.manage`).",
    Permission::FacultyAssignmentManage,
    FacultyAssignmentStatus::Withdrawn,
    false
);
assignment_command!(archive, "/api/v1/academic/faculty-assignments/{id}/archive", "faculty_assignment_archive",
    "Archive an ended assignment (completed / revoked / rejected / withdrawn, `faculty_assignment.decide`).",
    Permission::FacultyAssignmentDecide, FacultyAssignmentStatus::Archived, false);

// ================================================================== operational change (D-018)

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyAssignmentChange {
    pub id: Uuid,
    pub assignment_id: Uuid,
    /// `role` | `hours` | `period` | `reassignment`.
    pub change_kind: String,
    #[schema(value_type = Object)]
    pub proposed: serde_json::Value,
    #[schema(value_type = Object)]
    pub baseline: serde_json::Value,
    pub status: ChangeRequestStatus,
    pub reason: String,
    pub decision_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub replacement_assignment_id: Option<Uuid>,
    pub applied_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FacultyAssignmentChangeList {
    pub items: Vec<FacultyAssignmentChange>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewFacultyAssignmentChange {
    pub change_kind: String,
    pub role: Option<FacultyRole>,
    pub weekly_hours: Option<f64>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    /// `reassignment`: the new faculty member's login subject and name.
    pub faculty_subject_ref: Option<String>,
    pub faculty_name: Option<String>,
    pub reason: String,
}

const CHANGE_COLUMNS: &str = "id, assignment_id, change_kind, proposed, baseline, status, reason, decision_reason, process_instance_id, replacement_assignment_id, applied_at, version, created_at";

fn change_from_row(r: &PgRow) -> Result<FacultyAssignmentChange, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(FacultyAssignmentChange {
        id: r.try_get("id").map_err(map_db_error)?,
        assignment_id: r.try_get("assignment_id").map_err(map_db_error)?,
        change_kind: r.try_get("change_kind").map_err(map_db_error)?,
        proposed: r.try_get("proposed").map_err(map_db_error)?,
        baseline: r.try_get("baseline").map_err(map_db_error)?,
        status: ChangeRequestStatus::parse(&status).unwrap_or(ChangeRequestStatus::Requested),
        reason: r.try_get("reason").map_err(map_db_error)?,
        decision_reason: r.try_get("decision_reason").map_err(map_db_error)?,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        replacement_assignment_id: r
            .try_get("replacement_assignment_id")
            .map_err(map_db_error)?,
        applied_at: r.try_get("applied_at").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
    })
}

async fn validate_change(
    st: &AcademicState,
    c: &mut ACmd,
    p: &NewFacultyAssignmentChange,
) -> Result<(), KernelError> {
    if !matches!(
        c.a.status,
        FacultyAssignmentStatus::Active | FacultyAssignmentStatus::Approved
    ) {
        return Err(conflict(format!(
            "the assignment is {}; operational changes apply to approved or active assignments",
            c.a.status
        )));
    }
    assignable(&c.o)?;
    match p.change_kind.as_str() {
        "role" => {
            let role = p
                .role
                .ok_or_else(|| invalid("role", "required", "the new role is required"))?;
            if role == c.a.role {
                return Err(invalid("role", "unchanged", "the role is already set"));
            }
            check_policy(
                &mut c.tx,
                &c.o,
                &c.a.faculty_subject_ref,
                role,
                c.a.weekly_hours,
                Some(c.a.id),
            )
            .await?;
        }
        "hours" => {
            let h = check_hours("weeklyHours", p.weekly_hours)?.ok_or_else(|| {
                invalid(
                    "weeklyHours",
                    "required",
                    "the new weekly hours are required",
                )
            })?;
            check_policy(
                &mut c.tx,
                &c.o,
                &c.a.faculty_subject_ref,
                c.a.role,
                Some(h),
                Some(c.a.id),
            )
            .await?;
        }
        "period" => {
            if p.effective_from.is_none() && p.effective_to.is_none() {
                return Err(invalid("effectiveTo", "required", "give the new period"));
            }
            check_period(
                p.effective_from.or(c.a.effective_from),
                p.effective_to.or(c.a.effective_to),
            )?;
        }
        "reassignment" => {
            let subject = p
                .faculty_subject_ref
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    invalid(
                        "facultySubjectRef",
                        "required",
                        "the replacement faculty member is required",
                    )
                })?;
            if subject == c.a.faculty_subject_ref {
                return Err(invalid(
                    "facultySubjectRef",
                    "unchanged",
                    "that faculty member already holds the assignment",
                ));
            }
            st.staff
                .staff_by_subject(&c.scope, subject)
                .await?
                .filter(|x| x.active)
                .ok_or_else(|| {
                    invalid(
                        "facultySubjectRef",
                        "not_found",
                        "no active staff member has this login",
                    )
                })?;
            check_policy(&mut c.tx, &c.o, subject, c.a.role, c.a.weekly_hours, None).await?;
        }
        _ => {
            return Err(invalid(
                "changeKind",
                "invalid",
                "role, hours, period or reassignment",
            ))
        }
    }
    Ok(())
}

async fn set_change(
    conn: &mut PgConnection,
    ch: &FacultyAssignmentChange,
    to: ChangeRequestStatus,
    decision_reason: Option<&str>,
    decided_by: Option<&str>,
    replacement: Option<Uuid>,
) -> Result<FacultyAssignmentChange, KernelError> {
    if !change_can_transition(ch.status, to) {
        return Err(conflict(format!(
            "the change is {}; it cannot become {to}",
            ch.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE academic.faculty_assignment_change SET status = $2, decision_reason = COALESCE($3, decision_reason), \
         decided_by = COALESCE($4, decided_by), replacement_assignment_id = COALESCE($5, replacement_assignment_id), \
         applied_at = CASE WHEN $2 = 'applied' THEN now() ELSE applied_at END, version = version + 1, updated_at = now() \
         WHERE id = $1 AND version = $6 RETURNING {CHANGE_COLUMNS}"
    ))
    .bind(ch.id).bind(to.as_str()).bind(decision_reason).bind(decided_by).bind(replacement).bind(ch.version)
    .fetch_optional(&mut *conn).await.map_err(map_db_error)?
    .ok_or_else(|| conflict("the change changed concurrently"))?;
    change_from_row(&row)
}

/// Approves and applies a change inside the assignment command, as `decider`.
async fn apply_change(
    st: &AcademicState,
    c: &mut ACmd,
    ch: FacultyAssignmentChange,
    decider: &Actor,
    reason: Option<&str>,
) -> Result<FacultyAssignmentChange, KernelError> {
    let user = std::mem::replace(&mut c.actor, decider.clone());
    let res = async {
        let p: NewFacultyAssignmentChange = serde_json::from_value(ch.proposed.clone()).unwrap_or_default();
        validate_change(st, c, &p).await?;
        let ch = set_change(&mut c.tx, &ch, ChangeRequestStatus::Approved, reason, decider.id.as_deref(), None).await?;
        let mut replacement = None;
        if p.change_kind == "reassignment" {
            let subject = p.faculty_subject_ref.clone().unwrap_or_default();
            let row = sqlx::query(&format!(
                "INSERT INTO academic.faculty_assignment (tenant_id, institution_id, offering_id, section_id, faculty_subject_ref, \
                 faculty_name, role, weekly_hours, effective_from, effective_to, status, status_reason, process_instance_id) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) RETURNING {ASSIGNMENT_COLUMNS}"
            ))
            .bind(c.a.tenant_id.as_uuid()).bind(c.a.institution_id.as_uuid()).bind(c.a.offering_id.as_uuid()).bind(c.a.section_id.as_uuid())
            .bind(&subject).bind(p.faculty_name.as_deref().map(str::trim)).bind(c.a.role.as_str()).bind(c.a.weekly_hours)
            .bind(c.a.effective_from).bind(c.a.effective_to).bind(c.a.status.as_str()).bind("reassignment").bind(&ch.process_instance_id)
            .fetch_one(&mut *c.tx).await
            .map_err(|e| unique_code(e, "facultySubjectRef", "the replacement already holds a live assignment in this role"))?;
            let new = assignment_from_row(&row).map_err(map_db_error)?;
            if new.status == FacultyAssignmentStatus::Active {
                publish::event(&mut c.tx, &c.scope, ev::FACULTY_ASSIGNMENT_ACTIVATED, ev::FACULTY_ASSIGNMENT_ACTIVATED_SCHEMA, new.id.to_string(), &assignment_v1(&new), &c.actor).await?;
            }
            let ended = if c.a.status == FacultyAssignmentStatus::Active { FacultyAssignmentStatus::Revoked } else { FacultyAssignmentStatus::Withdrawn };
            let was_active = c.a.status == FacultyAssignmentStatus::Active;
            c.set(ended, ev::FACULTY_ASSIGNMENT_CHANGED, Some(&ch.reason), ch.process_instance_id.as_deref()).await?;
            if was_active {
                let (scope, a, actor) = (c.scope, c.a.clone(), c.actor.clone());
                emit_assignment_ended(&mut c.tx, &scope, &a, &actor).await?;
            }
            replacement = Some(new.id.as_uuid());
        } else {
            let row = sqlx::query(&format!(
                "UPDATE academic.faculty_assignment SET role = COALESCE($2, role), weekly_hours = COALESCE($3, weekly_hours), \
                 effective_from = COALESCE($4, effective_from), effective_to = COALESCE($5, effective_to), version = version + 1, updated_at = now() \
                 WHERE id = $1 RETURNING {ASSIGNMENT_COLUMNS}"
            ))
            .bind(c.a.id.as_uuid()).bind(p.role.map(|r| r.as_str())).bind(p.weekly_hours).bind(p.effective_from).bind(p.effective_to)
            .fetch_one(&mut *c.tx).await
            .map_err(|e| unique_code(e, "role", "the faculty member already holds a live assignment in that role"))?;
            c.a = assignment_from_row(&row).map_err(map_db_error)?;
        }
        let field = match p.change_kind.as_str() {
            "role" => "role",
            "hours" => "weeklyHours",
            "period" => "effectiveTo",
            _ => "facultySubjectRef",
        };
        let data = sutra_contracts::academic::events::ModifiedV1 {
            aggregate_id: c.a.id.as_uuid(),
            institution_id: c.a.institution_id.as_uuid(),
            offering_id: Some(c.a.offering_id.as_uuid()),
            section_id: Some(c.a.section_id.as_uuid()),
            version: c.a.version.0,
            changed_fields: vec![field.to_owned()],
        };
        publish::event(&mut c.tx, &c.scope, ev::FACULTY_ASSIGNMENT_CHANGED, ev::FACULTY_ASSIGNMENT_CHANGED_SCHEMA, c.a.id.to_string(), &data, &c.actor).await?;
        let ch = set_change(&mut c.tx, &ch, ChangeRequestStatus::Applied, None, None, replacement).await?;
        c.audit("academic.faculty_assignment.apply_change", serde_json::json!({"changeId": ch.id, "kind": ch.change_kind, "replacement": replacement})).await?;
        Ok::<_, KernelError>(ch)
    }
    .await;
    c.actor = user;
    res
}

/// Request an operational change (L3-D-018, `faculty_assignment.manage`). Validated at once,
/// approved through the WorkflowPort (`faculty_assignment_change`) and applied when approved.
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignments/{id}/changes",
    operation_id = "faculty_assignment_change_create",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewFacultyAssignmentChange,
    responses(
        (status = 201, body = FacultyAssignmentChange),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn change_create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<FacultyAssignmentId>,
    headers: HeaderMap,
    Json(mut cmd): Json<NewFacultyAssignmentChange>,
) -> ApiResult<(StatusCode, Json<FacultyAssignmentChange>)> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    require_idempotency_key(&headers)?;
    cmd.reason = check_text("reason", &cmd.reason, 1000)?;
    let t = tenant(ctx);
    let a = load_assignment(&st.pool, t, id).await?;
    let o = load_offering(&st.pool, t, a.offering_id).await?;
    let outcome = request_approval(
        &st,
        &institution_scope(ctx, o.institution_id),
        process::FACULTY_ASSIGNMENT_CHANGE,
        business_key(
            &format!("faculty_assignment_change_{}", cmd.change_kind),
            a.id,
            a.version.0,
        ),
        BTreeMap::from([("kind".to_owned(), serde_json::json!(cmd.change_kind))]),
    )
    .await?;
    let mut c = begin_assignment(&st, ctx, id, None).await?;
    validate_change(&st, &mut c, &cmd).await?;
    let baseline = serde_json::json!({
        "role": c.a.role.as_str(), "weeklyHours": c.a.weekly_hours, "effectiveFrom": c.a.effective_from,
        "effectiveTo": c.a.effective_to, "facultySubjectRef": c.a.faculty_subject_ref, "facultyName": c.a.faculty_name,
    });
    let row = sqlx::query(&format!(
        "INSERT INTO academic.faculty_assignment_change (tenant_id, institution_id, assignment_id, offering_id, section_id, change_kind, \
         proposed, baseline, status, reason, process_instance_id, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'validated', $9, $10, $11) RETURNING {CHANGE_COLUMNS}"
    ))
    .bind(t.as_uuid()).bind(c.o.institution_id.as_uuid()).bind(c.a.id.as_uuid()).bind(c.a.offering_id.as_uuid()).bind(c.a.section_id.as_uuid())
    .bind(&cmd.change_kind).bind(serde_json::to_value(&cmd).map_err(KernelError::internal)?).bind(&baseline).bind(&cmd.reason)
    .bind(outcome.process_instance_id()).bind(c.actor.id.as_deref())
    .fetch_one(&mut *c.tx).await
    .map_err(|e| unique_code(e, "changeKind", "an operational change is already open on this assignment"))?;
    let mut ch = change_from_row(&row)?;
    c.audit(
        "academic.faculty_assignment.request_change",
        serde_json::json!({"changeId": ch.id, "kind": ch.change_kind}),
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(c.actor.correlation_id.clone());
        ch = apply_change(&st, &mut c, ch, &system, Some("auto-approved")).await?;
    }
    c.tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(ch)))
}

/// Operational changes of an assignment (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/faculty-assignments/{id}/changes",
    operation_id = "faculty_assignment_change_list",
    tag = "faculty-assignments",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = FacultyAssignmentChangeList), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn change_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<FacultyAssignmentId>,
) -> ApiResult<Json<FacultyAssignmentChangeList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let a = load_assignment(&st.pool, tenant(ctx), id).await?;
    let rows = sqlx::query(&format!("SELECT {CHANGE_COLUMNS} FROM academic.faculty_assignment_change WHERE assignment_id = $1 ORDER BY created_at DESC"))
        .bind(a.id.as_uuid()).fetch_all(&st.pool).await.map_err(map_db_error)?;
    let items = rows.iter().map(change_from_row).collect::<Result<_, _>>()?;
    Ok(Json(FacultyAssignmentChangeList { items }))
}

/// Decide a pending operational change (`faculty_assignment.decide`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignments/{id}/changes/{changeId}/decision",
    operation_id = "faculty_assignment_change_decision",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("changeId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the change"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = FacultyAssignmentChange),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn change_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, cid)): Path<(FacultyAssignmentId, Uuid)>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentDecide)?;
    let v = expect_version!(headers);
    let reason = clean_reason(d.reason, d.decision == StepDecision::Reject)?;
    let mut c = begin_assignment(&st, ctx, id, None).await?;
    let row = sqlx::query(&format!("SELECT {CHANGE_COLUMNS} FROM academic.faculty_assignment_change WHERE id = $1 AND assignment_id = $2 FOR UPDATE"))
        .bind(cid).bind(c.a.id.as_uuid()).fetch_optional(&mut *c.tx).await.map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "assignment change" })?;
    let ch = change_from_row(&row)?;
    check_version(Version(ch.version), v)?;
    if !matches!(
        ch.status,
        ChangeRequestStatus::Requested | ChangeRequestStatus::Validated
    ) {
        return Err(conflict(format!(
            "the change is {}; nothing awaits a decision",
            ch.status
        ))
        .into());
    }
    let decider = c.actor.clone();
    let ch = match d.decision {
        StepDecision::Approve => apply_change(&st, &mut c, ch, &decider, reason.as_deref()).await?,
        StepDecision::Reject => {
            let ch = set_change(
                &mut c.tx,
                &ch,
                ChangeRequestStatus::Rejected,
                reason.as_deref(),
                decider.id.as_deref(),
                None,
            )
            .await?;
            c.audit(
                "academic.faculty_assignment.reject_change",
                serde_json::json!({"changeId": ch.id, "reason": reason}),
            )
            .await?;
            ch
        }
    };
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &ch, Version(ch.version)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hours_and_periods() {
        assert!(check_hours("h", Some(61.0)).is_err());
        assert!(check_hours("h", Some(-1.0)).is_err());
        assert!(check_hours("h", Some(4.5)).is_ok());
        let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok();
        assert!(check_period(d("2026-07-01"), d("2026-06-01")).is_err());
        assert!(check_period(d("2026-06-01"), None).is_ok());
    }
}
