//! Library memberships (L3-G-124 request → active, G-125 suspension, G-126 closure) and the
//! borrowing restriction check (L3-G-152: a validation condition over membership, limit,
//! outstanding dues and administrative restriction — not a lifecycle of its own).

use super::common::{
    check_version, clean, expected, invalid, optional_text, required_reason, transition,
    LibraryReasonBody, Transition,
};
use super::domain::LibraryMembershipStatus;
use super::fines::{sync_billed_fines, unsettled_total};
use super::policy::active_policy;
use crate::shared::decimal::fmt2;
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::illegal;
use crate::shared::me::{require_person, PersonRef};
use crate::shared::numbering::{self, kind};
use crate::shared::publish;
use crate::shared::rows::decode;
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, CampusState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::campus::events::library as ev;
use sutra_contracts::campus::status::PersonKind;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// A library membership.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryMembershipView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "LM-000001")]
    pub number: String,
    pub person_kind: PersonKind,
    pub person_id: Uuid,
    pub person_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub status: LibraryMembershipStatus,
    pub status_reason: Option<String>,
    /// Administrative borrowing restriction (L3-G-152), when one applies.
    pub restriction_reason: Option<String>,
    pub valid_until: Option<NaiveDate>,
    /// Loans still out (active or overdue).
    pub active_loans: i64,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct LibraryMembershipList {
    pub items: Vec<LibraryMembershipView>,
}

pub const SELECT: &str = "SELECT m.id, m.institution_id, m.number, m.person_kind, m.person_id, \
    m.person_number, m.display_name, m.subject_ref, m.status, m.status_reason, m.restriction_reason, \
    m.valid_until, m.version, (SELECT count(*) FROM campus.library_loan l WHERE l.membership_id = m.id \
    AND l.status IN ('active', 'overdue')) AS active_loans FROM campus.library_membership m";

pub fn from_row(r: &PgRow) -> Result<LibraryMembershipView, sqlx::Error> {
    Ok(LibraryMembershipView {
        id: r.try_get("id")?,
        institution_id: r.try_get("institution_id")?,
        number: r.try_get("number")?,
        person_kind: decode(r.try_get("person_kind")?, PersonKind::parse)?,
        person_id: r.try_get("person_id")?,
        person_number: r.try_get("person_number")?,
        display_name: r.try_get("display_name")?,
        subject_ref: r.try_get("subject_ref")?,
        status: decode(r.try_get("status")?, LibraryMembershipStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        restriction_reason: r.try_get("restriction_reason")?,
        valid_until: r.try_get("valid_until")?,
        active_loans: r.try_get("active_loans")?,
        version: r.try_get("version")?,
    })
}

/// Loads one membership of the tenant (404), optionally locked.
pub async fn load_membership(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<LibraryMembershipView, KernelError> {
    if lock {
        sqlx::query(
            "SELECT id FROM campus.library_membership WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let sql = format!("{SELECT} WHERE m.id = $1 AND m.tenant_id = $2");
    let row = sqlx::query(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "library membership",
        })?;
    from_row(&row).map_err(map_db_error)
}

/// The live (not closed) membership of a person in an institution, if any.
pub async fn membership_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: Option<InstitutionId>,
    person: &PersonRef,
) -> Result<Option<LibraryMembershipView>, KernelError> {
    let sql = format!(
        "{SELECT} WHERE m.tenant_id = $1 AND ($2::uuid IS NULL OR m.institution_id = $2) \
         AND m.person_kind = $3 AND m.person_id = $4 AND m.status <> 'closed' \
         ORDER BY m.created_at DESC LIMIT 1"
    );
    let row = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution.map(|i| i.as_uuid()))
        .bind(person.kind.as_str())
        .bind(person.id)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
    row.as_ref().map(from_row).transpose().map_err(map_db_error)
}

/// Why a member cannot borrow right now (L3-G-152): empty = eligible.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEligibilityView {
    pub membership_id: Uuid,
    pub eligible: bool,
    pub blockers: Vec<String>,
    pub active_loans: i64,
    pub max_loans: Option<i32>,
    #[schema(value_type = String, example = "0.00")]
    pub unsettled_fines: String,
}

