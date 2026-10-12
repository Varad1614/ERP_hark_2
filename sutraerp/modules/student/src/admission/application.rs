//! Admission applications: draft → submitted → under review → offered (Admission Record
//! initiated) | rejected | withdrawn (L3-C-001 intake, L2-C01 WF-C01-01).

use super::domain::{
    application_can_transition, display_name, AdmissionApplication, DEFAULT_REQUIRED_DOCUMENTS,
};
use super::flow::{self as adm, history as admission_history};
use crate::record::refs;
use crate::record::support::{
    check_version, expect_version, finish, invalid, norm, status_changed, validate_email,
    validate_key, validate_opt_text, validate_phone, validate_text, CommandBody, CommandErrors,
    CreateErrors, ReadErrors,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{publish, Actor, StudentState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::student::events::admission::{
    AdmissionInitiatedV1, ADMISSION_APPLICATION_OFFERED, ADMISSION_APPLICATION_OFFERED_SCHEMA,
    ADMISSION_APPLICATION_REJECTED, ADMISSION_APPLICATION_REJECTED_SCHEMA,
    ADMISSION_APPLICATION_SUBMITTED, ADMISSION_APPLICATION_SUBMITTED_SCHEMA,
    ADMISSION_APPLICATION_WITHDRAWN, ADMISSION_APPLICATION_WITHDRAWN_SCHEMA, ADMISSION_INITIATED,
    ADMISSION_INITIATED_SCHEMA,
};
use sutra_contracts::student::ids::{AdmissionApplicationId, AdmissionId};
use sutra_contracts::student::status::{AdmissionApplicationStatus, AdmissionStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

const COLUMNS: &str = "id, institution_id, programme_id, curriculum_version_id, campus_id, \
    admission_term_id, admission_year_id, given_name, middle_name, family_name, date_of_birth, \
    gender, email, phone, subject_ref, prior_qualification, notes, status, status_reason, \
    submitted_at, decided_at, admission_id, version, created_at, updated_at";

fn from_row(r: &PgRow) -> Result<AdmissionApplication, sqlx::Error> {
    let given: String = r.try_get("given_name")?;
    let middle: Option<String> = r.try_get("middle_name")?;
    let family: Option<String> = r.try_get("family_name")?;
    let status: String = r.try_get("status")?;
    Ok(AdmissionApplication {
        id: AdmissionApplicationId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
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
        prior_qualification: r.try_get("prior_qualification")?,
        notes: r.try_get("notes")?,
        status: AdmissionApplicationStatus::parse(&status)
            .ok_or_else(|| sqlx::Error::Decode(format!("application status {status}").into()))?,
        status_reason: r.try_get("status_reason")?,
        submitted_at: r.try_get("submitted_at")?,
        decided_at: r.try_get("decided_at")?,
        admission_id: r
            .try_get::<Option<Uuid>, _>("admission_id")?
            .map(AdmissionId),
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: AdmissionApplicationId,
    lock: bool,
) -> Result<AdmissionApplication, KernelError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM student.admission_application WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "admission application",
        })?;
    from_row(&row).map_err(map_db_error)
}

/// Applicant details of a new application.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateApplication {
    /// Published programme of the institution.
    pub programme_id: Uuid,
    pub curriculum_version_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub given_name: String,
    pub middle_name: Option<String>,
    pub family_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    /// Login subject of the applicant (e.g. `dev|aarav.desai@sutra.dev`), carried to the student.
    pub subject_ref: Option<String>,
    pub prior_qualification: Option<String>,
    pub notes: Option<String>,
}

/// Editable applicant fields of a draft / submitted application (present = replace).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateApplication {
    pub programme_id: Option<Uuid>,
    pub curriculum_version_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub admission_term_id: Option<Uuid>,
    pub given_name: Option<String>,
    pub middle_name: Option<String>,
    pub family_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub subject_ref: Option<String>,
    pub prior_qualification: Option<String>,
    pub notes: Option<String>,
}

/// Body of `POST /admission-applications/{id}/offer`.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[schema(as = AdmissionOffer)]
pub struct OfferBody {
    /// Business-local last day the offer may be acted on (≥ today). Default: today + 14 days.
    pub offer_expires_on: Option<NaiveDate>,
    /// Document types required for verification (snake_case keys). Default:
    /// `identity_proof`, `qualifying_marksheet`.
    pub required_documents: Option<Vec<String>>,
    /// Curriculum version to admit on (default: the application's, else the one in force).
    pub curriculum_version_id: Option<Uuid>,
    pub reason: Option<String>,
}

