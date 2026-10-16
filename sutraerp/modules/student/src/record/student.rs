//! Student record: directory, Student 360 read model, enrolment (activation → active),
//! enrolment update (C-008), academic assignment (C-009), suspension / restoration (C-010/011),
//! profile (C-014), archival (C-019), histories and self-service `me`.

use super::common::{
    home_institution, prepare_context, respond, update_placement, validate_code, view, view_of,
    Placement,
};
use super::creation::allocate_enrolment_number;
use super::dto::Student;
use super::refs;
use super::support::{
    check_version, expect_version, finish, invalid, modified, norm, record_history, status_changed,
    validate_email, validate_opt_text, validate_phone, validate_text, CommandBody, CommandErrors,
    ReadErrors,
};
use crate::admission::flow::complete_for_context;
use crate::shared::approval::{business_key, process, request_approval};
use crate::shared::guard::{require_institution_writable, require_student_writable};
use crate::shared::http::actor;
use crate::shared::lifecycle::{change_context_status, change_student_status, StatusChange};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::student::{
    contexts_in_institution, contexts_of, load_student, lock_context, lock_student, me,
    student_from_row, StudentContextRow, StudentRow, STUDENT_COLUMNS,
};
use crate::shared::time::business_today;
use crate::shared::{publish, Actor, StudentState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::finance::StudentFinanceQuery;
use sutra_contracts::student::events::record::*;
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_contracts::student::status::{StudentContextStatus, StudentStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

// ---------------------------------------------------------------------------------------------
// Directory + reads
// ---------------------------------------------------------------------------------------------

/// Directory filters.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryQuery {
    /// Name, student number or enrolment number (case-insensitive substring).
    pub q: Option<String>,
    /// Persona status.
    pub status: Option<StudentStatus>,
    pub programme_id: Option<Uuid>,
    /// Page size (1–200, default 50).
    pub limit: Option<i64>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<Uuid>,
}

/// One directory page.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StudentDirectory {
    pub items: Vec<Student>,
    pub next_cursor: Option<Uuid>,
}

/// Students with a context in the institution, newest first (`student.read`). `currentContext`
/// is the student's context **in this institution** (live preferred).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/students",
    operation_id = "student_directory",
    tag = "students",
    params(("id" = String, Path, format = Uuid), DirectoryQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = StudentDirectory), ReadErrors)
)]
pub async fn directory(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<DirectoryQuery>,
) -> ApiResult<Json<StudentDirectory>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let pattern = norm(q.q.as_deref()).map(|s| {
        format!(
            "%{}%",
            s.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        )
    });
    let sql = format!(
        "SELECT {STUDENT_COLUMNS} FROM student.student s WHERE s.tenant_id = $1 \
         AND EXISTS (SELECT 1 FROM student.student_context c WHERE c.student_id = s.id \
             AND c.tenant_id = s.tenant_id AND c.institution_id = $2 \
             AND ($3::uuid IS NULL OR c.programme_id = $3)) \
         AND ($4::text IS NULL OR s.status = $4) \
         AND ($5::text IS NULL OR s.given_name ILIKE $5 OR s.family_name ILIKE $5 \
             OR s.preferred_name ILIKE $5 OR s.student_number ILIKE $5 \
             OR (s.given_name || ' ' || coalesce(s.family_name, '')) ILIKE $5 \
             OR EXISTS (SELECT 1 FROM student.student_context e WHERE e.student_id = s.id \
                 AND e.institution_id = $2 AND e.enrolment_number ILIKE $5)) \
         AND ($6::uuid IS NULL OR s.id < $6) \
         ORDER BY s.id DESC LIMIT $7"
    );
    // One connection for the page: the students, then their contexts in one batch (was one
    // query and one pool checkout per student).
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let rows = sqlx::query(&sql)
        .bind(tenant(ctx).as_uuid())
        .bind(id.as_uuid())
        .bind(q.programme_id)
        .bind(q.status.map(|s| s.as_str()))
        .bind(pattern)
        .bind(q.cursor)
        .bind(limit + 1)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut students: Vec<StudentRow> = rows
        .iter()
        .map(student_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    let next_cursor = if students.len() as i64 > limit {
        students.truncate(limit as usize);
        students.last().map(|s| s.id.as_uuid())
    } else {
        None
    };
    let ids: Vec<Uuid> = students.iter().map(|s| s.id.as_uuid()).collect();
    let mut by_student: std::collections::HashMap<Uuid, Vec<StudentContextRow>> =
        std::collections::HashMap::with_capacity(ids.len());
    for c in contexts_in_institution(&mut *conn, tenant(ctx), id, &ids).await? {
        by_student
            .entry(c.student_id.as_uuid())
            .or_default()
            .push(c);
    }
    let mut items = Vec::with_capacity(students.len());
    for s in &students {
        let cs: Vec<StudentContextRow> = by_student.remove(&s.id.as_uuid()).unwrap_or_default();
        let cur = cs
            .iter()
            .find(|c| c.status.is_live())
            .or_else(|| cs.first());
        items.push(Student::from_rows(s, cur, &[]));
    }
    Ok(Json(StudentDirectory { items, next_cursor }))
}

