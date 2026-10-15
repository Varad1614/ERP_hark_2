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
//!
//! Part 1 · Stage 3 (DEF-J-004): once a supplier has been approved its bank details are no longer
//! profile fields. `POST /suppliers/{id}/bank-change` records the new details as a pending request
//! (`SUPPLIER_BANK_CHANGE_APPROVAL`, `supplier.bank_change_requested` notifies the approvers; the
//! live details stay in use) and `POST /suppliers/{id}/bank-change/decision` by **another** user
//! with `resources.approve` / `finance.approve` promotes or discards it. Every step is audited with
//! masked old / new accounts and recorded in the status history (`supplier_bank_change`). A
//! profile PATCH that would change the bank details of an approved (or approval-pending) supplier
//! is refused (`409`), and a database trigger refuses any other path.

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
    /// Submitted for `SUPPLIER_APPROVAL`, awaiting a decision. Deprecated (Batch 9 §6.4): read
    /// `approvalState`; kept for existing clients.
    pub approval_pending: bool,
    /// 9E: approvalState — canonical approval state (`not_requested | pending | approved |
    /// rejected`, contract batch9 §6.4), derived from the status + `approvalPending`.
    pub approval_state: String,
    pub canonical_supplier_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub registered_at: Option<DateTime<Utc>>,
    pub approved_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    /// Part 1 · Stage 3 (DEF-J-004): a bank-change request awaits an independent decision.
    pub bank_change_pending: bool,
    pub pending_bank_account_name: Option<String>,
    /// Masked (`XXXXXX7890`).
    pub pending_bank_account_number: Option<String>,
    pub pending_bank_ifsc: Option<String>,
    pub bank_change_reason: Option<String>,
    pub bank_change_requested_by: Option<String>,
    pub bank_change_requested_at: Option<DateTime<Utc>>,
    pub bank_change_process_instance_id: Option<String>,
    /// Id of the open bank-change request (its status history is keyed by it).
    pub bank_change_request_id: Option<Uuid>,
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
    approved_at, version, created_at, \
    CASE WHEN approval_pending THEN 'pending' \
         WHEN approved_at IS NOT NULL OR status IN ('approved', 'active', 'suspended') THEN 'approved' \
         ELSE 'not_requested' END AS approval_state, \
    (pending_bank_account_number IS NOT NULL) AS bank_change_pending, pending_bank_account_name, \
    pending_bank_account_number, pending_bank_ifsc, bank_change_reason, bank_change_requested_by, \
    bank_change_requested_at, bank_change_process_instance_id, bank_change_request_id \
    FROM resources.supplier"; // 9E: approvalState · Part 1 S3: DEF-J-004 pending bank change

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
    s.pending_bank_account_number = mask_account(s.pending_bank_account_number.take());
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
    let mut p = validate_profile(b)?;
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
    // Part 1 · Stage 3 (DEF-J-004): approved bank details change only through an independently
    // approved bank-change request; while the registration awaits its decision they are frozen
    // (the approver decides on what was submitted). Unchanged values are accepted (no-op).
    if s.approved_at.is_some() || s.approval_pending {
        let differs = |new: &Option<String>, old: &Option<String>| {
            new.as_ref().is_some_and(|v| Some(v) != old.as_ref())
        };
        if differs(&p.bank_account_name, &s.bank_account_name)
            || differs(&p.bank_account_number, &s.bank_account_number)
            || differs(&p.bank_ifsc, &s.bank_ifsc)
        {
            return Err(KernelError::Conflict(format!(
                "supplier {}: bank details of an approved or approval-pending supplier change only \
                 through POST /api/v1/resources/suppliers/{id}/bank-change (independent approval)",
                s.number
            ))
            .into());
        }
        p.bank_account_name = None;
        p.bank_account_number = None;
        p.bank_ifsc = None;
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

// ------------------------------------------------------------------ Part 1 · Stage 3 (DEF-J-004)

/// New bank details for an approved supplier (`POST /suppliers/{id}/bank-change`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SupplierBankChangeRequest {
    /// Defaults to the current account name.
    pub bank_account_name: Option<String>,
    #[schema(example = "001234567890")]
    pub bank_account_number: String,
    #[schema(example = "HDFC0001234")]
    pub bank_ifsc: String,
    /// Why the details change (shown to the approver; required).
    pub reason: String,
}

