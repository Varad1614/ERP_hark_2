//! Completion evaluation (L3-I-145, 150, 153) and session closure / archival (L3-I-187, 192).
//!
//! Session status moves only through `shared::lifecycle::change_session_status` (6C:
//! `active | reopened → closing`, `closing → closed → archived`, `closing → reopened` on a
//! rejected closure); registrations `locked → completed` through
//! `change_registration_status`.

use super::domain::{evaluate, ClosureChecks, CompletionInput};
use crate::results::common::{ensure_version, history, status_payload};
use crate::results::repo::session_of;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::grading::{cumulative, round2, AttemptLine};
use crate::shared::guard::require_institution_writable;
use crate::shared::lifecycle::{change_registration_status, change_session_status, StatusChange};
use crate::shared::publish::{audit, event};
use crate::shared::results::load_result;
use crate::shared::rows::{decode, lock_registration, lock_session};
use crate::shared::{Actor, ExamsState};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::exams::events::results::{
    COMPLETION_EVALUATED, COMPLETION_EVALUATED_SCHEMA, EXAM_SESSION_CLOSED,
    EXAM_SESSION_CLOSED_SCHEMA,
};
use sutra_contracts::exams::ids::{ExamRegistrationId, ExamSessionId, TermResultId};
use sutra_contracts::exams::status::{
    CompletionEvaluationStatus, ExamRegistrationStatus, ExamSessionStatus,
};
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Decimal from a frozen Domain B `f64` credit value (converted once, at the port boundary).
fn credits(v: f64) -> Decimal {
    Decimal::try_from(v).map(round2).unwrap_or(Decimal::ZERO)
}

// =================================================================== completion

/// A completion evaluation (with its evidence record).
#[derive(Clone, Debug, PartialEq)]
pub struct CompletionRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub curriculum_version_id: Option<Uuid>,
    pub credits_required: Decimal,
    pub credits_earned: Decimal,
    pub components_required: i32,
    pub components_passed: i32,
    pub cgpa: Option<Decimal>,
    pub status: CompletionEvaluationStatus,
    pub status_reason: Option<String>,
    pub evidence: serde_json::Value,
    pub supersedes_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub version: i64,
}

const COMPLETION_COLUMNS: &str = "id, institution_id, student_id, student_number, display_name, \
    curriculum_version_id, credits_required, credits_earned, components_required, \
    components_passed, cgpa, status, status_reason, evidence, supersedes_id, created_at, version";

fn completion_from_row(r: &PgRow) -> Result<CompletionRow, sqlx::Error> {
    Ok(CompletionRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        student_id: r.try_get("student_id")?,
        student_number: r.try_get("student_number")?,
        display_name: r.try_get("display_name")?,
        curriculum_version_id: r.try_get("curriculum_version_id")?,
        credits_required: r.try_get("credits_required")?,
        credits_earned: r.try_get("credits_earned")?,
        components_required: r.try_get("components_required")?,
        components_passed: r.try_get("components_passed")?,
        cgpa: r.try_get("cgpa")?,
        status: decode(r.try_get("status")?, CompletionEvaluationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        evidence: r.try_get("evidence")?,
        supersedes_id: r.try_get("supersedes_id")?,
        created_at: r.try_get("created_at")?,
        version: r.try_get("version")?,
    })
}

/// One evaluation (404).
pub async fn get_completion(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
) -> Result<CompletionRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {COMPLETION_COLUMNS} FROM exams.completion_evaluation WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "completion evaluation",
    })?;
    completion_from_row(&row).map_err(map_db_error)
}