/// Student 360 read model: persona, current context and every context (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}",
    operation_id = "student_get",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = Student, headers(("ETag" = String, description = "Persona version"))), ReadErrors)
)]
pub async fn get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = view(&mut conn, tenant(ctx), id).await?;
    Ok(respond(StatusCode::OK, &s, s.version))
}

/// Own Student record (`student.self`; resolved from the session, 404 without a link).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/student",
    operation_id = "student_self_get",
    tag = "students",
    security(("session_cookie" = [])),
    responses((status = 200, body = Student, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get_me(State(st): State<StudentState>, auth: Authenticated) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentSelfService)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let s = me(&mut *conn, ctx).await?;
    let v = view_of(&mut conn, &s).await?;
    Ok(respond(StatusCode::OK, &v, v.version))
}

/// One status-history row.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = StudentStatusHistoryEntry)]
pub struct StatusHistoryEntry {
    pub student_context_id: Option<Uuid>,
    pub institution_id: Uuid,
    /// `student` | `context`.
    pub scope: String,
    pub from_status: Option<String>,
    pub to_status: String,
    pub reason: Option<String>,
    pub cause_event_type: String,
    pub process_instance_id: Option<String>,
    pub actor: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

/// Status history collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[schema(as = StudentStatusHistory)]
pub struct StatusHistory {
    pub items: Vec<StatusHistoryEntry>,
}

/// Persona and context status history, oldest first (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/status-history",
    operation_id = "student_status_history",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = StatusHistory), ReadErrors)
)]
pub async fn status_history(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<StatusHistory>> {
    let ctx = auth.require(Permission::StudentRead)?;
    load_student(&st.pool, tenant(ctx), id).await?;
    let rows = sqlx::query(
        "SELECT student_context_id, institution_id, scope, from_status, to_status, reason, \
         cause_event_type, process_instance_id, actor, occurred_at \
         FROM student.student_status_history WHERE student_id = $1 AND tenant_id = $2 \
         ORDER BY occurred_at, id",
    )
    .bind(id.as_uuid())
    .bind(tenant(ctx).as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| {
            Ok(StatusHistoryEntry {
                student_context_id: r.try_get("student_context_id")?,
                institution_id: r.try_get("institution_id")?,
                scope: r.try_get("scope")?,
                from_status: r.try_get("from_status")?,
                to_status: r.try_get("to_status")?,
                reason: r.try_get("reason")?,
                cause_event_type: r.try_get("cause_event_type")?,
                process_instance_id: r.try_get("process_instance_id")?,
                actor: r.try_get("actor")?,
                occurred_at: r.try_get("occurred_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(StatusHistory { items }))
}

/// One record-history row (before/after values of a governed change).
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = StudentRecordHistoryEntry)]
pub struct RecordHistoryEntry {
    pub student_context_id: Option<Uuid>,
    /// `enrolment` | `assignment` | `transfer` | `profile` | `identity` | `merge` | `communication`.
    pub kind: String,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

/// Record history collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[schema(as = StudentRecordHistory)]
pub struct RecordHistory {
    pub items: Vec<RecordHistoryEntry>,
}

/// Enrolment / assignment / profile / identity history, oldest first (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/record-history",
    operation_id = "student_record_history",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = RecordHistory), ReadErrors)
)]
pub async fn record_history_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<RecordHistory>> {
    let ctx = auth.require(Permission::StudentRead)?;
    load_student(&st.pool, tenant(ctx), id).await?;
    let rows = sqlx::query(
        "SELECT student_context_id, kind, before_values, after_values, reason, actor, occurred_at \
         FROM student.student_record_history WHERE student_id = $1 AND tenant_id = $2 \
         ORDER BY occurred_at, id",
    )
    .bind(id.as_uuid())
    .bind(tenant(ctx).as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| {
            Ok(RecordHistoryEntry {
                student_context_id: r.try_get("student_context_id")?,
                kind: r.try_get("kind")?,
                before: r.try_get("before_values")?,
                after: r.try_get("after_values")?,
                reason: r.try_get("reason")?,
                actor: r.try_get("actor")?,
                occurred_at: r.try_get("occurred_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(RecordHistory { items }))
}

// ---------------------------------------------------------------------------------------------
// Enrolment (activation → active), enrolment update, assignment
// ---------------------------------------------------------------------------------------------

/// Context status event (StatusChangedV1 with the context as aggregate).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn context_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event: (&'static str, &'static str),
    s: &StudentRow,
    c: &StudentContextRow,
    from: Option<StudentContextStatus>,
    reason: Option<&str>,
    pid: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::event(
        conn,
        scope,
        event.0,
        event.1,
        c.id.to_string(),
        &status_changed(
            c.id.as_uuid(),
            c.institution_id,
            Some(s.id.as_uuid()),
            from.map(|f| f.as_str()),
            c.status.as_str(),
            c.version.0,
            reason.map(str::to_owned),
            pid.map(str::to_owned),
            s.subject_ref.clone(),
        ),
        actor,
    )
    .await
}

/// Walks an `activated` persona through `enrolled → academically_assigned → active`
/// (L2-C02 §11.10.3); any other persona status is left unchanged.
pub(crate) async fn persona_to_active(
    conn: &mut PgConnection,
    s: StudentRow,
    institution: InstitutionId,
    actor: &Actor,
) -> Result<StudentRow, KernelError> {
    if s.status != StudentStatus::Activated {
        return Ok(s);
    }
    let change = StatusChange {
        cause_event_type: STUDENT_ENROLLED,
        reason: Some("enrolment completed"),
        ..Default::default()
    };
    let s = change_student_status(
        conn,
        &s,
        institution,
        StudentStatus::Enrolled,
        &change,
        actor,
    )
    .await?;
    let s = change_student_status(
        conn,
        &s,
        institution,
        StudentStatus::AcademicallyAssigned,
        &change,
        actor,
    )
    .await?;
    change_student_status(conn, &s, institution, StudentStatus::Active, &change, actor).await
}

/// Body of `POST /students/{id}/enrol`.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = StudentEnrolment)]
pub struct EnrolBody {
    /// Default: the student's `pending_activation` context.
    pub context_id: Option<StudentContextId>,
    /// Default: allocated `EN<year><seq>`.
    pub enrolment_number: Option<String>,
    pub roll_number: Option<String>,
    pub campus_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub batch_code: Option<String>,
    pub section_code: Option<String>,
    /// Business-local date; default today.
    pub enrolled_on: Option<NaiveDate>,
}

/// Complete enrolment (`student.manage`, L3-C-001 end): the `pending_activation` context becomes
/// `active` with its placement; the persona moves `activated → enrolled → academically_assigned →
/// active` in the same transaction; the admission behind it becomes `completed`. `If-Match` =
/// the context's version; the response `ETag` is the context's new version.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/enrol",
    operation_id = "student_enrol",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag (version) of the context being enrolled")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<EnrolBody>, description = "Placement"),
    responses((status = 200, body = Student, headers(("ETag" = String, description = "Context version"))), CommandErrors)
)]
pub async fn enrol(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    body: Option<Json<EnrolBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let b = body.map(|b| b.0).unwrap_or_default();
    let mut v = vec![];
    validate_opt_text("enrolmentNumber", b.enrolment_number.as_deref(), 40, &mut v);
    validate_opt_text("rollNumber", b.roll_number.as_deref(), 40, &mut v);
    validate_code("batchCode", b.batch_code.as_deref(), &mut v);
    validate_code("sectionCode", b.section_code.as_deref(), &mut v);
    finish(v)?;
    let (_, c, scope) = prepare_context(&st, ctx, id, b.context_id).await?;
    refs::campus(&st, &scope, c.institution_id, b.campus_id, "campusId").await?;
    refs::org_unit(&st, &scope, c.institution_id, b.org_unit_id, "orgUnitId").await?;
    let today = business_today(&st, &scope, c.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_context(&mut tx, scope.tenant_id, c.id).await?;
    check_version(expected, c.version.0)?;
    if c.status != StudentContextStatus::PendingActivation {
        return Err(KernelError::Conflict(format!(
            "student context is {}; only a pending_activation context is enrolled",
            c.status
        ))
        .into());
    }
    let number = match norm(b.enrolment_number.as_deref()) {
        Some(n) => Some(n),
        None => match c.enrolment_number.clone() {
            Some(n) => Some(n),
            None => Some(
                allocate_enrolment_number(&mut tx, &scope, c.institution_id.as_uuid(), today)
                    .await?,
            ),
        },
    };
    let mut p = Placement::from(&c);
    p.enrolment_number = number;
    p.roll_number = norm(b.roll_number.as_deref()).or(p.roll_number);
    p.campus_id = b.campus_id.or(p.campus_id);
    p.org_unit_id = b.org_unit_id.or(p.org_unit_id);
    p.batch_code = norm(b.batch_code.as_deref()).or(p.batch_code);
    p.section_code = norm(b.section_code.as_deref()).or(p.section_code);
    p.enrolled_on = Some(b.enrolled_on.unwrap_or(today));
    let c = update_placement(&mut tx, &c, &p, "enrolment", None, &a).await?;
    let c = change_context_status(
        &mut tx,
        &c,
        StudentContextStatus::Active,
        &StatusChange {
            cause_event_type: STUDENT_ENROLLED,
            reason: Some("enrolment completed"),
            ..Default::default()
        },
        &a,
    )
    .await?;
    let s = lock_student(&mut tx, scope.tenant_id, id).await?;
    let s = persona_to_active(&mut tx, s, c.institution_id, &a).await?;
    context_event(
        &mut tx,
        &scope,
        (STUDENT_ENROLLED, STUDENT_ENROLLED_SCHEMA),
        &s,
        &c,
        Some(StudentContextStatus::PendingActivation),
        None,
        None,
        &a,
    )
    .await?;
    if let Some(adm) = c.admission_id {
        complete_for_context(&mut tx, &scope, adm, &a).await?;
    }
    publish::audit(
        &mut tx,
        &scope,
        "student.student.enrol",
        "student",
        s.id.to_string(),
        serde_json::json!({ "studentContextId": c.id, "enrolmentNumber": c.enrolment_number }),
        &a,
    )
    .await?;
    let out = view_of(&mut tx, &s).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &out, c.version))
}

/// Requires an `active` context (enrolment / assignment changes, L4-C-008/009).
fn require_active(c: &StudentContextRow) -> Result<(), KernelError> {
    if c.status != StudentContextStatus::Active {
        return Err(KernelError::Conflict(format!(
            "student context is {}; the student must be active",
            c.status
        )));
    }
    Ok(())
}

/// Body of `PATCH /students/{id}/enrolment` (present = replace).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = StudentEnrolmentUpdate)]
pub struct EnrolmentUpdate {
    pub context_id: Option<StudentContextId>,
    pub enrolment_number: Option<String>,
    pub roll_number: Option<String>,
    /// `regular` | `lateral_entry` | `readmission` | `transfer_in` | `re_entry`.
    pub enrolment_type: Option<String>,
    pub enrolled_on: Option<NaiveDate>,
    pub admission_term_id: Option<Uuid>,
    pub reason: Option<String>,
}

/// Update enrolment attributes of an active student (`student.manage`, L3-C-008): numbers,
/// enrolment type/date, admission term. Previous values are kept as history; programme / batch /
/// section are academic assignment (`PATCH …/assignment`). `If-Match` = context version.
#[utoipa::path(
    patch,
    path = "/api/v1/student/students/{id}/enrolment",
    operation_id = "student_update_enrolment",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Context version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = EnrolmentUpdate,
    responses((status = 200, body = Student, headers(("ETag" = String, description = "Context version"))), CommandErrors)
)]
pub async fn update_enrolment(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<EnrolmentUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let reason = CommandBody {
        reason: b.reason.clone(),
    }
    .reason()?;
    let mut v = vec![];
    validate_opt_text("enrolmentNumber", b.enrolment_number.as_deref(), 40, &mut v);
    validate_opt_text("rollNumber", b.roll_number.as_deref(), 40, &mut v);
    if let Some(t) = b.enrolment_type.as_deref() {
        if ![
            "regular",
            "lateral_entry",
            "readmission",
            "transfer_in",
            "re_entry",
        ]
        .contains(&t)
        {
            v.push(sutra_kernel::error::FieldViolation::new(
                "enrolmentType",
                "unknown",
                "unknown enrolment type".to_owned(),
            ));
        }
    }
    finish(v)?;
    let (_, c, scope) = prepare_context(&st, ctx, id, b.context_id).await?;
    let year = refs::term(
        &st,
        &scope,
        c.institution_id,
        b.admission_term_id,
        "admissionTermId",
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_context(&mut tx, scope.tenant_id, c.id).await?;
    check_version(expected, c.version.0)?;
    require_active(&c)?;
    let before = Placement::from(&c);
    let mut p = before.clone();
    if let Some(n) = norm(b.enrolment_number.as_deref()) {
        p.enrolment_number = Some(n);
    }
    if let Some(r) = b.roll_number.as_deref() {
        p.roll_number = norm(Some(r));
    }
    if let Some(t) = b.enrolment_type.as_deref() {
        p.enrolment_type = t.to_owned();
    }
    if let Some(d) = b.enrolled_on {
        p.enrolled_on = Some(d);
    }
    if b.admission_term_id.is_some() {
        p.admission_term_id = b.admission_term_id;
        p.admission_year_id = year;
    }
    let changed = before.changed(&p);
    if changed.is_empty() {
        return Err(invalid("enrolment", "no-change", "nothing to change").into());
    }
    let c = update_placement(&mut tx, &c, &p, "enrolment", reason.as_deref(), &a).await?;
    let s = load_student(&mut *tx, scope.tenant_id, id).await?;
    publish::event(
        &mut tx,
        &scope,
        STUDENT_ENROLMENT_UPDATED,
        STUDENT_ENROLMENT_UPDATED_SCHEMA,
        c.id.to_string(),
        &modified(
            c.id.as_uuid(),
            c.institution_id,
            Some(s.id.as_uuid()),
            c.version.0,
            changed.clone(),
            s.subject_ref.clone(),
        ),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student.update_enrolment",
        "student",
        s.id.to_string(),
        serde_json::json!({ "studentContextId": c.id, "changedFields": changed, "reason": reason }),
        &a,
    )
    .await?;
    let out = view_of(&mut tx, &s).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &out, c.version))
}

