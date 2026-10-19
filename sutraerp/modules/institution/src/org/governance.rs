//! Governance configuration of organisation units (L3-B-011): revisions per unit, each
//! `draft → published → superseded`. A published revision governs from its business-local
//! `effectiveFrom` (never earlier); activating a successor closes the predecessor the day before
//! (`effectiveTo`), so two revisions never govern the same unit on the same day.

use super::domain::{DraftGovernanceConfig, GovernanceConfig, GovernanceSettings};
use super::unit;
use crate::lifecycle::common::{
    business_today, check_version, expect_version, invalid, CommandErrors, CreateErrors, ReadErrors,
};
use crate::shared::guard::lock_institution_for_change;
use crate::shared::http::{actor, with_etag};
use crate::shared::publish::{audit, event};
use crate::shared::{Actor, InstitutionState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{Days, NaiveDate};
use sqlx::{PgConnection, PgPool, Row};
use sutra_contracts::institution::events::org::{
    GovernanceConfigActivatedV1, GOVERNANCE_CONFIG_ACTIVATED, GOVERNANCE_CONFIG_ACTIVATED_SCHEMA,
};
use sutra_contracts::institution::ids::{GovernanceConfigId, OrgUnitId};
use sutra_contracts::institution::status::{OrgUnitStatus, PublicationStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};

const COLUMNS: &str =
    "id, institution_id, org_unit_id, revision, status, supersedes_id, settings, \
    effective_from, effective_to, published_at, published_by, version, created_at, updated_at";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<GovernanceConfig, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    let sup: Option<uuid::Uuid> = r.try_get("supersedes_id").map_err(map_db_error)?;
    let settings: serde_json::Value = r.try_get("settings").map_err(map_db_error)?;
    Ok(GovernanceConfig {
        id: GovernanceConfigId(r.try_get("id").map_err(map_db_error)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        org_unit_id: OrgUnitId(r.try_get("org_unit_id").map_err(map_db_error)?),
        revision: r.try_get("revision").map_err(map_db_error)?,
        status: PublicationStatus::parse(&status)
            .ok_or_else(|| KernelError::internal_msg(format!("unknown status `{status}`")))?,
        supersedes_id: sup.map(GovernanceConfigId),
        settings: serde_json::from_value::<GovernanceSettings>(settings)
            .map_err(KernelError::internal)?,
        effective_from: r.try_get("effective_from").map_err(map_db_error)?,
        effective_to: r.try_get("effective_to").map_err(map_db_error)?,
        published_at: r.try_get("published_at").map_err(map_db_error)?,
        published_by: r.try_get("published_by").map_err(map_db_error)?,
        version: Version(r.try_get("version").map_err(map_db_error)?),
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

async fn load(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: GovernanceConfigId,
    for_update: bool,
) -> Result<GovernanceConfig, KernelError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.governance_config WHERE id = $1 AND tenant_id = $2{lock}"
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "governance_config",
    })?;
    from_row(&row)
}

fn inst_scope(tenant: &ExecutionScope, id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(tenant.tenant_id, id)
}

/// Latest published revision of a unit (the one without `effective_to`).
async fn latest_published(
    conn: &mut PgConnection,
    unit_id: OrgUnitId,
) -> Result<Option<GovernanceConfig>, KernelError> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.governance_config WHERE org_unit_id = $1 \
         AND status = 'published' ORDER BY effective_from DESC LIMIT 1"
    ))
    .bind(unit_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(|r| from_row(&r))
    .transpose()
}

/// Activation date check (pure): not retroactive and strictly after the predecessor's date.
pub fn check_activation(
    effective_from: NaiveDate,
    today: NaiveDate,
    predecessor_from: Option<NaiveDate>,
) -> Result<(), KernelError> {
    if effective_from < today {
        return Err(invalid(
            "effectiveFrom",
            "retroactive",
            format!("effectiveFrom must not be before today ({today}) in the institution timezone"),
        ));
    }
    if predecessor_from.is_some_and(|p| effective_from <= p) {
        return Err(invalid(
            "effectiveFrom",
            "overlap",
            "effectiveFrom must be after the current configuration's effective date",
        ));
    }
    Ok(())
}

