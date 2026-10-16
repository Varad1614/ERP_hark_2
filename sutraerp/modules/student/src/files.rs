//! **Part 2 · S4-docs** — the stored files of admission documents and of documents on the
//! student's record, kept by the platform document service
//! ([`sutra_contracts::platform::documents::DocumentVault`], S3-compatible object storage).
//!
//! Authorisation is decided here, on the owning record, before the vault is called; the vault
//! then re-checks tenant, institution and owner (a guessed id of another record is a 404) and
//! verifies the SHA-256 of every read.
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `PUT  /api/v1/student/admissions/{id}/documents/{documentId}/file` | `admission.manage` (document `submitted`, admission `initiated` / `documents_submitted`, no file yet) |
//! | `GET  /api/v1/student/admissions/{id}/documents/{documentId}/file` | `admission.manage` or `admission.decide` |
//! | `POST /api/v1/student/admissions/{id}/documents/{documentId}/file/link` | same; presigned link (when enabled) |
//! | `PUT  /api/v1/student/students/{id}/documents/{documentId}/file` | `student.manage` (document `submitted`, no file yet) |
//! | `GET  /api/v1/student/students/{id}/documents/{documentId}/file` | `student.manage` |
//! | `GET  /api/v1/student/me/documents/{documentId}/file` | `student.self` — own record only |
//! | `POST /api/v1/student/me/documents/{documentId}/file/link` | `student.self` — own record only |
//!
//! Uploads are the raw file as the request body (`Content-Type` = its media type, `?filename=`
//! the original name); they need no `Idempotency-Key`: a document takes exactly one file, so a
//! retried upload answers 409 and the first file stands. Verified admission documents move to
//! the student's record with their file, so the student can retrieve what was verified.

use crate::shared::guard::{require_institution_writable, require_student_writable};
use crate::shared::http::actor;
use crate::shared::publish;
use crate::shared::scope::{institution_scope, tenant, tenant_scope};
use crate::shared::student::{load_student, me};
use crate::shared::StudentState;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::platform::documents::{
    DocumentActor, DocumentContent, DocumentLink, DocumentOwner, NewDocument, StoredDocument,
    OWNER_ADMISSION_DOCUMENT, OWNER_STUDENT_DOCUMENT,
};
use sutra_contracts::student::ids::{
    AdmissionDocumentId, AdmissionId, StudentDocumentId, StudentId,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use uuid::Uuid;

/// Routes of the stored-file endpoints (mounted by `crate::router_with_state`).
pub fn router(state: StudentState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(upload_admission_file, download_admission_file))
        .routes(routes!(admission_file_link))
        .routes(routes!(upload_student_file, download_student_file))
        .routes(routes!(my_file))
        .routes(routes!(my_file_link))
        .with_state(state)
}

/// `?filename=` of an upload.
#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct UploadQuery {
    /// Original file name (sanitised; its extension must match the content: `.pdf`, `.png`,
    /// `.jpg` / `.jpeg`).
    pub filename: String,
}

/// Result of an upload: the document's new version and the stored file.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttachedFile {
    pub document_id: Uuid,
    /// New version (ETag) of the admission / record document.
    pub document_version: i64,
    pub file: StoredDocument,
}

fn doc_actor(ctx: &AuthContext) -> DocumentActor {
    let a = actor(ctx);
    DocumentActor {
        id: a.id,
        correlation_id: a.correlation_id,
    }
}

fn storage_off() -> KernelError {
    KernelError::Unavailable("document storage is not configured".into())
}

fn no_file() -> KernelError {
    KernelError::NotFound {
        entity: "document file",
    }
}

fn too_large(max: u64) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(
        "file",
        "too_large",
        format!("the file is larger than {max} bytes (or the upload broke off)"),
    )])
}

