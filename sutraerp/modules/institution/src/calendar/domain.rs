//! Pure academic-calendar domain (L2-B05): DTOs, commands, validation, lifecycle transitions,
//! closure evidence and the change-simulation impact model. No I/O.
//!
//! Business dates (`NaiveDate`) are always interpreted in the institution business timezone
//! (ADR-L5-0036 §3.4); "today" is computed by the repository in that zone and passed in.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc, Weekday};
use sutra_contracts::institution::ids::{
    AcademicCalendarId, AcademicTermId, AcademicYearId, CalendarAssignmentId, CalendarExceptionId,
    CalendarSimulationId, CalendarVersionId, KeyDateId,
};
use sutra_contracts::institution::status::{
    AcademicYearStatus, CalendarAssignmentStatus, CalendarExceptionStatus, PublicationStatus,
    SimulationStatus,
};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::InstitutionId;
use uuid::Uuid;

/// Longest academic year / calendar version accepted (two years incl. a leap day).
pub const MAX_SPAN_DAYS: i64 = 731;

// ---------------------------------------------------------------------------------------------
// Lifecycle transitions (contract doc §4.2: slice-owned, pure, unit-tested)
// ---------------------------------------------------------------------------------------------

/// Academic calendar (the series): Draft → Published (first version published) → Retired.
pub fn calendar_can_transition(from: PublicationStatus, to: PublicationStatus) -> bool {
    use PublicationStatus::*;
    matches!(
        (from, to),
        (Draft, Published) | (Draft, Retired) | (Published, Retired)
    )
}

/// Calendar version: Draft → InReview → Published → Superseded; Published → Retired (B-033);
/// InReview → Draft (returned); Draft → Retired (discarded draft). Retired is terminal
/// (L4-B-033-BH-007) and Published is immutable (edits go to a successor, L4-B-029-BH-001).
pub fn version_can_transition(from: PublicationStatus, to: PublicationStatus) -> bool {
    use PublicationStatus::*;
    matches!(
        (from, to),
        (Draft, InReview)
            | (InReview, Published)
            | (InReview, Draft)
            | (Published, Superseded)
            | (Published, Retired)
            | (Draft, Retired)
    )
}

/// Academic year: Planned → Open → Closing → Closed; Closing → Open (closure not approved).
pub fn year_can_transition(from: AcademicYearStatus, to: AcademicYearStatus) -> bool {
    use AcademicYearStatus::*;
    matches!(
        (from, to),
        (Planned, Open) | (Open, Closing) | (Closing, Closed) | (Closing, Open)
    )
}

/// Assignment: Draft → Effective → Retired; Draft → Retired (never governed).
pub fn assignment_can_transition(
    from: CalendarAssignmentStatus,
    to: CalendarAssignmentStatus,
) -> bool {
    use CalendarAssignmentStatus::*;
    matches!(
        (from, to),
        (Draft, Effective) | (Draft, Retired) | (Effective, Retired)
    )
}

/// Exception: Requested → Approved | Rejected | Withdrawn; Approved → InEffect (applied) |
/// Withdrawn | Completed (expired unapplied); InEffect → Completed.
pub fn exception_can_transition(
    from: CalendarExceptionStatus,
    to: CalendarExceptionStatus,
) -> bool {
    use CalendarExceptionStatus::*;
    matches!(
        (from, to),
        (Requested, Approved)
            | (Requested, Rejected)
            | (Requested, Withdrawn)
            | (Approved, InEffect)
            | (Approved, Withdrawn)
            | (Approved, Completed)
            | (InEffect, Completed)
    )
}

/// Simulation: Draft → Evaluated → Accepted | Discarded.
pub fn simulation_can_transition(from: SimulationStatus, to: SimulationStatus) -> bool {
    use SimulationStatus::*;
    matches!(
        (from, to),
        (Draft, Evaluated) | (Evaluated, Accepted) | (Evaluated, Discarded)
    )
}

/// 409 for an illegal transition.
pub fn illegal(
    what: &str,
    from: impl std::fmt::Display,
    to: impl std::fmt::Display,
) -> KernelError {
    KernelError::Conflict(format!("{what} cannot move from {from} to {to}"))
}

// ---------------------------------------------------------------------------------------------
// Enumerations
// ---------------------------------------------------------------------------------------------

/// Governing scope a calendar is assigned to (contract §4.3 `scopeKind`).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ScopeKind {
    Institution,
    Campus,
    OrgUnit,
    Programme,
}

impl ScopeKind {
    pub const ALL: &'static [ScopeKind] = &[
        ScopeKind::Institution,
        ScopeKind::Campus,
        ScopeKind::OrgUnit,
        ScopeKind::Programme,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Institution => "institution",
            Self::Campus => "campus",
            Self::OrgUnit => "org_unit",
            Self::Programme => "programme",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
    /// Resolution precedence: the most specific scope wins (programme > unit > campus > institution).
    pub fn precedence(self) -> u8 {
        match self {
            Self::Programme => 4,
            Self::OrgUnit => 3,
            Self::Campus => 2,
            Self::Institution => 1,
        }
    }
    /// Table of the owning slice that holds this scope (`None` = the institution itself).
    pub fn table(self) -> Option<&'static str> {
        match self {
            Self::Institution => None,
            Self::Campus => Some("institution.campus"),
            Self::OrgUnit => Some("institution.org_unit"),
            Self::Programme => Some("institution.programme"),
        }
    }
}

/// Kind of a key date of a calendar version.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum KeyDateKind {
    Holiday,
    InstructionStart,
    InstructionEnd,
    ExamWindow,
    RegistrationWindow,
    Break,
    Event,
}

