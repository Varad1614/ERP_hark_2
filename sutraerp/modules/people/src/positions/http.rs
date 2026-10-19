//! Positions and position assignments (L2-E02; L3-E-011 assignment, 012 change / reassignment,
//! 013 completion / termination). Occupancy is governed (`POSITION_ASSIGNMENT`); one primary
//! occupant per position unless the position allows several; one active primary assignment per
//! engagement.

use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::rows::{decode, load_engagement, load_staff};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{
    self, check_org_unit, check_version, clean, emit, expected, history, invalid, reason_of,
    required_reason, required_text, status_payload, valid_code, Decision, DecisionBody, ReasonBody,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::StaffEngagementId;
use sutra_contracts::people::ports::EngagementSummary;
use sutra_contracts::people::status::{
    EngagementStatus, PositionAssignmentStatus, PositionStatus, StaffCategory,
};
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

// ------------------------------------------------------------------ positions

/// An institutional position.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PositionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub org_unit_id: Option<Uuid>,
    #[schema(example = "PROF-CS-01")]
    pub code: String,
    pub title: String,
    pub staff_category: Option<StaffCategory>,
    pub grade: Option<String>,
    pub allows_multiple: bool,
    pub status: PositionStatus,
    pub status_reason: Option<String>,
    /// Active occupants.
    pub occupants: i64,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[schema(as = StaffPositionList)]
pub struct PositionList {
    pub items: Vec<PositionView>,
}

const P_SELECT: &str = "SELECT p.id, p.institution_id, p.org_unit_id, p.code, p.title, \
    p.staff_category, p.grade, p.allows_multiple, p.status, p.status_reason, p.version, \
    (SELECT count(*) FROM people.position_assignment a WHERE a.position_id = p.id \
     AND a.status = 'active') AS occupants FROM people.position p";

fn position_from_row(r: &PgRow) -> Result<PositionView, sqlx::Error> {
    Ok(PositionView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        org_unit_id: r.try_get("org_unit_id")?,
        code: r.try_get("code")?,
        title: r.try_get("title")?,
        staff_category: r
            .try_get::<Option<String>, _>("staff_category")?
            .map(|c| decode(c, StaffCategory::parse))
            .transpose()?,
        grade: r.try_get("grade")?,
        allows_multiple: r.try_get("allows_multiple")?,
        status: decode(r.try_get("status")?, PositionStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        occupants: r.try_get("occupants")?,
        version: r.try_get("version")?,
    })
}

async fn load_position(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PositionView, KernelError> {
    if lock {
        sqlx::query("SELECT 1 FROM people.position WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
            .bind(id)
            .bind(tenant.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!("{P_SELECT} WHERE p.id = $1 AND p.tenant_id = $2"))
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "position" })?;
    position_from_row(&row).map_err(map_db_error)
}

/// Legal position moves (L2-E02).
pub fn position_can_transition(from: PositionStatus, to: PositionStatus) -> bool {
    use PositionStatus::*;
    matches!(
        (from, to),
        (Draft, Active)
            | (Active, Frozen)
            | (Frozen, Active)
            | (Active, Retired)
            | (Frozen, Retired)
            | (Retired, Archived)
    )
}

/// Legal assignment moves (L2-E02; the subset Batch 7 uses).
pub fn assignment_can_transition(
    from: PositionAssignmentStatus,
    to: PositionAssignmentStatus,
) -> bool {
    use PositionAssignmentStatus::*;
    matches!(
        (from, to),
        (PendingApproval, Active)
            | (PendingApproval, Archived)
            | (Active, Closed)
            | (Active, Superseded)
            | (Closed, Archived)
            | (Superseded, Archived)
    )
}

/// A new position (`draft`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PositionCreate {
    pub org_unit_id: Option<Uuid>,
    /// `^[A-Z0-9][A-Z0-9_.-]{0,31}$`, unique per institution.
    pub code: String,
    pub title: String,
    pub staff_category: Option<StaffCategory>,
    pub grade: Option<String>,
    #[serde(default)]
    pub allows_multiple: bool,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PositionQuery {
    /// `PositionStatus` wire name.
    pub status: Option<String>,
}

fn valid_position_code(c: &str) -> bool {
    let b = c.as_bytes();
    (1..=32).contains(&b.len())
        && (b[0].is_ascii_uppercase() || b[0].is_ascii_digit())
        && b.iter().all(|x| {
            x.is_ascii_uppercase() || x.is_ascii_digit() || matches!(x, b'_' | b'.' | b'-')
        })
}

/// Create a position (`people.staff.manage`; Idempotency-Key). 409 for a duplicate code.
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/positions", operation_id = "people_position_create", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = PositionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PositionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PositionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let code = b.code.trim().to_uppercase();
    if !valid_position_code(&code) {
        return Err(invalid(
            "code",
            "invalid_code",
            "must match ^[A-Z0-9][A-Z0-9_.-]{0,31}$",
        )
        .into());
    }
    let title = required_text("title", &b.title, 160)?;
    let grade = valid_code("grade", b.grade)?;
    check_org_unit(&st, &scope, institution, "orgUnitId", b.org_unit_id).await?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.position WHERE tenant_id = $1 AND institution_id = $2 AND code = $3)",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&code)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if taken {
        return Err(KernelError::Conflict(format!("position code {code} is already used")).into());
    }
    let pid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.position (tenant_id, institution_id, org_unit_id, code, title, \
         staff_category, grade, allows_multiple) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.org_unit_id)
    .bind(&code)
    .bind(&title)
    .bind(b.staff_category.map(|c| c.as_str()))
    .bind(&grade)
    .bind(b.allows_multiple)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "position",
        pid,
        None,
        "draft",
        "sutraerp.people.position.created",
        None,
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "position",
        pid,
        "people.position.create",
        serde_json::json!({ "code": code }),
        &actor,
    )
    .await?;
    let p = load_position(&mut tx, t, pid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &p, Version(p.version)))
}

