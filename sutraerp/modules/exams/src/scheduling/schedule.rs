//! Examination schedules / timetables and their sittings (L2-I03; L3-I-007 plan → approval,
//! I-008 revision → authoritative updated schedule, I-013 publication of the official
//! timetable, I-014 conflict detection and resolution, I-016 withdrawal → notified
//! stakeholders).
//!
//! One live schedule per session; a revision turns the published version `revised` and opens
//! the next draft version with the sittings (venue allocations and live duties) copied, so the
//! history of every published timetable is kept.

use crate::setup::common::{
    check_version, clean, emit, illegal, invalid, move_status, reason_of, require_name,
    require_reason, status_event, ExamSetupDecisionBody, ExamSetupDecisionKind, Move, ReasonBody,
};
use crate::setup::domain::{overlaps, schedule_can, schedule_is_editable};
use crate::setup::session::{load_paper, load_session, move_paper};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
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
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::{BTreeMap, HashMap, HashSet};
use sutra_contracts::academic::ids::CourseOfferingId;
use sutra_contracts::exams::events::setup::{
    EXAM_SCHEDULE_APPROVED, EXAM_SCHEDULE_APPROVED_SCHEMA, EXAM_TIMETABLE_PUBLISHED,
    EXAM_TIMETABLE_PUBLISHED_SCHEMA, EXAM_TIMETABLE_WITHDRAWN, EXAM_TIMETABLE_WITHDRAWN_SCHEMA,
};
use sutra_contracts::exams::status::{ExamPaperStatus, ExamScheduleStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

const TABLE: &str = "exams.exam_schedule";
const AGG: &str = "exam_schedule";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ScheduleRow {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub name: String,
    pub version_no: i32,
    pub supersedes_id: Option<Uuid>,
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub version: i64,
}

impl ScheduleRow {
    pub fn state(&self) -> Result<ExamScheduleStatus, KernelError> {
        ExamScheduleStatus::parse(&self.status).ok_or_else(|| {
            KernelError::internal_msg(format!("bad schedule status {}", self.status))
        })
    }
}

const SCHEDULE_COLUMNS: &str = "id, institution_id, session_id, name, version_no, supersedes_id, \
    status, status_reason, process_instance_id, published_at, version";

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct SittingRow {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub schedule_id: Uuid,
    pub session_id: Uuid,
    pub paper_id: Uuid,
    pub paper_code: String,
    pub paper_title: String,
    pub paper_mode: String,
    pub offering_id: Option<Uuid>,
    pub sits_on: NaiveDate,
    pub starts_at: NaiveTime,
    pub duration_minutes: i32,
    pub status: String,
    pub status_reason: Option<String>,
    pub postponed_from: Option<NaiveDate>,
    pub version: i64,
}

pub const SITTING_SELECT: &str = "SELECT s.id, s.institution_id, s.schedule_id, s.session_id, \
    s.paper_id, p.paper_code, p.title AS paper_title, p.mode AS paper_mode, p.offering_id, \
    s.sits_on, s.starts_at, s.duration_minutes, s.status, s.status_reason, s.postponed_from, \
    s.version FROM exams.exam_sitting s JOIN exams.exam_paper p ON p.id = s.paper_id";

/// A venue allocated to a sitting.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamVenueAllocationView {
    pub id: Uuid,
    pub sitting_id: Uuid,
    pub venue_id: Uuid,
    pub venue_code: String,
    pub venue_name: String,
    pub capacity: i32,
    pub seats: i32,
    #[schema(example = "allocated")]
    pub status: String,
}

/// One sitting (paper, date, start, duration) of a schedule.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSittingView {
    pub id: Uuid,
    pub schedule_id: Uuid,
    pub session_id: Uuid,
    pub paper_id: Uuid,
    pub paper_code: String,
    pub title: String,
    #[schema(example = "theory")]
    pub mode: String,
    pub sits_on: NaiveDate,
    /// Local start time `HH:MM`.
    #[schema(example = "09:30")]
    pub starts_at: String,
    pub duration_minutes: i32,
    /// `planned | postponed | cancelled | completed`.
    pub status: String,
    pub status_reason: Option<String>,
    pub postponed_from: Option<NaiveDate>,
    pub venues: Vec<ExamVenueAllocationView>,
    pub version: i64,
}

/// A detected scheduling conflict (L3-I-014).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamScheduleConflict {
    /// `candidate_overlap | venue_over_capacity`.
    pub kind: String,
    pub sitting_ids: Vec<Uuid>,
    /// Candidates affected (candidate overlaps) or seats over capacity (venues).
    pub count: i64,
    pub detail: String,
}

