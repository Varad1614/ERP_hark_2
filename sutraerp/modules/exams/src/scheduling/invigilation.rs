//! Invigilation and examiner duties (L3-I-010 schedule → assigned invigilation, I-011
//! invigilator shortage → maintained schedule, I-012 practical / viva examiners, I-060
//! mid-conduct replacement → governed continuity).
//!
//! Invigilators / examiners are login subjects (no People staff record until Batch 7). The
//! staffing view reports the required count (one per `invigilatorRatio` candidates of the
//! governing policy) against live duties, so a shortage is visible before conduct. A
//! replacement keeps the replaced duty (`replaced`, history) and links the new one to it.

use super::schedule::{load_sitting, SittingRow};
use crate::setup::common::{
    check_version, clean, emit, illegal, invalid, move_status, reason_of, require_reason,
    status_event, Move, ReasonBody,
};
use crate::setup::domain::{duty_can, duty_is_live, invigilators_required, overlaps};
use crate::setup::policy::rules_for;
use crate::setup::session::load_session;
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::publish::audit;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ExamsState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{NaiveDate, NaiveTime};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use sutra_contracts::exams::events::setup::{INVIGILATION_ASSIGNED, INVIGILATION_ASSIGNED_SCHEMA};
use sutra_contracts::exams::status::InvigilationDutyStatus;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// One invigilation / examiner duty.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamInvigilationDutyView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub sitting_id: Uuid,
    pub venue_id: Option<Uuid>,
    pub subject_ref: String,
    pub name: Option<String>,
    /// `invigilator | chief_invigilator | practical_examiner | external_examiner`.
    pub role: String,
    pub replaces_id: Option<Uuid>,
    /// `assigned | confirmed | replaced | released | completed`.
    pub status: String,
    pub status_reason: Option<String>,
    pub version: i64,
}

/// Staffing of a sitting (L3-I-011).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamInvigilationStaffing {
    pub sitting_id: Uuid,
    pub candidates: i64,
    /// Candidates per invigilator under the governing policy.
    pub ratio: i32,
    pub required: i64,
    /// Live invigilator duties (examiners are counted separately).
    pub assigned: i64,
    pub examiners: i64,
    pub shortage: i64,
    pub duties: Vec<ExamInvigilationDutyView>,
}

/// Assign a duty.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamInvigilationAssign {
    /// Login subject of the invigilator / examiner (e.g. `dev|meera.shah@sutra.dev`).
    pub subject_ref: String,
    pub name: Option<String>,
    /// Default `invigilator`.
    pub role: Option<String>,
    pub venue_id: Option<Uuid>,
}

/// Replace a duty (mid-conduct allowed, reason required).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamInvigilationReplace {
    pub subject_ref: String,
    pub name: Option<String>,
    pub reason: Option<String>,
}

/// A duty of the caller with its sitting (`/me/invigilation`).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamMyDutyView {
    pub id: Uuid,
    pub sitting_id: Uuid,
    pub session_id: Uuid,
    pub paper_code: String,
    pub title: String,
    pub role: String,
    pub status: String,
    pub sits_on: NaiveDate,
    #[schema(value_type = String, example = "09:30:00")]
    pub starts_at: NaiveTime,
    pub duration_minutes: i32,
    pub venue_code: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamMyDutyList {
    pub items: Vec<ExamMyDutyView>,
}

const DUTY_COLUMNS: &str = "id, institution_id, sitting_id, venue_id, subject_ref, name, role, \
    replaces_id, status, status_reason, version";

const ROLES: &[&str] = &[
    "invigilator",
    "chief_invigilator",
    "practical_examiner",
    "external_examiner",
];

/// `true` for the examiner roles of practical / viva assessments (L3-I-012 / I-043).
pub fn is_examiner(role: &str) -> bool {
    matches!(role, "practical_examiner" | "external_examiner")
}

async fn load_duty<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ExamInvigilationDutyView, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ExamInvigilationDutyView>(&format!(
        "SELECT {DUTY_COLUMNS} FROM exams.invigilation_duty WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "invigilation duty",
    })
}

