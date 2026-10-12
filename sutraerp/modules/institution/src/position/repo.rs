//! Position definition persistence (owner-local transactions with outbox + audit).

use super::domain::{
    can_transition, check_change_date, in_force, ChangePosition, EstablishPosition, Position,
    PositionRevision, RetirePosition, UpdateDraftPosition,
};
use crate::domain::illegal_transition;
use crate::lifecycle::common::{business_today, check_version, invalid, norm, status_changed};
use crate::org::domain::accepts_new_children;
use crate::org::unit;
use crate::shared::guard::lock_institution_for_change;
use crate::shared::publish::{audit, event};
use crate::shared::Actor;
use chrono::{DateTime, Days, NaiveDate, Utc};
use sqlx::{PgConnection, PgPool, Row};
use sutra_contracts::institution::events::org::*;
use sutra_contracts::institution::ids::{OrgUnitId, PositionId};
use sutra_contracts::institution::status::{OrgUnitStatus, PositionStatus};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

struct Row0 {
    id: PositionId,
    institution_id: InstitutionId,
    code: String,
    status: PositionStatus,
    status_reason: Option<String>,
    latest_revision: i32,
    retired_on: Option<NaiveDate>,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

const COLUMNS: &str =
    "id, institution_id, code, status, status_reason, latest_revision, retired_on, \
    version, created_at, updated_at";
const REV_COLUMNS: &str =
    "revision, org_unit_id, title, grade, description, headcount, effective_from, \
    effective_to, reason, recorded_by, created_at";

async fn load_row(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: PositionId,
    for_update: bool,
) -> Result<Row0, KernelError> {
    let lock = if for_update { " FOR UPDATE" } else { "" };
    let r = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.position_definition WHERE id = $1 AND tenant_id = $2{lock}"
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "position" })?;
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(Row0 {
        id: PositionId(r.try_get("id").map_err(map_db_error)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(map_db_error)?),
        code: r.try_get("code").map_err(map_db_error)?,
        status: PositionStatus::parse(&status).ok_or_else(|| {
            KernelError::internal_msg(format!("unknown position status `{status}`"))
        })?,
        status_reason: r.try_get("status_reason").map_err(map_db_error)?,
        latest_revision: r.try_get("latest_revision").map_err(map_db_error)?,
        retired_on: r.try_get("retired_on").map_err(map_db_error)?,
        version: r.try_get("version").map_err(map_db_error)?,
        created_at: r.try_get("created_at").map_err(map_db_error)?,
        updated_at: r.try_get("updated_at").map_err(map_db_error)?,
    })
}

async fn revisions_of(
    conn: &mut PgConnection,
    id: PositionId,
) -> Result<Vec<PositionRevision>, KernelError> {
    sqlx::query(&format!(
        "SELECT {REV_COLUMNS} FROM institution.position_definition_revision WHERE position_id = $1 ORDER BY revision"
    ))
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|r| {
        Ok(PositionRevision {
            revision: r.try_get("revision")?,
            org_unit_id: OrgUnitId(r.try_get("org_unit_id")?),
            title: r.try_get("title")?,
            grade: r.try_get("grade")?,
            description: r.try_get("description")?,
            headcount: r.try_get("headcount")?,
            effective_from: r.try_get("effective_from")?,
            effective_to: r.try_get("effective_to")?,
            reason: r.try_get("reason")?,
            recorded_by: r.try_get("recorded_by")?,
            created_at: r.try_get("created_at")?,
        })
    })
    .collect::<Result<_, sqlx::Error>>()
    .map_err(map_db_error)
}

