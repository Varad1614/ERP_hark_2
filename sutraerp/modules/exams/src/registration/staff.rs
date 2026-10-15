//! Staff registration endpoints (tag `exam-registration`): administrative / late registration
//! on behalf of a candidate (L3-I-034 / I-019), bulk / institution-assisted registration from
//! rosters (I-018), review decision + rejection (I-026), amendment and correction (I-021 /
//! I-022), withdrawal / reinstatement (I-023 / I-024), scope change after a 3B add / drop
//! (I-027), special arrangements (I-029), deadline overrides (I-020), reopening of the
//! registration window (I-025) and population finalisation (I-036).

use super::base::{
    candidate, check_eligibility, confirm, live_registration, load_registration, raise_late_fee,
    reg_event, register, registration_view, today_for, window_of, Eligibility,
    ExamRegistrationList, ExamRegistrationPaperInput, ExamRegistrationView, Register, Window,
};
use super::hall_ticket::invalidate_ticket;
use crate::setup::common::{
    check_version, clean, emit, invalid, reason_of, require_reason, ExamSetupBulkFailure,
    ExamSetupBulkSummary, ExamSetupDecisionBody, ExamSetupDecisionKind, ReasonBody,
};
use crate::setup::policy::{rules_for, PolicyRules};
use crate::setup::session::{load_session, papers_of, record_revision, RevisionRow};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution_writable, require_operational};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::{
    change_registration_status, record_history, HistoryEntry, StatusChange,
};
use crate::shared::publish::audit;
use crate::shared::registration::{add_paper, set_paper_status, NewRegistrationPaper};
use crate::shared::rows::{lock_registration, lock_session, session_from_row, SESSION_COLUMNS};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ExamsState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::exams::events::setup::{
    EXAM_REGISTRATION_CONFIRMED, EXAM_REGISTRATION_LOCKED, EXAM_REGISTRATION_LOCKED_SCHEMA,
    EXAM_REGISTRATION_REJECTED, EXAM_REGISTRATION_REJECTED_SCHEMA, EXAM_REGISTRATION_WITHDRAWN,
    EXAM_REGISTRATION_WITHDRAWN_SCHEMA,
};
use sutra_contracts::exams::ids::{ExamPaperId, ExamRegistrationId, ExamSessionId};
use sutra_contracts::exams::ports::{CandidateSummary, ExamSessionSummary};
use sutra_contracts::exams::status::{
    AttemptKind, ExamPaperStatus, ExamRegistrationStatus, RegistrationChannel,
    RegistrationPaperStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Registration on behalf of a candidate (administrative / late).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationCreate {
    pub student_id: Uuid,
    /// Empty = every paper the student's term registration covers.
    #[serde(default)]
    pub papers: Vec<ExamRegistrationPaperInput>,
    /// Hold for review (`REGISTRATION_REVIEW`) instead of confirming at once.
    #[serde(default)]
    pub review: bool,
    pub reason: Option<String>,
}

/// Bulk / institution-assisted registration.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationBulk {
    /// Students to register; empty = every student seated in a section of a paper's offering.
    #[serde(default)]
    pub student_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationQuery {
    /// `ExamRegistrationStatus` wire name.
    pub status: Option<String>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
    /// Registration number to continue after.
    pub cursor: Option<String>,
}

/// Amend / correct a registration (L3-I-021 / I-022).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationAmend {
    #[serde(default)]
    pub add_papers: Vec<ExamRegistrationPaperInput>,
    #[serde(default)]
    pub withdraw_papers: Vec<Uuid>,
    pub reason: Option<String>,
}

/// Add one paper (amendment shortcut).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationAddPaper {
    pub paper_id: Uuid,
    pub attempt_kind: Option<AttemptKind>,
    pub attempt_no: Option<i32>,
    pub reason: Option<String>,
}

/// A special examination arrangement.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamArrangementCreate {
    pub kind: String,
    pub paper_id: Option<Uuid>,
    pub extra_minutes: Option<i32>,
    pub note: String,
}

/// Deadline override request (L3-I-020).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamDeadlineOverrideCreate {
    pub student_id: Uuid,
    pub valid_until: NaiveDate,
    pub reason: String,
}

/// A deadline override.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamDeadlineOverrideView {
    pub id: Uuid,
    pub session_id: Uuid,
    pub student_id: Uuid,
    pub reason: String,
    pub valid_until: NaiveDate,
    /// `pending | approved | rejected | used | expired`.
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamDeadlineOverrideList {
    pub items: Vec<ExamDeadlineOverrideView>,
}

/// Reopen the registration window (L3-I-025).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationReopen {
    pub closes_on: NaiveDate,
    pub reason: Option<String>,
}

