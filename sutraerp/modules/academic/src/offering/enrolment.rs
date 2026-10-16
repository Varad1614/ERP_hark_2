//! Section enrolment (the roster) gated on 3B registrations through `RegistrationReader`
//! (L3-D-006/008), the waitlist → seat flow (L3-D-007) and student self-service (`/me/…`).
//!
//! Seat gate (in order): offering accepts enrolment (`enrollment_open | active`), section
//! active, an active StudentContext in the offering's institution (or a cross-institution
//! permit), offering-level restrictions (programme, minimum term, cross-campus), an open
//! enrolment window when the offering has windows, a free seat (section and offering caps minus
//! held waitlist offers), and finally the student's effective registration covering the
//! offering's curriculum component — otherwise only `offering.enrolment.manage` may seat with
//! an `override` reason (approval step `enrolment_eligibility_override`).

use super::lifecycle::expect_version;
use super::support::*;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::http::with_etag;
use crate::shared::lifecycle::{change_enrolment_status, StatusChange};
use crate::shared::offering::{
    enrolment_from_row, load_enrolment, load_section, lock_enrolment, lock_section, OfferingRow,
    SectionEnrolmentRow, SectionRow, ENROLMENT_COLUMNS,
};
use crate::shared::scope::tenant;
use crate::shared::time::business_today;
use crate::shared::{publish, AcademicState, Actor};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::academic::events::offering::{self as ev, SectionEnrolmentV1};
use sutra_contracts::academic::events::StatusChangedV1;
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId, SectionEnrolmentId};
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, SectionEnrolmentStatus, SectionStatus,
    WaitlistStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::{StudentContextSummary, StudentSummary, TermRegistration};
use sutra_contracts::student::status::StudentContextStatus;
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

// ================================================================== views / requests

/// A seat in a section.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SectionEnrolment {
    pub id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub student_id: Uuid,
    pub student_context_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub registration_id: Option<Uuid>,
    pub registration_line_id: Option<Uuid>,
    /// `registration` | `override` | `waitlist` | `transfer`.
    pub origin: String,
    pub status: SectionEnrolmentStatus,
    pub status_reason: Option<String>,
    pub transferred_from_id: Option<Uuid>,
    pub enrolled_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub version: i64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SectionEnrolmentList {
    pub items: Vec<SectionEnrolment>,
    pub next_cursor: Option<String>,
}

pub fn enrolment_view(e: &SectionEnrolmentRow) -> SectionEnrolment {
    SectionEnrolment {
        id: e.id.as_uuid(),
        offering_id: e.offering_id.as_uuid(),
        section_id: e.section_id.as_uuid(),
        student_id: e.student_id.as_uuid(),
        student_context_id: e.student_context_id.as_uuid(),
        student_number: e.student_number.clone(),
        display_name: e.display_name.clone(),
        registration_id: e.registration_id,
        registration_line_id: e.registration_line_id,
        origin: e.origin.clone(),
        status: e.status,
        status_reason: e.status_reason.clone(),
        transferred_from_id: e.transferred_from_id.map(|x| x.as_uuid()),
        enrolled_at: e.enrolled_at,
        ended_at: e.ended_at,
        version: e.version.0,
    }
}

fn enrolment_etag(status: StatusCode, e: &SectionEnrolment) -> Response {
    with_etag(status, e, Version(e.version))
}

