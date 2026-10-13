//! Examination sessions and their papers (L2-I02; L3-I-004 cycle → governed session, I-005
//! governed revision with preserved downstream continuity, I-006 duplicate → single
//! authoritative session).
//!
//! Session status moves only through `shared::lifecycle::change_session_status` on a row locked
//! with `shared::rows::lock_session`; paper status only through `change_paper_status`. Papers
//! come from the term's Domain D offerings (credits, title and curriculum component from the
//! offering; one live paper per offering and session).

use super::common::{
    check_version, clean, code_from, credits_of, emit, invalid, parse_dec, reason_of, require_code,
    require_name, require_reason, status_event, ExamSetupBulkFailure, ExamSetupBulkSummary,
    ExamSetupDecisionBody, ExamSetupDecisionKind, ReasonBody,
};
use super::policy::{active_policy, load_policy};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::grading::fmt2;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::{change_paper_status, change_session_status, StatusChange};
use crate::shared::publish::audit;
use crate::shared::rows::{
    lock_paper, lock_session, paper_from_row, session_from_row, PAPER_COLUMNS, SESSION_COLUMNS,
};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ExamsState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::academic::ports::OfferingSummary;
use sutra_contracts::academic::status::CourseOfferingStatus;
use sutra_contracts::exams::events::setup::{
    EXAM_SESSION_ACTIVATED, EXAM_SESSION_ACTIVATED_SCHEMA, EXAM_SESSION_APPROVED,
    EXAM_SESSION_APPROVED_SCHEMA, EXAM_SESSION_REOPENED, EXAM_SESSION_REOPENED_SCHEMA,
    EXAM_SESSION_SUBMITTED, EXAM_SESSION_SUBMITTED_SCHEMA, EXAM_SESSION_SUSPENDED,
    EXAM_SESSION_SUSPENDED_SCHEMA,
};
use sutra_contracts::exams::ids::{ExamPaperId, ExamSessionId, GradingSchemeId};
use sutra_contracts::exams::ports::{ExamPaperSummary, ExamSessionSummary};
use sutra_contracts::exams::status::{
    ExamMode, ExamPaperStatus, ExamPolicyStatus, ExamSessionKind, ExamSessionStatus,
    GradingSchemeStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// A paper of a session (decimals as strings).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamPaperView {
    pub id: ExamPaperId,
    pub session_id: ExamSessionId,
    pub offering_id: Option<Uuid>,
    pub curriculum_component_id: Uuid,
    pub academic_term_id: Uuid,
    pub paper_code: String,
    pub title: String,
    #[schema(example = "4.00")]
    pub credits: String,
    pub mode: ExamMode,
    #[schema(example = "100.00")]
    pub max_marks: String,
    #[schema(example = "40.00")]
    pub pass_percent: String,
    #[schema(example = "40.00")]
    pub internal_weight: String,
    pub duration_minutes: Option<i32>,
    pub status: ExamPaperStatus,
    pub version: i64,
}

impl From<&ExamPaperSummary> for ExamPaperView {
    fn from(p: &ExamPaperSummary) -> Self {
        Self {
            id: p.id,
            session_id: p.session_id,
            offering_id: p.offering_id,
            curriculum_component_id: p.curriculum_component_id,
            academic_term_id: p.academic_term_id,
            paper_code: p.paper_code.clone(),
            title: p.title.clone(),
            credits: fmt2(p.credits),
            mode: p.mode,
            max_marks: fmt2(p.max_marks),
            pass_percent: fmt2(p.pass_percent),
            internal_weight: fmt2(p.internal_weight),
            duration_minutes: p.duration_minutes,
            status: p.status,
            version: p.version,
        }
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamPaperList {
    pub items: Vec<ExamPaperView>,
}

/// An examination session with its papers.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSessionView {
    pub id: ExamSessionId,
    pub institution_id: Uuid,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Uuid,
    pub code: String,
    pub name: String,
    pub kind: ExamSessionKind,
    pub status: ExamSessionStatus,
    pub policy_id: Option<Uuid>,
    pub grading_scheme_id: Option<GradingSchemeId>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub registration_opens_on: Option<NaiveDate>,
    pub registration_closes_on: Option<NaiveDate>,
    /// Set when this duplicate was merged into the authoritative session (L3-I-006).
    pub merged_into_id: Option<Uuid>,
    /// Governed revisions recorded after the draft stage (L3-I-005).
    pub revisions: i64,
    pub papers: Vec<ExamPaperView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamSessionList {
    pub items: Vec<ExamSessionView>,
}

/// New examination session (draft).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSessionCreate {
    /// Domain B academic term of the institution.
    pub academic_term_id: Uuid,
    #[schema(example = "ESE-NOV-2026")]
    pub code: String,
    pub name: String,
    pub kind: Option<ExamSessionKind>,
    pub grading_scheme_id: Option<Uuid>,
    /// Defaults to the institution's active policy.
    pub policy_id: Option<Uuid>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub registration_opens_on: Option<NaiveDate>,
    pub registration_closes_on: Option<NaiveDate>,
}

/// Session edit; after the draft stage it is a governed revision (reason required, recorded
/// with before / after values).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSessionUpdate {
    pub name: Option<String>,
    pub grading_scheme_id: Option<Uuid>,
    pub policy_id: Option<Uuid>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub registration_opens_on: Option<NaiveDate>,
    pub registration_closes_on: Option<NaiveDate>,
    pub reason: Option<String>,
}

/// Merge a duplicate session into the authoritative one (L3-I-006).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSessionMerge {
    pub into_session_id: Uuid,
    pub reason: Option<String>,
}

/// New paper from a Domain D offering of the session's term.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamPaperCreate {
    pub offering_id: Uuid,
    /// Defaults to the offering code.
    pub paper_code: Option<String>,
    /// Defaults to the offering title.
    pub title: Option<String>,
    pub mode: Option<ExamMode>,
    /// External maximum (default `"100"`).
    pub max_marks: Option<String>,
    /// Default `"40"`.
    pub pass_percent: Option<String>,
    /// Share of internal assessment (default `"0"`).
    pub internal_weight: Option<String>,
    /// Default 180.
    pub duration_minutes: Option<i32>,
}