/// Reads the raw upload, refusing more than `max` bytes.
async fn read_upload(body: Body, max: u64) -> Result<Vec<u8>, KernelError> {
    let limit = usize::try_from(max).unwrap_or(usize::MAX).saturating_add(1);
    let bytes = axum::body::to_bytes(body, limit)
        .await
        .map_err(|_| too_large(max))?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err(too_large(max));
    }
    Ok(bytes.to_vec())
}

fn declared_type(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// The file as an attachment that browsers never render inline.
fn file_response(c: DocumentContent) -> Response {
    let d = c.document;
    let mut res = (StatusCode::OK, c.bytes).into_response();
    let h = res.headers_mut();
    let pairs = [
        (header::CONTENT_TYPE, d.content_type.clone()),
        (
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{}\"", d.filename.replace('"', "_")),
        ),
        (header::CACHE_CONTROL, "no-store".to_owned()),
        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
        (header::ETAG, format!("\"{}\"", d.sha256)),
        (
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'".to_owned(),
        ),
    ];
    for (name, value) in pairs {
        if let Ok(v) = HeaderValue::from_str(&value) {
            h.insert(name, v);
        }
    }
    if let Ok(v) = HeaderValue::from_str(&d.sha256) {
        h.insert("x-document-sha256", v);
    }
    res
}

// ---------------------------------------------------------------------------------------------
// Admission documents
// ---------------------------------------------------------------------------------------------

struct AdmissionDocRow {
    id: Uuid,
    institution_id: InstitutionId,
    admission_status: String,
    status: String,
    document_object_id: Option<Uuid>,
}

fn admission_doc_row(r: &PgRow) -> Result<AdmissionDocRow, sqlx::Error> {
    Ok(AdmissionDocRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        admission_status: r.try_get("admission_status")?,
        status: r.try_get("status")?,
        document_object_id: r.try_get("document_object_id")?,
    })
}

async fn admission_doc(
    conn: &mut PgConnection,
    tenant: TenantId,
    admission: AdmissionId,
    document: AdmissionDocumentId,
    lock: bool,
) -> Result<AdmissionDocRow, KernelError> {
    let sql = format!(
        "SELECT d.id, a.institution_id, a.status AS admission_status, d.status, \
         d.document_object_id FROM student.admission_document d \
         JOIN student.admission a ON a.id = d.admission_id AND a.tenant_id = d.tenant_id \
         WHERE d.id = $1 AND d.admission_id = $2 AND d.tenant_id = $3{}",
        if lock { " FOR UPDATE OF d" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(document.as_uuid())
        .bind(admission.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "admission document",
        })?;
    admission_doc_row(&r).map_err(map_db_error)
}

/// Staff who handle or decide admissions.
fn admission_reader(auth: &Authenticated) -> ApiResult<&AuthContext> {
    auth.require(Permission::AdmissionManage)
        .or_else(|_| auth.require(Permission::AdmissionDecide))
}

/// Attach the file of an admission document (`admission.manage`): the raw file is the body.
/// Type from the content (PDF / PNG / JPEG), size ≤ `SUTRA_DOCS_MAX_BYTES`, SHA-256 recorded.
#[utoipa::path(
    put,
    path = "/api/v1/student/admissions/{id}/documents/{documentId}/file",
    operation_id = "admission_document_file_upload",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("documentId" = String, Path, format = Uuid), UploadQuery),
    request_body(content = String, content_type = "application/octet-stream", description = "The file (PDF, PNG or JPEG); `Content-Type` its media type"),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = AttachedFile),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Not awaiting verification, or a file is already attached", body = Problem, content_type = "application/problem+json"),
        (status = 413, description = "Body above the upload limit", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Empty, too large, type not allowed or name / type mismatch", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "Document storage is not configured or unavailable", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn upload_admission_file(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, document_id)): Path<(AdmissionId, AdmissionDocumentId)>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    if !st.documents.enabled() {
        return Err(storage_off().into());
    }
    let bytes = read_upload(body, st.documents.max_bytes()).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = admission_doc(&mut tx, tenant(ctx), id, document_id, true).await?;
    if !matches!(
        row.admission_status.as_str(),
        "initiated" | "documents_submitted"
    ) {
        return Err(KernelError::Conflict(format!(
            "admission is {}; files can no longer be attached",
            row.admission_status
        ))
        .into());
    }
    if row.status != "submitted" {
        return Err(KernelError::Conflict(format!(
            "document is {}; a file is attached while the document awaits verification",
            row.status
        ))
        .into());
    }
    if row.document_object_id.is_some() {
        return Err(KernelError::Conflict(
            "a file is already attached; hand in a new document to replace it".into(),
        )
        .into());
    }
    let scope = institution_scope(ctx, row.institution_id);
    require_institution_writable(&st, &scope, row.institution_id).await?;
    let stored = st
        .documents
        .store(
            &scope,
            NewDocument {
                owner: DocumentOwner::new(OWNER_ADMISSION_DOCUMENT, row.id),
                filename: q.filename,
                declared_content_type: declared_type(&headers),
                bytes,
                actor: doc_actor(ctx),
            },
        )
        .await?;
    let version: i64 = sqlx::query_scalar(
        "UPDATE student.admission_document SET document_object_id = $2, version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(row.id)
    .bind(stored.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.admission.attach_document_file",
        "admission",
        id.to_string(),
        serde_json::json!({
            "documentId": row.id,
            "documentObjectId": stored.id,
            "sha256": stored.sha256,
            "sizeBytes": stored.size_bytes,
        }),
        &actor(ctx),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(AttachedFile {
            document_id: row.id,
            document_version: version,
            file: stored,
        }),
    )
        .into_response())
}