/// A schedule version with its sittings and current conflicts.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamScheduleView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub name: String,
    pub version_no: i32,
    pub supersedes_id: Option<Uuid>,
    /// `ExamScheduleStatus` wire name.
    #[schema(example = "published")]
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub sittings: Vec<ExamSittingView>,
    pub conflicts: Vec<ExamScheduleConflict>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamScheduleList {
    pub items: Vec<ExamScheduleView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamSittingList {
    pub items: Vec<ExamSittingView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ExamScheduleConflictList {
    pub items: Vec<ExamScheduleConflict>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ExamScheduleCreate {
    pub name: String,
}

/// New sitting of a draft schedule.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSittingCreate {
    pub paper_id: Uuid,
    pub sits_on: NaiveDate,
    #[schema(example = "09:30")]
    pub starts_at: String,
    /// Defaults to the paper's duration (180 when unset).
    pub duration_minutes: Option<i32>,
}

/// Sitting edit (draft schedules; conflict resolution, L3-I-014).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSittingUpdate {
    pub sits_on: Option<NaiveDate>,
    pub starts_at: Option<String>,
    pub duration_minutes: Option<i32>,
}

pub fn parse_time(field: &str, raw: &str) -> Result<NaiveTime, KernelError> {
    NaiveTime::parse_from_str(raw.trim(), "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(raw.trim(), "%H:%M:%S"))
        .map_err(|_| invalid(field, "invalid_time", "must be a local time HH:MM"))
}

// ------------------------------------------------------------------ loading

pub async fn load_schedule<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<ScheduleRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, ScheduleRow>(&format!(
        "SELECT {SCHEDULE_COLUMNS} FROM exams.exam_schedule WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "examination schedule",
    })
}

pub async fn load_sitting<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<SittingRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, SittingRow>(&format!(
        "{SITTING_SELECT} WHERE s.id = $1 AND s.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "examination sitting",
    })
}

/// Sittings of a schedule ordered by date / time (every status).
pub async fn sittings_of<'c, E>(
    executor: E,
    tenant: TenantId,
    schedule: Uuid,
) -> Result<Vec<SittingRow>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, SittingRow>(&format!(
        "{SITTING_SELECT} WHERE s.schedule_id = $1 AND s.tenant_id = $2 \
         ORDER BY s.sits_on, s.starts_at, p.paper_code"
    ))
    .bind(schedule)
    .bind(tenant.as_uuid())
    .fetch_all(executor)
    .await
    .map_err(map_db_error)
}

pub async fn allocations_of(
    st: &ExamsState,
    tenant: TenantId,
    sittings: &[Uuid],
) -> Result<Vec<ExamVenueAllocationView>, KernelError> {
    sqlx::query_as::<_, ExamVenueAllocationView>(
        "SELECT a.id, a.sitting_id, a.venue_id, v.code AS venue_code, v.name AS venue_name, \
         v.capacity, a.seats, a.status FROM exams.venue_allocation a \
         JOIN exams.exam_venue v ON v.id = a.venue_id \
         WHERE a.tenant_id = $1 AND a.sitting_id = ANY($2) AND a.status = 'allocated' \
         ORDER BY v.code",
    )
    .bind(tenant.as_uuid())
    .bind(sittings)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)
}

pub fn sitting_view(s: &SittingRow, venues: Vec<ExamVenueAllocationView>) -> ExamSittingView {
    ExamSittingView {
        id: s.id,
        schedule_id: s.schedule_id,
        session_id: s.session_id,
        paper_id: s.paper_id,
        paper_code: s.paper_code.clone(),
        title: s.paper_title.clone(),
        mode: s.paper_mode.clone(),
        sits_on: s.sits_on,
        starts_at: s.starts_at.format("%H:%M").to_string(),
        duration_minutes: s.duration_minutes,
        status: s.status.clone(),
        status_reason: s.status_reason.clone(),
        postponed_from: s.postponed_from,
        venues,
        version: s.version,
    }
}

pub async fn sitting_views(
    st: &ExamsState,
    tenant: TenantId,
    rows: &[SittingRow],
) -> Result<Vec<ExamSittingView>, KernelError> {
    let ids: Vec<Uuid> = rows.iter().map(|s| s.id).collect();
    let mut allocs = allocations_of(st, tenant, &ids).await?;
    Ok(rows
        .iter()
        .map(|s| {
            let mine: Vec<_> = allocs
                .iter()
                .filter(|a| a.sitting_id == s.id)
                .cloned()
                .collect();
            allocs.retain(|a| a.sitting_id != s.id);
            sitting_view(s, mine)
        })
        .collect())
}

