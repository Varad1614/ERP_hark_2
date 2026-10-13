//! Student self-service registration (`exam.self`, L3-I-017 candidate registration →
//! confirmed registration). The student is resolved from the session subject; the client never
//! sends a student or registration id it could not own (`shared::me`).

use super::base::{
    candidate, check_eligibility, has_override, load_registration, raise_late_fee, register,
    registration_view, today_for, window_of, ExamRegistrationList, ExamRegistrationPaperInput,
    ExamRegistrationView, Register, Window,
};
use super::staff::withdraw_in_tx;
use crate::setup::common::{reason_of, require_reason, ReasonBody};
use crate::setup::policy::rules_for;
use crate::setup::session::{load_session, papers_of};
use crate::shared::guard::{require_institution_writable, require_operational};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::me::{my_registrations, require_own_registration};
use crate::shared::rows::lock_registration;
use crate::shared::scope::{institution_scope, tenant_scope};
use crate::shared::ExamsState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sutra_contracts::exams::status::{ExamPaperStatus, RegistrationChannel};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// Self-service registration.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSelfRegistrationCreate {
    pub session_id: Uuid,
    /// Empty = every paper the term registration covers.
    #[serde(default)]
    pub papers: Vec<ExamRegistrationPaperInput>,
}

/// A paper the caller may register for.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSelfOptionPaper {
    pub paper_id: Uuid,
    pub paper_code: String,
    pub title: String,
    #[schema(example = "4.00")]
    pub credits: String,
    pub covered: bool,
}

/// An operational session open to the caller.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSelfOption {
    pub session_id: Uuid,
    pub code: String,
    pub name: String,
    pub registration_opens_on: Option<NaiveDate>,
    pub registration_closes_on: Option<NaiveDate>,
    /// `not_yet_open | open | closed | override`.
    pub window: String,
    pub registered: bool,
    pub papers: Vec<ExamSelfOptionPaper>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamSelfOptionList {
    pub items: Vec<ExamSelfOption>,
}

