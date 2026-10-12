//! Approved organisation structure transformation (L3-B-009): `move` (reparent units with
//! their subtrees) or `merge` (children of the sources move under the target; the sources are
//! decommissioned). Validated against the current structure (no cycles, blueprint parent
//! rules), approved through the WorkflowPort (`ORG_RESTRUCTURE`), and applied only if the
//! structure baseline it was validated against is unchanged (L4-B-009-BH-006). Unit history
//! keeps every previous parent (no retroactive rewrite).

use super::blueprint;
use super::domain::{
    accepts_new_children, find_cycle, ApprovalDecision, Decide, OrgRestructure, OrgTypeBlueprint,
    OrgUnit, RequestRestructure, RequestedMove, RestructureKind, RestructureMove,
};
use super::unit::{self, UnitCommand};
use crate::lifecycle::common::{
    business_today, check_version, expect_version, invalid, CommandErrors, CreateErrors, ReadErrors,
};
use crate::shared::approval::{self, process, ApprovalOutcome};
use crate::shared::guard::lock_institution_for_change;
use crate::shared::http::{actor, with_etag};
use crate::shared::publish::{audit, event};
use crate::shared::scope::institution_scope;
use crate::shared::{Actor, InstitutionState};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::institution::events::org::{
    OrgStructureRestructuredV1, OrgUnitMoveV1, ORG_STRUCTURE_RESTRUCTURED,
    ORG_STRUCTURE_RESTRUCTURED_SCHEMA,
};
use sutra_contracts::institution::ids::{OrgRestructureId, OrgTypeBlueprintId, OrgUnitId};
use sutra_contracts::institution::status::OrgUnitStatus;
use sutra_kernel::auth::Permission;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated};
use uuid::Uuid;

/// The concrete effect of a transformation on the current structure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    /// (unit, from parent, to parent) in application order.
    pub moves: Vec<(OrgUnitId, Option<OrgUnitId>, OrgUnitId)>,
    /// Merge sources to decommission after the moves.
    pub decommission: Vec<OrgUnitId>,
    /// Units whose version must be unchanged when applying (requested units / sources).
    pub baseline: Vec<(OrgUnitId, i64)>,
}

fn type_allowed(
    blueprints: &HashMap<OrgTypeBlueprintId, OrgTypeBlueprint>,
    child: &OrgUnit,
    parent: &OrgUnit,
) -> bool {
    blueprints
        .get(&child.blueprint_id)
        .and_then(|bp| bp.unit_type(&child.unit_type))
        .is_some_and(|t| t.allowed_parent_types.contains(&parent.unit_type))
}

