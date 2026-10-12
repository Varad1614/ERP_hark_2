//! HTTP handlers of academic registration (`tag = "registration"`; operation ids
//! `registration_window_*`, `registration_*`, `registration_hold_*`, `registration_self_*`).

use super::domain::*;
use super::repo;
use super::support::{body, read_student, standing_of, StepDecision, StepDecisionBody, StepReason};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::student::me;
use crate::shared::StudentState;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sutra_contracts::institution::reference::CurriculumComponentRef;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::{
    AcademicStanding, RegistrationHoldStatus, RegistrationLineKind, RegistrationStatus,
    RegistrationWindowStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WindowQuery {
    #[param(inline)]
    pub status: Option<RegistrationWindowStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RegistrationQuery {
    #[param(inline)]
    pub status: Option<RegistrationStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct HoldQuery {
    #[param(inline)]
    pub status: Option<RegistrationHoldStatus>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct OptionsQuery {
    /// Term to check against an accepting window (optional).
    pub academic_term_id: Option<Uuid>,
}

fn etag_reg(status: StatusCode, r: &Registration) -> Response {
    with_etag(status, r, sutra_kernel::concurrency::Version(r.version))
}
fn etag_win(status: StatusCode, w: &RegistrationWindow) -> Response {
    with_etag(status, w, sutra_kernel::concurrency::Version(w.version))
}
fn etag_hold(status: StatusCode, h: &RegistrationHold) -> Response {
    with_etag(status, h, sutra_kernel::concurrency::Version(h.version))
}

macro_rules! expect_version {
    ($headers:expr) => {
        match if_match(&$headers)? {
            Ok(v) => v,
            Err(res) => return Ok(res),
        }
    };
}

// ================================================================== windows

/// Schedule a registration window for a term (L3-C-020, `registration.manage`). Dates default
/// to the calendar's `registration_window` key date.
#[utoipa::path(
    post,
    path = "/api/v1/student/institutions/{id}/registration-windows",
    operation_id = "registration_window_create",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateWindow,
    responses(
        (status = 201, body = RegistrationWindow, headers(("ETag" = String))),
        (status = 400, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "A live window exists for the term, or the institution is read-only", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn window_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<CreateWindow>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    let w = repo::create_window(&st, &scope, id, cmd, &actor(ctx)).await?;
    Ok(etag_win(StatusCode::CREATED, &w))
}

/// Registration windows of an institution (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/institutions/{id}/registration-windows",
    operation_id = "registration_window_list",
    tag = "registration",
    params(("id" = String, Path, format = Uuid, description = "Institution id"), WindowQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RegistrationWindowList),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn window_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<WindowQuery>,
) -> ApiResult<Json<RegistrationWindowList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let items = repo::list_windows(&st, &institution_scope(ctx, id), id, q.status).await?;
    Ok(Json(RegistrationWindowList { items }))
}

/// One registration window (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/registration-windows/{id}",
    operation_id = "registration_window_get",
    tag = "registration",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RegistrationWindow, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn window_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(etag_win(
        StatusCode::OK,
        &repo::get_window(&st.pool, tenant(ctx), id).await?,
    ))
}

/// Reschedule a window (scheduled: any field; open: `closesOn`, name and credit limits).
#[utoipa::path(
    patch,
    path = "/api/v1/student/registration-windows/{id}",
    operation_id = "registration_window_update",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the window"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateWindow,
    responses(
        (status = 200, body = RegistrationWindow, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn window_update(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateWindow>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let w = repo::update_window(&st, tenant(ctx), id, v, cmd, &actor(ctx)).await?;
    Ok(etag_win(StatusCode::OK, &w))
}

macro_rules! window_command {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $to:expr, $reason_required:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "registration",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the window"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<StepReason>),
            responses(
                (status = 200, body = RegistrationWindow, headers(("ETag" = String))),
                (status = 403, body = Problem, content_type = "application/problem+json"),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, description = "Illegal transition or prerequisites not met", body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State(st): State<StudentState>,
            auth: Authenticated,
            Path(id): Path<Uuid>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::RegistrationManage)?;
            let v = expect_version!(headers);
            let reason = body::<StepReason>(&raw)?.validated($reason_required)?;
            let w = repo::transition_window(&st, tenant(ctx), id, v, $to, reason, &actor(ctx)).await?;
            Ok(etag_win(StatusCode::OK, &w))
        }
    };
}

window_command!(
    window_open,
    "/api/v1/student/registration-windows/{id}/open",
    "registration_window_open",
    "Open a scheduled window (published calendar term, business date inside the window).",
    RegistrationWindowStatus::Open,
    false
);
window_command!(
    window_close,
    "/api/v1/student/registration-windows/{id}/close",
    "registration_window_close",
    "Close an open window; no further registration processing goes through it.",
    RegistrationWindowStatus::Closed,
    false
);
window_command!(
    window_cancel,
    "/api/v1/student/registration-windows/{id}/cancel",
    "registration_window_cancel",
    "Cancel a scheduled window (reason required).",
    RegistrationWindowStatus::Cancelled,
    true
);

/// Finalise (freeze + publish) every registered registration of a closed window (L3-C-025).
#[utoipa::path(
    post,
    path = "/api/v1/student/registration-windows/{id}/finalise",
    operation_id = "registration_window_finalise",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the window"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = WindowFinalisation),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn window_finalise(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let f = repo::finalise_window(&st, tenant(ctx), id, v, &actor(ctx)).await?;
    let version = sutra_kernel::concurrency::Version(f.window.version);
    Ok(with_etag(StatusCode::OK, &f, version))
}

// ================================================================== registration options

/// One curriculum component offered for registration.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationOption {
    pub curriculum_component_id: Uuid,
    pub term_no: i32,
    pub slot_code: String,
    pub title: String,
    pub category: String,
    pub credits: f64,
    /// Credits already earned in the ledger.
    pub earned: bool,
    /// Kind the component would be registered as (`regular`, `backlog`; `repeat` when it was
    /// registered before and not earned).
    pub suggested_kind: Option<RegistrationLineKind>,
}

