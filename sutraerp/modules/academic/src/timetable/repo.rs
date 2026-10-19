//! Timetable persistence and commands (L3-D-020…022).

use super::domain::{self, Conflict, SectionFacts, SlotFacts};
use super::kit::{self, check_version, illegal, invalid, required_text, StatusEvent};
use crate::shared::approval::{self, business_key, process, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::scope::institution_scope;
use crate::shared::{AcademicState, Actor};
use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::{BTreeMap, HashMap};
use sutra_contracts::academic::events::delivery::*;
use sutra_contracts::academic::ids::OfferingSectionId;
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, SectionStatus, TimetableStatus,
};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

// ------------------------------------------------------------------ DTOs

/// Scheduling requirement of one section (input to generation).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SchedulingRequirement {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Uuid,
    pub offering_id: Uuid,
    pub section_id: Uuid,
    pub sessions_per_week: i16,
    pub session_minutes: i16,
    pub preferred_days: Vec<i16>,
    pub room_label: Option<String>,
    pub teaching_space_id: Option<Uuid>,
    pub notes: Option<String>,
    /// `active` | `withdrawn`.
    pub status: String,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct SchedulingRequirementList {
    pub items: Vec<SchedulingRequirement>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewSchedulingRequirement {
    pub section_id: Uuid,
    pub sessions_per_week: i16,
    pub session_minutes: i16,
    #[serde(default)]
    pub preferred_days: Vec<i16>,
    pub room_label: Option<String>,
    pub teaching_space_id: Option<Uuid>,
    pub notes: Option<String>,
}

/// Outcome of the last validation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ValidationReport {
    pub valid: bool,
    pub conflicts: Vec<Conflict>,
    pub validated_at: DateTime<Utc>,
}

/// One weekly slot (contract §5.3).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimetableSlot {
    pub id: Uuid,
    pub timetable_id: Uuid,
    pub section_id: Uuid,
    pub offering_id: Uuid,
    #[serde(default)]
    pub offering_code: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub section_code: String,
    /// 1 = Monday … 7 = Sunday.
    pub day_of_week: i16,
    /// `HH:MM`.
    pub starts_at: String,
    /// `HH:MM`.
    pub ends_at: String,
    pub room_label: Option<String>,
    pub teaching_space_id: Option<Uuid>,
    #[serde(default)]
    pub faculty_names: Vec<String>,
}

