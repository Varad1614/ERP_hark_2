//! Employee profile depth (L2-E07): qualifications (L3-E-060) and profile documents (L3-E-064)
//! with verification, the workforce → profile synchronisation record (L3-E-066), and the staff
//! member's own profile (`/me/profile`).

use crate::hr::certificate::CertificateView;
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::me::my_staff;
use crate::shared::rows::{current_engagement, decode, load_staff};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::PeopleState;
use crate::staff_directory::http::EngagementView;
use crate::workforce::common::{
    self, check_version, clean, emit, expected, history, invalid, optional_text, required_text,
    staff_or_404, status_payload,
};
use crate::workforce::faculty::FacultyProfileView;
use crate::workforce::staff::{record_view, StaffRecordView};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::StaffMemberId;
use sutra_contracts::people::status::{StaffStatus, VerificationStatus};
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

// ------------------------------------------------------------------ views

/// A qualification of a staff member.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualificationView {
    pub id: Uuid,
    pub staff_id: Uuid,
    /// `doctorate`, `postgraduate`, `graduate`, `diploma`, `certificate`, `other`.
    pub qualification_type: String,
    pub title: String,
    pub awarding_body: String,
    pub year_of_completion: Option<i32>,
    pub grade: Option<String>,
    pub status: VerificationStatus,
    pub status_reason: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub version: i64,
}

/// A profile document (reference only, never the content).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentView {
    pub id: Uuid,
    pub staff_id: Uuid,
    /// `identity`, `address`, `education`, `employment`, `medical`, `other`.
    pub document_type: String,
    pub title: String,
    pub reference: String,
    pub valid_until: Option<NaiveDate>,
    pub status: VerificationStatus,
    pub status_reason: Option<String>,
    pub verified_by: Option<String>,
    pub verified_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct QualificationList {
    pub items: Vec<QualificationView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DocumentList {
    pub items: Vec<DocumentView>,
}

/// Which engagement attributes flowed into the profile.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProfileSyncView {
    pub id: Uuid,
    pub engagement_id: Uuid,
    /// `modification`, `transfer`, `probation`.
    pub cause: String,
    pub fields: Vec<String>,
    #[schema(value_type = Object)]
    pub before_values: serde_json::Value,
    #[schema(value_type = Object)]
    pub after_values: serde_json::Value,
    pub actor: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProfileSyncList {
    pub items: Vec<ProfileSyncView>,
}

/// The caller's own profile (My HR).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyProfileView {
    /// `null` when the login has no staff record (the lists are then empty).
    pub staff: Option<StaffRecordView>,
    pub engagement: Option<EngagementView>,
    pub faculty: Option<FacultyProfileView>,
    pub qualifications: Vec<QualificationView>,
    pub documents: Vec<DocumentView>,
    pub certificate_requests: Vec<CertificateView>,
}

const Q_COLUMNS: &str =
    "id, staff_id, qualification_type, title, awarding_body, year_of_completion, \
    grade, status, status_reason, verified_by, verified_at, version";
const D_COLUMNS: &str = "id, staff_id, document_type, title, reference, valid_until, status, \
    status_reason, verified_by, verified_at, version";