/// Borrowing check of a member (settlement of billed fines refreshed from Finance first).
pub async fn eligibility(
    st: &CampusState,
    scope: &ExecutionScope,
    m: &LibraryMembershipView,
) -> Result<LibraryEligibilityView, KernelError> {
    sync_billed_fines(st, scope, m.id).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let unsettled = unsettled_total(&mut conn, m.id).await?;
    let policy = active_policy(&mut conn, scope.tenant_id, InstitutionId(m.institution_id))
        .await
        .ok();
    let max_loans = policy.as_ref().map(|p| p.rules(m.person_kind).max_loans);
    let mut blockers = Vec::new();
    if m.status != LibraryMembershipStatus::Active {
        blockers.push(format!("membership {} is {}", m.number, m.status));
    }
    if let Some(r) = &m.restriction_reason {
        blockers.push(format!("administrative restriction: {r}"));
    }
    if unsettled > Decimal::ZERO {
        blockers.push(format!("unsettled library fines of {}", fmt2(unsettled)));
    }
    match max_loans {
        None => blockers.push("the library has no active borrowing policy".into()),
        Some(max) if m.active_loans >= i64::from(max) => {
            blockers.push(format!("borrowing limit of {max} loans reached"))
        }
        _ => {}
    }
    Ok(LibraryEligibilityView {
        membership_id: m.id,
        eligible: blockers.is_empty(),
        blockers,
        active_loans: m.active_loans,
        max_loans,
        unsettled_fines: fmt2(unsettled),
    })
}

/// A new membership (L3-G-124).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryMembershipCreate {
    pub person_kind: PersonKind,
    /// Domain C student id or Domain E staff id.
    pub person_id: Uuid,
    pub valid_until: Option<NaiveDate>,
}

/// Inserts an active membership for `person` (409 when the person already has a live one).
pub async fn insert_membership(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution: InstitutionId,
    person: &PersonRef,
    valid_until: Option<NaiveDate>,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let tenant = scope.tenant_id;
    let number = numbering::next(conn, tenant, institution, kind::LIBRARY_MEMBERSHIP).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO campus.library_membership (tenant_id, institution_id, number, person_kind, \
         person_id, person_number, display_name, subject_ref, status, valid_until) VALUES ($1, $2, \
         $3, $4, $5, $6, $7, $8, 'active', $9) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(person.kind.as_str())
    .bind(person.id)
    .bind(&person.number)
    .bind(&person.display_name)
    .bind(person.subject_ref.as_deref())
    .bind(valid_until)
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!(
            "{} {} already has a live library membership",
            person.kind, person.number
        )),
        other => other,
    })?;
    transition(
        conn,
        scope,
        &Transition {
            aggregate_type: "library_membership",
            id,
            institution,
            from: None,
            to: "active",
            version: 1,
            reason: None,
            process_instance_id: None,
            number: Some(&number),
            subject_ref: person.subject_ref.as_deref(),
        },
        Some((
            ev::LIBRARY_MEMBERSHIP_ACTIVATED,
            ev::LIBRARY_MEMBERSHIP_ACTIVATED_SCHEMA,
        )),
        "campus.library_membership.create",
        actor,
    )
    .await?;
    Ok(id)
}

/// Enrol a student or staff member as a library member (`campus.library.manage`;
/// Idempotency-Key). 422 for an unknown person, 409 when a live membership exists.
#[utoipa::path(post, path = "/api/v1/campus/institutions/{id}/library-memberships", operation_id = "campus_library_membership_create", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = LibraryMembershipCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryMembershipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryMembershipCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let person = require_person(&st, &scope, b.person_kind, b.person_id, "personId").await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mid =
        insert_membership(&mut tx, &scope, institution, &person, b.valid_until, &actor).await?;
    let m = load_membership(&mut tx, scope.tenant_id, mid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &m, Version(m.version)))
}

