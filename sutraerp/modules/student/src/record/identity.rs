//! Governed identity corrections (L3-C-016) and duplicate resolution (L3-C-017).
//! Both are `CaseStatus` cases: `requested → under_review` (approval through the WorkflowPort)
//! `→ approved → completed` (applied in the approving transaction) or `rejected`; `withdrawn`
//! while open.

use super::common::home_institution;
use super::support::{
    check_version, expect_version, finish, invalid, modified, norm, record_history,
    validate_opt_text, validate_text, CommandBody, CommandErrors, CreateErrors, Decision,
    DecisionBody, ReadErrors,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution_writable, require_student_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_student_status, StatusChange};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::student::{
    contexts_of, load_student, lock_student, student_from_row, STUDENT_COLUMNS,
};
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
    StudentDuplicateResolvedV1, STUDENT_DUPLICATE_RESOLVED, STUDENT_DUPLICATE_RESOLVED_SCHEMA,
    STUDENT_IDENTITY_CORRECTED, STUDENT_IDENTITY_CORRECTED_SCHEMA,
};
use sutra_contracts::student::ids::{DuplicateCaseId, IdentityCorrectionId, StudentId};
use sutra_contracts::student::status::{CaseStatus, StudentStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

/// Legal transitions of a governed 3A case (identity correction, duplicate case, transfer).
pub fn case_can_transition(from: CaseStatus, to: CaseStatus) -> bool {
    use CaseStatus::*;
    matches!(
        (from, to),
        (Requested, UnderReview)
            | (UnderReview, Approved | Rejected)
            | (Approved, Completed)
            | (Requested | UnderReview, Withdrawn)
    )
}

/// 409 unless `from → to` is legal.
pub fn require_case(from: CaseStatus, to: CaseStatus, what: &str) -> Result<(), KernelError> {
    if case_can_transition(from, to) {
        Ok(())
    } else {
        Err(KernelError::Conflict(format!(
            "{what} is {from}; it cannot become {to}"
        )))
    }
}

fn parse_case(s: &str) -> Result<CaseStatus, sqlx::Error> {
    CaseStatus::parse(s).ok_or_else(|| sqlx::Error::Decode(format!("case status {s}").into()))
}

/// Case filter.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct CaseListQuery {
    pub status: Option<CaseStatus>,
}

// ---------------------------------------------------------------------------------------------
// Identity correction
// ---------------------------------------------------------------------------------------------

/// Identity attributes a correction may set (student number and ids are immutable).
#[derive(
    Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityChanges {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub given_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub middle_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_of_birth: Option<NaiveDate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gender: Option<String>,
}