fn q_from_row(r: &PgRow) -> Result<QualificationView, sqlx::Error> {
    Ok(QualificationView {
        id: r.try_get("id")?,
        staff_id: r.try_get("staff_id")?,
        qualification_type: r.try_get("qualification_type")?,
        title: r.try_get("title")?,
        awarding_body: r.try_get("awarding_body")?,
        year_of_completion: r.try_get("year_of_completion")?,
        grade: r.try_get("grade")?,
        status: decode(r.try_get("status")?, VerificationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        verified_by: r.try_get("verified_by")?,
        verified_at: r.try_get("verified_at")?,
        version: r.try_get("version")?,
    })
}

fn d_from_row(r: &PgRow) -> Result<DocumentView, sqlx::Error> {
    Ok(DocumentView {
        id: r.try_get("id")?,
        staff_id: r.try_get("staff_id")?,
        document_type: r.try_get("document_type")?,
        title: r.try_get("title")?,
        reference: r.try_get("reference")?,
        valid_until: r.try_get("valid_until")?,
        status: decode(r.try_get("status")?, VerificationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        verified_by: r.try_get("verified_by")?,
        verified_at: r.try_get("verified_at")?,
        version: r.try_get("version")?,
    })
}

async fn qualifications_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: Uuid,
) -> Result<Vec<QualificationView>, KernelError> {
    let rows = sqlx::query(&format!("SELECT {Q_COLUMNS} FROM people.staff_qualification WHERE staff_id = $1 AND tenant_id = $2 ORDER BY created_at"))
        .bind(staff)
        .bind(tenant.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(q_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

async fn documents_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: Uuid,
) -> Result<Vec<DocumentView>, KernelError> {
    let rows = sqlx::query(&format!("SELECT {D_COLUMNS} FROM people.staff_document WHERE staff_id = $1 AND tenant_id = $2 ORDER BY created_at"))
        .bind(staff)
        .bind(tenant.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(d_from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)
}

const QUALIFICATION_TYPES: &[&str] = &[
    "doctorate",
    "postgraduate",
    "graduate",
    "diploma",
    "certificate",
    "other",
];
const DOCUMENT_TYPES: &[&str] = &[
    "identity",
    "address",
    "education",
    "employment",
    "medical",
    "other",
];

// ------------------------------------------------------------------ bodies

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QualificationCreate {
    pub qualification_type: String,
    pub title: String,
    pub awarding_body: String,
    pub year_of_completion: Option<i32>,
    pub grade: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentCreate {
    pub document_type: String,
    pub title: String,
    pub reference: String,
    pub valid_until: Option<NaiveDate>,
}

/// Verification outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum VerifyOutcome {
    Verified,
    Rejected,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = ProfileVerifyBody)]
pub struct VerifyBody {
    pub outcome: VerifyOutcome,
    pub reason: Option<String>,
}

// ------------------------------------------------------------------ handlers

async fn writable_staff(
    st: &PeopleState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<sutra_contracts::people::ports::StaffSummary, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = staff_or_404(&mut conn, ctx.scope.tenant_id, id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(st, &scope, s.institution_id).await?;
    if !s.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "staff record {} is {}",
            s.staff_number, s.status
        )));
    }
    Ok(s)
}

/// Register a qualification (`people.staff.manage`; Idempotency-Key; starts `pending`).
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/qualifications", operation_id = "people_profile_qualification_create", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = QualificationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = QualificationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn qualification_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<QualificationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let s = writable_staff(&st, ctx, id).await?;
    let ty = b.qualification_type.trim().to_lowercase();
    if !QUALIFICATION_TYPES.contains(&ty.as_str()) {
        return Err(invalid(
            "qualificationType",
            "invalid",
            "doctorate, postgraduate, graduate, diploma, certificate or other",
        )
        .into());
    }
    let title = required_text("title", &b.title, 200)?;
    let body = required_text("awardingBody", &b.awarding_body, 200)?;
    if b.year_of_completion
        .is_some_and(|y| !(1940..=2200).contains(&y))
    {
        return Err(invalid("yearOfCompletion", "out_of_range", "1940 to 2200").into());
    }
    let grade = optional_text("grade", b.grade, 40)?;
    let t = ctx.scope.tenant_id;
    let scope = institution_scope(ctx, s.institution_id);
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "INSERT INTO people.staff_qualification (tenant_id, institution_id, staff_id, qualification_type, \
         title, awarding_body, year_of_completion, grade) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING {Q_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(id)
    .bind(&ty)
    .bind(&title)
    .bind(&body)
    .bind(b.year_of_completion)
    .bind(&grade)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let q = q_from_row(&row).map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        s.institution_id,
        "staff_qualification",
        q.id,
        None,
        "pending",
        "sutraerp.people.staff_qualification.registered",
        None,
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "staff_qualification",
        q.id,
        "people.staff_qualification.register",
        serde_json::json!({ "staffId": id, "type": ty }),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &q, Version(q.version)))
}

/// Qualifications of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/qualifications", operation_id = "people_profile_qualification_list", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = QualificationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn qualification_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<QualificationList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    staff_or_404(&mut conn, t, id).await?;
    Ok(Json(QualificationList {
        items: qualifications_of(&mut conn, t, id).await?,
    }))
}

