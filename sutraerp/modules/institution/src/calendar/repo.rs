//! PostgreSQL persistence of the academic calendar (schema `institution`, slice 2C tables).
//!
//! Every mutation is one owner-local transaction (ADR-L5-0006/0007): lock the institution
//! (`shared::guard::lock_institution_for_change` → 409 while suspended/archived), lock the
//! aggregate row, check `If-Match`, apply the transition, enqueue the outbox event and write the
//! audit entry, commit. Every query filters on the session tenant; foreign rows are 404.
//!
//! "Today" is always the business date in the institution timezone, computed by PostgreSQL
//! (`(now() AT TIME ZONE institution.timezone)::date`), never the server's local date.

use super::domain::*;
use crate::shared::approval::{self, process, ApprovalOutcome};
use crate::shared::{guard, publish, Actor, InstitutionState};
use chrono::{NaiveDate, Utc};
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgPool, Row};
use std::collections::BTreeMap;
use sutra_contracts::institution::events::calendar::*;
use sutra_contracts::institution::events::StatusChangedV1;
use sutra_contracts::institution::ids::*;
use sutra_contracts::institution::status::{
    AcademicYearStatus, CalendarAssignmentStatus, CalendarExceptionStatus, PublicationStatus,
    SimulationStatus,
};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

type R<T> = Result<T, KernelError>;

fn db(e: sqlx::Error) -> KernelError {
    map_db_error(e)
}

fn bad_status(raw: &str) -> KernelError {
    KernelError::internal_msg(format!("unknown stored status `{raw}`"))
}

fn pub_status(raw: &str) -> R<PublicationStatus> {
    PublicationStatus::parse(raw).ok_or_else(|| bad_status(raw))
}

fn trimmed(s: Option<&str>) -> Option<String> {
    s.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

// ---------------------------------------------------------------------------------------------
// Row mapping
// ---------------------------------------------------------------------------------------------

const CAL_SELECT: &str = "SELECT c.id, c.institution_id, c.code, c.name, c.description, c.status, \
     c.status_reason, c.retired_at, c.version, c.created_at, c.updated_at \
     FROM institution.academic_calendar c";

fn calendar_row(r: &PgRow) -> R<Calendar> {
    let status: String = r.try_get("status").map_err(db)?;
    Ok(Calendar {
        id: AcademicCalendarId(r.try_get("id").map_err(db)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(db)?),
        code: r.try_get("code").map_err(db)?,
        name: r.try_get("name").map_err(db)?,
        description: r.try_get("description").map_err(db)?,
        status: pub_status(&status)?,
        status_reason: r.try_get("status_reason").map_err(db)?,
        retired_at: r.try_get("retired_at").map_err(db)?,
        version: Version(r.try_get("version").map_err(db)?),
        created_at: r.try_get("created_at").map_err(db)?,
        updated_at: r.try_get("updated_at").map_err(db)?,
    })
}

const YEAR_SELECT: &str = "SELECT y.id, y.institution_id, y.calendar_id, y.label, y.status, \
     y.starts_on, y.ends_on, y.predecessor_year_id, \
     (SELECT s.id FROM institution.academic_year s WHERE s.predecessor_year_id = y.id \
      ORDER BY s.created_at LIMIT 1) AS successor_year_id, \
     y.opened_at, y.closed_at, y.version, y.created_at, y.updated_at \
     FROM institution.academic_year y";

fn year_row(r: &PgRow) -> R<AcademicYear> {
    let status: String = r.try_get("status").map_err(db)?;
    Ok(AcademicYear {
        id: AcademicYearId(r.try_get("id").map_err(db)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(db)?),
        calendar_id: AcademicCalendarId(r.try_get("calendar_id").map_err(db)?),
        label: r.try_get("label").map_err(db)?,
        status: AcademicYearStatus::parse(&status).ok_or_else(|| bad_status(&status))?,
        starts_on: r.try_get("starts_on").map_err(db)?,
        ends_on: r.try_get("ends_on").map_err(db)?,
        predecessor_year_id: r
            .try_get::<Option<Uuid>, _>("predecessor_year_id")
            .map_err(db)?
            .map(AcademicYearId),
        successor_year_id: r
            .try_get::<Option<Uuid>, _>("successor_year_id")
            .map_err(db)?
            .map(AcademicYearId),
        opened_at: r.try_get("opened_at").map_err(db)?,
        closed_at: r.try_get("closed_at").map_err(db)?,
        version: Version(r.try_get("version").map_err(db)?),
        created_at: r.try_get("created_at").map_err(db)?,
        updated_at: r.try_get("updated_at").map_err(db)?,
    })
}

const VERSION_SELECT: &str = "SELECT v.id, v.calendar_id, v.institution_id, v.academic_year_id, \
     v.version_no, v.status, v.supersedes_id, v.starts_on, v.ends_on, \
     COALESCE(v.timezone, i.timezone) AS tz, v.notes, v.process_instance_id, v.status_reason, \
     v.published_at, v.retired_at, v.version, v.created_at, v.updated_at \
     FROM institution.calendar_version v JOIN institution.institution i ON i.id = v.institution_id";

fn version_row(r: &PgRow) -> R<CalendarVersion> {
    let status: String = r.try_get("status").map_err(db)?;
    Ok(CalendarVersion {
        id: CalendarVersionId(r.try_get("id").map_err(db)?),
        calendar_id: AcademicCalendarId(r.try_get("calendar_id").map_err(db)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(db)?),
        academic_year_id: AcademicYearId(r.try_get("academic_year_id").map_err(db)?),
        version_no: r.try_get("version_no").map_err(db)?,
        status: pub_status(&status)?,
        supersedes_version_id: r
            .try_get::<Option<Uuid>, _>("supersedes_id")
            .map_err(db)?
            .map(CalendarVersionId),
        starts_on: r.try_get("starts_on").map_err(db)?,
        ends_on: r.try_get("ends_on").map_err(db)?,
        timezone: r.try_get("tz").map_err(db)?,
        notes: r.try_get("notes").map_err(db)?,
        process_instance_id: r.try_get("process_instance_id").map_err(db)?,
        status_reason: r.try_get("status_reason").map_err(db)?,
        published_at: r.try_get("published_at").map_err(db)?,
        retired_at: r.try_get("retired_at").map_err(db)?,
        terms: vec![],
        key_dates: vec![],
        version: Version(r.try_get("version").map_err(db)?),
        created_at: r.try_get("created_at").map_err(db)?,
        updated_at: r.try_get("updated_at").map_err(db)?,
    })
}

const GOVERNING_TODAY: &str = "(now() AT TIME ZONE i.timezone)::date";

fn assignment_select() -> String {
    format!(
        "SELECT a.id, a.institution_id, a.calendar_id, a.calendar_version_id, a.scope_kind, \
         a.scope_id, a.effective_from, a.effective_to, a.status, a.status_reason, \
         (a.status = 'effective' AND a.effective_from <= {GOVERNING_TODAY} \
          AND (a.effective_to IS NULL OR a.effective_to >= {GOVERNING_TODAY})) AS governing, \
         a.activated_at, a.retired_at, a.version, a.created_at, a.updated_at \
         FROM institution.calendar_assignment a JOIN institution.institution i ON i.id = a.institution_id"
    )
}

fn assignment_row(r: &PgRow) -> R<CalendarAssignment> {
    let status: String = r.try_get("status").map_err(db)?;
    let kind: String = r.try_get("scope_kind").map_err(db)?;
    Ok(CalendarAssignment {
        id: CalendarAssignmentId(r.try_get("id").map_err(db)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(db)?),
        calendar_id: AcademicCalendarId(r.try_get("calendar_id").map_err(db)?),
        calendar_version_id: CalendarVersionId(r.try_get("calendar_version_id").map_err(db)?),
        scope_kind: ScopeKind::parse(&kind).ok_or_else(|| bad_status(&kind))?,
        scope_id: r.try_get("scope_id").map_err(db)?,
        effective_from: r.try_get("effective_from").map_err(db)?,
        effective_to: r.try_get("effective_to").map_err(db)?,
        status: CalendarAssignmentStatus::parse(&status).ok_or_else(|| bad_status(&status))?,
        status_reason: r.try_get("status_reason").map_err(db)?,
        governing: r.try_get("governing").map_err(db)?,
        activated_at: r.try_get("activated_at").map_err(db)?,
        retired_at: r.try_get("retired_at").map_err(db)?,
        version: Version(r.try_get("version").map_err(db)?),
        created_at: r.try_get("created_at").map_err(db)?,
        updated_at: r.try_get("updated_at").map_err(db)?,
    })
}

fn exception_select() -> String {
    format!(
        "SELECT e.id, e.institution_id, e.calendar_id, e.calendar_version_id, e.kind, e.scope_kind, \
         e.scope_id, e.starts_on, e.ends_on, e.reason, e.owner_subject, e.status, \
         (e.status = 'in_effect' AND e.starts_on <= {GOVERNING_TODAY} AND e.ends_on >= {GOVERNING_TODAY}) AS governing, \
         e.process_instance_id, e.decision_reason, e.decided_at, e.applied_at, e.completed_at, \
         e.version, e.created_at, e.updated_at \
         FROM institution.calendar_exception e JOIN institution.institution i ON i.id = e.institution_id"
    )
}

fn exception_row(r: &PgRow) -> R<CalendarException> {
    let status: String = r.try_get("status").map_err(db)?;
    let kind: String = r.try_get("kind").map_err(db)?;
    let scope_kind: String = r.try_get("scope_kind").map_err(db)?;
    Ok(CalendarException {
        id: CalendarExceptionId(r.try_get("id").map_err(db)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(db)?),
        calendar_id: AcademicCalendarId(r.try_get("calendar_id").map_err(db)?),
        calendar_version_id: CalendarVersionId(r.try_get("calendar_version_id").map_err(db)?),
        kind: ExceptionKind::parse(&kind).ok_or_else(|| bad_status(&kind))?,
        scope_kind: ScopeKind::parse(&scope_kind).ok_or_else(|| bad_status(&scope_kind))?,
        scope_id: r.try_get("scope_id").map_err(db)?,
        starts_on: r.try_get("starts_on").map_err(db)?,
        ends_on: r.try_get("ends_on").map_err(db)?,
        reason: r.try_get("reason").map_err(db)?,
        owner_subject: r.try_get("owner_subject").map_err(db)?,
        status: CalendarExceptionStatus::parse(&status).ok_or_else(|| bad_status(&status))?,
        governing: r.try_get("governing").map_err(db)?,
        process_instance_id: r.try_get("process_instance_id").map_err(db)?,
        decision_reason: r.try_get("decision_reason").map_err(db)?,
        decided_at: r.try_get("decided_at").map_err(db)?,
        applied_at: r.try_get("applied_at").map_err(db)?,
        completed_at: r.try_get("completed_at").map_err(db)?,
        version: Version(r.try_get("version").map_err(db)?),
        created_at: r.try_get("created_at").map_err(db)?,
        updated_at: r.try_get("updated_at").map_err(db)?,
    })
}

const SIM_SELECT: &str = "SELECT s.id, s.institution_id, s.calendar_id, s.baseline_version_id, \
     s.status, s.outcome, s.baseline, s.proposal, s.impact, s.created_by, s.decision_reason, \
     s.decided_by, s.decided_at, s.version, s.created_at, s.updated_at \
     FROM institution.calendar_simulation s";

fn simulation_row(r: &PgRow) -> R<CalendarSimulation> {
    let status: String = r.try_get("status").map_err(db)?;
    let outcome: String = r.try_get("outcome").map_err(db)?;
    let js = |c: &str| -> R<serde_json::Value> { r.try_get(c).map_err(db) };
    Ok(CalendarSimulation {
        id: CalendarSimulationId(r.try_get("id").map_err(db)?),
        institution_id: InstitutionId(r.try_get("institution_id").map_err(db)?),
        calendar_id: AcademicCalendarId(r.try_get("calendar_id").map_err(db)?),
        baseline_version_id: CalendarVersionId(r.try_get("baseline_version_id").map_err(db)?),
        status: SimulationStatus::parse(&status).ok_or_else(|| bad_status(&status))?,
        outcome: SimulationOutcome::parse(&outcome).ok_or_else(|| bad_status(&outcome))?,
        baseline: serde_json::from_value(js("baseline")?).map_err(KernelError::internal)?,
        proposal: serde_json::from_value(js("proposal")?).map_err(KernelError::internal)?,
        impact: serde_json::from_value(js("impact")?).map_err(KernelError::internal)?,
        created_by: r.try_get("created_by").map_err(db)?,
        decision_reason: r.try_get("decision_reason").map_err(db)?,
        decided_by: r.try_get("decided_by").map_err(db)?,
        decided_at: r.try_get("decided_at").map_err(db)?,
        version: Version(r.try_get("version").map_err(db)?),
        created_at: r.try_get("created_at").map_err(db)?,
        updated_at: r.try_get("updated_at").map_err(db)?,
    })
}

// ---------------------------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------------------------

/// Business "today" and zone of an institution of the tenant (404 when missing / foreign).
pub async fn business_today(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution_id: InstitutionId,
) -> R<(NaiveDate, String)> {
    let row = sqlx::query(
        "SELECT (now() AT TIME ZONE timezone)::date AS today, timezone \
         FROM institution.institution WHERE id = $1 AND tenant_id = $2",
    )
    .bind(institution_id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "institution",
    })?;
    Ok((
        row.try_get("today").map_err(db)?,
        row.try_get("timezone").map_err(db)?,
    ))
}

