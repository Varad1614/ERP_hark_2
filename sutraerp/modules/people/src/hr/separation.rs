//! Separation case → accepted exit hand-offs (L3-E-041; `SEPARATION_APPROVAL`) and governed
//! profile archive (L3-E-068): initiated → pending_approval → approved → processing (clearance)
//! → completed, which closes the engagement with its last working date, ends its positions and
//! archives the profile when no other engagement is live.

use super::recruitment::StatusQuery;
use crate::shared::approval::{
    business_key, process, request_approval, ApprovalOutcome, AUTO_APPROVER,
};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_staff_status, StatusChange};
use crate::shared::numbering::{kind, next};
use crate::shared::rows::{decode, load_engagement, load_staff, lock_engagement, lock_staff};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{
    self, check_version, clean, emit, expected, history, invalid, required_reason, status_payload,
    Decision, DecisionBody,
};
use crate::workforce::engagement::close_engagement;
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
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::status::{
    EngagementStatus, HrCaseStatus, SeparationKind, StaffStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One clearance line (library, finance, IT, …).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = SeparationClearanceItem)]
pub struct ClearanceItem {
    pub department: String,
    pub cleared: bool,
    pub remarks: Option<String>,
}

/// A separation case.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeparationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "SEP-000001")]
    pub number: String,
    pub engagement_id: Uuid,
    pub engagement_number: String,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub kind: SeparationKind,
    pub notice_date: Option<NaiveDate>,
    pub last_working_date: NaiveDate,
    pub reason: String,
    pub clearance: Vec<ClearanceItem>,
    pub clearance_complete: bool,
    pub archived_profile: bool,
    pub status: HrCaseStatus,
    pub status_reason: Option<String>,
    pub decided_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SeparationList {
    pub items: Vec<SeparationView>,
}

const SELECT: &str = "SELECT c.id, c.institution_id, c.case_number, c.engagement_id, \
    e.engagement_number, c.staff_id, s.staff_number, s.display_name, c.kind, c.notice_date, \
    c.last_working_date, c.reason, c.clearance, c.clearance_complete, c.archived_profile, c.status, \
    c.status_reason, c.decided_by, c.process_instance_id, c.version FROM people.separation_case c \
    JOIN people.staff_engagement e ON e.id = c.engagement_id \
    JOIN people.staff_member s ON s.id = c.staff_id";

