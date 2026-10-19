//! Hall tickets (L2-I05; L3-I-037 issuance to eligible candidates, I-038 unsatisfied conditions
//! → withheld entry authorisation, I-039 invalidation after withdrawal, I-040 special
//! arrangements, I-041 duplicate / conflicting ticket → one valid ticket, I-042 population
//! finalisation).
//!
//! Entry conditions per the governing policy: outstanding dues through
//! `FinanceClearancePort::student_dues(...).permits_clearance()` (when `requireDuesCleared`) and
//! attendance through `AcademicExamFacts::attendance_standing` (`below_threshold`, or below the
//! policy's `minAttendancePercent`) on every registered paper's offering. A failed condition
//! makes the ticket `withheld` with the evidence; `release` re-evaluates. One current ticket per
//! registration (unique index); a reissue replaces it with a new number.

use super::base::{load_registration, ExamHallTicketRef};
use crate::setup::common::{
    check_version, emit, illegal, move_status, reason_of, require_reason, status_event,
    ExamSetupBulkFailure, ExamSetupBulkSummary, Move, ReasonBody,
};
use crate::setup::domain::{hall_ticket_can, hall_ticket_is_current};
use crate::setup::policy::{rules_for, PolicyRules};
use crate::setup::session::{load_session, record_revision, RevisionRow};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution_writable, require_operational};
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::me::my_registrations;
use crate::shared::numbering::{self, kind};
use crate::shared::publish::audit;
use crate::shared::rows::lock_session;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ExamsState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::exams::events::setup::{
    HALL_TICKET_ISSUED, HALL_TICKET_ISSUED_SCHEMA, HALL_TICKET_WITHDRAWN,
    HALL_TICKET_WITHDRAWN_SCHEMA, HALL_TICKET_WITHHELD, HALL_TICKET_WITHHELD_SCHEMA,
};
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_contracts::exams::ids::ExamSessionId;
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_contracts::exams::status::{HallTicketStatus, RegistrationPaperStatus};
use sutra_contracts::finance::StudentFinanceQuery;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// A hall ticket.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamHallTicketView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub registration_id: Uuid,
    pub student_id: Uuid,
    #[schema(example = "HT-000001")]
    pub number: String,
    /// `HallTicketStatus` wire name.
    pub status: String,
    /// Evidence of the entry-condition evaluation.
    #[schema(value_type = Object)]
    pub conditions: serde_json::Value,
    pub withheld_reason: Option<String>,
    pub special_arrangements: Option<String>,
    pub replaces_id: Option<Uuid>,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub issued_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamHallTicketList {
    pub items: Vec<ExamHallTicketView>,
}

/// A hall ticket with the registration it admits to (self-service).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamMyHallTicket {
    pub registration_number: String,
    pub display_name: String,
    pub student_number: String,
    pub ticket: ExamHallTicketView,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamMyHallTicketList {
    pub items: Vec<ExamMyHallTicket>,
}

const COLUMNS: &str = "id, institution_id, session_id, registration_id, student_id, number, \
    status, conditions, withheld_reason, special_arrangements, replaces_id, status_reason, \
    process_instance_id, issued_at, version";

async fn load<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ExamHallTicketView, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ExamHallTicketView>(&format!(
        "SELECT {COLUMNS} FROM exams.hall_ticket WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "hall ticket",
    })
}

async fn current_of<'c, E>(
    executor: E,
    tenant: TenantId,
    registration: Uuid,
) -> Result<Option<ExamHallTicketView>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ExamHallTicketView>(&format!(
        "SELECT {COLUMNS} FROM exams.hall_ticket WHERE tenant_id = $1 AND registration_id = $2 \
         AND status IN ('draft', 'under_review', 'approved', 'published', 'active', 'withheld') FOR UPDATE"
    ))
    .bind(tenant.as_uuid())
    .bind(registration)
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)
}

/// Result of the entry-condition evaluation.
pub struct Evaluation {
    pub satisfied: bool,
    pub reasons: Vec<String>,
    pub evidence: serde_json::Value,
}