/// Body of `PATCH /students/{id}/assignment` (present = replace; empty string clears a code).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = StudentAssignmentUpdate)]
pub struct AssignmentUpdate {
    pub context_id: Option<StudentContextId>,
    /// Must equal the current programme: a programme change needs an approved programme
    /// migration (3B, L4-C-009-BH-002) → 422 `programme-migration-required`.
    pub programme_id: Option<Uuid>,
    /// Department / organisation unit (active, same institution).
    pub org_unit_id: Option<Uuid>,
    pub batch_code: Option<String>,
    pub section_code: Option<String>,
    pub reason: Option<String>,
}

/// Change the academic assignment of an active student (`student.manage`, L3-C-009):
/// department, batch and section. `If-Match` = context version.
#[utoipa::path(
    patch,
    path = "/api/v1/student/students/{id}/assignment",
    operation_id = "student_change_assignment",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Context version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = AssignmentUpdate,
    responses((status = 200, body = Student, headers(("ETag" = String, description = "Context version"))), CommandErrors)
)]
pub async fn change_assignment(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<AssignmentUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let reason = CommandBody {
        reason: b.reason.clone(),
    }
    .reason()?;
    let mut v = vec![];
    validate_code("batchCode", b.batch_code.as_deref(), &mut v);
    validate_code("sectionCode", b.section_code.as_deref(), &mut v);
    finish(v)?;
    let (_, c, scope) = prepare_context(&st, ctx, id, b.context_id).await?;
    if b.programme_id.is_some_and(|p| Some(p) != c.programme_id) {
        return Err(invalid(
            "programmeId",
            "programme-migration-required",
            "a programme change needs an approved programme migration",
        )
        .into());
    }
    refs::org_unit(&st, &scope, c.institution_id, b.org_unit_id, "orgUnitId").await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_context(&mut tx, scope.tenant_id, c.id).await?;
    check_version(expected, c.version.0)?;
    require_active(&c)?;
    let before = Placement::from(&c);
    let mut p = before.clone();
    if b.org_unit_id.is_some() {
        p.org_unit_id = b.org_unit_id;
    }
    if let Some(x) = b.batch_code.as_deref() {
        p.batch_code = norm(Some(x));
    }
    if let Some(x) = b.section_code.as_deref() {
        p.section_code = norm(Some(x));
    }
    let changed = before.changed(&p);
    if changed.is_empty() {
        return Err(invalid("assignment", "no-change", "nothing to change").into());
    }
    let c = update_placement(&mut tx, &c, &p, "assignment", reason.as_deref(), &a).await?;
    let s = load_student(&mut *tx, scope.tenant_id, id).await?;
    publish::event(
        &mut tx,
        &scope,
        STUDENT_ACADEMIC_ASSIGNMENT_CHANGED,
        STUDENT_ACADEMIC_ASSIGNMENT_CHANGED_SCHEMA,
        c.id.to_string(),
        &modified(
            c.id.as_uuid(),
            c.institution_id,
            Some(s.id.as_uuid()),
            c.version.0,
            changed.clone(),
            s.subject_ref.clone(),
        ),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student.change_assignment",
        "student",
        s.id.to_string(),
        serde_json::json!({ "studentContextId": c.id, "changedFields": changed, "reason": reason }),
        &a,
    )
    .await?;
    let out = view_of(&mut tx, &s).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &out, c.version))
}