async fn assemble(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    p: Row0,
    as_of: Option<NaiveDate>,
) -> Result<Position, KernelError> {
    let as_of = match as_of {
        Some(d) => d,
        None => business_today(&mut *conn, tenant, p.institution_id).await?,
    };
    let revs = revisions_of(conn, p.id).await?;
    let cur = in_force(&revs, as_of)
        .cloned()
        .ok_or_else(|| KernelError::internal_msg("position without revisions"))?;
    let scheduled = revs
        .iter()
        .rev()
        .find(|r| r.effective_from > as_of && r.revision > cur.revision)
        .cloned();
    Ok(Position {
        id: p.id,
        institution_id: p.institution_id,
        code: p.code,
        status: p.status,
        status_reason: p.status_reason,
        org_unit_id: cur.org_unit_id,
        title: cur.title,
        grade: cur.grade,
        description: cur.description,
        headcount: cur.headcount,
        revision: cur.revision,
        effective_from: cur.effective_from,
        effective_to: cur.effective_to,
        latest_revision: p.latest_revision,
        scheduled_revision: scheduled,
        retired_on: p.retired_on,
        as_of,
        version: Version(p.version),
        created_at: p.created_at,
        updated_at: p.updated_at,
    })
}

fn inst_scope(tenant: &ExecutionScope, id: InstitutionId) -> ExecutionScope {
    ExecutionScope::institution(tenant.tenant_id, id)
}

/// Loads an organisation unit of `institution_id` that may hold positions (`statuses`).
async fn placement(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    institution_id: InstitutionId,
    unit_id: OrgUnitId,
    require_active: bool,
) -> Result<(), KernelError> {
    let u = unit::load(conn, tenant, unit_id, false)
        .await
        .map_err(|e| match e {
            KernelError::NotFound { .. } => invalid(
                "orgUnitId",
                "unknown",
                "orgUnitId is not a unit of this institution",
            ),
            e => e,
        })?;
    if u.institution_id != institution_id {
        return Err(invalid(
            "orgUnitId",
            "unknown",
            "orgUnitId is not a unit of this institution",
        ));
    }
    let ok = if require_active {
        u.status == OrgUnitStatus::Active
    } else {
        accepts_new_children(u.status)
    };
    if !ok {
        return Err(KernelError::Conflict(format!(
            "organisation unit {} is {}; it cannot hold {}positions",
            u.code,
            u.status,
            if require_active { "active " } else { "new " }
        )));
    }
    Ok(())
}

fn payload(p: &Position) -> PositionDefinitionV1 {
    PositionDefinitionV1 {
        position_id: p.id.as_uuid(),
        institution_id: p.institution_id.as_uuid(),
        org_unit_id: p.org_unit_id.as_uuid(),
        code: p.code.clone(),
        title: p.title.clone(),
        revision: p.revision,
        effective_from: p.effective_from,
        effective_to: p.effective_to,
    }
}

/// Establishes a definition in `draft` with revision 1 (no occupant, no assignment).
pub async fn establish(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: &EstablishPosition,
    actor: &Actor,
) -> Result<Position, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    lock_institution_for_change(&mut tx, scope, institution_id).await?;
    placement(&mut tx, scope, institution_id, cmd.org_unit_id, false).await?;
    let id = PositionId::new();
    sqlx::query(
        "INSERT INTO institution.position_definition (id, tenant_id, institution_id, code) VALUES ($1, $2, $3, $4)",
    )
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(cmd.code.trim())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO institution.position_definition_revision (tenant_id, institution_id, position_id, revision, \
           org_unit_id, title, grade, description, headcount, effective_from, recorded_by) \
         VALUES ($1, $2, $3, 1, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(id.as_uuid())
    .bind(cmd.org_unit_id.as_uuid())
    .bind(cmd.title.trim())
    .bind(norm(cmd.grade.as_deref()))
    .bind(norm(cmd.description.as_deref()))
    .bind(cmd.headcount.unwrap_or(1))
    .bind(cmd.effective_from)
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let row = load_row(&mut tx, scope, id, false).await?;
    let p = assemble(&mut tx, scope, row, Some(cmd.effective_from)).await?;
    let es = inst_scope(scope, institution_id);
    event(
        &mut tx,
        &es,
        POSITION_ESTABLISHED,
        POSITION_ESTABLISHED_SCHEMA,
        id.to_string(),
        &payload(&p),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.position.establish",
        "position",
        id.to_string(),
        serde_json::json!({ "code": p.code, "orgUnitId": p.org_unit_id, "effectiveFrom": p.effective_from }),
        actor,
    )
    .await?;
    let row = load_row(&mut tx, scope, id, false).await?;
    let out = assemble(&mut tx, scope, row, None).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Definitions of an institution by code (`org_unit_id` filters on the revision in force).
pub async fn list(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<PositionStatus>,
    org_unit_id: Option<OrgUnitId>,
    as_of: Option<NaiveDate>,
) -> Result<Vec<Position>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    crate::shared::guard::institution_status(&mut *conn, scope, institution_id).await?;
    let ids: Vec<uuid::Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.position_definition WHERE institution_id = $1 AND tenant_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY code",
    )
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let mut out = vec![];
    for id in ids {
        let row = load_row(&mut conn, scope, PositionId(id), false).await?;
        let p = assemble(&mut conn, scope, row, as_of).await?;
        if org_unit_id.is_none_or(|u| u == p.org_unit_id) {
            out.push(p);
        }
    }
    Ok(out)
}

