//! 6B malpractice persistence + commands (L2-I14; L3-I-162…168, 170, 171).
//!
//! One authoritative open case per candidate (registration) and paper (`MAL-` numbers; a
//! duplicate report is `409`, an existing duplicate is merged — L3-I-168). Committee decisions
//! (`MALPRACTICE_DECISION`) and appeal reviews (`MALPRACTICE_REVIEW`) follow the WorkflowPort
//! pattern: the proposal is stored, auto-approval applies it in the same transaction, otherwise
//! `approve_proposal` decides. A debarring penalty sets the registered paper(s) `debarred`
//! through `shared::registration::set_paper_status`, remembering the previous statuses so an
//! overturned finding restores the examination outcome (L3-I-167).
//!
//! 6C reads decided cases **read-only**: `session_id, registration_id, paper_id, status, penalty,
//! marks_deduction` are a stable contract.

use super::domain::{
    case_can_transition, debars, is_decided, is_undecided_open, whole_registration,
};
use crate::marks::repo::{changed, inst_scope, invalid};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::grading::fmt2;
use crate::shared::guard::require_institution_writable;
use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::numbering::{self, kind};
use crate::shared::publish::{audit, event};
use crate::shared::registration::set_paper_status;
use crate::shared::rows::{decode, lock_registration};
use crate::shared::{Actor, ExamsState};
use chrono::{DateTime, Utc};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::exams::events::marks::{
    MALPRACTICE_CLOSED, MALPRACTICE_CLOSED_SCHEMA, MALPRACTICE_DECIDED, MALPRACTICE_DECIDED_SCHEMA,
    MALPRACTICE_REPORTED, MALPRACTICE_REPORTED_SCHEMA, MALPRACTICE_REVIEWED,
    MALPRACTICE_REVIEWED_SCHEMA,
};
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_contracts::exams::ids::{
    ExamPaperId, ExamRegistrationId, ExamSessionId, MalpracticeCaseId, MalpracticeDecisionId,
};
use sutra_contracts::exams::status::{
    MalpracticeCaseStatus, MalpracticePenalty, RegistrationPaperStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

fn conflict(msg: impl Into<String>) -> KernelError {
    KernelError::Conflict(msg.into())
}

fn required(field: &str, value: &str, what: &str) -> Result<String, KernelError> {
    let v = value.trim();
    if v.is_empty() {
        Err(invalid(field, "required", &format!("{what} is required")))
    } else {
        Ok(v.to_owned())
    }
}

fn check_version(expected: Option<i64>, actual: i64) -> Result<(), KernelError> {
    match expected {
        Some(e) if e != actual => Err(KernelError::VersionConflict {
            expected: e,
            actual,
        }),
        _ => Ok(()),
    }
}

// ------------------------------------------------------------------ rows

/// A malpractice case.
#[derive(Clone, Debug)]
pub struct CaseRow {
    pub id: MalpracticeCaseId,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub registration_id: ExamRegistrationId,
    pub paper_id: Option<ExamPaperId>,
    pub student_id: Uuid,
    pub case_number: String,
    pub allegation: String,
    pub evidence: Option<String>,
    pub status: MalpracticeCaseStatus,
    pub status_reason: Option<String>,
    pub penalty: Option<MalpracticePenalty>,
    pub marks_deduction: Option<Decimal>,
    pub proposal_kind: Option<String>,
    pub proposed_outcome: Option<String>,
    pub proposed_penalty: Option<MalpracticePenalty>,
    pub proposed_deduction: Option<Decimal>,
    pub proposed_reason: Option<String>,
    pub prior_paper_statuses: serde_json::Value,
    pub merged_into_id: Option<MalpracticeCaseId>,
    pub process_instance_id: Option<String>,
    pub reported_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub closed_at: Option<DateTime<Utc>>,
    pub archived_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
}

const CASE_COLUMNS: &str = "id, institution_id, session_id, registration_id, paper_id, \
    student_id, case_number, allegation, evidence, status, status_reason, penalty, \
    marks_deduction, proposal_kind, proposed_outcome, proposed_penalty, proposed_deduction, \
    proposed_reason, prior_paper_statuses, merged_into_id, process_instance_id, reported_by, \
    decided_at, closed_at, archived_at, version, created_at";

fn penalty_opt(raw: Option<String>) -> Result<Option<MalpracticePenalty>, sqlx::Error> {
    raw.map(|r| decode(r, MalpracticePenalty::parse))
        .transpose()
}

fn case_from_row(r: &PgRow) -> Result<CaseRow, sqlx::Error> {
    Ok(CaseRow {
        id: MalpracticeCaseId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        registration_id: ExamRegistrationId(r.try_get("registration_id")?),
        paper_id: r.try_get::<Option<Uuid>, _>("paper_id")?.map(ExamPaperId),
        student_id: r.try_get("student_id")?,
        case_number: r.try_get("case_number")?,
        allegation: r.try_get("allegation")?,
        evidence: r.try_get("evidence")?,
        status: decode(r.try_get("status")?, MalpracticeCaseStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        penalty: penalty_opt(r.try_get("penalty")?)?,
        marks_deduction: r.try_get("marks_deduction")?,
        proposal_kind: r.try_get("proposal_kind")?,
        proposed_outcome: r.try_get("proposed_outcome")?,
        proposed_penalty: penalty_opt(r.try_get("proposed_penalty")?)?,
        proposed_deduction: r.try_get("proposed_deduction")?,
        proposed_reason: r.try_get("proposed_reason")?,
        prior_paper_statuses: r.try_get("prior_paper_statuses")?,
        merged_into_id: r
            .try_get::<Option<Uuid>, _>("merged_into_id")?
            .map(MalpracticeCaseId),
        process_instance_id: r.try_get("process_instance_id")?,
        reported_by: r.try_get("reported_by")?,
        decided_at: r.try_get("decided_at")?,
        closed_at: r.try_get("closed_at")?,
        archived_at: r.try_get("archived_at")?,
        version: r.try_get("version")?,
        created_at: r.try_get("created_at")?,
    })
}

/// One decision / review / merge / reopen record (append-only).
#[derive(Clone, Debug)]
pub struct DecisionRow {
    pub id: MalpracticeDecisionId,
    pub kind: String,
    pub outcome: String,
    pub penalty: Option<String>,
    pub marks_deduction: Option<Decimal>,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
    pub process_instance_id: Option<String>,
    pub created_at: DateTime<Utc>,
}

fn decision_from_row(r: &PgRow) -> Result<DecisionRow, sqlx::Error> {
    Ok(DecisionRow {
        id: MalpracticeDecisionId(r.try_get("id")?),
        kind: r.try_get("kind")?,
        outcome: r.try_get("outcome")?,
        penalty: r.try_get("penalty")?,
        marks_deduction: r.try_get("marks_deduction")?,
        reason: r.try_get("reason")?,
        decided_by: r.try_get("decided_by")?,
        process_instance_id: r.try_get("process_instance_id")?,
        created_at: r.try_get("created_at")?,
    })
}

/// Decisions of a case, oldest first.
pub async fn decisions_of(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
) -> Result<Vec<DecisionRow>, KernelError> {
    sqlx::query(
        "SELECT id, kind, outcome, penalty, marks_deduction, reason, decided_by, \
         process_instance_id, created_at FROM exams.malpractice_decision \
         WHERE tenant_id = $1 AND case_id = $2 ORDER BY created_at, id",
    )
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(decision_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

/// One case (404).
pub async fn get_case(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
) -> Result<CaseRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {CASE_COLUMNS} FROM exams.malpractice_case WHERE tenant_id = $1 AND id = $2"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "malpractice case",
    })?;
    case_from_row(&row).map_err(map_db_error)
}

async fn lock_case(
    conn: &mut PgConnection,
    t: TenantId,
    id: MalpracticeCaseId,
) -> Result<CaseRow, KernelError> {
    let row = sqlx::query(&format!(
        "SELECT {CASE_COLUMNS} FROM exams.malpractice_case WHERE tenant_id = $1 AND id = $2 FOR UPDATE"
    ))
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "malpractice case",
    })?;
    case_from_row(&row).map_err(map_db_error)
}