/// `XXXXXX7890 (HDFC0001234)`.
fn masked_bank(number: Option<&str>, ifsc: Option<&str>) -> String {
    format!(
        "{} ({})",
        mask_account(number.map(str::to_owned)).unwrap_or_else(|| "none".into()),
        ifsc.unwrap_or("no IFSC")
    )
}

/// Promotes the pending bank details of an approved supplier (approval of the request).
async fn apply_bank_change(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    s: &SupplierView,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let new = masked_bank(
        s.pending_bank_account_number.as_deref(),
        s.pending_bank_ifsc.as_deref(),
    );
    let old = masked_bank(s.bank_account_number.as_deref(), s.bank_ifsc.as_deref());
    sqlx::query(
        "UPDATE resources.supplier SET bank_account_name = pending_bank_account_name, \
         bank_account_number = pending_bank_account_number, bank_ifsc = pending_bank_ifsc, \
         pending_bank_account_name = NULL, pending_bank_account_number = NULL, \
         pending_bank_ifsc = NULL, bank_change_reason = NULL, bank_change_requested_by = NULL, \
         bank_change_requested_at = NULL, bank_change_request_id = NULL, version = version + 1, \
         updated_at = now() WHERE id = $1 AND pending_bank_account_number IS NOT NULL",
    )
    .bind(s.id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let request = s.bank_change_request_id.ok_or_else(|| {
        KernelError::Conflict(format!(
            "supplier {} has no open bank-change request",
            s.number
        ))
    })?;
    history(
        conn,
        tenant,
        InstitutionId(s.institution_id),
        "supplier_bank_change",
        request,
        Some("pending"),
        "approved",
        ev::SUPPLIER_BANK_CHANGE_APPROVED,
        reason,
        s.bank_change_process_instance_id.as_deref(),
        actor,
    )
    .await?;
    emit(
        conn,
        scope,
        ev::SUPPLIER_BANK_CHANGE_APPROVED,
        ev::SUPPLIER_BANK_CHANGE_APPROVED_SCHEMA,
        "supplier",
        s.id,
        "resources.supplier.bank_change.approve",
        &status_payload(
            s.id,
            s.institution_id,
            Some("pending"),
            "approved",
            s.version + 1,
            Some(&format!("{old} → {new}")),
            s.bank_change_process_instance_id.as_deref(),
            Some(&s.number),
            None,
        ),
        actor,
    )
    .await
}

/// Request new bank details for an approved supplier (`resources.procurement.manage`; If-Match;
/// Part 1 · Stage 3, DEF-J-004). The live details stay in use; the request waits for
/// `SUPPLIER_BANK_CHANGE_APPROVAL` (approvers notified, audited with masked accounts). One open
/// request at a time (`409`).
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/bank-change", operation_id = "resources_supplier_bank_change_request", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body = SupplierBankChangeRequest, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn request_bank_change(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<SupplierBankChangeRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::ProcurementManage)?;
    let v = expected!(headers);
    let clean_req = validate_profile(SupplierProfile {
        bank_account_name: b.bank_account_name.clone(),
        bank_account_number: Some(b.bank_account_number.clone()),
        bank_ifsc: Some(b.bank_ifsc.clone()),
        ..SupplierProfile::default()
    })?;
    let (Some(number), Some(ifsc)) = (
        clean_req.bank_account_number.clone(),
        clean_req.bank_ifsc.clone(),
    ) else {
        return Err(invalid(
            "bankAccountNumber",
            "required",
            "bank account number and IFSC are required",
        )
        .into());
    };
    let reason = required_reason(Some(b.reason.clone()))?;
    let t = ctx.scope.tenant_id;
    let scope = supplier_scope(&st, ctx, id).await?;
    // Validate first (no locks) so a refused request never starts an approval process.
    let s = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load(&mut conn, t, id, false).await?
    };
    check_version(v, s.version)?;
    let status = s.status()?;
    if s.approved_at.is_none()
        || !matches!(
            status,
            SupplierStatus::Approved | SupplierStatus::Active | SupplierStatus::Suspended
        )
    {
        return Err(KernelError::Conflict(format!(
            "supplier {} is {}; bank details of a supplier that is not approved are edited on its \
             profile and approved with its registration",
            s.number, s.status
        ))
        .into());
    }
    if s.bank_change_pending {
        return Err(KernelError::Conflict(format!(
            "supplier {} already has a bank-change request awaiting a decision",
            s.number
        ))
        .into());
    }
    let name = clean_req
        .bank_account_name
        .clone()
        .or_else(|| s.bank_account_name.clone());
    if (name.as_deref(), Some(number.as_str()), Some(ifsc.as_str()))
        == (
            s.bank_account_name.as_deref(),
            s.bank_account_number.as_deref(),
            s.bank_ifsc.as_deref(),
        )
    {
        return Err(invalid(
            "bankAccountNumber",
            "unchanged",
            "these are the supplier's current bank details",
        )
        .into());
    }
    let mut vars = BTreeMap::new();
    vars.insert("supplierNumber".to_owned(), serde_json::json!(s.number));
    vars.insert(
        "newAccount".to_owned(),
        serde_json::json!(masked_bank(Some(&number), Some(&ifsc))),
    );
    let outcome = request_approval(
        &st,
        &scope,
        process::SUPPLIER_BANK_CHANGE_APPROVAL,
        business_key("supplier_bank_change", id, s.version),
        vars,
    )
    .await?;
    let actor = actor(ctx);
    let request = Uuid::now_v7();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = load(&mut tx, t, id, true).await?;
    check_version(v, s.version)?;
    if s.bank_change_pending {
        return Err(KernelError::Conflict(format!(
            "supplier {} already has a bank-change request awaiting a decision",
            s.number
        ))
        .into());
    }
    sqlx::query(
        "UPDATE resources.supplier SET pending_bank_account_name = $2, \
         pending_bank_account_number = $3, pending_bank_ifsc = $4, bank_change_reason = $5, \
         bank_change_requested_by = $6, bank_change_requested_at = now(), \
         bank_change_process_instance_id = $7, bank_change_request_id = $8, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&name)
    .bind(&number)
    .bind(&ifsc)
    .bind(&reason)
    .bind(actor.id.as_deref())
    .bind(outcome.process_instance_id())
    .bind(request)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        t,
        InstitutionId(s.institution_id),
        "supplier_bank_change",
        request,
        None,
        "pending",
        ev::SUPPLIER_BANK_CHANGE_REQUESTED,
        Some(&reason),
        Some(outcome.process_instance_id()),
        &actor,
    )
    .await?;
    let old = masked_bank(s.bank_account_number.as_deref(), s.bank_ifsc.as_deref());
    let new = masked_bank(Some(&number), Some(&ifsc));
    emit(
        &mut tx,
        &scope,
        ev::SUPPLIER_BANK_CHANGE_REQUESTED,
        ev::SUPPLIER_BANK_CHANGE_REQUESTED_SCHEMA,
        "supplier",
        id,
        "resources.supplier.bank_change.request",
        &status_payload(
            id,
            s.institution_id,
            None,
            "pending",
            s.version + 1,
            Some(&format!("{old} → {new}: {reason}")),
            Some(outcome.process_instance_id()),
            Some(&s.number),
            None,
        ),
        &actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let pending = load(&mut tx, t, id, true).await?;
        apply_bank_change(
            &mut tx,
            &scope,
            t,
            &pending,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let res = respond(&mut tx, t, id, StatusCode::OK).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(res)
}

/// Decide a supplier's pending bank-change request (`resources.approve` **or**
/// `finance.approve`, step-up; If-Match; Part 1 · Stage 3, DEF-J-004): approve → the new details
/// go live (`supplier.bank_change_approved` warns finance); reject → discarded (reason
/// required). Whoever requested the change — or a system actor — is refused (`403`).
#[utoipa::path(post, path = "/api/v1/resources/suppliers/{id}/bank-change/decision", operation_id = "resources_supplier_bank_change_decide", tag = "resources-procurement",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the supplier")),
    request_body = ResourcesDecisionBody, security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = SupplierView, headers(("ETag" = String))), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn decide_bank_change(
    State(st): State<ResourcesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(b): Json<ResourcesDecisionBody>,
) -> ApiResult<Response> {
    let ctx = if auth.0.has_permission(Permission::FinanceApprove) {
        auth.require(Permission::FinanceApprove)?
    } else {
        auth.require(Permission::ResourcesApprove)?
    };
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
    if !s.bank_change_pending {
        return Err(KernelError::Conflict(format!(
            "supplier {} has no bank-change request awaiting a decision",
            s.number
        ))
        .into());
    }
    // Whoever asked for new bank details never approves them (DEF-X-002 / DEF-J-004).
    let request = s.bank_change_request_id.ok_or_else(|| {
        KernelError::Conflict(format!(
            "supplier {} has no open bank-change request",
            s.number
        ))
    })?;
    crate::shared::approval::ensure_independent_decider(
        &mut tx,
        t,
        "supplier_bank_change",
        request,
        &["pending"],
        &[s.bank_change_requested_by.as_deref()],
        actor.id.as_deref(),
        "supplier bank-change request",
    )
    .await?;
    let old = masked_bank(s.bank_account_number.as_deref(), s.bank_ifsc.as_deref());
    let new = masked_bank(
        s.pending_bank_account_number.as_deref(),
        s.pending_bank_ifsc.as_deref(),
    );
    match b.decision {
        ResourcesDecision::Approve => {
            apply_bank_change(&mut tx, &scope, t, &s, reason.as_deref(), &actor).await?;
        }
        ResourcesDecision::Reject => {
            sqlx::query(
                "UPDATE resources.supplier SET pending_bank_account_name = NULL, \
                 pending_bank_account_number = NULL, pending_bank_ifsc = NULL, \
                 bank_change_reason = NULL, bank_change_requested_by = NULL, \
                 bank_change_requested_at = NULL, bank_change_request_id = NULL, \
                 version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            history(
                &mut tx,
                t,
                InstitutionId(s.institution_id),
                "supplier_bank_change",
                request,
                Some("pending"),
                "rejected",
                ev::SUPPLIER_BANK_CHANGE_REJECTED,
                reason.as_deref(),
                s.bank_change_process_instance_id.as_deref(),
                &actor,
            )
            .await?;
            emit(
                &mut tx,
                &scope,
                ev::SUPPLIER_BANK_CHANGE_REJECTED,
                ev::SUPPLIER_BANK_CHANGE_REJECTED_SCHEMA,
                "supplier",
                id,
                "resources.supplier.bank_change.reject",
                &status_payload(
                    id,
                    s.institution_id,
                    Some("pending"),
                    "rejected",
                    s.version + 1,
                    reason.as_deref(),
                    s.bank_change_process_instance_id.as_deref(),
                    Some(&s.number),
                    None,
                ),
                &actor,
            )
            .await?;
        }
    }
    audit_only(
        &mut tx,
        &scope,
        "supplier",
        id,
        "resources.supplier.bank_change.decide",
        serde_json::json!({
            "number": s.number,
            "decision": b.decision,
            "reason": reason,
            "oldAccount": old,
            "newAccount": new,
            "requestedBy": s.bank_change_requested_by,
            "requestId": request,
        }),
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
        .routes(routes!(request_bank_change)) // Part 1 · S3: DEF-J-004
        .routes(routes!(decide_bank_change))
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
