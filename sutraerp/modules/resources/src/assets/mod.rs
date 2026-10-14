//! **8D** — asset register: registration (from a receipt or manually), assignment / custody, return, condition change, retirement (L3-J-074, 075, 076, 079, 083). Operation ids `resources_asset_*`, tag `resources-assets`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * register (`AST-` number, `asset.registered`) manually or from an accepted **asset** goods
//!   receipt line (cost = order price, acquired on the receipt date; at most one asset per
//!   accepted unit) → `registered`;
//! * assign custody to a staff member (`WorkforceDirectory`) or a space (Domain G node id, plain
//!   uuid) → `assigned` (`asset.assigned`), return → `available` (assignment history kept);
//! * condition change (`new | good | fair | poor | damaged`; `repair` sends it `under_repair`,
//!   back to `available` once repaired);
//! * retire (`ASSET_RETIREMENT`, not while assigned) → `retired` (`asset.retired`).
//!
//! Assets are operational records only; capitalisation / depreciation are deferred (contract
//! §12).

use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::time::business_today;
use crate::shared::{Actor, ResourcesState};
use crate::suppliers::common::{
    audit_only, check_staff, check_version, clean, emit, expected, history, illegal,
    institution_of, invalid, load_item, optional_text, parse_money, reason_of, required_reason,
    slice_status, status_payload, ResourcesDecision, ResourcesDecisionBody, ResourcesReasonBody,
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
    /// Asset lifecycle (L2-J08).
    AssetStatus {
        Registered => "registered",
        Available => "available",
        Assigned => "assigned",
        UnderRepair => "under_repair",
        Retired => "retired",
    }
);

impl AssetStatus {
    pub fn can_transition(self, to: Self) -> bool {
        use AssetStatus::*;
        matches!(
            (self, to),
            (Registered, Available)
                | (Registered, Assigned)
                | (Registered, UnderRepair)
                | (Registered, Retired)
                | (Available, Assigned)
                | (Available, UnderRepair)
                | (Available, Retired)
                | (Assigned, Available)
                | (UnderRepair, Available)
                | (UnderRepair, Retired)
        )
    }
}

const CONDITIONS: [&str; 5] = ["new", "good", "fair", "poor", "damaged"];

fn condition(field: &str, raw: Option<String>) -> Result<Option<String>, KernelError> {
    match clean(raw) {
        None => Ok(None),
        Some(c) if CONDITIONS.contains(&c.as_str()) => Ok(Some(c)),
        Some(_) => Err(invalid(
            field,
            "invalid",
            "new, good, fair, poor or damaged",
        )),
    }
}

/// An asset.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AssetView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "AST-000001")]
    pub number: String,
    pub name: String,
    pub item_id: Option<Uuid>,
    pub goods_receipt_line_id: Option<Uuid>,
    pub serial_number: Option<String>,
    pub category: Option<String>,
    #[schema(value_type = Option<String>, example = "45000.00")]
    pub acquisition_cost: Option<Decimal>,
    pub acquired_on: Option<NaiveDate>,
    /// `new | good | fair | poor | damaged`.
    pub condition: String,
    /// `registered | available | assigned | under_repair | retired`.
    pub status: String,
    pub status_reason: Option<String>,
    /// `staff | space` while assigned.
    pub custodian_kind: Option<String>,
    pub custodian_id: Option<Uuid>,
    pub retirement_pending: bool,
    pub process_instance_id: Option<String>,
    pub retired_on: Option<NaiveDate>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    #[serde(skip)]
    pub custodian_subject: Option<String>,
}

impl AssetView {
    pub fn status(&self) -> Result<AssetStatus, KernelError> {
        AssetStatus::parse(&self.status)
            .ok_or_else(|| KernelError::Conflict(format!("unknown asset status {}", self.status)))
    }
}

/// One custody period of an asset.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct AssetAssignmentView {
    pub id: Uuid,
    /// `staff | space`.
    pub assignee_kind: String,
    pub assignee_id: Uuid,
    pub assigned_on: NaiveDate,
    pub returned_on: Option<NaiveDate>,
    pub condition_out: String,
    pub condition_in: Option<String>,
    pub note: Option<String>,
}

/// An asset with its custody history (newest first).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetDetail {
    #[serde(flatten)]
    pub asset: AssetView,
    pub assignments: Vec<AssetAssignmentView>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AssetList {
    pub items: Vec<AssetView>,
}