/// Evaluations of an institution (optionally one student; `history` includes superseded).
pub async fn list_completions(
    st: &ExamsState,
    tenant: TenantId,
    institution: InstitutionId,
    student: Option<Uuid>,
    include_superseded: bool,
) -> Result<Vec<CompletionRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {COMPLETION_COLUMNS} FROM exams.completion_evaluation WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::uuid IS NULL OR student_id = $3) \
         AND ($4 OR status <> 'superseded') ORDER BY created_at DESC LIMIT 500"
    ))
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(student)
    .bind(include_superseded)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(completion_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// Evaluates (or re-evaluates, superseding the current evaluation — L3-I-150) a student's
/// academic completion from every published result (one per session; each course counted once
/// with its effective attempt — DEF-I-005): credits earned vs the
/// curriculum (Domain B), every component passed, CGPA; the row is the completion evidence
/// record (L3-I-153).
pub async fn evaluate_completion(
    st: &ExamsState,
    tenant: TenantId,
    institution: InstitutionId,
    student_id: Uuid,
    actor: &Actor,
) -> Result<CompletionRow, KernelError> {
    let scope = ExecutionScope::institution(tenant, institution);
    require_institution_writable(st, &scope, institution).await?;
    let student = st
        .students
        .student(&scope, StudentId(student_id))
        .await?
        .ok_or_else(|| {
            crate::results::common::invalid("studentId", "not_found", "unknown student")
        })?;
    let context = st
        .students
        .live_context(&scope, StudentId(student_id), institution)
        .await?
        .ok_or_else(|| {
            crate::results::common::invalid(
                "studentId",
                "no_live_context",
                "the student has no live context in this institution",
            )
        })?;
    let (cv, components) = match context.curriculum_version_id {
        Some(cv) => (
            st.references.curriculum_version(&scope, cv).await?,
            st.references.curriculum_components(&scope, cv).await?,
        ),
        None => (None, Vec::new()),
    };
    let component_credits: Decimal = components.iter().map(|c| credits(c.credits)).sum();
    let required = cv
        .as_ref()
        .and_then(|v| v.min_total_credits)
        .map(credits)
        .unwrap_or(component_credits);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Every current published result (one per session — regular, supplementary, improvement),
    // first-published order; the cumulative standing counts each course once with its effective
    // attempt (grade-replacement policy, `shared::grading::cumulative`, DEF-I-005).
    let latest: Vec<Uuid> = sqlx::query_scalar(
        "SELECT r.id FROM exams.term_result r WHERE r.tenant_id = $1 \
         AND r.institution_id = $2 AND r.student_id = $3 AND r.status = 'published' \
         ORDER BY (SELECT min(f.published_at) FROM exams.term_result f \
                   WHERE f.tenant_id = r.tenant_id AND f.session_id = r.session_id \
                   AND f.student_id = r.student_id) NULLS LAST, r.session_id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(student_id)
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let blocked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM exams.term_result WHERE tenant_id = $1 \
         AND institution_id = $2 AND student_id = $3 AND status IN ('withheld', 'withdrawn'))",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(student_id)
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut attempts = Vec::new();
    let mut evidence_terms = Vec::new();
    for id in &latest {
        let r = load_result(&mut tx, tenant, TermResultId(*id), false).await?;
        attempts.extend(r.lines.iter().map(|l| AttemptLine {
            component: l.curriculum_component_id,
            credits: l.credits,
            grade_point: l.grade_point,
            passed: l.passed,
        }));
        evidence_terms.push(json!({
            "resultId": r.id, "sessionId": r.session_id, "academicTermId": r.academic_term_id,
            "revision": r.revision, "sgpa": r.sgpa.map(|v| v.to_string()),
            "creditsEarned": r.credits_earned.to_string(),
        }));
    }
    let standing = cumulative(&attempts);
    let input = CompletionInput {
        has_curriculum: context.curriculum_version_id.is_some(),
        credits_required: required,
        components_required: components.iter().map(|c| c.id).collect(),
        passed_components: standing.passed_components.clone(),
        credits_earned: standing.credits_earned,
        blocked,
    };
    let outcome = evaluate(&input);
    let overall = standing.cgpa;
    let previous: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.completion_evaluation WHERE tenant_id = $1 AND institution_id = $2 \
         AND student_id = $3 AND status <> 'superseded' FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(student_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some(prev) = previous {
        sqlx::query(
            "UPDATE exams.completion_evaluation SET status = 'superseded', version = version + 1, \
             updated_at = now() WHERE id = $1",
        )
        .bind(prev)
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            tenant,
            institution,
            "completion_evaluation",
            prev,
            None,
            "superseded",
            COMPLETION_EVALUATED,
            Some("re-evaluated"),
            None,
            actor,
        )
        .await?;
    }
    let evidence = json!({
        "terms": evidence_terms,
        "missingComponents": outcome.missing_components,
        "creditsRequired": required.to_string(),
        "creditsEarned": input.credits_earned.to_string(),
        "cgpa": overall.map(|v| v.to_string()),
    });
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.completion_evaluation (tenant_id, institution_id, student_id, \
         student_context_id, student_number, display_name, curriculum_version_id, \
         credits_required, credits_earned, components_required, components_passed, cgpa, status, \
         status_reason, evidence, supersedes_id, evaluated_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
         RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .bind(student_id)
    .bind(context.id.as_uuid())
    .bind(&student.student_number)
    .bind(&student.display_name)
    .bind(context.curriculum_version_id)
    .bind(required)
    .bind(input.credits_earned)
    .bind(i32::try_from(input.components_required.len()).unwrap_or(i32::MAX))
    .bind(i32::try_from(outcome.components_passed).unwrap_or(i32::MAX))
    .bind(overall)
    .bind(outcome.status.as_str())
    .bind(outcome.reason.as_deref())
    .bind(&evidence)
    .bind(previous)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        tenant,
        institution,
        "completion_evaluation",
        id,
        None,
        outcome.status.as_str(),
        COMPLETION_EVALUATED,
        outcome.reason.as_deref(),
        None,
        actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        COMPLETION_EVALUATED,
        COMPLETION_EVALUATED_SCHEMA,
        id.to_string(),
        &status_payload(
            id,
            institution,
            None,
            None,
            Some(student_id),
            None,
            outcome.status.as_str(),
            1,
            outcome.reason.clone(),
            None,
            student.subject_ref.clone(),
        ),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.completion_evaluation.evaluate",
        "completion_evaluation",
        id.to_string(),
        json!({"studentId": student_id, "status": outcome.status.as_str(), "supersedes": previous}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_completion(st, tenant, id).await
}

// =================================================================== closure

/// A session closure.
#[derive(Clone, Debug, PartialEq)]
pub struct ClosureRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub status: String,
    pub status_reason: Option<String>,
    pub checks: serde_json::Value,
    pub completed_registrations: i32,
    pub process_instance_id: Option<String>,
    pub closed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
}

const CLOSURE_COLUMNS: &str = "id, institution_id, session_id, status, status_reason, checks, \
    completed_registrations, process_instance_id, closed_at, archived_at, version";

fn closure_from_row(r: &PgRow) -> Result<ClosureRow, sqlx::Error> {
    Ok(ClosureRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        status: r.try_get("status")?,
        status_reason: r.try_get("status_reason")?,
        checks: r.try_get("checks")?,
        completed_registrations: r.try_get("completed_registrations")?,
        process_instance_id: r.try_get("process_instance_id")?,
        closed_at: r.try_get("closed_at")?,
        archived_at: r.try_get("archived_at")?,
        version: r.try_get("version")?,
    })
}

async fn load_closure<'c, E>(
    ex: E,
    tenant: TenantId,
    session: ExamSessionId,
    lock: bool,
) -> Result<Option<ClosureRow>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    sqlx::query(&format!(
        "SELECT {CLOSURE_COLUMNS} FROM exams.session_closure WHERE session_id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(session.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(ex)
    .await
    .map_err(map_db_error)?
    .map(|r| closure_from_row(&r))
    .transpose()
    .map_err(map_db_error)
}

/// The closure of a session (404 when never initiated).
pub async fn get_closure(
    st: &ExamsState,
    tenant: TenantId,
    session: ExamSessionId,
) -> Result<ClosureRow, KernelError> {
    session_of(st, tenant, session).await?;
    load_closure(&st.pool, tenant, session, false)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "session closure",
        })
}

