//! Faculty assignment governance configuration (L3-D-019): versioned policy (workload ceiling,
//! allowed roles, primary-instructor requirement) — draft → in review → impact / architecture
//! review outcome → approval through the WorkflowPort → effective (prior effective version
//! superseded, never rewritten) → retired.

use crate::offering::lifecycle::expect_version;
use crate::offering::support::*;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::{publish, AcademicState, Actor};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::offering as ev;
use sutra_contracts::academic::events::StatusChangedV1;
use sutra_contracts::academic::status::{FacultyRole, PolicyVersionStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyAssignmentPolicy {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub name: String,
    pub version_no: i32,
    pub max_weekly_hours: Option<f64>,
    pub allowed_roles: Vec<String>,
    pub require_primary_instructor: bool,
    pub rationale: String,
    /// `compatible` | `incompatible` once reviewed.
    pub review_outcome: Option<String>,
    pub review_notes: Option<String>,
    pub status: PolicyVersionStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub superseded_by: Option<Uuid>,
    pub effective_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FacultyAssignmentPolicyList {
    pub items: Vec<FacultyAssignmentPolicy>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyPolicyDraft {
    pub name: Option<String>,
    pub max_weekly_hours: Option<f64>,
    pub allowed_roles: Option<Vec<FacultyRole>>,
    pub require_primary_instructor: Option<bool>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyPolicyReview {
    /// `compatible` | `incompatible` (impact + architecture review outcome).
    pub outcome: String,
    pub notes: Option<String>,
}

#[derive(sqlx::FromRow)]
struct PolicyRow {
    id: Uuid,
    institution_id: Uuid,
    name: String,
    version_no: i32,
    max_weekly_hours: Option<f64>,
    allowed_roles: Vec<String>,
    require_primary_instructor: bool,
    rationale: String,
    review_outcome: Option<String>,
    review_notes: Option<String>,
    status: String,
    status_reason: Option<String>,
    process_instance_id: Option<String>,
    superseded_by: Option<Uuid>,
    effective_at: Option<DateTime<Utc>>,
    version: i64,
    created_at: DateTime<Utc>,
}
const POLICY_COLUMNS: &str = "id, institution_id, name, version_no, max_weekly_hours::float8 AS max_weekly_hours, allowed_roles, require_primary_instructor, rationale, \
    review_outcome, review_notes, status, status_reason, process_instance_id, superseded_by, effective_at, version, created_at";

fn policy_view(r: PolicyRow) -> FacultyAssignmentPolicy {
    FacultyAssignmentPolicy {
        id: r.id,
        institution_id: r.institution_id,
        name: r.name,
        version_no: r.version_no,
        max_weekly_hours: r.max_weekly_hours,
        allowed_roles: r.allowed_roles,
        require_primary_instructor: r.require_primary_instructor,
        rationale: r.rationale,
        review_outcome: r.review_outcome,
        review_notes: r.review_notes,
        status: PolicyVersionStatus::parse(&r.status).unwrap_or(PolicyVersionStatus::Draft),
        status_reason: r.status_reason,
        process_instance_id: r.process_instance_id,
        superseded_by: r.superseded_by,
        effective_at: r.effective_at,
        version: r.version,
        created_at: r.created_at,
    }
}

/// Legal policy-version transitions (slice-owned machine).
pub fn policy_can_transition(from: PolicyVersionStatus, to: PolicyVersionStatus) -> bool {
    use PolicyVersionStatus::*;
    matches!(
        (from, to),
        (Draft, InReview)
            | (InReview, Draft)
            | (InReview, Effective)
            | (Effective, Superseded)
            | (Effective, Retired)
    )
}

/// Compatibility of a configuration (L4-D-019-BH-003).
pub fn validate_config(
    max: Option<f64>,
    roles: &[String],
    require_primary: bool,
) -> Result<(), KernelError> {
    if max.is_some_and(|m| !(m > 0.0 && m <= 80.0)) {
        return Err(invalid(
            "maxWeeklyHours",
            "out_of_range",
            "more than 0 and at most 80",
        ));
    }
    if require_primary
        && !roles.is_empty()
        && !roles
            .iter()
            .any(|r| r == FacultyRole::PrimaryInstructor.as_str())
    {
        return Err(invalid(
            "allowedRoles",
            "incompatible",
            "a primary instructor is required but the role is not allowed",
        ));
    }
    Ok(())
}

async fn load(
    conn: &mut PgConnection,
    tenant: sutra_kernel::scope::TenantId,
    id: Uuid,
    lock: bool,
) -> Result<FacultyAssignmentPolicy, KernelError> {
    let r: PolicyRow = sqlx::query_as(&format!(
        "SELECT {POLICY_COLUMNS} FROM academic.faculty_assignment_policy WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id).bind(tenant.as_uuid()).fetch_optional(&mut *conn).await.map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "assignment policy" })?;
    Ok(policy_view(r))
}

async fn set_status(
    conn: &mut PgConnection,
    p: &FacultyAssignmentPolicy,
    to: PolicyVersionStatus,
    reason: Option<&str>,
    pid: Option<&str>,
) -> Result<FacultyAssignmentPolicy, KernelError> {
    if !policy_can_transition(p.status, to) {
        return Err(conflict(format!(
            "the policy is {}; it cannot become {to}",
            p.status
        )));
    }
    let r: PolicyRow = sqlx::query_as(&format!(
        "UPDATE academic.faculty_assignment_policy SET status = $2, status_reason = $3, process_instance_id = COALESCE($4, process_instance_id), \
         effective_at = CASE WHEN $2 = 'effective' THEN now() ELSE effective_at END, version = version + 1, updated_at = now() \
         WHERE id = $1 AND version = $5 RETURNING {POLICY_COLUMNS}"
    ))
    .bind(p.id).bind(to.as_str()).bind(reason).bind(pid).bind(p.version)
    .fetch_optional(&mut *conn).await.map_err(map_db_error)?
    .ok_or_else(|| conflict("the policy changed concurrently"))?;
    Ok(policy_view(r))
}

/// in_review → effective; the prior effective version is superseded (kept). Emits
/// `faculty_assignment_policy.effective`.
async fn make_effective(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: &FacultyAssignmentPolicy,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<FacultyAssignmentPolicy, KernelError> {
    if p.review_outcome.as_deref() != Some("compatible") {
        return Err(conflict(
            "approval needs a compatible impact / architecture review outcome first",
        ));
    }
    validate_config(
        p.max_weekly_hours,
        &p.allowed_roles,
        p.require_primary_instructor,
    )?;
    let prior: Option<PolicyRow> = sqlx::query_as(&format!(
        "SELECT {POLICY_COLUMNS} FROM academic.faculty_assignment_policy WHERE tenant_id = $1 AND institution_id = $2 AND status = 'effective' FOR UPDATE"
    ))
    .bind(scope.tenant_id.as_uuid()).bind(p.institution_id).fetch_optional(&mut *conn).await.map_err(map_db_error)?;
    if let Some(prior) = prior.map(policy_view) {
        set_status(
            conn,
            &prior,
            PolicyVersionStatus::Superseded,
            Some("superseded by a newer version"),
            None,
        )
        .await?;
        sqlx::query(
            "UPDATE academic.faculty_assignment_policy SET superseded_by = $2 WHERE id = $1",
        )
        .bind(prior.id)
        .bind(p.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let p = set_status(conn, p, PolicyVersionStatus::Effective, reason, pid).await?;
    let data = StatusChangedV1 {
        aggregate_id: p.id,
        institution_id: p.institution_id,
        offering_id: None,
        section_id: None,
        student_id: None,
        from_status: Some(PolicyVersionStatus::InReview.as_str().to_owned()),
        to_status: p.status.as_str().to_owned(),
        version: p.version,
        reason: reason.map(str::to_owned),
        process_instance_id: p.process_instance_id.clone(),
        student_subject_ref: None,
        faculty_subject_ref: None,
    };
    publish::event(
        conn,
        scope,
        ev::FACULTY_ASSIGNMENT_POLICY_EFFECTIVE,
        ev::FACULTY_ASSIGNMENT_POLICY_EFFECTIVE_SCHEMA,
        p.id.to_string(),
        &data,
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        "academic.faculty_assignment_policy.effective",
        "faculty_assignment_policy",
        p.id.to_string(),
        serde_json::json!({"versionNo": p.version_no}),
        actor,
    )
    .await?;
    Ok(p)
}

fn roles_of(r: Option<Vec<FacultyRole>>) -> Vec<String> {
    let mut v: Vec<String> = r
        .unwrap_or_default()
        .iter()
        .map(|x| x.as_str().to_owned())
        .collect();
    v.sort();
    v.dedup();
    v
}

/// Draft a new governance configuration version (L3-D-019, `faculty_assignment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/institutions/{id}/faculty-assignment-policies",
    operation_id = "faculty_policy_create",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = FacultyPolicyDraft,
    responses(
        (status = 201, body = FacultyAssignmentPolicy, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<FacultyPolicyDraft>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    require_institution_writable(&st, &scope, id).await?;
    let name = check_text(
        "name",
        cmd.name.as_deref().unwrap_or("Faculty assignment policy"),
        200,
    )?;
    let rationale = check_text("rationale", cmd.rationale.as_deref().unwrap_or(""), 2000)?;
    let roles = roles_of(cmd.allowed_roles);
    let require_primary = cmd.require_primary_instructor.unwrap_or(true);
    validate_config(cmd.max_weekly_hours, &roles, require_primary)?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 4))")
        .bind(id.as_uuid().to_string())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let r: PolicyRow = sqlx::query_as(&format!(
        "INSERT INTO academic.faculty_assignment_policy (tenant_id, institution_id, name, version_no, max_weekly_hours, allowed_roles, \
         require_primary_instructor, rationale) VALUES ($1, $2, $3, \
           (SELECT COALESCE(max(version_no), 0) + 1 FROM academic.faculty_assignment_policy WHERE tenant_id = $1 AND institution_id = $2), \
           $4, $5, $6, $7) RETURNING {POLICY_COLUMNS}"
    ))
    .bind(scope.tenant_id.as_uuid()).bind(id.as_uuid()).bind(&name).bind(cmd.max_weekly_hours).bind(&roles).bind(require_primary).bind(&rationale)
    .fetch_one(&mut *tx).await.map_err(map_db_error)?;
    let p = policy_view(r);
    publish::audit(
        &mut tx,
        &scope,
        "academic.faculty_assignment_policy.create",
        "faculty_assignment_policy",
        p.id.to_string(),
        serde_json::json!({"versionNo": p.version_no}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &p, Version(p.version)))
}

/// Policy versions of an institution, newest first (`faculty_assignment.manage`; Audit C3 —
/// was `academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/institutions/{id}/faculty-assignment-policies",
    operation_id = "faculty_policy_list",
    tag = "faculty-assignments",
    params(("id" = String, Path, format = Uuid, description = "Institution id")),
    security(("session_cookie" = [])),
    responses((status = 200, body = FacultyAssignmentPolicyList))
)]
pub async fn list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<FacultyAssignmentPolicyList>> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    let rows: Vec<PolicyRow> = sqlx::query_as(&format!("SELECT {POLICY_COLUMNS} FROM academic.faculty_assignment_policy WHERE tenant_id = $1 AND institution_id = $2 ORDER BY version_no DESC"))
        .bind(tenant(ctx).as_uuid()).bind(id).fetch_all(&st.pool).await.map_err(map_db_error)?;
    Ok(Json(FacultyAssignmentPolicyList {
        items: rows.into_iter().map(policy_view).collect(),
    }))
}

/// One policy version (`faculty_assignment.manage`; Audit C3 — was `academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/faculty-assignment-policies/{id}",
    operation_id = "faculty_policy_get",
    tag = "faculty-assignments",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = FacultyAssignmentPolicy, headers(("ETag" = String))), (status = 404, body = Problem, content_type = "application/problem+json"))
)]
pub async fn get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load(&mut conn, tenant(ctx), id, false).await?;
    Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
}

