//! Student documents held on the record and document / certificate requests (L3-C-018).
//! Document generation, rendering and storage stay with Document Services: a request ends at
//! the hand-off (`issued` with the issuing reference).

use super::common::{home_institution, pick_context};
use super::support::{
    check_version, expect_version, finish, invalid, norm, status_changed, validate_key,
    validate_opt_text, validate_text, CommandBody, CommandErrors, CreateErrors, ReadErrors,
};
use crate::shared::guard::{require_institution_writable, require_student_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::student::{load_student, me, StudentRow};
use crate::shared::time::business_today;
use crate::shared::{publish, Actor, StudentState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::finance::StudentFinanceQuery;
use sutra_contracts::student::events::record::{
    DOCUMENT_REQUEST_ISSUED, DOCUMENT_REQUEST_ISSUED_SCHEMA, DOCUMENT_REQUEST_REJECTED,
    DOCUMENT_REQUEST_REJECTED_SCHEMA, DOCUMENT_REQUEST_SUBMITTED,
    DOCUMENT_REQUEST_SUBMITTED_SCHEMA,
};
use sutra_contracts::student::ids::{DocumentRequestId, StudentDocumentId, StudentId};
use sutra_contracts::student::status::{DocumentRequestStatus, DocumentStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

// ---------------------------------------------------------------------------------------------
// Student documents
// ---------------------------------------------------------------------------------------------

/// A document on the student's record.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StudentDocument {
    pub id: StudentDocumentId,
    pub student_id: StudentId,
    pub institution_id: Option<Uuid>,
    pub document_type: String,
    pub title: String,
    pub file_ref: Option<String>,
    pub status: DocumentStatus,
    /// `admission` | `staff` | `request` | `transfer`.
    pub source: String,
    pub note: Option<String>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
}

/// Document collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct StudentDocumentList {
    pub items: Vec<StudentDocument>,
}

const DOC_COLUMNS: &str = "id, student_id, institution_id, document_type, title, file_ref, \
    status, source, note, version, created_at";

fn doc_from_row(r: &PgRow) -> Result<StudentDocument, sqlx::Error> {
    let status: String = r.try_get("status")?;
    Ok(StudentDocument {
        id: StudentDocumentId(r.try_get("id")?),
        student_id: StudentId(r.try_get("student_id")?),
        institution_id: r.try_get("institution_id")?,
        document_type: r.try_get("document_type")?,
        title: r.try_get("title")?,
        file_ref: r.try_get("file_ref")?,
        status: DocumentStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("document status {status}").into()))?,
        source: r.try_get("source")?,
        note: r.try_get("note")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
    })
}

async fn docs_of(st: &StudentState, s: &StudentRow) -> Result<Vec<StudentDocument>, KernelError> {
    let sql = format!(
        "SELECT {DOC_COLUMNS} FROM student.student_document WHERE student_id = $1 \
         AND tenant_id = $2 ORDER BY created_at DESC, id DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(s.id.as_uuid())
        .bind(s.tenant_id.as_uuid())
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(doc_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Documents on a student's record (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/documents",
    operation_id = "student_document_list",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = StudentDocumentList), ReadErrors)
)]
pub async fn list_documents(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<StudentDocumentList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    Ok(Json(StudentDocumentList {
        items: docs_of(&st, &s).await?,
    }))
}

/// Own documents (`student.self`).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/documents",
    operation_id = "student_self_documents",
    tag = "students",
    security(("session_cookie" = [])),
    responses((status = 200, body = StudentDocumentList), ReadErrors)
)]
pub async fn my_documents(
    State(st): State<StudentState>,
    auth: Authenticated,
) -> ApiResult<Json<StudentDocumentList>> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let s = me(&st.pool, ctx).await?;
    Ok(Json(StudentDocumentList {
        items: docs_of(&st, &s).await?,
    }))
}

/// Body of `POST /students/{id}/documents`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddStudentDocument {
    pub document_type: String,
    pub title: String,
    pub file_ref: Option<String>,
    pub note: Option<String>,
}

