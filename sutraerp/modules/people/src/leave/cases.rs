//! Leave exceptions (L3-E-052: raised → under_review → resolved), leave case coordination
//! (L3-E-034: an HR case around a special / long leave whose outcome is Leave Management's
//! decision, `LEAVE_CASE_DECISION`) and long leave (L3-E-006: engagement `active → long_leave →
//! active` through the shared lifecycle helper — the only engagement move 7B makes).

use super::common::{
    audit, clean, emit_status, ensure_version, expected, history, illegal, invalid, limit,
    parse_date, required, subject_of, EventParties, LeaveDecisionBody, LeaveReasonBody, Transition,
};
use super::domain::{case_can_transition, exception_can_transition};
use super::request::load_request;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{
    require_institution, require_institution_writable, require_live_engagement,
};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_engagement_status, StatusChange};
use crate::shared::numbering::{self, kind};
use crate::shared::rows::{load_engagement, lock_engagement};
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::leave::{
    LEAVE_CASE_DECIDED, LEAVE_CASE_DECIDED_SCHEMA, LEAVE_EXCEPTION_RESOLVED,
    LEAVE_EXCEPTION_RESOLVED_SCHEMA, LONG_LEAVE_ENDED, LONG_LEAVE_ENDED_SCHEMA, LONG_LEAVE_STARTED,
    LONG_LEAVE_STARTED_SCHEMA,
};
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::status::{
    EngagementStatus, HrCaseStatus, LeaveExceptionStatus, LeaveRequestStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

// ------------------------------------------------------------------ exceptions (E-052)

/// Leave exception categories.
pub const EXCEPTION_CATEGORIES: &[&str] = &[
    "balance_shortfall",
    "policy_conflict",
    "attendance_mismatch",
    "backdated_request",
    "other",
];

/// A leave exception under review.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveExceptionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "LEX-000001")]
    pub number: String,
    pub staff_id: Option<Uuid>,
    pub leave_request_id: Option<Uuid>,
    /// `balance_shortfall`, `policy_conflict`, `attendance_mismatch`, `backdated_request`, `other`.
    pub category: String,
    pub description: String,
    pub status: LeaveExceptionStatus,
    pub resolution: Option<String>,
    pub resolved_by: Option<String>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveExceptionList {
    pub items: Vec<LeaveExceptionView>,
}

/// Raise a leave exception.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveExceptionCreate {
    pub staff_id: Option<Uuid>,
    pub leave_request_id: Option<Uuid>,
    pub category: String,
    pub description: Option<String>,
}

/// Resolve an exception.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveExceptionResolve {
    pub resolution: Option<String>,
}

/// `?status=`.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LeaveExceptionQuery {
    /// `LeaveExceptionStatus` wire name.
    pub status: Option<String>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
}

const EXC_COLUMNS: &str = "id, institution_id, number, staff_id, leave_request_id, category, \
    description, status, resolution, resolved_by, resolved_at, created_at, version";

fn exception_from_row(r: &PgRow) -> Result<LeaveExceptionView, sqlx::Error> {
    Ok(LeaveExceptionView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("number")?,
        staff_id: r.try_get("staff_id")?,
        leave_request_id: r.try_get("leave_request_id")?,
        category: r.try_get("category")?,
        description: r.try_get("description")?,
        status: crate::shared::rows::decode(r.try_get("status")?, LeaveExceptionStatus::parse)?,
        resolution: r.try_get("resolution")?,
        resolved_by: r.try_get("resolved_by")?,
        resolved_at: r.try_get("resolved_at")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    })
}

async fn load_exception(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LeaveExceptionView, KernelError> {
    let sql = format!(
        "SELECT {EXC_COLUMNS} FROM people.leave_exception WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "leave exception",
        })?;
    exception_from_row(&r).map_err(map_db_error)
}

fn exc_response(status: StatusCode, v: &LeaveExceptionView) -> Response {
    with_etag(status, v, Version(v.version))
}