struct PCmd {
    tx: sqlx::Transaction<'static, sqlx::Postgres>,
    p: FacultyAssignmentPolicy,
    scope: ExecutionScope,
    actor: Actor,
}

async fn begin(
    st: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    v: Version,
) -> Result<PCmd, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load(&mut tx, tenant(ctx), id, true).await?;
    check_version(Version(p.version), v)?;
    let scope = institution_scope(ctx, InstitutionId(p.institution_id));
    Ok(PCmd {
        tx,
        p,
        scope,
        actor: actor(ctx),
    })
}

impl PCmd {
    async fn finish(
        mut self,
        action: &str,
        details: serde_json::Value,
    ) -> Result<FacultyAssignmentPolicy, KernelError> {
        publish::audit(
            &mut self.tx,
            &self.scope,
            action,
            "faculty_assignment_policy",
            self.p.id.to_string(),
            details,
            &self.actor,
        )
        .await?;
        self.tx.commit().await.map_err(map_db_error)?;
        Ok(self.p)
    }
}

fn policy_etag(p: &FacultyAssignmentPolicy) -> Response {
    with_etag(StatusCode::OK, p, Version(p.version))
}

/// Edit a draft version (`faculty_assignment.manage`).
#[utoipa::path(
    patch,
    path = "/api/v1/academic/faculty-assignment-policies/{id}",
    operation_id = "faculty_policy_update",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the policy"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = FacultyPolicyDraft,
    responses(
        (status = 200, body = FacultyAssignmentPolicy, headers(("ETag" = String))),
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
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FacultyPolicyDraft>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    let v = expect_version!(headers);
    let mut c = begin(&st, ctx, id, v).await?;
    if c.p.status != PolicyVersionStatus::Draft {
        return Err(conflict(format!(
            "the policy is {}; only drafts are edited — draft a new version instead",
            c.p.status
        ))
        .into());
    }
    let name = match cmd.name.as_deref() {
        Some(n) => check_text("name", n, 200)?,
        None => c.p.name.clone(),
    };
    let rationale = match cmd.rationale.as_deref() {
        Some(r) => check_text("rationale", r, 2000)?,
        None => c.p.rationale.clone(),
    };
    let max = cmd.max_weekly_hours.or(c.p.max_weekly_hours);
    let roles = if cmd.allowed_roles.is_some() {
        roles_of(cmd.allowed_roles)
    } else {
        c.p.allowed_roles.clone()
    };
    let primary = cmd
        .require_primary_instructor
        .unwrap_or(c.p.require_primary_instructor);
    validate_config(max, &roles, primary)?;
    let r: PolicyRow = sqlx::query_as(&format!(
        "UPDATE academic.faculty_assignment_policy SET name = $2, rationale = $3, max_weekly_hours = $4, allowed_roles = $5, \
         require_primary_instructor = $6, review_outcome = NULL, review_notes = NULL, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {POLICY_COLUMNS}"
    ))
    .bind(c.p.id).bind(&name).bind(&rationale).bind(max).bind(&roles).bind(primary)
    .fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    c.p = policy_view(r);
    Ok(policy_etag(
        &c.finish(
            "academic.faculty_assignment_policy.update",
            serde_json::json!({}),
        )
        .await?,
    ))
}

