//! Conduct of a sitting (L3-I-055 scheduled theory examination → completed conduct record,
//! I-043 non-theory assessment → completed assessment record, I-057 failed entry condition →
//! governed non-participation record) and contingencies (I-174 authorised suspension, I-175
//! resumption, I-185 postponement + reschedule under `CONTINGENCY_DECISION`).
//!
//! Completion records one immutable attendance row per effective candidate and sets each
//! registered paper `appeared` / `absent` through `shared::registration::set_paper_status`; a
//! candidate whose current hall ticket is withheld / withdrawn is recorded `denied_entry`
//! (paper `absent`, reason kept). The paper then becomes `conducted` (`change_paper_status`).
//! The candidates' `paper_mark` rows are left to 6B (the contract makes `shared::marks` 6B's
//! write path); 6B reads the registered paper status `absent`.

use super::invigilation::{duties_of, is_examiner};
use super::schedule::{load_schedule, load_sitting, parse_time};
use crate::setup::common::{
    check_version, clean, emit, illegal, invalid, move_status, reason_of, require_reason,
    status_event, ExamSetupDecisionBody, ExamSetupDecisionKind, Move, ReasonBody,
};
use crate::setup::domain::{conduct_can, duty_is_live, schedule_is_official};
use crate::setup::session::{load_paper, load_session, move_paper};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution_writable, require_operational};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::publish::audit;
use crate::shared::registration::set_paper_status;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ExamsState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::exams::events::setup::{
    EXAM_CONDUCT_COMPLETED, EXAM_CONDUCT_COMPLETED_SCHEMA, EXAM_CONDUCT_SUSPENDED,
    EXAM_CONDUCT_SUSPENDED_SCHEMA, EXAM_POSTPONED, EXAM_POSTPONED_SCHEMA,
};
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_contracts::exams::ids::{ExamPaperId, ExamSessionId};
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_contracts::exams::status::{
    ConductStatus, ExamMode, ExamPaperStatus, InvigilationDutyStatus, RegistrationPaperStatus,
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

#[derive(Debug, Clone, sqlx::FromRow)]
struct ConductRow {
    id: Uuid,
    institution_id: Uuid,
    session_id: Uuid,
    sitting_id: Uuid,
    paper_id: Uuid,
    status: String,
    status_reason: Option<String>,
    started_at: Option<DateTime<Utc>>,
    completed_at: Option<DateTime<Utc>>,
    present_count: i32,
    absent_count: i32,
    denied_count: i32,
    pending_postpone: Option<serde_json::Value>,
    process_instance_id: Option<String>,
    version: i64,
}

const CONDUCT_COLUMNS: &str = "id, institution_id, session_id, sitting_id, paper_id, status, \
    status_reason, started_at, completed_at, present_count, absent_count, denied_count, \
    pending_postpone, process_instance_id, version";

/// One attendance row of a completed conduct.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamConductAttendanceView {
    pub registration_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    /// `present | absent | denied_entry`.
    pub outcome: String,
    pub reason: Option<String>,
}

/// A conduct record with its attendance.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamConductRecordView {
    pub id: Uuid,
    pub session_id: Uuid,
    pub sitting_id: Uuid,
    pub paper_id: Uuid,
    /// `ConductStatus` wire name.
    pub status: String,
    pub status_reason: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub present_count: i32,
    pub absent_count: i32,
    pub denied_count: i32,
    /// Postponement awaiting its contingency decision.
    #[schema(value_type = Option<Object>)]
    pub pending_postpone: Option<serde_json::Value>,
    pub process_instance_id: Option<String>,
    pub attendance: Vec<ExamConductAttendanceView>,
    pub version: i64,
}

/// One candidate's attendance outcome at completion.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamAttendanceEntry {
    pub registration_id: Uuid,
    /// `present | absent | denied_entry`.
    pub outcome: String,
    pub reason: Option<String>,
}

/// Completion of a conduct: listed outcomes; unlisted candidates get `defaultOutcome`
/// (default `absent`).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamConductComplete {
    #[serde(default)]
    pub attendance: Vec<ExamAttendanceEntry>,
    pub default_outcome: Option<String>,
}