/// The caller's own registrations (`exam.self`).
#[utoipa::path(get, path = "/api/v1/exams/me/exam-registrations", operation_id = "exam_registration_self_list", tag = "exam-registration",
    security(("session_cookie" = [])),
    responses((status = 200, body = ExamRegistrationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_registration_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<ExamRegistrationList>> {
    let ctx = &auth.0;
    let regs = my_registrations(&st, ctx).await?;
    let mut items = Vec::with_capacity(regs.len());
    for r in regs {
        items.push(registration_view(&st, ctx.scope.tenant_id, r).await?);
    }
    Ok(Json(ExamRegistrationList {
        items,
        next_cursor: None,
    }))
}

/// Operational sessions the caller can register for, with the papers their term registration
/// covers (`exam.self`).
#[utoipa::path(get, path = "/api/v1/exams/me/exam-registrations/options", operation_id = "exam_registration_self_options", tag = "exam-registration",
    security(("session_cookie" = [])),
    responses((status = 200, body = ExamSelfOptionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_registration_options(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<ExamSelfOptionList>> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    let tenant = ctx.scope.tenant_id;
    let tscope = tenant_scope(ctx);
    let Some(student) = st.students.by_subject(&tscope, &ctx.subject).await? else {
        return Ok(Json(ExamSelfOptionList { items: Vec::new() }));
    };
    let sessions: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.exam_session WHERE tenant_id = $1 AND status IN ('active', 'reopened') \
         ORDER BY starts_on NULLS LAST, code",
    )
    .bind(tenant.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mine = my_registrations(&st, ctx).await?;
    let mut items = Vec::new();
    for sid in sessions {
        let s = load_session(&st.pool, tenant, sid).await?;
        let scope = institution_scope(ctx, s.institution_id);
        if st
            .students
            .live_context(&scope, student.id, s.institution_id)
            .await?
            .is_none()
        {
            continue;
        }
        let term = st
            .registrations
            .term_registration(&scope, student.id, s.academic_term_id)
            .await?;
        let today = today_for(&st, &scope, &s).await?;
        let window = match window_of(&s, today) {
            Window::NotYetOpen => "not_yet_open",
            Window::Open => "open",
            Window::Closed => {
                if has_override(&st, tenant, sid, student.id.as_uuid(), today)
                    .await?
                    .is_some()
                {
                    "override"
                } else {
                    "closed"
                }
            }
        };
        let papers = papers_of(&st.pool, tenant, sid)
            .await?
            .into_iter()
            .filter(|p| p.status != ExamPaperStatus::Cancelled)
            .map(|p| ExamSelfOptionPaper {
                paper_id: p.id.as_uuid(),
                covered: term
                    .as_ref()
                    .is_some_and(|t| t.covers(p.curriculum_component_id)),
                paper_code: p.paper_code,
                title: p.title,
                credits: crate::shared::grading::fmt2(p.credits),
            })
            .collect();
        items.push(ExamSelfOption {
            session_id: sid,
            code: s.code.clone(),
            name: s.name.clone(),
            registration_opens_on: s.registration_opens_on,
            registration_closes_on: s.registration_closes_on,
            window: window.into(),
            registered: mine
                .iter()
                .any(|r| r.session_id == s.id && r.status.is_live()),
            papers,
        });
    }
    Ok(Json(ExamSelfOptionList { items }))
}

/// Register oneself for an operational session (`exam.self`): inside the window (or with an
/// approved deadline override, channel `late` + late fee); papers must be covered by the
/// student's term registration; ineligible papers record a `rejected` registration.
#[utoipa::path(post, path = "/api/v1/exams/me/exam-registrations", operation_id = "exam_registration_self_create", tag = "exam-registration",
    params(("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamSelfRegistrationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_registration_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<ExamSelfRegistrationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSelfService)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let student = st
        .students
        .by_subject(&tenant_scope(ctx), &ctx.subject)
        .await?
        .ok_or(KernelError::NotFound { entity: "student" })?;
    let s = load_session(&st.pool, tenant, b.session_id)
        .await
        .map_err(|_| KernelError::NotFound {
            entity: "examination session",
        })?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    require_operational(&s)?;
    let rules = rules_for(&st, tenant, s.institution_id, s.policy_id).await?;
    if !rules.self_service_allowed {
        return Err(KernelError::Conflict("self-service registration is not offered for this session; contact the examination office".into()).into());
    }
    let today = today_for(&st, &scope, &s).await?;
    let channel = match window_of(&s, today) {
        Window::Open => RegistrationChannel::SelfService,
        _ if !rules.registration_window_enforced => RegistrationChannel::SelfService,
        Window::NotYetOpen => {
            return Err(KernelError::Conflict(
                "registration for this session has not opened yet".into(),
            )
            .into())
        }
        Window::Closed => {
            if has_override(&st, tenant, s.id.as_uuid(), student.id.as_uuid(), today)
                .await?
                .is_none()
            {
                return Err(KernelError::Conflict("the registration window has closed; ask the examination office for a deadline override".into()).into());
            }
            RegistrationChannel::Late
        }
    };
    let cand = candidate(&st, &scope, &s, student.id).await?;
    let elig = check_eligibility(&st, &scope, tenant, &s, student.id, &rules, &b.papers).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let reg = register(
        &mut tx,
        &st,
        &scope,
        tenant,
        Register {
            session: &s,
            candidate: cand,
            channel,
            eligibility: elig,
            review: false,
        },
        &act,
    )
    .await?;
    if channel == RegistrationChannel::Late && reg.status.is_effective() {
        sqlx::query(
            "UPDATE exams.registration_override SET status = 'used', version = version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND session_id = $2 AND student_id = $3 AND status = 'approved'",
        )
        .bind(tenant.as_uuid())
        .bind(s.id.as_uuid())
        .bind(student.id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    tx.commit().await.map_err(map_db_error)?;
    if channel == RegistrationChannel::Late && reg.status.is_effective() {
        raise_late_fee(&st, &scope, tenant, &reg, &rules, s.academic_term_id, &act).await?;
    }
    let r = load_registration(&st.pool, tenant, reg.registration_id.as_uuid()).await?;
    let v: ExamRegistrationView = registration_view(&st, tenant, r).await?;
    let version = v.version;
    Ok(with_etag(StatusCode::CREATED, &v, Version(version)))
}

/// Withdraw one's own registration (`exam.self`, reason required, L3-I-023).
#[utoipa::path(post, path = "/api/v1/exams/me/exam-registrations/{id}/withdraw", operation_id = "exam_registration_self_withdraw", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_registration_withdraw(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = &auth.0;
    let own = require_own_registration(
        &st,
        ctx,
        sutra_contracts::exams::ids::ExamRegistrationId(id),
    )
    .await?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let why = require_reason(&reason)?.to_owned();
    let tenant = ctx.scope.tenant_id;
    let s = load_session(&st.pool, tenant, own.session_id.as_uuid()).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    require_operational(&s)?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = lock_registration(&mut tx, tenant, own.registration_id).await?;
    crate::setup::common::check_version(expected, r.version)?;
    withdraw_in_tx(&mut tx, &scope, tenant, &r, &why, &act).await?;
    tx.commit().await.map_err(map_db_error)?;
    let r = load_registration(&st.pool, tenant, id).await?;
    let v = registration_view(&st, tenant, r).await?;
    let version = v.version;
    Ok(with_etag(StatusCode::OK, &v, Version(version)))
}