const ARRANGEMENT_KINDS: &[&str] = &[
    "extra_time",
    "scribe",
    "separate_room",
    "assistive_technology",
    "medical",
    "other",
];

async fn respond(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let r = load_registration(&st.pool, tenant, id).await?;
    let v = registration_view(st, tenant, r).await?;
    let version = v.version;
    Ok(with_etag(status, &v, Version(version)))
}

/// Session + scope + rules for a registration command on an operational session.
async fn operational(
    st: &ExamsState,
    ctx: &AuthContext,
    session: Uuid,
) -> Result<(ExamSessionSummary, ExecutionScope, PolicyRules), KernelError> {
    let tenant = ctx.scope.tenant_id;
    let s = load_session(&st.pool, tenant, session).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(st, &scope, s.institution_id).await?;
    require_operational(&s)?;
    let rules = rules_for(st, tenant, s.institution_id, s.policy_id).await?;
    Ok((s, scope, rules))
}

/// 409 when the population of the session was finalised and not reopened since.
async fn require_not_finalised(
    st: &ExamsState,
    tenant: TenantId,
    session: Uuid,
) -> Result<(), KernelError> {
    let last: Option<String> = sqlx::query_scalar(
        "SELECT kind FROM exams.exam_session_revision WHERE tenant_id = $1 AND session_id = $2 \
         AND kind IN ('registration_finalised', 'registration_reopened') ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(session)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    if last.as_deref() == Some("registration_finalised") {
        return Err(KernelError::Conflict(
            "the registration population of this session is finalised; reopen registration first"
                .into(),
        ));
    }
    Ok(())
}

/// Registrations of a session ordered by number (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/registrations", operation_id = "exam_registration_list", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ExamRegistrationQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ExamRegistrationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ExamRegistrationQuery>,
) -> ApiResult<Json<ExamRegistrationList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    let s = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    if let Some(status) = q.status.as_deref() {
        if ExamRegistrationStatus::parse(status).is_none() {
            return Err(invalid("status", "invalid", "unknown registration status").into());
        }
    }
    let limit = usize::try_from(q.limit.unwrap_or(50).clamp(1, 200)).unwrap_or(50);
    let all = st.directory.registrations_of_session(&scope, s.id).await?;
    let mut rows: Vec<CandidateSummary> = all
        .into_iter()
        .filter(|r| q.status.as_deref().is_none_or(|x| r.status.as_str() == x))
        .filter(|r| {
            q.cursor
                .as_deref()
                .is_none_or(|c| r.registration_number.as_str() > c)
        })
        .collect();
    let next_cursor = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|r| r.registration_number.clone())
    } else {
        None
    };
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        items.push(registration_view(&st, tenant, r).await?);
    }
    Ok(Json(ExamRegistrationList { items, next_cursor }))
}

/// Register a candidate on their behalf (`exam.registration.manage`, L3-I-034). Within the
/// window the channel is `administrative`; after it (policy permitting late registration) it is
/// `late` and the policy's late fee is raised in Finance (L3-I-019). Ineligible papers record a
/// `rejected` registration with the reasons (L3-I-026); a second live registration is `409`
/// (L3-I-032).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/registrations", operation_id = "exam_registration_create", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamRegistrationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamRegistrationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let (s, scope, rules) = operational(&st, ctx, id).await?;
    require_not_finalised(&st, tenant, id).await?;
    let student = StudentId(b.student_id);
    if let Some(existing) = live_registration(&st, &scope, s.id, student).await? {
        return Err(KernelError::Conflict(format!(
            "the candidate already holds registration {} in this session",
            existing.registration_number
        ))
        .into());
    }
    let today = today_for(&st, &scope, &s).await?;
    let channel = match window_of(&s, today) {
        Window::Open => RegistrationChannel::Administrative,
        _ if !rules.registration_window_enforced => RegistrationChannel::Administrative,
        Window::NotYetOpen => {
            return Err(KernelError::Conflict(
                "registration for this session has not opened yet".into(),
            )
            .into())
        }
        Window::Closed => {
            if !rules.late_registration_allowed {
                return Err(KernelError::Conflict(
                    "the registration window has closed and the policy allows no late registration"
                        .into(),
                )
                .into());
            }
            RegistrationChannel::Late
        }
    };
    let cand = candidate(&st, &scope, &s, student).await?;
    let elig = check_eligibility(&st, &scope, tenant, &s, student, &rules, &b.papers).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    lock_session(&mut tx, tenant, s.id).await?;
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
            review: b.review,
        },
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    if channel == RegistrationChannel::Late && reg.status.is_effective() {
        raise_late_fee(&st, &scope, tenant, &reg, &rules, s.academic_term_id, &act).await?;
    }
    respond(
        &st,
        tenant,
        reg.registration_id.as_uuid(),
        StatusCode::CREATED,
    )
    .await
}