/// File a document on the record (`student.manage`), status `submitted`.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/documents",
    operation_id = "student_document_add",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = AddStudentDocument,
    responses((status = 201, body = StudentDocument, headers(("ETag" = String))), CreateErrors)
)]
pub async fn add_document(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<AddStudentDocument>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    require_idempotency_key(&headers)?;
    let mut v = vec![];
    validate_key("documentType", b.document_type.trim(), &mut v);
    validate_text("title", &b.title, 200, &mut v);
    validate_opt_text("fileRef", b.file_ref.as_deref(), 500, &mut v);
    validate_opt_text("note", b.note.as_deref(), 1000, &mut v);
    finish(v)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    require_student_writable(&s)?;
    let inst = home_institution(&st.pool, &s).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.student_document (tenant_id, student_id, institution_id, \
         document_type, title, file_ref, note, source) VALUES ($1,$2,$3,$4,$5,$6,$7,'staff') \
         RETURNING {DOC_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .bind(inst.as_uuid())
        .bind(b.document_type.trim())
        .bind(b.title.trim())
        .bind(norm(b.file_ref.as_deref()))
        .bind(norm(b.note.as_deref()))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let d = doc_from_row(&row).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student_document.add",
        "student_document",
        d.id.to_string(),
        serde_json::json!({ "studentId": id, "documentType": d.document_type }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &d, d.version))
}

/// Body of `…/documents/{documentId}/verify`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VerifyStudentDocument {
    /// `verified` | `rejected` | `waived` | `superseded`.
    pub outcome: DocumentStatus,
    pub note: Option<String>,
}

/// Decide a `submitted` document (`student.manage`, `If-Match` = document version).
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/documents/{documentId}/verify",
    operation_id = "student_document_verify",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("documentId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Document version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = VerifyStudentDocument,
    responses((status = 200, body = StudentDocument, headers(("ETag" = String))), CommandErrors)
)]
pub async fn verify_document(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, document_id)): Path<(StudentId, StudentDocumentId)>,
    headers: HeaderMap,
    Json(b): Json<VerifyStudentDocument>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    if b.outcome == DocumentStatus::Submitted {
        return Err(invalid("outcome", "invalid", "outcome must be a decision").into());
    }
    let note = norm(b.note.as_deref());
    if b.outcome != DocumentStatus::Verified && note.is_none() {
        return Err(invalid("note", "required", "a note is required unless verified").into());
    }
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    require_student_writable(&s)?;
    let inst = home_institution(&st.pool, &s).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {DOC_COLUMNS} FROM student.student_document WHERE id = $1 AND student_id = $2 \
         AND tenant_id = $3 FOR UPDATE"
    );
    let cur = sqlx::query(&sql)
        .bind(document_id.as_uuid())
        .bind(id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "student document",
        })?;
    let cur = doc_from_row(&cur).map_err(map_db_error)?;
    check_version(expected, cur.version.0)?;
    if cur.status != DocumentStatus::Submitted
        && !(b.outcome == DocumentStatus::Superseded && cur.status == DocumentStatus::Verified)
    {
        return Err(KernelError::Conflict(format!(
            "document is {}; it was already decided",
            cur.status
        ))
        .into());
    }
    let sql = format!(
        "UPDATE student.student_document SET status = $2, note = COALESCE($3, note), \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {DOC_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(document_id.as_uuid())
        .bind(b.outcome.as_str())
        .bind(&note)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let d = doc_from_row(&row).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student_document.verify",
        "student_document",
        d.id.to_string(),
        serde_json::json!({ "outcome": b.outcome, "note": note }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &d, d.version))
}

// ---------------------------------------------------------------------------------------------
// Document / certificate requests
// ---------------------------------------------------------------------------------------------

/// Supported request types.
pub const REQUEST_TYPES: &[&str] = &[
    "bonafide_certificate",
    "transcript_copy",
    "migration_certificate",
    "character_certificate",
    "conduct_certificate",
    "enrolment_letter",
    "id_card_reissue",
];

/// Types that need a **live** context (proof of current studentship).
const NEEDS_LIVE: &[&str] = &[
    "bonafide_certificate",
    "enrolment_letter",
    "id_card_reissue",
];

/// Types that need Finance evidence permitting clearance (no outstanding dues).
const NEEDS_FINANCE: &[&str] = &["migration_certificate", "transcript_copy"];

