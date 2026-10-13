//! Row decoders of the foundation tables (`people.staff_member`, `people.staff_engagement`).
//! Used by the foundation directory and by the slices' own queries, so every slice decodes the
//! shared rows the same way.

use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_contracts::people::status::{
    EmploymentType, EngagementStatus, StaffCategory, StaffStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Strict decode of a stored status / classification name.
pub fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown value `{raw}`").into()))
}

// ------------------------------------------------------------------ staff

pub const STAFF_COLUMNS: &str = "id, institution_id, staff_number, subject_ref, given_name, \
    family_name, display_name, work_email, staff_category, status, canonical_staff_id, version";

pub fn staff_from_row(r: &PgRow) -> Result<StaffSummary, sqlx::Error> {
    Ok(StaffSummary {
        id: StaffMemberId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        staff_number: r.try_get("staff_number")?,
        subject_ref: r.try_get("subject_ref")?,
        given_name: r.try_get("given_name")?,
        family_name: r.try_get("family_name")?,
        display_name: r.try_get("display_name")?,
        work_email: r.try_get("work_email")?,
        staff_category: decode(r.try_get("staff_category")?, StaffCategory::parse)?,
        status: decode(r.try_get("status")?, StaffStatus::parse)?,
        canonical_staff_id: r
            .try_get::<Option<Uuid>, _>("canonical_staff_id")?
            .map(StaffMemberId),
        version: r.try_get("version")?,
    })
}

/// Reads a staff record (`None` when missing / foreign).
pub async fn load_staff(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StaffMemberId,
) -> Result<Option<StaffSummary>, KernelError> {
    let sql =
        format!("SELECT {STAFF_COLUMNS} FROM people.staff_member WHERE id = $1 AND tenant_id = $2");
    sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(staff_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// Locks a staff row (`FOR UPDATE`) in the caller's transaction (404 when missing / foreign).
pub async fn lock_staff(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StaffMemberId,
) -> Result<StaffSummary, KernelError> {
    let sql = format!(
        "SELECT {STAFF_COLUMNS} FROM people.staff_member WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    staff_from_row(&row).map_err(map_db_error)
}

// ------------------------------------------------------------------ engagement

pub const ENGAGEMENT_COLUMNS: &str = "id, staff_id, institution_id, engagement_number, \
    org_unit_id, campus_id, staff_category, employment_type, designation, grade, start_date, \
    end_date, last_working_date, probation_end_date, reports_to_staff_id, rejoin_of_engagement_id, \
    status, version";

pub fn engagement_from_row(r: &PgRow) -> Result<EngagementSummary, sqlx::Error> {
    Ok(EngagementSummary {
        id: StaffEngagementId(r.try_get("id")?),
        staff_id: StaffMemberId(r.try_get("staff_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        engagement_number: r.try_get("engagement_number")?,
        org_unit_id: r.try_get("org_unit_id")?,
        campus_id: r.try_get("campus_id")?,
        staff_category: decode(r.try_get("staff_category")?, StaffCategory::parse)?,
        employment_type: decode(r.try_get("employment_type")?, EmploymentType::parse)?,
        designation: r.try_get("designation")?,
        grade: r.try_get("grade")?,
        start_date: r.try_get("start_date")?,
        end_date: r.try_get("end_date")?,
        last_working_date: r.try_get("last_working_date")?,
        probation_end_date: r.try_get("probation_end_date")?,
        reports_to_staff_id: r
            .try_get::<Option<Uuid>, _>("reports_to_staff_id")?
            .map(StaffMemberId),
        rejoin_of_engagement_id: r
            .try_get::<Option<Uuid>, _>("rejoin_of_engagement_id")?
            .map(StaffEngagementId),
        status: decode(r.try_get("status")?, EngagementStatus::parse)?,
        version: r.try_get("version")?,
    })
}

/// Reads an engagement (`None` when missing / foreign).
pub async fn load_engagement(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StaffEngagementId,
) -> Result<Option<EngagementSummary>, KernelError> {
    let sql = format!(
        "SELECT {ENGAGEMENT_COLUMNS} FROM people.staff_engagement WHERE id = $1 AND tenant_id = $2"
    );
    sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(engagement_from_row)
        .transpose()
        .map_err(map_db_error)
}

/// Locks an engagement row (`FOR UPDATE`) in the caller's transaction (404 when missing / foreign).
pub async fn lock_engagement(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: StaffEngagementId,
) -> Result<EngagementSummary, KernelError> {
    let sql = format!(
        "SELECT {ENGAGEMENT_COLUMNS} FROM people.staff_engagement WHERE id = $1 AND tenant_id = $2 \
         FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "engagement",
        })?;
    engagement_from_row(&row).map_err(map_db_error)
}

/// The live engagement of a staff member, else the most recently started one.
pub async fn current_engagement(
    conn: &mut PgConnection,
    tenant: TenantId,
    staff: StaffMemberId,
) -> Result<Option<EngagementSummary>, KernelError> {
    let sql = format!(
        "SELECT {ENGAGEMENT_COLUMNS} FROM people.staff_engagement WHERE staff_id = $1 AND tenant_id = $2 \
         ORDER BY (status NOT IN ('closed', 'cancelled')) DESC, start_date DESC, created_at DESC LIMIT 1"
    );
    sqlx::query(&sql)
        .bind(staff.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .as_ref()
        .map(engagement_from_row)
        .transpose()
        .map_err(map_db_error)
}