impl IdentityChanges {
    fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// An identity correction case.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCorrection {
    pub id: IdentityCorrectionId,
    pub student_id: StudentId,
    pub institution_id: InstitutionId,
    pub changes: IdentityChanges,
    /// Values before the correction was applied.
    pub previous: Option<IdentityChanges>,
    pub evidence: String,
    pub reason: Option<String>,
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
pub struct IdentityCorrectionList {
    pub items: Vec<IdentityCorrection>,
}

const IC_COLUMNS: &str = "id, student_id, institution_id, changes, previous, evidence, reason, \
    status, decision_reason, process_instance_id, version, created_at, updated_at";

fn ic_from_row(r: &PgRow) -> Result<IdentityCorrection, sqlx::Error> {
    let changes: serde_json::Value = r.try_get("changes")?;
    let previous: Option<serde_json::Value> = r.try_get("previous")?;
    Ok(IdentityCorrection {
        id: IdentityCorrectionId(r.try_get("id")?),
        student_id: StudentId(r.try_get("student_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        changes: serde_json::from_value(changes).map_err(|e| sqlx::Error::Decode(e.into()))?,
        previous: previous
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| sqlx::Error::Decode(e.into()))?,
        evidence: r.try_get("evidence")?,
        reason: r.try_get("reason")?,
        status: parse_case(&r.try_get::<String, _>("status")?)?,
        decision_reason: r.try_get("decision_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn load_ic(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: IdentityCorrectionId,
    lock: bool,
) -> Result<IdentityCorrection, KernelError> {
    let sql = format!(
        "SELECT {IC_COLUMNS} FROM student.identity_correction WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "identity correction",
        })?;
    ic_from_row(&r).map_err(map_db_error)
}

async fn set_ic_status(
    conn: &mut PgConnection,
    cur: &IdentityCorrection,
    to: CaseStatus,
    decision_reason: Option<&str>,
    pid: Option<&str>,
) -> Result<IdentityCorrection, KernelError> {
    require_case(cur.status, to, "identity correction")?;
    let sql = format!(
        "UPDATE student.identity_correction SET status = $2, \
         decision_reason = COALESCE($3, decision_reason), \
         process_instance_id = COALESCE($4, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {IC_COLUMNS}"
    );
    let r = sqlx::query(&sql)
        .bind(cur.id.as_uuid())
        .bind(to.as_str())
        .bind(decision_reason)
        .bind(pid)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    ic_from_row(&r).map_err(map_db_error)
}

/// Body of a new identity correction.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewIdentityCorrection {
    pub changes: IdentityChanges,
    /// Supporting evidence (e.g. "birth certificate no. …").
    pub evidence: String,
    pub reason: Option<String>,
}

fn validate_changes(c: &IdentityChanges) -> Result<(), KernelError> {
    if c.is_empty() {
        return Err(invalid("changes", "required", "name what to correct"));
    }
    let mut v = vec![];
    if let Some(g) = &c.given_name {
        validate_text("changes.givenName", g, 100, &mut v);
    }
    validate_opt_text("changes.middleName", c.middle_name.as_deref(), 100, &mut v);
    validate_opt_text("changes.familyName", c.family_name.as_deref(), 100, &mut v);
    validate_opt_text("changes.gender", c.gender.as_deref(), 40, &mut v);
    finish(v)
}

/// Raise an identity correction (`student.identity.manage`, **step-up**): `requested`.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/identity-corrections",
    operation_id = "identity_correction_create",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewIdentityCorrection,
    responses((status = 201, body = IdentityCorrection, headers(("ETag" = String))), CreateErrors)
)]
pub async fn create_correction(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<NewIdentityCorrection>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    require_idempotency_key(&headers)?;
    validate_changes(&b.changes)?;
    let mut v = vec![];
    validate_text("evidence", &b.evidence, 2000, &mut v);
    validate_opt_text("reason", b.reason.as_deref(), 1000, &mut v);
    finish(v)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    require_student_writable(&s)?;
    let inst = home_institution(&st.pool, &s).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.identity_correction (tenant_id, student_id, institution_id, changes, \
         evidence, reason) VALUES ($1, $2, $3, $4, $5, $6) RETURNING {IC_COLUMNS}"
    );
    let r = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(id.as_uuid())
        .bind(inst.as_uuid())
        .bind(serde_json::to_value(&b.changes).map_err(KernelError::internal)?)
        .bind(b.evidence.trim())
        .bind(norm(b.reason.as_deref()))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let ic = ic_from_row(&r).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.identity_correction.create",
        "identity_correction",
        ic.id.to_string(),
        serde_json::json!({ "studentId": id }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &ic, ic.version))
}