/// Positions of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/positions", operation_id = "people_position_list", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), PositionQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PositionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<PositionQuery>,
) -> ApiResult<Json<PositionList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            PositionStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown position status"))?,
        ),
    };
    let rows = sqlx::query(&format!(
        "{P_SELECT} WHERE p.tenant_id = $1 AND p.institution_id = $2 \
         AND ($3::text IS NULL OR p.status = $3) ORDER BY p.code"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(position_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(PositionList { items }))
}

/// One position (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/positions/{id}", operation_id = "people_position_get", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PositionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load_position(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
}

async fn position_move(
    st: &PeopleState,
    auth: &Authenticated,
    id: Uuid,
    headers: &HeaderMap,
    reason: Option<String>,
    to: PositionStatus,
    reason_required: bool,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = match crate::shared::http::if_match(headers)? {
        Ok(v) => v.0,
        Err(resp) => return Ok(resp),
    };
    let reason = if reason_required {
        Some(required_reason(reason)?)
    } else {
        reason
    };
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load_position(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    let institution = InstitutionId(p.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    if !position_can_transition(p.status, to) {
        return Err(common::illegal("position", &p.code, p.status.as_str(), to.as_str()).into());
    }
    if to == PositionStatus::Retired {
        let busy: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.position_assignment WHERE position_id = $1 \
             AND status IN ('pending_approval', 'approved', 'scheduled', 'active', 'suspended'))",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if busy {
            return Err(KernelError::Conflict(format!(
                "position {} still has occupants; end their assignments first",
                p.code
            ))
            .into());
        }
    }
    sqlx::query(
        "UPDATE people.position SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "position",
        id,
        Some(p.status.as_str()),
        to.as_str(),
        "sutraerp.people.position.status",
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "position",
        id,
        &format!("people.position.{}", to.as_str()),
        serde_json::json!({ "from": p.status, "to": to, "reason": reason }),
        &actor,
    )
    .await?;
    let out = load_position(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

macro_rules! position_command {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $to:expr, $required:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "people-workforce",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the position")),
            request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = PositionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(
            State(st): State<PeopleState>,
            auth: Authenticated,
            Path(id): Path<Uuid>,
            headers: HeaderMap,
            body: Option<Json<ReasonBody>>,
        ) -> ApiResult<Response> {
            position_move(&st, &auth, id, &headers, reason_of(body), $to, $required).await
        }
    };
}

position_command!(
    /// Activate a draft or frozen position (`people.staff.manage`).
    activate, "/api/v1/people/positions/{id}/activate", "people_position_activate", PositionStatus::Active, false);
position_command!(
    /// Freeze an active position: no new occupants (`people.staff.manage`; reason required).
    freeze, "/api/v1/people/positions/{id}/freeze", "people_position_freeze", PositionStatus::Frozen, true);
position_command!(
    /// Retire a position without occupants (`people.staff.manage`; reason required).
    retire, "/api/v1/people/positions/{id}/retire", "people_position_retire", PositionStatus::Retired, true);

// ------------------------------------------------------------------ assignments

/// An occupancy of a position by an engagement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub position_id: Uuid,
    pub position_code: String,
    pub position_title: String,
    pub engagement_id: Uuid,
    pub engagement_number: String,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub is_primary: bool,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub supersedes_id: Option<Uuid>,
    /// `completed`, `terminated`, `superseded`, `engagement_closed`.
    pub end_outcome: Option<String>,
    pub reason: Option<String>,
    pub status: PositionAssignmentStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssignmentList {
    pub items: Vec<AssignmentView>,
}

const A_SELECT: &str = "SELECT a.id, a.institution_id, a.position_id, p.code AS position_code, \
    p.title AS position_title, a.engagement_id, e.engagement_number, a.staff_id, s.staff_number, \
    s.display_name, a.is_primary, a.start_date, a.end_date, a.supersedes_id, a.end_outcome, \
    a.reason, a.status, a.status_reason, a.process_instance_id, a.version \
    FROM people.position_assignment a JOIN people.position p ON p.id = a.position_id \
    JOIN people.staff_engagement e ON e.id = a.engagement_id \
    JOIN people.staff_member s ON s.id = a.staff_id";

fn assignment_from_row(r: &PgRow) -> Result<AssignmentView, sqlx::Error> {
    Ok(AssignmentView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        position_id: r.try_get("position_id")?,
        position_code: r.try_get("position_code")?,
        position_title: r.try_get("position_title")?,
        engagement_id: r.try_get("engagement_id")?,
        engagement_number: r.try_get("engagement_number")?,
        staff_id: r.try_get("staff_id")?,
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        is_primary: r.try_get("is_primary")?,
        start_date: r.try_get("start_date")?,
        end_date: r.try_get("end_date")?,
        supersedes_id: r.try_get("supersedes_id")?,
        end_outcome: r.try_get("end_outcome")?,
        reason: r.try_get("reason")?,
        status: decode(r.try_get("status")?, PositionAssignmentStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

async fn load_assignment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<AssignmentView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF a" } else { "" };
    let row = sqlx::query(&format!(
        "{A_SELECT} WHERE a.id = $1 AND a.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "position assignment",
    })?;
    assignment_from_row(&row).map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn set_assignment(
    conn: &mut PgConnection,
    tenant: TenantId,
    a: &AssignmentView,
    to: PositionAssignmentStatus,
    end_date: Option<NaiveDate>,
    end_outcome: Option<&str>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<AssignmentView, KernelError> {
    if !assignment_can_transition(a.status, to) {
        return Err(common::illegal(
            "position assignment",
            &a.position_code,
            a.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE people.position_assignment SET status = $2, end_date = COALESCE($3, end_date), \
         end_outcome = COALESCE($4, end_outcome), status_reason = COALESCE($5, status_reason), \
         version = version + 1, updated_at = now() WHERE id = $1 AND version = $6",
    )
    .bind(a.id)
    .bind(to.as_str())
    .bind(end_date)
    .bind(end_outcome)
    .bind(reason)
    .bind(a.version)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(a.institution_id),
        "position_assignment",
        a.id,
        Some(a.status.as_str()),
        to.as_str(),
        if to == PositionAssignmentStatus::Active {
            ev::POSITION_ASSIGNED
        } else {
            ev::POSITION_ASSIGNMENT_ENDED
        },
        reason,
        a.process_instance_id.as_deref(),
        actor,
    )
    .await?;
    load_assignment(conn, tenant, a.id, false).await
}

#[allow(clippy::too_many_arguments)]
async fn assignment_event(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    before: &AssignmentView,
    after: &AssignmentView,
    event: (&'static str, &'static str),
    action: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let staff = load_staff(
        conn,
        tenant,
        sutra_contracts::people::ids::StaffMemberId(after.staff_id),
    )
    .await?;
    let e = load_engagement(conn, tenant, StaffEngagementId(after.engagement_id)).await?;
    emit(
        conn,
        scope,
        event.0,
        event.1,
        "position_assignment",
        after.id,
        action,
        &status_payload(
            after.id,
            InstitutionId(after.institution_id),
            staff.as_ref(),
            e.as_ref(),
            Some(before.status.as_str()),
            after.status.as_str(),
            after.version,
            reason,
            after.process_instance_id.as_deref(),
            None,
        ),
        actor,
    )
    .await
}

/// Activates an approved assignment: occupancy rules, then the replaced assignment is superseded.
async fn activate_assignment(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    a: &AssignmentView,
    actor: &Actor,
) -> Result<AssignmentView, KernelError> {
    let position = load_position(conn, tenant, a.position_id, true).await?;
    if position.status != PositionStatus::Active {
        return Err(KernelError::Conflict(format!(
            "position {} is {}",
            position.code, position.status
        )));
    }
    let e = load_engagement(conn, tenant, StaffEngagementId(a.engagement_id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    if e.status != EngagementStatus::Active {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {}; only an active engagement occupies a position",
            e.engagement_number, e.status
        )));
    }
    if a.is_primary && !position.allows_multiple {
        let occupied: Option<String> = sqlx::query_scalar(
            "SELECT s.staff_number FROM people.position_assignment x \
             JOIN people.staff_member s ON s.id = x.staff_id WHERE x.position_id = $1 \
             AND x.status = 'active' AND x.is_primary AND x.id <> $2 \
             AND ($3::uuid IS NULL OR x.id <> $3) LIMIT 1",
        )
        .bind(a.position_id)
        .bind(a.id)
        .bind(a.supersedes_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if let Some(n) = occupied {
            return Err(KernelError::Conflict(format!(
                "position {} is already occupied by {n}",
                position.code
            )));
        }
    }
    if let Some(old_id) = a.supersedes_id {
        let old = load_assignment(conn, tenant, old_id, true).await?;
        let end = if a.start_date > old.start_date {
            a.start_date.pred_opt().unwrap_or(a.start_date)
        } else {
            old.start_date
        };
        let ended = set_assignment(
            conn,
            tenant,
            &old,
            PositionAssignmentStatus::Superseded,
            Some(end),
            Some("superseded"),
            Some("reassigned"),
            actor,
        )
        .await?;
        assignment_event(
            conn,
            tenant,
            scope,
            &old,
            &ended,
            (
                ev::POSITION_ASSIGNMENT_ENDED,
                ev::POSITION_ASSIGNMENT_ENDED_SCHEMA,
            ),
            "people.position_assignment.supersede",
            Some("reassigned"),
            actor,
        )
        .await?;
    }
    if a.is_primary {
        let other: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.position_assignment WHERE engagement_id = $1 \
             AND status = 'active' AND is_primary AND id <> $2)",
        )
        .bind(a.engagement_id)
        .bind(a.id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if other {
            return Err(KernelError::Conflict(format!(
                "engagement {} already holds a primary position; change that assignment instead",
                a.engagement_number
            )));
        }
    }
    let active = set_assignment(
        conn,
        tenant,
        a,
        PositionAssignmentStatus::Active,
        None,
        None,
        None,
        actor,
    )
    .await?;
    assignment_event(
        conn,
        tenant,
        scope,
        a,
        &active,
        (ev::POSITION_ASSIGNED, ev::POSITION_ASSIGNED_SCHEMA),
        "people.position_assignment.activate",
        a.reason.as_deref(),
        actor,
    )
    .await?;
    Ok(active)
}

/// Inserts a pending assignment and resolves it when auto-approved.
#[allow(clippy::too_many_arguments)]
async fn new_assignment(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    position: &PositionView,
    e: &EngagementSummary,
    is_primary: bool,
    start: NaiveDate,
    end: Option<NaiveDate>,
    supersedes: Option<Uuid>,
    reason: Option<&str>,
    outcome: &ApprovalOutcome,
    actor: &Actor,
) -> Result<AssignmentView, KernelError> {
    if position.status != PositionStatus::Active {
        return Err(KernelError::Conflict(format!(
            "position {} is {}; it takes no new occupants",
            position.code, position.status
        )));
    }
    if e.institution_id.as_uuid() != position.institution_id {
        return Err(invalid(
            "engagementId",
            "other_institution",
            "the engagement belongs to another institution",
        ));
    }
    if e.status != EngagementStatus::Active {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {}; only an active engagement occupies a position",
            e.engagement_number, e.status
        )));
    }
    if position
        .staff_category
        .is_some_and(|c| c != e.staff_category)
    {
        return Err(invalid(
            "engagementId",
            "category_mismatch",
            "the position is for another staff category",
        ));
    }
    if start < e.start_date {
        return Err(invalid(
            "startDate",
            "before_engagement",
            "must not be before the engagement start date",
        ));
    }
    if end.is_some_and(|d| d < start) {
        return Err(invalid(
            "endDate",
            "before_start",
            "must not be before startDate",
        ));
    }
    let dup: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.position_assignment WHERE position_id = $1 \
         AND engagement_id = $2 AND status IN ('pending_approval', 'approved', 'scheduled', 'active'))",
    )
    .bind(position.id)
    .bind(e.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if dup {
        return Err(KernelError::Conflict(format!(
            "engagement {} already holds (or awaits) position {}",
            e.engagement_number, position.code
        )));
    }
    let pid = outcome.process_instance_id();
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO people.position_assignment (tenant_id, institution_id, position_id, \
         engagement_id, staff_id, is_primary, start_date, end_date, supersedes_id, reason, \
         process_instance_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(position.institution_id)
    .bind(position.id)
    .bind(e.id.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(is_primary)
    .bind(start)
    .bind(end)
    .bind(supersedes)
    .bind(reason)
    .bind(pid)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        e.institution_id,
        "position_assignment",
        id,
        None,
        "pending_approval",
        "sutraerp.people.position_assignment.requested",
        reason,
        Some(pid),
        actor,
    )
    .await?;
    common::audit_only(
        conn,
        scope,
        "position_assignment",
        id,
        "people.position_assignment.request",
        serde_json::json!({ "positionId": position.id, "engagementId": e.id, "supersedes": supersedes }),
        actor,
    )
    .await?;
    let a = load_assignment(conn, tenant, id, true).await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(actor.correlation_id.clone());
        return activate_assignment(conn, tenant, scope, &a, &system).await;
    }
    Ok(a)
}

async fn approval(
    st: &PeopleState,
    scope: &ExecutionScope,
    position: &PositionView,
    e: &EngagementSummary,
) -> Result<ApprovalOutcome, KernelError> {
    let mut vars = BTreeMap::new();
    vars.insert("positionCode".to_owned(), serde_json::json!(position.code));
    vars.insert(
        "engagementNumber".to_owned(),
        serde_json::json!(e.engagement_number),
    );
    request_approval(
        st,
        scope,
        process::POSITION_ASSIGNMENT,
        business_key(
            "position_assignment",
            format!("{}:{}", position.id, e.id),
            e.version,
        ),
        vars,
    )
    .await
}

/// Assign an engagement to a position.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentCreate {
    pub engagement_id: Uuid,
    /// Primary occupancy (default `true`).
    pub is_primary: Option<bool>,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub reason: Option<String>,
}

