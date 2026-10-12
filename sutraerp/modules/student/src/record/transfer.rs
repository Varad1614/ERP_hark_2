//! Student transfers (L3-C-012 intra-institute reassignment, L3-C-013 inter-institute transfer
//! inside the tenant with StudentContext re-establishment).
//!
//! * `intra_institute` (`student.manage`): campus / department / batch / section move inside the
//!   same institution, programme unchanged; the active context's placement is updated (history
//!   kept) and `student.transferred` is published.
//! * `inter_institute` (`student.lifecycle`, **step-up**): the source context becomes
//!   `transferred` and a new `pending_activation` context opens at the destination
//!   (`enrolmentType = transfer_in`, `predecessorContextId`), on the destination's published
//!   programme/curriculum; the canonical Student is unchanged. Enrolment there
//!   (`POST /students/{id}/enrol`) activates it. No 3C exit record is created.

use super::common::{update_placement, validate_code, Placement};
use super::identity::{require_case, CaseListQuery};
use super::refs;
use super::support::{
    check_version, expect_version, finish, norm, validate_text, CommandBody, CommandErrors,
    CreateErrors, Decision, DecisionBody, ReadErrors,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution_writable, require_student_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_context_status, StatusChange};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::student::{
    context_from_row, live_context, load_student, lock_context, StudentContextRow, CONTEXT_COLUMNS,
};
use crate::shared::time::business_today;
use crate::shared::{publish, Actor, StudentState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::student::events::record::{
    StudentTransferredV1, STUDENT_TRANSFERRED, STUDENT_TRANSFERRED_SCHEMA,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId, StudentTransferId};
use sutra_contracts::student::status::{CaseStatus, StudentContextStatus, StudentTransferKind};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

/// A transfer case.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StudentTransfer {
    pub id: StudentTransferId,
    pub student_id: StudentId,
    pub kind: StudentTransferKind,
    pub from_context_id: StudentContextId,
    /// The destination context (inter-institute) or the same context (intra), once completed.
    pub to_context_id: Option<StudentContextId>,
    pub from_institution_id: InstitutionId,
    pub to_institution_id: InstitutionId,
    pub to_campus_id: Option<Uuid>,
    pub to_org_unit_id: Option<Uuid>,
    pub to_programme_id: Option<Uuid>,
    pub to_curriculum_version_id: Option<Uuid>,
    pub to_batch_code: Option<String>,
    pub to_section_code: Option<String>,
    pub effective_on: NaiveDate,
    pub reason: String,
    pub status: CaseStatus,
    pub decision_reason: Option<String>,
    pub process_instance_id: Option<String>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct StudentTransferList {
    pub items: Vec<StudentTransfer>,
}

const COLUMNS: &str = "id, student_id, kind, from_context_id, to_context_id, from_institution_id, \
    to_institution_id, to_campus_id, to_org_unit_id, to_programme_id, to_curriculum_version_id, \
    to_batch_code, to_section_code, effective_on, reason, status, decision_reason, \
    process_instance_id, version, created_at, updated_at";

fn from_row(r: &PgRow) -> Result<StudentTransfer, sqlx::Error> {
    let kind: String = r.try_get("kind")?;
    let status: String = r.try_get("status")?;
    Ok(StudentTransfer {
        id: StudentTransferId(r.try_get("id")?),
        student_id: StudentId(r.try_get("student_id")?),
        kind: StudentTransferKind::parse(&kind)
            .ok_or_else(|| sqlx::Error::Decode(format!("transfer kind {kind}").into()))?,
        from_context_id: StudentContextId(r.try_get("from_context_id")?),
        to_context_id: r
            .try_get::<Option<Uuid>, _>("to_context_id")?
            .map(StudentContextId),
        from_institution_id: InstitutionId(r.try_get("from_institution_id")?),
        to_institution_id: InstitutionId(r.try_get("to_institution_id")?),
        to_campus_id: r.try_get("to_campus_id")?,
        to_org_unit_id: r.try_get("to_org_unit_id")?,
        to_programme_id: r.try_get("to_programme_id")?,
        to_curriculum_version_id: r.try_get("to_curriculum_version_id")?,
        to_batch_code: r.try_get("to_batch_code")?,
        to_section_code: r.try_get("to_section_code")?,
        effective_on: r.try_get("effective_on")?,
        reason: r.try_get("reason")?,
        status: CaseStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("case status {status}").into()))?,
        decision_reason: r.try_get("decision_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: StudentTransferId,
    lock: bool,
) -> Result<StudentTransfer, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM student.student_transfer WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "student transfer",
        })?;
    from_row(&r).map_err(map_db_error)
}

