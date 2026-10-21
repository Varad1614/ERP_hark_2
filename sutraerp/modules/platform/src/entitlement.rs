//! Module entitlements and licence administration (Part 2 · Stage 7, S7-entitle;
//! `docs/readiness/ENTITLEMENT_IMPLEMENTATION.md`). Always the **session** tenant (RLS).
//!
//! | Endpoint | Permission |
//! |----------|------------|
//! | `GET  /api/v1/platform/entitlement` (active modules + licence state) | any signed-in user of the tenant |
//! | `GET  /api/v1/platform/entitlement/catalogue` (modules, packages, dependencies) | any signed-in user |
//! | `GET  /api/v1/platform/entitlement-requests` | `platform.entitlement.manage` or `.approve` |
//! | `POST /api/v1/platform/entitlement-requests` (activate · change · renew · suspend · reinstate) | `platform.entitlement.manage` |
//! | `POST /api/v1/platform/entitlement-requests/{id}/decision` | `platform.entitlement.approve` (step-up, never the requester) |
//! | `POST /api/v1/platform/entitlement-requests/{id}/withdraw` | the requester |
//!
//! Every change is maker-checker: a request carries the **full desired state** (package, add-on
//! modules, licence status, validity window, grace days) and is validated against the registry
//! (`catalog/entitlements.json`, dependency closure included) when it is made **and** when it is
//! approved; the approval applies it to `platform.tenant_entitlement` in the same transaction,
//! refuses a request made against an older entitlement version, and is decided by a second,
//! independent holder of `platform.entitlement.approve` (`ensure_independent_decider` + a database
//! CHECK). Requests, decisions and withdrawals are audited in the same transaction. The new state
//! applies to the very next request (the gate reads it per request) — renewal restores access
//! immediately.
//!
//! [`PgEntitlementStore`] is the PostgreSQL adapter of the kernel's [`EntitlementStore`] port the
//! HTTP gate (`sutra_web::entitlement`) uses.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use chrono::{DateTime, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use sutra_kernel::approval::ensure_independent_decider;
use sutra_kernel::audit::{ActorKind, AuditEntry, AuditOutcome};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::entitlement::{
    registry, CoreSpec, EffectiveEntitlement, EntitlementOrigin, EntitlementPolicy,
    EntitlementStore, LicenceStatus, ModuleSpec, PackageSpec, Registry, TenantEntitlement,
    MAX_GRACE_DAYS,
};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::TenantId;
use sutra_persistence::{audit, map_db_error};
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

const ENT_COLUMNS: &str =
    "package, add_on_modules, licence_status, valid_from, valid_until, grace_days, version";

fn entitlement_row(r: &PgRow) -> Result<TenantEntitlement, KernelError> {
    let status: String = r.try_get("licence_status").map_err(map_db_error)?;
    Ok(TenantEntitlement {
        package: r.try_get("package").map_err(map_db_error)?,
        add_on_modules: r.try_get("add_on_modules").map_err(map_db_error)?,
        status: LicenceStatus::parse(&status).ok_or_else(|| {
            KernelError::internal_msg(format!("unknown stored licence status `{status}`"))
        })?,
        valid_from: r.try_get("valid_from").map_err(map_db_error)?,
        valid_until: r.try_get("valid_until").map_err(map_db_error)?,
        grace_days: r.try_get("grace_days").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        origin: EntitlementOrigin::Stored,
    })
}

/// `platform.tenant_entitlement` (read by the gate under the request's tenant RLS scope).
pub struct PgEntitlementStore {
    pool: PgPool,
}

impl PgEntitlementStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl EntitlementStore for PgEntitlementStore {
    async fn load(&self, tenant: TenantId) -> Result<Option<TenantEntitlement>, KernelError> {
        let row = sqlx::query(&format!(
            "SELECT {ENT_COLUMNS} FROM platform.tenant_entitlement WHERE tenant_id = $1"
        ))
        .bind(tenant.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.as_ref().map(entitlement_row).transpose()
    }
}

/// `SUTRA_ENTITLEMENT_STORE=memory` (tests without a database): nothing stored, every tenant
/// gets the configured default.
pub struct NoStoredEntitlements;

#[async_trait::async_trait]
impl EntitlementStore for NoStoredEntitlements {
    async fn load(&self, _tenant: TenantId) -> Result<Option<TenantEntitlement>, KernelError> {
        Ok(None)
    }
}

/// Handler state.
#[derive(Clone)]
pub struct EntitlementState {
    pub pool: PgPool,
    pub policy: EntitlementPolicy,
}

pub fn router(state: EntitlementState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(get_entitlement))
        .routes(routes!(get_catalogue))
        .routes(routes!(list_requests, create_request))
        .routes(routes!(request_decision))
        .routes(routes!(withdraw_request))
        .with_state(state)
}

