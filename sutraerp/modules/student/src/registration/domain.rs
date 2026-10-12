//! Registration aggregates (L2-C03): window, registration (+ lines) and hold. DTOs, stored rows
//! and the pure lifecycle / selection rules.

use crate::registration::support::{invalid, parse_enum};
use chrono::{DateTime, NaiveDate, Utc};
use sutra_contracts::finance::FinanceEvidence;
use sutra_contracts::institution::reference::CurriculumComponentRef;
use sutra_contracts::student::status::{
    AcademicStanding, HoldSource, RegistrationHoldStatus, RegistrationLineKind,
    RegistrationLineStatus, RegistrationStatus, RegistrationWindowStatus,
};
use sutra_kernel::error::KernelError;
use uuid::Uuid;

// ---------------------------------------------------------------- lifecycle rules

/// Registration window transitions (L3-C-020): scheduled → open → closed; scheduled → cancelled.
pub fn window_can_transition(from: RegistrationWindowStatus, to: RegistrationWindowStatus) -> bool {
    use RegistrationWindowStatus::*;
    matches!(
        (from, to),
        (Scheduled, Open) | (Scheduled, Cancelled) | (Open, Closed)
    )
}

/// Registration transitions (L2-C03 §6.9 condensed: Created/Validated = `draft`, Frozen =
/// `finalised`; a rejected approval returns the registration (`returned`) for correction).
pub fn registration_can_transition(from: RegistrationStatus, to: RegistrationStatus) -> bool {
    use RegistrationStatus::*;
    matches!(
        (from, to),
        (Draft, Submitted)
            | (Draft, Cancelled)
            | (Submitted, Approved)
            | (Submitted, Returned)
            | (Submitted, Cancelled)
            | (Returned, Submitted)
            | (Returned, Cancelled)
            | (Approved, Registered)
            | (Approved, Cancelled)
            | (Registered, Finalised)
            | (Registered, Cancelled)
            | (Finalised, Archived)
    )
}

/// Statuses in which lines may be added or dropped (draft editing, or add/drop after
/// registration while the window is open).
pub fn lines_editable(s: RegistrationStatus) -> bool {
    matches!(
        s,
        RegistrationStatus::Draft | RegistrationStatus::Returned | RegistrationStatus::Registered
    )
}

/// Facts a line selection is validated against.
pub struct SelectionContext<'a> {
    pub components: &'a [CurriculumComponentRef],
    /// Programme term of the registration.
    pub term_no: i32,
    pub standing: AcademicStanding,
    /// Components with a live (selected/registered) line on this registration.
    pub selected: &'a [Uuid],
    /// Components with net earned credits in the ledger.
    pub earned: &'a [Uuid],
    /// Components registered in an earlier finalised registration.
    pub previously_registered: &'a [Uuid],
    /// Current counted credits (non-audit live lines).
    pub current_credits: f64,
    pub max_credits: Option<f64>,
}