/// Send a draft to impact / architecture review (draft → in_review, `faculty_assignment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignment-policies/{id}/submit",
    operation_id = "faculty_policy_submit",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the policy"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = FacultyAssignmentPolicy, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentManage)?;
    let v = expect_version!(headers);
    let mut c = begin(&st, ctx, id, v).await?;
    c.p = set_status(&mut c.tx, &c.p, PolicyVersionStatus::InReview, None, None).await?;
    Ok(policy_etag(
        &c.finish(
            "academic.faculty_assignment_policy.submit",
            serde_json::json!({}),
        )
        .await?,
    ))
}

/// Record the impact / architecture review outcome (`faculty_assignment.decide`). Incompatible
/// returns the version to draft; compatible starts the approval through the WorkflowPort
/// (`faculty_assignment_policy`) — auto-approval makes it effective at once.
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignment-policies/{id}/review",
    operation_id = "faculty_policy_review",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the policy"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = FacultyPolicyReview,
    responses(
        (status = 200, body = FacultyAssignmentPolicy, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn review(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<FacultyPolicyReview>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentDecide)?;
    let v = expect_version!(headers);
    if !matches!(cmd.outcome.as_str(), "compatible" | "incompatible") {
        return Err(invalid("outcome", "invalid", "compatible or incompatible").into());
    }
    let notes = clean_reason(cmd.notes, cmd.outcome == "incompatible")?;
    let (inst, pv) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let p = load(&mut conn, tenant(ctx), id, false).await?;
        (p.institution_id, p.version)
    };
    let outcome = if cmd.outcome == "compatible" {
        Some(
            request_approval(
                &st,
                &institution_scope(ctx, InstitutionId(inst)),
                process::FACULTY_ASSIGNMENT_POLICY,
                business_key("faculty_assignment_policy", id, pv),
                BTreeMap::new(),
            )
            .await?,
        )
    } else {
        None
    };
    let mut c = begin(&st, ctx, id, v).await?;
    if c.p.status != PolicyVersionStatus::InReview {
        return Err(conflict(format!(
            "the policy is {}; only versions in review are reviewed",
            c.p.status
        ))
        .into());
    }
    let r: PolicyRow = sqlx::query_as(&format!(
        "UPDATE academic.faculty_assignment_policy SET review_outcome = $2, review_notes = $3, version = version + 1, updated_at = now() WHERE id = $1 RETURNING {POLICY_COLUMNS}"
    ))
    .bind(c.p.id).bind(&cmd.outcome).bind(&notes).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    c.p = policy_view(r);
    match outcome {
        None => {
            c.p = set_status(
                &mut c.tx,
                &c.p,
                PolicyVersionStatus::Draft,
                notes.as_deref(),
                None,
            )
            .await?;
        }
        Some(ApprovalOutcome::Pending {
            process_instance_id,
        }) => {
            let r: PolicyRow = sqlx::query_as(&format!("UPDATE academic.faculty_assignment_policy SET process_instance_id = $2 WHERE id = $1 RETURNING {POLICY_COLUMNS}"))
                .bind(c.p.id).bind(&process_instance_id).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
            c.p = policy_view(r);
        }
        Some(ApprovalOutcome::Approved {
            process_instance_id,
        }) => {
            let system = Actor::system(c.actor.correlation_id.clone());
            c.p = make_effective(
                &mut c.tx,
                &c.scope,
                &c.p,
                Some("auto-approved"),
                Some(&process_instance_id),
                &system,
            )
            .await?;
        }
    }
    let outcome_s = cmd.outcome.clone();
    Ok(policy_etag(
        &c.finish(
            "academic.faculty_assignment_policy.review",
            serde_json::json!({"outcome": outcome_s, "notes": notes}),
        )
        .await?,
    ))
}

