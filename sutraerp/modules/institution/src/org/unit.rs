//! Organisation units (L3-B-006..008, -010, -012, -016): register in `draft` under a parent
//! allowed by the effective type blueprint, activate, modify descriptive attributes, migrate
//! type version, suspend / restore, decommission (no live children or positions), archive.
//! Every version of a unit is kept in `org_unit_history`.

use super::blueprint;
use super::domain::{
    accepts_new_children, can_transition, MigrateOrgUnitType, ModifyOrgUnit, OrgUnit,
    OrgUnitHistoryEntry, RegisterOrgUnit,
};
use crate::domain::illegal_transition;
use crate::lifecycle::common::{
    business_today, check_version, expect_version, invalid, status_changed, CommandErrors,
    CreateErrors, ReadErrors,
};
use crate::lifecycle::domain::CommandBody;
use crate::shared::guard::lock_institution_for_change;
use crate::shared::http::{actor, with_etag};
use crate::shared::publish::{audit, event};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, InstitutionState};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::NaiveDate;
use sqlx::{PgConnection, PgPool, Row};
use sutra_contracts::institution::events::org::*;
use sutra_contracts::institution::events::ModifiedV1;
use sutra_contracts::institution::ids::{OrgRestructureId, OrgTypeBlueprintId, OrgUnitId};
use sutra_contracts::institution::status::{OrgUnitStatus, PublicationStatus};
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};

const COLUMNS: &str =
    "id, institution_id, parent_id, path, depth, unit_type, blueprint_id, code, name, \
    description, status, status_reason, version, created_at, updated_at";

pub(crate) fn parse_status(s: &str) -> Result<OrgUnitStatus, KernelError> {
    OrgUnitStatus::parse(s)
        .ok_or_else(|| KernelError::internal_msg(format!("unknown org unit status `{s}`")))
}

fn from_row(r: &sqlx::postgres::PgRow) -> Result<OrgUnit, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    let parent: Option<uuid::Uuid> = r.try_get("parent_id").map_err(map_db_error)?;
    Ok(OrgUnit {
        id: OrgUnitId(r.try_get("id").map_err(map_db_error)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        parent_id: parent.map(OrgUnitId),
        path: r.try_get("path").map_err(map_db_error)?,
        depth: r.try_get("depth").map_err(map_db_error)?,
        unit_type: r.try_get("unit_type").map_err(map_db_error)?,
        blueprint_id: OrgTypeBlueprintId(r.try_get("blueprint_id").map_err(map_db_error)?),
        code: r.try_get("code").map_err(map_db_error)?,
        name: r.try_get("name").map_err(map_db_error)?,
        description: r.try_get("description").map_err(map_db_error)?,
        status: parse_status(&status)?,
        status_reason: r.try_get("status_reason").map_err(map_db_error)?,
        version: Version(r.try_get("version").map_err(map_db_error)?),
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

/// One unit of the scope's tenant (404 when missing / foreign).
pub(crate) async fn load(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: OrgUnitId,
    for_update: bool,
) -> Result<OrgUnit, KernelError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_unit WHERE id = $1 AND tenant_id = $2{lock}"
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "org_unit" })?;
    from_row(&row)
}