/// Postpone + reschedule a sitting (contingency, reason required).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamConductPostpone {
    pub new_date: NaiveDate,
    #[schema(example = "14:00")]
    pub new_starts_at: String,
    pub reason: Option<String>,
}

async fn load_conduct<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ConductRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ConductRow>(&format!(
        "SELECT {CONDUCT_COLUMNS} FROM exams.conduct_record WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "conduct record",
    })
}

async fn view(
    st: &ExamsState,
    tenant: TenantId,
    c: ConductRow,
) -> Result<ExamConductRecordView, KernelError> {
    let attendance = sqlx::query_as::<_, ExamConductAttendanceView>(
        "SELECT a.registration_id, a.student_id, r.student_number, r.display_name, a.outcome, a.reason \
         FROM exams.conduct_attendance a JOIN exams.exam_registration r ON r.id = a.registration_id \
         WHERE a.tenant_id = $1 AND a.conduct_id = $2 ORDER BY r.student_number",
    )
    .bind(tenant.as_uuid())
    .bind(c.id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(ExamConductRecordView {
        id: c.id,
        session_id: c.session_id,
        sitting_id: c.sitting_id,
        paper_id: c.paper_id,
        status: c.status,
        status_reason: c.status_reason,
        started_at: c.started_at,
        completed_at: c.completed_at,
        present_count: c.present_count,
        absent_count: c.absent_count,
        denied_count: c.denied_count,
        pending_postpone: c.pending_postpone,
        process_instance_id: c.process_instance_id,
        attendance,
        version: c.version,
    })
}

async fn respond(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let c = load_conduct(&st.pool, tenant, id).await?;
    let v = view(st, tenant, c).await?;
    let version = v.version;
    Ok(with_etag(status, &v, Version(version)))
}

/// The sitting's live conduct record, else its latest (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sittings/{id}/conduct", operation_id = "exam_conduct_get_for_sitting", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_for_sitting(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_sitting(&st.pool, tenant, id).await?;
    let cid: Uuid = sqlx::query_scalar(
        "SELECT id FROM exams.conduct_record WHERE tenant_id = $1 AND sitting_id = $2 \
         ORDER BY (status NOT IN ('postponed', 'terminated')) DESC, created_at DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "conduct record",
    })?;
    respond(&st, tenant, cid, StatusCode::OK).await
}

/// Open the conduct record of a sitting (`exam.setup.manage`): the session must be operational
/// and the timetable official; practical / viva / project papers need a live examiner duty
/// (L3-I-012 / I-043). A scheduled paper becomes `ready`.
#[utoipa::path(post, path = "/api/v1/exams/exam-sittings/{id}/conduct", operation_id = "exam_conduct_open", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_open(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let x = load_sitting(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(x.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(x.institution_id)).await?;
    let session = load_session(&st.pool, tenant, x.session_id).await?;
    require_operational(&session)?;
    let schedule = load_schedule(&st.pool, tenant, x.schedule_id).await?;
    if !schedule_is_official(schedule.state()?) {
        return Err(KernelError::Conflict(format!(
            "schedule {} is {}; only a published timetable can be conducted",
            schedule.name, schedule.status
        ))
        .into());
    }
    if x.status != "planned" {
        return Err(KernelError::Conflict(format!("the sitting is {}", x.status)).into());
    }
    let paper = load_paper(&st.pool, tenant, x.paper_id).await?;
    if paper.mode != ExamMode::Theory {
        let examiners = duties_of(&st.pool, tenant, id)
            .await?
            .iter()
            .filter(|d| {
                is_examiner(&d.role)
                    && InvigilationDutyStatus::parse(&d.status).is_some_and(duty_is_live)
            })
            .count();
        if examiners == 0 {
            return Err(KernelError::Conflict(format!(
                "{} is a {} assessment; assign a practical / external examiner first",
                paper.paper_code, paper.mode
            ))
            .into());
        }
    }
    if !matches!(
        paper.status,
        ExamPaperStatus::Scheduled | ExamPaperStatus::Ready
    ) {
        return Err(KernelError::Conflict(format!(
            "paper {} is {}; it must be scheduled or ready",
            paper.paper_code, paper.status
        ))
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_paper(
        &mut tx,
        tenant,
        paper.id,
        ExamPaperStatus::Ready,
        Some("conduct opened"),
        &act,
    )
    .await?;
    let cid: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.conduct_record (tenant_id, institution_id, session_id, sitting_id, \
         paper_id, created_by) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(x.institution_id)
    .bind(x.session_id)
    .bind(id)
    .bind(x.paper_id)
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("the sitting already has a live conduct record".into())
        }
        other => other,
    })?;
    audit(
        &mut tx,
        &scope,
        "exams.conduct_record.open",
        "conduct_record",
        cid.to_string(),
        json!({"sittingId": id}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, cid, StatusCode::CREATED).await
}

