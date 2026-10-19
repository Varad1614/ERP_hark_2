//! Pure position-definition rules (L3-B-013..015): lifecycle, effective-dated revisions.

use crate::lifecycle::common::{finish, invalid, validate_code, validate_text};
use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::institution::ids::{OrgUnitId, PositionId};
use sutra_contracts::institution::status::PositionStatus;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::InstitutionId;

/// Allowed transitions: `draft → active → retired → archived`. No shortcut back from retired
/// or archived (L4-B-015-BH-005).
pub fn can_transition(from: PositionStatus, to: PositionStatus) -> bool {
    use PositionStatus::*;
    matches!(
        (from, to),
        (Draft, Active) | (Active, Retired) | (Retired, Archived)
    )
}

/// One effective-dated revision of a definition.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PositionRevision {
    pub revision: i32,
    pub org_unit_id: OrgUnitId,
    pub title: String,
    pub grade: Option<String>,
    pub description: Option<String>,
    pub headcount: i32,
    /// Business-local date (institution timezone) from which the revision applies.
    pub effective_from: NaiveDate,
    /// Last business-local day it applies (closed by a successor or the retirement).
    pub effective_to: Option<NaiveDate>,
    pub reason: Option<String>,
    pub recorded_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// A position definition as of a business-local date. Never an occupant or an appointment.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Position {
    pub id: PositionId,
    pub institution_id: InstitutionId,
    pub code: String,
    pub status: PositionStatus,
    pub status_reason: Option<String>,
    /// Fields of the revision in force on `asOf` (or the first revision when none is yet).
    pub org_unit_id: OrgUnitId,
    pub title: String,
    pub grade: Option<String>,
    pub description: Option<String>,
    pub headcount: i32,
    pub revision: i32,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    /// Highest revision number.
    pub latest_revision: i32,
    /// A later revision that is not yet in force on `asOf`.
    pub scheduled_revision: Option<PositionRevision>,
    pub retired_on: Option<NaiveDate>,
    /// Business-local date this view was computed for.
    pub as_of: NaiveDate,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// The revision in force on `as_of` (revisions ordered by number), else the earliest one.
pub fn in_force(revs: &[PositionRevision], as_of: NaiveDate) -> Option<&PositionRevision> {
    revs.iter()
        .rev()
        .find(|r| r.effective_from <= as_of && r.effective_to.is_none_or(|t| t >= as_of))
        .or_else(|| {
            // Before the first revision, or after retirement: show the nearest one.
            revs.iter()
                .rev()
                .find(|r| r.effective_from <= as_of)
                .or_else(|| revs.first())
        })
}

/// `POST …/institutions/{id}/positions` (created in `draft`, revision 1).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EstablishPosition {
    pub org_unit_id: OrgUnitId,
    #[schema(example = "HOD-CSE")]
    pub code: String,
    pub title: String,
    pub grade: Option<String>,
    pub description: Option<String>,
    /// Sanctioned headcount (default 1).
    pub headcount: Option<i32>,
    pub effective_from: NaiveDate,
}

fn attrs(
    title: Option<&str>,
    grade: Option<&str>,
    description: Option<&str>,
    headcount: Option<i32>,
    v: &mut Vec<FieldViolation>,
) {
    if let Some(t) = title {
        validate_text("title", t, 200, v);
    }
    if grade.is_some_and(|g| g.chars().count() > 50) {
        v.push(FieldViolation::new(
            "grade",
            "too-long",
            "grade must be at most 50 characters",
        ));
    }
    if description.is_some_and(|d| d.chars().count() > 2000) {
        v.push(FieldViolation::new(
            "description",
            "too-long",
            "description must be at most 2000 characters",
        ));
    }
    if headcount.is_some_and(|h| !(1..=10_000).contains(&h)) {
        v.push(FieldViolation::new(
            "headcount",
            "range",
            "headcount must be 1-10000",
        ));
    }
}

impl EstablishPosition {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        validate_code("code", &self.code, &mut v);
        validate_text("title", &self.title, 200, &mut v);
        attrs(
            None,
            self.grade.as_deref(),
            self.description.as_deref(),
            self.headcount,
            &mut v,
        );
        finish(v)
    }
}

/// `PATCH …/positions/{id}`: edits the (non-authoritative) draft in place.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateDraftPosition {
    pub org_unit_id: Option<OrgUnitId>,
    pub title: Option<String>,
    pub grade: Option<String>,
    pub description: Option<String>,
    pub headcount: Option<i32>,
    pub effective_from: Option<NaiveDate>,
}

impl UpdateDraftPosition {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        attrs(
            self.title.as_deref(),
            self.grade.as_deref(),
            self.description.as_deref(),
            self.headcount,
            &mut v,
        );
        finish(v)
    }
}

/// `POST …/positions/{id}/changes`: an effective-dated successor revision of an active
/// definition (absent members carry over from the latest revision).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ChangePosition {
    pub org_unit_id: Option<OrgUnitId>,
    pub title: Option<String>,
    pub grade: Option<String>,
    pub description: Option<String>,
    pub headcount: Option<i32>,
    /// Not before today (institution timezone) and after the latest revision's date.
    pub effective_from: NaiveDate,
    pub reason: String,
}