async fn set_status(
    conn: &mut PgConnection,
    cur: &StudentTransfer,
    to: CaseStatus,
    decision_reason: Option<&str>,
    pid: Option<&str>,
    to_context: Option<StudentContextId>,
) -> Result<StudentTransfer, KernelError> {
    require_case(cur.status, to, "transfer")?;
    let sql = format!(
        "UPDATE student.student_transfer SET status = $2, \
         decision_reason = COALESCE($3, decision_reason), \
         process_instance_id = COALESCE($4, process_instance_id), \
         to_context_id = COALESCE($5, to_context_id), version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING {COLUMNS}"
    );
    let r = sqlx::query(&sql)
        .bind(cur.id.as_uuid())
        .bind(to.as_str())
        .bind(decision_reason)
        .bind(pid)
        .bind(to_context.map(|c| c.as_uuid()))
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    from_row(&r).map_err(map_db_error)
}

/// Permission per kind: intra → `student.manage`, inter → `student.lifecycle` (step-up).
fn permission(kind: StudentTransferKind) -> Permission {
    match kind {
        StudentTransferKind::IntraInstitute => Permission::StudentManage,
        StudentTransferKind::InterInstitute => Permission::StudentLifecycle,
    }
}

/// Body of a new transfer.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = NewStudentTransfer)]
pub struct NewTransfer {
    pub kind: StudentTransferKind,
    /// Source context (default: the current live context).
    pub from_context_id: Option<StudentContextId>,
    /// Required for `inter_institute` (another institution of the tenant).
    pub to_institution_id: Option<InstitutionId>,
    pub to_campus_id: Option<Uuid>,
    pub to_org_unit_id: Option<Uuid>,
    /// `inter_institute` only: the destination's published programme (required).
    pub to_programme_id: Option<Uuid>,
    pub to_curriculum_version_id: Option<Uuid>,
    pub to_batch_code: Option<String>,
    pub to_section_code: Option<String>,
    /// Business-local; default today.
    pub effective_on: Option<NaiveDate>,
    pub reason: String,
}

/// Validates the destination against Domain B (read only).
async fn validate_destination(
    st: &StudentState,
    scope_of: impl Fn(InstitutionId) -> ExecutionScope,
    t: &StudentTransfer,
) -> Result<(), KernelError> {
    let dest = t.to_institution_id;
    let scope = scope_of(dest);
    require_institution_writable(st, &scope, dest).await?;
    refs::campus(st, &scope, dest, t.to_campus_id, "toCampusId").await?;
    refs::org_unit(st, &scope, dest, t.to_org_unit_id, "toOrgUnitId").await?;
    if t.kind == StudentTransferKind::InterInstitute {
        let programme = t.to_programme_id.ok_or_else(|| {
            super::support::invalid(
                "toProgrammeId",
                "required",
                "an inter-institute transfer needs the destination programme",
            )
        })?;
        refs::programme(st, &scope, dest, programme, "toProgrammeId").await?;
        let today = business_today(st, &scope, dest).await?;
        refs::curriculum(
            st,
            &scope,
            programme,
            t.to_curriculum_version_id,
            today,
            "toCurriculumVersionId",
        )
        .await?;
    }
    Ok(())
}