// ---------------------------------------------------------------------------------------------
// Views and commands
// ---------------------------------------------------------------------------------------------

/// The registry: licensable modules (with dependencies), the always-available core, packages.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementCatalogue {
    pub version: u32,
    pub modules: Vec<ModuleSpec>,
    pub core: Vec<CoreSpec>,
    pub packages: Vec<PackageSpec>,
    pub default_package: String,
}

/// Request a change of the tenant's entitlement: the full desired state.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementChangeRequest {
    /// `activate` · `change` · `renew` · `suspend` · `reinstate`.
    pub kind: String,
    /// Package key (`core_academic`, `academic_plus`, `institution_operations`, `complete_erp`).
    pub package: String,
    /// Modules added on top of the package.
    #[serde(default)]
    pub add_on_modules: Vec<String>,
    /// `active` (default) · `grace` · `expired` · `suspended` (only with kind `suspend`).
    #[serde(default)]
    pub licence_status: Option<String>,
    /// Start of validity (default: now).
    #[serde(default)]
    pub valid_from: Option<DateTime<Utc>>,
    /// End of validity (`null` = perpetual).
    #[serde(default)]
    pub valid_until: Option<DateTime<Utc>>,
    /// Days of full access after `validUntil` (0–365, default 0).
    #[serde(default)]
    pub grace_days: Option<i32>,
    pub reason: String,
}

/// One entitlement change request.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementRequestView {
    pub id: Uuid,
    pub kind: String,
    pub package: Option<String>,
    pub add_on_modules: Vec<String>,
    pub licence_status: String,
    pub valid_from: DateTime<Utc>,
    pub valid_until: Option<DateTime<Utc>>,
    pub grace_days: i32,
    /// Entitlement version the request was made against (0 = none stored yet).
    pub base_version: i64,
    pub reason: String,
    /// `pending` · `approved` · `rejected` · `withdrawn`.
    pub status: String,
    pub requested_by: String,
    pub requested_at: DateTime<Utc>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
}

/// Entitlement change requests of the tenant, newest first.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementRequestPage {
    pub items: Vec<EntitlementRequestView>,
    pub total: usize,
}

/// Decision on a pending request.
#[derive(Clone, Debug, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementDecision {
    /// `approve` or `reject`.
    pub decision: String,
    pub reason: String,
}

/// A decided request and the tenant's entitlement after it.
#[derive(Clone, Debug, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitlementDecided {
    pub request: EntitlementRequestView,
    pub entitlement: EffectiveEntitlement,
}

const KINDS: [&str; 5] = ["activate", "change", "renew", "suspend", "reinstate"];

const REQUEST_COLUMNS: &str = "id, kind, package, add_on_modules, licence_status, valid_from, \
     valid_until, grace_days, base_version, reason, status, requested_by, requested_at, \
     decided_by, decided_at, decision_reason";

fn request_view(r: &PgRow) -> Result<EntitlementRequestView, KernelError> {
    let get = map_db_error;
    Ok(EntitlementRequestView {
        id: r.try_get("id").map_err(get)?,
        kind: r.try_get("kind").map_err(get)?,
        package: r.try_get("package").map_err(get)?,
        add_on_modules: r.try_get("add_on_modules").map_err(get)?,
        licence_status: r.try_get("licence_status").map_err(get)?,
        valid_from: r.try_get("valid_from").map_err(get)?,
        valid_until: r.try_get("valid_until").map_err(get)?,
        grace_days: r.try_get("grace_days").map_err(get)?,
        base_version: r.try_get("base_version").map_err(get)?,
        reason: r.try_get("reason").map_err(get)?,
        status: r.try_get("status").map_err(get)?,
        requested_by: r.try_get("requested_by").map_err(get)?,
        requested_at: r.try_get("requested_at").map_err(get)?,
        decided_by: r.try_get("decided_by").map_err(get)?,
        decided_at: r.try_get("decided_at").map_err(get)?,
        decision_reason: r.try_get("decision_reason").map_err(get)?,
    })
}

fn invalid(field: &str, code: &str, msg: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, msg)])
}

fn reason(r: &str) -> Result<String, KernelError> {
    let t = r.trim();
    if t.is_empty() || t.chars().count() > 1000 {
        return Err(invalid(
            "reason",
            "length",
            "a reason of 1–1000 characters is required",
        ));
    }
    Ok(t.to_owned())
}