impl ChangePosition {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        attrs(
            self.title.as_deref(),
            self.grade.as_deref(),
            self.description.as_deref(),
            self.headcount,
            &mut v,
        );
        validate_text("reason", &self.reason, 1000, &mut v);
        finish(v)
    }

    /// The successor revision content built on `latest` (`None` = nothing changes).
    pub fn successor(&self, latest: &PositionRevision) -> Option<PositionRevision> {
        let norm = |o: &Option<String>, cur: &Option<String>| match o {
            Some(s) => crate::lifecycle::common::norm(Some(s)),
            None => cur.clone(),
        };
        let next = PositionRevision {
            revision: latest.revision + 1,
            org_unit_id: self.org_unit_id.unwrap_or(latest.org_unit_id),
            title: self
                .title
                .as_deref()
                .map(str::trim)
                .unwrap_or(&latest.title)
                .to_owned(),
            grade: norm(&self.grade, &latest.grade),
            description: norm(&self.description, &latest.description),
            headcount: self.headcount.unwrap_or(latest.headcount),
            effective_from: self.effective_from,
            effective_to: None,
            reason: Some(self.reason.trim().to_owned()),
            recorded_by: None,
            created_at: latest.created_at,
        };
        let same = next.org_unit_id == latest.org_unit_id
            && next.title == latest.title
            && next.grade == latest.grade
            && next.description == latest.description
            && next.headcount == latest.headcount;
        (!same).then_some(next)
    }
}

/// Successor date check (pure): never retroactive, strictly after the latest revision.
pub fn check_change_date(
    effective_from: NaiveDate,
    today: NaiveDate,
    latest_from: NaiveDate,
) -> Result<(), KernelError> {
    if effective_from < today {
        return Err(invalid(
            "effectiveFrom",
            "retroactive",
            format!("effectiveFrom must not be before today ({today}) in the institution timezone"),
        ));
    }
    if effective_from <= latest_from {
        return Err(invalid(
            "effectiveFrom",
            "overlap",
            format!("effectiveFrom must be after the latest revision's date ({latest_from})"),
        ));
    }
    Ok(())
}

/// `POST …/positions/{id}/retire`.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RetirePosition {
    pub reason: Option<String>,
    /// Last business-local day the definition applies (default today; not before today).
    pub effective_to: Option<NaiveDate>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        s.parse().unwrap_or_default()
    }

    fn rev(n: i32, from: &str, to: Option<&str>) -> PositionRevision {
        PositionRevision {
            revision: n,
            org_unit_id: OrgUnitId::new(),
            title: format!("T{n}"),
            grade: None,
            description: None,
            headcount: 1,
            effective_from: d(from),
            effective_to: to.map(d),
            reason: None,
            recorded_by: None,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn position_transitions() {
        use PositionStatus::*;
        for f in PositionStatus::ALL {
            for t in PositionStatus::ALL {
                let ok = matches!(
                    (f, t),
                    (Draft, Active) | (Active, Retired) | (Retired, Archived)
                );
                assert_eq!(can_transition(*f, *t), ok, "{f}->{t}");
            }
        }
        assert!(!can_transition(Retired, Active) && !can_transition(Archived, Active));
    }

    #[test]
    fn revision_in_force_by_date() {
        let revs = vec![
            rev(1, "2026-01-01", Some("2026-06-30")),
            rev(2, "2026-07-01", None),
        ];
        assert_eq!(
            in_force(&revs, d("2025-12-31")).map(|r| r.revision),
            Some(1)
        );
        assert_eq!(
            in_force(&revs, d("2026-03-01")).map(|r| r.revision),
            Some(1)
        );
        assert_eq!(
            in_force(&revs, d("2026-06-30")).map(|r| r.revision),
            Some(1)
        );
        assert_eq!(
            in_force(&revs, d("2026-07-01")).map(|r| r.revision),
            Some(2)
        );
        assert!(in_force(&[], d("2026-07-01")).is_none());
    }

    #[test]
    fn change_dates_and_successor() {
        assert!(check_change_date(d("2026-10-12"), d("2026-10-12"), d("2026-01-01")).is_ok());
        assert!(check_change_date(d("2026-10-11"), d("2026-10-12"), d("2026-01-01")).is_err());
        assert!(check_change_date(d("2026-11-01"), d("2026-10-12"), d("2026-11-01")).is_err());
        let latest = rev(1, "2026-01-01", None);
        let same = ChangePosition {
            title: Some(" T1 ".into()),
            effective_from: d("2026-11-01"),
            reason: "r".into(),
            ..Default::default()
        };
        assert!(same.successor(&latest).is_none());
        let change = ChangePosition {
            headcount: Some(3),
            ..same
        };
        let next = change
            .successor(&latest)
            .unwrap_or_else(|| panic!("changes"));
        assert_eq!(
            (next.revision, next.headcount, next.title.as_str()),
            (2, 3, "T1")
        );
        assert!(change.validate().is_ok());
        assert!(ChangePosition::default().validate().is_err());
    }
}