/// Appends the unit's current representation to its history.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_history(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    u: &OrgUnit,
    change: &str,
    reason: Option<&str>,
    restructure_id: Option<OrgRestructureId>,
    effective_from: NaiveDate,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO institution.org_unit_history (tenant_id, institution_id, org_unit_id, unit_version, change, \
           parent_id, path, unit_type, blueprint_id, code, name, description, status, reason, restructure_id, \
           effective_from, recorded_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)",
    )
    .bind(tenant.tenant_id.as_uuid())
    .bind(u.institution_id.as_uuid())
    .bind(u.id.as_uuid())
    .bind(u.version.0)
    .bind(change)
    .bind(u.parent_id.map(|p| p.as_uuid()))
    .bind(&u.path)
    .bind(&u.unit_type)
    .bind(u.blueprint_id.as_uuid())
    .bind(&u.code)
    .bind(&u.name)
    .bind(&u.description)
    .bind(u.status.as_str())
    .bind(reason)
    .bind(restructure_id.map(|r| r.as_uuid()))
    .bind(effective_from)
    .bind(actor.id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

fn inst_scope(tenant: &ExecutionScope, id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(tenant.tenant_id, id)
}

/// Checks that `unit_type` (of blueprint `bp`) may sit under `parent` (or at the root).
pub(crate) fn check_placement(
    bp: &super::domain::OrgTypeBlueprint,
    unit_type: &str,
    parent_type: Option<&str>,
) -> Result<(), KernelError> {
    let t = bp.unit_type(unit_type).ok_or_else(|| {
        invalid(
            "unitType",
            "unknown-type",
            format!(
                "`{unit_type}` is not a unit type of blueprint version {}",
                bp.version_no
            ),
        )
    })?;
    match parent_type {
        None if !t.allow_root => Err(KernelError::Conflict(format!(
            "unit type `{unit_type}` cannot be the root unit"
        ))),
        Some(p) if !t.allowed_parent_types.iter().any(|a| a == p) => Err(KernelError::Conflict(
            format!("unit type `{unit_type}` is not allowed under a `{p}` unit"),
        )),
        _ => Ok(()),
    }
}

/// Registers a unit in `draft` (L3-B-006): requires a published type blueprint and an allowed
/// placement; the parent must belong to the same institution and accept children.
pub async fn register(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: &RegisterOrgUnit,
    actor: &Actor,
) -> Result<OrgUnit, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let unit = register_in(&mut tx, scope, institution_id, cmd, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(unit)
}

/// [`register`] inside the caller's transaction (Part 2 · S4-import runs it per row).
pub async fn register_in(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: &RegisterOrgUnit,
    actor: &Actor,
) -> Result<OrgUnit, KernelError> {
    cmd.validate()?;
    lock_institution_for_change(&mut *conn, scope, institution_id).await?;
    let bp = blueprint::current_published(&mut *conn, scope, institution_id)
        .await?
        .ok_or_else(|| {
            KernelError::Conflict(
                "publish an organisation type blueprint before registering units".into(),
            )
        })?;
    let parent = match cmd.parent_id {
        Some(pid) => {
            let p = load(&mut *conn, scope, pid, true).await.map_err(|e| match e {
                KernelError::NotFound { .. } => invalid(
                    "parentId",
                    "unknown",
                    "parentId is not a unit of this institution",
                ),
                e => e,
            })?;
            if p.institution_id != institution_id {
                return Err(invalid(
                    "parentId",
                    "unknown",
                    "parentId is not a unit of this institution",
                ));
            }
            if !accepts_new_children(p.status) {
                return Err(KernelError::Conflict(format!(
                    "parent unit is {}; it cannot take new units",
                    p.status
                )));
            }
            Some(p)
        }
        None => None,
    };
    let unit_type = cmd.unit_type.trim();
    check_placement(
        &bp,
        unit_type,
        parent.as_ref().map(|p| p.unit_type.as_str()),
    )?;
    let id = OrgUnitId::new();
    let (path, depth) = match &parent {
        Some(p) => (format!("{}{id}/", p.path), p.depth + 1),
        None => (format!("/{id}/"), 0),
    };
    let row = sqlx::query(&format!(
        "INSERT INTO institution.org_unit (id, tenant_id, institution_id, parent_id, path, depth, unit_type, \
           blueprint_id, code, name, description) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11) RETURNING {COLUMNS}"
    ))
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(parent.as_ref().map(|p| p.id.as_uuid()))
    .bind(&path)
    .bind(depth)
    .bind(unit_type)
    .bind(bp.id.as_uuid())
    .bind(cmd.code.trim())
    .bind(cmd.name.trim())
    .bind(crate::lifecycle::common::norm(cmd.description.as_deref()))
    .fetch_one(&mut *conn)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.constraint() == Some("org_unit_one_live_root") => {
            KernelError::Conflict("the institution already has a root unit".into())
        }
        _ => map_db_error(e),
    })?;
    let unit = from_row(&row)?;
    let today = business_today(&mut *conn, scope, institution_id).await?;
    record_history(
        &mut *conn,
        scope,
        &unit,
        "registered",
        None,
        None,
        today,
        actor,
    )
    .await?;
    let es = inst_scope(scope, institution_id);
    event(
        &mut *conn,
        &es,
        ORG_UNIT_REGISTERED,
        ORG_UNIT_REGISTERED_SCHEMA,
        id.to_string(),
        &OrgUnitRegisteredV1 {
            org_unit_id: id.as_uuid(),
            institution_id: institution_id.as_uuid(),
            parent_id: unit.parent_id.map(|p| p.as_uuid()),
            unit_type: unit.unit_type.clone(),
            code: unit.code.clone(),
            name: unit.name.clone(),
        },
        actor,
    )
    .await?;
    audit(
        &mut *conn,
        &es,
        "institution.org_unit.register",
        "org_unit",
        id.to_string(),
        serde_json::json!({ "code": unit.code, "unitType": unit.unit_type, "parentId": unit.parent_id, "blueprintId": bp.id }),
        actor,
    )
    .await?;
    Ok(unit)
}