async fn admission_file(
    st: &StudentState,
    ctx: &AuthContext,
    id: AdmissionId,
    document_id: AdmissionDocumentId,
) -> Result<(ExecutionScope, Uuid, DocumentOwner), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = admission_doc(&mut conn, tenant(ctx), id, document_id, false).await?;
    let object = row.document_object_id.ok_or_else(no_file)?;
    Ok((
        institution_scope(ctx, row.institution_id),
        object,
        DocumentOwner::new(OWNER_ADMISSION_DOCUMENT, row.id),
    ))
}

/// Download the file of an admission document (`admission.manage` or `admission.decide`). The
/// SHA-256 is verified before a byte is sent (a mismatch quarantines the file: 409).
#[utoipa::path(
    get,
    path = "/api/v1/student/admissions/{id}/documents/{documentId}/file",
    operation_id = "admission_document_file_download",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("documentId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The file (attachment; `X-Document-SHA256`)", body = String, content_type = "application/octet-stream"),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Integrity check failed (quarantined)", body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn download_admission_file(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, document_id)): Path<(AdmissionId, AdmissionDocumentId)>,
) -> ApiResult<Response> {
    let ctx = admission_reader(&auth)?;
    let (scope, object, owner) = admission_file(&st, ctx, id, document_id).await?;
    let c = st
        .documents
        .fetch(&scope, object, &[owner], &doc_actor(ctx))
        .await?;
    Ok(file_response(c))
}

/// Short-lived presigned download link of an admission document's file (same permissions as
/// the download; 403 when `SUTRA_DOCS_PRESIGN_TTL_SECS` is 0). Audited.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/documents/{documentId}/file/link",
    operation_id = "admission_document_file_link",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("documentId" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = DocumentLink),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Not permitted, or presigned links are disabled", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn admission_file_link(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, document_id)): Path<(AdmissionId, AdmissionDocumentId)>,
) -> ApiResult<Json<DocumentLink>> {
    let ctx = admission_reader(&auth)?;
    let (scope, object, owner) = admission_file(&st, ctx, id, document_id).await?;
    Ok(Json(
        st.documents
            .link(&scope, object, &[owner], &doc_actor(ctx))
            .await?,
    ))
}