// ---------------------------------------------------------------------------------------------
// Suspension / restoration
// ---------------------------------------------------------------------------------------------

/// Body of `…/suspend`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = StudentSuspend)]
pub struct SuspendBody {
    pub context_id: Option<StudentContextId>,
    pub reason: String,
    /// `administrative` (default) | `disciplinary` | `financial` | `academic` | `medical` | `other`.
    pub category: Option<String>,
    /// Planned review date (business-local).
    pub review_on: Option<NaiveDate>,
}

const SUSPENSION_CATEGORIES: &[&str] = &[
    "administrative",
    "disciplinary",
    "financial",
    "academic",
    "medical",
    "other",
];

/// Suspend an active student (`student.lifecycle`, **step-up**, L3-C-010): context `active →
/// suspended`, persona `active → suspended` when no other active context remains, a suspension
/// episode with its reason (`student.suspended`). The `student.student_suspension` process is
/// started for traceability; the step-up-authorised caller is the deciding authority.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/suspend",
    operation_id = "student_suspend",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Context version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = SuspendBody,
    responses((status = 200, body = Student, headers(("ETag" = String, description = "Context version"))), CommandErrors)
)]
pub async fn suspend(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<SuspendBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentLifecycle)?;
    let expected = expect_version!(headers);
    let mut v = vec![];
    validate_text("reason", &b.reason, 1000, &mut v);
    let category = b
        .category
        .as_deref()
        .map(str::trim)
        .unwrap_or("administrative")
        .to_owned();
    if !SUSPENSION_CATEGORIES.contains(&category.as_str()) {
        v.push(sutra_kernel::error::FieldViolation::new(
            "category",
            "unknown",
            "unknown suspension category".to_owned(),
        ));
    }
    finish(v)?;
    let (_, c, scope) = prepare_context(&st, ctx, id, b.context_id).await?;
    check_version(expected, c.version.0)?;
    require_active(&c)?;
    let today = business_today(&st, &scope, c.institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("studentId".to_owned(), serde_json::json!(id));
    vars.insert("category".to_owned(), serde_json::json!(category));
    let pid = request_approval(
        &st,
        &scope,
        process::STUDENT_SUSPENSION,
        business_key("student-suspension", c.id, c.version.0),
        vars,
    )
    .await?
    .process_instance_id()
    .to_owned();
    let a = actor(ctx);
    let reason = b.reason.trim().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_context(&mut tx, scope.tenant_id, c.id).await?;
    check_version(expected, c.version.0)?;
    let c = change_context_status(
        &mut tx,
        &c,
        StudentContextStatus::Suspended,
        &StatusChange {
            cause_event_type: STUDENT_SUSPENDED,
            reason: Some(&reason),
            process_instance_id: Some(&pid),
            closed_on: None,
        },
        &a,
    )
    .await?;
    sqlx::query(
        "INSERT INTO student.student_suspension (tenant_id, student_id, student_context_id, \
         institution_id, category, reason, suspended_on, review_on, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .bind(c.id.as_uuid())
    .bind(c.institution_id.as_uuid())
    .bind(&category)
    .bind(&reason)
    .bind(today)
    .bind(b.review_on)
    .bind(&pid)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let s = lock_student(&mut tx, scope.tenant_id, id).await?;
    let other_active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.student_context WHERE student_id = $1 AND id <> $2 \
         AND status = 'active'",
    )
    .bind(id.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let s = if s.status == StudentStatus::Active && other_active == 0 {
        change_student_status(
            &mut tx,
            &s,
            c.institution_id,
            StudentStatus::Suspended,
            &StatusChange {
                cause_event_type: STUDENT_SUSPENDED,
                reason: Some(&reason),
                process_instance_id: Some(&pid),
                closed_on: None,
            },
            &a,
        )
        .await?
    } else {
        s
    };
    context_event(
        &mut tx,
        &scope,
        (STUDENT_SUSPENDED, STUDENT_SUSPENDED_SCHEMA),
        &s,
        &c,
        Some(StudentContextStatus::Active),
        Some(&reason),
        Some(&pid),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student.suspend",
        "student",
        s.id.to_string(),
        serde_json::json!({ "studentContextId": c.id, "category": category, "reason": reason }),
        &a,
    )
    .await?;
    let out = view_of(&mut tx, &s).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &out, c.version))
}