/// Units of an institution in tree order (by path).
pub async fn list(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    q: &OrgUnitListQuery,
) -> Result<Vec<OrgUnit>, KernelError> {
    crate::shared::guard::institution_status(pool, scope, institution_id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_unit WHERE institution_id = $1 AND tenant_id = $2 \
         AND ($3::text IS NULL OR status = $3) AND ($4::uuid IS NULL OR parent_id = $4) ORDER BY path"
    ))
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(q.status.map(|s| s.as_str()))
    .bind(q.parent_id.map(|p| p.as_uuid()))
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    rows.iter().map(from_row).collect()
}

/// Ordinary governed modification (name / description) of a draft or active unit.
pub async fn modify(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: OrgUnitId,
    expected: Version,
    cmd: &ModifyOrgUnit,
    actor: &Actor,
) -> Result<OrgUnit, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let u = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, u.institution_id).await?;
    check_version(expected, u.version.0)?;
    if !accepts_new_children(u.status) {
        return Err(KernelError::Conflict(format!(
            "unit is {}; it cannot be modified",
            u.status
        )));
    }
    let name = cmd
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or(&u.name)
        .to_owned();
    let description = match &cmd.description {
        Some(d) => crate::lifecycle::common::norm(Some(d)),
        None => u.description.clone(),
    };
    let mut changed = vec![];
    if name != u.name {
        changed.push("name".to_owned());
    }
    if description != u.description {
        changed.push("description".to_owned());
    }
    if changed.is_empty() {
        tx.rollback().await.map_err(map_db_error)?;
        return Ok(u);
    }
    let row = sqlx::query(&format!(
        "UPDATE institution.org_unit SET name = $1, description = $2, version = version + 1, updated_at = now() \
         WHERE id = $3 RETURNING {COLUMNS}"
    ))
    .bind(&name)
    .bind(&description)
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row)?;
    let today = business_today(&mut *tx, tenant, u.institution_id).await?;
    record_history(&mut tx, tenant, &next, "modified", None, None, today, actor).await?;
    let es = inst_scope(tenant, u.institution_id);
    event(
        &mut tx,
        &es,
        ORG_UNIT_MODIFIED,
        ORG_UNIT_MODIFIED_SCHEMA,
        id.to_string(),
        &ModifiedV1 {
            aggregate_id: id.as_uuid(),
            institution_id: u.institution_id.as_uuid(),
            version: next.version.0,
            changed_fields: changed.clone(),
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.org_unit.modify",
        "org_unit",
        id.to_string(),
        serde_json::json!({ "changedFields": changed, "fromVersion": expected.0 }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Lifecycle commands of a unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnitCommand {
    Activate,
    Suspend,
    Restore,
    Decommission,
    Archive,
}

impl UnitCommand {
    fn target(self) -> OrgUnitStatus {
        match self {
            Self::Activate | Self::Restore => OrgUnitStatus::Active,
            Self::Suspend => OrgUnitStatus::Suspended,
            Self::Decommission => OrgUnitStatus::Decommissioned,
            Self::Archive => OrgUnitStatus::Archived,
        }
    }
    fn name(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::Suspend => "suspend",
            Self::Restore => "restore",
            Self::Decommission => "decommission",
            Self::Archive => "archive",
        }
    }
    fn history(self) -> &'static str {
        match self {
            Self::Activate => "activated",
            Self::Suspend => "suspended",
            Self::Restore => "restored",
            Self::Decommission => "decommissioned",
            Self::Archive => "archived",
        }
    }
    fn event(self) -> (&'static str, &'static str) {
        match self {
            Self::Activate => (ORG_UNIT_ACTIVATED, ORG_UNIT_ACTIVATED_SCHEMA),
            Self::Suspend => (ORG_UNIT_SUSPENDED, ORG_UNIT_SUSPENDED_SCHEMA),
            Self::Restore => (ORG_UNIT_RESTORED, ORG_UNIT_RESTORED_SCHEMA),
            Self::Decommission => (ORG_UNIT_DECOMMISSIONED, ORG_UNIT_DECOMMISSIONED_SCHEMA),
            Self::Archive => (ORG_UNIT_ARCHIVED, ORG_UNIT_ARCHIVED_SCHEMA),
        }
    }
    /// Suspension, decommissioning and archival need a reason as evidence.
    fn needs_reason(self) -> bool {
        matches!(self, Self::Suspend | Self::Decommission | Self::Archive)
    }
}

/// Live children and positions still placed in the unit (decommissioning preconditions).
pub(crate) async fn unresolved_dependants(
    conn: &mut PgConnection,
    unit: &OrgUnit,
    today: NaiveDate,
) -> Result<(i64, i64), KernelError> {
    sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM institution.org_unit WHERE parent_id = $1 \
              AND status IN ('draft', 'active', 'suspended')), \
           (SELECT count(*) FROM institution.position_definition p WHERE p.status IN ('draft', 'active') \
              AND EXISTS (SELECT 1 FROM institution.position_definition_revision r WHERE r.position_id = p.id \
                AND r.org_unit_id = $1 AND (r.effective_to IS NULL OR r.effective_to >= $2)))",
    )
    .bind(unit.id.as_uuid())
    .bind(today)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Applies one lifecycle transition to a locked unit (status, history, event, audit).
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply_status(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    u: &OrgUnit,
    command: UnitCommand,
    reason: Option<String>,
    restructure_id: Option<OrgRestructureId>,
    today: NaiveDate,
    actor: &Actor,
) -> Result<OrgUnit, KernelError> {
    let to = command.target();
    if !can_transition(u.status, to)
        || (command == UnitCommand::Restore)
            != (u.status == OrgUnitStatus::Suspended && to == OrgUnitStatus::Active)
    {
        return Err(illegal_transition("org_unit", u.status, to));
    }
    let row = sqlx::query(&format!(
        "UPDATE institution.org_unit SET status = $1, status_reason = $2, version = version + 1, updated_at = now() \
         WHERE id = $3 RETURNING {COLUMNS}"
    ))
    .bind(to.as_str())
    .bind(&reason)
    .bind(u.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row)?;
    record_history(
        conn,
        tenant,
        &next,
        command.history(),
        reason.as_deref(),
        restructure_id,
        today,
        actor,
    )
    .await?;
    let (et, schema) = command.event();
    let es = inst_scope(tenant, u.institution_id);
    event(
        conn,
        &es,
        et,
        schema,
        u.id.to_string(),
        &status_changed(
            u.id.as_uuid(),
            u.institution_id,
            u.status.as_str(),
            to.as_str(),
            next.version.0,
            reason.clone(),
            None,
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        &es,
        &format!("institution.org_unit.{}", command.name()),
        "org_unit",
        u.id.to_string(),
        serde_json::json!({ "fromStatus": u.status, "toStatus": to, "reason": reason, "restructureId": restructure_id }),
        actor,
    )
    .await?;
    Ok(next)
}

/// Runs a lifecycle command with its preconditions:
/// * activate — the unit's blueprint version is the published one; the parent is active;
/// * restore — the parent is active;
/// * decommission — no live child units and no draft/active positions placed in the unit.
pub async fn command(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: OrgUnitId,
    expected: Version,
    command: UnitCommand,
    body: &CommandBody,
    actor: &Actor,
) -> Result<OrgUnit, KernelError> {
    let reason = if command.needs_reason() {
        Some(body.required_reason()?)
    } else {
        body.optional_reason()?
    };
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let u = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, u.institution_id).await?;
    check_version(expected, u.version.0)?;
    let to = command.target();
    if !can_transition(u.status, to) {
        return Err(illegal_transition("org_unit", u.status, to));
    }
    let today = business_today(&mut *tx, tenant, u.institution_id).await?;
    match command {
        UnitCommand::Activate => {
            let bp = blueprint::load(&mut tx, tenant, u.blueprint_id, false).await?;
            if bp.status != PublicationStatus::Published {
                return Err(KernelError::Conflict(format!(
                    "the unit's type blueprint version {} is {}; migrate it to the effective version first",
                    bp.version_no, bp.status
                )));
            }
            require_active_parent(&mut tx, tenant, &u).await?;
        }
        UnitCommand::Restore => require_active_parent(&mut tx, tenant, &u).await?,
        UnitCommand::Decommission => {
            let (children, positions) = unresolved_dependants(&mut tx, &u, today).await?;
            if children > 0 || positions > 0 {
                return Err(KernelError::Conflict(format!(
                    "unit still has {children} live child units and {positions} draft/active positions; \
                     restructure or retire them first"
                )));
            }
        }
        UnitCommand::Suspend | UnitCommand::Archive => {}
    }
    let next = apply_status(&mut tx, tenant, &u, command, reason, None, today, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

async fn require_active_parent(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    u: &OrgUnit,
) -> Result<(), KernelError> {
    if let Some(pid) = u.parent_id {
        let p = load(conn, tenant, pid, false).await?;
        if p.status != OrgUnitStatus::Active {
            return Err(KernelError::Conflict(format!(
                "parent unit {} is {}; it must be active first",
                p.code, p.status
            )));
        }
    }
    Ok(())
}

/// Moves a locked unit (and its subtree's paths) under `new_parent` as part of a restructure.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn move_unit(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    u: &OrgUnit,
    new_parent: &OrgUnit,
    change: &str,
    restructure_id: OrgRestructureId,
    today: NaiveDate,
    actor: &Actor,
) -> Result<OrgUnit, KernelError> {
    let new_path = format!("{}{}/", new_parent.path, u.id);
    let delta = new_parent.depth + 1 - u.depth;
    // Descendants first (their path prefix changes; parent links stay).
    sqlx::query(
        "UPDATE institution.org_unit SET path = $1 || substr(path, length($2) + 1), depth = depth + $3, \
         updated_at = now() WHERE institution_id = $4 AND path LIKE $2 || '%' AND id <> $5",
    )
    .bind(&new_path)
    .bind(&u.path)
    .bind(delta)
    .bind(u.institution_id.as_uuid())
    .bind(u.id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "UPDATE institution.org_unit SET parent_id = $1, path = $2, depth = $3, version = version + 1, \
         updated_at = now() WHERE id = $4 RETURNING {COLUMNS}"
    ))
    .bind(new_parent.id.as_uuid())
    .bind(&new_path)
    .bind(new_parent.depth + 1)
    .bind(u.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row)?;
    record_history(
        conn,
        tenant,
        &next,
        change,
        None,
        Some(restructure_id),
        today,
        actor,
    )
    .await?;
    Ok(next)
}

