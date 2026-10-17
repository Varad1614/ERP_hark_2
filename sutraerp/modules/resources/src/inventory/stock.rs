//! Stock documents over the foundation ledger (every movement through
//! `shared::stock::post_movement`): issues (`SIV-`, L3-J-064) with returns to store (L3-J-066),
//! reservations (available = on hand − reserved, L3-J-065), approved adjustments
//! (`STOCK_ADJUSTMENT_APPROVAL`, L3-J-059) and the reorder-level run → replenishment needs
//! (L3-J-061, optional draft requisition).

use crate::requisitions;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::stock::{on_hand, post_movement, NewMovement};
use crate::shared::time::business_today;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers::common::{
    active_item, active_location, audit_only, check_org_unit, check_staff, check_version, clean,
    emit, expected, history, illegal, institution_of, invalid, optional_text, parse_qty,
    parse_signed_qty, reason_of, required_reason, required_text, reserved, slice_status,
    status_payload, ResourcesBulkResult, ResourcesDecision, ResourcesDecisionBody,
    ResourcesReasonBody, ResourcesStatusQuery,
};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::resources::events::procurement as ev;
use sutra_contracts::resources::ids::{ItemId, StoreLocationId};
use sutra_contracts::resources::status::StockMovementKind;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

slice_status!(
    /// Stock issue lifecycle (L3-J-064 / 066).
    StockIssueStatus {
        Requested => "requested",
        Approved => "approved",
        Issued => "issued",
        Returned => "returned",
        Cancelled => "cancelled",
    }
);

slice_status!(
    /// Stock adjustment lifecycle (L3-J-059).
    AdjustmentStatus {
        Requested => "requested",
        Approved => "approved",
        Applied => "applied",
        Rejected => "rejected",
    }
);

slice_status!(
    /// Stock reservation lifecycle (L3-J-065).
    ReservationStatus {
        Active => "active",
        Released => "released",
        Fulfilled => "fulfilled",
    }
);

impl StockIssueStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use StockIssueStatus::*;
        matches!(
            (self, to),
            (Requested, Approved)
                | (Requested, Issued)
                | (Requested, Cancelled)
                | (Approved, Issued)
                | (Approved, Cancelled)
                | (Issued, Returned)
        )
    }
}

/// Available to issue at a location: on hand − active reservations (excluding `own`).
async fn available(
    conn: &mut PgConnection,
    tenant: TenantId,
    item: Uuid,
    location: Uuid,
    own: Option<Uuid>,
) -> Result<(Decimal, Decimal), KernelError> {
    let oh = on_hand(conn, tenant, ItemId(item), Some(StoreLocationId(location))).await?;
    let res = reserved(conn, tenant, item, Some(location), own).await?;
    Ok((oh, oh - res))
}

// ================================================================== issues + returns