/// Creates the next draft revision for a draft or active unit.
pub async fn create(
    pool: &PgPool,
    tenant: &ExecutionScope,
    unit_id: OrgUnitId,
    cmd: &DraftGovernanceConfig,
    actor: &Actor,
) -> Result<GovernanceConfig, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let u = unit::load(&mut tx, tenant, unit_id, true).await?;
    lock_institution_for_change(&mut tx, tenant, u.institution_id).await?;
    if !super::domain::accepts_new_children(u.status) {
        return Err(KernelError::Conflict(format!(
            "unit is {}; it cannot take a governance configuration",
            u.status
        )));
    }
    let latest = latest_published(&mut tx, unit_id).await?;
    let id = GovernanceConfigId::new();
    let row = sqlx::query(&format!(
        "INSERT INTO institution.governance_config (id, tenant_id, institution_id, org_unit_id, revision, supersedes_id, \
           settings, effective_from) \
         SELECT $1, $2, $3, $4, COALESCE(MAX(revision), 0) + 1, $5, $6, $7 FROM institution.governance_config \
         WHERE org_unit_id = $4 RETURNING {COLUMNS}"
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .bind(u.institution_id.as_uuid())
    .bind(unit_id.as_uuid())
    .bind(latest.as_ref().map(|l| l.id.as_uuid()))
    .bind(serde_json::to_value(&cmd.settings).map_err(KernelError::internal)?)
    .bind(cmd.effective_from)
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.constraint() == Some("governance_config_one_open") => {
            KernelError::Conflict("the unit already has a draft governance configuration".into())
        }
        _ => map_db_error(e),
    })?;
    let g = from_row(&row)?;
    audit(
        &mut tx,
        &inst_scope(tenant, u.institution_id),
        "institution.governance_config.create",
        "governance_config",
        id.to_string(),
        serde_json::json!({ "orgUnitId": unit_id, "revision": g.revision }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(g)
}

/// Replaces a draft revision.
pub async fn update(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: GovernanceConfigId,
    expected: Version,
    cmd: &DraftGovernanceConfig,
    actor: &Actor,
) -> Result<GovernanceConfig, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let g = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, g.institution_id).await?;
    check_version(expected, g.version.0)?;
    if g.status != PublicationStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "governance configuration is {}; only drafts can be changed",
            g.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE institution.governance_config SET settings = $1, effective_from = $2, version = version + 1, \
         updated_at = now() WHERE id = $3 RETURNING {COLUMNS}"
    ))
    .bind(serde_json::to_value(&cmd.settings).map_err(KernelError::internal)?)
    .bind(cmd.effective_from)
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row)?;
    audit(
        &mut tx,
        &inst_scope(tenant, g.institution_id),
        "institution.governance_config.update",
        "governance_config",
        id.to_string(),
        serde_json::json!({ "fromVersion": expected.0 }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Activates (publishes) a draft for an active unit; the predecessor is closed the day before.
pub async fn activate(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: GovernanceConfigId,
    expected: Version,
    actor: &Actor,
) -> Result<GovernanceConfig, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let g = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, g.institution_id).await?;
    check_version(expected, g.version.0)?;
    if g.status != PublicationStatus::Draft {
        return Err(crate::domain::illegal_transition(
            "governance_config",
            g.status,
            PublicationStatus::Published,
        ));
    }
    let u = unit::load(&mut tx, tenant, g.org_unit_id, false).await?;
    if u.status != OrgUnitStatus::Active {
        return Err(KernelError::Conflict(format!(
            "unit {} is {}; governance can only be activated for an active unit",
            u.code, u.status
        )));
    }
    let today = business_today(&mut *tx, tenant, g.institution_id).await?;
    let prev = latest_published(&mut tx, g.org_unit_id).await?;
    check_activation(
        g.effective_from,
        today,
        prev.as_ref().map(|p| p.effective_from),
    )?;
    if let Some(p) = &prev {
        let to = g
            .effective_from
            .checked_sub_days(Days::new(1))
            .unwrap_or(g.effective_from);
        sqlx::query(
            "UPDATE institution.governance_config SET status = 'superseded', effective_to = $1, \
             version = version + 1, updated_at = now() WHERE id = $2",
        )
        .bind(to)
        .bind(p.id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    let row = sqlx::query(&format!(
        "UPDATE institution.governance_config SET status = 'published', supersedes_id = $1, published_at = now(), \
         published_by = $2, version = version + 1, updated_at = now() WHERE id = $3 RETURNING {COLUMNS}"
    ))
    .bind(prev.as_ref().map(|p| p.id.as_uuid()))
    .bind(actor.id.as_deref())
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row)?;
    let es = inst_scope(tenant, g.institution_id);
    event(
        &mut tx,
        &es,
        GOVERNANCE_CONFIG_ACTIVATED,
        GOVERNANCE_CONFIG_ACTIVATED_SCHEMA,
        id.to_string(),
        &GovernanceConfigActivatedV1 {
            governance_config_id: id.as_uuid(),
            org_unit_id: g.org_unit_id.as_uuid(),
            institution_id: g.institution_id.as_uuid(),
            revision: g.revision,
            effective_from: g.effective_from,
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.governance_config.activate",
        "governance_config",
        id.to_string(),
        serde_json::json!({ "revision": g.revision, "effectiveFrom": g.effective_from, "supersedesId": prev.map(|p| p.id) }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Revisions of a unit, newest first.
pub async fn list(
    pool: &PgPool,
    tenant: &ExecutionScope,
    unit_id: OrgUnitId,
) -> Result<Vec<GovernanceConfig>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    unit::load(&mut conn, tenant, unit_id, false).await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.governance_config WHERE org_unit_id = $1 ORDER BY revision DESC"
    ))
    .bind(unit_id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter().map(from_row).collect()
}

/// The revision governing the unit on `as_of` (default today, institution timezone); drafts
/// and future-dated revisions never govern.
pub async fn effective(
    pool: &PgPool,
    tenant: &ExecutionScope,
    unit_id: OrgUnitId,
    as_of: Option<NaiveDate>,
) -> Result<GovernanceConfig, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    let u = unit::load(&mut conn, tenant, unit_id, false).await?;
    let as_of = match as_of {
        Some(d) => d,
        None => business_today(&mut *conn, tenant, u.institution_id).await?,
    };
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.governance_config WHERE org_unit_id = $1 \
         AND status IN ('published', 'superseded') AND effective_from <= $2 \
         AND (effective_to IS NULL OR effective_to >= $2) ORDER BY effective_from DESC LIMIT 1"
    ))
    .bind(unit_id.as_uuid())
    .bind(as_of)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .map(|r| from_row(&r))
    .transpose()?
    .ok_or(KernelError::NotFound {
        entity: "governance_config",
    })
}