/// Verify or reject a pending qualification (`people.staff.manage`; If-Match; L3-E-060).
#[utoipa::path(post, path = "/api/v1/people/staff-qualifications/{id}/verify", operation_id = "people_profile_qualification_verify", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the qualification")),
    request_body = VerifyBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = QualificationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn qualification_verify(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VerifyBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.outcome == VerifyOutcome::Rejected && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!("SELECT {Q_COLUMNS}, institution_id FROM people.staff_qualification WHERE id = $1 AND tenant_id = $2 FOR UPDATE"))
        .bind(id)
        .bind(t.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "qualification" })?;
    let q = q_from_row(&row).map_err(map_db_error)?;
    let institution = InstitutionId(row.try_get("institution_id").map_err(map_db_error)?);
    check_version(v, q.version)?;
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if q.status != VerificationStatus::Pending {
        return Err(
            common::illegal("qualification", &q.title, q.status.as_str(), "verified").into(),
        );
    }
    let to = if b.outcome == VerifyOutcome::Verified {
        VerificationStatus::Verified
    } else {
        VerificationStatus::Rejected
    };
    let row = sqlx::query(&format!(
        "UPDATE people.staff_qualification SET status = $2, status_reason = $3, verified_by = $4, verified_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {Q_COLUMNS}"
    ))
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = q_from_row(&row).map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "staff_qualification",
        id,
        Some(q.status.as_str()),
        to.as_str(),
        ev::QUALIFICATION_VERIFIED,
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    let staff = load_staff(&mut tx, t, StaffMemberId(q.staff_id)).await?;
    emit(
        &mut tx,
        &scope,
        ev::QUALIFICATION_VERIFIED,
        ev::QUALIFICATION_VERIFIED_SCHEMA,
        "staff_qualification",
        id,
        "people.staff_qualification.verify",
        &status_payload(
            id,
            institution,
            staff.as_ref(),
            None,
            Some(q.status.as_str()),
            to.as_str(),
            out.version,
            reason.as_deref(),
            None,
            None,
        ),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Register a profile document (`people.staff.manage`; Idempotency-Key; starts `pending`).
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/documents", operation_id = "people_profile_document_create", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = DocumentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = DocumentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn document_create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DocumentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let s = writable_staff(&st, ctx, id).await?;
    let ty = b.document_type.trim().to_lowercase();
    if !DOCUMENT_TYPES.contains(&ty.as_str()) {
        return Err(invalid(
            "documentType",
            "invalid",
            "identity, address, education, employment, medical or other",
        )
        .into());
    }
    let title = required_text("title", &b.title, 200)?;
    let reference = required_text("reference", &b.reference, 200)?;
    let t = ctx.scope.tenant_id;
    let scope = institution_scope(ctx, s.institution_id);
    let today = business_today(&st, &scope, s.institution_id).await?;
    if b.valid_until.is_some_and(|d| d < today) {
        return Err(invalid("validUntil", "expired", "the document has already expired").into());
    }
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "INSERT INTO people.staff_document (tenant_id, institution_id, staff_id, document_type, title, \
         reference, valid_until) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING {D_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(id)
    .bind(&ty)
    .bind(&title)
    .bind(&reference)
    .bind(b.valid_until)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let d = d_from_row(&row).map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        s.institution_id,
        "staff_document",
        d.id,
        None,
        "pending",
        "sutraerp.people.staff_document.registered",
        None,
        None,
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "staff_document",
        d.id,
        "people.staff_document.register",
        serde_json::json!({ "staffId": id, "type": ty }),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &d, Version(d.version)))
}

/// Profile documents of a staff member (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/documents", operation_id = "people_profile_document_list", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = DocumentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn document_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<DocumentList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    staff_or_404(&mut conn, t, id).await?;
    Ok(Json(DocumentList {
        items: documents_of(&mut conn, t, id).await?,
    }))
}