const SELECT: &str = "SELECT id, institution_id, number, name, item_id, goods_receipt_line_id, \
    serial_number, category, acquisition_cost, acquired_on, condition, status, status_reason, \
    custodian_kind, custodian_id, retirement_pending, process_instance_id, retired_on, version, \
    created_at, custodian_subject FROM resources.asset";

async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<AssetView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE" } else { "" };
    sqlx::query_as::<_, AssetView>(&format!(
        "{SELECT} WHERE id = $1 AND tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "asset" })
}

async fn respond(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let asset = load(conn, tenant, id, false).await?;
    let assignments = sqlx::query_as::<_, AssetAssignmentView>(
        "SELECT id, assignee_kind, assignee_id, assigned_on, returned_on, condition_out, \
         condition_in, note FROM resources.asset_assignment WHERE asset_id = $1 \
         ORDER BY created_at DESC",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let v = asset.version;
    Ok(with_etag(
        status,
        &AssetDetail { asset, assignments },
        Version(v),
    ))
}

async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    a: &AssetView,
    to: AssetStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = a.status()?;
    if !from.can_transition(to) {
        return Err(illegal("asset", &a.number, from.as_str(), to.as_str()));
    }
    sqlx::query(
        "UPDATE resources.asset SET status = $2, status_reason = $3, version = version + 1, \
         updated_at = now() WHERE id = $1",
    )
    .bind(a.id)
    .bind(to.as_str())
    .bind(reason)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        InstitutionId(a.institution_id),
        "asset",
        a.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        a.process_instance_id.as_deref(),
        actor,
    )
    .await
}

async fn asset_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(&mut conn, ctx.scope.tenant_id, "asset", "asset", id).await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

/// Register an asset manually or from an accepted asset receipt line.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetCreate {
    /// Accepted `asset` line of a goods receipt (cost, item and date come from it).
    pub goods_receipt_line_id: Option<Uuid>,
    pub item_id: Option<Uuid>,
    /// Default: the item / order line description.
    pub name: Option<String>,
    pub serial_number: Option<String>,
    pub category: Option<String>,
    #[schema(example = "45000.00")]
    pub acquisition_cost: Option<String>,
    pub acquired_on: Option<NaiveDate>,
    /// `new` by default.
    pub condition: Option<String>,
}