/// Legal request transitions.
pub fn request_can_transition(from: DocumentRequestStatus, to: DocumentRequestStatus) -> bool {
    use DocumentRequestStatus::*;
    matches!(
        (from, to),
        (Requested, InProgress) | (Requested | InProgress, Issued | Rejected | Cancelled)
    )
}

/// A document / certificate request.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DocumentRequest {
    pub id: DocumentRequestId,
    pub student_id: StudentId,
    pub student_context_id: Option<Uuid>,
    pub institution_id: InstitutionId,
    pub document_type: String,
    pub purpose: Option<String>,
    pub copies: i32,
    /// `self_service` | `staff`.
    pub channel: String,
    pub status: DocumentRequestStatus,
    pub status_reason: Option<String>,
    pub issued_reference: Option<String>,
    pub issued_on: Option<NaiveDate>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Request collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct DocumentRequestList {
    pub items: Vec<DocumentRequest>,
}

const REQ_COLUMNS: &str = "id, student_id, student_context_id, institution_id, document_type, \
    purpose, copies, channel, status, status_reason, issued_reference, issued_on, version, \
    created_at, updated_at";

fn req_from_row(r: &PgRow) -> Result<DocumentRequest, sqlx::Error> {
    let status: String = r.try_get("status")?;
    Ok(DocumentRequest {
        id: DocumentRequestId(r.try_get("id")?),
        student_id: StudentId(r.try_get("student_id")?),
        student_context_id: r.try_get("student_context_id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        document_type: r.try_get("document_type")?,
        purpose: r.try_get("purpose")?,
        copies: r.try_get("copies")?,
        channel: r.try_get("channel")?,
        status: DocumentRequestStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("request status {status}").into()))?,
        status_reason: r.try_get("status_reason")?,
        issued_reference: r.try_get("issued_reference")?,
        issued_on: r.try_get("issued_on")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

/// Body of a new request.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDocumentRequest {
    /// One of `bonafide_certificate`, `transcript_copy`, `migration_certificate`,
    /// `character_certificate`, `conduct_certificate`, `enrolment_letter`, `id_card_reissue`.
    pub document_type: String,
    pub purpose: Option<String>,
    /// 1–10, default 1.
    pub copies: Option<i32>,
}

/// Request filters.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct RequestListQuery {
    pub status: Option<DocumentRequestStatus>,
}