/// Filters of the membership list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LibraryMembershipQuery {
    pub status: Option<LibraryMembershipStatus>,
    /// Matches number, person number or name.
    pub q: Option<String>,
    pub limit: Option<i64>,
}

/// Memberships of an institution (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/library-memberships", operation_id = "campus_library_membership_list", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), LibraryMembershipQuery), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryMembershipList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<LibraryMembershipQuery>,
) -> ApiResult<Json<LibraryMembershipList>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let needle = clean(q.q).map(|s| format!("%{}%", s.to_lowercase()));
    let sql = format!(
        "{SELECT} WHERE m.tenant_id = $1 AND m.institution_id = $2 AND ($3::text IS NULL OR m.status = $3) \
         AND ($4::text IS NULL OR lower(m.number || ' ' || m.person_number || ' ' || m.display_name) LIKE $4) \
         ORDER BY m.created_at DESC LIMIT $5"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(q.status.map(|s| s.as_str()))
        .bind(needle)
        .bind(super::common::page_limit(q.limit))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(from_row)
        .collect::<Result<Vec<_>, _>>()
        .map_err(map_db_error)?;
    Ok(Json(LibraryMembershipList { items }))
}

/// One membership (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/library-memberships/{id}", operation_id = "campus_library_membership_get", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryMembershipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let m = load_membership(&mut conn, ctx.scope.tenant_id, id, false).await?;
    Ok(with_etag(StatusCode::OK, &m, Version(m.version)))
}

/// Borrowing eligibility of a member (`campus.read`; L3-G-152).
#[utoipa::path(get, path = "/api/v1/campus/library-memberships/{id}/eligibility", operation_id = "campus_library_membership_eligibility", tag = "campus-library",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = LibraryEligibilityView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn check(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<LibraryEligibilityView>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let m = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_membership(&mut conn, ctx.scope.tenant_id, id, false).await?
    };
    let scope = institution_scope(ctx, InstitutionId(m.institution_id));
    Ok(Json(eligibility(&st, &scope, &m).await?))
}

/// Moves a membership to `to` (illegal → 409).
async fn change(
    st: &CampusState,
    auth: &Authenticated,
    id: Uuid,
    expected_version: i64,
    to: LibraryMembershipStatus,
    reason: Option<String>,
    action: &'static str,
) -> Result<LibraryMembershipView, KernelError> {
    let ctx = auth.0.clone();
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_membership(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(&ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    if to == LibraryMembershipStatus::Closed {
        sync_billed_fines(st, &scope, id).await?;
    }
    let actor = actor(&ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = load_membership(&mut tx, t, id, true).await?;
    check_version(expected_version, m.version)?;
    if !m.status.can_transition(to) {
        return Err(illegal(
            "library membership",
            &m.number,
            m.status.as_str(),
            to.as_str(),
        ));
    }
    if to == LibraryMembershipStatus::Closed {
        let unsettled = unsettled_total(&mut tx, id).await?;
        if m.active_loans > 0 || unsettled > Decimal::ZERO {
            return Err(KernelError::Conflict(format!(
                "membership {} still has {} loan(s) out and {} in unsettled fines",
                m.number,
                m.active_loans,
                fmt2(unsettled)
            )));
        }
    }
    sqlx::query(
        "UPDATE campus.library_membership SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(to.as_str())
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let event = match to {
        LibraryMembershipStatus::Suspended => Some((
            ev::LIBRARY_MEMBERSHIP_SUSPENDED,
            ev::LIBRARY_MEMBERSHIP_SUSPENDED_SCHEMA,
        )),
        LibraryMembershipStatus::Active => Some((
            ev::LIBRARY_MEMBERSHIP_ACTIVATED,
            ev::LIBRARY_MEMBERSHIP_ACTIVATED_SCHEMA,
        )),
        _ => None,
    };
    transition(
        &mut tx,
        &scope,
        &Transition {
            aggregate_type: "library_membership",
            id,
            institution,
            from: Some(m.status.as_str()),
            to: to.as_str(),
            version: m.version + 1,
            reason: reason.as_deref(),
            process_instance_id: None,
            number: Some(&m.number),
            subject_ref: m.subject_ref.as_deref(),
        },
        event,
        action,
        &actor,
    )
    .await?;
    let out = load_membership(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Suspend an active membership (`campus.library.manage`; If-Match; reason required; L3-G-125).
#[utoipa::path(post, path = "/api/v1/campus/library-memberships/{id}/suspend", operation_id = "campus_library_membership_suspend", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryMembershipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn suspend(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    let m = change(
        &st,
        &auth,
        id,
        v,
        LibraryMembershipStatus::Suspended,
        Some(reason),
        "campus.library_membership.suspend",
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &m, Version(m.version)))
}

/// Restore a suspended membership (`campus.library.manage`; If-Match).
#[utoipa::path(post, path = "/api/v1/campus/library-memberships/{id}/restore", operation_id = "campus_library_membership_restore", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryMembershipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn restore(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let m = change(
        &st,
        &auth,
        id,
        v,
        LibraryMembershipStatus::Active,
        clean(b.reason),
        "campus.library_membership.restore",
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &m, Version(m.version)))
}

