//! **8D** — sourcing: RFQ with quotations → evaluation → award, direct / simplified sourcing (L3-J-004, 007). Operation ids `resources_sourcing_*`, tag `resources-procurement`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * create from a **released** requisition → `open` (`RFQ-` number; one live event per
//!   requisition). Method `rfq` (competitive quotations) or `direct` (simplified sourcing, only up
//!   to [`DIRECT_SOURCING_LIMIT`] of estimated value and with exactly one quotation);
//! * quotations from **active** suppliers price every requisition line (one per supplier);
//! * evaluate (`open → evaluating`): quotations ranked by total (rank 1 = lowest), shortlisted;
//! * award → `AWARD_APPROVAL` → `awarded` (`sourcing_event.awarded`): the quotation is accepted,
//!   the others rejected; awarding anything but the lowest quotation needs a reason;
//! * cancel (reason; an awarded event only while no purchase order uses it).

use crate::requisitions::{self, PurchaseRequisitionLineView, RequisitionStatus};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers::active_supplier;
use crate::suppliers::common::{
    audit_only, check_version, clean, emit, expected, history, illegal, institution_of, invalid,
    line_amount, optional_text, parse_money, reason_of, required_reason, slice_status,
    status_payload, ResourcesDecision, ResourcesDecisionBody, ResourcesReasonBody,
    ResourcesStatusQuery,
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
use std::str::FromStr;
use sutra_contracts::resources::events::procurement as ev;
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

/// Highest estimated value (INR) a requisition may be sourced directly (simplified sourcing).
pub const DIRECT_SOURCING_LIMIT: &str = "100000.00";

fn direct_limit() -> Decimal {
    Decimal::from_str(DIRECT_SOURCING_LIMIT).unwrap_or(Decimal::ZERO)
}

/// Part 1 (DEF-J-005): the direct-purchase limit applies to the **actual** value of what is bought
/// — the quotation, the award and the purchase order total — not only to the requester's
/// estimate. `Err(409)` when `total` is above [`DIRECT_SOURCING_LIMIT`]: the purchase must take the
/// competitive route (an RFQ, evaluated, awarded and approved).
pub fn ensure_within_direct_limit(what: &str, total: Decimal) -> Result<(), KernelError> {
    if total > direct_limit() {
        return Err(KernelError::Conflict(format!(
            "{what} totals {total}; above the direct-purchase limit of {DIRECT_SOURCING_LIMIT} it \
             must be sourced competitively (RFQ, evaluation, approved award)"
        )));
    }
    Ok(())
}

slice_status!(
    /// Sourcing event lifecycle (L3-J-004 / 007).
    SourcingStatus {
        Draft => "draft",
        Open => "open",
        Evaluating => "evaluating",
        Awarded => "awarded",
        Cancelled => "cancelled",
    }
);

slice_status!(
    /// Quotation lifecycle.
    QuotationStatus {
        Received => "received",
        Shortlisted => "shortlisted",
        Accepted => "accepted",
        Rejected => "rejected",
    }
);

impl SourcingStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use SourcingStatus::*;
        matches!(
            (self, to),
            (Draft, Open)
                | (Open, Evaluating)
                | (Open, Awarded)
                | (Evaluating, Awarded)
                | (Draft, Cancelled)
                | (Open, Cancelled)
                | (Evaluating, Cancelled)
                | (Awarded, Cancelled)
        )
    }
}

/// A sourcing event (header).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct SourcingEventView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "RFQ-000001")]
    pub number: String,
    pub requisition_id: Uuid,
    pub requisition_number: String,
    /// `rfq | direct`.
    pub method: String,
    pub title: String,
    pub closing_date: Option<NaiveDate>,
    #[schema(value_type = String, example = "25000.00")]
    pub estimated_value: Decimal,
    /// `draft | open | evaluating | awarded | cancelled`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Quotation proposed for award, awaiting `AWARD_APPROVAL`.
    pub pending_quotation_id: Option<Uuid>,
    pub awarded_quotation_id: Option<Uuid>,
    pub awarded_supplier_id: Option<Uuid>,
    pub award_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub awarded_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl SourcingEventView {
    pub fn status(&self) -> Result<SourcingStatus, KernelError> {
        SourcingStatus::parse(&self.status).ok_or_else(|| {
            KernelError::Conflict(format!("unknown sourcing status {}", self.status))
        })
    }
}