/// Migrates a unit to a published type blueprint version (L3-B-010): the target type must
/// exist, and the unit's parent and live children must stay compatible.
pub async fn migrate_type(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: OrgUnitId,
    expected: Version,
    cmd: &MigrateOrgUnitType,
    actor: &Actor,
) -> Result<OrgUnit, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let u = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, u.institution_id).await?;
    check_version(expected, u.version.0)?;
    if !super::domain::is_live(u.status) {
        return Err(KernelError::Conflict(format!(
            "unit is {}; its type can no longer change",
            u.status
        )));
    }
    let target = blueprint::load(&mut tx, tenant, cmd.target_blueprint_id, false)
        .await
        .map_err(|e| match e {
            KernelError::NotFound { .. } => invalid(
                "targetBlueprintId",
                "unknown",
                "targetBlueprintId is not a blueprint of this tenant",
            ),
            e => e,
        })?;
    if target.institution_id != u.institution_id {
        return Err(invalid(
            "targetBlueprintId",
            "unknown",
            "targetBlueprintId belongs to another institution",
        ));
    }
    if target.status != PublicationStatus::Published {
        return Err(KernelError::Conflict(format!(
            "target blueprint version {} is {}; only the published version is eligible",
            target.version_no, target.status
        )));
    }
    let new_type = cmd
        .unit_type
        .as_deref()
        .map(str::trim)
        .unwrap_or(&u.unit_type)
        .to_owned();
    if target.id == u.blueprint_id && new_type == u.unit_type {
        return Err(invalid(
            "targetBlueprintId",
            "no-change",
            "the unit already uses this blueprint version and type",
        ));
    }
    let parent = match u.parent_id {
        Some(p) => Some(load(&mut tx, tenant, p, false).await?),
        None => None,
    };
    check_placement(
        &target,
        &new_type,
        parent.as_ref().map(|p| p.unit_type.as_str()),
    )?;
    // Live children keep their own blueprint association; each must still accept the new type
    // as parent.
    let children = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_unit WHERE parent_id = $1 AND status IN ('draft', 'active', 'suspended')"
    ))
    .bind(id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut incompatible = vec![];
    for c in &children {
        let c = from_row(c)?;
        let cbp = blueprint::load(&mut tx, tenant, c.blueprint_id, false).await?;
        let ok = cbp
            .unit_type(&c.unit_type)
            .is_some_and(|t| t.allowed_parent_types.contains(&new_type));
        if !ok {
            incompatible.push(c.code);
        }
    }
    if !incompatible.is_empty() {
        return Err(KernelError::Conflict(format!(
            "child units {} do not allow a `{new_type}` parent; migrate or restructure them first",
            incompatible.join(", ")
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE institution.org_unit SET blueprint_id = $1, unit_type = $2, version = version + 1, updated_at = now() \
         WHERE id = $3 RETURNING {COLUMNS}"
    ))
    .bind(target.id.as_uuid())
    .bind(&new_type)
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row)?;
    let today = business_today(&mut *tx, tenant, u.institution_id).await?;
    record_history(
        &mut tx,
        tenant,
        &next,
        "type_migrated",
        None,
        None,
        today,
        actor,
    )
    .await?;
    let es = inst_scope(tenant, u.institution_id);
    let mut changed = vec!["blueprintId".to_owned()];
    if new_type != u.unit_type {
        changed.push("unitType".to_owned());
    }
    event(
        &mut tx,
        &es,
        ORG_UNIT_MODIFIED,
        ORG_UNIT_MODIFIED_SCHEMA,
        id.to_string(),
        &ModifiedV1 {
            aggregate_id: id.as_uuid(),
            institution_id: u.institution_id.as_uuid(),
            version: next.version.0,
            changed_fields: changed,
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.org_unit.migrate_type",
        "org_unit",
        id.to_string(),
        serde_json::json!({
            "fromBlueprintId": u.blueprint_id, "toBlueprintId": target.id,
            "fromUnitType": u.unit_type, "toUnitType": new_type,
        }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// The unit's append-only history, oldest first.
pub async fn history(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: OrgUnitId,
) -> Result<Vec<OrgUnitHistoryEntry>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load(&mut conn, tenant, id, false).await?;
    let rows = sqlx::query(
        "SELECT unit_version, change, parent_id, path, unit_type, blueprint_id, code, name, description, status, \
           reason, restructure_id, effective_from, recorded_by, recorded_at \
         FROM institution.org_unit_history WHERE org_unit_id = $1 ORDER BY unit_version",
    )
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            let status: String = r.try_get("status").map_err(map_db_error)?;
            let parent: Option<uuid::Uuid> = r.try_get("parent_id").map_err(map_db_error)?;
            let rid: Option<uuid::Uuid> = r.try_get("restructure_id").map_err(map_db_error)?;
            Ok(OrgUnitHistoryEntry {
                unit_version: r.try_get("unit_version").map_err(map_db_error)?,
                change: r.try_get("change").map_err(map_db_error)?,
                parent_id: parent.map(OrgUnitId),
                path: r.try_get("path").map_err(map_db_error)?,
                unit_type: r.try_get("unit_type").map_err(map_db_error)?,
                blueprint_id: OrgTypeBlueprintId(r.try_get("blueprint_id").map_err(map_db_error)?),
                code: r.try_get("code").map_err(map_db_error)?,
                name: r.try_get("name").map_err(map_db_error)?,
                description: r.try_get("description").map_err(map_db_error)?,
                status: parse_status(&status)?,
                reason: r.try_get("reason").map_err(map_db_error)?,
                restructure_id: rid.map(OrgRestructureId),
                effective_from: r.try_get("effective_from").map_err(map_db_error)?,
                recorded_by: r.try_get("recorded_by").map_err(map_db_error)?,
                recorded_at: r.try_get("recorded_at").map_err(map_db_error)?,
            })
        })
        .collect()
}

// ------------------------------------------------------------------------------------ HTTP

/// Unit collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct OrgUnitList {
    pub items: Vec<OrgUnit>,
}

/// Unit history.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct OrgUnitHistory {
    pub items: Vec<OrgUnitHistoryEntry>,
}