/// Reassignment to another position (L3-E-012).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentChange {
    pub position_id: Uuid,
    pub start_date: NaiveDate,
    pub reason: String,
}

/// How an occupancy ends (L3-E-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EndOutcome {
    Completed,
    Terminated,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssignmentEnd {
    pub end_date: NaiveDate,
    pub outcome: EndOutcome,
    pub reason: String,
}

async fn engagement_for(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<EngagementSummary, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_engagement(&mut conn, ctx.scope.tenant_id, StaffEngagementId(id))
        .await?
        .ok_or_else(|| invalid("engagementId", "not_found", "unknown engagement"))
}

/// Assignments of a position (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/positions/{id}/assignments", operation_id = "people_position_assignment_list", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssignmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignments(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<AssignmentList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_position(&mut conn, t, id, false).await?;
    let rows = sqlx::query(&format!("{A_SELECT} WHERE a.position_id = $1 AND a.tenant_id = $2 ORDER BY a.start_date DESC, a.created_at DESC"))
        .bind(id)
        .bind(t.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(assignment_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(AssignmentList { items }))
}

/// Assign an active engagement to an active position (`people.staff.manage`; Idempotency-Key;
/// `POSITION_ASSIGNMENT`; L3-E-011). 409 when the position is frozen / occupied.
#[utoipa::path(post, path = "/api/v1/people/positions/{id}/assignments", operation_id = "people_position_assign", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = AssignmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assign(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AssignmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_position(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let e = engagement_for(&st, ctx, b.engagement_id).await?;
    let outcome = approval(&st, &scope, &pre, &e).await?;
    let reason = clean(b.reason);
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let position = load_position(&mut tx, t, id, true).await?;
    let out = new_assignment(
        &mut tx,
        t,
        &scope,
        &position,
        &e,
        b.is_primary.unwrap_or(true),
        b.start_date,
        b.end_date,
        None,
        reason.as_deref(),
        &outcome,
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &out, Version(out.version)))
}

