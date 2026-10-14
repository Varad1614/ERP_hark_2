//! **8D** — suppliers: prospective → registered → approved / active, suspension, duplicates → canonical (L3-J-020, 021, 030, 035). Operation ids `resources_supplier_*`, tag `resources-procurement`.
//!
//! Owned by slice 8D (`docs/batch8-contracts.md` §2.1). The mount point is pre-registered in
//! `crate::router_with_state`; the slice fills [`router`] and adds files in this folder.
//!
//! * create → `prospective` (`SUP-` number; GSTIN / PAN / IFSC format checks);
//! * register → `registered` (PAN — or the PAN inside the GSTIN — and bank details required; a
//!   live supplier with the same GSTIN / PAN is a duplicate → 409 naming it);
//! * submit → `SUPPLIER_APPROVAL` → approved → `active` (`supplier.approved`); reject keeps it
//!   `registered` with the reason;
//! * suspend (`supplier.suspended`) ↔ restore; merge a duplicate onto its canonical supplier
//!   (`merged`, `canonicalSupplierId`; not while it has open purchase orders).
//!
//! Bank account numbers are masked on every read.

pub mod common;

use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution, require_institution_writable};
use crate::shared::http::{actor, with_etag};
use crate::shared::numbering::{kind, next};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, ResourcesState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, Utc};
use common::{
    audit_only, check_version, clean, emit, expected, history, illegal, institution_of, invalid,
    mask_account, optional_text, reason_of, required_reason, required_text, slice_status,
    status_payload, valid_gstin, valid_ifsc, valid_pan, ResourcesDecision, ResourcesDecisionBody,
    ResourcesReasonBody, ResourcesStatusQuery,
};
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
    /// Supplier lifecycle (L2-J02).
    SupplierStatus {
        Prospective => "prospective",
        Registered => "registered",
        Approved => "approved",
        Active => "active",
        Suspended => "suspended",
        Merged => "merged",
        Inactive => "inactive",
    }
);

impl SupplierStatus {
    /// Legal moves (merge from every live state).
    pub fn can_transition(self, to: Self) -> bool {
        use SupplierStatus::*;
        if to == Merged {
            return !matches!(self, Merged | Inactive);
        }
        matches!(
            (self, to),
            (Prospective, Registered)
                | (Registered, Approved)
                | (Approved, Active)
                | (Active, Suspended)
                | (Suspended, Active)
                | (Active, Inactive)
                | (Suspended, Inactive)
        )
    }
}

/// A supplier (bank account masked).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct SupplierView {
    pub id: Uuid,
    pub institution_id: Uuid,
    #[schema(example = "SUP-000001")]
    pub number: String,
    pub legal_name: String,
    pub trade_name: Option<String>,
    pub category: Option<String>,
    #[schema(example = "27ABCDE1234F1Z5")]
    pub gstin: Option<String>,
    #[schema(example = "ABCDE1234F")]
    pub pan: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub bank_account_name: Option<String>,
    /// Masked (`XXXXXX7890`).
    pub bank_account_number: Option<String>,
    pub bank_ifsc: Option<String>,
    /// `prospective | registered | approved | active | suspended | merged | inactive`.
    pub status: String,
    pub status_reason: Option<String>,
    /// Submitted for `SUPPLIER_APPROVAL`, awaiting a decision.
    pub approval_pending: bool,
    pub canonical_supplier_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub registered_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

impl SupplierView {
    pub fn status(&self) -> Result<SupplierStatus, KernelError> {
        SupplierStatus::parse(&self.status).ok_or_else(|| {
            KernelError::Conflict(format!("unknown supplier status {}", self.status))
        })
    }

