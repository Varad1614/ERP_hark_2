//! Persistence + commands of the registration aggregates (owner-local transactions with audit
//! and outbox in the same transaction).

use super::domain::*;
use super::support::{
    audit, check_version, conflict, emit, invalid, standing_of, status_changed, write_student,
    StudentScope,
};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::student::lock_context;
use crate::shared::time::business_today;
use crate::shared::{Actor, StudentState};
use serde_json::json;
use sqlx::{PgConnection, PgExecutor, PgPool};
use std::collections::BTreeMap;
use sutra_contracts::finance::{FinanceEvidence, StudentFinanceQuery};
use sutra_contracts::institution::reference::CurriculumComponentRef;
use sutra_contracts::institution::status::PublicationStatus;
use sutra_contracts::student::events::registration::*;
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::status::{
    AcademicStanding, HoldSource, RegistrationHoldStatus, RegistrationLineKind,
    RegistrationLineStatus, RegistrationStatus, RegistrationWindowStatus,
};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

const WINDOW: KernelError = KernelError::NotFound {
    entity: "registration window",
};
const REGISTRATION: KernelError = KernelError::NotFound {
    entity: "registration",
};
const LINE: KernelError = KernelError::NotFound {
    entity: "registration line",
};
const HOLD: KernelError = KernelError::NotFound {
    entity: "registration hold",
};

fn opt_text(v: Option<String>, field: &str, max: usize) -> Result<Option<String>, KernelError> {
    let v = v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    if v.as_ref().is_some_and(|s| s.chars().count() > max) {
        return Err(invalid(
            field,
            "too_long",
            format!("at most {max} characters"),
        ));
    }
    Ok(v)
}

// ================================================================== windows