/// Candidates (students) of each paper: effective registrations holding the paper, else the
/// seated roster of the paper's offering (planning before registration, L3-I-014).
async fn candidates_by_paper(
    st: &ExamsState,
    scope: &ExecutionScope,
    tenant: TenantId,
    sittings: &[&SittingRow],
) -> Result<HashMap<Uuid, HashSet<Uuid>>, KernelError> {
    let papers: Vec<Uuid> = sittings.iter().map(|s| s.paper_id).collect();
    let rows: Vec<(Uuid, Uuid)> = sqlx::query_as(
        "SELECT rp.paper_id, r.student_id FROM exams.exam_registration_paper rp \
         JOIN exams.exam_registration r ON r.id = rp.registration_id \
         WHERE rp.tenant_id = $1 AND rp.paper_id = ANY($2) AND rp.status IN ('registered', 'appeared') \
         AND r.status IN ('approved', 'confirmed', 'locked', 'amended')",
    )
    .bind(tenant.as_uuid())
    .bind(&papers)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut out: HashMap<Uuid, HashSet<Uuid>> = HashMap::new();
    for (paper, student) in rows {
        out.entry(paper).or_default().insert(student);
    }
    for s in sittings {
        if out.contains_key(&s.paper_id) {
            continue;
        }
        let Some(offering) = s.offering_id else {
            continue;
        };
        let mut set = HashSet::new();
        for section in st
            .offerings
            .sections_of_offering(scope, CourseOfferingId(offering))
            .await?
        {
            for r in st.offerings.roster(scope, section.id, true).await? {
                set.insert(r.student_id.as_uuid());
            }
        }
        out.insert(s.paper_id, set);
    }
    Ok(out)
}

/// Candidate overlaps between the schedule's live sittings and venue over-booking against every
/// live sitting using the same venue (L3-I-014 / I-009).
pub async fn detect_conflicts(
    st: &ExamsState,
    scope: &ExecutionScope,
    tenant: TenantId,
    schedule: Uuid,
) -> Result<Vec<ExamScheduleConflict>, KernelError> {
    let rows = sittings_of(&st.pool, tenant, schedule).await?;
    let live: Vec<&SittingRow> = rows.iter().filter(|s| s.status == "planned").collect();
    let candidates = candidates_by_paper(st, scope, tenant, &live).await?;
    let mut out = Vec::new();
    for (i, a) in live.iter().enumerate() {
        for b in live.iter().skip(i + 1) {
            if !overlaps(
                (a.sits_on, a.starts_at, a.duration_minutes),
                (b.sits_on, b.starts_at, b.duration_minutes),
            ) {
                continue;
            }
            let (Some(ca), Some(cb)) = (candidates.get(&a.paper_id), candidates.get(&b.paper_id))
            else {
                continue;
            };
            let both = ca.intersection(cb).count() as i64;
            if both > 0 {
                out.push(ExamScheduleConflict {
                    kind: "candidate_overlap".into(),
                    sitting_ids: vec![a.id, b.id],
                    count: both,
                    detail: format!(
                        "{both} candidate(s) sit both {} and {} at overlapping times on {}",
                        a.paper_code, b.paper_code, a.sits_on
                    ),
                });
            }
        }
    }
    // Venues: every live allocation of a venue used by this schedule, across live schedules.
    #[derive(sqlx::FromRow)]
    struct Use {
        sitting_id: Uuid,
        venue_id: Uuid,
        code: String,
        capacity: i32,
        seats: i32,
        sits_on: NaiveDate,
        starts_at: NaiveTime,
        duration_minutes: i32,
    }
    let uses: Vec<Use> = sqlx::query_as(
        "SELECT a.sitting_id, a.venue_id, v.code, v.capacity, a.seats, s.sits_on, s.starts_at, \
         s.duration_minutes FROM exams.venue_allocation a \
         JOIN exams.exam_venue v ON v.id = a.venue_id \
         JOIN exams.exam_sitting s ON s.id = a.sitting_id \
         JOIN exams.exam_schedule sc ON sc.id = s.schedule_id \
         WHERE a.tenant_id = $1 AND a.status = 'allocated' AND s.status = 'planned' \
         AND sc.status IN ('draft', 'under_review', 'approved', 'published', 'active') \
         AND a.venue_id IN (SELECT venue_id FROM exams.venue_allocation x \
             JOIN exams.exam_sitting y ON y.id = x.sitting_id \
             WHERE y.schedule_id = $2 AND x.status = 'allocated')",
    )
    .bind(tenant.as_uuid())
    .bind(schedule)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let ours: HashSet<Uuid> = live.iter().map(|s| s.id).collect();
    let mut seen: HashSet<(Uuid, Uuid)> = HashSet::new();
    for u in uses.iter().filter(|u| ours.contains(&u.sitting_id)) {
        let concurrent: Vec<&Use> = uses
            .iter()
            .filter(|o| {
                o.venue_id == u.venue_id
                    && overlaps(
                        (u.sits_on, u.starts_at, u.duration_minutes),
                        (o.sits_on, o.starts_at, o.duration_minutes),
                    )
            })
            .collect();
        let seats: i64 = concurrent.iter().map(|o| i64::from(o.seats)).sum();
        if concurrent.len() > 1 && seats > i64::from(u.capacity) {
            let mut ids: Vec<Uuid> = concurrent.iter().map(|o| o.sitting_id).collect();
            ids.sort();
            if seen.insert((u.venue_id, ids[0])) {
                out.push(ExamScheduleConflict {
                    kind: "venue_over_capacity".into(),
                    sitting_ids: ids,
                    count: seats - i64::from(u.capacity),
                    detail: format!(
                        "venue {} is double-booked: {seats} seats allocated for {} concurrent sittings, capacity {}",
                        u.code,
                        concurrent.len(),
                        u.capacity
                    ),
                });
            }
        }
    }
    Ok(out)
}

