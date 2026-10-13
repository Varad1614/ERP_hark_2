//! The **only** inserters of staff records and engagements (contract doc §3.3).
//!
//! 7A's onboarding / manual creation / rejoining flows (and the test seeders in
//! `apps/api/tests/support/b7.rs`) create rows exclusively through [`insert_staff`] and
//! [`insert_engagement`], inside the caller's owner-local transaction. Both take a gap-free
//! document number (`EMP-…`, `ENG-…`) and append the initial `people.status_history` row; the
//! caller emits its own business event and audit entry. Duplicates are `409`: a live staff record
//! per login subject, a live engagement per staff member.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::numbering::{kind, next};
use crate::shared::rows::{engagement_from_row, staff_from_row, ENGAGEMENT_COLUMNS, STAFF_COLUMNS};
use crate::shared::Actor;
use chrono::NaiveDate;
use sqlx::PgConnection;
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_contracts::people::status::{
    EmploymentType, EngagementStatus, Gender, StaffCategory, StaffStatus,
};
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// A new staff record.
#[derive(Clone, Debug)]
pub struct NewStaff<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub subject_ref: Option<&'a str>,
    pub given_name: &'a str,
    pub family_name: Option<&'a str>,
    /// Defaults to `given_name family_name`.
    pub display_name: Option<&'a str>,
    pub work_email: Option<&'a str>,
    pub phone: Option<&'a str>,
    pub date_of_birth: Option<NaiveDate>,
    pub gender: Option<Gender>,
    pub staff_category: StaffCategory,
    /// Initial status: `draft`, `pending_verification` or `active` (HR creation with verified
    /// identity, seeds).
    pub status: StaffStatus,
}

fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

fn trimmed(v: Option<&str>) -> Option<String> {
    v.map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Inserts a staff record (see the module docs).
pub async fn insert_staff(
    conn: &mut PgConnection,
    n: &NewStaff<'_>,
    actor: &Actor,
) -> Result<StaffSummary, KernelError> {
    if !matches!(
        n.status,
        StaffStatus::Draft | StaffStatus::PendingVerification | StaffStatus::Active
    ) {
        return Err(invalid(
            "status",
            "invalid_initial_status",
            "a staff record starts draft, pending_verification or active",
        ));
    }
    let given = n.given_name.trim();
    if given.is_empty() || given.chars().count() > 100 {
        return Err(invalid("givenName", "required", "1 to 100 characters"));
    }
    let family = trimmed(n.family_name);
    let display = trimmed(n.display_name).unwrap_or_else(|| match &family {
        Some(f) => format!("{given} {f}"),
        None => given.to_owned(),
    });
    if let Some(e) = trimmed(n.work_email) {
        if !e.contains('@') || e.contains(char::is_whitespace) {
            return Err(invalid(
                "workEmail",
                "invalid_email",
                "must be an e-mail address",
            ));
        }
    }
    let subject = trimmed(n.subject_ref);
    if let Some(s) = &subject {
        let taken: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM people.staff_member WHERE tenant_id = $1 \
             AND subject_ref = $2 AND status NOT IN ('merged', 'archived'))",
        )
        .bind(n.tenant.as_uuid())
        .bind(s)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if taken {
            return Err(KernelError::Conflict(
                "a live staff record is already linked to this login".into(),
            ));
        }
    }
    let number = next(conn, n.tenant, n.institution, kind::STAFF).await?;
    let row = sqlx::query(&format!(
        "INSERT INTO people.staff_member (tenant_id, institution_id, staff_number, subject_ref, \
         given_name, family_name, display_name, work_email, phone, date_of_birth, gender, \
         staff_category, status, verified_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
         CASE WHEN $13 = 'active' THEN now() END) RETURNING {STAFF_COLUMNS}"
    ))
    .bind(n.tenant.as_uuid())
    .bind(n.institution.as_uuid())
    .bind(&number)
    .bind(&subject)
    .bind(given)
    .bind(&family)
    .bind(&display)
    .bind(trimmed(n.work_email))
    .bind(trimmed(n.phone))
    .bind(n.date_of_birth)
    .bind(n.gender.map(|g| g.as_str()))
    .bind(n.staff_category.as_str())
    .bind(n.status.as_str())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let staff = staff_from_row(&row).map_err(map_db_error)?;
    record_history(
        conn,
        HistoryEntry {
            tenant: n.tenant,
            institution: n.institution,
            aggregate_type: "staff_member",
            aggregate_id: staff.id.as_uuid(),
            from: None,
            to: staff.status.as_str(),
        },
        &StatusChange {
            cause_event_type: "sutraerp.people.staff_member.created",
            reason: None,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(staff)
}

/// A new engagement of a staff member.
#[derive(Clone, Debug)]
pub struct NewEngagement<'a> {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub staff_id: StaffMemberId,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub staff_category: StaffCategory,
    pub employment_type: EmploymentType,
    pub designation: &'a str,
    pub grade: Option<&'a str>,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    pub reports_to_staff_id: Option<StaffMemberId>,
    /// The closed engagement this one rejoins (L3-E-008).
    pub rejoin_of: Option<StaffEngagementId>,
    /// Initial status: `pending` (governed activation) or `active` (seeds, migrations).
    pub status: EngagementStatus,
}