/// One conduct record (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/conduct-records/{id}", operation_id = "exam_conduct_get", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    respond(&st, ctx.scope.tenant_id, id, StatusCode::OK).await
}

async fn begin(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: ConductStatus,
) -> Result<(ConductRow, ExecutionScope), KernelError> {
    let c = load_conduct(&st.pool, ctx.scope.tenant_id, id).await?;
    let scope = institution_scope(ctx, InstitutionId(c.institution_id));
    require_institution_writable(st, &scope, InstitutionId(c.institution_id)).await?;
    check_version(expected, c.version)?;
    let from = ConductStatus::parse(&c.status)
        .ok_or_else(|| KernelError::internal_msg("bad conduct status"))?;
    if !conduct_can(from, to) {
        return Err(illegal("the conduct record", from.as_str(), to.as_str()));
    }
    Ok((c, scope))
}

fn conduct_move<'a>(
    c: &'a ConductRow,
    tenant: TenantId,
    version: i64,
    from: &'a str,
    to: &'a str,
) -> Move<'a> {
    Move {
        table: "exams.conduct_record",
        aggregate: "conduct_record",
        has_process: true,
        tenant,
        institution: InstitutionId(c.institution_id),
        id: c.id,
        version,
        from,
        to,
    }
}

fn conduct_event(c: &ConductRow, from: &str, to: &str, version: i64) -> StatusChangedV1 {
    let mut e = status_event(
        c.id,
        InstitutionId(c.institution_id),
        Some(from),
        to,
        version,
    );
    e.session_id = Some(c.session_id);
    e.paper_id = Some(c.paper_id);
    e
}