pub async fn schedule_view(
    st: &ExamsState,
    scope: &ExecutionScope,
    tenant: TenantId,
    s: ScheduleRow,
) -> Result<ExamScheduleView, KernelError> {
    let rows = sittings_of(&st.pool, tenant, s.id).await?;
    let sittings = sitting_views(st, tenant, &rows).await?;
    let conflicts = if matches!(
        s.state()?,
        ExamScheduleStatus::Draft | ExamScheduleStatus::UnderReview | ExamScheduleStatus::Approved
    ) {
        detect_conflicts(st, scope, tenant, s.id).await?
    } else {
        Vec::new()
    };
    Ok(ExamScheduleView {
        id: s.id,
        institution_id: s.institution_id,
        session_id: s.session_id,
        name: s.name,
        version_no: s.version_no,
        supersedes_id: s.supersedes_id,
        status: s.status,
        status_reason: s.status_reason,
        process_instance_id: s.process_instance_id,
        published_at: s.published_at,
        sittings,
        conflicts,
        version: s.version,
    })
}

async fn respond(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let tenant = ctx.scope.tenant_id;
    let s = load_schedule(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(s.institution_id));
    let v = schedule_view(st, &scope, tenant, s).await?;
    let version = v.version;
    Ok(with_etag(status, &v, Version(version)))
}

/// The official (published / active) schedule of a session, else the latest revised one.
pub async fn official_schedule(
    st: &ExamsState,
    tenant: TenantId,
    session: Uuid,
) -> Result<Option<ScheduleRow>, KernelError> {
    sqlx::query_as::<_, ScheduleRow>(&format!(
        "SELECT {SCHEDULE_COLUMNS} FROM exams.exam_schedule WHERE tenant_id = $1 AND session_id = $2 \
         AND status IN ('published', 'active', 'revised') \
         ORDER BY (status <> 'revised') DESC, version_no DESC LIMIT 1"
    ))
    .bind(tenant.as_uuid())
    .bind(session)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)
}

// ------------------------------------------------------------------ schedules

/// Schedule versions of a session, newest first (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sessions/{id}/schedules", operation_id = "exam_schedule_list", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamScheduleList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamScheduleList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    let session = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, session.institution_id);
    let rows = sqlx::query_as::<_, ScheduleRow>(&format!(
        "SELECT {SCHEDULE_COLUMNS} FROM exams.exam_schedule WHERE tenant_id = $1 AND session_id = $2 \
         ORDER BY version_no DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows {
        items.push(schedule_view(&st, &scope, tenant, r).await?);
    }
    Ok(Json(ExamScheduleList { items }))
}

/// Plan a draft schedule for a session (`exam.setup.manage`; one live schedule per session).
#[utoipa::path(post, path = "/api/v1/exams/exam-sessions/{id}/schedules", operation_id = "exam_schedule_create", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamScheduleCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamScheduleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamScheduleCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let session = load_session(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, session.institution_id);
    require_institution_writable(&st, &scope, session.institution_id).await?;
    if !session.status.is_configurable() {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; no new schedule can be planned",
            session.code, session.status
        ))
        .into());
    }
    let name = require_name("name", &b.name)?;
    let live: Option<String> = sqlx::query_scalar(
        "SELECT name FROM exams.exam_schedule WHERE tenant_id = $1 AND session_id = $2 \
         AND status IN ('draft', 'under_review', 'approved', 'published', 'active')",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    if let Some(n) = live {
        return Err(KernelError::Conflict(format!(
            "the session already has a live schedule ({n}); revise it instead"
        ))
        .into());
    }
    let next: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(version_no), 0) + 1 FROM exams.exam_schedule WHERE session_id = $1",
    )
    .bind(id)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_schedule (tenant_id, institution_id, session_id, name, version_no, \
         created_by) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(session.institution_id.as_uuid())
    .bind(id)
    .bind(&name)
    .bind(next)
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_schedule.create",
        AGG,
        new_id.to_string(),
        json!({"sessionId": id, "versionNo": next}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, new_id, StatusCode::CREATED).await
}