/// Approve (→ effective, superseding the prior version) or reject (→ draft) a reviewed version
/// (`faculty_assignment.decide`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignment-policies/{id}/decision",
    operation_id = "faculty_policy_decision",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the policy"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = FacultyAssignmentPolicy, headers(("ETag" = String))),
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
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentDecide)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "faculty assignment policy")
        .await?;
    let v = expect_version!(headers);
    let reason = clean_reason(d.reason, d.decision == StepDecision::Reject)?;
    let mut c = begin(&st, ctx, id, v).await?;
    if c.p.status != PolicyVersionStatus::InReview {
        return Err(conflict(format!(
            "the policy is {}; nothing awaits a decision",
            c.p.status
        ))
        .into());
    }
    let pid = c.p.process_instance_id.clone();
    c.p = match d.decision {
        StepDecision::Approve => {
            let actor = c.actor.clone();
            make_effective(
                &mut c.tx,
                &c.scope,
                &c.p,
                reason.as_deref(),
                pid.as_deref(),
                &actor,
            )
            .await?
        }
        StepDecision::Reject => {
            set_status(
                &mut c.tx,
                &c.p,
                PolicyVersionStatus::Draft,
                reason.as_deref(),
                None,
            )
            .await?
        }
    };
    Ok(policy_etag(
        &c.finish(
            "academic.faculty_assignment_policy.decide",
            serde_json::json!({"decision": d.decision, "reason": reason}),
        )
        .await?,
    ))
}