fn reg() -> Result<&'static Registry, KernelError> {
    registry().map_err(|e| KernelError::Unavailable(e.to_owned()))
}

fn entitlement_audit(
    ctx: &AuthContext,
    action: &str,
    request_id: Uuid,
    details: serde_json::Value,
) -> AuditEntry {
    AuditEntry::new(ctx.scope, action, AuditOutcome::Succeeded)
        .actor(ActorKind::User, Some(ctx.subject.clone()))
        .target(
            "platform.tenant_entitlement_request",
            request_id.to_string(),
        )
        .details(details)
}

fn pending_conflict(e: sqlx::Error) -> KernelError {
    if let sqlx::Error::Database(db) = &e {
        if db.is_unique_violation() {
            return KernelError::Conflict(
                "an entitlement change request is already pending for this institution; \
                 decide or withdraw it first"
                    .into(),
            );
        }
    }
    map_db_error(e)
}

async fn pending_for_update(
    conn: &mut sqlx::PgConnection,
    tenant: Uuid,
    id: Uuid,
) -> Result<EntitlementRequestView, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM platform.tenant_entitlement_request \
         WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(tenant)
    .bind(id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "entitlement request",
    })?;
    let req = request_view(&row)?;
    if req.status != "pending" {
        return Err(KernelError::Conflict(format!(
            "the entitlement request is {}",
            req.status
        )));
    }
    Ok(req)
}

// ---------------------------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------------------------

/// Active modules and licence state of the caller's institution (drives navigation and the
/// renewal / expiry banner). Audit C3: the full record (package, add-ons, validity window,
/// grace) only for `platform.entitlement.manage` / `platform.entitlement.approve`; every other
/// session gets [`EffectiveEntitlement::summary_only`] (modules, status, read-only flag).
#[utoipa::path(
    get,
    path = "/api/v1/platform/entitlement",
    operation_id = "platform_entitlement_get",
    tag = "platform-entitlement",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = EffectiveEntitlement),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_entitlement(
    State(st): State<EntitlementState>,
    auth: Authenticated,
) -> ApiResult<Json<EffectiveEntitlement>> {
    let full = st.policy.effective(auth.scope().tenant_id).await?;
    let ctx = &auth.0;
    if ctx.has_permission(Permission::EntitlementManage)
        || ctx.has_permission(Permission::EntitlementApprove)
    {
        Ok(Json(full))
    } else {
        Ok(Json(full.summary_only()))
    }
}

/// The module / package registry (modules, dependencies, packages, always-available core).
#[utoipa::path(
    get,
    path = "/api/v1/platform/entitlement/catalogue",
    operation_id = "platform_entitlement_catalogue",
    tag = "platform-entitlement",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = EntitlementCatalogue),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 503, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_catalogue(_auth: Authenticated) -> ApiResult<Json<EntitlementCatalogue>> {
    let r = reg()?;
    Ok(Json(EntitlementCatalogue {
        version: r.version,
        modules: r.modules.clone(),
        core: r.core.clone(),
        packages: r.packages.clone(),
        default_package: r.default_package.clone(),
    }))
}

/// Entitlement change requests of the session tenant, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/platform/entitlement-requests",
    operation_id = "platform_entitlement_request_list",
    tag = "platform-entitlement",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = EntitlementRequestPage),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list_requests(
    State(st): State<EntitlementState>,
    auth: Authenticated,
) -> ApiResult<Json<EntitlementRequestPage>> {
    let ctx = auth
        .require(Permission::EntitlementManage)
        .or_else(|_| auth.require(Permission::EntitlementApprove))?;
    let rows = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM platform.tenant_entitlement_request \
         WHERE tenant_id = $1 ORDER BY requested_at DESC, id DESC LIMIT 200"
    ))
    .bind(ctx.scope.tenant_id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(request_view)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(EntitlementRequestPage {
        total: items.len(),
        items,
    }))
}