/// One schedule with its sittings and conflicts (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-schedules/{id}", operation_id = "exam_schedule_get", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamScheduleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Current conflicts of a schedule (`exam.read`, L3-I-014).
#[utoipa::path(get, path = "/api/v1/exams/exam-schedules/{id}/conflicts", operation_id = "exam_schedule_conflicts", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamScheduleConflictList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_conflicts(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamScheduleConflictList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    let s = load_schedule(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(s.institution_id));
    let items = detect_conflicts(&st, &scope, tenant, id).await?;
    Ok(Json(ExamScheduleConflictList { items }))
}

fn mv<'a>(
    s: &'a ScheduleRow,
    tenant: TenantId,
    version: i64,
    from: &'a str,
    to: &'a str,
) -> Move<'a> {
    Move {
        table: TABLE,
        aggregate: AGG,
        has_process: true,
        tenant,
        institution: InstitutionId(s.institution_id),
        id: s.id,
        version,
        from,
        to,
    }
}

async fn begin(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    to: ExamScheduleStatus,
) -> Result<(ScheduleRow, ExecutionScope), KernelError> {
    let tenant = ctx.scope.tenant_id;
    let s = load_schedule(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(s.institution_id));
    require_institution_writable(st, &scope, InstitutionId(s.institution_id)).await?;
    check_version(expected, s.version)?;
    let from = s.state()?;
    if !schedule_can(from, to) {
        return Err(illegal(
            &format!("schedule {} v{}", s.name, s.version_no),
            from.as_str(),
            to.as_str(),
        ));
    }
    Ok((s, scope))
}

fn schedule_event(
    s: &ScheduleRow,
    from: &str,
    to: &str,
    version: i64,
) -> sutra_contracts::exams::events::StatusChangedV1 {
    let mut e = status_event(
        s.id,
        InstitutionId(s.institution_id),
        Some(from),
        to,
        version,
    );
    e.session_id = Some(s.session_id);
    e
}