/// Application collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct AdmissionApplicationList {
    pub items: Vec<AdmissionApplication>,
}

/// `?status=`.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct ApplicationListQuery {
    pub status: Option<AdmissionApplicationStatus>,
}

#[allow(clippy::too_many_arguments)]
fn validate_applicant(
    given: &str,
    middle: Option<&str>,
    family: Option<&str>,
    gender: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    subject: Option<&str>,
    prior: Option<&str>,
    notes: Option<&str>,
) -> Result<(), KernelError> {
    let mut v = vec![];
    validate_text("givenName", given, 100, &mut v);
    validate_opt_text("middleName", middle, 100, &mut v);
    validate_opt_text("familyName", family, 100, &mut v);
    validate_opt_text("gender", gender, 40, &mut v);
    validate_email("email", email, &mut v);
    validate_phone("phone", phone, &mut v);
    validate_opt_text("subjectRef", subject, 200, &mut v);
    validate_opt_text("priorQualification", prior, 500, &mut v);
    validate_opt_text("notes", notes, 2000, &mut v);
    finish(v)
}

/// Programme / curriculum / campus / term checks of an application.
async fn validate_refs(
    st: &StudentState,
    scope: &ExecutionScope,
    inst: InstitutionId,
    programme_id: Uuid,
    cv: Option<Uuid>,
    campus: Option<Uuid>,
    term: Option<Uuid>,
) -> Result<Option<Uuid>, KernelError> {
    refs::programme(st, scope, inst, programme_id, "programmeId").await?;
    if cv.is_some() {
        let today = business_today(st, scope, inst).await?;
        refs::curriculum(st, scope, programme_id, cv, today, "curriculumVersionId").await?;
    }
    refs::campus(st, scope, inst, campus, "campusId").await?;
    refs::term(st, scope, inst, term, "admissionTermId").await
}

async fn audit_app(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    command: &str,
    a: &AdmissionApplication,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::audit(
        conn,
        scope,
        &format!("student.admission_application.{command}"),
        "admission_application",
        a.id.to_string(),
        serde_json::json!({ "status": a.status, "version": a.version.0 }),
        actor,
    )
    .await
}

fn respond(status: StatusCode, a: &AdmissionApplication) -> Response {
    with_etag(status, a, a.version)
}

/// Record a new application in `draft` (`admission.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/institutions/{id}/admission-applications",
    operation_id = "admission_application_create",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateApplication,
    responses((status = 201, body = AdmissionApplication, headers(("ETag" = String))), CreateErrors)
)]
pub async fn create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<CreateApplication>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    require_institution_writable(&st, &scope, id).await?;
    validate_applicant(
        &cmd.given_name,
        cmd.middle_name.as_deref(),
        cmd.family_name.as_deref(),
        cmd.gender.as_deref(),
        cmd.email.as_deref(),
        cmd.phone.as_deref(),
        cmd.subject_ref.as_deref(),
        cmd.prior_qualification.as_deref(),
        cmd.notes.as_deref(),
    )?;
    let year = validate_refs(
        &st,
        &scope,
        id,
        cmd.programme_id,
        cmd.curriculum_version_id,
        cmd.campus_id,
        cmd.admission_term_id,
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.admission_application (tenant_id, institution_id, programme_id, \
         curriculum_version_id, campus_id, admission_term_id, admission_year_id, given_name, \
         middle_name, family_name, date_of_birth, gender, email, phone, subject_ref, \
         prior_qualification, notes) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17) \
         RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(tenant(ctx).as_uuid())
        .bind(id.as_uuid())
        .bind(cmd.programme_id)
        .bind(cmd.curriculum_version_id)
        .bind(cmd.campus_id)
        .bind(cmd.admission_term_id)
        .bind(year)
        .bind(cmd.given_name.trim())
        .bind(norm(cmd.middle_name.as_deref()))
        .bind(norm(cmd.family_name.as_deref()))
        .bind(cmd.date_of_birth)
        .bind(norm(cmd.gender.as_deref()))
        .bind(norm(cmd.email.as_deref()))
        .bind(norm(cmd.phone.as_deref()))
        .bind(norm(cmd.subject_ref.as_deref()))
        .bind(norm(cmd.prior_qualification.as_deref()))
        .bind(norm(cmd.notes.as_deref()))
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let app = from_row(&row).map_err(map_db_error)?;
    audit_app(&mut tx, &scope, "create", &app, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::CREATED, &app))
}