/// Evaluates the policy's entry conditions for one registration.
pub async fn evaluate(
    st: &ExamsState,
    scope: &ExecutionScope,
    r: &CandidateSummary,
    rules: &PolicyRules,
    offerings: &BTreeMap<Uuid, (String, Option<Uuid>)>,
) -> Result<Evaluation, KernelError> {
    let mut reasons = Vec::new();
    let mut evidence = serde_json::Map::new();
    if rules.require_dues_cleared {
        let dues = st
            .finance
            .student_dues(
                scope,
                StudentFinanceQuery {
                    institution_id: r.institution_id,
                    student_id: r.student_id.as_uuid(),
                },
            )
            .await?;
        if !dues.permits_clearance() {
            let detail = match &dues {
                sutra_contracts::finance::FinanceEvidence::Outstanding { detail }
                | sutra_contracts::finance::FinanceEvidence::Pending { detail } => detail.clone(),
                _ => "dues not cleared".into(),
            };
            reasons.push(format!("outstanding dues: {detail}"));
        }
        evidence.insert(
            "dues".into(),
            serde_json::to_value(&dues).unwrap_or_default(),
        );
    }
    let mut attendance = Vec::new();
    for p in r
        .papers
        .iter()
        .filter(|p| p.status == RegistrationPaperStatus::Registered)
    {
        let Some((code, Some(offering))) = offerings.get(&p.paper_id.as_uuid()).cloned() else {
            continue;
        };
        let Some(a) = st
            .academic
            .attendance_standing(scope, CourseOfferingId(offering), r.student_id)
            .await?
        else {
            continue;
        };
        let below_policy = rules.min_attendance_percent.is_some_and(|m| a.percent < m);
        if a.below_threshold || below_policy {
            reasons.push(format!(
                "attendance {}% in {code} is below the required minimum",
                crate::shared::grading::fmt2(a.percent)
            ));
        }
        attendance.push(json!({"paperCode": code, "percent": crate::shared::grading::fmt2(a.percent), "attended": a.attended, "total": a.total, "belowThreshold": a.below_threshold || below_policy}));
    }
    evidence.insert("attendance".into(), json!(attendance));
    Ok(Evaluation {
        satisfied: reasons.is_empty(),
        reasons,
        evidence: serde_json::Value::Object(evidence),
    })
}

async fn arrangements_text(
    conn: &mut PgConnection,
    tenant: TenantId,
    registration: Uuid,
) -> Result<Option<String>, KernelError> {
    let rows: Vec<(String, Option<i32>, String)> = sqlx::query_as(
        "SELECT kind, extra_minutes, note FROM exams.registration_arrangement WHERE tenant_id = $1 \
         AND registration_id = $2 AND status = 'approved' ORDER BY created_at",
    )
    .bind(tenant.as_uuid())
    .bind(registration)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if rows.is_empty() {
        return Ok(None);
    }
    Ok(Some(
        rows.iter()
            .map(|(k, m, n)| match m {
                Some(m) => format!("{k} (+{m} min): {n}"),
                None => format!("{k}: {n}"),
            })
            .collect::<Vec<_>>()
            .join("; "),
    ))
}

fn ticket_event(
    t: &ExamHallTicketView,
    r: &CandidateSummary,
    from: Option<&str>,
    to: &str,
    version: i64,
    reason: String,
) -> StatusChangedV1 {
    let mut e = status_event(t.id, InstitutionId(t.institution_id), from, to, version);
    e.session_id = Some(t.session_id);
    e.registration_id = Some(t.registration_id);
    e.student_id = Some(t.student_id);
    e.student_subject_ref.clone_from(&r.subject_ref);
    e.reason = Some(reason);
    e
}