/// Seat a student (administrator).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SectionSeatRequest {
    pub student_id: Uuid,
    /// Seat without a covering registration / past a restriction or closed window (reason required).
    #[serde(default)]
    pub override_eligibility: bool,
    pub reason: Option<String>,
    /// Cross-institution enrolment: the student's home institution (needs a permit, L3-D-008).
    pub home_institution_id: Option<Uuid>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SeatTransferRequest {
    pub to_section_id: Uuid,
    pub reason: Option<String>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct RosterQuery {
    #[param(inline)]
    pub status: Option<SectionEnrolmentStatus>,
    /// ≤ 200, default 50.
    pub limit: Option<i64>,
    /// Opaque cursor from `nextCursor`.
    pub cursor: Option<String>,
}

/// Waitlist entry (L3-D-007).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingWaitlistEntry {
    pub id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Option<Uuid>,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub position: i32,
    pub status: WaitlistStatus,
    pub status_reason: Option<String>,
    pub offered_section_id: Option<Uuid>,
    pub offered_at: Option<DateTime<Utc>>,
    pub offer_expires_on: Option<NaiveDate>,
    pub enrolment_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingWaitlist {
    pub items: Vec<OfferingWaitlistEntry>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingWaitlistJoin {
    pub student_id: Uuid,
    pub section_id: Option<Uuid>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingWaitlistPromote {
    /// Manual promotion: must be the entry the FIFO policy selects (409 otherwise).
    pub entry_id: Option<Uuid>,
    /// Days the seat offer stays open (1..=14, default 2).
    pub offer_days: Option<i32>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct WaitlistQuery {
    #[param(inline)]
    pub status: Option<WaitlistStatus>,
}

const WAITLIST_COLUMNS: &str =
    "id, offering_id, section_id, student_id, student_context_id, student_number, \
    display_name, subject_ref, position, status, status_reason, offered_section_id, offered_at, \
    offer_expires_on, enrolment_id, version, created_at";

struct WaitRow {
    view: OfferingWaitlistEntry,
    student_context_id: Uuid,
    subject_ref: Option<String>,
}

fn wait_from_row(r: &PgRow) -> Result<WaitRow, KernelError> {
    let status: String = r.try_get("status").map_err(map_db_error)?;
    Ok(WaitRow {
        student_context_id: r.try_get("student_context_id").map_err(map_db_error)?,
        subject_ref: r.try_get("subject_ref").map_err(map_db_error)?,
        view: OfferingWaitlistEntry {
            id: r.try_get("id").map_err(map_db_error)?,
            offering_id: r.try_get("offering_id").map_err(map_db_error)?,
            section_id: r.try_get("section_id").map_err(map_db_error)?,
            student_id: r.try_get("student_id").map_err(map_db_error)?,
            student_number: r.try_get("student_number").map_err(map_db_error)?,
            display_name: r.try_get("display_name").map_err(map_db_error)?,
            position: r.try_get("position").map_err(map_db_error)?,
            status: WaitlistStatus::parse(&status).ok_or_else(|| {
                KernelError::internal(std::io::Error::other("bad waitlist status"))
            })?,
            status_reason: r.try_get("status_reason").map_err(map_db_error)?,
            offered_section_id: r.try_get("offered_section_id").map_err(map_db_error)?,
            offered_at: r.try_get("offered_at").map_err(map_db_error)?,
            offer_expires_on: r.try_get("offer_expires_on").map_err(map_db_error)?,
            enrolment_id: r.try_get("enrolment_id").map_err(map_db_error)?,
            version: r.try_get("version").map_err(map_db_error)?,
            created_at: r.try_get("created_at").map_err(map_db_error)?,
        },
    })
}

/// Legal waitlist transitions (slice-owned machine).
pub fn waitlist_can_transition(from: WaitlistStatus, to: WaitlistStatus) -> bool {
    use WaitlistStatus::*;
    matches!(
        (from, to),
        (Waiting, Offered)
            | (Waiting, Removed)
            | (Offered, Accepted)
            | (Offered, Expired)
            | (Offered, Removed)
    )
}

// ================================================================== the seat gate

/// How a seat request is evaluated.
#[derive(Clone, Debug, Default)]
pub struct Gate {
    /// Check free seats (false for an accepted waitlist offer, which holds its seat).
    pub capacity: bool,
    /// Waitlist entry whose held offer is being converted (excluded from held seats).
    pub holding_entry: Option<Uuid>,
    /// Require an open window when the offering has windows.
    pub window: bool,
    /// Administrator override reason (bypasses registration / restriction / window failures).
    pub override_reason: Option<String>,
    /// Cross-institution home institution.
    pub home_institution: Option<InstitutionId>,
    /// Skip the registration coverage check (waitlist join re-checks it itself).
    pub registration: bool,
    /// The override was already approved by an independent checker (DEF-P1-DEMO-006): no new
    /// approval is requested.
    pub override_approved: bool,
}

/// What justified the seat.
pub struct SeatBasis {
    pub student: StudentSummary,
    pub context: StudentContextSummary,
    pub origin: &'static str,
    pub registration_id: Option<Uuid>,
    pub registration_line_id: Option<Uuid>,
    pub reason: Option<String>,
}

async fn held_offers(
    conn: &mut PgConnection,
    section: Option<OfferingSectionId>,
    offering: CourseOfferingId,
    today: NaiveDate,
    except: Option<Uuid>,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM academic.offering_waitlist_entry WHERE offering_id = $1 AND status = 'offered' \
         AND offer_expires_on >= $2 AND ($3::uuid IS NULL OR offered_section_id = $3) AND ($4::uuid IS NULL OR id <> $4)",
    )
    .bind(offering.as_uuid())
    .bind(today)
    .bind(section.map(|s| s.as_uuid()))
    .bind(except)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// `true` when one more student fits the section and the offering (held offers count).
pub async fn has_seat(
    conn: &mut PgConnection,
    o: &OfferingRow,
    s: &SectionRow,
    today: NaiveDate,
    except: Option<Uuid>,
) -> Result<bool, KernelError> {
    if let Some(cap) = s.capacity {
        let used = seated_in_section(&mut *conn, s.id).await?
            + held_offers(conn, Some(s.id), o.id, today, except).await?;
        if used >= i64::from(cap) {
            return Ok(false);
        }
    }
    if let Some(cap) = o.capacity {
        let used = seated_in_offering(&mut *conn, o.id).await?
            + held_offers(conn, None, o.id, today, except).await?;
        if used >= i64::from(cap) {
            return Ok(false);
        }
    }
    Ok(true)
}

struct EnrolmentRestriction {
    id: Uuid,
    kind: String,
    ref_ids: Vec<Uuid>,
    min_term_no: Option<i32>,
}

async fn effective_restrictions(
    conn: &mut PgConnection,
    o: &OfferingRow,
    today: NaiveDate,
) -> Result<Vec<EnrolmentRestriction>, KernelError> {
    let rows: Vec<(Uuid, String, Vec<Uuid>, Option<i32>)> = sqlx::query_as(
        "SELECT id, kind, ref_ids, min_term_no FROM academic.enrolment_restriction WHERE offering_id = $1 \
         AND status = 'effective' AND (effective_from IS NULL OR effective_from <= $2) AND (effective_to IS NULL OR effective_to >= $2)",
    )
    .bind(o.id.as_uuid())
    .bind(today)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(|(id, kind, ref_ids, min_term_no)| EnrolmentRestriction {
            id,
            kind,
            ref_ids,
            min_term_no,
        })
        .collect())
}

/// What a seat decision needs from OTHER modules' ports (student, live contexts, the
/// institution's business day, the term registration). Every port call takes its own pool
/// connection, so these are read BEFORE the offering row is locked (Part 2 · S5-capacity,
/// DEF-P2-CAP-002): a seat transaction that held `course_offering … FOR UPDATE` and then waited
/// for a second pool connection starved the pool as soon as as many seat requests as pool
/// connections queued on one offering (each waiter holds a connection) — every request then
/// failed with 503 "database unavailable" after the acquire timeout.
pub struct SeatFacts {
    student_id: StudentId,
    institution_id: InstitutionId,
    academic_term_id: Uuid,
    student: Option<StudentSummary>,
    today: NaiveDate,
    local_context: Option<StudentContextSummary>,
    home_context: Option<StudentContextSummary>,
    registration: Option<TermRegistration>,
}

impl SeatFacts {
    /// Reads the facts for `student_id` and offering `o` (an unlocked read is enough: the
    /// institution and term of an offering never change; [`evaluate_with`] re-checks them).
    pub async fn gather(
        st: &AcademicState,
        scope: &ExecutionScope,
        o: &OfferingRow,
        student_id: StudentId,
        gate: &Gate,
    ) -> Result<Self, KernelError> {
        let student = st.students.student(scope, student_id).await?;
        let today = business_today(st, scope, o.institution_id).await?;
        let local_context = st
            .students
            .live_context(scope, student_id, o.institution_id)
            .await?;
        let home_context = match (&local_context, gate.home_institution) {
            (None, Some(home)) => st.students.live_context(scope, student_id, home).await?,
            _ => None,
        };
        let registration = if local_context.is_some() {
            st.registrations
                .term_registration(scope, student_id, o.academic_term_id)
                .await?
        } else {
            None
        };
        Ok(Self {
            student_id,
            institution_id: o.institution_id,
            academic_term_id: o.academic_term_id,
            student,
            today,
            local_context,
            home_context,
            registration,
        })
    }
}

/// Evaluates a seat request for `student_id` in section `s` of the locked offering `o`.
///
/// Reads the port facts itself (nested pool connections while `o` is locked): only for the
/// low-volume paths (waitlist, offers, overrides). The seat hot path gathers [`SeatFacts`]
/// before locking and calls [`evaluate_with`].
pub async fn evaluate(
    st: &AcademicState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    o: &OfferingRow,
    s: &SectionRow,
    student_id: StudentId,
    gate: &Gate,
) -> Result<SeatBasis, KernelError> {
    let facts = SeatFacts::gather(st, scope, o, student_id, gate).await?;
    evaluate_with(st, conn, scope, o, s, facts, gate).await
}

/// [`evaluate`] with facts gathered before the offering was locked: inside the transaction it
/// only reads through `conn` (no second pool connection), except for an override that needs a
/// new approval.
pub async fn evaluate_with(
    st: &AcademicState,
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    o: &OfferingRow,
    s: &SectionRow,
    facts: SeatFacts,
    gate: &Gate,
) -> Result<SeatBasis, KernelError> {
    let student_id = facts.student_id;
    if facts.institution_id != o.institution_id || facts.academic_term_id != o.academic_term_id {
        return Err(conflict(
            "the course offering changed while the seat was evaluated; retry",
        ));
    }
    if !o.status.accepts_enrolment() {
        return Err(conflict(format!(
            "the course offering is {}; it does not accept enrolment",
            o.status
        )));
    }
    if s.offering_id != o.id || s.status != SectionStatus::Active {
        return Err(conflict(format!(
            "the section is {}; seats need an active section",
            s.status
        )));
    }
    let SeatFacts {
        student,
        today,
        local_context,
        home_context,
        registration,
        ..
    } = facts;
    let student = student.ok_or_else(|| invalid("studentId", "not_found", "no such student"))?;
    let overriding = gate.override_reason.is_some();
    let mut failures: Vec<String> = Vec::new();
    // Context in the offering's institution, or a cross-institution permit.
    let mut cross_institution = false;
    let context = match local_context {
        Some(c) => c,
        None => {
            let home = gate.home_institution.ok_or_else(|| {
                conflict("the student has no live context in the offering's institution (cross-institution enrolment needs homeInstitutionId and a permit)")
            })?;
            let permitted = effective_restrictions(&mut *conn, o, today)
                .await?
                .iter()
                .any(|r| r.kind == "cross_institution" && r.ref_ids.contains(&home.as_uuid()));
            if !permitted {
                return Err(conflict(
                    "no effective cross-institution permit covers the student's home institution",
                ));
            }
            cross_institution = true;
            home_context.ok_or_else(|| {
                conflict("the student has no live context in the home institution")
            })?
        }
    };
    if context.status != StudentContextStatus::Active {
        return Err(conflict(format!(
            "the student's context is {}; only active students are seated",
            context.status
        )));
    }
    // Offering-level restrictions (D-006 / D-008).
    let restrictions = effective_restrictions(&mut *conn, o, today).await?;
    let programmes: Vec<&EnrolmentRestriction> = restrictions
        .iter()
        .filter(|r| r.kind == "programme")
        .collect();
    if !cross_institution
        && !programmes.is_empty()
        && !programmes
            .iter()
            .any(|r| context.programme_id.is_some_and(|p| r.ref_ids.contains(&p)))
    {
        failures.push("the student's programme is not admitted to this offering".into());
    }
    if let Some(min) = restrictions
        .iter()
        .filter(|r| r.kind == "min_term")
        .filter_map(|r| r.min_term_no)
        .max()
    {
        if context.current_term_no.unwrap_or(0) < min {
            failures.push(format!("the offering requires term {min} or later"));
        }
    }
    let delivery_campus = s.campus_id.or(o.campus_id);
    if let (Some(dc), Some(sc)) = (delivery_campus, context.campus_id) {
        if !cross_institution && dc != sc {
            let permit = restrictions
                .iter()
                .find(|r| r.kind == "cross_campus" && r.ref_ids.contains(&sc));
            if permit.is_none() {
                failures.push(
                    "cross-campus enrolment is not permitted for the student's campus".into(),
                );
            } else if let Some(p) = permit {
                tracing::debug!(restriction = %p.id, "cross-campus permit applied");
            }
        }
    }
    // Enrolment window.
    if gate.window {
        let (total, open): (i64, i64) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE status <> 'closed_permanently'), count(*) FILTER (WHERE status = 'open') \
             FROM academic.offering_enrolment_window WHERE offering_id = $1",
        )
        .bind(o.id.as_uuid())
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if total > 0 && open == 0 {
            failures.push("no enrolment window is open".into());
        }
    }
    if !failures.is_empty() && !overriding {
        return Err(conflict(failures.join("; ")));
    }
    // Capacity is never overridden.
    if gate.capacity && !has_seat(conn, o, s, today, gate.holding_entry).await? {
        return Err(conflict("the section is full; join the waitlist instead"));
    }
    // Registration coverage (3B, RegistrationReader).
    let mut basis = SeatBasis {
        student,
        context,
        origin: "registration",
        registration_id: None,
        registration_line_id: None,
        reason: None,
    };
    let covered = if cross_institution {
        None
    } else {
        registration.filter(|r| r.covers(o.curriculum_component_id))
    };
    match covered {
        Some(r) => {
            basis.registration_id = Some(r.registration_id);
            basis.registration_line_id = r
                .lines
                .iter()
                .find(|l| {
                    l.curriculum_component_id == o.curriculum_component_id
                        && matches!(l.status.as_str(), "selected" | "registered")
                })
                .map(|l| l.registration_line_id);
            if overriding && !failures.is_empty() {
                basis.origin = "override";
                basis.reason = gate.override_reason.clone();
            }
        }
        None if !gate.registration => {}
        None => {
            if !overriding {
                return Err(conflict(
                    "the student's registration for the term does not cover this course component",
                ));
            }
            basis.origin = "override";
            basis.reason = gate.override_reason.clone();
        }
    }
    if basis.origin == "override" && !gate.override_approved {
        let outcome = request_approval(
            st,
            scope,
            process::ENROLMENT_ELIGIBILITY_OVERRIDE,
            business_key(
                "section_enrolment_override",
                format!("{}:{}", s.id, student_id),
                o.version.0,
            ),
            BTreeMap::from([
                ("offeringId".to_owned(), serde_json::json!(o.id)),
                ("studentId".to_owned(), serde_json::json!(student_id)),
                ("reason".to_owned(), serde_json::json!(basis.reason)),
            ]),
        )
        .await?;
        if let ApprovalOutcome::Pending {
            process_instance_id,
        } = outcome
        {
            return Err(conflict(format!(
                "the eligibility override awaits a decision (process {process_instance_id}); \
                 request it through POST /api/v1/academic/sections/{}/enrolment-overrides so an \
                 independent approver can decide it",
                s.id
            )));
        }
    }
    Ok(basis)
}

/// Inserts the seat and emits `section_enrolment.enrolled` + audit.
#[allow(clippy::too_many_arguments)]
pub async fn insert_seat(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    o: &OfferingRow,
    s: &SectionRow,
    b: &SeatBasis,
    origin: &str,
    transferred_from: Option<SectionEnrolmentId>,
    actor: &Actor,
) -> Result<SectionEnrolmentRow, KernelError> {
    let sql = format!(
        "INSERT INTO academic.section_enrolment (tenant_id, institution_id, offering_id, section_id, student_id, \
         student_context_id, student_number, display_name, subject_ref, registration_id, registration_line_id, \
         origin, status_reason, transferred_from_id) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) \
         RETURNING {ENROLMENT_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(o.tenant_id.as_uuid())
        .bind(o.institution_id.as_uuid())
        .bind(o.id.as_uuid())
        .bind(s.id.as_uuid())
        .bind(b.student.id.as_uuid())
        .bind(b.context.id.as_uuid())
        .bind(&b.student.student_number)
        .bind(&b.student.display_name)
        .bind(&b.student.subject_ref)
        .bind(b.registration_id)
        .bind(b.registration_line_id)
        .bind(origin)
        .bind(&b.reason)
        .bind(transferred_from.map(|x| x.as_uuid()))
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| {
            super::lifecycle::unique_code(
                e,
                "studentId",
                "the student already holds a seat in this offering",
            )
        })?;
    let e = enrolment_from_row(&row).map_err(map_db_error)?;
    enrolment_event(
        conn,
        scope,
        &e,
        ev::SECTION_ENROLMENT_ENROLLED,
        ev::SECTION_ENROLMENT_ENROLLED_SCHEMA,
        actor,
    )
    .await?;
    publish::audit(
        conn,
        scope,
        "academic.section_enrolment.enrol",
        "section_enrolment",
        e.id.to_string(),
        serde_json::json!({"origin": origin, "sectionId": s.id, "studentId": e.student_id, "reason": b.reason}),
        actor,
    )
    .await?;
    Ok(e)
}

async fn enrolment_event(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    e: &SectionEnrolmentRow,
    ty: &'static str,
    schema: &'static str,
    actor: &Actor,
) -> Result<(), KernelError> {
    let data = SectionEnrolmentV1 {
        enrolment_id: e.id.as_uuid(),
        section_id: e.section_id.as_uuid(),
        offering_id: e.offering_id.as_uuid(),
        institution_id: e.institution_id.as_uuid(),
        student_id: e.student_id.as_uuid(),
        student_context_id: e.student_context_id.as_uuid(),
        status: e.status.as_str().to_owned(),
        reason: e.status_reason.clone(),
        student_subject_ref: e.subject_ref.clone(),
    };
    publish::event(conn, scope, ty, schema, e.id.to_string(), &data, actor).await
}

/// Seat a student in a section inside a section command.
async fn seat(
    st: &AcademicState,
    ctx: &AuthContext,
    section: OfferingSectionId,
    student: StudentId,
    gate: Gate,
) -> Result<SectionEnrolment, KernelError> {
    let s0 = load_section(&st.pool, tenant(ctx), section).await?;
    // Port reads first, then the lock (DEF-P2-CAP-002: no second pool connection under it).
    let o0 = crate::shared::offering::load_offering(&st.pool, tenant(ctx), s0.offering_id).await?;
    let facts = SeatFacts::gather(
        st,
        &crate::shared::scope::institution_scope(ctx, o0.institution_id),
        &o0,
        student,
        &gate,
    )
    .await?;
    let mut c = begin(st, ctx, s0.offering_id, None).await?;
    let s = lock_section(&mut c.tx, c.o.tenant_id, section).await?;
    let b = evaluate_with(st, &mut c.tx, &c.scope, &c.o, &s, facts, &gate).await?;
    let origin = b.origin;
    let e = insert_seat(&mut c.tx, &c.scope, &c.o, &s, &b, origin, None, &c.actor).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(enrolment_view(&e))
}

// ================================================================== enrolment handlers

/// Seat a student in a section (`offering.enrolment.manage`). Gated on the student's 3B
/// registration; `overrideEligibility` + reason records an override.
#[utoipa::path(
    post,
    path = "/api/v1/academic/sections/{id}/enrolments",
    operation_id = "section_enrolment_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid, description = "Section id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = SectionSeatRequest,
    responses(
        (status = 201, body = SectionEnrolment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Not accepting enrolment, not registered, restricted, full or already seated", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<OfferingSectionId>,
    headers: HeaderMap,
    Json(cmd): Json<SectionSeatRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    require_idempotency_key(&headers)?;
    let override_reason = if cmd.override_eligibility {
        clean_reason(cmd.reason, true)?
    } else {
        None
    };
    let gate = Gate {
        capacity: true,
        window: true,
        registration: true,
        override_reason,
        home_institution: cmd.home_institution_id.map(InstitutionId),
        ..Gate::default()
    };
    let e = seat(&st, ctx, id, StudentId(cmd.student_id), gate).await?;
    Ok(enrolment_etag(StatusCode::CREATED, &e))
}

/// Roster of a section (`academic.read`; `?status=`, `?limit=` ≤ 200, `nextCursor`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/sections/{id}/enrolments",
    operation_id = "section_enrolment_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid, description = "Section id"), RosterQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = SectionEnrolmentList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<OfferingSectionId>,
    Query(q): Query<RosterQuery>,
) -> ApiResult<Json<SectionEnrolmentList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let s = load_section(&st.pool, tenant(ctx), id).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let offset: i64 = q
        .cursor
        .as_deref()
        .and_then(|c| c.parse().ok())
        .unwrap_or(0)
        .max(0);
    let sql = format!(
        "SELECT {ENROLMENT_COLUMNS} FROM academic.section_enrolment WHERE section_id = $1 \
         AND ($2::text IS NULL OR status = $2) ORDER BY student_number, enrolled_at LIMIT $3 OFFSET $4"
    );
    let rows = sqlx::query(&sql)
        .bind(s.id.as_uuid())
        .bind(q.status.map(|x| x.as_str()))
        .bind(limit + 1)
        .bind(offset)
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let mut items: Vec<SectionEnrolment> = rows
        .iter()
        .map(|r| enrolment_from_row(r).map(|e| enrolment_view(&e)))
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    let next_cursor = if items.len() as i64 > limit {
        items.truncate(usize::try_from(limit).unwrap_or(50));
        Some((offset + limit).to_string())
    } else {
        None
    };
    Ok(Json(SectionEnrolmentList { items, next_cursor }))
}

