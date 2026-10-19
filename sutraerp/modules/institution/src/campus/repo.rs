//! PostgreSQL persistence + commands of the institutional campus. Every mutation runs in one
//! owner-local transaction: institution guard → row lock → version check → state change →
//! revision snapshot → outbox event → audit (ADR-L5-0006/0007).

use super::domain::{
    ensure_transition, modifiable, Campus, CampusAddress, CampusRevision, CampusType,
    DecommissionCampus, GeoPoint, ModifyCampus, RegisterCampus,
};
use crate::curriculum::support::{
    audit_transition, begin, expect_version, institution_of, Decision, Transition,
};
use crate::shared::approval::{self, process, ApprovalOutcome};
use crate::shared::guard::lock_institution_for_change;
use crate::shared::{publish, Actor, InstitutionState};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::BTreeMap;
use sutra_contracts::institution::events::campus::{
    CampusRegisteredV1, CAMPUS_ACTIVATED, CAMPUS_ACTIVATED_SCHEMA, CAMPUS_ARCHIVED,
    CAMPUS_ARCHIVED_SCHEMA, CAMPUS_DECOMMISSIONED, CAMPUS_DECOMMISSIONED_SCHEMA, CAMPUS_MODIFIED,
    CAMPUS_MODIFIED_SCHEMA, CAMPUS_REGISTERED, CAMPUS_REGISTERED_SCHEMA, CAMPUS_RESTORED,
    CAMPUS_RESTORED_SCHEMA, CAMPUS_SUSPENDED, CAMPUS_SUSPENDED_SCHEMA,
};
use sutra_contracts::institution::events::ModifiedV1;
use sutra_contracts::institution::ids::CampusId;
use sutra_contracts::institution::status::CampusStatus;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const ENTITY: &str = "campus";
const COLUMNS: &str = "id, institution_id, code, name, campus_type, is_main, address_line1, \
    address_line2, city, state, postal_code, country, latitude, longitude, timezone, \
    established_on, declared_capacity, contact_email, contact_phone, description, working_hours, \
    status, pending_action, pending_reason, pending_effective_on, process_instance_id, \
    status_reason, decommission_effective_on, status_changed_at, version, created_at, updated_at";

/// A campus row plus the pending-decision context that is not part of the public resource.
struct Loaded {
    campus: Campus,
    pending_reason: Option<String>,
    pending_effective_on: Option<chrono::NaiveDate>,
}

fn status_of(raw: &str) -> Result<CampusStatus, sqlx::Error> {
    CampusStatus::parse(raw)
        .ok_or_else(|| sqlx::Error::Decode(format!("campus status `{raw}`").into()))
}

fn from_row(r: &PgRow) -> Result<Loaded, sqlx::Error> {
    let campus_type: String = r.try_get("campus_type")?;
    let lat: Option<f64> = r.try_get("latitude")?;
    let lng: Option<f64> = r.try_get("longitude")?;
    let status: String = r.try_get("status")?;
    Ok(Loaded {
        campus: Campus {
            id: CampusId(r.try_get("id")?),
            institution_id: InstitutionId(r.try_get("institution_id")?),
            code: r.try_get("code")?,
            name: r.try_get("name")?,
            campus_type: CampusType::parse(&campus_type).ok_or_else(|| {
                sqlx::Error::Decode(format!("campus type `{campus_type}`").into())
            })?,
            is_main: r.try_get("is_main")?,
            address: CampusAddress {
                line1: r.try_get("address_line1")?,
                line2: r.try_get("address_line2")?,
                city: r.try_get("city")?,
                state: r.try_get("state")?,
                postal_code: r.try_get("postal_code")?,
                country: Some(r.try_get("country")?),
            },
            geo: lat.zip(lng).map(|(latitude, longitude)| GeoPoint {
                latitude,
                longitude,
            }),
            timezone: r.try_get("timezone")?,
            established_on: r.try_get("established_on")?,
            declared_capacity: r.try_get("declared_capacity")?,
            contact_email: r.try_get("contact_email")?,
            contact_phone: r.try_get("contact_phone")?,
            description: r.try_get("description")?,
            working_hours: r.try_get("working_hours")?,
            status: status_of(&status)?,
            pending_action: r.try_get("pending_action")?,
            process_instance_id: r.try_get("process_instance_id")?,
            status_reason: r.try_get("status_reason")?,
            decommission_effective_on: r.try_get("decommission_effective_on")?,
            status_changed_at: r.try_get("status_changed_at")?,
            version: Version(r.try_get("version")?),
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
        },
        pending_reason: r.try_get("pending_reason")?,
        pending_effective_on: r.try_get("pending_effective_on")?,
    })
}

