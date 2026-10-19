//! Admission Record lifecycle (L2-C01 §6.9.1, L3-C-001…007): acceptance, documents and
//! verification, approval decision through the WorkflowPort, financial clearance from Finance
//! evidence, confirmation, student creation, cancellation, withdrawal, revocation, expiry and
//! readmission.

use super::application::required_documents;
use super::domain::{
    admission_can_transition, display_name, readmission_source, verification_state, Admission,
    AdmissionDocument, AdmissionHistoryEntry, VerificationState,
};
use crate::record::creation;
use crate::record::refs;
use crate::record::support::{
    check_version, expect_version, finish, invalid, norm, status_changed, validate_key,
    validate_opt_text, validate_text, CommandBody, CommandErrors, CreateErrors, Decision,
    DecisionBody, ReadErrors,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_context_status, StatusChange};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::student::{live_context, lock_context};
use crate::shared::time::business_today;
use crate::shared::{publish, Actor, StudentState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::AdmissionFinanceQuery;
use sutra_contracts::platform::documents::{
    DocumentActor, DocumentOwner, OWNER_ADMISSION_DOCUMENT,
};
use sutra_contracts::student::events::admission::*;
use sutra_contracts::student::ids::{
    AdmissionApplicationId, AdmissionDocumentId, AdmissionId, StudentContextId, StudentId,
};
use sutra_contracts::student::status::{AdmissionStatus, DocumentStatus, StudentContextStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

pub(crate) const COLUMNS: &str = "id, institution_id, application_id, programme_id, \
    curriculum_version_id, campus_id, admission_term_id, admission_year_id, given_name, \
    middle_name, family_name, date_of_birth, gender, email, phone, subject_ref, offer_expires_on, \
    offer_accepted_at, required_documents, enrolment_type, status, status_reason, \
    process_instance_id, approval_decision, approval_reason, approved_by, finance_evidence, \
    student_id, student_context_id, previous_admission_id, version, created_at, updated_at";

pub(crate) fn from_row(r: &PgRow) -> Result<Admission, sqlx::Error> {
    let given: String = r.try_get("given_name")?;
    let middle: Option<String> = r.try_get("middle_name")?;
    let family: Option<String> = r.try_get("family_name")?;
    let status: String = r.try_get("status")?;
    Ok(Admission {
        id: AdmissionId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        application_id: r
            .try_get::<Option<Uuid>, _>("application_id")?
            .map(AdmissionApplicationId),
        programme_id: r.try_get("programme_id")?,
        curriculum_version_id: r.try_get("curriculum_version_id")?,
        campus_id: r.try_get("campus_id")?,
        admission_term_id: r.try_get("admission_term_id")?,
        admission_year_id: r.try_get("admission_year_id")?,
        display_name: display_name(&given, middle.as_deref(), family.as_deref()),
        given_name: given,
        middle_name: middle,
        family_name: family,
        date_of_birth: r.try_get("date_of_birth")?,
        gender: r.try_get("gender")?,
        email: r.try_get("email")?,
        phone: r.try_get("phone")?,
        subject_ref: r.try_get("subject_ref")?,
        offer_expires_on: r.try_get("offer_expires_on")?,
        offer_accepted_at: r.try_get("offer_accepted_at")?,
        required_documents: r.try_get("required_documents")?,
        enrolment_type: r.try_get("enrolment_type")?,
        status: AdmissionStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("admission status {status}").into()))?,
        status_reason: r.try_get("status_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        approval_decision: r.try_get("approval_decision")?,
        approval_reason: r.try_get("approval_reason")?,
        approved_by: r.try_get("approved_by")?,
        finance_evidence: r.try_get("finance_evidence")?,
        student_id: r.try_get::<Option<Uuid>, _>("student_id")?.map(StudentId),
        student_context_id: r
            .try_get::<Option<Uuid>, _>("student_context_id")?
            .map(StudentContextId),
        previous_admission_id: r
            .try_get::<Option<Uuid>, _>("previous_admission_id")?
            .map(AdmissionId),
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

const DOC_COLUMNS: &str = "id, admission_id, document_type, title, file_ref, status, note, \
    verified_by, verified_at, document_object_id, version, created_at";

fn doc_from_row(r: &PgRow) -> Result<AdmissionDocument, sqlx::Error> {
    let status: String = r.try_get("status")?;
    Ok(AdmissionDocument {
        id: AdmissionDocumentId(r.try_get("id")?),
        admission_id: AdmissionId(r.try_get("admission_id")?),
        document_type: r.try_get("document_type")?,
        title: r.try_get("title")?,
        file_ref: r.try_get("file_ref")?,
        status: DocumentStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("document status {status}").into()))?,
        note: r.try_get("note")?,
        verified_by: r.try_get("verified_by")?,
        verified_at: r.try_get("verified_at")?,
        document_object_id: r.try_get("document_object_id")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
    })
}

/// Loads (optionally `FOR UPDATE`) one admission of the tenant.
pub(crate) async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AdmissionId,
    lock: bool,
) -> Result<Admission, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM student.admission WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "admission",
        })?;
    from_row(&row).map_err(map_db_error)
}