async fn list_requests(
    st: &StudentState,
    tenant_id: Uuid,
    student: Option<Uuid>,
    institution: Option<Uuid>,
    status: Option<DocumentRequestStatus>,
) -> Result<Vec<DocumentRequest>, KernelError> {
    let sql = format!(
        "SELECT {REQ_COLUMNS} FROM student.document_request WHERE tenant_id = $1 \
         AND ($2::uuid IS NULL OR student_id = $2) AND ($3::uuid IS NULL OR institution_id = $3) \
         AND ($4::text IS NULL OR status = $4) ORDER BY created_at DESC, id DESC LIMIT 500"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(student)
        .bind(institution)
        .bind(status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(req_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Raises a request (L4-C-018-BH-001/002): eligibility, supported type, Finance prerequisite,
/// no duplicate active request of the same type. Emits `document_request.submitted`.
async fn raise(
    st: &StudentState,
    ctx: &AuthContext,
    s: StudentRow,
    b: NewDocumentRequest,
    channel: &str,
) -> ApiResult<Response> {
    let doc_type = b.document_type.trim().to_owned();
    if !REQUEST_TYPES.contains(&doc_type.as_str()) {
        return Err(invalid("documentType", "unsupported", "unsupported document type").into());
    }
    let copies = b.copies.unwrap_or(1);
    let mut v = vec![];
    validate_opt_text("purpose", b.purpose.as_deref(), 500, &mut v);
    if !(1..=10).contains(&copies) {
        v.push(sutra_kernel::error::FieldViolation::new(
            "copies",
            "range",
            "copies must be 1-10".to_owned(),
        ));
    }
    finish(v)?;
    require_student_writable(&s)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let context = if NEEDS_LIVE.contains(&doc_type.as_str()) {
        Some(pick_context(&mut conn, &s, None).await.map_err(|_| {
            KernelError::Conflict(
                "this certificate needs a current enrolment (live context)".into(),
            )
        })?)
    } else {
        let all = crate::shared::student::contexts_of(&mut *conn, s.tenant_id, s.id).await?;
        all.iter()
            .find(|c| c.status.is_live())
            .or_else(|| all.first())
            .cloned()
    };
    drop(conn);
    let Some(context) = context else {
        return Err(KernelError::Conflict("the student has no institution context".into()).into());
    };
    let inst = context.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    if NEEDS_FINANCE.contains(&doc_type.as_str()) {
        let ev = st
            .finance
            .student_dues(
                &scope,
                StudentFinanceQuery {
                    institution_id: inst,
                    student_id: s.id.as_uuid(),
                },
            )
            .await?;
        if !ev.permits_clearance() {
            return Err(KernelError::Conflict(
                "Finance evidence does not permit this request (outstanding dues)".into(),
            )
            .into());
        }
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.document_request (tenant_id, student_id, student_context_id, \
         institution_id, document_type, purpose, copies, channel, requested_by) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING {REQ_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(s.id.as_uuid())
        .bind(context.id.as_uuid())
        .bind(inst.as_uuid())
        .bind(&doc_type)
        .bind(norm(b.purpose.as_deref()))
        .bind(copies)
        .bind(channel)
        .bind(a.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("document_request_one_active") => {
                KernelError::Conflict(
                    "an active request of this document type already exists".into(),
                )
            }
            _ => map_db_error(e),
        })?;
    let r = req_from_row(&row).map_err(map_db_error)?;
    req_event(
        &mut tx,
        &scope,
        &r,
        None,
        None,
        &s,
        (
            DOCUMENT_REQUEST_SUBMITTED,
            DOCUMENT_REQUEST_SUBMITTED_SCHEMA,
        ),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.document_request.submit",
        "document_request",
        r.id.to_string(),
        serde_json::json!({ "documentType": r.document_type, "channel": channel }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &r, r.version))
}

#[allow(clippy::too_many_arguments)]
async fn req_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    r: &DocumentRequest,
    from: Option<DocumentRequestStatus>,
    reason: Option<&str>,
    s: &StudentRow,
    event: (&'static str, &'static str),
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::event(
        conn,
        scope,
        event.0,
        event.1,
        r.id.to_string(),
        &status_changed(
            r.id.as_uuid(),
            r.institution_id,
            Some(s.id.as_uuid()),
            from.map(|f| f.as_str()),
            r.status.as_str(),
            r.version.0,
            reason.map(str::to_owned),
            None,
            s.subject_ref.clone(),
        ),
        actor,
    )
    .await
}

/// A student's requests (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/document-requests",
    operation_id = "document_request_list_for_student",
    tag = "students",
    params(("id" = String, Path, format = Uuid), RequestListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = DocumentRequestList), ReadErrors)
)]
pub async fn list_for_student(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<RequestListQuery>,
) -> ApiResult<Json<DocumentRequestList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    load_student(&st.pool, tenant(ctx), id).await?;
    Ok(Json(DocumentRequestList {
        items: list_requests(
            &st,
            tenant(ctx).as_uuid(),
            Some(id.as_uuid()),
            None,
            q.status,
        )
        .await?,
    }))
}

/// The institution's request queue (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/document-requests",
    operation_id = "document_request_list",
    tag = "students",
    params(("id" = String, Path, format = Uuid), RequestListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = DocumentRequestList), ReadErrors)
)]
pub async fn list_for_institution(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<RequestListQuery>,
) -> ApiResult<Json<DocumentRequestList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(DocumentRequestList {
        items: list_requests(
            &st,
            tenant(ctx).as_uuid(),
            None,
            Some(id.as_uuid()),
            q.status,
        )
        .await?,
    }))
}

/// Raise a request on a student's behalf (`student.manage`, channel `staff`).
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/document-requests",
    operation_id = "document_request_create",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewDocumentRequest,
    responses((status = 201, body = DocumentRequest, headers(("ETag" = String))), CreateErrors)
)]
pub async fn create_for_student(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<NewDocumentRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    require_idempotency_key(&headers)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    raise(&st, ctx, s, b, "staff").await
}