/// Applications of an institution (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/admission-applications",
    operation_id = "admission_application_list",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ApplicationListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionApplicationList), ReadErrors)
)]
pub async fn list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<ApplicationListQuery>,
) -> ApiResult<Json<AdmissionApplicationList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let sql = format!(
        "SELECT {COLUMNS} FROM student.admission_application WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::text IS NULL OR status = $3) \
         ORDER BY created_at DESC, id DESC LIMIT 500"
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
    Ok(Json(AdmissionApplicationList { items }))
}

/// One application (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/admission-applications/{id}",
    operation_id = "admission_application_get",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = AdmissionApplication, headers(("ETag" = String))), ReadErrors)
)]
pub async fn get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionApplicationId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let a = load(&mut conn, tenant(ctx), id, false).await?;
    Ok(respond(StatusCode::OK, &a))
}

/// Edit a `draft` or `submitted` application (`admission.manage`).
#[utoipa::path(
    patch,
    path = "/api/v1/student/admission-applications/{id}",
    operation_id = "admission_application_update",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateApplication,
    responses((status = 200, body = AdmissionApplication, headers(("ETag" = String))), CommandErrors)
)]
pub async fn update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionApplicationId>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateApplication>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, tenant(ctx), id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    if !matches!(
        cur.status,
        AdmissionApplicationStatus::Draft | AdmissionApplicationStatus::Submitted
    ) {
        return Err(KernelError::Conflict(format!(
            "application is {}; only draft or submitted applications can be edited",
            cur.status
        ))
        .into());
    }
    let pick = |new: &Option<String>, old: &Option<String>| match new {
        Some(n) => norm(Some(n)),
        None => old.clone(),
    };
    let given = cmd.given_name.clone().unwrap_or(cur.given_name.clone());
    let middle = pick(&cmd.middle_name, &cur.middle_name);
    let family = pick(&cmd.family_name, &cur.family_name);
    let gender = pick(&cmd.gender, &cur.gender);
    let email = pick(&cmd.email, &cur.email);
    let phone = pick(&cmd.phone, &cur.phone);
    let subject = pick(&cmd.subject_ref, &cur.subject_ref);
    let prior = pick(&cmd.prior_qualification, &cur.prior_qualification);
    let notes = pick(&cmd.notes, &cur.notes);
    validate_applicant(
        &given,
        middle.as_deref(),
        family.as_deref(),
        gender.as_deref(),
        email.as_deref(),
        phone.as_deref(),
        subject.as_deref(),
        prior.as_deref(),
        notes.as_deref(),
    )?;
    let programme = cmd.programme_id.unwrap_or(cur.programme_id);
    let cv = cmd.curriculum_version_id.or(cur.curriculum_version_id);
    let campus = cmd.campus_id.or(cur.campus_id);
    let term = cmd.admission_term_id.or(cur.admission_term_id);
    let year = validate_refs(&st, &scope, cur.institution_id, programme, cv, campus, term).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = load(&mut tx, tenant(ctx), id, true).await?;
    check_version(expected, locked.version.0)?;
    let sql = format!(
        "UPDATE student.admission_application SET programme_id = $3, curriculum_version_id = $4, \
         campus_id = $5, admission_term_id = $6, admission_year_id = $7, given_name = $8, \
         middle_name = $9, family_name = $10, date_of_birth = $11, gender = $12, email = $13, \
         phone = $14, subject_ref = $15, prior_qualification = $16, notes = $17, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant(ctx).as_uuid())
        .bind(programme)
        .bind(cv)
        .bind(campus)
        .bind(term)
        .bind(year)
        .bind(given.trim())
        .bind(middle)
        .bind(family)
        .bind(cmd.date_of_birth.or(cur.date_of_birth))
        .bind(gender)
        .bind(email)
        .bind(phone)
        .bind(subject)
        .bind(prior)
        .bind(notes)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let app = from_row(&row).map_err(map_db_error)?;
    audit_app(&mut tx, &scope, "update", &app, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &app))
}

