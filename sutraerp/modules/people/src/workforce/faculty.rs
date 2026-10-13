//! Faculty workforce (L2-E03): establishment as active faculty (L3-E-026), teaching eligibility
//! lifecycle (L3-E-029), faculty suspension / reactivation (L3-E-031). Suspension suspends the
//! engagement too, so Domain D (`StaffDirectory`) stops treating the member as active.

use super::common::{
    self, check_version, clean, emit, expected, history, invalid, reason_of, required_reason,
    staff_or_404, status_payload, ReasonBody,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{change_engagement_status, StatusChange};
use crate::shared::rows::{current_engagement, load_staff, lock_engagement, lock_staff};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, PeopleState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::people::events::workforce as ev;
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::status::{
    EngagementStatus, FacultyEligibilityStatus, StaffCategory, StaffStatus,
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

/// Faculty profile of a teaching staff member.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyProfileView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub staff_id: Uuid,
    pub staff_number: String,
    pub display_name: String,
    pub engagement_id: Uuid,
    pub disciplines: Option<String>,
    pub highest_qualification: Option<String>,
    /// `active` | `suspended`.
    pub status: String,
    pub status_reason: Option<String>,
    pub eligibility_status: Option<FacultyEligibilityStatus>,
    pub eligibility_valid_until: Option<NaiveDate>,
    pub eligibility_reason: Option<String>,
    pub version: i64,
}

const F_SELECT: &str =
    "SELECT f.id, f.institution_id, f.staff_id, s.staff_number, s.display_name, \
    f.engagement_id, f.disciplines, f.highest_qualification, f.status, f.status_reason, \
    f.eligibility_status, f.eligibility_valid_until, f.eligibility_reason, f.version \
    FROM people.faculty_profile f JOIN people.staff_member s ON s.id = f.staff_id";

fn from_row(r: &PgRow) -> Result<FacultyProfileView, sqlx::Error> {
    Ok(FacultyProfileView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        staff_id: r.try_get("staff_id")?,
        staff_number: r.try_get("staff_number")?,
        display_name: r.try_get("display_name")?,
        engagement_id: r.try_get("engagement_id")?,
        disciplines: r.try_get("disciplines")?,
        highest_qualification: r.try_get("highest_qualification")?,
        status: r.try_get("status")?,
        status_reason: r.try_get("status_reason")?,
        eligibility_status: r
            .try_get::<Option<String>, _>("eligibility_status")?
            .map(|s| crate::shared::rows::decode(s, FacultyEligibilityStatus::parse))
            .transpose()?,
        eligibility_valid_until: r.try_get("eligibility_valid_until")?,
        eligibility_reason: r.try_get("eligibility_reason")?,
        version: r.try_get("version")?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    where_sql: &str,
    id: Uuid,
    lock: bool,
) -> Result<Option<FacultyProfileView>, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF f" } else { "" };
    sqlx::query(&format!(
        "{F_SELECT} WHERE {where_sql} = $1 AND f.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .as_ref()
    .map(from_row)
    .transpose()
    .map_err(map_db_error)
}

async fn by_id(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<FacultyProfileView, KernelError> {
    load(conn, tenant, "f.id", id, lock)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "faculty profile",
        })
}

/// A faculty profile by id (404).
pub async fn load_by_id(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<FacultyProfileView, KernelError> {
    by_id(conn, tenant, id, false).await
}

/// Eligibility commands (L3-E-029).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EligibilityAction {
    /// none / rejected / expired → requested
    Request,
    /// requested → under_review
    Review,
    /// under_review → approved (`people.approve`)
    Approve,
    /// requested / under_review → rejected (`people.approve`)
    Reject,
    /// approved → suspended
    Suspend,
    /// suspended → approved
    Reinstate,
    /// approved / suspended → expired
    Expire,
}

/// The eligibility status an action leads to (`None` = illegal from `from`).
pub fn eligibility_next(
    from: Option<FacultyEligibilityStatus>,
    action: EligibilityAction,
) -> Option<FacultyEligibilityStatus> {
    use EligibilityAction as A;
    use FacultyEligibilityStatus as S;
    match (from, action) {
        (None | Some(S::Rejected) | Some(S::Expired), A::Request) => Some(S::Requested),
        (Some(S::Requested), A::Review) => Some(S::UnderReview),
        (Some(S::UnderReview), A::Approve) => Some(S::Approved),
        (Some(S::Requested | S::UnderReview), A::Reject) => Some(S::Rejected),
        (Some(S::Approved), A::Suspend) => Some(S::Suspended),
        (Some(S::Suspended), A::Reinstate) => Some(S::Approved),
        (Some(S::Approved | S::Suspended), A::Expire) => Some(S::Expired),
        _ => None,
    }
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyCreate {
    pub disciplines: Option<String>,
    pub highest_qualification: Option<String>,
}

#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EligibilityBody {
    pub action: EligibilityAction,
    /// Approval validity (approve only; after today).
    pub valid_until: Option<NaiveDate>,
    pub reason: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn faculty_event(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    f: &FacultyProfileView,
    from: Option<&str>,
    to: &str,
    version: i64,
    reason: Option<&str>,
    event: (&'static str, &'static str),
    action: &str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let staff = load_staff(conn, tenant, StaffMemberId(f.staff_id)).await?;
    emit(
        conn,
        scope,
        event.0,
        event.1,
        "faculty_profile",
        f.id,
        action,
        &status_payload(
            f.id,
            InstitutionId(f.institution_id),
            staff.as_ref(),
            None,
            from,
            to,
            version,
            reason,
            None,
            None,
        ),
        actor,
    )
    .await
}

/// Establish a teaching staff member with an active engagement as active faculty
/// (`people.staff.manage`; Idempotency-Key; L3-E-026). 409 for non-teaching staff, a
/// non-active engagement, or an existing profile.
#[utoipa::path(post, path = "/api/v1/people/staff/{id}/faculty-profile", operation_id = "people_faculty_establish", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = FacultyCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = FacultyProfileView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn establish(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<FacultyCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    require_idempotency_key(&headers)?;
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        staff_or_404(&mut conn, t, id).await?
    };
    let scope = institution_scope(ctx, pre.institution_id);
    require_institution_writable(&st, &scope, pre.institution_id).await?;
    let disciplines = common::optional_text("disciplines", b.disciplines, 300)?;
    let highest = common::optional_text("highestQualification", b.highest_qualification, 200)?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let staff = lock_staff(&mut tx, t, pre.id).await?;
    if staff.staff_category != StaffCategory::Teaching {
        return Err(KernelError::Conflict(format!(
            "staff record {} is {}; only teaching staff become faculty",
            staff.staff_number, staff.staff_category
        ))
        .into());
    }
    if staff.status != StaffStatus::Active {
        return Err(KernelError::Conflict(format!(
            "staff record {} is {}",
            staff.staff_number, staff.status
        ))
        .into());
    }
    let e = current_engagement(&mut tx, t, staff.id)
        .await?
        .filter(|e| e.status == EngagementStatus::Active)
        .ok_or_else(|| {
            KernelError::Conflict(format!(
                "staff record {} has no active engagement",
                staff.staff_number
            ))
        })?;
    if e.staff_category != StaffCategory::Teaching {
        return Err(KernelError::Conflict(format!(
            "engagement {} is not a teaching engagement",
            e.engagement_number
        ))
        .into());
    }
    if load(&mut tx, t, "f.staff_id", staff.id.as_uuid(), false)
        .await?
        .is_some()
    {
        return Err(KernelError::Conflict(format!(
            "staff record {} already has a faculty profile",
            staff.staff_number
        ))
        .into());
    }
    let fid: Uuid = sqlx::query_scalar(
        "INSERT INTO people.faculty_profile (tenant_id, institution_id, staff_id, engagement_id, \
         disciplines, highest_qualification) VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(staff.institution_id.as_uuid())
    .bind(staff.id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(&disciplines)
    .bind(&highest)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        staff.institution_id,
        "faculty_profile",
        fid,
        None,
        "active",
        ev::FACULTY_ESTABLISHED,
        None,
        None,
        &actor,
    )
    .await?;
    let f = by_id(&mut tx, t, fid, false).await?;
    faculty_event(
        &mut tx,
        t,
        &scope,
        &f,
        None,
        "active",
        f.version,
        None,
        (ev::FACULTY_ESTABLISHED, ev::FACULTY_ESTABLISHED_SCHEMA),
        "people.faculty_profile.establish",
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &f, Version(f.version)))
}

/// The faculty profile of a staff record (`people.read`; 404 when none).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}/faculty-profile", operation_id = "people_faculty_get_by_staff", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FacultyProfileView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn of_staff(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = load(&mut conn, ctx.scope.tenant_id, "f.staff_id", id, false)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "faculty profile",
        })?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