/// A locked aggregate row: institution scope, stored status and version.
struct Locked {
    scope: ExecutionScope,
    institution_id: InstitutionId,
    status: String,
}

/// Locks the institution (structure editable) and then the aggregate row `FOR UPDATE`, checks
/// the expected version (412) and returns the institution scope. Unknown / foreign → 404.
async fn lock(
    conn: &mut PgConnection,
    table: &'static str,
    entity: &'static str,
    tenant: TenantId,
    id: Uuid,
    expected: Option<Version>,
) -> R<Locked> {
    let inst: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT institution_id FROM institution.{table} WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?;
    let inst = InstitutionId(inst.ok_or(KernelError::NotFound { entity })?);
    let scope = ExecutionScope::institution(tenant, inst);
    guard::lock_institution_for_change(conn, &scope, inst).await?;
    let row = sqlx::query(&format!(
        "SELECT status, version FROM institution.{table} WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(db)?;
    let version: i64 = row.try_get("version").map_err(db)?;
    if let Some(e) = expected {
        if e.0 != version {
            return Err(KernelError::VersionConflict {
                expected: e.0,
                actual: version,
            });
        }
    }
    Ok(Locked {
        scope,
        institution_id: inst,
        status: row.try_get("status").map_err(db)?,
    })
}

/// Unlocked peek used before an approval step: (institution scope, status, version).
async fn peek(
    pool: &PgPool,
    table: &'static str,
    entity: &'static str,
    tenant: TenantId,
    id: Uuid,
) -> R<(ExecutionScope, String, i64)> {
    let row = sqlx::query(&format!(
        "SELECT institution_id, status, version FROM institution.{table} WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound { entity })?;
    let inst = InstitutionId(row.try_get("institution_id").map_err(db)?);
    Ok((
        ExecutionScope::institution(tenant, inst),
        row.try_get("status").map_err(db)?,
        row.try_get("version").map_err(db)?,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn status_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    event_type: &'static str,
    schema: &'static str,
    aggregate_id: Uuid,
    from: &str,
    to: &str,
    version: i64,
    reason: Option<String>,
    process_instance_id: Option<String>,
    actor: &Actor,
) -> R<()> {
    let institution_id = scope
        .institution_id
        .map(|i| i.as_uuid())
        .unwrap_or_default();
    publish::event(
        conn,
        scope,
        event_type,
        schema,
        aggregate_id.to_string(),
        &StatusChangedV1 {
            aggregate_id,
            institution_id,
            from_status: from.to_owned(),
            to_status: to.to_owned(),
            version,
            reason,
            process_instance_id,
        },
        actor,
    )
    .await
}

async fn audit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    action: &str,
    target_type: &str,
    target_id: Uuid,
    details: serde_json::Value,
    actor: &Actor,
) -> R<()> {
    publish::audit(
        conn,
        scope,
        &format!("institution.{action}"),
        target_type,
        target_id.to_string(),
        details,
        actor,
    )
    .await
}

// ---------------------------------------------------------------------------------------------
// Calendars
// ---------------------------------------------------------------------------------------------

pub async fn get_calendar(pool: &PgPool, tenant: TenantId, id: AcademicCalendarId) -> R<Calendar> {
    let row = sqlx::query(&format!(
        "{CAL_SELECT} WHERE c.id = $1 AND c.tenant_id = $2"
    ))
    .bind(id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "academic calendar",
    })?;
    calendar_row(&row)
}

async fn calendar_in(conn: &mut PgConnection, tenant: TenantId, id: Uuid) -> R<Calendar> {
    let row = sqlx::query(&format!(
        "{CAL_SELECT} WHERE c.id = $1 AND c.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "academic calendar",
    })?;
    calendar_row(&row)
}

pub async fn list_calendars(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<PublicationStatus>,
) -> R<Vec<Calendar>> {
    guard::institution_status(pool, scope, institution_id).await?;
    let rows = sqlx::query(&format!(
        "{CAL_SELECT} WHERE c.tenant_id = $1 AND c.institution_id = $2 \
         AND ($3::text IS NULL OR c.status = $3) ORDER BY c.created_at"
    ))
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(pool)
    .await
    .map_err(db)?;
    rows.iter().map(calendar_row).collect()
}

pub async fn create_calendar(
    pool: &PgPool,
    tenant: TenantId,
    institution_id: InstitutionId,
    cmd: NewCalendar,
    actor: &Actor,
) -> R<Calendar> {
    cmd.validate()?;
    let scope = ExecutionScope::institution(tenant, institution_id);
    let mut tx = pool.begin().await.map_err(db)?;
    guard::lock_institution_for_change(&mut tx, &scope, institution_id).await?;
    let id = AcademicCalendarId::new();
    sqlx::query(
        "INSERT INTO institution.academic_calendar (id, tenant_id, institution_id, code, name, description) \
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(cmd.code.trim())
    .bind(cmd.name.trim())
    .bind(trimmed(cmd.description.as_deref()))
    .execute(&mut *tx)
    .await
    .map_err(|e| match db(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!(
            "a calendar with code {} already exists in this institution",
            cmd.code.trim()
        )),
        other => other,
    })?;
    audit(
        &mut tx,
        &scope,
        "academic_calendar.create",
        "academic_calendar",
        id.as_uuid(),
        json!({ "code": cmd.code.trim() }),
        actor,
    )
    .await?;
    let cal = calendar_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(cal)
}

/// Retires a calendar (L3-B-033): every published / draft version is retired, draft assignments
/// are retired; refused while an assignment is effective or an academic year is open. Nothing
/// is deleted; superseded versions, closed years and exceptions stay as history.
pub async fn retire_calendar(
    pool: &PgPool,
    tenant: TenantId,
    id: AcademicCalendarId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> R<Calendar> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "academic_calendar",
        "academic calendar",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = pub_status(&l.status)?;
    if !calendar_can_transition(from, PublicationStatus::Retired) {
        return Err(illegal("calendar", from, PublicationStatus::Retired));
    }
    let effective: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.calendar_assignment WHERE calendar_id = $1 AND status = 'effective' ORDER BY created_at",
    )
    .bind(id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(db)?;
    if !effective.is_empty() {
        return Err(KernelError::Conflict(format!(
            "retire or end the {} effective assignment(s) of this calendar first",
            effective.len()
        )));
    }
    let open: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.academic_year WHERE calendar_id = $1 AND status IN ('open', 'closing')",
    )
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    if open > 0 {
        return Err(KernelError::Conflict(
            "close the open academic year of this calendar first".into(),
        ));
    }
    let reason = trimmed(reason.as_deref());
    // Draft assignments never governed: retire them with the calendar.
    sqlx::query(
        "UPDATE institution.calendar_assignment SET status = 'retired', retired_at = now(), \
         status_reason = 'calendar retired', version = version + 1, updated_at = now() \
         WHERE calendar_id = $1 AND status = 'draft'",
    )
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    let versions = sqlx::query(
        "UPDATE institution.calendar_version v SET status = 'retired', retired_at = now(), \
         status_reason = $2, version = v.version + 1, updated_at = now() \
         FROM (SELECT id, status FROM institution.calendar_version WHERE calendar_id = $1 \
               AND status IN ('draft', 'in_review', 'published') FOR UPDATE) old \
         WHERE v.id = old.id RETURNING v.id, old.status AS from_status, v.version",
    )
    .bind(id.as_uuid())
    .bind(&reason)
    .fetch_all(&mut *tx)
    .await
    .map_err(db)?;
    for v in &versions {
        let vid: Uuid = v.try_get("id").map_err(db)?;
        let from_status: String = v.try_get("from_status").map_err(db)?;
        if from_status == "published" {
            status_event(
                &mut tx,
                &l.scope,
                CALENDAR_VERSION_RETIRED,
                CALENDAR_VERSION_RETIRED_SCHEMA,
                vid,
                &from_status,
                "retired",
                v.try_get("version").map_err(db)?,
                reason.clone(),
                None,
                actor,
            )
            .await?;
        }
    }
    let new_version: i64 = sqlx::query_scalar(
        "UPDATE institution.academic_calendar SET status = 'retired', retired_at = now(), \
         status_reason = $2, version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(id.as_uuid())
    .bind(&reason)
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    status_event(
        &mut tx,
        &l.scope,
        CALENDAR_RETIRED,
        CALENDAR_RETIRED_SCHEMA,
        id.as_uuid(),
        from.as_str(),
        "retired",
        new_version,
        reason.clone(),
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &l.scope,
        "academic_calendar.retire",
        "academic_calendar",
        id.as_uuid(),
        json!({ "fromStatus": from.as_str(), "retiredVersions": versions.len(), "reason": reason }),
        actor,
    )
    .await?;
    let cal = calendar_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(cal)
}

async fn require_live_calendar(
    conn: &mut PgConnection,
    tenant: TenantId,
    calendar_id: Uuid,
) -> R<Calendar> {
    let cal = calendar_in(conn, tenant, calendar_id).await?;
    if cal.status == PublicationStatus::Retired {
        return Err(KernelError::Conflict(
            "the calendar is retired; it accepts no new use".into(),
        ));
    }
    Ok(cal)
}

// ---------------------------------------------------------------------------------------------
// Academic years
// ---------------------------------------------------------------------------------------------

async fn year_in(conn: &mut PgConnection, tenant: TenantId, id: Uuid) -> R<AcademicYear> {
    let row = sqlx::query(&format!(
        "{YEAR_SELECT} WHERE y.id = $1 AND y.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "academic year",
    })?;
    year_row(&row)
}

pub async fn get_year(pool: &PgPool, tenant: TenantId, id: AcademicYearId) -> R<AcademicYear> {
    let mut conn = pool.acquire().await.map_err(db)?;
    year_in(&mut conn, tenant, id.as_uuid()).await
}

pub async fn list_years(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<AcademicYearStatus>,
    calendar_id: Option<AcademicCalendarId>,
) -> R<Vec<AcademicYear>> {
    guard::institution_status(pool, scope, institution_id).await?;
    let rows = sqlx::query(&format!(
        "{YEAR_SELECT} WHERE y.tenant_id = $1 AND y.institution_id = $2 \
         AND ($3::text IS NULL OR y.status = $3) AND ($4::uuid IS NULL OR y.calendar_id = $4) \
         ORDER BY y.starts_on"
    ))
    .bind(scope.tenant_id.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .bind(calendar_id.map(|c| c.as_uuid()))
    .fetch_all(pool)
    .await
    .map_err(db)?;
    rows.iter().map(year_row).collect()
}

/// Inserts a planned year after checking it does not overlap another year of the calendar.
#[allow(clippy::too_many_arguments)]
async fn insert_year(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution_id: InstitutionId,
    calendar_id: Uuid,
    label: &str,
    starts_on: NaiveDate,
    ends_on: NaiveDate,
    predecessor: Option<Uuid>,
) -> R<Uuid> {
    // Serialise year creation per calendar (overlap check + insert).
    sqlx::query("SELECT id FROM institution.academic_calendar WHERE id = $1 FOR UPDATE")
        .bind(calendar_id)
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    let clash: Option<String> = sqlx::query_scalar(
        "SELECT label FROM institution.academic_year WHERE calendar_id = $1 \
         AND starts_on <= $3 AND ends_on >= $2 LIMIT 1",
    )
    .bind(calendar_id)
    .bind(starts_on)
    .bind(ends_on)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?;
    if let Some(other) = clash {
        return Err(KernelError::Conflict(format!(
            "the dates overlap academic year {other} of this calendar"
        )));
    }
    let id = AcademicYearId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.academic_year (id, tenant_id, institution_id, calendar_id, label, starts_on, ends_on, predecessor_year_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(calendar_id)
    .bind(label.trim())
    .bind(starts_on)
    .bind(ends_on)
    .bind(predecessor)
    .execute(&mut *conn)
    .await
    .map_err(|e| match db(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!(
            "academic year {} already exists in this calendar",
            label.trim()
        )),
        other => other,
    })?;
    Ok(id)
}

pub async fn create_year(
    pool: &PgPool,
    tenant: TenantId,
    institution_id: InstitutionId,
    cmd: NewAcademicYear,
    actor: &Actor,
) -> R<AcademicYear> {
    cmd.validate()?;
    let scope = ExecutionScope::institution(tenant, institution_id);
    let mut tx = pool.begin().await.map_err(db)?;
    guard::lock_institution_for_change(&mut tx, &scope, institution_id).await?;
    let cal = require_live_calendar(&mut tx, tenant, cmd.calendar_id.as_uuid()).await?;
    if cal.institution_id != institution_id {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "calendarId",
            "foreign",
            "the calendar belongs to another institution",
        )]));
    }
    let id = insert_year(
        &mut tx,
        tenant,
        institution_id,
        cal.id.as_uuid(),
        &cmd.label,
        cmd.starts_on,
        cmd.ends_on,
        None,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "academic_year.create",
        "academic_year",
        id,
        json!({ "calendarId": cal.id, "label": cmd.label.trim(), "startsOn": cmd.starts_on, "endsOn": cmd.ends_on }),
        actor,
    )
    .await?;
    let y = year_in(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(db)?;
    Ok(y)
}

async fn year_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    y: &AcademicYear,
    event_type: &'static str,
    schema: &'static str,
    actor: &Actor,
) -> R<()> {
    publish::event(
        conn,
        scope,
        event_type,
        schema,
        y.id.to_string(),
        &AcademicYearV1 {
            academic_year_id: y.id.as_uuid(),
            institution_id: y.institution_id.as_uuid(),
            calendar_id: y.calendar_id.as_uuid(),
            label: y.label.clone(),
            starts_on: y.starts_on,
            ends_on: y.ends_on,
            successor_year_id: y.successor_year_id.map(|s| s.as_uuid()),
        },
        actor,
    )
    .await
}

/// Planned → Open: the open academic context (L3-B-027). Requires a published calendar version
/// for the year (BH-001) and no other open year in the calendar. Creates no assignment (BH-006).
pub async fn open_year(
    pool: &PgPool,
    tenant: TenantId,
    id: AcademicYearId,
    expected: Version,
    actor: &Actor,
) -> R<AcademicYear> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "academic_year",
        "academic year",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = AcademicYearStatus::parse(&l.status).ok_or_else(|| bad_status(&l.status))?;
    if !year_can_transition(from, AcademicYearStatus::Open) {
        return Err(illegal("academic year", from, AcademicYearStatus::Open));
    }
    let y = year_in(&mut tx, tenant, id.as_uuid()).await?;
    require_live_calendar(&mut tx, tenant, y.calendar_id.as_uuid()).await?;
    let published: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.calendar_version WHERE academic_year_id = $1 AND status = 'published'",
    )
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    if published == 0 {
        return Err(KernelError::Conflict(
            "publish a calendar version for this academic year before opening it".into(),
        ));
    }
    let other: Option<String> = sqlx::query_scalar(
        "SELECT label FROM institution.academic_year WHERE calendar_id = $1 AND id <> $2 AND status IN ('open', 'closing')",
    )
    .bind(y.calendar_id.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?;
    if let Some(label) = other {
        return Err(KernelError::Conflict(format!(
            "academic year {label} is still open in this calendar; close it first"
        )));
    }
    sqlx::query(
        "UPDATE institution.academic_year SET status = 'open', opened_at = now(), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    let y = year_in(&mut tx, tenant, id.as_uuid()).await?;
    year_event(
        &mut tx,
        &l.scope,
        &y,
        ACADEMIC_YEAR_OPENED,
        ACADEMIC_YEAR_OPENED_SCHEMA,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &l.scope,
        "academic_year.open",
        "academic_year",
        id.as_uuid(),
        json!({ "fromStatus": from.as_str(), "toStatus": "open" }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(db)?;
    Ok(y)
}

/// Result of a closure request: the year plus the evidence evaluated.
pub struct ClosureResult {
    pub year: AcademicYear,
    pub evidence: Vec<ClosureEvidence>,
}

async fn closure_facts(
    conn: &mut PgConnection,
    tenant: TenantId,
    y: &AcademicYear,
    early: Option<String>,
) -> R<Vec<ClosureEvidence>> {
    let (today, _) = business_today(conn, tenant, y.institution_id).await?;
    let published: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.calendar_version WHERE academic_year_id = $1 AND status IN ('published', 'superseded')",
    )
    .bind(y.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(db)?;
    let pending: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.calendar_exception WHERE calendar_id = $1 AND status = 'requested' \
         AND starts_on <= $3 AND ends_on >= $2",
    )
    .bind(y.calendar_id.as_uuid())
    .bind(y.starts_on)
    .bind(y.ends_on)
    .fetch_one(&mut *conn)
    .await
    .map_err(db)?;
    Ok(closure_evidence(&ClosureFacts {
        today,
        ends_on: y.ends_on,
        published_versions: published,
        pending_exceptions: pending,
        early_closure_reason: early,
    }))
}

/// Open → Closing (approval step) → Closed with the next-cycle foundation (L3-B-031).
pub async fn close_year(
    st: &InstitutionState,
    tenant: TenantId,
    id: AcademicYearId,
    expected: Version,
    cmd: CloseAcademicYear,
    actor: &Actor,
) -> R<ClosureResult> {
    let (scope, status, version) = peek(
        &st.pool,
        "academic_year",
        "academic year",
        tenant,
        id.as_uuid(),
    )
    .await?;
    let from = AcademicYearStatus::parse(&status).ok_or_else(|| bad_status(&status))?;
    if version != expected.0 {
        return Err(KernelError::VersionConflict {
            expected: expected.0,
            actual: version,
        });
    }
    if !year_can_transition(from, AcademicYearStatus::Closing) {
        return Err(illegal("academic year", from, AcademicYearStatus::Closing));
    }
    // Evidence first (read only): a blocked closure starts no approval process.
    let early = trimmed(cmd.early_closure_reason.as_deref());
    {
        let mut conn = st.pool.acquire().await.map_err(db)?;
        let y = year_in(&mut conn, tenant, id.as_uuid()).await?;
        let evidence = closure_facts(&mut conn, tenant, &y, early.clone()).await?;
        let blocking: Vec<&str> = evidence
            .iter()
            .filter(|e| e.blocks())
            .map(|e| e.key.as_str())
            .collect();
        if !blocking.is_empty() {
            return Err(KernelError::Conflict(format!(
                "the academic year cannot close yet: {}",
                evidence
                    .iter()
                    .filter(|e| e.blocks())
                    .map(|e| format!("{} ({})", e.key, e.detail))
                    .collect::<Vec<_>>()
                    .join("; ")
            )));
        }
    }
    let outcome = approval::request_approval(
        st,
        &scope,
        process::ACADEMIC_YEAR_CLOSURE,
        approval::business_key("academic_year", id, version),
        BTreeMap::from([("academicYearId".to_owned(), json!(id))]),
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "academic_year",
        "academic year",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    if l.status != "open" {
        return Err(illegal("academic year", &l.status, "closing"));
    }
    let y = year_in(&mut tx, tenant, id.as_uuid()).await?;
    let evidence = closure_facts(&mut tx, tenant, &y, early.clone()).await?;
    if evidence.iter().any(ClosureEvidence::blocks) {
        return Err(KernelError::Conflict(
            "closure evidence changed; retry".into(),
        ));
    }
    sqlx::query(
        "UPDATE institution.academic_year SET status = 'closing', version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    let pid = outcome.process_instance_id().to_owned();
    audit(
        &mut tx,
        &l.scope,
        "academic_year.request_closure",
        "academic_year",
        id.as_uuid(),
        json!({ "evidence": evidence, "earlyClosureReason": early, "processInstanceId": pid, "reason": trimmed(cmd.reason.as_deref()) }),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        complete_closure(
            &mut tx,
            tenant,
            &l.scope,
            &y,
            &cmd,
            &evidence,
            early,
            Some(pid),
            &Actor::system(actor.correlation_id.clone()),
            actor,
        )
        .await?;
    } else {
        // Pending: remember the inputs on the closure-decision audit trail; the decision endpoint re-reads them.
        sqlx::query(
            "INSERT INTO institution.academic_year_closure (tenant_id, institution_id, academic_year_id, evidence, early_closure_reason, process_instance_id, closed_by, closed_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, now())",
        )
        .bind(tenant.as_uuid())
        .bind(l.institution_id.as_uuid())
        .bind(id.as_uuid())
        .bind(json!({ "pending": true, "evidence": evidence, "request": { "prepareSuccessor": cmd.prepare_successor, "successor": cmd.successor.as_ref().map(|s| json!({"label": s.label, "startsOn": s.starts_on, "endsOn": s.ends_on})) } }))
        .bind(&early)
        .bind(&pid)
        .bind(&actor.id)
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    }
    let year = year_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(ClosureResult { year, evidence })
}

/// Closing → Closed: completes temporary exceptions (never carried forward, BH-005), prepares
/// the next-cycle foundation as a planned year + draft version (BH-004, never opened, BH-006),
/// records the closure and emits `academic_year.closed`.
#[allow(clippy::too_many_arguments)]
async fn complete_closure(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    y: &AcademicYear,
    cmd: &CloseAcademicYear,
    evidence: &[ClosureEvidence],
    early: Option<String>,
    pid: Option<String>,
    decider: &Actor,
    requester: &Actor,
) -> R<()> {
    // Temporary exceptions end with the year.
    let ended = sqlx::query(
        "UPDATE institution.calendar_exception e SET status = 'completed', completed_at = now(), \
         version = e.version + 1, updated_at = now() \
         FROM (SELECT id, status FROM institution.calendar_exception WHERE calendar_id = $1 \
               AND status IN ('approved', 'in_effect') AND starts_on <= $3 AND ends_on >= $2 FOR UPDATE) old \
         WHERE e.id = old.id RETURNING e.id, old.status AS from_status, e.version",
    )
    .bind(y.calendar_id.as_uuid())
    .bind(y.starts_on)
    .bind(y.ends_on)
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    for e in &ended {
        let from_status: String = e.try_get("from_status").map_err(db)?;
        status_event(
            conn,
            scope,
            CALENDAR_EXCEPTION_COMPLETED,
            CALENDAR_EXCEPTION_COMPLETED_SCHEMA,
            e.try_get("id").map_err(db)?,
            &from_status,
            "completed",
            e.try_get("version").map_err(db)?,
            Some(format!("academic year {} closed", y.label)),
            pid.clone(),
            decider,
        )
        .await?;
    }
    // Next-cycle foundation.
    let (mut successor_year, mut successor_version) = (None, None);
    if cmd.prepare_successor {
        let s = cmd.successor.clone().unwrap_or_default();
        let label = trimmed(s.label.as_deref()).unwrap_or_else(|| next_label(&y.label));
        let (ds, de) = successor_dates(y.starts_on, y.ends_on);
        let starts = s.starts_on.unwrap_or(ds);
        let ends = s.ends_on.unwrap_or(de);
        NewAcademicYear {
            calendar_id: y.calendar_id,
            label: label.clone(),
            starts_on: starts,
            ends_on: ends,
        }
        .validate()?;
        let existing: Option<(Uuid, String)> = sqlx::query_as(
            "SELECT id, status FROM institution.academic_year WHERE calendar_id = $1 AND label = $2",
        )
        .bind(y.calendar_id.as_uuid())
        .bind(&label)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db)?;
        let year_id = match existing {
            Some((eid, _)) => {
                // A successor already planned by hand: link it, do not duplicate it.
                sqlx::query(
                    "UPDATE institution.academic_year SET predecessor_year_id = COALESCE(predecessor_year_id, $2), updated_at = now() WHERE id = $1",
                )
                .bind(eid)
                .bind(y.id.as_uuid())
                .execute(&mut *conn)
                .await
                .map_err(db)?;
                eid
            }
            None => {
                let nid = insert_year(
                    conn,
                    tenant,
                    y.institution_id,
                    y.calendar_id.as_uuid(),
                    &label,
                    starts,
                    ends,
                    Some(y.id.as_uuid()),
                )
                .await?;
                // Draft calendar version copying the governing version's structure, shifted.
                let base: Option<Uuid> = sqlx::query_scalar(
                    "SELECT id FROM institution.calendar_version WHERE academic_year_id = $1 \
                     AND status IN ('published', 'superseded') ORDER BY version_no DESC LIMIT 1",
                )
                .bind(y.id.as_uuid())
                .fetch_optional(&mut *conn)
                .await
                .map_err(db)?;
                if let Some(base) = base {
                    let vid = insert_version(
                        conn,
                        tenant,
                        y.institution_id,
                        y.calendar_id.as_uuid(),
                        nid,
                        starts,
                        ends,
                        Some(
                            "Next-cycle foundation prepared by academic year closure (draft)"
                                .into(),
                        ),
                        None,
                    )
                    .await?;
                    copy_structure(
                        conn,
                        tenant,
                        y.institution_id,
                        base,
                        vid,
                        (starts - y.starts_on).num_days(),
                        (starts, ends),
                    )
                    .await?;
                    successor_version = Some(vid);
                }
                nid
            }
        };
        successor_year = Some(year_id);
    }
    sqlx::query(
        "UPDATE institution.academic_year SET status = 'closed', closed_at = now(), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(y.id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    sqlx::query(
        "INSERT INTO institution.academic_year_closure (tenant_id, institution_id, academic_year_id, successor_year_id, successor_version_id, evidence, early_closure_reason, process_instance_id, closed_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
         ON CONFLICT (academic_year_id) DO UPDATE SET successor_year_id = EXCLUDED.successor_year_id, \
           successor_version_id = EXCLUDED.successor_version_id, evidence = EXCLUDED.evidence, \
           early_closure_reason = EXCLUDED.early_closure_reason, closed_by = EXCLUDED.closed_by, closed_at = now()",
    )
    .bind(tenant.as_uuid())
    .bind(y.institution_id.as_uuid())
    .bind(y.id.as_uuid())
    .bind(successor_year)
    .bind(successor_version)
    .bind(json!({ "evidence": evidence, "completedExceptions": ended.len() }))
    .bind(&early)
    .bind(&pid)
    .bind(&requester.id)
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    let closed = year_in(conn, tenant, y.id.as_uuid()).await?;
    year_event(
        conn,
        scope,
        &closed,
        ACADEMIC_YEAR_CLOSED,
        ACADEMIC_YEAR_CLOSED_SCHEMA,
        decider,
    )
    .await?;
    audit(
        conn,
        scope,
        "academic_year.close",
        "academic_year",
        y.id.as_uuid(),
        json!({ "successorYearId": successor_year, "successorVersionId": successor_version, "completedExceptions": ended.len(), "processInstanceId": pid }),
        decider,
    )
    .await
}

/// Explicit closure decision (`ApprovalMode::AwaitDecision`): approve → closed, reject → open.
pub async fn decide_closure(
    pool: &PgPool,
    tenant: TenantId,
    id: AcademicYearId,
    expected: Version,
    d: Decision,
    actor: &Actor,
) -> R<AcademicYear> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "academic_year",
        "academic year",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    if l.status != "closing" {
        return Err(KernelError::Conflict(format!(
            "academic year is {}; no closure awaits a decision",
            l.status
        )));
    }
    let y = year_in(&mut tx, tenant, id.as_uuid()).await?;
    let pending: Option<(serde_json::Value, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT evidence, early_closure_reason, process_instance_id FROM institution.academic_year_closure WHERE academic_year_id = $1",
    )
    .bind(id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?;
    let (stored, early, pid) = pending.unwrap_or((json!({}), None, None));
    match d.decision {
        DecisionKind::Approve => {
            let req = &stored["request"];
            let cmd = CloseAcademicYear {
                reason: d.reason.clone(),
                early_closure_reason: early.clone(),
                prepare_successor: req["prepareSuccessor"].as_bool().unwrap_or(true),
                successor: serde_json::from_value(req["successor"].clone())
                    .ok()
                    .flatten(),
            };
            let evidence = closure_facts(&mut tx, tenant, &y, early.clone()).await?;
            if evidence.iter().any(ClosureEvidence::blocks) {
                return Err(KernelError::Conflict(
                    "closure evidence is no longer satisfied".into(),
                ));
            }
            complete_closure(
                &mut tx, tenant, &l.scope, &y, &cmd, &evidence, early, pid, actor, actor,
            )
            .await?;
        }
        DecisionKind::Reject => {
            sqlx::query(
                "DELETE FROM institution.academic_year_closure WHERE academic_year_id = $1",
            )
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            sqlx::query(
                "UPDATE institution.academic_year SET status = 'open', version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            audit(
                &mut tx,
                &l.scope,
                "academic_year.reject_closure",
                "academic_year",
                id.as_uuid(),
                json!({ "reason": trimmed(d.reason.as_deref()) }),
                actor,
            )
            .await?;
        }
    }
    let y = year_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(y)
}

// ---------------------------------------------------------------------------------------------
// Calendar versions, terms, key dates
// ---------------------------------------------------------------------------------------------

async fn attach_children(conn: &mut PgConnection, versions: &mut [CalendarVersion]) -> R<()> {
    if versions.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = versions.iter().map(|v| v.id.as_uuid()).collect();
    let terms = sqlx::query(
        "SELECT id, calendar_version_id, code, name, sequence, starts_on, ends_on FROM institution.academic_term \
         WHERE calendar_version_id = ANY($1) ORDER BY sequence, starts_on",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    let keys = sqlx::query(
        "SELECT id, calendar_version_id, kind, name, starts_on, ends_on FROM institution.calendar_key_date \
         WHERE calendar_version_id = ANY($1) ORDER BY starts_on, name",
    )
    .bind(&ids)
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    for v in versions.iter_mut() {
        for t in &terms {
            if t.try_get::<Uuid, _>("calendar_version_id").map_err(db)? == v.id.as_uuid() {
                v.terms.push(Term {
                    id: AcademicTermId(t.try_get("id").map_err(db)?),
                    code: t.try_get("code").map_err(db)?,
                    name: t.try_get("name").map_err(db)?,
                    sequence: t.try_get("sequence").map_err(db)?,
                    starts_on: t.try_get("starts_on").map_err(db)?,
                    ends_on: t.try_get("ends_on").map_err(db)?,
                });
            }
        }
        for k in &keys {
            if k.try_get::<Uuid, _>("calendar_version_id").map_err(db)? == v.id.as_uuid() {
                let kind: String = k.try_get("kind").map_err(db)?;
                v.key_dates.push(KeyDate {
                    id: KeyDateId(k.try_get("id").map_err(db)?),
                    kind: KeyDateKind::parse(&kind).ok_or_else(|| bad_status(&kind))?,
                    name: k.try_get("name").map_err(db)?,
                    starts_on: k.try_get("starts_on").map_err(db)?,
                    ends_on: k.try_get("ends_on").map_err(db)?,
                });
            }
        }
    }
    Ok(())
}

async fn version_in(conn: &mut PgConnection, tenant: TenantId, id: Uuid) -> R<CalendarVersion> {
    let row = sqlx::query(&format!(
        "{VERSION_SELECT} WHERE v.id = $1 AND v.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "calendar version",
    })?;
    let mut v = [version_row(&row)?];
    attach_children(conn, &mut v).await?;
    let [v] = v;
    Ok(v)
}

pub async fn get_version(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
) -> R<CalendarVersion> {
    let mut conn = pool.acquire().await.map_err(db)?;
    version_in(&mut conn, tenant, id.as_uuid()).await
}

pub async fn list_versions(
    pool: &PgPool,
    tenant: TenantId,
    calendar_id: AcademicCalendarId,
    status: Option<PublicationStatus>,
) -> R<Vec<CalendarVersion>> {
    let mut conn = pool.acquire().await.map_err(db)?;
    calendar_in(&mut conn, tenant, calendar_id.as_uuid()).await?;
    let rows = sqlx::query(&format!(
        "{VERSION_SELECT} WHERE v.tenant_id = $1 AND v.calendar_id = $2 AND ($3::text IS NULL OR v.status = $3) \
         ORDER BY v.version_no"
    ))
    .bind(tenant.as_uuid())
    .bind(calendar_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    let mut v = rows.iter().map(version_row).collect::<R<Vec<_>>>()?;
    attach_children(&mut conn, &mut v).await?;
    Ok(v)
}

#[allow(clippy::too_many_arguments)]
async fn insert_version(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution_id: InstitutionId,
    calendar_id: Uuid,
    year_id: Uuid,
    starts_on: NaiveDate,
    ends_on: NaiveDate,
    notes: Option<String>,
    supersedes: Option<Uuid>,
) -> R<Uuid> {
    let id = CalendarVersionId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.calendar_version (id, tenant_id, institution_id, calendar_id, academic_year_id, version_no, starts_on, ends_on, notes, supersedes_id) \
         VALUES ($1, $2, $3, $4, $5, (SELECT COALESCE(max(version_no), 0) + 1 FROM institution.calendar_version WHERE calendar_id = $4), $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(institution_id.as_uuid())
    .bind(calendar_id)
    .bind(year_id)
    .bind(starts_on)
    .bind(ends_on)
    .bind(notes)
    .bind(supersedes)
    .execute(&mut *conn)
    .await
    .map_err(|e| match db(e) {
        KernelError::Conflict(_) => KernelError::Conflict(
            "this academic year already has a draft calendar version; edit or discard it".into(),
        ),
        other => other,
    })?;
    Ok(id)
}

/// Copies terms + key dates from `from` into `to`, shifting by `shift` days within `limit`.
async fn copy_structure(
    conn: &mut PgConnection,
    tenant: TenantId,
    institution_id: InstitutionId,
    from: Uuid,
    to: Uuid,
    shift: i64,
    limit: (NaiveDate, NaiveDate),
) -> R<()> {
    let terms: Vec<(String, String, i32, NaiveDate, NaiveDate)> = sqlx::query_as(
        "SELECT code, name, sequence, starts_on, ends_on FROM institution.academic_term WHERE calendar_version_id = $1",
    )
    .bind(from)
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    for (code, name, seq, s, e) in terms {
        if let Some((s, e)) = shift_into(s, e, shift, limit).filter(|(s, e)| e > s) {
            sqlx::query(
                "INSERT INTO institution.academic_term (tenant_id, institution_id, calendar_version_id, code, name, sequence, starts_on, ends_on) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
            )
            .bind(tenant.as_uuid())
            .bind(institution_id.as_uuid())
            .bind(to)
            .bind(code)
            .bind(name)
            .bind(seq)
            .bind(s)
            .bind(e)
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        }
    }
    let keys: Vec<(String, String, NaiveDate, NaiveDate)> = sqlx::query_as(
        "SELECT kind, name, starts_on, ends_on FROM institution.calendar_key_date WHERE calendar_version_id = $1",
    )
    .bind(from)
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    for (kind, name, s, e) in keys {
        if let Some((s, e)) = shift_into(s, e, shift, limit) {
            sqlx::query(
                "INSERT INTO institution.calendar_key_date (tenant_id, institution_id, calendar_version_id, kind, name, starts_on, ends_on) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7)",
            )
            .bind(tenant.as_uuid())
            .bind(institution_id.as_uuid())
            .bind(to)
            .bind(kind)
            .bind(name)
            .bind(s)
            .bind(e)
            .execute(&mut *conn)
            .await
            .map_err(db)?;
        }
    }
    Ok(())
}

pub async fn create_version(
    pool: &PgPool,
    tenant: TenantId,
    calendar_id: AcademicCalendarId,
    cmd: NewCalendarVersion,
    actor: &Actor,
) -> R<CalendarVersion> {
    let mut tx = pool.begin().await.map_err(db)?;
    let cal = calendar_in(&mut tx, tenant, calendar_id.as_uuid()).await?;
    let scope = ExecutionScope::institution(tenant, cal.institution_id);
    guard::lock_institution_for_change(&mut tx, &scope, cal.institution_id).await?;
    require_live_calendar(&mut tx, tenant, calendar_id.as_uuid()).await?;
    let y = year_in(&mut tx, tenant, cmd.academic_year_id.as_uuid())
        .await
        .map_err(|_| {
            KernelError::Validation(vec![FieldViolation::new(
                "academicYearId",
                "not-found",
                "unknown academic year",
            )])
        })?;
    if y.calendar_id != calendar_id {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "academicYearId",
            "foreign",
            "the academic year belongs to another calendar",
        )]));
    }
    if y.status == AcademicYearStatus::Closed {
        return Err(KernelError::Conflict("the academic year is closed".into()));
    }
    let published: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.calendar_version WHERE academic_year_id = $1 AND status = 'published'",
    )
    .bind(y.id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    if published > 0 {
        return Err(KernelError::Conflict(
            "this academic year already has a published version; revise it to create a successor"
                .into(),
        ));
    }
    let starts = cmd.starts_on.unwrap_or(y.starts_on);
    let ends = cmd.ends_on.unwrap_or(y.ends_on);
    let mut v = vec![];
    if ends <= starts {
        v.push(FieldViolation::new(
            "endsOn",
            "range",
            "the version must end after it starts",
        ));
    }
    if starts < y.starts_on || ends > y.ends_on {
        v.push(FieldViolation::new(
            "startsOn",
            "outside-year",
            "the version must lie inside its academic year",
        ));
    }
    if !v.is_empty() {
        return Err(KernelError::Validation(v));
    }
    let id = insert_version(
        &mut tx,
        tenant,
        cal.institution_id,
        calendar_id.as_uuid(),
        y.id.as_uuid(),
        starts,
        ends,
        trimmed(cmd.notes.as_deref()),
        None,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "calendar_version.create",
        "calendar_version",
        id,
        json!({ "calendarId": calendar_id, "academicYearId": y.id }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Locks a version that must be an editable draft.
async fn lock_draft(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
) -> R<Locked> {
    let l = lock(
        conn,
        "calendar_version",
        "calendar version",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    if l.status != "draft" {
        return Err(KernelError::Conflict(format!(
            "calendar version is {}; only a draft can be edited (revise a published version to change it)",
            l.status
        )));
    }
    Ok(l)
}

async fn bump(conn: &mut PgConnection, id: CalendarVersionId) -> R<()> {
    sqlx::query("UPDATE institution.calendar_version SET version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id.as_uuid())
        .execute(&mut *conn)
        .await
        .map_err(db)?;
    Ok(())
}

pub async fn update_version(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
    cmd: UpdateCalendarVersion,
    actor: &Actor,
) -> R<CalendarVersion> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock_draft(&mut tx, tenant, id, expected).await?;
    let cur = version_in(&mut tx, tenant, id.as_uuid()).await?;
    let y = year_in(&mut tx, tenant, cur.academic_year_id.as_uuid()).await?;
    let starts = cmd.starts_on.unwrap_or(cur.starts_on);
    let ends = cmd.ends_on.unwrap_or(cur.ends_on);
    if ends <= starts || starts < y.starts_on || ends > y.ends_on {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "startsOn",
            "range",
            format!(
                "the version must end after it starts and lie inside {} – {}",
                y.starts_on, y.ends_on
            ),
        )]));
    }
    let notes = match cmd.notes.as_deref() {
        Some(n) => trimmed(Some(n)),
        None => cur.notes.clone(),
    };
    sqlx::query(
        "UPDATE institution.calendar_version SET starts_on = $2, ends_on = $3, notes = $4, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(starts)
    .bind(ends)
    .bind(&notes)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.modify",
        "calendar_version",
        id.as_uuid(),
        json!({ "startsOn": starts, "endsOn": ends }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

pub async fn add_term(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
    cmd: NewTerm,
    actor: &Actor,
) -> R<CalendarVersion> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock_draft(&mut tx, tenant, id, expected).await?;
    sqlx::query(
        "INSERT INTO institution.academic_term (tenant_id, institution_id, calendar_version_id, code, name, sequence, starts_on, ends_on) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(tenant.as_uuid())
    .bind(l.institution_id.as_uuid())
    .bind(id.as_uuid())
    .bind(cmd.code.trim())
    .bind(cmd.name.trim())
    .bind(cmd.sequence)
    .bind(cmd.starts_on)
    .bind(cmd.ends_on)
    .execute(&mut *tx)
    .await
    .map_err(|e| match db(e) {
        KernelError::Conflict(_) => KernelError::Conflict(format!("term {} already exists in this version", cmd.code.trim())),
        other => other,
    })?;
    bump(&mut tx, id).await?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.add_term",
        "calendar_version",
        id.as_uuid(),
        json!({ "code": cmd.code.trim(), "startsOn": cmd.starts_on, "endsOn": cmd.ends_on }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

pub async fn remove_term(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    term: AcademicTermId,
    expected: Version,
    actor: &Actor,
) -> R<CalendarVersion> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock_draft(&mut tx, tenant, id, expected).await?;
    let n = sqlx::query("DELETE FROM institution.academic_term WHERE id = $1 AND calendar_version_id = $2 AND tenant_id = $3")
        .bind(term.as_uuid())
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected();
    if n == 0 {
        return Err(KernelError::NotFound {
            entity: "academic term",
        });
    }
    bump(&mut tx, id).await?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.remove_term",
        "calendar_version",
        id.as_uuid(),
        json!({ "termId": term }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

pub async fn add_key_date(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
    cmd: NewKeyDate,
    actor: &Actor,
) -> R<CalendarVersion> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock_draft(&mut tx, tenant, id, expected).await?;
    sqlx::query(
        "INSERT INTO institution.calendar_key_date (tenant_id, institution_id, calendar_version_id, kind, name, starts_on, ends_on) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(tenant.as_uuid())
    .bind(l.institution_id.as_uuid())
    .bind(id.as_uuid())
    .bind(cmd.kind.as_str())
    .bind(cmd.name.trim())
    .bind(cmd.starts_on)
    .bind(cmd.ends())
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    bump(&mut tx, id).await?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.add_key_date",
        "calendar_version",
        id.as_uuid(),
        json!({ "kind": cmd.kind.as_str(), "startsOn": cmd.starts_on }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

pub async fn remove_key_date(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    key: KeyDateId,
    expected: Version,
    actor: &Actor,
) -> R<CalendarVersion> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock_draft(&mut tx, tenant, id, expected).await?;
    let n = sqlx::query("DELETE FROM institution.calendar_key_date WHERE id = $1 AND calendar_version_id = $2 AND tenant_id = $3")
        .bind(key.as_uuid())
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(db)?
        .rows_affected();
    if n == 0 {
        return Err(KernelError::NotFound { entity: "key date" });
    }
    bump(&mut tx, id).await?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.remove_key_date",
        "calendar_version",
        id.as_uuid(),
        json!({ "keyDateId": key }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

fn definition_errors(v: &CalendarVersion, y: &AcademicYear) -> Vec<FieldViolation> {
    let terms: Vec<_> = v
        .terms
        .iter()
        .map(|t| (t.code.clone(), t.sequence, t.starts_on, t.ends_on))
        .collect();
    let keys: Vec<_> = v
        .key_dates
        .iter()
        .map(|k| (k.name.clone(), k.starts_on, k.ends_on))
        .collect();
    validate_definition(
        v.starts_on,
        v.ends_on,
        (y.starts_on, y.ends_on),
        &terms,
        &keys,
    )
}

/// Draft → InReview (approval step) → Published (L3-B-027 / B-029). The institution timezone is
/// snapshotted into the version; a predecessor is superseded in the same transaction; existing
/// assignments are **not** migrated (L4-B-029-BH-005).
pub async fn publish_version(
    st: &InstitutionState,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> R<CalendarVersion> {
    let (scope, status, version) = peek(
        &st.pool,
        "calendar_version",
        "calendar version",
        tenant,
        id.as_uuid(),
    )
    .await?;
    if version != expected.0 {
        return Err(KernelError::VersionConflict {
            expected: expected.0,
            actual: version,
        });
    }
    let from = pub_status(&status)?;
    if !version_can_transition(from, PublicationStatus::InReview) {
        return Err(illegal(
            "calendar version",
            from,
            PublicationStatus::InReview,
        ));
    }
    {
        let mut conn = st.pool.acquire().await.map_err(db)?;
        let v = version_in(&mut conn, tenant, id.as_uuid()).await?;
        let y = year_in(&mut conn, tenant, v.academic_year_id.as_uuid()).await?;
        let errors = definition_errors(&v, &y);
        if !errors.is_empty() {
            return Err(KernelError::Validation(errors));
        }
    }
    let outcome = approval::request_approval(
        st,
        &scope,
        process::CALENDAR_PUBLICATION,
        approval::business_key("calendar_version", id, version),
        BTreeMap::from([("calendarVersionId".to_owned(), json!(id))]),
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_version",
        "calendar version",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    if l.status != "draft" {
        return Err(illegal("calendar version", &l.status, "in_review"));
    }
    let cal_id = version_in(&mut tx, tenant, id.as_uuid()).await?.calendar_id;
    require_live_calendar(&mut tx, tenant, cal_id.as_uuid()).await?;
    let pid = outcome.process_instance_id().to_owned();
    sqlx::query(
        "UPDATE institution.calendar_version SET status = 'in_review', process_instance_id = $2, status_reason = $3, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(&pid)
    .bind(trimmed(reason.as_deref()))
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.submit",
        "calendar_version",
        id.as_uuid(),
        json!({ "processInstanceId": pid }),
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        apply_publication(
            &mut tx,
            tenant,
            &l.scope,
            id,
            Some(pid),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

async fn apply_publication(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    id: CalendarVersionId,
    pid: Option<String>,
    decider: &Actor,
) -> R<()> {
    let v = version_in(conn, tenant, id.as_uuid()).await?;
    // Supersede the governing predecessor of the same year (successor publication).
    let superseded: Vec<(Uuid, i64)> = sqlx::query_as(
        "UPDATE institution.calendar_version SET status = 'superseded', version = version + 1, updated_at = now() \
         WHERE calendar_id = $1 AND academic_year_id = $2 AND status = 'published' AND id <> $3 RETURNING id, version",
    )
    .bind(v.calendar_id.as_uuid())
    .bind(v.academic_year_id.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    for (sid, _) in &superseded {
        audit(
            conn,
            scope,
            "calendar_version.supersede",
            "calendar_version",
            *sid,
            json!({ "successorId": id }),
            decider,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE institution.calendar_version v SET status = 'published', published_at = now(), timezone = i.timezone, \
         version = v.version + 1, updated_at = now() FROM institution.institution i WHERE v.id = $1 AND i.id = v.institution_id",
    )
    .bind(id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    sqlx::query(
        "UPDATE institution.academic_calendar SET status = 'published', version = version + 1, updated_at = now() WHERE id = $1 AND status = 'draft'",
    )
    .bind(v.calendar_id.as_uuid())
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    let p = version_in(conn, tenant, id.as_uuid()).await?;
    publish::event(
        conn,
        scope,
        CALENDAR_VERSION_PUBLISHED,
        CALENDAR_VERSION_PUBLISHED_SCHEMA,
        id.to_string(),
        &CalendarVersionPublishedV1 {
            calendar_id: p.calendar_id.as_uuid(),
            calendar_version_id: id.as_uuid(),
            institution_id: p.institution_id.as_uuid(),
            academic_year_id: p.academic_year_id.as_uuid(),
            version_no: p.version_no,
            supersedes_version_id: p.supersedes_version_id.map(|s| s.as_uuid()),
            starts_on: p.starts_on,
            ends_on: p.ends_on,
            timezone: p.timezone.clone(),
            process_instance_id: pid.clone(),
        },
        decider,
    )
    .await?;
    audit(
        conn,
        scope,
        "calendar_version.publish",
        "calendar_version",
        id.as_uuid(),
        json!({ "versionNo": p.version_no, "timezone": p.timezone, "superseded": superseded.iter().map(|s| s.0).collect::<Vec<_>>(), "processInstanceId": pid }),
        decider,
    )
    .await
}

/// Explicit publication decision for an `in_review` version (`AwaitDecision` mode).
pub async fn decide_version(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
    d: Decision,
    actor: &Actor,
) -> R<CalendarVersion> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_version",
        "calendar version",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    if l.status != "in_review" {
        return Err(KernelError::Conflict(format!(
            "calendar version is {}; nothing awaits a decision",
            l.status
        )));
    }
    let pid: Option<String> = sqlx::query_scalar(
        "SELECT process_instance_id FROM institution.calendar_version WHERE id = $1",
    )
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    match d.decision {
        DecisionKind::Approve => {
            let v = version_in(&mut tx, tenant, id.as_uuid()).await?;
            let y = year_in(&mut tx, tenant, v.academic_year_id.as_uuid()).await?;
            let errors = definition_errors(&v, &y);
            if !errors.is_empty() {
                return Err(KernelError::Validation(errors));
            }
            apply_publication(&mut tx, tenant, &l.scope, id, pid, actor).await?;
        }
        DecisionKind::Reject => {
            sqlx::query(
                "UPDATE institution.calendar_version SET status = 'draft', status_reason = $2, version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id.as_uuid())
            .bind(trimmed(d.reason.as_deref()))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            audit(
                &mut tx,
                &l.scope,
                "calendar_version.return",
                "calendar_version",
                id.as_uuid(),
                json!({ "reason": trimmed(d.reason.as_deref()) }),
                actor,
            )
            .await?;
        }
    }
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Successor revision (L3-B-029): a new draft under the same calendar identity, copying the
/// published version's period, terms and key dates, `supersedesVersionId` = the predecessor.
pub async fn revise_version(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
    notes: Option<String>,
    actor: &Actor,
) -> R<CalendarVersion> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_version",
        "calendar version",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    if l.status != "published" {
        return Err(KernelError::Conflict(format!(
            "only the published version can be revised (this one is {})",
            l.status
        )));
    }
    let base = version_in(&mut tx, tenant, id.as_uuid()).await?;
    require_live_calendar(&mut tx, tenant, base.calendar_id.as_uuid()).await?;
    let new_id = insert_version(
        &mut tx,
        tenant,
        l.institution_id,
        base.calendar_id.as_uuid(),
        base.academic_year_id.as_uuid(),
        base.starts_on,
        base.ends_on,
        trimmed(notes.as_deref())
            .or_else(|| Some(format!("Revision of version {}", base.version_no))),
        Some(id.as_uuid()),
    )
    .await?;
    copy_structure(
        &mut tx,
        tenant,
        l.institution_id,
        id.as_uuid(),
        new_id,
        0,
        (base.starts_on, base.ends_on),
    )
    .await?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.revise",
        "calendar_version",
        new_id,
        json!({ "supersedesVersionId": id }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, new_id).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Retires a version (L3-B-033): published → retired (or discards a draft). Refused while an
/// effective assignment still governs through it (BH-002) or when it is the only governing
/// version of an open academic year (no resolution gap). Draft assignments on it are retired.
pub async fn retire_version(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarVersionId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> R<CalendarVersion> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_version",
        "calendar version",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = pub_status(&l.status)?;
    if !version_can_transition(from, PublicationStatus::Retired) {
        return Err(illegal(
            "calendar version",
            from,
            PublicationStatus::Retired,
        ));
    }
    let effective: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM institution.calendar_assignment WHERE calendar_version_id = $1 AND status = 'effective'",
    )
    .bind(id.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    if effective > 0 {
        return Err(KernelError::Conflict(format!(
            "{effective} effective assignment(s) still govern through this version; retire them or assign another version first"
        )));
    }
    if from == PublicationStatus::Published {
        let open: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM institution.academic_year y JOIN institution.calendar_version v ON v.academic_year_id = y.id \
             WHERE v.id = $1 AND y.status IN ('open', 'closing')",
        )
        .bind(id.as_uuid())
        .fetch_one(&mut *tx)
        .await
        .map_err(db)?;
        if open > 0 {
            return Err(KernelError::Conflict(
                "this version governs an open academic year; close the year or publish a successor first".into(),
            ));
        }
    }
    let reason = trimmed(reason.as_deref());
    sqlx::query(
        "UPDATE institution.calendar_assignment SET status = 'retired', retired_at = now(), status_reason = 'calendar version retired', \
         version = version + 1, updated_at = now() WHERE calendar_version_id = $1 AND status = 'draft'",
    )
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    let new_version: i64 = sqlx::query_scalar(
        "UPDATE institution.calendar_version SET status = 'retired', retired_at = now(), status_reason = $2, version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING version",
    )
    .bind(id.as_uuid())
    .bind(&reason)
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    if from == PublicationStatus::Published {
        status_event(
            &mut tx,
            &l.scope,
            CALENDAR_VERSION_RETIRED,
            CALENDAR_VERSION_RETIRED_SCHEMA,
            id.as_uuid(),
            from.as_str(),
            "retired",
            new_version,
            reason.clone(),
            None,
            actor,
        )
        .await?;
    }
    audit(
        &mut tx,
        &l.scope,
        "calendar_version.retire",
        "calendar_version",
        id.as_uuid(),
        json!({ "fromStatus": from.as_str(), "reason": reason }),
        actor,
    )
    .await?;
    let out = version_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Assignments
// ---------------------------------------------------------------------------------------------

async fn assignment_in(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> R<CalendarAssignment> {
    let row = sqlx::query(&format!(
        "{} WHERE a.id = $1 AND a.tenant_id = $2",
        assignment_select()
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "calendar assignment",
    })?;
    assignment_row(&row)
}

pub async fn get_assignment(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarAssignmentId,
) -> R<CalendarAssignment> {
    let mut conn = pool.acquire().await.map_err(db)?;
    assignment_in(&mut conn, tenant, id.as_uuid()).await
}

pub async fn list_assignments(
    pool: &PgPool,
    tenant: TenantId,
    calendar_id: Option<AcademicCalendarId>,
    institution_id: Option<InstitutionId>,
    status: Option<CalendarAssignmentStatus>,
) -> R<Vec<CalendarAssignment>> {
    let rows = sqlx::query(&format!(
        "{} WHERE a.tenant_id = $1 AND ($2::uuid IS NULL OR a.calendar_id = $2) AND ($3::uuid IS NULL OR a.institution_id = $3) \
         AND ($4::text IS NULL OR a.status = $4) ORDER BY a.effective_from, a.created_at",
        assignment_select()
    ))
    .bind(tenant.as_uuid())
    .bind(calendar_id.map(|c| c.as_uuid()))
    .bind(institution_id.map(|i| i.as_uuid()))
    .bind(status.map(|s| s.as_str()))
    .fetch_all(pool)
    .await
    .map_err(db)?;
    rows.iter().map(assignment_row).collect()
}

/// Whether the referenced governing scope exists in this institution. Campus / unit /
/// programme tables belong to 2A/2B and are only **read**; when the owning slice's table is not
/// deployed the scope cannot be validated and is refused.
async fn scope_exists(
    conn: &mut PgConnection,
    tenant: TenantId,
    inst: InstitutionId,
    kind: ScopeKind,
    id: Uuid,
) -> R<bool> {
    let Some(table) = kind.table() else {
        return Ok(id == inst.as_uuid());
    };
    let deployed: Option<String> = sqlx::query_scalar("SELECT to_regclass($1)::text")
        .bind(table)
        .fetch_one(&mut *conn)
        .await
        .map_err(db)?;
    if deployed.is_none() {
        return Ok(false);
    }
    sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {table} WHERE id = $1 AND tenant_id = $2 AND institution_id = $3)"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(inst.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(db)
}

pub async fn create_assignment(
    pool: &PgPool,
    tenant: TenantId,
    calendar_id: AcademicCalendarId,
    cmd: NewAssignment,
    actor: &Actor,
) -> R<CalendarAssignment> {
    cmd.validate()?;
    let mut tx = pool.begin().await.map_err(db)?;
    let cal = calendar_in(&mut tx, tenant, calendar_id.as_uuid()).await?;
    let scope = ExecutionScope::institution(tenant, cal.institution_id);
    guard::lock_institution_for_change(&mut tx, &scope, cal.institution_id).await?;
    require_live_calendar(&mut tx, tenant, calendar_id.as_uuid()).await?;
    let v = version_in(&mut tx, tenant, cmd.calendar_version_id.as_uuid())
        .await
        .map_err(|_| {
            KernelError::Validation(vec![FieldViolation::new(
                "calendarVersionId",
                "not-found",
                "unknown calendar version",
            )])
        })?;
    if v.calendar_id != calendar_id {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "calendarVersionId",
            "foreign",
            "the version belongs to another calendar",
        )]));
    }
    if matches!(
        v.status,
        PublicationStatus::Retired | PublicationStatus::Superseded
    ) {
        return Err(KernelError::Conflict(format!(
            "calendar version {} is {}; it accepts no new assignment",
            v.version_no, v.status
        )));
    }
    let scope_id = cmd.scope_id.unwrap_or(cal.institution_id.as_uuid());
    if !scope_exists(
        &mut tx,
        tenant,
        cal.institution_id,
        cmd.scope_kind,
        scope_id,
    )
    .await?
    {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "scopeId",
            "not-found",
            format!(
                "no {} with this id in the institution",
                cmd.scope_kind.as_str()
            ),
        )]));
    }
    if cmd.effective_from > v.ends_on {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "effectiveFrom",
            "range",
            format!(
                "the assignment would start after the version ends ({})",
                v.ends_on
            ),
        )]));
    }
    let id = CalendarAssignmentId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.calendar_assignment (id, tenant_id, institution_id, calendar_id, calendar_version_id, scope_kind, scope_id, effective_from, effective_to) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(cal.institution_id.as_uuid())
    .bind(calendar_id.as_uuid())
    .bind(v.id.as_uuid())
    .bind(cmd.scope_kind.as_str())
    .bind(scope_id)
    .bind(cmd.effective_from)
    .bind(cmd.effective_to)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    audit(&mut tx, &scope, "calendar_assignment.create", "calendar_assignment", id, json!({ "calendarVersionId": v.id, "scopeKind": cmd.scope_kind.as_str(), "scopeId": scope_id, "effectiveFrom": cmd.effective_from, "effectiveTo": cmd.effective_to }), actor).await?;
    let out = assignment_in(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Draft → Effective (L3-B-028): the version must be published, the scope must exist and no
/// other effective assignment may overlap the same scope (BH-005). It governs from
/// `effectiveFrom` (business date) onwards — `governing` tells whether it does today.
pub async fn activate_assignment(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarAssignmentId,
    expected: Version,
    actor: &Actor,
) -> R<CalendarAssignment> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_assignment",
        "calendar assignment",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = CalendarAssignmentStatus::parse(&l.status).ok_or_else(|| bad_status(&l.status))?;
    if !assignment_can_transition(from, CalendarAssignmentStatus::Effective) {
        return Err(illegal(
            "calendar assignment",
            from,
            CalendarAssignmentStatus::Effective,
        ));
    }
    let a = assignment_in(&mut tx, tenant, id.as_uuid()).await?;
    require_live_calendar(&mut tx, tenant, a.calendar_id.as_uuid()).await?;
    let v = version_in(&mut tx, tenant, a.calendar_version_id.as_uuid()).await?;
    if v.status != PublicationStatus::Published {
        return Err(KernelError::Conflict(format!(
            "calendar version {} is {}; only a published version can govern a scope",
            v.version_no, v.status
        )));
    }
    if !scope_exists(&mut tx, tenant, a.institution_id, a.scope_kind, a.scope_id).await? {
        return Err(KernelError::Conflict(
            "the governing scope no longer exists".into(),
        ));
    }
    // Serialise activations per scope, then check for overlapping effective assignments.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(format!(
            "calendar_assignment:{}:{}:{}",
            tenant,
            a.scope_kind.as_str(),
            a.scope_id
        ))
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    let clash: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.calendar_assignment WHERE tenant_id = $1 AND scope_kind = $2 AND scope_id = $3 \
         AND status = 'effective' AND id <> $4 AND effective_from <= COALESCE($6, 'infinity'::date) \
         AND COALESCE(effective_to, 'infinity'::date) >= $5 LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(a.scope_kind.as_str())
    .bind(a.scope_id)
    .bind(id.as_uuid())
    .bind(a.effective_from)
    .bind(a.effective_to)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db)?;
    if let Some(other) = clash {
        return Err(KernelError::Conflict(format!(
            "another effective assignment ({other}) already governs this scope in an overlapping period; retire or end it first"
        )));
    }
    sqlx::query(
        "UPDATE institution.calendar_assignment SET status = 'effective', activated_at = now(), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    let out = assignment_in(&mut tx, tenant, id.as_uuid()).await?;
    publish::event(
        &mut tx,
        &l.scope,
        CALENDAR_ASSIGNMENT_EFFECTIVE,
        CALENDAR_ASSIGNMENT_EFFECTIVE_SCHEMA,
        id.to_string(),
        &CalendarAssignmentV1 {
            assignment_id: id.as_uuid(),
            calendar_id: out.calendar_id.as_uuid(),
            calendar_version_id: out.calendar_version_id.as_uuid(),
            institution_id: out.institution_id.as_uuid(),
            scope_kind: match out.scope_kind {
                ScopeKind::Institution => CalendarScopeKindV1::Institution,
                ScopeKind::Campus => CalendarScopeKindV1::Campus,
                ScopeKind::OrgUnit => CalendarScopeKindV1::OrgUnit,
                ScopeKind::Programme => CalendarScopeKindV1::Programme,
            },
            scope_id: out.scope_id,
            effective_from: out.effective_from,
            effective_to: out.effective_to,
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_assignment.activate",
        "calendar_assignment",
        id.as_uuid(),
        json!({ "fromStatus": from.as_str(), "toStatus": "effective" }),
        actor,
    )
    .await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Effective | Draft → Retired; the row stays for historical resolution (BH-007).
pub async fn retire_assignment(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarAssignmentId,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> R<CalendarAssignment> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_assignment",
        "calendar assignment",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = CalendarAssignmentStatus::parse(&l.status).ok_or_else(|| bad_status(&l.status))?;
    if !assignment_can_transition(from, CalendarAssignmentStatus::Retired) {
        return Err(illegal(
            "calendar assignment",
            from,
            CalendarAssignmentStatus::Retired,
        ));
    }
    let reason = trimmed(reason.as_deref());
    // An effective assignment that already governed ends today (business date) at the latest.
    let (today, _) = business_today(&mut tx, tenant, l.institution_id).await?;
    let new_version: i64 = sqlx::query_scalar(
        "UPDATE institution.calendar_assignment SET status = 'retired', retired_at = now(), status_reason = $2, \
         effective_to = CASE WHEN status = 'effective' AND effective_from <= $3 THEN LEAST(COALESCE(effective_to, $3), $3) ELSE effective_to END, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING version",
    )
    .bind(id.as_uuid())
    .bind(&reason)
    .bind(today)
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    status_event(
        &mut tx,
        &l.scope,
        CALENDAR_ASSIGNMENT_RETIRED,
        CALENDAR_ASSIGNMENT_RETIRED_SCHEMA,
        id.as_uuid(),
        from.as_str(),
        "retired",
        new_version,
        reason.clone(),
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &l.scope,
        "calendar_assignment.retire",
        "calendar_assignment",
        id.as_uuid(),
        json!({ "fromStatus": from.as_str(), "reason": reason }),
        actor,
    )
    .await?;
    let out = assignment_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Which calendar version governs `scope` on `on` (default: today in the institution timezone).
/// Historical dates resolve against the assignment effective then, including retired ones that
/// governed in that period (L4-B-029-BH-007).
pub async fn resolve(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    kind: ScopeKind,
    scope_id: Option<Uuid>,
    on: Option<NaiveDate>,
) -> R<CalendarResolution> {
    let mut conn = pool.acquire().await.map_err(db)?;
    let (today, tz) = business_today(&mut conn, scope.tenant_id, institution_id).await?;
    let on = on.unwrap_or(today);
    let scope_id = scope_id.unwrap_or(institution_id.as_uuid());
    let mut candidates = vec![(kind, scope_id)];
    if kind != ScopeKind::Institution {
        candidates.push((ScopeKind::Institution, institution_id.as_uuid()));
    }
    let mut found = None;
    for (k, sid) in candidates {
        let row = sqlx::query(&format!(
            "{} WHERE a.tenant_id = $1 AND a.institution_id = $2 AND a.scope_kind = $3 AND a.scope_id = $4 \
             AND a.activated_at IS NOT NULL AND a.status IN ('effective', 'retired') \
             AND a.effective_from <= $5 AND COALESCE(a.effective_to, 'infinity'::date) >= $5 \
             AND (a.status = 'effective' OR a.effective_to IS NOT NULL) \
             ORDER BY (a.status = 'effective') DESC, a.effective_from DESC LIMIT 1",
            assignment_select()
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(k.as_str())
        .bind(sid)
        .bind(on)
        .fetch_optional(&mut *conn)
        .await
        .map_err(db)?;
        if let Some(r) = row {
            found = Some((k, assignment_row(&r)?));
            break;
        }
    }
    let (resolved_via, assignment, version, exceptions) = match found {
        Some((k, a)) => {
            let v = version_in(&mut conn, scope.tenant_id, a.calendar_version_id.as_uuid()).await?;
            let rows = sqlx::query(&format!(
                "{} WHERE e.tenant_id = $1 AND e.calendar_id = $2 AND e.status IN ('in_effect', 'completed') \
                 AND e.applied_at IS NOT NULL AND e.starts_on <= $3 AND e.ends_on >= $3 \
                 AND ((e.scope_kind = $4 AND e.scope_id = $5) OR e.scope_kind = 'institution') ORDER BY e.starts_on",
                exception_select()
            ))
            .bind(scope.tenant_id.as_uuid())
            .bind(a.calendar_id.as_uuid())
            .bind(on)
            .bind(kind.as_str())
            .bind(scope_id)
            .fetch_all(&mut *conn)
            .await
            .map_err(db)?;
            let ex = rows.iter().map(exception_row).collect::<R<Vec<_>>>()?;
            (Some(k), Some(a), Some(v), ex)
        }
        None => (None, None, None, vec![]),
    };
    Ok(CalendarResolution {
        scope_kind: kind,
        scope_id,
        on,
        timezone: tz,
        resolved_via,
        assignment,
        calendar_version: version,
        exceptions,
    })
}

// ---------------------------------------------------------------------------------------------
// Exceptions
// ---------------------------------------------------------------------------------------------

async fn exception_in(conn: &mut PgConnection, tenant: TenantId, id: Uuid) -> R<CalendarException> {
    let row = sqlx::query(&format!(
        "{} WHERE e.id = $1 AND e.tenant_id = $2",
        exception_select()
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "calendar exception",
    })?;
    exception_row(&row)
}

pub async fn get_exception(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarExceptionId,
) -> R<CalendarException> {
    let mut conn = pool.acquire().await.map_err(db)?;
    exception_in(&mut conn, tenant, id.as_uuid()).await
}

pub async fn list_exceptions(
    pool: &PgPool,
    tenant: TenantId,
    calendar_id: AcademicCalendarId,
    status: Option<CalendarExceptionStatus>,
) -> R<Vec<CalendarException>> {
    let mut conn = pool.acquire().await.map_err(db)?;
    calendar_in(&mut conn, tenant, calendar_id.as_uuid()).await?;
    let rows = sqlx::query(&format!(
        "{} WHERE e.tenant_id = $1 AND e.calendar_id = $2 AND ($3::text IS NULL OR e.status = $3) ORDER BY e.starts_on, e.created_at",
        exception_select()
    ))
    .bind(tenant.as_uuid())
    .bind(calendar_id.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    rows.iter().map(exception_row).collect()
}

/// Rejects an exception that overlaps an approved / applied one of the same kind and scope
/// (L4-B-030-BH-004).
async fn exception_overlap(conn: &mut PgConnection, e: &CalendarException) -> R<()> {
    let clash: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM institution.calendar_exception WHERE calendar_id = $1 AND id <> $2 AND kind = $3 \
         AND scope_kind = $4 AND scope_id = $5 AND status IN ('approved', 'in_effect') AND starts_on <= $7 AND ends_on >= $6 LIMIT 1",
    )
    .bind(e.calendar_id.as_uuid())
    .bind(e.id.as_uuid())
    .bind(e.kind.as_str())
    .bind(e.scope_kind.as_str())
    .bind(e.scope_id)
    .bind(e.starts_on)
    .bind(e.ends_on)
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?;
    match clash {
        Some(other) => Err(KernelError::Conflict(format!(
            "an approved {} exception ({other}) already covers this scope in an overlapping period",
            e.kind.as_str()
        ))),
        None => Ok(()),
    }
}

async fn approve_exception_in(
    conn: &mut PgConnection,
    tenant: TenantId,
    scope: &ExecutionScope,
    id: Uuid,
    pid: Option<String>,
    reason: Option<String>,
    decider: &Actor,
) -> R<CalendarException> {
    let e = exception_in(conn, tenant, id).await?;
    exception_overlap(conn, &e).await?;
    sqlx::query(
        "UPDATE institution.calendar_exception SET status = 'approved', decided_at = now(), decision_reason = $2, \
         process_instance_id = COALESCE($3, process_instance_id), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id)
    .bind(&reason)
    .bind(&pid)
    .execute(&mut *conn)
    .await
    .map_err(db)?;
    let e = exception_in(conn, tenant, id).await?;
    publish::event(
        conn,
        scope,
        CALENDAR_EXCEPTION_APPROVED,
        CALENDAR_EXCEPTION_APPROVED_SCHEMA,
        id.to_string(),
        &CalendarExceptionV1 {
            exception_id: id,
            calendar_id: e.calendar_id.as_uuid(),
            institution_id: e.institution_id.as_uuid(),
            kind: e.kind.as_str().to_owned(),
            starts_on: e.starts_on,
            ends_on: e.ends_on,
            reason: e.reason.clone(),
            process_instance_id: e.process_instance_id.clone(),
        },
        decider,
    )
    .await?;
    audit(
        conn,
        scope,
        "calendar_exception.approve",
        "calendar_exception",
        id,
        json!({ "processInstanceId": e.process_instance_id, "reason": reason }),
        decider,
    )
    .await?;
    Ok(e)
}

/// Requests a temporary deviation (L3-B-030) and runs its approval step: Requested → Approved
/// (auto-approval in Batch 2). The base version is never modified (BH-002).
pub async fn create_exception(
    st: &InstitutionState,
    tenant: TenantId,
    calendar_id: AcademicCalendarId,
    cmd: NewException,
    actor: &Actor,
) -> R<CalendarException> {
    cmd.validate()?;
    let cal = get_calendar(&st.pool, tenant, calendar_id).await?;
    let scope = ExecutionScope::institution(tenant, cal.institution_id);
    let id = CalendarExceptionId::new();
    let mut tx = st.pool.begin().await.map_err(db)?;
    guard::lock_institution_for_change(&mut tx, &scope, cal.institution_id).await?;
    require_live_calendar(&mut tx, tenant, calendar_id.as_uuid()).await?;
    let v = version_in(&mut tx, tenant, cmd.calendar_version_id.as_uuid())
        .await
        .map_err(|_| {
            KernelError::Validation(vec![FieldViolation::new(
                "calendarVersionId",
                "not-found",
                "unknown calendar version",
            )])
        })?;
    if v.calendar_id != calendar_id {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "calendarVersionId",
            "foreign",
            "the version belongs to another calendar",
        )]));
    }
    if v.status != PublicationStatus::Published {
        return Err(KernelError::Conflict(format!(
            "an exception needs a published base version (version {} is {})",
            v.version_no, v.status
        )));
    }
    if cmd.starts_on < v.starts_on || cmd.ends_on > v.ends_on {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "startsOn",
            "outside-version",
            format!(
                "the exception must lie inside the base version ({} – {})",
                v.starts_on, v.ends_on
            ),
        )]));
    }
    let kind = cmd.scope_kind.unwrap_or(ScopeKind::Institution);
    let scope_id = cmd.scope_id.unwrap_or(cal.institution_id.as_uuid());
    if !scope_exists(&mut tx, tenant, cal.institution_id, kind, scope_id).await? {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "scopeId",
            "not-found",
            format!("no {} with this id in the institution", kind.as_str()),
        )]));
    }
    let owner = actor.id.clone().unwrap_or_else(|| "unknown".into());
    sqlx::query(
        "INSERT INTO institution.calendar_exception (id, tenant_id, institution_id, calendar_id, calendar_version_id, kind, scope_kind, scope_id, starts_on, ends_on, reason, owner_subject) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
    )
    .bind(id.as_uuid())
    .bind(tenant.as_uuid())
    .bind(cal.institution_id.as_uuid())
    .bind(calendar_id.as_uuid())
    .bind(v.id.as_uuid())
    .bind(cmd.kind.as_str())
    .bind(kind.as_str())
    .bind(scope_id)
    .bind(cmd.starts_on)
    .bind(cmd.ends_on)
    .bind(cmd.reason.trim())
    .bind(&owner)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    let e = exception_in(&mut tx, tenant, id.as_uuid()).await?;
    exception_overlap(&mut tx, &e).await?;
    audit(&mut tx, &scope, "calendar_exception.request", "calendar_exception", id.as_uuid(), json!({ "kind": cmd.kind.as_str(), "startsOn": cmd.starts_on, "endsOn": cmd.ends_on, "scopeKind": kind.as_str() }), actor).await?;
    tx.commit().await.map_err(db)?;

    // Approval step (deterministic business key → a retried request reuses it).
    let outcome = approval::request_approval(
        st,
        &scope,
        process::CALENDAR_EXCEPTION,
        approval::business_key("calendar_exception", id, 1),
        BTreeMap::from([("calendarExceptionId".to_owned(), json!(id))]),
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_exception",
        "calendar exception",
        tenant,
        id.as_uuid(),
        None,
    )
    .await?;
    sqlx::query("UPDATE institution.calendar_exception SET process_instance_id = $2 WHERE id = $1")
        .bind(id.as_uuid())
        .bind(outcome.process_instance_id())
        .execute(&mut *tx)
        .await
        .map_err(db)?;
    let out = if let (
        ApprovalOutcome::Approved {
            process_instance_id,
        },
        "requested",
    ) = (&outcome, l.status.as_str())
    {
        approve_exception_in(
            &mut tx,
            tenant,
            &l.scope,
            id.as_uuid(),
            Some(process_instance_id.clone()),
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        exception_in(&mut tx, tenant, id.as_uuid()).await?
    };
    tx.commit().await.map_err(db)?;
    Ok(out)
}

pub async fn decide_exception(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarExceptionId,
    expected: Version,
    d: Decision,
    actor: &Actor,
) -> R<CalendarException> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_exception",
        "calendar exception",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = CalendarExceptionStatus::parse(&l.status).ok_or_else(|| bad_status(&l.status))?;
    let to = match d.decision {
        DecisionKind::Approve => CalendarExceptionStatus::Approved,
        DecisionKind::Reject => CalendarExceptionStatus::Rejected,
    };
    if !exception_can_transition(from, to) {
        return Err(illegal("calendar exception", from, to));
    }
    let out = match d.decision {
        DecisionKind::Approve => {
            approve_exception_in(
                &mut tx,
                tenant,
                &l.scope,
                id.as_uuid(),
                None,
                trimmed(d.reason.as_deref()),
                actor,
            )
            .await?
        }
        DecisionKind::Reject => {
            sqlx::query(
                "UPDATE institution.calendar_exception SET status = 'rejected', decided_at = now(), decision_reason = $2, version = version + 1, updated_at = now() WHERE id = $1",
            )
            .bind(id.as_uuid())
            .bind(trimmed(d.reason.as_deref()))
            .execute(&mut *tx)
            .await
            .map_err(db)?;
            audit(
                &mut tx,
                &l.scope,
                "calendar_exception.reject",
                "calendar_exception",
                id.as_uuid(),
                json!({ "reason": trimmed(d.reason.as_deref()) }),
                actor,
            )
            .await?;
            exception_in(&mut tx, tenant, id.as_uuid()).await?
        }
    };
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Approved → InEffect (applied) only inside its period in the institution's business timezone
/// (L4-B-030-BH-003/-005); InEffect | Approved → Completed (expiry, BH-006); Requested |
/// Approved → Withdrawn.
pub async fn transition_exception(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarExceptionId,
    expected: Version,
    to: CalendarExceptionStatus,
    reason: Option<String>,
    actor: &Actor,
) -> R<CalendarException> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_exception",
        "calendar exception",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = CalendarExceptionStatus::parse(&l.status).ok_or_else(|| bad_status(&l.status))?;
    if !exception_can_transition(from, to)
        || matches!(
            to,
            CalendarExceptionStatus::Approved | CalendarExceptionStatus::Rejected
        )
    {
        return Err(illegal("calendar exception", from, to));
    }
    let e = exception_in(&mut tx, tenant, id.as_uuid()).await?;
    let (today, tz) = business_today(&mut tx, tenant, l.institution_id).await?;
    let column = match to {
        CalendarExceptionStatus::InEffect => {
            if today < e.starts_on {
                return Err(KernelError::Conflict(format!(
                    "the exception starts on {} ({tz}); today there is {today}",
                    e.starts_on
                )));
            }
            if today > e.ends_on {
                return Err(KernelError::Conflict(format!(
                    "the exception expired on {} ({tz}); complete it instead",
                    e.ends_on
                )));
            }
            exception_overlap(&mut tx, &e).await?;
            "applied_at"
        }
        CalendarExceptionStatus::Completed => "completed_at",
        _ => "decided_at",
    };
    let new_version: i64 = sqlx::query_scalar(&format!(
        "UPDATE institution.calendar_exception SET status = $2, {column} = now(), \
         decision_reason = COALESCE($3, decision_reason), version = version + 1, updated_at = now() WHERE id = $1 RETURNING version"
    ))
    .bind(id.as_uuid())
    .bind(to.as_str())
    .bind(trimmed(reason.as_deref()))
    .fetch_one(&mut *tx)
    .await
    .map_err(db)?;
    if to == CalendarExceptionStatus::Completed {
        status_event(
            &mut tx,
            &l.scope,
            CALENDAR_EXCEPTION_COMPLETED,
            CALENDAR_EXCEPTION_COMPLETED_SCHEMA,
            id.as_uuid(),
            from.as_str(),
            to.as_str(),
            new_version,
            trimmed(reason.as_deref()),
            e.process_instance_id.clone(),
            actor,
        )
        .await?;
    }
    let verb = match to {
        CalendarExceptionStatus::InEffect => "apply",
        CalendarExceptionStatus::Completed => "complete",
        _ => "withdraw",
    };
    audit(&mut tx, &l.scope, &format!("calendar_exception.{verb}"), "calendar_exception", id.as_uuid(), json!({ "fromStatus": from.as_str(), "toStatus": to.as_str(), "businessDate": today, "timezone": tz }), actor).await?;
    let out = exception_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Simulations
// ---------------------------------------------------------------------------------------------

async fn simulation_in(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> R<CalendarSimulation> {
    let row = sqlx::query(&format!(
        "{SIM_SELECT} WHERE s.id = $1 AND s.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(db)?
    .ok_or(KernelError::NotFound {
        entity: "calendar simulation",
    })?;
    simulation_row(&row)
}

pub async fn get_simulation(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarSimulationId,
) -> R<CalendarSimulation> {
    let mut conn = pool.acquire().await.map_err(db)?;
    simulation_in(&mut conn, tenant, id.as_uuid()).await
}

pub async fn list_simulations(
    pool: &PgPool,
    tenant: TenantId,
    calendar_id: AcademicCalendarId,
) -> R<Vec<CalendarSimulation>> {
    let mut conn = pool.acquire().await.map_err(db)?;
    calendar_in(&mut conn, tenant, calendar_id.as_uuid()).await?;
    let rows = sqlx::query(&format!(
        "{SIM_SELECT} WHERE s.tenant_id = $1 AND s.calendar_id = $2 ORDER BY s.created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(calendar_id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?;
    rows.iter().map(simulation_row).collect()
}

/// Point-in-time baseline of a version: definition, assignments and exceptions (read only).
async fn baseline(
    conn: &mut PgConnection,
    tenant: TenantId,
    v: &CalendarVersion,
) -> R<SimulationBaseline> {
    let y = year_in(conn, tenant, v.academic_year_id.as_uuid()).await?;
    let assignments = sqlx::query(&format!(
        "{} WHERE a.tenant_id = $1 AND a.calendar_version_id = $2 ORDER BY a.created_at",
        assignment_select()
    ))
    .bind(tenant.as_uuid())
    .bind(v.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?
    .iter()
    .map(assignment_row)
    .collect::<R<Vec<_>>>()?;
    let exceptions = sqlx::query(&format!(
        "{} WHERE e.tenant_id = $1 AND e.calendar_version_id = $2 ORDER BY e.created_at",
        exception_select()
    ))
    .bind(tenant.as_uuid())
    .bind(v.id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(db)?
    .iter()
    .map(exception_row)
    .collect::<R<Vec<_>>>()?;
    Ok(SimulationBaseline {
        calendar_version_id: v.id,
        version_no: v.version_no,
        status: v.status,
        aggregate_version: v.version.0,
        timezone: v.timezone.clone(),
        year_starts_on: y.starts_on,
        year_ends_on: y.ends_on,
        starts_on: v.starts_on,
        ends_on: v.ends_on,
        terms: v
            .terms
            .iter()
            .map(|t| NewTerm {
                code: t.code.clone(),
                name: t.name.clone(),
                sequence: t.sequence,
                starts_on: t.starts_on,
                ends_on: t.ends_on,
            })
            .collect(),
        key_dates: v
            .key_dates
            .iter()
            .map(|k| NewKeyDate {
                kind: k.kind,
                name: k.name.clone(),
                starts_on: k.starts_on,
                ends_on: Some(k.ends_on),
            })
            .collect(),
        assignments: assignments
            .iter()
            .map(|a| BaselineAssignment {
                id: a.id.as_uuid(),
                scope_kind: a.scope_kind,
                scope_id: a.scope_id,
                status: a.status,
                effective_from: a.effective_from,
                effective_to: a.effective_to,
            })
            .collect(),
        exceptions: exceptions
            .iter()
            .map(|e| BaselineException {
                id: e.id.as_uuid(),
                kind: e.kind,
                status: e.status,
                starts_on: e.starts_on,
                ends_on: e.ends_on,
            })
            .collect(),
        captured_at: Utc::now(),
    })
}

/// Evaluates a proposed change (L3-B-032) and records it as a draft analysis
/// (`evaluated`). Only the `calendar_simulation` row and its audit entry are written; no
/// calendar state changes and **no event** is published (contract §4.3).
pub async fn create_simulation(
    pool: &PgPool,
    tenant: TenantId,
    calendar_id: AcademicCalendarId,
    cmd: NewSimulation,
    actor: &Actor,
) -> R<CalendarSimulation> {
    for t in cmd.proposal.terms.iter().flatten() {
        t.validate()?;
    }
    for k in cmd.proposal.key_dates.iter().flatten() {
        k.validate()?;
    }
    let mut tx = pool.begin().await.map_err(db)?;
    let cal = calendar_in(&mut tx, tenant, calendar_id.as_uuid()).await?;
    let scope = ExecutionScope::institution(tenant, cal.institution_id);
    guard::lock_institution_for_change(&mut tx, &scope, cal.institution_id).await?;
    let v = version_in(&mut tx, tenant, cmd.baseline_version_id.as_uuid())
        .await
        .map_err(|_| {
            KernelError::Validation(vec![FieldViolation::new(
                "baselineVersionId",
                "not-found",
                "unknown calendar version",
            )])
        })?;
    if v.calendar_id != calendar_id {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "baselineVersionId",
            "foreign",
            "the version belongs to another calendar",
        )]));
    }
    let b = baseline(&mut tx, tenant, &v).await?;
    let report = simulate(&b, &cmd.proposal);
    let id = CalendarSimulationId::new().as_uuid();
    sqlx::query(
        "INSERT INTO institution.calendar_simulation (id, tenant_id, institution_id, calendar_id, baseline_version_id, status, baseline, proposal, impact, outcome, created_by) \
         VALUES ($1, $2, $3, $4, $5, 'evaluated', $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(cal.institution_id.as_uuid())
    .bind(calendar_id.as_uuid())
    .bind(v.id.as_uuid())
    .bind(serde_json::to_value(&b).map_err(KernelError::internal)?)
    .bind(serde_json::to_value(&cmd.proposal).map_err(KernelError::internal)?)
    .bind(serde_json::to_value(&report).map_err(KernelError::internal)?)
    .bind(report.outcome.as_str())
    .bind(&actor.id)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    audit(&mut tx, &scope, "calendar_simulation.evaluate", "calendar_simulation", id, json!({ "baselineVersionId": v.id, "outcome": report.outcome.as_str(), "items": report.items.len() }), actor).await?;
    let out = simulation_in(&mut tx, tenant, id).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}

/// Records the governed impact decision (BH-005). Accepting authorises nothing (BH-006): the
/// change still goes through a revision + publication.
pub async fn decide_simulation(
    pool: &PgPool,
    tenant: TenantId,
    id: CalendarSimulationId,
    expected: Version,
    d: SimulationDecision,
    actor: &Actor,
) -> R<CalendarSimulation> {
    let mut tx = pool.begin().await.map_err(db)?;
    let l = lock(
        &mut tx,
        "calendar_simulation",
        "calendar simulation",
        tenant,
        id.as_uuid(),
        Some(expected),
    )
    .await?;
    let from = SimulationStatus::parse(&l.status).ok_or_else(|| bad_status(&l.status))?;
    let to = match d.decision {
        SimulationDecisionKind::Accept => SimulationStatus::Accepted,
        SimulationDecisionKind::Discard => SimulationStatus::Discarded,
    };
    if !simulation_can_transition(from, to) {
        return Err(illegal("calendar simulation", from, to));
    }
    sqlx::query(
        "UPDATE institution.calendar_simulation SET status = $2, decision_reason = $3, decided_by = $4, decided_at = now(), version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(id.as_uuid())
    .bind(to.as_str())
    .bind(trimmed(d.reason.as_deref()))
    .bind(&actor.id)
    .execute(&mut *tx)
    .await
    .map_err(db)?;
    audit(
        &mut tx,
        &l.scope,
        &format!(
            "calendar_simulation.{}",
            if to == SimulationStatus::Accepted {
                "accept"
            } else {
                "discard"
            }
        ),
        "calendar_simulation",
        id.as_uuid(),
        json!({ "reason": trimmed(d.reason.as_deref()) }),
        actor,
    )
    .await?;
    let out = simulation_in(&mut tx, tenant, id.as_uuid()).await?;
    tx.commit().await.map_err(db)?;
    Ok(out)
}
