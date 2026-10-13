//! Employee certificate request → authorised certificate (L3-E-039; `CERTIFICATE_APPROVAL`):
//! requested by HR or by the staff member (`/me/certificate-requests`), decided, then issued
//! with a `CERT-` number and a content snapshot of the service record.

use crate::shared::approval::{
    business_key, process, request_approval, ApprovalOutcome, AUTO_APPROVER,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_staff;
use crate::shared::numbering::{kind, next};
use crate::shared::rows::{current_engagement, decode, load_engagement, load_staff};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{
    self, check_version, clean, emit, expected, history, invalid, required_text, staff_or_404,
    status_payload, Decision, DecisionBody, ReasonBody,
};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::StaffSummary;
use sutra_contracts::people::status::{HrCaseStatus, StaffStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Certificate kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CertificateKind {
    /// Current service (needs a live engagement).
    Service,
    /// Experience (any engagement, typically after exit).
    Experience,
}

impl CertificateKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Experience => "experience",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Service, Self::Experience]
            .into_iter()
            .find(|k| k.as_str() == s)
    }
}

/// A certificate request.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub engagement_id: Option<Uuid>,
    pub kind: CertificateKind,
    pub purpose: String,
    /// `self` | `hr`.
    pub requested_via: String,
    #[schema(example = "CERT-000001")]
    pub certificate_number: Option<String>,
    /// What the issued certificate states.
    #[schema(value_type = Option<Object>)]
    pub content: Option<serde_json::Value>,
    pub issued_at: Option<DateTime<Utc>>,
    pub status: HrCaseStatus,
    pub status_reason: Option<String>,
    pub decided_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CertificateList {
    pub items: Vec<CertificateView>,
}

const SELECT: &str = "SELECT c.id, c.institution_id, c.staff_id, s.staff_number, s.display_name, \
    c.engagement_id, c.kind, c.purpose, c.requested_via, c.certificate_number, c.content, \
    c.issued_at, c.status, c.status_reason, c.decided_by, c.process_instance_id, c.version \
    FROM people.certificate_request c JOIN people.staff_member s ON s.id = c.staff_id";

fn from_row(r: &PgRow) -> Result<CertificateView, sqlx::Error> {
    let kind: String = r.try_get("kind")?;
    Ok(CertificateView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        staff_id: r.try_get("staff_id")?,
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        engagement_id: r.try_get("engagement_id")?,
        kind: CertificateKind::parse(&kind)
            .ok_or_else(|| sqlx::Error::Decode(format!("unknown kind `{kind}`").into()))?,
        purpose: r.try_get("purpose")?,
        requested_via: r.try_get("requested_via")?,
        certificate_number: r.try_get("certificate_number")?,
        content: r.try_get("content")?,
        issued_at: r.try_get("issued_at")?,
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
) -> Result<CertificateView, KernelError> {
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
        entity: "certificate request",
    })?;
    from_row(&row).map_err(map_db_error)
}

async fn list_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: Uuid,
) -> Result<Vec<CertificateView>, KernelError> {
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE c.staff_id = $1 AND c.tenant_id = $2 ORDER BY c.created_at DESC"
    ))
    .bind(staff)
    .bind(tenant.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

/// Certificate requests of a staff member, newest first.
pub async fn list_for_staff(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: Uuid,
) -> Result<Vec<CertificateView>, KernelError> {
    list_of(conn, tenant, staff).await
}

async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    c: &CertificateView,
    to: HrCaseStatus,
    reason: Option<&str>,
    decided_by: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE people.certificate_request SET status = $2, status_reason = COALESCE($3, status_reason), \
         decided_by = COALESCE($4, decided_by), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(c.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(decided_by)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(c.institution_id),
        "certificate_request",
        c.id,
        Some(c.status.as_str()),
        to.as_str(),
        "sutraerp.people.certificate_request.status",
        reason,
        c.process_instance_id.as_deref(),
        actor,
    )
    .await
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CertificateCreate {
    pub kind: CertificateKind,
    pub purpose: String,
}

/// Creates a request (both channels).
async fn request(
    st: &PeopleState,
    ctx: &AuthContext,
    staff: StaffSummary,
    b: CertificateCreate,
    via: &'static str,
) -> ApiResult<Response> {
    let t = ctx.scope.tenant_id;
    let scope = institution_scope(ctx, staff.institution_id);
    require_institution_writable(st, &scope, staff.institution_id).await?;
    let purpose = required_text("purpose", &b.purpose, 500)?;
    if staff.status == StaffStatus::Merged {
        return Err(KernelError::Conflict(format!(
            "staff record {} is merged",
            staff.staff_number
        ))
        .into());
    }
    let engagement = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        current_engagement(&mut conn, t, staff.id).await?
    };
    let engagement = match (b.kind, engagement) {
        (CertificateKind::Service, Some(e))
            if e.status.is_live() && e.status.as_str() != "pending" =>
        {
            e
        }
        (CertificateKind::Service, _) => {
            return Err(KernelError::Conflict(format!(
                "staff record {} has no current service; ask for an experience certificate",
                staff.staff_number
            ))
            .into())
        }
        (CertificateKind::Experience, Some(e))
            if e.status.as_str() != "pending" && e.status.as_str() != "cancelled" =>
        {
            e
        }
        (CertificateKind::Experience, _) => {
            return Err(KernelError::Conflict(format!(
                "staff record {} has no service to certify",
                staff.staff_number
            ))
            .into())
        }
    };
    let mut vars = BTreeMap::new();
    vars.insert(
        "staffNumber".to_owned(),
        serde_json::json!(staff.staff_number),
    );
    vars.insert("kind".to_owned(), serde_json::json!(b.kind.as_str()));
    let key = business_key(
        "certificate_request",
        format!("{}:{}", staff.id, Uuid::now_v7()),
        staff.version,
    );
    let outcome = request_approval(st, &scope, process::CERTIFICATE_APPROVAL, key, vars).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let pid = outcome.process_instance_id();
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.certificate_request (tenant_id, institution_id, staff_id, engagement_id, \
         kind, purpose, requested_via, status, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending_approval', $8) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(staff.institution_id.as_uuid())
    .bind(staff.id.as_uuid())
    .bind(engagement.id.as_uuid())
    .bind(b.kind.as_str())
    .bind(&purpose)
    .bind(via)
    .bind(pid)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        staff.institution_id,
        "certificate_request",
        cid,
        None,
        "pending_approval",
        "sutraerp.people.certificate_request.submitted",
        None,
        Some(pid),
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "certificate_request",
        cid,
        "people.certificate_request.submit",
        serde_json::json!({ "kind": b.kind, "via": via }),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let c = load(&mut tx, t, cid, true).await?;
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

/// Request a certificate on behalf of a staff member (`people.staff.manage`; Idempotency-Key;
/// `CERTIFICATE_APPROVAL`).
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/certificate-requests", operation_id = "people_certificate_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = CertificateCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CertificateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<CertificateCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let staff = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        staff_or_404(&mut conn, ctx.scope.tenant_id, id).await?
    };
    request(&st, ctx, staff, b, "hr").await
}

