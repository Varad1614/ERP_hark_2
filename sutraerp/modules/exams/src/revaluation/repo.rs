//! Revaluation / rechecking requests (L3-I-118 … 121, 128 … 130, 132) and supplementary
//! eligibility (L3-I-133, 134, 138, 139, 142).
//!
//! Fees go through Domain F's `ExamFeePort` (fee code `EXAM-REVAL` / `EXAM-RECHECK`,
//! `source_id` = request id, idempotent) and settlement is read through `ChargeDirectory`
//! (`outstanding_amount == 0`). Revised marks go through `shared::marks::revise_final`, the
//! result through `results::repo::revise_result` (new revision, `term_result.revised`).

use super::domain::{
    can_transition, eligibility_can_transition, eligibility_status, fee_of, offers_attempts,
    withdrawable, within_window, DEFAULT_MAX_ATTEMPTS,
};
use crate::results::common::{ensure_version, history, status_payload};
use crate::results::repo::{current_result, revise_result, session_of};
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::require_institution_writable;
use crate::shared::marks::{lock_mark, revise_final, MarkValues};
use crate::shared::numbering::{self, kind};
use crate::shared::publish::{audit, event};
use crate::shared::results::load_result;
use crate::shared::rows::decode;
use crate::shared::time::business_today;
use crate::shared::{Actor, ExamsState};
use rust_decimal::Decimal;
use serde_json::json;
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use sutra_contracts::exams::events::results::{
    RESULT_REVISED, REVALUATION_COMPLETED, REVALUATION_COMPLETED_SCHEMA, REVALUATION_DECIDED,
    REVALUATION_DECIDED_SCHEMA, REVALUATION_REQUESTED, REVALUATION_REQUESTED_SCHEMA,
    SUPPLEMENTARY_ELIGIBILITY_DETERMINED, SUPPLEMENTARY_ELIGIBILITY_DETERMINED_SCHEMA,
};
use sutra_contracts::exams::ids::{ExamPaperId, ExamSessionId, PaperMarkId, TermResultId};
use sutra_contracts::exams::ports::CandidateSummary;
use sutra_contracts::exams::status::{
    ResultStatus, RevaluationStatus, ReviewKind, SupplementaryEligibilityStatus,
};
use sutra_contracts::finance::ids::ChargeId;
use sutra_contracts::finance::ports::ExamFeeRequest;
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// A review request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevaluationRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub registration_id: Uuid,
    pub paper_id: Uuid,
    pub paper_mark_id: Uuid,
    pub result_id: Uuid,
    pub student_id: Uuid,
    pub subject_ref: Option<String>,
    pub student_number: String,
    pub display_name: String,
    pub paper_code: String,
    pub request_number: String,
    pub kind: ReviewKind,
    pub status: RevaluationStatus,
    pub status_reason: Option<String>,
    pub reason: Option<String>,
    pub fee_amount: Decimal,
    pub fee_charge_id: Option<Uuid>,
    pub fee_charge_number: Option<String>,
    pub from_marks: Option<Decimal>,
    pub to_marks: Option<Decimal>,
    pub review_notes: Option<String>,
    pub revised_result_id: Option<Uuid>,
    pub process_instance_id: Option<String>,
    pub version: i64,
}

const COLUMNS: &str = "id, institution_id, session_id, registration_id, paper_id, paper_mark_id, \
    result_id, student_id, subject_ref, student_number, display_name, paper_code, request_number, \
    kind, status, status_reason, reason, fee_amount, fee_charge_id, fee_charge_number, from_marks, \
    to_marks, review_notes, revised_result_id, process_instance_id, version";

fn from_row(r: &PgRow) -> Result<RevaluationRow, sqlx::Error> {
    Ok(RevaluationRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        registration_id: r.try_get("registration_id")?,
        paper_id: r.try_get("paper_id")?,
        paper_mark_id: r.try_get("paper_mark_id")?,
        result_id: r.try_get("result_id")?,
        student_id: r.try_get("student_id")?,
        subject_ref: r.try_get("subject_ref")?,
        student_number: r.try_get("student_number")?,
        display_name: r.try_get("display_name")?,
        paper_code: r.try_get("paper_code")?,
        request_number: r.try_get("request_number")?,
        kind: decode(r.try_get("kind")?, ReviewKind::parse)?,
        status: decode(r.try_get("status")?, RevaluationStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        reason: r.try_get("reason")?,
        fee_amount: r.try_get("fee_amount")?,
        fee_charge_id: r.try_get("fee_charge_id")?,
        fee_charge_number: r.try_get("fee_charge_number")?,
        from_marks: r.try_get("from_marks")?,
        to_marks: r.try_get("to_marks")?,
        review_notes: r.try_get("review_notes")?,
        revised_result_id: r.try_get("revised_result_id")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: r.try_get("version")?,
    })
}