/// Close a membership (`campus.library.manage`; If-Match; L3-G-126): 409 while loans are out or
/// fines are unsettled.
#[utoipa::path(post, path = "/api/v1/campus/library-memberships/{id}/close", operation_id = "campus_library_membership_close", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryMembershipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn close(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let m = change(
        &st,
        &auth,
        id,
        v,
        LibraryMembershipStatus::Closed,
        clean(b.reason),
        "campus.library_membership.close",
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &m, Version(m.version)))
}

/// Set or lift an administrative borrowing restriction.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryRestrictionBody {
    /// `true` to restrict borrowing, `false` to resolve the restriction.
    pub restricted: bool,
    pub reason: Option<String>,
}

/// Restrict borrowing of a member or resolve the restriction (`campus.library.manage`;
/// If-Match; L3-G-152). A restriction needs a reason.
#[utoipa::path(post, path = "/api/v1/campus/library-memberships/{id}/restriction", operation_id = "campus_library_membership_restriction", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryRestrictionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryMembershipView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn restriction(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryRestrictionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::LibraryManage)?;
    let v = expected!(headers);
    let reason = optional_text("reason", b.reason, 1000)?;
    if b.restricted && reason.is_none() {
        return Err(invalid("reason", "required", "a restriction needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let pre = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_membership(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(pre.institution_id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = load_membership(&mut tx, t, id, true).await?;
    check_version(v, m.version)?;
    if m.status == LibraryMembershipStatus::Closed {
        return Err(illegal("library membership", &m.number, "closed", "restricted").into());
    }
    if !b.restricted && m.restriction_reason.is_none() {
        return Err(KernelError::Conflict(format!(
            "membership {} has no restriction to resolve",
            m.number
        ))
        .into());
    }
    let stored = if b.restricted { reason.clone() } else { None };
    sqlx::query(
        "UPDATE campus.library_membership SET restriction_reason = $2, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(stored.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    publish::audit(
        &mut tx,
        &scope,
        if b.restricted {
            "campus.library_membership.restrict"
        } else {
            "campus.library_membership.resolve_restriction"
        },
        "library_membership",
        id.to_string(),
        serde_json::json!({ "restricted": b.restricted, "reason": reason, "previous": m.restriction_reason }),
        &actor,
    )
    .await?;
    let out = load_membership(&mut tx, t, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get))
        .routes(routes!(check))
        .routes(routes!(suspend))
        .routes(routes!(restore))
        .routes(routes!(close))
        .routes(routes!(restriction))
        .with_state(state)
}