/// Verify or reject a pending document (`people.staff.manage`; If-Match; L3-E-064). A document
/// past its validity is marked `expired` instead (409).
#[utoipa::path(post, path = "/api/v1/people/staff-documents/{id}/verify", operation_id = "people_profile_document_verify", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the document")),
    request_body = VerifyBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DocumentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn document_verify(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<VerifyBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.outcome == VerifyOutcome::Rejected && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!("SELECT {D_COLUMNS}, institution_id FROM people.staff_document WHERE id = $1 AND tenant_id = $2 FOR UPDATE"))
        .bind(id)
        .bind(t.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "document" })?;
    let d = d_from_row(&row).map_err(map_db_error)?;
    let institution = InstitutionId(row.try_get("institution_id").map_err(map_db_error)?);
    check_version(v, d.version)?;
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if d.status != VerificationStatus::Pending {
        return Err(common::illegal("document", &d.title, d.status.as_str(), "verified").into());
    }
    let today = business_today(&st, &scope, institution).await?;
    let expired = d.valid_until.is_some_and(|u| u < today);
    let to = if expired {
        VerificationStatus::Expired
    } else if b.outcome == VerifyOutcome::Verified {
        VerificationStatus::Verified
    } else {
        VerificationStatus::Rejected
    };
    let row = sqlx::query(&format!(
        "UPDATE people.staff_document SET status = $2, status_reason = $3, verified_by = $4, verified_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {D_COLUMNS}"
    ))
    .bind(id)
    .bind(to.as_str())
    .bind(if expired { Some("validity passed before verification") } else { reason.as_deref() })
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = d_from_row(&row).map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "staff_document",
        id,
        Some(d.status.as_str()),
        to.as_str(),
        ev::DOCUMENT_VERIFIED,
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    let staff = load_staff(&mut tx, t, StaffMemberId(d.staff_id)).await?;
    emit(
        &mut tx,
        &scope,
        ev::DOCUMENT_VERIFIED,
        ev::DOCUMENT_VERIFIED_SCHEMA,
        "staff_document",
        id,
        "people.staff_document.verify",
        &status_payload(
            id,
            institution,
            staff.as_ref(),
            None,
            Some(d.status.as_str()),
            to.as_str(),
            out.version,
            reason.as_deref(),
            None,
            None,
        ),
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    if expired {
        return Err(KernelError::Conflict(format!(
            "document {} expired on {}; it was marked expired",
            d.title,
            d.valid_until.map(|u| u.to_string()).unwrap_or_default()
        ))
        .into());
    }
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Workforce → profile synchronisation records of a staff member (`people.read`; L3-E-066).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/profile-syncs", operation_id = "people_profile_sync_list", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ProfileSyncList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sync_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ProfileSyncList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    staff_or_404(&mut conn, t, id).await?;
    let rows = sqlx::query(
        "SELECT id, engagement_id, cause, fields, before_values, after_values, actor, created_at \
         FROM people.profile_sync WHERE staff_id = $1 AND tenant_id = $2 ORDER BY created_at DESC",
    )
    .bind(id)
    .bind(t.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        let get = || -> Result<ProfileSyncView, sqlx::Error> {
            Ok(ProfileSyncView {
                id: r.try_get("id")?,
                engagement_id: r.try_get("engagement_id")?,
                cause: r.try_get("cause")?,
                fields: r.try_get("fields")?,
                before_values: r.try_get("before_values")?,
                after_values: r.try_get("after_values")?,
                actor: r.try_get("actor")?,
                created_at: r.try_get("created_at")?,
            })
        };
        items.push(get().map_err(map_db_error)?);
    }
    Ok(Json(ProfileSyncList { items }))
}

/// My own profile: record, engagement, faculty profile, qualifications, documents and
/// certificate requests (`people.self`). A login without a staff record gets `staff: null` and
/// empty lists (200, so My HR renders without an error).
#[utoipa::path(get, path = "/api/v1/people/me/profile", operation_id = "people_profile_self_get", tag = "people-workforce",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyProfileView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_profile(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<MyProfileView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let s = match my_staff(&st, ctx).await {
        Ok(s) if s.status != StaffStatus::Merged => s,
        Ok(_) | Err(KernelError::NotFound { .. }) => {
            return Ok(Json(MyProfileView {
                staff: None,
                engagement: None,
                faculty: None,
                qualifications: Vec::new(),
                documents: Vec::new(),
                certificate_requests: Vec::new(),
            }))
        }
        Err(e) => return Err(e.into()),
    };
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let staff = record_view(&mut conn, t, &s).await?;
    let engagement = current_engagement(&mut conn, t, s.id)
        .await?
        .as_ref()
        .map(EngagementView::from);
    let faculty_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM people.faculty_profile WHERE staff_id = $1 AND tenant_id = $2",
    )
    .bind(s.id.as_uuid())
    .bind(t.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let faculty = match faculty_id {
        Some(f) => Some(crate::workforce::faculty::load_by_id(&mut conn, t, f).await?),
        None => None,
    };
    let qualifications = qualifications_of(&mut conn, t, s.id.as_uuid()).await?;
    let documents = documents_of(&mut conn, t, s.id.as_uuid()).await?;
    let certificate_requests =
        crate::hr::certificate::list_for_staff(&mut conn, t, s.id.as_uuid()).await?;
    Ok(Json(MyProfileView {
        staff: Some(staff),
        engagement,
        faculty,
        qualifications,
        documents,
        certificate_requests,
    }))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(qualification_create, qualification_list))
        .routes(routes!(qualification_verify))
        .routes(routes!(document_create, document_list))
        .routes(routes!(document_verify))
        .routes(routes!(sync_list))
        .routes(routes!(my_profile))
        .with_state(state)
}