async fn documents(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AdmissionId,
) -> Result<Vec<AdmissionDocument>, KernelError> {
    let sql = format!(
        "SELECT {DOC_COLUMNS} FROM student.admission_document WHERE admission_id = $1 \
         AND tenant_id = $2 ORDER BY created_at, id"
    );
    let rows = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(doc_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Fields of a new Admission Record.
pub(crate) struct NewAdmission {
    pub application_id: Option<AdmissionApplicationId>,
    pub programme_id: Uuid,
    pub curriculum_version_id: Uuid,
    pub campus_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub admission_year_id: Option<Uuid>,
    pub given_name: String,
    pub middle_name: Option<String>,
    pub family_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub subject_ref: Option<String>,
    pub offer_expires_on: Option<NaiveDate>,
    pub required_documents: Vec<String>,
    pub enrolment_type: &'static str,
    pub student_id: Option<StudentId>,
    pub previous_admission_id: Option<AdmissionId>,
}

/// Inserts an `initiated` Admission Record (caller emits `AdmissionInitiated` + history).
pub(crate) async fn insert(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    n: NewAdmission,
) -> Result<Admission, KernelError> {
    let inst = scope
        .institution_id
        .ok_or_else(|| KernelError::Scope("institution scope required".into()))?;
    let sql = format!(
        "INSERT INTO student.admission (tenant_id, institution_id, application_id, programme_id, \
         curriculum_version_id, campus_id, admission_term_id, admission_year_id, given_name, \
         middle_name, family_name, date_of_birth, gender, email, phone, subject_ref, \
         offer_expires_on, required_documents, enrolment_type, student_id, previous_admission_id) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21) \
         RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(inst.as_uuid())
        .bind(n.application_id.map(|i| i.as_uuid()))
        .bind(n.programme_id)
        .bind(n.curriculum_version_id)
        .bind(n.campus_id)
        .bind(n.admission_term_id)
        .bind(n.admission_year_id)
        .bind(&n.given_name)
        .bind(&n.middle_name)
        .bind(&n.family_name)
        .bind(n.date_of_birth)
        .bind(&n.gender)
        .bind(&n.email)
        .bind(&n.phone)
        .bind(&n.subject_ref)
        .bind(n.offer_expires_on)
        .bind(&n.required_documents)
        .bind(n.enrolment_type)
        .bind(n.student_id.map(|s| s.as_uuid()))
        .bind(n.previous_admission_id.map(|p| p.as_uuid()))
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("admission_open_per_student") => {
                KernelError::Conflict(
                    "the student already has an open admission in this institution".into(),
                )
            }
            _ => map_db_error(e),
        })?;
    from_row(&row).map_err(map_db_error)
}

/// Appends an admission history row.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn history(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: AdmissionId,
    from: Option<AdmissionStatus>,
    to: AdmissionStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO student.admission_status_history (tenant_id, admission_id, from_status, \
         to_status, reason, process_instance_id, actor, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(from.map(|f| f.as_str()))
    .bind(to.as_str())
    .bind(reason)
    .bind(process_instance_id)
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// One lifecycle step of a locked admission: legal-transition check, status (+ process id),
/// version bump, history row, business event (StatusChangedV1) and audit — one transaction.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    cur: &Admission,
    to: AdmissionStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    event: (&'static str, &'static str),
    command: &str,
    actor: &Actor,
) -> Result<Admission, KernelError> {
    if !admission_can_transition(cur.status, to) {
        return Err(KernelError::Conflict(format!(
            "admission is {}; it cannot become {to}",
            cur.status
        )));
    }
    let sql = format!(
        "UPDATE student.admission SET status = $3, status_reason = COALESCE($4, status_reason), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(cur.id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(to.as_str())
        .bind(reason)
        .bind(process_instance_id)
        .bind(cur.version.0)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict("admission changed concurrently".into()))?;
    let next = from_row(&row).map_err(map_db_error)?;
    history(
        conn,
        scope,
        next.id,
        Some(cur.status),
        to,
        reason,
        process_instance_id,
        actor,
    )
    .await?;
    publish::event(
        conn,
        scope,
        event.0,
        event.1,
        next.id.to_string(),
        &status_changed(
            next.id.as_uuid(),
            next.institution_id,
            next.student_id.map(|s| s.as_uuid()),
            Some(cur.status.as_str()),
            to.as_str(),
            next.version.0,
            reason.map(str::to_owned),
            process_instance_id
                .map(str::to_owned)
                .or_else(|| next.process_instance_id.clone()),
            next.subject_ref.clone(),
        ),
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        &format!("student.admission.{command}"),
        "admission",
        next.id.to_string(),
        serde_json::json!({ "from": cur.status, "to": to, "reason": reason, "version": next.version.0 }),
        actor,
    )
    .await?;
    Ok(next)
}

fn respond(status: StatusCode, a: &Admission) -> Response {
    with_etag(status, a, a.version)
}

/// Unlocked read + institution checks before a command.
async fn prepare(
    st: &StudentState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: AdmissionId,
) -> Result<(Admission, ExecutionScope), KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, tenant(ctx), id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(st, &scope, cur.institution_id).await?;
    Ok((cur, scope))
}

/// Opens the tx, locks and version-checks the admission.
async fn begin_locked(
    st: &StudentState,
    scope: &ExecutionScope,
    id: AdmissionId,
    expected: Version,
) -> Result<(sqlx::Transaction<'static, sqlx::Postgres>, Admission), KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut tx, scope.tenant_id, id, true).await?;
    check_version(expected, cur.version.0)?;
    Ok((tx, cur))
}

// ---------------------------------------------------------------------------------------------
// Reads
// ---------------------------------------------------------------------------------------------

/// Admission collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AdmissionList {
    pub items: Vec<Admission>,
}

/// Admission document collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AdmissionDocumentList {
    pub items: Vec<AdmissionDocument>,
}

/// Admission history.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AdmissionHistory {
    pub items: Vec<AdmissionHistoryEntry>,
}

/// `?status=`.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct AdmissionListQuery {
    pub status: Option<AdmissionStatus>,
}