/// Register an asset (`resources.asset.manage`; Idempotency-Key; L3-J-074): `asset.registered`.
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/assets", operation_id = "resources_asset_create", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = AssetCreate, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = AssetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
#[allow(clippy::type_complexity)]
pub async fn create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AssetCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AssetManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    let cond = condition("condition", b.condition)?.unwrap_or_else(|| "new".to_owned());
    let serial = optional_text("serialNumber", b.serial_number, 100)?;
    let category = optional_text("category", b.category, 100)?;
    let mut name = optional_text("name", b.name, 200)?;
    let mut cost = match clean(b.acquisition_cost) {
        Some(c) => Some(parse_money("acquisitionCost", &c)?),
        None => None,
    };
    let mut acquired_on = b.acquired_on;
    let mut item_id = b.item_id;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    if let Some(line) = b.goods_receipt_line_id {
        let row: Option<(
            Uuid,
            String,
            Option<Uuid>,
            String,
            Decimal,
            Decimal,
            NaiveDate,
            String,
            String,
        )> = sqlx::query_as(
            "SELECT g.institution_id, gl.line_kind, gl.item_id, g.status, gl.accepted_quantity, \
                 gl.unit_cost, g.received_on, pl.description, g.number \
                 FROM resources.goods_receipt_line gl \
                 JOIN resources.goods_receipt g ON g.id = gl.goods_receipt_id \
                 JOIN resources.purchase_order_line pl ON pl.id = gl.purchase_order_line_id \
                 WHERE gl.id = $1 AND gl.tenant_id = $2 FOR UPDATE OF gl",
        )
        .bind(line)
        .bind(t.as_uuid())
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        let Some((inst, kind_, item, gstatus, accepted, unit_cost, received_on, desc, grn)) = row
        else {
            return Err(invalid(
                "goodsReceiptLineId",
                "unknown_line",
                "no such goods receipt line",
            )
            .into());
        };
        if inst != id || kind_ != "asset" {
            return Err(invalid(
                "goodsReceiptLineId",
                "not_asset",
                "only accepted asset lines of this institution's receipts become assets",
            )
            .into());
        }
        if !matches!(gstatus.as_str(), "accepted" | "partially_accepted") {
            return Err(KernelError::Conflict(format!(
                "goods receipt {grn} is {gstatus}; register assets once it is accepted"
            ))
            .into());
        }
        let registered: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM resources.asset WHERE goods_receipt_line_id = $1",
        )
        .bind(line)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if Decimal::from(registered) >= accepted {
            return Err(KernelError::Conflict(format!(
                "every accepted unit of this {grn} line is already registered ({registered})"
            ))
            .into());
        }
        name = name.or(Some(desc));
        cost = cost.or(Some(unit_cost));
        acquired_on = acquired_on.or(Some(received_on));
        item_id = item_id.or(item);
    }
    if let Some(item) = item_id {
        match load_item(&mut tx, t, item).await? {
            Some(i) if i.institution_id == id => {
                if name.is_none() {
                    name = Some(i.name.clone());
                }
            }
            _ => {
                return Err(
                    invalid("itemId", "unknown_item", "no such item in the institution").into(),
                )
            }
        }
    }
    let Some(name) = name else {
        return Err(invalid(
            "name",
            "required",
            "name the asset (or give its item / receipt line)",
        )
        .into());
    };
    let number = next(&mut tx, t, institution, kind::ASSET).await?;
    let aid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.asset (tenant_id, institution_id, number, name, item_id, \
         goods_receipt_line_id, serial_number, category, acquisition_cost, acquired_on, condition) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(&name)
    .bind(item_id)
    .bind(b.goods_receipt_line_id)
    .bind(&serial)
    .bind(&category)
    .bind(cost)
    .bind(acquired_on)
    .bind(&cond)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("an asset with this serial number is already registered".into())
        }
        other => other,
    })?;
    history(
        &mut tx,
        t,
        institution,
        "asset",
        aid,
        None,
        "registered",
        ev::ASSET_REGISTERED,
        None,
        None,
        &actor,
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        ev::ASSET_REGISTERED,
        ev::ASSET_REGISTERED_SCHEMA,
        "asset",
        aid,
        "resources.asset.register",
        &status_payload(
            aid,
            id,
            None,
            "registered",
            1,
            Some(&name),
            None,
            Some(&number),
            None,
        ),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, aid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Assets of an institution (`resources.read`; `?status=`), by number.
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/assets", operation_id = "resources_asset_list", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = AssetList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<AssetList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let items = sqlx::query_as::<_, AssetView>(&format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY number LIMIT 1000"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(AssetList { items }))
}

/// One asset with its custody history (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/assets/{id}", operation_id = "resources_asset_get", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = AssetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_one(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Assign custody.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetAssign {
    /// `staff | space`.
    pub assignee_kind: String,
    /// Domain E staff id or Domain G infrastructure node id.
    pub assignee_id: Uuid,
    /// Default: today.
    pub assigned_on: Option<NaiveDate>,
    pub note: Option<String>,
}