/// What a student may register (curriculum components + current position).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationOptions {
    pub student_id: Uuid,
    pub student_context_id: Option<Uuid>,
    pub institution_id: Option<Uuid>,
    pub term_no: Option<i32>,
    pub curriculum_version_id: Option<Uuid>,
    pub standing: AcademicStanding,
    pub components: Vec<RegistrationOption>,
    /// Open/scheduled windows of the institution.
    pub windows: Vec<RegistrationWindow>,
    /// Active holds.
    pub active_holds: Vec<RegistrationHold>,
}

async fn options_for(
    st: &StudentState,
    ctx: &AuthContext,
    student_id: StudentId,
) -> Result<RegistrationOptions, KernelError> {
    let t = tenant(ctx);
    let (s, c) = read_student(st, t, student_id).await?;
    let holds = repo::list_holds(st, t, student_id, Some(RegistrationHoldStatus::Active)).await?;
    let Some(c) = c else {
        return Ok(RegistrationOptions {
            student_id: s.id.as_uuid(),
            student_context_id: None,
            institution_id: None,
            term_no: None,
            curriculum_version_id: None,
            standing: AcademicStanding::GoodStanding,
            components: vec![],
            windows: vec![],
            active_holds: holds.items,
        });
    };
    let scope = ExecutionScope::institution(t, c.institution_id);
    let standing = standing_of(&st.pool, t, c.id.as_uuid()).await?;
    let comps: Vec<CurriculumComponentRef> = match c.curriculum_version_id {
        Some(cv) => st.references.curriculum_components(&scope, cv).await?,
        None => vec![],
    };
    let earned: Vec<Uuid> = sqlx::query_scalar(
        "SELECT curriculum_component_id FROM student.credit_ledger_entry WHERE tenant_id = $1 \
         AND student_context_id = $2 AND curriculum_component_id IS NOT NULL GROUP BY 1 \
         HAVING sum(CASE WHEN kind IN ('revoked', 'expired') THEN -credits ELSE credits END) > 0",
    )
    .bind(t.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(sutra_persistence::map_db_error)?;
    let prev: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT l.curriculum_component_id FROM student.registration_line l JOIN \
         student.registration r ON r.id = l.registration_id WHERE r.tenant_id = $1 AND \
         r.student_context_id = $2 AND r.status IN ('finalised', 'archived') AND l.status = 'registered'",
    )
    .bind(t.as_uuid())
    .bind(c.id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(sutra_persistence::map_db_error)?;
    let term_no = c.current_term_no.unwrap_or(1);
    let components = comps
        .into_iter()
        .filter(|k| k.term_no <= term_no)
        .map(|k| {
            let is_earned = earned.contains(&k.id);
            let suggested_kind = if is_earned {
                None
            } else if prev.contains(&k.id) {
                Some(RegistrationLineKind::Repeat)
            } else if k.term_no < term_no {
                Some(RegistrationLineKind::Backlog)
            } else {
                Some(RegistrationLineKind::Regular)
            };
            RegistrationOption {
                curriculum_component_id: k.id,
                term_no: k.term_no,
                slot_code: k.slot_code,
                title: k.title,
                category: k.category,
                credits: k.credits,
                earned: is_earned,
                suggested_kind,
            }
        })
        .collect();
    let windows = repo::open_windows_for(&st.pool, t, c.institution_id.as_uuid()).await?;
    Ok(RegistrationOptions {
        student_id: s.id.as_uuid(),
        student_context_id: Some(c.id.as_uuid()),
        institution_id: Some(c.institution_id.as_uuid()),
        term_no: Some(term_no),
        curriculum_version_id: c.curriculum_version_id,
        standing,
        components,
        windows,
        active_holds: holds.items,
    })
}

/// Registration options of a student: curriculum components, windows, holds, standing.
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/registrations/options",
    operation_id = "registration_options",
    tag = "registration",
    params(("id" = String, Path, format = Uuid, description = "Student id")),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RegistrationOptions),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn options(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
) -> ApiResult<Json<RegistrationOptions>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(options_for(&st, ctx, id).await?))
}