/// Request a transfer (`student.manage` for intra, `student.lifecycle` + step-up for inter):
/// `requested`.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/transfers",
    operation_id = "student_transfer_create",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewTransfer,
    responses((status = 201, body = StudentTransfer, headers(("ETag" = String))), CreateErrors)
)]
pub async fn create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<NewTransfer>,
) -> ApiResult<Response> {
    let ctx = auth.require(permission(b.kind))?;
    require_idempotency_key(&headers)?;
    let mut v = vec![];
    validate_text("reason", &b.reason, 1000, &mut v);
    validate_code("toBatchCode", b.to_batch_code.as_deref(), &mut v);
    validate_code("toSectionCode", b.to_section_code.as_deref(), &mut v);
    finish(v)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    require_student_writable(&s)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let src = super::common::pick_context(&mut conn, &s, b.from_context_id).await?;
    drop(conn);
    if src.status != StudentContextStatus::Active {
        return Err(KernelError::Conflict(format!(
            "student context is {}; only an active student is transferred",
            src.status
        ))
        .into());
    }
    let to_inst = match b.kind {
        StudentTransferKind::IntraInstitute => {
            if b.to_institution_id.is_some_and(|i| i != src.institution_id) {
                return Err(super::support::invalid(
                    "toInstitutionId",
                    "other-institution",
                    "an intra-institute transfer stays in the same institution",
                )
                .into());
            }
            if b.to_programme_id
                .is_some_and(|p| Some(p) != src.programme_id)
            {
                return Err(super::support::invalid(
                    "toProgrammeId",
                    "programme-migration-required",
                    "a programme change is a programme migration, not a transfer",
                )
                .into());
            }
            src.institution_id
        }
        StudentTransferKind::InterInstitute => {
            let to = b.to_institution_id.ok_or_else(|| {
                super::support::invalid(
                    "toInstitutionId",
                    "required",
                    "destination institution required",
                )
            })?;
            if to == src.institution_id {
                return Err(super::support::invalid(
                    "toInstitutionId",
                    "same-institution",
                    "use an intra-institute transfer inside the same institution",
                )
                .into());
            }
            if live_context(&st.pool, s.tenant_id, s.id, to)
                .await?
                .is_some()
            {
                return Err(KernelError::Conflict(
                    "the student already has a live context at the destination".into(),
                )
                .into());
            }
            to
        }
    };
    let scope = institution_scope(ctx, src.institution_id);
    require_institution_writable(&st, &scope, src.institution_id).await?;
    let today = business_today(&st, &scope, src.institution_id).await?;
    let draft = StudentTransfer {
        id: StudentTransferId::new(),
        student_id: id,
        kind: b.kind,
        from_context_id: src.id,
        to_context_id: None,
        from_institution_id: src.institution_id,
        to_institution_id: to_inst,
        to_campus_id: b.to_campus_id,
        to_org_unit_id: b.to_org_unit_id,
        to_programme_id: match b.kind {
            StudentTransferKind::IntraInstitute => src.programme_id,
            StudentTransferKind::InterInstitute => b.to_programme_id,
        },
        to_curriculum_version_id: b.to_curriculum_version_id,
        to_batch_code: norm(b.to_batch_code.as_deref()),
        to_section_code: norm(b.to_section_code.as_deref()),
        effective_on: b.effective_on.unwrap_or(today),
        reason: b.reason.trim().to_owned(),
        status: CaseStatus::Requested,
        decision_reason: None,
        process_instance_id: None,
        version: Version(1),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };
    validate_destination(&st, |i| institution_scope(ctx, i), &draft).await?;
    if draft.kind == StudentTransferKind::IntraInstitute
        && draft.to_campus_id.is_none()
        && draft.to_org_unit_id.is_none()
        && draft.to_batch_code.is_none()
        && draft.to_section_code.is_none()
    {
        return Err(super::support::invalid(
            "toCampusId",
            "required",
            "name the destination placement",
        )
        .into());
    }
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.student_transfer (tenant_id, student_id, kind, from_context_id, \
         from_institution_id, to_institution_id, to_campus_id, to_org_unit_id, to_programme_id, \
         to_curriculum_version_id, to_batch_code, to_section_code, effective_on, reason) \
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) RETURNING {COLUMNS}"
    );
    let r = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .bind(draft.kind.as_str())
        .bind(draft.from_context_id.as_uuid())
        .bind(draft.from_institution_id.as_uuid())
        .bind(draft.to_institution_id.as_uuid())
        .bind(draft.to_campus_id)
        .bind(draft.to_org_unit_id)
        .bind(draft.to_programme_id)
        .bind(draft.to_curriculum_version_id)
        .bind(&draft.to_batch_code)
        .bind(&draft.to_section_code)
        .bind(draft.effective_on)
        .bind(&draft.reason)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("student_transfer_one_open") => {
                KernelError::Conflict("an open transfer already exists for this context".into())
            }
            _ => map_db_error(e),
        })?;
    let t = from_row(&r).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student_transfer.create",
        "student_transfer",
        t.id.to_string(),
        serde_json::json!({ "kind": t.kind, "toInstitutionId": t.to_institution_id }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &t, t.version))
}