/// Raise a leave exception for review (`people.leave.manage`).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/leave-exceptions", operation_id = "people_leave_exception_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveExceptionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeaveExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveExceptionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let category = b.category.trim().to_owned();
    if !EXCEPTION_CATEGORIES.contains(&category.as_str()) {
        return Err(invalid("category", "invalid", "unknown exception category").into());
    }
    let description = required("description", b.description)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut staff_id = b.staff_id;
    if let Some(rid) = b.leave_request_id {
        let r = load_request(&mut tx, t, rid, false)
            .await
            .map_err(|_| invalid("leaveRequestId", "unknown", "unknown leave request"))?;
        if r.institution_id != institution || staff_id.is_some_and(|s| s != r.staff_id.as_uuid()) {
            return Err(invalid(
                "leaveRequestId",
                "mismatch",
                "the leave request belongs to another staff member or institution",
            )
            .into());
        }
        staff_id = Some(r.staff_id.as_uuid());
    }
    if let Some(sid) = staff_id {
        let known: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.staff_member WHERE id = $1 AND tenant_id = $2 \
             AND institution_id = $3)",
        )
        .bind(sid)
        .bind(t.as_uuid())
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if !known {
            return Err(invalid("staffId", "unknown", "unknown staff member").into());
        }
    }
    let number = numbering::next(&mut tx, t, institution, kind::LEAVE_EXCEPTION).await?;
    let eid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.leave_exception (tenant_id, institution_id, number, staff_id, \
         leave_request_id, category, description, raised_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(staff_id)
    .bind(b.leave_request_id)
    .bind(&category)
    .bind(&description)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution,
            aggregate_type: "leave_exception",
            aggregate_id: eid,
            from: None,
            to: "raised",
            reason: Some(&category),
            process_instance_id: None,
            cause: "sutraerp.people.leave_exception.raised",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_exception.raise",
        "leave_exception",
        eid,
        serde_json::json!({"number": number, "category": category}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load_exception(&mut conn, t, eid, false).await?;
    Ok(exc_response(StatusCode::CREATED, &v))
}

/// Leave exceptions of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/leave-exceptions", operation_id = "people_leave_exception_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), LeaveExceptionQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveExceptionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LeaveExceptionQuery>,
) -> ApiResult<Json<LeaveExceptionList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            LeaveExceptionStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown exception status"))?,
        ),
    };
    let rows = sqlx::query(&format!(
        "SELECT {EXC_COLUMNS} FROM people.leave_exception WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::text IS NULL OR status = $3) \
         ORDER BY number DESC LIMIT $4"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(status.map(|s| s.as_str()))
    .bind(limit(q.limit))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(exception_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveExceptionList { items }))
}