/// Admission Records of an institution (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/admissions",
    operation_id = "admission_list",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), AdmissionListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionList), ReadErrors)
)]
pub async fn list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<AdmissionListQuery>,
) -> ApiResult<Json<AdmissionList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let sql = format!(
        "SELECT {COLUMNS} FROM student.admission WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC LIMIT 500"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant(ctx).as_uuid())
        .bind(id.as_uuid())
        .bind(q.status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    Ok(Json(AdmissionList { items }))
}

/// One Admission Record (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/admissions/{id}",
    operation_id = "admission_get",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = Admission, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let a = load(&mut conn, tenant(ctx), id, false).await?;
    Ok(respond(StatusCode::OK, &a))
}

/// Lifecycle history of an admission, oldest first (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/admissions/{id}/history",
    operation_id = "admission_history",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionHistory), ReadErrors)
)]
pub async fn get_history(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
) -> ApiResult<Json<AdmissionHistory>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant(ctx), id, false).await?;
    let rows = sqlx::query(
        "SELECT from_status, to_status, reason, process_instance_id, actor, occurred_at \
         FROM student.admission_status_history WHERE admission_id = $1 AND tenant_id = $2 \
         ORDER BY occurred_at, id",
    )
    .bind(id.as_uuid())
    .bind(tenant(ctx).as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| {
            Ok(AdmissionHistoryEntry {
                from_status: r.try_get("from_status")?,
                to_status: r.try_get("to_status")?,
                reason: r.try_get("reason")?,
                process_instance_id: r.try_get("process_instance_id")?,
                actor: r.try_get("actor")?,
                occurred_at: r.try_get("occurred_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(AdmissionHistory { items }))
}

/// Documents of an admission (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/admissions/{id}/documents",
    operation_id = "admission_document_list",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionDocumentList), ReadErrors)
)]
pub async fn list_documents(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
) -> ApiResult<Json<AdmissionDocumentList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant(ctx), id, false).await?;
    Ok(Json(AdmissionDocumentList {
        items: documents(&mut conn, tenant(ctx), id).await?,
    }))
}

// ---------------------------------------------------------------------------------------------
// Offer acceptance, documents, verification
// ---------------------------------------------------------------------------------------------