/// Certificate requests of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/certificate-requests", operation_id = "people_certificate_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CertificateList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<CertificateList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    staff_or_404(&mut conn, t, id).await?;
    Ok(Json(CertificateList {
        items: list_of(&mut conn, t, id).await?,
    }))
}

/// Request one of my own certificates (`people.self`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/people/me/certificate-requests", operation_id = "people_certificate_self_create", tag = "people-hr",
    params(("Idempotency-Key" = String, Header)),
    request_body = CertificateCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = CertificateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<CertificateCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    require_idempotency_key(&headers)?;
    let staff = my_staff(&st, ctx).await?;
    request(&st, ctx, staff, b, "self").await
}

/// My certificate requests (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/me/certificate-requests", operation_id = "people_certificate_self_list", tag = "people-hr",
    security(("session_cookie" = [])),
    responses((status = 200, body = CertificateList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<CertificateList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let staff = my_staff(&st, ctx).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(Json(CertificateList {
        items: list_of(&mut conn, ctx.scope.tenant_id, staff.id.as_uuid()).await?,
    }))
}

/// One certificate request (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/certificate-requests/{id}", operation_id = "people_certificate_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = CertificateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
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
) -> Result<sutra_kernel::scope::ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let c = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(c.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    Ok(scope)
}

/// Decide a pending certificate request (`people.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/people/certificate-requests/{id}/decision", operation_id = "people_certificate_decide", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CertificateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
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
    if c.status != HrCaseStatus::PendingApproval {
        return Err(common::illegal(
            "certificate request of",
            &c.staff_number,
            c.status.as_str(),
            "decided",
        )
        .into());
    }
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
        "certificate_request",
        id,
        "people.certificate_request.decide",
        serde_json::json!({ "decision": to, "reason": reason }),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Issue an approved certificate (`people.staff.manage`; If-Match): `CERT-` number + content
/// snapshot (name, designation, service period); `certificate_request.issued`.
#[utoipa::path(post, path = "/api/v1/people/certificate-requests/{id}/issue", operation_id = "people_certificate_issue", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the request")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = CertificateView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn issue(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = case_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load(&mut tx, t, id, true).await?;
    check_version(v, c.version)?;
    if c.status != HrCaseStatus::Approved {
        return Err(common::illegal(
            "certificate request of",
            &c.staff_number,
            c.status.as_str(),
            "completed",
        )
        .into());
    }
    let staff = load_staff(&mut tx, t, StaffMemberId(c.staff_id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let e = match c.engagement_id {
        Some(e) => load_engagement(&mut tx, t, StaffEngagementId(e)).await?,
        None => None,
    };
    let number = next(
        &mut tx,
        t,
        InstitutionId(c.institution_id),
        kind::CERTIFICATE,
    )
    .await?;
    let content = serde_json::json!({
        "kind": c.kind,
        "certificateNumber": number,
        "staffNumber": staff.staff_number,
        "displayName": staff.display_name,
        "designation": e.as_ref().map(|e| e.designation.clone()),
        "engagementNumber": e.as_ref().map(|e| e.engagement_number.clone()),
        "serviceFrom": e.as_ref().map(|e| e.start_date),
        "serviceTo": e.as_ref().and_then(|e| e.last_working_date),
        "purpose": c.purpose,
    });
    sqlx::query(
        "UPDATE people.certificate_request SET certificate_number = $2, content = $3, issued_at = now(), \
         status = 'completed', version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&number)
    .bind(&content)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        InstitutionId(c.institution_id),
        "certificate_request",
        id,
        Some(c.status.as_str()),
        "completed",
        ev::CERTIFICATE_ISSUED,
        Some(number.as_str()),
        c.process_instance_id.as_deref(),
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    emit(
        &mut tx,
        &scope,
        ev::CERTIFICATE_ISSUED,
        ev::CERTIFICATE_ISSUED_SCHEMA,
        "certificate_request",
        id,
        "people.certificate_request.issue",
        &status_payload(
            id,
            InstitutionId(c.institution_id),
            Some(&staff),
            e.as_ref(),
            Some(c.status.as_str()),
            out.status.as_str(),
            out.version,
            Some(number.as_str()),
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
        .routes(routes!(create, list))
        .routes(routes!(my_create, my_list))
        .routes(routes!(get))
        .routes(routes!(decide))
        .routes(routes!(issue))
        .with_state(state)
}