/// A stock issue voucher.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct StockIssueView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "SIV-000001")]
    pub number: String,
    pub item_id: Uuid,
    pub item_code: String,
    pub location_id: Uuid,
    pub location_code: String,
    #[schema(value_type = String, example = "4.000")]
    pub quantity: Decimal,
    #[schema(value_type = String, example = "1.000")]
    pub returned_quantity: Decimal,
    pub issued_to_staff_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub purpose: String,
    pub reservation_id: Option<Uuid>,
    /// `requested | approved | issued | returned | cancelled`.
    pub status: String,
    pub status_reason: Option<String>,
    pub ledger_entry_id: Option<Uuid>,
    pub issued_on: Option<NaiveDate>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl StockIssueView {
    pub fn status(&self) -> Result<StockIssueStatus, KernelError> {
        StockIssueStatus::parse(&self.status).ok_or_else(|| {
            KernelError::Conflict(format!("unknown stock issue status {}", self.status))
        })
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StockIssueList {
    pub items: Vec<StockIssueView>,
}

const ISSUE_SELECT: &str = "SELECT s.id, s.institution_id, s.number, s.item_id, i.code AS item_code, \
    s.location_id, l.code AS location_code, s.quantity, s.returned_quantity, s.issued_to_staff_id, \
    s.org_unit_id, s.purpose, s.reservation_id, s.status, s.status_reason, s.ledger_entry_id, \
    s.issued_on, s.version, s.created_at FROM resources.stock_issue s \
    JOIN resources.item i ON i.id = s.item_id JOIN resources.store_location l ON l.id = s.location_id";

async fn load_issue(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<StockIssueView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF s" } else { "" };
    sqlx::query_as::<_, StockIssueView>(&format!(
        "{ISSUE_SELECT} WHERE s.id = $1 AND s.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "stock issue",
    })
}

async fn issue_response(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let v = load_issue(conn, tenant, id, false).await?;
    Ok(with_etag(status, &v, Version(v.version)))
}

async fn set_issue_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &StockIssueView,
    to: StockIssueStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = s.status()?;
    if !from.can_transition(to) {
        return Err(illegal(
            "stock issue",
            &s.number,
            from.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE resources.stock_issue SET status = $2, status_reason = COALESCE($3, status_reason), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(s.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(s.institution_id),
        "stock_issue",
        s.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        None,
        actor,
    )
    .await
}

/// Posts the `issue` movement of an approved / requested issue and marks it `issued`
/// (`stock_issue.issued`). 409 when the available quantity does not cover it.
async fn post_issue(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    id: Uuid,
    today: NaiveDate,
    subject: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let s = load_issue(conn, tenant, id, true).await?;
    let (oh, avail) = available(conn, tenant, s.item_id, s.location_id, s.reservation_id).await?;
    if avail < s.quantity {
        return Err(KernelError::Conflict(format!(
            "insufficient stock of {} at {}: {} on hand, {} available after reservations, {} requested",
            s.item_code, s.location_code, oh, avail, s.quantity
        )));
    }
    let m = post_movement(
        conn,
        NewMovement {
            tenant,
            institution: InstitutionId(s.institution_id),
            item_id: ItemId(s.item_id),
            location_id: StoreLocationId(s.location_id),
            kind: StockMovementKind::Issue,
            quantity: -s.quantity,
            unit_cost: None,
            effective_on: today,
            source_type: "stock_issue",
            source_id: id,
            posting_key: "issue",
            reverses_entry_id: None,
            reason: Some(&s.purpose),
            expiry_on: None,
        },
        actor,
    )
    .await?;
    set_issue_status(
        conn,
        tenant,
        &s,
        StockIssueStatus::Issued,
        ev::STOCK_ISSUED,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.stock_issue SET ledger_entry_id = $2, issued_on = $3 WHERE id = $1",
    )
    .bind(id)
    .bind(m.entry_id.as_uuid())
    .bind(today)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(r) = s.reservation_id {
        sqlx::query(
            "UPDATE resources.stock_reservation SET status = 'fulfilled', stock_issue_id = $2, \
             version = version + 1, updated_at = now() WHERE id = $1 AND status = 'active'",
        )
        .bind(r)
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    let out = load_issue(conn, tenant, id, false).await?;
    emit(
        conn,
        scope,
        ev::STOCK_ISSUED,
        ev::STOCK_ISSUED_SCHEMA,
        "stock_issue",
        id,
        "resources.stock_issue.issue",
        &status_payload(
            id,
            out.institution_id,
            Some(s.status.as_str()),
            "issued",
            out.version,
            Some(&format!(
                "{} {} × {}",
                out.item_code, out.quantity, out.location_code
            )),
            None,
            Some(&out.number),
            subject,
        ),
        actor,
    )
    .await
}

/// Request stock from a store.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockIssueCreate {
    pub item_id: Uuid,
    pub location_id: Uuid,
    #[schema(example = "4")]
    pub quantity: String,
    /// Domain E staff member receiving the goods.
    pub issued_to_staff_id: Option<Uuid>,
    /// Domain B org unit consuming them.
    pub org_unit_id: Option<Uuid>,
    pub purpose: String,
}

/// Inserts a requested issue; returns `(id, staff subject)`.
#[allow(clippy::too_many_arguments)]
async fn insert_issue(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    institution: InstitutionId,
    item: Uuid,
    location: Uuid,
    quantity: Decimal,
    staff: Option<Uuid>,
    org_unit: Option<Uuid>,
    purpose: &str,
    reservation: Option<Uuid>,
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let number = next(conn, tenant, institution, kind::STOCK_ISSUE).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.stock_issue (tenant_id, institution_id, number, item_id, location_id, \
         quantity, issued_to_staff_id, org_unit_id, purpose, reservation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(item)
    .bind(location)
    .bind(quantity)
    .bind(staff)
    .bind(org_unit)
    .bind(purpose)
    .bind(reservation)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        institution,
        "stock_issue",
        id,
        None,
        "requested",
        "sutraerp.resources.stock_issue.requested",
        None,
        None,
        actor,
    )
    .await?;
    audit_only(
        conn,
        scope,
        "stock_issue",
        id,
        "resources.stock_issue.create",
        serde_json::json!({ "number": number, "quantity": quantity.to_string() }),
        actor,
    )
    .await?;
    Ok(id)
}

/// Request an issue from a store (`resources.inventory.manage`; Idempotency-Key): `requested`.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/stock-issues", operation_id = "resources_stock_issue_create", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = StockIssueCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = StockIssueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_issue(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StockIssueCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let quantity = parse_qty("quantity", &b.quantity)?;
    let purpose = required_text("purpose", &b.purpose, 300)?;
    check_staff(
        &st,
        &scope,
        institution,
        "issuedToStaffId",
        b.issued_to_staff_id,
    )
    .await?;
    check_org_unit(&st, &scope, institution, "orgUnitId", b.org_unit_id).await?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    active_item(&mut tx, t, institution, "itemId", b.item_id).await?;
    active_location(&mut tx, t, institution, "locationId", b.location_id).await?;
    let sid = insert_issue(
        &mut tx,
        &scope,
        t,
        institution,
        b.item_id,
        b.location_id,
        quantity,
        b.issued_to_staff_id,
        b.org_unit_id,
        &purpose,
        None,
        &actor,
    )
    .await?;
    let res = issue_response(&mut tx, t, sid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Stock issues of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-issues", operation_id = "resources_stock_issue_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StockIssueList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_issues(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<StockIssueList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, StockIssueView>(&format!(
        "{ISSUE_SELECT} WHERE s.tenant_id = $1 AND s.institution_id = $2 \
         AND ($3::text IS NULL OR s.status = $3) ORDER BY s.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(StockIssueList { items }))
}

async fn issue_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(
        &mut conn,
        ctx.scope.tenant_id,
        "stock_issue",
        "stock issue",
        id,
    )
    .await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

async fn staff_subject(
    st: &ResourcesState,
    scope: &ExecutionScope,
    staff: Option<Uuid>,
) -> Result<Option<String>, KernelError> {
    let Some(s) = staff else { return Ok(None) };
    Ok(st
        .workforce
        .staff(scope, sutra_contracts::people::ids::StaffMemberId(s))
        .await?
        .and_then(|x| x.subject_ref))
}

/// Decide a requested issue (`resources.inventory.manage`; If-Match): approve → `approved`;
/// reject → `cancelled` (reason required).
#[utoipa::path(post, path = "/api/v1/resources/stock-issues/{id}/decision", operation_id = "resources_stock_issue_decide", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the issue")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StockIssueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide_issue(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ResourcesDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == ResourcesDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = issue_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load_issue(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if s.status()? != StockIssueStatus::Requested {
        return Err(illegal("stock issue", &s.number, &s.status, "decided").into());
    }
    let to = if b.decision == ResourcesDecision::Approve {
        StockIssueStatus::Approved
    } else {
        StockIssueStatus::Cancelled
    };
    set_issue_status(
        &mut tx,
        t,
        &s,
        to,
        "sutraerp.resources.stock_issue.decided",
        reason.as_deref(),
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "stock_issue",
        id,
        "resources.stock_issue.decide",
        serde_json::json!({ "number": s.number, "decision": b.decision, "reason": reason }),
        &actor,
    )
    .await?;
    let res = issue_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Hand the goods over (`resources.inventory.manage`; If-Match): posts the `issue` movement
/// (409 when on hand − reservations does not cover it); `stock_issue.issued`.
#[utoipa::path(post, path = "/api/v1/resources/stock-issues/{id}/issue", operation_id = "resources_stock_issue_issue", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the issue")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StockIssueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn issue_issue(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = issue_scope(&st, ctx, id).await?;
    let s = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_issue(&mut conn, t, id, false).await?
    };
    let today = business_today(&st, &scope, InstitutionId(s.institution_id)).await?;
    let subject = staff_subject(&st, &scope, s.issued_to_staff_id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load_issue(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if s.status()? != StockIssueStatus::Approved {
        return Err(illegal("stock issue", &s.number, &s.status, "issued").into());
    }
    post_issue(&mut tx, &scope, t, id, today, subject.as_deref(), &actor).await?;
    let res = issue_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Return issued goods to the store.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockReturnCreate {
    #[schema(example = "1")]
    pub quantity: String,
    pub reason: Option<String>,
}

/// Return issued goods to the store (`resources.inventory.manage`; If-Match; L3-J-066): posts a
/// `return_to_store` movement; the issue becomes `returned` once everything is back.
#[utoipa::path(post, path = "/api/v1/resources/stock-issues/{id}/return", operation_id = "resources_stock_issue_return", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the issue")),
    request_body = StockReturnCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StockIssueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn return_issue(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StockReturnCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    let quantity = parse_qty("quantity", &b.quantity)?;
    let reason = optional_text("reason", b.reason, 500)?;
    let t = ctx.scope.tenant_id;
    let scope = issue_scope(&st, ctx, id).await?;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        institution_of(&mut conn, t, "stock_issue", "stock issue", id).await?
    };
    let today = business_today(&st, &scope, inst).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load_issue(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if s.status()? != StockIssueStatus::Issued {
        return Err(illegal("stock issue", &s.number, &s.status, "returned").into());
    }
    let outstanding = s.quantity - s.returned_quantity;
    if quantity > outstanding {
        return Err(invalid(
            "quantity",
            "exceeds_issued",
            &format!("only {outstanding} is still out"),
        )
        .into());
    }
    let rid = Uuid::now_v7();
    let m = post_movement(
        &mut tx,
        NewMovement {
            tenant: t,
            institution: InstitutionId(s.institution_id),
            item_id: ItemId(s.item_id),
            location_id: StoreLocationId(s.location_id),
            kind: StockMovementKind::ReturnToStore,
            quantity,
            unit_cost: None,
            effective_on: today,
            source_type: "stock_return",
            source_id: rid,
            posting_key: "return",
            reverses_entry_id: None,
            reason: reason.as_deref(),
            expiry_on: None,
        },
        &actor,
    )
    .await?;
    sqlx::query(
        "INSERT INTO resources.stock_return (id, tenant_id, institution_id, stock_issue_id, quantity, \
         reason, returned_on, ledger_entry_id, actor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(rid)
    .bind(t.as_uuid())
    .bind(s.institution_id)
    .bind(id)
    .bind(quantity)
    .bind(&reason)
    .bind(today)
    .bind(m.entry_id.as_uuid())
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE resources.stock_issue SET returned_quantity = returned_quantity + $2, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(quantity)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if quantity == outstanding {
        let s2 = load_issue(&mut tx, t, id, true).await?;
        set_issue_status(
            &mut tx,
            t,
            &s2,
            StockIssueStatus::Returned,
            "sutraerp.resources.stock_issue.returned",
            reason.as_deref(),
            &actor,
        )
        .await?;
    }
    audit_only(
        &mut tx,
        &scope,
        "stock_issue",
        id,
        "resources.stock_issue.return",
        serde_json::json!({ "number": s.number, "quantity": quantity.to_string(), "reason": reason }),
        &actor,
    )
    .await?;
    let res = issue_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Cancel a requested / approved issue (`resources.inventory.manage`; If-Match; reason).
#[utoipa::path(post, path = "/api/v1/resources/stock-issues/{id}/cancel", operation_id = "resources_stock_issue_cancel", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the issue")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StockIssueView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cancel_issue(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    let reason = required_reason(reason_of(body))?;
    let t = ctx.scope.tenant_id;
    let scope = issue_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load_issue(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    set_issue_status(
        &mut tx,
        t,
        &s,
        StockIssueStatus::Cancelled,
        "sutraerp.resources.stock_issue.cancelled",
        Some(&reason),
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "stock_issue",
        id,
        "resources.stock_issue.cancel",
        serde_json::json!({ "number": s.number, "reason": reason }),
        &actor,
    )
    .await?;
    let res = issue_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

// ================================================================== reservations

/// A stock reservation.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct StockReservationView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub item_id: Uuid,
    pub item_code: String,
    pub location_id: Uuid,
    pub location_code: String,
    #[schema(value_type = String, example = "3.000")]
    pub quantity: Decimal,
    pub purpose: String,
    pub needed_on: Option<NaiveDate>,
    /// `active | released | fulfilled`.
    pub status: String,
    pub status_reason: Option<String>,
    pub stock_issue_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StockReservationList {
    pub items: Vec<StockReservationView>,
}

const RES_SELECT: &str = "SELECT r.id, r.institution_id, r.item_id, i.code AS item_code, r.location_id, \
    l.code AS location_code, r.quantity, r.purpose, r.needed_on, r.status, r.status_reason, \
    r.stock_issue_id, r.version, r.created_at FROM resources.stock_reservation r \
    JOIN resources.item i ON i.id = r.item_id JOIN resources.store_location l ON l.id = r.location_id";

async fn load_reservation(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<StockReservationView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF r" } else { "" };
    sqlx::query_as::<_, StockReservationView>(&format!(
        "{RES_SELECT} WHERE r.id = $1 AND r.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "stock reservation",
    })
}

async fn reservation_response(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let v = load_reservation(conn, tenant, id, false).await?;
    Ok(with_etag(status, &v, Version(v.version)))
}

/// Hold stock for a purpose.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockReservationCreate {
    pub item_id: Uuid,
    pub location_id: Uuid,
    #[schema(example = "3")]
    pub quantity: String,
    pub purpose: String,
    pub needed_on: Option<NaiveDate>,
}

/// Reserve stock at a location (`resources.inventory.manage`; Idempotency-Key; L3-J-065): 409
/// when on hand − existing reservations does not cover it.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/stock-reservations", operation_id = "resources_stock_reservation_create", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = StockReservationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = StockReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_reservation(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StockReservationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let quantity = parse_qty("quantity", &b.quantity)?;
    let purpose = required_text("purpose", &b.purpose, 300)?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let item = active_item(&mut tx, t, institution, "itemId", b.item_id).await?;
    active_location(&mut tx, t, institution, "locationId", b.location_id).await?;
    // Serialise reservations of the location.
    sqlx::query("SELECT id FROM resources.store_location WHERE id = $1 FOR UPDATE")
        .bind(b.location_id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let (oh, avail) = available(&mut tx, t, b.item_id, b.location_id, None).await?;
    if avail < quantity {
        return Err(KernelError::Conflict(format!(
            "insufficient stock of {}: {oh} on hand, {avail} available, {quantity} requested",
            item.code
        ))
        .into());
    }
    let rid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.stock_reservation (tenant_id, institution_id, item_id, location_id, \
         quantity, purpose, needed_on) VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.item_id)
    .bind(b.location_id)
    .bind(quantity)
    .bind(&purpose)
    .bind(b.needed_on)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "stock_reservation",
        rid,
        None,
        "active",
        "sutraerp.resources.stock_reservation.created",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "stock_reservation",
        rid,
        "resources.stock_reservation.create",
        serde_json::json!({ "item": item.code, "quantity": quantity.to_string() }),
        &actor,
    )
    .await?;
    let res = reservation_response(&mut tx, t, rid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Reservations of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-reservations", operation_id = "resources_stock_reservation_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StockReservationList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_reservations(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<StockReservationList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, StockReservationView>(&format!(
        "{RES_SELECT} WHERE r.tenant_id = $1 AND r.institution_id = $2 \
         AND ($3::text IS NULL OR r.status = $3) ORDER BY r.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(StockReservationList { items }))
}

async fn reservation_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(
        &mut conn,
        ctx.scope.tenant_id,
        "stock_reservation",
        "stock reservation",
        id,
    )
    .await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// Release an active reservation (`resources.inventory.manage`; If-Match; reason).
#[utoipa::path(post, path = "/api/v1/resources/stock-reservations/{id}/release", operation_id = "resources_stock_reservation_release", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the reservation")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StockReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn release_reservation(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    let reason = reason_of(body);
    let t = ctx.scope.tenant_id;
    let scope = reservation_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_reservation(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    if r.status != ReservationStatus::Active.as_str() {
        return Err(illegal("stock reservation of", &r.item_code, &r.status, "released").into());
    }
    sqlx::query(
        "UPDATE resources.stock_reservation SET status = 'released', status_reason = $2, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        InstitutionId(r.institution_id),
        "stock_reservation",
        id,
        Some("active"),
        "released",
        "sutraerp.resources.stock_reservation.released",
        reason.as_deref(),
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "stock_reservation",
        id,
        "resources.stock_reservation.release",
        serde_json::json!({ "item": r.item_code, "reason": reason }),
        &actor,
    )
    .await?;
    let res = reservation_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Fulfil a reservation.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockReservationFulfil {
    pub issued_to_staff_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
}

/// Fulfil an active reservation (`resources.inventory.manage`; If-Match): issues the reserved
/// quantity at once (an `SIV-` issue, `issue` movement, `stock_issue.issued`) and marks the
/// reservation `fulfilled`.
#[utoipa::path(post, path = "/api/v1/resources/stock-reservations/{id}/fulfil", operation_id = "resources_stock_reservation_fulfil", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the reservation")),
    request_body(content = StockReservationFulfil, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StockReservationView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn fulfil_reservation(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<StockReservationFulfil>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let v = expected!(headers);
    let b = body.map(|b| b.0).unwrap_or_default();
    let t = ctx.scope.tenant_id;
    let scope = reservation_scope(&st, ctx, id).await?;
    let r = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_reservation(&mut conn, t, id, false).await?
    };
    let institution = InstitutionId(r.institution_id);
    check_staff(
        &st,
        &scope,
        institution,
        "issuedToStaffId",
        b.issued_to_staff_id,
    )
    .await?;
    check_org_unit(&st, &scope, institution, "orgUnitId", b.org_unit_id).await?;
    let subject = staff_subject(&st, &scope, b.issued_to_staff_id).await?;
    let today = business_today(&st, &scope, institution).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load_reservation(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    if r.status != ReservationStatus::Active.as_str() {
        return Err(illegal("stock reservation of", &r.item_code, &r.status, "fulfilled").into());
    }
    let sid = insert_issue(
        &mut tx,
        &scope,
        t,
        institution,
        r.item_id,
        r.location_id,
        r.quantity,
        b.issued_to_staff_id,
        b.org_unit_id,
        &r.purpose,
        Some(id),
        &actor,
    )
    .await?;
    post_issue(&mut tx, &scope, t, sid, today, subject.as_deref(), &actor).await?;
    history(
        &mut tx,
        t,
        institution,
        "stock_reservation",
        id,
        Some("active"),
        "fulfilled",
        ev::STOCK_ISSUED,
        None,
        None,
        &actor,
    )
    .await?;
    let res = reservation_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

// ================================================================== adjustments

/// An approved stock correction.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct StockAdjustmentView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub item_id: Uuid,
    pub item_code: String,
    pub location_id: Uuid,
    pub location_code: String,
    /// Signed (+ found / − lost or damaged).
    #[schema(value_type = String, example = "-2.000")]
    pub quantity: Decimal,
    pub reason: String,
    /// `requested | approved | applied | rejected`.
    pub status: String,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub ledger_entry_id: Option<Uuid>,
    pub decided_by: Option<String>,
    pub applied_on: Option<NaiveDate>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct StockAdjustmentList {
    pub items: Vec<StockAdjustmentView>,
}

const ADJ_SELECT: &str = "SELECT a.id, a.institution_id, a.item_id, i.code AS item_code, a.location_id, \
    l.code AS location_code, a.quantity, a.reason, a.status, a.status_reason, a.process_instance_id, \
    a.ledger_entry_id, a.decided_by, a.applied_on, a.version, a.created_at \
    FROM resources.stock_adjustment a JOIN resources.item i ON i.id = a.item_id \
    JOIN resources.store_location l ON l.id = a.location_id";

async fn load_adjustment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<StockAdjustmentView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF a" } else { "" };
    sqlx::query_as::<_, StockAdjustmentView>(&format!(
        "{ADJ_SELECT} WHERE a.id = $1 AND a.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "stock adjustment",
    })
}

async fn adjustment_response(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let v = load_adjustment(conn, tenant, id, false).await?;
    Ok(with_etag(status, &v, Version(v.version)))
}

/// Approves and applies an adjustment: `adjustment` movement (409 when it would go below zero).
async fn apply_adjustment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    today: NaiveDate,
    actor: &Actor,
) -> Result<(), KernelError> {
    let a = load_adjustment(conn, tenant, id, true).await?;
    let m = post_movement(
        conn,
        NewMovement {
            tenant,
            institution: InstitutionId(a.institution_id),
            item_id: ItemId(a.item_id),
            location_id: StoreLocationId(a.location_id),
            kind: StockMovementKind::Adjustment,
            quantity: a.quantity,
            unit_cost: None,
            effective_on: today,
            source_type: "stock_adjustment",
            source_id: id,
            posting_key: "apply",
            reverses_entry_id: None,
            reason: Some(&a.reason),
            expiry_on: None,
        },
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.stock_adjustment SET status = 'applied', ledger_entry_id = $2, \
         applied_on = $3, decided_by = $4, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(m.entry_id.as_uuid())
    .bind(today)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    for (from, to) in [("requested", "approved"), ("approved", "applied")] {
        history(
            conn,
            tenant,
            InstitutionId(a.institution_id),
            "stock_adjustment",
            id,
            Some(from),
            to,
            "sutraerp.resources.stock_adjustment.applied",
            None,
            a.process_instance_id.as_deref(),
            actor,
        )
        .await?;
    }
    Ok(())
}

/// Request a stock correction.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StockAdjustmentCreate {
    pub item_id: Uuid,
    pub location_id: Uuid,
    /// Signed quantity (`"-2"` lost, `"3"` found).
    #[schema(example = "-2")]
    pub quantity: String,
    pub reason: String,
}

/// Request a stock adjustment (`resources.inventory.manage`; Idempotency-Key;
/// `STOCK_ADJUSTMENT_APPROVAL`, L3-J-059): auto-approval applies it at once.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/stock-adjustments", operation_id = "resources_stock_adjustment_create", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = StockAdjustmentCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = StockAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create_adjustment(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<StockAdjustmentCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InventoryManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let quantity = parse_signed_qty("quantity", &b.quantity)?;
    let reason = required_text("reason", &b.reason, 1000)?;
    let t = ctx.scope.tenant_id;
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        active_item(&mut conn, t, institution, "itemId", b.item_id).await?;
        active_location(&mut conn, t, institution, "locationId", b.location_id).await?;
    }
    let today = business_today(&st, &scope, institution).await?;
    let aid = Uuid::now_v7();
    let mut vars = BTreeMap::new();
    vars.insert(
        "quantity".to_owned(),
        serde_json::json!(quantity.to_string()),
    );
    vars.insert("reason".to_owned(), serde_json::json!(reason));
    let outcome = request_approval(
        &st,
        &scope,
        process::STOCK_ADJUSTMENT_APPROVAL,
        business_key("stock_adjustment", aid, 1),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO resources.stock_adjustment (id, tenant_id, institution_id, item_id, location_id, \
         quantity, reason, process_instance_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(aid)
    .bind(t.as_uuid())
    .bind(id)
    .bind(b.item_id)
    .bind(b.location_id)
    .bind(quantity)
    .bind(&reason)
    .bind(outcome.process_instance_id())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "stock_adjustment",
        aid,
        None,
        "requested",
        "sutraerp.resources.stock_adjustment.requested",
        Some(&reason),
        Some(outcome.process_instance_id()),
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "stock_adjustment",
        aid,
        "resources.stock_adjustment.create",
        serde_json::json!({ "quantity": quantity.to_string(), "reason": reason }),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        apply_adjustment(
            &mut tx,
            t,
            aid,
            today,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let res = adjustment_response(&mut tx, t, aid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Adjustments of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/stock-adjustments", operation_id = "resources_stock_adjustment_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StockAdjustmentList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_adjustments(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<StockAdjustmentList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, StockAdjustmentView>(&format!(
        "{ADJ_SELECT} WHERE a.tenant_id = $1 AND a.institution_id = $2 \
         AND ($3::text IS NULL OR a.status = $3) ORDER BY a.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(StockAdjustmentList { items }))
}

/// Decide a pending adjustment (`resources.approve`, step-up; If-Match): approve → applied;
/// reject → rejected (reason required).
#[utoipa::path(post, path = "/api/v1/resources/stock-adjustments/{id}/decision", operation_id = "resources_stock_adjustment_decide", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the adjustment")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = StockAdjustmentView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide_adjustment(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ResourcesDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesApprove)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the pending request never decides it.
    sutra_persistence::approval::guard_decision(&st.pool, ctx, &id, "stock adjustment").await?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == ResourcesDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let inst = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        institution_of(&mut conn, t, "stock_adjustment", "stock adjustment", id).await?
    };
    let scope = institution_scope(ctx, inst);
    require_institution_writable(&st, &scope, inst).await?;
    let today = business_today(&st, &scope, inst).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load_adjustment(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    if a.status != AdjustmentStatus::Requested.as_str() {
        return Err(illegal("stock adjustment of", &a.item_code, &a.status, "decided").into());
    }
    match b.decision {
        ResourcesDecision::Approve => apply_adjustment(&mut tx, t, id, today, &actor).await?,
        ResourcesDecision::Reject => {
            sqlx::query(
                "UPDATE resources.stock_adjustment SET status = 'rejected', status_reason = $2, \
                 decided_by = $3, version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&reason)
            .bind(actor.id.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            history(
                &mut tx,
                t,
                inst,
                "stock_adjustment",
                id,
                Some("requested"),
                "rejected",
                "sutraerp.resources.stock_adjustment.rejected",
                reason.as_deref(),
                a.process_instance_id.as_deref(),
                &actor,
            )
            .await?;
        }
    }
    audit_only(
        &mut tx,
        &scope,
        "stock_adjustment",
        id,
        "resources.stock_adjustment.decide",
        serde_json::json!({ "decision": b.decision, "reason": reason }),
        &actor,
    )
    .await?;
    let res = adjustment_response(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

// ================================================================== replenishment

/// A replenishment need raised by the reorder-level run.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ReplenishmentNeedView {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub item_id: Uuid,
    pub item_code: String,
    pub item_name: String,
    #[schema(value_type = String, example = "8.000")]
    pub on_hand: Decimal,
    #[schema(value_type = String, example = "0.000")]
    pub reserved: Decimal,
    #[schema(value_type = String, example = "12.000")]
    pub reorder_level: Decimal,
    #[schema(value_type = String, example = "50.000")]
    pub suggested_quantity: Decimal,
    /// `open | requisitioned | closed`.
    pub status: String,
    pub requisition_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ReplenishmentNeedList {
    pub items: Vec<ReplenishmentNeedView>,
}

/// Options of the reorder-level run.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReplenishmentRun {
    /// Also draft one purchase requisition covering every new need.
    #[serde(default)]
    pub draft_requisition: bool,
}

/// Run the reorder-level check (`resources.inventory.manage`; Idempotency-Key; L3-J-061): every
/// active item whose on hand − reservations is at / below its reorder level and has no open
/// need gets one (`replenishment_need.raised`); optionally one draft requisition for them all.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/replenishment", operation_id = "resources_stock_replenishment_run", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body(content = ReplenishmentRun, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = ResourcesBulkResult), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
#[allow(clippy::type_complexity)]
pub async fn run_replenishment(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ReplenishmentRun>>,
) -> ApiResult<Json<ResourcesBulkResult>> {
    let ctx = auth.require(Permission::InventoryManage)?;
    let run_key = require_idempotency_key(&headers)?;
    let opts = body.map(|b| b.0).unwrap_or_default();
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let candidates: Vec<(Uuid, String, String, String, Decimal, Option<Decimal>, Option<Decimal>)> =
        sqlx::query_as(
            "SELECT id, code, name, unit, reorder_level, reorder_quantity, standard_cost \
             FROM resources.item WHERE tenant_id = $1 AND institution_id = $2 AND status = 'active' \
             AND reorder_level IS NOT NULL AND kind <> 'service' ORDER BY code",
        )
        .bind(t.as_uuid())
        .bind(id)
        .fetch_all(&mut *tx)
        .await
        .map_err(map_db_error)?;
    let mut result = ResourcesBulkResult::default();
    let mut lines = Vec::new();
    let mut new_needs = Vec::new();
    for (item, code, name, unit, level, reorder_qty, cost) in candidates {
        let oh = on_hand(&mut tx, t, ItemId(item), None).await?;
        let res = reserved(&mut tx, t, item, None, None).await?;
        let avail = oh - res;
        if avail > level {
            result.skipped += 1;
            continue;
        }
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resources.replenishment_need WHERE tenant_id = $1 AND item_id = $2 \
             AND status IN ('open', 'requisitioned')",
        )
        .bind(t.as_uuid())
        .bind(item)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if open > 0 {
            result.skipped += 1;
            continue;
        }
        let suggested = match reorder_qty {
            Some(q) => q,
            None => {
                let s = level * Decimal::from(2) - avail;
                if s > Decimal::ZERO {
                    s
                } else {
                    Decimal::ONE
                }
            }
        };
        let nid: Uuid = sqlx::query_scalar(
            "INSERT INTO resources.replenishment_need (tenant_id, institution_id, item_id, on_hand, \
             reserved, reorder_level, suggested_quantity, run_key) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
        )
        .bind(t.as_uuid())
        .bind(id)
        .bind(item)
        .bind(oh)
        .bind(res)
        .bind(level)
        .bind(suggested)
        .bind(&run_key)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        emit(
            &mut tx,
            &scope,
            ev::REPLENISHMENT_NEEDED,
            ev::REPLENISHMENT_NEEDED_SCHEMA,
            "replenishment_need",
            nid,
            "resources.replenishment_need.raise",
            &status_payload(
                nid,
                id,
                None,
                "open",
                1,
                Some(&format!(
                    "{code} {name}: {avail} available ≤ reorder level {level}; order {suggested}"
                )),
                None,
                Some(&code),
                None,
            ),
            &actor,
        )
        .await?;
        lines.push(requisitions::replenishment_line(
            item, &name, &unit, suggested, cost,
        ));
        new_needs.push(nid);
        result.processed += 1;
        result.created_ids.push(nid);
    }
    if opts.draft_requisition && !lines.is_empty() {
        let rid = requisitions::insert_draft(
            &mut tx,
            &scope,
            t,
            institution,
            "Replenishment below reorder level",
            Some("Raised by the reorder-level run"),
            None,
            None,
            None,
            "replenishment",
            &lines,
            &actor,
        )
        .await?;
        sqlx::query(
            "UPDATE resources.replenishment_need SET status = 'requisitioned', requisition_id = $2, \
             version = version + 1, updated_at = now() WHERE id = ANY($1)",
        )
        .bind(&new_needs)
        .bind(rid)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        result.created_ids.push(rid);
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(result))
}

/// Replenishment needs of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/replenishment", operation_id = "resources_stock_replenishment_list", tag = "resources-inventory",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = ReplenishmentNeedList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list_replenishment(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<ReplenishmentNeedList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, ReplenishmentNeedView>(
        "SELECT n.id, n.institution_id, n.item_id, i.code AS item_code, i.name AS item_name, \
         n.on_hand, n.reserved, n.reorder_level, n.suggested_quantity, n.status, n.requisition_id, \
         n.version, n.created_at FROM resources.replenishment_need n \
         JOIN resources.item i ON i.id = n.item_id WHERE n.tenant_id = $1 AND n.institution_id = $2 \
         AND ($3::text IS NULL OR n.status = $3) ORDER BY n.created_at DESC LIMIT 500",
    )
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(ReplenishmentNeedList { items }))
}

/// Routes of the stock documents.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create_issue, list_issues))
        .routes(routes!(decide_issue))
        .routes(routes!(issue_issue))
        .routes(routes!(return_issue))
        .routes(routes!(cancel_issue))
        .routes(routes!(create_reservation, list_reservations))
        .routes(routes!(release_reservation))
        .routes(routes!(fulfil_reservation))
        .routes(routes!(create_adjustment, list_adjustments))
        .routes(routes!(decide_adjustment))
        .routes(routes!(run_replenishment, list_replenishment))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_transitions() {
        use StockIssueStatus::*;
        assert!(Requested.can_transition(Approved) && Approved.can_transition(Issued));
        assert!(Issued.can_transition(Returned));
        assert!(!Issued.can_transition(Cancelled));
        assert_eq!(
            AdjustmentStatus::parse("applied"),
            Some(AdjustmentStatus::Applied)
        );
        assert_eq!(ReservationStatus::Active.as_str(), "active");
    }
}