// ---------------------------------------------------------------------------------------------
// Documents on the student's record
// ---------------------------------------------------------------------------------------------

struct StudentDocRow {
    id: Uuid,
    institution_id: Option<InstitutionId>,
    status: String,
    document_object_id: Option<Uuid>,
    admission_document_id: Option<Uuid>,
}

impl StudentDocRow {
    /// Owners the stored file may belong to: this record document, or the admission document it
    /// was moved from.
    fn owners(&self) -> Vec<DocumentOwner> {
        let mut v = vec![DocumentOwner::new(OWNER_STUDENT_DOCUMENT, self.id)];
        if let Some(a) = self.admission_document_id {
            v.push(DocumentOwner::new(OWNER_ADMISSION_DOCUMENT, a));
        }
        v
    }

    fn scope(&self, ctx: &AuthContext) -> ExecutionScope {
        match self.institution_id {
            Some(i) => institution_scope(ctx, i),
            None => tenant_scope(ctx),
        }
    }
}

fn student_doc_row(r: &PgRow) -> Result<StudentDocRow, sqlx::Error> {
    Ok(StudentDocRow {
        id: r.try_get("id")?,
        institution_id: r
            .try_get::<Option<Uuid>, _>("institution_id")?
            .map(InstitutionId),
        status: r.try_get("status")?,
        document_object_id: r.try_get("document_object_id")?,
        admission_document_id: r.try_get("admission_document_id")?,
    })
}

async fn student_doc(
    conn: &mut PgConnection,
    tenant: TenantId,
    student: StudentId,
    document: StudentDocumentId,
    lock: bool,
) -> Result<StudentDocRow, KernelError> {
    let sql = format!(
        "SELECT id, institution_id, status, document_object_id, admission_document_id \
         FROM student.student_document WHERE id = $1 AND student_id = $2 AND tenant_id = $3{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(document.as_uuid())
        .bind(student.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "student document",
        })?;
    student_doc_row(&r).map_err(map_db_error)
}

/// Attach the file of a document on the student's record (`student.manage`; the document must
/// be `submitted` and have no file yet).
#[utoipa::path(
    put,
    path = "/api/v1/student/students/{id}/documents/{documentId}/file",
    operation_id = "student_document_file_upload",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("documentId" = String, Path, format = Uuid), UploadQuery),
    request_body(content = String, content_type = "application/octet-stream", description = "The file (PDF, PNG or JPEG); `Content-Type` its media type"),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = AttachedFile),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 413, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn upload_student_file(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, document_id)): Path<(StudentId, StudentDocumentId)>,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    if !st.documents.enabled() {
        return Err(storage_off().into());
    }
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    require_student_writable(&s)?;
    let bytes = read_upload(body, st.documents.max_bytes()).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = student_doc(&mut tx, tenant(ctx), id, document_id, true).await?;
    if row.status != "submitted" {
        return Err(KernelError::Conflict(format!(
            "document is {}; a file is attached while the document awaits verification",
            row.status
        ))
        .into());
    }
    if row.document_object_id.is_some() {
        return Err(KernelError::Conflict(
            "a file is already attached; file a new document to replace it".into(),
        )
        .into());
    }
    let scope = row.scope(ctx);
    if let Some(inst) = row.institution_id {
        require_institution_writable(&st, &scope, inst).await?;
    }
    let stored = st
        .documents
        .store(
            &scope,
            NewDocument {
                owner: DocumentOwner::new(OWNER_STUDENT_DOCUMENT, row.id),
                filename: q.filename,
                declared_content_type: declared_type(&headers),
                bytes,
                actor: doc_actor(ctx),
            },
        )
        .await?;
    let version: i64 = sqlx::query_scalar(
        "UPDATE student.student_document SET document_object_id = $2, version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(row.id)
    .bind(stored.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student_document.attach_file",
        "student_document",
        row.id.to_string(),
        serde_json::json!({
            "studentId": id,
            "documentObjectId": stored.id,
            "sha256": stored.sha256,
            "sizeBytes": stored.size_bytes,
        }),
        &actor(ctx),
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(AttachedFile {
            document_id: row.id,
            document_version: version,
            file: stored,
        }),
    )
        .into_response())
}

