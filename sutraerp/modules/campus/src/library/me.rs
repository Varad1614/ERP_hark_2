//! `/me/library` self-service (`campus.self`; the caller is resolved from the session subject with
//! `shared::me::my_person`, never from a client-supplied id; another member's record is 404):
//! catalogue search, own loans (renewal), reservations (place / cancel), fines, clearance and
//! library complaints (opened through `shared::tickets::open_ticket`, source `library`).

use super::catalogue::{search, CatalogueRecordList};
use super::circulation::{
    close_reservation, load_reservation, loans_of, place_reservation, renew_loan,
    reservations_where, LibraryLoanList, LibraryLoanView, LibraryReservationList,
    LibraryReservationView,
};
use super::clearance::{latest_of, request_clearance, LibraryClearanceView};
use super::common::{clean, expected, invalid, optional_text, required_text, LibraryReasonBody};
use super::domain::{CatalogueStatus, LibraryReservationStatus};
use super::fines::{fines_of, sync_billed_fines, LibraryFineList, LibraryFineView};
use super::members::{
    eligibility, load_membership, membership_of, LibraryEligibilityView, LibraryMembershipView,
};
use crate::shared::guard::require_institution_writable;
use crate::shared::http::{actor, with_etag};
use crate::shared::me::{my_person, PersonRef};
use crate::shared::rows::ServiceTicket;
use crate::shared::scope::institution_scope;
use crate::shared::tickets::{open_ticket, NewTicket};
use crate::shared::CampusState;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sutra_contracts::campus::status::{TicketCategory, TicketPriority, TicketSource};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// The caller and their live membership (404 without one).
async fn me(
    st: &CampusState,
    ctx: &AuthContext,
) -> Result<(PersonRef, LibraryMembershipView), KernelError> {
    let person = my_person(st, ctx).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let m = membership_of(&mut conn, ctx.scope.tenant_id, None, &person)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "library membership",
        })?;
    Ok((person, m))
}

fn scope_of(ctx: &AuthContext, m: &LibraryMembershipView) -> ExecutionScope {
    institution_scope(ctx, InstitutionId(m.institution_id))
}

/// Everything the Student Library panel shows.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyLibraryView {
    pub person_kind: sutra_contracts::campus::status::PersonKind,
    pub person_number: String,
    pub display_name: String,
    /// `None` when the caller is not a library member yet.
    pub membership: Option<LibraryMembershipView>,
    pub eligibility: Option<LibraryEligibilityView>,
    pub loans: Vec<LibraryLoanView>,
    pub reservations: Vec<LibraryReservationView>,
    pub fines: Vec<LibraryFineView>,
    pub clearance: Option<LibraryClearanceView>,
}

/// My library: membership, borrowing eligibility, loans, reservations, fines and the latest
/// clearance (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/library", operation_id = "campus_library_self_overview", tag = "campus-library",
    security(("session_cookie" = [])),
    responses((status = 200, body = MyLibraryView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn overview(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<MyLibraryView>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let person = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let membership = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        membership_of(&mut conn, t, None, &person).await?
    };
    let Some(m) = membership else {
        return Ok(Json(MyLibraryView {
            person_kind: person.kind,
            person_number: person.number,
            display_name: person.display_name,
            membership: None,
            eligibility: None,
            loans: Vec::new(),
            reservations: Vec::new(),
            fines: Vec::new(),
            clearance: None,
        }));
    };
    let scope = scope_of(ctx, &m);
    let elig = eligibility(&st, &scope, &m).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let loans = loans_of(&mut conn, t, m.id).await?;
    let reservations = reservations_where(&mut conn, t, None, None, Some(m.id), None).await?;
    let fines = fines_of(&mut conn, t, m.id).await?;
    let clearance = latest_of(&mut conn, t, m.id).await?;
    Ok(Json(MyLibraryView {
        person_kind: person.kind,
        person_number: person.number,
        display_name: person.display_name,
        membership: Some(m),
        eligibility: Some(elig),
        loans,
        reservations,
        fines,
        clearance,
    }))
}

/// Catalogue search of the self-service view.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MyCatalogueQuery {
    pub q: Option<String>,
    pub limit: Option<i64>,
}