/// One section enrolment (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/enrolments/{id}",
    operation_id = "section_enrolment_get",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = SectionEnrolment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<SectionEnrolmentId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let e = load_enrolment(&st.pool, tenant(ctx), id).await?;
    Ok(enrolment_etag(StatusCode::OK, &enrolment_view(&e)))
}

/// Ends a seat: `drop` (inside the window, `enrollment_open`) or `withdraw` (later; reason).
pub async fn end_seat(
    st: &AcademicState,
    ctx: &AuthContext,
    id: SectionEnrolmentId,
    v: Version,
    to: SectionEnrolmentStatus,
    reason: Option<String>,
    own_student: Option<StudentId>,
) -> Result<SectionEnrolment, KernelError> {
    let e0 = load_enrolment(&st.pool, tenant(ctx), id).await?;
    if own_student.is_some_and(|s| s != e0.student_id) {
        return Err(KernelError::NotFound {
            entity: "section enrolment",
        });
    }
    let mut c = begin(st, ctx, e0.offering_id, None).await?;
    let e = lock_enrolment(&mut c.tx, c.o.tenant_id, id).await?;
    check_version(e.version, v)?;
    let (ty, schema) = match to {
        SectionEnrolmentStatus::Dropped => {
            if c.o.status != CourseOfferingStatus::EnrollmentOpen {
                return Err(conflict(format!(
                    "the course offering is {}; seats are dropped only while enrolment is open — withdraw instead",
                    c.o.status
                )));
            }
            (
                ev::SECTION_ENROLMENT_DROPPED,
                ev::SECTION_ENROLMENT_DROPPED_SCHEMA,
            )
        }
        _ => {
            if !matches!(
                c.o.status,
                CourseOfferingStatus::EnrollmentOpen
                    | CourseOfferingStatus::EnrollmentClosed
                    | CourseOfferingStatus::Active
                    | CourseOfferingStatus::Suspended
            ) {
                return Err(conflict(format!(
                    "the course offering is {}; seats cannot be withdrawn",
                    c.o.status
                )));
            }
            (
                ev::SECTION_ENROLMENT_WITHDRAWN,
                ev::SECTION_ENROLMENT_WITHDRAWN_SCHEMA,
            )
        }
    };
    let e = change_enrolment_status(
        &mut c.tx,
        &e,
        to,
        &StatusChange {
            cause_event_type: ty,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &c.actor,
    )
    .await?;
    enrolment_event(&mut c.tx, &c.scope, &e, ty, schema, &c.actor).await?;
    let verb = if to == SectionEnrolmentStatus::Dropped {
        "drop"
    } else {
        "withdraw"
    };
    publish::audit(
        &mut c.tx,
        &c.scope,
        &format!("academic.section_enrolment.{verb}"),
        "section_enrolment",
        e.id.to_string(),
        serde_json::json!({"reason": reason}),
        &c.actor,
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(enrolment_view(&e))
}

macro_rules! seat_command {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $to:expr, $reason_required:expr) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "offerings",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the enrolment"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<StepReason>),
            responses(
                (status = 200, body = SectionEnrolment, headers(("ETag" = String))),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State(st): State<AcademicState>,
            auth: Authenticated,
            Path(id): Path<SectionEnrolmentId>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
            let v = expect_version!(headers);
            let reason = body::<StepReason>(&raw)?.validated($reason_required)?;
            let e = end_seat(&st, ctx, id, v, $to, reason, None).await?;
            Ok(enrolment_etag(StatusCode::OK, &e))
        }
    };
}