fn from_row(r: &PgRow) -> Result<SeparationView, sqlx::Error> {
    let clearance: serde_json::Value = r.try_get("clearance")?;
    Ok(SeparationView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("case_number")?,
        engagement_id: r.try_get("engagement_id")?,
        engagement_number: r.try_get("engagement_number")?,
        staff_id: r.try_get("staff_id")?,
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        kind: decode(r.try_get("kind")?, SeparationKind::parse)?,
        notice_date: r.try_get("notice_date")?,
        last_working_date: r.try_get("last_working_date")?,
        reason: r.try_get("reason")?,
        clearance: serde_json::from_value(clearance).unwrap_or_default(),
        clearance_complete: r.try_get("clearance_complete")?,
        archived_profile: r.try_get("archived_profile")?,
        status: decode(r.try_get("status")?, HrCaseStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        decided_by: r.try_get("decided_by")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<SeparationView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF c" } else { "" };
    let row = sqlx::query(&format!(
        "{SELECT} WHERE c.id = $1 AND c.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "separation case",
    })?;
    from_row(&row).map_err(map_db_error)
}

/// Legal separation moves (generic HR case machine, L2-E04).
pub fn separation_can_transition(from: HrCaseStatus, to: HrCaseStatus) -> bool {
    use HrCaseStatus::*;
    matches!(
        (from, to),
        (Initiated, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Rejected)
            | (Approved, Processing)
            | (Processing, Processing)
            | (Processing, Completed)
    )
}

async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    c: &SeparationView,
    to: HrCaseStatus,
    reason: Option<&str>,
    decided_by: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    if !separation_can_transition(c.status, to) {
        return Err(common::illegal(
            "separation case",
            &c.number,
            c.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE people.separation_case SET status = $2, status_reason = COALESCE($3, status_reason), \
         decided_by = COALESCE($4, decided_by), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(decided_by)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if c.status != to {
        history(
            conn,
            tenant,
            InstitutionId(c.institution_id),
            "separation_case",
            c.id,
            Some(c.status.as_str()),
            to.as_str(),
            "sutraerp.people.separation_case.status",
            reason,
            c.process_instance_id.as_deref(),
            actor,
        )
        .await?;
    }
    Ok(())
}

/// Opens a separation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeparationCreate {
    pub kind: SeparationKind,
    pub notice_date: Option<NaiveDate>,
    pub last_working_date: NaiveDate,
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClearanceBody {
    pub items: Vec<ClearanceItem>,
}

#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CompleteBody {
    /// Archive the profile when no other engagement is live (default `true`).
    pub archive_profile: Option<bool>,
}

/// Open a separation case for a live engagement (`people.staff.manage`; Idempotency-Key;
/// `SEPARATION_APPROVAL`). One open case per engagement (409).
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/separation", operation_id = "people_separation_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = SeparationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SeparationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SeparationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let reason = required_reason(Some(b.reason))?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_engagement(&mut conn, t, StaffEngagementId(id))
            .await?
            .ok_or(KernelError::NotFound {
                entity: "engagement",
            })?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    if b.last_working_date < pre.start_date {
        return Err(invalid(
            "lastWorkingDate",
            "before_start",
            "must not be before the engagement start",
        )
        .into());
    }
    if b.notice_date.is_some_and(|n| n > b.last_working_date) {
        return Err(invalid(
            "noticeDate",
            "after_exit",
            "must not be after the last working date",
        )
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert(
        "engagementNumber".to_owned(),
        serde_json::json!(pre.engagement_number),
    );
    vars.insert("kind".to_owned(), serde_json::json!(b.kind.as_str()));
    let outcome = request_approval(
        &st,
        &scope,
        process::SEPARATION_APPROVAL,
        business_key("separation", pre.id, pre.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    if !e.status.is_live() || e.status == EngagementStatus::Pending {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {}; only a started, live engagement separates",
            e.engagement_number, e.status
        ))
        .into());
    }
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.separation_case WHERE engagement_id = $1 \
         AND status IN ('initiated', 'pending_approval', 'approved', 'processing'))",
    )
    .bind(e.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open {
        return Err(KernelError::Conflict(format!(
            "engagement {} already has an open separation case",
            e.engagement_number
        ))
        .into());
    }
    let number = next(&mut tx, t, e.institution_id, kind::SEPARATION).await?;
    let pid = outcome.process_instance_id();
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.separation_case (tenant_id, institution_id, case_number, engagement_id, \
         staff_id, kind, notice_date, last_working_date, reason, status, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending_approval', $10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(e.institution_id.as_uuid())
    .bind(&number)
    .bind(e.id.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(b.kind.as_str())
    .bind(b.notice_date)
    .bind(b.last_working_date)
    .bind(&reason)
    .bind(pid)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        e.institution_id,
        "separation_case",
        cid,
        None,
        "pending_approval",
        ev::SEPARATION_SUBMITTED,
        Some(reason.as_str()),
        Some(pid),
        &actor,
    )
    .await?;
    let c = load(&mut tx, t, cid, true).await?;
    let staff = load_staff(&mut tx, t, e.staff_id).await?;
    let label = format!("{} {}", number, b.kind.as_str());
    emit(
        &mut tx,
        &scope,
        ev::SEPARATION_SUBMITTED,
        ev::SEPARATION_SUBMITTED_SCHEMA,
        "separation_case",
        cid,
        "people.separation_case.submit",
        &status_payload(
            cid,
            e.institution_id,
            staff.as_ref(),
            Some(&e),
            None,
            c.status.as_str(),
            c.version,
            Some(label.as_str()),
            Some(pid),
            None,
        ),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        set_status(
            &mut tx,
            t,
            &c,
            HrCaseStatus::Approved,
            None,
            Some(AUTO_APPROVER),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let out = load(&mut tx, t, cid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &out, Version(out.version)))
}

/// The latest separation case of an engagement (`people.read`; 404 when none).
#[utoipa::path(get, path = "/api/v1/people/engagements/{id}/separation", operation_id = "people_separation_of_engagement", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SeparationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn of_engagement(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let row = sqlx::query(&format!("{SELECT} WHERE c.engagement_id = $1 AND c.tenant_id = $2 ORDER BY c.created_at DESC LIMIT 1"))
        .bind(id)
        .bind(ctx.scope.tenant_id.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "separation case",
        })?;
    let c = from_row(&row).map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &c, Version(c.version)))
}