/// Body of `…/restore`.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = StudentRestoration)]
pub struct RestoreBody {
    pub context_id: Option<StudentContextId>,
    pub reason: String,
}

/// Restore a suspended student (`student.lifecycle`, **step-up**, L3-C-011): context
/// `suspended → active`, persona `suspended → active`, the suspension episode is closed (history
/// kept). A `financial` suspension needs Finance evidence that permits clearance
/// (`FinanceClearancePort::student_dues`), else 409.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/restore",
    operation_id = "student_restore",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Context version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RestoreBody,
    responses((status = 200, body = Student, headers(("ETag" = String, description = "Context version"))), CommandErrors)
)]
pub async fn restore(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<RestoreBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentLifecycle)?;
    let expected = expect_version!(headers);
    let mut v = vec![];
    validate_text("reason", &b.reason, 1000, &mut v);
    finish(v)?;
    let (_, c, scope) = prepare_context(&st, ctx, id, b.context_id).await?;
    check_version(expected, c.version.0)?;
    if c.status != StudentContextStatus::Suspended {
        return Err(KernelError::Conflict(format!(
            "student context is {}; only a suspended student is restored",
            c.status
        ))
        .into());
    }
    let category: Option<String> = sqlx::query_scalar(
        "SELECT category FROM student.student_suspension WHERE student_context_id = $1 \
         AND status = 'active'",
    )
    .bind(c.id.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    if category.as_deref() == Some("financial") {
        let ev = st
            .finance
            .student_dues(
                &scope,
                StudentFinanceQuery {
                    institution_id: c.institution_id,
                    student_id: id.as_uuid(),
                },
            )
            .await?;
        if !ev.permits_clearance() {
            return Err(KernelError::Conflict(
                "Finance evidence does not permit restoration of a financial suspension".into(),
            )
            .into());
        }
    }
    let today = business_today(&st, &scope, c.institution_id).await?;
    let a = actor(ctx);
    let reason = b.reason.trim().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_context(&mut tx, scope.tenant_id, c.id).await?;
    check_version(expected, c.version.0)?;
    let change = StatusChange {
        cause_event_type: STUDENT_RESTORED,
        reason: Some(&reason),
        ..Default::default()
    };
    let c = change_context_status(&mut tx, &c, StudentContextStatus::Active, &change, &a).await?;
    sqlx::query(
        "UPDATE student.student_suspension SET status = 'restored', restored_on = $2, \
         restoration_reason = $3, version = version + 1, updated_at = now() \
         WHERE student_context_id = $1 AND status = 'active'",
    )
    .bind(c.id.as_uuid())
    .bind(today)
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let s = lock_student(&mut tx, scope.tenant_id, id).await?;
    let s = if s.status == StudentStatus::Suspended {
        change_student_status(
            &mut tx,
            &s,
            c.institution_id,
            StudentStatus::Active,
            &change,
            &a,
        )
        .await?
    } else {
        s
    };
    context_event(
        &mut tx,
        &scope,
        (STUDENT_RESTORED, STUDENT_RESTORED_SCHEMA),
        &s,
        &c,
        Some(StudentContextStatus::Suspended),
        Some(&reason),
        None,
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student.restore",
        "student",
        s.id.to_string(),
        serde_json::json!({ "studentContextId": c.id, "reason": reason }),
        &a,
    )
    .await?;
    let out = view_of(&mut tx, &s).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &out, c.version))
}