/// Cases of a session (optionally by status), newest first.
pub async fn cases_of_session(
    st: &ExamsState,
    t: TenantId,
    session: ExamSessionId,
    status: Option<MalpracticeCaseStatus>,
) -> Result<Vec<CaseRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {CASE_COLUMNS} FROM exams.malpractice_case WHERE tenant_id = $1 AND session_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC, id DESC LIMIT 200"
    ))
    .bind(t.as_uuid())
    .bind(session.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(case_from_row)
    .collect::<Result<Vec<_>, _>>()
    .map_err(map_db_error)
}

/// Moves a locked case (legal transition) and appends its status history.
async fn move_case(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    to: MalpracticeCaseStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    if !case_can_transition(c.status, to) {
        return Err(conflict(format!(
            "malpractice case {} is {}; it cannot become {to}",
            c.case_number, c.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.malpractice_case SET status = $3, status_reason = $4, \
         closed_at = CASE WHEN $3::text = 'closed' THEN now() ELSE closed_at END, \
         archived_at = CASE WHEN $3::text = 'archived' THEN now() ELSE archived_at END, \
         version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND version = $5 RETURNING {CASE_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(c.id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(c.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| conflict("the malpractice case changed concurrently"))?;
    record_history(
        conn,
        HistoryEntry {
            tenant: t,
            institution: c.institution_id,
            aggregate_type: "malpractice_case",
            aggregate_id: c.id.as_uuid(),
            from: Some(c.status.as_str()),
            to: to.as_str(),
        },
        &StatusChange {
            cause_event_type: "",
            reason,
            process_instance_id: c.process_instance_id.as_deref(),
        },
        actor,
    )
    .await?;
    case_from_row(&row).map_err(map_db_error)
}

#[allow(clippy::too_many_arguments)]
async fn insert_decision(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    kind: &str,
    outcome: &str,
    penalty: Option<MalpracticePenalty>,
    deduction: Option<Decimal>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    sqlx::query(
        "INSERT INTO exams.malpractice_decision (tenant_id, institution_id, case_id, kind, \
         outcome, penalty, marks_deduction, reason, decided_by, process_instance_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(t.as_uuid())
    .bind(c.institution_id.as_uuid())
    .bind(c.id.as_uuid())
    .bind(kind)
    .bind(outcome)
    .bind(penalty.map(|p| p.as_str()))
    .bind(deduction)
    .bind(reason)
    .bind(actor.id.as_deref())
    .bind(c.process_instance_id.as_deref())
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(())
}

async fn case_event(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    from: MalpracticeCaseStatus,
    types: (&'static str, &'static str),
    reason: Option<String>,
    actor: &Actor,
) -> Result<(), KernelError> {
    let subject: Option<String> = sqlx::query_scalar(
        "SELECT subject_ref FROM exams.exam_registration WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(c.registration_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .flatten();
    event(
        conn,
        &inst_scope(t, c.institution_id),
        types.0,
        types.1,
        c.id.to_string(),
        &StatusChangedV1 {
            session_id: Some(c.session_id.as_uuid()),
            paper_id: c.paper_id.map(|p| p.as_uuid()),
            registration_id: Some(c.registration_id.as_uuid()),
            student_id: Some(c.student_id),
            reason,
            process_instance_id: c.process_instance_id.clone(),
            student_subject_ref: subject,
            ..changed(
                c.id.as_uuid(),
                c.institution_id,
                Some(from.as_str()),
                c.status.as_str(),
                c.version,
            )
        },
        actor,
    )
    .await
}

fn describe(p: Option<MalpracticePenalty>, deduction: Option<Decimal>) -> String {
    match (p, deduction) {
        (Some(MalpracticePenalty::MarksDeduction), Some(d)) => {
            format!("marks deduction of {}", fmt2(d))
        }
        (Some(p), _) => p.as_str().replace('_', " "),
        (None, _) => "no penalty".into(),
    }
}

/// Brings the registered papers in line with `penalty`: a debarring penalty sets the target
/// paper(s) `debarred` (remembering the previous status); papers no longer targeted are
/// restored. Returns the new prior-status map.
async fn apply_penalty(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    penalty: Option<MalpracticePenalty>,
) -> Result<serde_json::Value, KernelError> {
    let reg = lock_registration(conn, t, c.registration_id).await?;
    let mut prior: BTreeMap<String, String> =
        serde_json::from_value(c.prior_paper_statuses.clone()).unwrap_or_default();
    let targets: Vec<ExamPaperId> = match penalty {
        Some(p) if debars(p) => {
            if whole_registration(p, c.paper_id.is_some()) {
                reg.papers
                    .iter()
                    .filter(|x| x.status != RegistrationPaperStatus::Withdrawn)
                    .map(|x| x.paper_id)
                    .collect()
            } else {
                c.paper_id.into_iter().collect()
            }
        }
        _ => Vec::new(),
    };
    let reason = format!("malpractice case {}", c.case_number);
    // Restore papers that are no longer targeted.
    let restore: Vec<(String, String)> = prior
        .iter()
        .filter(|(k, _)| !targets.iter().any(|p| p.to_string() == **k))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    for (k, v) in restore {
        let (Ok(pid), Some(status)) = (Uuid::parse_str(&k), RegistrationPaperStatus::parse(&v))
        else {
            prior.remove(&k);
            continue;
        };
        set_paper_status(
            conn,
            t,
            &reg,
            ExamPaperId(pid),
            status,
            Some(format!("{reason}: finding no longer debars").as_str()),
        )
        .await?;
        prior.remove(&k);
    }
    for p in targets {
        let Some(cur) = reg.papers.iter().find(|x| x.paper_id == p) else {
            continue;
        };
        if cur.status == RegistrationPaperStatus::Debarred {
            continue;
        }
        prior
            .entry(p.to_string())
            .or_insert_with(|| cur.status.as_str().to_owned());
        set_paper_status(
            conn,
            t,
            &reg,
            p,
            RegistrationPaperStatus::Debarred,
            Some(reason.as_str()),
        )
        .await?;
    }
    serde_json::to_value(prior).map_err(KernelError::internal)
}

/// Stores the effective penalty + prior paper statuses and clears the proposal.
async fn store_penalty(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    penalty: Option<MalpracticePenalty>,
    deduction: Option<Decimal>,
    prior: &serde_json::Value,
) -> Result<CaseRow, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE exams.malpractice_case SET penalty = $3, marks_deduction = $4, \
         prior_paper_statuses = $5, proposal_kind = NULL, proposed_outcome = NULL, \
         proposed_penalty = NULL, proposed_deduction = NULL, proposed_reason = NULL, \
         decided_at = now(), updated_at = now() WHERE tenant_id = $1 AND id = $2 \
         RETURNING {CASE_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(c.id.as_uuid())
    .bind(penalty.map(|p| p.as_str()))
    .bind(deduction)
    .bind(prior)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    case_from_row(&row).map_err(map_db_error)
}

// ------------------------------------------------------------------ report

/// A new malpractice report.
#[derive(Clone, Debug)]
pub struct NewCase {
    pub registration_id: ExamRegistrationId,
    pub paper_id: Option<ExamPaperId>,
    pub allegation: String,
    pub evidence: Option<String>,
}

/// Reports suspected malpractice (L3-I-170). A second open case for the same candidate and
/// paper is a `409` naming the authoritative case (L3-I-168).
pub async fn report_case(
    st: &ExamsState,
    t: TenantId,
    session_id: ExamSessionId,
    n: NewCase,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let allegation = required("allegation", &n.allegation, "the allegation")?;
    let session = st
        .directory
        .session(&ExecutionScope::tenant(t), session_id)
        .await?
        .ok_or(KernelError::NotFound {
            entity: "examination session",
        })?;
    let scope = inst_scope(t, session.institution_id);
    require_institution_writable(st, &scope, session.institution_id).await?;
    let reg = st
        .directory
        .registration(&ExecutionScope::tenant(t), n.registration_id)
        .await?
        .filter(|r| r.session_id == session_id)
        .ok_or_else(|| {
            invalid(
                "registrationId",
                "not_found",
                "no such registration in this examination session",
            )
        })?;
    if let Some(p) = n.paper_id {
        if !reg.papers.iter().any(|x| x.paper_id == p) {
            return Err(invalid(
                "paperId",
                "not_found",
                "the paper is not on the candidate's registration",
            ));
        }
    }
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT case_number FROM exams.malpractice_case WHERE tenant_id = $1 \
         AND registration_id = $2 AND paper_id IS NOT DISTINCT FROM $3 \
         AND status NOT IN ('closed', 'archived') AND merged_into_id IS NULL",
    )
    .bind(t.as_uuid())
    .bind(n.registration_id.as_uuid())
    .bind(n.paper_id.map(|p| p.as_uuid()))
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    if let Some(num) = existing {
        return Err(conflict(format!(
            "malpractice case {num} is already open for this candidate and paper"
        )));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let number = numbering::next(&mut tx, t, session.institution_id, kind::MALPRACTICE).await?;
    let row = sqlx::query(&format!(
        "INSERT INTO exams.malpractice_case (tenant_id, institution_id, session_id, \
         registration_id, paper_id, student_id, case_number, allegation, evidence, reported_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10) RETURNING {CASE_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(session.institution_id.as_uuid())
    .bind(session_id.as_uuid())
    .bind(n.registration_id.as_uuid())
    .bind(n.paper_id.map(|p| p.as_uuid()))
    .bind(reg.student_id.as_uuid())
    .bind(&number)
    .bind(&allegation)
    .bind(n.evidence.as_deref())
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let c = case_from_row(&row).map_err(map_db_error)?;
    record_history(
        &mut tx,
        HistoryEntry {
            tenant: t,
            institution: c.institution_id,
            aggregate_type: "malpractice_case",
            aggregate_id: c.id.as_uuid(),
            from: None,
            to: c.status.as_str(),
        },
        &StatusChange {
            cause_event_type: MALPRACTICE_REPORTED,
            reason: Some("reported"),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        MALPRACTICE_REPORTED,
        MALPRACTICE_REPORTED_SCHEMA,
        c.id.to_string(),
        &StatusChangedV1 {
            session_id: Some(session_id.as_uuid()),
            paper_id: c.paper_id.map(|p| p.as_uuid()),
            registration_id: Some(c.registration_id.as_uuid()),
            student_id: Some(c.student_id),
            reason: Some(allegation.clone()),
            ..changed(
                c.id.as_uuid(),
                c.institution_id,
                None,
                "reported",
                c.version,
            )
        },
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.malpractice_case.report",
        "malpractice_case",
        c.id.to_string(),
        json!({"caseNumber": number, "registrationId": c.registration_id, "paperId": c.paper_id}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(c)
}

// ------------------------------------------------------------------ simple moves

/// Investigation (`reported → under_investigation`) or hearing (`under_investigation → hearing`).
pub async fn advance(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    to: MalpracticeCaseStatus,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_case(&mut tx, t, id).await?;
    check_version(expected, c.version)?;
    if c.proposal_kind.is_some() {
        return Err(conflict("a decision on this case is awaiting approval"));
    }
    if to == MalpracticeCaseStatus::UnderInvestigation
        && c.status != MalpracticeCaseStatus::Reported
    {
        return Err(conflict(format!(
            "malpractice case {} is {}; a decided case is reopened instead",
            c.case_number, c.status
        )));
    }
    let updated = move_case(&mut tx, t, &c, to, reason, actor).await?;
    audit(
        &mut tx,
        &inst_scope(t, c.institution_id),
        &format!("exams.malpractice_case.{}", to.as_str()),
        "malpractice_case",
        id.to_string(),
        json!({"from": c.status.as_str(), "to": to.as_str(), "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Appeal against a decision (`decided → appealed`, L3-I-163); reason required.
pub async fn appeal(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    reason: &str,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let reason = required("reason", reason, "the appeal's grounds")?;
    let c = get_case(st, t, id).await?;
    if c.status != MalpracticeCaseStatus::Decided {
        return Err(conflict(format!(
            "malpractice case {} is {}; only a decided case is appealed",
            c.case_number, c.status
        )));
    }
    advance(
        st,
        t,
        id,
        expected,
        MalpracticeCaseStatus::Appealed,
        Some(reason.as_str()),
        actor,
    )
    .await
}

// ------------------------------------------------------------------ governed decision + review

/// Applies a committee decision (penalty) — the decision half of `MALPRACTICE_DECISION`.
async fn apply_decision(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let penalty = c.proposed_penalty;
    let deduction = c.proposed_deduction;
    let reason = c.proposed_reason.clone();
    let from = c.status;
    let moved = move_case(
        conn,
        t,
        c,
        MalpracticeCaseStatus::Decided,
        reason.as_deref(),
        actor,
    )
    .await?;
    let prior = apply_penalty(conn, t, &moved, penalty).await?;
    let stored = store_penalty(conn, t, &moved, penalty, deduction, &prior).await?;
    insert_decision(
        conn,
        t,
        &stored,
        "decision",
        "decided",
        penalty,
        deduction,
        reason.as_deref(),
        actor,
    )
    .await?;
    case_event(
        conn,
        t,
        &stored,
        from,
        (MALPRACTICE_DECIDED, MALPRACTICE_DECIDED_SCHEMA),
        Some(format!(
            "Committee decision: {}",
            describe(penalty, deduction)
        )),
        actor,
    )
    .await?;
    audit(
        conn,
        &inst_scope(t, c.institution_id),
        "exams.malpractice_case.decide",
        "malpractice_case",
        c.id.to_string(),
        json!({"penalty": penalty.map(|p| p.as_str()), "marksDeduction": deduction.map(fmt2),
               "processInstanceId": stored.process_instance_id}),
        actor,
    )
    .await?;
    Ok(stored)
}

/// Applies an appeal review (uphold / modify / overturn) — the decision half of
/// `MALPRACTICE_REVIEW`. Overturning clears the penalty and restores the papers (L3-I-167).
async fn apply_review(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let outcome = c.proposed_outcome.clone().unwrap_or_default();
    let (to, penalty, deduction) = match outcome.as_str() {
        "upheld" => (MalpracticeCaseStatus::Upheld, c.penalty, c.marks_deduction),
        "modified" => (
            MalpracticeCaseStatus::Modified,
            c.proposed_penalty,
            c.proposed_deduction,
        ),
        "overturned" => (
            MalpracticeCaseStatus::Overturned,
            Some(MalpracticePenalty::None),
            None,
        ),
        other => return Err(conflict(format!("unknown review outcome `{other}`"))),
    };
    let reason = c.proposed_reason.clone();
    let from = c.status;
    let moved = move_case(conn, t, c, to, reason.as_deref(), actor).await?;
    let prior = apply_penalty(conn, t, &moved, penalty).await?;
    let stored = store_penalty(conn, t, &moved, penalty, deduction, &prior).await?;
    insert_decision(
        conn,
        t,
        &stored,
        "review",
        &outcome,
        penalty,
        deduction,
        reason.as_deref(),
        actor,
    )
    .await?;
    case_event(
        conn,
        t,
        &stored,
        from,
        (MALPRACTICE_REVIEWED, MALPRACTICE_REVIEWED_SCHEMA),
        Some(format!(
            "Appeal review: decision {outcome}; {}",
            describe(penalty, deduction)
        )),
        actor,
    )
    .await?;
    audit(
        conn,
        &inst_scope(t, c.institution_id),
        "exams.malpractice_case.review",
        "malpractice_case",
        c.id.to_string(),
        json!({"outcome": outcome, "penalty": penalty.map(|p| p.as_str())}),
        actor,
    )
    .await?;
    Ok(stored)
}

fn validate_penalty(
    penalty: MalpracticePenalty,
    deduction: Option<Decimal>,
) -> Result<Option<Decimal>, KernelError> {
    match (penalty, deduction) {
        (MalpracticePenalty::MarksDeduction, Some(d))
            if d > Decimal::ZERO && d.normalize().scale() <= 2 =>
        {
            Ok(Some(d))
        }
        (MalpracticePenalty::MarksDeduction, _) => Err(invalid(
            "marksDeduction",
            "required",
            "a marks deduction needs a positive amount with at most two decimals",
        )),
        (_, Some(_)) => Err(invalid(
            "marksDeduction",
            "not_allowed",
            "only a marks_deduction penalty carries an amount",
        )),
        (_, None) => Ok(None),
    }
}

#[allow(clippy::too_many_arguments)]
async fn store_proposal(
    conn: &mut PgConnection,
    t: TenantId,
    c: &CaseRow,
    kind: &str,
    outcome: &str,
    penalty: Option<MalpracticePenalty>,
    deduction: Option<Decimal>,
    reason: &str,
    process_instance_id: &str,
) -> Result<CaseRow, KernelError> {
    let row = sqlx::query(&format!(
        "UPDATE exams.malpractice_case SET proposal_kind = $3, proposed_outcome = $4, \
         proposed_penalty = $5, proposed_deduction = $6, proposed_reason = $7, \
         process_instance_id = $8, version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND id = $2 AND version = $9 RETURNING {CASE_COLUMNS}"
    ))
    .bind(t.as_uuid())
    .bind(c.id.as_uuid())
    .bind(kind)
    .bind(outcome)
    .bind(penalty.map(|p| p.as_str()))
    .bind(deduction)
    .bind(reason)
    .bind(process_instance_id)
    .bind(c.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| conflict("the malpractice case changed concurrently"))?;
    case_from_row(&row).map_err(map_db_error)
}

/// Committee decision with a penalty (L3-I-170) through `MALPRACTICE_DECISION`: auto-approval
/// applies it at once (`decided`, debarment of the paper(s)); otherwise it awaits
/// [`approve_proposal`].
#[allow(clippy::too_many_arguments)]
pub async fn propose_decision(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    penalty: MalpracticePenalty,
    deduction: Option<Decimal>,
    reason: &str,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let reason = required("reason", reason, "the decision's reasoning")?;
    let deduction = validate_penalty(penalty, deduction)?;
    let c = get_case(st, t, id).await?;
    check_version(expected, c.version)?;
    if !matches!(
        c.status,
        MalpracticeCaseStatus::UnderInvestigation | MalpracticeCaseStatus::Hearing
    ) {
        return Err(conflict(format!(
            "malpractice case {} is {}; a decision follows investigation or hearing",
            c.case_number, c.status
        )));
    }
    if c.proposal_kind.is_some() {
        return Err(conflict(
            "a decision on this case is already awaiting approval",
        ));
    }
    let scope = inst_scope(t, c.institution_id);
    require_institution_writable(st, &scope, c.institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("caseNumber".to_owned(), json!(c.case_number));
    vars.insert("penalty".to_owned(), json!(penalty.as_str()));
    let outcome = request_approval(
        st,
        &scope,
        process::MALPRACTICE_DECISION,
        business_key("malpractice_case", id, c.version),
        vars,
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let locked = lock_case(&mut tx, t, id).await?;
    check_version(Some(c.version), locked.version)?;
    let proposed = store_proposal(
        &mut tx,
        t,
        &locked,
        "decision",
        "decided",
        Some(penalty),
        deduction,
        &reason,
        outcome.process_instance_id(),
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.malpractice_case.propose_decision",
        "malpractice_case",
        id.to_string(),
        json!({"penalty": penalty.as_str(), "processInstanceId": outcome.process_instance_id()}),
        actor,
    )
    .await?;
    let result = match outcome {
        ApprovalOutcome::Approved { .. } => {
            apply_decision(
                &mut tx,
                t,
                &proposed,
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => proposed,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(result)
}

/// Appeal review outcome (L3-I-163…166) through `MALPRACTICE_REVIEW`: `uphold`, `modify` (new
/// penalty) or `overturn` (penalty cleared, papers restored — L3-I-167).
#[allow(clippy::too_many_arguments)]
pub async fn propose_review(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    outcome: &str,
    penalty: Option<MalpracticePenalty>,
    deduction: Option<Decimal>,
    reason: &str,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let reason = required("reason", reason, "the review's reasoning")?;
    let c = get_case(st, t, id).await?;
    check_version(expected, c.version)?;
    let (stored_outcome, penalty, deduction) = match outcome {
        "uphold" => ("upheld", None, None),
        "modify" => {
            let p = penalty.ok_or_else(|| {
                invalid(
                    "penalty",
                    "required",
                    "a modified decision needs the new penalty",
                )
            })?;
            if Some(p) == c.penalty && deduction == c.marks_deduction {
                return Err(invalid(
                    "penalty",
                    "unchanged",
                    "the modified penalty equals the current one; uphold instead",
                ));
            }
            ("modified", Some(p), validate_penalty(p, deduction)?)
        }
        "overturn" => ("overturned", None, None),
        _ => {
            return Err(invalid(
                "outcome",
                "invalid",
                "must be uphold, modify or overturn",
            ))
        }
    };
    if !matches!(
        c.status,
        MalpracticeCaseStatus::Appealed | MalpracticeCaseStatus::UnderReview
    ) {
        return Err(conflict(format!(
            "malpractice case {} is {}; only an appealed case is reviewed",
            c.case_number, c.status
        )));
    }
    if c.proposal_kind.is_some() {
        return Err(conflict("a review outcome is already awaiting approval"));
    }
    let scope = inst_scope(t, c.institution_id);
    require_institution_writable(st, &scope, c.institution_id).await?;
    let mut vars = BTreeMap::new();
    vars.insert("caseNumber".to_owned(), json!(c.case_number));
    vars.insert("outcome".to_owned(), json!(stored_outcome));
    let approval = request_approval(
        st,
        &scope,
        process::MALPRACTICE_REVIEW,
        business_key("malpractice_case", id, c.version),
        vars,
    )
    .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let mut locked = lock_case(&mut tx, t, id).await?;
    check_version(Some(c.version), locked.version)?;
    if locked.status == MalpracticeCaseStatus::Appealed {
        locked = move_case(
            &mut tx,
            t,
            &locked,
            MalpracticeCaseStatus::UnderReview,
            Some(reason.as_str()),
            actor,
        )
        .await?;
    }
    let proposed = store_proposal(
        &mut tx,
        t,
        &locked,
        "review",
        stored_outcome,
        penalty,
        deduction,
        &reason,
        approval.process_instance_id(),
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.malpractice_case.propose_review",
        "malpractice_case",
        id.to_string(),
        json!({"outcome": stored_outcome, "processInstanceId": approval.process_instance_id()}),
        actor,
    )
    .await?;
    let result = match approval {
        ApprovalOutcome::Approved { .. } => {
            apply_review(
                &mut tx,
                t,
                &proposed,
                &Actor::system(actor.correlation_id.clone()),
            )
            .await?
        }
        ApprovalOutcome::Pending { .. } => proposed,
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(result)
}

/// Explicit decision on the pending proposal (`exam.approve`): approve applies the committee
/// decision / review outcome; reject discards the proposal.
pub async fn approve_proposal(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_case(&mut tx, t, id).await?;
    check_version(expected, c.version)?;
    let Some(kind) = c.proposal_kind.clone() else {
        return Err(conflict("nothing on this case is awaiting approval"));
    };
    let updated = if approve {
        match kind.as_str() {
            "decision" => apply_decision(&mut tx, t, &c, actor).await?,
            _ => apply_review(&mut tx, t, &c, actor).await?,
        }
    } else {
        let r = reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .ok_or_else(|| invalid("reason", "required", "a rejection needs a reason"))?;
        let row = sqlx::query(&format!(
            "UPDATE exams.malpractice_case SET proposal_kind = NULL, proposed_outcome = NULL, \
             proposed_penalty = NULL, proposed_deduction = NULL, proposed_reason = NULL, \
             status_reason = $3, version = version + 1, updated_at = now() \
             WHERE tenant_id = $1 AND id = $2 RETURNING {CASE_COLUMNS}"
        ))
        .bind(t.as_uuid())
        .bind(id.as_uuid())
        .bind(r)
        .fetch_one(&mut *tx)
        .await
        .map_err(map_db_error)?;
        audit(
            &mut tx,
            &inst_scope(t, c.institution_id),
            "exams.malpractice_case.reject_proposal",
            "malpractice_case",
            id.to_string(),
            json!({"proposal": kind, "reason": r}),
            actor,
        )
        .await?;
        case_from_row(&row).map_err(map_db_error)?
    };
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

// ------------------------------------------------------------------ reopen / merge / close / archive

/// Formal reopening after an authorised review (L3-I-162): back to `under_investigation`; the
/// current penalty stays effective until a new decision replaces it.
pub async fn reopen(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    reason: &str,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let reason = required("reason", reason, "the reopening authority / reason")?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_case(&mut tx, t, id).await?;
    check_version(expected, c.version)?;
    if c.merged_into_id.is_some() {
        return Err(conflict(
            "a merged duplicate is not reopened; reopen the authoritative case",
        ));
    }
    if c.proposal_kind.is_some() {
        return Err(conflict("a decision on this case is awaiting approval"));
    }
    let updated = move_case(
        &mut tx,
        t,
        &c,
        MalpracticeCaseStatus::UnderInvestigation,
        Some(reason.as_str()),
        actor,
    )
    .await?;
    insert_decision(
        &mut tx,
        t,
        &updated,
        "reopen",
        "under_investigation",
        updated.penalty,
        updated.marks_deduction,
        Some(reason.as_str()),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &inst_scope(t, c.institution_id),
        "exams.malpractice_case.reopen",
        "malpractice_case",
        id.to_string(),
        json!({"from": c.status.as_str(), "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Merges an undecided duplicate into the authoritative case of the same candidate and paper
/// (L3-I-168): the duplicate is closed with `merged_into_id`.
pub async fn merge(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    into: MalpracticeCaseId,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    if id == into {
        return Err(invalid(
            "intoCaseId",
            "invalid",
            "a case cannot be merged into itself",
        ));
    }
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_case(&mut tx, t, id).await?;
    check_version(expected, c.version)?;
    let target = lock_case(&mut tx, t, into).await?;
    if c.merged_into_id.is_some() || !is_undecided_open(c.status) {
        return Err(conflict(format!(
            "malpractice case {} is {}; only an undecided case is merged",
            c.case_number, c.status
        )));
    }
    if target.merged_into_id.is_some()
        || matches!(
            target.status,
            MalpracticeCaseStatus::Closed | MalpracticeCaseStatus::Archived
        )
    {
        return Err(conflict(format!(
            "malpractice case {} is not an open authoritative case",
            target.case_number
        )));
    }
    if target.registration_id != c.registration_id
        || (target.paper_id.is_some() && target.paper_id != c.paper_id)
    {
        return Err(invalid(
            "intoCaseId",
            "mismatch",
            "a duplicate concerns the same candidate and the same paper (or the whole registration)",
        ));
    }
    let note = format!(
        "duplicate of {}{}",
        target.case_number,
        reason.map(|r| format!(": {r}")).unwrap_or_default()
    );
    sqlx::query(
        "UPDATE exams.malpractice_case SET merged_into_id = $3 WHERE tenant_id = $1 AND id = $2",
    )
    .bind(t.as_uuid())
    .bind(id.as_uuid())
    .bind(into.as_uuid())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let c = lock_case(&mut tx, t, id).await?;
    let updated = move_case(
        &mut tx,
        t,
        &c,
        MalpracticeCaseStatus::Closed,
        Some(note.as_str()),
        actor,
    )
    .await?;
    insert_decision(
        &mut tx,
        t,
        &updated,
        "merge",
        "merged",
        None,
        None,
        Some(note.as_str()),
        actor,
    )
    .await?;
    case_event(
        &mut tx,
        t,
        &updated,
        c.status,
        (MALPRACTICE_CLOSED, MALPRACTICE_CLOSED_SCHEMA),
        Some(note.clone()),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &inst_scope(t, c.institution_id),
        "exams.malpractice_case.merge",
        "malpractice_case",
        id.to_string(),
        json!({"intoCaseId": into, "intoCaseNumber": target.case_number}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Closes a decided / reviewed case (L3-I-171).
pub async fn close(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let c = lock_case(&mut tx, t, id).await?;
    check_version(expected, c.version)?;
    if !is_decided(c.status) || c.proposal_kind.is_some() {
        return Err(conflict(format!(
            "malpractice case {} is {}; only a decided case without a pending proposal is closed",
            c.case_number, c.status
        )));
    }
    let updated = move_case(&mut tx, t, &c, MalpracticeCaseStatus::Closed, reason, actor).await?;
    case_event(
        &mut tx,
        t,
        &updated,
        c.status,
        (MALPRACTICE_CLOSED, MALPRACTICE_CLOSED_SCHEMA),
        reason.map(str::to_owned),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &inst_scope(t, c.institution_id),
        "exams.malpractice_case.close",
        "malpractice_case",
        id.to_string(),
        json!({"reason": reason, "penalty": c.penalty.map(|p| p.as_str())}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(updated)
}

/// Archives a closed case (L3-I-171).
pub async fn archive(
    st: &ExamsState,
    t: TenantId,
    id: MalpracticeCaseId,
    expected: Option<i64>,
    actor: &Actor,
) -> Result<CaseRow, KernelError> {
    advance(
        st,
        t,
        id,
        expected,
        MalpracticeCaseStatus::Archived,
        Some("archived"),
        actor,
    )
    .await
}