seat_command!(
    drop,
    "/api/v1/academic/enrolments/{id}/drop",
    "section_enrolment_drop",
    "Drop a seat while enrolment is open (`offering.enrolment.manage`).",
    SectionEnrolmentStatus::Dropped,
    false
);
seat_command!(
    withdraw,
    "/api/v1/academic/enrolments/{id}/withdraw",
    "section_enrolment_withdraw",
    "Withdraw a seat after enrolment (reason required, `offering.enrolment.manage`).",
    SectionEnrolmentStatus::Withdrawn,
    true
);

/// Move a seat to another active section of the same offering (`offering.enrolment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/enrolments/{id}/transfer",
    operation_id = "section_enrolment_transfer",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the enrolment"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = SeatTransferRequest,
    responses(
        (status = 200, description = "The new seat", body = SectionEnrolment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn transfer(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<SectionEnrolmentId>,
    headers: HeaderMap,
    Json(cmd): Json<SeatTransferRequest>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    let v = expect_version!(headers);
    let reason = clean_reason(cmd.reason, false)?;
    let e0 = load_enrolment(&st.pool, tenant(ctx), id).await?;
    let mut c = begin(&st, ctx, e0.offering_id, None).await?;
    let e = lock_enrolment(&mut c.tx, c.o.tenant_id, id).await?;
    check_version(e.version, v)?;
    if !e.status.is_seated() {
        return Err(conflict(format!("the enrolment is {}", e.status)).into());
    }
    let target = lock_section(
        &mut c.tx,
        c.o.tenant_id,
        OfferingSectionId(cmd.to_section_id),
    )
    .await?;
    if target.offering_id != c.o.id {
        return Err(invalid(
            "toSectionId",
            "other_offering",
            "the target section belongs to another offering",
        )
        .into());
    }
    if target.id == e.section_id {
        return Err(invalid(
            "toSectionId",
            "same_section",
            "the student is already in this section",
        )
        .into());
    }
    if !matches!(
        c.o.status,
        CourseOfferingStatus::EnrollmentOpen
            | CourseOfferingStatus::EnrollmentClosed
            | CourseOfferingStatus::Active
    ) {
        return Err(conflict(format!(
            "the course offering is {}; seats cannot move",
            c.o.status
        ))
        .into());
    }
    if target.status != SectionStatus::Active {
        return Err(conflict(format!("the target section is {}", target.status)).into());
    }
    let today = business_today(&st, &c.scope, c.o.institution_id).await?;
    // The seat moves within the offering: only the section cap matters.
    if let Some(cap) = target.capacity {
        let used = seated_in_section(&mut *c.tx, target.id).await?
            + held_offers(&mut c.tx, Some(target.id), c.o.id, today, None).await?;
        if used >= i64::from(cap) {
            return Err(conflict("the target section is full").into());
        }
    }
    let old = change_enrolment_status(
        &mut c.tx,
        &e,
        SectionEnrolmentStatus::Transferred,
        &StatusChange {
            cause_event_type: ev::SECTION_ENROLMENT_ENROLLED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &c.actor,
    )
    .await?;
    let student = st
        .students
        .student(&c.scope, old.student_id)
        .await?
        .map(|mut s| {
            s.subject_ref = s.subject_ref.or(old.subject_ref.clone());
            s
        })
        .unwrap_or(StudentSummary {
            id: old.student_id,
            student_number: old.student_number.clone(),
            display_name: old.display_name.clone(),
            status: sutra_contracts::student::status::StudentStatus::Active,
            subject_ref: old.subject_ref.clone(),
        });
    let context = StudentContextSummary {
        id: old.student_context_id,
        student_id: old.student_id,
        institution_id: c.o.institution_id,
        status: StudentContextStatus::Active,
        programme_id: None,
        curriculum_version_id: None,
        campus_id: None,
        current_term_no: None,
    };
    let basis = SeatBasis {
        student,
        context,
        origin: "transfer",
        registration_id: old.registration_id,
        registration_line_id: old.registration_line_id,
        reason: reason.clone(),
    };
    let new = insert_seat(
        &mut c.tx,
        &c.scope,
        &c.o,
        &target,
        &basis,
        "transfer",
        Some(old.id),
        &c.actor,
    )
    .await?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.section_enrolment.transfer",
        "section_enrolment",
        old.id.to_string(),
        serde_json::json!({"toSectionId": target.id, "newEnrolmentId": new.id, "reason": reason}),
        &c.actor,
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(enrolment_etag(StatusCode::OK, &enrolment_view(&new)))
}

// ================================================================== waitlist

async fn load_wait(
    conn: &mut PgConnection,
    offering: CourseOfferingId,
    entry: Uuid,
    lock: bool,
) -> Result<WaitRow, KernelError> {
    let sql = format!(
        "SELECT {WAITLIST_COLUMNS} FROM academic.offering_waitlist_entry WHERE id = $1 AND offering_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    let row = sqlx::query(&sql)
        .bind(entry)
        .bind(offering.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "waitlist entry",
        })?;
    wait_from_row(&row)
}

#[allow(clippy::too_many_arguments)]
async fn set_wait_status(
    conn: &mut PgConnection,
    w: &WaitRow,
    to: WaitlistStatus,
    reason: Option<&str>,
    extra: &str,
    section: Option<Uuid>,
    expires: Option<NaiveDate>,
    enrolment: Option<Uuid>,
) -> Result<WaitRow, KernelError> {
    if !waitlist_can_transition(w.view.status, to) {
        return Err(conflict(format!(
            "the waitlist entry is {}; it cannot become {to}",
            w.view.status
        )));
    }
    let sql = format!(
        "UPDATE academic.offering_waitlist_entry SET status = $2, status_reason = COALESCE($3, status_reason), \
         offered_section_id = COALESCE($4, offered_section_id), offer_expires_on = COALESCE($5, offer_expires_on), \
         enrolment_id = COALESCE($6, enrolment_id){extra}, version = version + 1, updated_at = now() \
         WHERE id = $1 AND version = $7 RETURNING {WAITLIST_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(w.view.id)
        .bind(to.as_str())
        .bind(reason)
        .bind(section)
        .bind(expires)
        .bind(enrolment)
        .bind(w.view.version)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| conflict("the waitlist entry changed concurrently"))?;
    wait_from_row(&row)
}

async fn wait_event(
    c: &mut Cmd,
    w: &WaitRow,
    from: WaitlistStatus,
    ty: &'static str,
    schema: &'static str,
) -> Result<(), KernelError> {
    let data = StatusChangedV1 {
        aggregate_id: w.view.id,
        institution_id: c.o.institution_id.as_uuid(),
        offering_id: Some(c.o.id.as_uuid()),
        section_id: w.view.offered_section_id.or(w.view.section_id),
        student_id: Some(w.view.student_id),
        from_status: Some(from.as_str().to_owned()),
        to_status: w.view.status.as_str().to_owned(),
        version: w.view.version,
        reason: w.view.status_reason.clone(),
        process_instance_id: None,
        student_subject_ref: w.subject_ref.clone(),
        faculty_subject_ref: None,
    };
    publish::event(
        &mut c.tx,
        &c.scope,
        ty,
        schema,
        w.view.id.to_string(),
        &data,
        &c.actor,
    )
    .await
}

/// Adds a student to the waitlist of the locked offering (eligibility re-checked; no seat).
async fn join(
    st: &AcademicState,
    c: &mut Cmd,
    student_id: StudentId,
    section: Option<Uuid>,
) -> Result<OfferingWaitlistEntry, KernelError> {
    let sections = sections_of(&mut c.tx, c.o.tenant_id, c.o.id).await?;
    let probe = match section {
        Some(sid) => sections
            .iter()
            .find(|s| s.id.as_uuid() == sid)
            .ok_or_else(|| invalid("sectionId", "not_found", "not a section of this offering"))?,
        None => sections
            .iter()
            .find(|s| s.status == SectionStatus::Active)
            .ok_or_else(|| conflict("the offering has no active section"))?,
    }
    .clone();
    let gate = Gate {
        capacity: false,
        window: true,
        registration: true,
        ..Gate::default()
    };
    let b = evaluate(st, &mut c.tx, &c.scope, &c.o, &probe, student_id, &gate).await?;
    let seated: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.section_enrolment WHERE offering_id = $1 AND student_id = $2 AND status = 'enrolled'")
        .bind(c.o.id.as_uuid()).bind(student_id.as_uuid()).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    if seated > 0 {
        return Err(conflict(
            "the student already holds a seat in this offering",
        ));
    }
    let sql = format!(
        "INSERT INTO academic.offering_waitlist_entry (tenant_id, institution_id, offering_id, section_id, student_id, \
         student_context_id, student_number, display_name, subject_ref, position) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, \
           (SELECT COALESCE(max(position), 0) + 1 FROM academic.offering_waitlist_entry WHERE offering_id = $3)) \
         RETURNING {WAITLIST_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(c.o.tenant_id.as_uuid())
        .bind(c.o.institution_id.as_uuid())
        .bind(c.o.id.as_uuid())
        .bind(section)
        .bind(student_id.as_uuid())
        .bind(b.context.id.as_uuid())
        .bind(&b.student.student_number)
        .bind(&b.student.display_name)
        .bind(&b.student.subject_ref)
        .fetch_one(&mut *c.tx)
        .await
        .map_err(|e| {
            super::lifecycle::unique_code(e, "studentId", "the student is already on the waitlist")
        })?;
    let w = wait_from_row(&row)?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.offering_waitlist.join",
        "offering_waitlist_entry",
        w.view.id.to_string(),
        serde_json::json!({"studentId": student_id, "position": w.view.position}),
        &c.actor,
    )
    .await?;
    Ok(w.view)
}

/// Waitlist of an offering (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/waitlist",
    operation_id = "offering_waitlist_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid), WaitlistQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = OfferingWaitlist),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn waitlist_list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    Query(q): Query<WaitlistQuery>,
) -> ApiResult<Json<OfferingWaitlist>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let o = crate::shared::offering::load_offering(&st.pool, tenant(ctx), id).await?;
    let sql = format!(
        "SELECT {WAITLIST_COLUMNS} FROM academic.offering_waitlist_entry WHERE offering_id = $1 \
         AND ($2::text IS NULL OR status = $2) ORDER BY position"
    );
    let rows = sqlx::query(&sql)
        .bind(o.id.as_uuid())
        .bind(q.status.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| wait_from_row(r).map(|w| w.view))
        .collect::<Result<_, _>>()?;
    Ok(Json(OfferingWaitlist { items }))
}

/// Put a student on the waitlist (`offering.enrolment.manage`; eligibility is re-checked).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/waitlist",
    operation_id = "offering_waitlist_join",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = OfferingWaitlistJoin,
    responses(
        (status = 201, body = OfferingWaitlistEntry),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn waitlist_join(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<OfferingWaitlistJoin>,
) -> ApiResult<(StatusCode, Json<OfferingWaitlistEntry>)> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    require_idempotency_key(&headers)?;
    let mut c = begin(&st, ctx, id, None).await?;
    let w = join(&st, &mut c, StudentId(cmd.student_id), cmd.section_id).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(w)))
}

