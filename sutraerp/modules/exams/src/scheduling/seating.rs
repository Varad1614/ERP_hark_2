//! Seating plans per sitting (L3-I-015 venue allocation → authoritative seating arrangement,
//! L3-I-059 seating conflict → valid seating outcome).
//!
//! A plan seats every effective registration holding the sitting's paper (registered, not
//! withdrawn / debarred) in the sitting's allocated venues, ordered by student number, with
//! deterministic seat numbers `<VENUE>-001 …`. More candidates than allocated seats is `409`.
//! Seat assignments are immutable; a published plan is changed by a revision (`revised` + a new
//! draft plan regenerated from the current registrations and venues).

use super::schedule::{allocations_of, load_sitting, SittingRow};
use crate::setup::common::{
    emit, illegal, move_status, reason_of, require_reason, status_event, Move, ReasonBody,
};
use crate::setup::domain::seating_can;
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, if_match, with_etag};
use crate::shared::lifecycle::StatusChange;
use crate::shared::publish::audit;
use crate::shared::scope::institution_scope;
use crate::shared::ExamsState;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::json;
use sqlx::PgConnection;
use sutra_contracts::exams::events::setup::{
    SEATING_PLAN_PUBLISHED, SEATING_PLAN_PUBLISHED_SCHEMA,
};
use sutra_contracts::exams::status::SeatingPlanStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use uuid::Uuid;

/// One seat of a plan.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamSeatView {
    pub registration_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub venue_id: Uuid,
    pub venue_code: String,
    #[schema(example = "HALL-A-001")]
    pub seat_no: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
struct PlanRow {
    id: Uuid,
    institution_id: Uuid,
    sitting_id: Uuid,
    plan_no: i32,
    supersedes_id: Option<Uuid>,
    candidates: i32,
    status: String,
    status_reason: Option<String>,
    published_at: Option<DateTime<Utc>>,
    version: i64,
}

/// A seating plan with its seats.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamSeatingPlanView {
    pub id: Uuid,
    pub sitting_id: Uuid,
    pub plan_no: i32,
    pub supersedes_id: Option<Uuid>,
    pub candidates: i32,
    /// `draft | published | revised | cancelled`.
    pub status: String,
    pub status_reason: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub seats: Vec<ExamSeatView>,
    pub version: i64,
}

const PLAN_COLUMNS: &str = "id, institution_id, sitting_id, plan_no, supersedes_id, candidates, \
    status, status_reason, published_at, version";

async fn load_plan<'c, E>(executor: E, tenant: TenantId, id: Uuid) -> Result<PlanRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query_as::<_, PlanRow>(&format!(
        "SELECT {PLAN_COLUMNS} FROM exams.seating_plan WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "seating plan",
    })
}