/// Plans a transformation over the live units of one institution (pure). `positions` counts
/// draft/active positions per unit (merge sources must have none).
pub fn plan(
    cmd: &RequestRestructure,
    units: &[OrgUnit],
    blueprints: &HashMap<OrgTypeBlueprintId, OrgTypeBlueprint>,
    positions: &HashMap<OrgUnitId, i64>,
) -> Result<Plan, KernelError> {
    let by_id: HashMap<OrgUnitId, &OrgUnit> = units.iter().map(|u| (u.id, u)).collect();
    let find = |id: OrgUnitId, field: &str| {
        by_id.get(&id).copied().ok_or_else(|| {
            invalid(
                field,
                "unknown",
                format!("{id} is not a live unit of this institution"),
            )
        })
    };
    let parents: HashMap<Uuid, Option<Uuid>> = units
        .iter()
        .map(|u| (u.id.as_uuid(), u.parent_id.map(|p| p.as_uuid())))
        .collect();
    let mut out = Plan {
        moves: vec![],
        decommission: vec![],
        baseline: vec![],
    };
    match cmd.kind {
        RestructureKind::Move => {
            for m in &cmd.moves {
                let u = find(m.org_unit_id, "moves.orgUnitId")?;
                let to = find(m.to_parent_id, "moves.toParentId")?;
                if !accepts_new_children(u.status) {
                    return Err(KernelError::Conflict(format!(
                        "unit {} is {}; only draft or active units can be moved",
                        u.code, u.status
                    )));
                }
                let Some(from) = u.parent_id else {
                    return Err(invalid(
                        "moves.orgUnitId",
                        "root",
                        "the root unit cannot be moved",
                    ));
                };
                if from == to.id {
                    return Err(invalid(
                        "moves.toParentId",
                        "no-change",
                        format!("unit {} already sits under {}", u.code, to.code),
                    ));
                }
                if !accepts_new_children(to.status) {
                    return Err(KernelError::Conflict(format!(
                        "target parent {} is {}; it cannot take units",
                        to.code, to.status
                    )));
                }
                if !type_allowed(blueprints, u, to) {
                    return Err(KernelError::Conflict(format!(
                        "unit type `{}` ({}) is not allowed under a `{}` unit ({})",
                        u.unit_type, u.code, to.unit_type, to.code
                    )));
                }
                out.moves.push((u.id, Some(from), to.id));
                out.baseline.push((u.id, u.version.0));
            }
            let proposed: Vec<(Uuid, Uuid)> = out
                .moves
                .iter()
                .map(|(u, _, to)| (u.as_uuid(), to.as_uuid()))
                .collect();
            if let Some(c) = find_cycle(&parents, &proposed) {
                return Err(KernelError::Conflict(format!(
                    "the transformation would make unit {} its own ancestor (hierarchy cycle)",
                    by_id
                        .get(&OrgUnitId(c))
                        .map_or_else(|| c.to_string(), |u| u.code.clone())
                )));
            }
        }
        RestructureKind::Merge => {
            let target_id = cmd
                .merge_target_id
                .ok_or_else(|| invalid("mergeTargetId", "required", "mergeTargetId is required"))?;
            let target = find(target_id, "mergeTargetId")?;
            if target.status != OrgUnitStatus::Active {
                return Err(KernelError::Conflict(format!(
                    "merge target {} is {}; it must be active",
                    target.code, target.status
                )));
            }
            let sources: Vec<&OrgUnit> = cmd
                .merge_source_ids
                .iter()
                .map(|s| find(*s, "mergeSourceIds"))
                .collect::<Result<_, _>>()?;
            for s in &sources {
                if !matches!(s.status, OrgUnitStatus::Active | OrgUnitStatus::Suspended) {
                    return Err(KernelError::Conflict(format!(
                        "merge source {} is {}; only active or suspended units can be merged",
                        s.code, s.status
                    )));
                }
                if target.path.starts_with(&s.path) {
                    return Err(KernelError::Conflict(format!(
                        "merge target {} lies inside source {} (hierarchy cycle)",
                        target.code, s.code
                    )));
                }
                if sources
                    .iter()
                    .any(|o| o.id != s.id && o.path.starts_with(&s.path))
                {
                    return Err(invalid(
                        "mergeSourceIds",
                        "nested",
                        format!("merge sources must not contain each other ({})", s.code),
                    ));
                }
                if positions.get(&s.id).copied().unwrap_or(0) > 0 {
                    return Err(KernelError::Conflict(format!(
                        "merge source {} still has draft/active positions; change or retire them first",
                        s.code
                    )));
                }
                out.baseline.push((s.id, s.version.0));
                let mut children: Vec<&OrgUnit> =
                    units.iter().filter(|c| c.parent_id == Some(s.id)).collect();
                children.sort_by(|a, b| a.code.cmp(&b.code));
                for c in children {
                    if !type_allowed(blueprints, c, target) {
                        return Err(KernelError::Conflict(format!(
                            "child unit {} (`{}`) is not allowed under the target's type `{}`",
                            c.code, c.unit_type, target.unit_type
                        )));
                    }
                    out.moves.push((c.id, Some(s.id), target.id));
                }
                out.decommission.push(s.id);
            }
        }
    }
    Ok(out)
}

const COLUMNS: &str = "id, institution_id, kind, status, reason, effective_from, merge_target_id, \
    process_instance_id, requested_by, decided_by, decided_at, decision_reason, version, created_at, updated_at";

async fn load(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: OrgRestructureId,
    for_update: bool,
) -> Result<OrgRestructure, KernelError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let r = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_restructure WHERE id = $1 AND tenant_id = $2{lock}"
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "org_restructure",
    })?;
    hydrate(conn, &r).await
}