/// Filters of `GET …/org-units`.
#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct OrgUnitListQuery {
    pub status: Option<OrgUnitStatus>,
    /// Only direct children of this unit.
    pub parent_id: Option<OrgUnitId>,
}

fn respond(status: StatusCode, u: OrgUnit) -> Response {
    with_etag(status, &u, u.version)
}

/// Register an organisation unit in `draft` (`organisation.manage`).
///
/// Needs a published type blueprint whose rules allow the unit type under the parent (or at the
/// root; one live root per institution).
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/org-units",
    operation_id = "org_unit_register",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RegisterOrgUnit,
    responses((status = 201, body = OrgUnit, headers(("ETag" = String))), CreateErrors)
)]
pub async fn http_register(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<RegisterOrgUnit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    Ok(respond(
        StatusCode::CREATED,
        register(&st.pool, &scope, id, &cmd, &actor(ctx)).await?,
    ))
}

/// Organisation units of an institution in tree order (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/org-units",
    operation_id = "org_unit_list",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), OrgUnitListQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = OrgUnitList), ReadErrors)
)]
pub async fn http_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    Query(q): Query<OrgUnitListQuery>,
) -> ApiResult<Json<OrgUnitList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(OrgUnitList {
        items: list(&st.pool, &scope, id, &q).await?,
    }))
}