fn not_found() -> KernelError {
    KernelError::NotFound {
        entity: "revaluation request",
    }
}

async fn load<'c, E>(
    ex: E,
    tenant: TenantId,
    id: Uuid,
    lock: bool,
) -> Result<RevaluationRow, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    let row = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM exams.revaluation_request WHERE id = $1 AND tenant_id = $2{}",
        if lock { " FOR UPDATE" } else { "" }
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(ex)
    .await
    .map_err(map_db_error)?
    .ok_or_else(not_found)?;
    from_row(&row).map_err(map_db_error)
}

/// One request (404).
pub async fn get(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
) -> Result<RevaluationRow, KernelError> {
    load(&st.pool, tenant, id, false).await
}

/// Requests of a session (optionally one status).
pub async fn list_of_session(
    st: &ExamsState,
    tenant: TenantId,
    session: ExamSessionId,
    status: Option<RevaluationStatus>,
) -> Result<Vec<RevaluationRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM exams.revaluation_request WHERE tenant_id = $1 AND session_id = $2 \
         AND ($3::text IS NULL OR status = $3) ORDER BY created_at DESC LIMIT 500"
    ))
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// Requests of a set of registrations (self-service).
pub async fn list_of_registrations(
    st: &ExamsState,
    tenant: TenantId,
    registrations: &[Uuid],
) -> Result<Vec<RevaluationRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {COLUMNS} FROM exams.revaluation_request WHERE tenant_id = $1 \
         AND registration_id = ANY($2) ORDER BY created_at DESC"
    ))
    .bind(tenant.as_uuid())
    .bind(registrations)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// Whether the fee charge is settled, in the caller's tenant.
pub async fn fee_settled_in(
    st: &ExamsState,
    tenant: TenantId,
    r: &RevaluationRow,
) -> Result<bool, KernelError> {
    let Some(id) = r.fee_charge_id else {
        return Ok(false);
    };
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    Ok(st
        .charges
        .charge(&scope, ChargeId(id))
        .await?
        .is_some_and(|c| c.outstanding_amount <= Decimal::ZERO))
}