impl KeyDateKind {
    pub const ALL: &'static [KeyDateKind] = &[
        Self::Holiday,
        Self::InstructionStart,
        Self::InstructionEnd,
        Self::ExamWindow,
        Self::RegistrationWindow,
        Self::Break,
        Self::Event,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Holiday => "holiday",
            Self::InstructionStart => "instruction_start",
            Self::InstructionEnd => "instruction_end",
            Self::ExamWindow => "exam_window",
            Self::RegistrationWindow => "registration_window",
            Self::Break => "break",
            Self::Event => "event",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
    /// Days of this kind are not instructional days.
    pub fn non_instructional(self) -> bool {
        matches!(self, Self::Holiday | Self::Break)
    }
}

/// Kind of a temporary calendar deviation.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ExceptionKind {
    Closure,
    RescheduledWorkingDay,
    ExtendedTerm,
    AdditionalHoliday,
    Other,
}

impl ExceptionKind {
    pub const ALL: &'static [ExceptionKind] = &[
        Self::Closure,
        Self::RescheduledWorkingDay,
        Self::ExtendedTerm,
        Self::AdditionalHoliday,
        Self::Other,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Closure => "closure",
            Self::RescheduledWorkingDay => "rescheduled_working_day",
            Self::ExtendedTerm => "extended_term",
            Self::AdditionalHoliday => "additional_holiday",
            Self::Other => "other",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|k| k.as_str() == s)
    }
}

// ---------------------------------------------------------------------------------------------
// Resources (API DTOs)
// ---------------------------------------------------------------------------------------------