/// Own requests (`student.self`).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/document-requests",
    operation_id = "student_self_document_requests",
    tag = "students",
    params(RequestListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = DocumentRequestList), ReadErrors)
)]
pub async fn my_requests(
    State(st): State<StudentState>,
    auth: Authenticated,
    Query(q): Query<RequestListQuery>,
) -> ApiResult<Json<DocumentRequestList>> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let s = me(&st.pool, ctx).await?;
    Ok(Json(DocumentRequestList {
        items: list_requests(
            &st,
            tenant(ctx).as_uuid(),
            Some(s.id.as_uuid()),
            None,
            q.status,
        )
        .await?,
    }))
}

/// Raise an own request (`student.self`, channel `self_service`).
#[utoipa::path(
    post,
    path = "/api/v1/student/me/document-requests",
    operation_id = "student_self_request_document",
    tag = "students",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewDocumentRequest,
    responses((status = 201, body = DocumentRequest, headers(("ETag" = String))), CreateErrors)
)]
pub async fn request_mine(
    State(st): State<StudentState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<NewDocumentRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    require_idempotency_key(&headers)?;
    let s = me(&st.pool, ctx).await?;
    raise(&st, ctx, s, b, "self_service").await
}

/// Body of `…/issue`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = DocumentRequestIssue)]
pub struct IssueBody {
    /// Document Services reference of the generated document.
    pub issued_reference: String,
}

/// Request transition shared by the staff and self commands.
#[allow(clippy::too_many_arguments)]
async fn move_request(
    st: &StudentState,
    ctx: &AuthContext,
    id: DocumentRequestId,
    expected: Version,
    to: DocumentRequestStatus,
    reason: Option<String>,
    issued_reference: Option<String>,
    own_student: Option<StudentId>,
) -> ApiResult<Response> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {REQ_COLUMNS} FROM student.document_request WHERE id = $1 AND tenant_id = $2"
    );
    let cur = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant(ctx).as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "document request",
        })?;
    let cur = req_from_row(&cur).map_err(map_db_error)?;
    if own_student.is_some_and(|s| s != cur.student_id) {
        return Err(KernelError::NotFound {
            entity: "document request",
        }
        .into());
    }
    let s = load_student(&mut *conn, tenant(ctx), cur.student_id).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(st, &scope, cur.institution_id).await?;
    let today = business_today(st, &scope, cur.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked: i64 =
        sqlx::query_scalar("SELECT version FROM student.document_request WHERE id = $1 FOR UPDATE")
            .bind(id.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
    check_version(expected, locked)?;
    if !request_can_transition(cur.status, to) {
        return Err(KernelError::Conflict(format!(
            "document request is {}; it cannot become {to}",
            cur.status
        ))
        .into());
    }
    let sql = format!(
        "UPDATE student.document_request SET status = $2, status_reason = COALESCE($3, status_reason), \
         issued_reference = COALESCE($4, issued_reference), \
         issued_on = CASE WHEN $2 = 'issued' THEN $5 ELSE issued_on END, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {REQ_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(to.as_str())
        .bind(&reason)
        .bind(&issued_reference)
        .bind(today)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let r = req_from_row(&row).map_err(map_db_error)?;
    let event = match to {
        DocumentRequestStatus::Issued => {
            Some((DOCUMENT_REQUEST_ISSUED, DOCUMENT_REQUEST_ISSUED_SCHEMA))
        }
        DocumentRequestStatus::Rejected => {
            Some((DOCUMENT_REQUEST_REJECTED, DOCUMENT_REQUEST_REJECTED_SCHEMA))
        }
        _ => None,
    };
    if let Some(e) = event {
        req_event(
            &mut tx,
            &scope,
            &r,
            Some(cur.status),
            reason.as_deref(),
            &s,
            e,
            &a,
        )
        .await?;
    }
    if to == DocumentRequestStatus::Issued {
        sqlx::query(
            "INSERT INTO student.student_document (tenant_id, student_id, institution_id, \
             document_type, title, file_ref, status, source, note) \
             VALUES ($1, $2, $3, $4, $5, $6, 'verified', 'request', $7)",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(s.id.as_uuid())
        .bind(r.institution_id.as_uuid())
        .bind(&r.document_type)
        .bind(r.document_type.replace('_', " "))
        .bind(&r.issued_reference)
        .bind(format!("issued for request {}", r.id))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    publish::audit(
        &mut tx,
        &scope,
        &format!("student.document_request.{}", to.as_str()),
        "document_request",
        r.id.to_string(),
        serde_json::json!({ "from": cur.status, "to": to, "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &r, r.version))
}

/// One request (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/document-requests/{id}",
    operation_id = "document_request_get",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = DocumentRequest, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get_request(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DocumentRequestId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let sql = format!(
        "SELECT {REQ_COLUMNS} FROM student.document_request WHERE id = $1 AND tenant_id = $2"
    );
    let r = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant(ctx).as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "document request",
        })?;
    let r = req_from_row(&r).map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &r, r.version))
}

/// Start processing a request (`student.manage`): `requested → in_progress`.
#[utoipa::path(
    post,
    path = "/api/v1/student/document-requests/{id}/start",
    operation_id = "document_request_start",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Request version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = DocumentRequest, headers(("ETag" = String))), CommandErrors)
)]
pub async fn start(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DocumentRequestId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    move_request(
        &st,
        ctx,
        id,
        expected,
        DocumentRequestStatus::InProgress,
        reason,
        None,
        None,
    )
    .await
}