/// Moves a locked application to `to` (version bump) and emits `event` (when given).
#[allow(clippy::too_many_arguments)]
async fn transition(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    cur: &AdmissionApplication,
    to: AdmissionApplicationStatus,
    reason: Option<String>,
    event: Option<(&'static str, &'static str)>,
    command: &str,
    actor: &Actor,
) -> Result<AdmissionApplication, KernelError> {
    if !application_can_transition(cur.status, to) {
        return Err(KernelError::Conflict(format!(
            "application is {}; it cannot become {to}",
            cur.status
        )));
    }
    let sql = format!(
        "UPDATE student.admission_application SET status = $3, status_reason = $4, \
         submitted_at = CASE WHEN $3 = 'submitted' THEN now() ELSE submitted_at END, \
         decided_at = CASE WHEN $3 IN ('offered', 'rejected') THEN now() ELSE decided_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(cur.id.as_uuid())
        .bind(scope.tenant_id.as_uuid())
        .bind(to.as_str())
        .bind(reason.as_deref())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let app = from_row(&row).map_err(map_db_error)?;
    if let Some((t, schema)) = event {
        publish::event(
            conn,
            scope,
            t,
            schema,
            app.id.to_string(),
            &status_changed(
                app.id.as_uuid(),
                app.institution_id,
                None,
                Some(cur.status.as_str()),
                to.as_str(),
                app.version.0,
                reason,
                None,
                app.subject_ref.clone(),
            ),
            actor,
        )
        .await?;
    }
    audit_app(conn, scope, command, &app, actor).await?;
    Ok(app)
}

/// Shared shape of the simple application commands.
#[allow(clippy::too_many_arguments)]
async fn simple_command(
    st: &StudentState,
    auth: &Authenticated,
    id: AdmissionApplicationId,
    headers: &HeaderMap,
    to: AdmissionApplicationStatus,
    reason: Option<String>,
    event: Option<(&'static str, &'static str)>,
    command: &str,
    expected: Version,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let _ = headers;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, tenant(ctx), id, false).await?;
    drop(conn);
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(st, &scope, cur.institution_id).await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = load(&mut tx, tenant(ctx), id, true).await?;
    check_version(expected, locked.version.0)?;
    let app = transition(&mut tx, &scope, &locked, to, reason, event, command, &a).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &app))
}

/// Submit a draft application (`admission.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/admission-applications/{id}/submit",
    operation_id = "admission_application_submit",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = AdmissionApplication, headers(("ETag" = String))), CommandErrors)
)]
pub async fn submit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionApplicationId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    simple_command(
        &st,
        &auth,
        id,
        &headers,
        AdmissionApplicationStatus::Submitted,
        reason,
        Some((
            ADMISSION_APPLICATION_SUBMITTED,
            ADMISSION_APPLICATION_SUBMITTED_SCHEMA,
        )),
        "submit",
        expected,
    )
    .await
}

/// Start the review of a submitted application (`admission.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/admission-applications/{id}/review",
    operation_id = "admission_application_review",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
    responses((status = 200, body = AdmissionApplication, headers(("ETag" = String))), CommandErrors)
)]
pub async fn review(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionApplicationId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().reason()?;
    simple_command(
        &st,
        &auth,
        id,
        &headers,
        AdmissionApplicationStatus::UnderReview,
        reason,
        None,
        "review",
        expected,
    )
    .await
}

/// Reject a submitted / under-review application (`admission.manage`; reason required).
#[utoipa::path(
    post,
    path = "/api/v1/student/admission-applications/{id}/reject",
    operation_id = "admission_application_reject",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = AdmissionApplication, headers(("ETag" = String))), CommandErrors)
)]
pub async fn reject(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionApplicationId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().required_reason()?;
    simple_command(
        &st,
        &auth,
        id,
        &headers,
        AdmissionApplicationStatus::Rejected,
        Some(reason),
        Some((
            ADMISSION_APPLICATION_REJECTED,
            ADMISSION_APPLICATION_REJECTED_SCHEMA,
        )),
        "reject",
        expected,
    )
    .await
}

/// Withdraw an application before an offer (`admission.manage`; reason required).
#[utoipa::path(
    post,
    path = "/api/v1/student/admission-applications/{id}/withdraw",
    operation_id = "admission_application_withdraw",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CommandBody,
    responses((status = 200, body = AdmissionApplication, headers(("ETag" = String))), CommandErrors)
)]
pub async fn withdraw(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionApplicationId>,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
) -> ApiResult<Response> {
    auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let reason = body.map(|b| b.0).unwrap_or_default().required_reason()?;
    simple_command(
        &st,
        &auth,
        id,
        &headers,
        AdmissionApplicationStatus::Withdrawn,
        Some(reason),
        Some((
            ADMISSION_APPLICATION_WITHDRAWN,
            ADMISSION_APPLICATION_WITHDRAWN_SCHEMA,
        )),
        "withdraw",
        expected,
    )
    .await
}

