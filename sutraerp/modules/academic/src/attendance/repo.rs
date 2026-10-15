//! Teaching sessions, attendance capture/finalisation, governed corrections, attendance policy
//! and threshold evaluation (L3-D-023…025).

use super::domain::{self, CountingRules, Tally};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::faculty::require_teaching;
use crate::shared::guard::{require_deliverable, require_institution_writable};
use crate::shared::scope::institution_scope;
use crate::shared::{publish, AcademicState, Actor};
use crate::timetable::kit::{
    self, check_version, illegal, invalid, required_text, SectionCtx, StatusEvent,
};
use crate::timetable::repo::{operational_slot, parse_time};
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::delivery::*;
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::ports::RosterEntry;
use sutra_contracts::academic::status::{
    AttendanceCaptureStatus, AttendanceMark, ChangeRequestStatus, PolicyVersionStatus,
    TeachingSessionStatus,
};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

// ------------------------------------------------------------------ DTOs

/// One student's attendance line of a session (roster entry + recorded mark, if any).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceRecord {
    /// `null` until a mark is recorded.
    pub id: Option<Uuid>,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub mark: Option<AttendanceMark>,
    pub remarks: Option<String>,
    pub version: Option<i64>,
    /// `false` when the student is no longer seated (kept for finalised history).
    #[serde(default = "yes")]
    pub seated: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RecordRow {
    id: Uuid,
    student_id: Uuid,
    student_number: String,
    display_name: String,
    subject_ref: Option<String>,
    mark: AttendanceMark,
    remarks: Option<String>,
    version: i64,
}