/// Issues a new ticket for `r` (active, withheld, or under review when the policy requires
/// approval and it is awaited) in the caller's transaction; returns its id and status.
#[allow(clippy::too_many_arguments)]
async fn issue_one(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: &CandidateSummary,
    ev: &Evaluation,
    approval: Option<&ApprovalOutcome>,
    replaces: Option<Uuid>,
    actor: &Actor,
) -> Result<(Uuid, String), KernelError> {
    let number = numbering::next(conn, tenant, r.institution_id, kind::HALL_TICKET).await?;
    let status = if !ev.satisfied {
        "withheld"
    } else if matches!(approval, Some(ApprovalOutcome::Pending { .. })) {
        "under_review"
    } else {
        "active"
    };
    let withheld = (!ev.satisfied).then(|| ev.reasons.join("; "));
    let arrangements = arrangements_text(conn, tenant, r.registration_id.as_uuid()).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.hall_ticket (tenant_id, institution_id, session_id, registration_id, \
         student_id, number, status, conditions, withheld_reason, special_arrangements, replaces_id, \
         process_instance_id, issued_at, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, CASE WHEN $7 = 'active' THEN now() END, $13) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(r.institution_id.as_uuid())
    .bind(r.session_id.as_uuid())
    .bind(r.registration_id.as_uuid())
    .bind(r.student_id.as_uuid())
    .bind(&number)
    .bind(status)
    .bind(&ev.evidence)
    .bind(withheld.as_deref())
    .bind(arrangements.as_deref())
    .bind(replaces)
    .bind(approval.map(|a| a.process_instance_id().to_owned()))
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!("registration {} already holds a current hall ticket", r.registration_number)),
        other => other,
    })?;
    record_history(
        conn,
        HistoryEntry {
            tenant,
            institution: r.institution_id,
            aggregate_type: "hall_ticket",
            aggregate_id: id,
            from: None,
            to: status,
        },
        &StatusChange {
            cause_event_type: if status == "withheld" {
                HALL_TICKET_WITHHELD
            } else {
                HALL_TICKET_ISSUED
            },
            reason: withheld.as_deref(),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let t = load(&mut *conn, tenant, id).await?;
    match status {
        "active" => {
            let e = ticket_event(&t, r, None, "active", t.version, number.clone());
            emit(
                conn,
                scope,
                (HALL_TICKET_ISSUED, HALL_TICKET_ISSUED_SCHEMA),
                &e,
                actor,
            )
            .await?;
        }
        "withheld" => {
            let e = ticket_event(
                &t,
                r,
                None,
                "withheld",
                t.version,
                withheld.clone().unwrap_or_default(),
            );
            emit(
                conn,
                scope,
                (HALL_TICKET_WITHHELD, HALL_TICKET_WITHHELD_SCHEMA),
                &e,
                actor,
            )
            .await?;
        }
        _ => {}
    }
    Ok((id, status.to_owned()))
}

/// Offering + code of every paper of the session.
async fn paper_offerings(
    st: &ExamsState,
    tenant: TenantId,
    session: Uuid,
) -> Result<BTreeMap<Uuid, (String, Option<Uuid>)>, KernelError> {
    Ok(crate::setup::session::papers_of(&st.pool, tenant, session)
        .await?
        .into_iter()
        .map(|p| (p.id.as_uuid(), (p.paper_code, p.offering_id)))
        .collect())
}

async fn finalised(st: &ExamsState, tenant: TenantId, session: Uuid) -> Result<bool, KernelError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM exams.exam_session_revision WHERE tenant_id = $1 AND session_id = $2 AND kind = 'hall_tickets_finalised'",
    )
    .bind(tenant.as_uuid())
    .bind(session)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(n > 0)
}

/// Hall tickets of a session (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/hall-tickets", operation_id = "exam_hall_ticket_list", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamHallTicketList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamHallTicketList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_session(&st.pool, tenant, id).await?;
    let items = sqlx::query_as::<_, ExamHallTicketView>(&format!(
        "SELECT {COLUMNS} FROM exams.hall_ticket WHERE tenant_id = $1 AND session_id = $2 ORDER BY number"
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(ExamHallTicketList { items }))
}