/// One definition as of `as_of` (default today, institution timezone).
pub async fn get(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: PositionId,
    as_of: Option<NaiveDate>,
) -> Result<Position, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    let row = load_row(&mut conn, tenant, id, false).await?;
    assemble(&mut conn, tenant, row, as_of).await
}

/// Every revision, oldest first.
pub async fn revisions(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: PositionId,
) -> Result<Vec<PositionRevision>, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    load_row(&mut conn, tenant, id, false).await?;
    revisions_of(&mut conn, id).await
}

/// Edits the draft (revision 1) in place; a draft is not authoritative yet.
pub async fn update_draft(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: PositionId,
    expected: Version,
    cmd: &UpdateDraftPosition,
    actor: &Actor,
) -> Result<Position, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let p = load_row(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, p.institution_id).await?;
    check_version(expected, p.version)?;
    if p.status != PositionStatus::Draft {
        return Err(KernelError::Conflict(format!(
            "position is {}; record an effective-dated change instead",
            p.status
        )));
    }
    if let Some(u) = cmd.org_unit_id {
        placement(&mut tx, tenant, p.institution_id, u, false).await?;
    }
    let revs = revisions_of(&mut tx, id).await?;
    let r = revs
        .first()
        .ok_or_else(|| KernelError::internal_msg("position without revisions"))?;
    let keep = |o: &Option<String>, cur: &Option<String>| match o {
        Some(s) => norm(Some(s)),
        None => cur.clone(),
    };
    sqlx::query(
        "UPDATE institution.position_definition_revision SET org_unit_id = $1, title = $2, grade = $3, \
         description = $4, headcount = $5, effective_from = $6 WHERE position_id = $7 AND revision = 1",
    )
    .bind(cmd.org_unit_id.unwrap_or(r.org_unit_id).as_uuid())
    .bind(cmd.title.as_deref().map(str::trim).unwrap_or(&r.title))
    .bind(keep(&cmd.grade, &r.grade))
    .bind(keep(&cmd.description, &r.description))
    .bind(cmd.headcount.unwrap_or(r.headcount))
    .bind(cmd.effective_from.unwrap_or(r.effective_from))
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query("UPDATE institution.position_definition SET version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    audit(
        &mut tx,
        &inst_scope(tenant, p.institution_id),
        "institution.position.update_draft",
        "position",
        id.to_string(),
        serde_json::json!({ "fromVersion": expected.0 }),
        actor,
    )
    .await?;
    let row = load_row(&mut tx, tenant, id, false).await?;
    let out = assemble(&mut tx, tenant, row, None).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    p: &Row0,
    to: PositionStatus,
    reason: Option<String>,
    retired_on: Option<NaiveDate>,
    event_type: (&'static str, &'static str),
    command: &str,
    actor: &Actor,
) -> Result<Position, KernelError> {
    let version: i64 = sqlx::query_scalar(
        "UPDATE institution.position_definition SET status = $1, status_reason = $2, \
         retired_on = COALESCE($3, retired_on), version = version + 1, updated_at = now() WHERE id = $4 RETURNING version",
    )
    .bind(to.as_str())
    .bind(&reason)
    .bind(retired_on)
    .bind(p.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let es = inst_scope(tenant, p.institution_id);
    event(
        conn,
        &es,
        event_type.0,
        event_type.1,
        p.id.to_string(),
        &status_changed(
            p.id.as_uuid(),
            p.institution_id,
            p.status.as_str(),
            to.as_str(),
            version,
            reason.clone(),
            None,
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        &es,
        &format!("institution.position.{command}"),
        "position",
        p.id.to_string(),
        serde_json::json!({ "fromStatus": p.status, "toStatus": to, "reason": reason, "retiredOn": retired_on }),
        actor,
    )
    .await?;
    let row = load_row(conn, tenant, p.id, false).await?;
    assemble(conn, tenant, row, None).await
}

/// `draft → active`; the definition's organisation unit must be active (L4-B-013-BH-003).
pub async fn activate(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: PositionId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Position, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let p = load_row(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, p.institution_id).await?;
    check_version(expected, p.version)?;
    if !can_transition(p.status, PositionStatus::Active) {
        return Err(illegal_transition(
            "position",
            p.status,
            PositionStatus::Active,
        ));
    }
    let revs = revisions_of(&mut tx, id).await?;
    for r in &revs {
        placement(&mut tx, tenant, p.institution_id, r.org_unit_id, true).await?;
    }
    let out = set_status(
        &mut tx,
        tenant,
        &p,
        PositionStatus::Active,
        reason,
        None,
        (POSITION_ACTIVATED, POSITION_ACTIVATED_SCHEMA),
        "activate",
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Records an effective-dated successor revision of an active definition (L3-B-014): the
/// predecessor stays authoritative until the day before `effectiveFrom` and is never edited
/// beyond closing its `effectiveTo`.
pub async fn change(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: PositionId,
    expected: Version,
    cmd: &ChangePosition,
    actor: &Actor,
) -> Result<Position, KernelError> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let p = load_row(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, p.institution_id).await?;
    check_version(expected, p.version)?;
    if p.status != PositionStatus::Active {
        return Err(KernelError::Conflict(format!(
            "position is {}; only active definitions take effective-dated changes",
            p.status
        )));
    }
    let revs = revisions_of(&mut tx, id).await?;
    let latest = revs
        .last()
        .ok_or_else(|| KernelError::internal_msg("position without revisions"))?;
    let today = business_today(&mut *tx, tenant, p.institution_id).await?;
    check_change_date(cmd.effective_from, today, latest.effective_from)?;
    let next = cmd.successor(latest).ok_or_else(|| {
        invalid(
            "effectiveFrom",
            "no-change",
            "the change does not alter the definition",
        )
    })?;
    if next.org_unit_id != latest.org_unit_id {
        placement(&mut tx, tenant, p.institution_id, next.org_unit_id, true).await?;
    }
    let close = cmd
        .effective_from
        .checked_sub_days(Days::new(1))
        .unwrap_or(cmd.effective_from);
    sqlx::query(
        "UPDATE institution.position_definition_revision SET effective_to = $1 WHERE position_id = $2 AND revision = $3",
    )
    .bind(close)
    .bind(id.as_uuid())
    .bind(latest.revision)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO institution.position_definition_revision (tenant_id, institution_id, position_id, revision, \
           org_unit_id, title, grade, description, headcount, effective_from, reason, recorded_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(tenant.tenant_id.as_uuid())
    .bind(p.institution_id.as_uuid())
    .bind(id.as_uuid())
    .bind(next.revision)
    .bind(next.org_unit_id.as_uuid())
    .bind(&next.title)
    .bind(&next.grade)
    .bind(&next.description)
    .bind(next.headcount)
    .bind(next.effective_from)
    .bind(&next.reason)
    .bind(actor.id.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    sqlx::query(
        "UPDATE institution.position_definition SET latest_revision = $1, version = version + 1, updated_at = now() WHERE id = $2",
    )
    .bind(next.revision)
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let row = load_row(&mut tx, tenant, id, false).await?;
    let at_change = assemble(&mut tx, tenant, row, Some(next.effective_from)).await?;
    let es = inst_scope(tenant, p.institution_id);
    event(
        &mut tx,
        &es,
        POSITION_CHANGED,
        POSITION_CHANGED_SCHEMA,
        id.to_string(),
        &payload(&at_change),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &es,
        "institution.position.change",
        "position",
        id.to_string(),
        serde_json::json!({ "revision": next.revision, "effectiveFrom": next.effective_from, "reason": next.reason }),
        actor,
    )
    .await?;
    let row = load_row(&mut tx, tenant, id, false).await?;
    let out = assemble(&mut tx, tenant, row, None).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// `active → retired` with a reason; the definition applies until `effectiveTo` (default today)
/// and takes no new assignments. A change scheduled after that date blocks retirement.
pub async fn retire(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: PositionId,
    expected: Version,
    cmd: &RetirePosition,
    actor: &Actor,
) -> Result<Position, KernelError> {
    let reason = crate::lifecycle::domain::CommandBody {
        reason: cmd.reason.clone(),
    }
    .required_reason()?;
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let p = load_row(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, p.institution_id).await?;
    check_version(expected, p.version)?;
    if !can_transition(p.status, PositionStatus::Retired) {
        return Err(illegal_transition(
            "position",
            p.status,
            PositionStatus::Retired,
        ));
    }
    let today = business_today(&mut *tx, tenant, p.institution_id).await?;
    let to = cmd.effective_to.unwrap_or(today);
    if to < today {
        return Err(invalid(
            "effectiveTo",
            "retroactive",
            format!("effectiveTo must not be before today ({today}) in the institution timezone"),
        ));
    }
    let revs = revisions_of(&mut tx, id).await?;
    let latest = revs
        .last()
        .ok_or_else(|| KernelError::internal_msg("position without revisions"))?;
    if latest.effective_from > to {
        return Err(KernelError::Conflict(format!(
            "revision {} is scheduled from {}; retirement must not end the definition before it",
            latest.revision, latest.effective_from
        )));
    }
    sqlx::query(
        "UPDATE institution.position_definition_revision SET effective_to = $1 \
         WHERE position_id = $2 AND revision = $3 AND (effective_to IS NULL OR effective_to > $1)",
    )
    .bind(to)
    .bind(id.as_uuid())
    .bind(latest.revision)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = set_status(
        &mut tx,
        tenant,
        &p,
        PositionStatus::Retired,
        Some(reason),
        Some(to),
        (POSITION_RETIRED, POSITION_RETIRED_SCHEMA),
        "retire",
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// `retired → archived` (terminal; the code is never reused).
pub async fn archive(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: PositionId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Position, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let p = load_row(&mut tx, tenant, id, true).await?;
    lock_institution_for_change(&mut tx, tenant, p.institution_id).await?;
    check_version(expected, p.version)?;
    if !can_transition(p.status, PositionStatus::Archived) {
        return Err(illegal_transition(
            "position",
            p.status,
            PositionStatus::Archived,
        ));
    }
    let out = set_status(
        &mut tx,
        tenant,
        &p,
        PositionStatus::Archived,
        reason,
        None,
        (POSITION_ARCHIVED, POSITION_ARCHIVED_SCHEMA),
        "archive",
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}