/// A teaching session with its attendance (contract §5.3).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeachingSession {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub timetable_slot_id: Option<Uuid>,
    pub session_date: NaiveDate,
    /// `HH:MM`.
    pub starts_at: String,
    /// `HH:MM`.
    pub ends_at: String,
    pub topic: Option<String>,
    pub notes: Option<String>,
    pub status: TeachingSessionStatus,
    pub attendance_status: AttendanceCaptureStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub interruption_count: i32,
    pub started_at: Option<DateTime<Utc>>,
    pub ended_at: Option<DateTime<Utc>>,
    pub attendance_finalised_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub records: Vec<AttendanceRecord>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TeachingSessionList {
    pub items: Vec<TeachingSession>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewTeachingSession {
    pub session_date: NaiveDate,
    /// From an operational timetable slot of the section (times are taken from the slot).
    pub timetable_slot_id: Option<Uuid>,
    /// `HH:MM` (required without a slot).
    pub starts_at: Option<String>,
    pub ends_at: Option<String>,
    pub topic: Option<String>,
}

/// Body of `complete` (instructional progress of the session).
#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionProgress {
    pub topic: Option<String>,
    pub notes: Option<String>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkInput {
    pub student_id: Uuid,
    pub mark: AttendanceMark,
    pub remarks: Option<String>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaptureAttendance {
    pub records: Vec<MarkInput>,
}

/// Governed correction of a finalised attendance record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceCorrection {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub record_id: Uuid,
    pub student_id: Uuid,
    pub from_mark: AttendanceMark,
    pub to_mark: AttendanceMark,
    pub reason: String,
    pub evidence: Option<String>,
    pub status: ChangeRequestStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AttendanceCorrectionList {
    pub items: Vec<AttendanceCorrection>,
}

/// A correction in an institution's decision queue, with the student, session and section it
/// touches (DEF-D-003: the HOD / administrator decision screen).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceCorrectionQueueItem {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub session_id: Uuid,
    pub record_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub section_id: Uuid,
    pub offering_code: String,
    pub section_code: String,
    pub session_date: NaiveDate,
    /// `HH:MM`.
    pub starts_at: String,
    pub from_mark: AttendanceMark,
    pub to_mark: AttendanceMark,
    pub reason: String,
    pub evidence: Option<String>,
    pub status: ChangeRequestStatus,
    pub status_reason: Option<String>,
    pub requested_by: Option<String>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AttendanceCorrectionQueue {
    pub items: Vec<AttendanceCorrectionQueueItem>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAttendanceCorrection {
    pub record_id: Uuid,
    pub to_mark: AttendanceMark,
    pub reason: String,
    pub evidence: Option<String>,
}

/// Attendance policy version (threshold rules).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendancePolicy {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub name: String,
    pub threshold_percent: f64,
    pub min_sessions: i32,
    pub late_counts_as_present: bool,
    pub exclude_excused: bool,
    pub effective_from: Option<NaiveDate>,
    pub status: PolicyVersionStatus,
    pub status_reason: Option<String>,
    pub process_instance_id: Option<String>,
    pub supersedes_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AttendancePolicy {
    pub fn rules(&self) -> CountingRules {
        CountingRules {
            threshold_percent: self.threshold_percent,
            min_sessions: self.min_sessions,
            late_counts_as_present: self.late_counts_as_present,
            exclude_excused: self.exclude_excused,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AttendancePolicyList {
    pub items: Vec<AttendancePolicy>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewAttendancePolicy {
    pub name: String,
    pub threshold_percent: f64,
    pub min_sessions: Option<i32>,
    pub late_counts_as_present: Option<bool>,
    pub exclude_excused: Option<bool>,
    pub effective_from: Option<NaiveDate>,
}

/// One student's attendance in a section (contract §5.3).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceSummaryLine {
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub attended: i32,
    pub total: i32,
    pub percent: f64,
    pub below_threshold: bool,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AttendanceSummary {
    pub section_id: Uuid,
    pub threshold_percent: f64,
    /// The effective policy (`null` = default 75 % rule, no breach events).
    pub policy_id: Option<Uuid>,
    pub finalised_sessions: i64,
    pub items: Vec<AttendanceSummaryLine>,
}

/// The student's own attendance per seated section.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyAttendanceLine {
    pub section_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub section_code: String,
    pub attended: i32,
    pub total: i32,
    pub percent: f64,
    pub below_threshold: bool,
    pub threshold_percent: f64,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MyAttendance {
    pub items: Vec<MyAttendanceLine>,
}

/// A taught section with its recent sessions (faculty Attendance).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaughtSectionSessions {
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub section_code: String,
    pub seated_count: usize,
    pub sessions: Vec<TeachingSession>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct FacultyAttendance {
    pub items: Vec<TaughtSectionSessions>,
}

const TS: &str = "teaching_session";
const TS_ENTITY: &str = "teaching session";

// ------------------------------------------------------------------ helpers

async fn roster(
    state: &AcademicState,
    tenant: TenantId,
    section: Uuid,
) -> Result<Vec<RosterEntry>, KernelError> {
    state
        .offerings
        .roster(
            &ExecutionScope::tenant(tenant),
            OfferingSectionId(section),
            true,
        )
        .await
}

async fn records_of<'e, E: sqlx::PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    session: Uuid,
) -> Result<Vec<RecordRow>, KernelError> {
    kit::list_by(
        ex,
        "attendance_record",
        tenant,
        "session_id",
        session,
        None,
        "t.student_number, t.id",
        5000,
    )
    .await
}

/// Session + attendance lines (roster at read time for open capture; recorded lines once finalised).
pub async fn get(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<TeachingSession, KernelError> {
    let mut s: TeachingSession = kit::fetch(&state.pool, TS, TS_ENTITY, tenant, id).await?;
    s.records = lines(state, tenant, &s).await?;
    Ok(s)
}

async fn lines(
    state: &AcademicState,
    tenant: TenantId,
    s: &TeachingSession,
) -> Result<Vec<AttendanceRecord>, KernelError> {
    let recs = records_of(&state.pool, tenant, s.id).await?;
    let seated = roster(state, tenant, s.section_id).await?;
    let mut out: Vec<AttendanceRecord> = Vec::new();
    if s.attendance_status != AttendanceCaptureStatus::Finalised {
        for r in &seated {
            let rec = recs.iter().find(|x| x.student_id == r.student_id.as_uuid());
            out.push(AttendanceRecord {
                id: rec.map(|x| x.id),
                student_id: r.student_id.as_uuid(),
                student_number: r.student_number.clone(),
                display_name: r.display_name.clone(),
                mark: rec.map(|x| x.mark),
                remarks: rec.and_then(|x| x.remarks.clone()),
                version: rec.map(|x| x.version),
                seated: true,
            });
        }
    }
    for rec in &recs {
        if out.iter().any(|o| o.student_id == rec.student_id) {
            continue;
        }
        out.push(AttendanceRecord {
            id: Some(rec.id),
            student_id: rec.student_id,
            student_number: rec.student_number.clone(),
            display_name: rec.display_name.clone(),
            mark: Some(rec.mark),
            remarks: rec.remarks.clone(),
            version: Some(rec.version),
            seated: seated
                .iter()
                .any(|r| r.student_id.as_uuid() == rec.student_id),
        });
    }
    out.sort_by(|a, b| a.student_number.cmp(&b.student_number));
    Ok(out)
}

pub async fn list_for_section(
    state: &AcademicState,
    tenant: TenantId,
    section: Uuid,
    status: Option<&str>,
    limit: i64,
) -> Result<Vec<TeachingSession>, KernelError> {
    kit::list_by(
        &state.pool,
        TS,
        tenant,
        "section_id",
        section,
        status,
        "t.session_date DESC, t.starts_at DESC",
        limit,
    )
    .await
}

struct LockedSession {
    s: TeachingSession,
    ctx: SectionCtx,
}

/// Loads the session, authorises the caller on its section (`manage` or an active assignment),
/// applies the institution writable rule and locks the row.
async fn lock_session(
    state: &AcademicState,
    conn: &mut PgConnection,
    auth: &AuthContext,
    id: Uuid,
    expected: Option<Version>,
    manage: Permission,
) -> Result<LockedSession, KernelError> {
    let tenant = auth.scope.tenant_id;
    let cur: TeachingSession = kit::fetch(&state.pool, TS, TS_ENTITY, tenant, id).await?;
    require_teaching(state, auth, OfferingSectionId(cur.section_id), manage).await?;
    let sctx = kit::section_ctx(state, auth, cur.section_id).await?;
    require_institution_writable(state, &sctx.scope, sctx.section.institution_id).await?;
    let s: TeachingSession = kit::lock(conn, TS, TS_ENTITY, tenant, id).await?;
    if let Some(v) = expected {
        check_version(s.version, v)?;
    }
    Ok(LockedSession { s, ctx: sctx })
}

async fn emit_session(
    conn: &mut PgConnection,
    c: &SectionCtx,
    s: &TeachingSession,
    from: &str,
    ev: (&'static str, &'static str),
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    StatusEvent {
        aggregate_id: s.id,
        institution_id: c.institution_uuid(),
        offering_id: Some(s.offering_id),
        section_id: Some(s.section_id),
        from: Some(from),
        to: s.status.as_str(),
        version: s.version,
        reason,
        ..Default::default()
    }
    .emit(conn, &c.scope, ev, actor)
    .await
}

// ------------------------------------------------------------------ sessions

pub async fn create_session(
    state: &AcademicState,
    auth: &AuthContext,
    section_id: Uuid,
    cmd: NewTeachingSession,
    actor: &Actor,
) -> Result<TeachingSession, KernelError> {
    require_teaching(
        state,
        auth,
        OfferingSectionId(section_id),
        Permission::AttendanceManage,
    )
    .await?;
    let c = kit::deliverable_section(state, auth, section_id).await?;
    let tenant = auth.scope.tenant_id;
    let (starts, ends) = match cmd.timetable_slot_id {
        Some(slot) => {
            let (dow, st, en) = operational_slot(state, tenant, slot, section_id).await?;
            if i64::from(dow) != i64::from(cmd.session_date.weekday().number_from_monday()) {
                return Err(invalid(
                    "sessionDate",
                    "slot_day",
                    "the session date does not fall on the slot's weekday",
                ));
            }
            (st, en)
        }
        None => {
            let st = parse_time("startsAt", cmd.starts_at.as_deref().unwrap_or(""))?;
            let en = parse_time("endsAt", cmd.ends_at.as_deref().unwrap_or(""))?;
            if en <= st {
                return Err(invalid("endsAt", "range", "endsAt must be after startsAt"));
            }
            (st, en)
        }
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let s: TeachingSession = kit::insert(
        &mut tx,
        TS,
        json!({
            "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "offering_id": c.offering.id.as_uuid(),
            "section_id": section_id, "timetable_slot_id": cmd.timetable_slot_id, "session_date": cmd.session_date,
            "starts_at": starts.format("%H:%M").to_string(), "ends_at": ends.format("%H:%M").to_string(),
            "topic": kit::opt_text(cmd.topic.as_deref()), "created_by": actor.id,
        }),
    )
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("the section already has a session at that date and time".into()),
        other => other,
    })?;
    kit::audit(
        &mut tx,
        &c.scope,
        TS,
        "schedule",
        s.id,
        json!({"sectionId": section_id, "sessionDate": cmd.session_date}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, s.id).await
}

/// One session state command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionCommand {
    Start,
    Pause,
    Resume,
    Complete,
    Cancel,
    Abandon,
    Lock,
}

pub async fn session_command(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: SessionCommand,
    body: SessionProgress,
    actor: &Actor,
) -> Result<TeachingSession, KernelError> {
    use TeachingSessionStatus as S;
    let tenant = auth.scope.tenant_id;
    // Abandoning a started session is an exceptional outcome: administrators only (L4-D-023-BH-007).
    let manage = Permission::AttendanceManage;
    if cmd == SessionCommand::Abandon {
        auth.require(manage)?;
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let LockedSession { s, ctx: c } =
        lock_session(state, &mut tx, auth, id, Some(expected), manage).await?;
    let reason = kit::opt_text(body.reason.as_deref());
    let to = match cmd {
        SessionCommand::Start => S::Started,
        SessionCommand::Pause => S::Paused,
        SessionCommand::Resume => S::InProgress,
        SessionCommand::Complete => S::Completed,
        SessionCommand::Cancel => S::Cancelled,
        SessionCommand::Abandon => S::Abandoned,
        SessionCommand::Lock => S::Locked,
    };
    if !domain::can_transition(s.status, to) {
        return Err(illegal("teaching session", s.status, to));
    }
    if matches!(cmd, SessionCommand::Start | SessionCommand::Resume) {
        require_deliverable(c.offering.status, c.section.status)?;
    }
    if matches!(cmd, SessionCommand::Cancel | SessionCommand::Abandon) && reason.is_none() {
        return Err(invalid("reason", "required", "a reason is required"));
    }
    if cmd == SessionCommand::Lock
        && s.status == S::Completed
        && s.attendance_status != AttendanceCaptureStatus::Finalised
    {
        return Err(KernelError::Conflict(
            "finalise the attendance before locking the session".into(),
        ));
    }
    let now = Utc::now();
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": now});
    let m = changes
        .as_object_mut()
        .ok_or_else(|| KernelError::internal_msg("object"))?;
    match cmd {
        SessionCommand::Start => {
            m.insert("started_at".into(), json!(now));
        }
        SessionCommand::Pause => {
            m.insert("interruption_count".into(), json!(s.interruption_count + 1));
        }
        SessionCommand::Complete | SessionCommand::Abandon | SessionCommand::Cancel => {
            m.insert("ended_at".into(), json!(now));
        }
        _ => {}
    }
    if let Some(t) = kit::opt_text(body.topic.as_deref()) {
        m.insert("topic".into(), json!(t));
    }
    if let Some(n) = kit::opt_text(body.notes.as_deref()) {
        m.insert("notes".into(), json!(n));
    }
    let from = s.status;
    let ns: TeachingSession = kit::patch(&mut tx, TS, tenant, id, changes).await?;
    let ev = match cmd {
        SessionCommand::Start => Some((TEACHING_SESSION_STARTED, TEACHING_SESSION_STARTED_SCHEMA)),
        SessionCommand::Complete => Some((
            TEACHING_SESSION_COMPLETED,
            TEACHING_SESSION_COMPLETED_SCHEMA,
        )),
        SessionCommand::Cancel => Some((
            TEACHING_SESSION_CANCELLED,
            TEACHING_SESSION_CANCELLED_SCHEMA,
        )),
        _ => None,
    };
    if let Some(ev) = ev {
        emit_session(
            &mut tx,
            &c,
            &ns,
            from.as_str(),
            ev,
            reason.as_deref(),
            actor,
        )
        .await?;
    }
    let verb = format!("{cmd:?}").to_lowercase();
    kit::audit(
        &mut tx,
        &c.scope,
        TS,
        &verb,
        id,
        json!({"from": from, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ------------------------------------------------------------------ attendance capture

pub async fn capture(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: CaptureAttendance,
    actor: &Actor,
) -> Result<TeachingSession, KernelError> {
    let tenant = auth.scope.tenant_id;
    if cmd.records.is_empty() {
        return Err(invalid("records", "required", "record at least one mark"));
    }
    let ids: Vec<Uuid> = cmd.records.iter().map(|r| r.student_id).collect();
    let dups = domain::duplicates(&ids);
    if !dups.is_empty() {
        return Err(invalid(
            "records",
            "duplicate",
            format!("{} student(s) appear more than once", dups.len()),
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let LockedSession { s, ctx: c } = lock_session(
        state,
        &mut tx,
        auth,
        id,
        Some(expected),
        Permission::AttendanceManage,
    )
    .await?;
    if !domain::accepts_attendance(s.status) {
        return Err(KernelError::Conflict(format!(
            "attendance cannot be recorded for a {} session",
            s.status
        )));
    }
    if !domain::capture_open(s.attendance_status) {
        return Err(KernelError::Conflict(
            "attendance is finalised; use a governed correction".into(),
        ));
    }
    // eligibility = the seated roster at capture time (L4-D-024-BH-003)
    let seated = roster(state, tenant, s.section_id).await?;
    let existing = records_of(&mut *tx, tenant, id).await?;
    for r in &cmd.records {
        let Some(entry) = seated
            .iter()
            .find(|e| e.student_id.as_uuid() == r.student_id)
        else {
            return Err(invalid(
                "records",
                "not_seated",
                format!("student {} is not seated in this section", r.student_id),
            ));
        };
        match existing.iter().find(|e| e.student_id == r.student_id) {
            Some(e) => {
                kit::patch::<serde_json::Value>(&mut tx, "attendance_record", tenant, e.id, json!({
                    "mark": r.mark.as_str(), "remarks": kit::opt_text(r.remarks.as_deref()), "recorded_by": actor.id,
                })).await?;
            }
            None => {
                kit::append(&mut tx, "attendance_record", json!({
                    "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "session_id": id,
                    "section_id": s.section_id, "student_id": r.student_id, "student_number": entry.student_number,
                    "display_name": entry.display_name, "subject_ref": entry.subject_ref, "mark": r.mark.as_str(),
                    "remarks": kit::opt_text(r.remarks.as_deref()), "recorded_by": actor.id,
                })).await?;
            }
        }
    }
    let mut changes = json!({"attendance_status": "capturing"});
    if s.status == TeachingSessionStatus::Started {
        if let Some(m) = changes.as_object_mut() {
            m.insert("status".into(), json!("in_progress"));
            m.insert("status_changed_at".into(), json!(Utc::now()));
        }
    }
    kit::patch::<serde_json::Value>(&mut tx, TS, tenant, id, changes).await?;
    kit::audit(
        &mut tx,
        &c.scope,
        "attendance",
        "capture",
        id,
        json!({"marks": cmd.records.len()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

async fn require_complete(
    state: &AcademicState,
    conn: &mut PgConnection,
    tenant: TenantId,
    s: &TeachingSession,
) -> Result<(), KernelError> {
    let seated = roster(state, tenant, s.section_id).await?;
    let recs = records_of(&mut *conn, tenant, s.id).await?;
    let missing = seated
        .iter()
        .filter(|e| !recs.iter().any(|r| r.student_id == e.student_id.as_uuid()))
        .count();
    if missing > 0 {
        return Err(KernelError::Conflict(format!(
            "{missing} seated student(s) have no attendance mark yet"
        )));
    }
    Ok(())
}

pub async fn submit_attendance(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<TeachingSession, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let LockedSession { s, ctx: c } = lock_session(
        state,
        &mut tx,
        auth,
        id,
        Some(expected),
        Permission::AttendanceManage,
    )
    .await?;
    if s.attendance_status != AttendanceCaptureStatus::Capturing {
        return Err(illegal(
            "attendance register",
            s.attendance_status,
            AttendanceCaptureStatus::Submitted,
        ));
    }
    require_complete(state, &mut tx, tenant, &s).await?;
    kit::patch::<serde_json::Value>(
        &mut tx,
        TS,
        tenant,
        id,
        json!({"attendance_status": "submitted"}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &c.scope,
        "attendance",
        "submit",
        id,
        json!({}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn finalise_attendance(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<TeachingSession, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let LockedSession { s, ctx: c } = lock_session(
        state,
        &mut tx,
        auth,
        id,
        Some(expected),
        Permission::AttendanceManage,
    )
    .await?;
    if !domain::can_finalise(s.status, s.attendance_status) {
        return Err(KernelError::Conflict(format!(
            "attendance of a {} session in capture state {} cannot be finalised",
            s.status, s.attendance_status
        )));
    }
    require_complete(state, &mut tx, tenant, &s).await?;
    let from = s.attendance_status;
    let ns: TeachingSession = kit::patch(&mut tx, TS, tenant, id, json!({
        "attendance_status": "finalised", "attendance_finalised_at": Utc::now(), "attendance_finalised_by": actor.id,
    })).await?;
    StatusEvent {
        aggregate_id: id,
        institution_id: c.institution_uuid(),
        offering_id: Some(ns.offering_id),
        section_id: Some(ns.section_id),
        from: Some(from.as_str()),
        to: "finalised",
        version: ns.version,
        ..Default::default()
    }
    .emit(
        &mut tx,
        &c.scope,
        (ATTENDANCE_FINALISED, ATTENDANCE_FINALISED_SCHEMA),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &c.scope,
        "attendance",
        "finalise",
        id,
        json!({}),
        actor,
    )
    .await?;
    evaluate_section(&mut tx, &c, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

// ------------------------------------------------------------------ thresholds

async fn effective_policy<'e, E: sqlx::PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    institution: Uuid,
) -> Result<Option<AttendancePolicy>, KernelError> {
    let v: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(p) FROM academic.attendance_policy p WHERE p.tenant_id = $1 AND p.institution_id = $2 AND p.status = 'effective'",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .fetch_optional(ex)
    .await
    .map_err(map_db_error)?;
    v.map(kit::decode).transpose()
}

/// Finalised marks of a section by student: (student, number, name, subject, marks).
async fn finalised_marks<'e, E: sqlx::PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    section: Uuid,
) -> Result<Vec<(Uuid, String, String, Option<String>, Vec<String>)>, KernelError> {
    sqlx::query_as(
        "SELECT r.student_id, max(r.student_number), max(r.display_name), max(r.subject_ref), array_agg(r.mark) \
         FROM academic.attendance_record r JOIN academic.teaching_session s ON s.id = r.session_id \
         WHERE r.tenant_id = $1 AND s.section_id = $2 AND s.attendance_status = 'finalised' GROUP BY r.student_id",
    )
    .bind(tenant.as_uuid())
    .bind(section)
    .fetch_all(ex)
    .await
    .map_err(map_db_error)
}

fn marks(raw: &[String]) -> Vec<AttendanceMark> {
    raw.iter()
        .filter_map(|m| AttendanceMark::parse(m))
        .collect()
}

/// Re-evaluates every student of the section under the effective policy and raises
/// `attendance.threshold_breached` for each new breach (once per breach).
pub async fn evaluate_section(
    conn: &mut PgConnection,
    c: &SectionCtx,
    actor: &Actor,
) -> Result<usize, KernelError> {
    let tenant = c.scope.tenant_id;
    let Some(policy) = effective_policy(&mut *conn, tenant, c.institution_uuid()).await? else {
        return Ok(0);
    };
    let rules = policy.rules();
    let rows = finalised_marks(&mut *conn, tenant, c.section.id.as_uuid()).await?;
    let mut breaches = 0;
    for (student, _num, _name, subject, raw) in rows {
        let t: Tally = domain::tally(&marks(&raw), &rules);
        let prev: Option<bool> = sqlx::query_scalar(
            "SELECT below_threshold FROM academic.attendance_evaluation WHERE section_id = $1 AND student_id = $2",
        )
        .bind(c.section.id.as_uuid())
        .bind(student)
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?;
        let new_breach = domain::is_new_breach(prev, &t);
        let eval_id: Uuid = sqlx::query_scalar(
            "INSERT INTO academic.attendance_evaluation (tenant_id, institution_id, policy_id, section_id, student_id, \
             attended, total, percent, below_threshold, breached_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, \
             CASE WHEN $10 THEN now() END) ON CONFLICT (section_id, student_id) DO UPDATE SET policy_id = EXCLUDED.policy_id, \
             attended = EXCLUDED.attended, total = EXCLUDED.total, percent = EXCLUDED.percent, \
             below_threshold = EXCLUDED.below_threshold, evaluated_at = now(), \
             breached_at = CASE WHEN $10 THEN now() WHEN EXCLUDED.below_threshold THEN academic.attendance_evaluation.breached_at END \
             RETURNING id",
        )
        .bind(tenant.as_uuid())
        .bind(c.institution_uuid())
        .bind(policy.id)
        .bind(c.section.id.as_uuid())
        .bind(student)
        .bind(t.attended)
        .bind(t.total)
        .bind(t.percent)
        .bind(t.below_threshold)
        .bind(new_breach)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
        if new_breach {
            breaches += 1;
            publish::event(
                &mut *conn,
                &c.scope,
                ATTENDANCE_THRESHOLD_BREACHED,
                ATTENDANCE_THRESHOLD_BREACHED_SCHEMA,
                eval_id.to_string(),
                &AttendanceThresholdV1 {
                    section_id: c.section.id.as_uuid(),
                    offering_id: c.offering.id.as_uuid(),
                    institution_id: c.institution_uuid(),
                    student_id: student,
                    academic_term_id: c.section.academic_term_id,
                    attendance_percent: t.percent,
                    threshold_percent: rules.threshold_percent,
                    sessions_counted: t.total,
                    student_subject_ref: subject.clone(),
                },
                actor,
            )
            .await?;
            kit::audit(
                &mut *conn,
                &c.scope,
                "attendance",
                "threshold_breached",
                eval_id,
                json!({"studentId": student, "percent": t.percent}),
                actor,
            )
            .await?;
        }
    }
    Ok(breaches)
}

pub async fn summary(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
) -> Result<AttendanceSummary, KernelError> {
    let tenant = auth.scope.tenant_id;
    let c = kit::section_ctx(state, auth, section).await?;
    let policy = effective_policy(&state.pool, tenant, c.institution_uuid()).await?;
    let rules = policy
        .as_ref()
        .map(AttendancePolicy::rules)
        .unwrap_or_default();
    let rows = finalised_marks(&state.pool, tenant, section).await?;
    let seated = roster(state, tenant, section).await?;
    let mut items: Vec<AttendanceSummaryLine> = Vec::new();
    for (student, num, name, _sub, raw) in &rows {
        let t = domain::tally(&marks(raw), &rules);
        items.push(AttendanceSummaryLine {
            student_id: *student,
            student_number: num.clone(),
            display_name: name.clone(),
            attended: t.attended,
            total: t.total,
            percent: t.percent,
            below_threshold: t.below_threshold,
        });
    }
    for e in seated {
        if !items.iter().any(|i| i.student_id == e.student_id.as_uuid()) {
            let t = domain::tally(&[], &rules);
            items.push(AttendanceSummaryLine {
                student_id: e.student_id.as_uuid(),
                student_number: e.student_number,
                display_name: e.display_name,
                attended: t.attended,
                total: t.total,
                percent: t.percent,
                below_threshold: false,
            });
        }
    }
    items.sort_by(|a, b| a.student_number.cmp(&b.student_number));
    let finalised_sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.teaching_session WHERE tenant_id = $1 AND section_id = $2 AND attendance_status = 'finalised'",
    )
    .bind(tenant.as_uuid())
    .bind(section)
    .fetch_one(&state.pool)
    .await
    .map_err(map_db_error)?;
    Ok(AttendanceSummary {
        section_id: section,
        threshold_percent: rules.threshold_percent,
        policy_id: policy.map(|p| p.id),
        finalised_sessions,
        items,
    })
}

pub async fn evaluate(
    state: &AcademicState,
    auth: &AuthContext,
    section: Uuid,
    actor: &Actor,
) -> Result<AttendanceSummary, KernelError> {
    auth.require(Permission::AttendanceManage)?;
    let c = kit::section_ctx(state, auth, section).await?;
    require_institution_writable(state, &c.scope, c.section.institution_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    evaluate_section(&mut tx, &c, actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    summary(state, auth, section).await
}

pub async fn my_attendance(
    state: &AcademicState,
    auth: &AuthContext,
) -> Result<MyAttendance, KernelError> {
    let (me, sections) = crate::shared::me::my_sections(state, auth).await?;
    let tenant = auth.scope.tenant_id;
    let mut items = Vec::new();
    for s in sections {
        let policy = effective_policy(&state.pool, tenant, s.institution_id.as_uuid()).await?;
        let rules = policy
            .as_ref()
            .map(AttendancePolicy::rules)
            .unwrap_or_default();
        let raw: Vec<String> = sqlx::query_scalar(
            "SELECT r.mark FROM academic.attendance_record r JOIN academic.teaching_session t ON t.id = r.session_id \
             WHERE r.tenant_id = $1 AND t.section_id = $2 AND r.student_id = $3 AND t.attendance_status = 'finalised'",
        )
        .bind(tenant.as_uuid())
        .bind(s.id.as_uuid())
        .bind(me.id.as_uuid())
        .fetch_all(&state.pool)
        .await
        .map_err(map_db_error)?;
        let t = domain::tally(&marks(&raw), &rules);
        items.push(MyAttendanceLine {
            section_id: s.id.as_uuid(),
            offering_code: s.offering_code,
            title: s.offering_title,
            section_code: s.section_code,
            attended: t.attended,
            total: t.total,
            percent: t.percent,
            below_threshold: t.below_threshold,
            threshold_percent: rules.threshold_percent,
        });
    }
    Ok(MyAttendance { items })
}

pub async fn faculty_attendance(
    state: &AcademicState,
    auth: &AuthContext,
) -> Result<FacultyAttendance, KernelError> {
    let sections = crate::shared::faculty::my_sections(state, auth).await?;
    let tenant = auth.scope.tenant_id;
    let mut items = Vec::new();
    for s in sections {
        let seated = roster(state, tenant, s.id.as_uuid()).await?.len();
        let sessions = list_for_section(state, tenant, s.id.as_uuid(), None, 50).await?;
        items.push(TaughtSectionSessions {
            section_id: s.id.as_uuid(),
            offering_id: s.offering_id.as_uuid(),
            offering_code: s.offering_code,
            title: s.offering_title,
            section_code: s.section_code,
            seated_count: seated,
            sessions,
        });
    }
    Ok(FacultyAttendance { items })
}

// ------------------------------------------------------------------ corrections

const AC: &str = "attendance_correction";
const AC_ENTITY: &str = "attendance correction";

pub async fn list_corrections(
    state: &AcademicState,
    tenant: TenantId,
    session: Uuid,
) -> Result<Vec<AttendanceCorrection>, KernelError> {
    kit::list_by(
        &state.pool,
        AC,
        tenant,
        "session_id",
        session,
        None,
        "t.created_at DESC",
        500,
    )
    .await
}

/// Corrections of an institution, newest first (`status` filters, e.g. `requested` for the
/// decision queue).
pub async fn list_institution_corrections(
    state: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<AttendanceCorrectionQueueItem>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) || jsonb_build_object( \
             'student_number', r.student_number, 'display_name', r.display_name, \
             'section_id', s.section_id, 'offering_code', o.offering_code, \
             'section_code', sec.section_code, 'session_date', s.session_date, \
             'starts_at', to_char(s.starts_at, 'HH24:MI')) \
         FROM academic.attendance_correction t \
         JOIN academic.attendance_record r ON r.id = t.record_id \
         JOIN academic.teaching_session s ON s.id = t.session_id \
         JOIN academic.offering_section sec ON sec.id = s.section_id \
         JOIN academic.course_offering o ON o.id = s.offering_id \
         WHERE t.tenant_id = $1 AND t.institution_id = $2 AND ($3::text IS NULL OR t.status = $3) \
         ORDER BY t.created_at DESC LIMIT 500",
    )
    .bind(tenant.as_uuid())
    .bind(institution)
    .bind(status)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

pub async fn get_correction(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<AttendanceCorrection, KernelError> {
    kit::fetch(&state.pool, AC, AC_ENTITY, tenant, id).await
}

async fn apply_correction(
    conn: &mut PgConnection,
    c: &SectionCtx,
    corr: &AttendanceCorrection,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<AttendanceCorrection, KernelError> {
    let tenant = c.scope.tenant_id;
    if !approve {
        let out = kit::patch(conn, AC, tenant, corr.id, json!({"status": "rejected", "status_reason": reason, "decided_by": actor.id, "decided_at": Utc::now()})).await?;
        kit::audit(
            conn,
            &c.scope,
            AC,
            "reject",
            corr.id,
            json!({"reason": reason, "requestedBy": corr.requested_by}),
            actor,
        )
        .await?;
        return Ok(out);
    }
    let rec: RecordRow = kit::lock(
        conn,
        "attendance_record",
        "attendance record",
        tenant,
        corr.record_id,
    )
    .await?;
    if rec.mark != corr.from_mark {
        return Err(KernelError::Conflict(
            "the attendance record changed since the correction was requested".into(),
        ));
    }
    // History first: the `attendance_record_finalised_guard` trigger (DEF-D-009) only lets a
    // finalised mark change when this transaction has already recorded the matching audit row of
    // an open correction.
    kit::append(conn, "attendance_audit", json!({
        "tenant_id": tenant.as_uuid(), "record_id": rec.id, "correction_id": corr.id,
        "from_mark": corr.from_mark.as_str(), "to_mark": corr.to_mark.as_str(), "actor": actor.id,
    })).await?;
    let rec2: RecordRow = kit::patch(
        conn,
        "attendance_record",
        tenant,
        rec.id,
        json!({"mark": corr.to_mark.as_str()}),
    )
    .await?;
    let out: AttendanceCorrection = kit::patch(conn, AC, tenant, corr.id, json!({"status": "applied", "status_reason": reason, "decided_by": actor.id, "decided_at": Utc::now()})).await?;
    kit::modified_event(
        conn,
        &c.scope,
        (ATTENDANCE_CORRECTED, ATTENDANCE_CORRECTED_SCHEMA),
        corr.session_id,
        Some(c.offering.id.as_uuid()),
        Some(c.section.id.as_uuid()),
        rec2.version,
        vec!["mark".into()],
        actor,
    )
    .await?;
    kit::audit(conn, &c.scope, AC, "apply", corr.id, json!({"recordId": rec.id, "studentId": rec.student_id, "from": corr.from_mark, "to": corr.to_mark, "subject": rec.subject_ref, "requestedBy": corr.requested_by}), actor).await?;
    evaluate_section(conn, c, actor).await?;
    Ok(out)
}

pub async fn request_correction(
    state: &AcademicState,
    auth: &AuthContext,
    session_id: Uuid,
    cmd: NewAttendanceCorrection,
    actor: &Actor,
) -> Result<AttendanceCorrection, KernelError> {
    let tenant = auth.scope.tenant_id;
    let reason = required_text("reason", Some(&cmd.reason))?;
    let s: TeachingSession = kit::fetch(&state.pool, TS, TS_ENTITY, tenant, session_id).await?;
    require_teaching(
        state,
        auth,
        OfferingSectionId(s.section_id),
        Permission::AttendanceCorrect,
    )
    .await?;
    let c = kit::section_ctx(state, auth, s.section_id).await?;
    require_institution_writable(state, &c.scope, c.section.institution_id).await?;
    if s.attendance_status != AttendanceCaptureStatus::Finalised {
        return Err(KernelError::Conflict(
            "only finalised attendance is corrected; edit the open register instead".into(),
        ));
    }
    let rec: RecordRow = kit::fetch(
        &state.pool,
        "attendance_record",
        "attendance record",
        tenant,
        cmd.record_id,
    )
    .await?;
    let rec_session: Uuid =
        sqlx::query_scalar("SELECT session_id FROM academic.attendance_record WHERE id = $1")
            .bind(rec.id)
            .fetch_one(&state.pool)
            .await
            .map_err(map_db_error)?;
    if rec_session != session_id {
        return Err(invalid(
            "recordId",
            "session",
            "the record belongs to another session",
        ));
    }
    if rec.mark == cmd.to_mark {
        return Err(invalid(
            "toMark",
            "unchanged",
            "the record already has this mark",
        ));
    }
    let mut vars = BTreeMap::new();
    vars.insert("recordId".to_owned(), json!(rec.id));
    let outcome = approval::request_approval(
        state,
        &c.scope,
        process::ATTENDANCE_CORRECTION,
        business_key("attendance_record", rec.id, rec.version),
        vars,
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let corr: AttendanceCorrection = kit::insert(&mut tx, AC, json!({
        "tenant_id": tenant.as_uuid(), "institution_id": c.institution_uuid(), "session_id": session_id,
        "record_id": rec.id, "student_id": rec.student_id, "from_mark": rec.mark.as_str(), "to_mark": cmd.to_mark.as_str(),
        "reason": reason, "evidence": kit::opt_text(cmd.evidence.as_deref()),
        "process_instance_id": outcome.process_instance_id(), "requested_by": actor.id,
    }))
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => KernelError::Conflict("a correction of this record is already pending".into()),
        other => other,
    })?;
    kit::audit(
        &mut tx,
        &c.scope,
        AC,
        "request",
        corr.id,
        json!({
            "recordId": rec.id, "from": rec.mark, "to": cmd.to_mark, "reason": reason,
            "workflow": if matches!(outcome, ApprovalOutcome::Approved { .. }) { "cleared" } else { "pending" },
        }),
        actor,
    )
    .await?;
    // DEF-D-002: a correction of a finalised register is never applied by its requester or by
    // the automatic approver. Whatever the approval mode reports (the workflow step is started
    // above in both modes), the correction stays `requested` until an authorised approver other
    // than the requester decides it through `decide_correction` (attendance.correct + step-up).
    tx.commit().await.map_err(map_db_error)?;
    Ok(corr)
}

pub async fn decide_correction(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AttendanceCorrection, KernelError> {
    // The approver needs attendance.correct; `require` also enforces the MFA step-up of that
    // permission when the platform enforces step-up (DEF-D-002).
    auth.require(Permission::AttendanceCorrect)?;
    let tenant = auth.scope.tenant_id;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let cur = get_correction(state, tenant, id).await?;
    if is_requester(&cur, auth, actor) {
        return Err(KernelError::Forbidden(
            "the requester cannot decide their own attendance correction".into(),
        ));
    }
    // Part 1 · S3-appr (DEF-X-002): also no system actor / ledger requester of the record.
    sutra_persistence::approval::guard_decision_by(
        &state.pool,
        tenant.as_uuid(),
        Some(&auth.subject),
        &cur.record_id,
        &[cur.requested_by.as_deref()],
        "attendance correction",
    )
    .await?;
    let s: TeachingSession = kit::fetch(&state.pool, TS, TS_ENTITY, tenant, cur.session_id).await?;
    let c = kit::section_ctx(state, auth, s.section_id).await?;
    require_institution_writable(state, &c.scope, c.section.institution_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let corr: AttendanceCorrection = kit::lock(&mut tx, AC, AC_ENTITY, tenant, id).await?;
    check_version(corr.version, expected)?;
    if corr.status != ChangeRequestStatus::Requested {
        return Err(KernelError::Conflict(format!(
            "the correction is {}; it has no pending decision",
            corr.status
        )));
    }
    let out = apply_correction(&mut tx, &c, &corr, approve, reason.as_deref(), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Maker-checker: the person who requested a correction never decides it.
fn is_requester(corr: &AttendanceCorrection, auth: &AuthContext, actor: &Actor) -> bool {
    match corr.requested_by.as_deref() {
        Some(r) => r == auth.subject || actor.id.as_deref() == Some(r),
        None => false,
    }
}

pub async fn withdraw_correction(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AttendanceCorrection, KernelError> {
    let tenant = auth.scope.tenant_id;
    let cur = get_correction(state, tenant, id).await?;
    let s: TeachingSession = kit::fetch(&state.pool, TS, TS_ENTITY, tenant, cur.session_id).await?;
    require_teaching(
        state,
        auth,
        OfferingSectionId(s.section_id),
        Permission::AttendanceCorrect,
    )
    .await?;
    let c = kit::section_ctx(state, auth, s.section_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let corr: AttendanceCorrection = kit::lock(&mut tx, AC, AC_ENTITY, tenant, id).await?;
    check_version(corr.version, expected)?;
    if corr.status != ChangeRequestStatus::Requested {
        return Err(illegal(
            "attendance correction",
            corr.status,
            ChangeRequestStatus::Withdrawn,
        ));
    }
    let out = kit::patch(
        &mut tx,
        AC,
        tenant,
        id,
        json!({"status": "withdrawn", "status_reason": reason}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &c.scope,
        AC,
        "withdraw",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// ------------------------------------------------------------------ policies

const AP: &str = "attendance_policy";
const AP_ENTITY: &str = "attendance policy";

pub async fn list_policies(
    state: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<&str>,
) -> Result<Vec<AttendancePolicy>, KernelError> {
    kit::list_by(
        &state.pool,
        AP,
        tenant,
        "institution_id",
        institution,
        status,
        "t.created_at DESC",
        200,
    )
    .await
}

pub async fn get_policy(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<AttendancePolicy, KernelError> {
    kit::fetch(&state.pool, AP, AP_ENTITY, tenant, id).await
}

pub async fn create_policy(
    state: &AcademicState,
    auth: &AuthContext,
    institution: Uuid,
    cmd: NewAttendancePolicy,
    actor: &Actor,
) -> Result<AttendancePolicy, KernelError> {
    let scope = institution_scope(auth, InstitutionId(institution));
    require_institution_writable(state, &scope, InstitutionId(institution)).await?;
    let name = required_text("name", Some(&cmd.name))?;
    if !(cmd.threshold_percent > 0.0 && cmd.threshold_percent <= 100.0) {
        return Err(invalid(
            "thresholdPercent",
            "range",
            "the threshold is a percentage above 0 and at most 100",
        ));
    }
    let min_sessions = cmd.min_sessions.unwrap_or(1);
    if min_sessions < 1 {
        return Err(invalid("minSessions", "range", "at least one session"));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p: AttendancePolicy = kit::insert(&mut tx, AP, json!({
        "tenant_id": auth.scope.tenant_id.as_uuid(), "institution_id": institution, "name": name,
        "threshold_percent": cmd.threshold_percent, "min_sessions": min_sessions,
        "late_counts_as_present": cmd.late_counts_as_present.unwrap_or(true),
        "exclude_excused": cmd.exclude_excused.unwrap_or(true), "effective_from": cmd.effective_from,
    })).await?;
    kit::audit(
        &mut tx,
        &scope,
        AP,
        "create",
        p.id,
        json!({"thresholdPercent": cmd.threshold_percent}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

async fn apply_policy_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    p: AttendancePolicy,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<AttendancePolicy, KernelError> {
    let tenant = scope.tenant_id;
    if !approve {
        let out = kit::patch(
            conn,
            AP,
            tenant,
            p.id,
            json!({"status": "draft", "status_reason": reason}),
        )
        .await?;
        kit::audit(
            conn,
            scope,
            AP,
            "reject",
            p.id,
            json!({"reason": reason}),
            actor,
        )
        .await?;
        return Ok(out);
    }
    let prior: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM academic.attendance_policy WHERE tenant_id = $1 AND institution_id = $2 AND status = 'effective' FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(p.institution_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if let Some(prior) = prior {
        kit::patch::<AttendancePolicy>(
            conn,
            AP,
            tenant,
            prior,
            json!({"status": "superseded", "status_reason": "superseded by a newer version"}),
        )
        .await?;
    }
    let out = kit::patch(
        conn,
        AP,
        tenant,
        p.id,
        json!({"status": "effective", "status_reason": reason, "supersedes_id": prior}),
    )
    .await?;
    kit::audit(
        conn,
        scope,
        AP,
        "approve",
        p.id,
        json!({"supersedes": prior}),
        actor,
    )
    .await?;
    Ok(out)
}

pub async fn submit_policy(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<AttendancePolicy, KernelError> {
    let tenant = auth.scope.tenant_id;
    let cur = get_policy(state, tenant, id).await?;
    check_version(cur.version, expected)?;
    if !domain::policy_can_transition(cur.status, PolicyVersionStatus::InReview) {
        return Err(illegal(
            "attendance policy",
            cur.status,
            PolicyVersionStatus::InReview,
        ));
    }
    let scope = institution_scope(auth, InstitutionId(cur.institution_id));
    require_institution_writable(state, &scope, InstitutionId(cur.institution_id)).await?;
    let outcome = approval::request_approval(
        state,
        &scope,
        process::ATTENDANCE_POLICY,
        business_key(AP, id, cur.version),
        BTreeMap::new(),
    )
    .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p: AttendancePolicy = kit::lock(&mut tx, AP, AP_ENTITY, tenant, id).await?;
    check_version(p.version, expected)?;
    let p: AttendancePolicy = kit::patch(
        &mut tx,
        AP,
        tenant,
        id,
        json!({"status": "in_review", "process_instance_id": outcome.process_instance_id()}),
    )
    .await?;
    kit::audit(&mut tx, &scope, AP, "submit", id, json!({}), actor).await?;
    let p = if let ApprovalOutcome::Approved { .. } = outcome {
        apply_policy_decision(
            &mut tx,
            &scope,
            p,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        p
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(p)
}

pub async fn decide_policy(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AttendancePolicy, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p: AttendancePolicy = kit::lock(&mut tx, AP, AP_ENTITY, tenant, id).await?;
    check_version(p.version, expected)?;
    if p.status != PolicyVersionStatus::InReview {
        return Err(KernelError::Conflict(format!(
            "the policy is {}; it has no pending decision",
            p.status
        )));
    }
    let scope = institution_scope(auth, InstitutionId(p.institution_id));
    let out = apply_policy_decision(&mut tx, &scope, p, approve, reason.as_deref(), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

pub async fn retire_policy(
    state: &AcademicState,
    auth: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<AttendancePolicy, KernelError> {
    let tenant = auth.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let p: AttendancePolicy = kit::lock(&mut tx, AP, AP_ENTITY, tenant, id).await?;
    check_version(p.version, expected)?;
    if !domain::policy_can_transition(p.status, PolicyVersionStatus::Retired) {
        return Err(illegal(
            "attendance policy",
            p.status,
            PolicyVersionStatus::Retired,
        ));
    }
    let scope = institution_scope(auth, InstitutionId(p.institution_id));
    let out = kit::patch(
        &mut tx,
        AP,
        tenant,
        id,
        json!({"status": "retired", "status_reason": reason}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        AP,
        "retire",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}