/// Separation cases of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/separation-cases", operation_id = "people_separation_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), StatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = SeparationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<SeparationList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            HrCaseStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown case status"))?,
        ),
    };
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE c.tenant_id = $1 AND c.institution_id = $2 \
         AND ($3::text IS NULL OR c.status = $3) ORDER BY c.case_number DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(SeparationList { items }))
}

/// One separation case (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/separation-cases/{id}", operation_id = "people_separation_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SeparationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &c, Version(c.version)))
}

async fn case_scope(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    Ok(scope)
}

/// Decide a pending separation (`people.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/people/separation-cases/{id}/decision", operation_id = "people_separation_decide", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SeparationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == Decision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    let to = if b.decision == Decision::Approve {
        HrCaseStatus::Approved
    } else {
        HrCaseStatus::Rejected
    };
    set_status(
        &mut tx,
        t,
        &c,
        to,
        reason.as_deref(),
        actor.id.as_deref(),
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "separation_case",
        id,
        "people.separation_case.decide",
        serde_json::json!({ "decision": to, "reason": reason }),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Record clearance (`people.staff.manage`; If-Match): approved → processing; lines merge by
/// department; the clearance is complete when every line is cleared.
#[utoipa::path(post, path = "/api/v1/people/separation-cases/{id}/clearance", operation_id = "people_separation_clearance", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body = ClearanceBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SeparationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn clearance(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ClearanceBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    if b.items.is_empty() {
        return Err(invalid("items", "required", "at least one clearance line").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    let mut lines = c.clearance.clone();
    for (i, item) in b.items.iter().enumerate() {
        let dept = item.department.trim();
        if dept.is_empty() || dept.chars().count() > 80 {
            return Err(invalid(
                &format!("items[{i}].department"),
                "required",
                "1 to 80 characters",
            )
            .into());
        }
        let line = ClearanceItem {
            department: dept.to_owned(),
            cleared: item.cleared,
            remarks: clean(item.remarks.clone()),
        };
        match lines
            .iter_mut()
            .find(|l| l.department.eq_ignore_ascii_case(dept))
        {
            Some(l) => *l = line,
            None => lines.push(line),
        }
    }
    let complete = lines.iter().all(|l| l.cleared);
    sqlx::query(
        "UPDATE people.separation_case SET clearance = $2, clearance_complete = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(serde_json::to_value(&lines).map_err(KernelError::internal)?)
    .bind(complete)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    set_status(&mut tx, t, &c, HrCaseStatus::Processing, None, None, &actor).await?;
    common::audit_only(
        &mut tx,
        &scope,
        "separation_case",
        id,
        "people.separation_case.clearance",
        serde_json::json!({ "lines": lines.len(), "complete": complete }),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Complete the separation (`people.staff.manage`; If-Match; clearance complete): closes the
/// engagement with the last working date, ends its positions, archives the profile (L3-E-068).
#[utoipa::path(post, path = "/api/v1/people/separation-cases/{id}/complete", operation_id = "people_separation_complete", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the case")),
    request_body(content = CompleteBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SeparationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn complete(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<CompleteBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let archive = body.and_then(|b| b.0.archive_profile).unwrap_or(true);
    let t = ctx.scope.tenant_id;
    let scope = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    if c.status != HrCaseStatus::Processing {
        return Err(
            common::illegal("separation case", &c.number, c.status.as_str(), "completed").into(),
        );
    }
    if !c.clearance_complete {
        return Err(KernelError::Conflict(format!(
            "separation case {}: clearance is not complete",
            c.number
        ))
        .into());
    }
    let e = lock_engagement(&mut tx, t, StaffEngagementId(c.engagement_id)).await?;
    let note = format!("{} ({}): {}", c.kind.as_str(), c.number, c.reason);
    let closed = if e.status.is_live() {
        close_engagement(&mut tx, t, &scope, &e, c.last_working_date, &note, &actor).await?
    } else {
        e
    };
    let mut archived = false;
    if archive {
        let staff = lock_staff(&mut tx, t, StaffMemberId(c.staff_id)).await?;
        let live: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.staff_engagement WHERE staff_id = $1 AND status NOT IN ('closed', 'cancelled'))",
        )
        .bind(c.staff_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if !live && matches!(staff.status, StaffStatus::Active | StaffStatus::Suspended) {
            let moved = change_staff_status(
                &mut tx,
                t,
                &staff,
                StaffStatus::Archived,
                &StatusChange {
                    cause_event_type: ev::STAFF_ARCHIVED,
                    reason: Some(note.as_str()),
                    process_instance_id: None,
                },
                &actor,
            )
            .await?;
            emit(
                &mut tx,
                &scope,
                ev::STAFF_ARCHIVED,
                ev::STAFF_ARCHIVED_SCHEMA,
                "staff_member",
                staff.id.as_uuid(),
                "people.staff_member.archive",
                &status_payload(
                    staff.id.as_uuid(),
                    staff.institution_id,
                    Some(&moved),
                    None,
                    Some(staff.status.as_str()),
                    moved.status.as_str(),
                    moved.version,
                    Some(note.as_str()),
                    None,
                    None,
                ),
                &actor,
            )
            .await?;
            archived = true;
        }
    }
    sqlx::query("UPDATE people.separation_case SET archived_profile = $2 WHERE id = $1")
        .bind(id)
        .bind(archived)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    set_status(&mut tx, t, &c, HrCaseStatus::Completed, None, None, &actor).await?;
    let out = load(&mut tx, t, id, false).await?;
    let staff = load_staff(&mut tx, t, StaffMemberId(c.staff_id)).await?;
    emit(
        &mut tx,
        &scope,
        ev::SEPARATION_COMPLETED,
        ev::SEPARATION_COMPLETED_SCHEMA,
        "separation_case",
        id,
        "people.separation_case.complete",
        &status_payload(
            id,
            InstitutionId(c.institution_id),
            staff.as_ref(),
            Some(&closed),
            Some(c.status.as_str()),
            out.status.as_str(),
            out.version,
            Some(c.kind.as_str()),
            c.process_instance_id.as_deref(),
            None,
        ),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, of_engagement))
        .routes(routes!(list))
        .routes(routes!(get))
        .routes(routes!(decide))
        .routes(routes!(clearance))
        .routes(routes!(complete))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separation_machine() {
        use HrCaseStatus::*;
        assert!(separation_can_transition(PendingApproval, Approved));
        assert!(separation_can_transition(Approved, Processing));
        assert!(separation_can_transition(Processing, Completed));
        assert!(!separation_can_transition(Approved, Completed));
        assert!(!separation_can_transition(Rejected, Approved));
    }
}