/// Issue hall tickets to every effective registration without a current ticket
/// (`exam.registration.manage`, `Idempotency-Key`, L3-I-037 / I-038). Candidates failing an
/// entry condition get a `withheld` ticket with the evidence.
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/hall-tickets/issue", operation_id = "exam_hall_ticket_issue", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSetupBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_issue(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Json<ExamSetupBulkSummary>> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let s = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    require_operational(&s)?;
    if finalised(&st, tenant, id).await? {
        return Err(KernelError::Conflict(
            "the hall-ticket population of this session is finalised".into(),
        )
        .into());
    }
    let rules = rules_for(&st, tenant, s.institution_id, s.policy_id).await?;
    let offerings = paper_offerings(&st, tenant, id).await?;
    let regs = st.directory.registrations_of_session(&scope, s.id).await?;
    let act = actor(ctx);
    let mut summary = ExamSetupBulkSummary::default();
    for r in regs.into_iter().filter(|r| r.status.is_effective()) {
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        if current_of(&mut *tx, tenant, r.registration_id.as_uuid())
            .await?
            .is_some()
        {
            summary.skipped += 1;
            continue;
        }
        let ev = match evaluate(&st, &scope, &r, &rules, &offerings).await {
            Ok(ev) => ev,
            Err(e) => {
                summary.failed.push(ExamSetupBulkFailure {
                    id: Some(r.registration_id.as_uuid()),
                    reason: e.to_string(),
                });
                continue;
            }
        };
        let approval = if rules.hall_ticket_approval && ev.satisfied {
            Some(
                request_approval(
                    &st,
                    &scope,
                    process::HALL_TICKET_APPROVAL,
                    business_key("hall_ticket", r.registration_id, r.version),
                    BTreeMap::from([(
                        "registrationNumber".to_owned(),
                        json!(r.registration_number),
                    )]),
                )
                .await?,
            )
        } else {
            None
        };
        match issue_one(
            &mut tx,
            &scope,
            tenant,
            &r,
            &ev,
            approval.as_ref(),
            None,
            &act,
        )
        .await
        {
            Ok(_) => {
                tx.commit().await.map_err(map_db_error)?;
                summary.processed += 1;
            }
            Err(e) => summary.failed.push(ExamSetupBulkFailure {
                id: Some(r.registration_id.as_uuid()),
                reason: e.to_string(),
            }),
        }
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    audit(&mut tx, &scope, "exams.hall_ticket.issue", "exam_session", id.to_string(), json!({"processed": summary.processed, "skipped": summary.skipped, "failed": summary.failed.len()}), &act).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(summary))
}

/// One hall ticket (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/hall-tickets/{id}", operation_id = "exam_hall_ticket_get", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamHallTicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let t = load(&st.pool, ctx.scope.tenant_id, id).await?;
    Ok(with_etag(StatusCode::OK, &t, Version(t.version)))
}

struct Ctx {
    t: ExamHallTicketView,
    r: CandidateSummary,
    scope: ExecutionScope,
    rules: PolicyRules,
}

async fn begin(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: HallTicketStatus,
) -> Result<Ctx, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let t = load(&st.pool, tenant, id).await?;
    let s = load_session(&st.pool, tenant, t.session_id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(st, &scope, s.institution_id).await?;
    check_version(expected, t.version)?;
    let from = HallTicketStatus::parse(&t.status)
        .ok_or_else(|| KernelError::internal_msg("bad hall ticket status"))?;
    if !hall_ticket_can(from, to) {
        return Err(illegal(
            &format!("hall ticket {}", t.number),
            from.as_str(),
            to.as_str(),
        ));
    }
    let r = load_registration(&st.pool, tenant, t.registration_id).await?;
    let rules = rules_for(st, tenant, s.institution_id, s.policy_id).await?;
    Ok(Ctx { t, r, scope, rules })
}

fn ticket_move<'a>(
    t: &'a ExamHallTicketView,
    tenant: TenantId,
    version: i64,
    to: &'a str,
) -> Move<'a> {
    Move {
        table: "exams.hall_ticket",
        aggregate: "hall_ticket",
        has_process: true,
        tenant,
        institution: InstitutionId(t.institution_id),
        id: t.id,
        version,
        from: &t.status,
        to,
    }
}