/// Requests an entitlement change (activation, package change, renewal, suspension,
/// reinstatement). Validated against the registry, dependencies included; waits for a second
/// administrator's decision.
#[utoipa::path(
    post,
    path = "/api/v1/platform/entitlement-requests",
    operation_id = "platform_entitlement_request_create",
    tag = "platform-entitlement",
    request_body = EntitlementChangeRequest,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 201, body = EntitlementRequestView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "A request is already pending", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Unknown package / module, missing dependency, invalid validity", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create_request(
    State(st): State<EntitlementState>,
    auth: Authenticated,
    Json(cmd): Json<EntitlementChangeRequest>,
) -> ApiResult<(StatusCode, Json<EntitlementRequestView>)> {
    let ctx = auth.require(Permission::EntitlementManage)?;
    let r = reg()?;
    let kind = cmd.kind.trim();
    if !KINDS.contains(&kind) {
        return Err(invalid(
            "kind",
            "enum",
            "kind must be one of activate, change, renew, suspend, reinstate",
        )
        .into());
    }
    let status = match cmd.licence_status.as_deref().map(str::trim) {
        None | Some("") if kind == "suspend" => LicenceStatus::Suspended,
        None | Some("") => LicenceStatus::Active,
        Some(s) => LicenceStatus::parse(s).ok_or_else(|| {
            invalid(
                "licenceStatus",
                "enum",
                "licenceStatus must be active, grace, expired or suspended",
            )
        })?,
    };
    if (kind == "suspend") != (status == LicenceStatus::Suspended) {
        return Err(invalid(
            "licenceStatus",
            "kind_mismatch",
            "a suspension is requested with kind `suspend` and licenceStatus `suspended`",
        )
        .into());
    }
    let package = cmd.package.trim().to_owned();
    let mut add_ons: Vec<String> = cmd
        .add_on_modules
        .iter()
        .map(|m| m.trim().to_owned())
        .collect();
    add_ons.sort();
    add_ons.dedup();
    r.resolve(&package, &add_ons)
        .map_err(KernelError::Validation)?;
    let valid_from = cmd.valid_from.unwrap_or_else(|| st.policy.now());
    if cmd.valid_until.is_some_and(|u| u <= valid_from) {
        return Err(invalid(
            "validUntil",
            "after_valid_from",
            "validUntil must be after validFrom",
        )
        .into());
    }
    let grace_days = cmd.grace_days.unwrap_or(0);
    if !(0..=MAX_GRACE_DAYS).contains(&grace_days) {
        return Err(invalid("graceDays", "range", "graceDays must be 0–365").into());
    }
    let why = reason(&cmd.reason)?;
    let tenant = ctx.scope.tenant_id.as_uuid();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let base_version: i64 = sqlx::query_scalar::<_, i64>(
        "SELECT version FROM platform.tenant_entitlement WHERE tenant_id = $1",
    )
    .bind(tenant)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .unwrap_or(0);
    let row = sqlx::query(&format!(
        "INSERT INTO platform.tenant_entitlement_request (tenant_id, kind, package, \
           add_on_modules, licence_status, valid_from, valid_until, grace_days, base_version, \
           reason, requested_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {REQUEST_COLUMNS}"
    ))
    .bind(tenant)
    .bind(kind)
    .bind(&package)
    .bind(&add_ons)
    .bind(status.as_str())
    .bind(valid_from)
    .bind(cmd.valid_until)
    .bind(grace_days)
    .bind(base_version)
    .bind(&why)
    .bind(&ctx.subject)
    .fetch_one(&mut *tx)
    .await
    .map_err(pending_conflict)?;
    let view = request_view(&row)?;
    audit::insert(
        &mut *tx,
        &entitlement_audit(
            ctx,
            "platform.entitlement.change_requested",
            view.id,
            serde_json::json!({
                "kind": view.kind, "package": view.package, "addOnModules": view.add_on_modules,
                "licenceStatus": view.licence_status, "validFrom": view.valid_from,
                "validUntil": view.valid_until, "graceDays": view.grace_days,
                "baseVersion": view.base_version, "reason": why,
            }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(view)))
}

/// Decides a pending entitlement change. The decider must hold `platform.entitlement.approve`
/// and must not be the requester (separation of duties). Approval re-validates the request
/// against the registry and the current entitlement version and applies it at once.
#[utoipa::path(
    post,
    path = "/api/v1/platform/entitlement-requests/{id}/decision",
    operation_id = "platform_entitlement_request_decision",
    tag = "platform-entitlement",
    params(("id" = String, Path, format = Uuid)),
    request_body = EntitlementDecision,
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = EntitlementDecided),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing permission / step-up, or the requester", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Not pending, or the entitlement changed since the request", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn request_decision(
    State(st): State<EntitlementState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Json(cmd): Json<EntitlementDecision>,
) -> ApiResult<Json<EntitlementDecided>> {
    let ctx = auth.require(Permission::EntitlementApprove)?;
    let approve = match cmd.decision.trim() {
        "approve" => true,
        "reject" => false,
        _ => return Err(invalid("decision", "enum", "decision must be approve or reject").into()),
    };
    let why = reason(&cmd.reason)?;
    let tenant = ctx.scope.tenant_id.as_uuid();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let req = pending_for_update(&mut tx, tenant, id).await?;
    ensure_independent_decider(
        Some(ctx.subject.as_str()),
        [req.requested_by.as_str()],
        "entitlement change",
    )?;
    if approve {
        reg()?
            .resolve(
                req.package.as_deref().unwrap_or_default(),
                &req.add_on_modules,
            )
            .map_err(KernelError::Validation)?;
        let current: i64 = sqlx::query_scalar::<_, i64>(
            "SELECT version FROM platform.tenant_entitlement WHERE tenant_id = $1 FOR UPDATE",
        )
        .bind(tenant)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?
        .unwrap_or(0);
        if current != req.base_version {
            return Err(KernelError::Conflict(
                "the entitlement changed since this request was made; raise a new request".into(),
            )
            .into());
        }
        sqlx::query(
            "INSERT INTO platform.tenant_entitlement (tenant_id, package, add_on_modules, \
               licence_status, valid_from, valid_until, grace_days, version, request_id, \
               updated_by) VALUES ($1, $2, $3, $4, $5, $6, $7, 1, $8, $9) \
             ON CONFLICT (tenant_id) DO UPDATE SET package = EXCLUDED.package, \
               add_on_modules = EXCLUDED.add_on_modules, \
               licence_status = EXCLUDED.licence_status, valid_from = EXCLUDED.valid_from, \
               valid_until = EXCLUDED.valid_until, grace_days = EXCLUDED.grace_days, \
               version = platform.tenant_entitlement.version + 1, \
               request_id = EXCLUDED.request_id, updated_by = EXCLUDED.updated_by, \
               updated_at = now()",
        )
        .bind(tenant)
        .bind(&req.package)
        .bind(&req.add_on_modules)
        .bind(&req.licence_status)
        .bind(req.valid_from)
        .bind(req.valid_until)
        .bind(req.grace_days)
        .bind(req.id)
        .bind(&ctx.subject)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!(
        "UPDATE platform.tenant_entitlement_request SET status = $2, decided_by = $3, \
           decided_at = now(), decision_reason = $4 WHERE id = $1 RETURNING {REQUEST_COLUMNS}"
    ))
    .bind(id)
    .bind(if approve { "approved" } else { "rejected" })
    .bind(&ctx.subject)
    .bind(&why)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let view = request_view(&row)?;
    audit::insert(
        &mut *tx,
        &entitlement_audit(
            ctx,
            if approve {
                "platform.entitlement.change_approved"
            } else {
                "platform.entitlement.change_rejected"
            },
            view.id,
            serde_json::json!({
                "kind": view.kind, "package": view.package, "addOnModules": view.add_on_modules,
                "licenceStatus": view.licence_status, "validFrom": view.valid_from,
                "validUntil": view.valid_until, "graceDays": view.grace_days,
                "requestedBy": view.requested_by, "reason": why,
            }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    let entitlement = st.policy.effective(ctx.scope.tenant_id).await?;
    Ok(Json(EntitlementDecided {
        request: view,
        entitlement,
    }))
}

/// Withdraws one's own pending request.
#[utoipa::path(
    post,
    path = "/api/v1/platform/entitlement-requests/{id}/withdraw",
    operation_id = "platform_entitlement_request_withdraw",
    tag = "platform-entitlement",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = EntitlementRequestView),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Not the requester", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn withdraw_request(
    State(st): State<EntitlementState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<EntitlementRequestView>> {
    let ctx = auth.require(Permission::EntitlementManage)?;
    let tenant = ctx.scope.tenant_id.as_uuid();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let req = pending_for_update(&mut tx, tenant, id).await?;
    if req.requested_by != ctx.subject {
        return Err(KernelError::Forbidden(
            "only the requester can withdraw an entitlement request".into(),
        )
        .into());
    }
    let row = sqlx::query(&format!(
        "UPDATE platform.tenant_entitlement_request SET status = 'withdrawn', decided_by = $2, \
           decided_at = now(), decision_reason = 'withdrawn by the requester' \
         WHERE id = $1 RETURNING {REQUEST_COLUMNS}"
    ))
    .bind(id)
    .bind(&ctx.subject)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let view = request_view(&row)?;
    audit::insert(
        &mut *tx,
        &entitlement_audit(
            ctx,
            "platform.entitlement.change_withdrawn",
            view.id,
            serde_json::json!({ "kind": view.kind }),
        ),
    )
    .await
    .map_err(map_db_error)?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(Json(view))
}