// ================================================================== registrations (staff)

/// Start a registration (draft) for the student's live context in an open window (L3-C-021).
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/registrations",
    operation_id = "registration_create",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid, description = "Student id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateRegistration,
    responses(
        (status = 201, body = Registration, headers(("ETag" = String))),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Window not open, hold, dues, standing, inactive context, or a registration already exists", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<CreateRegistration>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    require_idempotency_key(&headers)?;
    let r = repo::create_registration(&st, tenant(ctx), id, cmd, "staff", &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::CREATED, &r))
}

/// Registrations of a student, newest first (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/registrations",
    operation_id = "registration_list",
    tag = "registration",
    params(("id" = String, Path, format = Uuid, description = "Student id"), RegistrationQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RegistrationList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<RegistrationQuery>,
) -> ApiResult<Json<RegistrationList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let items = repo::list_registrations(&st.pool, tenant(ctx), id, q.status).await?;
    Ok(Json(RegistrationList { items }))
}

/// One registration with its lines (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/registrations/{id}",
    operation_id = "registration_get",
    tag = "registration",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(etag_reg(
        StatusCode::OK,
        &repo::get_registration(&st.pool, tenant(ctx), id).await?,
    ))
}

/// Immutable lifecycle history of a registration (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/registrations/{id}/history",
    operation_id = "registration_history",
    tag = "registration",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = repo::RegistrationHistoryList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn history(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<repo::RegistrationHistoryList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    let items = repo::registration_history(&st.pool, tenant(ctx), id).await?;
    Ok(Json(repo::RegistrationHistoryList { items }))
}