/// Retire the effective version (`faculty_assignment.decide`); assignments keep their history.
#[utoipa::path(
    post,
    path = "/api/v1/academic/faculty-assignment-policies/{id}/retire",
    operation_id = "faculty_policy_retire",
    tag = "faculty-assignments",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the policy"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<StepReason>),
    responses(
        (status = 200, body = FacultyAssignmentPolicy, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn retire(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::FacultyAssignmentDecide)?;
    let v = expect_version!(headers);
    let reason = body::<StepReason>(&raw)?.validated(false)?;
    let mut c = begin(&st, ctx, id, v).await?;
    c.p = set_status(
        &mut c.tx,
        &c.p,
        PolicyVersionStatus::Retired,
        reason.as_deref(),
        None,
    )
    .await?;
    Ok(policy_etag(
        &c.finish(
            "academic.faculty_assignment_policy.retire",
            serde_json::json!({"reason": reason}),
        )
        .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_machine_and_compatibility() {
        use PolicyVersionStatus::*;
        assert!(policy_can_transition(Draft, InReview));
        assert!(policy_can_transition(InReview, Effective));
        assert!(policy_can_transition(Effective, Superseded));
        assert!(!policy_can_transition(Draft, Effective));
        assert!(!policy_can_transition(Superseded, Effective));
        assert!(validate_config(Some(20.0), &["primary_instructor".into()], true).is_ok());
        assert!(validate_config(Some(0.0), &[], true).is_err());
        assert!(validate_config(None, &["tutor".into()], true).is_err());
        assert!(validate_config(None, &["tutor".into()], false).is_ok());
    }
}
