//! Recruitment (L3-E-033, first half): requisitions (draft → pending_approval
//! (`REQUISITION_APPROVAL`) → approved → open → filled | cancelled) and candidates (applied →
//! shortlisted → interviewed → offered → offer_accepted | offer_declined | rejected | withdrawn).

use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::rows::decode;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{
    self, check_campus, check_org_unit, check_version, clean, emit, expected, history, invalid,
    optional_text, reason_of, required_reason, required_text, valid_code, Decision, DecisionBody,
    ReasonBody,
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
use sutra_contracts::people::status::{
    CandidateStatus, EmploymentType, RequisitionStatus, StaffCategory,
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

// ------------------------------------------------------------------ requisitions

/// A hiring requisition.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequisitionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "REQ-000001")]
    pub number: String,
    pub title: String,
    pub position_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub staff_category: StaffCategory,
    pub employment_type: EmploymentType,
    pub designation: String,
    pub grade: Option<String>,
    pub openings: i32,
    /// Candidates who joined through onboarding.
    pub filled: i64,
    pub justification: Option<String>,
    pub status: RequisitionStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RequisitionList {
    pub items: Vec<RequisitionView>,
}

const R_SELECT: &str =
    "SELECT r.id, r.institution_id, r.requisition_number, r.title, r.position_id, \
    r.org_unit_id, r.campus_id, r.staff_category, r.employment_type, r.designation, r.grade, \
    r.openings, r.justification, r.status, r.status_reason, r.process_instance_id, r.version, \
    (SELECT count(*) FROM people.onboarding_case o WHERE o.requisition_id = r.id \
     AND o.status = 'joined') AS filled FROM people.requisition r";

fn requisition_from_row(r: &PgRow) -> Result<RequisitionView, sqlx::Error> {
    Ok(RequisitionView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("requisition_number")?,
        title: r.try_get("title")?,
        position_id: r.try_get("position_id")?,
        org_unit_id: r.try_get("org_unit_id")?,
        campus_id: r.try_get("campus_id")?,
        staff_category: decode(r.try_get("staff_category")?, StaffCategory::parse)?,
        employment_type: decode(r.try_get("employment_type")?, EmploymentType::parse)?,
        designation: r.try_get("designation")?,
        grade: r.try_get("grade")?,
        openings: r.try_get("openings")?,
        filled: r.try_get("filled")?,
        justification: r.try_get("justification")?,
        status: decode(r.try_get("status")?, RequisitionStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

/// Loads a requisition (404), optionally locked.
pub async fn load_requisition(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<RequisitionView, KernelError> {
    if lock {
        sqlx::query("SELECT 1 FROM people.requisition WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
            .bind(id)
            .bind(tenant.as_uuid())
            .fetch_optional(&mut *conn)
            .await
            .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!("{R_SELECT} WHERE r.id = $1 AND r.tenant_id = $2"))
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "requisition",
        })?;
    requisition_from_row(&row).map_err(map_db_error)
}

/// Legal requisition moves.
pub fn requisition_can_transition(from: RequisitionStatus, to: RequisitionStatus) -> bool {
    use RequisitionStatus::*;
    matches!(
        (from, to),
        (Draft, PendingApproval)
            | (PendingApproval, Approved)
            | (PendingApproval, Draft)
            | (Approved, Open)
            | (Open, Filled)
            | (Draft | PendingApproval | Approved | Open, Cancelled)
    )
}

/// Moves a locked requisition (history + version).
pub async fn set_requisition(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &RequisitionView,
    to: RequisitionStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<RequisitionView, KernelError> {
    if !requisition_can_transition(r.status, to) {
        return Err(common::illegal(
            "requisition",
            &r.number,
            r.status.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE people.requisition SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(r.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(r.institution_id),
        "requisition",
        r.id,
        Some(r.status.as_str()),
        to.as_str(),
        "sutraerp.people.requisition.status",
        reason,
        process_instance_id.or(r.process_instance_id.as_deref()),
        actor,
    )
    .await?;
    load_requisition(conn, tenant, r.id, false).await
}

/// A new requisition (`draft`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RequisitionCreate {
    pub title: String,
    pub position_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub staff_category: StaffCategory,
    pub employment_type: EmploymentType,
    pub designation: String,
    pub grade: Option<String>,
    pub openings: i32,
    pub justification: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct StatusQuery {
    /// Status wire name.
    pub status: Option<String>,
}

/// Create a requisition (`people.staff.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/people/institutions/{id}/requisitions", operation_id = "people_recruitment_requisition_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = RequisitionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = RequisitionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requisition_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<RequisitionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let title = required_text("title", &b.title, 160)?;
    let designation = required_text("designation", &b.designation, 120)?;
    let grade = valid_code("grade", b.grade)?;
    let justification = optional_text("justification", b.justification, 2000)?;
    if !(1..=500).contains(&b.openings) {
        return Err(invalid("openings", "out_of_range", "1 to 500").into());
    }
    check_org_unit(&st, &scope, institution, "orgUnitId", b.org_unit_id).await?;
    check_campus(&st, &scope, institution, "campusId", b.campus_id).await?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    if let Some(p) = b.position_id {
        let ok: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.position WHERE id = $1 AND tenant_id = $2 \
             AND institution_id = $3 AND status IN ('draft', 'active'))",
        )
        .bind(p)
        .bind(t.as_uuid())
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if !ok {
            return Err(invalid("positionId", "not_found", "unknown or retired position").into());
        }
    }
    let number = next(&mut tx, t, institution, kind::REQUISITION).await?;
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.requisition (tenant_id, institution_id, requisition_number, title, \
         position_id, org_unit_id, campus_id, staff_category, employment_type, designation, grade, \
         openings, justification) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(&title)
    .bind(b.position_id)
    .bind(b.org_unit_id)
    .bind(b.campus_id)
    .bind(b.staff_category.as_str())
    .bind(b.employment_type.as_str())
    .bind(&designation)
    .bind(&grade)
    .bind(b.openings)
    .bind(&justification)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "requisition",
        rid,
        None,
        "draft",
        "sutraerp.people.requisition.created",
        None,
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "requisition",
        rid,
        "people.requisition.create",
        serde_json::json!({ "number": number }),
        &actor,
    )
    .await?;
    let r = load_requisition(&mut tx, t, rid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &r, Version(r.version)))
}

/// Requisitions of an institution (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/requisitions", operation_id = "people_recruitment_requisition_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), StatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = RequisitionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requisition_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StatusQuery>,
) -> ApiResult<Json<RequisitionList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            RequisitionStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown requisition status"))?,
        ),
    };
    let rows = sqlx::query(&format!(
        "{R_SELECT} WHERE r.tenant_id = $1 AND r.institution_id = $2 \
         AND ($3::text IS NULL OR r.status = $3) ORDER BY r.requisition_number DESC"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(requisition_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(RequisitionList { items }))
}

/// One requisition (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/requisitions/{id}", operation_id = "people_recruitment_requisition_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = RequisitionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requisition_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_requisition(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

async fn requisition_scope(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<(RequisitionView, ExecutionScope), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_requisition(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(r.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    Ok((r, scope))
}

/// approve: pending_approval → approved → open; reject: back to draft.
async fn decide_requisition(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    r: &RequisitionView,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<RequisitionView, KernelError> {
    let out = if approve {
        let approved = set_requisition(
            conn,
            tenant,
            r,
            RequisitionStatus::Approved,
            reason,
            None,
            actor,
        )
        .await?;
        set_requisition(
            conn,
            tenant,
            &approved,
            RequisitionStatus::Open,
            reason,
            None,
            actor,
        )
        .await?
    } else {
        set_requisition(
            conn,
            tenant,
            r,
            RequisitionStatus::Draft,
            reason,
            None,
            actor,
        )
        .await?
    };
    emit(
        conn,
        scope,
        ev::REQUISITION_DECIDED,
        ev::REQUISITION_DECIDED_SCHEMA,
        "requisition",
        r.id,
        if approve {
            "people.requisition.approve"
        } else {
            "people.requisition.reject"
        },
        &common::status_payload(
            r.id,
            InstitutionId(r.institution_id),
            None,
            None,
            Some(r.status.as_str()),
            out.status.as_str(),
            out.version,
            reason,
            out.process_instance_id.as_deref(),
            None,
        ),
        actor,
    )
    .await?;
    Ok(out)
}

/// Submit a draft requisition (`people.staff.manage`; If-Match; `REQUISITION_APPROVAL`). Approval
/// opens it for candidates.
#[utoipa::path(post, path = "/api/v1/people/requisitions/{id}/submit", operation_id = "people_recruitment_requisition_submit", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RequisitionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requisition_submit(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = reason_of(body);
    let t = ctx.scope.tenant_id;
    let (pre, scope) = requisition_scope(&st, ctx, id).await?;
    check_version(v, pre.version)?;
    if pre.status != RequisitionStatus::Draft {
        return Err(common::illegal(
            "requisition",
            &pre.number,
            pre.status.as_str(),
            "pending_approval",
        )
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert(
        "requisitionNumber".to_owned(),
        serde_json::json!(pre.number),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::REQUISITION_APPROVAL,
        business_key("requisition", pre.id, pre.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_requisition(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    let pending = set_requisition(
        &mut tx,
        t,
        &r,
        RequisitionStatus::PendingApproval,
        reason.as_deref(),
        Some(outcome.process_instance_id()),
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "requisition",
        id,
        "people.requisition.submit",
        serde_json::json!({ "processInstanceId": outcome.process_instance_id() }),
        &actor,
    )
    .await?;
    let out = if let ApprovalOutcome::Approved { .. } = outcome {
        decide_requisition(
            &mut tx,
            t,
            &scope,
            &pending,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        pending
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Decide a pending requisition (`people.approve`, step-up; If-Match). Rejection returns it to draft.
#[utoipa::path(post, path = "/api/v1/people/requisitions/{id}/decision", operation_id = "people_recruitment_requisition_decide", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RequisitionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requisition_decide(
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
    let (_, scope) = requisition_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_requisition(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    if r.status != RequisitionStatus::PendingApproval {
        return Err(common::illegal("requisition", &r.number, r.status.as_str(), "decided").into());
    }
    let out = decide_requisition(
        &mut tx,
        t,
        &scope,
        &r,
        b.decision == Decision::Approve,
        reason.as_deref(),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Close a requisition (`people.staff.manage`; If-Match; reason required): an open requisition
/// with joined hires becomes `filled`, anything else `cancelled`.
#[utoipa::path(post, path = "/api/v1/people/requisitions/{id}/close", operation_id = "people_recruitment_requisition_close", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = RequisitionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn requisition_close(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = required_reason(reason_of(body))?;
    let t = ctx.scope.tenant_id;
    let (_, scope) = requisition_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_requisition(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    let to = if r.status == RequisitionStatus::Open && r.filled > 0 {
        RequisitionStatus::Filled
    } else {
        RequisitionStatus::Cancelled
    };
    let out = set_requisition(&mut tx, t, &r, to, Some(reason.as_str()), None, &actor).await?;
    common::audit_only(
        &mut tx,
        &scope,
        "requisition",
        id,
        "people.requisition.close",
        serde_json::json!({ "to": to, "reason": reason }),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

// ------------------------------------------------------------------ candidates

/// A candidate of a requisition.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CandidateView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub requisition_id: Uuid,
    pub requisition_number: String,
    pub given_name: String,
    pub family_name: Option<String>,
    pub email: String,
    pub phone: Option<String>,
    pub source: Option<String>,
    pub notes: Option<String>,
    pub offered_designation: Option<String>,
    pub offered_grade: Option<String>,
    pub proposed_joining_date: Option<NaiveDate>,
    pub status: CandidateStatus,
    pub status_reason: Option<String>,
    pub onboarding_case_id: Option<Uuid>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CandidateList {
    pub items: Vec<CandidateView>,
}

const C_SELECT: &str = "SELECT c.id, c.institution_id, c.requisition_id, r.requisition_number, \
    c.given_name, c.family_name, c.email, c.phone, c.source, c.notes, c.offered_designation, \
    c.offered_grade, c.proposed_joining_date, c.status, c.status_reason, c.version, \
    (SELECT o.id FROM people.onboarding_case o WHERE o.candidate_id = c.id \
     AND o.status <> 'cancelled' LIMIT 1) AS onboarding_case_id \
    FROM people.candidate c JOIN people.requisition r ON r.id = c.requisition_id";

fn candidate_from_row(r: &PgRow) -> Result<CandidateView, sqlx::Error> {
    Ok(CandidateView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        requisition_id: r.try_get("requisition_id")?,
        requisition_number: r.try_get("requisition_number")?,
        given_name: r.try_get("given_name")?,
        family_name: r.try_get("family_name")?,
        email: r.try_get("email")?,
        phone: r.try_get("phone")?,
        source: r.try_get("source")?,
        notes: r.try_get("notes")?,
        offered_designation: r.try_get("offered_designation")?,
        offered_grade: r.try_get("offered_grade")?,
        proposed_joining_date: r.try_get("proposed_joining_date")?,
        status: decode(r.try_get("status")?, CandidateStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        onboarding_case_id: r.try_get("onboarding_case_id")?,
        version: r.try_get("version")?,
    })
}

/// Loads a candidate (404), optionally locked.
pub async fn load_candidate(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<CandidateView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF c" } else { "" };
    let row = sqlx::query(&format!(
        "{C_SELECT} WHERE c.id = $1 AND c.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "candidate",
    })?;
    candidate_from_row(&row).map_err(map_db_error)
}

/// Legal candidate moves.
pub fn candidate_can_transition(from: CandidateStatus, to: CandidateStatus) -> bool {
    use CandidateStatus::*;
    matches!(
        (from, to),
        (Applied, Shortlisted)
            | (Shortlisted, Interviewed)
            | (Shortlisted | Interviewed, Offered)
            | (Offered, OfferAccepted)
            | (Offered, OfferDeclined)
            | (Applied | Shortlisted | Interviewed | Offered, Rejected)
            | (Applied | Shortlisted | Interviewed | Offered, Withdrawn)
    )
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CandidateCreate {
    pub given_name: String,
    pub family_name: Option<String>,
    pub email: String,
    pub phone: Option<String>,
    pub source: Option<String>,
    pub notes: Option<String>,
}

/// Interview notes / decline / reject reason.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CandidateStep {
    pub notes: Option<String>,
    pub reason: Option<String>,
}

/// Offer terms.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferBody {
    /// Defaults to the requisition's designation.
    pub offered_designation: Option<String>,
    pub offered_grade: Option<String>,
    pub proposed_joining_date: NaiveDate,
}

/// Add a candidate to an open requisition (`people.staff.manage`; Idempotency-Key). 409 for a
/// duplicate e-mail on the same requisition.
#[utoipa::path(post, path = "/api/v1/people/requisitions/{id}/candidates", operation_id = "people_recruitment_candidate_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = CandidateCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CandidateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn candidate_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CandidateCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let (_, scope) = requisition_scope(&st, ctx, id).await?;
    let given = required_text("givenName", &b.given_name, 100)?;
    let family = optional_text("familyName", b.family_name, 100)?;
    let email = b.email.trim().to_lowercase();
    let at = email.split('@').collect::<Vec<_>>();
    if at.len() != 2 || at.iter().any(|p| p.is_empty()) || email.contains(char::is_whitespace) {
        return Err(invalid("email", "invalid_email", "must be an e-mail address").into());
    }
    let phone = optional_text("phone", b.phone, 32)?;
    if phone.as_deref().is_some_and(|p| p.chars().count() < 3) {
        return Err(invalid("phone", "invalid_phone", "3 to 32 characters").into());
    }
    let source = optional_text("source", b.source, 120)?;
    let notes = optional_text("notes", b.notes, 2000)?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_requisition(&mut tx, t, id, true).await?;
    if r.status != RequisitionStatus::Open {
        return Err(KernelError::Conflict(format!(
            "requisition {} is {}; candidates join open requisitions",
            r.number, r.status
        ))
        .into());
    }
    let dup: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM people.candidate WHERE requisition_id = $1 AND lower(email) = $2)")
        .bind(id)
        .bind(&email)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    if dup {
        return Err(
            KernelError::Conflict(format!("{email} already applied to {}", r.number)).into(),
        );
    }
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.candidate (tenant_id, institution_id, requisition_id, given_name, \
         family_name, email, phone, source, notes) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(r.institution_id)
    .bind(id)
    .bind(&given)
    .bind(&family)
    .bind(&email)
    .bind(&phone)
    .bind(&source)
    .bind(&notes)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        InstitutionId(r.institution_id),
        "candidate",
        cid,
        None,
        "applied",
        "sutraerp.people.candidate.created",
        None,
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "candidate",
        cid,
        "people.candidate.create",
        serde_json::json!({ "requisition": r.number }),
        &actor,
    )
    .await?;
    let c = load_candidate(&mut tx, t, cid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &c, Version(c.version)))
}

/// Candidates of a requisition (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/requisitions/{id}/candidates", operation_id = "people_recruitment_candidate_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CandidateList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn candidate_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CandidateList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load_requisition(&mut conn, t, id, false).await?;
    let rows = sqlx::query(&format!(
        "{C_SELECT} WHERE c.requisition_id = $1 AND c.tenant_id = $2 ORDER BY c.created_at"
    ))
    .bind(id)
    .bind(t.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(candidate_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(CandidateList { items }))
}

/// One candidate (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/candidates/{id}", operation_id = "people_recruitment_candidate_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CandidateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn candidate_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load_candidate(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &c, Version(c.version)))
}

#[derive(Clone, Copy)]
enum Step {
    Shortlist,
    Interview,
    Offer,
    Accept,
    Decline,
    Reject,
}

#[allow(clippy::too_many_arguments)]
async fn candidate_step(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
    v: i64,
    step: Step,
    notes: Option<String>,
    reason: Option<String>,
    offer: Option<OfferBody>,
) -> ApiResult<Response> {
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_candidate(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    let (to, action) = match step {
        Step::Shortlist => (CandidateStatus::Shortlisted, "people.candidate.shortlist"),
        Step::Interview => (CandidateStatus::Interviewed, "people.candidate.interview"),
        Step::Offer => (CandidateStatus::Offered, "people.candidate.offer"),
        Step::Accept => (CandidateStatus::OfferAccepted, "people.candidate.accept"),
        Step::Decline => (CandidateStatus::OfferDeclined, "people.candidate.decline"),
        Step::Reject => (CandidateStatus::Rejected, "people.candidate.reject"),
    };
    let reason = if matches!(step, Step::Reject | Step::Decline) {
        Some(required_reason(reason)?)
    } else {
        clean(reason)
    };
    let notes = optional_text("notes", notes, 2000)?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_candidate(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    if !candidate_can_transition(c.status, to) {
        return Err(common::illegal("candidate", &c.email, c.status.as_str(), to.as_str()).into());
    }
    let r = load_requisition(&mut tx, t, c.requisition_id, true).await?;
    if matches!(step, Step::Offer | Step::Accept) && r.status != RequisitionStatus::Open {
        return Err(
            KernelError::Conflict(format!("requisition {} is {}", r.number, r.status)).into(),
        );
    }
    let (mut designation, mut grade, mut joining) = (
        c.offered_designation.clone(),
        c.offered_grade.clone(),
        c.proposed_joining_date,
    );
    if let Some(o) = offer {
        designation = Some(match o.offered_designation {
            Some(d) => required_text("offeredDesignation", &d, 120)?,
            None => r.designation.clone(),
        });
        grade = valid_code("offeredGrade", o.offered_grade)?.or_else(|| r.grade.clone());
        joining = Some(o.proposed_joining_date);
    }
    if matches!(step, Step::Accept) {
        let accepted: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM people.candidate WHERE requisition_id = $1 AND status = 'offer_accepted'",
        )
        .bind(r.id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if accepted >= i64::from(r.openings) {
            return Err(KernelError::Conflict(format!(
                "requisition {} has no opening left ({} of {} accepted)",
                r.number, accepted, r.openings
            ))
            .into());
        }
    }
    sqlx::query(
        "UPDATE people.candidate SET status = $2, status_reason = $3, notes = COALESCE($4, notes), \
         offered_designation = $5, offered_grade = $6, proposed_joining_date = $7, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .bind(notes.as_deref())
    .bind(&designation)
    .bind(&grade)
    .bind(joining)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "candidate",
        id,
        Some(c.status.as_str()),
        to.as_str(),
        "sutraerp.people.candidate.status",
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "candidate",
        id,
        action,
        serde_json::json!({ "from": c.status, "to": to, "reason": reason }),
        &actor,
    )
    .await?;
    let out = load_candidate(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

macro_rules! candidate_command {
    ($(#[$m:meta])* $fn:ident, $path:literal, $op:literal, $step:expr) => {
        $(#[$m])*
        #[utoipa::path(post, path = $path, operation_id = $op, tag = "people-hr",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the candidate")),
            request_body(content = CandidateStep, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
            responses((status = 200, body = CandidateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
        pub async fn $fn(
            State(st): State<PeopleState>,
            auth: Authenticated,
            Path(id): Path<Uuid>,
            headers: HeaderMap,
            body: Option<Json<CandidateStep>>,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::StaffManage)?;
            let v = expected!(headers);
            let b = body.map(|b| b.0).unwrap_or_default();
            candidate_step(&st, ctx, id, v, $step, b.notes, b.reason, None).await
        }
    };
}

candidate_command!(
    /// Shortlist an applicant (`people.staff.manage`).
    candidate_shortlist, "/api/v1/people/candidates/{id}/shortlist", "people_recruitment_candidate_shortlist", Step::Shortlist);
candidate_command!(
    /// Record the interview (`people.staff.manage`; optional notes).
    candidate_interview, "/api/v1/people/candidates/{id}/interview", "people_recruitment_candidate_interview", Step::Interview);
candidate_command!(
    /// The candidate accepted the offer (`people.staff.manage`); 409 when the requisition's openings are taken.
    candidate_accept, "/api/v1/people/candidates/{id}/accept", "people_recruitment_candidate_accept", Step::Accept);
candidate_command!(
    /// The candidate declined the offer (`people.staff.manage`; reason required).
    candidate_decline, "/api/v1/people/candidates/{id}/decline", "people_recruitment_candidate_decline", Step::Decline);
candidate_command!(
    /// Reject a candidate (`people.staff.manage`; reason required).
    candidate_reject, "/api/v1/people/candidates/{id}/reject", "people_recruitment_candidate_reject", Step::Reject);

/// Make an offer (`people.staff.manage`; If-Match): designation / grade default to the
/// requisition's.
#[utoipa::path(post, path = "/api/v1/people/candidates/{id}/offer", operation_id = "people_recruitment_candidate_offer", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the candidate")),
    request_body = OfferBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CandidateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn candidate_offer(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<OfferBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    candidate_step(&st, ctx, id, v, Step::Offer, None, None, Some(b)).await
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(requisition_create, requisition_list))
        .routes(routes!(requisition_get))
        .routes(routes!(requisition_submit))
        .routes(routes!(requisition_decide))
        .routes(routes!(requisition_close))
        .routes(routes!(candidate_create, candidate_list))
        .routes(routes!(candidate_get))
        .routes(routes!(candidate_shortlist))
        .routes(routes!(candidate_interview))
        .routes(routes!(candidate_offer))
        .routes(routes!(candidate_accept))
        .routes(routes!(candidate_decline))
        .routes(routes!(candidate_reject))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recruitment_machines() {
        use CandidateStatus as C;
        use RequisitionStatus as R;
        assert!(requisition_can_transition(R::Draft, R::PendingApproval));
        assert!(requisition_can_transition(R::Approved, R::Open));
        assert!(requisition_can_transition(R::Open, R::Filled));
        assert!(!requisition_can_transition(R::Draft, R::Open));
        assert!(!requisition_can_transition(R::Filled, R::Cancelled));
        assert!(candidate_can_transition(C::Applied, C::Shortlisted));
        assert!(candidate_can_transition(C::Offered, C::OfferAccepted));
        assert!(!candidate_can_transition(C::Applied, C::Offered));
        assert!(!candidate_can_transition(C::OfferAccepted, C::Rejected));
    }
}