/// Current closure blockers of a session.
pub async fn closure_checks(
    conn: &mut PgConnection,
    tenant: TenantId,
    session: ExamSessionId,
) -> Result<ClosureChecks, KernelError> {
    let (unpublished, reviews, publications): (i64, i64, i64) = sqlx::query_as(
        "SELECT \
           (SELECT count(*) FROM exams.term_result WHERE tenant_id = $1 AND session_id = $2 \
              AND status NOT IN ('published', 'withdrawn', 'superseded', 'archived')), \
           (SELECT count(*) FROM exams.revaluation_request WHERE tenant_id = $1 AND session_id = $2 \
              AND status NOT IN ('withdrawn', 'rejected', 'closed', 'archived')), \
           (SELECT count(*) FROM exams.publication WHERE tenant_id = $1 AND session_id = $2 \
              AND status IN ('planned', 'scheduled', 'authorised', 'ready', 'delayed', 'failed'))",
    )
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(ClosureChecks {
        unpublished_results: unpublished,
        live_reviews: reviews,
        open_publications: publications,
    })
}

fn checks_json(c: &ClosureChecks) -> serde_json::Value {
    json!({
        "unpublishedResults": c.unpublished_results,
        "liveReviews": c.live_reviews,
        "openPublications": c.open_publications,
        "ready": c.ready(),
        "blockers": c.blockers(),
    })
}