async fn student_file(
    st: &StudentState,
    ctx: &AuthContext,
    student: StudentId,
    document_id: StudentDocumentId,
) -> Result<(ExecutionScope, Uuid, Vec<DocumentOwner>), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let row = student_doc(&mut conn, tenant(ctx), student, document_id, false).await?;
    let object = row.document_object_id.ok_or_else(no_file)?;
    Ok((row.scope(ctx), object, row.owners()))
}

async fn serve_student_file(
    st: &StudentState,
    ctx: &AuthContext,
    student: StudentId,
    document_id: StudentDocumentId,
) -> ApiResult<Response> {
    let (scope, object, owners) = student_file(st, ctx, student, document_id).await?;
    let c = st
        .documents
        .fetch(&scope, object, &owners, &doc_actor(ctx))
        .await?;
    Ok(file_response(c))
}

/// Download the file of a document on a student's record (`student.manage`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/documents/{documentId}/file",
    operation_id = "student_document_file_download",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("documentId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The file (attachment; `X-Document-SHA256`)", body = String, content_type = "application/octet-stream"),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn download_student_file(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, document_id)): Path<(StudentId, StudentDocumentId)>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    serve_student_file(&st, ctx, s.id, document_id).await
}

/// Download the file of a document on the caller's own record (`student.self`; another
/// student's document is a 404).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/documents/{documentId}/file",
    operation_id = "student_self_document_file",
    tag = "students",
    params(("documentId" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, description = "The file (attachment; `X-Document-SHA256`)", body = String, content_type = "application/octet-stream"),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn my_file(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(document_id): Path<StudentDocumentId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let s = me(&st.pool, ctx).await?;
    serve_student_file(&st, ctx, s.id, document_id).await
}

/// Presigned download link of a document on the caller's own record (`student.self`; 403 when
/// presigned links are disabled). Audited.
#[utoipa::path(
    post,
    path = "/api/v1/student/me/documents/{documentId}/file/link",
    operation_id = "student_self_document_file_link",
    tag = "students",
    params(("documentId" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = DocumentLink),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn my_file_link(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(document_id): Path<StudentDocumentId>,
) -> ApiResult<Json<DocumentLink>> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let s = me(&st.pool, ctx).await?;
    let (scope, object, owners) = student_file(&st, ctx, s.id, document_id).await?;
    Ok(Json(
        st.documents
            .link(&scope, object, &owners, &doc_actor(ctx))
            .await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn uploads_are_capped() {
        assert_eq!(
            read_upload(Body::from(vec![1u8; 10]), 10)
                .await
                .ok()
                .map(|b| b.len()),
            Some(10)
        );
        assert!(matches!(
            read_upload(Body::from(vec![1u8; 11]), 10).await,
            Err(KernelError::Validation(_))
        ));
    }

    #[test]
    fn record_documents_accept_files_moved_from_the_admission() {
        let (d, a) = (Uuid::now_v7(), Uuid::now_v7());
        let row = StudentDocRow {
            id: d,
            institution_id: None,
            status: "verified".into(),
            document_object_id: Some(Uuid::now_v7()),
            admission_document_id: Some(a),
        };
        assert_eq!(
            row.owners(),
            vec![
                DocumentOwner::new(OWNER_STUDENT_DOCUMENT, d),
                DocumentOwner::new(OWNER_ADMISSION_DOCUMENT, a)
            ]
        );
        assert_eq!(row.status, "verified");
        assert!(row.document_object_id.is_some());
    }
}
