//! **2C** — academic calendar, academic years/terms, scope assignment, successor revisions,
//! exceptions, year closure (L2-B05).

use super::event_type;
use chrono::NaiveDate;
use uuid::Uuid;

event_type!(
    /// Calendar version published ([`CalendarVersionPublishedV1`]); a successor supersedes its
    /// predecessor in the same transaction.
    CALENDAR_VERSION_PUBLISHED, CALENDAR_VERSION_PUBLISHED_SCHEMA, "calendar_version.published");
event_type!(
    /// Calendar retired, history preserved (StatusChangedV1, `aggregateId` = calendar id).
    CALENDAR_RETIRED, CALENDAR_RETIRED_SCHEMA, "calendar.retired");
event_type!(
    /// Calendar version retired (L4-B-033-BH-008), history preserved (StatusChangedV1,
    /// `aggregateId` = calendar version id). Added by slice 2C.
    CALENDAR_VERSION_RETIRED, CALENDAR_VERSION_RETIRED_SCHEMA, "calendar_version.retired");
event_type!(
    /// Academic year opened — the open academic context ([`AcademicYearV1`]).
    ACADEMIC_YEAR_OPENED, ACADEMIC_YEAR_OPENED_SCHEMA, "academic_year.opened");
event_type!(
    /// Academic year closed ([`AcademicYearV1`]).
    ACADEMIC_YEAR_CLOSED, ACADEMIC_YEAR_CLOSED_SCHEMA, "academic_year.closed");
event_type!(
    /// Calendar assignment became effective for a scope ([`CalendarAssignmentV1`]).
    CALENDAR_ASSIGNMENT_EFFECTIVE, CALENDAR_ASSIGNMENT_EFFECTIVE_SCHEMA,
    "calendar_assignment.effective");
event_type!(
    /// Calendar assignment retired (StatusChangedV1).
    CALENDAR_ASSIGNMENT_RETIRED, CALENDAR_ASSIGNMENT_RETIRED_SCHEMA, "calendar_assignment.retired");
event_type!(
    /// Exception approved ([`CalendarExceptionV1`]).
    CALENDAR_EXCEPTION_APPROVED, CALENDAR_EXCEPTION_APPROVED_SCHEMA, "calendar_exception.approved");
event_type!(
    /// Exception completed / ended (StatusChangedV1).
    CALENDAR_EXCEPTION_COMPLETED, CALENDAR_EXCEPTION_COMPLETED_SCHEMA,
    "calendar_exception.completed");

pub const EVENT_TYPES: &[&str] = &[
    CALENDAR_VERSION_PUBLISHED,
    CALENDAR_RETIRED,
    CALENDAR_VERSION_RETIRED,
    ACADEMIC_YEAR_OPENED,
    ACADEMIC_YEAR_CLOSED,
    CALENDAR_ASSIGNMENT_EFFECTIVE,
    CALENDAR_ASSIGNMENT_RETIRED,
    CALENDAR_EXCEPTION_APPROVED,
    CALENDAR_EXCEPTION_COMPLETED,
];

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarVersionPublishedV1 {
    pub calendar_id: Uuid,
    pub calendar_version_id: Uuid,
    pub institution_id: Uuid,
    pub academic_year_id: Uuid,
    pub version_no: i32,
    pub supersedes_version_id: Option<Uuid>,
    /// Business-local dates in `timezone`.
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    /// IANA zone the dates are expressed in (institution business timezone at publication).
    pub timezone: String,
    pub process_instance_id: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcademicYearV1 {
    pub academic_year_id: Uuid,
    pub institution_id: Uuid,
    pub calendar_id: Uuid,
    /// Display label, e.g. `2026-27`.
    pub label: String,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    /// For `…closed`: the successor year prepared by the closure, if any.
    pub successor_year_id: Option<Uuid>,
}

/// Governing scope a calendar is assigned to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalendarScopeKindV1 {
    Institution,
    Campus,
    OrgUnit,
    Programme,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAssignmentV1 {
    pub assignment_id: Uuid,
    pub calendar_id: Uuid,
    pub calendar_version_id: Uuid,
    pub institution_id: Uuid,
    pub scope_kind: CalendarScopeKindV1,
    /// Id of the institution / campus / org unit / programme.
    pub scope_id: Uuid,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarExceptionV1 {
    pub exception_id: Uuid,
    pub calendar_id: Uuid,
    pub institution_id: Uuid,
    /// e.g. `closure`, `rescheduled_working_day`, `extended_term`.
    pub kind: String,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub reason: String,
    pub process_instance_id: Option<String>,
}