/// Paper edit (draft / scheduled papers only).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamPaperUpdate {
    pub title: Option<String>,
    pub mode: Option<ExamMode>,
    pub max_marks: Option<String>,
    pub pass_percent: Option<String>,
    pub internal_weight: Option<String>,
    pub duration_minutes: Option<i32>,
}

/// Defaults for papers generated from every offering of the term.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamPaperGenerate {
    pub mode: Option<ExamMode>,
    pub max_marks: Option<String>,
    pub pass_percent: Option<String>,
    pub internal_weight: Option<String>,
    pub duration_minutes: Option<i32>,
}

// ------------------------------------------------------------------ loading

/// Loads a session (404 when missing / foreign).
pub async fn load_session<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ExamSessionSummary, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM exams.exam_session WHERE id = $1 AND tenant_id = $2"
    );
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "examination session",
        })?;
    session_from_row(&row).map_err(map_db_error)
}

/// Loads a paper (404 when missing / foreign).
pub async fn load_paper<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ExamPaperSummary, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let sql =
        format!("SELECT {PAPER_COLUMNS} FROM exams.exam_paper WHERE id = $1 AND tenant_id = $2");
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(executor)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "examination paper",
        })?;
    paper_from_row(&row).map_err(map_db_error)
}

/// Papers of a session ordered by code.
pub async fn papers_of<'c, E>(
    executor: E,
    tenant: TenantId,
    session: Uuid,
) -> Result<Vec<ExamPaperSummary>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let sql = format!(
        "SELECT {PAPER_COLUMNS} FROM exams.exam_paper WHERE session_id = $1 AND tenant_id = $2 \
         ORDER BY paper_code"
    );
    sqlx::query(&sql)
        .bind(session)
        .bind(tenant.as_uuid())
        .fetch_all(executor)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(|r| paper_from_row(r).map_err(map_db_error))
        .collect()
}

async fn merged_into(
    st: &ExamsState,
    tenant: TenantId,
    session: Uuid,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT merged_into_session_id FROM exams.exam_session_revision WHERE tenant_id = $1 \
         AND session_id = $2 AND kind = 'merge' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(session)
    .fetch_optional(&st.pool)
    .await
    .map(Option::flatten)
    .map_err(map_db_error)
}

/// Full session view (papers, merge marker, revision count).
pub async fn session_view(
    st: &ExamsState,
    tenant: TenantId,
    s: ExamSessionSummary,
) -> Result<ExamSessionView, KernelError> {
    let id = s.id.as_uuid();
    let papers = papers_of(&st.pool, tenant, id).await?;
    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.exam_session_revision WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(ExamSessionView {
        id: s.id,
        institution_id: s.institution_id.as_uuid(),
        academic_year_id: s.academic_year_id,
        academic_term_id: s.academic_term_id,
        code: s.code,
        name: s.name,
        kind: s.kind,
        status: s.status,
        policy_id: s.policy_id,
        grading_scheme_id: s.grading_scheme_id,
        starts_on: s.starts_on,
        ends_on: s.ends_on,
        registration_opens_on: s.registration_opens_on,
        registration_closes_on: s.registration_closes_on,
        merged_into_id: merged_into(st, tenant, id).await?,
        revisions,
        papers: papers.iter().map(ExamPaperView::from).collect(),
        version: s.version,
    })
}

async fn respond(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let s = load_session(&st.pool, tenant, id).await?;
    let v = session_view(st, tenant, s).await?;
    let version = v.version;
    Ok(with_etag(status, &v, Version(version)))
}

/// One governed revision row (append-only).
pub struct RevisionRow<'a> {
    pub kind: &'static str,
    pub changed: Vec<String>,
    pub before: serde_json::Value,
    pub after: serde_json::Value,
    pub reason: &'a str,
    pub merged_into: Option<Uuid>,
}