/// A supplier quotation (header).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct QuotationView {
    pub id: Uuid,
    pub sourcing_event_id: Uuid,
    pub supplier_id: Uuid,
    pub supplier_number: String,
    pub supplier_name: String,
    pub reference: Option<String>,
    #[schema(value_type = String, example = "24000.00")]
    pub total_amount: Decimal,
    pub delivery_days: Option<i32>,
    pub valid_until: Option<NaiveDate>,
    /// 1 = lowest total after evaluation.
    pub rank: Option<i32>,
    /// `received | shortlisted | accepted | rejected`.
    pub status: String,
    pub version: i64,
}

/// One priced line of a quotation (requisition line number).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct QuotationLineView {
    pub line_no: i32,
    #[schema(value_type = String, example = "240.00")]
    pub unit_price: Decimal,
    #[schema(value_type = String, example = "2400.00")]
    pub amount: Decimal,
}

/// A quotation with its lines.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuotationDetail {
    #[serde(flatten)]
    pub quotation: QuotationView,
    pub lines: Vec<QuotationLineView>,
}

/// A sourcing event with the demand lines and the quotations.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourcingEventDetail {
    #[serde(flatten)]
    pub event: SourcingEventView,
    pub lines: Vec<PurchaseRequisitionLineView>,
    pub quotations: Vec<QuotationDetail>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SourcingEventList {
    pub items: Vec<SourcingEventView>,
}

const SELECT: &str = "SELECT e.id, e.institution_id, e.number, e.requisition_id, \
    r.number AS requisition_number, e.method, e.title, e.closing_date, e.estimated_value, \
    e.status, e.status_reason, e.pending_quotation_id, e.awarded_quotation_id, \
    e.awarded_supplier_id, e.award_reason, e.process_instance_id, e.awarded_at, e.version, \
    e.created_at FROM resources.sourcing_event e \
    JOIN resources.purchase_requisition r ON r.id = e.requisition_id";

const QSELECT: &str =
    "SELECT q.id, q.sourcing_event_id, q.supplier_id, s.number AS supplier_number, \
    s.legal_name AS supplier_name, q.reference, q.total_amount, q.delivery_days, q.valid_until, \
    q.rank, q.status, q.version FROM resources.quotation q \
    JOIN resources.supplier s ON s.id = q.supplier_id";

/// A sourcing event of the tenant (404 when missing).
pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<SourcingEventView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE OF e" } else { "" };
    sqlx::query_as::<_, SourcingEventView>(&format!(
        "{SELECT} WHERE e.id = $1 AND e.tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "sourcing event",
    })
}