// ------------------------------------------------------------------------------------ HTTP

/// Governance revisions of a unit.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct GovernanceConfigList {
    pub items: Vec<GovernanceConfig>,
}

/// `?asOf=` business-local date.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct GovernanceAsOf {
    /// Business-local date (`YYYY-MM-DD`); default today in the institution timezone.
    pub as_of: Option<NaiveDate>,
}

fn respond(status: StatusCode, g: GovernanceConfig) -> Response {
    with_etag(status, &g, g.version)
}

/// Create the next draft governance configuration of a unit (`organisation.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/org-units/{id}/governance-configs",
    operation_id = "governance_config_create",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DraftGovernanceConfig,
    responses((status = 201, body = GovernanceConfig, headers(("ETag" = String))), CreateErrors)
)]
pub async fn http_create(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgUnitId>,
    headers: HeaderMap,
    Json(cmd): Json<DraftGovernanceConfig>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    require_idempotency_key(&headers)?;
    Ok(respond(
        StatusCode::CREATED,
        create(&st.pool, &ctx.scope, id, &cmd, &actor(ctx)).await?,
    ))
}

/// Governance revisions of a unit, newest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/org-units/{id}/governance-configs",
    operation_id = "governance_config_list",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceConfigList), ReadErrors)
)]
pub async fn http_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgUnitId>,
) -> ApiResult<Json<GovernanceConfigList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(Json(GovernanceConfigList {
        items: list(&st.pool, &ctx.scope, id).await?,
    }))
}

/// The governance configuration in force for a unit on `asOf` (`institution.read`); 404 when none.
#[utoipa::path(
    get,
    path = "/api/v1/institution/org-units/{id}/effective-governance",
    operation_id = "governance_config_effective",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), GovernanceAsOf),
    security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceConfig, headers(("ETag" = String))), ReadErrors)
)]
pub async fn http_effective(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgUnitId>,
    Query(q): Query<GovernanceAsOf>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(respond(
        StatusCode::OK,
        effective(&st.pool, &ctx.scope, id, q.as_of).await?,
    ))
}

/// One governance configuration revision (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/governance-configs/{id}",
    operation_id = "governance_config_get",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = GovernanceConfig, headers(("ETag" = String))), ReadErrors)
)]
pub async fn http_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<GovernanceConfigId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(
        StatusCode::OK,
        load(&mut conn, &ctx.scope, id, false).await?,
    ))
}

/// Replace a draft governance configuration (`organisation.manage`).
#[utoipa::path(
    put,
    path = "/api/v1/institution/governance-configs/{id}",
    operation_id = "governance_config_update",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = DraftGovernanceConfig,
    responses((status = 200, body = GovernanceConfig, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_update(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<GovernanceConfigId>,
    headers: HeaderMap,
    Json(cmd): Json<DraftGovernanceConfig>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        update(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

/// Activate a draft governance configuration of an active unit (`organisation.manage`).
///
/// It governs from `effectiveFrom` (not before today, after the predecessor's date); the
/// predecessor becomes `superseded` and ends the day before.
#[utoipa::path(
    post,
    path = "/api/v1/institution/governance-configs/{id}/activate",
    operation_id = "governance_config_activate",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses((status = 200, body = GovernanceConfig, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_activate(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<GovernanceConfigId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        activate(&st.pool, &ctx.scope, id, expected, &actor(ctx)).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_dates() {
        let d = |s: &str| s.parse::<NaiveDate>().unwrap_or_default();
        assert!(check_activation(d("2026-10-12"), d("2026-10-12"), None).is_ok());
        assert!(check_activation(d("2026-10-11"), d("2026-10-12"), None).is_err());
        assert!(check_activation(d("2026-11-01"), d("2026-10-12"), Some(d("2026-11-01"))).is_err());
        assert!(check_activation(d("2026-11-02"), d("2026-10-12"), Some(d("2026-11-01"))).is_ok());
    }
}