/// Appends an `exams.exam_session_revision` row for `s` (as it stands after the change).
pub async fn record_revision(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &ExamSessionSummary,
    r: RevisionRow<'_>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO exams.exam_session_revision (tenant_id, institution_id, session_id, kind, \
         changed_fields, before_values, after_values, reason, merged_into_session_id, \
         session_status, session_version, actor) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(tenant.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(s.id.as_uuid())
    .bind(r.kind)
    .bind(&r.changed)
    .bind(&r.before)
    .bind(&r.after)
    .bind(r.reason)
    .bind(r.merged_into)
    .bind(s.status.as_str())
    .bind(s.version)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

// ------------------------------------------------------------------ validation

async fn check_scheme(
    st: &ExamsState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    id: Uuid,
) -> Result<(), KernelError> {
    match st
        .directory
        .grading_scheme(scope, GradingSchemeId(id))
        .await?
    {
        Some(s) if s.institution_id == institution && s.status == GradingSchemeStatus::Active => {
            Ok(())
        }
        Some(_) => Err(invalid(
            "gradingSchemeId",
            "unusable",
            "the grading scheme is retired or belongs to another institution",
        )),
        None => Err(invalid(
            "gradingSchemeId",
            "not_found",
            "unknown grading scheme",
        )),
    }
}

async fn check_policy(
    st: &ExamsState,
    tenant: TenantId,
    institution: InstitutionId,
    id: Uuid,
) -> Result<(), KernelError> {
    let p = load_policy(&st.pool, tenant, id, false)
        .await
        .map_err(|_| invalid("policyId", "not_found", "unknown examination policy"))?;
    let usable = matches!(
        ExamPolicyStatus::parse(&p.status),
        Some(ExamPolicyStatus::Active | ExamPolicyStatus::Scheduled | ExamPolicyStatus::Approved)
    );
    if p.institution_id != institution.as_uuid() || !usable {
        return Err(invalid(
            "policyId",
            "unusable",
            "the policy must be an approved, scheduled or active version of this institution",
        ));
    }
    Ok(())
}

struct Dates {
    starts_on: Option<NaiveDate>,
    ends_on: Option<NaiveDate>,
    opens: Option<NaiveDate>,
    closes: Option<NaiveDate>,
}

fn check_dates(d: &Dates) -> Result<(), KernelError> {
    if let (Some(s), Some(e)) = (d.starts_on, d.ends_on) {
        if e < s {
            return Err(invalid(
                "endsOn",
                "before_start",
                "must not be before startsOn",
            ));
        }
    }
    if let (Some(o), Some(c)) = (d.opens, d.closes) {
        if c < o {
            return Err(invalid(
                "registrationClosesOn",
                "before_open",
                "must not be before registrationOpensOn",
            ));
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ sessions

/// Sessions of an institution, newest first (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/institutions/{id}/exam-sessions", operation_id = "exam_session_list", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamSessionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamSessionList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let scope = institution_scope(ctx, InstitutionId(id));
    require_institution(&st, &scope, InstitutionId(id)).await?;
    let tenant = ctx.scope.tenant_id;
    let sessions = st
        .directory
        .sessions_of_institution(&scope, InstitutionId(id))
        .await?;
    let mut items = Vec::with_capacity(sessions.len());
    for s in sessions {
        items.push(session_view(&st, tenant, s).await?);
    }
    Ok(Json(ExamSessionList { items }))
}

/// Create a draft session for an academic term (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/institutions/{id}/exam-sessions", operation_id = "exam_session_create", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamSessionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSessionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let tenant = ctx.scope.tenant_id;
    let code = require_code("code", &b.code)?;
    let name = require_name("name", &b.name)?;
    let term = st
        .references
        .academic_term(&scope, b.academic_term_id)
        .await?
        .filter(|t| t.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "not_found",
                "unknown academic term of this institution",
            )
        })?;
    if let Some(scheme) = b.grading_scheme_id {
        check_scheme(&st, &scope, institution, scheme).await?;
    }
    let policy_id = match b.policy_id {
        Some(p) => {
            check_policy(&st, tenant, institution, p).await?;
            Some(p)
        }
        None => active_policy(&st.pool, tenant, institution)
            .await?
            .map(|p| p.id),
    };
    check_dates(&Dates {
        starts_on: b.starts_on,
        ends_on: b.ends_on,
        opens: b.registration_opens_on,
        closes: b.registration_closes_on,
    })?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_session (tenant_id, institution_id, academic_year_id, \
         academic_term_id, code, name, kind, policy_id, grading_scheme_id, starts_on, ends_on, \
         registration_opens_on, registration_closes_on, status_changed_at, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, now(), $14) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .bind(term.academic_year_id)
    .bind(term.id)
    .bind(&code)
    .bind(&name)
    .bind(b.kind.unwrap_or(ExamSessionKind::Regular).as_str())
    .bind(policy_id)
    .bind(b.grading_scheme_id)
    .bind(b.starts_on)
    .bind(b.ends_on)
    .bind(b.registration_opens_on)
    .bind(b.registration_closes_on)
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    crate::shared::lifecycle::record_history(
        &mut tx,
        crate::shared::lifecycle::HistoryEntry {
            tenant,
            institution,
            aggregate_type: "exam_session",
            aggregate_id: new_id,
            from: None,
            to: "draft",
        },
        &StatusChange {
            cause_event_type: "exams.exam_session.create",
            reason: None,
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_session.create",
        "exam_session",
        new_id.to_string(),
        json!({"code": code, "academicTermId": term.id}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, new_id, StatusCode::CREATED).await
}

/// One session with its papers (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}", operation_id = "exam_session_get", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    respond(&st, ctx.scope.tenant_id, id, StatusCode::OK).await
}

/// Edit a session (`exam.setup.manage`, `If-Match`). After the draft stage the edit is a
/// governed revision: `reason` required, before / after recorded; activities already completed
/// under the session keep their context (L3-I-005).
#[utoipa::path(patch, path = "/api/v1/exams/exam-sessions/{id}", operation_id = "exam_session_update", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = ExamSessionUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_update(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSessionUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let cur = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    check_version(expected, cur.version)?;
    if !cur.status.is_configurable() {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; it can no longer be revised",
            cur.code, cur.status
        ))
        .into());
    }
    if merged_into(&st, tenant, id).await?.is_some() {
        return Err(KernelError::Conflict(format!(
            "examination session {} was merged into another session",
            cur.code
        ))
        .into());
    }
    let reason = clean(b.reason.as_deref());
    let governed = cur.status != ExamSessionStatus::Draft;
    if governed {
        require_reason(&reason)?;
    }
    if let Some(scheme) = b.grading_scheme_id {
        if Some(GradingSchemeId(scheme)) != cur.grading_scheme_id {
            if cur.status.is_operational() {
                return Err(KernelError::Conflict(
                    "the grading scheme of an operational session cannot change".into(),
                )
                .into());
            }
            check_scheme(&st, &scope, cur.institution_id, scheme).await?;
        }
    }
    if let Some(p) = b.policy_id {
        if Some(p) != cur.policy_id {
            check_policy(&st, tenant, cur.institution_id, p).await?;
        }
    }
    let name = match b.name.as_deref() {
        Some(n) => require_name("name", n)?,
        None => cur.name.clone(),
    };
    let next = Dates {
        starts_on: b.starts_on.or(cur.starts_on),
        ends_on: b.ends_on.or(cur.ends_on),
        opens: b.registration_opens_on.or(cur.registration_opens_on),
        closes: b.registration_closes_on.or(cur.registration_closes_on),
    };
    check_dates(&next)?;
    let scheme = b
        .grading_scheme_id
        .or(cur.grading_scheme_id.map(|g| g.as_uuid()));
    let policy = b.policy_id.or(cur.policy_id);
    let before = json!({"name": cur.name, "startsOn": cur.starts_on, "endsOn": cur.ends_on,
        "registrationOpensOn": cur.registration_opens_on, "registrationClosesOn": cur.registration_closes_on,
        "policyId": cur.policy_id, "gradingSchemeId": cur.grading_scheme_id});
    let after = json!({"name": name, "startsOn": next.starts_on, "endsOn": next.ends_on,
        "registrationOpensOn": next.opens, "registrationClosesOn": next.closes,
        "policyId": policy, "gradingSchemeId": scheme});
    let changed: Vec<String> = after
        .as_object()
        .map(|a| {
            a.iter()
                .filter(|(k, v)| before.get(k.as_str()) != Some(*v))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default();
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_session(&mut tx, tenant, ExamSessionId(id)).await?;
    check_version(expected, s.version)?;
    let row = sqlx::query(&format!(
        "UPDATE exams.exam_session SET name = $3, starts_on = $4, ends_on = $5, \
         registration_opens_on = $6, registration_closes_on = $7, policy_id = $8, \
         grading_scheme_id = $9, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING {SESSION_COLUMNS}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(&name)
    .bind(next.starts_on)
    .bind(next.ends_on)
    .bind(next.opens)
    .bind(next.closes)
    .bind(policy)
    .bind(scheme)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let updated = session_from_row(&row).map_err(map_db_error)?;
    if governed && !changed.is_empty() {
        record_revision(
            &mut tx,
            tenant,
            &updated,
            RevisionRow {
                kind: "revision",
                changed: changed.clone(),
                before,
                after,
                reason: reason.as_deref().unwrap_or_default(),
                merged_into: None,
            },
            &act,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_session.update",
        "exam_session",
        id.to_string(),
        json!({"changed": changed, "governed": governed, "reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Context of a session status command.
struct SessionMove<'a> {
    ctx: &'a AuthContext,
    id: Uuid,
    expected: Version,
    to: ExamSessionStatus,
    reason: Option<String>,
    event: Option<(&'static str, &'static str)>,
}

/// Locks, version-checks and moves a session (+ event + audit) in one transaction.
async fn move_session(st: &ExamsState, m: SessionMove<'_>) -> ApiResult<Response> {
    let tenant = m.ctx.scope.tenant_id;
    let cur = load_session(&st.pool, tenant, m.id).await?;
    let scope = institution_scope(m.ctx, cur.institution_id);
    require_institution_writable(st, &scope, cur.institution_id).await?;
    let act = actor(m.ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_session(&mut tx, tenant, ExamSessionId(m.id)).await?;
    check_version(m.expected, s.version)?;
    let updated = change_session_status(
        &mut tx,
        tenant,
        &s,
        m.to,
        &StatusChange {
            cause_event_type: m.event.map_or("exams.exam_session.move", |e| e.0),
            reason: m.reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    if let Some(kind) = m.event {
        let mut e = status_event(
            m.id,
            s.institution_id,
            Some(s.status.as_str()),
            m.to.as_str(),
            updated.version,
        );
        e.session_id = Some(m.id);
        e.reason.clone_from(&m.reason);
        emit(&mut tx, &scope, kind, &e, &act).await?;
    }
    audit(
        &mut tx,
        &scope,
        &format!("exams.exam_session.{}", m.to.as_str()),
        "exam_session",
        m.id.to_string(),
        json!({"from": s.status.as_str(), "reason": m.reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(st, tenant, m.id, StatusCode::OK).await
}

/// Submit a draft session for review (`exam.setup.manage`); `SESSION_APPROVAL` decides
/// (auto-approved in Batch 6). A second governed session of the same term and kind is a
/// duplicate (`409`; merge it instead, L3-I-006).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/submit", operation_id = "exam_session_submit", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_submit(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let tenant = ctx.scope.tenant_id;
    let cur = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    check_version(expected, cur.version)?;
    if !cur.status.can_transition(ExamSessionStatus::UnderReview) {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; it cannot become under_review",
            cur.code, cur.status
        ))
        .into());
    }
    if merged_into(&st, tenant, id).await?.is_some() {
        return Err(KernelError::Conflict(format!(
            "examination session {} was merged into another session",
            cur.code
        ))
        .into());
    }
    let duplicate: Option<String> = sqlx::query_scalar(
        "SELECT code FROM exams.exam_session WHERE tenant_id = $1 AND institution_id = $2 \
         AND academic_term_id = $3 AND kind = $4 AND id <> $5 AND status IN \
         ('under_review', 'approved', 'scheduled', 'active', 'suspended', 'reopened') LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(cur.institution_id.as_uuid())
    .bind(cur.academic_term_id)
    .bind(cur.kind.as_str())
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    if let Some(code) = duplicate {
        return Err(KernelError::Conflict(format!(
            "session {code} already governs this term ({}); merge this duplicate into it",
            cur.kind
        ))
        .into());
    }
    let outcome = request_approval(
        &st,
        &scope,
        process::SESSION_APPROVAL,
        business_key("exam_session", id, cur.version),
        BTreeMap::from([
            ("code".to_owned(), json!(cur.code)),
            ("academicTermId".to_owned(), json!(cur.academic_term_id)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let auto = matches!(outcome, ApprovalOutcome::Approved { .. });
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_session(&mut tx, tenant, ExamSessionId(id)).await?;
    check_version(expected, s.version)?;
    let pending = change_session_status(
        &mut tx,
        tenant,
        &s,
        ExamSessionStatus::UnderReview,
        &StatusChange {
            cause_event_type: EXAM_SESSION_SUBMITTED,
            reason: reason.as_deref(),
            process_instance_id: Some(&pid),
        },
        &act,
    )
    .await?;
    let mut e = status_event(
        id,
        s.institution_id,
        Some("draft"),
        "under_review",
        pending.version,
    );
    e.session_id = Some(id);
    e.process_instance_id = Some(pid.clone());
    e.reason = Some(if auto {
        AUTO_APPROVED.to_owned()
    } else {
        format!("{} — {}", s.code, s.name)
    });
    emit(
        &mut tx,
        &scope,
        (EXAM_SESSION_SUBMITTED, EXAM_SESSION_SUBMITTED_SCHEMA),
        &e,
        &act,
    )
    .await?;
    if auto {
        let sys = Actor::system(act.correlation_id.clone());
        let approved = change_session_status(
            &mut tx,
            tenant,
            &pending,
            ExamSessionStatus::Approved,
            &StatusChange {
                cause_event_type: EXAM_SESSION_APPROVED,
                reason: Some(AUTO_APPROVED),
                process_instance_id: Some(&pid),
            },
            &sys,
        )
        .await?;
        let mut e = status_event(
            id,
            s.institution_id,
            Some("under_review"),
            "approved",
            approved.version,
        );
        e.session_id = Some(id);
        e.process_instance_id = Some(pid.clone());
        emit(
            &mut tx,
            &scope,
            (EXAM_SESSION_APPROVED, EXAM_SESSION_APPROVED_SCHEMA),
            &e,
            &sys,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_session.submit",
        "exam_session",
        id.to_string(),
        json!({"processInstanceId": pid, "autoApproved": auto}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Reason recorded on automatic approvals (the inbox rule skips the decision to-do for it).
pub const AUTO_APPROVED: &str = "auto-approved";

/// Decide a session under review (`exam.approve`): approve → `approved`, reject → `draft`
/// (reason required).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/decision", operation_id = "exam_session_decision", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body = ExamSetupDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_decision(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSetupDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    let cur = load_session(&st.pool, ctx.scope.tenant_id, id).await?;
    if cur.status != ExamSessionStatus::UnderReview {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; only a session under review can be decided",
            cur.code, cur.status
        ))
        .into());
    }
    let (to, event) = match b.decision {
        ExamSetupDecisionKind::Approve => (
            ExamSessionStatus::Approved,
            Some((EXAM_SESSION_APPROVED, EXAM_SESSION_APPROVED_SCHEMA)),
        ),
        ExamSetupDecisionKind::Reject => {
            require_reason(&reason)?;
            (ExamSessionStatus::Draft, None)
        }
    };
    move_session(
        &st,
        SessionMove {
            ctx,
            id,
            expected,
            to,
            reason,
            event,
        },
    )
    .await
}

/// Schedule an approved session for its operational period (`exam.setup.manage`); a grading
/// scheme must be bound.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/schedule", operation_id = "exam_session_schedule", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_schedule(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let cur = load_session(&st.pool, ctx.scope.tenant_id, id).await?;
    if cur.grading_scheme_id.is_none() {
        return Err(KernelError::Conflict(
            "bind a grading scheme before scheduling the session".into(),
        )
        .into());
    }
    move_session(
        &st,
        SessionMove {
            ctx,
            id,
            expected,
            to: ExamSessionStatus::Scheduled,
            reason: reason_of(body),
            event: None,
        },
    )
    .await
}

/// Activate a scheduled session (`exam.setup.manage`): from now on operational workflows
/// (registration, conduct, marks, results) may consume it. Needs a grading scheme and at least
/// one live paper.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/activate", operation_id = "exam_session_activate", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_activate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let cur = load_session(&st.pool, tenant, id).await?;
    if cur.grading_scheme_id.is_none() {
        return Err(KernelError::Conflict(
            "bind a grading scheme before activating the session".into(),
        )
        .into());
    }
    let live = papers_of(&st.pool, tenant, id)
        .await?
        .iter()
        .filter(|p| p.status != ExamPaperStatus::Cancelled)
        .count();
    if live == 0 {
        return Err(KernelError::Conflict(
            "the session has no papers; add papers before activating it".into(),
        )
        .into());
    }
    move_session(
        &st,
        SessionMove {
            ctx,
            id,
            expected,
            to: ExamSessionStatus::Active,
            reason: reason_of(body),
            event: Some((EXAM_SESSION_ACTIVATED, EXAM_SESSION_ACTIVATED_SCHEMA)),
        },
    )
    .await
}

/// Suspend an operational session (`exam.setup.manage`, reason required, L3-I-005).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/suspend", operation_id = "exam_session_suspend", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_suspend(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    require_reason(&reason)?;
    move_session(
        &st,
        SessionMove {
            ctx,
            id,
            expected,
            to: ExamSessionStatus::Suspended,
            reason,
            event: Some((EXAM_SESSION_SUSPENDED, EXAM_SESSION_SUSPENDED_SCHEMA)),
        },
    )
    .await
}

/// Reopen a suspended session (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/reopen", operation_id = "exam_session_reopen", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the session")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_reopen(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    move_session(
        &st,
        SessionMove {
            ctx,
            id,
            expected,
            to: ExamSessionStatus::Reopened,
            reason: reason_of(body),
            event: Some((EXAM_SESSION_REOPENED, EXAM_SESSION_REOPENED_SCHEMA)),
        },
    )
    .await
}

/// Merge a duplicate session (draft / under review / approved, no registrations) into the
/// authoritative session of the same term (`exam.setup.manage`, L3-I-006): papers for offerings
/// the target lacks move over, the rest are cancelled; the duplicate returns to draft and is
/// marked merged (it can no longer be submitted or revised).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/merge", operation_id = "exam_session_merge", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duplicate session")),
    request_body = ExamSessionMerge, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSessionView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn session_merge(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSessionMerge>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    require_reason(&reason)?;
    let tenant = ctx.scope.tenant_id;
    if b.into_session_id == id {
        return Err(invalid(
            "intoSessionId",
            "same_session",
            "cannot merge a session into itself",
        )
        .into());
    }
    let src = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, src.institution_id);
    require_institution_writable(&st, &scope, src.institution_id).await?;
    check_version(expected, src.version)?;
    let target = load_session(&st.pool, tenant, b.into_session_id)
        .await
        .map_err(|_| invalid("intoSessionId", "not_found", "unknown examination session"))?;
    if target.institution_id != src.institution_id
        || target.academic_term_id != src.academic_term_id
    {
        return Err(invalid(
            "intoSessionId",
            "different_term",
            "the authoritative session must belong to the same institution and term",
        )
        .into());
    }
    if matches!(
        target.status,
        ExamSessionStatus::Closing | ExamSessionStatus::Closed | ExamSessionStatus::Archived
    ) {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; nothing can be merged into it",
            target.code, target.status
        ))
        .into());
    }
    if !matches!(
        src.status,
        ExamSessionStatus::Draft | ExamSessionStatus::UnderReview | ExamSessionStatus::Approved
    ) {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; only a draft, under-review or approved duplicate can be merged",
            src.code, src.status
        ))
        .into());
    }
    if merged_into(&st, tenant, id).await?.is_some() {
        return Err(KernelError::Conflict("the session was already merged".into()).into());
    }
    let regs: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.exam_registration WHERE tenant_id = $1 AND session_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    if regs > 0 {
        return Err(KernelError::Conflict(
            "the duplicate already has registrations; it cannot be merged".into(),
        )
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_session(&mut tx, tenant, ExamSessionId(id)).await?;
    check_version(expected, s.version)?;
    lock_session(&mut tx, tenant, target.id).await?;
    let target_papers = papers_of(&mut *tx, tenant, target.id.as_uuid()).await?;
    let (mut moved, mut cancelled) = (Vec::new(), Vec::new());
    for p in papers_of(&mut *tx, tenant, id).await? {
        if p.status == ExamPaperStatus::Cancelled {
            continue;
        }
        let clash = target_papers.iter().any(|t| {
            t.status != ExamPaperStatus::Cancelled
                && (t.paper_code == p.paper_code
                    || (t.offering_id.is_some() && t.offering_id == p.offering_id))
        });
        let movable = matches!(
            p.status,
            ExamPaperStatus::Draft | ExamPaperStatus::Scheduled
        );
        if clash || !movable {
            let locked = lock_paper(&mut tx, tenant, p.id).await?;
            change_paper_status(
                &mut tx,
                tenant,
                &locked,
                ExamPaperStatus::Cancelled,
                &StatusChange {
                    cause_event_type: "exams.exam_session.merge",
                    reason: Some("duplicate session merged"),
                    process_instance_id: None,
                },
                &act,
            )
            .await?;
            cancelled.push(p.paper_code.clone());
        } else {
            sqlx::query(
                "UPDATE exams.exam_paper SET session_id = $3, version = version + 1, \
                 updated_at = now() WHERE id = $1 AND tenant_id = $2",
            )
            .bind(p.id.as_uuid())
            .bind(tenant.as_uuid())
            .bind(target.id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            moved.push(p.paper_code.clone());
        }
    }
    let after = if s.status == ExamSessionStatus::Draft {
        s.clone()
    } else {
        change_session_status(
            &mut tx,
            tenant,
            &s,
            ExamSessionStatus::Draft,
            &StatusChange {
                cause_event_type: "exams.exam_session.merge",
                reason: Some("merged into the authoritative session"),
                process_instance_id: None,
            },
            &act,
        )
        .await?
    };
    record_revision(
        &mut tx,
        tenant,
        &after,
        RevisionRow {
            kind: "merge",
            changed: vec!["papers".into()],
            before: json!({"status": s.status.as_str()}),
            after: json!({"mergedInto": target.id, "movedPapers": moved, "cancelledPapers": cancelled}),
            reason: reason.as_deref().unwrap_or_default(),
            merged_into: Some(target.id.as_uuid()),
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_session.merge",
        "exam_session",
        id.to_string(),
        json!({"into": target.id, "moved": moved, "cancelled": cancelled}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

// ------------------------------------------------------------------ papers

struct PaperSpec {
    mode: ExamMode,
    max_marks: Decimal,
    pass_percent: Decimal,
    internal_weight: Decimal,
    duration_minutes: Option<i32>,
}

fn paper_spec(
    mode: Option<ExamMode>,
    max: Option<&str>,
    pass: Option<&str>,
    weight: Option<&str>,
    duration: Option<i32>,
    base: Option<&ExamPaperSummary>,
) -> Result<PaperSpec, KernelError> {
    let dec = |field: &str, raw: Option<&str>, dflt: Decimal| -> Result<Decimal, KernelError> {
        match raw {
            Some(r) => parse_dec(field, r),
            None => Ok(dflt),
        }
    };
    let max_marks = dec(
        "maxMarks",
        max,
        base.map_or(Decimal::ONE_HUNDRED, |b| b.max_marks),
    )?;
    let pass_percent = dec(
        "passPercent",
        pass,
        base.map_or(Decimal::new(40, 0), |b| b.pass_percent),
    )?;
    let internal_weight = dec(
        "internalWeight",
        weight,
        base.map_or(Decimal::ZERO, |b| b.internal_weight),
    )?;
    if max_marks <= Decimal::ZERO || max_marks > Decimal::new(1000, 0) || max_marks.scale() > 2 {
        return Err(invalid(
            "maxMarks",
            "out_of_range",
            "must be > 0 and ≤ 1000 with ≤ 2 decimals",
        ));
    }
    for (f, v) in [
        ("passPercent", pass_percent),
        ("internalWeight", internal_weight),
    ] {
        if v < Decimal::ZERO || v > Decimal::ONE_HUNDRED || v.scale() > 2 {
            return Err(invalid(f, "out_of_range", "must be between 0 and 100"));
        }
    }
    let duration_minutes = duration.or(base.map_or(Some(180), |b| b.duration_minutes));
    if duration_minutes.is_some_and(|d| !(1..=1440).contains(&d)) {
        return Err(invalid(
            "durationMinutes",
            "out_of_range",
            "must be 1 … 1440",
        ));
    }
    Ok(PaperSpec {
        mode: mode.or(base.map(|b| b.mode)).unwrap_or(ExamMode::Theory),
        max_marks,
        pass_percent,
        internal_weight,
        duration_minutes,
    })
}

fn offering_usable(o: &OfferingSummary) -> bool {
    !matches!(
        o.status,
        CourseOfferingStatus::Cancelled
            | CourseOfferingStatus::Archived
            | CourseOfferingStatus::Merged
            | CourseOfferingStatus::Split
    )
}

/// Inserts one paper of `offering` into `s` (caller validated the offering).
async fn insert_paper(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &ExamSessionSummary,
    o: &OfferingSummary,
    code: &str,
    title: &str,
    spec: &PaperSpec,
) -> Result<Uuid, KernelError> {
    sqlx::query_scalar(
        "INSERT INTO exams.exam_paper (tenant_id, institution_id, session_id, offering_id, \
         curriculum_component_id, academic_term_id, paper_code, title, credits, mode, max_marks, \
         pass_percent, internal_weight, duration_minutes) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(s.institution_id.as_uuid())
    .bind(s.id.as_uuid())
    .bind(o.id.as_uuid())
    .bind(o.curriculum_component_id)
    .bind(s.academic_term_id)
    .bind(code)
    .bind(title)
    .bind(credits_of(o.credits))
    .bind(spec.mode.as_str())
    .bind(spec.max_marks)
    .bind(spec.pass_percent)
    .bind(spec.internal_weight)
    .bind(spec.duration_minutes)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

fn paper_response(p: &ExamPaperSummary, status: StatusCode) -> Response {
    with_etag(status, &ExamPaperView::from(p), Version(p.version))
}

/// Papers of a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/papers", operation_id = "exam_paper_list", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamPaperList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamPaperList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_session(&st.pool, tenant, id).await?;
    let items = papers_of(&st.pool, tenant, id)
        .await?
        .iter()
        .map(ExamPaperView::from)
        .collect();
    Ok(Json(ExamPaperList { items }))
}

/// Add a paper for a Domain D offering of the session's term (`exam.setup.manage`). Credits,
/// title and curriculum component come from the offering; one live paper per offering (`409`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/papers", operation_id = "exam_paper_create", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamPaperCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamPaperView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamPaperCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let s = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    if !s.status.is_configurable() {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; papers can no longer be added",
            s.code, s.status
        ))
        .into());
    }
    let o = st
        .offerings
        .offering(&scope, CourseOfferingId(b.offering_id))
        .await?
        .filter(|o| o.institution_id == s.institution_id)
        .ok_or_else(|| invalid("offeringId", "not_found", "unknown course offering"))?;
    if o.academic_term_id != s.academic_term_id {
        return Err(invalid(
            "offeringId",
            "other_term",
            "the offering belongs to another academic term",
        )
        .into());
    }
    if !offering_usable(&o) {
        return Err(invalid(
            "offeringId",
            "not_usable",
            "the offering is no longer running",
        )
        .into());
    }
    let live = papers_of(&st.pool, tenant, id)
        .await?
        .into_iter()
        .find(|p| p.status != ExamPaperStatus::Cancelled && p.offering_id == Some(o.id.as_uuid()));
    if let Some(p) = live {
        return Err(KernelError::Conflict(format!(
            "paper {} already examines this offering in the session",
            p.paper_code
        ))
        .into());
    }
    let code = match b.paper_code.as_deref() {
        Some(c) => require_code("paperCode", c)?,
        None => require_code("paperCode", &code_from(&o.offering_code))?,
    };
    let title = match b.title.as_deref() {
        Some(t) => require_name("title", t)?,
        None => o.title.chars().take(200).collect(),
    };
    let spec = paper_spec(
        b.mode,
        b.max_marks.as_deref(),
        b.pass_percent.as_deref(),
        b.internal_weight.as_deref(),
        b.duration_minutes,
        None,
    )?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let pid = insert_paper(&mut tx, tenant, &s, &o, &code, &title, &spec).await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_paper.create",
        "exam_paper",
        pid.to_string(),
        json!({"sessionId": id, "offeringId": o.id, "paperCode": code}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let p = load_paper(&st.pool, tenant, pid).await?;
    Ok(paper_response(&p, StatusCode::CREATED))
}

/// Generate one paper per running offering of the session's term that has none yet
/// (`exam.setup.manage`, bulk: `Idempotency-Key`, summary).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/papers/generate", operation_id = "exam_paper_generate", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamPaperGenerate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSetupBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_generate(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ExamPaperGenerate>>,
) -> ApiResult<Json<ExamSetupBulkSummary>> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let b = body.map(|Json(b)| b).unwrap_or_default();
    let tenant = ctx.scope.tenant_id;
    let s = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    if !s.status.is_configurable() {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; papers can no longer be added",
            s.code, s.status
        ))
        .into());
    }
    let spec = paper_spec(
        b.mode,
        b.max_marks.as_deref(),
        b.pass_percent.as_deref(),
        b.internal_weight.as_deref(),
        b.duration_minutes,
        None,
    )?;
    let offerings = st
        .offerings
        .offerings_in_term(&scope, s.institution_id, s.academic_term_id)
        .await?;
    let existing = papers_of(&st.pool, tenant, id).await?;
    let mut summary = ExamSetupBulkSummary::default();
    let act = actor(ctx);
    for o in offerings {
        let taken = existing.iter().any(|p| {
            p.status != ExamPaperStatus::Cancelled && p.offering_id == Some(o.id.as_uuid())
        });
        if taken || !offering_usable(&o) {
            summary.skipped += 1;
            continue;
        }
        let code = code_from(&o.offering_code);
        if require_code("paperCode", &code).is_err()
            || existing.iter().any(|p| p.paper_code == code)
        {
            summary.failed.push(ExamSetupBulkFailure {
                id: Some(o.id.as_uuid()),
                reason: format!(
                    "offering code {} is not a usable paper code",
                    o.offering_code
                ),
            });
            continue;
        }
        let title: String = o.title.chars().take(200).collect();
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        match insert_paper(&mut tx, tenant, &s, &o, &code, &title, &spec).await {
            Ok(pid) => {
                audit(
                    &mut tx,
                    &scope,
                    "exams.exam_paper.create",
                    "exam_paper",
                    pid.to_string(),
                    json!({"sessionId": id, "offeringId": o.id, "generated": true}),
                    &act,
                )
                .await?;
                tx.commit().await.map_err(map_db_error)?;
                summary.processed += 1;
            }
            Err(e) => summary.failed.push(ExamSetupBulkFailure {
                id: Some(o.id.as_uuid()),
                reason: e.to_string(),
            }),
        }
    }
    Ok(Json(summary))
}

/// One paper (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-papers/{id}", operation_id = "exam_paper_get", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamPaperView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let p = load_paper(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(paper_response(&p, StatusCode::OK))
}

