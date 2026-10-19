//! Probation review → confirmation decision (L3-E-043; `PROBATION_DECISION`). Confirmation makes
//! the engagement permanent, extension moves the probation end date, termination closes the
//! engagement. Each outcome is synchronised into the profile.

use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::rows::{decode, load_engagement, load_staff, lock_engagement};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use crate::workforce::common::{
    self, check_version, clean, emit, expected, history, invalid, modified, optional_text,
    status_payload, Decision, DecisionBody,
};
use crate::workforce::engagement::{close_engagement, sync_profile, write_attrs, Attrs};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::StaffEngagementId;
use sutra_contracts::people::status::{
    EmploymentType, EngagementStatus, HrCaseStatus, ProbationOutcome,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A probation review.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub engagement_id: Uuid,
    pub engagement_number: String,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub review_date: NaiveDate,
    pub assessment: Option<String>,
    pub outcome: ProbationOutcome,
    pub extended_until: Option<NaiveDate>,
    pub previous_probation_end: Option<NaiveDate>,
    pub status: HrCaseStatus,
    pub status_reason: Option<String>,
    pub decided_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ProbationList {
    pub items: Vec<ProbationView>,
}

const SELECT: &str = "SELECT p.id, p.institution_id, p.engagement_id, e.engagement_number, \
    p.staff_id, s.staff_number, s.display_name, p.review_date, p.assessment, p.outcome, \
    p.extended_until, p.previous_probation_end, p.status, p.status_reason, p.decided_by, \
    p.process_instance_id, p.version FROM people.probation_review p \
    JOIN people.staff_engagement e ON e.id = p.engagement_id \
    JOIN people.staff_member s ON s.id = p.staff_id";

fn from_row(r: &PgRow) -> Result<ProbationView, sqlx::Error> {
    Ok(ProbationView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        engagement_id: r.try_get("engagement_id")?,
        engagement_number: r.try_get("engagement_number")?,
        staff_id: r.try_get("staff_id")?,
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        review_date: r.try_get("review_date")?,
        assessment: r.try_get("assessment")?,
        outcome: decode(r.try_get("outcome")?, ProbationOutcome::parse)?,
        extended_until: r.try_get("extended_until")?,
        previous_probation_end: r.try_get("previous_probation_end")?,
        status: decode(r.try_get("status")?, HrCaseStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        decided_by: r.try_get("decided_by")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<ProbationView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF p" } else { "" };
    let row = sqlx::query(&format!(
        "{SELECT} WHERE p.id = $1 AND p.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "probation review",
    })?;
    from_row(&row).map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    p: &ProbationView,
    to: HrCaseStatus,
    reason: Option<&str>,
    decided_by: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE people.probation_review SET status = $2, status_reason = COALESCE($3, status_reason), \
         decided_by = COALESCE($4, decided_by), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(p.id)
    .bind(to.as_str())
    .bind(reason)
    .bind(decided_by)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(p.institution_id),
        "probation_review",
        p.id,
        Some(p.status.as_str()),
        to.as_str(),
        ev::PROBATION_DECIDED,
        reason,
        p.process_instance_id.as_deref(),
        actor,
    )
    .await
}

/// Applies an approved review to the engagement and completes it.
async fn apply(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    p: &ProbationView,
    decided_by: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    set_status(
        conn,
        tenant,
        p,
        HrCaseStatus::Approved,
        None,
        Some(decided_by),
        actor,
    )
    .await?;
    let mut approved = p.clone();
    approved.status = HrCaseStatus::Approved;
    let e = lock_engagement(conn, tenant, StaffEngagementId(p.engagement_id)).await?;
    match p.outcome {
        ProbationOutcome::Terminated => {
            let lwd = p.review_date.max(e.start_date);
            close_engagement(conn, tenant, scope, &e, lwd, "probation terminated", actor).await?;
        }
        ProbationOutcome::Confirmed | ProbationOutcome::Extended => {
            let mut a = Attrs::of(&e);
            if p.outcome == ProbationOutcome::Confirmed {
                a.employment_type = EmploymentType::Permanent;
            } else {
                a.probation_end_date = p.extended_until;
            }
            let changed = Attrs::of(&e).changed(&a);
            let updated =
                write_attrs(conn, tenant, &e, &a, p.process_instance_id.as_deref()).await?;
            emit(
                conn,
                scope,
                ev::ENGAGEMENT_MODIFIED,
                ev::ENGAGEMENT_MODIFIED_SCHEMA,
                "staff_engagement",
                e.id.as_uuid(),
                "people.staff_engagement.probation",
                &modified(e.id.as_uuid(), e.institution_id, updated.version, &changed),
                actor,
            )
            .await?;
            sync_profile(conn, tenant, scope, &e, &updated, "probation", actor).await?;
        }
    }
    set_status(
        conn,
        tenant,
        &approved,
        HrCaseStatus::Completed,
        Some(p.outcome.as_str()),
        None,
        actor,
    )
    .await?;
    let done = load(conn, tenant, p.id, false).await?;
    let staff = load_staff(conn, tenant, e.staff_id).await?;
    let e_now = load_engagement(conn, tenant, e.id).await?;
    emit(
        conn,
        scope,
        ev::PROBATION_DECIDED,
        ev::PROBATION_DECIDED_SCHEMA,
        "probation_review",
        p.id,
        "people.probation_review.decide",
        &status_payload(
            p.id,
            InstitutionId(p.institution_id),
            staff.as_ref(),
            e_now.as_ref(),
            Some(p.status.as_str()),
            done.status.as_str(),
            done.version,
            Some(p.outcome.as_str()),
            p.process_instance_id.as_deref(),
            None,
        ),
        actor,
    )
    .await
}

/// A probation review recommendation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbationCreate {
    /// Defaults to today (business date).
    pub review_date: Option<NaiveDate>,
    pub assessment: Option<String>,
    pub outcome: ProbationOutcome,
    /// New probation end (extension only; after the current one).
    pub extended_until: Option<NaiveDate>,
}

/// Review the probation of an active engagement (`people.staff.manage`; Idempotency-Key;
/// `PROBATION_DECISION`). 409 unless the engagement is on probation; one open review at a time.
#[utoipa::path(post, path = "/api/v1/people/engagements/{id}/probation-reviews", operation_id = "people_probation_create", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = ProbationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = ProbationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ProbationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_engagement(&mut conn, t, StaffEngagementId(id))
            .await?
            .ok_or(KernelError::NotFound {
                entity: "engagement",
            })?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    let assessment = optional_text("assessment", b.assessment, 4000)?;
    match (b.outcome, b.extended_until) {
        (ProbationOutcome::Extended, None) => {
            return Err(invalid(
                "extendedUntil",
                "required",
                "an extension needs the new probation end",
            )
            .into())
        }
        (ProbationOutcome::Extended, Some(d))
            if pre.probation_end_date.is_some_and(|c| d <= c) || d <= pre.start_date =>
        {
            return Err(invalid(
                "extendedUntil",
                "not_later",
                "must be after the current probation end",
            )
            .into())
        }
        (ProbationOutcome::Confirmed | ProbationOutcome::Terminated, Some(_)) => {
            return Err(invalid(
                "extendedUntil",
                "not_applicable",
                "only an extension carries a new end",
            )
            .into())
        }
        _ => {}
    }
    let today = business_today(&st, &scope, pre.institution_id).await?;
    let review_date = b.review_date.unwrap_or(today);
    if review_date < pre.start_date {
        return Err(invalid(
            "reviewDate",
            "before_start",
            "must not be before the engagement start",
        )
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert(
        "engagementNumber".to_owned(),
        serde_json::json!(pre.engagement_number),
    );
    vars.insert("outcome".to_owned(), serde_json::json!(b.outcome.as_str()));
    let outcome = request_approval(
        &st,
        &scope,
        process::PROBATION_DECISION,
        business_key("probation_review", pre.id, pre.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = lock_engagement(&mut tx, t, pre.id).await?;
    if e.status != EngagementStatus::Active || e.employment_type != EmploymentType::Probation {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {} / {}; only an active probation engagement is reviewed",
            e.engagement_number, e.status, e.employment_type
        ))
        .into());
    }
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.probation_review WHERE engagement_id = $1 \
         AND status IN ('initiated', 'pending_approval', 'approved', 'processing'))",
    )
    .bind(e.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open {
        return Err(KernelError::Conflict(format!(
            "engagement {} already has an open probation review",
            e.engagement_number
        ))
        .into());
    }
    let pid = outcome.process_instance_id();
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.probation_review (tenant_id, institution_id, engagement_id, staff_id, \
         review_date, assessment, outcome, extended_until, previous_probation_end, status, \
         process_instance_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 'pending_approval', $10) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(e.institution_id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(e.staff_id.as_uuid())
    .bind(review_date)
    .bind(&assessment)
    .bind(b.outcome.as_str())
    .bind(b.extended_until)
    .bind(e.probation_end_date)
    .bind(pid)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        e.institution_id,
        "probation_review",
        rid,
        None,
        "pending_approval",
        "sutraerp.people.probation_review.submitted",
        Some(b.outcome.as_str()),
        Some(pid),
        &actor,
    )
    .await?;
    common::audit_only(
        &mut tx,
        &scope,
        "probation_review",
        rid,
        "people.probation_review.submit",
        serde_json::json!({ "outcome": b.outcome, "processInstanceId": pid }),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let p = load(&mut tx, t, rid, true).await?;
        apply(
            &mut tx,
            t,
            &scope,
            &p,
            crate::shared::approval::AUTO_APPROVER,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let out = load(&mut tx, t, rid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &out, Version(out.version)))
}

/// Probation reviews of an engagement (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/engagements/{id}/probation-reviews", operation_id = "people_probation_list", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ProbationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<ProbationList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let rows = sqlx::query(&format!(
        "{SELECT} WHERE p.engagement_id = $1 AND p.tenant_id = $2 ORDER BY p.created_at DESC"
    ))
    .bind(id)
    .bind(ctx.scope.tenant_id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(ProbationList { items }))
}