/// Submit a draft schedule for approval (`exam.setup.manage`): it needs sittings and no open
/// conflict (`409` listing them); `SCHEDULE_APPROVAL` decides (auto-approved in Batch 6).
#[utoipa::path(post, path = "/api/v1/exams/exam-schedules/{id}/submit", operation_id = "exam_schedule_submit", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the schedule")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamScheduleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_submit(
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
    let (s, scope) = begin(&st, ctx, id, expected, ExamScheduleStatus::UnderReview).await?;
    let sittings = sittings_of(&st.pool, tenant, id).await?;
    if !sittings.iter().any(|x| x.status == "planned") {
        return Err(KernelError::Conflict("the schedule has no sittings yet".into()).into());
    }
    let conflicts = detect_conflicts(&st, &scope, tenant, id).await?;
    if !conflicts.is_empty() {
        let what: Vec<String> = conflicts.iter().map(|c| c.detail.clone()).collect();
        return Err(KernelError::Conflict(format!(
            "resolve the scheduling conflicts first: {}",
            what.join("; ")
        ))
        .into());
    }
    let outcome = request_approval(
        &st,
        &scope,
        process::SCHEDULE_APPROVAL,
        business_key(AGG, id, s.version),
        BTreeMap::from([
            ("sessionId".to_owned(), json!(s.session_id)),
            ("sittings".to_owned(), json!(sittings.len())),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        mv(&s, tenant, s.version, "draft", "under_review"),
        &StatusChange {
            cause_event_type: "exams.exam_schedule.submit",
            reason: reason.as_deref(),
            process_instance_id: Some(&pid),
        },
        &act,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let sys = Actor::system(act.correlation_id.clone());
        let v = move_status(
            &mut tx,
            mv(&s, tenant, v, "under_review", "approved"),
            &StatusChange {
                cause_event_type: EXAM_SCHEDULE_APPROVED,
                reason: Some("auto-approved"),
                process_instance_id: Some(&pid),
            },
            &sys,
        )
        .await?;
        let mut e = schedule_event(&s, "under_review", "approved", v);
        e.process_instance_id = Some(pid.clone());
        emit(
            &mut tx,
            &scope,
            (EXAM_SCHEDULE_APPROVED, EXAM_SCHEDULE_APPROVED_SCHEMA),
            &e,
            &sys,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_schedule.submit",
        AGG,
        id.to_string(),
        json!({"processInstanceId": pid}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Decide a schedule under review (`exam.approve`): approve → `approved`, reject → `draft`.
#[utoipa::path(post, path = "/api/v1/exams/exam-schedules/{id}/decision", operation_id = "exam_schedule_decision", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the schedule")),
    request_body = ExamSetupDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamScheduleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_decision(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSetupDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "exam timetable").await?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = clean(b.reason.as_deref());
    let to = match b.decision {
        ExamSetupDecisionKind::Approve => ExamScheduleStatus::Approved,
        ExamSetupDecisionKind::Reject => {
            require_reason(&reason)?;
            ExamScheduleStatus::Draft
        }
    };
    let tenant = ctx.scope.tenant_id;
    let cur = load_schedule(&st.pool, tenant, id).await?;
    if cur.state()? != ExamScheduleStatus::UnderReview {
        return Err(KernelError::Conflict(format!(
            "schedule {} is {}; only a schedule under review can be decided",
            cur.name, cur.status
        ))
        .into());
    }
    let (s, scope) = begin(&st, ctx, id, expected, to).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        mv(&s, tenant, s.version, "under_review", to.as_str()),
        &StatusChange {
            cause_event_type: EXAM_SCHEDULE_APPROVED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    if to == ExamScheduleStatus::Approved {
        let mut e = schedule_event(&s, "under_review", "approved", v);
        e.process_instance_id.clone_from(&s.process_instance_id);
        e.reason.clone_from(&reason);
        emit(
            &mut tx,
            &scope,
            (EXAM_SCHEDULE_APPROVED, EXAM_SCHEDULE_APPROVED_SCHEMA),
            &e,
            &act,
        )
        .await?;
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_schedule.decision",
        AGG,
        id.to_string(),
        json!({"to": to.as_str(), "reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Publish an approved schedule as the official timetable (`exam.setup.manage`, L3-I-013):
/// draft papers of its sittings become `scheduled`; students and faculty are notified.
#[utoipa::path(post, path = "/api/v1/exams/exam-schedules/{id}/publish", operation_id = "exam_schedule_publish", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the schedule")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamScheduleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_publish(
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
    let (s, scope) = begin(&st, ctx, id, expected, ExamScheduleStatus::Published).await?;
    let session = load_session(&st.pool, tenant, s.session_id).await?;
    if matches!(
        session.status,
        sutra_contracts::exams::status::ExamSessionStatus::Draft
            | sutra_contracts::exams::status::ExamSessionStatus::UnderReview
    ) || !session.status.is_configurable()
    {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; the timetable can be published once the session is approved",
            session.code, session.status
        ))
        .into());
    }
    let sittings = sittings_of(&st.pool, tenant, id).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    for x in sittings.iter().filter(|x| x.status == "planned") {
        let p = load_paper(&mut *tx, tenant, x.paper_id).await?;
        if p.status == ExamPaperStatus::Draft {
            move_paper(
                &mut tx,
                tenant,
                p.id,
                ExamPaperStatus::Scheduled,
                Some("timetable published"),
                &act,
            )
            .await?;
        }
    }
    let v = move_status(
        &mut tx,
        mv(&s, tenant, s.version, "approved", "published"),
        &StatusChange {
            cause_event_type: EXAM_TIMETABLE_PUBLISHED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    sqlx::query("UPDATE exams.exam_schedule SET published_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let mut e = schedule_event(&s, "approved", "published", v);
    e.reason = Some(format!("{} ({} v{})", s.name, session.code, s.version_no));
    emit(
        &mut tx,
        &scope,
        (EXAM_TIMETABLE_PUBLISHED, EXAM_TIMETABLE_PUBLISHED_SCHEMA),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_schedule.publish",
        AGG,
        id.to_string(),
        json!({"sittings": sittings.len()}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

/// Copies the live sittings (with venue allocations and live duties) of `from` into `to`.
async fn copy_sittings(
    conn: &mut PgConnection,
    tenant: TenantId,
    from: Uuid,
    to: Uuid,
) -> Result<(), KernelError> {
    let rows = sittings_of(&mut *conn, tenant, from).await?;
    for x in rows.iter().filter(|x| x.status == "planned") {
        let new_id: Uuid = sqlx::query_scalar(
            "INSERT INTO exams.exam_sitting (tenant_id, institution_id, schedule_id, session_id, \
             paper_id, sits_on, starts_at, duration_minutes, postponed_from) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
        )
        .bind(tenant.as_uuid())
        .bind(x.institution_id)
        .bind(to)
        .bind(x.session_id)
        .bind(x.paper_id)
        .bind(x.sits_on)
        .bind(x.starts_at)
        .bind(x.duration_minutes)
        .bind(x.postponed_from)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        sqlx::query(
            "INSERT INTO exams.venue_allocation (tenant_id, institution_id, sitting_id, venue_id, seats) \
             SELECT tenant_id, institution_id, $2, venue_id, seats FROM exams.venue_allocation \
             WHERE sitting_id = $1 AND status = 'allocated'",
        )
        .bind(x.id)
        .bind(new_id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
        sqlx::query(
            "INSERT INTO exams.invigilation_duty (tenant_id, institution_id, sitting_id, venue_id, \
             subject_ref, name, role, status, created_by) \
             SELECT tenant_id, institution_id, $2, venue_id, subject_ref, name, role, status, created_by \
             FROM exams.invigilation_duty WHERE sitting_id = $1 AND status IN ('assigned', 'confirmed')",
        )
        .bind(x.id)
        .bind(new_id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(())
}

/// Revise the published timetable (`exam.setup.manage`, reason required, L3-I-008): the
/// published version becomes `revised` and the next draft version opens with its sittings;
/// returns the new draft.
#[utoipa::path(post, path = "/api/v1/exams/exam-schedules/{id}/revise", operation_id = "exam_schedule_revise", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the schedule")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamScheduleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_revise(
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
    let (s, scope) = begin(&st, ctx, id, expected, ExamScheduleStatus::Revised).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        mv(&s, tenant, s.version, &s.status, "revised"),
        &StatusChange {
            cause_event_type: "exams.exam_schedule.revise",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_schedule (tenant_id, institution_id, session_id, name, version_no, \
         supersedes_id, created_by) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(s.institution_id)
    .bind(s.session_id)
    .bind(&s.name)
    .bind(s.version_no + 1)
    .bind(s.id)
    .bind(act.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    copy_sittings(&mut tx, tenant, id, new_id).await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_schedule.revise",
        AGG,
        id.to_string(),
        json!({"reason": reason, "nextVersionId": new_id}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, new_id, StatusCode::OK).await
}

/// Withdraw the published timetable (`exam.setup.manage`, reason required, L3-I-016):
/// students and faculty are notified.
#[utoipa::path(post, path = "/api/v1/exams/exam-schedules/{id}/withdraw", operation_id = "exam_schedule_withdraw", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the schedule")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamScheduleView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn schedule_withdraw(
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
    let (s, scope) = begin(&st, ctx, id, expected, ExamScheduleStatus::Cancelled).await?;
    if !matches!(
        s.state()?,
        ExamScheduleStatus::Published | ExamScheduleStatus::Active
    ) {
        return Err(KernelError::Conflict(
            "only a published timetable can be withdrawn; cancel a draft by revising it".into(),
        )
        .into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        mv(&s, tenant, s.version, &s.status, "cancelled"),
        &StatusChange {
            cause_event_type: EXAM_TIMETABLE_WITHDRAWN,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let mut e = schedule_event(&s, &s.status, "cancelled", v);
    e.reason.clone_from(&reason);
    emit(
        &mut tx,
        &scope,
        (EXAM_TIMETABLE_WITHDRAWN, EXAM_TIMETABLE_WITHDRAWN_SCHEMA),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_schedule.withdraw",
        AGG,
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, ctx, id, StatusCode::OK).await
}

// ------------------------------------------------------------------ sittings

/// Sittings of a schedule (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-schedules/{id}/sittings", operation_id = "exam_schedule_sitting_list", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamSittingList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sitting_list(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ExamSittingList>> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_schedule(&st.pool, tenant, id).await?;
    let rows = sittings_of(&st.pool, tenant, id).await?;
    Ok(Json(ExamSittingList {
        items: sitting_views(&st, tenant, &rows).await?,
    }))
}

async fn sitting_response(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let s = load_sitting(&st.pool, tenant, id).await?;
    let venues = allocations_of(st, tenant, &[id]).await?;
    let v = sitting_view(&s, venues);
    Ok(with_etag(status, &v, Version(s.version)))
}

fn check_window(
    session: &sutra_contracts::exams::ports::ExamSessionSummary,
    day: NaiveDate,
) -> Result<(), KernelError> {
    let before = session.starts_on.is_some_and(|d| day < d);
    let after = session.ends_on.is_some_and(|d| day > d);
    if before || after {
        return Err(invalid(
            "sitsOn",
            "outside_session",
            "the sitting must fall within the session's examination window",
        ));
    }
    Ok(())
}

/// Add a sitting to a draft schedule (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/exam-schedules/{id}/sittings", operation_id = "exam_schedule_sitting_create", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    request_body = ExamSittingCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamSittingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sitting_create(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSittingCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    require_idempotency_key(&headers)?;
    let tenant = ctx.scope.tenant_id;
    let s = load_schedule(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(s.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(s.institution_id)).await?;
    if !schedule_is_editable(s.state()?) {
        return Err(KernelError::Conflict(format!(
            "schedule {} is {}; revise it to change sittings",
            s.name, s.status
        ))
        .into());
    }
    let paper = load_paper(&st.pool, tenant, b.paper_id)
        .await
        .map_err(|_| invalid("paperId", "not_found", "unknown examination paper"))?;
    if paper.session_id.as_uuid() != s.session_id {
        return Err(invalid(
            "paperId",
            "other_session",
            "the paper belongs to another session",
        )
        .into());
    }
    if paper.status == ExamPaperStatus::Cancelled {
        return Err(
            KernelError::Conflict(format!("paper {} is cancelled", paper.paper_code)).into(),
        );
    }
    let session = load_session(&st.pool, tenant, s.session_id).await?;
    check_window(&session, b.sits_on)?;
    let starts = parse_time("startsAt", &b.starts_at)?;
    let duration = b.duration_minutes.or(paper.duration_minutes).unwrap_or(180);
    if !(1..=1440).contains(&duration) {
        return Err(invalid("durationMinutes", "out_of_range", "must be 1 … 1440").into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let new_id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_sitting (tenant_id, institution_id, schedule_id, session_id, \
         paper_id, sits_on, starts_at, duration_minutes) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(s.institution_id)
    .bind(id)
    .bind(s.session_id)
    .bind(b.paper_id)
    .bind(b.sits_on)
    .bind(starts)
    .bind(duration)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!(
            "paper {} already has a sitting in this schedule",
            paper.paper_code
        )),
        other => other,
    })?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_sitting.create",
        "exam_sitting",
        new_id.to_string(),
        json!({"scheduleId": id, "paperId": b.paper_id, "sitsOn": b.sits_on}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    sitting_response(&st, tenant, new_id, StatusCode::CREATED).await
}

/// One sitting with its venues (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sittings/{id}", operation_id = "exam_sitting_get", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamSittingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sitting_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    sitting_response(&st, ctx.scope.tenant_id, id, StatusCode::OK).await
}

async fn editable_sitting(
    st: &ExamsState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
) -> Result<(SittingRow, ExecutionScope), KernelError> {
    let tenant = ctx.scope.tenant_id;
    let x = load_sitting(&st.pool, tenant, id).await?;
    let scope = institution_scope(ctx, InstitutionId(x.institution_id));
    require_institution_writable(st, &scope, InstitutionId(x.institution_id)).await?;
    check_version(expected, x.version)?;
    let s = load_schedule(&st.pool, tenant, x.schedule_id).await?;
    if !schedule_is_editable(s.state()?) {
        return Err(KernelError::Conflict(format!(
            "schedule {} is {}; revise it to change sittings",
            s.name, s.status
        )));
    }
    if x.status != "planned" {
        return Err(KernelError::Conflict(format!(
            "the sitting is {}",
            x.status
        )));
    }
    Ok((x, scope))
}

/// Move / resize a sitting of a draft schedule (`exam.setup.manage`, `If-Match`; conflict
/// resolution, L3-I-014).
#[utoipa::path(patch, path = "/api/v1/exams/exam-sittings/{id}", operation_id = "exam_sitting_update", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the sitting")),
    request_body = ExamSittingUpdate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSittingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sitting_update(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ExamSittingUpdate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let expected = match if_match(&headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let (x, scope) = editable_sitting(&st, ctx, id, expected).await?;
    let day = b.sits_on.unwrap_or(x.sits_on);
    let session = load_session(&st.pool, tenant, x.session_id).await?;
    check_window(&session, day)?;
    let starts = match b.starts_at.as_deref() {
        Some(t) => parse_time("startsAt", t)?,
        None => x.starts_at,
    };
    let duration = b.duration_minutes.unwrap_or(x.duration_minutes);
    if !(1..=1440).contains(&duration) {
        return Err(invalid("durationMinutes", "out_of_range", "must be 1 … 1440").into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let n = sqlx::query(
        "UPDATE exams.exam_sitting SET sits_on = $3, starts_at = $4, duration_minutes = $5, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $6",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(day)
    .bind(starts)
    .bind(duration)
    .bind(x.version)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?
    .rows_affected();
    if n == 0 {
        return Err(KernelError::Conflict("the sitting changed concurrently".into()).into());
    }
    audit(
        &mut tx,
        &scope,
        "exams.exam_sitting.update",
        "exam_sitting",
        id.to_string(),
        json!({"sitsOn": day, "startsAt": starts.format("%H:%M").to_string(), "durationMinutes": duration}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    sitting_response(&st, tenant, id, StatusCode::OK).await
}

/// Cancel a sitting of a draft schedule (`exam.setup.manage`, reason required).
#[utoipa::path(post, path = "/api/v1/exams/exam-sittings/{id}/cancel", operation_id = "exam_sitting_cancel", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the sitting")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSittingView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn sitting_cancel(
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
    let (x, scope) = editable_sitting(&st, ctx, id, expected).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        Move {
            table: "exams.exam_sitting",
            aggregate: "exam_sitting",
            has_process: false,
            tenant,
            institution: InstitutionId(x.institution_id),
            id,
            version: x.version,
            from: "planned",
            to: "cancelled",
        },
        &StatusChange {
            cause_event_type: "exams.exam_sitting.cancel",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    sqlx::query("UPDATE exams.venue_allocation SET status = 'released', version = version + 1, updated_at = now() WHERE sitting_id = $1 AND status = 'allocated'")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(
        &mut tx,
        &scope,
        "exams.exam_sitting.cancel",
        "exam_sitting",
        id.to_string(),
        json!({"reason": reason}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    sitting_response(&st, tenant, id, StatusCode::OK).await
}