/// Duties of a sitting (all statuses).
pub async fn duties_of<'c, E>(
    executor: E,
    tenant: TenantId,
    sitting: Uuid,
) -> Result<Vec<ExamInvigilationDutyView>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ExamInvigilationDutyView>(&format!(
        "SELECT {DUTY_COLUMNS} FROM exams.invigilation_duty WHERE tenant_id = $1 AND sitting_id = $2 \
         ORDER BY created_at"
    ))
    .bind(tenant.as_uuid())
    .bind(sitting)
    .fetch_all(executor)
    .await
    .map_err(map_db_error)
}

/// Effective candidates of a sitting's paper.
pub async fn sitting_candidates(
    st: &ExamsState,
    tenant: TenantId,
    paper: Uuid,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM exams.exam_registration_paper rp \
         JOIN exams.exam_registration r ON r.id = rp.registration_id \
         WHERE rp.tenant_id = $1 AND rp.paper_id = $2 AND rp.status IN ('registered', 'appeared') \
         AND r.status IN ('approved', 'confirmed', 'locked', 'amended')",
    )
    .bind(tenant.as_uuid())
    .bind(paper)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)
}

async fn staffing(
    st: &ExamsState,
    tenant: TenantId,
    x: &SittingRow,
) -> Result<ExamInvigilationStaffing, KernelError> {
    let session = load_session(&st.pool, tenant, x.session_id).await?;
    let rules = rules_for(st, tenant, session.institution_id, session.policy_id).await?;
    let candidates = sitting_candidates(st, tenant, x.paper_id).await?;
    let duties = duties_of(&st.pool, tenant, x.id).await?;
    let live = |d: &&ExamInvigilationDutyView| {
        InvigilationDutyStatus::parse(&d.status).is_some_and(duty_is_live)
    };
    let assigned = duties
        .iter()
        .filter(live)
        .filter(|d| !is_examiner(&d.role))
        .count() as i64;
    let examiners = duties
        .iter()
        .filter(live)
        .filter(|d| is_examiner(&d.role))
        .count() as i64;
    let required = invigilators_required(candidates, i64::from(rules.invigilator_ratio));
    Ok(ExamInvigilationStaffing {
        sitting_id: x.id,
        candidates,
        ratio: rules.invigilator_ratio,
        required,
        assigned,
        examiners,
        shortage: (required - assigned).max(0),
        duties,
    })
}

/// Staffing of a sitting: duties, required invigilators and shortage (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sittings/{id}/invigilation", operation_id = "exam_invigilation_list", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamInvigilationStaffing), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn invigilation_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamInvigilationStaffing>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    let x = load_sitting(&st.pool, tenant, id).await?;
    Ok(Json(staffing(&st, tenant, &x).await?))
}

/// 409 when the subject already holds a live duty on an overlapping sitting.
async fn check_clash(
    conn: &mut PgConnection,
    tenant: TenantId,
    x: &SittingRow,
    subject: &str,
) -> Result<(), KernelError> {
    let rows: Vec<(Uuid, String, NaiveDate, NaiveTime, i32)> = sqlx::query_as(
        "SELECT s.id, p.paper_code, s.sits_on, s.starts_at, s.duration_minutes \
         FROM exams.invigilation_duty d JOIN exams.exam_sitting s ON s.id = d.sitting_id \
         JOIN exams.exam_paper p ON p.id = s.paper_id \
         JOIN exams.exam_schedule sc ON sc.id = s.schedule_id \
         WHERE d.tenant_id = $1 AND d.subject_ref = $2 AND d.status IN ('assigned', 'confirmed') \
         AND s.status = 'planned' AND s.sits_on = $3 \
         AND sc.status IN ('draft', 'under_review', 'approved', 'published', 'active')",
    )
    .bind(tenant.as_uuid())
    .bind(subject)
    .bind(x.sits_on)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (sid, code, d, t, m) in rows {
        if sid == x.id {
            return Err(KernelError::Conflict(format!(
                "{subject} already holds a duty on this sitting"
            )));
        }
        if overlaps((x.sits_on, x.starts_at, x.duration_minutes), (d, t, m)) {
            return Err(KernelError::Conflict(format!(
                "{subject} already invigilates {code} at an overlapping time"
            )));
        }
    }
    Ok(())
}

struct NewDuty<'a> {
    subject: &'a str,
    name: Option<String>,
    role: &'a str,
    venue: Option<Uuid>,
    replaces: Option<Uuid>,
}