/// A timetable version with its slots.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Timetable {
    pub id: Uuid,
    pub institution_id: Uuid,
    pub academic_term_id: Uuid,
    pub name: String,
    pub version_no: i32,
    pub revision_of_id: Option<Uuid>,
    pub status: TimetableStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub validation_report: Option<ValidationReport>,
    pub pending_decision: bool,
    pub process_instance_id: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub published_at: Option<DateTime<Utc>>,
    pub activated_at: Option<DateTime<Utc>>,
    pub retired_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub slots: Vec<TimetableSlot>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct TimetableList {
    pub items: Vec<Timetable>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewTimetable {
    pub academic_term_id: Uuid,
    pub name: String,
    pub effective_from: Option<NaiveDate>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewSlot {
    pub section_id: Uuid,
    pub day_of_week: i16,
    /// `HH:MM`.
    pub starts_at: String,
    /// `HH:MM`.
    pub ends_at: String,
    pub room_label: Option<String>,
    pub teaching_space_id: Option<Uuid>,
}

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewRevision {
    pub reason: String,
    #[serde(default)]
    pub affected_section_ids: Vec<Uuid>,
}

/// Student / faculty weekly view (contract §5.3).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WeeklyTimetable {
    pub items: Vec<TimetableSlot>,
    /// Monday of the current business week.
    pub week_of: NaiveDate,
}

#[allow(dead_code)]
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevisionRow {
    id: Uuid,
    timetable_id: Uuid,
    status: String,
    version: i64,
}

const TT: &str = "timetable";
const TT_ENTITY: &str = "timetable";

pub fn parse_time(field: &str, s: &str) -> Result<NaiveTime, KernelError> {
    NaiveTime::parse_from_str(s.trim(), "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s.trim(), "%H:%M:%S"))
        .map_err(|_| invalid(field, "format", format!("{field} must be HH:MM")))
}

fn hhmm(t: NaiveTime) -> String {
    t.format("%H:%M").to_string()
}

// ------------------------------------------------------------------ scheduling requirements

pub async fn list_requirements(
    state: &AcademicState,
    tenant: TenantId,
    institution_id: Uuid,
    term: Option<Uuid>,
    status: Option<&str>,
) -> Result<Vec<SchedulingRequirement>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.scheduling_requirement t WHERE t.tenant_id = $1 \
         AND t.institution_id = $2 AND ($3::uuid IS NULL OR t.academic_term_id = $3) \
         AND ($4::text IS NULL OR t.status = $4) ORDER BY t.created_at LIMIT 500",
    )
    .bind(tenant.as_uuid())
    .bind(institution_id)
    .bind(term)
    .bind(status)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

pub async fn create_requirement(
    state: &AcademicState,
    ctx: &AuthContext,
    institution_id: Uuid,
    cmd: NewSchedulingRequirement,
    actor: &Actor,
) -> Result<SchedulingRequirement, KernelError> {
    let s = kit::section_ctx(state, ctx, cmd.section_id).await?;
    if s.institution_uuid() != institution_id {
        return Err(invalid(
            "sectionId",
            "institution",
            "the section belongs to another institution",
        ));
    }
    require_institution_writable(state, &s.scope, s.section.institution_id).await?;
    if s.offering.status.is_terminal()
        || matches!(
            s.offering.status,
            CourseOfferingStatus::Cancelled | CourseOfferingStatus::Completed
        )
        || !matches!(
            s.section.status,
            SectionStatus::Planned | SectionStatus::Active
        )
    {
        return Err(KernelError::Conflict(
            "the section is no longer offered and cannot be scheduled".into(),
        ));
    }
    if !(1..=20).contains(&cmd.sessions_per_week) {
        return Err(invalid(
            "sessionsPerWeek",
            "range",
            "1 to 20 sessions a week",
        ));
    }
    if !(15..=480).contains(&cmd.session_minutes) {
        return Err(invalid("sessionMinutes", "range", "15 to 480 minutes"));
    }
    if cmd.preferred_days.iter().any(|d| !(1..=7).contains(d)) {
        return Err(invalid(
            "preferredDays",
            "range",
            "days are 1 (Monday) to 7",
        ));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let r: SchedulingRequirement = kit::insert(
        &mut tx,
        "scheduling_requirement",
        json!({
            "tenant_id": ctx.scope.tenant_id.as_uuid(),
            "institution_id": institution_id,
            "academic_term_id": s.section.academic_term_id,
            "offering_id": s.offering.id.as_uuid(),
            "section_id": cmd.section_id,
            "sessions_per_week": cmd.sessions_per_week,
            "session_minutes": cmd.session_minutes,
            "preferred_days": cmd.preferred_days,
            "room_label": kit::opt_text(cmd.room_label.as_deref()),
            "teaching_space_id": cmd.teaching_space_id,
            "notes": kit::opt_text(cmd.notes.as_deref()),
        }),
    )
    .await
    .map_err(|e| match e {
        KernelError::Conflict(_) => {
            KernelError::Conflict("the section already has an active scheduling requirement".into())
        }
        other => other,
    })?;
    kit::audit(
        &mut tx,
        &s.scope,
        "scheduling_requirement",
        "create",
        r.id,
        json!({"sectionId": r.section_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

pub async fn withdraw_requirement(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<SchedulingRequirement, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let r: SchedulingRequirement = kit::lock(
        &mut tx,
        "scheduling_requirement",
        "scheduling requirement",
        tenant,
        id,
    )
    .await?;
    check_version(r.version, expected)?;
    if r.status != "active" {
        return Err(illegal("scheduling requirement", &r.status, "withdrawn"));
    }
    let scope = institution_scope(ctx, InstitutionId(r.institution_id));
    let r: SchedulingRequirement = kit::patch(
        &mut tx,
        "scheduling_requirement",
        tenant,
        id,
        json!({"status": "withdrawn"}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        "scheduling_requirement",
        "withdraw",
        id,
        json!({}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

// ------------------------------------------------------------------ views

/// Decorates slot rows with offering/section codes and faculty names (offering read port).
pub async fn decorate(
    state: &AcademicState,
    tenant: TenantId,
    mut slots: Vec<TimetableSlot>,
) -> Result<Vec<TimetableSlot>, KernelError> {
    let scope = ExecutionScope::tenant(tenant);
    let mut cache: HashMap<Uuid, (String, String, String, Vec<String>)> = HashMap::new();
    for s in &mut slots {
        if let std::collections::hash_map::Entry::Vacant(slot) = cache.entry(s.section_id) {
            let sec = state
                .offerings
                .section(&scope, OfferingSectionId(s.section_id))
                .await?;
            let fac = state
                .offerings
                .faculty_of_section(&scope, OfferingSectionId(s.section_id))
                .await?
                .into_iter()
                .filter(|f| f.status == FacultyAssignmentStatus::Active)
                .map(|f| f.faculty_name.unwrap_or(f.faculty_subject_ref))
                .collect();
            let (code, title, sc) = sec
                .map(|x| (x.offering_code, x.offering_title, x.section_code))
                .unwrap_or_default();
            slot.insert((code, title, sc, fac));
        }
        if let Some((code, title, sc, fac)) = cache.get(&s.section_id) {
            s.offering_code.clone_from(code);
            s.title.clone_from(title);
            s.section_code.clone_from(sc);
            s.faculty_names.clone_from(fac);
        }
    }
    Ok(slots)
}

async fn slot_rows<'e, E: sqlx::PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    timetable_id: Uuid,
) -> Result<Vec<TimetableSlot>, KernelError> {
    kit::list_by(
        ex,
        "timetable_slot",
        tenant,
        "timetable_id",
        timetable_id,
        None,
        "t.day_of_week, t.starts_at, t.id",
        2000,
    )
    .await
}

pub async fn get(
    state: &AcademicState,
    tenant: TenantId,
    id: Uuid,
) -> Result<Timetable, KernelError> {
    let mut t: Timetable = kit::fetch(&state.pool, TT, TT_ENTITY, tenant, id).await?;
    let rows = slot_rows(&state.pool, tenant, id).await?;
    t.slots = decorate(state, tenant, rows).await?;
    Ok(t)
}

pub async fn list(
    state: &AcademicState,
    tenant: TenantId,
    institution_id: Uuid,
    term: Option<Uuid>,
    status: Option<&str>,
) -> Result<Vec<Timetable>, KernelError> {
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(t) FROM academic.timetable t WHERE t.tenant_id = $1 AND t.institution_id = $2 \
         AND ($3::uuid IS NULL OR t.academic_term_id = $3) AND ($4::text IS NULL OR t.status = $4) \
         ORDER BY t.created_at DESC LIMIT 200",
    )
    .bind(tenant.as_uuid())
    .bind(institution_id)
    .bind(term)
    .bind(status)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    rows.into_iter().map(kit::decode).collect()
}

/// Slots of the operational (active/frozen) timetables that touch `sections`.
pub async fn operational_slots(
    state: &AcademicState,
    tenant: TenantId,
    sections: &[Uuid],
) -> Result<Vec<TimetableSlot>, KernelError> {
    if sections.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(s) FROM academic.timetable_slot s JOIN academic.timetable t ON t.id = s.timetable_id \
         WHERE s.tenant_id = $1 AND t.status IN ('active', 'frozen') AND s.section_id = ANY($2) \
         ORDER BY s.day_of_week, s.starts_at",
    )
    .bind(tenant.as_uuid())
    .bind(sections)
    .fetch_all(&state.pool)
    .await
    .map_err(map_db_error)?;
    let slots = rows
        .into_iter()
        .map(kit::decode)
        .collect::<Result<Vec<_>, _>>()?;
    decorate(state, tenant, slots).await
}

/// Monday of a date's week.
pub fn monday_of(d: NaiveDate) -> NaiveDate {
    d - chrono::Duration::days(i64::from(d.weekday().num_days_from_monday()))
}

// ------------------------------------------------------------------ commands

pub async fn create(
    state: &AcademicState,
    ctx: &AuthContext,
    institution_id: Uuid,
    cmd: NewTimetable,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let scope = institution_scope(ctx, InstitutionId(institution_id));
    require_institution_writable(state, &scope, InstitutionId(institution_id)).await?;
    let name = required_text("name", Some(&cmd.name))?;
    let term = state
        .references
        .academic_term(&scope, cmd.academic_term_id)
        .await?
        .filter(|t| t.institution_id.as_uuid() == institution_id)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "not_found",
                "no such academic term in this institution",
            )
        })?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let t: Timetable = kit::insert(
        &mut tx,
        TT,
        json!({
            "tenant_id": ctx.scope.tenant_id.as_uuid(),
            "institution_id": institution_id,
            "academic_term_id": term.id,
            "name": name,
            "effective_from": cmd.effective_from.unwrap_or(term.starts_on),
        }),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "create",
        t.id,
        json!({"academicTermId": term.id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(t)
}

struct Locked {
    t: Timetable,
    scope: ExecutionScope,
}

async fn lock_tt(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    id: Uuid,
    expected: Option<Version>,
) -> Result<Locked, KernelError> {
    let t: Timetable = kit::lock(conn, TT, TT_ENTITY, ctx.scope.tenant_id, id).await?;
    if let Some(v) = expected {
        check_version(t.version, v)?;
    }
    let scope = institution_scope(ctx, InstitutionId(t.institution_id));
    Ok(Locked { t, scope })
}

fn require(from: TimetableStatus, to: TimetableStatus) -> Result<(), KernelError> {
    if domain::can_transition(from, to) {
        Ok(())
    } else {
        Err(illegal("timetable", from, to))
    }
}

async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
    to: TimetableStatus,
    reason: Option<&str>,
    extra: serde_json::Value,
) -> Result<Timetable, KernelError> {
    let mut changes =
        json!({"status": to.as_str(), "status_reason": reason, "status_changed_at": Utc::now()});
    if let (Some(m), Some(e)) = (changes.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            m.insert(k.clone(), v.clone());
        }
    }
    kit::patch(conn, TT, tenant, id, changes).await
}

async fn emit(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: &Timetable,
    from: TimetableStatus,
    ev: (&'static str, &'static str),
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    StatusEvent {
        aggregate_id: t.id,
        institution_id: t.institution_id,
        from: Some(from.as_str()),
        to: t.status.as_str(),
        version: t.version,
        reason,
        process_instance_id: t.process_instance_id.as_deref(),
        ..Default::default()
    }
    .emit(conn, scope, ev, actor)
    .await
}

/// Section facts of a term (requirements + faculty) for validation and generation.
async fn section_facts(
    state: &AcademicState,
    tenant: TenantId,
    institution_id: Uuid,
    term: Uuid,
    extra_sections: &[Uuid],
) -> Result<Vec<SectionFacts>, KernelError> {
    let scope = ExecutionScope::tenant(tenant);
    let reqs = list_requirements(state, tenant, institution_id, Some(term), Some("active")).await?;
    let mut ids: Vec<Uuid> = reqs.iter().map(|r| r.section_id).collect();
    for s in extra_sections {
        if !ids.contains(s) {
            ids.push(*s);
        }
    }
    let mut out = Vec::new();
    for sid in ids {
        let Some(sec) = state
            .offerings
            .section(&scope, OfferingSectionId(sid))
            .await?
        else {
            continue;
        };
        let req = reqs.iter().find(|r| r.section_id == sid);
        let faculty = state
            .offerings
            .faculty_of_section(&scope, OfferingSectionId(sid))
            .await?
            .into_iter()
            .filter(|f| {
                matches!(
                    f.status,
                    FacultyAssignmentStatus::Active | FacultyAssignmentStatus::Approved
                )
            })
            .map(|f| f.faculty_subject_ref)
            .collect();
        let schedulable = !sec.offering_status.is_terminal()
            && !matches!(
                sec.offering_status,
                CourseOfferingStatus::Cancelled | CourseOfferingStatus::Completed
            )
            && matches!(sec.status, SectionStatus::Planned | SectionStatus::Active);
        out.push(SectionFacts {
            section_id: sid,
            offering_id: sec.offering_id.as_uuid(),
            label: format!("{}/{}", sec.offering_code, sec.section_code),
            schedulable,
            sessions_per_week: req.map(|r| r.sessions_per_week),
            session_minutes: req.map(|r| r.session_minutes).unwrap_or(60),
            preferred_days: req.map(|r| r.preferred_days.clone()).unwrap_or_default(),
            room_label: req.and_then(|r| r.room_label.clone()),
            faculty,
        });
    }
    Ok(out)
}

/// Replaces the slots with a generated draft from the term's active requirements.
pub async fn generate(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let cur: Timetable = kit::fetch(&state.pool, TT, TT_ENTITY, tenant, id).await?;
    let facts = section_facts(state, tenant, cur.institution_id, cur.academic_term_id, &[]).await?;
    if facts.iter().all(|f| f.sessions_per_week.is_none()) {
        return Err(KernelError::Conflict(
            "the term has no active scheduling requirements to generate from".into(),
        ));
    }
    let generated = domain::generate(&facts);
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    require_institution_writable(state, &scope, InstitutionId(t.institution_id)).await?;
    if !domain::slots_editable(t.status) {
        return Err(KernelError::Conflict(format!(
            "a {} timetable cannot be regenerated",
            t.status
        )));
    }
    sqlx::query("DELETE FROM academic.timetable_slot WHERE timetable_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
    for g in &generated {
        kit::append(
            &mut tx,
            "timetable_slot",
            json!({
                "tenant_id": tenant.as_uuid(), "institution_id": t.institution_id, "timetable_id": id,
                "offering_id": g.offering_id, "section_id": g.section_id, "day_of_week": g.day_of_week,
                "starts_at": hhmm(g.starts_at), "ends_at": hhmm(g.ends_at), "room_label": g.room_label,
            }),
        )
        .await?;
    }
    let t = set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Draft,
        None,
        json!({"validation_report": null}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "generate",
        id,
        json!({"slots": generated.len()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    let _ = t;
    get(state, tenant, id).await
}

pub async fn add_slot(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    cmd: NewSlot,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let starts = parse_time("startsAt", &cmd.starts_at)?;
    let ends = parse_time("endsAt", &cmd.ends_at)?;
    if ends <= starts {
        return Err(invalid("endsAt", "range", "endsAt must be after startsAt"));
    }
    if !(1..=7).contains(&cmd.day_of_week) {
        return Err(invalid(
            "dayOfWeek",
            "range",
            "dayOfWeek is 1 (Monday) to 7",
        ));
    }
    let s = kit::section_ctx(state, ctx, cmd.section_id).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    require_institution_writable(state, &scope, InstitutionId(t.institution_id)).await?;
    if !domain::slots_editable(t.status) {
        return Err(KernelError::Conflict(format!(
            "slots of a {} timetable are immutable",
            t.status
        )));
    }
    if s.section.academic_term_id != t.academic_term_id || s.institution_uuid() != t.institution_id
    {
        return Err(invalid(
            "sectionId",
            "term",
            "the section is not offered in the timetable's term",
        ));
    }
    if let Some(space) = cmd.teaching_space_id {
        let known = state.spaces.space(&scope, space).await?;
        if known.is_none()
            && !state
                .spaces
                .spaces_of_campus(&scope, s.offering.campus_id.unwrap_or_default())
                .await?
                .is_empty()
        {
            return Err(invalid(
                "teachingSpaceId",
                "not_found",
                "unknown teaching space",
            ));
        }
    }
    kit::append(
        &mut tx,
        "timetable_slot",
        json!({
            "tenant_id": tenant.as_uuid(), "institution_id": t.institution_id, "timetable_id": id,
            "offering_id": s.offering.id.as_uuid(), "section_id": cmd.section_id, "day_of_week": cmd.day_of_week,
            "starts_at": hhmm(starts), "ends_at": hhmm(ends),
            "room_label": kit::opt_text(cmd.room_label.as_deref()), "teaching_space_id": cmd.teaching_space_id,
        }),
    )
    .await?;
    set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Draft,
        None,
        json!({"validation_report": null}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "add_slot",
        id,
        json!({"sectionId": cmd.section_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn remove_slot(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    slot_id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    if !domain::slots_editable(t.status) {
        return Err(KernelError::Conflict(format!(
            "slots of a {} timetable are immutable",
            t.status
        )));
    }
    let n = sqlx::query("DELETE FROM academic.timetable_slot WHERE id = $1 AND timetable_id = $2 AND tenant_id = $3")
        .bind(slot_id)
        .bind(id)
        .bind(tenant.as_uuid())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?
        .rows_affected();
    if n == 0 {
        return Err(KernelError::NotFound {
            entity: "timetable slot",
        });
    }
    set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Draft,
        None,
        json!({"validation_report": null}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "remove_slot",
        id,
        json!({"slotId": slot_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn validate(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let cur = get(state, tenant, id).await?;
    if !matches!(
        cur.status,
        TimetableStatus::Draft | TimetableStatus::Validated
    ) {
        return Err(illegal("timetable", cur.status, TimetableStatus::Validated));
    }
    let slot_sections: Vec<Uuid> = cur.slots.iter().map(|s| s.section_id).collect();
    let facts = section_facts(
        state,
        tenant,
        cur.institution_id,
        cur.academic_term_id,
        &slot_sections,
    )
    .await?;
    let slot_facts: Vec<SlotFacts> = cur
        .slots
        .iter()
        .map(|s| {
            let f = facts.iter().find(|f| f.section_id == s.section_id);
            Ok(SlotFacts {
                id: s.id,
                section_id: s.section_id,
                section_label: f.map(|f| f.label.clone()).unwrap_or_default(),
                day_of_week: s.day_of_week,
                starts_at: parse_time("startsAt", &s.starts_at)?,
                ends_at: parse_time("endsAt", &s.ends_at)?,
                room_label: s.room_label.clone(),
                faculty: f.map(|f| f.faculty.clone()).unwrap_or_default(),
            })
        })
        .collect::<Result<_, KernelError>>()?;
    let conflicts = domain::validate(&slot_facts, &facts);
    let valid = domain::is_valid(&conflicts);
    let report = ValidationReport {
        valid,
        conflicts,
        validated_at: Utc::now(),
    };
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    if t.pending_decision {
        return Err(KernelError::Conflict(
            "the timetable is waiting for an approval decision".into(),
        ));
    }
    let to = if valid {
        TimetableStatus::Validated
    } else {
        TimetableStatus::Draft
    };
    let report_json = serde_json::to_value(&report).map_err(KernelError::internal)?;
    if to == t.status {
        kit::patch::<Timetable>(
            &mut tx,
            TT,
            tenant,
            id,
            json!({"validation_report": report_json}),
        )
        .await?;
    } else {
        require(t.status, to)?;
        set_status(
            &mut tx,
            tenant,
            id,
            to,
            None,
            json!({"validation_report": report_json}),
        )
        .await?;
    }
    if let Some(rev) = open_revision_for(&mut tx, id).await? {
        if valid && rev.status == "requested" {
            kit::patch::<serde_json::Value>(
                &mut tx,
                "timetable_revision",
                tenant,
                rev.id,
                json!({"status": "validated"}),
            )
            .await?;
        }
    }
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "validate",
        id,
        json!({"valid": valid, "conflicts": report.conflicts.len()}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

async fn open_revision_for(
    conn: &mut PgConnection,
    revised_id: Uuid,
) -> Result<Option<RevisionRow>, KernelError> {
    let v: Option<serde_json::Value> = sqlx::query_scalar(
        "SELECT to_jsonb(r) FROM academic.timetable_revision r WHERE r.revised_timetable_id = $1 \
         AND r.status IN ('requested', 'validated', 'approved') FOR UPDATE",
    )
    .bind(revised_id)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    v.map(kit::decode).transpose()
}

/// Requests approval (`TIMETABLE_APPROVAL`, or `TIMETABLE_REVISION` for a revised version).
pub async fn submit(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let cur: Timetable = kit::fetch(&state.pool, TT, TT_ENTITY, tenant, id).await?;
    check_version(cur.version, expected)?;
    if cur.status != TimetableStatus::Validated || cur.pending_decision {
        return Err(KernelError::Conflict(format!(
            "only a validated timetable can be submitted for approval (it is {}{})",
            cur.status,
            if cur.pending_decision {
                ", awaiting a decision"
            } else {
                ""
            }
        )));
    }
    if !cur.validation_report.as_ref().is_some_and(|r| r.valid) {
        return Err(KernelError::Conflict(
            "unresolved critical conflicts block approval".into(),
        ));
    }
    let scope = institution_scope(ctx, InstitutionId(cur.institution_id));
    require_institution_writable(state, &scope, InstitutionId(cur.institution_id)).await?;
    let key = if cur.revision_of_id.is_some() {
        process::TIMETABLE_REVISION
    } else {
        process::TIMETABLE_APPROVAL
    };
    let mut vars = BTreeMap::new();
    vars.insert("timetableId".to_owned(), json!(id));
    let outcome =
        approval::request_approval(state, &scope, key, business_key(TT, id, cur.version), vars)
            .await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    let pid = outcome.process_instance_id().to_owned();
    let t = kit::patch::<Timetable>(
        &mut tx,
        TT,
        tenant,
        t.id,
        json!({"pending_decision": true, "process_instance_id": pid}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "submit",
        id,
        json!({"processInstanceId": pid}),
        actor,
    )
    .await?;
    let t = if let ApprovalOutcome::Approved { .. } = outcome {
        apply_decision(
            &mut tx,
            &scope,
            t,
            true,
            None,
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?
    } else {
        t
    };
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, t.id).await
}

async fn apply_decision(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    t: Timetable,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = scope.tenant_id;
    let to = if approve {
        TimetableStatus::Approved
    } else {
        TimetableStatus::Draft
    };
    require(t.status, to)?;
    let nt = set_status(
        conn,
        tenant,
        t.id,
        to,
        reason,
        json!({"pending_decision": false}),
    )
    .await?;
    if let Some(rev) = open_revision_for(conn, t.id).await? {
        if approve {
            kit::patch::<serde_json::Value>(
                conn,
                "timetable_revision",
                tenant,
                rev.id,
                json!({"status": "approved"}),
            )
            .await?;
        } else {
            // a rejected revision goes back for rework; the operational version stays authoritative
            kit::patch::<serde_json::Value>(
                conn,
                "timetable_revision",
                tenant,
                rev.id,
                json!({"status": "requested", "status_reason": reason}),
            )
            .await?;
        }
    }
    kit::audit(
        conn,
        scope,
        TT,
        if approve { "approve" } else { "reject" },
        t.id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    Ok(nt)
}

pub async fn decide(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    if !t.pending_decision {
        return Err(KernelError::Conflict(
            "the timetable is not waiting for an approval decision".into(),
        ));
    }
    apply_decision(&mut tx, &scope, t, approve, reason.as_deref(), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn publish(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    require_institution_writable(state, &scope, InstitutionId(t.institution_id)).await?;
    require(t.status, TimetableStatus::Published)?;
    let from = t.status;
    let t = set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Published,
        None,
        json!({"published_at": Utc::now()}),
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        &t,
        from,
        (TIMETABLE_PUBLISHED, TIMETABLE_PUBLISHED_SCHEMA),
        None,
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "publish",
        id,
        json!({"versionNo": t.version_no}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Published → active (a revised version replaces the operational one); frozen → active.
pub async fn activate(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    require_institution_writable(state, &scope, InstitutionId(t.institution_id)).await?;
    require(t.status, TimetableStatus::Active)?;
    let from = t.status;
    if from == TimetableStatus::Published {
        // the one operational timetable of the scope
        let current: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM academic.timetable WHERE tenant_id = $1 AND institution_id = $2 \
             AND academic_term_id = $3 AND status IN ('active', 'frozen') AND id <> $4 FOR UPDATE",
        )
        .bind(tenant.as_uuid())
        .bind(t.institution_id)
        .bind(t.academic_term_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if let Some(cur_id) = current {
            if t.revision_of_id != Some(cur_id) {
                return Err(KernelError::Conflict(
                    "another timetable is operational for this term; revise or retire it first"
                        .into(),
                ));
            }
            let base: Timetable = kit::lock(&mut tx, TT, TT_ENTITY, tenant, cur_id).await?;
            let base_from = base.status;
            let base = set_status(
                &mut tx,
                tenant,
                cur_id,
                TimetableStatus::Revised,
                Some("replaced by a revised version"),
                json!({}),
            )
            .await?;
            emit(
                &mut tx,
                &scope,
                &base,
                base_from,
                (TIMETABLE_REVISED, TIMETABLE_REVISED_SCHEMA),
                Some("replaced by a revised version"),
                actor,
            )
            .await?;
            if let Some(rev) = open_revision_for(&mut tx, id).await? {
                kit::patch::<serde_json::Value>(
                    &mut tx,
                    "timetable_revision",
                    tenant,
                    rev.id,
                    json!({"status": "applied"}),
                )
                .await?;
            }
        }
    }
    let t = set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Active,
        None,
        json!({"activated_at": Utc::now()}),
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        &t,
        from,
        (TIMETABLE_ACTIVATED, TIMETABLE_ACTIVATED_SCHEMA),
        None,
        actor,
    )
    .await?;
    kit::audit(&mut tx, &scope, TT, "activate", id, json!({}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn freeze(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    require(t.status, TimetableStatus::Frozen)?;
    set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Frozen,
        reason.as_deref(),
        json!({}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "freeze",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Opens a governed revision: a distinct draft version copying the operational slots.
pub async fn request_revision(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    cmd: NewRevision,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let reason = required_text("reason", Some(&cmd.reason))?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, None).await?;
    require_institution_writable(state, &scope, InstitutionId(t.institution_id)).await?;
    if !matches!(t.status, TimetableStatus::Active | TimetableStatus::Frozen) {
        return Err(KernelError::Conflict(format!(
            "only the operational timetable can be revised (it is {})",
            t.status
        )));
    }
    let open: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM academic.timetable_revision WHERE timetable_id = $1 AND status IN ('requested', 'validated', 'approved')",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open.is_some() {
        return Err(KernelError::Conflict(
            "a revision of this timetable is already in progress".into(),
        ));
    }
    let nt: Timetable = kit::insert(
        &mut tx,
        TT,
        json!({
            "tenant_id": tenant.as_uuid(), "institution_id": t.institution_id, "academic_term_id": t.academic_term_id,
            "name": t.name, "version_no": t.version_no + 1, "revision_of_id": id, "status": "draft",
            "effective_from": t.effective_from,
        }),
    )
    .await?;
    sqlx::query(
        "INSERT INTO academic.timetable_slot (tenant_id, institution_id, timetable_id, offering_id, section_id, \
         day_of_week, starts_at, ends_at, room_label, teaching_space_id) \
         SELECT tenant_id, institution_id, $2, offering_id, section_id, day_of_week, starts_at, ends_at, room_label, \
         teaching_space_id FROM academic.timetable_slot WHERE timetable_id = $1",
    )
    .bind(id)
    .bind(nt.id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    kit::append(
        &mut tx,
        "timetable_revision",
        json!({
            "tenant_id": tenant.as_uuid(), "institution_id": t.institution_id, "timetable_id": id,
            "revised_timetable_id": nt.id, "reason": reason, "affected_section_ids": cmd.affected_section_ids,
        }),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "request_revision",
        id,
        json!({"revisedTimetableId": nt.id, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, nt.id).await
}

/// Withdraws an unpublished revision draft (archived; the operational version is untouched).
pub async fn withdraw(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    if t.pending_decision {
        return Err(KernelError::Conflict(
            "the timetable is waiting for an approval decision".into(),
        ));
    }
    require(t.status, TimetableStatus::Archived)?;
    if let Some(rev) = open_revision_for(&mut tx, id).await? {
        kit::patch::<serde_json::Value>(
            &mut tx,
            "timetable_revision",
            tenant,
            rev.id,
            json!({"status": "withdrawn", "status_reason": reason}),
        )
        .await?;
    }
    set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Archived,
        reason.as_deref().or(Some("withdrawn")),
        json!({}),
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "withdraw",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn retire(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let reason = required_text("reason", reason.as_deref())?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    require(t.status, TimetableStatus::Retired)?;
    let open: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM academic.timetable_revision WHERE timetable_id = $1 AND status IN ('requested', 'validated', 'approved')",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if open.is_some() {
        return Err(KernelError::Conflict(
            "a revision of this timetable is in progress; withdraw or apply it first".into(),
        ));
    }
    let live_sessions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.teaching_session s JOIN academic.timetable_slot sl ON sl.id = s.timetable_slot_id \
         WHERE sl.timetable_id = $1 AND s.status IN ('started', 'in_progress', 'paused')",
    )
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if live_sessions > 0 {
        return Err(KernelError::Conflict(
            "teaching sessions from this timetable are still running".into(),
        ));
    }
    let from = t.status;
    let t = set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Retired,
        Some(&reason),
        json!({"retired_at": Utc::now()}),
    )
    .await?;
    emit(
        &mut tx,
        &scope,
        &t,
        from,
        (TIMETABLE_RETIRED, TIMETABLE_RETIRED_SCHEMA),
        Some(&reason),
        actor,
    )
    .await?;
    kit::audit(
        &mut tx,
        &scope,
        TT,
        "retire",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

pub async fn archive(
    state: &AcademicState,
    ctx: &AuthContext,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Timetable, KernelError> {
    let tenant = ctx.scope.tenant_id;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let Locked { t, scope } = lock_tt(&mut tx, ctx, id, Some(expected)).await?;
    if !matches!(
        t.status,
        TimetableStatus::Retired | TimetableStatus::Revised
    ) {
        return Err(illegal("timetable", t.status, TimetableStatus::Archived));
    }
    set_status(
        &mut tx,
        tenant,
        id,
        TimetableStatus::Archived,
        None,
        json!({}),
    )
    .await?;
    kit::audit(&mut tx, &scope, TT, "archive", id, json!({}), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    get(state, tenant, id).await
}

/// Validates that `slot_id` belongs to an operational timetable and to `section_id`; returns its times.
pub async fn operational_slot(
    state: &AcademicState,
    tenant: TenantId,
    slot_id: Uuid,
    section_id: Uuid,
) -> Result<(i16, NaiveTime, NaiveTime), KernelError> {
    let row: Option<(i16, NaiveTime, NaiveTime, Uuid, String)> = sqlx::query_as(
        "SELECT s.day_of_week, s.starts_at, s.ends_at, s.section_id, t.status FROM academic.timetable_slot s \
         JOIN academic.timetable t ON t.id = s.timetable_id WHERE s.id = $1 AND s.tenant_id = $2",
    )
    .bind(slot_id)
    .bind(tenant.as_uuid())
    .fetch_optional(&state.pool)
    .await
    .map_err(map_db_error)?;
    let Some((dow, st, en, sec, status)) = row else {
        return Err(invalid(
            "timetableSlotId",
            "not_found",
            "no such timetable slot",
        ));
    };
    if sec != section_id {
        return Err(invalid(
            "timetableSlotId",
            "section",
            "the slot belongs to another section",
        ));
    }
    if status != "active" {
        return Err(KernelError::Conflict(format!(
            "the slot's timetable is {status}; sessions come from the active timetable"
        )));
    }
    Ok((dow, st, en))
}