/// Record the applicant's acceptance of the offer (`admission.manage`). Status stays
/// `initiated`; an expired offer cannot be accepted.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/accept",
    operation_id = "admission_accept_offer",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn accept(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let (cur, scope) = prepare(&st, ctx, id).await?;
    let today = business_today(&st, &scope, cur.institution_id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    if cur.status != AdmissionStatus::Initiated {
        return Err(KernelError::Conflict(format!(
            "admission is {}; only an initiated offer can be accepted",
            cur.status
        ))
        .into());
    }
    if cur.offer_accepted_at.is_some() {
        return Err(KernelError::Conflict("the offer was already accepted".into()).into());
    }
    if cur.offer_expires_on.is_some_and(|d| d < today) {
        return Err(KernelError::Conflict("the offer has expired".into()).into());
    }
    let sql = format!(
        "UPDATE student.admission SET offer_accepted_at = now(), version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let next = from_row(&row).map_err(map_db_error)?;
    publish::event(
        &mut tx,
        &scope,
        ADMISSION_OFFER_ACCEPTED,
        ADMISSION_OFFER_ACCEPTED_SCHEMA,
        next.id.to_string(),
        &status_changed(
            next.id.as_uuid(),
            next.institution_id,
            next.student_id.map(|s| s.as_uuid()),
            Some(cur.status.as_str()),
            next.status.as_str(),
            next.version.0,
            reason.clone(),
            None,
            next.subject_ref.clone(),
        ),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.admission.accept_offer",
        "admission",
        next.id.to_string(),
        serde_json::json!({ "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// A document handed in for an admission.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AddAdmissionDocument {
    /// snake_case type key (e.g. `identity_proof`).
    pub document_type: String,
    pub title: String,
    /// Reference into document storage.
    pub file_ref: Option<String>,
    pub note: Option<String>,
}

/// Hand in a document (`admission.manage`) while the admission is `initiated` or
/// `documents_submitted`. Earlier rejected documents of the same type are superseded.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/documents",
    operation_id = "admission_document_add",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = AddAdmissionDocument,
    responses((status = 201, body = AdmissionDocument, headers(("ETag" = String))), CreateErrors)
)]
pub async fn add_document(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    Json(cmd): Json<AddAdmissionDocument>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    require_idempotency_key(&headers)?;
    let mut v = vec![];
    validate_key("documentType", cmd.document_type.trim(), &mut v);
    validate_text("title", &cmd.title, 200, &mut v);
    validate_opt_text("fileRef", cmd.file_ref.as_deref(), 500, &mut v);
    validate_opt_text("note", cmd.note.as_deref(), 1000, &mut v);
    finish(v)?;
    let (_, scope) = prepare(&st, ctx, id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut tx, scope.tenant_id, id, true).await?;
    if !matches!(
        cur.status,
        AdmissionStatus::Initiated | AdmissionStatus::DocumentsSubmitted
    ) {
        return Err(KernelError::Conflict(format!(
            "admission is {}; documents can no longer be added",
            cur.status
        ))
        .into());
    }
    sqlx::query(
        "UPDATE student.admission_document SET status = 'superseded', version = version + 1, \
         updated_at = now() WHERE admission_id = $1 AND document_type = $2 AND status = 'rejected'",
    )
    .bind(id.as_uuid())
    .bind(cmd.document_type.trim())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.admission_document (tenant_id, admission_id, document_type, title, \
         file_ref, note) VALUES ($1, $2, $3, $4, $5, $6) RETURNING {DOC_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .bind(cmd.document_type.trim())
        .bind(cmd.title.trim())
        .bind(norm(cmd.file_ref.as_deref()))
        .bind(norm(cmd.note.as_deref()))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let doc = doc_from_row(&row).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.admission.add_document",
        "admission",
        id.to_string(),
        serde_json::json!({ "documentId": doc.id, "documentType": doc.document_type }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &doc, doc.version))
}

/// Declare the documents submitted (`admission.manage`): the offer must be accepted and every
/// required document type handed in (`initiated → documents_submitted`).
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/submit-documents",
    operation_id = "admission_submit_documents",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn submit_documents(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let (_, scope) = prepare(&st, ctx, id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    if cur.status == AdmissionStatus::Initiated && cur.offer_accepted_at.is_none() {
        return Err(KernelError::Conflict("the offer has not been accepted yet".into()).into());
    }
    let docs = documents(&mut tx, scope.tenant_id, id).await?;
    let pairs: Vec<_> = docs
        .iter()
        .map(|d| (d.document_type.clone(), d.status))
        .collect();
    if let VerificationState::Missing(m) = verification_state(&cur.required_documents, &pairs) {
        return Err(invalid(
            "documents",
            "missing",
            format!("required documents missing: {}", m.join(", ")),
        )
        .into());
    }
    let next = transition(
        &mut tx,
        &scope,
        &cur,
        AdmissionStatus::DocumentsSubmitted,
        reason.as_deref(),
        None,
        (
            ADMISSION_DOCUMENTS_SUBMITTED,
            ADMISSION_DOCUMENTS_SUBMITTED_SCHEMA,
        ),
        "submit_documents",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Verification outcome of one document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
#[schema(as = AdmissionVerificationOutcome)]
pub enum VerificationOutcome {
    Verified,
    Rejected,
    Waived,
}

/// Body of `…/documents/{documentId}/verify`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = VerifyAdmissionDocument)]
pub struct VerifyDocument {
    pub outcome: VerificationOutcome,
    /// Required for `rejected` and `waived`.
    pub note: Option<String>,
}

/// Record the verification outcome of one document (`admission.manage`; `If-Match` = the
/// document's ETag). When every required type is verified or waived and nothing is pending,
/// the admission becomes `verification_completed` (`DocumentsVerified`) in the same transaction.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/documents/{documentId}/verify",
    operation_id = "admission_document_verify",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("documentId" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the document version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = VerifyDocument,
    responses((status = 200, body = AdmissionDocument, headers(("ETag" = String))), CommandErrors)
)]
pub async fn verify_document(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, document_id)): Path<(AdmissionId, AdmissionDocumentId)>,
    headers: HeaderMap,
    Json(cmd): Json<VerifyDocument>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let note = norm(cmd.note.as_deref());
    if cmd.outcome != VerificationOutcome::Verified && note.is_none() {
        return Err(invalid("note", "required", "a note is required to reject or waive").into());
    }
    let mut v = vec![];
    validate_opt_text("note", note.as_deref(), 1000, &mut v);
    finish(v)?;
    let (_, scope) = prepare(&st, ctx, id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut tx, scope.tenant_id, id, true).await?;
    if cur.status != AdmissionStatus::DocumentsSubmitted {
        return Err(KernelError::Conflict(format!(
            "admission is {}; documents are verified after submission",
            cur.status
        ))
        .into());
    }
    let sql = format!(
        "SELECT {DOC_COLUMNS} FROM student.admission_document WHERE id = $1 AND admission_id = $2 \
         FOR UPDATE"
    );
    let doc = sqlx::query(&sql)
        .bind(document_id.as_uuid())
        .bind(id.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "admission document",
        })?;
    let doc = doc_from_row(&doc).map_err(map_db_error)?;
    check_version(expected, doc.version.0)?;
    if doc.status != DocumentStatus::Submitted {
        return Err(KernelError::Conflict(format!(
            "document is {}; it was already decided",
            doc.status
        ))
        .into());
    }
    let to = match cmd.outcome {
        VerificationOutcome::Verified => DocumentStatus::Verified,
        VerificationOutcome::Rejected => DocumentStatus::Rejected,
        VerificationOutcome::Waived => DocumentStatus::Waived,
    };
    // Part 2 · S4-docs: a document with a stored file is verified against the bytes that were
    // uploaded — the SHA-256 is re-checked (a changed or missing object is quarantined, 409).
    if let (DocumentStatus::Verified, Some(object)) = (to, doc.document_object_id) {
        st.documents
            .verify_integrity(
                &scope,
                object,
                &[DocumentOwner::new(
                    OWNER_ADMISSION_DOCUMENT,
                    doc.id.as_uuid(),
                )],
                &DocumentActor {
                    id: a.id.clone(),
                    correlation_id: a.correlation_id.clone(),
                },
            )
            .await?;
    }
    let sql = format!(
        "UPDATE student.admission_document SET status = $2, note = COALESCE($3, note), \
         verified_by = $4, verified_at = now(), version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING {DOC_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(document_id.as_uuid())
        .bind(to.as_str())
        .bind(&note)
        .bind(a.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let updated = doc_from_row(&row).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.admission.verify_document",
        "admission",
        id.to_string(),
        serde_json::json!({ "documentId": document_id, "outcome": to, "note": note }),
        &a,
    )
    .await?;
    let docs = documents(&mut tx, scope.tenant_id, id).await?;
    let pairs: Vec<_> = docs
        .iter()
        .map(|d| (d.document_type.clone(), d.status))
        .collect();
    if verification_state(&cur.required_documents, &pairs) == VerificationState::Complete {
        transition(
            &mut tx,
            &scope,
            &cur,
            AdmissionStatus::VerificationCompleted,
            None,
            None,
            (
                ADMISSION_DOCUMENTS_VERIFIED,
                ADMISSION_DOCUMENTS_VERIFIED_SCHEMA,
            ),
            "complete_verification",
            &a,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &updated, updated.version))
}

// ---------------------------------------------------------------------------------------------
// Approval (WorkflowPort), financial clearance, confirmation
// ---------------------------------------------------------------------------------------------

/// Applies an approval decision to a locked `approval_pending` admission.
async fn apply_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    cur: &Admission,
    decision: Decision,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<Admission, KernelError> {
    sqlx::query(
        "UPDATE student.admission SET approval_decision = $2, approval_reason = $3, \
         approved_by = $4 WHERE id = $1",
    )
    .bind(cur.id.as_uuid())
    .bind(match decision {
        Decision::Approve => "approve",
        Decision::Reject => "reject",
    })
    .bind(reason)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let pid = cur.process_instance_id.clone();
    match decision {
        Decision::Approve => {
            transition(
                conn,
                scope,
                cur,
                AdmissionStatus::Approved,
                reason,
                pid.as_deref(),
                (ADMISSION_APPROVED, ADMISSION_APPROVED_SCHEMA),
                "approve",
                actor,
            )
            .await
        }
        Decision::Reject => {
            transition(
                conn,
                scope,
                cur,
                AdmissionStatus::Cancelled,
                reason,
                pid.as_deref(),
                (ADMISSION_CANCELLED, ADMISSION_CANCELLED_SCHEMA),
                "reject",
                actor,
            )
            .await
        }
    }
}

/// Start the admission approval (`admission.manage`): **the reference BPMN approval of Batch 3**.
/// The process `student.admission_approval` is started through the WorkflowPort with the
/// deterministic business key `admission:<id>:v<version>`; the admission becomes
/// `approval_pending` (`admission.approval_requested`, carrying `processInstanceId`). With the
/// Batch 3 auto-approval the decision (`approved`, actor `system:auto-approval`) is applied in the
/// same transaction; with `AwaitDecision` it waits for `POST …/decision`.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/submit-for-approval",
    operation_id = "admission_submit_for_approval",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn submit_for_approval(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let (cur, scope) = prepare(&st, ctx, id).await?;
    check_version(expected, cur.version.0)?;
    if !admission_can_transition(cur.status, AdmissionStatus::ApprovalPending) {
        return Err(KernelError::Conflict(format!(
            "admission is {}; approval needs completed verification",
            cur.status
        ))
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("admissionId".to_owned(), serde_json::json!(cur.id));
    vars.insert(
        "institutionId".to_owned(),
        serde_json::json!(cur.institution_id),
    );
    vars.insert(
        "programmeId".to_owned(),
        serde_json::json!(cur.programme_id),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::ADMISSION_APPROVAL,
        business_key("admission", cur.id, cur.version.0),
        vars,
    )
    .await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    let pending = transition(
        &mut tx,
        &scope,
        &cur,
        AdmissionStatus::ApprovalPending,
        reason.as_deref(),
        Some(outcome.process_instance_id()),
        (
            ADMISSION_APPROVAL_REQUESTED,
            ADMISSION_APPROVAL_REQUESTED_SCHEMA,
        ),
        "request_approval",
        &a,
    )
    .await?;
    let next = match outcome {
        ApprovalOutcome::Approved { .. } => {
            apply_decision(
                &mut tx,
                &scope,
                &pending,
                Decision::Approve,
                None,
                &Actor::system(a.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => pending,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Decide a pending admission approval (`admission.decide`): `approve` → `approved`
/// (`AdmissionApproved`); `reject` (reason required) → `cancelled` (`AdmissionCancelled`).
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/decision",
    operation_id = "admission_decide",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn decide(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    Json(cmd): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionDecide)?;
    let expected = expect_version!(headers);
    let reason = cmd.checked_reason()?;
    let (_, scope) = prepare(&st, ctx, id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    if cur.status != AdmissionStatus::ApprovalPending {
        return Err(KernelError::Conflict(format!(
            "admission is {}; no approval is pending",
            cur.status
        ))
        .into());
    }
    // Part 1 (DEF-X-002): whoever created the admission or requested its approval never decides it.
    let makers: Vec<String> = sqlx::query_scalar(
        "SELECT DISTINCT actor FROM student.admission_status_history \
         WHERE tenant_id = $1 AND admission_id = $2 AND actor IS NOT NULL AND actor <> '' \
         AND (from_status IS NULL OR to_status = 'approval_pending')",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sutra_kernel::approval::ensure_independent_decider(
        a.id.as_deref(),
        makers.iter().map(String::as_str),
        "admission",
    )?;
    let next = apply_decision(&mut tx, &scope, &cur, cmd.decision, reason.as_deref(), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Financial clearance decision (`admission.decide`): Admissions reads Finance-owned evidence
/// through `FinanceClearancePort` (amendment 2026-10-05) and, when it permits clearance
/// (`cleared` or `not_applicable`), records its own decision `approved → financially_cleared`
/// with the evidence. Outstanding / pending evidence is a `409` and changes nothing.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/financial-clearance",
    operation_id = "admission_financial_clearance",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn financial_clearance(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionDecide)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let (cur, scope) = prepare(&st, ctx, id).await?;
    if cur.status != AdmissionStatus::Approved {
        return Err(KernelError::Conflict(format!(
            "admission is {}; financial clearance follows approval",
            cur.status
        ))
        .into());
    }
    let evidence = st
        .finance
        .admission_clearance(
            &scope,
            AdmissionFinanceQuery {
                institution_id: cur.institution_id,
                admission_id: cur.id.as_uuid(),
                programme_id: cur.programme_id,
                student_id: cur.student_id.map(|s| s.as_uuid()),
            },
        )
        .await?;
    if !evidence.permits_clearance() {
        let detail = match &evidence {
            sutra_contracts::finance::FinanceEvidence::Outstanding { detail }
            | sutra_contracts::finance::FinanceEvidence::Pending { detail } => detail.clone(),
            _ => String::new(),
        };
        return Err(KernelError::Conflict(format!(
            "Finance evidence does not permit clearance: {detail}"
        ))
        .into());
    }
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    sqlx::query("UPDATE student.admission SET finance_evidence = $2 WHERE id = $1")
        .bind(cur.id.as_uuid())
        .bind(serde_json::to_value(&evidence).map_err(KernelError::internal)?)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let next = transition(
        &mut tx,
        &scope,
        &cur,
        AdmissionStatus::FinanciallyCleared,
        reason.as_deref(),
        None,
        (
            ADMISSION_FINANCIALLY_CLEARED,
            ADMISSION_FINANCIALLY_CLEARED_SCHEMA,
        ),
        "clear_financially",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Confirm the admission (`admission.decide`): `financially_cleared → confirmed`
/// (`AdmissionConfirmed`, the authoritative basis for student creation).
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/confirm",
    operation_id = "admission_confirm",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn confirm(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionDecide)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let (_, scope) = prepare(&st, ctx, id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    let next = transition(
        &mut tx,
        &scope,
        &cur,
        AdmissionStatus::Confirmed,
        reason.as_deref(),
        None,
        (ADMISSION_CONFIRMED, ADMISSION_CONFIRMED_SCHEMA),
        "confirm",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Create the student from a confirmed admission (`student.manage`, L3-C-001): the canonical
/// Student (or, on readmission, the existing persona) and a `pending_activation` StudentContext
/// are established and the persona activated (`StudentCreated`, `StudentActivated`); the
/// admission becomes `student_created`. Enrolment (`POST /students/{id}/enrol`) completes it.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/create-student",
    operation_id = "admission_create_student",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn create_student(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let (cur, scope) = prepare(&st, ctx, id).await?;
    if cur.status != AdmissionStatus::Confirmed {
        return Err(KernelError::Conflict(format!(
            "admission is {}; a student is created from a confirmed admission",
            cur.status
        ))
        .into());
    }
    let today = business_today(&st, &scope, cur.institution_id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    let (student_id, context_id) =
        creation::establish_from_admission(&mut tx, &scope, &cur, today, &a).await?;
    sqlx::query(
        "UPDATE student.admission SET student_id = $2, student_context_id = $3 WHERE id = $1",
    )
    .bind(cur.id.as_uuid())
    .bind(student_id.as_uuid())
    .bind(context_id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut with_student = cur.clone();
    with_student.student_id = Some(student_id);
    let next = transition(
        &mut tx,
        &scope,
        &with_student,
        AdmissionStatus::StudentCreated,
        reason.as_deref(),
        None,
        (ADMISSION_STUDENT_CREATED, ADMISSION_STUDENT_CREATED_SCHEMA),
        "create_student",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Marks the admission behind an enrolled context `completed` (called by enrolment).
pub(crate) async fn complete_for_context(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    admission_id: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let cur = load(conn, scope.tenant_id, AdmissionId(admission_id), true).await?;
    if cur.status == AdmissionStatus::StudentCreated {
        transition(
            conn,
            scope,
            &cur,
            AdmissionStatus::Completed,
            None,
            None,
            (ADMISSION_COMPLETED, ADMISSION_COMPLETED_SCHEMA),
            "complete",
            actor,
        )
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Cancellation, withdrawal, revocation, expiry
// ---------------------------------------------------------------------------------------------

/// Cancel an admission before confirmation (`admission.manage`; reason required, L3-C-002).
/// Not possible once the student exists (use revocation).
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/cancel",
    operation_id = "admission_cancel",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn cancel(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().required_reason()?;
    let (_, scope) = prepare(&st, ctx, id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    let next = transition(
        &mut tx,
        &scope,
        &cur,
        AdmissionStatus::Cancelled,
        Some(&reason),
        None,
        (ADMISSION_CANCELLED, ADMISSION_CANCELLED_SCHEMA),
        "cancel",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Withdraw a confirmed admission at the applicant's request (`admission.manage`; reason
/// required, L3-C-003).
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/withdraw",
    operation_id = "admission_withdraw",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn withdraw(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().required_reason()?;
    let (_, scope) = prepare(&st, ctx, id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    let next = transition(
        &mut tx,
        &scope,
        &cur,
        AdmissionStatus::Withdrawn,
        Some(&reason),
        None,
        (ADMISSION_WITHDRAWN, ADMISSION_WITHDRAWN_SCHEMA),
        "withdraw",
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Body of `…/revoke`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = AdmissionRevocation)]
pub struct RevokeBody {
    pub reason: String,
    /// Supporting evidence (e.g. a forged certificate finding).
    pub evidence: String,
}

/// Revoke an admission (`admission.revoke`, **step-up**, L3-C-004) from `confirmed`, or —
/// policy-controlled — after the student was created. The `student.admission_revocation`
/// process is started for traceability; the step-up-authorised caller is the deciding authority.
/// When a live StudentContext exists, Student Lifecycle applies its governed consequence in the
/// same transaction: the context becomes `terminated` (cause `admission.revoked`).
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/revoke",
    operation_id = "admission_revoke",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RevokeBody,
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn revoke(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    Json(cmd): Json<RevokeBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionRevoke)?;
    let expected = expect_version!(headers);
    let mut v = vec![];
    validate_text("reason", &cmd.reason, 1000, &mut v);
    validate_text("evidence", &cmd.evidence, 2000, &mut v);
    finish(v)?;
    let (cur, scope) = prepare(&st, ctx, id).await?;
    check_version(expected, cur.version.0)?;
    if !admission_can_transition(cur.status, AdmissionStatus::Revoked) {
        return Err(KernelError::Conflict(format!(
            "admission is {}; it cannot be revoked",
            cur.status
        ))
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("admissionId".to_owned(), serde_json::json!(cur.id));
    let outcome = request_approval(
        &st,
        &scope,
        process::ADMISSION_REVOCATION,
        business_key("admission-revocation", cur.id, cur.version.0),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let a = actor(ctx);
    // A terminated context closes on the business-local date (ADR-L5-0036 §3.4), not UTC.
    let today = business_today(&st, &scope, cur.institution_id).await?;
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    sqlx::query("UPDATE student.admission SET revocation_evidence = $2 WHERE id = $1")
        .bind(cur.id.as_uuid())
        .bind(cmd.evidence.trim())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let next = transition(
        &mut tx,
        &scope,
        &cur,
        AdmissionStatus::Revoked,
        Some(cmd.reason.trim()),
        Some(&pid),
        (ADMISSION_REVOKED, ADMISSION_REVOKED_SCHEMA),
        "revoke",
        &a,
    )
    .await?;
    if let Some(cid) = cur.student_context_id {
        let c = lock_context(&mut tx, scope.tenant_id, cid).await?;
        if c.status.is_live() {
            change_context_status(
                &mut tx,
                &c,
                StudentContextStatus::Terminated,
                &StatusChange {
                    cause_event_type: ADMISSION_REVOKED,
                    reason: Some(cmd.reason.trim()),
                    process_instance_id: Some(&pid),
                    closed_on: Some(today),
                },
                &a,
            )
            .await?;
        }
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

/// Expire one un-accepted offer whose validity date has passed (`admission.manage`, L3-C-006):
/// `initiated → expired` (`AdmissionExpired`). A valid offer is a `409`.
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/expire",
    operation_id = "admission_expire",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = Admission, headers(("ETag" = String))), CommandErrors)
)]
pub async fn expire(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let (cur, scope) = prepare(&st, ctx, id).await?;
    let today = business_today(&st, &scope, cur.institution_id).await?;
    let a = actor(ctx);
    let (mut tx, cur) = begin_locked(&st, &scope, id, expected).await?;
    let next = expire_one(&mut tx, &scope, &cur, today, reason.as_deref(), &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &next))
}

async fn expire_one(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    cur: &Admission,
    today: NaiveDate,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<Admission, KernelError> {
    if cur.status != AdmissionStatus::Initiated || cur.offer_accepted_at.is_some() {
        return Err(KernelError::Conflict(format!(
            "admission is {}{}; only an un-accepted offer expires",
            cur.status,
            if cur.offer_accepted_at.is_some() {
                " (accepted)"
            } else {
                ""
            }
        )));
    }
    match cur.offer_expires_on {
        Some(d) if d < today => {}
        _ => {
            return Err(KernelError::Conflict(
                "the offer is still valid (no validity date passed)".into(),
            ))
        }
    }
    transition(
        conn,
        scope,
        cur,
        AdmissionStatus::Expired,
        Some(reason.unwrap_or("offer validity date passed without acceptance")),
        None,
        (ADMISSION_EXPIRED, ADMISSION_EXPIRED_SCHEMA),
        "expire",
        actor,
    )
    .await
}

/// Result of an expiry sweep.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = AdmissionExpirySweep)]
pub struct ExpirySweep {
    /// Business date the sweep compared against.
    pub as_of: NaiveDate,
    pub expired: Vec<AdmissionId>,
}

/// Expire every un-accepted offer of the institution whose validity date has passed
/// (`admission.manage`; the explicit sweep command, idempotent: a second run expires nothing).
#[utoipa::path(
    post,
    path = "/api/v1/student/institutions/{id}/admissions/expire-offers",
    operation_id = "admission_expire_offers",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExpirySweep), CommandErrors)
)]
pub async fn expire_offers(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<ExpirySweep>> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let scope = institution_scope(ctx, id);
    require_institution_writable(&st, &scope, id).await?;
    let today = business_today(&st, &scope, id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM student.admission WHERE tenant_id = $1 AND institution_id = $2 \
         AND status = 'initiated' AND offer_accepted_at IS NULL AND offer_expires_on < $3 \
         ORDER BY id FOR UPDATE SKIP LOCKED",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(today)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut expired = vec![];
    for i in ids {
        let cur = load(&mut tx, scope.tenant_id, AdmissionId(i), true).await?;
        expire_one(&mut tx, &scope, &cur, today, None, &a).await?;
        expired.push(cur.id);
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(ExpirySweep {
        as_of: today,
        expired,
    }))
}

// ---------------------------------------------------------------------------------------------
// Readmission
// ---------------------------------------------------------------------------------------------

/// Body of `…/readmit`.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = AdmissionReadmission)]
pub struct ReadmitBody {
    /// Defaults to the historical admission's programme.
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    /// Default: today + 14 days.
    pub offer_expires_on: Option<NaiveDate>,
    pub required_documents: Option<Vec<String>>,
    pub reason: String,
}

/// Readmission (`admission.manage`, L3-C-005): a **new** Admission Record (`initiated`,
/// `AdmissionInitiated` with `previousAdmissionId`) reopens a closed admission (cancelled,
/// withdrawn, revoked, expired, or completed when the student no longer has a live context in
/// the institution). The historical record is untouched; an existing student is reused
/// (`enrolmentType = readmission`); duplicate open admissions are refused (409).
#[utoipa::path(
    post,
    path = "/api/v1/student/admissions/{id}/readmit",
    operation_id = "admission_readmit",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ReadmitBody,
    responses((status = 201, body = Admission, headers(("ETag" = String))), CreateErrors)
)]
pub async fn readmit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionId>,
    headers: HeaderMap,
    Json(cmd): Json<ReadmitBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    require_idempotency_key(&headers)?;
    let reason = CommandBody {
        reason: Some(cmd.reason.clone()),
    }
    .required_reason()?;
    let (src, scope) = prepare(&st, ctx, id).await?;
    let inst = src.institution_id;
    if !readmission_source(src.status) {
        return Err(KernelError::Conflict(format!(
            "admission is {}; readmission starts from a closed admission",
            src.status
        ))
        .into());
    }
    if let Some(sid) = src.student_id {
        if live_context(&st.pool, scope.tenant_id, sid, inst)
            .await?
            .is_some()
        {
            return Err(KernelError::Conflict(
                "the student still has a live context in this institution".into(),
            )
            .into());
        }
    }
    let today = business_today(&st, &scope, inst).await?;
    let expires = cmd
        .offer_expires_on
        .unwrap_or(today + chrono::Duration::days(14));
    if expires < today {
        return Err(invalid(
            "offerExpiresOn",
            "in-past",
            "the offer cannot expire before today",
        )
        .into());
    }
    let required = required_documents(cmd.required_documents.as_deref())?;
    let programme_id = cmd.programme_id.unwrap_or(src.programme_id);
    refs::programme(&st, &scope, inst, programme_id, "programmeId").await?;
    let cv = refs::curriculum(
        &st,
        &scope,
        programme_id,
        cmd.curriculum_version_id,
        today,
        "curriculumVersionId",
    )
    .await?;
    let term = cmd.admission_term_id.or(src.admission_term_id);
    let year = refs::term(&st, &scope, inst, term, "admissionTermId").await?;
    let campus = cmd.campus_id.or(src.campus_id);
    refs::campus(&st, &scope, inst, campus, "campusId").await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // DEF-C-002: one closed admission reopens at most one live admission. Lock the source so
    // concurrent readmissions serialise, then refuse when a live readmission of it exists.
    sqlx::query("SELECT id FROM student.admission WHERE id = $1 AND tenant_id = $2 FOR UPDATE")
        .bind(src.id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let open_readmission: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM student.admission WHERE tenant_id = $1 AND previous_admission_id = $2 \
         AND status NOT IN ('cancelled', 'withdrawn', 'revoked', 'expired', 'completed') LIMIT 1",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(src.id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(open) = open_readmission {
        return Err(KernelError::Conflict(format!(
            "admission {} was already readmitted as admission {open}, which is still open",
            src.id
        ))
        .into());
    }
    // Without a student yet, the person key (name + DOB + contact) guards against a second live
    // admission / application / student for the same person (an existing student is guarded by
    // the `admission_open_per_student` index).
    if src.student_id.is_none() {
        if let Some(key) = super::duplicates::PersonKey::new(
            &src.given_name,
            src.family_name.as_deref(),
            src.date_of_birth,
            src.email.as_deref(),
            src.phone.as_deref(),
        ) {
            let found = super::duplicates::find(
                &mut tx,
                scope.tenant_id,
                inst,
                &key,
                Some(src.id.as_uuid()),
            )
            .await?;
            if !found.is_empty() {
                return Err(super::duplicates::conflict(
                    &found,
                    "the person already has a live record in this institution; continue that one",
                )
                .into());
            }
        }
    }
    // The student's current identity wins over the historical snapshot.
    let (given, middle, family, dob, gender, email, phone, subject) = match src.student_id {
        Some(sid) => {
            let s = crate::shared::student::lock_student(&mut tx, scope.tenant_id, sid).await?;
            (
                s.given_name,
                s.middle_name,
                s.family_name,
                s.date_of_birth,
                s.gender,
                s.primary_email,
                s.primary_phone,
                s.subject_ref,
            )
        }
        None => (
            src.given_name.clone(),
            src.middle_name.clone(),
            src.family_name.clone(),
            src.date_of_birth,
            src.gender.clone(),
            src.email.clone(),
            src.phone.clone(),
            src.subject_ref.clone(),
        ),
    };
    let next = insert(
        &mut tx,
        &scope,
        NewAdmission {
            application_id: None,
            programme_id,
            curriculum_version_id: cv.id,
            campus_id: campus,
            admission_term_id: term,
            admission_year_id: year.or(src.admission_year_id),
            given_name: given,
            middle_name: middle,
            family_name: family,
            date_of_birth: dob,
            gender,
            email,
            phone,
            subject_ref: subject,
            offer_expires_on: Some(expires),
            required_documents: required,
            enrolment_type: if src.student_id.is_some() {
                "readmission"
            } else {
                "regular"
            },
            student_id: src.student_id,
            previous_admission_id: Some(src.id),
        },
    )
    .await?;
    history(
        &mut tx,
        &scope,
        next.id,
        None,
        AdmissionStatus::Initiated,
        Some(&reason),
        None,
        &a,
    )
    .await?;
    publish::event(
        &mut tx,
        &scope,
        ADMISSION_INITIATED,
        ADMISSION_INITIATED_SCHEMA,
        next.id.to_string(),
        &AdmissionInitiatedV1 {
            admission_id: next.id.as_uuid(),
            institution_id: inst.as_uuid(),
            application_id: None,
            programme_id,
            curriculum_version_id: Some(cv.id),
            admission_term_id: term,
            offer_expires_on: Some(expires),
            student_id: src.student_id.map(|s| s.as_uuid()),
            previous_admission_id: Some(src.id.as_uuid()),
        },
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.admission.readmit",
        "admission",
        next.id.to_string(),
        serde_json::json!({ "previousAdmissionId": src.id, "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::CREATED, &next))
}
