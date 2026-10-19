//! The **only** writer of the `status` of the four shared delivery tables (contract doc §3.2):
//! `course_offering`, `offering_section`, `faculty_assignment`, `section_enrolment`.
//!
//! Each call, inside the caller's owner-local transaction and on a row the caller locked
//! ([`crate::shared::offering`]`::lock_*`):
//!
//! 1. checks the legal transition (`can_transition` in `sutra_contracts::academic::status`) →
//!    `409 conflict` otherwise;
//! 2. updates status, reason, `status_changed_at` (+ `process_instance_id` / `ended_at` where the
//!    table has them) and bumps `version` (optimistic: the row's version must still match);
//! 3. appends an `academic.status_history` row.
//!
//! The slice still emits its own business event (e.g. `course_offering.activated`) and audit
//! entry in the same transaction. Only slice **4A** calls these.

use crate::shared::offering::{
    assignment_from_row, enrolment_from_row, offering_from_row, section_from_row,
    FacultyAssignmentRow, OfferingRow, SectionEnrolmentRow, SectionRow, ASSIGNMENT_COLUMNS,
    ENROLMENT_COLUMNS, OFFERING_COLUMNS, SECTION_COLUMNS,
};
use crate::shared::Actor;
use sqlx::postgres::PgRow;
use sqlx::PgConnection;
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, SectionEnrolmentStatus, SectionStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Why a status changes.
#[derive(Clone, Debug, Default)]
pub struct StatusChange<'a> {
    /// The slice business event type that causes the change (e.g. `COURSE_OFFERING_ACTIVATED`).
    pub cause_event_type: &'static str,
    pub reason: Option<&'a str>,
    pub process_instance_id: Option<&'a str>,
}

struct Target<'a> {
    table: &'static str,
    aggregate: &'static str,
    columns: &'static str,
    id: Uuid,
    tenant: TenantId,
    institution: InstitutionId,
    version: i64,
    from: &'a str,
    to: &'a str,
    /// Extra `SET` fragment (no leading comma).
    extra: &'static str,
}

async fn apply(
    conn: &mut PgConnection,
    t: Target<'_>,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<PgRow, KernelError> {
    let extra = if t.extra.is_empty() {
        String::new()
    } else {
        format!(", {}", t.extra)
    };
    let sql = format!(
        "UPDATE academic.{} SET status = $3, status_reason = $4, status_changed_at = now(){extra}, \
         version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 AND version = $5 RETURNING {}",
        t.table, t.columns
    );
    let mut q = sqlx::query(&sql)
        .bind(t.id)
        .bind(t.tenant.as_uuid())
        .bind(t.to)
        .bind(change.reason)
        .bind(t.version);
    if t.extra.contains("$6") {
        q = q.bind(change.process_instance_id);
    }
    let row = q
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| KernelError::Conflict(format!("{} changed concurrently", t.aggregate)))?;
    sqlx::query(
        "INSERT INTO academic.status_history (tenant_id, institution_id, aggregate_type, \
         aggregate_id, from_status, to_status, reason, cause_event_type, process_instance_id, \
         actor, correlation_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(t.tenant.as_uuid())
    .bind(t.institution.as_uuid())
    .bind(t.table)
    .bind(t.id)
    .bind(t.from)
    .bind(t.to)
    .bind(change.reason)
    .bind(change.cause_event_type)
    .bind(change.process_instance_id)
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(row)
}

fn illegal(what: &str, from: impl std::fmt::Display, to: impl std::fmt::Display) -> KernelError {
    KernelError::Conflict(format!("{what} is {from}; it cannot become {to}"))
}

/// Moves a locked offering to `to` (stores `process_instance_id` when given).
pub async fn change_offering_status(
    conn: &mut PgConnection,
    o: &OfferingRow,
    to: CourseOfferingStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<OfferingRow, KernelError> {
    if !o.status.can_transition(to) {
        return Err(illegal("the course offering", o.status, to));
    }
    let row = apply(
        conn,
        Target {
            table: "course_offering",
            aggregate: "course offering",
            columns: OFFERING_COLUMNS,
            id: o.id.as_uuid(),
            tenant: o.tenant_id,
            institution: o.institution_id,
            version: o.version.0,
            from: o.status.as_str(),
            to: to.as_str(),
            extra: "process_instance_id = COALESCE($6, process_instance_id)",
        },
        change,
        actor,
    )
    .await?;
    offering_from_row(&row).map_err(map_db_error)
}

/// Moves a locked section to `to`.
pub async fn change_section_status(
    conn: &mut PgConnection,
    s: &SectionRow,
    to: SectionStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<SectionRow, KernelError> {
    if !s.status.can_transition(to) {
        return Err(illegal("the section", s.status, to));
    }
    let row = apply(
        conn,
        Target {
            table: "offering_section",
            aggregate: "section",
            columns: SECTION_COLUMNS,
            id: s.id.as_uuid(),
            tenant: s.tenant_id,
            institution: s.institution_id,
            version: s.version.0,
            from: s.status.as_str(),
            to: to.as_str(),
            extra: "",
        },
        change,
        actor,
    )
    .await?;
    section_from_row(&row).map_err(map_db_error)
}

/// Moves a locked faculty assignment to `to` (stores `process_instance_id` when given).
pub async fn change_assignment_status(
    conn: &mut PgConnection,
    a: &FacultyAssignmentRow,
    to: FacultyAssignmentStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<FacultyAssignmentRow, KernelError> {
    if !a.status.can_transition(to) {
        return Err(illegal("the faculty assignment", a.status, to));
    }
    let row = apply(
        conn,
        Target {
            table: "faculty_assignment",
            aggregate: "faculty assignment",
            columns: ASSIGNMENT_COLUMNS,
            id: a.id.as_uuid(),
            tenant: a.tenant_id,
            institution: a.institution_id,
            version: a.version.0,
            from: a.status.as_str(),
            to: to.as_str(),
            extra: "process_instance_id = COALESCE($6, process_instance_id)",
        },
        change,
        actor,
    )
    .await?;
    assignment_from_row(&row).map_err(map_db_error)
}

/// Moves a locked section enrolment to `to` (sets `ended_at` when the seat is given up).
pub async fn change_enrolment_status(
    conn: &mut PgConnection,
    e: &SectionEnrolmentRow,
    to: SectionEnrolmentStatus,
    change: &StatusChange<'_>,
    actor: &Actor,
) -> Result<SectionEnrolmentRow, KernelError> {
    if !e.status.can_transition(to) {
        return Err(illegal("the section enrolment", e.status, to));
    }
    let row = apply(
        conn,
        Target {
            table: "section_enrolment",
            aggregate: "section enrolment",
            columns: ENROLMENT_COLUMNS,
            id: e.id.as_uuid(),
            tenant: e.tenant_id,
            institution: e.institution_id,
            version: e.version.0,
            from: e.status.as_str(),
            to: to.as_str(),
            extra: "ended_at = now()",
        },
        change,
        actor,
    )
    .await?;
    enrolment_from_row(&row).map_err(map_db_error)
}