/// A student's identity corrections (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/identity-corrections",
    operation_id = "identity_correction_list",
    tag = "students",
    params(("id" = String, Path, format = Uuid), CaseListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = IdentityCorrectionList), ReadErrors)
)]
pub async fn list_corrections(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<CaseListQuery>,
) -> ApiResult<Json<IdentityCorrectionList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    load_student(&st.pool, tenant(ctx), id).await?;
    let sql = format!(
        "SELECT {IC_COLUMNS} FROM student.identity_correction WHERE student_id = $1 \
         AND tenant_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant(ctx).as_uuid())
        .bind(q.status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    Ok(Json(IdentityCorrectionList {
        items: rows
            .iter()
            .map(ic_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)?,
    }))
}

/// One identity correction (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/identity-corrections/{id}",
    operation_id = "identity_correction_get",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = IdentityCorrection, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get_correction(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<IdentityCorrectionId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let ic = load_ic(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    Ok(with_etag(StatusCode::OK, &ic, ic.version))
}

/// Applies an approved correction to the persona (no new identity; student number unchanged).
async fn apply_correction(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    ic: &IdentityCorrection,
    actor: &Actor,
) -> Result<IdentityCorrection, KernelError> {
    let s = lock_student(conn, scope.tenant_id, ic.student_id).await?;
    require_student_writable(&s)?;
    let previous = IdentityChanges {
        given_name: ic.changes.given_name.as_ref().map(|_| s.given_name.clone()),
        middle_name: ic
            .changes
            .middle_name
            .as_ref()
            .map(|_| s.middle_name.clone().unwrap_or_default()),
        family_name: ic
            .changes
            .family_name
            .as_ref()
            .map(|_| s.family_name.clone().unwrap_or_default()),
        date_of_birth: ic.changes.date_of_birth.and(s.date_of_birth),
        gender: ic
            .changes
            .gender
            .as_ref()
            .map(|_| s.gender.clone().unwrap_or_default()),
    };
    let c = &ic.changes;
    let sql = format!(
        "UPDATE student.student SET given_name = COALESCE($3, given_name), \
         middle_name = CASE WHEN $4::text IS NULL THEN middle_name ELSE NULLIF($4, '') END, \
         family_name = CASE WHEN $5::text IS NULL THEN family_name ELSE NULLIF($5, '') END, \
         date_of_birth = COALESCE($6, date_of_birth), \
         gender = CASE WHEN $7::text IS NULL THEN gender ELSE NULLIF($7, '') END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 \
         RETURNING {STUDENT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(s.id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(c.given_name.as_deref().map(str::trim))
        .bind(c.middle_name.as_deref().map(str::trim))
        .bind(c.family_name.as_deref().map(str::trim))
        .bind(c.date_of_birth)
        .bind(c.gender.as_deref().map(str::trim))
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let s2 = student_from_row(&row).map_err(map_db_error)?;
    let prev_json = serde_json::to_value(&previous).map_err(KernelError::internal)?;
    let changes_json = serde_json::to_value(c).map_err(KernelError::internal)?;
    record_history(
        conn,
        scope.tenant_id,
        s.id.as_uuid(),
        None,
        "identity",
        prev_json.clone(),
        changes_json.clone(),
        ic.reason.as_deref(),
        actor,
    )
    .await?;
    sqlx::query("UPDATE student.identity_correction SET previous = $2 WHERE id = $1")
        .bind(ic.id.as_uuid())
        .bind(prev_json)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let changed: Vec<String> = changes_json
        .as_object()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    publish::event(
        conn,
        scope,
        STUDENT_IDENTITY_CORRECTED,
        STUDENT_IDENTITY_CORRECTED_SCHEMA,
        s.id.to_string(),
        &modified(
            s.id.as_uuid(),
            ic.institution_id,
            Some(s.id.as_uuid()),
            s2.version.0,
            changed,
            s2.subject_ref.clone(),
        ),
        actor,
    )
    .await?;
    let ic = set_ic_status(conn, ic, CaseStatus::Completed, None, None).await?;
    publish::audit(
        conn,
        scope,
        "student.identity_correction.apply",
        "identity_correction",
        ic.id.to_string(),
        serde_json::json!({ "studentId": s.id }),
        actor,
    )
    .await?;
    Ok(ic)
}

/// Submit a correction for approval (`student.identity.manage`, **step-up**): the
/// `student.identity_correction` process starts; auto-approval applies it at once
/// (`completed`, `student.identity_corrected`), `AwaitDecision` leaves it `under_review`.
#[utoipa::path(
    post,
    path = "/api/v1/student/identity-corrections/{id}/submit",
    operation_id = "identity_correction_submit",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Case version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = IdentityCorrection, headers(("ETag" = String))), CommandErrors)
)]
pub async fn submit_correction(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<IdentityCorrectionId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    let expected = expect_version!(headers);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load_ic(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    drop(conn);
    check_version(expected, cur.version.0)?;
    require_case(cur.status, CaseStatus::UnderReview, "identity correction")?;
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("studentId".to_owned(), serde_json::json!(cur.student_id));
    let outcome = request_approval(
        &st,
        &scope,
        process::IDENTITY_CORRECTION,
        business_key("identity-correction", cur.id, cur.version.0),
        vars,
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_ic(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let mut ic = set_ic_status(
        &mut tx,
        &cur,
        CaseStatus::UnderReview,
        None,
        Some(outcome.process_instance_id()),
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(a.correlation_id.clone());
        ic = set_ic_status(&mut tx, &ic, CaseStatus::Approved, None, None).await?;
        ic = apply_correction(&mut tx, &scope, &ic, &sys).await?;
    }
    publish::audit(
        &mut tx,
        &scope,
        "student.identity_correction.submit",
        "identity_correction",
        ic.id.to_string(),
        serde_json::json!({ "status": ic.status }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &ic, ic.version))
}

/// Decide a correction under review (`student.identity.manage`, **step-up**).
#[utoipa::path(
    post,
    path = "/api/v1/student/identity-corrections/{id}/decision",
    operation_id = "identity_correction_decide",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Case version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses((status = 200, body = IdentityCorrection, headers(("ETag" = String))), CommandErrors)
)]
pub async fn decide_correction(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<IdentityCorrectionId>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "identity correction").await?;
    let expected = expect_version!(headers);
    let reason = b.checked_reason()?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load_ic(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_ic(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let ic = match b.decision {
        Decision::Approve => {
            let ic =
                set_ic_status(&mut tx, &cur, CaseStatus::Approved, reason.as_deref(), None).await?;
            apply_correction(&mut tx, &scope, &ic, &a).await?
        }
        Decision::Reject => {
            set_ic_status(&mut tx, &cur, CaseStatus::Rejected, reason.as_deref(), None).await?
        }
    };
    publish::audit(
        &mut tx,
        &scope,
        "student.identity_correction.decide",
        "identity_correction",
        ic.id.to_string(),
        serde_json::json!({ "decision": b.decision, "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &ic, ic.version))
}

/// Withdraw an open correction (`student.identity.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/identity-corrections/{id}/withdraw",
    operation_id = "identity_correction_withdraw",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Case version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = IdentityCorrection, headers(("ETag" = String))), CommandErrors)
)]
pub async fn withdraw_correction(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<IdentityCorrectionId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load_ic(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_ic(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let ic = set_ic_status(
        &mut tx,
        &cur,
        CaseStatus::Withdrawn,
        reason.as_deref(),
        None,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.identity_correction.withdraw",
        "identity_correction",
        ic.id.to_string(),
        serde_json::json!({ "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &ic, ic.version))
}

// ---------------------------------------------------------------------------------------------
// Duplicate resolution
// ---------------------------------------------------------------------------------------------

/// A duplicate case: `duplicateStudentId` is merged into `survivorStudentId`.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DuplicateCase {
    pub id: DuplicateCaseId,
    pub survivor_student_id: StudentId,
    pub duplicate_student_id: StudentId,
    pub institution_id: InstitutionId,
    pub evidence: String,
    pub status: CaseStatus,
    pub decision_reason: Option<String>,
    pub process_instance_id: Option<String>,
    /// Live contexts of the duplicate re-pointed to the survivor on completion.
    pub moved_context_ids: Vec<Uuid>,
    #[schema(value_type = i64)]
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct DuplicateCaseList {
    pub items: Vec<DuplicateCase>,
}

const DC_COLUMNS: &str = "id, survivor_student_id, duplicate_student_id, institution_id, evidence, \
    status, decision_reason, process_instance_id, moved_context_ids, version, created_at, updated_at";

fn dc_from_row(r: &PgRow) -> Result<DuplicateCase, sqlx::Error> {
    Ok(DuplicateCase {
        id: DuplicateCaseId(r.try_get("id")?),
        survivor_student_id: StudentId(r.try_get("survivor_student_id")?),
        duplicate_student_id: StudentId(r.try_get("duplicate_student_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        evidence: r.try_get("evidence")?,
        status: parse_case(&r.try_get::<String, _>("status")?)?,
        decision_reason: r.try_get("decision_reason")?,
        process_instance_id: r.try_get("process_instance_id")?,
        moved_context_ids: r.try_get("moved_context_ids")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn load_dc(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: DuplicateCaseId,
    lock: bool,
) -> Result<DuplicateCase, KernelError> {
    let sql = format!(
        "SELECT {DC_COLUMNS} FROM student.duplicate_case WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let r = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "duplicate case",
        })?;
    dc_from_row(&r).map_err(map_db_error)
}

async fn set_dc_status(
    conn: &mut PgConnection,
    cur: &DuplicateCase,
    to: CaseStatus,
    decision_reason: Option<&str>,
    pid: Option<&str>,
) -> Result<DuplicateCase, KernelError> {
    require_case(cur.status, to, "duplicate case")?;
    let sql = format!(
        "UPDATE student.duplicate_case SET status = $2, \
         decision_reason = COALESCE($3, decision_reason), \
         process_instance_id = COALESCE($4, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 RETURNING {DC_COLUMNS}"
    );
    let r = sqlx::query(&sql)
        .bind(cur.id.as_uuid())
        .bind(to.as_str())
        .bind(decision_reason)
        .bind(pid)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    dc_from_row(&r).map_err(map_db_error)
}

/// Body of a new duplicate case.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewDuplicateCase {
    pub survivor_student_id: StudentId,
    pub duplicate_student_id: StudentId,
    pub evidence: String,
}

/// Open a duplicate case (`student.identity.manage`, **step-up**).
#[utoipa::path(
    post,
    path = "/api/v1/student/duplicate-cases",
    operation_id = "duplicate_case_create",
    tag = "students",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = NewDuplicateCase,
    responses((status = 201, body = DuplicateCase, headers(("ETag" = String))), CreateErrors)
)]
pub async fn create_duplicate(
    State(st): State<StudentState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<NewDuplicateCase>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    require_idempotency_key(&headers)?;
    let mut v = vec![];
    validate_text("evidence", &b.evidence, 2000, &mut v);
    finish(v)?;
    if b.survivor_student_id == b.duplicate_student_id {
        return Err(invalid(
            "duplicateStudentId",
            "same",
            "a student cannot duplicate itself",
        )
        .into());
    }
    let survivor = load_student(&st.pool, tenant(ctx), b.survivor_student_id).await?;
    let dup = load_student(&st.pool, tenant(ctx), b.duplicate_student_id).await?;
    require_student_writable(&survivor)?;
    require_student_writable(&dup)?;
    let inst = home_institution(&st.pool, &survivor).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.duplicate_case (tenant_id, survivor_student_id, duplicate_student_id, \
         institution_id, evidence) VALUES ($1, $2, $3, $4, $5) RETURNING {DC_COLUMNS}"
    );
    let r = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(survivor.id.as_uuid())
        .bind(dup.id.as_uuid())
        .bind(inst.as_uuid())
        .bind(b.evidence.trim())
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("duplicate_case_open_pair") => {
                KernelError::Conflict("an open case already covers these two students".into())
            }
            _ => map_db_error(e),
        })?;
    let dc = dc_from_row(&r).map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        "student.duplicate_case.create",
        "duplicate_case",
        dc.id.to_string(),
        serde_json::json!({ "survivor": survivor.id, "duplicate": dup.id }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &dc, dc.version))
}

/// Duplicate cases of the tenant (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/duplicate-cases",
    operation_id = "duplicate_case_list",
    tag = "students",
    params(CaseListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = DuplicateCaseList), ReadErrors)
)]
pub async fn list_duplicates(
    State(st): State<StudentState>,
    auth: Authenticated,
    Query(q): Query<CaseListQuery>,
) -> ApiResult<Json<DuplicateCaseList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let sql = format!(
        "SELECT {DC_COLUMNS} FROM student.duplicate_case WHERE tenant_id = $1 \
         AND ($2::text IS NULL OR status = $2) ORDER BY created_at DESC, id DESC LIMIT 500"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant(ctx).as_uuid())
        .bind(q.status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    Ok(Json(DuplicateCaseList {
        items: rows
            .iter()
            .map(dc_from_row)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)?,
    }))
}