/// Assign the asset to a staff member or a space (`resources.asset.manage`; If-Match;
/// L3-J-075 / 076): `asset.assigned`.
#[utoipa::path(post, path = "/api/v1/resources/assets/{id}/assign", operation_id = "resources_asset_assign", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the asset")),
    request_body = AssetAssign, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn assign(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AssetAssign>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AssetManage)?;
    let v = expected!(headers);
    let note = optional_text("note", b.note, 500)?;
    let t = ctx.scope.tenant_id;
    let scope = asset_scope(&st, ctx, id).await?;
    let inst = scope.require_institution()?;
    let subject = match b.assignee_kind.as_str() {
        "staff" => check_staff(&st, &scope, inst, "assigneeId", Some(b.assignee_id))
            .await?
            .and_then(|s| s.subject_ref),
        "space" => None,
        _ => return Err(invalid("assigneeKind", "invalid", "staff or space").into()),
    };
    let on = match b.assigned_on {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    if a.retirement_pending {
        return Err(KernelError::Conflict(format!("asset {} awaits retirement", a.number)).into());
    }
    sqlx::query(
        "UPDATE resources.asset SET custodian_kind = $2, custodian_id = $3, custodian_subject = $4 \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&b.assignee_kind)
    .bind(b.assignee_id)
    .bind(&subject)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    set_status(
        &mut tx,
        t,
        &a,
        AssetStatus::Assigned,
        ev::ASSET_ASSIGNED,
        None,
        &actor,
    )
    .await?;
    sqlx::query(
        "INSERT INTO resources.asset_assignment (tenant_id, asset_id, assignee_kind, assignee_id, \
         assigned_on, condition_out, note, actor) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&b.assignee_kind)
    .bind(b.assignee_id)
    .bind(on)
    .bind(&a.condition)
    .bind(&note)
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = load(&mut tx, t, id, false).await?;
    emit(
        &mut tx,
        &scope,
        ev::ASSET_ASSIGNED,
        ev::ASSET_ASSIGNED_SCHEMA,
        "asset",
        id,
        "resources.asset.assign",
        &status_payload(
            id,
            out.institution_id,
            Some(a.status.as_str()),
            "assigned",
            out.version,
            Some(&format!("{} {}", out.number, out.name)),
            None,
            Some(&out.number),
            subject.as_deref(),
        ),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Return an asset from custody.
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetReturn {
    /// Condition on return (default: unchanged).
    pub condition: Option<String>,
    pub returned_on: Option<NaiveDate>,
    pub note: Option<String>,
}

/// Take the asset back from its custodian (`resources.asset.manage`; If-Match): `available`.
#[utoipa::path(post, path = "/api/v1/resources/assets/{id}/return", operation_id = "resources_asset_return", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the asset")),
    request_body(content = AssetReturn, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn return_asset(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<AssetReturn>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AssetManage)?;
    let v = expected!(headers);
    let b = body.map(|b| b.0).unwrap_or_default();
    let cond = condition("condition", b.condition)?;
    let note = optional_text("note", b.note, 500)?;
    let t = ctx.scope.tenant_id;
    let scope = asset_scope(&st, ctx, id).await?;
    let inst = scope.require_institution()?;
    let on = match b.returned_on {
        Some(d) => d,
        None => business_today(&st, &scope, inst).await?,
    };
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    set_status(
        &mut tx,
        t,
        &a,
        AssetStatus::Available,
        "sutraerp.resources.asset.returned",
        note.as_deref(),
        &actor,
    )
    .await?;
    let cond_in = cond.unwrap_or_else(|| a.condition.clone());
    sqlx::query(
        "UPDATE resources.asset SET custodian_kind = NULL, custodian_id = NULL, \
         custodian_subject = NULL, condition = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(&cond_in)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE resources.asset_assignment SET returned_on = $2, condition_in = $3, \
         note = COALESCE($4, note) WHERE asset_id = $1 AND returned_on IS NULL",
    )
    .bind(id)
    .bind(on)
    .bind(&cond_in)
    .bind(&note)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "asset",
        id,
        "resources.asset.return",
        serde_json::json!({ "number": a.number, "condition": cond_in }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Record a condition change.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AssetCondition {
    /// `new | good | fair | poor | damaged`.
    pub condition: String,
    /// `true` sends an unassigned asset for repair; `false` on an asset under repair brings it
    /// back to `available`.
    pub repair: Option<bool>,
    pub reason: Option<String>,
}

/// Record the asset's condition (`resources.asset.manage`; If-Match; L3-J-079), optionally
/// sending it for repair or bringing it back from repair.
#[utoipa::path(post, path = "/api/v1/resources/assets/{id}/condition", operation_id = "resources_asset_condition", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the asset")),
    request_body = AssetCondition, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn change_condition(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<AssetCondition>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AssetManage)?;
    let v = expected!(headers);
    let cond = condition("condition", Some(b.condition))?
        .ok_or_else(|| invalid("condition", "required", "new, good, fair, poor or damaged"))?;
    let reason = optional_text("reason", b.reason, 500)?;
    let t = ctx.scope.tenant_id;
    let scope = asset_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    let status = a.status()?;
    if status == AssetStatus::Retired {
        return Err(KernelError::Conflict(format!("asset {} is retired", a.number)).into());
    }
    match (b.repair, status) {
        (Some(true), AssetStatus::UnderRepair)
        | (None, _)
        | (Some(false), AssetStatus::Registered)
        | (Some(false), AssetStatus::Available)
        | (Some(false), AssetStatus::Assigned) => {}
        (Some(true), _) => {
            set_status(
                &mut tx,
                t,
                &a,
                AssetStatus::UnderRepair,
                "sutraerp.resources.asset.under_repair",
                reason.as_deref(),
                &actor,
            )
            .await?
        }
        (Some(false), _) => {
            set_status(
                &mut tx,
                t,
                &a,
                AssetStatus::Available,
                "sutraerp.resources.asset.repaired",
                reason.as_deref(),
                &actor,
            )
            .await?
        }
    }
    sqlx::query(
        "UPDATE resources.asset SET condition = $2, version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&cond)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "asset",
        id,
        "resources.asset.condition",
        serde_json::json!({ "number": a.number, "from": a.condition, "to": cond, "reason": reason }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

async fn retire_now(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    id: Uuid,
    today: NaiveDate,
    actor: &Actor,
) -> Result<(), KernelError> {
    let a = load(conn, tenant, id, true).await?;
    let reason = a.status_reason.clone();
    set_status(
        conn,
        tenant,
        &a,
        AssetStatus::Retired,
        ev::ASSET_RETIRED,
        reason.as_deref(),
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE resources.asset SET retirement_pending = false, retired_on = $2 WHERE id = $1",
    )
    .bind(id)
    .bind(today)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let out = load(conn, tenant, id, false).await?;
    emit(
        conn,
        scope,
        ev::ASSET_RETIRED,
        ev::ASSET_RETIRED_SCHEMA,
        "asset",
        id,
        "resources.asset.retire",
        &status_payload(
            id,
            out.institution_id,
            Some(a.status.as_str()),
            "retired",
            out.version,
            reason.as_deref(),
            out.process_instance_id.as_deref(),
            Some(&out.number),
            None,
        ),
        actor,
    )
    .await
}

/// Retire an asset (`resources.asset.manage`; If-Match; reason required; `ASSET_RETIREMENT`,
/// L3-J-083): not while assigned; `asset.retired`.
#[utoipa::path(post, path = "/api/v1/resources/assets/{id}/retire", operation_id = "resources_asset_retire", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the asset")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn retire(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<ResourcesReasonBody>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AssetManage)?;
    let v = expected!(headers);
    let reason = required_reason(reason_of(body))?;
    let t = ctx.scope.tenant_id;
    let scope = asset_scope(&st, ctx, id).await?;
    let a = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    check_version(v, a.version)?;
    if !a.status()?.can_transition(AssetStatus::Retired) || a.retirement_pending {
        return Err(illegal("asset", &a.number, &a.status, "retired").into());
    }
    let today = business_today(&st, &scope, InstitutionId(a.institution_id)).await?;
    let mut vars = BTreeMap::new();
    vars.insert("assetNumber".to_owned(), serde_json::json!(a.number));
    vars.insert("reason".to_owned(), serde_json::json!(reason));
    let outcome = request_approval(
        &st,
        &scope,
        process::ASSET_RETIREMENT,
        business_key("asset", id, a.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    sqlx::query(
        "UPDATE resources.asset SET retirement_pending = true, status_reason = $2, \
         process_instance_id = $3, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&reason)
    .bind(outcome.process_instance_id())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "asset",
        id,
        "resources.asset.request_retirement",
        serde_json::json!({ "number": a.number, "reason": reason }),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        retire_now(
            &mut tx,
            &scope,
            t,
            id,
            today,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Decide a pending retirement (`resources.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/resources/assets/{id}/decision", operation_id = "resources_asset_decide", tag = "resources-assets",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the asset")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = AssetDetail, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
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
    let scope = asset_scope(&st, ctx, id).await?;
    let inst = scope.require_institution()?;
    let today = business_today(&st, &scope, inst).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let a = load(&mut tx, t, id, true).await?;
    check_version(v, a.version)?;
    if !a.retirement_pending {
        return Err(illegal("asset", &a.number, &a.status, "decided").into());
    }
    match b.decision {
        ResourcesDecision::Approve => retire_now(&mut tx, &scope, t, id, today, &actor).await?,
        ResourcesDecision::Reject => {
            sqlx::query(
                "UPDATE resources.asset SET retirement_pending = false, status_reason = $2, \
                 version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .bind(&reason)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            audit_only(
                &mut tx,
                &scope,
                "asset",
                id,
                "resources.asset.reject_retirement",
                serde_json::json!({ "number": a.number, "reason": reason }),
                &actor,
            )
            .await?;
        }
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Routes of this area.
pub fn router(state: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(create, list))
        .routes(routes!(get_one))
        .routes(routes!(assign))
        .routes(routes!(return_asset))
        .routes(routes!(change_condition))
        .routes(routes!(retire))
        .routes(routes!(decide))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_transitions_and_conditions() {
        use AssetStatus::*;
        assert!(Registered.can_transition(Assigned) && Assigned.can_transition(Available));
        assert!(!Assigned.can_transition(Retired));
        assert!(UnderRepair.can_transition(Available) && Available.can_transition(Retired));
        assert!(condition("c", Some("good".into())).is_ok());
        assert!(condition("c", Some("broken".into())).is_err());
    }
}