/// Edit a draft / scheduled paper (`exam.setup.manage`, `If-Match`).
#[utoipa::path(patch, path = "/api/v1/exams/exam-papers/{id}", operation_id = "exam_paper_update", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the paper")),
    request_body = ExamPaperUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPaperView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_update(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamPaperUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let cur = load_paper(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(&st, &scope, cur.institution_id).await?;
    check_version(expected, cur.version)?;
    if !matches!(
        cur.status,
        ExamPaperStatus::Draft | ExamPaperStatus::Scheduled
    ) {
        return Err(KernelError::Conflict(format!(
            "paper {} is {}; only draft or scheduled papers can be edited",
            cur.paper_code, cur.status
        ))
        .into());
    }
    let title = match b.title.as_deref() {
        Some(t) => require_name("title", t)?,
        None => cur.title.clone(),
    };
    let spec = paper_spec(
        b.mode,
        b.max_marks.as_deref(),
        b.pass_percent.as_deref(),
        b.internal_weight.as_deref(),
        b.duration_minutes,
        Some(&cur),
    )?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = lock_paper(&mut tx, tenant, ExamPaperId(id)).await?;
    check_version(expected, p.version)?;
    sqlx::query(
        "UPDATE exams.exam_paper SET title = $3, mode = $4, max_marks = $5, pass_percent = $6, \
         internal_weight = $7, duration_minutes = $8, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(&title)
    .bind(spec.mode.as_str())
    .bind(spec.max_marks)
    .bind(spec.pass_percent)
    .bind(spec.internal_weight)
    .bind(spec.duration_minutes)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_paper.update",
        "exam_paper",
        id.to_string(),
        json!({}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let p = load_paper(&st.pool, tenant, id).await?;
    Ok(paper_response(&p, StatusCode::OK))
}

/// Moves a locked paper (status only through `change_paper_status`) inside the caller's tx.
pub async fn move_paper(
    conn: &mut PgConnection,
    tenant: TenantId,
    paper: ExamPaperId,
    to: ExamPaperStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<ExamPaperSummary, KernelError> {
    let p = lock_paper(conn, tenant, paper).await?;
    if p.status == to {
        return Ok(p);
    }
    change_paper_status(
        conn,
        tenant,
        &p,
        to,
        &StatusChange {
            cause_event_type: "exams.exam_paper.move",
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await
}

async fn paper_command(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: ExamPaperStatus,
    reason: Option<String>,
) -> ApiResult<Response> {
    let tenant = ctx.scope.tenant_id;
    let cur = load_paper(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, cur.institution_id);
    require_institution_writable(st, &scope, cur.institution_id).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = lock_paper(&mut tx, tenant, ExamPaperId(id)).await?;
    check_version(expected, p.version)?;
    let updated = change_paper_status(
        &mut tx,
        tenant,
        &p,
        to,
        &StatusChange {
            cause_event_type: "exams.exam_paper.move",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        &format!("exams.exam_paper.{}", to.as_str()),
        "exam_paper",
        id.to_string(),
        json!({"from": p.status.as_str(), "reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(paper_response(&updated, StatusCode::OK))
}

/// Schedule a draft paper (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-papers/{id}/schedule", operation_id = "exam_paper_schedule", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the paper")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPaperView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_schedule(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    paper_command(&st, ctx, id, expected, ExamPaperStatus::Scheduled, reason).await
}

/// Mark a scheduled paper ready for conduct (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-papers/{id}/ready", operation_id = "exam_paper_ready", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the paper")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPaperView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_ready(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    paper_command(&st, ctx, id, expected, ExamPaperStatus::Ready, reason).await
}

/// Mark a ready paper conducted (`exam.setup.manage`; normally set by conduct completion).
#[utoipa::path(post, path = "/api/v1/exams/exam-papers/{id}/conducted", operation_id = "exam_paper_conducted", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the paper")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPaperView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_conducted(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    paper_command(&st, ctx, id, expected, ExamPaperStatus::Conducted, reason).await
}

/// Cancel a draft / scheduled / ready paper (`exam.setup.manage`, reason required).
#[utoipa::path(post, path = "/api/v1/exams/exam-papers/{id}/cancel", operation_id = "exam_paper_cancel", tag = "exam-setup",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the paper")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamPaperView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn paper_cancel(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    require_reason(&reason)?;
    paper_command(&st, ctx, id, expected, ExamPaperStatus::Cancelled, reason).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paper_spec_defaults_and_bounds() {
        let s = paper_spec(None, None, None, None, None, None).map_err(|e| e.to_string());
        let s = s.ok();
        assert_eq!(s.as_ref().map(|s| s.max_marks), Some(Decimal::ONE_HUNDRED));
        assert_eq!(s.as_ref().map(|s| s.duration_minutes), Some(Some(180)));
        assert!(paper_spec(None, Some("0"), None, None, None, None).is_err());
        assert!(paper_spec(None, None, Some("101"), None, None, None).is_err());
        assert!(paper_spec(None, None, None, None, Some(0), None).is_err());
        assert!(check_dates(&Dates {
            starts_on: NaiveDate::from_ymd_opt(2026, 11, 2),
            ends_on: NaiveDate::from_ymd_opt(2026, 11, 1),
            opens: None,
            closes: None,
        })
        .is_err());
    }
}