fn trimmed(s: Option<&String>) -> Option<String> {
    s.map(|x| x.trim().to_owned()).filter(|x| !x.is_empty())
}

async fn load(
    conn: &mut PgConnection,
    tenant: &ExecutionScope,
    id: CampusId,
    lock: bool,
) -> Result<Loaded, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.campus WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: ENTITY })?;
    from_row(&row).map_err(map_db_error)
}

async fn scope_of(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: CampusId,
) -> Result<ExecutionScope, KernelError> {
    let inst = institution_of(pool, tenant, "campus", ENTITY, id.as_uuid()).await?;
    Ok(ExecutionScope::institution(tenant.tenant_id, inst))
}

#[allow(clippy::too_many_arguments)]
async fn record_revision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    c: &Campus,
    kind: &str,
    changed: &[&str],
    from: Option<CampusStatus>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let snapshot = serde_json::to_value(c).map_err(KernelError::internal)?;
    sqlx::query(
        "INSERT INTO institution.campus_revision (tenant_id, institution_id, campus_id, revision, \
         kind, changed_fields, from_status, to_status, reason, actor, snapshot) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(c.institution_id.as_uuid())
    .bind(c.id.as_uuid())
    .bind(c.version.0)
    .bind(kind)
    .bind(changed.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>())
    .bind(from.map(|s| s.as_str()))
    .bind(from.map(|_| c.status.as_str()))
    .bind(reason)
    .bind(actor.id.as_deref())
    .bind(snapshot)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Moves the locked campus to `to` (version + 1) and records the revision. Optional columns:
/// pending context, process instance, decommission date.
#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    current: &Campus,
    to: CampusStatus,
    pending: Option<(&str, Option<&str>, Option<chrono::NaiveDate>)>,
    process_instance_id: Option<&str>,
    reason: Option<&str>,
    decommission_on: Option<chrono::NaiveDate>,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE institution.campus SET status = $1, pending_action = $2, pending_reason = $3, \
         pending_effective_on = $4, process_instance_id = COALESCE($5, process_instance_id), \
         status_reason = $6, decommission_effective_on = COALESCE($7, decommission_effective_on), \
         status_changed_at = CASE WHEN status = $1 THEN status_changed_at ELSE now() END, \
         version = version + 1, updated_at = now() \
         WHERE id = $8 AND tenant_id = $9 RETURNING {COLUMNS}"
    ))
    .bind(to.as_str())
    .bind(pending.map(|p| p.0))
    .bind(pending.and_then(|p| p.1))
    .bind(pending.and_then(|p| p.2))
    .bind(process_instance_id)
    .bind(reason)
    .bind(decommission_on)
    .bind(current.id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row).map_err(map_db_error)?.campus;
    record_revision(
        conn,
        scope,
        &next,
        "status",
        &[],
        Some(current.status),
        reason,
        actor,
    )
    .await?;
    Ok(next)
}

// ------------------------------------------------------------------------------------------
// Queries
// ------------------------------------------------------------------------------------------