async fn move_exception(
    st: &PeopleState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    v: i64,
    to: LeaveExceptionStatus,
    resolution: Option<String>,
) -> Result<Response, KernelError> {
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = load_exception(&mut tx, t, id, true).await?;
    ensure_version(v, e.version)?;
    if !exception_can_transition(e.status, to) {
        return Err(illegal(
            &format!("leave exception {}", e.number),
            e.status.as_str(),
            to.as_str(),
        ));
    }
    let institution = InstitutionId(e.institution_id);
    let scope = institution_scope(ctx, institution);
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.leave_exception SET status = $2, \
         resolution = COALESCE($3, resolution), \
         reviewed_by = CASE WHEN $2 = 'under_review' THEN $4 ELSE reviewed_by END, \
         resolved_by = CASE WHEN $2 = 'resolved' THEN $4 ELSE resolved_by END, \
         resolved_at = CASE WHEN $2 = 'resolved' THEN now() ELSE resolved_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(resolution.as_deref())
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution,
        aggregate_type: "leave_exception",
        aggregate_id: id,
        from: Some(e.status.as_str()),
        to: to.as_str(),
        reason: resolution.as_deref(),
        process_instance_id: None,
        cause: if to == LeaveExceptionStatus::Resolved {
            LEAVE_EXCEPTION_RESOLVED
        } else {
            "sutraerp.people.leave_exception.reviewed"
        },
    };
    history(&mut tx, &tr, &a).await?;
    if to == LeaveExceptionStatus::Resolved {
        let subject = subject_of(&mut tx, t, e.staff_id.map(StaffMemberId)).await?;
        emit_status(
            &mut tx,
            &scope,
            LEAVE_EXCEPTION_RESOLVED,
            LEAVE_EXCEPTION_RESOLVED_SCHEMA,
            &tr,
            version,
            &EventParties {
                staff_id: e.staff_id,
                engagement_id: None,
                staff_subject_ref: subject,
                manager_subject_ref: None,
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        &format!(
            "people.leave_exception.{}",
            if to == LeaveExceptionStatus::Resolved {
                "resolve"
            } else {
                "review"
            }
        ),
        "leave_exception",
        id,
        serde_json::json!({"resolution": resolution}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load_exception(&mut conn, t, id, false).await?;
    Ok(exc_response(StatusCode::OK, &v))
}

/// Take a raised exception under review (`people.leave.manage`).
#[utoipa::path(post, path = "/api/v1/people/leave-exceptions/{id}/review", operation_id = "people_leave_exception_review", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_review(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<LeaveReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    Ok(move_exception(&st, ctx, id, v, LeaveExceptionStatus::UnderReview, None).await?)
}

/// Resolve an exception under review with a recorded resolution (`people.leave.manage`).
#[utoipa::path(post, path = "/api/v1/people/leave-exceptions/{id}/resolve", operation_id = "people_leave_exception_resolve", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the exception")),
    request_body = LeaveExceptionResolve, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveExceptionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn exception_resolve(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveExceptionResolve>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    let resolution = required("resolution", b.resolution)?;
    Ok(move_exception(
        &st,
        ctx,
        id,
        v,
        LeaveExceptionStatus::Resolved,
        Some(resolution),
    )
    .await?)
}

// ------------------------------------------------------------------ leave cases (E-034)

/// Leave case types.
pub const CASE_TYPES: &[&str] = &[
    "special_leave",
    "medical",
    "maternity",
    "long_leave",
    "study_leave",
    "other",
];

/// An HR leave case coordinating a Leave Management decision.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveCaseView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub case_type: String,
    pub description: String,
    /// The Leave Management request the case coordinates (owned by Leave Management).
    pub leave_request_id: Option<Uuid>,
    /// The authoritative leave outcome recorded on the case (`approved`, `rejected`, `cancelled`).
    pub leave_outcome: Option<String>,
    pub status: HrCaseStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LeaveCaseList {
    pub items: Vec<LeaveCaseView>,
}

/// Open a leave case.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveCaseCreate {
    pub case_type: String,
    pub description: Option<String>,
    pub leave_request_id: Option<Uuid>,
}

/// Complete a case with Leave Management's decision.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LeaveCaseComplete {
    /// Links the request when the case was opened without one.
    pub leave_request_id: Option<Uuid>,
    pub reason: Option<String>,
}

const CASE_COLUMNS: &str = "id, institution_id, staff_id, engagement_id, case_type, description, \
    leave_request_id, leave_outcome, status, status_reason, process_instance_id, created_at, version";

fn case_from_row(r: &PgRow) -> Result<LeaveCaseView, sqlx::Error> {
    Ok(LeaveCaseView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        staff_id: r.try_get("staff_id")?,
        engagement_id: r.try_get("engagement_id")?,
        case_type: r.try_get("case_type")?,
        description: r.try_get("description")?,
        leave_request_id: r.try_get("leave_request_id")?,
        leave_outcome: r.try_get("leave_outcome")?,
        status: crate::shared::rows::decode(r.try_get("status")?, HrCaseStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    })
}

async fn load_case(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LeaveCaseView, KernelError> {
    let sql = format!(
        "SELECT {CASE_COLUMNS} FROM people.leave_case WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "leave case",
        })?;
    case_from_row(&r).map_err(map_db_error)
}

fn case_response(status: StatusCode, v: &LeaveCaseView) -> Response {
    with_etag(status, v, Version(v.version))
}

/// Moves a locked case (history + optional `leave_case.decided`).
#[allow(clippy::too_many_arguments)]
async fn move_case(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &LeaveCaseView,
    to: HrCaseStatus,
    reason: Option<&str>,
    outcome: Option<&str>,
    emit: bool,
    a: &Actor,
) -> Result<(), KernelError> {
    let t = scope.tenant_id;
    if !case_can_transition(c.status, to) {
        return Err(illegal("the leave case", c.status.as_str(), to.as_str()));
    }
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.leave_case SET status = $2, status_reason = COALESCE($3, status_reason), \
         leave_outcome = COALESCE($4, leave_outcome), decided_by = $5, version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(outcome)
    .bind(a.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution: InstitutionId(c.institution_id),
        aggregate_type: "leave_case",
        aggregate_id: c.id,
        from: Some(c.status.as_str()),
        to: to.as_str(),
        reason,
        process_instance_id: c.process_instance_id.as_deref(),
        cause: if emit {
            LEAVE_CASE_DECIDED
        } else {
            "sutraerp.people.leave_case.transitioned"
        },
    };
    history(conn, &tr, a).await?;
    if emit {
        let subject = subject_of(conn, t, Some(StaffMemberId(c.staff_id))).await?;
        emit_status(
            conn,
            scope,
            LEAVE_CASE_DECIDED,
            LEAVE_CASE_DECIDED_SCHEMA,
            &tr,
            version,
            &EventParties {
                staff_id: Some(c.staff_id),
                engagement_id: Some(c.engagement_id),
                staff_subject_ref: subject,
                manager_subject_ref: None,
            },
            a,
        )
        .await?;
    }
    Ok(())
}