/// One organisation unit (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/org-units/{id}",
    operation_id = "org_unit_get",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OrgUnit, headers(("ETag" = String))), ReadErrors)
)]
pub async fn http_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgUnitId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(
        StatusCode::OK,
        load(&mut conn, &ctx.scope, id, false).await?,
    ))
}

/// Modify a draft or active unit's name / description (`organisation.manage`). `parentId`,
/// `unitType`, `status` and `code` are refused (422).
#[utoipa::path(
    patch,
    path = "/api/v1/institution/org-units/{id}",
    operation_id = "org_unit_modify",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = ModifyOrgUnit,
    responses((status = 200, body = OrgUnit, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_modify(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgUnitId>,
    headers: HeaderMap,
    Json(cmd): Json<ModifyOrgUnit>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        modify(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

async fn run(
    st: InstitutionState,
    auth: Authenticated,
    id: OrgUnitId,
    headers: HeaderMap,
    body: Option<Json<CommandBody>>,
    cmd: UnitCommand,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    let body = body.map(|b| b.0).unwrap_or_default();
    Ok(respond(
        StatusCode::OK,
        command(&st.pool, &ctx.scope, id, expected, cmd, &body, &actor(ctx)).await?,
    ))
}

macro_rules! unit_command {
    ($fn:ident, $verb:literal, $op:literal, $cmd:expr, $doc:literal) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = concat!("/api/v1/institution/org-units/{id}/", $verb),
            operation_id = $op,
            tag = "organisation",
            params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<CommandBody>, description = "Optional `{ reason }`"),
            responses((status = 200, body = OrgUnit, headers(("ETag" = String))), CommandErrors)
        )]
        pub async fn $fn(
            State(st): State<InstitutionState>,
            auth: Authenticated,
            Path(id): Path<OrgUnitId>,
            headers: HeaderMap,
            body: Option<Json<CommandBody>>,
        ) -> ApiResult<Response> {
            run(st, auth, id, headers, body, $cmd).await
        }
    };
}