/// One suspension episode.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = StudentSuspensionEpisode)]
pub struct Suspension {
    pub id: Uuid,
    pub student_context_id: Uuid,
    pub category: String,
    pub reason: String,
    pub suspended_on: NaiveDate,
    pub review_on: Option<NaiveDate>,
    /// `active` | `restored`.
    pub status: String,
    pub restored_on: Option<NaiveDate>,
    pub restoration_reason: Option<String>,
    pub process_instance_id: Option<String>,
}

/// Suspension collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
#[schema(as = StudentSuspensionList)]
pub struct SuspensionList {
    pub items: Vec<Suspension>,
}

/// Suspension episodes of a student, newest first (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/suspensions",
    operation_id = "student_suspensions",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = SuspensionList), ReadErrors)
)]
pub async fn suspensions(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<SuspensionList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    load_student(&st.pool, tenant(ctx), id).await?;
    let rows = sqlx::query(
        "SELECT id, student_context_id, category, reason, suspended_on, review_on, status, \
         restored_on, restoration_reason, process_instance_id FROM student.student_suspension \
         WHERE student_id = $1 AND tenant_id = $2 ORDER BY created_at DESC, id DESC",
    )
    .bind(id.as_uuid())
    .bind(tenant(ctx).as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| {
            Ok(Suspension {
                id: r.try_get("id")?,
                student_context_id: r.try_get("student_context_id")?,
                category: r.try_get("category")?,
                reason: r.try_get("reason")?,
                suspended_on: r.try_get("suspended_on")?,
                review_on: r.try_get("review_on")?,
                status: r.try_get("status")?,
                restored_on: r.try_get("restored_on")?,
                restoration_reason: r.try_get("restoration_reason")?,
                process_instance_id: r.try_get("process_instance_id")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)?;
    Ok(Json(SuspensionList { items }))
}

// ---------------------------------------------------------------------------------------------
// Archival, profile
// ---------------------------------------------------------------------------------------------

/// Archive the student record (`student.archive`, **step-up**, L3-C-019): permanent record
/// closure once every context is terminal (an exit/closure condition exists). The persona
/// becomes `archived` and read-only; nothing is deleted. `If-Match` = persona version.
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/archive",
    operation_id = "student_archive",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Persona version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = Student, headers(("ETag" = String))), CommandErrors)
)]
pub async fn archive(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentArchive)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().required_reason()?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    require_student_writable(&s)?;
    let all = contexts_of(&st.pool, s.tenant_id, s.id).await?;
    let Some(last) = all.first() else {
        return Err(KernelError::Conflict(
            "no exit or closure condition: the student has no institution context".into(),
        )
        .into());
    };
    if all.iter().any(|c| c.status.is_live()) {
        return Err(KernelError::Conflict(
            "the student still has a live context; complete the exit before archiving".into(),
        )
        .into());
    }
    let inst = last.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let mut vars = BTreeMap::new();
    vars.insert("studentId".to_owned(), serde_json::json!(id));
    let pid = request_approval(
        &st,
        &scope,
        process::STUDENT_ARCHIVAL,
        business_key("student-archival", id, s.version.0),
        vars,
    )
    .await?
    .process_instance_id()
    .to_owned();
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_student(&mut tx, scope.tenant_id, id).await?;
    check_version(expected, s.version.0)?;
    let from = s.status;
    let s = change_student_status(
        &mut tx,
        &s,
        inst,
        StudentStatus::Archived,
        &StatusChange {
            cause_event_type: STUDENT_ARCHIVED,
            reason: Some(&reason),
            process_instance_id: Some(&pid),
            closed_on: None,
        },
        &a,
    )
    .await?;
    sqlx::query(
        "UPDATE student.document_request SET status = 'cancelled', \
         status_reason = 'student record archived', version = version + 1, updated_at = now() \
         WHERE student_id = $1 AND status IN ('requested', 'in_progress')",
    )
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    publish::event(
        &mut tx,
        &scope,
        STUDENT_ARCHIVED,
        STUDENT_ARCHIVED_SCHEMA,
        s.id.to_string(),
        &status_changed(
            s.id.as_uuid(),
            inst,
            Some(s.id.as_uuid()),
            Some(from.as_str()),
            s.status.as_str(),
            s.version.0,
            Some(reason.clone()),
            Some(pid.clone()),
            s.subject_ref.clone(),
        ),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student.archive",
        "student",
        s.id.to_string(),
        serde_json::json!({ "reason": reason, "processInstanceId": pid }),
        &a,
    )
    .await?;
    let out = view_of(&mut tx, &s).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &out, out.version))
}