/// Start the conduct (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/conduct-records/{id}/start", operation_id = "exam_conduct_start", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conduct record")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_start(
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
    let (c, scope) = begin(&st, ctx, id, expected, ConductStatus::InProgress).await?;
    let session = load_session(&st.pool, tenant, c.session_id).await?;
    require_operational(&session)?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        conduct_move(&c, tenant, c.version, &c.status, "in_progress"),
        &StatusChange {
            cause_event_type: "exams.conduct_record.start",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    sqlx::query(
        "UPDATE exams.conduct_record SET started_at = COALESCE(started_at, now()) WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.conduct_record.start",
        "conduct_record",
        id.to_string(),
        json!({}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Suspend a conduct in progress under an authorised contingency (`exam.setup.manage`, reason
/// required, L3-I-174).
#[utoipa::path(post, path = "/api/v1/exams/conduct-records/{id}/suspend", operation_id = "exam_conduct_suspend", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conduct record")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_suspend(
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
    let tenant = ctx.scope.tenant_id;
    let (c, scope) = begin(&st, ctx, id, expected, ConductStatus::Suspended).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        conduct_move(&c, tenant, c.version, "in_progress", "suspended"),
        &StatusChange {
            cause_event_type: EXAM_CONDUCT_SUSPENDED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let mut e = conduct_event(&c, "in_progress", "suspended", v);
    e.reason.clone_from(&reason);
    emit(
        &mut tx,
        &scope,
        (EXAM_CONDUCT_SUSPENDED, EXAM_CONDUCT_SUSPENDED_SCHEMA),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.conduct_record.suspend",
        "conduct_record",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Resume a suspended conduct once the interruption is resolved (`exam.setup.manage`,
/// L3-I-175).
#[utoipa::path(post, path = "/api/v1/exams/conduct-records/{id}/resume", operation_id = "exam_conduct_resume", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conduct record")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_resume(
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
    let (c, scope) = begin(&st, ctx, id, expected, ConductStatus::InProgress).await?;
    if c.status != "suspended" {
        return Err(KernelError::Conflict("only a suspended conduct can be resumed".into()).into());
    }
    let reason = reason_of(body);
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        conduct_move(&c, tenant, c.version, "suspended", "in_progress"),
        &StatusChange {
            cause_event_type: "exams.conduct_record.resume",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.conduct_record.resume",
        "conduct_record",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Effective candidates of a paper (registered paper status `registered`).
async fn candidates(
    st: &ExamsState,
    scope: &ExecutionScope,
    session: Uuid,
    paper: Uuid,
) -> Result<Vec<CandidateSummary>, KernelError> {
    Ok(st
        .directory
        .registrations_of_session(scope, ExamSessionId(session))
        .await?
        .into_iter()
        .filter(|r| r.status.is_effective())
        .filter(|r| {
            r.papers.iter().any(|p| {
                p.paper_id.as_uuid() == paper && p.status == RegistrationPaperStatus::Registered
            })
        })
        .collect())
}

/// Registrations whose current hall ticket is withheld / withdrawn (entry condition failed).
async fn blocked_entries(
    st: &ExamsState,
    tenant: TenantId,
    session: Uuid,
) -> Result<HashMap<Uuid, String>, KernelError> {
    let rows: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
        "SELECT DISTINCT ON (registration_id) registration_id, status, withheld_reason \
         FROM exams.hall_ticket WHERE tenant_id = $1 AND session_id = $2 \
         ORDER BY registration_id, created_at DESC",
    )
    .bind(tenant.as_uuid())
    .bind(session)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .filter(|(_, s, _)| matches!(s.as_str(), "withheld" | "withdrawn" | "cancelled"))
        .map(|(r, s, why)| {
            (
                r,
                format!(
                    "hall ticket {s}{}",
                    why.map(|w| format!(": {w}")).unwrap_or_default()
                ),
            )
        })
        .collect())
}

fn valid_outcome(o: &str) -> bool {
    matches!(o, "present" | "absent" | "denied_entry")
}

/// Complete the conduct with attendance (`exam.setup.manage`, L3-I-055 / I-043 / I-057).
#[utoipa::path(post, path = "/api/v1/exams/conduct-records/{id}/complete", operation_id = "exam_conduct_complete", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conduct record")),
    request_body = ExamConductComplete, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_complete(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamConductComplete>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let (c, scope) = begin(&st, ctx, id, expected, ConductStatus::Completed).await?;
    let session = load_session(&st.pool, tenant, c.session_id).await?;
    require_operational(&session)?;
    let default = b.default_outcome.clone().unwrap_or_else(|| "absent".into());
    if !valid_outcome(&default) {
        return Err(invalid(
            "defaultOutcome",
            "invalid",
            "present, absent or denied_entry",
        )
        .into());
    }
    let mut listed: HashMap<Uuid, (String, Option<String>)> = HashMap::new();
    for (i, a) in b.attendance.iter().enumerate() {
        if !valid_outcome(&a.outcome) {
            return Err(invalid(
                &format!("attendance[{i}].outcome"),
                "invalid",
                "present, absent or denied_entry",
            )
            .into());
        }
        listed.insert(
            a.registration_id,
            (a.outcome.clone(), clean(a.reason.as_deref())),
        );
    }
    let cands = candidates(&st, &scope, c.session_id, c.paper_id).await?;
    if let Some(unknown) = listed
        .keys()
        .find(|r| !cands.iter().any(|c| c.registration_id.as_uuid() == **r))
    {
        return Err(invalid(
            "attendance",
            "not_a_candidate",
            &format!("registration {unknown} is not a candidate of this sitting"),
        )
        .into());
    }
    let blocked = blocked_entries(&st, tenant, c.session_id).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let (mut present, mut absent, mut denied) = (0, 0, 0);
    for r in &cands {
        let rid = r.registration_id.as_uuid();
        let (mut outcome, mut reason) =
            listed.get(&rid).cloned().unwrap_or((default.clone(), None));
        if let Some(why) = blocked.get(&rid) {
            if outcome == "present" {
                outcome = "denied_entry".into();
                reason = Some(why.clone());
            }
        }
        let Some(rp) = r.papers.iter().find(|p| p.paper_id.as_uuid() == c.paper_id) else {
            continue;
        };
        sqlx::query(
            "INSERT INTO exams.conduct_attendance (tenant_id, institution_id, conduct_id, \
             registration_id, registration_paper_id, student_id, outcome, reason) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(tenant.as_uuid())
        .bind(c.institution_id)
        .bind(id)
        .bind(rid)
        .bind(rp.registration_paper_id.as_uuid())
        .bind(r.student_id.as_uuid())
        .bind(&outcome)
        .bind(reason.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let (to, why) = match outcome.as_str() {
            "present" => {
                present += 1;
                (RegistrationPaperStatus::Appeared, None)
            }
            "denied_entry" => {
                denied += 1;
                (
                    RegistrationPaperStatus::Absent,
                    Some(format!(
                        "entry denied: {}",
                        reason
                            .clone()
                            .unwrap_or_else(|| "entry condition not met".into())
                    )),
                )
            }
            _ => {
                absent += 1;
                (RegistrationPaperStatus::Absent, reason.clone())
            }
        };
        set_paper_status(&mut tx, tenant, r, rp.paper_id, to, why.as_deref()).await?;
    }
    let v = move_status(
        &mut tx,
        conduct_move(&c, tenant, c.version, "in_progress", "completed"),
        &StatusChange {
            cause_event_type: EXAM_CONDUCT_COMPLETED,
            reason: None,
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    sqlx::query(
        "UPDATE exams.conduct_record SET completed_at = now(), present_count = $2, \
         absent_count = $3, denied_count = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(present)
    .bind(absent)
    .bind(denied)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE exams.exam_sitting SET status = 'completed', version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(c.sitting_id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    move_paper(
        &mut tx,
        tenant,
        ExamPaperId(c.paper_id),
        ExamPaperStatus::Conducted,
        Some("conduct completed"),
        &act,
    )
    .await?;
    let mut e = conduct_event(&c, "in_progress", "completed", v);
    e.reason = Some(format!(
        "{present} present, {absent} absent, {denied} denied entry"
    ));
    emit(
        &mut tx,
        &scope,
        (EXAM_CONDUCT_COMPLETED, EXAM_CONDUCT_COMPLETED_SCHEMA),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.conduct_record.complete",
        "conduct_record",
        id.to_string(),
        json!({"present": present, "absent": absent, "deniedEntry": denied}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Applies an authorised postponement: conduct `postponed`, sitting moved to the new slot
/// (`postponed_from` kept) so a new conduct can be opened for it.
async fn apply_postpone(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &ConductRow,
    version: i64,
    plan: &serde_json::Value,
    actor: &Actor,
) -> Result<(), KernelError> {
    let tenant = scope.tenant_id;
    let new_date = plan["newDate"]
        .as_str()
        .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
        .ok_or_else(|| KernelError::internal_msg("bad postponement plan"))?;
    let starts = parse_time(
        "newStartsAt",
        plan["newStartsAt"].as_str().unwrap_or_default(),
    )?;
    let reason = plan["reason"].as_str().map(str::to_owned);
    let from = c.status.clone();
    let v = move_status(
        conn,
        conduct_move(c, tenant, version, &from, "postponed"),
        &StatusChange {
            cause_event_type: EXAM_POSTPONED,
            reason: reason.as_deref(),
            process_instance_id: c.process_instance_id.as_deref(),
        },
        actor,
    )
    .await?;
    sqlx::query("UPDATE exams.conduct_record SET pending_postpone = NULL WHERE id = $1")
        .bind(c.id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE exams.exam_sitting SET postponed_from = sits_on, sits_on = $2, starts_at = $3, \
         status_reason = $4, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(c.sitting_id)
    .bind(new_date)
    .bind(starts)
    .bind(reason.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut e = conduct_event(c, &from, "postponed", v);
    e.reason = Some(format!(
        "rescheduled to {new_date} {}: {}",
        starts.format("%H:%M"),
        reason.unwrap_or_default()
    ));
    e.process_instance_id.clone_from(&c.process_instance_id);
    emit(
        conn,
        scope,
        (EXAM_POSTPONED, EXAM_POSTPONED_SCHEMA),
        &e,
        actor,
    )
    .await
}

/// Postpone and reschedule a sitting under an authorised contingency (`exam.setup.manage`,
/// reason required, `CONTINGENCY_DECISION`, L3-I-185). Auto-approved in Batch 6; with an awaited
/// decision the plan stays pending on the record until `/decision`.
#[utoipa::path(post, path = "/api/v1/exams/conduct-records/{id}/postpone", operation_id = "exam_conduct_postpone", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conduct record")),
    request_body = ExamConductPostpone, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_postpone(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamConductPostpone>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    require_reason(&reason)?;
    let starts = parse_time("newStartsAt", &b.new_starts_at)?;
    let tenant = ctx.scope.tenant_id;
    let (c, scope) = begin(&st, ctx, id, expected, ConductStatus::Postponed).await?;
    if c.pending_postpone.is_some() {
        return Err(
            KernelError::Conflict("a postponement already awaits its decision".into()).into(),
        );
    }
    let x = load_sitting(&st.pool, tenant, c.sitting_id).await?;
    let session = load_session(&st.pool, tenant, c.session_id).await?;
    if session.ends_on.is_some_and(|d| b.new_date > d) || b.new_date < x.sits_on {
        return Err(invalid(
            "newDate",
            "outside_session",
            "the new date must be on or after the original date and within the session",
        )
        .into());
    }
    let plan = json!({"newDate": b.new_date, "newStartsAt": starts.format("%H:%M").to_string(), "reason": reason});
    let outcome = request_approval(
        &st,
        &scope,
        process::CONTINGENCY_DECISION,
        business_key("conduct_record", id, c.version),
        BTreeMap::from([
            ("plan".to_owned(), plan.clone()),
            ("sittingId".to_owned(), json!(c.sitting_id)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let version: i64 = sqlx::query_scalar(
        "UPDATE exams.conduct_record SET pending_postpone = $3, process_instance_id = $4, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $5 RETURNING version",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(&plan)
    .bind(&pid)
    .bind(c.version)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("the conduct record changed concurrently".into()))?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(act.correlation_id.clone());
        let current = ConductRow {
            process_instance_id: Some(pid.clone()),
            ..c.clone()
        };
        apply_postpone(&mut tx, &scope, &current, version, &plan, &sys).await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.conduct_record.postpone",
        "conduct_record",
        id.to_string(),
        json!({"plan": plan, "processInstanceId": pid}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Decide a pending postponement (`exam.approve`): approve applies it, reject clears it.
#[utoipa::path(post, path = "/api/v1/exams/conduct-records/{id}/decision", operation_id = "exam_conduct_decision", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the conduct record")),
    request_body = ExamSetupDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamConductRecordView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn conduct_decision(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSetupDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "exam postponement").await?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let c = load_conduct(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(c.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(c.institution_id)).await?;
    check_version(expected, c.version)?;
    let Some(plan) = c.pending_postpone.clone() else {
        return Err(KernelError::Conflict("no postponement awaits a decision".into()).into());
    };
    let reason = clean(b.reason.as_deref());
    if b.decision == ExamSetupDecisionKind::Reject {
        require_reason(&reason)?;
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    match b.decision {
        ExamSetupDecisionKind::Approve => {
            let from = ConductStatus::parse(&c.status)
                .ok_or_else(|| KernelError::internal_msg("bad conduct status"))?;
            if !conduct_can(from, ConductStatus::Postponed) {
                return Err(illegal("the conduct record", from.as_str(), "postponed").into());
            }
            apply_postpone(&mut tx, &scope, &c, c.version, &plan, &act).await?;
        }
        ExamSetupDecisionKind::Reject => {
            sqlx::query(
                "UPDATE exams.conduct_record SET pending_postpone = NULL, status_reason = $3, \
                 version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
            )
            .bind(id)
            .bind(tenant.as_uuid())
            .bind(reason.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    audit(
        &mut tx,
        &scope,
        "exams.conduct_record.decision",
        "conduct_record",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}