/// Academic calendar: the stable identity of a calendar series (versions live under it).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Calendar {
    pub id: AcademicCalendarId,
    pub institution_id: InstitutionId,
    pub code: String,
    pub name: String,
    pub description: Option<String>,
    pub status: PublicationStatus,
    pub status_reason: Option<String>,
    pub retired_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AcademicYear {
    pub id: AcademicYearId,
    pub institution_id: InstitutionId,
    pub calendar_id: AcademicCalendarId,
    /// e.g. `2026-27`.
    pub label: String,
    pub status: AcademicYearStatus,
    /// Business-local dates (institution timezone).
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub predecessor_year_id: Option<AcademicYearId>,
    /// Next-cycle year prepared by this year's closure (draft foundation, never opened automatically).
    pub successor_year_id: Option<AcademicYearId>,
    pub opened_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Term {
    pub id: AcademicTermId,
    pub code: String,
    pub name: String,
    pub sequence: i32,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct KeyDate {
    pub id: KeyDateId,
    pub kind: KeyDateKind,
    pub name: String,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

/// One calendar version (Draft → Published → Superseded / Retired) with its terms and key dates.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CalendarVersion {
    pub id: CalendarVersionId,
    pub calendar_id: AcademicCalendarId,
    pub institution_id: InstitutionId,
    pub academic_year_id: AcademicYearId,
    pub version_no: i32,
    pub status: PublicationStatus,
    pub supersedes_version_id: Option<CalendarVersionId>,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    /// IANA zone the dates are expressed in: the snapshot taken at publication, or the current
    /// institution business timezone while the version is a draft.
    pub timezone: String,
    pub notes: Option<String>,
    pub process_instance_id: Option<String>,
    pub status_reason: Option<String>,
    pub published_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub terms: Vec<Term>,
    pub key_dates: Vec<KeyDate>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CalendarAssignment {
    pub id: CalendarAssignmentId,
    pub institution_id: InstitutionId,
    pub calendar_id: AcademicCalendarId,
    pub calendar_version_id: CalendarVersionId,
    pub scope_kind: ScopeKind,
    pub scope_id: Uuid,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
    pub status: CalendarAssignmentStatus,
    pub status_reason: Option<String>,
    /// `effective` and today (institution timezone) inside the effective period.
    pub governing: bool,
    pub activated_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CalendarException {
    pub id: CalendarExceptionId,
    pub institution_id: InstitutionId,
    pub calendar_id: AcademicCalendarId,
    /// The immutable base version this deviation applies to (never rewritten).
    pub calendar_version_id: CalendarVersionId,
    pub kind: ExceptionKind,
    pub scope_kind: ScopeKind,
    pub scope_id: Uuid,
    pub starts_on: NaiveDate,
    /// Expiry (inclusive): the deviation stops governing after this business date.
    pub ends_on: NaiveDate,
    pub reason: String,
    /// Accountable owner (subject of the requester).
    pub owner_subject: String,
    pub status: CalendarExceptionStatus,
    /// `in_effect` and today (institution timezone) inside the period.
    pub governing: bool,
    pub process_instance_id: Option<String>,
    pub decision_reason: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub applied_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// A recorded change simulation: point-in-time baseline + proposal + impact report.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CalendarSimulation {
    pub id: CalendarSimulationId,
    pub institution_id: InstitutionId,
    pub calendar_id: AcademicCalendarId,
    pub baseline_version_id: CalendarVersionId,
    pub status: SimulationStatus,
    pub outcome: SimulationOutcome,
    pub baseline: SimulationBaseline,
    pub proposal: SimulationProposal,
    pub impact: ImpactReport,
    pub created_by: Option<String>,
    pub decision_reason: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

macro_rules! list {
    ($(#[$m:meta])* $name:ident, $item:ty) => {
        $(#[$m])*
        #[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
        pub struct $name {
            pub items: Vec<$item>,
        }
    };
}
list!(/// `{ "items": [...] }` collection of calendars.
    CalendarList, Calendar);
list!(/// `{ "items": [...] }` collection of academic years.
    AcademicYearList, AcademicYear);
list!(/// `{ "items": [...] }` collection of calendar versions.
    CalendarVersionList, CalendarVersion);
list!(/// `{ "items": [...] }` collection of calendar assignments.
    CalendarAssignmentList, CalendarAssignment);
list!(/// `{ "items": [...] }` collection of calendar exceptions.
    CalendarExceptionList, CalendarException);
list!(/// `{ "items": [...] }` collection of calendar simulations.
    CalendarSimulationList, CalendarSimulation);

/// Which calendar governs a scope on a business date.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CalendarResolution {
    pub scope_kind: ScopeKind,
    pub scope_id: Uuid,
    /// Business date resolved (defaults to today in the institution timezone).
    pub on: NaiveDate,
    pub timezone: String,
    /// The scope the governing assignment was found on (fallback to the institution).
    pub resolved_via: Option<ScopeKind>,
    pub assignment: Option<CalendarAssignment>,
    pub calendar_version: Option<CalendarVersion>,
    /// Exceptions of that calendar in effect on that date for the scope or the institution.
    pub exceptions: Vec<CalendarException>,
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewCalendar {
    /// `A-Z0-9-`, 2–32 chars, unique per institution.
    #[schema(example = "MAIN")]
    pub code: String,
    #[schema(example = "Main academic calendar")]
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAcademicYear {
    pub calendar_id: AcademicCalendarId,
    #[schema(example = "2026-27")]
    pub label: String,
    #[schema(example = "2026-06-01")]
    pub starts_on: NaiveDate,
    #[schema(example = "2027-05-31")]
    pub ends_on: NaiveDate,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewCalendarVersion {
    pub academic_year_id: AcademicYearId,
    /// Defaults to the academic year's dates.
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub notes: Option<String>,
}

/// Draft-only changes (a published version is immutable — revise it instead).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateCalendarVersion {
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewTerm {
    #[schema(example = "T1")]
    pub code: String,
    #[schema(example = "Semester 1")]
    pub name: String,
    pub sequence: i32,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewKeyDate {
    pub kind: KeyDateKind,
    #[schema(example = "Independence Day")]
    pub name: String,
    pub starts_on: NaiveDate,
    /// Defaults to `startsOn` (single day).
    pub ends_on: Option<NaiveDate>,
}

/// Optional reason carried by a state command.
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StateCommand {
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAssignment {
    pub calendar_version_id: CalendarVersionId,
    pub scope_kind: ScopeKind,
    /// Required except for `institution` (defaults to the calendar's institution).
    pub scope_id: Option<Uuid>,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewException {
    pub calendar_version_id: CalendarVersionId,
    pub kind: ExceptionKind,
    /// Defaults to `institution`.
    pub scope_kind: Option<ScopeKind>,
    pub scope_id: Option<Uuid>,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    #[schema(example = "Heavy rain: campus closed by district order")]
    pub reason: String,
}

/// Decision on a pending approval step (used with `ApprovalMode::AwaitDecision`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecisionKind {
    Approve,
    /// Reject (exception) / return to draft (version) / keep the year open (closure).
    Reject,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Decision {
    pub decision: DecisionKind,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SuccessorYear {
    pub label: Option<String>,
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CloseAcademicYear {
    pub reason: Option<String>,
    /// Governed exception permitting closure before the year's end date (recorded as evidence).
    pub early_closure_reason: Option<String>,
    /// Next-cycle foundation (planned year + draft version). Defaults: next label
    /// (`2026-27` → `2027-28`), the day after this year ends, same length.
    pub successor: Option<SuccessorYear>,
    /// `false` = close without preparing a next-cycle foundation.
    #[serde(default = "yes")]
    pub prepare_successor: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewSimulation {
    pub baseline_version_id: CalendarVersionId,
    pub proposal: SimulationProposal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SimulationDecisionKind {
    Accept,
    Discard,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SimulationDecision {
    pub decision: SimulationDecisionKind,
    pub reason: Option<String>,
}

// ---------------------------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------------------------

fn finish(v: Vec<FieldViolation>) -> Result<(), KernelError> {
    if v.is_empty() {
        Ok(())
    } else {
        Err(KernelError::Validation(v))
    }
}

fn text(field: &str, value: &str, max: usize, v: &mut Vec<FieldViolation>) {
    let len = value.trim().chars().count();
    if len == 0 {
        v.push(FieldViolation::new(
            field,
            "required",
            format!("{field} is required"),
        ));
    } else if len > max {
        v.push(FieldViolation::new(
            field,
            "too-long",
            format!("{field} must be at most {max} characters"),
        ));
    }
}

fn code(field: &str, value: &str, v: &mut Vec<FieldViolation>) {
    let c = value.trim();
    if !(1..=32).contains(&c.len())
        || !c
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-')
    {
        v.push(FieldViolation::new(
            field,
            "format",
            format!("{field} must be 1-32 characters of A-Z, 0-9 or '-'"),
        ));
    }
}

fn range(
    from_field: &str,
    start: NaiveDate,
    end: NaiveDate,
    allow_same_day: bool,
    v: &mut Vec<FieldViolation>,
) {
    let ok = if allow_same_day {
        end >= start
    } else {
        end > start
    };
    if !ok {
        v.push(FieldViolation::new(
            from_field,
            "range",
            "the end date must be after the start date",
        ));
    } else if (end - start).num_days() > MAX_SPAN_DAYS {
        v.push(FieldViolation::new(
            from_field,
            "range",
            format!("a period may span at most {MAX_SPAN_DAYS} days"),
        ));
    }
}

impl NewCalendar {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        code("code", &self.code, &mut v);
        text("name", &self.name, 200, &mut v);
        if self
            .description
            .as_deref()
            .is_some_and(|d| d.chars().count() > 2000)
        {
            v.push(FieldViolation::new(
                "description",
                "too-long",
                "at most 2000 characters",
            ));
        }
        finish(v)
    }
}

impl NewAcademicYear {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        text("label", &self.label, 40, &mut v);
        range("endsOn", self.starts_on, self.ends_on, false, &mut v);
        finish(v)
    }
}

impl NewTerm {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        code("code", &self.code, &mut v);
        text("name", &self.name, 120, &mut v);
        if !(1..=24).contains(&self.sequence) {
            v.push(FieldViolation::new(
                "sequence",
                "range",
                "sequence must be 1-24",
            ));
        }
        range("endsOn", self.starts_on, self.ends_on, false, &mut v);
        finish(v)
    }
}

impl NewKeyDate {
    pub fn ends(&self) -> NaiveDate {
        self.ends_on.unwrap_or(self.starts_on)
    }
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        text("name", &self.name, 120, &mut v);
        range("endsOn", self.starts_on, self.ends(), true, &mut v);
        finish(v)
    }
}

impl NewException {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        text("reason", &self.reason, 1000, &mut v);
        range("endsOn", self.starts_on, self.ends_on, true, &mut v);
        if self.scope_kind.unwrap_or(ScopeKind::Institution) != ScopeKind::Institution
            && self.scope_id.is_none()
        {
            v.push(FieldViolation::new(
                "scopeId",
                "required",
                "scopeId is required for this scope",
            ));
        }
        finish(v)
    }
}

impl NewAssignment {
    pub fn validate(&self) -> Result<(), KernelError> {
        let mut v = vec![];
        if let Some(to) = self.effective_to {
            if to < self.effective_from {
                v.push(FieldViolation::new(
                    "effectiveTo",
                    "range",
                    "effectiveTo must not be before effectiveFrom",
                ));
            }
        }
        if self.scope_kind != ScopeKind::Institution && self.scope_id.is_none() {
            v.push(FieldViolation::new(
                "scopeId",
                "required",
                "scopeId is required for this scope",
            ));
        }
        finish(v)
    }
}

/// Closed intervals overlap (inclusive business dates; `None` end = open-ended).
pub fn overlaps(
    a_from: NaiveDate,
    a_to: Option<NaiveDate>,
    b_from: NaiveDate,
    b_to: Option<NaiveDate>,
) -> bool {
    let a_end = a_to.unwrap_or(NaiveDate::MAX);
    let b_end = b_to.unwrap_or(NaiveDate::MAX);
    a_from <= b_end && b_from <= a_end
}

/// Definition checks before a version may be published (L4-B-027-BH-002): at least one term,
/// terms and key dates inside the version period, the version inside its academic year, terms
/// not overlapping and sequences unique.
pub fn validate_definition(
    starts_on: NaiveDate,
    ends_on: NaiveDate,
    year: (NaiveDate, NaiveDate),
    terms: &[(String, i32, NaiveDate, NaiveDate)],
    key_dates: &[(String, NaiveDate, NaiveDate)],
) -> Vec<FieldViolation> {
    let mut v = vec![];
    if ends_on <= starts_on {
        v.push(FieldViolation::new(
            "endsOn",
            "range",
            "the version must end after it starts",
        ));
    }
    if starts_on < year.0 || ends_on > year.1 {
        v.push(FieldViolation::new(
            "startsOn",
            "outside-year",
            format!(
                "the version must lie inside its academic year ({} – {})",
                year.0, year.1
            ),
        ));
    }
    if terms.is_empty() {
        v.push(FieldViolation::new(
            "terms",
            "required",
            "add at least one term",
        ));
    }
    let mut sorted: Vec<_> = terms.iter().collect();
    sorted.sort_by_key(|t| t.2);
    for t in &sorted {
        if t.2 < starts_on || t.3 > ends_on {
            v.push(FieldViolation::new(
                "terms",
                "outside-version",
                format!("term {} lies outside the version period", t.0),
            ));
        }
    }
    for w in sorted.windows(2) {
        if let [a, b] = w {
            if b.2 <= a.3 {
                v.push(FieldViolation::new(
                    "terms",
                    "overlap",
                    format!("terms {} and {} overlap", a.0, b.0),
                ));
            }
        }
    }
    let mut seqs: Vec<i32> = terms.iter().map(|t| t.1).collect();
    seqs.sort_unstable();
    if seqs.windows(2).any(|w| w[0] == w[1]) {
        v.push(FieldViolation::new(
            "terms",
            "sequence",
            "term sequences must be unique",
        ));
    }
    for k in key_dates {
        if k.1 < starts_on || k.2 > ends_on {
            v.push(FieldViolation::new(
                "keyDates",
                "outside-version",
                format!("key date {} lies outside the version period", k.0),
            ));
        }
    }
    v
}

/// Next academic-year label: `2026-27` → `2027-28`, `2026-2027` → `2027-2028`, `2026` → `2027`,
/// anything else gets a ` (next)` suffix.
pub fn next_label(label: &str) -> String {
    let l = label.trim();
    let parts: Vec<&str> = l.split(['-', '/']).collect();
    let sep = if l.contains('/') { "/" } else { "-" };
    let num = |s: &str| s.parse::<u32>().ok();
    match parts.as_slice() {
        [a] if a.len() == 4 => num(a).map(|y| (y + 1).to_string()),
        [a, b] if a.len() == 4 && (b.len() == 2 || b.len() == 4) => match (num(a), num(b)) {
            (Some(y), Some(z)) => Some(if b.len() == 2 {
                format!("{}{sep}{:02}", y + 1, (z + 1) % 100)
            } else {
                format!("{}{sep}{}", y + 1, z + 1)
            }),
            _ => None,
        },
        _ => None,
    }
    .unwrap_or_else(|| format!("{l} (next)"))
}

/// Default successor dates: the day after `ends_on` until the same calendar date one year
/// later (`2026-06-01 – 2027-05-31` → `2027-06-01 – 2028-05-31`).
pub fn successor_dates(starts_on: NaiveDate, ends_on: NaiveDate) -> (NaiveDate, NaiveDate) {
    let s = ends_on + Duration::days(1);
    let e = ends_on
        .checked_add_months(chrono::Months::new(12))
        .unwrap_or(s + (ends_on - starts_on));
    (s, e)
}

/// Shifts a period by `days`, clamped to `limit`; `None` when it falls outside entirely.
pub fn shift_into(
    start: NaiveDate,
    end: NaiveDate,
    days: i64,
    limit: (NaiveDate, NaiveDate),
) -> Option<(NaiveDate, NaiveDate)> {
    let (s, e) = (start + Duration::days(days), end + Duration::days(days));
    let (s, e) = (s.max(limit.0), e.min(limit.1));
    (s <= e).then_some((s, e))
}

// ---------------------------------------------------------------------------------------------
// Academic year closure evidence (L4-B-031)
// ---------------------------------------------------------------------------------------------

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStatus {
    Satisfied,
    /// Missing, but a governed exception (early-closure reason) permits closure.
    Waived,
    Missing,
    Failed,
    /// The owning domain is not deployed yet; recorded, not assumed.
    NotAvailable,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ClosureEvidence {
    pub key: String,
    /// Owning domain of the evidence (Academic Calendar evaluates, never modifies it).
    pub owner: String,
    pub mandatory: bool,
    pub status: EvidenceStatus,
    pub detail: String,
}

impl ClosureEvidence {
    pub fn blocks(&self) -> bool {
        self.mandatory
            && matches!(
                self.status,
                EvidenceStatus::Missing | EvidenceStatus::Failed
            )
    }
}

/// Inputs to [`closure_evidence`], gathered by the repository.
pub struct ClosureFacts {
    pub today: NaiveDate,
    pub ends_on: NaiveDate,
    pub published_versions: i64,
    pub pending_exceptions: i64,
    pub early_closure_reason: Option<String>,
}

/// Evaluates the closure evidence (L4-B-031-BH-001/-002).
pub fn closure_evidence(f: &ClosureFacts) -> Vec<ClosureEvidence> {
    let early = f
        .early_closure_reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let ended = f.today > f.ends_on;
    vec![
        ClosureEvidence {
            key: "calendar.year_ended".into(),
            owner: "academic_calendar".into(),
            mandatory: true,
            status: match (ended, early) {
                (true, _) => EvidenceStatus::Satisfied,
                (false, Some(_)) => EvidenceStatus::Waived,
                (false, None) => EvidenceStatus::Missing,
            },
            detail: match (ended, early) {
                (true, _) => format!("the year ended on {}", f.ends_on),
                (false, Some(r)) => format!("early closure permitted: {r}"),
                (false, None) => format!(
                    "the year runs until {} (today is {}); give an early-closure reason to close it now",
                    f.ends_on, f.today
                ),
            },
        },
        ClosureEvidence {
            key: "calendar.version_published".into(),
            owner: "academic_calendar".into(),
            mandatory: true,
            status: if f.published_versions > 0 {
                EvidenceStatus::Satisfied
            } else {
                EvidenceStatus::Missing
            },
            detail: format!("{} published calendar version(s)", f.published_versions),
        },
        ClosureEvidence {
            key: "calendar.no_pending_exceptions".into(),
            owner: "academic_calendar".into(),
            mandatory: true,
            status: if f.pending_exceptions == 0 {
                EvidenceStatus::Satisfied
            } else {
                EvidenceStatus::Failed
            },
            detail: format!(
                "{} exception request(s) still awaiting a decision",
                f.pending_exceptions
            ),
        },
        ClosureEvidence {
            key: "assessment.results_finalised".into(),
            owner: "assessment".into(),
            mandatory: false,
            status: EvidenceStatus::NotAvailable,
            detail: "the assessment domain is not deployed yet".into(),
        },
        ClosureEvidence {
            key: "enrolment.records_closed".into(),
            owner: "student_lifecycle".into(),
            mandatory: false,
            status: EvidenceStatus::NotAvailable,
            detail: "the student lifecycle domain is not deployed yet".into(),
        },
    ]
}

// ---------------------------------------------------------------------------------------------
// Change simulation (L4-B-032) — pure impact evaluation
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BaselineAssignment {
    pub id: Uuid,
    pub scope_kind: ScopeKind,
    pub scope_id: Uuid,
    pub status: CalendarAssignmentStatus,
    pub effective_from: NaiveDate,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BaselineException {
    pub id: Uuid,
    pub kind: ExceptionKind,
    pub status: CalendarExceptionStatus,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
}

/// Immutable point-in-time baseline a simulation is bound to (L4-B-032-BH-001).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SimulationBaseline {
    pub calendar_version_id: CalendarVersionId,
    pub version_no: i32,
    pub status: PublicationStatus,
    /// Aggregate version (`ETag`) of the baseline at simulation time.
    pub aggregate_version: i64,
    pub timezone: String,
    pub year_starts_on: NaiveDate,
    pub year_ends_on: NaiveDate,
    pub starts_on: NaiveDate,
    pub ends_on: NaiveDate,
    pub terms: Vec<NewTerm>,
    pub key_dates: Vec<NewKeyDate>,
    pub assignments: Vec<BaselineAssignment>,
    pub exceptions: Vec<BaselineException>,
    pub captured_at: DateTime<Utc>,
}

/// Proposed change; omitted members keep the baseline's value.
#[derive(
    Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct SimulationProposal {
    pub starts_on: Option<NaiveDate>,
    pub ends_on: Option<NaiveDate>,
    pub terms: Option<Vec<NewTerm>>,
    pub key_dates: Option<Vec<NewKeyDate>>,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SimulationOutcome {
    Favourable,
    Unfavourable,
    /// Material evidence is missing; nothing was assumed (L4-B-032-BH-004).
    Uncertain,
}

impl SimulationOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Favourable => "favourable",
            Self::Unfavourable => "unfavourable",
            Self::Uncertain => "uncertain",
        }
    }
    pub fn parse(s: &str) -> Option<Self> {
        [Self::Favourable, Self::Unfavourable, Self::Uncertain]
            .into_iter()
            .find(|o| o.as_str() == s)
    }
}

#[derive(
    Clone,
    Copy,
    Debug,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
    utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ImpactSeverity {
    Info,
    Warning,
    Uncertain,
    Blocking,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImpactItem {
    /// Stable code, e.g. `term.outside_period`.
    pub code: String,
    pub severity: ImpactSeverity,
    pub message: String,
    /// Ids of affected records (assignments, exceptions) where applicable.
    pub affected: Vec<Uuid>,
    /// Where the evidence came from (`baseline`, `proposal`, or the owning domain).
    pub provenance: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImpactReport {
    pub outcome: SimulationOutcome,
    pub baseline_instructional_days: i64,
    pub proposed_instructional_days: i64,
    pub period_shift_days: i64,
    pub items: Vec<ImpactItem>,
}

/// Instructional days: weekdays (Mon–Fri) inside a term, minus holidays/breaks.
pub fn instructional_days(terms: &[NewTerm], key_dates: &[NewKeyDate]) -> i64 {
    let mut n = 0;
    for t in terms {
        let mut d = t.starts_on;
        while d <= t.ends_on {
            let weekday = !matches!(d.weekday(), Weekday::Sat | Weekday::Sun);
            let off = key_dates
                .iter()
                .any(|k| k.kind.non_instructional() && k.starts_on <= d && d <= k.ends());
            if weekday && !off {
                n += 1;
            }
            match d.succ_opt() {
                Some(next) => d = next,
                None => break,
            }
        }
    }
    n
}

fn item(
    code: &str,
    severity: ImpactSeverity,
    message: String,
    affected: Vec<Uuid>,
    provenance: &str,
) -> ImpactItem {
    ImpactItem {
        code: code.into(),
        severity,
        message,
        affected,
        provenance: provenance.into(),
    }
}

/// Evaluates a proposal against its baseline. Pure: it reads only its inputs and changes nothing
/// (L4-B-032-BH-002); a favourable outcome authorises nothing (BH-006).
pub fn simulate(b: &SimulationBaseline, p: &SimulationProposal) -> ImpactReport {
    use ImpactSeverity::*;
    let starts = p.starts_on.unwrap_or(b.starts_on);
    let ends = p.ends_on.unwrap_or(b.ends_on);
    let terms = p.terms.clone().unwrap_or_else(|| b.terms.clone());
    let keys = p.key_dates.clone().unwrap_or_else(|| b.key_dates.clone());
    let mut items = vec![];

    if ends <= starts {
        items.push(item(
            "period.invalid",
            Blocking,
            "the proposed period ends before it starts".into(),
            vec![],
            "proposal",
        ));
    }
    if starts < b.year_starts_on || ends > b.year_ends_on {
        items.push(item(
            "period.outside_year",
            Blocking,
            format!(
                "the proposed period leaves the academic year ({} – {})",
                b.year_starts_on, b.year_ends_on
            ),
            vec![],
            "proposal",
        ));
    }
    let shift = (starts - b.starts_on).num_days();
    if starts != b.starts_on || ends != b.ends_on {
        let delta = (ends - starts).num_days() - (b.ends_on - b.starts_on).num_days();
        items.push(item(
            "period.changed",
            Info,
            format!(
                "period {} – {} becomes {} – {} ({:+} days long)",
                b.starts_on, b.ends_on, starts, ends, delta
            ),
            vec![],
            "proposal",
        ));
    }
    let defs: Vec<_> = terms
        .iter()
        .map(|t| (t.code.clone(), t.sequence, t.starts_on, t.ends_on))
        .collect();
    let kd: Vec<_> = keys
        .iter()
        .map(|k| (k.name.clone(), k.starts_on, k.ends()))
        .collect();
    for violation in
        validate_definition(starts, ends, (b.year_starts_on, b.year_ends_on), &defs, &kd)
    {
        if violation.code == "outside-year" {
            continue; // reported above
        }
        let severity = if violation.field == "keyDates" {
            Warning
        } else {
            Blocking
        };
        items.push(item(
            &format!("{}.{}", violation.field, violation.code.replace('-', "_")),
            severity,
            violation.message,
            vec![],
            "proposal",
        ));
    }

    // Governing assignments of the baseline whose period the proposal no longer covers.
    let governed: Vec<&BaselineAssignment> = b
        .assignments
        .iter()
        .filter(|a| a.status != CalendarAssignmentStatus::Retired)
        .collect();
    let uncovered: Vec<Uuid> = governed
        .iter()
        .filter(|a| a.effective_from < starts || a.effective_to.is_some_and(|t| t > ends))
        .map(|a| a.id)
        .collect();
    if !uncovered.is_empty() {
        items.push(item(
            "assignment.period_uncovered",
            Warning,
            format!(
                "{} assignment(s) would govern dates outside the proposed period",
                uncovered.len()
            ),
            uncovered,
            "baseline",
        ));
    }
    let live_exceptions: Vec<Uuid> = b
        .exceptions
        .iter()
        .filter(|e| {
            matches!(
                e.status,
                CalendarExceptionStatus::Requested
                    | CalendarExceptionStatus::Approved
                    | CalendarExceptionStatus::InEffect
            ) && (e.starts_on < starts || e.ends_on > ends)
        })
        .map(|e| e.id)
        .collect();
    if !live_exceptions.is_empty() {
        items.push(item(
            "exception.outside_period",
            Warning,
            format!(
                "{} exception(s) fall outside the proposed period",
                live_exceptions.len()
            ),
            live_exceptions,
            "baseline",
        ));
    }
    let base_days = instructional_days(&b.terms, &b.key_dates);
    let new_days = instructional_days(&terms, &keys);
    if new_days != base_days {
        items.push(item(
            "instruction.days_changed",
            if new_days < base_days { Warning } else { Info },
            format!("instructional days {base_days} → {new_days}"),
            vec![],
            "proposal",
        ));
    }
    // Downstream consumers (timetables, attendance, exams) are not deployed yet: if the calendar
    // governs anything, their impact is unknown — report it instead of assuming none.
    let changed =
        starts != b.starts_on || ends != b.ends_on || terms != b.terms || keys != b.key_dates;
    if changed
        && governed
            .iter()
            .any(|a| a.status == CalendarAssignmentStatus::Effective)
    {
        items.push(item(
            "downstream.unknown",
            Uncertain,
            "the calendar governs scopes whose timetables, attendance and exams are not yet visible to the simulation".into(),
            governed.iter().map(|a| a.id).collect(),
            "timetable, attendance, assessment (not deployed)",
        ));
    }
    let worst = items.iter().map(|i| i.severity).max().unwrap_or(Info);
    ImpactReport {
        outcome: match worst {
            Blocking => SimulationOutcome::Unfavourable,
            Uncertain => SimulationOutcome::Uncertain,
            _ => SimulationOutcome::Favourable,
        },
        baseline_instructional_days: base_days,
        proposed_instructional_days: new_days,
        period_shift_days: shift,
        items,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").expect("date")
    }

    #[test]
    fn version_transitions_follow_the_publication_lifecycle() {
        use PublicationStatus::*;
        assert!(version_can_transition(Draft, InReview));
        assert!(version_can_transition(InReview, Published));
        assert!(version_can_transition(Published, Superseded));
        assert!(version_can_transition(Published, Retired));
        // Published is immutable / Retired is terminal (L4-B-033-BH-007).
        for to in PublicationStatus::ALL {
            assert!(!version_can_transition(Retired, *to), "retired → {to}");
        }
        assert!(!version_can_transition(Published, Draft));
        assert!(!version_can_transition(Draft, Published));
        assert!(!version_can_transition(Superseded, Published));
        assert!(calendar_can_transition(Draft, Published));
        assert!(!calendar_can_transition(Retired, Published));
    }

    #[test]
    fn year_assignment_exception_and_simulation_transitions() {
        use AcademicYearStatus as Y;
        assert!(year_can_transition(Y::Planned, Y::Open));
        assert!(year_can_transition(Y::Closing, Y::Closed));
        assert!(!year_can_transition(Y::Planned, Y::Closed));
        assert!(!year_can_transition(Y::Closed, Y::Open));
        use CalendarAssignmentStatus as A;
        assert!(assignment_can_transition(A::Draft, A::Effective));
        assert!(!assignment_can_transition(A::Retired, A::Effective));
        use CalendarExceptionStatus as E;
        assert!(exception_can_transition(E::Approved, E::InEffect));
        assert!(!exception_can_transition(E::Requested, E::InEffect));
        assert!(!exception_can_transition(E::Completed, E::InEffect));
        use SimulationStatus as S;
        assert!(simulation_can_transition(S::Evaluated, S::Accepted));
        assert!(!simulation_can_transition(S::Accepted, S::Discarded));
    }

    #[test]
    fn validates_commands() {
        assert!(NewAcademicYear {
            calendar_id: AcademicCalendarId::new(),
            label: "2026-27".into(),
            starts_on: d("2026-06-01"),
            ends_on: d("2026-06-01"),
        }
        .validate()
        .is_err());
        assert!(NewAcademicYear {
            calendar_id: AcademicCalendarId::new(),
            label: "2026-27".into(),
            starts_on: d("2026-06-01"),
            ends_on: d("2029-06-01"),
        }
        .validate()
        .is_err());
        assert!(NewCalendar {
            code: "main".into(),
            name: "x".into(),
            description: None
        }
        .validate()
        .is_err());
        assert!(NewCalendar {
            code: "MAIN".into(),
            name: "Main".into(),
            description: None
        }
        .validate()
        .is_ok());
        let k = NewKeyDate {
            kind: KeyDateKind::Holiday,
            name: "Diwali".into(),
            starts_on: d("2026-11-08"),
            ends_on: None,
        };
        assert_eq!(k.ends(), d("2026-11-08"));
        assert!(k.validate().is_ok());
        assert!(NewAssignment {
            calendar_version_id: CalendarVersionId::new(),
            scope_kind: ScopeKind::Campus,
            scope_id: None,
            effective_from: d("2026-06-01"),
            effective_to: None,
        }
        .validate()
        .is_err());
    }

    #[test]
    fn definition_rules() {
        let year = (d("2026-06-01"), d("2027-05-31"));
        let ok = validate_definition(
            d("2026-06-01"),
            d("2027-05-31"),
            year,
            &[
                ("T1".into(), 1, d("2026-06-15"), d("2026-11-30")),
                ("T2".into(), 2, d("2026-12-01"), d("2027-04-30")),
            ],
            &[("Holiday".into(), d("2026-08-15"), d("2026-08-15"))],
        );
        assert!(ok.is_empty(), "{ok:?}");
        let bad = validate_definition(
            d("2026-05-01"),
            d("2027-05-31"),
            year,
            &[
                ("T1".into(), 1, d("2026-06-15"), d("2026-12-31")),
                ("T2".into(), 1, d("2026-12-01"), d("2027-07-30")),
            ],
            &[("Late".into(), d("2027-07-01"), d("2027-07-01"))],
        );
        let codes: Vec<_> = bad.iter().map(|v| v.code.as_str()).collect();
        assert!(codes.contains(&"outside-year"));
        assert!(codes.contains(&"overlap"));
        assert!(codes.contains(&"sequence"));
        assert!(codes.contains(&"outside-version"));
        assert!(
            validate_definition(d("2026-06-01"), d("2027-05-31"), year, &[], &[])
                .iter()
                .any(|v| v.field == "terms")
        );
    }

    #[test]
    fn overlap_and_labels() {
        assert!(overlaps(
            d("2026-01-01"),
            Some(d("2026-01-10")),
            d("2026-01-10"),
            None
        ));
        assert!(!overlaps(
            d("2026-01-01"),
            Some(d("2026-01-09")),
            d("2026-01-10"),
            None
        ));
        assert!(overlaps(
            d("2026-01-01"),
            None,
            d("2030-01-10"),
            Some(d("2030-02-01"))
        ));
        assert_eq!(next_label("2026-27"), "2027-28");
        assert_eq!(next_label("2099-00"), "2100-01");
        assert_eq!(next_label("2026-2027"), "2027-2028");
        assert_eq!(next_label("2026"), "2027");
        assert_eq!(next_label("AY one"), "AY one (next)");
        assert_eq!(
            successor_dates(d("2026-06-01"), d("2027-05-31")),
            (d("2027-06-01"), d("2028-05-31"))
        );
        assert_eq!(
            shift_into(
                d("2026-06-15"),
                d("2026-11-30"),
                365,
                (d("2027-06-01"), d("2028-05-31"))
            ),
            Some((d("2027-06-15"), d("2027-11-30")))
        );
        assert_eq!(
            shift_into(
                d("2026-01-01"),
                d("2026-01-02"),
                0,
                (d("2027-06-01"), d("2028-05-31"))
            ),
            None
        );
    }

    #[test]
    fn closure_needs_evidence_or_an_early_closure_exception() {
        let mut f = ClosureFacts {
            today: d("2027-03-01"),
            ends_on: d("2027-05-31"),
            published_versions: 1,
            pending_exceptions: 0,
            early_closure_reason: None,
        };
        assert!(closure_evidence(&f).iter().any(ClosureEvidence::blocks));
        f.early_closure_reason = Some("Board directive 12/2027".into());
        let ev = closure_evidence(&f);
        assert!(!ev.iter().any(ClosureEvidence::blocks));
        assert_eq!(ev[0].status, EvidenceStatus::Waived);
        f.pending_exceptions = 2;
        assert!(closure_evidence(&f).iter().any(ClosureEvidence::blocks));
        f.pending_exceptions = 0;
        f.early_closure_reason = None;
        f.today = d("2027-06-01");
        assert!(!closure_evidence(&f).iter().any(ClosureEvidence::blocks));
    }

    fn baseline(assignments: Vec<BaselineAssignment>) -> SimulationBaseline {
        SimulationBaseline {
            calendar_version_id: CalendarVersionId::new(),
            version_no: 1,
            status: PublicationStatus::Published,
            aggregate_version: 3,
            timezone: "Asia/Kolkata".into(),
            year_starts_on: d("2026-06-01"),
            year_ends_on: d("2027-05-31"),
            starts_on: d("2026-06-01"),
            ends_on: d("2027-05-31"),
            terms: vec![NewTerm {
                code: "T1".into(),
                name: "Term 1".into(),
                sequence: 1,
                starts_on: d("2026-06-01"),
                ends_on: d("2026-06-14"),
            }],
            key_dates: vec![NewKeyDate {
                kind: KeyDateKind::Holiday,
                name: "H".into(),
                starts_on: d("2026-06-03"),
                ends_on: None,
            }],
            assignments,
            exceptions: vec![],
            captured_at: Utc::now(),
        }
    }

    #[test]
    fn simulation_reports_impact_without_assuming() {
        let b = baseline(vec![]);
        // 2026-06-01 is a Monday: two weeks = 10 weekdays, minus one holiday.
        assert_eq!(instructional_days(&b.terms, &b.key_dates), 9);
        let same = simulate(&b, &SimulationProposal::default());
        assert_eq!(same.outcome, SimulationOutcome::Favourable);
        assert!(same.items.is_empty());

        let bad = simulate(
            &b,
            &SimulationProposal {
                ends_on: Some(d("2027-07-31")),
                ..Default::default()
            },
        );
        assert_eq!(bad.outcome, SimulationOutcome::Unfavourable);
        assert!(bad.items.iter().any(|i| i.code == "period.outside_year"));

        let gov = baseline(vec![BaselineAssignment {
            id: Uuid::nil(),
            scope_kind: ScopeKind::Institution,
            scope_id: Uuid::nil(),
            status: CalendarAssignmentStatus::Effective,
            effective_from: d("2026-06-01"),
            effective_to: None,
        }]);
        let shorter = simulate(
            &gov,
            &SimulationProposal {
                ends_on: Some(d("2027-04-30")),
                ..Default::default()
            },
        );
        assert_eq!(shorter.outcome, SimulationOutcome::Uncertain);
        assert!(shorter.items.iter().any(|i| i.code == "downstream.unknown"));
        assert!(shorter.items.iter().any(|i| i.code == "period.changed"));

        let fewer = simulate(
            &b,
            &SimulationProposal {
                key_dates: Some(vec![NewKeyDate {
                    kind: KeyDateKind::Break,
                    name: "B".into(),
                    starts_on: d("2026-06-08"),
                    ends_on: Some(d("2026-06-12")),
                }]),
                ..Default::default()
            },
        );
        assert_eq!(fewer.proposed_instructional_days, 5);
        assert!(
            fewer
                .items
                .iter()
                .any(|i| i.code == "instruction.days_changed"
                    && i.severity == ImpactSeverity::Warning)
        );
    }
}