/// Search the published catalogue of my library (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/library/catalogue", operation_id = "campus_library_self_catalogue", tag = "campus-library",
    params(MyCatalogueQuery), security(("session_cookie" = [])),
    responses((status = 200, body = CatalogueRecordList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn catalogue(
    State(st): State<CampusState>,
    auth: Authenticated,
    Query(q): Query<MyCatalogueQuery>,
) -> ApiResult<Json<CatalogueRecordList>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let person = my_person(&st, ctx).await?;
    let t = ctx.scope.tenant_id;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let institution = membership_of(&mut conn, t, None, &person)
        .await?
        .map(|m| InstitutionId(m.institution_id));
    let items = search(
        &mut conn,
        t,
        institution,
        Some(CatalogueStatus::Published),
        q.q,
        q.limit,
    )
    .await?;
    Ok(Json(CatalogueRecordList { items }))
}

/// My loans, newest first (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/library/loans", operation_id = "campus_library_self_loans", tag = "campus-library",
    security(("session_cookie" = [])),
    responses((status = 200, body = LibraryLoanList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_loans(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<LibraryLoanList>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let (_, m) = me(&st, ctx).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = loans_of(&mut conn, ctx.scope.tenant_id, m.id).await?;
    Ok(Json(LibraryLoanList {
        items,
        next_cursor: None,
    }))
}

/// Renew one of my loans (`campus.self`; If-Match): same rules as the office renewal.
#[utoipa::path(post, path = "/api/v1/campus/me/library/loans/{id}/renew", operation_id = "campus_library_self_loan_renew", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryLoanView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn renew_mine(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(_b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let v = expected!(headers);
    let (_, m) = me(&st, ctx).await?;
    let l = renew_loan(
        &st,
        |i| institution_scope(ctx, i),
        ctx.scope.tenant_id,
        id,
        v,
        Some(m.id),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &l, Version(l.version)))
}

/// My reservations (`campus.self`).
#[utoipa::path(get, path = "/api/v1/campus/me/library/reservations", operation_id = "campus_library_self_reservations", tag = "campus-library",
    security(("session_cookie" = [])),
    responses((status = 200, body = LibraryReservationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_reservations(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<LibraryReservationList>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let (_, m) = me(&st, ctx).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items =
        reservations_where(&mut conn, ctx.scope.tenant_id, None, None, Some(m.id), None).await?;
    Ok(Json(LibraryReservationList { items }))
}

/// A reservation I place.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyReservationCreate {
    pub record_id: Uuid,
}

/// Reserve a title (`campus.self`; Idempotency-Key; L3-G-134).
#[utoipa::path(post, path = "/api/v1/campus/me/library/reservations", operation_id = "campus_library_self_reserve", tag = "campus-library",
    params(("Idempotency-Key" = String, Header)),
    request_body = MyReservationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn reserve_mine(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<MyReservationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let (_, pre) = me(&st, ctx).await?;
    let scope = scope_of(ctx, &pre);
    require_institution_writable(&st, &scope, InstitutionId(pre.institution_id)).await?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let m = load_membership(&mut tx, t, pre.id, true).await?;
    let rid = place_reservation(&mut tx, &scope, &m, b.record_id, &actor).await?;
    let r = load_reservation(&mut tx, t, rid, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &r, Version(r.version)))
}

/// Cancel one of my reservations (`campus.self`; If-Match).
#[utoipa::path(post, path = "/api/v1/campus/me/library/reservations/{id}/cancel", operation_id = "campus_library_self_reservation_cancel", tag = "campus-library",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header)),
    request_body = LibraryReasonBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = LibraryReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cancel_mine(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<LibraryReasonBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let v = expected!(headers);
    let (_, m) = me(&st, ctx).await?;
    let scope = scope_of(ctx, &m);
    require_institution_writable(&st, &scope, InstitutionId(m.institution_id)).await?;
    let r = close_reservation(
        &st,
        &scope,
        id,
        Some(v),
        Some(m.id),
        LibraryReservationStatus::Cancelled,
        clean(b.reason).or_else(|| Some("cancelled by the member".into())),
        &actor(ctx),
    )
    .await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

/// My fines (`campus.self`; billed fines re-checked against Finance).
#[utoipa::path(get, path = "/api/v1/campus/me/library/fines", operation_id = "campus_library_self_fines", tag = "campus-library",
    security(("session_cookie" = [])),
    responses((status = 200, body = LibraryFineList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_fines(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<LibraryFineList>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let (_, m) = me(&st, ctx).await?;
    sync_billed_fines(&st, &scope_of(ctx, &m), m.id).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let items = fines_of(&mut conn, ctx.scope.tenant_id, m.id).await?;
    Ok(Json(LibraryFineList { items }))
}

/// My latest clearance request (`campus.self`; 404 when none).
#[utoipa::path(get, path = "/api/v1/campus/me/library/clearance", operation_id = "campus_library_self_clearance", tag = "campus-library",
    security(("session_cookie" = [])),
    responses((status = 200, body = LibraryClearanceView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_clearance(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<LibraryClearanceView>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let (_, m) = me(&st, ctx).await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let k = latest_of(&mut conn, ctx.scope.tenant_id, m.id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "library clearance",
        })?;
    Ok(Json(k))
}

/// A clearance I request.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyClearanceCreate {
    pub purpose: String,
}

/// Request library clearance (`campus.self`; Idempotency-Key; L3-G-149).
#[utoipa::path(post, path = "/api/v1/campus/me/library/clearance", operation_id = "campus_library_self_clearance_request", tag = "campus-library",
    params(("Idempotency-Key" = String, Header)),
    request_body = MyClearanceCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryClearanceView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_mine(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<MyClearanceCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let purpose = required_text("purpose", &b.purpose, 200)?;
    let (_, m) = me(&st, ctx).await?;
    let k = request_clearance(&st, &scope_of(ctx, &m), m.id, &purpose, &actor(ctx)).await?;
    Ok(with_etag(StatusCode::CREATED, &k, Version(k.version)))
}

/// A library complaint or service request.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyLibraryComplaint {
    pub title: String,
    pub description: Option<String>,
    /// The copy concerned, when any.
    pub copy_id: Option<Uuid>,
}

/// The service ticket opened for a library complaint.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LibraryComplaintView {
    pub ticket_id: Uuid,
    #[schema(example = "SR-000001")]
    pub number: String,
    pub title: String,
    pub status: sutra_contracts::campus::status::TicketStatus,
    pub sla_due_at: DateTime<Utc>,
}

impl From<&ServiceTicket> for LibraryComplaintView {
    fn from(t: &ServiceTicket) -> Self {
        Self {
            ticket_id: t.id.as_uuid(),
            number: t.number.clone(),
            title: t.title.clone(),
            status: t.status,
            sla_due_at: t.sla_due_at,
        }
    }
}

/// Raise a library complaint (`campus.self`; Idempotency-Key): a service desk ticket (category
/// and source `library`) followed up by the service desk.
#[utoipa::path(post, path = "/api/v1/campus/me/library/complaints", operation_id = "campus_library_self_complaint", tag = "campus-library",
    params(("Idempotency-Key" = String, Header)),
    request_body = MyLibraryComplaint, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = LibraryComplaintView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn complain(
    State(st): State<CampusState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(b): Json<MyLibraryComplaint>,
) -> ApiResult<(StatusCode, Json<LibraryComplaintView>)> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    require_idempotency_key(&headers)?;
    let title = required_text("title", &b.title, 200)?;
    let description = optional_text("description", b.description, 4000)?;
    let (person, m) = me(&st, ctx).await?;
    let institution = InstitutionId(m.institution_id);
    let scope = scope_of(ctx, &m);
    require_institution_writable(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let node = match b.copy_id {
        Some(copy) => {
            let row: Option<(Uuid, Option<Uuid>)> = sqlx::query_as(
                "SELECT id, node_id FROM campus.library_copy WHERE id = $1 AND tenant_id = $2 \
                 AND institution_id = $3",
            )
            .bind(copy)
            .bind(ctx.scope.tenant_id.as_uuid())
            .bind(institution.as_uuid())
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?;
            row.ok_or_else(|| invalid("copyId", "unknown_copy", "no such copy in your library"))?
                .1
        }
        None => None,
    };
    let ticket = open_ticket(
        &mut tx,
        &scope,
        NewTicket {
            institution,
            category: TicketCategory::Library,
            source: TicketSource::Library,
            source_ref_id: b.copy_id.or(Some(m.id)),
            is_complaint: true,
            node_id: node.map(sutra_contracts::campus::ids::InfrastructureNodeId),
            requester: Some(&person),
            title: &title,
            description: description.as_deref(),
            priority: TicketPriority::Medium,
            sla_hours: None,
        },
        &actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((
        StatusCode::CREATED,
        Json(LibraryComplaintView::from(&ticket)),
    ))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(overview))
        .routes(routes!(catalogue))
        .routes(routes!(my_loans))
        .routes(routes!(renew_mine))
        .routes(routes!(my_reservations, reserve_mine))
        .routes(routes!(cancel_mine))
        .routes(routes!(my_fines))
        .routes(routes!(my_clearance, request_mine))
        .routes(routes!(complain))
        .with_state(state)
}