/// Body of `PATCH /students/{id}/profile`. Only editable profile attributes are accepted;
/// identity attributes (names, date of birth, gender) change through identity correction and
/// are rejected here (unknown member → 4xx). Present = replace; empty string clears.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = StudentProfileUpdate)]
pub struct ProfileUpdate {
    pub preferred_name: Option<String>,
    pub primary_email: Option<String>,
    pub primary_phone: Option<String>,
    /// Login subject for self-service (e.g. `dev|aarav.desai@sutra.dev`).
    pub subject_ref: Option<String>,
    pub reason: Option<String>,
}

/// Update the student's editable profile (`student.manage`, L3-C-014). Lifecycle status and
/// identity are unchanged. `If-Match` = persona version.
#[utoipa::path(
    patch,
    path = "/api/v1/student/students/{id}/profile",
    operation_id = "student_update_profile",
    tag = "students",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "Persona version")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ProfileUpdate,
    responses((status = 200, body = Student, headers(("ETag" = String))), CommandErrors)
)]
pub async fn update_profile(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(b): Json<ProfileUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentManage)?;
    let expected = expect_version!(headers);
    let reason = CommandBody {
        reason: b.reason.clone(),
    }
    .reason()?;
    let mut v = vec![];
    validate_opt_text("preferredName", b.preferred_name.as_deref(), 100, &mut v);
    validate_email("primaryEmail", b.primary_email.as_deref(), &mut v);
    validate_phone("primaryPhone", b.primary_phone.as_deref(), &mut v);
    validate_opt_text("subjectRef", b.subject_ref.as_deref(), 200, &mut v);
    finish(v)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    require_student_writable(&s)?;
    let inst = home_institution(&st.pool, &s).await?;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_student(&mut tx, scope.tenant_id, id).await?;
    check_version(expected, s.version.0)?;
    let before = serde_json::json!({
        "preferredName": s.preferred_name, "primaryEmail": s.primary_email,
        "primaryPhone": s.primary_phone, "subjectLinked": s.subject_ref.is_some(),
    });
    let pick = |new: &Option<String>, old: &Option<String>| match new {
        Some(n) => norm(Some(n)),
        None => old.clone(),
    };
    let preferred = pick(&b.preferred_name, &s.preferred_name);
    let email = pick(&b.primary_email, &s.primary_email);
    let phone = pick(&b.primary_phone, &s.primary_phone);
    let subject = pick(&b.subject_ref, &s.subject_ref);
    let mut changed = vec![];
    for (name, new, old) in [
        ("preferredName", &preferred, &s.preferred_name),
        ("primaryEmail", &email, &s.primary_email),
        ("primaryPhone", &phone, &s.primary_phone),
        ("subjectRef", &subject, &s.subject_ref),
    ] {
        if new != old {
            changed.push(name.to_owned());
        }
    }
    if changed.is_empty() {
        return Err(invalid("profile", "no-change", "nothing to change").into());
    }
    let sql = format!(
        "UPDATE student.student SET preferred_name = $3, primary_email = $4, primary_phone = $5, \
         subject_ref = $6, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING {STUDENT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(&preferred)
        .bind(&email)
        .bind(&phone)
        .bind(&subject)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match &e {
            sqlx::Error::Database(d) if d.constraint() == Some("student_subject_unique") => {
                KernelError::Conflict("the login is already linked to another student".into())
            }
            _ => map_db_error(e),
        })?;
    let s = student_from_row(&row).map_err(map_db_error)?;
    let after = serde_json::json!({
        "preferredName": s.preferred_name, "primaryEmail": s.primary_email,
        "primaryPhone": s.primary_phone, "subjectLinked": s.subject_ref.is_some(),
    });
    record_history(
        &mut tx,
        scope.tenant_id,
        id.as_uuid(),
        None,
        "profile",
        before,
        after,
        reason.as_deref(),
        &a,
    )
    .await?;
    publish::event(
        &mut tx,
        &scope,
        STUDENT_PROFILE_UPDATED,
        STUDENT_PROFILE_UPDATED_SCHEMA,
        s.id.to_string(),
        &modified(
            s.id.as_uuid(),
            inst,
            Some(s.id.as_uuid()),
            s.version.0,
            changed.clone(),
            s.subject_ref.clone(),
        ),
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.student.update_profile",
        "student",
        s.id.to_string(),
        serde_json::json!({ "changedFields": changed, "reason": reason }),
        &a,
    )
    .await?;
    let out = view_of(&mut tx, &s).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &out, out.version))
}