/// Add a component (draft/returned, or add/drop after registration while the window is open).
#[utoipa::path(
    post,
    path = "/api/v1/student/registrations/{id}/lines",
    operation_id = "registration_add_line",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = LineInput,
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn add_line(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<LineInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let r = repo::add_line(&st, tenant(ctx), id, v, input, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Drop a component (kept as `dropped` for history).
#[utoipa::path(
    delete,
    path = "/api/v1/student/registrations/{id}/lines/{lineId}",
    operation_id = "registration_drop_line",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("lineId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<StepReason>),
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn drop_line(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, line_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let reason = body::<StepReason>(&raw)?.validated(false)?;
    let r = repo::drop_line(&st, tenant(ctx), id, line_id, v, reason, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Submit for approval (WorkflowPort `student.registration_approval`; auto-approved in Batch 3
/// → `registered`).
#[utoipa::path(
    post,
    path = "/api/v1/student/registrations/{id}/submit",
    operation_id = "registration_submit",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "No lines or credits outside the limits", body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn submit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let r = repo::submit(&st, tenant(ctx), id, v, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Decide a pending registration approval (approve → registered; reject → returned).
#[utoipa::path(
    post,
    path = "/api/v1/student/registrations/{id}/decision",
    operation_id = "registration_decision",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decision(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let reason = StepReason { reason: d.reason }.validated(false)?;
    let r = repo::decide(
        &st,
        tenant(ctx),
        id,
        v,
        d.decision == StepDecision::Approve,
        reason,
        &actor(ctx),
    )
    .await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Cancel a registration (L3-C-023, reason required).
#[utoipa::path(
    post,
    path = "/api/v1/student/registrations/{id}/cancel",
    operation_id = "registration_cancel",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepReason,
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn cancel(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let reason = body::<StepReason>(&raw)?.validated(true)?;
    let r = repo::cancel(&st, tenant(ctx), id, v, reason, false, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Finalise (freeze + publish) a registered registration (L3-C-025).
#[utoipa::path(
    post,
    path = "/api/v1/student/registrations/{id}/finalise",
    operation_id = "registration_finalise",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn finalise(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let r = repo::finalise(&st, tenant(ctx), id, v, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

// ================================================================== holds

/// Place a registration hold (L3-C-026, `registration.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/student/students/{id}/holds",
    operation_id = "registration_hold_place",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid, description = "Student id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = PlaceHold,
    responses(
        (status = 201, body = RegistrationHold, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn hold_place(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    headers: HeaderMap,
    Json(cmd): Json<PlaceHold>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    require_idempotency_key(&headers)?;
    let h = repo::place_hold(&st, tenant(ctx), id, cmd, &actor(ctx)).await?;
    Ok(etag_hold(StatusCode::CREATED, &h))
}

/// Holds of a student plus the live Finance evidence (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/students/{id}/holds",
    operation_id = "registration_hold_list",
    tag = "registration",
    params(("id" = String, Path, format = Uuid, description = "Student id"), HoldQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = HoldList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn hold_list(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<StudentId>,
    Query(q): Query<HoldQuery>,
) -> ApiResult<Json<HoldList>> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(Json(
        repo::list_holds(&st, tenant(ctx), id, q.status).await?,
    ))
}

/// One hold (`student.read`).
#[utoipa::path(
    get,
    path = "/api/v1/student/registration-holds/{id}",
    operation_id = "registration_hold_get",
    tag = "registration",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RegistrationHold, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn hold_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StudentRead)?;
    Ok(etag_hold(
        StatusCode::OK,
        &repo::get_hold(&st.pool, tenant(ctx), id).await?,
    ))
}

/// Release an active hold (reason required; a finance hold needs Finance to permit clearance).
#[utoipa::path(
    post,
    path = "/api/v1/student/registration-holds/{id}/release",
    operation_id = "registration_hold_release",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the hold"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepReason,
    responses(
        (status = 200, body = RegistrationHold, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn hold_release(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationManage)?;
    let v = expect_version!(headers);
    let reason = body::<StepReason>(&raw)?
        .validated(true)?
        .unwrap_or_default();
    let h = repo::release_hold(&st, tenant(ctx), id, v, reason, &actor(ctx)).await?;
    Ok(etag_hold(StatusCode::OK, &h))
}

// ================================================================== self-service

async fn my_registration(
    st: &StudentState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<Registration, KernelError> {
    let s = me(&st.pool, ctx).await?;
    let r = repo::get_registration(&st.pool, tenant(ctx), id).await?;
    if r.student_id != s.id.as_uuid() {
        return Err(KernelError::NotFound {
            entity: "registration",
        });
    }
    Ok(r)
}

/// My registration options: components, windows, holds and standing (`registration.self`).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/registration-windows",
    operation_id = "registration_self_options",
    tag = "registration",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RegistrationOptions),
        (status = 404, description = "The account has no student record", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_options(
    State(st): State<StudentState>,
    auth: Authenticated,
) -> ApiResult<Json<RegistrationOptions>> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    let s = me(&st.pool, ctx).await?;
    Ok(Json(options_for(&st, ctx, s.id).await?))
}

/// My registrations (`registration.self`).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/registrations",
    operation_id = "registration_self_list",
    tag = "registration",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = RegistrationList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_list(
    State(st): State<StudentState>,
    auth: Authenticated,
) -> ApiResult<Json<RegistrationList>> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    let s = me(&st.pool, ctx).await?;
    let items = repo::list_registrations(&st.pool, tenant(ctx), s.id, None).await?;
    Ok(Json(RegistrationList { items }))
}

/// Register myself for a term in an open window (`registration.self`).
#[utoipa::path(
    post,
    path = "/api/v1/student/me/registrations",
    operation_id = "registration_self_create",
    tag = "registration",
    params(
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateRegistration,
    responses(
        (status = 201, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_create(
    State(st): State<StudentState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<CreateRegistration>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    require_idempotency_key(&headers)?;
    let s = me(&st.pool, ctx).await?;
    let r = repo::create_registration(&st, tenant(ctx), s.id, cmd, "self", &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::CREATED, &r))
}

/// One of my registrations (`registration.self`).
#[utoipa::path(
    get,
    path = "/api/v1/student/me/registrations/{id}",
    operation_id = "registration_self_get",
    tag = "registration",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_get(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    Ok(etag_reg(
        StatusCode::OK,
        &my_registration(&st, ctx, id).await?,
    ))
}

/// Add a component to my registration (open window).
#[utoipa::path(
    post,
    path = "/api/v1/student/me/registrations/{id}/lines",
    operation_id = "registration_self_add_line",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = LineInput,
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_add_line(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(input): Json<LineInput>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    let v = expect_version!(headers);
    my_registration(&st, ctx, id).await?;
    let r = repo::add_line(&st, tenant(ctx), id, v, input, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Drop a component from my registration (open window).
#[utoipa::path(
    delete,
    path = "/api/v1/student/me/registrations/{id}/lines/{lineId}",
    operation_id = "registration_self_drop_line",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("lineId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<StepReason>),
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_drop_line(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path((id, line_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    let v = expect_version!(headers);
    my_registration(&st, ctx, id).await?;
    let reason = body::<StepReason>(&raw)?.validated(false)?;
    let r = repo::drop_line(&st, tenant(ctx), id, line_id, v, reason, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Submit my registration for approval.
#[utoipa::path(
    post,
    path = "/api/v1/student/me/registrations/{id}/submit",
    operation_id = "registration_self_submit",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_submit(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    let v = expect_version!(headers);
    my_registration(&st, ctx, id).await?;
    let r = repo::submit(&st, tenant(ctx), id, v, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}

/// Cancel my registration while the window is open (L3-C-023).
#[utoipa::path(
    post,
    path = "/api/v1/student/me/registrations/{id}/cancel",
    operation_id = "registration_self_cancel",
    tag = "registration",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the registration"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepReason,
    responses(
        (status = 200, body = Registration, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_cancel(
    State(st): State<StudentState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::RegistrationSelf)?;
    let v = expect_version!(headers);
    my_registration(&st, ctx, id).await?;
    let reason = body::<StepReason>(&raw)?.validated(true)?;
    let r = repo::cancel(&st, tenant(ctx), id, v, reason, true, &actor(ctx)).await?;
    Ok(etag_reg(StatusCode::OK, &r))
}
