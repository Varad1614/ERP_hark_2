//! The **only** writer of `people.attendance_day` (contract doc §3.3): 7B's attendance capture
//! (single, bulk, correction) and leave flow (`on_leave` days) record days through
//! [`record_day`], inside their owner-local transaction. One row per staff member and date
//! (upsert); a status change bumps `version` and appends `people.status_history`
//! (`attendance_day`) so corrections stay traceable. Payroll reads the days only through
//! `LeaveReader::payroll_inputs`.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::rows::decode;
use crate::shared::Actor;
use chrono::NaiveDate;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::people::ids::{AttendanceDayId, StaffMemberId};
use sutra_contracts::people::status::{AttendanceDayStatus, AttendanceSource};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// One attendance day to record.
#[derive(Clone, Debug)]
pub struct DayRecord<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub staff_id: StaffMemberId,
    pub work_date: NaiveDate,
    pub status: AttendanceDayStatus,
    pub source: AttendanceSource,
    /// 7B leave request behind an `on_leave` day.
    pub leave_request_id: Option<Uuid>,
    pub remark: Option<&'a str>,
}

/// A stored attendance day.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttendanceDayRow {
    pub id: AttendanceDayId,
    pub staff_id: StaffMemberId,
    pub work_date: NaiveDate,
    pub status: AttendanceDayStatus,
    pub source: AttendanceSource,
    pub leave_request_id: Option<Uuid>,
    pub remark: Option<String>,
    pub version: i64,
}

pub const ATTENDANCE_COLUMNS: &str =
    "id, staff_id, work_date, status, source, leave_request_id, remark, version";

pub fn attendance_from_row(r: &PgRow) -> Result<AttendanceDayRow, sqlx::Error> {
    Ok(AttendanceDayRow {
        id: AttendanceDayId(r.try_get("id")?),
        staff_id: StaffMemberId(r.try_get("staff_id")?),
        work_date: r.try_get("work_date")?,
        status: decode(r.try_get("status")?, AttendanceDayStatus::parse)?,
        source: decode(r.try_get("source")?, AttendanceSource::parse)?,
        leave_request_id: r.try_get("leave_request_id")?,
        remark: r.try_get("remark")?,
        version: r.try_get("version")?,
    })
}

/// Records (inserts or updates) one attendance day (see the module docs). The staff record must
/// belong to the tenant and institution (404 otherwise). Recording the same status again is a
/// no-op that returns the stored row.
pub async fn record_day(
    conn: &mut PgConnection,
    d: &DayRecord<'_>,
    actor: &Actor,
) -> Result<AttendanceDayRow, KernelError> {
    let known: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.staff_member WHERE id = $1 AND tenant_id = $2 \
         AND institution_id = $3)",
    )
    .bind(d.staff_id.as_uuid())
    .bind(d.tenant.as_uuid())
    .bind(d.institution.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if !known {
        return Err(KernelError::NotFound {
            entity: "staff member",
        });
    }
    let previous = sqlx::query(&format!(
        "SELECT {ATTENDANCE_COLUMNS} FROM people.attendance_day WHERE staff_id = $1 \
         AND work_date = $2 FOR UPDATE"
    ))
    .bind(d.staff_id.as_uuid())
    .bind(d.work_date)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .as_ref()
    .map(attendance_from_row)
    .transpose()
    .map_err(map_db_error)?;
    if let Some(p) = &previous {
        if p.status == d.status
            && p.leave_request_id == d.leave_request_id
            && p.remark.as_deref() == d.remark
        {
            return Ok(p.clone());
        }
    }
    let row = sqlx::query(&format!(
        "INSERT INTO people.attendance_day (tenant_id, institution_id, staff_id, work_date, \
         status, source, leave_request_id, remark) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         ON CONFLICT (staff_id, work_date) DO UPDATE SET status = EXCLUDED.status, \
         source = EXCLUDED.source, leave_request_id = EXCLUDED.leave_request_id, \
         remark = EXCLUDED.remark, version = people.attendance_day.version + 1, updated_at = now() \
         RETURNING {ATTENDANCE_COLUMNS}"
    ))
    .bind(d.tenant.as_uuid())
    .bind(d.institution.as_uuid())
    .bind(d.staff_id.as_uuid())
    .bind(d.work_date)
    .bind(d.status.as_str())
    .bind(d.source.as_str())
    .bind(d.leave_request_id)
    .bind(d.remark)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let day = attendance_from_row(&row).map_err(map_db_error)?;
    if previous.as_ref().map(|p| p.status) != Some(day.status) {
        record_history(
            conn,
            HistoryEntry {
                tenant: d.tenant,
                institution: d.institution,
                aggregate_type: "attendance_day",
                aggregate_id: day.id.as_uuid(),
                from: previous.as_ref().map(|p| p.status.as_str()),
                to: day.status.as_str(),
            },
            &StatusChange {
                cause_event_type: "sutraerp.people.attendance_day.recorded",
                reason: d.remark,
                process_instance_id: None,
            },
            actor,
        )
        .await?;
    }
    Ok(day)
}
