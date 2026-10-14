//! **8D** — purchase requisitions: draft → submitted → approved / released or rejected, amendment, withdrawal, cancellation (L3-J-038 … 041). Operation ids `resources_requisition_*`, tag `resources-procurement`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * create → `draft` (`PR-` number) with lines (an active item or free text, quantity,
//!   estimated unit cost), org unit through `InstitutionReferencePort`, requester through
//!   `WorkforceDirectory` (defaults to the caller's staff record);
//! * submit → `REQUISITION_APPROVAL`: auto-approval releases it at once (submitted → approved →
//!   `released`, `purchase_requisition.approved`); with `AwaitDecision` it stays `submitted`
//!   (`purchase_requisition.submitted` → administrators' inbox) until the decision endpoint;
//!   rejection → `rejected` (`purchase_requisition.rejected`, reason required);
//! * amend (new revision, re-approval): draft keeps `draft`; rejected / withdrawn / approved /
//!   released → `amended`; withdraw (submitted → `withdrawn`); cancel (reason; not once ordered or
//!   while a live sourcing event uses it). A purchase order marks it `ordered` → `closed`.

use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers::common::{
    active_item, audit_only, check_org_unit, check_staff, check_version, clean, emit, expected,
    history, illegal, institution_of, invalid, line_amount, optional_text, parse_money, parse_qty,
    reason_of, required_reason, required_text, slice_status, status_payload, ResourcesDecision,
    ResourcesDecisionBody, ResourcesReasonBody, ResourcesStatusQuery,
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
    /// Purchase requisition lifecycle (L2-J03).
    RequisitionStatus {
        Draft => "draft",
        Submitted => "submitted",
        Approved => "approved",
        Released => "released",
        Rejected => "rejected",
        Amended => "amended",
        Withdrawn => "withdrawn",
        Cancelled => "cancelled",
        Ordered => "ordered",
        Closed => "closed",
    }
);

impl RequisitionStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use RequisitionStatus::*;
        matches!(
            (self, to),
            (Draft, Submitted)
                | (Draft, Cancelled)
                | (Amended, Submitted)
                | (Amended, Cancelled)
                | (Submitted, Approved)
                | (Submitted, Rejected)
                | (Submitted, Withdrawn)
                | (Approved, Released)
                | (Approved, Amended)
                | (Approved, Cancelled)
                | (Released, Ordered)
                | (Released, Amended)
                | (Released, Cancelled)
                | (Rejected, Amended)
                | (Rejected, Cancelled)
                | (Withdrawn, Amended)
                | (Withdrawn, Cancelled)
                | (Ordered, Released)
                | (Ordered, Closed)
        )
    }
}

/// One requisition line.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseRequisitionLineView {
    pub line_no: i32,
    pub item_id: Option<Uuid>,
    pub item_code: Option<String>,
    pub description: String,
    #[schema(value_type = String, example = "10.000")]
    pub quantity: Decimal,
    pub unit: String,
    #[schema(value_type = String, example = "250.00")]
    pub estimated_unit_cost: Decimal,
    #[schema(value_type = String, example = "2500.00")]
    pub estimated_amount: Decimal,
}

/// A purchase requisition (header).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseRequisitionView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "PR-000001")]
    pub number: String,
    pub title: String,
    pub justification: Option<String>,
    pub org_unit_id: Option<Uuid>,
    pub requester_staff_id: Option<Uuid>,
    pub needed_by: Option<NaiveDate>,
    pub revision: i32,
    #[schema(value_type = String, example = "2500.00")]
    pub estimated_total: Decimal,
    /// `draft | submitted | approved | released | rejected | amended | withdrawn | cancelled | ordered | closed`.
    pub status: String,
    pub status_reason: Option<String>,
    /// `manual | replenishment`.
    pub source: String,
    pub process_instance_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decided_by: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub requester_subject: Option<String>,
}

impl PurchaseRequisitionView {
    pub fn status(&self) -> Result<RequisitionStatus, KernelError> {
        RequisitionStatus::parse(&self.status).ok_or_else(|| {
            KernelError::Conflict(format!("unknown requisition status {}", self.status))
        })
    }
}

/// A purchase requisition with its lines.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseRequisitionDetail {
    #[serde(flatten)]
    pub requisition: PurchaseRequisitionView,
    pub lines: Vec<PurchaseRequisitionLineView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct PurchaseRequisitionList {
    pub items: Vec<PurchaseRequisitionView>,
}

