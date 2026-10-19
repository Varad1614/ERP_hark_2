//! **2C** readiness provider (contract doc §7). Returns **every** key on each call:
//! `calendar.version_published`, `calendar.academic_year_open`, `calendar.assignment_effective`.
//!
//! Computed live from the calendar tables (nothing is persisted). Business dates are compared
//! with "today" in the institution business timezone.

use crate::shared::readiness::{ReadinessArea, ReadinessItem, ReadinessStatus};
use sqlx::{PgPool, Row};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;

pub const VERSION_PUBLISHED: &str = "calendar.version_published";
pub const ACADEMIC_YEAR_OPEN: &str = "calendar.academic_year_open";
pub const ASSIGNMENT_EFFECTIVE: &str = "calendar.assignment_effective";
const LINK: &str = "/academics/calendar";

/// Counts the provider reads (one round trip).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CalendarFacts {
    pub calendars: i64,
    pub published_versions: i64,
    pub open_years: i64,
    /// Planned years that already have a published version (ready to open).
    pub planned_years_with_version: i64,
    pub institution_assignments: i64,
    /// Effective institution-level assignments not ended before today.
    pub effective_institution_assignments: i64,
    /// … of which already governing today.
    pub governing_institution_assignments: i64,
}

fn status(met: bool) -> ReadinessStatus {
    if met {
        ReadinessStatus::Met
    } else {
        ReadinessStatus::Unmet
    }
}

/// Pure mapping from facts to the three checklist items.
pub fn items(f: &CalendarFacts) -> Vec<ReadinessItem> {
    let published = ReadinessItem::new(
        VERSION_PUBLISHED,
        ReadinessArea::Calendar,
        "An academic calendar version is published",
        true,
        status(f.published_versions > 0),
    )
    .detail(match (f.calendars, f.published_versions) {
        (0, _) => "No academic calendar yet".to_owned(),
        (c, 0) => format!("{c} calendar(s), no published version"),
        (_, p) => format!("{p} published version(s)"),
    })
    .link(LINK);
    let year_met = f.open_years > 0 || f.planned_years_with_version > 0;
    let year = ReadinessItem::new(
        ACADEMIC_YEAR_OPEN,
        ReadinessArea::Calendar,
        "An academic year is open (or planned with a published calendar)",
        true,
        status(year_met),
    )
    .detail(if f.open_years > 0 {
        format!("{} open academic year(s)", f.open_years)
    } else if f.planned_years_with_version > 0 {
        format!(
            "{} planned year(s) with a published calendar, ready to open",
            f.planned_years_with_version
        )
    } else {
        "No open academic year".to_owned()
    })
    .link(LINK);
    let assignment = ReadinessItem::new(
        ASSIGNMENT_EFFECTIVE,
        ReadinessArea::Calendar,
        "The calendar is assigned to the institution",
        true,
        status(f.effective_institution_assignments > 0),
    )
    .detail(if f.governing_institution_assignments > 0 {
        "An institution-wide assignment governs today".to_owned()
    } else if f.effective_institution_assignments > 0 {
        "An institution-wide assignment is effective from a later date".to_owned()
    } else if f.institution_assignments > 0 {
        "An institution-wide assignment exists but is not effective".to_owned()
    } else {
        "No institution-wide calendar assignment".to_owned()
    })
    .link(LINK);
    vec![published, year, assignment]
}

pub async fn facts(
    pool: &PgPool,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
) -> Result<Vec<ReadinessItem>, KernelError> {
    let row = sqlx::query(
        "WITH inst AS ( \
           SELECT id, tenant_id, (now() AT TIME ZONE timezone)::date AS today \
           FROM institution.institution WHERE id = $1 AND tenant_id = $2) \
         SELECT \
           (SELECT count(*) FROM institution.academic_calendar c, inst \
             WHERE c.institution_id = inst.id AND c.tenant_id = inst.tenant_id AND c.status <> 'retired') AS calendars, \
           (SELECT count(*) FROM institution.calendar_version v JOIN institution.academic_calendar c ON c.id = v.calendar_id, inst \
             WHERE v.institution_id = inst.id AND v.tenant_id = inst.tenant_id AND v.status = 'published' AND c.status <> 'retired') AS published_versions, \
           (SELECT count(*) FROM institution.academic_year y, inst \
             WHERE y.institution_id = inst.id AND y.tenant_id = inst.tenant_id AND y.status = 'open') AS open_years, \
           (SELECT count(*) FROM institution.academic_year y, inst \
             WHERE y.institution_id = inst.id AND y.tenant_id = inst.tenant_id AND y.status = 'planned' \
               AND EXISTS (SELECT 1 FROM institution.calendar_version v WHERE v.academic_year_id = y.id AND v.status = 'published')) AS planned_ready, \
           (SELECT count(*) FROM institution.calendar_assignment a, inst \
             WHERE a.institution_id = inst.id AND a.tenant_id = inst.tenant_id AND a.scope_kind = 'institution' \
               AND a.scope_id = inst.id AND a.status <> 'retired') AS inst_assignments, \
           (SELECT count(*) FROM institution.calendar_assignment a, inst \
             WHERE a.institution_id = inst.id AND a.tenant_id = inst.tenant_id AND a.scope_kind = 'institution' \
               AND a.scope_id = inst.id AND a.status = 'effective' \
               AND (a.effective_to IS NULL OR a.effective_to >= inst.today)) AS effective_assignments, \
           (SELECT count(*) FROM institution.calendar_assignment a, inst \
             WHERE a.institution_id = inst.id AND a.tenant_id = inst.tenant_id AND a.scope_kind = 'institution' \
               AND a.scope_id = inst.id AND a.status = 'effective' AND a.effective_from <= inst.today \
               AND (a.effective_to IS NULL OR a.effective_to >= inst.today)) AS governing_assignments",
    )
    .bind(institution_id.as_uuid())
    .bind(scope.tenant_id.as_uuid())
    .fetch_one(pool)
    .await
    .map_err(map_db_error)?;
    let get = |c: &str| -> Result<i64, KernelError> { row.try_get(c).map_err(map_db_error) };
    Ok(items(&CalendarFacts {
        calendars: get("calendars")?,
        published_versions: get("published_versions")?,
        open_years: get("open_years")?,
        planned_years_with_version: get("planned_ready")?,
        institution_assignments: get("inst_assignments")?,
        effective_institution_assignments: get("effective_assignments")?,
        governing_institution_assignments: get("governing_assignments")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn always_returns_all_three_keys() {
        let empty = items(&CalendarFacts::default());
        let keys: Vec<_> = empty.iter().map(|i| i.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![VERSION_PUBLISHED, ACADEMIC_YEAR_OPEN, ASSIGNMENT_EFFECTIVE]
        );
        assert!(empty
            .iter()
            .all(|i| i.required && i.status == ReadinessStatus::Unmet));
        assert!(empty.iter().all(|i| i.link.as_deref() == Some(LINK)));
    }

    #[test]
    fn planned_year_with_published_version_counts_and_future_assignment_is_met() {
        let f = CalendarFacts {
            calendars: 1,
            published_versions: 1,
            open_years: 0,
            planned_years_with_version: 1,
            institution_assignments: 1,
            effective_institution_assignments: 1,
            governing_institution_assignments: 0,
        };
        let it = items(&f);
        assert!(it.iter().all(|i| i.status == ReadinessStatus::Met));
        assert!(it[2]
            .detail
            .as_deref()
            .unwrap_or_default()
            .contains("later date"));
    }
}