/// Bulk / institution-assisted registration (`exam.registration.manage`, `Idempotency-Key`,
/// L3-I-018): each student is registered for the session's papers their term registration
/// covers; students already registered are skipped, ineligible ones reported.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/registrations/bulk", operation_id = "exam_registration_bulk", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamRegistrationBulk, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSetupBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_bulk(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ExamRegistrationBulk>>,
) -> ApiResult<Json<ExamSetupBulkSummary>> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    require_idempotency_key(&headers)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let tenant = ctx.scope.tenant_id;
    let (s, scope, rules) = operational(&st, ctx, id).await?;
    require_not_finalised(&st, tenant, id).await?;
    let mut students: BTreeSet<Uuid> = b.student_ids.iter().copied().collect();
    if students.is_empty() {
        for p in papers_of(&st.pool, tenant, id).await? {
            let Some(o) = p
                .offering_id
                .filter(|_| p.status != ExamPaperStatus::Cancelled)
            else {
                continue;
            };
            for section in st
                .offerings
                .sections_of_offering(&scope, CourseOfferingId(o))
                .await?
            {
                for r in st.offerings.roster(&scope, section.id, true).await? {
                    students.insert(r.student_id.as_uuid());
                }
            }
        }
    }
    let act = actor(ctx);
    let mut summary = ExamSetupBulkSummary::default();
    for sid in students {
        let student = StudentId(sid);
        if live_registration(&st, &scope, s.id, student)
            .await?
            .is_some()
        {
            summary.skipped += 1;
            continue;
        }
        let cand = match candidate(&st, &scope, &s, student).await {
            Ok(c) => c,
            Err(e) => {
                summary.failed.push(ExamSetupBulkFailure {
                    id: Some(sid),
                    reason: e.to_string(),
                });
                continue;
            }
        };
        let elig: Eligibility =
            check_eligibility(&st, &scope, tenant, &s, student, &rules, &[]).await?;
        if elig.papers.is_empty() {
            summary.failed.push(ExamSetupBulkFailure {
                id: Some(sid),
                reason: elig.problems.join("; "),
            });
            continue;
        }
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        match register(
            &mut tx,
            &st,
            &scope,
            tenant,
            Register {
                session: &s,
                candidate: cand,
                channel: RegistrationChannel::Bulk,
                eligibility: Eligibility {
                    papers: elig.papers,
                    problems: Vec::new(),
                },
                review: false,
            },
            &act,
        )
        .await
        {
            Ok(_) => {
                tx.commit().await.map_err(map_db_error)?;
                summary.processed += 1;
            }
            Err(e) => summary.failed.push(ExamSetupBulkFailure {
                id: Some(sid),
                reason: e.to_string(),
            }),
        }
    }
    Ok(Json(summary))
}

/// One registration (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-registrations/{id}", operation_id = "exam_registration_get", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    respond(&st, ctx.scope.tenant_id, id, StatusCode::OK).await
}

/// Locks the registration for a command on an operational session and checks `If-Match`.
async fn lock_for(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
) -> Result<
    (
        CandidateSummary,
        ExamSessionSummary,
        ExecutionScope,
        PolicyRules,
    ),
    KernelError,
> {
    let tenant = ctx.scope.tenant_id;
    let r = load_registration(&st.pool, tenant, id).await?;
    let (s, scope, rules) = operational(st, ctx, r.session_id.as_uuid()).await?;
    check_version(expected, r.version)?;
    Ok((r, s, scope, rules))
}