unit_command!(
    http_activate,
    "activate",
    "org_unit_activate",
    UnitCommand::Activate,
    "Activate a draft unit (`organisation.manage`): its blueprint version must be the published one and its parent active."
);
unit_command!(
    http_suspend,
    "suspend",
    "org_unit_suspend",
    UnitCommand::Suspend,
    "Suspend an active unit; reason required (`organisation.manage`). Suspended units take no new units or positions."
);
unit_command!(
    http_restore,
    "restore",
    "org_unit_restore",
    UnitCommand::Restore,
    "Restore a suspended unit to active; its parent must be active (`organisation.manage`)."
);
unit_command!(
    http_decommission,
    "decommission",
    "org_unit_decommission",
    UnitCommand::Decommission,
    "Decommission an active or suspended unit; reason required. Refused while live child units or draft/active positions remain (`organisation.manage`)."
);
unit_command!(
    http_archive,
    "archive",
    "org_unit_archive",
    UnitCommand::Archive,
    "Archive a decommissioned unit (terminal, read-only); reason required (`organisation.manage`)."
);

/// Migrate a unit to the published type blueprint version (`organisation.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/institution/org-units/{id}/migrate-type",
    operation_id = "org_unit_migrate_type",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = MigrateOrgUnitType,
    responses((status = 200, body = OrgUnit, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_migrate_type(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgUnitId>,
    headers: HeaderMap,
    Json(cmd): Json<MigrateOrgUnitType>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        migrate_type(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

/// Every recorded version of a unit, oldest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/org-units/{id}/history",
    operation_id = "org_unit_history",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OrgUnitHistory), ReadErrors)
)]
pub async fn http_history(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgUnitId>,
) -> ApiResult<Json<OrgUnitHistory>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    Ok(Json(OrgUnitHistory {
        items: history(&st.pool, &ctx.scope, id).await?,
    }))
}