#[allow(clippy::too_many_arguments)]
async fn set_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &RevaluationRow,
    to: RevaluationStatus,
    reason: Option<&str>,
    pid: Option<&str>,
    cause: &'static str,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    if !can_transition(r.status, to) {
        return Err(KernelError::Conflict(format!(
            "{} {} is {}; it cannot become {to}",
            r.kind, r.request_number, r.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.revaluation_request SET status = $3, status_reason = COALESCE($4, status_reason), \
         process_instance_id = COALESCE($5, process_instance_id), version = version + 1, \
         updated_at = now() WHERE id = $1 AND tenant_id = $2 AND version = $6 RETURNING {COLUMNS}"
    ))
    .bind(r.id)
    .bind(tenant.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .bind(pid)
    .bind(r.version)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or_else(|| KernelError::Conflict("revaluation request changed concurrently".into()))?;
    history(
        conn,
        tenant,
        r.institution_id,
        "revaluation_request",
        r.id,
        Some(r.status.as_str()),
        to.as_str(),
        cause,
        reason,
        pid,
        actor,
    )
    .await?;
    from_row(&row).map_err(map_db_error)
}

fn payload(
    r: &RevaluationRow,
    from: Option<RevaluationStatus>,
    reason: Option<String>,
) -> sutra_contracts::exams::events::StatusChangedV1 {
    let mut p = status_payload(
        r.id,
        r.institution_id,
        Some(r.session_id.as_uuid()),
        Some(r.registration_id),
        Some(r.student_id),
        from.map(|f| f.as_str()),
        r.status.as_str(),
        r.version,
        reason,
        r.process_instance_id.clone(),
        r.subject_ref.clone(),
    );
    p.paper_id = Some(r.paper_id);
    p
}

/// Submits a review of one of the caller's own published result lines (L3-I-118/119):
/// eligibility (published, within the window, one live review per paper — duplicates and
/// conflicting kinds are 409, L3-I-128/129/130), then the fee is raised in Finance and the
/// request waits in `fee_pending`.
pub async fn submit(
    st: &ExamsState,
    tenant: TenantId,
    mine: &[CandidateSummary],
    paper_id: ExamPaperId,
    review: ReviewKind,
    reason: Option<String>,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let mut found = None;
    for reg in mine.iter().filter(|r| r.paper(paper_id).is_some()) {
        if let Some(cur) = current_result(&mut conn, tenant, reg.session_id, reg.student_id).await?
        {
            if cur.status == ResultStatus::Published {
                let line = cur.lines.iter().find(|l| l.paper_id == paper_id).cloned();
                if let Some(line) = line {
                    found = Some((reg.clone(), cur, line));
                    break;
                }
            }
        }
    }
    drop(conn);
    let (reg, result, line) = found.ok_or(KernelError::NotFound {
        entity: "published result line",
    })?;
    let scope = ExecutionScope::institution(tenant, reg.institution_id);
    require_institution_writable(st, &scope, reg.institution_id).await?;
    let today = business_today(st, &scope, reg.institution_id).await?;
    let published_on = result.published_at.map(|p| p.date_naive()).unwrap_or(today);
    if !within_window(published_on, today) {
        return Err(KernelError::Conflict(
            "the review window for this result has closed".into(),
        ));
    }
    let (fee_code, amount) = fee_of(review);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let live: Option<(Uuid, String)> = sqlx::query_as(
        "SELECT id, kind FROM exams.revaluation_request WHERE tenant_id = $1 \
         AND registration_id = $2 AND paper_id = $3 \
         AND status NOT IN ('withdrawn', 'rejected', 'closed', 'archived') FOR UPDATE",
    )
    .bind(tenant.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(paper_id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if let Some((_, k)) = live {
        return Err(KernelError::Conflict(if k == review.as_str() {
            format!("a {review} of this paper is already in progress")
        } else {
            format!("a {k} of this paper is in progress; a conflicting {review} is not accepted")
        }));
    }
    let mark: (Uuid, Option<Decimal>) = sqlx::query_as(
        "SELECT id, external_marks FROM exams.paper_mark WHERE tenant_id = $1 \
         AND registration_id = $2 AND paper_id = $3",
    )
    .bind(tenant.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(paper_id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound { entity: "marks" })?;
    let number = numbering::next(&mut tx, tenant, reg.institution_id, kind::REVALUATION).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.revaluation_request (tenant_id, institution_id, session_id, \
         registration_id, paper_id, paper_mark_id, result_id, student_id, subject_ref, \
         student_number, display_name, paper_code, request_number, kind, reason, fee_amount, \
         from_marks, requested_by) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, \
         $13, $14, $15, $16, $17, $18) RETURNING id",
    )
    .bind(tenant.as_uuid())
    .bind(reg.institution_id.as_uuid())
    .bind(reg.session_id.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(paper_id.as_uuid())
    .bind(mark.0)
    .bind(result.id.as_uuid())
    .bind(reg.student_id.as_uuid())
    .bind(reg.subject_ref.as_deref())
    .bind(&reg.student_number)
    .bind(&reg.display_name)
    .bind(&line.paper_code)
    .bind(&number)
    .bind(review.as_str())
    .bind(reason.as_deref())
    .bind(amount)
    .bind(mark.1)
    .bind(actor.id.as_deref())
    .fetch_one(&mut *tx)
    .await
    .map_err(|e| match map_db_error(e) {
        KernelError::Conflict(_) => {
            KernelError::Conflict("a review of this paper is already in progress".into())
        }
        other => other,
    })?;
    history(
        &mut tx,
        tenant,
        reg.institution_id,
        "revaluation_request",
        id,
        None,
        "submitted",
        REVALUATION_REQUESTED,
        reason.as_deref(),
        None,
        actor,
    )
    .await?;
    let r = load(&mut *tx, tenant, id, true).await?;
    event(
        &mut tx,
        &scope,
        REVALUATION_REQUESTED,
        REVALUATION_REQUESTED_SCHEMA,
        id.to_string(),
        &payload(&r, None, reason.clone()),
        actor,
    )
    .await?;
    let r = set_status(
        &mut tx,
        tenant,
        &r,
        RevaluationStatus::EligibilityVerified,
        Some("published result within the review window"),
        None,
        REVALUATION_REQUESTED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.revaluation_request.submit",
        "revaluation_request",
        id.to_string(),
        json!({"number": number, "kind": review.as_str(), "paperId": paper_id, "feeCode": fee_code}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    raise_fee(st, tenant, &r, actor).await
}

/// Raises the review fee (idempotent on the request id) and moves the request to `fee_pending`.
/// A Finance failure leaves it `eligibility_verified`; `verify` retries.
async fn raise_fee(
    st: &ExamsState,
    tenant: TenantId,
    r: &RevaluationRow,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    let session = session_of(st, tenant, r.session_id).await?;
    let (fee_code, amount) = fee_of(r.kind);
    let charge = match st
        .fees
        .raise_exam_fee(
            &scope,
            ExamFeeRequest {
                institution_id: r.institution_id,
                student_id: r.student_id,
                fee_code: fee_code.to_owned(),
                description: format!("{} fee — {} ({})", r.kind, r.paper_code, r.request_number),
                amount,
                academic_term_id: Some(session.academic_term_id),
                due_date: None,
                source_id: r.id,
                actor: actor.id.clone(),
            },
        )
        .await
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(request = %r.id, error = %e, "exam fee could not be raised; verify retries");
            return get(st, tenant, r.id).await;
        }
    };
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut *tx, tenant, r.id, true).await?;
    if cur.status != RevaluationStatus::EligibilityVerified {
        return Ok(cur);
    }
    sqlx::query(
        "UPDATE exams.revaluation_request SET fee_charge_id = $3, fee_charge_number = $4, \
         fee_amount = $5 WHERE id = $1 AND tenant_id = $2",
    )
    .bind(r.id)
    .bind(tenant.as_uuid())
    .bind(charge.charge_id.as_uuid())
    .bind(&charge.charge_number)
    .bind(charge.amount)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = set_status(
        &mut tx,
        tenant,
        &cur,
        RevaluationStatus::FeePending,
        Some(&format!("fee charge {} raised", charge.charge_number)),
        None,
        REVALUATION_REQUESTED,
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

async fn apply_decision(
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &RevaluationRow,
    approve: bool,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let to = if approve {
        RevaluationStatus::Approved
    } else {
        RevaluationStatus::Rejected
    };
    let out = set_status(
        conn,
        tenant,
        r,
        to,
        reason,
        None,
        REVALUATION_DECIDED,
        actor,
    )
    .await?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    event(
        conn,
        &scope,
        REVALUATION_DECIDED,
        REVALUATION_DECIDED_SCHEMA,
        r.id.to_string(),
        &payload(&out, Some(r.status), reason.map(str::to_owned)),
        actor,
    )
    .await?;
    audit(
        conn,
        &scope,
        "exams.revaluation_request.decide",
        "revaluation_request",
        r.id.to_string(),
        json!({"approve": approve, "reason": reason}),
        actor,
    )
    .await?;
    Ok(out)
}

/// Office verification: raises a missing fee, then checks settlement (`fee_verified`, 409 while
/// outstanding) and requests the decision (`REVALUATION_DECISION`; auto → `approved`).
pub async fn verify(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let mut r = get(st, tenant, id).await?;
    ensure_version(expected, r.version)?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    require_institution_writable(st, &scope, r.institution_id).await?;
    if r.status == RevaluationStatus::EligibilityVerified {
        r = raise_fee(st, tenant, &r, actor).await?;
    }
    if r.status != RevaluationStatus::FeePending {
        return Err(KernelError::Conflict(format!(
            "{} {} is {}; nothing to verify",
            r.kind, r.request_number, r.status
        )));
    }
    if !fee_settled_in(st, tenant, &r).await? {
        return Err(KernelError::Conflict(format!(
            "the fee charge {} is not settled yet",
            r.fee_charge_number.as_deref().unwrap_or("-")
        )));
    }
    let outcome = request_approval(
        st,
        &scope,
        process::REVALUATION_DECISION,
        business_key("revaluation_request", r.id, r.version),
        BTreeMap::from([("kind".to_owned(), json!(r.kind.as_str()))]),
    )
    .await?;
    let pid = outcome.process_instance_id().to_owned();
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let cur = load(&mut *tx, tenant, id, true).await?;
    ensure_version(r.version, cur.version)?;
    let mut out = set_status(
        &mut tx,
        tenant,
        &cur,
        RevaluationStatus::FeeVerified,
        Some("fee settled"),
        Some(&pid),
        REVALUATION_DECIDED,
        actor,
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        out = apply_decision(
            &mut tx,
            tenant,
            &out,
            true,
            Some("auto-approved"),
            &Actor::system(actor.correlation_id.clone()),
        )
        .await?;
    }
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Explicit decision on a fee-verified request (`exam.approve`).
pub async fn decide(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    approve: bool,
    reason: Option<String>,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    if r.status != RevaluationStatus::FeeVerified {
        return Err(KernelError::Conflict(format!(
            "{} {} is {}; no decision is pending",
            r.kind, r.request_number, r.status
        )));
    }
    let out = apply_decision(&mut tx, tenant, &r, approve, reason.as_deref(), actor).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// A simple step of the review machine (`start-review`, `verify-outcome`).
pub async fn step(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    to: RevaluationStatus,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    let out = set_status(
        &mut tx,
        tenant,
        &r,
        to,
        None,
        None,
        REVALUATION_DECIDED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        &format!("exams.revaluation_request.{}", to.as_str()),
        "revaluation_request",
        r.id.to_string(),
        json!({}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Records the review outcome (`review_in_progress → review_completed`, revised external marks).
pub async fn complete(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    to_marks: Decimal,
    notes: Option<String>,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    let max: Decimal = sqlx::query_scalar(
        "SELECT external_max FROM exams.paper_mark WHERE id = $1 AND tenant_id = $2",
    )
    .bind(r.paper_mark_id)
    .bind(tenant.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    if to_marks < Decimal::ZERO || to_marks > max || to_marks.normalize().scale() > 2 {
        return Err(crate::results::common::invalid(
            "externalMarks",
            "out_of_range",
            "must be between 0 and the paper's maximum, with at most two decimals",
        ));
    }
    sqlx::query(
        "UPDATE exams.revaluation_request SET to_marks = $3, review_notes = $4 \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(r.id)
    .bind(tenant.as_uuid())
    .bind(to_marks)
    .bind(notes.as_deref())
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let out = set_status(
        &mut tx,
        tenant,
        &r,
        RevaluationStatus::ReviewCompleted,
        notes.as_deref(),
        None,
        REVALUATION_COMPLETED,
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn apply_marks(
    st: &ExamsState,
    conn: &mut PgConnection,
    tenant: TenantId,
    r: &RevaluationRow,
    to_marks: Decimal,
    cause: &'static str,
    reason: &str,
    actor: &Actor,
) -> Result<Option<Uuid>, KernelError> {
    let m = lock_mark(conn, tenant, PaperMarkId(r.paper_mark_id)).await?;
    if m.external_marks == Some(to_marks) {
        return Ok(None);
    }
    let mut v = MarkValues::of(&m);
    v.external_marks = Some(to_marks);
    v.absent = false;
    revise_final(conn, tenant, &m, v, cause, reason, actor).await?;
    let cur = current_result(conn, tenant, r.session_id, StudentId(r.student_id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "term result",
        })?;
    let revised = revise_result(
        st,
        conn,
        tenant,
        &cur,
        RESULT_REVISED,
        reason,
        r.process_instance_id.as_deref(),
        actor,
    )
    .await?;
    Ok(Some(revised.id.as_uuid()))
}

/// Approves the verified outcome: revised final marks (`revise_final`) and a result revision
/// (`term_result.revised`) when the marks changed, then closes the request (L3-I-118/119).
pub async fn approve_marks(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let pre = get(st, tenant, id).await?;
    ensure_version(expected, pre.version)?;
    let scope = ExecutionScope::institution(tenant, pre.institution_id);
    require_institution_writable(st, &scope, pre.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    if r.status != RevaluationStatus::OutcomeVerified {
        return Err(KernelError::Conflict(format!(
            "{} {} is {}; verify the outcome first",
            r.kind, r.request_number, r.status
        )));
    }
    // Audit C1: whoever raised the request, completed the review or verified its outcome never
    // approves the revised marks (fails closed when no maker is recorded).
    crate::shared::approval::ensure_independent_decider(
        &mut tx,
        tenant,
        "revaluation_request",
        r.id,
        &[
            RevaluationStatus::ReviewCompleted.as_str(),
            RevaluationStatus::OutcomeVerified.as_str(),
        ],
        &[],
        actor.id.as_deref(),
        "revaluation marks approval",
    )
    .await?;
    let to = r
        .to_marks
        .ok_or_else(|| KernelError::Conflict("the review outcome has no revised marks".into()))?;
    let changed = r.from_marks != Some(to);
    let mut out = r.clone();
    if changed {
        set_status(
            &mut tx,
            tenant,
            &r,
            RevaluationStatus::RevisedMarksApproved,
            None,
            None,
            REVALUATION_COMPLETED,
            actor,
        )
        .await?;
        let reason = format!("{} {} outcome", r.kind, r.request_number);
        let revised =
            apply_marks(st, &mut tx, tenant, &r, to, r.kind.as_str(), &reason, actor).await?;
        if let Some(res) = revised {
            sqlx::query(
                "UPDATE exams.revaluation_request SET revised_result_id = $3 WHERE id = $1 AND tenant_id = $2",
            )
            .bind(r.id)
            .bind(tenant.as_uuid())
            .bind(res)
            .execute(&mut *tx)
            .await
            .map_err(map_db_error)?;
        }
        out = load(&mut *tx, tenant, id, true).await?;
        out = set_status(
            &mut tx,
            tenant,
            &out,
            RevaluationStatus::ResultRevisionInitiated,
            None,
            None,
            REVALUATION_COMPLETED,
            actor,
        )
        .await?;
    }
    let closing_reason = if changed {
        "marks revised"
    } else {
        "marks unchanged"
    };
    out = set_status(
        &mut tx,
        tenant,
        &out,
        RevaluationStatus::Closed,
        Some(closing_reason),
        None,
        REVALUATION_COMPLETED,
        actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        REVALUATION_COMPLETED,
        REVALUATION_COMPLETED_SCHEMA,
        r.id.to_string(),
        &payload(&out, Some(r.status), Some(closing_reason.to_owned())),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.revaluation_request.approve_marks",
        "revaluation_request",
        r.id.to_string(),
        json!({"from": r.from_marks, "to": to, "changed": changed}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Correction of a closed review's outcome (L3-I-132): `closed → correction_review → closed`
/// with the corrected marks applied through `revise_final` and a new result revision.
pub async fn correct(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    to_marks: Decimal,
    reason: &str,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let pre = get(st, tenant, id).await?;
    ensure_version(expected, pre.version)?;
    let scope = ExecutionScope::institution(tenant, pre.institution_id);
    require_institution_writable(st, &scope, pre.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut *tx, tenant, id, true).await?;
    ensure_version(expected, r.version)?;
    let review = set_status(
        &mut tx,
        tenant,
        &r,
        RevaluationStatus::CorrectionReview,
        Some(reason),
        None,
        REVALUATION_COMPLETED,
        actor,
    )
    .await?;
    let revised = apply_marks(
        st,
        &mut tx,
        tenant,
        &review,
        to_marks,
        "correction",
        reason,
        actor,
    )
    .await?;
    sqlx::query(
        "UPDATE exams.revaluation_request SET to_marks = $3, \
         revised_result_id = COALESCE($4, revised_result_id) WHERE id = $1 AND tenant_id = $2",
    )
    .bind(r.id)
    .bind(tenant.as_uuid())
    .bind(to_marks)
    .bind(revised)
    .execute(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let review = load(&mut *tx, tenant, id, true).await?;
    let out = set_status(
        &mut tx,
        tenant,
        &review,
        RevaluationStatus::Closed,
        Some("outcome corrected"),
        None,
        REVALUATION_COMPLETED,
        actor,
    )
    .await?;
    event(
        &mut tx,
        &scope,
        REVALUATION_COMPLETED,
        REVALUATION_COMPLETED_SCHEMA,
        r.id.to_string(),
        &payload(&out, Some(r.status), Some(reason.to_owned())),
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.revaluation_request.correct",
        "revaluation_request",
        r.id.to_string(),
        json!({"to": to_marks, "reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

/// Withdrawal before the review starts (L3-I-120/121). `owner` restricts it to the caller's
/// own registrations (self-service); the fee charge is left to Finance (refund policy).
pub async fn withdraw(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: Option<i64>,
    owner: Option<&[Uuid]>,
    reason: Option<String>,
    actor: &Actor,
) -> Result<RevaluationRow, KernelError> {
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let r = load(&mut *tx, tenant, id, true).await?;
    if let Some(regs) = owner {
        if !regs.contains(&r.registration_id) {
            return Err(not_found());
        }
    }
    if let Some(v) = expected {
        ensure_version(v, r.version)?;
    }
    if !withdrawable(r.status) {
        return Err(KernelError::Conflict(format!(
            "{} {} is {}; it can no longer be withdrawn",
            r.kind, r.request_number, r.status
        )));
    }
    let scope = ExecutionScope::institution(tenant, r.institution_id);
    let out = set_status(
        &mut tx,
        tenant,
        &r,
        RevaluationStatus::Withdrawn,
        reason.as_deref(),
        None,
        REVALUATION_DECIDED,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.revaluation_request.withdraw",
        "revaluation_request",
        r.id.to_string(),
        json!({"reason": reason}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(out)
}

// =================================================================== supplementary eligibility

/// A subsequent-attempt eligibility.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EligibilityRow {
    pub id: Uuid,
    pub institution_id: InstitutionId,
    pub session_id: ExamSessionId,
    pub registration_id: Uuid,
    pub result_id: Uuid,
    pub paper_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub paper_code: String,
    pub attempts_used: i32,
    pub max_attempts: i32,
    pub best_percent: Option<Decimal>,
    pub best_grade: Option<String>,
    pub status: SupplementaryEligibilityStatus,
    pub status_reason: Option<String>,
    pub authorised_session_id: Option<Uuid>,
    pub version: i64,
}

const ELIGIBILITY_COLUMNS: &str = "id, institution_id, session_id, registration_id, result_id, \
    paper_id, curriculum_component_id, student_id, student_number, display_name, paper_code, \
    attempts_used, max_attempts, best_percent, best_grade, status, status_reason, \
    authorised_session_id, version";

fn eligibility_from_row(r: &PgRow) -> Result<EligibilityRow, sqlx::Error> {
    Ok(EligibilityRow {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        registration_id: r.try_get("registration_id")?,
        result_id: r.try_get("result_id")?,
        paper_id: r.try_get("paper_id")?,
        curriculum_component_id: r.try_get("curriculum_component_id")?,
        student_id: r.try_get("student_id")?,
        student_number: r.try_get("student_number")?,
        display_name: r.try_get("display_name")?,
        paper_code: r.try_get("paper_code")?,
        attempts_used: r.try_get("attempts_used")?,
        max_attempts: r.try_get("max_attempts")?,
        best_percent: r.try_get("best_percent")?,
        best_grade: r.try_get("best_grade")?,
        status: decode(r.try_get("status")?, SupplementaryEligibilityStatus::parse)?,
        status_reason: r.try_get("status_reason")?,
        authorised_session_id: r.try_get("authorised_session_id")?,
        version: r.try_get("version")?,
    })
}

/// Eligibilities of a session (optionally one status).
pub async fn eligibility_of_session(
    st: &ExamsState,
    tenant: TenantId,
    session: ExamSessionId,
    status: Option<SupplementaryEligibilityStatus>,
) -> Result<Vec<EligibilityRow>, KernelError> {
    sqlx::query(&format!(
        "SELECT {ELIGIBILITY_COLUMNS} FROM exams.supplementary_eligibility WHERE tenant_id = $1 \
         AND session_id = $2 AND ($3::text IS NULL OR status = $3) ORDER BY student_number, paper_code"
    ))
    .bind(tenant.as_uuid())
    .bind(session.as_uuid())
    .bind(status.map(|s| s.as_str()))
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(eligibility_from_row)
    .collect::<Result<_, _>>()
    .map_err(map_db_error)
}

/// Summary of an eligibility determination.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DetermineSummary {
    pub processed: i32,
    pub eligible: i32,
    pub exhausted: i32,
    pub skipped: i32,
}

/// Determines subsequent-attempt eligibility from the session's published failed lines
/// (L3-I-133): attempts used from the registered paper, best result across the student's
/// published attempts (L3-I-139), one open opportunity per component (duplicates skipped,
/// L3-I-138), exhausted when the attempt limit is reached (L3-I-142). Idempotent.
pub async fn determine(
    st: &ExamsState,
    tenant: TenantId,
    session_id: ExamSessionId,
    actor: &Actor,
) -> Result<DetermineSummary, KernelError> {
    let session = session_of(st, tenant, session_id).await?;
    let scope = ExecutionScope::institution(tenant, session.institution_id);
    require_institution_writable(st, &scope, session.institution_id).await?;
    let regs = st
        .directory
        .registrations_of_session(&scope, session_id)
        .await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    // Integrator: the attempt limit comes from 6A's examination policy (the session's policy,
    // else the institution's active one); the 6C default applies when neither exists.
    let max_attempts: i32 = sqlx::query_scalar(
        "SELECT p.max_attempts FROM exams.exam_policy p WHERE p.tenant_id = $1 \
         AND (p.id = (SELECT s.policy_id FROM exams.exam_session s \
                      WHERE s.tenant_id = $1 AND s.id = $2) \
              OR (p.institution_id = $3 AND p.status = 'active')) \
         ORDER BY (p.id = (SELECT s.policy_id FROM exams.exam_session s \
                           WHERE s.tenant_id = $1 AND s.id = $2)) IS TRUE DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(session_id.as_uuid())
    .bind(session.institution_id.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .unwrap_or(DEFAULT_MAX_ATTEMPTS);
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.term_result WHERE tenant_id = $1 AND session_id = $2 \
         AND status = 'published'",
    )
    .bind(tenant.as_uuid())
    .bind(session_id.as_uuid())
    .fetch_all(&mut *tx)
    .await
    .map_err(map_db_error)?;
    let mut sum = DetermineSummary::default();
    for rid in ids {
        let r = load_result(&mut tx, tenant, TermResultId(rid), false).await?;
        let reg = regs.iter().find(|g| g.registration_id == r.registration_id);
        for line in r.lines.iter().filter(|l| !l.passed) {
            sum.processed += 1;
            let open: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM exams.supplementary_eligibility WHERE tenant_id = $1 \
                 AND student_id = $2 AND curriculum_component_id = $3 \
                 AND status IN ('eligible', 'applied', 'authorised')",
            )
            .bind(tenant.as_uuid())
            .bind(r.student_id.as_uuid())
            .bind(line.curriculum_component_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?;
            let done: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM exams.supplementary_eligibility WHERE tenant_id = $1 \
                 AND result_id = $2 AND paper_id = $3)",
            )
            .bind(tenant.as_uuid())
            .bind(r.id.as_uuid())
            .bind(line.paper_id.as_uuid())
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            if open.is_some() || done {
                sum.skipped += 1;
                continue;
            }
            let attempts = reg
                .and_then(|g| g.paper(line.paper_id))
                .map_or(1, |p| p.attempt_no.max(1));
            let best: Option<(Option<Decimal>, Option<String>)> = sqlx::query_as(
                "SELECT l.percent, l.grade FROM exams.term_result_line l \
                 JOIN exams.term_result t ON t.id = l.result_id \
                 WHERE t.tenant_id = $1 AND t.student_id = $2 AND t.status = 'published' \
                 AND l.curriculum_component_id = $3 AND l.percent IS NOT NULL \
                 ORDER BY l.percent DESC LIMIT 1",
            )
            .bind(tenant.as_uuid())
            .bind(r.student_id.as_uuid())
            .bind(line.curriculum_component_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(map_db_error)?;
            let status = eligibility_status(attempts, max_attempts);
            let (best_percent, best_grade) = best.unwrap_or((None, None));
            let id: Uuid = sqlx::query_scalar(
                "INSERT INTO exams.supplementary_eligibility (tenant_id, institution_id, \
                 session_id, registration_id, result_id, paper_id, curriculum_component_id, \
                 student_id, student_number, display_name, paper_code, attempts_used, \
                 max_attempts, best_percent, best_grade, status, status_reason) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) \
                 RETURNING id",
            )
            .bind(tenant.as_uuid())
            .bind(session.institution_id.as_uuid())
            .bind(session_id.as_uuid())
            .bind(r.registration_id.as_uuid())
            .bind(r.id.as_uuid())
            .bind(line.paper_id.as_uuid())
            .bind(line.curriculum_component_id)
            .bind(r.student_id.as_uuid())
            .bind(&r.student_number)
            .bind(&r.display_name)
            .bind(&line.paper_code)
            .bind(attempts)
            .bind(max_attempts)
            .bind(best_percent)
            .bind(best_grade.as_deref())
            .bind(status.as_str())
            .bind(
                (status == SupplementaryEligibilityStatus::Exhausted)
                    .then_some("attempt limit reached"),
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(map_db_error)?;
            history(
                &mut tx,
                tenant,
                session.institution_id,
                "supplementary_eligibility",
                id,
                None,
                status.as_str(),
                SUPPLEMENTARY_ELIGIBILITY_DETERMINED,
                None,
                None,
                actor,
            )
            .await?;
            let mut p = status_payload(
                id,
                session.institution_id,
                Some(session_id.as_uuid()),
                Some(r.registration_id.as_uuid()),
                Some(r.student_id.as_uuid()),
                None,
                status.as_str(),
                1,
                None,
                None,
                r.subject_ref.clone(),
            );
            p.paper_id = Some(line.paper_id.as_uuid());
            event(
                &mut tx,
                &scope,
                SUPPLEMENTARY_ELIGIBILITY_DETERMINED,
                SUPPLEMENTARY_ELIGIBILITY_DETERMINED_SCHEMA,
                id.to_string(),
                &p,
                actor,
            )
            .await?;
            if status == SupplementaryEligibilityStatus::Exhausted {
                sum.exhausted += 1;
            } else {
                sum.eligible += 1;
            }
        }
    }
    audit(
        &mut tx,
        &scope,
        "exams.supplementary_eligibility.determine",
        "exam_session",
        session_id.to_string(),
        json!({"processed": sum.processed, "eligible": sum.eligible, "exhausted": sum.exhausted,
               "skipped": sum.skipped}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(sum)
}

/// Authorises the attempt in a supplementary / improvement session (L3-I-134 hand-off: the
/// registration itself is 6A's `insert_registration` with the attempt kind).
pub async fn authorise(
    st: &ExamsState,
    tenant: TenantId,
    id: Uuid,
    expected: i64,
    target_session: ExamSessionId,
    actor: &Actor,
) -> Result<EligibilityRow, KernelError> {
    let target = session_of(st, tenant, target_session).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let row = sqlx::query(&format!(
        "SELECT {ELIGIBILITY_COLUMNS} FROM exams.supplementary_eligibility WHERE id = $1 \
         AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(&mut *tx)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "supplementary eligibility",
    })?;
    let e = eligibility_from_row(&row).map_err(map_db_error)?;
    ensure_version(expected, e.version)?;
    let scope = ExecutionScope::institution(tenant, e.institution_id);
    require_institution_writable(st, &scope, e.institution_id).await?;
    if target.institution_id != e.institution_id
        || !offers_attempts(target.kind)
        || target.status.is_terminal()
    {
        return Err(crate::results::common::invalid(
            "supplementarySessionId",
            "not_a_supplementary_session",
            "must be an open supplementary / improvement / makeup session of the institution",
        ));
    }
    if !eligibility_can_transition(e.status, SupplementaryEligibilityStatus::Authorised) {
        return Err(KernelError::Conflict(format!(
            "eligibility is {}; it cannot be authorised",
            e.status
        )));
    }
    let row = sqlx::query(&format!(
        "UPDATE exams.supplementary_eligibility SET status = 'authorised', \
         authorised_session_id = $3, version = version + 1, updated_at = now() \
         WHERE id = $1 AND tenant_id = $2 RETURNING {ELIGIBILITY_COLUMNS}"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .bind(target_session.as_uuid())
    .fetch_one(&mut *tx)
    .await
    .map_err(map_db_error)?;
    history(
        &mut tx,
        tenant,
        e.institution_id,
        "supplementary_eligibility",
        id,
        Some(e.status.as_str()),
        "authorised",
        SUPPLEMENTARY_ELIGIBILITY_DETERMINED,
        None,
        None,
        actor,
    )
    .await?;
    audit(
        &mut tx,
        &scope,
        "exams.supplementary_eligibility.authorise",
        "supplementary_eligibility",
        id.to_string(),
        json!({"sessionId": target_session}),
        actor,
    )
    .await?;
    tx.commit().await.map_err(map_db_error)?;
    eligibility_from_row(&row).map_err(map_db_error)
}