/// Starts closing an operational session (`active | reopened → closing`).
pub async fn initiate_closure(
    st: &ExamsState,
    tenant: TenantId,
    session_id: ExamSessionId,
    actor: &Actor,
) -> Result<ClosureRow, KernelError> {
    let s = session_of(st, tenant, session_id).await?;
    let scope = ExecutionScope::institution(tenant, s.institution_id);
    require_institution_writable(st, &scope, s.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let s = lock_session(&mut tx, tenant, session_id).await?;
    if !s.status.is_operational() {
        return Err(KernelError::Conflict(format!(
            "examination session {} is {}; only an active session is closed",
            s.code, s.status
        )));
    }
    change_session_status(
        &mut tx,
        tenant,
        &s,
        ExamSessionStatus::Closing,
        &StatusChange {
            cause_event_type: EXAM_SESSION_CLOSED,
            reason: Some("closure initiated"),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let checks = closure_checks(&mut tx, tenant, session_id).await?;
    let existing = load_closure(&mut *tx, tenant, session_id, true).await?;
    let id = match existing {
        Some(c) => {
            sqlx::query(
                "UPDATE exams.session_closure SET status = 'closing', status_reason = NULL, \
                 checks = $3, process_instance_id = NULL, initiated_by = $4, \
                 version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
            )
            .bind(c.id)
            .bind(tenant.as_uuid())
            .bind(checks_json(&checks))
            .bind(actor.id.as_deref())
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
            c.id
        }
        None => sqlx::query_scalar(
            "INSERT INTO exams.session_closure (tenant_id, institution_id, session_id, checks, \
             initiated_by) VALUES ($1, $2, $3, $4, $5) RETURNING id",
        )
        .bind(tenant.as_uuid())
        .bind(s.institution_id.as_uuid())
        .bind(session_id.as_uuid())
        .bind(checks_json(&checks))
        .bind(actor.id.as_deref())
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?,
    };
    history(
        &mut tx,
        tenant,
        s.institution_id,
        "session_closure",
        id,
        None,
        "closing",
        EXAM_SESSION_CLOSED,
        None,
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.session_closure.initiate",
        "session_closure",
        id.to_string(),
        checks_json(&checks),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_closure(st, tenant, session_id).await
}

async fn apply_close(
    conn: &mut PgConnection,
    tenant: TenantId,
    c: &ClosureRow,
    pid: &str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let s = lock_session(conn, tenant, c.session_id).await?;
    let closed = change_session_status(
        conn,
        tenant,
        &s,
        ExamSessionStatus::Closed,
        &StatusChange {
            cause_event_type: EXAM_SESSION_CLOSED,
            reason,
            process_instance_id: Some(pid),
        },
        actor,
    )
    .await?;
    let locked: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.exam_registration WHERE tenant_id = $1 AND session_id = $2 \
         AND status = 'locked' ORDER BY registration_number",
    )
    .bind(tenant.as_uuid())
    .bind(c.session_id.as_uuid())
    .fetch_all(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let change = StatusChange {
        cause_event_type: EXAM_SESSION_CLOSED,
        reason: Some("session closed"),
        process_instance_id: Some(pid),
    };
    for id in &locked {
        let reg = lock_registration(conn, tenant, ExamRegistrationId(*id)).await?;
        change_registration_status(
            conn,
            tenant,
            &reg,
            ExamRegistrationStatus::Completed,
            &change,
            actor,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE exams.session_closure SET status = 'closed', status_reason = $3, \
         completed_registrations = $4, closed_at = now(), process_instance_id = $5, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(c.id)
    .bind(tenant.as_uuid())
    .bind(reason)
    .bind(i32::try_from(locked.len()).unwrap_or(i32::MAX))
    .bind(pid)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    history(
        conn,
        tenant,
        c.institution_id,
        "session_closure",
        c.id,
        Some("closing"),
        "closed",
        EXAM_SESSION_CLOSED,
        reason,
        Some(pid),
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    event(
        conn,
        &scope,
        EXAM_SESSION_CLOSED,
        EXAM_SESSION_CLOSED_SCHEMA,
        c.session_id.to_string(),
        &status_payload(
            c.session_id.as_uuid(),
            c.institution_id,
            Some(c.session_id.as_uuid()),
            None,
            None,
            Some("closing"),
            "closed",
            closed.version,
            reason.map(str::to_owned),
            Some(pid.to_owned()),
            None,
        ),
        actor,
    )
    .await?;
    audit(
        conn,
        &scope,
        "exams.session_closure.close",
        "session_closure",
        c.id.to_string(),
        json!({"completedRegistrations": locked.len(), "processInstanceId": pid}),
        actor,
    )
    .await?;
    Ok(())
}

/// Closes a closing session once nothing blocks it (409 with the blockers otherwise):
/// `SESSION_CLOSURE` approval; auto-approval closes now (session `closed`, locked registrations
/// `completed`, `exam_session.closed`).
pub async fn close_session(
    st: &ExamsState,
    tenant: TenantId,
    session_id: ExamSessionId,
    expected: i64,
    actor: &Actor,
) -> Result<ClosureRow, KernelError> {
    let c = get_closure(st, tenant, session_id).await?;
    ensure_version(expected, c.version)?;
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    require_institution_writable(st, &scope, c.institution_id).await?;
    if c.status != "closing" || c.process_instance_id.is_some() {
        return Err(KernelError::Conflict(format!(
            "session closure is {}{}",
            c.status,
            if c.process_instance_id.is_some() {
                " and awaits its decision"
            } else {
                ""
            }
        )));
    }
    {
        let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
        let checks = closure_checks(&mut conn, tenant, session_id).await?;
        if !checks.ready() {
            return Err(KernelError::Conflict(format!(
                "the session cannot close yet: {}",
                checks.blockers().join(", ")
            )));
        }
    }
    let outcome = request_approval(
        st,
        &scope,
        process::SESSION_CLOSURE,
        business_key("session_closure", c.id, c.version),
        BTreeMap::from([("sessionId".to_owned(), json!(session_id))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_closure(&mut *tx, tenant, session_id, true)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "session closure",
        })?;
    ensure_version(expected, c.version)?;
    let checks = closure_checks(&mut tx, tenant, session_id).await?;
    sqlx::query(
        "UPDATE exams.session_closure SET checks = $3, process_instance_id = $4, \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(c.id)
    .bind(tenant.as_uuid())
    .bind(checks_json(&checks))
    .bind(&pid)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        apply_close(
            &mut tx,
            tenant,
            &c,
            &pid,
            Some("auto-approved"),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get_closure(st, tenant, session_id).await
}

/// Explicit closure decision (`exam.approve`): approve closes; reject reopens the session
/// (`closing → reopened`).
pub async fn decide_closure(
    st: &ExamsState,
    tenant: TenantId,
    session_id: ExamSessionId,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<ClosureRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_closure(&mut *tx, tenant, session_id, true)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "session closure",
        })?;
    ensure_version(expected, c.version)?;
    let Some(pid) = c
        .process_instance_id
        .clone()
        .filter(|_| c.status == "closing")
    else {
        return Err(KernelError::Conflict(format!(
            "session closure is {}; no decision is pending",
            c.status
        )));
    };
    if approve {
        apply_close(&mut tx, tenant, &c, &pid, reason.as_deref(), actor).await?;
    } else {
        let s = lock_session(&mut tx, tenant, session_id).await?;
        change_session_status(
            &mut tx,
            tenant,
            &s,
            ExamSessionStatus::Reopened,
            &StatusChange {
                cause_event_type: EXAM_SESSION_CLOSED,
                reason: reason.as_deref(),
                process_instance_id: Some(&pid),
            },
            actor,
        )
        .await?;
        sqlx::query(
            "UPDATE exams.session_closure SET status = 'reopened', status_reason = $3, \
             version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
        )
        .bind(c.id)
        .bind(tenant.as_uuid())
        .bind(reason.as_deref())
        .execute(&mut *tx)
        .await
        .map_err(map_db_error)?;
        history(
            &mut tx,
            tenant,
            c.institution_id,
            "session_closure",
            c.id,
            Some("closing"),
            "reopened",
            EXAM_SESSION_CLOSED,
            reason.as_deref(),
            Some(&pid),
            actor,
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    get_closure(st, tenant, session_id).await
}

/// Archives a closed session (L3-I-192): session `closed → archived`, closure `archived`.
pub async fn archive_session(
    st: &ExamsState,
    tenant: TenantId,
    session_id: ExamSessionId,
    expected: i64,
    actor: &Actor,
) -> Result<ClosureRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = load_closure(&mut *tx, tenant, session_id, true)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "session closure",
        })?;
    ensure_version(expected, c.version)?;
    if c.status != "closed" {
        return Err(KernelError::Conflict(format!(
            "session closure is {}; only a closed session is archived",
            c.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, c.institution_id);
    let s = lock_session(&mut tx, tenant, session_id).await?;
    change_session_status(
        &mut tx,
        tenant,
        &s,
        ExamSessionStatus::Archived,
        &StatusChange {
            cause_event_type: EXAM_SESSION_CLOSED,
            reason: Some("archived"),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE exams.session_closure SET status = 'archived', archived_at = now(), \
         version = version + 1, updated_at = now() WHERE id = $1 AND tenant_id = $2",
    )
    .bind(c.id)
    .bind(tenant.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        tenant,
        c.institution_id,
        "session_closure",
        c.id,
        Some("closed"),
        "archived",
        EXAM_SESSION_CLOSED,
        None,
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.session_closure.archive",
        "session_closure",
        c.id.to_string(),
        json!({}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    get_closure(st, tenant, session_id).await
}