async fn insert_duty(
    conn: &mut PgConnection,
    tenant: TenantId,
    x: &SittingRow,
    d: NewDuty<'_>,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    sqlx::query_scalar(
        "INSERT INTO exams.invigilation_duty (tenant_id, institution_id, sitting_id, venue_id, \
         subject_ref, name, role, replaces_id, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(x.institution_id)
    .bind(x.id)
    .bind(d.venue)
    .bind(d.subject)
    .bind(d.name.as_deref())
    .bind(d.role)
    .bind(d.replaces)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn emit_assigned(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    x: &SittingRow,
    duty: Uuid,
    subject: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let mut e = status_event(duty, InstitutionId(x.institution_id), None, "assigned", 1);
    e.session_id = Some(x.session_id);
    e.paper_id = Some(x.paper_id);
    e.reason = Some(format!("{} on {} — {subject}", x.paper_code, x.sits_on));
    emit(
        conn,
        scope,
        (INVIGILATION_ASSIGNED, INVIGILATION_ASSIGNED_SCHEMA),
        &e,
        actor,
    )
    .await
}

/// Assign an invigilator / examiner to a sitting (`exam.setup.manage`): one live duty per person
/// and sitting, no overlapping duties for the same person (`409`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sittings/{id}/invigilation", operation_id = "exam_invigilation_assign", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamInvigilationAssign, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamInvigilationDutyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn invigilation_assign(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamInvigilationAssign>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let x = load_sitting(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(x.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(x.institution_id)).await?;
    if x.status != "planned" {
        return Err(KernelError::Conflict(format!("the sitting is {}", x.status)).into());
    }
    let subject = clean(Some(&b.subject_ref)).ok_or_else(|| {
        invalid(
            "subjectRef",
            "required",
            "the invigilator's login subject is required",
        )
    })?;
    let role = b.role.clone().unwrap_or_else(|| "invigilator".into());
    if !ROLES.contains(&role.as_str()) {
        return Err(invalid(
            "role",
            "invalid",
            "invigilator, chief_invigilator, practical_examiner or external_examiner",
        )
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    check_clash(&mut tx, tenant, &x, &subject).await?;
    let duty = insert_duty(
        &mut tx,
        tenant,
        &x,
        NewDuty {
            subject: &subject,
            name: clean(b.name.as_deref()),
            role: &role,
            venue: b.venue_id,
            replaces: None,
        },
        &act,
    )
    .await?;
    emit_assigned(&mut tx, &scope, &x, duty, &subject, &act).await?;
    audit(
        &mut tx,
        &scope,
        "exams.invigilation_duty.assign",
        "invigilation_duty",
        duty.to_string(),
        json!({"sittingId": id, "subjectRef": subject, "role": role}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = load_duty(&st.pool, tenant, duty).await?;
    Ok(with_etag(StatusCode::CREATED, &d, Version(d.version)))
}

async fn begin(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: InvigilationDutyStatus,
) -> Result<(ExamInvigilationDutyView, ExecutionScope), KernelError> {
    let d = load_duty(&st.pool, ctx.scope.tenant_id, id).await?;
    let scope = institution_scope(ctx, InstitutionId(d.institution_id));
    require_institution_writable(st, &scope, InstitutionId(d.institution_id)).await?;
    check_version(expected, d.version)?;
    let from = InvigilationDutyStatus::parse(&d.status)
        .ok_or_else(|| KernelError::internal_msg("bad duty status"))?;
    if !duty_can(from, to) {
        return Err(illegal("the duty", from.as_str(), to.as_str()));
    }
    Ok((d, scope))
}

fn duty_move<'a>(d: &'a ExamInvigilationDutyView, tenant: TenantId, to: &'a str) -> Move<'a> {
    Move {
        table: "exams.invigilation_duty",
        aggregate: "invigilation_duty",
        has_process: false,
        tenant,
        institution: InstitutionId(d.institution_id),
        id: d.id,
        version: d.version,
        from: &d.status,
        to,
    }
}

async fn simple(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: InvigilationDutyStatus,
    reason: Option<String>,
) -> ApiResult<Response> {
    let tenant = ctx.scope.tenant_id;
    let (d, scope) = begin(st, ctx, id, expected, to).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        duty_move(&d, tenant, to.as_str()),
        &StatusChange {
            cause_event_type: "exams.invigilation_duty.move",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        &format!("exams.invigilation_duty.{}", to.as_str()),
        "invigilation_duty",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let d = load_duty(&st.pool, tenant, id).await?;
    Ok(with_etag(StatusCode::OK, &d, Version(d.version)))
}

/// Confirm an assigned duty (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/invigilation-duties/{id}/confirm", operation_id = "exam_invigilation_confirm", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duty")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamInvigilationDutyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn invigilation_confirm(
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
    simple(
        &st,
        ctx,
        id,
        expected,
        InvigilationDutyStatus::Confirmed,
        reason_of(body),
    )
    .await
}

/// Release a duty (`exam.setup.manage`, reason required) — the staffing view then shows any
/// shortage (L3-I-011).
#[utoipa::path(post, path = "/api/v1/exams/invigilation-duties/{id}/release", operation_id = "exam_invigilation_release", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duty")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamInvigilationDutyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn invigilation_release(
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
    simple(
        &st,
        ctx,
        id,
        expected,
        InvigilationDutyStatus::Released,
        reason,
    )
    .await
}

/// Replace a duty with another person (`exam.setup.manage`, reason required; allowed during
/// conduct, L3-I-060). The replaced duty keeps its history; the new one links to it.
#[utoipa::path(post, path = "/api/v1/exams/invigilation-duties/{id}/replace", operation_id = "exam_invigilation_replace", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duty")),
    request_body = ExamInvigilationReplace, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamInvigilationDutyView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn invigilation_replace(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamInvigilationReplace>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    require_reason(&reason)?;
    let subject = clean(Some(&b.subject_ref)).ok_or_else(|| {
        invalid(
            "subjectRef",
            "required",
            "the replacement's login subject is required",
        )
    })?;
    let tenant = ctx.scope.tenant_id;
    let (d, scope) = begin(&st, ctx, id, expected, InvigilationDutyStatus::Replaced).await?;
    if subject == d.subject_ref {
        return Err(invalid(
            "subjectRef",
            "same_person",
            "the replacement must be someone else",
        )
        .into());
    }
    let x = load_sitting(&st.pool, tenant, d.sitting_id).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        duty_move(&d, tenant, "replaced"),
        &StatusChange {
            cause_event_type: "exams.invigilation_duty.replace",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    if x.status == "planned" {
        check_clash(&mut tx, tenant, &x, &subject).await?;
    }
    let duty = insert_duty(
        &mut tx,
        tenant,
        &x,
        NewDuty {
            subject: &subject,
            name: clean(b.name.as_deref()),
            role: &d.role,
            venue: d.venue_id,
            replaces: Some(d.id),
        },
        &act,
    )
    .await?;
    emit_assigned(&mut tx, &scope, &x, duty, &subject, &act).await?;
    audit(
        &mut tx,
        &scope,
        "exams.invigilation_duty.replace",
        "invigilation_duty",
        id.to_string(),
        json!({"replacement": duty, "subjectRef": subject, "reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let n = load_duty(&st.pool, tenant, duty).await?;
    Ok(with_etag(StatusCode::OK, &n, Version(n.version)))
}

/// The caller's own invigilation / examiner duties (`exam.read`, faculty).
#[utoipa::path(get, path = "/api/v1/exams/me/invigilation", operation_id = "exam_invigilation_self_list", tag = "exam-scheduling",
    security(("session_cookie" = [])),
    responses((status = 200, body = ExamMyDutyList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_invigilation(
    State(st): State<ExamsState>,
    auth: Authenticated,
) -> ApiResult<Json<ExamMyDutyList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let items = sqlx::query_as::<_, ExamMyDutyView>(
        "SELECT d.id, d.sitting_id, s.session_id, p.paper_code, p.title, d.role, d.status, \
         s.sits_on, s.starts_at, s.duration_minutes, v.code AS venue_code \
         FROM exams.invigilation_duty d JOIN exams.exam_sitting s ON s.id = d.sitting_id \
         JOIN exams.exam_paper p ON p.id = s.paper_id \
         JOIN exams.exam_schedule sc ON sc.id = s.schedule_id \
         LEFT JOIN exams.exam_venue v ON v.id = d.venue_id \
         WHERE d.tenant_id = $1 AND d.subject_ref = $2 AND d.status IN ('assigned', 'confirmed', 'completed') \
         AND sc.status IN ('published', 'active') AND s.status <> 'cancelled' \
         ORDER BY s.sits_on, s.starts_at",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(&ctx.subject)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(ExamMyDutyList { items }))
}