pub async fn get(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: CampusId,
) -> Result<Campus, KernelError> {
    let mut conn = pool.acquire().await.map_err(map_db_error)?;
    Ok(load(&mut conn, tenant, id, false).await?.campus)
}

pub async fn list(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<CampusStatus>,
) -> Result<Vec<Campus>, KernelError> {
    crate::shared::guard::institution_status(pool, scope, institution_id).await?;
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM institution.campus WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY is_main DESC, code"
    ))
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| from_row(r).map(|l| l.campus))
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

pub async fn revisions(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: CampusId,
) -> Result<Vec<CampusRevision>, KernelError> {
    get(pool, tenant, id).await?;
    let rows = sqlx::query(
        "SELECT revision, kind, changed_fields, from_status, to_status, reason, actor, snapshot, \
         created_at FROM institution.campus_revision WHERE campus_id = $1 AND tenant_id = $2 \
         ORDER BY revision",
    )
    .bind(id.as_uuid())
    .bind(tenant.tenant_id.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    rows.iter()
        .map(|r| {
            Ok(CampusRevision {
                revision: r.try_get("revision")?,
                kind: r.try_get("kind")?,
                changed_fields: r.try_get("changed_fields")?,
                from_status: r.try_get("from_status")?,
                to_status: r.try_get("to_status")?,
                reason: r.try_get("reason")?,
                actor: r.try_get("actor")?,
                snapshot: r.try_get("snapshot")?,
                created_at: r.try_get("created_at")?,
            })
        })
        .collect::<Result<_, sqlx::Error>>()
        .map_err(map_db_error)
}

/// Rejects a second live main campus with a readable 409 (the partial unique index is the
/// backstop).
async fn ensure_single_main(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    except: Option<CampusId>,
) -> Result<(), KernelError> {
    let other: Option<String> = sqlx::query_scalar(
        "SELECT code FROM institution.campus WHERE tenant_id = $1 AND institution_id = $2 \
         AND is_main AND status NOT IN ('decommissioned', 'archived') \
         AND ($3::uuid IS NULL OR id <> $3) LIMIT 1",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(except.map(|c| c.as_uuid()))
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    match other {
        Some(code) => Err(KernelError::Conflict(format!(
            "campus {code} is already the main campus of this institution"
        ))),
        None => Ok(()),
    }
}

// ------------------------------------------------------------------------------------------
// Commands
// ------------------------------------------------------------------------------------------

/// L3-B-017: registers a campus in `registered` (Proposed).
pub async fn register(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: RegisterCampus,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    let mut tx = pool.begin().await.map_err(map_db_error)?;
    let c = register_in(&mut tx, scope, institution_id, cmd, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c)
}

/// [`register`] inside the caller's transaction (Part 2 · S4-import runs it per row).
pub async fn register_in(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: RegisterCampus,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    cmd.validate()?;
    lock_institution_for_change(&mut *conn, scope, institution_id).await?;
    if cmd.is_main {
        ensure_single_main(&mut *conn, scope, institution_id, None).await?;
    }
    let a = &cmd.address;
    let row = sqlx::query(&format!(
        "INSERT INTO institution.campus (id, tenant_id, institution_id, code, name, campus_type, \
         is_main, address_line1, address_line2, city, state, postal_code, country, latitude, \
         longitude, timezone, established_on, declared_capacity, contact_email, contact_phone, \
         description, working_hours) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, \
         COALESCE($13, 'IN'), $14, $15, $16, $17, $18, $19, $20, $21, $22) RETURNING {COLUMNS}"
    ))
    .bind(CampusId::new().as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(cmd.code.trim())
    .bind(cmd.name.trim())
    .bind(cmd.campus_type.as_str())
    .bind(cmd.is_main)
    .bind(trimmed(a.line1.as_ref()))
    .bind(trimmed(a.line2.as_ref()))
    .bind(trimmed(a.city.as_ref()))
    .bind(trimmed(a.state.as_ref()))
    .bind(trimmed(a.postal_code.as_ref()))
    .bind(trimmed(a.country.as_ref()))
    .bind(cmd.geo.map(|g| g.latitude))
    .bind(cmd.geo.map(|g| g.longitude))
    .bind(trimmed(cmd.timezone.as_ref()))
    .bind(cmd.established_on)
    .bind(cmd.declared_capacity)
    .bind(trimmed(cmd.contact_email.as_ref()))
    .bind(trimmed(cmd.contact_phone.as_ref()))
    .bind(trimmed(cmd.description.as_ref()))
    .bind(trimmed(cmd.working_hours.as_ref()))
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let c = from_row(&row).map_err(map_db_error)?.campus;
    record_revision(&mut *conn, scope, &c, "registered", &[], None, None, actor).await?;
    publish::event(
        &mut *conn,
        scope,
        CAMPUS_REGISTERED,
        CAMPUS_REGISTERED_SCHEMA,
        c.id.to_string(),
        &CampusRegisteredV1 {
            campus_id: c.id.as_uuid(),
            institution_id: institution_id.as_uuid(),
            code: c.code.clone(),
            name: c.name.clone(),
            timezone: c.timezone.clone(),
            is_main: c.is_main,
        },
        actor,
    )
    .await?;
    publish::audit(
        &mut *conn,
        scope,
        "institution.campus.register",
        ENTITY,
        c.id.to_string(),
        serde_json::json!({
            "code": c.code, "campusType": c.campus_type, "isMain": c.is_main,
            "declaredCapacity": c.declared_capacity, "status": c.status,
        }),
        actor,
    )
    .await?;
    Ok(c)
}

/// L3-B-019: governed modification of B03 metadata.
pub async fn modify(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: CampusId,
    expected: Version,
    cmd: ModifyCampus,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    let fields = cmd.validate()?;
    let scope = scope_of(pool, tenant, id).await?;
    let inst = scope
        .institution_id
        .ok_or_else(|| KernelError::internal_msg("institution scope"))?;
    let mut tx = begin(pool, &scope, inst).await?;
    let cur = load(&mut tx, &scope, id, true).await?.campus;
    expect_version(expected, cur.version.0)?;
    if !modifiable(cur.status) {
        return Err(KernelError::Conflict(format!(
            "a {} campus is read-only",
            cur.status
        )));
    }
    let is_main = cmd.is_main.unwrap_or(cur.is_main);
    if is_main && !cur.is_main {
        ensure_single_main(&mut tx, &scope, inst, Some(id)).await?;
    }
    let addr = cmd.address.clone().unwrap_or_else(|| cur.address.clone());
    let pick = |patch: &Option<Option<String>>, old: &Option<String>| match patch {
        Some(v) => trimmed(v.as_ref()),
        None => old.clone(),
    };
    let geo = match cmd.geo {
        Some(g) => g,
        None => cur.geo,
    };
    let row = sqlx::query(&format!(
        "UPDATE institution.campus SET name = $1, is_main = $2, address_line1 = $3, \
         address_line2 = $4, city = $5, state = $6, postal_code = $7, country = COALESCE($8, 'IN'), \
         latitude = $9, longitude = $10, timezone = $11, contact_email = $12, contact_phone = $13, \
         description = $14, working_hours = $15, version = version + 1, updated_at = now() \
         WHERE id = $16 AND tenant_id = $17 RETURNING {COLUMNS}"
    ))
    .bind(cmd.name.as_deref().map_or(cur.name.as_str(), str::trim))
    .bind(is_main)
    .bind(trimmed(addr.line1.as_ref()))
    .bind(trimmed(addr.line2.as_ref()))
    .bind(trimmed(addr.city.as_ref()))
    .bind(trimmed(addr.state.as_ref()))
    .bind(trimmed(addr.postal_code.as_ref()))
    .bind(trimmed(addr.country.as_ref()))
    .bind(geo.map(|g| g.latitude))
    .bind(geo.map(|g| g.longitude))
    .bind(pick(&cmd.timezone, &cur.timezone))
    .bind(pick(&cmd.contact_email, &cur.contact_email))
    .bind(pick(&cmd.contact_phone, &cur.contact_phone))
    .bind(pick(&cmd.description, &cur.description))
    .bind(pick(&cmd.working_hours, &cur.working_hours))
    .bind(id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let next = from_row(&row).map_err(map_db_error)?.campus;
    record_revision(
        &mut tx, &scope, &next, "modified", &fields, None, None, actor,
    )
    .await?;
    let changed: Vec<String> = fields.iter().map(|f| (*f).to_owned()).collect();
    publish::event(
        &mut tx,
        &scope,
        CAMPUS_MODIFIED,
        CAMPUS_MODIFIED_SCHEMA,
        id.to_string(),
        &ModifiedV1 {
            aggregate_id: id.as_uuid(),
            institution_id: inst.as_uuid(),
            version: next.version.0,
            changed_fields: changed.clone(),
        },
        actor,
    )
    .await?;
    publish::audit(
        &mut tx,
        &scope,
        "institution.campus.modify",
        ENTITY,
        id.to_string(),
        serde_json::json!({
            "fromVersion": expected.0, "version": next.version.0, "changedFields": changed,
            "before": serde_json::to_value(&cur).map_err(KernelError::internal)?,
        }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

fn ensure_no_pending(c: &Campus) -> Result<(), KernelError> {
    match &c.pending_action {
        Some(p) => Err(KernelError::Conflict(format!(
            "campus has a pending {p} decision"
        ))),
        None => Ok(()),
    }
}

/// Applies the approved activation (pending_activation → active) + `campus.activated`.
async fn apply_activation(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    cur: &Campus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    ensure_transition(cur.status, CampusStatus::Active)?;
    let next = set_status(
        conn,
        scope,
        cur,
        CampusStatus::Active,
        None,
        None,
        reason,
        None,
        actor,
    )
    .await?;
    Transition {
        event_type: CAMPUS_ACTIVATED,
        dataschema: CAMPUS_ACTIVATED_SCHEMA,
        audit_action: "institution.campus.activate",
        target_type: ENTITY,
        aggregate_id: cur.id.as_uuid(),
        from: cur.status.as_str(),
        to: next.status.as_str(),
        version: next.version.0,
        reason: reason.map(str::to_owned),
        process_instance_id: next.process_instance_id.clone(),
    }
    .emit(conn, scope, actor)
    .await?;
    Ok(next)
}

/// Applies the approved decommissioning (active → decommissioned) + `campus.decommissioned`.
async fn apply_decommission(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    cur: &Campus,
    reason: &str,
    effective_on: chrono::NaiveDate,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    ensure_transition(cur.status, CampusStatus::Decommissioned)?;
    let next = set_status(
        conn,
        scope,
        cur,
        CampusStatus::Decommissioned,
        None,
        None,
        Some(reason),
        Some(effective_on),
        actor,
    )
    .await?;
    Transition {
        event_type: CAMPUS_DECOMMISSIONED,
        dataschema: CAMPUS_DECOMMISSIONED_SCHEMA,
        audit_action: "institution.campus.decommission",
        target_type: ENTITY,
        aggregate_id: cur.id.as_uuid(),
        from: cur.status.as_str(),
        to: next.status.as_str(),
        version: next.version.0,
        reason: Some(reason.to_owned()),
        process_instance_id: next.process_instance_id.clone(),
    }
    .emit(conn, scope, actor)
    .await?;
    Ok(next)
}

/// L3-B-018: requests the activation decision through the WorkflowPort. With auto-approval the
/// campus is `active` on return; otherwise it stays `pending_activation`.
pub async fn activate(
    st: &InstitutionState,
    tenant: &ExecutionScope,
    id: CampusId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    let scope = scope_of(&st.pool, tenant, id).await?;
    let inst = scope
        .institution_id
        .ok_or_else(|| KernelError::internal_msg("institution scope"))?;
    // 1. Validate (read only).
    let cur = get(&st.pool, &scope, id).await?;
    expect_version(expected, cur.version.0)?;
    ensure_transition(cur.status, CampusStatus::PendingActivation)?;
    validate_activation(&cur)?;
    // 2. Approval step before the transaction (deterministic business key).
    let outcome = approval::request_approval(
        st,
        &scope,
        process::CAMPUS_ACTIVATION,
        approval::business_key(ENTITY, id, expected.0),
        BTreeMap::from([
            ("campusCode".to_owned(), serde_json::json!(cur.code)),
            ("campusName".to_owned(), serde_json::json!(cur.name)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    // 3. One owner-local transaction.
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let cur = load(&mut tx, &scope, id, true).await?.campus;
    expect_version(expected, cur.version.0)?;
    ensure_transition(cur.status, CampusStatus::PendingActivation)?;
    validate_activation(&cur)?;
    let pending = set_status(
        &mut tx,
        &scope,
        &cur,
        CampusStatus::PendingActivation,
        Some(("activation", reason.as_deref(), None)),
        Some(&pid),
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    audit_transition(
        &mut tx,
        &scope,
        "institution.campus.request_activation",
        ENTITY,
        id.as_uuid(),
        cur.status.as_str(),
        pending.status.as_str(),
        pending.version.0,
        reason.as_deref(),
        Some(&pid),
        actor,
    )
    .await?;
    let result = match outcome {
        ApprovalOutcome::Approved { .. } => {
            let system = Actor::system(actor.correlation_id.clone());
            apply_activation(&mut tx, &scope, &pending, reason.as_deref(), &system).await?
        }
        ApprovalOutcome::Pending { .. } => pending,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(result)
}

/// Mandatory B03 metadata before activation (L4-B-018-BH-002).
fn validate_activation(c: &Campus) -> Result<(), KernelError> {
    let missing = c.address.missing_for_activation();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(
            missing
                .into_iter()
                .map(|f| {
                    crate::curriculum::support::violation(
                        f,
                        "required-for-activation",
                        "required before the campus can become operational",
                    )
                })
                .collect(),
        ))
    }
}

/// Decision on a pending campus step (activation or decommissioning). `requires_decommission`
/// tells the handler which permission to enforce first.
pub async fn pending_action(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: CampusId,
) -> Result<Option<String>, KernelError> {
    Ok(get(pool, tenant, id).await?.pending_action)
}

/// `POST /campuses/{id}/decision` — completes a pending activation / decommissioning.
pub async fn decide(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: CampusId,
    expected: Version,
    decision: Decision,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    let scope = scope_of(pool, tenant, id).await?;
    let inst = scope
        .institution_id
        .ok_or_else(|| KernelError::internal_msg("institution scope"))?;
    let mut tx = begin(pool, &scope, inst).await?;
    let loaded = load(&mut tx, &scope, id, true).await?;
    let cur = &loaded.campus;
    expect_version(expected, cur.version.0)?;
    let next = match (cur.pending_action.as_deref(), decision) {
        (None, _) => {
            return Err(KernelError::Conflict(
                "campus has no pending decision".into(),
            ))
        }
        (Some(_), Decision::Return) => {
            return Err(KernelError::Validation(vec![
                crate::curriculum::support::violation(
                    "decision",
                    "unsupported",
                    "campus decisions are approve or reject",
                ),
            ]))
        }
        (Some("activation"), Decision::Approve) => {
            validate_activation(cur)?;
            apply_activation(&mut tx, &scope, cur, reason.as_deref(), actor).await?
        }
        (Some("activation"), Decision::Reject) => {
            ensure_transition(cur.status, CampusStatus::Registered)?;
            let next = set_status(
                &mut tx,
                &scope,
                cur,
                CampusStatus::Registered,
                None,
                None,
                reason.as_deref(),
                None,
                actor,
            )
            .await?;
            audit_transition(
                &mut tx,
                &scope,
                "institution.campus.reject_activation",
                ENTITY,
                id.as_uuid(),
                cur.status.as_str(),
                next.status.as_str(),
                next.version.0,
                reason.as_deref(),
                cur.process_instance_id.as_deref(),
                actor,
            )
            .await?;
            next
        }
        (Some(_), Decision::Approve) => {
            ensure_no_usages(&mut tx, &scope, id).await?;
            let why = loaded
                .pending_reason
                .clone()
                .or(reason.clone())
                .unwrap_or_default();
            let on = loaded
                .pending_effective_on
                .ok_or_else(|| KernelError::internal_msg("pending decommission without date"))?;
            apply_decommission(&mut tx, &scope, cur, &why, on, actor).await?
        }
        (Some(_), Decision::Reject) => {
            let next = set_status(
                &mut tx,
                &scope,
                cur,
                cur.status,
                None,
                None,
                reason.as_deref(),
                None,
                actor,
            )
            .await?;
            audit_transition(
                &mut tx,
                &scope,
                "institution.campus.reject_decommission",
                ENTITY,
                id.as_uuid(),
                cur.status.as_str(),
                next.status.as_str(),
                next.version.0,
                reason.as_deref(),
                cur.process_instance_id.as_deref(),
                actor,
            )
            .await?;
            next
        }
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Simple governed transitions without an approval step: suspend, restore, archive.
pub async fn transition(
    pool: &PgPool,
    tenant: &ExecutionScope,
    id: CampusId,
    expected: Version,
    to: CampusStatus,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    let (event_type, dataschema, action) = match to {
        CampusStatus::Suspended => (CAMPUS_SUSPENDED, CAMPUS_SUSPENDED_SCHEMA, "suspend"),
        CampusStatus::Active => (CAMPUS_RESTORED, CAMPUS_RESTORED_SCHEMA, "restore"),
        CampusStatus::Archived => (CAMPUS_ARCHIVED, CAMPUS_ARCHIVED_SCHEMA, "archive"),
        other => {
            return Err(KernelError::internal_msg(format!(
                "no direct command to {other}"
            )))
        }
    };
    let scope = scope_of(pool, tenant, id).await?;
    let inst = scope
        .institution_id
        .ok_or_else(|| KernelError::internal_msg("institution scope"))?;
    let mut tx = begin(pool, &scope, inst).await?;
    let cur = load(&mut tx, &scope, id, true).await?.campus;
    expect_version(expected, cur.version.0)?;
    // `active` is reachable from pending_activation only through the activation decision.
    if to == CampusStatus::Active && cur.status != CampusStatus::Suspended {
        return Err(KernelError::Conflict(format!(
            "only a suspended campus can be restored (campus is {})",
            cur.status
        )));
    }
    ensure_transition(cur.status, to)?;
    ensure_no_pending(&cur)?;
    let next = set_status(
        &mut tx,
        &scope,
        &cur,
        to,
        None,
        None,
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    Transition {
        event_type,
        dataschema,
        audit_action: match action {
            "suspend" => "institution.campus.suspend",
            "restore" => "institution.campus.restore",
            _ => "institution.campus.archive",
        },
        target_type: ENTITY,
        aggregate_id: id.as_uuid(),
        from: cur.status.as_str(),
        to: next.status.as_str(),
        version: next.version.0,
        reason,
        process_instance_id: None,
    }
    .emit(&mut tx, &scope, actor)
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(next)
}

/// Unresolved Domain B usages that block decommissioning (L4-B-021-BH-003): programmes that
/// are offered at the campus and not retired, and campus-scoped calendar assignments not retired.
async fn ensure_no_usages(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    id: CampusId,
) -> Result<(), KernelError> {
    let codes: Vec<String> = sqlx::query_scalar(
        "SELECT code FROM institution.programme WHERE tenant_id = $1 AND $2 = ANY(campus_ids) \
         AND status <> 'retired' ORDER BY code",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if !codes.is_empty() {
        return Err(KernelError::Conflict(format!(
            "campus is still used by programmes {}; retire them or remove the campus first",
            codes.join(", ")
        )));
    }
    // Integration (2B × 2C): an effective or draft campus-scoped calendar assignment (2C table,
    // read only) still governs the campus.
    let assignments: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.calendar_assignment WHERE tenant_id = $1 \
         AND scope_kind = 'campus' AND scope_id = $2 AND status <> 'retired'",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if assignments > 0 {
        return Err(KernelError::Conflict(format!(
            "campus is still governed by {assignments} calendar assignment(s); retire them first"
        )));
    }
    Ok(())
}

/// L3-B-021: decommissioning with explicit effective date, impact check and the
/// `CAMPUS_DECOMMISSION` approval step.
pub async fn decommission(
    st: &InstitutionState,
    tenant: &ExecutionScope,
    id: CampusId,
    expected: Version,
    cmd: DecommissionCampus,
    actor: &Actor,
) -> Result<Campus, KernelError> {
    cmd.validate()?;
    let reason = cmd.reason.trim().to_owned();
    let scope = scope_of(&st.pool, tenant, id).await?;
    let inst = scope
        .institution_id
        .ok_or_else(|| KernelError::internal_msg("institution scope"))?;
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let cur = load(&mut conn, &scope, id, false).await?.campus;
        expect_version(expected, cur.version.0)?;
        ensure_transition(cur.status, CampusStatus::Decommissioned)?;
        ensure_no_pending(&cur)?;
        ensure_no_usages(&mut conn, &scope, id).await?;
    }
    let outcome = approval::request_approval(
        st,
        &scope,
        process::CAMPUS_DECOMMISSION,
        approval::business_key(ENTITY, id, expected.0),
        BTreeMap::from([
            (
                "effectiveOn".to_owned(),
                serde_json::json!(cmd.effective_on),
            ),
            ("reason".to_owned(), serde_json::json!(reason)),
        ]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = begin(&st.pool, &scope, inst).await?;
    let cur = load(&mut tx, &scope, id, true).await?.campus;
    expect_version(expected, cur.version.0)?;
    ensure_transition(cur.status, CampusStatus::Decommissioned)?;
    ensure_no_pending(&cur)?;
    ensure_no_usages(&mut tx, &scope, id).await?;
    let pending = set_status(
        &mut tx,
        &scope,
        &cur,
        cur.status,
        Some(("decommission", Some(&reason), Some(cmd.effective_on))),
        Some(&pid),
        cur.status_reason.as_deref(),
        None,
        actor,
    )
    .await?;
    audit_transition(
        &mut tx,
        &scope,
        "institution.campus.request_decommission",
        ENTITY,
        id.as_uuid(),
        cur.status.as_str(),
        CampusStatus::Decommissioned.as_str(),
        pending.version.0,
        Some(&reason),
        Some(&pid),
        actor,
    )
    .await?;
    let result = match outcome {
        ApprovalOutcome::Approved { .. } => {
            let system = Actor::system(actor.correlation_id.clone());
            apply_decommission(
                &mut tx,
                &scope,
                &pending,
                &reason,
                cmd.effective_on,
                &system,
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => pending,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(result)
}

/// Live campuses (not decommissioned / archived) of an institution among `ids`; used by the
/// programme catalogue to validate offering campuses.
pub(crate) async fn live_campus_ids(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    ids: &[Uuid],
) -> Result<Vec<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM institution.campus WHERE tenant_id = $1 AND institution_id = $2 \
         AND id = ANY($3) AND status NOT IN ('decommissioned', 'archived')",
    )
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}