/// Resolve the waitlist when a seat is available (L3-D-007, `offering.enrolment.manage`):
/// expired offers lapse, the FIFO policy selects the first still-eligible entry (ineligible
/// entries are removed) and offers it a seat (`offering_waitlist.seat_offered`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/waitlist/promote",
    operation_id = "offering_waitlist_promote",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<OfferingWaitlistPromote>),
    responses(
        (status = 200, description = "The entry offered a seat", body = OfferingWaitlistEntry),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "No seat, nobody eligible waiting, or the manual pick breaks the FIFO policy", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn waitlist_promote(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Json<OfferingWaitlistEntry>> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    require_idempotency_key(&headers)?;
    let cmd = body::<OfferingWaitlistPromote>(&raw)?;
    let days = cmd.offer_days.unwrap_or(2);
    if !(1..=14).contains(&days) {
        return Err(invalid("offerDays", "out_of_range", "1 to 14").into());
    }
    let mut c = begin(&st, ctx, id, None).await?;
    let today = business_today(&st, &c.scope, c.o.institution_id).await?;
    sqlx::query(
        "UPDATE academic.offering_waitlist_entry SET status = 'expired', status_reason = 'seat offer lapsed', \
         resolved_at = now(), version = version + 1, updated_at = now() \
         WHERE offering_id = $1 AND status = 'offered' AND offer_expires_on < $2",
    )
    .bind(c.o.id.as_uuid())
    .bind(today)
    .execute(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    if !c.o.status.accepts_enrolment() {
        return Err(conflict(format!(
            "the course offering is {}; no seat can be offered",
            c.o.status
        ))
        .into());
    }
    let mut free = Vec::new();
    for s in sections_of(&mut c.tx, c.o.tenant_id, c.o.id).await? {
        if s.status == SectionStatus::Active && has_seat(&mut c.tx, &c.o, &s, today, None).await? {
            free.push(s.id.as_uuid());
        }
    }
    if free.is_empty() {
        return Err(conflict("no seat is available to offer").into());
    }
    let sql = format!(
        "SELECT {WAITLIST_COLUMNS} FROM academic.offering_waitlist_entry WHERE offering_id = $1 AND status = 'waiting' ORDER BY position FOR UPDATE"
    );
    let rows = sqlx::query(&sql)
        .bind(c.o.id.as_uuid())
        .fetch_all(&mut *c.tx)
        .await
        .map_err(map_db_error)?;
    if rows.is_empty() {
        return Err(conflict("nobody is waiting").into());
    }
    for r in &rows {
        let w = wait_from_row(r)?;
        // Section C eligibility facts, re-read now (never modified).
        let ctx_ok = st
            .students
            .live_context(&c.scope, StudentId(w.view.student_id), c.o.institution_id)
            .await?
            .is_some_and(|x| {
                x.status == StudentContextStatus::Active && x.id.as_uuid() == w.student_context_id
            });
        let reg_ok = st
            .registrations
            .term_registration(&c.scope, StudentId(w.view.student_id), c.o.academic_term_id)
            .await?
            .is_some_and(|r| r.covers(c.o.curriculum_component_id));
        if !(ctx_ok && reg_ok) {
            let why = if ctx_ok {
                "registration no longer covers the course"
            } else {
                "student context no longer active"
            };
            set_wait_status(
                &mut c.tx,
                &w,
                WaitlistStatus::Removed,
                Some(&format!("no longer eligible: {why}")),
                ", resolved_at = now()",
                None,
                None,
                None,
            )
            .await?;
            continue;
        }
        if cmd.entry_id.is_some_and(|e| e != w.view.id) {
            return Err(conflict("manual promotion must follow the waitlist policy: another eligible student is ahead").into());
        }
        let section = w
            .view
            .section_id
            .filter(|s| free.contains(s))
            .or_else(|| free.first().copied());
        let expires = today + chrono::Duration::days(i64::from(days));
        let offered = set_wait_status(
            &mut c.tx,
            &w,
            WaitlistStatus::Offered,
            Some("seat available"),
            ", offered_at = now()",
            section,
            Some(expires),
            None,
        )
        .await?;
        wait_event(
            &mut c,
            &offered,
            WaitlistStatus::Waiting,
            ev::OFFERING_WAITLIST_SEAT_OFFERED,
            ev::OFFERING_WAITLIST_SEAT_OFFERED_SCHEMA,
        )
        .await?;
        publish::audit(
            &mut c.tx,
            &c.scope,
            "academic.offering_waitlist.offer_seat",
            "offering_waitlist_entry",
            offered.view.id.to_string(),
            serde_json::json!({"sectionId": section, "expiresOn": expires}),
            &c.actor,
        )
        .await?;
        c.tx.commit().await.map_err(map_db_error)?;
        return Ok(Json(offered.view));
    }
    // Every waiting entry turned out ineligible: keep the removals, report nothing to offer.
    c.tx.commit().await.map_err(map_db_error)?;
    Err(conflict("nobody on the waitlist is still eligible").into())
}

/// Converts an offered entry into a seat (origin `waitlist`).
async fn accept_offer(
    st: &AcademicState,
    ctx: &AuthContext,
    offering: CourseOfferingId,
    entry: Uuid,
    v: Option<Version>,
    own_student: Option<StudentId>,
) -> Result<SectionEnrolment, KernelError> {
    let mut c = begin(st, ctx, offering, None).await?;
    let w = load_wait(&mut c.tx, c.o.id, entry, true).await?;
    if own_student.is_some_and(|s| s.as_uuid() != w.view.student_id) {
        return Err(KernelError::NotFound {
            entity: "waitlist entry",
        });
    }
    if let Some(v) = v {
        check_version(Version(w.view.version), v)?;
    }
    let today = business_today(st, &c.scope, c.o.institution_id).await?;
    if w.view.status != WaitlistStatus::Offered
        || w.view.offer_expires_on.is_some_and(|d| d < today)
    {
        return Err(conflict(
            "there is no open seat offer on this waitlist entry",
        ));
    }
    let sid = w
        .view
        .offered_section_id
        .ok_or_else(|| conflict("the offer has no section"))?;
    let s = lock_section(&mut c.tx, c.o.tenant_id, OfferingSectionId(sid)).await?;
    let gate = Gate {
        capacity: true,
        holding_entry: Some(w.view.id),
        window: false,
        registration: true,
        ..Gate::default()
    };
    let b = evaluate(
        st,
        &mut c.tx,
        &c.scope,
        &c.o,
        &s,
        StudentId(w.view.student_id),
        &gate,
    )
    .await?;
    let e = insert_seat(
        &mut c.tx, &c.scope, &c.o, &s, &b, "waitlist", None, &c.actor,
    )
    .await?;
    set_wait_status(
        &mut c.tx,
        &w,
        WaitlistStatus::Accepted,
        Some("seat accepted"),
        ", resolved_at = now()",
        None,
        None,
        Some(e.id.as_uuid()),
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(enrolment_view(&e))
}

/// Accept the seat offered to a waitlist entry on the student's behalf (`offering.enrolment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/waitlist/{entryId}/accept",
    operation_id = "offering_waitlist_accept",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("entryId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the entry"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, description = "The new seat", body = SectionEnrolment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn waitlist_accept(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, entry)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    let v = expect_version!(headers);
    let e = accept_offer(&st, ctx, id, entry, Some(v), None).await?;
    Ok(enrolment_etag(StatusCode::OK, &e))
}

/// Remove a waiting / offered entry (reason required, `offering.enrolment.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/waitlist/{entryId}/remove",
    operation_id = "offering_waitlist_remove",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("entryId" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag (version) of the entry"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body(content = Option<StepReason>),
    responses(
        (status = 200, body = OfferingWaitlistEntry),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn waitlist_remove(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path((id, entry)): Path<(CourseOfferingId, Uuid)>,
    headers: HeaderMap,
    raw: Bytes,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    let v = expect_version!(headers);
    let reason = body::<StepReason>(&raw)?.validated(true)?;
    let mut c = begin(&st, ctx, id, None).await?;
    let w = load_wait(&mut c.tx, c.o.id, entry, true).await?;
    check_version(Version(w.view.version), v)?;
    let w = set_wait_status(
        &mut c.tx,
        &w,
        WaitlistStatus::Removed,
        reason.as_deref(),
        ", resolved_at = now()",
        None,
        None,
        None,
    )
    .await?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.offering_waitlist.remove",
        "offering_waitlist_entry",
        w.view.id.to_string(),
        serde_json::json!({"reason": reason}),
        &c.actor,
    )
    .await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &w.view, Version(w.view.version)))
}

// ================================================================== student self-service

/// A course the student is seated in (§5.3).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyCourse {
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub enrolment_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub course_code: Option<String>,
    pub credits: f64,
    pub section_code: String,
    pub academic_term_id: Uuid,
    pub faculty: Vec<FacultyBrief>,
    /// Offering status.
    pub status: CourseOfferingStatus,
    pub enrolment_version: i64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MyCourseList {
    pub items: Vec<MyCourse>,
}

/// Section of an available offering.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvailableSection {
    pub section_id: Uuid,
    pub section_code: String,
    pub capacity: Option<i32>,
    pub seats_taken: i64,
    pub full: bool,
}

/// An offering open for enrolment in one of the student's institutions.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AvailableCourse {
    pub offering_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub credits: f64,
    pub academic_term_id: Uuid,
    pub status: CourseOfferingStatus,
    /// The student's registration covers the course component (seat allowed).
    pub covered: bool,
    pub seated: bool,
    pub waitlisted: bool,
    pub sections: Vec<AvailableSection>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AvailableCourseList {
    pub items: Vec<AvailableCourse>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MyEnrolments {
    pub items: Vec<SectionEnrolment>,
    pub waitlist: Vec<OfferingWaitlistEntry>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelfEnrol {
    pub section_id: Uuid,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SelfWaitlist {
    pub offering_id: Uuid,
    pub section_id: Option<Uuid>,
}

/// My courses: offerings I am seated in, with section and active faculty (`academic.self`).
/// Row of the `/me/courses` query (offering, section, enrolment, codes, title, credits, …).
type AvailableRow = (
    Uuid,
    Uuid,
    Uuid,
    String,
    String,
    Option<String>,
    f64,
    String,
    Uuid,
    String,
    i64,
);

#[utoipa::path(
    get,
    path = "/api/v1/academic/me/courses",
    operation_id = "offering_self_courses",
    tag = "offerings",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = MyCourseList),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, description = "No student is linked to this login", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn my_courses(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyCourseList>> {
    let me = crate::shared::me::me(&st, &auth.0).await?;
    let t = tenant(&auth.0);
    let rows: Vec<AvailableRow> = sqlx::query_as(
        "SELECT o.id, s.id, e.id, o.offering_code, o.title, o.course_code, o.credits::float8, s.section_code, \
         o.academic_term_id, o.status, e.version FROM academic.section_enrolment e \
         JOIN academic.offering_section s ON s.id = e.section_id JOIN academic.course_offering o ON o.id = e.offering_id \
         WHERE e.tenant_id = $1 AND e.student_id = $2 AND e.status = 'enrolled' ORDER BY o.offering_code",
    )
    .bind(t.as_uuid())
    .bind(me.id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::new();
    for (oid, sid, eid, code, title, course_code, credits, scode, term, status, ev) in rows {
        let faculty = st
            .offerings
            .faculty_of_section(&ExecutionScope::tenant(t), OfferingSectionId(sid))
            .await?
            .into_iter()
            .filter(|f| f.status == FacultyAssignmentStatus::Active)
            .map(|f| FacultyBrief {
                assignment_id: f.assignment_id.as_uuid(),
                faculty_subject_ref: f.faculty_subject_ref,
                faculty_name: f.faculty_name,
                role: f.role,
                status: f.status,
            })
            .collect();
        items.push(MyCourse {
            offering_id: oid,
            section_id: sid,
            enrolment_id: eid,
            offering_code: code,
            title,
            course_code,
            credits,
            section_code: scode,
            academic_term_id: term,
            faculty,
            status: CourseOfferingStatus::parse(&status).unwrap_or(CourseOfferingStatus::Active),
            enrolment_version: ev,
        });
    }
    Ok(Json(MyCourseList { items }))
}

/// Offerings open for enrolment that I could join, with my registration coverage (`academic.self`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/me/courses/available",
    operation_id = "offering_self_available",
    tag = "offerings",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = AvailableCourseList),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn available(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<AvailableCourseList>> {
    let me = crate::shared::me::me(&st, &auth.0).await?;
    let t = tenant(&auth.0);
    let scope = ExecutionScope::tenant(t);
    let institutions: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT institution_id FROM academic.course_offering WHERE tenant_id = $1 AND status IN ('enrollment_open', 'active')",
    )
    .bind(t.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let mut items = Vec::new();
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    for inst in institutions {
        let Some(ctx) = st
            .students
            .live_context(&scope, me.id, InstitutionId(inst))
            .await?
        else {
            continue;
        };
        if ctx.status != StudentContextStatus::Active {
            continue;
        }
        let sql = format!(
            "SELECT {} FROM academic.course_offering WHERE tenant_id = $1 AND institution_id = $2 \
             AND status IN ('enrollment_open', 'active') ORDER BY offering_code",
            crate::shared::offering::OFFERING_COLUMNS
        );
        let rows = sqlx::query(&sql)
            .bind(t.as_uuid())
            .bind(inst)
            .fetch_all(&mut *conn)
            .await
            .map_err(map_db_error)?;
        for r in &rows {
            let o = crate::shared::offering::offering_from_row(r).map_err(map_db_error)?;
            let covered = st
                .registrations
                .term_registration(&scope, me.id, o.academic_term_id)
                .await?
                .is_some_and(|reg| reg.covers(o.curriculum_component_id));
            let (seated, waitlisted): (i64, i64) = sqlx::query_as(
                "SELECT (SELECT count(*) FROM academic.section_enrolment WHERE offering_id = $1 AND student_id = $2 AND status = 'enrolled'), \
                        (SELECT count(*) FROM academic.offering_waitlist_entry WHERE offering_id = $1 AND student_id = $2 AND status IN ('waiting', 'offered'))",
            )
            .bind(o.id.as_uuid())
            .bind(me.id.as_uuid())
            .fetch_one(&mut *conn)
            .await
            .map_err(map_db_error)?;
            let mut sections = Vec::new();
            for s in sections_of(&mut conn, t, o.id).await? {
                if s.status != SectionStatus::Active {
                    continue;
                }
                let taken = seated_in_section(&mut *conn, s.id).await?;
                sections.push(AvailableSection {
                    section_id: s.id.as_uuid(),
                    section_code: s.section_code.clone(),
                    capacity: s.capacity,
                    seats_taken: taken,
                    full: s.capacity.is_some_and(|c| taken >= i64::from(c)),
                });
            }
            items.push(AvailableCourse {
                offering_id: o.id.as_uuid(),
                offering_code: o.offering_code.clone(),
                title: o.title.clone(),
                credits: o.credits,
                academic_term_id: o.academic_term_id,
                status: o.status,
                covered,
                seated: seated > 0,
                waitlisted: waitlisted > 0,
                sections,
            });
        }
    }
    Ok(Json(AvailableCourseList { items }))
}

/// My seats (any status) and waitlist entries (`academic.self`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/me/enrolments",
    operation_id = "offering_self_enrolment_list",
    tag = "offerings",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = MyEnrolments),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn my_enrolments(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyEnrolments>> {
    let me = crate::shared::me::me(&st, &auth.0).await?;
    let t = tenant(&auth.0);
    let sql = format!("SELECT {ENROLMENT_COLUMNS} FROM academic.section_enrolment WHERE tenant_id = $1 AND student_id = $2 ORDER BY enrolled_at DESC");
    let rows = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(me.id.as_uuid())
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let items = rows
        .iter()
        .map(|r| enrolment_from_row(r).map(|e| enrolment_view(&e)))
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    let sql = format!("SELECT {WAITLIST_COLUMNS} FROM academic.offering_waitlist_entry WHERE tenant_id = $1 AND student_id = $2 ORDER BY created_at DESC");
    let rows = sqlx::query(&sql)
        .bind(t.as_uuid())
        .bind(me.id.as_uuid())
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let waitlist = rows
        .iter()
        .map(|r| wait_from_row(r).map(|w| w.view))
        .collect::<Result<_, _>>()?;
    Ok(Json(MyEnrolments { items, waitlist }))
}

/// Enrol myself in a section — allowed only when my registration covers the course (`academic.self`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/me/enrolments",
    operation_id = "offering_self_enrol",
    tag = "offerings",
    params(
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = SelfEnrol,
    responses(
        (status = 201, body = SectionEnrolment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_enrol(
    State(st): State<AcademicState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<SelfEnrol>,
) -> ApiResult<Response> {
    let me = crate::shared::me::me(&st, &auth.0).await?;
    require_idempotency_key(&headers)?;
    let gate = Gate {
        capacity: true,
        window: true,
        registration: true,
        ..Gate::default()
    };
    let e = seat(&st, &auth.0, OfferingSectionId(cmd.section_id), me.id, gate).await?;
    Ok(enrolment_etag(StatusCode::CREATED, &e))
}

/// Drop my own seat while enrolment is open (`academic.self`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/me/enrolments/{id}/drop",
    operation_id = "offering_self_drop",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the enrolment"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = SectionEnrolment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_drop(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<SectionEnrolmentId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let me = crate::shared::me::me(&st, &auth.0).await?;
    let v = expect_version!(headers);
    let e = end_seat(
        &st,
        &auth.0,
        id,
        v,
        SectionEnrolmentStatus::Dropped,
        Some("dropped by the student".into()),
        Some(me.id),
    )
    .await?;
    Ok(enrolment_etag(StatusCode::OK, &e))
}

/// Join the waitlist of a full offering (`academic.self`; registration must cover the course).
#[utoipa::path(
    post,
    path = "/api/v1/academic/me/waitlist",
    operation_id = "offering_self_waitlist_join",
    tag = "offerings",
    params(
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = SelfWaitlist,
    responses(
        (status = 201, body = OfferingWaitlistEntry),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_waitlist(
    State(st): State<AcademicState>,
    auth: Authenticated,
    headers: HeaderMap,
    Json(cmd): Json<SelfWaitlist>,
) -> ApiResult<(StatusCode, Json<OfferingWaitlistEntry>)> {
    let me = crate::shared::me::me(&st, &auth.0).await?;
    require_idempotency_key(&headers)?;
    let mut c = begin(&st, &auth.0, CourseOfferingId(cmd.offering_id), None).await?;
    let w = join(&st, &mut c, me.id, cmd.section_id).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok((StatusCode::CREATED, Json(w)))
}

/// Accept the seat I was offered from the waitlist (`academic.self`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/me/waitlist/{id}/accept",
    operation_id = "offering_self_waitlist_accept",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid, description = "Waitlist entry id"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, description = "The new seat", body = SectionEnrolment, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn self_waitlist_accept(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let me = crate::shared::me::me(&st, &auth.0).await?;
    let offering: Uuid = sqlx::query_scalar("SELECT offering_id FROM academic.offering_waitlist_entry WHERE id = $1 AND tenant_id = $2 AND student_id = $3")
        .bind(id)
        .bind(tenant(&auth.0).as_uuid())
        .bind(me.id.as_uuid())
        .fetch_optional(&st.pool)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity: "waitlist entry" })?;
    let e = accept_offer(
        &st,
        &auth.0,
        CourseOfferingId(offering),
        id,
        None,
        Some(me.id),
    )
    .await?;
    Ok(enrolment_etag(StatusCode::OK, &e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waitlist_machine() {
        use WaitlistStatus::*;
        assert!(waitlist_can_transition(Waiting, Offered));
        assert!(waitlist_can_transition(Offered, Accepted));
        assert!(waitlist_can_transition(Offered, Expired));
        assert!(!waitlist_can_transition(Accepted, Waiting));
        assert!(!waitlist_can_transition(Expired, Offered));
        assert!(!waitlist_can_transition(Waiting, Accepted));
    }
}

// ================================================================== eligibility override requests
//
// Part 1 · S3-appr — DEF-P1-DEMO-006. In human approval mode an override used to wait for a
// decision that no endpoint could make. The override is now a persisted, governed request:
// the maker (`offering.enrolment.manage`) records it, an independent checker
// (`academic.offering.decide`, never the requester) approves — which seats the student through
// the same seat gate (capacity is still never overridden) — or rejects it.

/// An eligibility override request (`pending` → `approved` with the seat, or `rejected`).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnrolmentOverrideRequest {
    pub id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub student_id: Uuid,
    pub home_institution_id: Option<Uuid>,
    pub reason: String,
    /// `pending` | `approved` | `rejected`.
    pub status: String,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub decision_reason: Option<String>,
    /// The seat created by the approval.
    pub enrolment_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

/// Request an eligibility override for one student in a section.
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EnrolmentOverrideCreate {
    pub student_id: Uuid,
    /// Why the student should be seated despite the failed eligibility checks (required).
    pub reason: String,
    /// Home institution of a cross-institution student.
    pub home_institution_id: Option<Uuid>,
}

const OVERRIDE_COLUMNS: &str =
    "id, offering_id, section_id, student_id, home_institution_id, reason, \
     status, requested_by, decided_by, decided_at, decision_reason, enrolment_id, \
     process_instance_id, version, created_at";

fn override_from_row(r: &PgRow) -> Result<EnrolmentOverrideRequest, sqlx::Error> {
    Ok(EnrolmentOverrideRequest {
        id: r.try_get("id")?,
        offering_id: r.try_get("offering_id")?,
        section_id: r.try_get("section_id")?,
        student_id: r.try_get("student_id")?,
        home_institution_id: r.try_get("home_institution_id")?,
        reason: r.try_get("reason")?,
        status: r.try_get("status")?,
        requested_by: r.try_get("requested_by")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
        decision_reason: r.try_get("decision_reason")?,
        enrolment_id: r.try_get("enrolment_id")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

async fn load_override(
    conn: &mut PgConnection,
    tenant: sutra_kernel::scope::TenantId,
    id: Uuid,
    lock: bool,
) -> Result<EnrolmentOverrideRequest, KernelError> {
    let lock_sql = if lock { " FOR UPDATE" } else { "" };
    let row = sqlx::query(&format!(
        "SELECT {OVERRIDE_COLUMNS} FROM academic.enrolment_override_request \
         WHERE id = $1 AND tenant_id = $2{lock_sql}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "enrolment override request",
    })?;
    override_from_row(&row).map_err(map_db_error)
}

fn override_gate(reason: &str, home: Option<Uuid>) -> Gate {
    Gate {
        capacity: true,
        window: true,
        registration: true,
        override_reason: Some(reason.to_owned()),
        override_approved: true,
        home_institution: home.map(InstitutionId),
        ..Gate::default()
    }
}

/// Seats the student of an approved override (same gate, capacity never overridden) and marks
/// the request approved by `decided_by`.
async fn seat_approved_override(
    st: &AcademicState,
    c: &mut Cmd,
    r: &EnrolmentOverrideRequest,
    decided_by: &str,
    reason: Option<&str>,
) -> Result<(), KernelError> {
    let s = lock_section(&mut c.tx, c.o.tenant_id, OfferingSectionId(r.section_id)).await?;
    let gate = override_gate(&r.reason, r.home_institution_id);
    let b = evaluate(
        st,
        &mut c.tx,
        &c.scope,
        &c.o,
        &s,
        StudentId(r.student_id),
        &gate,
    )
    .await?;
    let origin = b.origin;
    let e = insert_seat(&mut c.tx, &c.scope, &c.o, &s, &b, origin, None, &c.actor).await?;
    sqlx::query(
        "UPDATE academic.enrolment_override_request SET status = 'approved', decided_by = $3, \
         decided_at = now(), decision_reason = $4, enrolment_id = $5, version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(r.id)
    .bind(c.o.tenant_id.as_uuid())
    .bind(decided_by)
    .bind(reason)
    .bind(e.id.as_uuid())
    .execute(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Request an eligibility override (`offering.enrolment.manage`): the student fails the seat
/// gate only on overridable checks (registration, restrictions, window). Pending until an
/// independent approver decides it (auto-approval in development seats at once).
#[utoipa::path(
    post,
    path = "/api/v1/academic/sections/{id}/enrolment-overrides",
    operation_id = "section_enrolment_override_request",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid, description = "Section id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = EnrolmentOverrideCreate,
    responses(
        (status = 201, body = EnrolmentOverrideRequest, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "No override needed, not overridable (full, inactive) or already requested", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn override_request(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<OfferingSectionId>,
    headers: HeaderMap,
    Json(cmd): Json<EnrolmentOverrideCreate>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingEnrolmentManage)?;
    require_idempotency_key(&headers)?;
    let reason = clean_reason(Some(cmd.reason), true)?.unwrap_or_default();
    let t = tenant(ctx);
    let student = StudentId(cmd.student_id);
    let gate = override_gate(&reason, cmd.home_institution_id);
    let s0 = load_section(&st.pool, t, id).await?;
    // Dry run of the seat gate: an override must be what is missing (nothing is written).
    {
        let mut probe = begin(&st, ctx, s0.offering_id, None).await?;
        let s = lock_section(&mut probe.tx, t, id).await?;
        let b = evaluate(
            &st,
            &mut probe.tx,
            &probe.scope,
            &probe.o,
            &s,
            student,
            &gate,
        )
        .await?;
        if b.origin != "override" {
            return Err(conflict(
                "the student is eligible for this section; seat them without an override",
            )
            .into());
        }
    }
    let rid = Uuid::now_v7();
    let scope = crate::shared::scope::institution_scope(ctx, s0.institution_id);
    let outcome = request_approval(
        &st,
        &scope,
        process::ENROLMENT_ELIGIBILITY_OVERRIDE,
        business_key("enrolment_override", rid, 1),
        BTreeMap::from([
            ("sectionId".to_owned(), serde_json::json!(id.as_uuid())),
            ("studentId".to_owned(), serde_json::json!(cmd.student_id)),
            ("reason".to_owned(), serde_json::json!(reason)),
        ]),
    )
    .await?;
    let mut c = begin(&st, ctx, s0.offering_id, None).await?;
    sqlx::query(
        "INSERT INTO academic.enrolment_override_request (id, tenant_id, institution_id, offering_id, \
         section_id, student_id, home_institution_id, reason, requested_by, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(rid)
    .bind(t.as_uuid())
    .bind(c.o.institution_id.as_uuid())
    .bind(c.o.id.as_uuid())
    .bind(id.as_uuid())
    .bind(cmd.student_id)
    .bind(cmd.home_institution_id)
    .bind(&reason)
    .bind(c.actor.id.as_deref())
    .bind(outcome.process_instance_id())
    .execute(&mut *c.tx)
    .await
    .map_err(|e| {
        super::lifecycle::unique_code(
            e,
            "studentId",
            "an override for this student and section already awaits a decision",
        )
    })?;
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.enrolment_override.request",
        "enrolment_override_request",
        rid.to_string(),
        serde_json::json!({"sectionId": id.as_uuid(), "studentId": cmd.student_id, "reason": reason}),
        &c.actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let r = load_override(&mut c.tx, t, rid, true).await?;
        seat_approved_override(
            &st,
            &mut c,
            &r,
            crate::shared::approval::AUTO_APPROVER,
            None,
        )
        .await?;
    }
    let r = load_override(&mut c.tx, t, rid, false).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::CREATED, &r, Version(r.version)))
}

/// One eligibility override request (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/enrolment-overrides/{id}",
    operation_id = "section_enrolment_override_get",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = EnrolmentOverrideRequest, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn override_get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let r = load_override(&mut conn, tenant(ctx), id, false).await?;
    Ok(with_etag(StatusCode::OK, &r, Version(r.version)))
}

/// Decide an eligibility override (`academic.offering.decide`, step-up; never the requester):
/// approve seats the student through the seat gate (capacity is never overridden); reject
/// (reason required) closes the request.
#[utoipa::path(
    post,
    path = "/api/v1/academic/enrolment-overrides/{id}/decision",
    operation_id = "section_enrolment_override_decision",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the override request"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = EnrolmentOverrideRequest, headers(("ETag" = String))),
        (status = 403, description = "Missing permission, or the requester deciding their own override", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Not pending, or the seat gate fails (e.g. the section is full)", body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn override_decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingDecide)?;
    // Part 1 · S3-appr (DEF-X-002): the requester of the override never decides it.
    sutra_persistence::approval::guard_decision_for_row(
        &st.pool,
        ctx,
        "academic.enrolment_override_request",
        "id",
        id,
        &["requested_by"],
        "enrolment eligibility override",
    )
    .await?;
    let v = expect_version!(headers);
    let reason = clean_reason(d.reason, d.decision == StepDecision::Reject)?;
    let t = tenant(ctx);
    let r0 = {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        load_override(&mut conn, t, id, false).await?
    };
    let mut c = begin(&st, ctx, CourseOfferingId(r0.offering_id), None).await?;
    let r = load_override(&mut c.tx, t, id, true).await?;
    check_version(Version(r.version), v)?;
    if r.status != "pending" {
        return Err(conflict(format!(
            "the override request is {}; nothing awaits a decision",
            r.status
        ))
        .into());
    }
    let decider = c.actor.id.clone().unwrap_or_default();
    match d.decision {
        StepDecision::Approve => {
            seat_approved_override(&st, &mut c, &r, &decider, reason.as_deref()).await?;
        }
        StepDecision::Reject => {
            sqlx::query(
                "UPDATE academic.enrolment_override_request SET status = 'rejected', decided_by = $3, \
                 decided_at = now(), decision_reason = $4, version = version + 1, updated_at = now() \
                 WHERE id = $1 AND tenant_id = $2",
            )
            .bind(id)
            .bind(t.as_uuid())
            .bind(&decider)
            .bind(reason.as_deref())
            .execute(&mut *c.tx)
            .await
            .map_err(map_db_error)?;
        }
    }
    publish::audit(
        &mut c.tx,
        &c.scope,
        "academic.enrolment_override.decide",
        "enrolment_override_request",
        id.to_string(),
        serde_json::json!({
            "decision": if d.decision == StepDecision::Approve { "approve" } else { "reject" },
            "reason": reason,
        }),
        &c.actor,
    )
    .await?;
    let out = load_override(&mut c.tx, t, id, false).await?;
    c.tx.commit().await.map_err(map_db_error)?;
    Ok(with_etag(StatusCode::OK, &out, Version(out.version)))
}