fn valid_grade(g: &str) -> bool {
    let b = g.as_bytes();
    (1..=32).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'.' | b'-'))
}

/// Inserts an engagement (see the module docs). The staff record must exist in the same tenant
/// and institution and be live.
pub async fn insert_engagement(
    conn: &mut PgConnection,
    n: &NewEngagement<'_>,
    actor: &Actor,
) -> Result<EngagementSummary, KernelError> {
    if !matches!(
        n.status,
        EngagementStatus::Pending | EngagementStatus::Active
    ) {
        return Err(invalid(
            "status",
            "invalid_initial_status",
            "an engagement starts pending or active",
        ));
    }
    let designation = n.designation.trim();
    if designation.is_empty() || designation.chars().count() > 120 {
        return Err(invalid("designation", "required", "1 to 120 characters"));
    }
    let grade = trimmed(n.grade);
    if grade.as_deref().is_some_and(|g| !valid_grade(g)) {
        return Err(invalid(
            "grade",
            "invalid_code",
            "must match ^[A-Za-z0-9][A-Za-z0-9_.-]{0,31}$",
        ));
    }
    if n.end_date.is_some_and(|e| e < n.start_date) {
        return Err(invalid(
            "endDate",
            "before_start",
            "must not be before startDate",
        ));
    }
    if n.reports_to_staff_id == Some(n.staff_id) {
        return Err(invalid(
            "reportsToStaffId",
            "self_reporting",
            "a staff member cannot report to themselves",
        ));
    }
    let staff = crate::shared::rows::lock_staff(conn, n.tenant, n.staff_id).await?;
    if staff.institution_id != n.institution {
        return Err(KernelError::NotFound {
            entity: "staff member",
        });
    }
    if !staff.status.is_live() {
        return Err(KernelError::Conflict(format!(
            "staff record {} is {}; it cannot take a new engagement",
            staff.staff_number, staff.status
        )));
    }
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM people.staff_engagement WHERE staff_id = $1 \
         AND status NOT IN ('closed', 'cancelled'))",
    )
    .bind(n.staff_id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if live {
        return Err(KernelError::Conflict(format!(
            "staff record {} already has a live engagement",
            staff.staff_number
        )));
    }
    let number = next(conn, n.tenant, n.institution, kind::ENGAGEMENT).await?;
    let row = sqlx::query(&format!(
        "INSERT INTO people.staff_engagement (tenant_id, institution_id, staff_id, \
         engagement_number, org_unit_id, campus_id, staff_category, employment_type, designation, \
         grade, start_date, end_date, probation_end_date, reports_to_staff_id, \
         rejoin_of_engagement_id, status) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16) \
         RETURNING {ENGAGEMENT_COLUMNS}"
    ))
    .bind(n.tenant.as_uuid())
    .bind(n.institution.as_uuid())
    .bind(n.staff_id.as_uuid())
    .bind(&number)
    .bind(n.org_unit_id)
    .bind(n.campus_id)
    .bind(n.staff_category.as_str())
    .bind(n.employment_type.as_str())
    .bind(designation)
    .bind(&grade)
    .bind(n.start_date)
    .bind(n.end_date)
    .bind(n.probation_end_date)
    .bind(n.reports_to_staff_id.map(|s| s.as_uuid()))
    .bind(n.rejoin_of.map(|e| e.as_uuid()))
    .bind(n.status.as_str())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let e = engagement_from_row(&row).map_err(map_db_error)?;
    record_history(
        conn,
        HistoryEntry {
            tenant: n.tenant,
            institution: n.institution,
            aggregate_type: "staff_engagement",
            aggregate_id: e.id.as_uuid(),
            from: None,
            to: e.status.as_str(),
        },
        &StatusChange {
            cause_event_type: "sutraerp.people.staff_engagement.created",
            reason: None,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    Ok(e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grades_follow_the_code_pattern() {
        assert!(valid_grade("L10") && valid_grade("AP-2") && valid_grade("7.CPC"));
        assert!(!valid_grade("-A") && !valid_grade("") && !valid_grade("a b"));
        assert_eq!(trimmed(Some("  x ")), Some("x".into()));
        assert_eq!(trimmed(Some("  ")), None);
    }
}