/// One assignment (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/position-assignments/{id}", operation_id = "people_position_assignment_get", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assignment_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let a = load_assignment(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &a, Version(a.version)))
}

async fn assignment_ctx(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<(AssignmentView, ExecutionScope), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let a = load_assignment(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(a.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    Ok((a, scope))
}

/// Decide a pending assignment (`people.approve`, step-up; If-Match). Rejection archives it.
#[utoipa::path(post, path = "/api/v1/people/position-assignments/{id}/decision", operation_id = "people_position_assignment_decide", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision_for_row(
        &st.pool,
        ctx,
        "people.position_assignment",
        "id",
        id,
        &[],
        "position assignment",
    )
    .await?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let (_, scope) = assignment_ctx(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load_assignment(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    if a.status != PositionAssignmentStatus::PendingApproval {
        return Err(common::illegal(
            "position assignment",
            &a.position_code,
            a.status.as_str(),
            "decided",
        )
        .into());
    }
    let out = if b.decision == Decision::Approve {
        activate_assignment(&mut tx, t, &scope, &a, &actor).await?
    } else {
        let r = reason.clone().unwrap_or_else(|| "rejected".into());
        let out = set_assignment(
            &mut tx,
            t,
            &a,
            PositionAssignmentStatus::Archived,
            None,
            None,
            Some(r.as_str()),
            &actor,
        )
        .await?;
        common::audit_only(
            &mut tx,
            &scope,
            "position_assignment",
            id,
            "people.position_assignment.reject",
            serde_json::json!({ "reason": reason }),
            &actor,
        )
        .await?;
        out
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Change / reassign an active assignment to another position (`people.staff.manage`; If-Match;
/// `POSITION_ASSIGNMENT`; L3-E-012). The old assignment is superseded (history kept) when the new
/// one becomes active.
#[utoipa::path(post, path = "/api/v1/people/position-assignments/{id}/change", operation_id = "people_position_assignment_change", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body = AssignmentChange, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn change(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AssignmentChange>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason))?;
    let t = ctx.scope.tenant_id;
    let (pre, scope) = assignment_ctx(&st, ctx, id).await?;
    check_version(v, pre.version)?;
    if pre.status != PositionAssignmentStatus::Active {
        return Err(common::illegal(
            "position assignment",
            &pre.position_code,
            pre.status.as_str(),
            "superseded",
        )
        .into());
    }
    if b.position_id == pre.position_id {
        return Err(invalid("positionId", "same_position", "choose another position").into());
    }
    if b.start_date < pre.start_date {
        return Err(invalid(
            "startDate",
            "before_current",
            "must not be before the current assignment start",
        )
        .into());
    }
    let target = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_position(&mut conn, t, b.position_id, false)
            .await
            .map_err(|_| invalid("positionId", "not_found", "unknown position"))?
    };
    let e = engagement_for(&st, ctx, pre.engagement_id).await?;
    let outcome = approval(&st, &scope, &target, &e).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load_assignment(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    let position = load_position(&mut tx, t, b.position_id, true).await?;
    let out = new_assignment(
        &mut tx,
        t,
        &scope,
        &position,
        &e,
        a.is_primary,
        b.start_date,
        None,
        Some(a.id),
        Some(reason.as_str()),
        &outcome,
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Complete or terminate an active assignment (`people.staff.manage`; If-Match; L3-E-013).
#[utoipa::path(post, path = "/api/v1/people/position-assignments/{id}/end", operation_id = "people_position_assignment_end", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the assignment")),
    request_body = AssignmentEnd, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssignmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn end(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AssignmentEnd>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(Some(b.reason))?;
    let t = ctx.scope.tenant_id;
    let (_, scope) = assignment_ctx(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load_assignment(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    if b.end_date < a.start_date {
        return Err(invalid(
            "endDate",
            "before_start",
            "must not be before the assignment start",
        )
        .into());
    }
    let outcome = match b.outcome {
        EndOutcome::Completed => "completed",
        EndOutcome::Terminated => "terminated",
    };
    let out = set_assignment(
        &mut tx,
        t,
        &a,
        PositionAssignmentStatus::Closed,
        Some(b.end_date),
        Some(outcome),
        Some(reason.as_str()),
        &actor,
    )
    .await?;
    assignment_event(
        &mut tx,
        t,
        &scope,
        &a,
        &out,
        (
            ev::POSITION_ASSIGNMENT_ENDED,
            ev::POSITION_ASSIGNMENT_ENDED_SCHEMA,
        ),
        "people.position_assignment.end",
        Some(reason.as_str()),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Ends every open assignment of a closed engagement (called by the engagement closure).
pub async fn end_for_engagement(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    e: &EngagementSummary,
    end_date: NaiveDate,
    actor: &Actor,
) -> Result<(), KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM people.position_assignment WHERE engagement_id = $1 AND tenant_id = $2 \
         AND status IN ('pending_approval', 'active') ORDER BY created_at",
    )
    .bind(e.id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for id in ids {
        let a = load_assignment(conn, tenant, id, true).await?;
        if a.status == PositionAssignmentStatus::PendingApproval {
            set_assignment(
                conn,
                tenant,
                &a,
                PositionAssignmentStatus::Archived,
                None,
                None,
                Some("engagement closed"),
                actor,
            )
            .await?;
            continue;
        }
        let end = end_date.max(a.start_date);
        let out = set_assignment(
            conn,
            tenant,
            &a,
            PositionAssignmentStatus::Closed,
            Some(end),
            Some("engagement_closed"),
            Some("engagement closed"),
            actor,
        )
        .await?;
        assignment_event(
            conn,
            tenant,
            scope,
            &a,
            &out,
            (
                ev::POSITION_ASSIGNMENT_ENDED,
                ev::POSITION_ASSIGNMENT_ENDED_SCHEMA,
            ),
            "people.position_assignment.end",
            Some("engagement closed"),
            actor,
        )
        .await?;
    }
    Ok(())
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get))
        .routes(routes!(activate))
        .routes(routes!(freeze))
        .routes(routes!(retire))
        .routes(routes!(assignments, assign))
        .routes(routes!(assignment_get))
        .routes(routes!(decide))
        .routes(routes!(change))
        .routes(routes!(end))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_and_assignment_machines() {
        use PositionAssignmentStatus as A;
        use PositionStatus as P;
        assert!(position_can_transition(P::Draft, P::Active));
        assert!(position_can_transition(P::Frozen, P::Active));
        assert!(!position_can_transition(P::Retired, P::Active));
        assert!(!position_can_transition(P::Draft, P::Frozen));
        assert!(assignment_can_transition(A::PendingApproval, A::Active));
        assert!(assignment_can_transition(A::Active, A::Superseded));
        assert!(!assignment_can_transition(A::Closed, A::Active));
        assert!(
            valid_position_code("PROF-CS.01")
                && !valid_position_code("-X")
                && !valid_position_code("ab")
        );
    }
}