async fn hydrate(
    conn: &mut PgConnection,
    r: &sqlx::postgres::PgRow,
) -> Result<OrgRestructure, KernelError> {
    let id: Uuid = r.try_get("id").map_err(map_db_error)?;
    let moves = sqlx::query(
        "SELECT org_unit_id, role, from_parent_id, to_parent_id, baseline_version \
         FROM institution.org_restructure_move WHERE restructure_id = $1 ORDER BY ordinal",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|m| {
        let from: Option<Uuid> = m.try_get("from_parent_id")?;
        let to: Option<Uuid> = m.try_get("to_parent_id")?;
        Ok(RestructureMove {
            org_unit_id: OrgUnitId(m.try_get("org_unit_id")?),
            role: m.try_get("role")?,
            from_parent_id: from.map(OrgUnitId),
            to_parent_id: to.map(OrgUnitId),
            baseline_version: m.try_get("baseline_version")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    let kind: String = r.try_get("kind").map_err(map_db_error)?;
    let target: Option<Uuid> = r.try_get("merge_target_id").map_err(map_db_error)?;
    Ok(OrgRestructure {
        id: OrgRestructureId(id),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        kind: if kind == "merge" {
            RestructureKind::Merge
        } else {
            RestructureKind::Move
        },
        status: r.try_get("status").map_err(map_db_error)?,
        reason: r.try_get("reason").map_err(map_db_error)?,
        effective_from: r.try_get("effective_from").map_err(map_db_error)?,
        merge_target_id: target.map(OrgUnitId),
        moves,
        process_instance_id: r.try_get("process_instance_id").map_err(map_db_error)?,
        requested_by: r.try_get("requested_by").map_err(map_db_error)?,
        decided_by: r.try_get("decided_by").map_err(map_db_error)?,
        decided_at: r.try_get("decided_at").map_err(map_db_error)?,
        decision_reason: r.try_get("decision_reason").map_err(map_db_error)?,
        version: Version(r.try_get("version").map_err(map_db_error)?),
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

/// Live units, their blueprints and position counts (`lock` takes the tree `FOR UPDATE`).
async fn snapshot(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    institution_id: InstitutionId,
    lock: bool,
) -> Result<
    (
        Vec<OrgUnit>,
        HashMap<OrgTypeBlueprintId, OrgTypeBlueprint>,
        HashMap<OrgUnitId, i64>,
    ),
    KernelError,
> {
    let ids: Vec<Uuid> = sqlx::query_scalar(&format!(
        "SELECT id FROM institution.org_unit WHERE institution_id = $1 AND tenant_id = $2 \
         AND status IN ('draft', 'active', 'suspended') ORDER BY path{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(institution_id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut units = Vec::with_capacity(ids.len());
    let mut blueprints = HashMap::new();
    for id in ids {
        let u = unit::load(conn, tenant, OrgUnitId(id), false).await?;
        if let std::collections::hash_map::Entry::Vacant(e) = blueprints.entry(u.blueprint_id) {
            e.insert(blueprint::load(conn, tenant, u.blueprint_id, false).await?);
        }
        units.push(u);
    }
    let today = business_today(&mut *conn, tenant, institution_id).await?;
    let mut positions = HashMap::new();
    for u in &units {
        let (_, p) = unit::unresolved_dependants(conn, u, today).await?;
        positions.insert(u.id, p);
    }
    Ok((units, blueprints, positions))
}

fn inst_scope(tenant: &ExecutionScope, id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(tenant.tenant_id, id)
}

fn command_of(r: &OrgRestructure) -> RequestRestructure {
    RequestRestructure {
        kind: r.kind,
        moves: r
            .moves
            .iter()
            .filter(|m| m.role == "move")
            .filter_map(|m| {
                m.to_parent_id.map(|to| RequestedMove {
                    org_unit_id: m.org_unit_id,
                    to_parent_id: to,
                })
            })
            .collect(),
        merge_source_ids: r
            .moves
            .iter()
            .filter(|m| m.role == "merge_source")
            .map(|m| m.org_unit_id)
            .collect(),
        merge_target_id: r.merge_target_id,
        reason: r.reason.clone(),
        effective_from: Some(r.effective_from),
    }
}

/// Applies an approved transformation in the caller's transaction (stale baseline → 409).
async fn apply(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    r: &OrgRestructure,
    actor: &Actor,
) -> Result<(), KernelError> {
    let (units, blueprints, positions) = snapshot(conn, tenant, r.institution_id, true).await?;
    let plan = plan(&command_of(r), &units, &blueprints, &positions)?;
    for m in &r.moves {
        let current = units.iter().find(|u| u.id == m.org_unit_id);
        if current.map(|u| u.version.0) != Some(m.baseline_version) {
            return Err(KernelError::Conflict(format!(
                "the structure changed since the transformation was requested (unit {} is no longer at version {}); \
                 request a new transformation",
                m.org_unit_id, m.baseline_version
            )));
        }
    }
    let today = business_today(&mut *conn, tenant, r.institution_id).await?;
    let mut applied = vec![];
    for (uid, from, to) in &plan.moves {
        let u = unit::load(conn, tenant, *uid, false).await?;
        let parent = unit::load(conn, tenant, *to, false).await?;
        unit::move_unit(conn, tenant, &u, &parent, "moved", r.id, today, actor).await?;
        applied.push(OrgUnitMoveV1 {
            org_unit_id: uid.as_uuid(),
            from_parent_id: from.map(|f| f.as_uuid()),
            to_parent_id: Some(to.as_uuid()),
        });
    }
    let target_code = match r.merge_target_id {
        Some(t) => Some(unit::load(conn, tenant, t, false).await?.code),
        None => None,
    };
    for s in &plan.decommission {
        let u = unit::load(conn, tenant, *s, false).await?;
        unit::apply_status(
            conn,
            tenant,
            &u,
            UnitCommand::Decommission,
            Some(format!(
                "merged into {}: {}",
                target_code.as_deref().unwrap_or("target"),
                r.reason
            )),
            Some(r.id),
            today,
            actor,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE institution.org_restructure SET status = 'applied', decided_by = $1, decided_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $2",
    )
    .bind(actor.id.as_deref())
    .bind(r.id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let es = inst_scope(tenant, r.institution_id);
    event(
        conn,
        &es,
        ORG_STRUCTURE_RESTRUCTURED,
        ORG_STRUCTURE_RESTRUCTURED_SCHEMA,
        r.id.to_string(),
        &OrgStructureRestructuredV1 {
            restructure_id: r.id.as_uuid(),
            institution_id: r.institution_id.as_uuid(),
            moves: applied,
            decommissioned_unit_ids: plan.decommission.iter().map(|d| d.as_uuid()).collect(),
            effective_from: r.effective_from,
            process_instance_id: r.process_instance_id.clone(),
        },
        actor,
    )
    .await?;
    audit(
        conn,
        &es,
        "institution.org_restructure.apply",
        "org_restructure",
        r.id.to_string(),
        serde_json::json!({ "kind": r.kind, "moves": plan.moves.len(), "decommissioned": plan.decommission }),
        actor,
    )
    .await
}

/// Requests a transformation: validates it against the current structure, starts the approval
/// step, records the request and — when approved — applies it in the same transaction.
pub async fn request(
    state: &InstitutionState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: &RequestRestructure,
    actor: &Actor,
) -> Result<OrgRestructure, KernelError> {
    cmd.validate()?;
    let today = business_today(&state.pool, scope, institution_id).await?;
    let effective_from = cmd.effective_from.unwrap_or(today);
    if effective_from != today {
        return Err(invalid(
            "effectiveFrom",
            "unsupported-date",
            format!(
                "structure transformations take effect today ({today}, institution timezone); \
                 future-dated application is not available yet"
            ),
        ));
    }
    let plan_now = {
        let mut conn = state.pool.acquire().await.map_err(map_db_error)?;
        crate::shared::guard::institution_status(&mut *conn, scope, institution_id).await?;
        let (units, blueprints, positions) =
            snapshot(&mut conn, scope, institution_id, false).await?;
        plan(cmd, &units, &blueprints, &positions)?
    };
    let id = OrgRestructureId::new();
    let es = inst_scope(scope, institution_id);
    let outcome = approval::request_approval(
        state,
        &es,
        process::ORG_RESTRUCTURE,
        approval::business_key("org_restructure", id, 1),
        BTreeMap::from([
            ("kind".to_owned(), serde_json::json!(cmd.kind)),
            ("moves".to_owned(), serde_json::json!(plan_now.moves.len())),
        ]),
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    lock_institution_for_change(&mut tx, scope, institution_id).await?;
    sqlx::query(
        "INSERT INTO institution.org_restructure (id, tenant_id, institution_id, kind, status, reason, effective_from, \
           merge_target_id, process_instance_id, requested_by) \
         VALUES ($1, $2, $3, $4, 'pending_approval', $5, $6, $7, $8, $9)",
    )
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(match cmd.kind {
        RestructureKind::Move => "move",
        RestructureKind::Merge => "merge",
    })
    .bind(cmd.reason.trim())
    .bind(effective_from)
    .bind(cmd.merge_target_id.map(|t| t.as_uuid()))
    .bind(outcome.process_instance_id())
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    // Requested entries (moves, or merge sources) with their baseline versions.
    let baseline: HashMap<OrgUnitId, i64> = plan_now.baseline.iter().copied().collect();
    let entries: Vec<(OrgUnitId, &str, Option<OrgUnitId>, Option<OrgUnitId>)> = match cmd.kind {
        RestructureKind::Move => plan_now
            .moves
            .iter()
            .map(|(u, from, to)| (*u, "move", *from, Some(*to)))
            .collect(),
        RestructureKind::Merge => cmd
            .merge_source_ids
            .iter()
            .map(|s| (*s, "merge_source", None, cmd.merge_target_id))
            .collect(),
    };
    for (i, (u, role, from, to)) in entries.iter().enumerate() {
        sqlx::query(
            "INSERT INTO institution.org_restructure_move (restructure_id, org_unit_id, role, from_parent_id, \
               to_parent_id, baseline_version, ordinal) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id.as_uuid())
        .bind(u.as_uuid())
        .bind(role)
        .bind(from.map(|f| f.as_uuid()))
        .bind(to.map(|t| t.as_uuid()))
        .bind(baseline.get(u).copied().unwrap_or(0))
        .bind(i32::try_from(i).unwrap_or(i32::MAX))
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    }
    audit(
        &mut tx,
        &es,
        "institution.org_restructure.request",
        "org_restructure",
        id.to_string(),
        serde_json::json!({ "kind": cmd.kind, "processInstanceId": outcome.process_instance_id() }),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let r = load(&mut tx, scope, id, true).await?;
        apply(
            &mut tx,
            scope,
            &r,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let out = load(&mut tx, scope, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Decision on a pending transformation: `approve` applies it (re-validated, stale baseline →
/// 409), `reject` (reason required) closes it.
pub async fn decide(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: OrgRestructureId,
    expected: Version,
    cmd: &Decide,
    actor: &Actor,
) -> Result<OrgRestructure, KernelError> {
    let body = crate::lifecycle::domain::CommandBody {
        reason: cmd.reason.clone(),
    };
    let reason = match cmd.decision {
        ApprovalDecision::Approve => body.optional_reason()?,
        ApprovalDecision::Reject => Some(body.required_reason()?),
    };
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, r.institution_id).await?;
    check_version(expected, r.version.0)?;
    if r.status != "pending_approval" {
        return Err(KernelError::Conflict(format!(
            "transformation is {}; only pending transformations can be decided",
            r.status
        )));
    }
    match cmd.decision {
        ApprovalDecision::Approve => {
            apply(&mut tx, tenant, &r, actor).await?;
            if let Some(reason) = reason {
                sqlx::query(
                    "UPDATE institution.org_restructure SET decision_reason = $1 WHERE id = $2",
                )
                .bind(reason)
                .bind(id.as_uuid())
                .execute(&mut *tx)
                .await
                .map_err(map_db_error)?;
            }
        }
        ApprovalDecision::Reject => {
            sqlx::query(
                "UPDATE institution.org_restructure SET status = 'rejected', decided_by = $1, decided_at = now(), \
                 decision_reason = $2, version = version + 1, updated_at = now() WHERE id = $3",
            )
            .bind(actor.id.as_deref())
            .bind(&reason)
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            audit(
                &mut tx,
                &inst_scope(tenant, r.institution_id),
                "institution.org_restructure.reject",
                "org_restructure",
                id.to_string(),
                serde_json::json!({ "reason": reason }),
                actor,
            )
            .await?;
        }
    }
    let out = load(&mut tx, tenant, id, false).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Transformations of an institution, newest first.
pub async fn list(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Vec<OrgRestructure>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    crate::shared::guard::institution_status(&mut *conn, scope, institution_id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.org_restructure WHERE institution_id = $1 AND tenant_id = $2 \
         ORDER BY created_at DESC, id DESC"
    ))
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = vec![];
    for r in &rows {
        out.push(hydrate(&mut conn, r).await?);
    }
    Ok(out)
}

// ------------------------------------------------------------------------------------ HTTP

/// Transformation collection.
#[derive(Debug, serde::Serialize, utoipa::ToSchema)]
pub struct OrgRestructureList {
    pub items: Vec<OrgRestructure>,
}

fn respond(status: StatusCode, r: OrgRestructure) -> Response {
    with_etag(status, &r, r.version)
}

/// Request an organisation structure transformation (`organisation.manage`).
///
/// Validated against the current structure (409 on a hierarchy cycle or a blueprint parent
/// rule); approved through the WorkflowPort — in the Batch 2 auto-approval mode the response is
/// already `applied`.
#[utoipa::path(
    post,
    path = "/api/v1/institution/institutions/{id}/org-restructures",
    operation_id = "org_restructure_request",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key.")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = RequestRestructure,
    responses((status = 201, body = OrgRestructure, headers(("ETag" = String))), CreateErrors)
)]
pub async fn http_request(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<RequestRestructure>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    require_idempotency_key(&headers)?;
    let scope = institution_scope(ctx, id);
    Ok(respond(
        StatusCode::CREATED,
        request(&st, &scope, id, &cmd, &actor(ctx)).await?,
    ))
}

/// Structure transformations of an institution, newest first (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/org-restructures",
    operation_id = "org_restructure_list",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OrgRestructureList), ReadErrors)
)]
pub async fn http_list(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<OrgRestructureList>> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(OrgRestructureList {
        items: list(&st.pool, &scope, id).await?,
    }))
}

/// One structure transformation (`institution.read`).
#[utoipa::path(
    get,
    path = "/api/v1/institution/org-restructures/{id}",
    operation_id = "org_restructure_get",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses((status = 200, body = OrgRestructure, headers(("ETag" = String))), ReadErrors)
)]
pub async fn http_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgRestructureId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::InstitutionRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    Ok(respond(
        StatusCode::OK,
        load(&mut conn, &ctx.scope, id, false).await?,
    ))
}

/// Decide a pending transformation (`organisation.manage`): `approve` applies it if the
/// structure baseline is unchanged (409 otherwise); `reject` needs a reason.
#[utoipa::path(
    post,
    path = "/api/v1/institution/org-restructures/{id}/decision",
    operation_id = "org_restructure_decide",
    tag = "organisation",
    params(("id" = String, Path, format = Uuid), ("If-Match" = String, Header, description = "ETag of the version being changed")),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = Decide,
    responses((status = 200, body = OrgRestructure, headers(("ETag" = String))), CommandErrors)
)]
pub async fn http_decide(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<OrgRestructureId>,
    headers: HeaderMap,
    Json(cmd): Json<Decide>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OrganisationManage)?;
    let expected = expect_version!(headers);
    Ok(respond(
        StatusCode::OK,
        decide(&st.pool, &ctx.scope, id, expected, &cmd, &actor(ctx)).await?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::org::domain::UnitType;
    use chrono::NaiveDate;
    use sutra_contracts::institution::status::PublicationStatus;

    fn bp() -> OrgTypeBlueprint {
        let t = |k: &str, root: bool, p: &[&str]| UnitType {
            key: k.into(),
            label: k.into(),
            allow_root: root,
            allowed_parent_types: p.iter().map(|s| (*s).into()).collect(),
        };
        OrgTypeBlueprint {
            id: OrgTypeBlueprintId::new(),
            institution_id: InstitutionId::new(),
            version_no: 1,
            status: PublicationStatus::Published,
            supersedes_id: None,
            effective_from: NaiveDate::default(),
            unit_types: vec![
                t("university", true, &[]),
                t("faculty", false, &["university"]),
                t("department", false, &["faculty"]),
            ],
            process_instance_id: None,
            published_at: None,
            published_by: None,
            decision_reason: None,
            version: Version(1),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn unit(code: &str, ty: &str, parent: Option<&OrgUnit>, bp: &OrgTypeBlueprint) -> OrgUnit {
        let id = OrgUnitId::new();
        OrgUnit {
            id,
            institution_id: bp.institution_id,
            parent_id: parent.map(|p| p.id),
            path: format!("{}{id}/", parent.map_or("/".to_owned(), |p| p.path.clone())),
            depth: parent.map_or(0, |p| p.depth + 1),
            unit_type: ty.into(),
            blueprint_id: bp.id,
            code: code.into(),
            name: code.into(),
            description: None,
            status: OrgUnitStatus::Active,
            status_reason: None,
            version: Version(2),
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn req(kind: RestructureKind) -> RequestRestructure {
        RequestRestructure {
            kind,
            moves: vec![],
            merge_source_ids: vec![],
            merge_target_id: None,
            reason: "reorganisation".into(),
            effective_from: None,
        }
    }

    #[test]
    fn plans_moves_and_merges() {
        let b = bp();
        let root = unit("U", "university", None, &b);
        let f1 = unit("F1", "faculty", Some(&root), &b);
        let f2 = unit("F2", "faculty", Some(&root), &b);
        let d1 = unit("D1", "department", Some(&f1), &b);
        let d2 = unit("D2", "department", Some(&f1), &b);
        let units = vec![root.clone(), f1.clone(), f2.clone(), d1.clone(), d2.clone()];
        let bps = HashMap::from([(b.id, b.clone())]);
        let none = HashMap::new();

        let mut mv = req(RestructureKind::Move);
        mv.moves = vec![RequestedMove {
            org_unit_id: d1.id,
            to_parent_id: f2.id,
        }];
        let p = plan(&mv, &units, &bps, &none).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.moves, vec![(d1.id, Some(f1.id), f2.id)]);
        assert_eq!(p.baseline, vec![(d1.id, 2)]);

        // Blueprint rule: a department cannot sit under the university.
        mv.moves = vec![RequestedMove {
            org_unit_id: d1.id,
            to_parent_id: root.id,
        }];
        assert!(matches!(
            plan(&mv, &units, &bps, &none),
            Err(KernelError::Conflict(_))
        ));
        // The root cannot move; no-op moves are refused.
        mv.moves = vec![RequestedMove {
            org_unit_id: root.id,
            to_parent_id: f1.id,
        }];
        assert!(matches!(
            plan(&mv, &units, &bps, &none),
            Err(KernelError::Validation(_))
        ));

        let mut merge = req(RestructureKind::Merge);
        merge.merge_source_ids = vec![f1.id];
        merge.merge_target_id = Some(f2.id);
        let p = plan(&merge, &units, &bps, &none).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(p.decommission, vec![f1.id]);
        assert_eq!(p.moves.len(), 2);
        // A source with positions cannot be merged.
        let pos = HashMap::from([(f1.id, 1)]);
        assert!(plan(&merge, &units, &bps, &pos).is_err());
        // Target inside a source = cycle.
        merge.merge_source_ids = vec![f1.id];
        merge.merge_target_id = Some(d1.id);
        assert!(matches!(
            plan(&merge, &units, &bps, &none),
            Err(KernelError::Conflict(_))
        ));
    }

    #[test]
    fn moves_that_loop_are_cycles() {
        let mut b = bp();
        b.unit_types[2].allowed_parent_types = vec!["faculty".into(), "department".into()];
        b.unit_types[1].allowed_parent_types = vec!["university".into(), "department".into()];
        let root = unit("U", "university", None, &b);
        let f1 = unit("F1", "faculty", Some(&root), &b);
        let d1 = unit("D1", "department", Some(&f1), &b);
        let units = vec![root, f1.clone(), d1.clone()];
        let bps = HashMap::from([(b.id, b)]);
        let mut mv = req(RestructureKind::Move);
        mv.moves = vec![RequestedMove {
            org_unit_id: f1.id,
            to_parent_id: d1.id,
        }];
        let Err(KernelError::Conflict(msg)) = plan(&mv, &units, &bps, &HashMap::new()) else {
            panic!("cycle expected")
        };
        assert!(msg.contains("cycle"), "{msg}");
    }
}