/// One faculty profile (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/faculty-profiles/{id}", operation_id = "people_faculty_get", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = FacultyProfileView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = by_id(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &f, Version(f.version)))
}

async fn faculty_scope(
    st: &PeopleState,
    ctx: &sutra_kernel::auth::AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let f = by_id(&mut conn, ctx.scope.tenant_id, id, false).await?;
    let institution = InstitutionId(f.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    Ok(scope)
}

/// Move the teaching eligibility (`people.staff.manage`; approve / reject need `people.approve`;
/// If-Match; L3-E-029). Illegal moves are 409.
#[utoipa::path(post, path = "/api/v1/people/faculty-profiles/{id}/eligibility", operation_id = "people_faculty_eligibility", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the faculty profile")),
    request_body = EligibilityBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FacultyProfileView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn eligibility(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<EligibilityBody>,
) -> ApiResult<Response> {
    let ctx = match b.action {
        EligibilityAction::Approve | EligibilityAction::Reject => {
            auth.require(Permission::PeopleApprove)?
        }
        _ => auth.require(Permission::StaffManage)?,
    };
    let v = expected!(headers);
    let reason = clean(b.reason);
    if matches!(
        b.action,
        EligibilityAction::Reject | EligibilityAction::Suspend
    ) && reason.is_none()
    {
        return Err(invalid("reason", "required", "a reason is required").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = faculty_scope(&st, ctx, id).await?;
    let today = business_today(&st, &scope, scope.require_institution()?).await?;
    if b.valid_until.is_some_and(|d| d <= today) {
        return Err(invalid("validUntil", "not_future", "must be after today").into());
    }
    if b.valid_until.is_some() && b.action != EligibilityAction::Approve {
        return Err(invalid(
            "validUntil",
            "not_applicable",
            "only an approval carries a validity",
        )
        .into());
    }
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = by_id(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    let to = eligibility_next(f.eligibility_status, b.action).ok_or_else(|| {
        common::illegal(
            "teaching eligibility of",
            &f.staff_number,
            f.eligibility_status.map(|s| s.as_str()).unwrap_or("none"),
            &format!("{:?}", b.action).to_lowercase(),
        )
    })?;
    if to == FacultyEligibilityStatus::Approved && f.status != "active" {
        return Err(KernelError::Conflict(format!(
            "faculty {} is {}; reactivate before approving eligibility",
            f.staff_number, f.status
        ))
        .into());
    }
    let valid_until = if b.action == EligibilityAction::Approve {
        b.valid_until
    } else {
        f.eligibility_valid_until
    };
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.faculty_profile SET eligibility_status = $2, eligibility_valid_until = $3, \
         eligibility_reason = $4, version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(valid_until)
    .bind(reason.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let from = f.eligibility_status.map(|s| s.as_str());
    history(
        &mut tx,
        t,
        InstitutionId(f.institution_id),
        "faculty_eligibility",
        id,
        from,
        to.as_str(),
        ev::FACULTY_ELIGIBILITY_CHANGED,
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    faculty_event(
        &mut tx,
        t,
        &scope,
        &f,
        from,
        to.as_str(),
        version,
        reason.as_deref(),
        (
            ev::FACULTY_ELIGIBILITY_CHANGED,
            ev::FACULTY_ELIGIBILITY_CHANGED_SCHEMA,
        ),
        "people.faculty_profile.eligibility",
        &actor,
    )
    .await?;
    let out = by_id(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

async fn suspend_or_reactivate(
    st: &PeopleState,
    auth: &Authenticated,
    id: Uuid,
    headers: &HeaderMap,
    reason: Option<String>,
    suspend: bool,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::StaffManage)?;
    let v = match crate::shared::http::if_match(headers)? {
        Ok(v) => v.0,
        Err(resp) => return Ok(resp),
    };
    let reason = if suspend {
        Some(required_reason(reason)?)
    } else {
        reason
    };
    let t = ctx.scope.tenant_id;
    let scope = faculty_scope(st, ctx, id).await?;
    let today = business_today(st, &scope, scope.require_institution()?).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let f = by_id(&mut tx, t, id, true).await?;
    check_version(v, f.version)?;
    let (from, to) = if suspend {
        ("active", "suspended")
    } else {
        ("suspended", "active")
    };
    if f.status != from {
        return Err(common::illegal("faculty profile of", &f.staff_number, &f.status, to).into());
    }
    // Engagement follows (Domain D sees `StaffDirectory.active` = engagement active).
    let e = lock_engagement(&mut tx, t, StaffEngagementId(f.engagement_id)).await?;
    let (e_from, e_to, e_event) = if suspend {
        (
            EngagementStatus::Active,
            EngagementStatus::Suspended,
            (ev::ENGAGEMENT_SUSPENDED, ev::ENGAGEMENT_SUSPENDED_SCHEMA),
        )
    } else {
        (
            EngagementStatus::Suspended,
            EngagementStatus::Active,
            (ev::ENGAGEMENT_RESTORED, ev::ENGAGEMENT_RESTORED_SCHEMA),
        )
    };
    if e.status == e_from {
        let moved = change_engagement_status(
            &mut tx,
            t,
            &e,
            e_to,
            &StatusChange {
                cause_event_type: e_event.0,
                reason: reason.as_deref(),
                process_instance_id: None,
            },
            &actor,
        )
        .await?;
        let staff = load_staff(&mut tx, t, e.staff_id).await?;
        emit(
            &mut tx,
            &scope,
            e_event.0,
            e_event.1,
            "staff_engagement",
            e.id.as_uuid(),
            if suspend {
                "people.staff_engagement.suspend"
            } else {
                "people.staff_engagement.restore"
            },
            &status_payload(
                e.id.as_uuid(),
                e.institution_id,
                staff.as_ref(),
                Some(&moved),
                Some(e.status.as_str()),
                moved.status.as_str(),
                moved.version,
                reason.as_deref(),
                None,
                None,
            ),
            &actor,
        )
        .await?;
    } else if !suspend {
        return Err(KernelError::Conflict(format!(
            "engagement {} is {}; it cannot be restored with the faculty profile",
            e.engagement_number, e.status
        ))
        .into());
    }
    let eligibility = match (suspend, f.eligibility_status) {
        (true, Some(FacultyEligibilityStatus::Approved)) => {
            Some(FacultyEligibilityStatus::Suspended)
        }
        (false, Some(FacultyEligibilityStatus::Suspended)) => {
            if f.eligibility_valid_until.is_some_and(|d| d < today) {
                Some(FacultyEligibilityStatus::Expired)
            } else {
                Some(FacultyEligibilityStatus::Approved)
            }
        }
        (_, other) => other,
    };
    let version: i64 = sqlx::query_scalar(
        "UPDATE people.faculty_profile SET status = $2, status_reason = $3, eligibility_status = $4, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(id)
    .bind(to)
    .bind(reason.as_deref())
    .bind(eligibility.map(|s| s.as_str()))
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        InstitutionId(f.institution_id),
        "faculty_profile",
        id,
        Some(from),
        to,
        ev::FACULTY_ELIGIBILITY_CHANGED,
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    faculty_event(
        &mut tx,
        t,
        &scope,
        &f,
        Some(from),
        to,
        version,
        reason.as_deref(),
        (
            ev::FACULTY_ELIGIBILITY_CHANGED,
            ev::FACULTY_ELIGIBILITY_CHANGED_SCHEMA,
        ),
        if suspend {
            "people.faculty_profile.suspend"
        } else {
            "people.faculty_profile.reactivate"
        },
        &actor,
    )
    .await?;
    let out = by_id(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

/// Suspend a faculty member (`people.staff.manage`; If-Match; reason required; L3-E-031): the
/// engagement is suspended and approved eligibility is suspended with it.
#[utoipa::path(post, path = "/api/v1/people/faculty-profiles/{id}/suspend", operation_id = "people_faculty_suspend", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the faculty profile")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FacultyProfileView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn suspend(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    suspend_or_reactivate(&st, &auth, id, &headers, reason_of(body), true).await
}

/// Reactivate a suspended faculty member (`people.staff.manage`; If-Match; L3-E-031): the
/// engagement is restored and suspended eligibility returns (or expires when its validity passed).
#[utoipa::path(post, path = "/api/v1/people/faculty-profiles/{id}/reactivate", operation_id = "people_faculty_reactivate", tag = "people-workforce",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the faculty profile")),
    request_body(content = ReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = FacultyProfileView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reactivate(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReasonBody>>,
) -> ApiResult<Response> {
    suspend_or_reactivate(&st, &auth, id, &headers, reason_of(body), false).await
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(establish, of_staff))
        .routes(routes!(get))
        .routes(routes!(eligibility))
        .routes(routes!(suspend))
        .routes(routes!(reactivate))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use EligibilityAction as A;
    use FacultyEligibilityStatus as S;

    #[test]
    fn eligibility_lifecycle() {
        assert_eq!(eligibility_next(None, A::Request), Some(S::Requested));
        assert_eq!(
            eligibility_next(Some(S::Requested), A::Review),
            Some(S::UnderReview)
        );
        assert_eq!(
            eligibility_next(Some(S::UnderReview), A::Approve),
            Some(S::Approved)
        );
        assert_eq!(
            eligibility_next(Some(S::Approved), A::Suspend),
            Some(S::Suspended)
        );
        assert_eq!(
            eligibility_next(Some(S::Suspended), A::Reinstate),
            Some(S::Approved)
        );
        assert_eq!(
            eligibility_next(Some(S::Approved), A::Expire),
            Some(S::Expired)
        );
        assert_eq!(
            eligibility_next(Some(S::Expired), A::Request),
            Some(S::Requested)
        );
        assert_eq!(eligibility_next(None, A::Approve), None);
        assert_eq!(eligibility_next(Some(S::Requested), A::Approve), None);
        assert_eq!(eligibility_next(Some(S::Rejected), A::Reinstate), None);
    }
}