/// Validates one line selection (L3-C-021 / C-024) and returns the curriculum component.
pub fn validate_selection<'a>(
    sel: &SelectionContext<'a>,
    component_id: Uuid,
    kind: RegistrationLineKind,
) -> Result<&'a CurriculumComponentRef, KernelError> {
    let c = sel
        .components
        .iter()
        .find(|c| c.id == component_id)
        .ok_or_else(|| {
            invalid(
                "curriculumComponentId",
                "not_in_curriculum",
                "the component is not part of the student's curriculum version",
            )
        })?;
    if sel.selected.contains(&component_id) {
        return Err(invalid(
            "curriculumComponentId",
            "duplicate",
            "the component is already selected on this registration",
        ));
    }
    let detained = sel.standing == AcademicStanding::Detained;
    match kind {
        RegistrationLineKind::Regular | RegistrationLineKind::Elective => {
            if detained {
                return Err(KernelError::Conflict(
                    "a detained student registers repeat or backlog components only".into(),
                ));
            }
            if c.term_no != sel.term_no {
                return Err(invalid(
                    "curriculumComponentId",
                    "wrong_term",
                    format!(
                        "regular components must belong to term {} (component is in term {})",
                        sel.term_no, c.term_no
                    ),
                ));
            }
        }
        RegistrationLineKind::Backlog => {
            if c.term_no >= sel.term_no {
                return Err(invalid(
                    "kind",
                    "not_backlog",
                    "a backlog component must belong to an earlier term",
                ));
            }
            if sel.earned.contains(&component_id) {
                return Err(invalid(
                    "kind",
                    "already_earned",
                    "the component's credits are already earned",
                ));
            }
        }
        RegistrationLineKind::Repeat => {
            if c.term_no > sel.term_no || !sel.previously_registered.contains(&component_id) {
                return Err(invalid(
                    "kind",
                    "not_repeatable",
                    "only a component registered in an earlier finalised registration can be repeated",
                ));
            }
        }
        RegistrationLineKind::Audit => {}
    }
    if kind != RegistrationLineKind::Audit {
        if let Some(max) = sel.max_credits {
            if sel.current_credits + c.credits > max + 1e-9 {
                return Err(invalid(
                    "curriculumComponentId",
                    "credit_limit",
                    format!(
                        "the registration would carry {} credits; the maximum is {max}",
                        sel.current_credits + c.credits
                    ),
                ));
            }
        }
    }
    Ok(c)
}

// ---------------------------------------------------------------- windows

/// An academic registration window for one term (L3-C-020).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationWindow {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Uuid,
    pub name: String,
    pub opens_on: NaiveDate,
    pub closes_on: NaiveDate,
    pub min_credits: Option<f64>,
    pub max_credits: Option<f64>,
    /// Calendar key date the dates were derived from.
    pub key_date_id: Option<Uuid>,
    pub status: RegistrationWindowStatus,
    pub status_reason: Option<String>,
    pub opened_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const WINDOW_COLUMNS: &str =
    "id, institution_id, academic_term_id, name, opens_on, closes_on, \
    min_credits::float8 AS min_credits, max_credits::float8 AS max_credits, key_date_id, status, \
    status_reason, opened_at, closed_at, version, created_at, updated_at";