async fn respond(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let t = load(&st.pool, tenant, id).await?;
    Ok(with_etag(status, &t, Version(t.version)))
}

/// Withhold an active ticket (`exam.registration.manage`, reason required).
#[utoipa::path(post, path = "/api/v1/exams/hall-tickets/{id}/withhold", operation_id = "exam_hall_ticket_withhold", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the hall ticket")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamHallTicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_withhold(
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
    let c = begin(&st, ctx, id, expected, HallTicketStatus::Withheld).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        ticket_move(&c.t, tenant, c.t.version, "withheld"),
        &StatusChange {
            cause_event_type: HALL_TICKET_WITHHELD,
            reason: Some(&why),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    sqlx::query("UPDATE exams.hall_ticket SET withheld_reason = $2 WHERE id = $1")
        .bind(id)
        .bind(&why)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let e = ticket_event(&c.t, &c.r, Some(&c.t.status), "withheld", v, why.clone());
    emit(
        &mut tx,
        &c.scope,
        (HALL_TICKET_WITHHELD, HALL_TICKET_WITHHELD_SCHEMA),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope,
        "exams.hall_ticket.withhold",
        "hall_ticket",
        id.to_string(),
        json!({"reason": why}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Release a withheld ticket once its conditions are satisfied (`exam.registration.manage`,
/// L3-I-038): the conditions are re-evaluated; still unsatisfied → `409` with the evidence.
/// `override: true` in the reason body is not supported — clear the condition at its source.
#[utoipa::path(post, path = "/api/v1/exams/hall-tickets/{id}/release", operation_id = "exam_hall_ticket_release", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the hall ticket")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamHallTicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_release(
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
    let c = begin(&st, ctx, id, expected, HallTicketStatus::Active).await?;
    if c.t.status == "withheld" {
        let offerings = paper_offerings(&st, tenant, c.t.session_id).await?;
        let ev = evaluate(&st, &c.scope, &c.r, &c.rules, &offerings).await?;
        if !ev.satisfied {
            return Err(KernelError::Conflict(format!(
                "the entry conditions are still unsatisfied: {}",
                ev.reasons.join("; ")
            ))
            .into());
        }
        let act = actor(ctx);
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let v = move_status(
            &mut tx,
            ticket_move(&c.t, tenant, c.t.version, "active"),
            &StatusChange {
                cause_event_type: HALL_TICKET_ISSUED,
                reason: reason.as_deref(),
                process_instance_id: None,
            },
            &act,
        )
        .await?;
        sqlx::query("UPDATE exams.hall_ticket SET conditions = $2, withheld_reason = NULL, issued_at = now() WHERE id = $1")
            .bind(id).bind(&ev.evidence).execute(&mut *tx).await.map_err(map_db_error)?;
        let e = ticket_event(
            &c.t,
            &c.r,
            Some("withheld"),
            "active",
            v,
            c.t.number.clone(),
        );
        emit(
            &mut tx,
            &c.scope,
            (HALL_TICKET_ISSUED, HALL_TICKET_ISSUED_SCHEMA),
            &e,
            &act,
        )
        .await?;
        audit(
            &mut tx,
            &c.scope,
            "exams.hall_ticket.release",
            "hall_ticket",
            id.to_string(),
            json!({"evidence": ev.evidence}),
            &act,
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
    } else {
        // under_review / approved → active (approval decided).
        let act = actor(ctx);
        let mut tx = st.pool.begin().await.map_err(map_db_error)?;
        let v = move_status(
            &mut tx,
            ticket_move(&c.t, tenant, c.t.version, "active"),
            &StatusChange {
                cause_event_type: HALL_TICKET_ISSUED,
                reason: reason.as_deref(),
                process_instance_id: None,
            },
            &act,
        )
        .await?;
        sqlx::query("UPDATE exams.hall_ticket SET issued_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        let e = ticket_event(
            &c.t,
            &c.r,
            Some(&c.t.status),
            "active",
            v,
            c.t.number.clone(),
        );
        emit(
            &mut tx,
            &c.scope,
            (HALL_TICKET_ISSUED, HALL_TICKET_ISSUED_SCHEMA),
            &e,
            &act,
        )
        .await?;
        audit(
            &mut tx,
            &c.scope,
            "exams.hall_ticket.release",
            "hall_ticket",
            id.to_string(),
            json!({}),
            &act,
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
    }
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Invalidates the registration's current ticket (registration withdrawn, L3-I-039) inside the
/// caller's transaction; nothing when it has none.
pub async fn invalidate_ticket(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: &CandidateSummary,
    why: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let Some(t) = current_of(&mut *conn, tenant, r.registration_id.as_uuid()).await? else {
        return Ok(());
    };
    let from = HallTicketStatus::parse(&t.status)
        .ok_or_else(|| KernelError::internal_msg("bad hall ticket status"))?;
    let to = if hall_ticket_can(from, HallTicketStatus::Withdrawn) {
        "withdrawn"
    } else {
        "cancelled"
    };
    let v = move_status(
        conn,
        ticket_move(&t, tenant, t.version, to),
        &StatusChange {
            cause_event_type: HALL_TICKET_WITHDRAWN,
            reason: Some(why),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let e = ticket_event(&t, r, Some(&t.status), to, v, why.to_owned());
    emit(
        conn,
        scope,
        (HALL_TICKET_WITHDRAWN, HALL_TICKET_WITHDRAWN_SCHEMA),
        &e,
        actor,
    )
    .await?;
    Ok(())
}

/// Withdraw (invalidate) a ticket (`exam.registration.manage`, reason required, L3-I-039).
#[utoipa::path(post, path = "/api/v1/exams/hall-tickets/{id}/withdraw", operation_id = "exam_hall_ticket_withdraw", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the hall ticket")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamHallTicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_withdraw(
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
    let c = begin(&st, ctx, id, expected, HallTicketStatus::Withdrawn).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        ticket_move(&c.t, tenant, c.t.version, "withdrawn"),
        &StatusChange {
            cause_event_type: HALL_TICKET_WITHDRAWN,
            reason: Some(&why),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let e = ticket_event(&c.t, &c.r, Some(&c.t.status), "withdrawn", v, why.clone());
    emit(
        &mut tx,
        &c.scope,
        (HALL_TICKET_WITHDRAWN, HALL_TICKET_WITHDRAWN_SCHEMA),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &c.scope,
        "exams.hall_ticket.withdraw",
        "hall_ticket",
        id.to_string(),
        json!({"reason": why}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Reissue a ticket (`exam.registration.manage`, reason required; duplicate / conflicting /
/// lost ticket, L3-I-041, or after a new special arrangement, I-040): the current one becomes
/// `reissued` and a new number replaces it, so exactly one valid ticket remains.
#[utoipa::path(post, path = "/api/v1/exams/hall-tickets/{id}/reissue", operation_id = "exam_hall_ticket_reissue", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the hall ticket")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamHallTicketView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_reissue(
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
    let c = begin(&st, ctx, id, expected, HallTicketStatus::Reissued).await?;
    let offerings = paper_offerings(&st, tenant, c.t.session_id).await?;
    let ev = evaluate(&st, &c.scope, &c.r, &c.rules, &offerings).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        ticket_move(&c.t, tenant, c.t.version, "reissued"),
        &StatusChange {
            cause_event_type: "exams.hall_ticket.reissue",
            reason: Some(&why),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let (new_id, _) = issue_one(&mut tx, &c.scope, tenant, &c.r, &ev, None, Some(id), &act).await?;
    audit(
        &mut tx,
        &c.scope,
        "exams.hall_ticket.reissue",
        "hall_ticket",
        id.to_string(),
        json!({"reason": why, "replacement": new_id}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, new_id, StatusCode::OK).await
}

/// Finalise the hall-ticket population for conduct (`exam.registration.manage`,
/// `Idempotency-Key`, L3-I-042): reports effective registrations without an active ticket and
/// freezes bulk issuance (release / reissue stay possible).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/hall-tickets/finalise", operation_id = "exam_hall_ticket_finalise", tag = "exam-registration",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSetupBulkSummary), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn hall_ticket_finalise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Json<ExamSetupBulkSummary>> {
    let ctx = auth.require(Permission::ExamRegistrationManage)?;
    require_idempotency_key(&headers)?;
    let reason = reason_of(body).unwrap_or_else(|| "hall-ticket population finalised".into());
    let tenant = ctx.scope.tenant_id;
    let s = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, s.institution_id);
    require_institution_writable(&st, &scope, s.institution_id).await?;
    require_operational(&s)?;
    if finalised(&st, tenant, id).await? {
        return Err(KernelError::Conflict(
            "the hall-ticket population is already finalised".into(),
        )
        .into());
    }
    let regs = st.directory.registrations_of_session(&scope, s.id).await?;
    let mut summary = ExamSetupBulkSummary::default();
    for r in regs.iter().filter(|r| r.status.is_effective()) {
        let status: Option<String> = sqlx::query_scalar(
            "SELECT status FROM exams.hall_ticket WHERE tenant_id = $1 AND registration_id = $2 \
             AND status IN ('draft', 'under_review', 'approved', 'published', 'active', 'withheld')",
        )
        .bind(tenant.as_uuid())
        .bind(r.registration_id.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
        match status.as_deref() {
            Some("active") => summary.processed += 1,
            Some(other) => summary.failed.push(ExamSetupBulkFailure {
                id: Some(r.registration_id.as_uuid()),
                reason: format!("{}: hall ticket {other}", r.registration_number),
            }),
            None => summary.failed.push(ExamSetupBulkFailure {
                id: Some(r.registration_id.as_uuid()),
                reason: format!("{}: no hall ticket", r.registration_number),
            }),
        }
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_session(&mut tx, tenant, ExamSessionId(id)).await?;
    record_revision(
        &mut tx,
        tenant,
        &locked,
        RevisionRow {
            kind: "hall_tickets_finalised",
            changed: vec!["hallTickets".into()],
            before: json!({}),
            after: json!({"active": summary.processed, "notAdmitted": summary.failed.len()}),
            reason: &reason,
            merged_into: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.hall_ticket.finalise",
        "exam_session",
        id.to_string(),
        json!({"active": summary.processed}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(summary))
}

/// The caller's own hall tickets (`exam.self`): current tickets of their registrations.
#[utoipa::path(get, path = "/api/v1/exams/me/hall-tickets", operation_id = "exam_hall_ticket_self_list", tag = "exam-registration",
    security(("session_cookie" = [])),
    responses((status = 200, body = ExamMyHallTicketList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_hall_tickets(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<ExamMyHallTicketList>> {
    let ctx = &auth.0;
    let regs = my_registrations(&st, ctx).await?;
    let tenant = ctx.scope.tenant_id;
    let mut items = Vec::new();
    for r in regs {
        let t = sqlx::query_as::<_, ExamHallTicketView>(&format!(
            "SELECT {COLUMNS} FROM exams.hall_ticket WHERE tenant_id = $1 AND registration_id = $2 \
             ORDER BY created_at DESC LIMIT 1"
        ))
        .bind(tenant.as_uuid())
        .bind(r.registration_id.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?;
        if let Some(t) = t.filter(|t| {
            HallTicketStatus::parse(&t.status)
                .is_some_and(|s| hall_ticket_is_current(s) || s == HallTicketStatus::Withdrawn)
        }) {
            items.push(ExamMyHallTicket {
                registration_number: r.registration_number,
                display_name: r.display_name,
                student_number: r.student_number,
                ticket: t,
            });
        }
    }
    Ok(Json(ExamMyHallTicketList { items }))
}

/// Ticket ref helper (unused fields kept for the view).
pub fn as_ref(t: &ExamHallTicketView) -> ExamHallTicketRef {
    ExamHallTicketRef {
        id: t.id,
        number: t.number.clone(),
        status: t.status.clone(),
    }
}