/// Possible duplicates of a student (same tenant; same date of birth and name, or same e-mail /
/// phone). Evidence for a duplicate case (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/duplicate-candidates",
    operation_id = "student_duplicate_candidates",
    tag = "students",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = super::student::StudentDirectory), ReadErrors)
)]
pub async fn duplicate_candidates(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<StudentDirectory>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let s = load_student(&st.pool, tenant(ctx), id).await?;
    let sql = format!(
        "SELECT {STUDENT_COLUMNS} FROM student.student WHERE tenant_id = $1 AND id <> $2 \
         AND status <> 'archived' AND ( \
           (date_of_birth IS NOT NULL AND date_of_birth = $3 AND lower(given_name) = lower($4) \
            AND lower(coalesce(family_name, '')) = lower(coalesce($5, ''))) \
           OR ($6::text IS NOT NULL AND lower(primary_email) = lower($6)) \
           OR ($7::text IS NOT NULL AND primary_phone = $7)) \
         ORDER BY id DESC LIMIT 20"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant(ctx).as_uuid())
        .bind(id.as_uuid())
        .bind(s.date_of_birth)
        .bind(&s.given_name)
        .bind(&s.family_name)
        .bind(&s.primary_email)
        .bind(&s.primary_phone)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let mut items = vec![];
    for r in &rows {
        let c = student_from_row(r).map_err(map_db_error)?;
        let cur = crate::shared::student::current_context(&st.pool, c.tenant_id, c.id).await?;
        items.push(Student::from_rows(&c, cur.as_ref(), &[]));
    }
    Ok(Json(StudentDirectory {
        items,
        next_cursor: None,
    }))
}