    /// `SUP-000001 Acme Traders`.
    pub fn label(&self) -> String {
        format!("{} {}", self.number, self.legal_name)
    }
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct SupplierList {
    pub items: Vec<SupplierView>,
}

const SELECT: &str = "SELECT id, institution_id, number, legal_name, trade_name, category, gstin, \
    pan, email, phone, address, bank_account_name, bank_account_number, bank_ifsc, status, \
    status_reason, approval_pending, canonical_supplier_id, process_instance_id, registered_at, \
    approved_at, version, created_at FROM resources.supplier";

/// A supplier of the tenant (404 when missing); `lock` takes the row lock. The bank account
/// number is **not** masked here (internal use); views mask it.
pub async fn load(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<SupplierView, KernelError> {
    let lock_sql = if lock { " FOR UPDATE" } else { "" };
    sqlx::query_as::<_, SupplierView>(&format!(
        "{SELECT} WHERE id = $1 AND tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "supplier" })
}

fn masked(mut s: SupplierView) -> SupplierView {
    s.bank_account_number = mask_account(s.bank_account_number.take());
    s
}

/// An **active** supplier of the institution (sourcing, purchase orders, invoices), else 422 on
/// `field` (409 when it exists but is not active).
pub async fn active_supplier(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution: InstitutionId,
    field: &str,
    id: Uuid,
) -> Result<SupplierView, KernelError> {
    let s = match load(conn, tenant, id, false).await {
        Ok(s) if s.institution_id == institution.as_uuid() => s,
        Ok(_) | Err(KernelError::NotFound { .. }) => {
            return Err(invalid(
                field,
                "unknown_supplier",
                "no such supplier in the institution",
            ))
        }
        Err(e) => return Err(e),
    };
    if s.status != SupplierStatus::Active.as_str() {
        return Err(KernelError::Conflict(format!(
            "supplier {} is {}; only active suppliers take part in procurement",
            s.number, s.status
        )));
    }
    Ok(s)
}

/// Profile fields (create / update / register).
#[derive(Debug, Clone, Default, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SupplierProfile {
    pub legal_name: Option<String>,
    pub trade_name: Option<String>,
    pub category: Option<String>,
    pub gstin: Option<String>,
    pub pan: Option<String>,
    pub email: Option<String>,
    pub phone: Option<String>,
    pub address: Option<String>,
    pub bank_account_name: Option<String>,
    pub bank_account_number: Option<String>,
    pub bank_ifsc: Option<String>,
}

/// Validated, normalised profile values (`None` = unchanged).
#[derive(Debug, Clone, Default)]
struct CleanProfile {
    legal_name: Option<String>,
    trade_name: Option<String>,
    category: Option<String>,
    gstin: Option<String>,
    pan: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    address: Option<String>,
    bank_account_name: Option<String>,
    bank_account_number: Option<String>,
    bank_ifsc: Option<String>,
}

fn validate_profile(p: SupplierProfile) -> Result<CleanProfile, KernelError> {
    let upper = |v: Option<String>| clean(v).map(|s| s.to_uppercase());
    let legal_name = match p.legal_name {
        Some(n) => Some(required_text("legalName", &n, 200)?),
        None => None,
    };
    let gstin = upper(p.gstin);
    if gstin.as_deref().is_some_and(|g| !valid_gstin(g)) {
        return Err(invalid(
            "gstin",
            "invalid_gstin",
            "15 characters: state code, PAN, entity code, Z, check character (27ABCDE1234F1Z5)",
        ));
    }
    let pan = upper(p.pan);
    if pan.as_deref().is_some_and(|v| !valid_pan(v)) {
        return Err(invalid(
            "pan",
            "invalid_pan",
            "10 characters such as ABCDE1234F",
        ));
    }
    if let (Some(g), Some(pn)) = (&gstin, &pan) {
        if g.get(2..12) != Some(pn.as_str()) {
            return Err(invalid(
                "gstin",
                "pan_mismatch",
                "the GSTIN does not contain the supplier's PAN",
            ));
        }
    }
    let bank_ifsc = upper(p.bank_ifsc);
    if bank_ifsc.as_deref().is_some_and(|v| !valid_ifsc(v)) {
        return Err(invalid(
            "bankIfsc",
            "invalid_ifsc",
            "11 characters such as HDFC0001234",
        ));
    }
    let bank_account_number = clean(p.bank_account_number);
    if bank_account_number
        .as_deref()
        .is_some_and(|v| !(6..=20).contains(&v.len()) || !v.bytes().all(|b| b.is_ascii_digit()))
    {
        return Err(invalid(
            "bankAccountNumber",
            "invalid_account",
            "6 to 20 digits",
        ));
    }
    let email = optional_text("email", p.email, 200)?;
    if email.as_deref().is_some_and(|e| !e.contains('@')) {
        return Err(invalid(
            "email",
            "invalid_email",
            "must be an email address",
        ));
    }
    Ok(CleanProfile {
        legal_name,
        trade_name: optional_text("tradeName", p.trade_name, 200)?,
        category: optional_text("category", p.category, 100)?,
        gstin,
        pan,
        email,
        phone: optional_text("phone", p.phone, 40)?,
        address: optional_text("address", p.address, 500)?,
        bank_account_name: optional_text("bankAccountName", p.bank_account_name, 200)?,
        bank_account_number,
        bank_ifsc,
    })
}

async fn apply_profile(
    conn: &mut PgConnection,
    id: Uuid,
    p: &CleanProfile,
) -> Result<(), KernelError> {
    sqlx::query(
        "UPDATE resources.supplier SET legal_name = COALESCE($2, legal_name), \
         trade_name = COALESCE($3, trade_name), category = COALESCE($4, category), \
         gstin = COALESCE($5, gstin), pan = COALESCE($6, pan), email = COALESCE($7, email), \
         phone = COALESCE($8, phone), address = COALESCE($9, address), \
         bank_account_name = COALESCE($10, bank_account_name), \
         bank_account_number = COALESCE($11, bank_account_number), \
         bank_ifsc = COALESCE($12, bank_ifsc), version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(&p.legal_name)
    .bind(&p.trade_name)
    .bind(&p.category)
    .bind(&p.gstin)
    .bind(&p.pan)
    .bind(&p.email)
    .bind(&p.phone)
    .bind(&p.address)
    .bind(&p.bank_account_name)
    .bind(&p.bank_account_number)
    .bind(&p.bank_ifsc)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// A live (not merged / inactive) supplier of the institution with the same GSTIN or PAN.
async fn duplicate_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &SupplierView,
) -> Result<Option<String>, KernelError> {
    let pan = s.pan.clone().or_else(|| {
        s.gstin
            .as_deref()
            .and_then(|g| g.get(2..12))
            .map(str::to_owned)
    });
    let dup: Option<String> = sqlx::query_scalar(
        "SELECT number FROM resources.supplier WHERE tenant_id = $1 AND institution_id = $2 \
         AND id <> $3 AND status NOT IN ('merged', 'inactive', 'prospective') \
         AND (($4::text IS NOT NULL AND gstin = $4) OR ($5::text IS NOT NULL AND \
              (pan = $5 OR substr(gstin, 3, 10) = $5))) ORDER BY number LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(s.institution_id)
    .bind(s.id)
    .bind(&s.gstin)
    .bind(&pan)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(dup)
}

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &SupplierView,
    to: SupplierStatus,
    reason: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let from = s.status()?;
    if !from.can_transition(to) {
        return Err(illegal("supplier", &s.number, from.as_str(), to.as_str()));
    }
    sqlx::query(
        "UPDATE resources.supplier SET status = $2, status_reason = $3, \
         registered_at = CASE WHEN $2 = 'registered' THEN now() ELSE registered_at END, \
         approved_at = CASE WHEN $2 = 'approved' THEN now() ELSE approved_at END, \
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
        "supplier",
        s.id,
        Some(from.as_str()),
        to.as_str(),
        cause,
        reason,
        s.process_instance_id.as_deref(),
        actor,
    )
    .await
}

/// Approval of a registered supplier: registered → approved → active + `supplier.approved`.
async fn approve(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    id: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let s = load(conn, tenant, id, true).await?;
    set_status(
        conn,
        tenant,
        &s,
        SupplierStatus::Approved,
        None,
        ev::SUPPLIER_APPROVED,
        actor,
    )
    .await?;
    let s = load(conn, tenant, id, true).await?;
    set_status(
        conn,
        tenant,
        &s,
        SupplierStatus::Active,
        None,
        ev::SUPPLIER_APPROVED,
        actor,
    )
    .await?;
    sqlx::query("UPDATE resources.supplier SET approval_pending = false WHERE id = $1")
        .bind(id)
        .execute(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let out = load(conn, tenant, id, false).await?;
    emit(
        conn,
        scope,
        ev::SUPPLIER_APPROVED,
        ev::SUPPLIER_APPROVED_SCHEMA,
        "supplier",
        id,
        "resources.supplier.approve",
        &status_payload(
            id,
            out.institution_id,
            Some("registered"),
            out.status.as_str(),
            out.version,
            None,
            out.process_instance_id.as_deref(),
            Some(&out.number),
            None,
        ),
        actor,
    )
    .await
}

/// Scope + writable institution of a supplier.
async fn supplier_scope(
    st: &ResourcesState,
    ctx: &AuthContext,
    id: Uuid,
) -> Result<ExecutionScope, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let inst = institution_of(&mut conn, ctx.scope.tenant_id, "supplier", "supplier", id).await?;
    drop(conn);
    let scope = institution_scope(ctx, inst);
    require_institution_writable(st, &scope, inst).await?;
    Ok(scope)
}

async fn respond(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    status: StatusCode,
) -> Result<Response, KernelError> {
    let out = masked(load(conn, tenant, id, false).await?);
    Ok(with_etag(status, &out, Version(out.version)))
}

/// Register a prospective supplier (`resources.procurement.manage`; Idempotency-Key).
#[utoipa::path(post, path = "/api/v1/resources/institutions/{id}/suppliers", operation_id = "resources_supplier_create", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header)),
    request_body = SupplierProfile, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 201, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn create(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SupplierProfile>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    require_idempotency_key(&headers)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution_writable(&st, &scope, institution).await?;
    if b.legal_name.is_none() {
        return Err(invalid("legalName", "required", "the supplier's legal name").into());
    }
    let p = validate_profile(b)?;
    let t = ctx.scope.tenant_id;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let number = next(&mut tx, t, institution, kind::SUPPLIER).await?;
    let sid: Uuid = sqlx::query_scalar(
        "INSERT INTO resources.supplier (tenant_id, institution_id, number, legal_name) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(t.as_uuid())
    .bind(id)
    .bind(&number)
    .bind(p.legal_name.as_deref().unwrap_or_default())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    apply_profile(&mut tx, sid, &p).await?;
    sqlx::query("UPDATE resources.supplier SET version = 1 WHERE id = $1")
        .bind(sid)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        institution,
        "supplier",
        sid,
        None,
        "prospective",
        "sutraerp.resources.supplier.created",
        None,
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "supplier",
        sid,
        "resources.supplier.create",
        serde_json::json!({ "number": number }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, sid, StatusCode::CREATED).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Suppliers of an institution (`resources.read`; `?status=`).
#[utoipa::path(get, path = "/api/v1/resources/institutions/{id}/suppliers", operation_id = "resources_supplier_list", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ResourcesStatusQuery), security(("session_cookie" = [])),
    responses((status = 200, body = SupplierList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn list(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<ResourcesStatusQuery>,
) -> ApiResult<Json<SupplierList>> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let institution = InstitutionId(id);
    require_institution(&st, &institution_scope(ctx, institution), institution).await?;
    let rows = sqlx::query_as::<_, SupplierView>(&format!(
        "{SELECT} WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY number LIMIT 500"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .bind(id)
    .bind(clean(q.status))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(Json(SupplierList {
        items: rows.into_iter().map(masked).collect(),
    }))
}

/// One supplier (`resources.read`).
#[utoipa::path(get, path = "/api/v1/resources/suppliers/{id}", operation_id = "resources_supplier_get", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn get_one(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ResourcesRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(&mut conn, ctx.scope.tenant_id, id, StatusCode::OK).await?)
}

/// Update the profile (`resources.procurement.manage`; If-Match; not merged / inactive). A
/// registered or active supplier keeps its GSTIN / PAN identity unique.
#[utoipa::path(patch, path = "/api/v1/resources/suppliers/{id}", operation_id = "resources_supplier_update", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body = SupplierProfile, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn update(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SupplierProfile>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let p = validate_profile(b)?;
    let t = ctx.scope.tenant_id;
    let scope = supplier_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if matches!(
        s.status()?,
        SupplierStatus::Merged | SupplierStatus::Inactive
    ) {
        return Err(KernelError::Conflict(format!(
            "supplier {} is {}; its profile is closed",
            s.number, s.status
        ))
        .into());
    }
    apply_profile(&mut tx, id, &p).await?;
    let after = load(&mut tx, t, id, false).await?;
    if s.status()? != SupplierStatus::Prospective {
        if let Some(dup) = duplicate_of(&mut tx, t, &after).await? {
            return Err(KernelError::Conflict(format!(
                "supplier {} would duplicate {dup} (same GSTIN / PAN); merge them instead",
                s.number
            ))
            .into());
        }
    }
    audit_only(
        &mut tx,
        &scope,
        "supplier",
        id,
        "resources.supplier.update",
        serde_json::json!({ "number": s.number }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Register a prospective supplier (`resources.procurement.manage`; If-Match): PAN (or GSTIN)
/// and bank details required; a live supplier with the same GSTIN / PAN → 409 (L3-J-020).
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/register", operation_id = "resources_supplier_register", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body = SupplierProfile, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn register(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    body: Option<Json<SupplierProfile>>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let p = validate_profile(body.map(|b| b.0).unwrap_or_default())?;
    let t = ctx.scope.tenant_id;
    let scope = supplier_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if s.status()? != SupplierStatus::Prospective {
        return Err(illegal("supplier", &s.number, &s.status, "registered").into());
    }
    apply_profile(&mut tx, id, &p).await?;
    let s = load(&mut tx, t, id, true).await?;
    if s.pan.is_none() && s.gstin.is_none() {
        return Err(invalid("pan", "required", "a PAN or GSTIN is required to register").into());
    }
    if let (Some(g), Some(pn)) = (&s.gstin, &s.pan) {
        if g.get(2..12) != Some(pn.as_str()) {
            return Err(invalid(
                "gstin",
                "pan_mismatch",
                "the GSTIN does not contain the supplier's PAN",
            )
            .into());
        }
    }
    if s.bank_account_number.is_none() || s.bank_ifsc.is_none() {
        return Err(invalid(
            "bankAccountNumber",
            "required",
            "bank account number and IFSC are required to register",
        )
        .into());
    }
    if let Some(dup) = duplicate_of(&mut tx, t, &s).await? {
        return Err(KernelError::Conflict(format!(
            "supplier {} duplicates {dup} (same GSTIN / PAN); merge it onto {dup}",
            s.number
        ))
        .into());
    }
    set_status(
        &mut tx,
        t,
        &s,
        SupplierStatus::Registered,
        None,
        "sutraerp.resources.supplier.registered",
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "supplier",
        id,
        "resources.supplier.register",
        serde_json::json!({ "number": s.number }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Submit a registered supplier for approval (`resources.procurement.manage`; If-Match;
/// `SUPPLIER_APPROVAL`). Auto-approval activates it at once (L3-J-021).
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/submit", operation_id = "resources_supplier_submit", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn submit(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let t = ctx.scope.tenant_id;
    let scope = supplier_scope(&st, ctx, id).await?;
    let s = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    check_version(v, s.version)?;
    if s.status()? != SupplierStatus::Registered || s.approval_pending {
        return Err(illegal("supplier", &s.number, &s.status, "submitted").into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("supplierNumber".to_owned(), serde_json::json!(s.number));
    let outcome = request_approval(
        &st,
        &scope,
        process::SUPPLIER_APPROVAL,
        business_key("supplier", id, s.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    sqlx::query(
        "UPDATE resources.supplier SET approval_pending = true, process_instance_id = $2, \
         status_reason = NULL, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(outcome.process_instance_id())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    audit_only(
        &mut tx,
        &scope,
        "supplier",
        id,
        "resources.supplier.submit",
        serde_json::json!({ "number": s.number, "processInstanceId": outcome.process_instance_id() }),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        approve(
            &mut tx,
            &scope,
            t,
            id,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Decide a pending supplier approval (`resources.approve`, step-up; If-Match).
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/decision", operation_id = "resources_supplier_decide", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
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
    let scope = supplier_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if !s.approval_pending {
        return Err(illegal("supplier", &s.number, &s.status, "decided").into());
    }
    match b.decision {
        ResourcesDecision::Approve => approve(&mut tx, &scope, t, id, &actor).await?,
        ResourcesDecision::Reject => {
            sqlx::query(
                "UPDATE resources.supplier SET approval_pending = false, status_reason = $2, \
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
                InstitutionId(s.institution_id),
                "supplier",
                id,
                Some("registered"),
                "registered",
                "sutraerp.resources.supplier.rejected",
                reason.as_deref(),
                s.process_instance_id.as_deref(),
                &actor,
            )
            .await?;
            audit_only(
                &mut tx,
                &scope,
                "supplier",
                id,
                "resources.supplier.reject",
                serde_json::json!({ "number": s.number, "reason": reason }),
                &actor,
            )
            .await?;
        }
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Suspend an active supplier (`resources.procurement.manage`; If-Match; reason required;
/// `supplier.suspended`, L3-J-030). No new RFQ / PO / invoice while suspended.
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/suspend", operation_id = "resources_supplier_suspend", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn suspend(
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
    let scope = supplier_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    set_status(
        &mut tx,
        t,
        &s,
        SupplierStatus::Suspended,
        Some(&reason),
        ev::SUPPLIER_SUSPENDED,
        &actor,
    )
    .await?;
    let out = load(&mut tx, t, id, false).await?;
    emit(
        &mut tx,
        &scope,
        ev::SUPPLIER_SUSPENDED,
        ev::SUPPLIER_SUSPENDED_SCHEMA,
        "supplier",
        id,
        "resources.supplier.suspend",
        &status_payload(
            id,
            out.institution_id,
            Some(s.status.as_str()),
            out.status.as_str(),
            out.version,
            Some(&reason),
            None,
            Some(&out.number),
            None,
        ),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Restore a suspended supplier to active (`resources.procurement.manage`; If-Match; reason
/// required).
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/restore", operation_id = "resources_supplier_restore", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body(content = ResourcesReasonBody, content_type = "application/json"), security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn restore(
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
    let scope = supplier_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if s.status()? != SupplierStatus::Suspended {
        return Err(illegal("supplier", &s.number, &s.status, "restored").into());
    }
    set_status(
        &mut tx,
        t,
        &s,
        SupplierStatus::Active,
        Some(&reason),
        "sutraerp.resources.supplier.restored",
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "supplier",
        id,
        "resources.supplier.restore",
        serde_json::json!({ "number": s.number, "reason": reason }),
        &actor,
    )
    .await?;
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Merge a duplicate onto its canonical supplier.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SupplierMerge {
    pub canonical_supplier_id: Uuid,
    pub reason: Option<String>,
}

/// Merge a duplicate supplier onto the canonical one (`resources.procurement.manage`; If-Match;
/// L3-J-035): the duplicate becomes `merged` with `canonicalSupplierId`; refused while it has
/// open purchase orders or the canonical record is itself merged / inactive.
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/merge", operation_id = "resources_supplier_merge", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the duplicate")),
    request_body = SupplierMerge, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn merge(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SupplierMerge>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let reason = required_reason(b.reason)?;
    if b.canonical_supplier_id == id {
        return Err(invalid(
            "canonicalSupplierId",
            "self",
            "a supplier cannot merge onto itself",
        )
        .into());
    }
    let t = ctx.scope.tenant_id;
    let scope = supplier_scope(&st, ctx, id).await?;
    let actor = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    let canonical = match load(&mut tx, t, b.canonical_supplier_id, false).await {
        Ok(c) if c.institution_id == s.institution_id => c,
        Ok(_) | Err(KernelError::NotFound { .. }) => {
            return Err(invalid(
                "canonicalSupplierId",
                "unknown_supplier",
                "no such supplier in the institution",
            )
            .into())
        }
        Err(e) => return Err(e.into()),
    };
    if matches!(
        canonical.status()?,
        SupplierStatus::Merged | SupplierStatus::Inactive
    ) {
        return Err(KernelError::Conflict(format!(
            "supplier {} is {}; merge onto a live record",
            canonical.number, canonical.status
        ))
        .into());
    }
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM resources.purchase_order WHERE tenant_id = $1 AND supplier_id = $2 \
         AND status IN ('draft', 'issued', 'acknowledged', 'partially_received')",
    )
    .bind(t.as_uuid())
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open > 0 {
        return Err(KernelError::Conflict(format!(
            "supplier {} has {open} open purchase order(s); close or cancel them before merging",
            s.number
        ))
        .into());
    }
    let from = s.status()?;
    if !from.can_transition(SupplierStatus::Merged) {
        return Err(illegal("supplier", &s.number, from.as_str(), "merged").into());
    }
    sqlx::query(
        "UPDATE resources.supplier SET status = 'merged', canonical_supplier_id = $2, \
         status_reason = $3, approval_pending = false, version = version + 1, updated_at = now() \
         WHERE id = $1",
    )
    .bind(id)
    .bind(canonical.id)
    .bind(&reason)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        InstitutionId(s.institution_id),
        "supplier",
        id,
        Some(from.as_str()),
        "merged",
        "sutraerp.resources.supplier.merged",
        Some(&reason),
        None,
        &actor,
    )
    .await?;
    audit_only(
        &mut tx,
        &scope,
        "supplier",
        id,
        "resources.supplier.merge",
        serde_json::json!({ "number": s.number, "canonical": canonical.number, "reason": reason }),
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
        .routes(routes!(get_one, update))
        .routes(routes!(register))
        .routes(routes!(submit))
        .routes(routes!(decide))
        .routes(routes!(suspend))
        .routes(routes!(restore))
        .routes(routes!(merge))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supplier_transitions() {
        use SupplierStatus::*;
        assert!(Prospective.can_transition(Registered));
        assert!(!Prospective.can_transition(Active));
        assert!(Registered.can_transition(Approved) && Approved.can_transition(Active));
        assert!(Active.can_transition(Suspended) && Suspended.can_transition(Active));
        assert!(Registered.can_transition(Merged) && !Merged.can_transition(Merged));
        assert_eq!(SupplierStatus::parse("active"), Some(Active));
    }
}