async fn plan_view(
    st: &ExamsState,
    tenant: TenantId,
    p: PlanRow,
) -> Result<ExamSeatingPlanView, KernelError> {
    let seats = sqlx::query_as::<_, ExamSeatView>(
        "SELECT a.registration_id, a.student_id, a.student_number, a.display_name, a.venue_id, \
         v.code AS venue_code, a.seat_no FROM exams.seat_assignment a \
         JOIN exams.exam_venue v ON v.id = a.venue_id \
         WHERE a.tenant_id = $1 AND a.plan_id = $2 ORDER BY v.code, a.seat_no",
    )
    .bind(tenant.as_uuid())
    .bind(p.id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(ExamSeatingPlanView {
        id: p.id,
        sitting_id: p.sitting_id,
        plan_no: p.plan_no,
        supersedes_id: p.supersedes_id,
        candidates: p.candidates,
        status: p.status,
        status_reason: p.status_reason,
        published_at: p.published_at,
        seats,
        version: p.version,
    })
}

async fn respond(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> ApiResult<Response> {
    let p = load_plan(&st.pool, tenant, id).await?;
    let v = plan_view(st, tenant, p).await?;
    let version = v.version;
    Ok(with_etag(status, &v, Version(version)))
}

#[derive(sqlx::FromRow)]
struct Candidate {
    registration_id: Uuid,
    student_id: Uuid,
    student_number: String,
    display_name: String,
}

/// Seat numbers for `candidates` over `(venue_id, code, seats)` in order; `Err` when they do
/// not fit (pure, unit-tested).
pub fn assign_seats(
    candidates: usize,
    venues: &[(Uuid, String, i32)],
) -> Result<Vec<(Uuid, String)>, usize> {
    let total: usize = venues
        .iter()
        .map(|v| usize::try_from(v.2).unwrap_or(0))
        .sum();
    if candidates > total {
        return Err(total);
    }
    let mut out = Vec::with_capacity(candidates);
    'outer: for (venue, code, seats) in venues {
        for n in 1..=usize::try_from(*seats).unwrap_or(0) {
            if out.len() == candidates {
                break 'outer;
            }
            out.push((*venue, format!("{code}-{n:03}")));
        }
    }
    Ok(out)
}

/// Builds a new draft plan for the sitting in the caller's transaction.
async fn build_plan(
    conn: &mut PgConnection,
    st: &ExamsState,
    tenant: TenantId,
    x: &SittingRow,
    supersedes: Option<Uuid>,
) -> Result<Uuid, KernelError> {
    let candidates: Vec<Candidate> = sqlx::query_as(
        "SELECT r.id AS registration_id, r.student_id, r.student_number, r.display_name \
         FROM exams.exam_registration r JOIN exams.exam_registration_paper rp ON rp.registration_id = r.id \
         WHERE r.tenant_id = $1 AND rp.paper_id = $2 AND rp.status = 'registered' \
         AND r.status IN ('approved', 'confirmed', 'locked', 'amended') \
         ORDER BY r.student_number, r.id",
    )
    .bind(tenant.as_uuid())
    .bind(x.paper_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let venues: Vec<(Uuid, String, i32)> = allocations_of(st, tenant, &[x.id])
        .await?
        .into_iter()
        .map(|a| (a.venue_id, a.venue_code, a.seats))
        .collect();
    if venues.is_empty() {
        return Err(KernelError::Conflict(
            "allocate at least one venue to the sitting before seating candidates".into(),
        ));
    }
    let seats = assign_seats(candidates.len(), &venues).map_err(|total| {
        KernelError::Conflict(format!(
            "{} candidate(s) but only {total} allocated seat(s); allocate more venue capacity",
            candidates.len()
        ))
    })?;
    let plan_no: i32 = sqlx::query_scalar(
        "SELECT COALESCE(max(plan_no), 0) + 1 FROM exams.seating_plan WHERE sitting_id = $1",
    )
    .bind(x.id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let plan: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.seating_plan (tenant_id, institution_id, sitting_id, plan_no, \
         supersedes_id, candidates) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(x.institution_id)
    .bind(x.id)
    .bind(plan_no)
    .bind(supersedes)
    .bind(i32::try_from(candidates.len()).unwrap_or(i32::MAX))
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (c, (venue, seat)) in candidates.iter().zip(seats) {
        sqlx::query(
            "INSERT INTO exams.seat_assignment (tenant_id, institution_id, plan_id, registration_id, \
             student_id, student_number, display_name, venue_id, seat_no) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(tenant.as_uuid())
        .bind(x.institution_id)
        .bind(plan)
        .bind(c.registration_id)
        .bind(c.student_id)
        .bind(&c.student_number)
        .bind(&c.display_name)
        .bind(venue)
        .bind(&seat)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    Ok(plan)
}

fn plan_move<'a>(p: &'a PlanRow, tenant: TenantId, to: &'a str) -> Move<'a> {
    Move {
        table: "exams.seating_plan",
        aggregate: "seating_plan",
        has_process: false,
        tenant,
        institution: InstitutionId(p.institution_id),
        id: p.id,
        version: p.version,
        from: &p.status,
        to,
    }
}

/// The sitting's current plan (draft / published), else its latest (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/exam-sittings/{id}/seating", operation_id = "exam_seating_get_for_sitting", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamSeatingPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn seating_for_sitting(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    let tenant = ctx.scope.tenant_id;
    load_sitting(&st.pool, tenant, id).await?;
    let plan: Uuid = sqlx::query_scalar(
        "SELECT id FROM exams.seating_plan WHERE tenant_id = $1 AND sitting_id = $2 \
         ORDER BY (status IN ('draft', 'published')) DESC, plan_no DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "seating plan",
    })?;
    respond(&st, tenant, plan, StatusCode::OK).await
}

/// Generate a draft seating plan for a sitting (`exam.setup.manage`); an existing draft is
/// cancelled and replaced, a published plan must be revised instead (`409`).
#[utoipa::path(post, path = "/api/v1/exams/exam-sittings/{id}/seating", operation_id = "exam_seating_generate", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ExamSeatingPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn seating_generate(
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
    if x.status != "planned" {
        return Err(KernelError::Conflict(format!("the sitting is {}", x.status)).into());
    }
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let live: Option<PlanRow> = sqlx::query_as::<_, PlanRow>(&format!(
        "SELECT {PLAN_COLUMNS} FROM exams.seating_plan WHERE tenant_id = $1 AND sitting_id = $2 \
         AND status IN ('draft', 'published') FOR UPDATE"
    ))
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(p) = live {
        if p.status == "published" {
            return Err(KernelError::Conflict(
                "the sitting has a published seating plan; revise it instead".into(),
            )
            .into());
        }
        move_status(
            &mut tx,
            plan_move(&p, tenant, "cancelled"),
            &StatusChange {
                cause_event_type: "exams.seating_plan.regenerate",
                reason: Some("replaced by a regenerated draft"),
                process_instance_id: None,
            },
            &act,
        )
        .await?;
    }
    let plan = build_plan(&mut tx, &st, tenant, &x, None).await?;
    audit(
        &mut tx,
        &scope,
        "exams.seating_plan.generate",
        "seating_plan",
        plan.to_string(),
        json!({"sittingId": id}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, plan, StatusCode::CREATED).await
}

/// One seating plan (`exam.read`).
#[utoipa::path(get, path = "/api/v1/exams/seating-plans/{id}", operation_id = "exam_seating_get", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ExamSeatingPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn seating_get(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamRead)?;
    respond(&st, ctx.scope.tenant_id, id, StatusCode::OK).await
}

async fn begin(
    st: &ExamsState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
    headers: &HeaderMap,
    to: SeatingPlanStatus,
) -> Result<Result<(PlanRow, sutra_kernel::scope::ExecutionScope), Response>, KernelError> {
    let expected = match if_match(headers)? {
        Ok(v) => v,
        Err(resp) => return Ok(Err(resp)),
    };
    let p = load_plan(&st.pool, ctx.scope.tenant_id, id).await?;
    let scope = institution_scope(ctx, InstitutionId(p.institution_id));
    require_institution_writable(st, &scope, InstitutionId(p.institution_id)).await?;
    crate::setup::common::check_version(expected, p.version)?;
    let from = SeatingPlanStatus::parse(&p.status)
        .ok_or_else(|| KernelError::internal_msg("bad seating plan status"))?;
    if !seating_can(from, to) {
        return Err(illegal("the seating plan", from.as_str(), to.as_str()));
    }
    Ok(Ok((p, scope)))
}

/// Publish a draft seating plan (`exam.setup.manage`).
#[utoipa::path(post, path = "/api/v1/exams/seating-plans/{id}/publish", operation_id = "exam_seating_publish", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSeatingPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn seating_publish(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let (p, scope) = match begin(&st, ctx, id, &headers, SeatingPlanStatus::Published).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let reason = reason_of(body);
    let tenant = ctx.scope.tenant_id;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let v = move_status(
        &mut tx,
        plan_move(&p, tenant, "published"),
        &StatusChange {
            cause_event_type: SEATING_PLAN_PUBLISHED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    sqlx::query("UPDATE exams.seating_plan SET published_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let x = load_sitting(&mut *tx, tenant, p.sitting_id).await?;
    let mut e = status_event(
        id,
        InstitutionId(p.institution_id),
        Some("draft"),
        "published",
        v,
    );
    e.session_id = Some(x.session_id);
    e.paper_id = Some(x.paper_id);
    e.reason = Some(format!("{} — {} seat(s)", x.paper_code, p.candidates));
    emit(
        &mut tx,
        &scope,
        (SEATING_PLAN_PUBLISHED, SEATING_PLAN_PUBLISHED_SCHEMA),
        &e,
        &act,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.seating_plan.publish",
        "seating_plan",
        id.to_string(),
        json!({}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, id, StatusCode::OK).await
}

/// Revise a published seating plan (`exam.setup.manage`, reason required, L3-I-059): the plan
/// becomes `revised` and a regenerated draft replaces it (returned).
#[utoipa::path(post, path = "/api/v1/exams/seating-plans/{id}/revise", operation_id = "exam_seating_revise", tag = "exam-scheduling",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the plan")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ExamSeatingPlanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn seating_revise(
    State(st): State<ExamsState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ExamSetupManage)?;
    let reason = reason_of(body);
    require_reason(&reason)?;
    let (p, scope) = match begin(&st, ctx, id, &headers, SeatingPlanStatus::Revised).await? {
        Ok(v) => v,
        Err(resp) => return Ok(resp),
    };
    let tenant = ctx.scope.tenant_id;
    let x = load_sitting(&st.pool, tenant, p.sitting_id).await?;
    let act = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    move_status(
        &mut tx,
        plan_move(&p, tenant, "revised"),
        &StatusChange {
            cause_event_type: "exams.seating_plan.revise",
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &act,
    )
    .await?;
    let next = build_plan(&mut tx, &st, tenant, &x, Some(id)).await?;
    audit(
        &mut tx,
        &scope,
        "exams.seating_plan.revise",
        "seating_plan",
        id.to_string(),
        json!({"reason": reason, "nextPlanId": next}),
        &act,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    respond(&st, tenant, next, StatusCode::OK).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seats_are_deterministic_and_capacity_checked() {
        let (a, b) = (Uuid::nil(), Uuid::from_u128(1));
        let venues = vec![(a, "HALL-A".to_owned(), 2), (b, "HALL-B".to_owned(), 3)];
        let seats = assign_seats(3, &venues).unwrap_or_default();
        assert_eq!(seats.len(), 3);
        assert_eq!(seats[0].1, "HALL-A-001");
        assert_eq!(seats[2], (b, "HALL-B-001".to_owned()));
        assert_eq!(assign_seats(6, &venues), Err(5));
        assert!(assign_seats(0, &venues).unwrap_or_default().is_empty());
    }
}