/// A student's transfers (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/transfers",
    operation_id = "student_transfer_list",
    tag = "students",
    params(("id" = String, Path, format = Uuid), CaseListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = StudentTransferList), ReadErrors)
)]
pub async fn list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<CaseListQuery>,
) -> ApiResult<Json<StudentTransferList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    load_student(&st.pool, tenant(ctx), id).await?;
    let sql = format!(
        "SELECT {COLUMNS} FROM student.student_transfer WHERE student_id = $1 AND tenant_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant(ctx).as_uuid())
        .bind(q.status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    Ok(Json(StudentTransferList {
        items: rows
            .iter()
            .map(from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)?,
    }))
}

/// One transfer (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/transfers/{id}",
    operation_id = "student_transfer_get",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = StudentTransfer, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentTransferId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let t = load(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    Ok(with_etag(StatusCode::OK, &t, t.version))
}

/// Applies an approved transfer (same transaction as the approval).
async fn apply(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: &StudentTransfer,
    curriculum_version_id: Option<Uuid>,
    actor: &Actor,
) -> Result<StudentTransfer, KernelError> {
    let src = lock_context(conn, scope.tenant_id, t.from_context_id).await?;
    if src.status != StudentContextStatus::Active {
        return Err(KernelError::Conflict(format!(
            "student context is {}; only an active student is transferred",
            src.status
        )));
    }
    let to_ctx: StudentContextRow = match t.kind {
        StudentTransferKind::IntraInstitute => {
            let mut p = Placement::from(&src);
            if t.to_campus_id.is_some() {
                p.campus_id = t.to_campus_id;
            }
            if t.to_org_unit_id.is_some() {
                p.org_unit_id = t.to_org_unit_id;
            }
            if t.to_batch_code.is_some() {
                p.batch_code = t.to_batch_code.clone();
            }
            if t.to_section_code.is_some() {
                p.section_code = t.to_section_code.clone();
            }
            update_placement(conn, &src, &p, "transfer", Some(&t.reason), actor).await?
        }
        StudentTransferKind::InterInstitute => {
            let change = StatusChange {
                cause_event_type: STUDENT_TRANSFERRED,
                reason: Some(&t.reason),
                process_instance_id: t.process_instance_id.as_deref(),
                closed_on: Some(t.effective_on),
            };
            let closed = change_context_status(
                conn,
                &src,
                StudentContextStatus::Transferred,
                &change,
                actor,
            )
            .await?;
            let sql = format!(
                "INSERT INTO student.student_context (tenant_id, student_id, institution_id, \
                 campus_id, programme_id, curriculum_version_id, org_unit_id, admission_term_id, \
                 admission_year_id, batch_code, section_code, current_term_no, enrolment_type, \
                 predecessor_context_id, status) \
                 VALUES ($1,$2,$3,$4,$5,$6,$7,NULL,NULL,$8,$9,$10,'transfer_in',$11,'pending_activation') \
                 RETURNING {CONTEXT_COLUMNS}"
            );
            let row = sqlx::query(&sql)
                .bind(scope.tenant_id.as_uuid())
                .bind(t.student_id.as_uuid())
                .bind(t.to_institution_id.as_uuid())
                .bind(t.to_campus_id)
                .bind(t.to_programme_id)
                .bind(curriculum_version_id)
                .bind(t.to_org_unit_id)
                .bind(&t.to_batch_code)
                .bind(&t.to_section_code)
                .bind(closed.current_term_no)
                .bind(closed.id.as_uuid())
                .fetch_one(&mut *conn)
                .await
                .map_err(|e| match &e {
                    sqlx::Error::Database(d)
                        if d.constraint() == Some("student_context_one_live") =>
                    {
                        KernelError::Conflict(
                            "the student already has a live context at the destination".into(),
                        )
                    }
                    _ => map_db_error(e),
                })?;
            let next = context_from_row(&row).map_err(map_db_error)?;
            super::support::record_history(
                conn,
                scope.tenant_id,
                t.student_id.as_uuid(),
                Some(next.id.as_uuid()),
                "transfer",
                serde_json::json!({ "institutionId": src.institution_id, "contextId": src.id, "programmeId": src.programme_id }),
                serde_json::json!({ "institutionId": next.institution_id, "contextId": next.id, "programmeId": next.programme_id }),
                Some(&t.reason),
                actor,
            )
            .await?;
            next
        }
    };
    let dest_scope = ExecutionScope::institution(scope.tenant_id, t.to_institution_id);
    publish::event(
        conn,
        &dest_scope,
        STUDENT_TRANSFERRED,
        STUDENT_TRANSFERRED_SCHEMA,
        t.student_id.to_string(),
        &StudentTransferredV1 {
            transfer_id: t.id.as_uuid(),
            student_id: t.student_id.as_uuid(),
            kind: t.kind.as_str().to_owned(),
            from_context_id: src.id.as_uuid(),
            to_context_id: to_ctx.id.as_uuid(),
            from_institution_id: src.institution_id.as_uuid(),
            to_institution_id: t.to_institution_id.as_uuid(),
            effective_on: t.effective_on,
            process_instance_id: t.process_instance_id.clone(),
        },
        actor,
    )
    .await?;
    let done = set_status(conn, t, CaseStatus::Completed, None, None, Some(to_ctx.id)).await?;
    publish::audit(
        conn,
        scope,
        "student.student_transfer.apply",
        "student_transfer",
        done.id.to_string(),
        serde_json::json!({ "toContextId": to_ctx.id }),
        actor,
    )
    .await?;
    Ok(done)
}

/// Resolves the destination curriculum version for an inter transfer.
async fn destination_curriculum(
    st: &StudentState,
    ctx: &AuthContext,
    t: &StudentTransfer,
) -> Result<Option<Uuid>, KernelError> {
    match (t.kind, t.to_programme_id) {
        (StudentTransferKind::InterInstitute, Some(p)) => {
            let scope = institution_scope(ctx, t.to_institution_id);
            let today = business_today(st, &scope, t.to_institution_id).await?;
            Ok(Some(
                refs::curriculum(
                    st,
                    &scope,
                    p,
                    t.to_curriculum_version_id,
                    today,
                    "toCurriculumVersionId",
                )
                .await?
                .id,
            ))
        }
        _ => Ok(None),
    }
}

/// Submit a transfer for approval (`student.manage` / `student.lifecycle` per kind): the
/// `student.student_transfer` process starts; auto-approval applies it at once.
#[utoipa::path(
    post,
    path = "/api/v1/student/transfers/{id}/submit",
    operation_id = "student_transfer_submit",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Transfer version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StudentTransfer, headers(("ETag" = String))), CommandErrors)
)]
pub async fn submit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentTransferId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx0 = auth.require(Permission::StudentRead)?;
    let expected = expect_version!(headers);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, tenant(ctx0).as_uuid(), id, false).await?;
    drop(conn);
    let ctx = auth.require(permission(cur.kind))?;
    check_version(expected, cur.version.0)?;
    require_case(cur.status, CaseStatus::UnderReview, "transfer")?;
    validate_destination(&st, |i| institution_scope(ctx, i), &cur).await?;
    let cv = destination_curriculum(&st, ctx, &cur).await?;
    let scope = institution_scope(ctx, cur.from_institution_id);
    require_institution_writable(&st, &scope, cur.from_institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("studentId".to_owned(), serde_json::json!(cur.student_id));
    vars.insert("kind".to_owned(), serde_json::json!(cur.kind));
    let outcome = request_approval(
        &st,
        &scope,
        process::STUDENT_TRANSFER,
        business_key("student-transfer", cur.id, cur.version.0),
        vars,
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let mut t = set_status(
        &mut tx,
        &cur,
        CaseStatus::UnderReview,
        None,
        Some(outcome.process_instance_id()),
        None,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(a.correlation_id.clone());
        t = set_status(&mut tx, &t, CaseStatus::Approved, None, None, None).await?;
        t = apply(&mut tx, &scope, &t, cv, &sys).await?;
    }
    publish::audit(
        &mut tx,
        &scope,
        "student.student_transfer.submit",
        "student_transfer",
        t.id.to_string(),
        serde_json::json!({ "status": t.status }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &t, t.version))
}

/// Decide a transfer under review (permission per kind).
#[utoipa::path(
    post,
    path = "/api/v1/student/transfers/{id}/decision",
    operation_id = "student_transfer_decide",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Transfer version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses((status = 200, body = StudentTransfer, headers(("ETag" = String))), CommandErrors)
)]
pub async fn decide(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentTransferId>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx0 = auth.require(Permission::StudentRead)?;
    let expected = expect_version!(headers);
    let reason = b.checked_reason()?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, tenant(ctx0).as_uuid(), id, false).await?;
    drop(conn);
    let ctx = auth.require(permission(cur.kind))?;
    let cv = if b.decision == Decision::Approve {
        validate_destination(&st, |i| institution_scope(ctx, i), &cur).await?;
        destination_curriculum(&st, ctx, &cur).await?
    } else {
        None
    };
    let scope = institution_scope(ctx, cur.from_institution_id);
    require_institution_writable(&st, &scope, cur.from_institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let t = match b.decision {
        Decision::Approve => {
            let t = set_status(
                &mut tx,
                &cur,
                CaseStatus::Approved,
                reason.as_deref(),
                None,
                None,
            )
            .await?;
            apply(&mut tx, &scope, &t, cv, &a).await?
        }
        Decision::Reject => {
            set_status(
                &mut tx,
                &cur,
                CaseStatus::Rejected,
                reason.as_deref(),
                None,
                None,
            )
            .await?
        }
    };
    publish::audit(
        &mut tx,
        &scope,
        "student.student_transfer.decide",
        "student_transfer",
        t.id.to_string(),
        serde_json::json!({ "decision": b.decision, "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &t, t.version))
}

/// Withdraw an open transfer (permission per kind).
#[utoipa::path(
    post,
    path = "/api/v1/student/transfers/{id}/withdraw",
    operation_id = "student_transfer_withdraw",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Transfer version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = StudentTransfer, headers(("ETag" = String))), CommandErrors)
)]
pub async fn withdraw(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentTransferId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx0 = auth.require(Permission::StudentRead)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, tenant(ctx0).as_uuid(), id, false).await?;
    drop(conn);
    let ctx = auth.require(permission(cur.kind))?;
    let scope = institution_scope(ctx, cur.from_institution_id);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let t = set_status(
        &mut tx,
        &cur,
        CaseStatus::Withdrawn,
        reason.as_deref(),
        None,
        None,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student_transfer.withdraw",
        "student_transfer",
        t.id.to_string(),
        serde_json::json!({ "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &t, t.version))
}