/// Quotations of an event (rank, then total).
pub async fn quotations_of(
    conn: &mut PgConnection,
    event: Uuid,
) -> Result<Vec<QuotationView>, KernelError> {
    sqlx::query_as::<_, QuotationView>(&format!(
        "{QSELECT} WHERE q.sourcing_event_id = $1 ORDER BY q.rank NULLS LAST, q.total_amount, q.created_at"
    ))
    .bind(event)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Priced lines of a quotation.
pub async fn quotation_lines(
    conn: &mut PgConnection,
    quotation: Uuid,
) -> Result<Vec<QuotationLineView>, KernelError> {
    sqlx::query_as::<_, QuotationLineView>(
        "SELECT line_no, unit_price, amount FROM resources.quotation_line \
         WHERE quotation_id = $1 ORDER BY line_no",
    )
    .bind(quotation)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn detail(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<SourcingEventDetail, KernelError> {
    let event = load(conn, tenant, id, false).await?;
    let lines = requisitions::lines_of(conn, event.requisition_id).await?;
    let mut quotations = Vec::new();
    for q in quotations_of(conn, id).await? {
        let lines = quotation_lines(conn, q.id).await?;
        quotations.push(QuotationDetail {
            quotation: q,
            lines,
        });
    }
    Ok(SourcingEventDetail {
        event,
        lines,
        quotations,
    })
}

async fn respond(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let d = detail(conn, tenant, id).await?;
    let v = d.event.version;
    Ok(with_etag(status, &d, Version(v)))
}

async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    e: &SourcingEventView,
    to: SourcingStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = e.status()?;
    if !from.can_transition(to) {
        return Err(illegal(
            "sourcing event",
            &e.number,
            from.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE resources.sourcing_event SET status = $2, status_reason = COALESCE($3, status_reason), \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(e.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(e.institution_id),
        "sourcing_event",
        e.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        e.process_instance_id.as_deref(),
        actor,
    )
    .await
}

async fn event_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(
        &mut conn,
        ctx.scope.tenant_id,
        "sourcing_event",
        "sourcing event",
        id,
    )
    .await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// Award: accepts `quotation`, rejects the others, records supplier + reason, `awarded`.
#[allow(clippy::too_many_arguments)]
async fn apply_award(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    id: Uuid,
    quotation: Uuid,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let e = load(conn, tenant, id, true).await?;
    let q = quotations_of(conn, id)
        .await?
        .into_iter()
        .find(|x| x.id == quotation)
        .ok_or(KernelError::NotFound {
            entity: "quotation",
        })?;
    active_supplier(
        conn,
        tenant,
        InstitutionId(e.institution_id),
        "quotationId",
        q.supplier_id,
    )
    .await?;
    set_status(
        conn,
        tenant,
        &e,
        SourcingStatus::Awarded,
        ev::SOURCING_EVENT_AWARDED,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.sourcing_event SET awarded_quotation_id = $2, awarded_supplier_id = $3, \
         award_reason = $4, pending_quotation_id = NULL, awarded_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(q.id)
    .bind(q.supplier_id)
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE resources.quotation SET status = CASE WHEN id = $2 THEN 'accepted' ELSE 'rejected' END, \
         version = version + 1, updated_at = now() WHERE sourcing_event_id = $1",
    )
    .bind(id)
    .bind(q.id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let out = load(conn, tenant, id, false).await?;
    emit(
        conn,
        scope,
        ev::SOURCING_EVENT_AWARDED,
        ev::SOURCING_EVENT_AWARDED_SCHEMA,
        "sourcing_event",
        id,
        "resources.sourcing_event.award",
        &status_payload(
            id,
            out.institution_id,
            Some(e.status.as_str()),
            "awarded",
            out.version,
            Some(&format!("{} {}", q.supplier_number, q.total_amount)),
            out.process_instance_id.as_deref(),
            Some(&out.number),
            None,
        ),
        actor,
    )
    .await
}

/// Start sourcing a released requisition.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourcingEventCreate {
    pub requisition_id: Uuid,
    /// `rfq` (competitive) or `direct` (simplified, small value).
    pub method: String,
    pub title: Option<String>,
    pub closing_date: Option<NaiveDate>,
}

/// Open a sourcing event for a released requisition (`resources.procurement.manage`;
/// Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/sourcing-events", operation_id = "resources_sourcing_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = SourcingEventCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SourcingEventDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SourcingEventCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let method = b.method.trim().to_owned();
    if !matches!(method.as_str(), "rfq" | "direct") {
        return Err(invalid("method", "invalid", "rfq or direct").into());
    }
    let title = optional_text("title", b.title, 200)?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = match requisitions::load(&mut tx, t, b.requisition_id, true).await {
        Ok(r) if r.institution_id == id => r,
        Ok(_) | Err(KernelError::NotFound { .. }) => {
            return Err(invalid(
                "requisitionId",
                "unknown_requisition",
                "no such requisition in the institution",
            )
            .into())
        }
        Err(e) => return Err(e.into()),
    };
    if r.status()? != RequisitionStatus::Released {
        return Err(KernelError::Conflict(format!(
            "purchase requisition {} is {}; only released demand is sourced",
            r.number, r.status
        ))
        .into());
    }
    if method == "direct" && r.estimated_total > direct_limit() {
        return Err(KernelError::Conflict(format!(
            "purchase requisition {} is estimated at {}; direct sourcing is limited to {DIRECT_SOURCING_LIMIT}, use an RFQ",
            r.number, r.estimated_total
        ))
        .into());
    }
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resources.sourcing_event WHERE requisition_id = $1 \
         AND status IN ('draft', 'open', 'evaluating', 'awarded')",
    )
    .bind(r.id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if live > 0 {
        return Err(KernelError::Conflict(format!(
            "purchase requisition {} is already being sourced",
            r.number
        ))
        .into());
    }
    let number = next(&mut tx, t, institution, kind::SOURCING_EVENT).await?;
    let eid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.sourcing_event (tenant_id, institution_id, number, requisition_id, \
         method, title, closing_date, estimated_value, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, 'open') RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(r.id)
    .bind(&method)
    .bind(title.unwrap_or_else(|| r.title.clone()))
    .bind(b.closing_date)
    .bind(r.estimated_total)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "sourcing_event",
        eid,
        None,
        "open",
        "sutraerp.resources.sourcing_event.opened",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "sourcing_event",
        eid,
        "resources.sourcing_event.create",
        serde_json::json!({ "number": number, "method": method, "requisition": r.number }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, eid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Sourcing events of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/sourcing-events", operation_id = "resources_sourcing_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = SourcingEventList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<SourcingEventList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, SourcingEventView>(&format!(
        "{SELECT} WHERE e.tenant_id = $1 AND e.institution_id = $2 \
         AND ($3::text IS NULL OR e.status = $3) ORDER BY e.created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(SourcingEventList { items }))
}

/// One sourcing event with demand lines and quotations (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/sourcing-events/{id}", operation_id = "resources_sourcing_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SourcingEventDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_one(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// One priced requisition line of a quotation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuotationLineInput {
    pub line_no: i32,
    #[schema(example = "240.00")]
    pub unit_price: String,
}

/// A supplier quotation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuotationCreate {
    pub supplier_id: Uuid,
    pub reference: Option<String>,
    pub delivery_days: Option<i32>,
    pub valid_until: Option<NaiveDate>,
    /// One price per requisition line.
    pub lines: Vec<QuotationLineInput>,
}

/// Record a supplier's quotation (`resources.procurement.manage`; If-Match on the event): the
/// event is open, the supplier active, every requisition line priced; one quotation per
/// supplier; a direct event takes exactly one.
#[utoipa::path(post, path = "/api/v1/resources/sourcing-events/{id}/quotations", operation_id = "resources_sourcing_quote", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the sourcing event")),
    request_body = QuotationCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SourcingEventDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn quote(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<QuotationCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let reference = optional_text("reference", b.reference, 100)?;
    if b.delivery_days.is_some_and(|d| d < 0) {
        return Err(invalid("deliveryDays", "negative", "must not be negative").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = event_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = load(&mut tx, t, id, true).await?;
    check_version(v, e.version)?;
    if e.status()? != SourcingStatus::Open {
        return Err(KernelError::Conflict(format!(
            "sourcing event {} is {}; quotations are taken while it is open",
            e.number, e.status
        ))
        .into());
    }
    let s = active_supplier(
        &mut tx,
        t,
        InstitutionId(e.institution_id),
        "supplierId",
        b.supplier_id,
    )
    .await?;
    let existing = quotations_of(&mut tx, id).await?;
    if existing.iter().any(|q| q.supplier_id == s.id) {
        return Err(KernelError::Conflict(format!(
            "supplier {} has already quoted on {}",
            s.number, e.number
        ))
        .into());
    }
    if e.method == "direct" && !existing.is_empty() {
        return Err(KernelError::Conflict(format!(
            "{} is a direct sourcing event; it takes one quotation",
            e.number
        ))
        .into());
    }
    let demand = requisitions::lines_of(&mut tx, e.requisition_id).await?;
    if b.lines.len() != demand.len() {
        return Err(invalid("lines", "incomplete", "price every requisition line once").into());
    }
    let mut priced: Vec<(i32, Decimal, Decimal)> = Vec::with_capacity(demand.len());
    for (i, l) in b.lines.iter().enumerate() {
        let d = demand
            .iter()
            .find(|d| d.line_no == l.line_no)
            .ok_or_else(|| {
                invalid(
                    &format!("lines[{i}].lineNo"),
                    "unknown_line",
                    "no such requisition line",
                )
            })?;
        if priced.iter().any(|p| p.0 == l.line_no) {
            return Err(
                invalid(&format!("lines[{i}].lineNo"), "duplicate", "each line once").into(),
            );
        }
        let price = parse_money(&format!("lines[{i}].unitPrice"), &l.unit_price)?;
        priced.push((l.line_no, price, line_amount(d.quantity, price)));
    }
    let total: Decimal = priced.iter().map(|p| p.2).sum();
    if e.method == "direct" {
        // Part 1 (DEF-J-005): a direct event cannot take a quotation above the limit.
        ensure_within_direct_limit(
            &format!("the quotation on direct event {}", e.number),
            total,
        )?;
    }
    let qid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.quotation (tenant_id, institution_id, sourcing_event_id, supplier_id, \
         reference, total_amount, delivery_days, valid_until) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(e.institution_id)
    .bind(id)
    .bind(s.id)
    .bind(&reference)
    .bind(total)
    .bind(b.delivery_days)
    .bind(b.valid_until)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for (line_no, price, amount) in &priced {
        sqlx::query(
            "INSERT INTO resources.quotation_line (tenant_id, quotation_id, line_no, unit_price, amount) \
             VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(t.as_uuid())
        .bind(qid)
        .bind(*line_no)
        .bind(*price)
        .bind(*amount)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    sqlx::query(
        "UPDATE resources.sourcing_event SET version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "sourcing_event",
        id,
        "resources.sourcing_event.quote",
        serde_json::json!({ "number": e.number, "supplier": s.number, "total": total.to_string() }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Evaluate the quotations (`resources.procurement.manage`; If-Match): `open → evaluating`;
/// quotations ranked by total (1 = lowest) and shortlisted.
#[utoipa::path(post, path = "/api/v1/resources/sourcing-events/{id}/evaluate", operation_id = "resources_sourcing_evaluate", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the sourcing event")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SourcingEventDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn evaluate(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = event_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = load(&mut tx, t, id, true).await?;
    check_version(v, e.version)?;
    let quotes = quotations_of(&mut tx, id).await?;
    if quotes.is_empty() {
        return Err(KernelError::Conflict(format!(
            "sourcing event {} has no quotation to evaluate",
            e.number
        ))
        .into());
    }
    set_status(
        &mut tx,
        t,
        &e,
        SourcingStatus::Evaluating,
        "sutraerp.resources.sourcing_event.evaluating",
        None,
        &actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.quotation q SET rank = r.rank, status = 'shortlisted', \
         version = q.version + 1, updated_at = now() \
         FROM (SELECT id, (row_number() OVER (ORDER BY total_amount, created_at))::int AS rank \
               FROM resources.quotation WHERE sourcing_event_id = $1) r \
         WHERE q.id = r.id",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "sourcing_event",
        id,
        "resources.sourcing_event.evaluate",
        serde_json::json!({ "number": e.number, "quotations": quotes.len() }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Award the event to one quotation.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SourcingAward {
    pub quotation_id: Uuid,
    /// Required when the quotation is not the lowest.
    pub reason: Option<String>,
}

/// Award (`resources.procurement.manage`; If-Match; `AWARD_APPROVAL`): RFQ events after
/// evaluation, direct events once their quotation is in.
#[utoipa::path(post, path = "/api/v1/resources/sourcing-events/{id}/award", operation_id = "resources_sourcing_award", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the sourcing event")),
    request_body = SourcingAward, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SourcingEventDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn award(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SourcingAward>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    let t = ctx.scope.tenant_id;
    let scope = event_scope(&st, ctx, id).await?;
    let (e, quotes) = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let e = load(&mut conn, t, id, false).await?;
        let q = quotations_of(&mut conn, id).await?;
        (e, q)
    };
    check_version(v, e.version)?;
    let status = e.status()?;
    let ok_state = match e.method.as_str() {
        "direct" => matches!(status, SourcingStatus::Open | SourcingStatus::Evaluating),
        _ => status == SourcingStatus::Evaluating,
    };
    if !ok_state || e.pending_quotation_id.is_some() {
        return Err(illegal("sourcing event", &e.number, &e.status, "awarded").into());
    }
    let q = quotes
        .iter()
        .find(|q| q.id == b.quotation_id)
        .ok_or_else(|| {
            invalid(
                "quotationId",
                "unknown_quotation",
                "no such quotation on this event",
            )
        })?;
    let lowest = quotes
        .iter()
        .map(|x| x.total_amount)
        .min()
        .unwrap_or(q.total_amount);
    if e.method == "direct" {
        // Part 1 (DEF-J-005): defence in depth — never award a direct event above the limit.
        ensure_within_direct_limit(
            &format!("the quotation awarded on direct event {}", e.number),
            q.total_amount,
        )?;
    }
    if q.total_amount > lowest && reason.is_none() {
        return Err(invalid(
            "reason",
            "required",
            "awarding a quotation that is not the lowest needs a justification",
        )
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("sourcingNumber".to_owned(), serde_json::json!(e.number));
    vars.insert("supplier".to_owned(), serde_json::json!(q.supplier_number));
    vars.insert(
        "total".to_owned(),
        serde_json::json!(q.total_amount.to_string()),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::AWARD_APPROVAL,
        business_key("sourcing_event", id, e.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = load(&mut tx, t, id, true).await?;
    check_version(v, e.version)?;
    sqlx::query(
        "UPDATE resources.sourcing_event SET pending_quotation_id = $2, award_reason = $3, \
         process_instance_id = $4, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(q.id)
    .bind(&reason)
    .bind(outcome.process_instance_id())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "sourcing_event",
        id,
        "resources.sourcing_event.propose_award",
        serde_json::json!({ "number": e.number, "quotation": q.id, "reason": reason }),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        apply_award(
            &mut tx,
            &scope,
            t,
            id,
            q.id,
            reason.as_deref(),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Decide a pending award (`resources.approve`, step-up; If-Match): approve → awarded; reject →
/// the proposal is dropped (reason required) and another quotation may be proposed.
#[utoipa::path(post, path = "/api/v1/resources/sourcing-events/{id}/decision", operation_id = "resources_sourcing_decide", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the sourcing event")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SourcingEventDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ResourcesDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesApprove)?;
    let v = expected!(headers);
    let reason = clean(b.reason);
    if b.decision == ResourcesDecision::Reject && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason").into());
    }
    let t = ctx.scope.tenant_id;
    let scope = event_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = load(&mut tx, t, id, true).await?;
    check_version(v, e.version)?;
    let Some(pending) = e.pending_quotation_id else {
        return Err(illegal("sourcing event", &e.number, &e.status, "decided").into());
    };
    match b.decision {
        ResourcesDecision::Approve => {
            let r = e.award_reason.clone();
            apply_award(&mut tx, &scope, t, id, pending, r.as_deref(), &actor).await?
        }
        ResourcesDecision::Reject => {
            sqlx::query(
                "UPDATE resources.sourcing_event SET pending_quotation_id = NULL, award_reason = NULL, \
                 status_reason = $2, version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&reason)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            audit_only(
                &mut tx,
                &scope,
                "sourcing_event",
                id,
                "resources.sourcing_event.reject_award",
                serde_json::json!({ "number": e.number, "reason": reason }),
                &actor,
            )
            .await?;
        }
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Cancel a sourcing event (`resources.procurement.manage`; If-Match; reason required); an
/// awarded event only while no live purchase order uses it.
#[utoipa::path(post, path = "/api/v1/resources/sourcing-events/{id}/cancel", operation_id = "resources_sourcing_cancel", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the sourcing event")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SourcingEventDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cancel(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let reason = required_reason(reason_of(body))?;
    let t = ctx.scope.tenant_id;
    let scope = event_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let e = load(&mut tx, t, id, true).await?;
    check_version(v, e.version)?;
    let pos: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resources.purchase_order WHERE sourcing_event_id = $1 \
         AND status <> 'cancelled'",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if pos > 0 {
        return Err(KernelError::Conflict(format!(
            "sourcing event {} has a purchase order; cancel the order first",
            e.number
        ))
        .into());
    }
    set_status(
        &mut tx,
        t,
        &e,
        SourcingStatus::Cancelled,
        "sutraerp.resources.sourcing_event.cancelled",
        Some(&reason),
        &actor,
    )
    .await?;
    sqlx::query("UPDATE resources.sourcing_event SET pending_quotation_id = NULL WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "sourcing_event",
        id,
        "resources.sourcing_event.cancel",
        serde_json::json!({ "number": e.number, "reason": reason }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Routes of this area.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get_one))
        .routes(routes!(quote))
        .routes(routes!(evaluate))
        .routes(routes!(award))
        .routes(routes!(decide))
        .routes(routes!(cancel))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sourcing_transitions_and_limit() {
        use SourcingStatus::*;
        assert!(Open.can_transition(Evaluating) && Evaluating.can_transition(Awarded));
        assert!(Open.can_transition(Awarded), "direct sourcing");
        assert!(!Cancelled.can_transition(Open));
        assert_eq!(direct_limit().to_string(), "100000.00");
        let at = Decimal::from_str("100000.00").unwrap_or_default();
        assert!(ensure_within_direct_limit("x", at).is_ok());
        assert!(matches!(
            ensure_within_direct_limit("x", at + Decimal::new(1, 2)),
            Err(KernelError::Conflict(_))
        ));
        assert_eq!(
            QuotationStatus::parse("accepted"),
            Some(QuotationStatus::Accepted)
        );
    }
}