/// One probation review (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/probation-reviews/{id}", operation_id = "people_probation_get", tag = "people-hr",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = ProbationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let p = load(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &p, Version(p.version)))
}

/// Decide a pending probation review (`people.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/people/probation-reviews/{id}/decision", operation_id = "people_probation_decide", tag = "people-hr",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the review")),
    request_body = DecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ProbationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<DecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "probation review").await?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == Decision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    let scope = institution_scope(ctx, InstitutionId(pre.institution_id));
    require_institution_writable(&st, &scope, InstitutionId(pre.institution_id)).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let p = load(&mut tx, t, id, true).await?;
    check_version(v, p.version)?;
    if p.status != HrCaseStatus::PendingApproval {
        return Err(common::illegal(
            "probation review of",
            &p.staff_number,
            p.status.as_str(),
            "decided",
        )
        .into());
    }
    let decider = actor.id.clone().unwrap_or_default();
    if b.decision == Decision::Approve {
        apply(&mut tx, t, &scope, &p, &decider, &actor).await?;
    } else {
        set_status(
            &mut tx,
            t,
            &p,
            HrCaseStatus::Rejected,
            reason.as_deref(),
            Some(decider.as_str()),
            &actor,
        )
        .await?;
        common::audit_only(
            &mut tx,
            &scope,
            "probation_review",
            id,
            "people.probation_review.reject",
            serde_json::json!({ "reason": reason }),
            &actor,
        )
        .await?;
    }
    let out = load(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get))
        .routes(routes!(decide))
        .with_state(state)
}