/// Record the hand-off result (`student.manage`): `issued` with the Document Services reference
/// (`document_request.issued`); the issued document is filed on the record.
#[utoipa::path(
    post,
    path = "/api/v1/student/document-requests/{id}/issue",
    operation_id = "document_request_issue",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Request version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = IssueBody,
    responses((status = 200, body = DocumentRequest, headers(("ETag" = String))), CommandErrors)
)]
pub async fn issue(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DocumentRequestId>,
    headers: HeaderMap,
    Json(b): Json<IssueBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let mut v = vec![];
    validate_text("issuedReference", &b.issued_reference, 200, &mut v);
    finish(v)?;
    move_request(
        &st,
        ctx,
        id,
        expected,
        DocumentRequestStatus::Issued,
        None,
        Some(b.issued_reference.trim().to_owned()),
        None,
    )
    .await
}

/// Reject a request (`student.manage`; reason required, `document_request.rejected`).
#[utoipa::path(
    post,
    path = "/api/v1/student/document-requests/{id}/reject",
    operation_id = "document_request_reject",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Request version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = DocumentRequest, headers(("ETag" = String))), CommandErrors)
)]
pub async fn reject(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DocumentRequestId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().required_reason()?;
    move_request(
        &st,
        ctx,
        id,
        expected,
        DocumentRequestStatus::Rejected,
        Some(reason),
        None,
        None,
    )
    .await
}

/// Cancel a request (`student.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/document-requests/{id}/cancel",
    operation_id = "document_request_cancel",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Request version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = DocumentRequest, headers(("ETag" = String))), CommandErrors)
)]
pub async fn cancel(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DocumentRequestId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    move_request(
        &st,
        ctx,
        id,
        expected,
        DocumentRequestStatus::Cancelled,
        reason,
        None,
        None,
    )
    .await
}

/// Cancel an own request (`student.self`).
#[utoipa::path(
    post,
    path = "/api/v1/student/me/document-requests/{id}/cancel",
    operation_id = "student_self_cancel_document_request",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Request version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = DocumentRequest, headers(("ETag" = String))), CommandErrors)
)]
pub async fn cancel_mine(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DocumentRequestId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let s = me(&st.pool, ctx).await?;
    move_request(
        &st,
        ctx,
        id,
        expected,
        DocumentRequestStatus::Cancelled,
        reason,
        None,
        Some(s.id),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_machine() {
        use DocumentRequestStatus as R;
        assert!(request_can_transition(R::Requested, R::InProgress));
        assert!(request_can_transition(R::InProgress, R::Issued));
        assert!(request_can_transition(R::Requested, R::Rejected));
        assert!(!request_can_transition(R::Issued, R::Cancelled));
        assert!(!request_can_transition(R::InProgress, R::Requested));
        assert!(REQUEST_TYPES.iter().all(|t| !t.is_empty()));
    }
}