async fn window_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<WindowRow, KernelError> {
    let sql = format!(
        "SELECT {WINDOW_COLUMNS} FROM student.registration_window WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, WindowRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(WINDOW)
}

pub async fn get_window(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<RegistrationWindow, KernelError> {
    window_row(pool, tenant, id, false).await?.into_dto()
}

pub async fn list_windows(
    state: &StudentState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    status: Option<RegistrationWindowStatus>,
) -> Result<Vec<RegistrationWindow>, KernelError> {
    state
        .references
        .institution(scope, institution_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "institution",
        })?;
    let sql = format!(
        "SELECT {WINDOW_COLUMNS} FROM student.registration_window WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY opens_on DESC, id DESC"
    );
    sqlx::query_as::<_, WindowRow>(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(status.map(|s| s.as_str()))
        .fetch_all(&state.pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(WindowRow::into_dto)
        .collect()
}

fn check_credit_bounds(min: Option<f64>, max: Option<f64>) -> Result<(), KernelError> {
    if min.is_some_and(|m| !(0.0..=1000.0).contains(&m)) {
        return Err(invalid("minCredits", "out_of_range", "0 … 1000"));
    }
    if max.is_some_and(|m| !(m > 0.0 && m <= 1000.0)) {
        return Err(invalid(
            "maxCredits",
            "out_of_range",
            "greater than 0, at most 1000",
        ));
    }
    if let (Some(a), Some(b)) = (min, max) {
        if a > b {
            return Err(invalid(
                "minCredits",
                "exceeds_max",
                "minCredits exceeds maxCredits",
            ));
        }
    }
    Ok(())
}

/// Schedules a window (L3-C-020). The term must belong to the institution and to a published
/// calendar version; dates default to the version's `registration_window` key date.
pub async fn create_window(
    state: &StudentState,
    scope: &ExecutionScope,
    institution_id: InstitutionId,
    cmd: CreateWindow,
    actor: &Actor,
) -> Result<RegistrationWindow, KernelError> {
    require_institution_writable(state, scope, institution_id).await?;
    let term = state
        .references
        .academic_term(scope, cmd.academic_term_id)
        .await?
        .filter(|t| t.institution_id == institution_id)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "unknown",
                "no such term in this institution",
            )
        })?;
    if term.calendar_version_status != PublicationStatus::Published {
        return Err(invalid(
            "academicTermId",
            "calendar_not_published",
            "the term's calendar version is not published",
        ));
    }
    let (mut opens_on, mut closes_on, mut key_date_id) = (cmd.opens_on, cmd.closes_on, None);
    if opens_on.is_none() || closes_on.is_none() {
        let kd = state
            .references
            .key_dates(scope, term.calendar_version_id)
            .await?
            .into_iter()
            .filter(|k| k.kind == "registration_window")
            .filter(|k| {
                k.ends_on <= term.ends_on && k.ends_on + chrono::Days::new(120) >= term.starts_on
            })
            .min_by_key(|k| (k.starts_on - term.starts_on).num_days().abs());
        match kd {
            Some(k) => {
                opens_on = opens_on.or(Some(k.starts_on));
                closes_on = closes_on.or(Some(k.ends_on));
                key_date_id = Some(k.id);
            }
            None => {
                return Err(invalid(
                    "opensOn",
                    "required",
                    "give opensOn/closesOn or add a registration_window key date to the calendar",
                ))
            }
        }
    }
    let (opens_on, closes_on) = (opens_on.unwrap_or_default(), closes_on.unwrap_or_default());
    if opens_on > closes_on {
        return Err(invalid(
            "closesOn",
            "before_open",
            "closesOn is before opensOn",
        ));
    }
    if closes_on > term.ends_on {
        return Err(invalid(
            "closesOn",
            "after_term",
            "the window must close by the end of the term",
        ));
    }
    check_credit_bounds(cmd.min_credits, cmd.max_credits)?;
    let name =
        opt_text(cmd.name, "name", 120)?.unwrap_or_else(|| format!("{} registration", term.name));
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.registration_window (tenant_id, institution_id, academic_term_id, name, \
         opens_on, closes_on, min_credits, max_credits, key_date_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {WINDOW_COLUMNS}"
    );
    let row = sqlx::query_as::<_, WindowRow>(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(term.id)
        .bind(&name)
        .bind(opens_on)
        .bind(closes_on)
        .bind(cmd.min_credits)
        .bind(cmd.max_credits)
        .bind(key_date_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                conflict("the term already has a scheduled or open registration window")
            }
            other => other,
        })?;
    let w = row.into_dto()?;
    audit(
        &mut tx,
        scope,
        "student.registration_window.create",
        "registration_window",
        w.id,
        json!({"academicTermId": w.academic_term_id, "opensOn": w.opens_on, "closesOn": w.closes_on, "keyDateId": w.key_date_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(w)
}

pub async fn update_window(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    cmd: UpdateWindow,
    actor: &Actor,
) -> Result<RegistrationWindow, KernelError> {
    let cur = window_row(&state.pool, tenant, id, false)
        .await?
        .into_dto()?;
    let scope = ExecutionScope::institution(tenant, InstitutionId(cur.institution_id));
    require_institution_writable(state, &scope, InstitutionId(cur.institution_id)).await?;
    let term = state
        .references
        .academic_term(&scope, cur.academic_term_id)
        .await?
        .ok_or_else(|| invalid("academicTermId", "unknown", "the term no longer exists"))?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let w = window_row(&mut *tx, tenant, id, true).await?.into_dto()?;
    check_version(w.version, expected)?;
    let open = match w.status {
        RegistrationWindowStatus::Scheduled => false,
        RegistrationWindowStatus::Open => true,
        s => {
            return Err(conflict(format!(
                "the window is {s} and can no longer change"
            )))
        }
    };
    if open && cmd.opens_on.is_some_and(|d| d != w.opens_on) {
        return Err(conflict("an open window cannot change its opening date"));
    }
    let opens_on = cmd.opens_on.unwrap_or(w.opens_on);
    let closes_on = cmd.closes_on.unwrap_or(w.closes_on);
    if opens_on > closes_on {
        return Err(invalid(
            "closesOn",
            "before_open",
            "closesOn is before opensOn",
        ));
    }
    if closes_on > term.ends_on {
        return Err(invalid(
            "closesOn",
            "after_term",
            "the window must close by the end of the term",
        ));
    }
    let min = cmd.min_credits.or(w.min_credits);
    let max = cmd.max_credits.or(w.max_credits);
    check_credit_bounds(min, max)?;
    let name = opt_text(cmd.name, "name", 120)?.unwrap_or(w.name.clone());
    let sql = format!(
        "UPDATE student.registration_window SET name = $3, opens_on = $4, closes_on = $5, \
         min_credits = $6, max_credits = $7, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING {WINDOW_COLUMNS}"
    );
    let w2 = sqlx::query_as::<_, WindowRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(&name)
        .bind(opens_on)
        .bind(closes_on)
        .bind(min)
        .bind(max)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .into_dto()?;
    audit(
        &mut tx,
        &scope,
        "student.registration_window.update",
        "registration_window",
        id,
        json!({"opensOn": opens_on, "closesOn": closes_on, "minCredits": min, "maxCredits": max}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(w2)
}

/// open / close / cancel a window.
pub async fn transition_window(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    to: RegistrationWindowStatus,
    reason: Option<String>,
    actor: &Actor,
) -> Result<RegistrationWindow, KernelError> {
    let cur = window_row(&state.pool, tenant, id, false)
        .await?
        .into_dto()?;
    let inst = InstitutionId(cur.institution_id);
    let scope = ExecutionScope::institution(tenant, inst);
    require_institution_writable(state, &scope, inst).await?;
    if to == RegistrationWindowStatus::Open {
        // Prerequisites (L4-C-020-BH-002/004): published calendar term, business date inside the window.
        let term = state
            .references
            .academic_term(&scope, cur.academic_term_id)
            .await?
            .ok_or_else(|| conflict("the window's term no longer exists"))?;
        if term.calendar_version_status != PublicationStatus::Published {
            return Err(conflict(
                "the term's calendar version is no longer published",
            ));
        }
        let today = business_today(state, &scope, inst).await?;
        if today < cur.opens_on {
            return Err(conflict(format!(
                "the window opens on {}; today is {today}",
                cur.opens_on
            )));
        }
        if today > cur.closes_on {
            return Err(conflict(format!(
                "the window closed on {}; reschedule it before opening",
                cur.closes_on
            )));
        }
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let w = window_row(&mut *tx, tenant, id, true).await?.into_dto()?;
    check_version(w.version, expected)?;
    if !window_can_transition(w.status, to) {
        return Err(conflict(format!(
            "registration window is {}; it cannot become {to}",
            w.status
        )));
    }
    let sql = format!(
        "UPDATE student.registration_window SET status = $3, status_reason = $4, \
         opened_at = CASE WHEN $3 = 'open' THEN now() ELSE opened_at END, \
         closed_at = CASE WHEN $3 IN ('closed', 'cancelled') THEN now() ELSE closed_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 \
         RETURNING {WINDOW_COLUMNS}"
    );
    let w2 = sqlx::query_as::<_, WindowRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(to.as_str())
        .bind(reason.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .into_dto()?;
    let verb = match to {
        RegistrationWindowStatus::Open => "open",
        RegistrationWindowStatus::Closed => "close",
        _ => "cancel",
    };
    let payload = status_changed(
        id,
        &scope,
        None,
        Some(w.status.as_str()),
        to.as_str(),
        w2.version,
        reason.as_deref(),
        None,
        None,
    );
    match to {
        RegistrationWindowStatus::Open => {
            emit(
                &mut tx,
                &scope,
                REGISTRATION_WINDOW_OPENED,
                REGISTRATION_WINDOW_OPENED_SCHEMA,
                id,
                &payload,
                actor,
            )
            .await?
        }
        RegistrationWindowStatus::Closed => {
            emit(
                &mut tx,
                &scope,
                REGISTRATION_WINDOW_CLOSED,
                REGISTRATION_WINDOW_CLOSED_SCHEMA,
                id,
                &payload,
                actor,
            )
            .await?
        }
        _ => {}
    }
    audit(
        &mut tx,
        &scope,
        &format!("student.registration_window.{verb}"),
        "registration_window",
        id,
        json!({"from": w.status, "to": to, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(w2)
}

/// Finalises (freezes + publishes) every `registered` registration of a closed window (L3-C-025).
pub async fn finalise_window(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<WindowFinalisation, KernelError> {
    let cur = window_row(&state.pool, tenant, id, false)
        .await?
        .into_dto()?;
    let inst = InstitutionId(cur.institution_id);
    let scope = ExecutionScope::institution(tenant, inst);
    require_institution_writable(state, &scope, inst).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let w = window_row(&mut *tx, tenant, id, true).await?.into_dto()?;
    check_version(w.version, expected)?;
    if w.status != RegistrationWindowStatus::Closed {
        return Err(conflict(
            "only a closed window's registrations can be finalised",
        ));
    }
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM student.registration WHERE tenant_id = $1 AND registration_window_id = $2 \
         AND status = 'registered' ORDER BY id",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    for rid in &ids {
        let row = lock_registration(&mut tx, tenant, *rid).await?;
        finalise_locked(&mut tx, state, &scope, row, actor).await?;
    }
    let w2 = sqlx::query_as::<_, WindowRow>(&format!(
        "UPDATE student.registration_window SET version = version + 1, updated_at = now() \
         WHERE id = $1 RETURNING {WINDOW_COLUMNS}"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?
    .into_dto()?;
    audit(
        &mut tx,
        &scope,
        "student.registration_window.finalise",
        "registration_window",
        id,
        json!({"finalised": ids}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(WindowFinalisation {
        window: w2,
        finalised_registration_ids: ids,
    })
}

/// Open windows of the student's institution (self-service).
pub async fn open_windows_for(
    pool: &PgPool,
    tenant: TenantId,
    institution_id: Uuid,
) -> Result<Vec<RegistrationWindow>, KernelError> {
    let sql = format!(
        "SELECT {WINDOW_COLUMNS} FROM student.registration_window WHERE tenant_id = $1 \
         AND institution_id = $2 AND status IN ('scheduled', 'open') ORDER BY opens_on, id"
    );
    sqlx::query_as::<_, WindowRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(institution_id)
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(WindowRow::into_dto)
        .collect()
}

// ================================================================== registrations

async fn registration_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<RegistrationRow, KernelError> {
    let sql = format!(
        "SELECT {REGISTRATION_COLUMNS} FROM student.registration WHERE id = $1 AND tenant_id = $2"
    );
    sqlx::query_as::<_, RegistrationRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(REGISTRATION)
}

async fn lock_registration(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: Uuid,
) -> Result<RegistrationRow, KernelError> {
    let sql = format!(
        "SELECT {REGISTRATION_COLUMNS} FROM student.registration WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    sqlx::query_as::<_, RegistrationRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(REGISTRATION)
}

async fn lines_of<'e, E: PgExecutor<'e>>(
    ex: E,
    registration_id: Uuid,
) -> Result<Vec<RegistrationLine>, KernelError> {
    let sql = format!(
        "SELECT {LINE_COLUMNS} FROM student.registration_line WHERE registration_id = $1 \
         ORDER BY component_term_no, slot_code, created_at"
    );
    sqlx::query_as::<_, LineRow>(&sql)
        .bind(registration_id)
        .fetch_all(ex)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(LineRow::into_dto)
        .collect()
}

async fn dto(conn: &mut PgConnection, row: RegistrationRow) -> Result<Registration, KernelError> {
    let lines = lines_of(&mut *conn, row.id).await?;
    row.into_dto(lines)
}

pub async fn get_registration(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<Registration, KernelError> {
    let row = registration_row(pool, tenant, id).await?;
    let lines = lines_of(pool, id).await?;
    row.into_dto(lines)
}

pub async fn list_registrations(
    pool: &PgPool,
    tenant: TenantId,
    student_id: StudentId,
    status: Option<RegistrationStatus>,
) -> Result<Vec<Registration>, KernelError> {
    crate::shared::student::load_student(pool, tenant, student_id).await?;
    let sql = format!(
        "SELECT {REGISTRATION_COLUMNS} FROM student.registration WHERE tenant_id = $1 \
         AND student_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC"
    );
    let rows = sqlx::query_as::<_, RegistrationRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .bind(status.map(|s| s.as_str()))
        .fetch_all(pool)
        .await
        .map_err(map_db_error)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let lines = lines_of(pool, r.id).await?;
        out.push(r.into_dto(lines)?);
    }
    Ok(out)
}

/// Every Finance / hold condition that blocks registration (L4-C-021-BH-006, §7).
pub async fn registration_blockers(
    state: &StudentState,
    ss: &StudentScope,
) -> Result<(), KernelError> {
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM student.registration_hold WHERE tenant_id = $1 AND student_id = $2 \
         AND status = 'active'",
    )
    .bind(ss.scope.tenant_id.as_uuid())
    .bind(ss.student.id.as_uuid())
    .fetch_one(&state.pool)
    .await
    .map_err(map_db_error)?;
    if active > 0 {
        return Err(conflict("the student has an active registration hold"));
    }
    let dues = state
        .finance
        .student_dues(
            &ss.scope,
            StudentFinanceQuery {
                institution_id: ss.context.institution_id,
                student_id: ss.student.id.as_uuid(),
            },
        )
        .await?;
    if !dues.permits_clearance() {
        return Err(conflict(format!(
            "Finance reports outstanding dues ({}); registration is on hold",
            match dues {
                FinanceEvidence::Outstanding { detail } | FinanceEvidence::Pending { detail } =>
                    detail,
                _ => String::new(),
            }
        )));
    }
    let standing = standing_of(&state.pool, ss.scope.tenant_id, ss.context.id.as_uuid()).await?;
    if standing == AcademicStanding::AcademicBreak {
        return Err(conflict(
            "the student is on an academic break; rejoin first",
        ));
    }
    Ok(())
}

/// The window accepting registrations for `term` today (409 otherwise).
async fn accepting_window(
    state: &StudentState,
    scope: &ExecutionScope,
    institution_id: Uuid,
    term_id: Uuid,
) -> Result<RegistrationWindow, KernelError> {
    let sql = format!(
        "SELECT {WINDOW_COLUMNS} FROM student.registration_window WHERE tenant_id = $1 \
         AND institution_id = $2 AND academic_term_id = $3 AND status IN ('scheduled', 'open')"
    );
    let w = sqlx::query_as::<_, WindowRow>(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id)
        .bind(term_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(map_db_error)?
        .map(WindowRow::into_dto)
        .transpose()?
        .ok_or_else(|| conflict("no registration window is open for this term"))?;
    let today = business_today(state, scope, InstitutionId(institution_id)).await?;
    if !w.accepts_on(today) {
        return Err(conflict(format!(
            "the registration window is {} ({} – {}); it does not accept registrations today",
            w.status, w.opens_on, w.closes_on
        )));
    }
    Ok(w)
}

async fn require_window_accepting(
    state: &StudentState,
    scope: &ExecutionScope,
    tenant: TenantId,
    window_id: Uuid,
) -> Result<RegistrationWindow, KernelError> {
    let w = get_window(&state.pool, tenant, window_id).await?;
    let today = business_today(state, scope, InstitutionId(w.institution_id)).await?;
    if !w.accepts_on(today) {
        return Err(conflict(format!(
            "the registration window is {} and no longer accepts changes",
            w.status
        )));
    }
    Ok(w)
}

/// Curriculum facts a registration is validated against.
struct CurriculumFacts {
    components: Vec<CurriculumComponentRef>,
    min_credits: Option<f64>,
    max_credits: Option<f64>,
}

async fn curriculum_facts(
    state: &StudentState,
    scope: &ExecutionScope,
    curriculum_version_id: Option<Uuid>,
    window: &RegistrationWindow,
) -> Result<CurriculumFacts, KernelError> {
    let cv_id = curriculum_version_id
        .ok_or_else(|| conflict("the student context has no curriculum version"))?;
    let cv = state
        .references
        .curriculum_version(scope, cv_id)
        .await?
        .ok_or_else(|| conflict("the student's curriculum version no longer exists"))?;
    let components = state.references.curriculum_components(scope, cv_id).await?;
    Ok(CurriculumFacts {
        components,
        min_credits: window.min_credits.or(cv.min_credits_per_term),
        max_credits: window.max_credits.or(cv.max_credits_per_term),
    })
}

/// Components with net positive earned credits in the ledger of a context.
async fn earned_components(
    conn: &mut PgConnection,
    tenant: TenantId,
    context_id: Uuid,
) -> Result<Vec<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT curriculum_component_id FROM student.credit_ledger_entry WHERE tenant_id = $1 \
         AND student_context_id = $2 AND curriculum_component_id IS NOT NULL \
         GROUP BY curriculum_component_id HAVING sum(CASE WHEN kind IN ('revoked', 'expired') \
         THEN -credits ELSE credits END) > 0",
    )
    .bind(tenant.as_uuid())
    .bind(context_id)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Components registered in earlier finalised registrations of a context.
async fn previously_registered(
    conn: &mut PgConnection,
    tenant: TenantId,
    context_id: Uuid,
    except: Uuid,
) -> Result<Vec<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT DISTINCT l.curriculum_component_id FROM student.registration_line l \
         JOIN student.registration r ON r.id = l.registration_id \
         WHERE r.tenant_id = $1 AND r.student_context_id = $2 AND r.id <> $3 \
         AND r.status IN ('finalised', 'archived') AND l.status = 'registered'",
    )
    .bind(tenant.as_uuid())
    .bind(context_id)
    .bind(except)
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)
}

/// Validates and inserts one line; returns it.
#[allow(clippy::too_many_arguments)]
async fn insert_line(
    conn: &mut PgConnection,
    tenant: TenantId,
    reg: &RegistrationRow,
    facts: &CurriculumFacts,
    standing: AcademicStanding,
    input: &LineInput,
    after_registration: bool,
) -> Result<RegistrationLine, KernelError> {
    let lines = lines_of(&mut *conn, reg.id).await?;
    let selected: Vec<Uuid> = lines
        .iter()
        .filter(|l| l.is_live())
        .map(|l| l.curriculum_component_id)
        .collect();
    let earned = earned_components(conn, tenant, reg.student_context_id).await?;
    let prev = previously_registered(conn, tenant, reg.student_context_id, reg.id).await?;
    let kind = input.kind.unwrap_or(RegistrationLineKind::Regular);
    let sel = SelectionContext {
        components: &facts.components,
        term_no: reg.term_no,
        standing,
        selected: &selected,
        earned: &earned,
        previously_registered: &prev,
        current_credits: counted_credits(&lines),
        max_credits: facts.max_credits,
    };
    let c = validate_selection(&sel, input.curriculum_component_id, kind)?;
    let status = if after_registration {
        RegistrationLineStatus::Registered
    } else {
        RegistrationLineStatus::Selected
    };
    let sql = format!(
        "INSERT INTO student.registration_line (tenant_id, registration_id, curriculum_component_id, \
         kind, status, credits, component_term_no, slot_code, title, added_after_registration) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING {LINE_COLUMNS}"
    );
    sqlx::query_as::<_, LineRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(reg.id)
        .bind(c.id)
        .bind(kind.as_str())
        .bind(status.as_str())
        .bind(c.credits)
        .bind(c.term_no)
        .bind(&c.slot_code)
        .bind(&c.title)
        .bind(after_registration)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?
        .into_dto()
}

async fn history(
    conn: &mut PgConnection,
    reg: &RegistrationRow,
    from: Option<&str>,
    to: &str,
    action: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO student.registration_status_history (tenant_id, registration_id, from_status, \
         to_status, action, reason, process_instance_id, actor, correlation_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
    )
    .bind(reg.tenant_id)
    .bind(reg.id)
    .bind(from)
    .bind(to)
    .bind(action)
    .bind(reason)
    .bind(reg.process_instance_id.as_deref())
    .bind(actor.id.as_deref())
    .bind(actor.correlation_id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

/// Moves a locked registration to `to` (legal transition check, timestamps, history).
async fn set_status(
    conn: &mut PgConnection,
    reg: &RegistrationRow,
    to: RegistrationStatus,
    reason: Option<&str>,
    process_instance_id: Option<&str>,
    actor: &Actor,
) -> Result<RegistrationRow, KernelError> {
    let from = reg.status()?;
    if !registration_can_transition(from, to) {
        return Err(conflict(format!(
            "registration is {from}; it cannot become {to}"
        )));
    }
    let sql = format!(
        "UPDATE student.registration SET status = $2, status_reason = $3, \
         process_instance_id = COALESCE($4, process_instance_id), \
         submitted_at = CASE WHEN $2 = 'submitted' THEN now() ELSE submitted_at END, \
         registered_at = CASE WHEN $2 = 'registered' THEN now() ELSE registered_at END, \
         finalised_at = CASE WHEN $2 = 'finalised' THEN now() ELSE finalised_at END, \
         cancelled_at = CASE WHEN $2 = 'cancelled' THEN now() ELSE cancelled_at END, \
         version = version + 1, updated_at = now() WHERE id = $1 RETURNING {REGISTRATION_COLUMNS}"
    );
    let row = sqlx::query_as::<_, RegistrationRow>(&sql)
        .bind(reg.id)
        .bind(to.as_str())
        .bind(reason)
        .bind(process_instance_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)?;
    history(
        conn,
        &row,
        Some(from.as_str()),
        to.as_str(),
        to.as_str(),
        reason,
        actor,
    )
    .await?;
    Ok(row)
}

async fn bump(conn: &mut PgConnection, id: Uuid) -> Result<RegistrationRow, KernelError> {
    sqlx::query_as::<_, RegistrationRow>(&format!(
        "UPDATE student.registration SET version = version + 1, updated_at = now() WHERE id = $1 \
         RETURNING {REGISTRATION_COLUMNS}"
    ))
    .bind(id)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)
}

fn registration_v1(r: &Registration, subject: Option<String>) -> RegistrationV1 {
    RegistrationV1 {
        registration_id: r.id,
        student_id: r.student_id,
        student_context_id: r.student_context_id,
        institution_id: r.institution_id,
        academic_term_id: r.academic_term_id,
        status: r.status.as_str().into(),
        component_ids: r
            .lines
            .iter()
            .filter(|l| l.is_live())
            .map(|l| l.curriculum_component_id)
            .collect(),
        total_credits: r.total_credits,
        version: r.version,
        student_subject_ref: subject,
    }
}

async fn subject_of(
    conn: &mut PgConnection,
    student_id: Uuid,
) -> Result<Option<String>, KernelError> {
    sqlx::query_scalar("SELECT subject_ref FROM student.student WHERE id = $1")
        .bind(student_id)
        .fetch_one(&mut *conn)
        .await
        .map_err(map_db_error)
}

/// Creates a draft registration for a term (L3-C-021 / C-024).
pub async fn create_registration(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: CreateRegistration,
    origin: &str,
    actor: &Actor,
) -> Result<Registration, KernelError> {
    let ss = write_student(state, tenant, student_id, true).await?;
    registration_blockers(state, &ss).await?;
    let window = accepting_window(
        state,
        &ss.scope,
        ss.context.institution_id.as_uuid(),
        cmd.academic_term_id,
    )
    .await?;
    let facts =
        curriculum_facts(state, &ss.scope, ss.context.curriculum_version_id, &window).await?;
    let standing = standing_of(&state.pool, tenant, ss.context.id.as_uuid()).await?;
    let term_no = ss.context.current_term_no.unwrap_or(1);
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    lock_context(&mut tx, tenant, ss.context.id).await?;
    let sql = format!(
        "INSERT INTO student.registration (tenant_id, institution_id, student_id, student_context_id, \
         academic_term_id, registration_window_id, term_no, curriculum_version_id, origin) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING {REGISTRATION_COLUMNS}"
    );
    let row = sqlx::query_as::<_, RegistrationRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(cmd.academic_term_id)
        .bind(window.id)
        .bind(term_no)
        .bind(ss.context.curriculum_version_id)
        .bind(origin)
        .fetch_one(&mut *tx)
        .await
        .map_err(|e| match map_db_error(e) {
            KernelError::Conflict(_) => {
                conflict("the student already has a registration for this term")
            }
            other => other,
        })?;
    history(&mut tx, &row, None, "draft", "create", None, actor).await?;
    for (i, l) in cmd.lines.iter().enumerate() {
        insert_line(&mut tx, tenant, &row, &facts, standing, l, false)
            .await
            .map_err(|e| match e {
                KernelError::Validation(mut v) => {
                    for f in &mut v {
                        f.field = format!("lines[{i}].{}", f.field);
                    }
                    KernelError::Validation(v)
                }
                other => other,
            })?;
    }
    let r = dto(&mut tx, row).await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.registration.create",
        "registration",
        r.id,
        json!({"academicTermId": r.academic_term_id, "origin": origin, "lines": r.lines.len(), "totalCredits": r.total_credits}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Loads the registration + its student scope for a command.
async fn command_scope(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    require_active: bool,
) -> Result<(RegistrationRow, StudentScope), KernelError> {
    let row = registration_row(&state.pool, tenant, id).await?;
    let ss = write_student(state, tenant, StudentId(row.student_id), require_active).await?;
    if ss.context.id.as_uuid() != row.student_context_id {
        return Err(conflict(
            "the registration belongs to a closed student context",
        ));
    }
    Ok((row, ss))
}

/// Adds a line (draft editing, or add during an open window after registration — L3-C-022).
pub async fn add_line(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    input: LineInput,
    actor: &Actor,
) -> Result<Registration, KernelError> {
    let (cur, ss) = command_scope(state, tenant, id, true).await?;
    let status = cur.status()?;
    if !lines_editable(status) {
        return Err(conflict(format!(
            "registration is {status}; its lines cannot change"
        )));
    }
    let window =
        require_window_accepting(state, &ss.scope, tenant, cur.registration_window_id).await?;
    let facts = curriculum_facts(state, &ss.scope, cur.curriculum_version_id, &window).await?;
    let standing = standing_of(&state.pool, tenant, ss.context.id.as_uuid()).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = lock_registration(&mut tx, tenant, id).await?;
    check_version(row.version, expected)?;
    let status = row.status()?;
    if !lines_editable(status) {
        return Err(conflict(format!(
            "registration is {status}; its lines cannot change"
        )));
    }
    let after = status == RegistrationStatus::Registered;
    let line = insert_line(&mut tx, tenant, &row, &facts, standing, &input, after).await?;
    let row = bump(&mut tx, id).await?;
    history(
        &mut tx,
        &row,
        Some(status.as_str()),
        status.as_str(),
        "add_line",
        None,
        actor,
    )
    .await?;
    let r = dto(&mut tx, row).await?;
    if after {
        emit(
            &mut tx,
            &ss.scope,
            REGISTRATION_MODIFIED,
            REGISTRATION_MODIFIED_SCHEMA,
            id,
            &registration_v1(&r, ss.subject_ref()),
            actor,
        )
        .await?;
    }
    audit(
        &mut tx,
        &ss.scope,
        "student.registration.add_line",
        "registration",
        id,
        json!({"lineId": line.id, "curriculumComponentId": line.curriculum_component_id, "kind": line.kind, "afterRegistration": after}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Drops a line (L3-C-022). After registration the remaining credits must stay ≥ the minimum.
#[allow(clippy::too_many_arguments)]
pub async fn drop_line(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    line_id: Uuid,
    expected: Version,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Registration, KernelError> {
    let (cur, ss) = command_scope(state, tenant, id, true).await?;
    let status = cur.status()?;
    if !lines_editable(status) {
        return Err(conflict(format!(
            "registration is {status}; its lines cannot change"
        )));
    }
    let window =
        require_window_accepting(state, &ss.scope, tenant, cur.registration_window_id).await?;
    let facts = curriculum_facts(state, &ss.scope, cur.curriculum_version_id, &window).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = lock_registration(&mut tx, tenant, id).await?;
    check_version(row.version, expected)?;
    let status = row.status()?;
    if !lines_editable(status) {
        return Err(conflict(format!(
            "registration is {status}; its lines cannot change"
        )));
    }
    let lines = lines_of(&mut *tx, id).await?;
    let line = lines.iter().find(|l| l.id == line_id).ok_or(LINE)?;
    if !line.is_live() {
        return Err(conflict("the line is already dropped"));
    }
    let after = status == RegistrationStatus::Registered;
    if after {
        let remaining = counted_credits(&lines)
            - if line.kind == RegistrationLineKind::Audit {
                0.0
            } else {
                line.credits
            };
        if let Some(min) = facts.min_credits {
            if remaining + 1e-9 < min {
                return Err(invalid(
                    "lineId",
                    "below_minimum",
                    format!("dropping leaves {remaining} credits; the minimum is {min}"),
                ));
            }
        }
    }
    sqlx::query(
        "UPDATE student.registration_line SET status = 'dropped', dropped_at = now(), drop_reason = $2, \
         updated_at = now() WHERE id = $1",
    )
    .bind(line_id)
    .bind(reason.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let row = bump(&mut tx, id).await?;
    history(
        &mut tx,
        &row,
        Some(status.as_str()),
        status.as_str(),
        "drop_line",
        reason.as_deref(),
        actor,
    )
    .await?;
    let r = dto(&mut tx, row).await?;
    if after {
        emit(
            &mut tx,
            &ss.scope,
            REGISTRATION_MODIFIED,
            REGISTRATION_MODIFIED_SCHEMA,
            id,
            &registration_v1(&r, ss.subject_ref()),
            actor,
        )
        .await?;
    }
    audit(
        &mut tx,
        &ss.scope,
        "student.registration.drop_line",
        "registration",
        id,
        json!({"lineId": line_id, "afterRegistration": after, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Applies an approval inside the tx: approved → registered (lines registered) + events.
async fn apply_approval(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    row: RegistrationRow,
    pid: Option<&str>,
    actor: &Actor,
    subject: Option<String>,
) -> Result<RegistrationRow, KernelError> {
    let approved = set_status(conn, &row, RegistrationStatus::Approved, None, pid, actor).await?;
    emit(
        conn,
        scope,
        REGISTRATION_APPROVED,
        REGISTRATION_APPROVED_SCHEMA,
        row.id,
        &status_changed(
            row.id,
            scope,
            Some(row.student_id),
            Some("submitted"),
            "approved",
            approved.version,
            None,
            pid,
            subject.clone(),
        ),
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE student.registration_line SET status = 'registered', updated_at = now() \
         WHERE registration_id = $1 AND status = 'selected'",
    )
    .bind(row.id)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let registered = set_status(
        conn,
        &approved,
        RegistrationStatus::Registered,
        None,
        None,
        actor,
    )
    .await?;
    let r = dto(conn, registered.clone()).await?;
    emit(
        conn,
        scope,
        REGISTRATION_REGISTERED,
        REGISTRATION_REGISTERED_SCHEMA,
        row.id,
        &registration_v1(&r, subject),
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        "student.registration.approve",
        "registration",
        row.id,
        json!({"processInstanceId": pid}),
        actor,
    )
    .await?;
    Ok(registered)
}

/// Submits a draft/returned registration and starts the approval step (L3-C-021 BH-004/005).
pub async fn submit(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Registration, KernelError> {
    let (cur, ss) = command_scope(state, tenant, id, true).await?;
    let status = cur.status()?;
    if !registration_can_transition(status, RegistrationStatus::Submitted) {
        return Err(conflict(format!(
            "registration is {status}; it cannot be submitted"
        )));
    }
    check_version(cur.version, expected)?;
    registration_blockers(state, &ss).await?;
    let window =
        require_window_accepting(state, &ss.scope, tenant, cur.registration_window_id).await?;
    let facts = curriculum_facts(state, &ss.scope, cur.curriculum_version_id, &window).await?;
    let lines = lines_of(&state.pool, id).await?;
    let live: Vec<_> = lines.iter().filter(|l| l.is_live()).collect();
    if live.is_empty() {
        return Err(invalid("lines", "empty", "select at least one component"));
    }
    let credits = counted_credits(&lines);
    if let Some(min) = facts.min_credits {
        if credits + 1e-9 < min {
            return Err(invalid(
                "lines",
                "below_minimum",
                format!("{credits} credits selected; the minimum is {min}"),
            ));
        }
    }
    if let Some(max) = facts.max_credits {
        if credits > max + 1e-9 {
            return Err(invalid(
                "lines",
                "credit_limit",
                format!("{credits} credits selected; the maximum is {max}"),
            ));
        }
    }
    let mut vars = BTreeMap::new();
    vars.insert("registrationId".into(), json!(id));
    vars.insert("studentId".into(), json!(cur.student_id));
    vars.insert("totalCredits".into(), json!(credits));
    let outcome = request_approval(
        state,
        &ss.scope,
        process::REGISTRATION_APPROVAL,
        business_key("registration", id, cur.version),
        vars,
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = lock_registration(&mut tx, tenant, id).await?;
    check_version(row.version, expected)?;
    let from = row.status()?;
    let row = set_status(
        &mut tx,
        &row,
        RegistrationStatus::Submitted,
        None,
        Some(&pid),
        actor,
    )
    .await?;
    emit(
        &mut tx,
        &ss.scope,
        REGISTRATION_SUBMITTED,
        REGISTRATION_SUBMITTED_SCHEMA,
        id,
        &status_changed(
            id,
            &ss.scope,
            Some(row.student_id),
            Some(from.as_str()),
            "submitted",
            row.version,
            None,
            Some(&pid),
            ss.subject_ref(),
        ),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.registration.submit",
        "registration",
        id,
        json!({"processInstanceId": pid, "totalCredits": credits}),
        actor,
    )
    .await?;
    let row = match outcome {
        ApprovalOutcome::Approved { .. } => {
            apply_approval(
                &mut tx,
                &ss.scope,
                row,
                Some(&pid),
                &Actor::system(actor.correlation_id.clone()),
                ss.subject_ref(),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => row,
    };
    let r = dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Explicit approval decision (`AwaitDecision`): approve → registered; reject → returned.
pub async fn decide(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<Registration, KernelError> {
    let (_, ss) = command_scope(state, tenant, id, false).await?;
    if !approve && reason.is_none() {
        return Err(invalid("reason", "required", "a rejection needs a reason"));
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = lock_registration(&mut tx, tenant, id).await?;
    check_version(row.version, expected)?;
    if row.status()? != RegistrationStatus::Submitted {
        return Err(conflict(format!(
            "registration is {}; there is no pending approval",
            row.status
        )));
    }
    let pid = row.process_instance_id.clone();
    let row = if approve {
        apply_approval(
            &mut tx,
            &ss.scope,
            row,
            pid.as_deref(),
            actor,
            ss.subject_ref(),
        )
        .await?
    } else {
        let r = set_status(
            &mut tx,
            &row,
            RegistrationStatus::Returned,
            reason.as_deref(),
            None,
            actor,
        )
        .await?;
        audit(
            &mut tx,
            &ss.scope,
            "student.registration.return",
            "registration",
            id,
            json!({"reason": reason}),
            actor,
        )
        .await?;
        r
    };
    let r = dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Cancels a registration (L3-C-023). Self-service cancellation needs the window to accept changes.
#[allow(clippy::too_many_arguments)]
pub async fn cancel(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    reason: Option<String>,
    self_service: bool,
    actor: &Actor,
) -> Result<Registration, KernelError> {
    let (cur, ss) = command_scope(state, tenant, id, false).await?;
    if self_service {
        require_window_accepting(state, &ss.scope, tenant, cur.registration_window_id).await?;
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = lock_registration(&mut tx, tenant, id).await?;
    check_version(row.version, expected)?;
    let from = row.status()?;
    let row = set_status(
        &mut tx,
        &row,
        RegistrationStatus::Cancelled,
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE student.registration_line SET status = 'withdrawn', updated_at = now() \
         WHERE registration_id = $1 AND status IN ('selected', 'registered')",
    )
    .bind(id)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    emit(
        &mut tx,
        &ss.scope,
        REGISTRATION_CANCELLED,
        REGISTRATION_CANCELLED_SCHEMA,
        id,
        &status_changed(
            id,
            &ss.scope,
            Some(row.student_id),
            Some(from.as_str()),
            "cancelled",
            row.version,
            reason.as_deref(),
            None,
            ss.subject_ref(),
        ),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.registration.cancel",
        "registration",
        id,
        json!({"from": from, "reason": reason, "selfService": self_service}),
        actor,
    )
    .await?;
    let r = dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

async fn finalise_locked(
    conn: &mut PgConnection,
    _state: &StudentState,
    scope: &ExecutionScope,
    row: RegistrationRow,
    actor: &Actor,
) -> Result<RegistrationRow, KernelError> {
    let id = row.id;
    let row = set_status(conn, &row, RegistrationStatus::Finalised, None, None, actor).await?;
    let subject = subject_of(conn, row.student_id).await?;
    let r = dto(conn, row.clone()).await?;
    emit(
        conn,
        scope,
        REGISTRATION_FINALISED,
        REGISTRATION_FINALISED_SCHEMA,
        id,
        &registration_v1(&r, subject),
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        "student.registration.finalise",
        "registration",
        id,
        json!({"totalCredits": r.total_credits}),
        actor,
    )
    .await?;
    Ok(row)
}

/// Freezes + publishes one registered registration (L3-C-025).
pub async fn finalise(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    actor: &Actor,
) -> Result<Registration, KernelError> {
    let (_, ss) = command_scope(state, tenant, id, false).await?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let row = lock_registration(&mut tx, tenant, id).await?;
    check_version(row.version, expected)?;
    let row = finalise_locked(&mut tx, state, &ss.scope, row, actor).await?;
    let r = dto(&mut tx, row).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(r)
}

/// Registration history (immutable).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationHistoryEntry {
    pub from_status: Option<String>,
    pub to_status: String,
    pub action: String,
    pub reason: Option<String>,
    pub actor: Option<String>,
    pub occurred_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RegistrationHistoryList {
    pub items: Vec<RegistrationHistoryEntry>,
}

pub async fn registration_history(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<Vec<RegistrationHistoryEntry>, KernelError> {
    registration_row(pool, tenant, id).await?;
    type HistoryRow = (
        Option<String>,
        String,
        String,
        Option<String>,
        Option<String>,
        chrono::DateTime<chrono::Utc>,
    );
    let rows: Vec<HistoryRow> = sqlx::query_as(
        "SELECT from_status, to_status, action, reason, actor, occurred_at FROM \
             student.registration_status_history WHERE registration_id = $1 AND tenant_id = $2 \
             ORDER BY occurred_at, id",
    )
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_all(pool)
    .await
    .map_err(map_db_error)?;
    Ok(rows
        .into_iter()
        .map(
            |(from_status, to_status, action, reason, actor, occurred_at)| {
                RegistrationHistoryEntry {
                    from_status,
                    to_status,
                    action,
                    reason,
                    actor,
                    occurred_at,
                }
            },
        )
        .collect())
}

// ================================================================== holds

async fn hold_row<'e, E: PgExecutor<'e>>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<HoldRow, KernelError> {
    let sql = format!(
        "SELECT {HOLD_COLUMNS} FROM student.registration_hold WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    sqlx::query_as::<_, HoldRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(ex)
        .await
        .map_err(map_db_error)?
        .ok_or(HOLD)
}

pub async fn get_hold(
    pool: &PgPool,
    tenant: TenantId,
    id: Uuid,
) -> Result<RegistrationHold, KernelError> {
    hold_row(pool, tenant, id, false).await?.into_dto()
}

pub async fn list_holds(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    status: Option<RegistrationHoldStatus>,
) -> Result<HoldList, KernelError> {
    let (_s, ctx) = super::support::read_student(state, tenant, student_id).await?;
    let sql = format!(
        "SELECT {HOLD_COLUMNS} FROM student.registration_hold WHERE tenant_id = $1 AND student_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY placed_at DESC, id DESC"
    );
    let items = sqlx::query_as::<_, HoldRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(student_id.as_uuid())
        .bind(status.map(|s| s.as_str()))
        .fetch_all(&state.pool)
        .await
        .map_err(map_db_error)?
        .into_iter()
        .map(HoldRow::into_dto)
        .collect::<Result<Vec<_>, _>>()?;
    let finance_evidence = match ctx {
        Some(c) => {
            state
                .finance
                .student_dues(
                    &ExecutionScope::institution(tenant, c.institution_id),
                    StudentFinanceQuery {
                        institution_id: c.institution_id,
                        student_id: student_id.as_uuid(),
                    },
                )
                .await?
        }
        None => FinanceEvidence::NotApplicable,
    };
    Ok(HoldList {
        items,
        finance_evidence,
    })
}

fn hold_v1(h: &RegistrationHold, subject: Option<String>) -> RegistrationHoldV1 {
    RegistrationHoldV1 {
        hold_id: h.id,
        student_id: h.student_id,
        institution_id: h.institution_id,
        source: h.source.as_str().into(),
        reason: h.reason.clone(),
        status: h.status.as_str().into(),
        student_subject_ref: subject,
    }
}

/// Places a hold on a student (L3-C-026).
pub async fn place_hold(
    state: &StudentState,
    tenant: TenantId,
    student_id: StudentId,
    cmd: PlaceHold,
    actor: &Actor,
) -> Result<RegistrationHold, KernelError> {
    let ss = write_student(state, tenant, student_id, false).await?;
    let reason = opt_text(Some(cmd.reason), "reason", 500)?
        .ok_or_else(|| invalid("reason", "required", "a reason is required"))?;
    let ext = opt_text(cmd.external_reference, "externalReference", 120)?;
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let sql = format!(
        "INSERT INTO student.registration_hold (tenant_id, institution_id, student_id, student_context_id, \
         source, reason, external_reference, placed_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8) \
         RETURNING {HOLD_COLUMNS}"
    );
    let h = sqlx::query_as::<_, HoldRow>(&sql)
        .bind(tenant.as_uuid())
        .bind(ss.context.institution_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(ss.context.id.as_uuid())
        .bind(cmd.source.as_str())
        .bind(&reason)
        .bind(ext.as_deref())
        .bind(actor.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .into_dto()?;
    let payload = hold_v1(&h, ss.subject_ref());
    emit(
        &mut tx,
        &ss.scope,
        REGISTRATION_HOLD_PLACED,
        REGISTRATION_HOLD_PLACED_SCHEMA,
        h.id,
        &payload,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.registration_hold.place",
        "registration_hold",
        h.id,
        json!({"source": cmd.source, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(h)
}

/// Releases an active hold (L3-C-026 BH-001…006). Finance holds need an external reference
/// (the Finance clearance evidence) unless Finance currently reports no obligation.
pub async fn release_hold(
    state: &StudentState,
    tenant: TenantId,
    id: Uuid,
    expected: Version,
    reason: String,
    actor: &Actor,
) -> Result<RegistrationHold, KernelError> {
    let cur = get_hold(&state.pool, tenant, id).await?;
    let ss = write_student(state, tenant, StudentId(cur.student_id), false).await?;
    if cur.source == HoldSource::Finance {
        let dues = state
            .finance
            .student_dues(
                &ss.scope,
                StudentFinanceQuery {
                    institution_id: ss.context.institution_id,
                    student_id: cur.student_id,
                },
            )
            .await?;
        if !dues.permits_clearance() {
            return Err(conflict(
                "Finance still reports outstanding dues; the finance hold stays",
            ));
        }
    }
    let mut tx = state.pool.begin().await.map_err(map_db_error)?;
    let h = hold_row(&mut *tx, tenant, id, true).await?.into_dto()?;
    check_version(h.version, expected)?;
    if h.status != RegistrationHoldStatus::Active {
        return Err(conflict("the hold is already released"));
    }
    let sql = format!(
        "UPDATE student.registration_hold SET status = 'released', released_by = $3, released_at = now(), \
         release_reason = $4, version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2 \
         RETURNING {HOLD_COLUMNS}"
    );
    let h2 = sqlx::query_as::<_, HoldRow>(&sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .bind(actor.id.as_deref())
        .bind(&reason)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?
        .into_dto()?;
    let payload = hold_v1(&h2, ss.subject_ref());
    emit(
        &mut tx,
        &ss.scope,
        REGISTRATION_HOLD_RELEASED,
        REGISTRATION_HOLD_RELEASED_SCHEMA,
        id,
        &payload,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &ss.scope,
        "student.registration_hold.release",
        "registration_hold",
        id,
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(h2)
}