#[derive(sqlx::FromRow)]
pub struct WindowRow {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Uuid,
    pub name: String,
    pub opens_on: NaiveDate,
    pub closes_on: NaiveDate,
    pub min_credits: Option<f64>,
    pub max_credits: Option<f64>,
    pub key_date_id: Option<Uuid>,
    pub status: String,
    pub status_reason: Option<String>,
    pub opened_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl WindowRow {
    pub fn into_dto(self) -> Result<RegistrationWindow, KernelError> {
        Ok(RegistrationWindow {
            status: parse_enum(
                &self.status,
                RegistrationWindowStatus::parse,
                "window status",
            )?,
            id: self.id,
            institution_id: self.institution_id,
            academic_term_id: self.academic_term_id,
            name: self.name,
            opens_on: self.opens_on,
            closes_on: self.closes_on,
            min_credits: self.min_credits,
            max_credits: self.max_credits,
            key_date_id: self.key_date_id,
            status_reason: self.status_reason,
            opened_at: self.opened_at,
            closed_at: self.closed_at,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

impl RegistrationWindow {
    /// Open and the business date inside `[opensOn, closesOn]`.
    pub fn accepts_on(&self, today: NaiveDate) -> bool {
        self.status == RegistrationWindowStatus::Open
            && self.opens_on <= today
            && today <= self.closes_on
    }
}

/// Schedule a registration window. Without `opensOn`/`closesOn` the dates come from the
/// `registration_window` key date of the term's published calendar version (L4-C-020-BH-001).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateWindow {
    pub academic_term_id: Uuid,
    pub name: Option<String>,
    pub opens_on: Option<NaiveDate>,
    pub closes_on: Option<NaiveDate>,
    pub min_credits: Option<f64>,
    pub max_credits: Option<f64>,
}

/// Change a scheduled window (an open window may only move its `closesOn`).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateWindow {
    pub name: Option<String>,
    pub opens_on: Option<NaiveDate>,
    pub closes_on: Option<NaiveDate>,
    pub min_credits: Option<f64>,
    pub max_credits: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationWindowList {
    pub items: Vec<RegistrationWindow>,
}

/// Result of finalising every registered registration of a closed window (L3-C-025).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WindowFinalisation {
    pub window: RegistrationWindow,
    pub finalised_registration_ids: Vec<Uuid>,
}

// ---------------------------------------------------------------- registrations

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationLine {
    pub id: Uuid,
    pub curriculum_component_id: Uuid,
    pub kind: RegistrationLineKind,
    pub status: RegistrationLineStatus,
    pub credits: f64,
    /// Programme term the component belongs to.
    pub component_term_no: i32,
    pub slot_code: String,
    pub title: String,
    pub added_after_registration: bool,
    pub dropped_at: Option<DateTime<Utc>>,
    pub drop_reason: Option<String>,
}

pub const LINE_COLUMNS: &str = "id, registration_id, curriculum_component_id, kind, status, \
    credits::float8 AS credits, component_term_no, slot_code, title, added_after_registration, \
    dropped_at, drop_reason";

#[derive(sqlx::FromRow)]
pub struct LineRow {
    pub id: Uuid,
    pub registration_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub kind: String,
    pub status: String,
    pub credits: f64,
    pub component_term_no: i32,
    pub slot_code: String,
    pub title: String,
    pub added_after_registration: bool,
    pub dropped_at: Option<DateTime<Utc>>,
    pub drop_reason: Option<String>,
}

impl LineRow {
    pub fn into_dto(self) -> Result<RegistrationLine, KernelError> {
        Ok(RegistrationLine {
            kind: parse_enum(&self.kind, RegistrationLineKind::parse, "line kind")?,
            status: parse_enum(&self.status, RegistrationLineStatus::parse, "line status")?,
            id: self.id,
            curriculum_component_id: self.curriculum_component_id,
            credits: self.credits,
            component_term_no: self.component_term_no,
            slot_code: self.slot_code,
            title: self.title,
            added_after_registration: self.added_after_registration,
            dropped_at: self.dropped_at,
            drop_reason: self.drop_reason,
        })
    }
}

impl RegistrationLine {
    pub fn is_live(&self) -> bool {
        matches!(
            self.status,
            RegistrationLineStatus::Selected | RegistrationLineStatus::Registered
        )
    }
}

/// Sum of the credits of live, non-audit lines.
pub fn counted_credits(lines: &[RegistrationLine]) -> f64 {
    lines
        .iter()
        .filter(|l| l.is_live() && l.kind != RegistrationLineKind::Audit)
        .map(|l| l.credits)
        .sum()
}

/// A student's academic registration for one term (L3-C-021).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Registration {
    pub id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Uuid,
    pub registration_window_id: Uuid,
    /// Programme term (semester) of the registration.
    pub term_no: i32,
    pub curriculum_version_id: Option<Uuid>,
    pub status: RegistrationStatus,
    pub status_reason: Option<String>,
    /// `staff` or `self`.
    pub origin: String,
    /// Credits of live, non-audit lines.
    pub total_credits: f64,
    pub lines: Vec<RegistrationLine>,
    pub process_instance_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub registered_at: Option<DateTime<Utc>>,
    pub finalised_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const REGISTRATION_COLUMNS: &str = "id, tenant_id, institution_id, student_id, student_context_id, \
    academic_term_id, registration_window_id, term_no, curriculum_version_id, status, status_reason, \
    origin, process_instance_id, submitted_at, registered_at, finalised_at, cancelled_at, version, \
    created_at, updated_at";

#[derive(sqlx::FromRow, Clone)]
pub struct RegistrationRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub academic_term_id: Uuid,
    pub registration_window_id: Uuid,
    pub term_no: i32,
    pub curriculum_version_id: Option<Uuid>,
    pub status: String,
    pub status_reason: Option<String>,
    pub origin: String,
    pub process_instance_id: Option<String>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub registered_at: Option<DateTime<Utc>>,
    pub finalised_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl RegistrationRow {
    pub fn status(&self) -> Result<RegistrationStatus, KernelError> {
        parse_enum(
            &self.status,
            RegistrationStatus::parse,
            "registration status",
        )
    }

    pub fn into_dto(self, lines: Vec<RegistrationLine>) -> Result<Registration, KernelError> {
        Ok(Registration {
            status: self.status()?,
            total_credits: counted_credits(&lines),
            lines,
            id: self.id,
            student_id: self.student_id,
            student_context_id: self.student_context_id,
            institution_id: self.institution_id,
            academic_term_id: self.academic_term_id,
            registration_window_id: self.registration_window_id,
            term_no: self.term_no,
            curriculum_version_id: self.curriculum_version_id,
            status_reason: self.status_reason,
            origin: self.origin,
            process_instance_id: self.process_instance_id,
            submitted_at: self.submitted_at,
            registered_at: self.registered_at,
            finalised_at: self.finalised_at,
            cancelled_at: self.cancelled_at,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

/// One component selection.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct LineInput {
    pub curriculum_component_id: Uuid,
    /// Defaults to `regular`.
    pub kind: Option<RegistrationLineKind>,
}

/// Start a registration for a term (draft) with optional initial selections.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateRegistration {
    pub academic_term_id: Uuid,
    #[serde(default)]
    pub lines: Vec<LineInput>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationList {
    pub items: Vec<Registration>,
}

// ---------------------------------------------------------------- holds

/// A registration hold (L3-C-026).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationHold {
    pub id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    pub student_context_id: Option<Uuid>,
    pub source: HoldSource,
    pub reason: String,
    pub external_reference: Option<String>,
    pub status: RegistrationHoldStatus,
    pub placed_by: Option<String>,
    pub placed_at: DateTime<Utc>,
    pub released_by: Option<String>,
    pub released_at: Option<DateTime<Utc>>,
    pub release_reason: Option<String>,
    pub version: i64,
}

pub const HOLD_COLUMNS: &str =
    "id, tenant_id, student_id, institution_id, student_context_id, source, \
    reason, external_reference, status, placed_by, placed_at, released_by, released_at, \
    release_reason, version";

#[derive(sqlx::FromRow)]
pub struct HoldRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub student_id: Uuid,
    pub institution_id: Uuid,
    pub student_context_id: Option<Uuid>,
    pub source: String,
    pub reason: String,
    pub external_reference: Option<String>,
    pub status: String,
    pub placed_by: Option<String>,
    pub placed_at: DateTime<Utc>,
    pub released_by: Option<String>,
    pub released_at: Option<DateTime<Utc>>,
    pub release_reason: Option<String>,
    pub version: i64,
}

impl HoldRow {
    pub fn into_dto(self) -> Result<RegistrationHold, KernelError> {
        Ok(RegistrationHold {
            source: parse_enum(&self.source, HoldSource::parse, "hold source")?,
            status: parse_enum(&self.status, RegistrationHoldStatus::parse, "hold status")?,
            id: self.id,
            student_id: self.student_id,
            institution_id: self.institution_id,
            student_context_id: self.student_context_id,
            reason: self.reason,
            external_reference: self.external_reference,
            placed_by: self.placed_by,
            placed_at: self.placed_at,
            released_by: self.released_by,
            released_at: self.released_at,
            release_reason: self.release_reason,
            version: self.version,
        })
    }
}

/// Place a hold. `finance` holds may be placed by hand; Finance-owned dues are also read live
/// through `FinanceClearancePort` at registration time.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlaceHold {
    pub source: HoldSource,
    pub reason: String,
    pub external_reference: Option<String>,
}

/// Holds of a student plus the live Finance evidence (`student_dues`).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HoldList {
    pub items: Vec<RegistrationHold>,
    pub finance_evidence: FinanceEvidence,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comp(term: i32, credits: f64) -> CurriculumComponentRef {
        CurriculumComponentRef {
            id: Uuid::now_v7(),
            curriculum_version_id: Uuid::nil(),
            term_no: term,
            slot_code: format!("C{term}"),
            title: "c".into(),
            category: "major_core".into(),
            credits,
            course_code: None,
        }
    }

    #[test]
    fn window_and_registration_transitions() {
        use RegistrationStatus as R;
        use RegistrationWindowStatus as W;
        assert!(window_can_transition(W::Scheduled, W::Open));
        assert!(window_can_transition(W::Open, W::Closed));
        assert!(!window_can_transition(W::Closed, W::Open));
        assert!(!window_can_transition(W::Open, W::Cancelled));
        assert!(registration_can_transition(R::Draft, R::Submitted));
        assert!(registration_can_transition(R::Submitted, R::Returned));
        assert!(registration_can_transition(R::Returned, R::Submitted));
        assert!(registration_can_transition(R::Registered, R::Finalised));
        assert!(!registration_can_transition(R::Finalised, R::Cancelled));
        assert!(!registration_can_transition(R::Draft, R::Registered));
        assert!(!registration_can_transition(R::Cancelled, R::Draft));
        for s in RegistrationStatus::ALL {
            assert!(!registration_can_transition(*s, *s));
        }
        assert!(lines_editable(R::Registered) && !lines_editable(R::Submitted));
    }

    #[test]
    fn selection_rules() {
        let comps = vec![comp(1, 12.0), comp(1, 8.0), comp(2, 12.0)];
        let prev = [comps[0].id];
        let mut sel = SelectionContext {
            components: &comps,
            term_no: 2,
            standing: AcademicStanding::GoodStanding,
            selected: &[],
            earned: &[],
            previously_registered: &prev,
            current_credits: 0.0,
            max_credits: Some(20.0),
        };
        assert!(validate_selection(&sel, comps[2].id, RegistrationLineKind::Regular).is_ok());
        assert!(validate_selection(&sel, comps[0].id, RegistrationLineKind::Regular).is_err());
        assert!(validate_selection(&sel, comps[1].id, RegistrationLineKind::Backlog).is_ok());
        assert!(validate_selection(&sel, comps[2].id, RegistrationLineKind::Backlog).is_err());
        assert!(validate_selection(&sel, comps[0].id, RegistrationLineKind::Repeat).is_ok());
        assert!(validate_selection(&sel, comps[1].id, RegistrationLineKind::Repeat).is_err());
        assert!(validate_selection(&sel, Uuid::now_v7(), RegistrationLineKind::Regular).is_err());
        sel.current_credits = 12.0;
        assert!(validate_selection(&sel, comps[2].id, RegistrationLineKind::Regular).is_err());
        assert!(validate_selection(&sel, comps[2].id, RegistrationLineKind::Audit).is_ok());
        sel.current_credits = 0.0;
        sel.standing = AcademicStanding::Detained;
        assert!(validate_selection(&sel, comps[2].id, RegistrationLineKind::Regular).is_err());
        assert!(validate_selection(&sel, comps[0].id, RegistrationLineKind::Repeat).is_ok());
        let selected = [comps[2].id];
        sel.selected = &selected;
        sel.standing = AcademicStanding::GoodStanding;
        assert!(validate_selection(&sel, comps[2].id, RegistrationLineKind::Regular).is_err());
    }
}