const SELECT: &str = "SELECT id, institution_id, number, title, justification, org_unit_id, \
    requester_staff_id, needed_by, revision, estimated_total, status, status_reason, source, \
    process_instance_id, submitted_at, decided_at, decided_by, version, created_at, \
    requester_subject FROM resources.purchase_requisition";

/// A requisition of the tenant (404 when missing).
pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<PurchaseRequisitionView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE" } else { "" };
    sqlx::query_as::<_, PurchaseRequisitionView>(&format!(
        "{SELECT} WHERE id = $1 AND tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "purchase requisition",
    })
}

/// Lines of a requisition, in line order.
pub async fn lines_of(
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<Vec<PurchaseRequisitionLineView>, KernelError> {
    sqlx::query_as::<_, PurchaseRequisitionLineView>(
        "SELECT l.line_no, l.item_id, i.code AS item_code, l.description, l.quantity, l.unit, \
         l.estimated_unit_cost, l.estimated_amount FROM resources.purchase_requisition_line l \
         LEFT JOIN resources.item i ON i.id = l.item_id WHERE l.requisition_id = $1 \
         ORDER BY l.line_no",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

async fn detail(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<PurchaseRequisitionDetail, KernelError> {
    let requisition = load(conn, tenant, id, false).await?;
    let lines = lines_of(conn, id).await?;
    Ok(PurchaseRequisitionDetail { requisition, lines })
}

async fn respond(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let d = detail(conn, tenant, id).await?;
    let v = d.requisition.version;
    Ok(with_etag(status, &d, Version(v)))
}

/// Moves a requisition (row locked by the caller) to `to`: legal-transition check, update,
/// history. Used by sourcing / purchase orders (`ordered`, back to `released`, `closed`).
pub async fn change_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &PurchaseRequisitionView,
    to: RequisitionStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = r.status()?;
    if !from.can_transition(to) {
        return Err(illegal(
            "purchase requisition",
            &r.number,
            from.as_str(),
            to.as_str(),
        ));
    }
    sqlx::query(
        "UPDATE resources.purchase_requisition SET status = $2, status_reason = $3, \
         submitted_at = CASE WHEN $2 = 'submitted' THEN now() ELSE submitted_at END, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(r.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(r.institution_id),
        "purchase_requisition",
        r.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        r.process_instance_id.as_deref(),
        actor,
    )
    .await
}

/// One line of a create / amend body: an active item **or** free text.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseRequisitionLineInput {
    pub item_id: Option<Uuid>,
    /// Required for free-text lines (defaults to the item name).
    pub description: Option<String>,
    #[schema(example = "10")]
    pub quantity: String,
    pub unit: Option<String>,
    /// Defaults to the item's standard cost; required for free-text lines.
    #[schema(example = "250.00")]
    pub estimated_unit_cost: Option<String>,
}

/// Create a requisition.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseRequisitionCreate {
    pub title: String,
    pub justification: Option<String>,
    pub org_unit_id: Option<Uuid>,
    /// Defaults to the caller's staff record (when the caller is staff).
    pub requester_staff_id: Option<Uuid>,
    pub needed_by: Option<NaiveDate>,
    pub lines: Vec<PurchaseRequisitionLineInput>,
}

/// Validated line.
#[derive(Debug, Clone)]
pub struct CleanLine {
    pub item_id: Option<Uuid>,
    pub description: String,
    pub quantity: Decimal,
    pub unit: String,
    pub unit_cost: Decimal,
    pub amount: Decimal,
}

async fn clean_lines(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    lines: Vec<PurchaseRequisitionLineInput>,
) -> Result<Vec<CleanLine>, KernelError> {
    if lines.is_empty() {
        return Err(invalid("lines", "required", "at least one line"));
    }
    if lines.len() > 200 {
        return Err(invalid("lines", "too_many", "at most 200 lines"));
    }
    let mut out = Vec::with_capacity(lines.len());
    for (i, l) in lines.into_iter().enumerate() {
        let f = |name: &str| format!("lines[{i}].{name}");
        let quantity = parse_qty(&f("quantity"), &l.quantity)?;
        let (description, unit, default_cost) = match l.item_id {
            Some(item) => {
                let it = active_item(conn, tenant, institution, &f("itemId"), item).await?;
                (
                    clean(l.description).unwrap_or_else(|| it.name.clone()),
                    clean(l.unit).unwrap_or_else(|| it.unit.clone()),
                    it.standard_cost,
                )
            }
            None => (
                clean(l.description).ok_or_else(|| {
                    invalid(
                        &f("description"),
                        "required",
                        "a free-text line needs a description",
                    )
                })?,
                clean(l.unit).unwrap_or_else(|| "each".to_owned()),
                None,
            ),
        };
        if description.chars().count() > 300 {
            return Err(invalid(
                &f("description"),
                "too_long",
                "at most 300 characters",
            ));
        }
        let unit_cost = match l.estimated_unit_cost {
            Some(c) => parse_money(&f("estimatedUnitCost"), &c)?,
            None => default_cost.ok_or_else(|| {
                invalid(
                    &f("estimatedUnitCost"),
                    "required",
                    "an estimated unit cost is required",
                )
            })?,
        };
        out.push(CleanLine {
            item_id: l.item_id,
            description,
            quantity,
            unit,
            unit_cost,
            amount: line_amount(quantity, unit_cost),
        });
    }
    Ok(out)
}

/// Replaces the lines of a requisition and recomputes its estimate.
pub async fn write_lines(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lines: &[CleanLine],
) -> Result<(), KernelError> {
    sqlx::query("DELETE FROM resources.purchase_requisition_line WHERE requisition_id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut total = Decimal::ZERO;
    for (i, l) in lines.iter().enumerate() {
        total += l.amount;
        sqlx::query(
            "INSERT INTO resources.purchase_requisition_line (tenant_id, requisition_id, line_no, \
             item_id, description, quantity, unit, estimated_unit_cost, estimated_amount) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        )
        .bind(tenant.as_uuid())
        .bind(id)
        .bind(i as i32 + 1)
        .bind(l.item_id)
        .bind(&l.description)
        .bind(l.quantity)
        .bind(&l.unit)
        .bind(l.unit_cost)
        .bind(l.amount)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    }
    sqlx::query("UPDATE resources.purchase_requisition SET estimated_total = $2 WHERE id = $1")
        .bind(id)
        .bind(total)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    Ok(())
}

/// Inserts a draft requisition (also used by the replenishment run). Returns its id.
#[allow(clippy::too_many_arguments)]
pub async fn insert_draft(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    institution: InstitutionId,
    title: &str,
    justification: Option<&str>,
    org_unit_id: Option<Uuid>,
    requester: Option<(Uuid, Option<String>)>,
    needed_by: Option<NaiveDate>,
    source: &str,
    lines: &[CleanLine],
    actor: &Actor,
) -> Result<Uuid, KernelError> {
    let number = next(conn, tenant, institution, kind::REQUISITION).await?;
    let (staff, subject) = match requester {
        Some((s, subj)) => (Some(s), subj),
        None => (None, None),
    };
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.purchase_requisition (tenant_id, institution_id, number, title, \
         justification, org_unit_id, requester_staff_id, requester_subject, needed_by, source) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(&number)
    .bind(title)
    .bind(justification)
    .bind(org_unit_id)
    .bind(staff)
    .bind(&subject)
    .bind(needed_by)
    .bind(source)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    write_lines(conn, tenant, id, lines).await?;
    history(
        conn,
        tenant,
        institution,
        "purchase_requisition",
        id,
        None,
        "draft",
        "sutraerp.resources.purchase_requisition.created",
        None,
        None,
        actor,
    )
    .await?;
    audit_only(
        conn,
        scope,
        "purchase_requisition",
        id,
        "resources.purchase_requisition.create",
        serde_json::json!({ "number": number, "source": source, "lines": lines.len() }),
        actor,
    )
    .await?;
    Ok(id)
}

/// Clean lines for a replenishment draft (`item`, quantity, standard cost or zero).
pub fn replenishment_line(
    item_id: Uuid,
    name: &str,
    unit: &str,
    quantity: Decimal,
    standard_cost: Option<Decimal>,
) -> CleanLine {
    let cost = standard_cost.unwrap_or(Decimal::ZERO);
    CleanLine {
        item_id: Some(item_id),
        description: name.to_owned(),
        quantity,
        unit: unit.to_owned(),
        unit_cost: cost,
        amount: line_amount(quantity, cost),
    }
}

async fn approve(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    id: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let r = load(conn, tenant, id, true).await?;
    change_status(
        conn,
        tenant,
        &r,
        RequisitionStatus::Approved,
        ev::PURCHASE_REQUISITION_APPROVED,
        None,
        actor,
    )
    .await?;
    let r = load(conn, tenant, id, true).await?;
    change_status(
        conn,
        tenant,
        &r,
        RequisitionStatus::Released,
        ev::PURCHASE_REQUISITION_APPROVED,
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.purchase_requisition SET decided_at = now(), decided_by = $2 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let out = load(conn, tenant, id, false).await?;
    emit(
        conn,
        scope,
        ev::PURCHASE_REQUISITION_APPROVED,
        ev::PURCHASE_REQUISITION_APPROVED_SCHEMA,
        "purchase_requisition",
        id,
        "resources.purchase_requisition.approve",
        &status_payload(
            id,
            out.institution_id,
            Some("submitted"),
            out.status.as_str(),
            out.version,
            None,
            out.process_instance_id.as_deref(),
            Some(&out.number),
            out.requester_subject.as_deref(),
        ),
        actor,
    )
    .await
}

async fn requisition_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(
        &mut conn,
        ctx.scope.tenant_id,
        "purchase_requisition",
        "purchase requisition",
        id,
    )
    .await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// Raise a purchase requisition (`resources.procurement.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/requisitions", operation_id = "resources_requisition_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = PurchaseRequisitionCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = PurchaseRequisitionDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PurchaseRequisitionCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let title = required_text("title", &b.title, 200)?;
    let justification = optional_text("justification", b.justification, 2000)?;
    check_org_unit(&st, &scope, institution, "orgUnitId", b.org_unit_id).await?;
    let requester = match b.requester_staff_id {
        Some(s) => check_staff(&st, &scope, institution, "requesterStaffId", Some(s)).await?,
        None => st
            .workforce
            .staff_by_subject(&scope, &ctx.subject)
            .await?
            .filter(|s| s.institution_id == institution),
    };
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let lines = clean_lines(&mut tx, t, institution, b.lines).await?;
    let rid = insert_draft(
        &mut tx,
        &scope,
        t,
        institution,
        &title,
        justification.as_deref(),
        b.org_unit_id,
        requester.map(|s| (s.id.as_uuid(), s.subject_ref.clone())),
        b.needed_by,
        "manual",
        &lines,
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, rid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Requisitions of an institution (`resources.read`; `?status=`), newest first.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/requisitions", operation_id = "resources_requisition_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = PurchaseRequisitionList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<PurchaseRequisitionList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, PurchaseRequisitionView>(&format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(PurchaseRequisitionList { items }))
}

/// One requisition with its lines (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/requisitions/{id}", operation_id = "resources_requisition_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = PurchaseRequisitionDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_one(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Submit for approval (`resources.procurement.manage`; If-Match; `REQUISITION_APPROVAL`).
#[utoipa::path(post, path = "/api/v1/resources/requisitions/{id}/submit", operation_id = "resources_requisition_submit", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseRequisitionDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submit(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = requisition_scope(&st, ctx, id).await?;
    let r = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    check_version(v, r.version)?;
    let from = r.status()?;
    if !from.can_transition(RequisitionStatus::Submitted) {
        return Err(illegal(
            "purchase requisition",
            &r.number,
            from.as_str(),
            "submitted",
        )
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("requisitionNumber".to_owned(), serde_json::json!(r.number));
    vars.insert(
        "estimatedTotal".to_owned(),
        serde_json::json!(r.estimated_total.to_string()),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::REQUISITION_APPROVAL,
        business_key("purchase_requisition", id, r.version),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    sqlx::query(
        "UPDATE resources.purchase_requisition SET process_instance_id = $2, decided_at = NULL, \
         decided_by = NULL WHERE id = $1",
    )
    .bind(id)
    .bind(&pid)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    change_status(
        &mut tx,
        t,
        &r,
        RequisitionStatus::Submitted,
        ev::PURCHASE_REQUISITION_SUBMITTED,
        None,
        &actor,
    )
    .await?;
    match outcome {
        ApprovalOutcome::Approved { .. } => {
            audit_only(
                &mut tx,
                &scope,
                "purchase_requisition",
                id,
                "resources.purchase_requisition.submit",
                serde_json::json!({ "number": r.number, "processInstanceId": pid }),
                &actor,
            )
            .await?;
            approve(
                &mut tx,
                &scope,
                t,
                id,
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?;
        }
        ApprovalOutcome::Pending { .. } => {
            let out = load(&mut tx, t, id, false).await?;
            emit(
                &mut tx,
                &scope,
                ev::PURCHASE_REQUISITION_SUBMITTED,
                ev::PURCHASE_REQUISITION_SUBMITTED_SCHEMA,
                "purchase_requisition",
                id,
                "resources.purchase_requisition.submit",
                &status_payload(
                    id,
                    out.institution_id,
                    Some(from.as_str()),
                    "submitted",
                    out.version,
                    Some(&format!("{} {}", out.number, out.title)),
                    Some(&pid),
                    Some(&out.number),
                    out.requester_subject.as_deref(),
                ),
                &actor,
            )
            .await?;
        }
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Decide a submitted requisition (`resources.approve`, step-up; If-Match): approve → released;
/// reject → rejected (reason required).
#[utoipa::path(post, path = "/api/v1/resources/requisitions/{id}/decision", operation_id = "resources_requisition_decide", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseRequisitionDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
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
    let scope = requisition_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    if r.status()? != RequisitionStatus::Submitted {
        return Err(illegal("purchase requisition", &r.number, &r.status, "decided").into());
    }
    match b.decision {
        ResourcesDecision::Approve => approve(&mut tx, &scope, t, id, &actor).await?,
        ResourcesDecision::Reject => {
            change_status(
                &mut tx,
                t,
                &r,
                RequisitionStatus::Rejected,
                ev::PURCHASE_REQUISITION_REJECTED,
                reason.as_deref(),
                &actor,
            )
            .await?;
            sqlx::query(
                "UPDATE resources.purchase_requisition SET decided_at = now(), decided_by = $2 \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(actor.id.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            let out = load(&mut tx, t, id, false).await?;
            emit(
                &mut tx,
                &scope,
                ev::PURCHASE_REQUISITION_REJECTED,
                ev::PURCHASE_REQUISITION_REJECTED_SCHEMA,
                "purchase_requisition",
                id,
                "resources.purchase_requisition.reject",
                &status_payload(
                    id,
                    out.institution_id,
                    Some("submitted"),
                    "rejected",
                    out.version,
                    reason.as_deref(),
                    out.process_instance_id.as_deref(),
                    Some(&out.number),
                    out.requester_subject.as_deref(),
                ),
                &actor,
            )
            .await?;
        }
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Amend a requisition.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PurchaseRequisitionAmend {
    pub title: Option<String>,
    pub justification: Option<String>,
    pub needed_by: Option<NaiveDate>,
    pub lines: Vec<PurchaseRequisitionLineInput>,
    /// Required unless the requisition is still a draft.
    pub reason: Option<String>,
}

/// Amend the demand (`resources.procurement.manage`; If-Match; L3-J-040): replaces the lines; a
/// draft stays `draft`, a rejected / withdrawn / approved / released requisition becomes
/// `amended` with the next revision and needs a fresh approval.
#[utoipa::path(post, path = "/api/v1/resources/requisitions/{id}/amend", operation_id = "resources_requisition_amend", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body = PurchaseRequisitionAmend, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseRequisitionDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn amend(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<PurchaseRequisitionAmend>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = requisition_scope(&st, ctx, id).await?;
    let title = match b.title {
        Some(x) => Some(required_text("title", &x, 200)?),
        None => None,
    };
    let justification = optional_text("justification", b.justification, 2000)?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    let from = r.status()?;
    let is_draft = matches!(from, RequisitionStatus::Draft | RequisitionStatus::Amended);
    let reason = if is_draft {
        clean(b.reason)
    } else {
        Some(required_reason(b.reason)?)
    };
    if !is_draft && !from.can_transition(RequisitionStatus::Amended) {
        return Err(illegal("purchase requisition", &r.number, from.as_str(), "amended").into());
    }
    if !is_draft {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resources.sourcing_event WHERE requisition_id = $1 \
             AND status IN ('draft', 'open', 'evaluating', 'awarded')",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if live > 0 {
            return Err(KernelError::Conflict(format!(
                "purchase requisition {} is being sourced; cancel the sourcing event first",
                r.number
            ))
            .into());
        }
    }
    let lines = clean_lines(&mut tx, t, InstitutionId(r.institution_id), b.lines).await?;
    write_lines(&mut tx, t, id, &lines).await?;
    sqlx::query(
        "UPDATE resources.purchase_requisition SET title = COALESCE($2, title), \
         justification = COALESCE($3, justification), needed_by = COALESCE($4, needed_by), \
         revision = revision + CASE WHEN $5 THEN 0 ELSE 1 END, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&title)
    .bind(&justification)
    .bind(b.needed_by)
    .bind(is_draft)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if !is_draft {
        let r = load(&mut tx, t, id, true).await?;
        change_status(
            &mut tx,
            t,
            &r,
            RequisitionStatus::Amended,
            "sutraerp.resources.purchase_requisition.amended",
            reason.as_deref(),
            &actor,
        )
        .await?;
    }
    audit_only(
        &mut tx,
        &scope,
        "purchase_requisition",
        id,
        "resources.purchase_requisition.amend",
        serde_json::json!({ "number": r.number, "reason": reason, "lines": lines.len() }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

#[allow(clippy::too_many_arguments)]
async fn simple_move(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
    headers: &HeaderMap,
    to: RequisitionStatus,
    reason: Option<String>,
    action: &'static str,
    cause: &'static str,
) -> Result<Result<Response, Response>, KernelError> {
    let v = match crate::shared::http::if_match(headers)? {
        Ok(v) => v.0,
        Err(resp) => return Ok(Err(resp)),
    };
    let t = ctx.scope.tenant_id;
    let scope = requisition_scope(st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, t, id, true).await?;
    check_version(v, r.version)?;
    if to == RequisitionStatus::Cancelled {
        let live: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resources.sourcing_event WHERE requisition_id = $1 \
             AND status IN ('draft', 'open', 'evaluating', 'awarded')",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if live > 0 {
            return Err(KernelError::Conflict(format!(
                "purchase requisition {} is being sourced; cancel the sourcing event first",
                r.number
            )));
        }
    }
    change_status(&mut tx, t, &r, to, cause, reason.as_deref(), &actor).await?;
    audit_only(
        &mut tx,
        &scope,
        "purchase_requisition",
        id,
        action,
        serde_json::json!({ "number": r.number, "reason": reason }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Ok(res))
}

/// Withdraw a submitted requisition before its decision (`resources.procurement.manage`;
/// If-Match; L3-J-041).
#[utoipa::path(post, path = "/api/v1/resources/requisitions/{id}/withdraw", operation_id = "resources_requisition_withdraw", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseRequisitionDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn withdraw(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let reason = reason_of(body);
    match simple_move(
        &st,
        ctx,
        id,
        &headers,
        RequisitionStatus::Withdrawn,
        reason,
        "resources.purchase_requisition.withdraw",
        "sutraerp.resources.purchase_requisition.withdrawn",
    )
    .await?
    {
        Ok(r) | Err(r) => Ok(r),
    }
}

/// Cancel a requisition that is not ordered (`resources.procurement.manage`; If-Match; reason
/// required; L3-J-041).
#[utoipa::path(post, path = "/api/v1/resources/requisitions/{id}/cancel", operation_id = "resources_requisition_cancel", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the requisition")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = PurchaseRequisitionDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn cancel(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let reason = required_reason(reason_of(body))?;
    match simple_move(
        &st,
        ctx,
        id,
        &headers,
        RequisitionStatus::Cancelled,
        Some(reason),
        "resources.purchase_requisition.cancel",
        "sutraerp.resources.purchase_requisition.cancelled",
    )
    .await?
    {
        Ok(r) | Err(r) => Ok(r),
    }
}

/// Routes of this area.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get_one))
        .routes(routes!(submit))
        .routes(routes!(decide))
        .routes(routes!(amend))
        .routes(routes!(withdraw))
        .routes(routes!(cancel))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requisition_transitions() {
        use RequisitionStatus::*;
        assert!(Draft.can_transition(Submitted));
        assert!(Submitted.can_transition(Approved) && Approved.can_transition(Released));
        assert!(Released.can_transition(Ordered) && Ordered.can_transition(Closed));
        assert!(!Ordered.can_transition(Cancelled));
        assert!(Submitted.can_transition(Withdrawn) && !Draft.can_transition(Withdrawn));
        assert!(Rejected.can_transition(Amended) && Amended.can_transition(Submitted));
    }
}