/// Validates required document keys (deduplicated, at most 20).
pub fn required_documents(given: Option<&[String]>) -> Result<Vec<String>, KernelError> {
    let list: Vec<String> = match given {
        Some(l) => l.iter().map(|s| s.trim().to_owned()).collect(),
        None => DEFAULT_REQUIRED_DOCUMENTS
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    };
    let mut v = vec![];
    for k in &list {
        validate_key("requiredDocuments", k, &mut v);
    }
    finish(v)?;
    let mut out: Vec<String> = vec![];
    for k in list {
        if !out.contains(&k) {
            out.push(k);
        }
    }
    if out.len() > 20 {
        return Err(invalid(
            "requiredDocuments",
            "too-many",
            "at most 20 required document types",
        ));
    }
    Ok(out)
}

/// Offer admission: the application becomes `offered` and an Admission Record is **initiated**
/// in the same transaction (`AdmissionInitiated`, `admission.manage`). Returns the application
/// (its `admissionId` names the new record).
#[utoipa::path(
    post,
    path = "/api/v1/student/admission-applications/{id}/offer",
    operation_id = "admission_application_offer",
    tag = "admissions",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<OfferBody>, description = "Offer terms"),
    responses((status = 200, body = AdmissionApplication, headers(("ETag" = String))), CommandErrors)
)]
pub async fn offer(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<AdmissionApplicationId>,
    headers: HeaderMap,
    body: Option<Json<OfferBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AdmissionManage)?;
    let expected = expect_version!(headers);
    let body = body.map(|b| b.0).unwrap_or_default();
    let reason = CommandBody {
        reason: body.reason.clone(),
    }
    .reason()?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let cur = load(&mut conn, tenant(ctx), id, false).await?;
    drop(conn);
    let inst = cur.institution_id;
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let expires = body
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
    let required = required_documents(body.required_documents.as_deref())?;
    refs::programme(&st, &scope, inst, cur.programme_id, "programmeId").await?;
    let cv = refs::curriculum(
        &st,
        &scope,
        cur.programme_id,
        body.curriculum_version_id.or(cur.curriculum_version_id),
        today,
        "curriculumVersionId",
    )
    .await?;
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = load(&mut tx, tenant(ctx), id, true).await?;
    check_version(expected, locked.version.0)?;
    if !application_can_transition(locked.status, AdmissionApplicationStatus::Offered) {
        return Err(KernelError::Conflict(format!(
            "application is {}; it cannot be offered",
            locked.status
        ))
        .into());
    }
    let admission = adm::insert(
        &mut tx,
        &scope,
        adm::NewAdmission {
            application_id: Some(locked.id),
            programme_id: locked.programme_id,
            curriculum_version_id: cv.id,
            campus_id: locked.campus_id,
            admission_term_id: locked.admission_term_id,
            admission_year_id: locked.admission_year_id,
            given_name: locked.given_name.clone(),
            middle_name: locked.middle_name.clone(),
            family_name: locked.family_name.clone(),
            date_of_birth: locked.date_of_birth,
            gender: locked.gender.clone(),
            email: locked.email.clone(),
            phone: locked.phone.clone(),
            subject_ref: locked.subject_ref.clone(),
            offer_expires_on: Some(expires),
            required_documents: required,
            enrolment_type: "regular",
            student_id: None,
            previous_admission_id: None,
        },
    )
    .await?;
    admission_history(
        &mut tx,
        &scope,
        admission.id,
        None,
        AdmissionStatus::Initiated,
        reason.as_deref(),
        None,
        &a,
    )
    .await?;
    sqlx::query("UPDATE student.admission_application SET admission_id = $2 WHERE id = $1")
        .bind(locked.id.as_uuid())
        .bind(admission.id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let app = transition(
        &mut tx,
        &scope,
        &locked,
        AdmissionApplicationStatus::Offered,
        reason,
        Some((
            ADMISSION_APPLICATION_OFFERED,
            ADMISSION_APPLICATION_OFFERED_SCHEMA,
        )),
        "offer",
        &a,
    )
    .await?;
    publish::event(
        &mut tx,
        &scope,
        ADMISSION_INITIATED,
        ADMISSION_INITIATED_SCHEMA,
        admission.id.to_string(),
        &AdmissionInitiatedV1 {
            admission_id: admission.id.as_uuid(),
            institution_id: inst.as_uuid(),
            application_id: Some(locked.id.as_uuid()),
            programme_id: admission.programme_id,
            curriculum_version_id: Some(admission.curriculum_version_id),
            admission_term_id: admission.admission_term_id,
            offer_expires_on: admission.offer_expires_on,
            student_id: None,
            previous_admission_id: None,
        },
        &a,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "student.admission.initiate",
        "admission",
        admission.id.to_string(),
        serde_json::json!({ "applicationId": locked.id, "offerExpiresOn": expires }),
        &a,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(respond(StatusCode::OK, &app))
}