/// Decide a registration held for review (`exam.approve`): approve → confirmed, reject →
/// rejected with the reason (L3-I-026).
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/decision", operation_id = "exam_registration_decision", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body = ExamSetupDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_decision(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSetupDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "exam registration").await?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    let tenant = ctx.scope.tenant_id;
    let (r, _s, scope, _) = lock_for(&st, ctx, id, expected).await?;
    if r.status != ExamRegistrationStatus::PendingReview {
        return Err(KernelError::Conflict(format!(
            "registration {} is {}; only a registration under review can be decided",
            r.registration_number, r.status
        ))
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_registration(&mut tx, tenant, ExamRegistrationId(id)).await?;
    check_version(expected, locked.version)?;
    match b.decision {
        ExamSetupDecisionKind::Approve => {
            confirm(&mut tx, &scope, tenant, &locked, reason.as_deref(), &act).await?;
        }
        ExamSetupDecisionKind::Reject => {
            let why = require_reason(&reason)?.to_owned();
            let out = change_registration_status(
                &mut tx,
                tenant,
                &locked,
                ExamRegistrationStatus::Rejected,
                &StatusChange {
                    cause_event_type: EXAM_REGISTRATION_REJECTED,
                    reason: Some(&why),
                    process_instance_id: None,
                },
                &act,
            )
            .await?;
            let e = reg_event(&out, Some("pending_review"), "rejected", Some(why));
            emit(
                &mut tx,
                &scope,
                (
                    EXAM_REGISTRATION_REJECTED,
                    EXAM_REGISTRATION_REJECTED_SCHEMA,
                ),
                &e,
                &act,
            )
            .await?;
        }
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_registration.decision",
        "exam_registration",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Confirm an amended registration again (`exam.registration.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/confirm", operation_id = "exam_registration_confirm", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_confirm(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let tenant = ctx.scope.tenant_id;
    let (_, _, scope, _) = lock_for(&st, ctx, id, expected).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = lock_registration(&mut tx, tenant, ExamRegistrationId(id)).await?;
    check_version(expected, r.version)?;
    let from = r.status;
    let out = change_registration_status(
        &mut tx,
        tenant,
        &r,
        ExamRegistrationStatus::Confirmed,
        &StatusChange {
            cause_event_type: EXAM_REGISTRATION_CONFIRMED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let e = reg_event(&out, Some(from.as_str()), "confirmed", reason.clone());
    emit(
        &mut tx,
        &scope,
        (
            EXAM_REGISTRATION_CONFIRMED,
            sutra_contracts::exams::events::setup::EXAM_REGISTRATION_CONFIRMED_SCHEMA,
        ),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_registration.confirm",
        "exam_registration",
        id.to_string(),
        json!({"from": from.as_str()}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Applies an amendment inside the caller's transaction: `confirmed | locked → amended`, papers
/// added through `add_paper` and withdrawn through `set_paper_status`.
#[allow(clippy::too_many_arguments)]
async fn apply_amendment(
    conn: &mut sqlx::PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: &CandidateSummary,
    add: &[NewRegistrationPaper],
    withdraw: &[Uuid],
    reason: &str,
    actor: &Actor,
) -> Result<CandidateSummary, KernelError> {
    let mut cur = if r.status == ExamRegistrationStatus::Amended {
        r.clone()
    } else {
        change_registration_status(
            conn,
            tenant,
            r,
            ExamRegistrationStatus::Amended,
            &StatusChange {
                cause_event_type: "exams.exam_registration.amend",
                reason: Some(reason),
                process_instance_id: None,
            },
            actor,
        )
        .await?
    };
    for p in add {
        // A paper withdrawn earlier (erroneous registration, L3-I-022) is re-registered on its
        // existing row; its mark row already exists.
        if let Some(existing) = cur
            .papers
            .iter()
            .find(|x| x.paper_id == p.paper_id)
            .cloned()
        {
            if existing.status != RegistrationPaperStatus::Withdrawn {
                return Err(KernelError::Conflict(
                    "the paper is already on the registration".into(),
                ));
            }
            let updated = set_paper_status(
                conn,
                tenant,
                &cur,
                p.paper_id,
                RegistrationPaperStatus::Registered,
                Some(reason),
            )
            .await?;
            if let Some(x) = cur.papers.iter_mut().find(|x| x.paper_id == p.paper_id) {
                *x = updated;
            }
            continue;
        }
        let added = add_paper(conn, &cur, p).await?;
        cur.papers.push(added);
    }
    for pid in withdraw {
        let Some(p) = cur.papers.iter().find(|p| p.paper_id.as_uuid() == *pid) else {
            return Err(invalid(
                "withdrawPapers",
                "not_registered",
                "the paper is not on the registration",
            ));
        };
        if p.status != RegistrationPaperStatus::Registered {
            return Err(KernelError::Conflict(format!(
                "the registered paper is {}",
                p.status.as_str()
            )));
        }
        let updated = set_paper_status(
            conn,
            tenant,
            &cur,
            ExamPaperId(*pid),
            RegistrationPaperStatus::Withdrawn,
            Some(reason),
        )
        .await?;
        if let Some(x) = cur.papers.iter_mut().find(|x| x.paper_id.as_uuid() == *pid) {
            *x = updated;
        }
    }
    audit(
        conn,
        scope,
        "exams.exam_registration.amend",
        "exam_registration",
        r.registration_id.to_string(),
        json!({"added": add.len(), "withdrawn": withdraw, "reason": reason}),
        actor,
    )
    .await?;
    Ok(cur)
}

/// Amend or correct a registration (`exam.registration.manage`, reason required, L3-I-021 /
/// I-022): papers are added / withdrawn with their history; the registration becomes `amended`
/// until confirmed again.
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/amend", operation_id = "exam_registration_amend", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body = ExamRegistrationAmend, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_amend(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamRegistrationAmend>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    let why = require_reason(&reason)?.to_owned();
    if b.add_papers.is_empty() && b.withdraw_papers.is_empty() {
        return Err(invalid("addPapers", "empty", "add or withdraw at least one paper").into());
    }
    let tenant = ctx.scope.tenant_id;
    let (r, s, scope, rules) = lock_for(&st, ctx, id, expected).await?;
    let elig = if b.add_papers.is_empty() {
        Eligibility::default()
    } else {
        check_eligibility(&st, &scope, tenant, &s, r.student_id, &rules, &b.add_papers).await?
    };
    if !elig.problems.is_empty() {
        return Err(KernelError::Conflict(elig.problems.join("; ")).into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_registration(&mut tx, tenant, ExamRegistrationId(id)).await?;
    check_version(expected, locked.version)?;
    apply_amendment(
        &mut tx,
        &scope,
        tenant,
        &locked,
        &elig.papers,
        &b.withdraw_papers,
        &why,
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Add one paper to a registration (`exam.registration.manage`; an amendment).
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/papers", operation_id = "exam_registration_paper_add", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body = ExamRegistrationAddPaper, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_add_paper(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamRegistrationAddPaper>,
) -> ApiResult<Response> {
    registration_amend(
        State(st),
        auth,
        Path(id),
        headers,
        Json(ExamRegistrationAmend {
            add_papers: vec![ExamRegistrationPaperInput {
                paper_id: b.paper_id,
                attempt_kind: b.attempt_kind,
                attempt_no: b.attempt_no,
            }],
            withdraw_papers: Vec::new(),
            reason: b.reason.or_else(|| Some("paper added".into())),
        }),
    )
    .await
}

/// Re-reads the student's 3B term registration and aligns the registered papers with it
/// (`exam.registration.manage`, L3-I-027): papers whose course was dropped are withdrawn,
/// newly added courses of the session are registered.
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/scope-sync", operation_id = "exam_registration_scope_sync", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_scope_sync(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body).unwrap_or_else(|| "academic registration changed".into());
    let tenant = ctx.scope.tenant_id;
    let (r, s, scope, _) = lock_for(&st, ctx, id, expected).await?;
    if !r.status.is_effective() || r.status == ExamRegistrationStatus::Completed {
        return Err(KernelError::Conflict(format!(
            "registration {} is {}",
            r.registration_number, r.status
        ))
        .into());
    }
    let term = st
        .registrations
        .term_registration(&scope, r.student_id, s.academic_term_id)
        .await?;
    let covers = |c: Uuid| term.as_ref().is_some_and(|t| t.covers(c));
    let papers = papers_of(&st.pool, tenant, s.id.as_uuid()).await?;
    let mut withdraw = Vec::new();
    for rp in r.papers.iter().filter(|p| {
        p.status == RegistrationPaperStatus::Registered && p.attempt_kind == AttemptKind::Regular
    }) {
        if let Some(p) = papers.iter().find(|p| p.id == rp.paper_id) {
            if !covers(p.curriculum_component_id) {
                withdraw.push(p.id.as_uuid());
            }
        }
    }
    let add: Vec<NewRegistrationPaper> = papers
        .iter()
        .filter(|p| p.status != ExamPaperStatus::Cancelled && covers(p.curriculum_component_id))
        .filter(|p| !r.papers.iter().any(|rp| rp.paper_id == p.id))
        .map(|p| NewRegistrationPaper {
            paper_id: p.id,
            attempt_kind: AttemptKind::Regular,
            attempt_no: 1,
        })
        .collect();
    if withdraw.is_empty() && add.is_empty() {
        return respond(&st, tenant, id, StatusCode::OK).await;
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_registration(&mut tx, tenant, ExamRegistrationId(id)).await?;
    check_version(expected, locked.version)?;
    apply_amendment(
        &mut tx, &scope, tenant, &locked, &add, &withdraw, &reason, &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Withdraw a registration (`exam.registration.manage`, reason required, L3-I-023); its
/// current hall ticket is invalidated (L3-I-039).
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/withdraw", operation_id = "exam_registration_withdraw", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_withdraw(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let why = require_reason(&reason)?.to_owned();
    let tenant = ctx.scope.tenant_id;
    let (_, _, scope, _) = lock_for(&st, ctx, id, expected).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = lock_registration(&mut tx, tenant, ExamRegistrationId(id)).await?;
    check_version(expected, r.version)?;
    withdraw_in_tx(&mut tx, &scope, tenant, &r, &why, &act).await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Withdrawal inside the caller's transaction (also used by self-service).
pub async fn withdraw_in_tx(
    conn: &mut sqlx::PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: &CandidateSummary,
    why: &str,
    act: &Actor,
) -> Result<CandidateSummary, KernelError> {
    let from = r.status;
    let out = change_registration_status(
        conn,
        tenant,
        r,
        ExamRegistrationStatus::Withdrawn,
        &StatusChange {
            cause_event_type: EXAM_REGISTRATION_WITHDRAWN,
            reason: Some(why),
            process_instance_id: None,
        },
        act,
    )
    .await?;
    let e = reg_event(&out, Some(from.as_str()), "withdrawn", Some(why.to_owned()));
    emit(
        conn,
        scope,
        (
            EXAM_REGISTRATION_WITHDRAWN,
            EXAM_REGISTRATION_WITHDRAWN_SCHEMA,
        ),
        &e,
        act,
    )
    .await?;
    invalidate_ticket(conn, scope, tenant, &out, "registration withdrawn", act).await?;
    audit(
        conn,
        scope,
        "exams.exam_registration.withdraw",
        "exam_registration",
        r.registration_id.to_string(),
        json!({"reason": why}),
        act,
    )
    .await?;
    Ok(out)
}

/// Reinstate a withdrawn registration (`exam.registration.manage`, reason required,
/// L3-I-024): back to `confirmed` unless the candidate registered again meanwhile (`409`).
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/reinstate", operation_id = "exam_registration_reinstate", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the registration")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_reinstate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let why = require_reason(&reason)?.to_owned();
    let tenant = ctx.scope.tenant_id;
    let (r, s, scope, _) = lock_for(&st, ctx, id, expected).await?;
    if r.status != ExamRegistrationStatus::Withdrawn {
        return Err(KernelError::Conflict(format!(
            "registration {} is {}; only a withdrawn registration can be reinstated",
            r.registration_number, r.status
        ))
        .into());
    }
    if let Some(other) = live_registration(&st, &scope, s.id, r.student_id).await? {
        return Err(KernelError::Conflict(format!(
            "the candidate already holds registration {} in this session",
            other.registration_number
        ))
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_registration(&mut tx, tenant, ExamRegistrationId(id)).await?;
    check_version(expected, locked.version)?;
    let out = change_registration_status(
        &mut tx,
        tenant,
        &locked,
        ExamRegistrationStatus::Confirmed,
        &StatusChange {
            cause_event_type: EXAM_REGISTRATION_CONFIRMED,
            reason: Some(&why),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let e = reg_event(
        &out,
        Some("withdrawn"),
        "confirmed",
        Some(format!("reinstated: {why}")),
    );
    emit(
        &mut tx,
        &scope,
        (
            EXAM_REGISTRATION_CONFIRMED,
            sutra_contracts::exams::events::setup::EXAM_REGISTRATION_CONFIRMED_SCHEMA,
        ),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_registration.reinstate",
        "exam_registration",
        id.to_string(),
        json!({"reason": why}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Record an approved special examination arrangement (`exam.registration.manage`, L3-I-029);
/// it is printed on the hall ticket (L3-I-040).
#[utoipa::path(post, path = "/api/v1/exams/exam-registrations/{id}/arrangements", operation_id = "exam_registration_arrangement_create", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamArrangementCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamRegistrationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn arrangement_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamArrangementCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let r = load_registration(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, r.institution_id);
    require_institution_writable(&st, &scope, r.institution_id).await?;
    if !r.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "registration {} is {}",
            r.registration_number, r.status
        ))
        .into());
    }
    if !ARRANGEMENT_KINDS.contains(&b.kind.as_str()) {
        return Err(invalid(
            "kind",
            "invalid",
            "extra_time, scribe, separate_room, assistive_technology, medical or other",
        )
        .into());
    }
    if let Some(p) = b.paper_id {
        if !r.papers.iter().any(|x| x.paper_id.as_uuid() == p) {
            return Err(invalid(
                "paperId",
                "not_registered",
                "the paper is not on the registration",
            )
            .into());
        }
    }
    if b.extra_minutes.is_some_and(|m| !(1..=600).contains(&m)) {
        return Err(invalid("extraMinutes", "out_of_range", "must be 1 … 600").into());
    }
    let note = clean(Some(&b.note))
        .ok_or_else(|| invalid("note", "required", "describe the arrangement"))?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let aid: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.registration_arrangement (tenant_id, institution_id, registration_id, \
         paper_id, kind, extra_minutes, note, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(r.institution_id.as_uuid())
    .bind(id)
    .bind(b.paper_id)
    .bind(&b.kind)
    .bind(b.extra_minutes)
    .bind(&note)
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_registration.arrangement",
        "exam_registration",
        id.to_string(),
        json!({"arrangementId": aid, "kind": b.kind}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::CREATED).await
}

// ------------------------------------------------------------------ overrides

const OVERRIDE_COLUMNS: &str = "id, session_id, student_id, reason, valid_until, status, \
    status_reason, process_instance_id, version";

async fn load_override(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
) -> Result<ExamDeadlineOverrideView, KernelError> {
    sqlx::query_as::<_, ExamDeadlineOverrideView>(&format!(
        "SELECT {OVERRIDE_COLUMNS} FROM exams.registration_override WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "registration deadline override" })
}

/// Deadline overrides of a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/registration-overrides", operation_id = "exam_registration_override_list", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamDeadlineOverrideList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn override_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamDeadlineOverrideList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_session(&st.pool, tenant, id).await?;
    let items = sqlx::query_as::<_, ExamDeadlineOverrideView>(&format!(
        "SELECT {OVERRIDE_COLUMNS} FROM exams.registration_override WHERE tenant_id = $1 AND session_id = $2 ORDER BY created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(ExamDeadlineOverrideList { items }))
}

/// Request a registration deadline override for one candidate (`exam.registration.manage`,
/// `DEADLINE_OVERRIDE`, L3-I-020). Auto-approved in Batch 6; the candidate may then register
/// (channel `late`) until `validUntil`.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/registration-overrides", operation_id = "exam_registration_override_create", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamDeadlineOverrideCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamDeadlineOverrideView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn override_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamDeadlineOverrideCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let (s, scope, _) = operational(&st, ctx, id).await?;
    let reason = clean(Some(&b.reason));
    let why = require_reason(&reason)?.to_owned();
    let today = today_for(&st, &scope, &s).await?;
    if b.valid_until < today {
        return Err(invalid("validUntil", "in_the_past", "must be today or later").into());
    }
    candidate(&st, &scope, &s, StudentId(b.student_id)).await?;
    let oid = Uuid::now_v7();
    let outcome = request_approval(
        &st,
        &scope,
        process::DEADLINE_OVERRIDE,
        business_key("registration_override", oid, 1),
        BTreeMap::from([
            ("studentId".to_owned(), json!(b.student_id)),
            ("sessionId".to_owned(), json!(id)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let approved = matches!(outcome, ApprovalOutcome::Approved { .. });
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO exams.registration_override (id, tenant_id, institution_id, session_id, student_id, \
         reason, valid_until, status, status_reason, process_instance_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(oid)
    .bind(tenant.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(id)
    .bind(b.student_id)
    .bind(&why)
    .bind(b.valid_until)
    .bind(if approved { "approved" } else { "pending" })
    .bind(if approved { Some("auto-approved") } else { None })
    .bind(&pid)
    .bind(act.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict("the candidate already has an open override for this session".into()),
        other => other,
    })?;
    record_history(
        &mut tx,
        HistoryEntry {
            tenant,
            institution: s.institution_id,
            aggregate_type: "registration_override",
            aggregate_id: oid,
            from: None,
            to: if approved { "approved" } else { "pending" },
        },
        &StatusChange {
            cause_event_type: "exams.registration_override.request",
            reason: Some(&why),
            process_instance_id: Some(&pid),
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.registration_override.request",
        "registration_override",
        oid.to_string(),
        json!({"studentId": b.student_id, "validUntil": b.valid_until}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let o = load_override(&st, tenant, oid).await?;
    Ok(with_etag(StatusCode::CREATED, &o, Version(o.version)))
}

/// Decide a pending deadline override (`exam.approve`).
#[utoipa::path(post, path = "/api/v1/exams/registration-overrides/{id}/decision", operation_id = "exam_registration_override_decision", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the override")),
    request_body = ExamSetupDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamDeadlineOverrideView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn override_decision(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSetupDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "registration override").await?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let o = load_override(&st, tenant, id).await?;
    check_version(expected, o.version)?;
    if o.status != "pending" {
        return Err(KernelError::Conflict(format!("the override is {}", o.status)).into());
    }
    let s = load_session(&st.pool, tenant, o.session_id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    let reason = clean(b.reason.as_deref());
    let to = match b.decision {
        ExamSetupDecisionKind::Approve => "approved",
        ExamSetupDecisionKind::Reject => {
            require_reason(&reason)?;
            "rejected"
        }
    };
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    crate::setup::common::move_status(
        &mut tx,
        crate::setup::common::Move {
            table: "exams.registration_override",
            aggregate: "registration_override",
            has_process: true,
            tenant,
            institution: s.institution_id,
            id,
            version: o.version,
            from: "pending",
            to,
        },
        &StatusChange {
            cause_event_type: "exams.registration_override.decision",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.registration_override.decision",
        "registration_override",
        id.to_string(),
        json!({"to": to, "reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let o = load_override(&st, tenant, id).await?;
    Ok(with_etag(StatusCode::OK, &o, Version(o.version)))
}

// ------------------------------------------------------------------ window + population

/// Reopen (extend) the registration window (`exam.setup.manage`, reason required, L3-I-025);
/// a finalised population becomes open for registration again.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/registration-window/reopen", operation_id = "exam_registration_window_reopen", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = ExamRegistrationReopen, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = crate::setup::session::ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn registration_reopen(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamRegistrationReopen>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    let why = require_reason(&reason)?.to_owned();
    let tenant = ctx.scope.tenant_id;
    let (s, scope, _) = operational(&st, ctx, id).await?;
    check_version(expected, s.version)?;
    let today = today_for(&st, &scope, &s).await?;
    if b.closes_on < today {
        return Err(invalid(
            "closesOn",
            "in_the_past",
            "the reopened window must close today or later",
        )
        .into());
    }
    if s.ends_on.is_some_and(|e| b.closes_on > e) {
        return Err(invalid(
            "closesOn",
            "after_session",
            "must not be after the session ends",
        )
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_session(&mut tx, tenant, ExamSessionId(id)).await?;
    check_version(expected, locked.version)?;
    let row = sqlx::query(&format!(
        "UPDATE exams.exam_session SET registration_closes_on = $3, \
         registration_opens_on = LEAST(COALESCE(registration_opens_on, $4), $4), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 RETURNING {SESSION_COLUMNS}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(b.closes_on)
    .bind(today)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let updated = session_from_row(&row).map_err(map_db_error)?;
    record_revision(
        &mut tx,
        tenant,
        &updated,
        RevisionRow {
            kind: "registration_reopened",
            changed: vec!["registrationClosesOn".into()],
            before: json!({"registrationClosesOn": s.registration_closes_on}),
            after: json!({"registrationClosesOn": b.closes_on}),
            reason: &why,
            merged_into: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_session.registration_reopen",
        "exam_session",
        id.to_string(),
        json!({"closesOn": b.closes_on, "reason": why}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let v = crate::setup::session::session_view(&st, tenant, updated).await?;
    let version = v.version;
    Ok(with_etag(StatusCode::OK, &v, Version(version)))
}

/// Finalise the registration population (`exam.registration.manage`, `Idempotency-Key`,
/// L3-I-036): every confirmed / amended registration becomes `locked`
/// (`exam_registration.locked`); registrations still pending are reported as skipped.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/registration-population/finalise", operation_id = "exam_registration_population_finalise", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSetupBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn population_finalise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Json<ExamSetupBulkSummary>> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    require_idempotency_key(&headers)?;
    let reason = reason_of(body).unwrap_or_else(|| "registration population finalised".into());
    let tenant = ctx.scope.tenant_id;
    let (s, scope, _) = operational(&st, ctx, id).await?;
    let regs = st.directory.registrations_of_session(&scope, s.id).await?;
    let act = actor(ctx);
    let mut summary = ExamSetupBulkSummary::default();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked_session = lock_session(&mut tx, tenant, s.id).await?;
    for r in regs {
        match r.status {
            ExamRegistrationStatus::Confirmed | ExamRegistrationStatus::Amended => {
                let cur = lock_registration(&mut tx, tenant, r.registration_id).await?;
                let out = change_registration_status(
                    &mut tx,
                    tenant,
                    &cur,
                    ExamRegistrationStatus::Locked,
                    &StatusChange {
                        cause_event_type: EXAM_REGISTRATION_LOCKED,
                        reason: Some(&reason),
                        process_instance_id: None,
                    },
                    &act,
                )
                .await?;
                let e = reg_event(
                    &out,
                    Some(cur.status.as_str()),
                    "locked",
                    Some(reason.clone()),
                );
                emit(
                    &mut tx,
                    &scope,
                    (EXAM_REGISTRATION_LOCKED, EXAM_REGISTRATION_LOCKED_SCHEMA),
                    &e,
                    &act,
                )
                .await?;
                summary.processed += 1;
            }
            ExamRegistrationStatus::Locked => summary.skipped += 1,
            ExamRegistrationStatus::Draft
            | ExamRegistrationStatus::PendingValidation
            | ExamRegistrationStatus::PendingReview
            | ExamRegistrationStatus::Approved => {
                summary.failed.push(ExamSetupBulkFailure {
                    id: Some(r.registration_id.as_uuid()),
                    reason: format!(
                        "registration {} is still {}",
                        r.registration_number, r.status
                    ),
                });
            }
            _ => {}
        }
    }
    record_revision(
        &mut tx,
        tenant,
        &locked_session,
        RevisionRow {
            kind: "registration_finalised",
            changed: vec!["registrations".into()],
            before: json!({}),
            after: json!({"locked": summary.processed, "alreadyLocked": summary.skipped, "pending": summary.failed.len()}),
            reason: &reason,
            merged_into: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_registration.finalise",
        "exam_session",
        id.to_string(),
        json!({"locked": summary.processed}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(summary))
}