/// One duplicate case (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/duplicate-cases/{id}",
    operation_id = "duplicate_case_get",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = DuplicateCase, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get_duplicate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DuplicateCaseId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let dc = load_dc(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    Ok(with_etag(StatusCode::OK, &dc, dc.version))
}

/// Merges an approved case: live contexts of the duplicate move to the survivor (409 when the
/// survivor already has a live context in the same institution), its documents and requests
/// follow, the login link moves when the survivor has none, and the duplicate persona is
/// archived pointing at the survivor (`student.duplicate_resolved`). Terminal contexts stay on
/// the duplicate as lineage; no Person merge is performed.
async fn apply_merge(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    dc: &DuplicateCase,
    actor: &Actor,
) -> Result<DuplicateCase, KernelError> {
    // Lock both personas in id order (deadlock-free).
    let (first, second) = if dc.survivor_student_id.as_uuid() < dc.duplicate_student_id.as_uuid() {
        (dc.survivor_student_id, dc.duplicate_student_id)
    } else {
        (dc.duplicate_student_id, dc.survivor_student_id)
    };
    lock_student(conn, scope.tenant_id, first).await?;
    lock_student(conn, scope.tenant_id, second).await?;
    let survivor = lock_student(conn, scope.tenant_id, dc.survivor_student_id).await?;
    let dup = lock_student(conn, scope.tenant_id, dc.duplicate_student_id).await?;
    require_student_writable(&survivor)?;
    require_student_writable(&dup)?;
    let survivor_ctx = contexts_of(&mut *conn, scope.tenant_id, survivor.id).await?;
    let dup_ctx = contexts_of(&mut *conn, scope.tenant_id, dup.id).await?;
    let mut moved = vec![];
    for c in dup_ctx.iter().filter(|c| c.status.is_live()) {
        if survivor_ctx
            .iter()
            .any(|s| s.status.is_live() && s.institution_id == c.institution_id)
        {
            return Err(KernelError::Conflict(
                "both students have a live context in the same institution; close one first".into(),
            ));
        }
        sqlx::query(
            "UPDATE student.student_context SET student_id = $2, version = version + 1, \
             updated_at = now() WHERE id = $1",
        )
        .bind(c.id.as_uuid())
        .bind(survivor.id.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        moved.push(c.id.as_uuid());
    }
    for table in ["student_document", "document_request"] {
        sqlx::query(&format!(
            "UPDATE student.{table} SET student_id = $2 WHERE student_id = $1 AND tenant_id = $3"
        ))
        .bind(dup.id.as_uuid())
        .bind(survivor.id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let subject = dup.subject_ref.clone();
    sqlx::query(
        "UPDATE student.student SET merged_into_id = $2, subject_ref = NULL, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(dup.id.as_uuid())
    .bind(survivor.id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if survivor.subject_ref.is_none() && subject.is_some() {
        sqlx::query("UPDATE student.student SET subject_ref = $2, version = version + 1, updated_at = now() WHERE id = $1")
            .bind(survivor.id.as_uuid())
            .bind(&subject)
            .execute(&mut *conn)
            .await
            .map_err(map_db_error)?;
    }
    let dup = lock_student(conn, scope.tenant_id, dup.id).await?;
    let inst = dc.institution_id;
    change_student_status(
        conn,
        &dup,
        inst,
        StudentStatus::Archived,
        &StatusChange {
            cause_event_type: STUDENT_DUPLICATE_RESOLVED,
            reason: Some("merged into the surviving student"),
            process_instance_id: dc.process_instance_id.as_deref(),
            closed_on: None,
        },
        actor,
    )
    .await?;
    record_history(
        conn,
        scope.tenant_id,
        survivor.id.as_uuid(),
        None,
        "merge",
        serde_json::json!({ "duplicateStudentId": dup.id, "duplicateStudentNumber": dup.student_number }),
        serde_json::json!({ "movedContextIds": moved }),
        dc.decision_reason.as_deref(),
        actor,
    )
    .await?;
    sqlx::query("UPDATE student.duplicate_case SET moved_context_ids = $2 WHERE id = $1")
        .bind(dc.id.as_uuid())
        .bind(&moved)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    publish::event(
        conn,
        scope,
        STUDENT_DUPLICATE_RESOLVED,
        STUDENT_DUPLICATE_RESOLVED_SCHEMA,
        survivor.id.to_string(),
        &StudentDuplicateResolvedV1 {
            case_id: dc.id.as_uuid(),
            surviving_student_id: survivor.id.as_uuid(),
            archived_student_id: dup.id.as_uuid(),
            institution_id: inst.as_uuid(),
        },
        actor,
    )
    .await?;
    let dc = set_dc_status(conn, dc, CaseStatus::Completed, None, None).await?;
    publish::audit(
        conn,
        scope,
        "student.duplicate_case.merge",
        "duplicate_case",
        dc.id.to_string(),
        serde_json::json!({ "movedContextIds": dc.moved_context_ids }),
        actor,
    )
    .await?;
    Ok(dc)
}

/// Submit a duplicate case for approval (`student.identity.manage`, **step-up**): the
/// `student.duplicate_resolution` process starts; auto-approval merges at once.
#[utoipa::path(
    post,
    path = "/api/v1/student/duplicate-cases/{id}/submit",
    operation_id = "duplicate_case_submit",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Case version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = DuplicateCase, headers(("ETag" = String))), CommandErrors)
)]
pub async fn submit_duplicate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DuplicateCaseId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    let expected = expect_version!(headers);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load_dc(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    drop(conn);
    check_version(expected, cur.version.0)?;
    require_case(cur.status, CaseStatus::UnderReview, "duplicate case")?;
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert(
        "survivorStudentId".to_owned(),
        serde_json::json!(cur.survivor_student_id),
    );
    vars.insert(
        "duplicateStudentId".to_owned(),
        serde_json::json!(cur.duplicate_student_id),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::DUPLICATE_RESOLUTION,
        business_key("duplicate-case", cur.id, cur.version.0),
        vars,
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_dc(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let mut dc = set_dc_status(
        &mut tx,
        &cur,
        CaseStatus::UnderReview,
        None,
        Some(outcome.process_instance_id()),
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(a.correlation_id.clone());
        dc = set_dc_status(&mut tx, &dc, CaseStatus::Approved, None, None).await?;
        dc = apply_merge(&mut tx, &scope, &dc, &sys).await?;
    }
    publish::audit(
        &mut tx,
        &scope,
        "student.duplicate_case.submit",
        "duplicate_case",
        dc.id.to_string(),
        serde_json::json!({ "status": dc.status }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &dc, dc.version))
}

/// Decide a duplicate case under review (`student.identity.manage`, **step-up**).
#[utoipa::path(
    post,
    path = "/api/v1/student/duplicate-cases/{id}/decision",
    operation_id = "duplicate_case_decide",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Case version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DecisionBody,
    responses((status = 200, body = DuplicateCase, headers(("ETag" = String))), CommandErrors)
)]
pub async fn decide_duplicate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DuplicateCaseId>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "duplicate resolution").await?;
    let expected = expect_version!(headers);
    let reason = b.checked_reason()?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load_dc(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_dc(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let dc = match b.decision {
        Decision::Approve => {
            let dc =
                set_dc_status(&mut tx, &cur, CaseStatus::Approved, reason.as_deref(), None).await?;
            apply_merge(&mut tx, &scope, &dc, &a).await?
        }
        Decision::Reject => {
            set_dc_status(&mut tx, &cur, CaseStatus::Rejected, reason.as_deref(), None).await?
        }
    };
    publish::audit(
        &mut tx,
        &scope,
        "student.duplicate_case.decide",
        "duplicate_case",
        dc.id.to_string(),
        serde_json::json!({ "decision": b.decision, "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &dc, dc.version))
}

/// Withdraw an open duplicate case (`student.identity.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/duplicate-cases/{id}/withdraw",
    operation_id = "duplicate_case_withdraw",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Case version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = DuplicateCase, headers(("ETag" = String))), CommandErrors)
)]
pub async fn withdraw_duplicate(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<DuplicateCaseId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentIdentityManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load_dc(&mut conn, tenant(ctx).as_uuid(), id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load_dc(&mut tx, scope.tenant_id.as_uuid(), id, true).await?;
    check_version(expected, cur.version.0)?;
    let dc = set_dc_status(
        &mut tx,
        &cur,
        CaseStatus::Withdrawn,
        reason.as_deref(),
        None,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.duplicate_case.withdraw",
        "duplicate_case",
        dc.id.to_string(),
        serde_json::json!({ "reason": reason }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &dc, dc.version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn case_machine() {
        use CaseStatus as C;
        assert!(case_can_transition(C::Requested, C::UnderReview));
        assert!(case_can_transition(C::UnderReview, C::Approved));
        assert!(case_can_transition(C::Approved, C::Completed));
        assert!(case_can_transition(C::UnderReview, C::Withdrawn));
        assert!(!case_can_transition(C::Requested, C::Approved));
        assert!(!case_can_transition(C::Completed, C::Withdrawn));
        assert!(!case_can_transition(C::Rejected, C::Approved));
        assert!(IdentityChanges::default().is_empty());
        assert!(validate_changes(&IdentityChanges::default()).is_err());
    }
}