/// Open a leave case for a staff member (`people.leave.manage`; `LEAVE_CASE_DECISION` →
/// pending_approval, auto-approval: processing — handed to Leave Management).
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/leave-cases", operation_id = "people_leave_case_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LeaveCaseCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LeaveCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveCaseCreate>,
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
    let case_type = b.case_type.trim().to_owned();
    if !CASE_TYPES.contains(&case_type.as_str()) {
        return Err(invalid("caseType", "invalid", "unknown leave case type").into());
    }
    let description = required("description", b.description)?;
    let e = st
        .directory
        .current_engagement(&scope, s.id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    require_live_engagement(&e)?;
    if let Some(rid) = b.leave_request_id {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let r = load_request(&mut conn, t, rid, false)
            .await
            .map_err(|_| invalid("leaveRequestId", "unknown", "unknown leave request"))?;
        if r.staff_id != s.id {
            return Err(invalid(
                "leaveRequestId",
                "mismatch",
                "the leave request belongs to another staff member",
            )
            .into());
        }
    }
    let case_id = Uuid::now_v7();
    let outcome = request_approval(
        &st,
        &scope,
        process::LEAVE_CASE_DECISION,
        business_key("leave_case", case_id, 1),
        BTreeMap::from([
            ("staffId".to_owned(), serde_json::json!(s.id)),
            ("caseType".to_owned(), serde_json::json!(case_type)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO people.leave_case (id, tenant_id, institution_id, staff_id, engagement_id, \
         case_type, description, leave_request_id, status, process_instance_id, opened_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'pending_approval', $9, $10)",
    )
    .bind(case_id)
    .bind(t.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(s.id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(&case_type)
    .bind(&description)
    .bind(b.leave_request_id)
    .bind(&pid)
    .bind(a.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (from, to) in [(None, "initiated"), (Some("initiated"), "pending_approval")] {
        history(
            &mut tx,
            &Transition {
                tenant: t,
                institution: s.institution_id,
                aggregate_type: "leave_case",
                aggregate_id: case_id,
                from,
                to,
                reason: Some(&case_type),
                process_instance_id: Some(&pid),
                cause: "sutraerp.people.leave_case.opened",
            },
            &a,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "people.leave_case.open",
        "leave_case",
        case_id,
        serde_json::json!({"caseType": case_type, "leaveRequestId": b.leave_request_id}),
        &a,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let c = load_case(&mut tx, t, case_id, true).await?;
        move_case(
            &mut tx,
            &scope,
            &c,
            HrCaseStatus::Processing,
            Some("auto-approved"),
            None,
            false,
            &Actor::system(a.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load_case(&mut conn, t, case_id, false).await?;
    Ok(case_response(StatusCode::CREATED, &v))
}

/// Leave cases of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/leave-cases", operation_id = "people_leave_case_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LeaveCaseList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LeaveCaseList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let rows = sqlx::query(&format!(
        "SELECT {CASE_COLUMNS} FROM people.leave_case WHERE tenant_id = $1 AND staff_id = $2 \
         ORDER BY created_at DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(case_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LeaveCaseList { items }))
}

/// Decide a pending leave case (`people.approve`, step-up): approve → processing (handed to
/// Leave Management), reject → rejected (`leave_case.decided`).
#[utoipa::path(post, path = "/api/v1/people/leave-cases/{id}/decision", operation_id = "people_leave_case_decide", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = LeaveDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_decide(
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
    if !b.approve() && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_case(&mut tx, t, id, true).await?;
    ensure_version(v, c.version)?;
    let scope = institution_scope(ctx, InstitutionId(c.institution_id));
    let (to, emit) = if b.approve() {
        (HrCaseStatus::Processing, false)
    } else {
        (HrCaseStatus::Rejected, true)
    };
    move_case(&mut tx, &scope, &c, to, reason.as_deref(), None, emit, &a).await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_case.decide",
        "leave_case",
        id,
        serde_json::json!({"decision": b.decision, "reason": reason}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load_case(&mut conn, t, id, false).await?;
    Ok(case_response(StatusCode::OK, &v))
}

/// Complete a processing case with Leave Management's authoritative decision
/// (`people.leave.manage`; 409 while the linked request is still undecided — the case never
/// infers approval, L4-E-034-BH-007/008). Records the outcome reference (`leave_case.decided`).
#[utoipa::path(post, path = "/api/v1/people/leave-cases/{id}/complete", operation_id = "people_leave_case_complete", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = LeaveCaseComplete, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LeaveCaseView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn case_complete(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveCaseComplete>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_case(&mut tx, t, id, true).await?;
    ensure_version(v, c.version)?;
    let scope = institution_scope(ctx, InstitutionId(c.institution_id));
    if c.status != HrCaseStatus::Processing {
        return Err(illegal("the leave case", c.status.as_str(), "completed").into());
    }
    let rid = b.leave_request_id.or(c.leave_request_id).ok_or_else(|| {
        invalid(
            "leaveRequestId",
            "required",
            "link the leave request whose decision completes the case",
        )
    })?;
    let r = load_request(&mut tx, t, rid, false)
        .await
        .map_err(|_| invalid("leaveRequestId", "unknown", "unknown leave request"))?;
    if r.staff_id.as_uuid() != c.staff_id {
        return Err(invalid(
            "leaveRequestId",
            "mismatch",
            "the leave request belongs to another staff member",
        )
        .into());
    }
    let outcome = match r.status {
        LeaveRequestStatus::Approved
        | LeaveRequestStatus::Reserved
        | LeaveRequestStatus::Consumed
        | LeaveRequestStatus::Closed => "approved",
        LeaveRequestStatus::Rejected => "rejected",
        LeaveRequestStatus::Cancelled => "cancelled",
        other => {
            return Err(KernelError::Conflict(format!(
                "leave request {} is {other}; the case completes once Leave Management has decided",
                r.number
            ))
            .into())
        }
    };
    sqlx::query("UPDATE people.leave_case SET leave_request_id = $2 WHERE id = $1")
        .bind(id)
        .bind(rid)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let reason = clean(b.reason).unwrap_or_else(|| format!("leave request {} {outcome}", r.number));
    move_case(
        &mut tx,
        &scope,
        &c,
        HrCaseStatus::Completed,
        Some(&reason),
        Some(outcome),
        true,
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.leave_case.complete",
        "leave_case",
        id,
        serde_json::json!({"leaveRequestId": rid, "leaveOutcome": outcome}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load_case(&mut conn, t, id, false).await?;
    Ok(case_response(StatusCode::OK, &v))
}

// ------------------------------------------------------------------ long leave (E-006)

/// A long leave of an engagement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LongLeaveView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub engagement_id: Uuid,
    pub leave_case_id: Option<Uuid>,
    pub reason: String,
    pub planned_start: NaiveDate,
    pub planned_end: Option<NaiveDate>,
    pub actual_start: Option<NaiveDate>,
    pub actual_end: Option<NaiveDate>,
    /// `planned`, `active`, `ended`, `cancelled`.
    pub status: String,
    /// Current engagement status and operational availability (working = available).
    pub engagement_status: EngagementStatus,
    pub available: bool,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LongLeaveList {
    pub items: Vec<LongLeaveView>,
}

/// Plan a long leave.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LongLeaveCreate {
    /// `YYYY-MM-DD`.
    pub planned_start: String,
    pub planned_end: Option<String>,
    pub reason: Option<String>,
    pub leave_case_id: Option<Uuid>,
}

const LL_COLUMNS: &str = "l.id, l.institution_id, l.staff_id, l.engagement_id, l.leave_case_id, \
    l.reason, l.planned_start, l.planned_end, l.actual_start, l.actual_end, l.status, \
    l.created_at, l.version, e.status AS engagement_status";

fn long_leave_from_row(r: &PgRow) -> Result<LongLeaveView, sqlx::Error> {
    let es = crate::shared::rows::decode(r.try_get("engagement_status")?, EngagementStatus::parse)?;
    Ok(LongLeaveView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        staff_id: r.try_get("staff_id")?,
        engagement_id: r.try_get("engagement_id")?,
        leave_case_id: r.try_get("leave_case_id")?,
        reason: r.try_get("reason")?,
        planned_start: r.try_get("planned_start")?,
        planned_end: r.try_get("planned_end")?,
        actual_start: r.try_get("actual_start")?,
        actual_end: r.try_get("actual_end")?,
        status: r.try_get("status")?,
        engagement_status: es,
        available: es.is_working(),
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    })
}

async fn load_long_leave(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LongLeaveView, KernelError> {
    let sql = format!(
        "SELECT {LL_COLUMNS} FROM people.long_leave l JOIN people.staff_engagement e \
         ON e.id = l.engagement_id WHERE l.id = $1 AND l.tenant_id = $2{}",
        if lock { " FOR UPDATE OF l" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "long leave",
        })?;
    long_leave_from_row(&r).map_err(map_db_error)
}

fn ll_response(status: StatusCode, v: &LongLeaveView) -> Response {
    with_etag(status, v, Version(v.version))
}

/// Plan a long leave of a live engagement (`people.leave.manage`; one open per engagement).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/long-leave", operation_id = "people_leave_long_create", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = LongLeaveCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LongLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn long_leave_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LongLeaveCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let reason = required("reason", b.reason)?;
    let start = parse_date("plannedStart", &b.planned_start)?;
    let end = match b.planned_end.as_deref() {
        None => None,
        Some(raw) => Some(parse_date("plannedEnd", raw)?),
    };
    if end.is_some_and(|e| e < start) {
        return Err(invalid(
            "plannedEnd",
            "end_before_start",
            "the end is before the start",
        )
        .into());
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, StaffEngagementId(id)).await?;
    require_live_engagement(&e)?;
    let scope = institution_scope(ctx, e.institution_id);
    require_institution_writable(&st, &scope, e.institution_id).await?;
    if let Some(cid) = b.leave_case_id {
        let c = load_case(&mut tx, t, cid, false)
            .await
            .map_err(|_| invalid("leaveCaseId", "unknown", "unknown leave case"))?;
        if c.engagement_id != id {
            return Err(invalid(
                "leaveCaseId",
                "mismatch",
                "the leave case belongs to another engagement",
            )
            .into());
        }
    }
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.long_leave WHERE engagement_id = $1 \
         AND status IN ('planned', 'active'))",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open {
        return Err(KernelError::Conflict(
            "the engagement already has a planned or running long leave".into(),
        )
        .into());
    }
    let lid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.long_leave (tenant_id, institution_id, staff_id, engagement_id, \
         leave_case_id, reason, planned_start, planned_end, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(e.institution_id.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(id)
    .bind(b.leave_case_id)
    .bind(&reason)
    .bind(start)
    .bind(end)
    .bind(a.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        &Transition {
            tenant: t,
            institution: e.institution_id,
            aggregate_type: "long_leave",
            aggregate_id: lid,
            from: None,
            to: "planned",
            reason: Some(&reason),
            process_instance_id: None,
            cause: "sutraerp.people.long_leave.planned",
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "people.long_leave.plan",
        "long_leave",
        lid,
        serde_json::json!({"engagementId": id, "plannedStart": start, "plannedEnd": end}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load_long_leave(&mut conn, t, lid, false).await?;
    Ok(ll_response(StatusCode::CREATED, &v))
}

/// Long leaves of an engagement with its current availability (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/engagements/{id}/long-leave", operation_id = "people_leave_long_list", tag = "people-leave",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LongLeaveList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn long_leave_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LongLeaveList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_engagement(&mut conn, t, StaffEngagementId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    let rows = sqlx::query(&format!(
        "SELECT {LL_COLUMNS} FROM people.long_leave l JOIN people.staff_engagement e \
         ON e.id = l.engagement_id WHERE l.tenant_id = $1 AND l.engagement_id = $2 \
         ORDER BY l.planned_start DESC"
    ))
    .bind(t.as_uuid())
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(long_leave_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LongLeaveList { items }))
}

async fn move_long_leave(
    st: &PeopleState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    v: i64,
    start: bool,
    reason: Option<String>,
) -> Result<Response, KernelError> {
    let t = ctx.scope.tenant_id;
    let a = actor(ctx);
    let probe = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_long_leave(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(probe.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    let today = business_today(st, &scope, institution).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let l = load_long_leave(&mut tx, t, id, true).await?;
    ensure_version(v, l.version)?;
    let (from_status, to_status, eng_to, event, schema) = if start {
        (
            "planned",
            "active",
            EngagementStatus::LongLeave,
            LONG_LEAVE_STARTED,
            LONG_LEAVE_STARTED_SCHEMA,
        )
    } else {
        (
            "active",
            "ended",
            EngagementStatus::Active,
            LONG_LEAVE_ENDED,
            LONG_LEAVE_ENDED_SCHEMA,
        )
    };
    if l.status != from_status {
        return Err(illegal("the long leave", &l.status, to_status));
    }
    if start && l.planned_start > today {
        return Err(KernelError::Conflict(format!(
            "the long leave starts on {}; start it then",
            l.planned_start
        )));
    }
    // The only engagement move 7B makes: active ↔ long_leave (shared lifecycle helper).
    let e = lock_engagement(&mut tx, t, StaffEngagementId(l.engagement_id)).await?;
    let changed = change_engagement_status(
        &mut tx,
        t,
        &e,
        eng_to,
        &StatusChange {
            cause_event_type: event,
            reason: reason.as_deref().or(Some(l.reason.as_str())),
            process_instance_id: None,
        },
        &a,
    )
    .await?;
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.long_leave SET status = $2, \
         actual_start = CASE WHEN $2 = 'active' THEN $3 ELSE actual_start END, \
         actual_end = CASE WHEN $2 = 'ended' THEN $3 ELSE actual_end END, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(id)
    .bind(to_status)
    .bind(today)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let tr = Transition {
        tenant: t,
        institution,
        aggregate_type: "long_leave",
        aggregate_id: id,
        from: Some(from_status),
        to: to_status,
        reason: reason.as_deref(),
        process_instance_id: None,
        cause: event,
    };
    history(&mut tx, &tr, &a).await?;
    let subject = subject_of(&mut tx, t, Some(e.staff_id)).await?;
    emit_status(
        &mut tx,
        &scope,
        event,
        schema,
        &tr,
        version,
        &EventParties {
            staff_id: Some(e.staff_id.as_uuid()),
            engagement_id: Some(e.id.as_uuid()),
            staff_subject_ref: subject,
            manager_subject_ref: None,
        },
        &a,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        if start {
            "people.long_leave.start"
        } else {
            "people.long_leave.end"
        },
        "long_leave",
        id,
        serde_json::json!({"engagementStatus": changed.status, "on": today}),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let v = load_long_leave(&mut conn, t, id, false).await?;
    Ok(ll_response(StatusCode::OK, &v))
}

/// Start a planned long leave (`people.leave.manage`): engagement `active → long_leave`
/// (`long_leave.started`); the staff member is unavailable until it ends.
#[utoipa::path(post, path = "/api/v1/people/long-leaves/{id}/start", operation_id = "people_leave_long_start", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the long leave")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LongLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn long_leave_start(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    Ok(move_long_leave(&st, ctx, id, v, true, clean(b.reason)).await?)
}

/// End a running long leave (`people.leave.manage`): engagement `long_leave → active`
/// (`long_leave.ended`); available again.
#[utoipa::path(post, path = "/api/v1/people/long-leaves/{id}/end", operation_id = "people_leave_long_end", tag = "people-leave",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the long leave")),
    request_body = LeaveReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LongLeaveView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn long_leave_end(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LeaveReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LeaveManage)?;
    let v = expected!(headers);
    Ok(move_long_leave(&st, ctx, id, v, false, clean(b.reason)).await?)
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(exception_create, exception_list))
        .routes(routes!(exception_review))
        .routes(routes!(exception_resolve))
        .routes(routes!(case_create, case_list))
        .routes(routes!(case_decide))
        .routes(routes!(case_complete))
        .routes(routes!(long_leave_create, long_leave_list))
        .routes(routes!(long_leave_start))
        .routes(routes!(long_leave_end))
        .with_state(state)
}
