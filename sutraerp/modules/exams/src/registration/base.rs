//! Shared registration machinery of 6A: views, eligibility (3B term registration coverage,
//! attempt limits), the registration window, the one validated insert path (always through
//! `shared::registration::insert_registration`) and its status walk
//! (`pending_validation → approved → confirmed`, or `→ rejected` with the reasons, or
//! `→ pending_review` when a review is requested).

use crate::setup::common::emit;
use crate::setup::common::status_event;
use crate::setup::policy::PolicyRules;
use crate::setup::session::papers_of;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::lifecycle::{change_registration_status, StatusChange};
use crate::shared::publish::audit;
use crate::shared::registration::{insert_registration, NewRegistration, NewRegistrationPaper};
use crate::shared::rows::{attach_papers, registration_from_row, REGISTRATION_COLUMNS};
use crate::shared::time::business_today;
use crate::shared::{Actor, ExamsState};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::exams::events::setup::{
    EXAM_REGISTRATION_CONFIRMED, EXAM_REGISTRATION_CONFIRMED_SCHEMA, EXAM_REGISTRATION_REJECTED,
    EXAM_REGISTRATION_REJECTED_SCHEMA,
};
use sutra_contracts::exams::events::StatusChangedV1;
use sutra_contracts::exams::ids::{ExamPaperId, ExamRegistrationId, ExamSessionId};
use sutra_contracts::exams::ports::{CandidateSummary, ExamPaperSummary, ExamSessionSummary};
use sutra_contracts::exams::status::{
    AttemptKind, ExamPaperStatus, ExamRegistrationStatus, ExamSessionKind, RegistrationChannel,
};
use sutra_contracts::finance::ports::ExamFeeRequest;
use sutra_contracts::student::ids::StudentId;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use utoipa::ToSchema;
use uuid::Uuid;

/// A registered paper as the API shows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationPaperView {
    pub registration_paper_id: Uuid,
    pub paper_id: Uuid,
    pub paper_code: String,
    pub title: String,
    pub attempt_kind: AttemptKind,
    pub attempt_no: i32,
    /// `registered | withdrawn | absent | appeared | debarred`.
    pub status: String,
}

/// Summary of the registration's current hall ticket.
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamHallTicketRef {
    pub id: Uuid,
    pub number: String,
    pub status: String,
}

/// An approved special examination arrangement (L3-I-029 / I-040).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamArrangementView {
    pub id: Uuid,
    pub paper_id: Option<Uuid>,
    /// `extra_time | scribe | separate_room | assistive_technology | medical | other`.
    pub kind: String,
    pub extra_minutes: Option<i32>,
    pub note: String,
    pub status: String,
}

/// Late registration fee raised in Finance (L3-I-019).
#[derive(Debug, Clone, Serialize, ToSchema, sqlx::FromRow)]
#[serde(rename_all = "camelCase")]
pub struct ExamLateFeeView {
    pub fee_code: String,
    #[schema(value_type = String, example = "500.00")]
    pub amount: Decimal,
    pub charge_id: Uuid,
    pub charge_number: String,
}

/// An examination registration (contract §5.3).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationView {
    pub id: Uuid,
    pub registration_number: String,
    pub session_id: Uuid,
    pub institution_id: Uuid,
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub channel: RegistrationChannel,
    pub status: ExamRegistrationStatus,
    pub status_reason: Option<String>,
    pub papers: Vec<ExamRegistrationPaperView>,
    pub hall_ticket: Option<ExamHallTicketRef>,
    pub arrangements: Vec<ExamArrangementView>,
    pub late_fee: Option<ExamLateFeeView>,
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationList {
    pub items: Vec<ExamRegistrationView>,
    pub next_cursor: Option<String>,
}

/// A paper to register (supplementary / backlog / improvement attempts: `attemptKind`).
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExamRegistrationPaperInput {
    pub paper_id: Uuid,
    pub attempt_kind: Option<AttemptKind>,
    /// Defaults to one more than the student's earlier attempts of the component.
    pub attempt_no: Option<i32>,
}

/// Builds the API view of a registration (papers, ticket, arrangements, fee).
pub async fn registration_view(
    st: &ExamsState,
    tenant: TenantId,
    r: CandidateSummary,
) -> Result<ExamRegistrationView, KernelError> {
    let id = r.registration_id.as_uuid();
    let papers = papers_of(&st.pool, tenant, r.session_id.as_uuid()).await?;
    let reason: Option<String> =
        sqlx::query_scalar("SELECT status_reason FROM exams.exam_registration WHERE id = $1")
            .bind(id)
            .fetch_optional(&st.pool)
            .await
            .map_err(map_db_error)?
            .flatten();
    let ticket = sqlx::query_as::<_, ExamHallTicketRef>(
        "SELECT id, number, status FROM exams.hall_ticket WHERE tenant_id = $1 AND registration_id = $2 \
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    let arrangements = sqlx::query_as::<_, ExamArrangementView>(
        "SELECT id, paper_id, kind, extra_minutes, note, status FROM exams.registration_arrangement \
         WHERE tenant_id = $1 AND registration_id = $2 ORDER BY created_at",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_all(&st.pool)
    .await
    .map_err(map_db_error)?;
    let late_fee = sqlx::query_as::<_, ExamLateFeeView>(
        "SELECT fee_code, amount, charge_id, charge_number FROM exams.registration_fee \
         WHERE tenant_id = $1 AND registration_id = $2",
    )
    .bind(tenant.as_uuid())
    .bind(id)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)?;
    Ok(ExamRegistrationView {
        id,
        registration_number: r.registration_number,
        session_id: r.session_id.as_uuid(),
        institution_id: r.institution_id.as_uuid(),
        student_id: r.student_id.as_uuid(),
        student_number: r.student_number,
        display_name: r.display_name,
        channel: r.channel,
        status: r.status,
        status_reason: reason,
        papers: r
            .papers
            .iter()
            .map(|p| {
                let paper = papers.iter().find(|x| x.id == p.paper_id);
                ExamRegistrationPaperView {
                    registration_paper_id: p.registration_paper_id.as_uuid(),
                    paper_id: p.paper_id.as_uuid(),
                    paper_code: paper.map(|x| x.paper_code.clone()).unwrap_or_default(),
                    title: paper.map(|x| x.title.clone()).unwrap_or_default(),
                    attempt_kind: p.attempt_kind,
                    attempt_no: p.attempt_no,
                    status: p.status.as_str().to_owned(),
                }
            })
            .collect(),
        hall_ticket: ticket,
        arrangements,
        late_fee,
        version: r.version,
    })
}

/// Loads a registration with its papers (404 when missing / foreign).
pub async fn load_registration<'c, E>(
    executor: E,
    tenant: TenantId,
    id: Uuid,
) -> Result<CandidateSummary, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres> + Copy,
{
    let row = sqlx::query(&format!(
        "SELECT {REGISTRATION_COLUMNS} FROM exams.exam_registration WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "examination registration",
    })?;
    let r = registration_from_row(&row).map_err(map_db_error)?;
    let mut v = attach_papers(executor, tenant, vec![r]).await?;
    v.pop().ok_or(KernelError::NotFound {
        entity: "examination registration",
    })
}

/// Where the registration date falls against the session window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Window {
    NotYetOpen,
    Open,
    Closed,
}

/// Window position of `today` (open when no dates are set).
pub fn window_of(s: &ExamSessionSummary, today: NaiveDate) -> Window {
    if s.registration_opens_on.is_some_and(|d| today < d) {
        Window::NotYetOpen
    } else if s.registration_closes_on.is_some_and(|d| today > d) {
        Window::Closed
    } else {
        Window::Open
    }
}

/// `true` when the student holds an approved, unexpired deadline override in the session.
pub async fn has_override(
    st: &ExamsState,
    tenant: TenantId,
    session: Uuid,
    student: Uuid,
    today: NaiveDate,
) -> Result<Option<Uuid>, KernelError> {
    sqlx::query_scalar(
        "SELECT id FROM exams.registration_override WHERE tenant_id = $1 AND session_id = $2 \
         AND student_id = $3 AND status = 'approved' AND valid_until >= $4",
    )
    .bind(tenant.as_uuid())
    .bind(session)
    .bind(student)
    .bind(today)
    .fetch_optional(&st.pool)
    .await
    .map_err(map_db_error)
}

/// Earlier attempts of a curriculum component by the student in other sessions (registered
/// papers not withdrawn).
pub async fn prior_attempts(
    st: &ExamsState,
    tenant: TenantId,
    student: Uuid,
    component: Uuid,
    session: Uuid,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM exams.exam_registration_paper rp \
         JOIN exams.exam_registration r ON r.id = rp.registration_id \
         JOIN exams.exam_paper p ON p.id = rp.paper_id \
         WHERE rp.tenant_id = $1 AND r.student_id = $2 AND p.curriculum_component_id = $3 \
         AND r.session_id <> $4 AND rp.status <> 'withdrawn' \
         AND r.status NOT IN ('withdrawn', 'rejected', 'archived')",
    )
    .bind(tenant.as_uuid())
    .bind(student)
    .bind(component)
    .bind(session)
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)
}

/// Outcome of the eligibility check of the requested papers.
#[derive(Debug, Default)]
pub struct Eligibility {
    pub papers: Vec<NewRegistrationPaper>,
    pub problems: Vec<String>,
}

/// Validates each requested paper: it belongs to the session and is live; regular attempts
/// must be covered by the student's effective 3B term registration (`TermRegistration::covers`);
/// repeat attempts (backlog / supplementary / improvement) must stay within the policy's
/// attempt limit (L3-I-030). With no papers requested, every covered live paper is taken.
pub async fn check_eligibility(
    st: &ExamsState,
    scope: &ExecutionScope,
    tenant: TenantId,
    session: &ExamSessionSummary,
    student: StudentId,
    rules: &PolicyRules,
    requested: &[ExamRegistrationPaperInput],
) -> Result<Eligibility, KernelError> {
    let papers = papers_of(&st.pool, tenant, session.id.as_uuid()).await?;
    let term = st
        .registrations
        .term_registration(scope, student, session.academic_term_id)
        .await?;
    let covers = |p: &ExamPaperSummary| {
        term.as_ref()
            .is_some_and(|t| t.covers(p.curriculum_component_id))
    };
    let default_kind = match session.kind {
        ExamSessionKind::Supplementary => AttemptKind::Supplementary,
        ExamSessionKind::Improvement => AttemptKind::Improvement,
        _ => AttemptKind::Regular,
    };
    let wanted: Vec<ExamRegistrationPaperInput> = if requested.is_empty() {
        papers
            .iter()
            .filter(|p| p.status != ExamPaperStatus::Cancelled && covers(p))
            .map(|p| ExamRegistrationPaperInput {
                paper_id: p.id.as_uuid(),
                attempt_kind: None,
                attempt_no: None,
            })
            .collect()
    } else {
        requested.to_vec()
    };
    let mut out = Eligibility::default();
    if wanted.is_empty() {
        out.problems.push(
            "no examination paper of the session is covered by the student's term registration"
                .into(),
        );
        return Ok(out);
    }
    for w in &wanted {
        let Some(p) = papers.iter().find(|p| p.id.as_uuid() == w.paper_id) else {
            out.problems
                .push(format!("paper {} is not part of the session", w.paper_id));
            continue;
        };
        if p.status == ExamPaperStatus::Cancelled {
            out.problems
                .push(format!("paper {} is cancelled", p.paper_code));
            continue;
        }
        if out.papers.iter().any(|x| x.paper_id == p.id) {
            continue;
        }
        let kind = w.attempt_kind.unwrap_or(default_kind);
        let prior = prior_attempts(
            st,
            tenant,
            student.as_uuid(),
            p.curriculum_component_id,
            session.id.as_uuid(),
        )
        .await?;
        let attempt_no = w
            .attempt_no
            .unwrap_or_else(|| i32::try_from(prior + 1).unwrap_or(i32::MAX));
        if kind == AttemptKind::Regular {
            if !covers(p) {
                out.problems.push(format!(
                    "{} is not on the student's academic registration for the term",
                    p.paper_code
                ));
                continue;
            }
        } else if prior == 0 {
            out.problems.push(format!(
                "{}: a {} attempt needs an earlier attempt of the course",
                p.paper_code,
                kind.as_str()
            ));
            continue;
        }
        if attempt_no < 1 || attempt_no > rules.max_attempts {
            out.problems.push(format!(
                "{}: attempt {attempt_no} exceeds the policy limit of {} attempts",
                p.paper_code, rules.max_attempts
            ));
            continue;
        }
        out.papers.push(NewRegistrationPaper {
            paper_id: p.id,
            attempt_kind: kind,
            attempt_no,
        });
    }
    Ok(out)
}

/// Identity snapshot of a candidate (Domain C) for a registration.
pub struct Candidate {
    pub student_id: StudentId,
    pub context_id: sutra_contracts::student::ids::StudentContextId,
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
}

/// Resolves the student and their live context in the session's institution (422 otherwise).
pub async fn candidate(
    st: &ExamsState,
    scope: &ExecutionScope,
    session: &ExamSessionSummary,
    student: StudentId,
) -> Result<Candidate, KernelError> {
    let s = st.students.student(scope, student).await?.ok_or_else(|| {
        crate::setup::common::invalid("studentId", "not_found", "unknown student")
    })?;
    let ctx = st
        .students
        .live_context(scope, student, session.institution_id)
        .await?
        .ok_or_else(|| {
            crate::setup::common::invalid(
                "studentId",
                "no_live_context",
                "the student has no live enrolment in the session's institution",
            )
        })?;
    Ok(Candidate {
        student_id: s.id,
        context_id: ctx.id,
        student_number: s.student_number,
        display_name: s.display_name,
        subject_ref: s.subject_ref,
    })
}

/// Registration event payload.
pub fn reg_event(
    r: &CandidateSummary,
    from: Option<&str>,
    to: &str,
    reason: Option<String>,
) -> StatusChangedV1 {
    let mut e = status_event(
        r.registration_id.as_uuid(),
        r.institution_id,
        from,
        to,
        r.version,
    );
    e.session_id = Some(r.session_id.as_uuid());
    e.registration_id = Some(r.registration_id.as_uuid());
    e.student_id = Some(r.student_id.as_uuid());
    e.student_subject_ref.clone_from(&r.subject_ref);
    e.reason = reason;
    e
}

/// What to do after the validated insert.
pub struct Register<'a> {
    pub session: &'a ExamSessionSummary,
    pub candidate: Candidate,
    pub channel: RegistrationChannel,
    pub eligibility: Eligibility,
    /// Hold for `REGISTRATION_REVIEW` instead of confirming.
    pub review: bool,
}

/// Inserts the registration (`pending_validation`) through `shared::registration` and walks
/// it to `confirmed` (or `rejected` with the eligibility problems, or `pending_review`) in the
/// caller's transaction. Returns the registration.
pub async fn register(
    conn: &mut PgConnection,
    st: &ExamsState,
    scope: &ExecutionScope,
    tenant: TenantId,
    r: Register<'_>,
    actor: &Actor,
) -> Result<CandidateSummary, KernelError> {
    let rejected = !r.eligibility.problems.is_empty() || r.eligibility.papers.is_empty();
    let reg = insert_registration(
        conn,
        &NewRegistration {
            tenant,
            institution: r.session.institution_id,
            session_id: r.session.id,
            student_id: r.candidate.student_id,
            student_context_id: r.candidate.context_id,
            student_number: r.candidate.student_number.clone(),
            display_name: r.candidate.display_name.clone(),
            subject_ref: r.candidate.subject_ref.clone(),
            channel: r.channel,
            status: ExamRegistrationStatus::PendingValidation,
            papers: if rejected {
                Vec::new()
            } else {
                r.eligibility.papers.clone()
            },
        },
        EXAM_REGISTRATION_CONFIRMED,
        actor,
    )
    .await?;
    if rejected {
        let why = r.eligibility.problems.join("; ");
        let out = change_registration_status(
            conn,
            tenant,
            &reg,
            ExamRegistrationStatus::Rejected,
            &StatusChange {
                cause_event_type: EXAM_REGISTRATION_REJECTED,
                reason: Some(&why),
                process_instance_id: None,
            },
            actor,
        )
        .await?;
        let e = reg_event(
            &out,
            Some("pending_validation"),
            "rejected",
            Some(why.clone()),
        );
        emit(
            conn,
            scope,
            (
                EXAM_REGISTRATION_REJECTED,
                EXAM_REGISTRATION_REJECTED_SCHEMA,
            ),
            &e,
            actor,
        )
        .await?;
        audit(
            conn,
            scope,
            "exams.exam_registration.reject",
            "exam_registration",
            out.registration_id.to_string(),
            json!({"reasons": r.eligibility.problems}),
            actor,
        )
        .await?;
        return Ok(out);
    }
    if r.review {
        let outcome = request_approval(
            st,
            scope,
            process::REGISTRATION_REVIEW,
            business_key("exam_registration", reg.registration_id, reg.version),
            BTreeMap::from([(
                "registrationNumber".to_owned(),
                json!(reg.registration_number),
            )]),
        )
        .await?;
        let pid = outcome.process_instance_id().to_owned();
        let pending = change_registration_status(
            conn,
            tenant,
            &reg,
            ExamRegistrationStatus::PendingReview,
            &StatusChange {
                cause_event_type: "exams.exam_registration.review",
                reason: Some("held for review"),
                process_instance_id: Some(&pid),
            },
            actor,
        )
        .await?;
        if let ApprovalOutcome::Pending { .. } = outcome {
            audit(
                conn,
                scope,
                "exams.exam_registration.review",
                "exam_registration",
                pending.registration_id.to_string(),
                json!({"processInstanceId": pid}),
                actor,
            )
            .await?;
            return Ok(pending);
        }
        let sys = Actor::system(actor.correlation_id.clone());
        return confirm(conn, scope, tenant, &pending, Some("auto-approved"), &sys).await;
    }
    confirm(conn, scope, tenant, &reg, None, actor).await
}

/// `pending_validation | pending_review → approved → confirmed` + `exam_registration.confirmed`.
pub async fn confirm(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    tenant: TenantId,
    reg: &CandidateSummary,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<CandidateSummary, KernelError> {
    let from = reg.status;
    let approved = change_registration_status(
        conn,
        tenant,
        reg,
        ExamRegistrationStatus::Approved,
        &StatusChange {
            cause_event_type: "exams.exam_registration.approve",
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let confirmed = change_registration_status(
        conn,
        tenant,
        &approved,
        ExamRegistrationStatus::Confirmed,
        &StatusChange {
            cause_event_type: EXAM_REGISTRATION_CONFIRMED,
            reason,
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let e = reg_event(
        &confirmed,
        Some(from.as_str()),
        "confirmed",
        Some(confirmed.registration_number.clone()),
    );
    emit(
        conn,
        scope,
        (
            EXAM_REGISTRATION_CONFIRMED,
            EXAM_REGISTRATION_CONFIRMED_SCHEMA,
        ),
        &e,
        actor,
    )
    .await?;
    audit(
        conn,
        scope,
        "exams.exam_registration.confirm",
        "exam_registration",
        confirmed.registration_id.to_string(),
        json!({"channel": confirmed.channel.as_str()}),
        actor,
    )
    .await?;
    Ok(confirmed)
}

/// Raises the policy's late-registration fee in Finance (idempotent on the registration id)
/// and records it; a Finance failure is logged and leaves the registration as it is.
pub async fn raise_late_fee(
    st: &ExamsState,
    scope: &ExecutionScope,
    tenant: TenantId,
    reg: &CandidateSummary,
    rules: &PolicyRules,
    academic_term_id: Uuid,
    actor: &Actor,
) -> Result<(), KernelError> {
    let (Some(code), Some(amount)) = (rules.late_fee_code.clone(), rules.late_fee_amount) else {
        return Ok(());
    };
    if amount <= Decimal::ZERO {
        return Ok(());
    }
    let fee = st
        .fees
        .raise_exam_fee(
            scope,
            ExamFeeRequest {
                institution_id: reg.institution_id,
                student_id: reg.student_id.as_uuid(),
                fee_code: code.clone(),
                description: format!("Late examination registration {}", reg.registration_number),
                amount,
                academic_term_id: Some(academic_term_id),
                due_date: None,
                source_id: reg.registration_id.as_uuid(),
                actor: actor.id.clone(),
            },
        )
        .await;
    match fee {
        Ok(c) => {
            sqlx::query(
                "INSERT INTO exams.registration_fee (tenant_id, institution_id, registration_id, \
                 fee_code, amount, charge_id, charge_number) VALUES ($1, $2, $3, $4, $5, $6, $7) \
                 ON CONFLICT (registration_id) DO NOTHING",
            )
            .bind(tenant.as_uuid())
            .bind(reg.institution_id.as_uuid())
            .bind(reg.registration_id.as_uuid())
            .bind(&code)
            .bind(c.amount)
            .bind(c.charge_id.as_uuid())
            .bind(&c.charge_number)
            .execute(&st.pool)
            .await
            .map_err(map_db_error)?;
            Ok(())
        }
        Err(e) => {
            tracing::warn!(registration = %reg.registration_id, error = %e, "late examination fee not raised");
            Ok(())
        }
    }
}

/// Live registration of the student in the session, if any (duplicate detection, L3-I-032).
pub async fn live_registration(
    st: &ExamsState,
    scope: &ExecutionScope,
    session: ExamSessionId,
    student: StudentId,
) -> Result<Option<CandidateSummary>, KernelError> {
    st.directory
        .registration_of_student(scope, session, student)
        .await
}

/// Paper ids helper.
pub fn paper_ids(r: &CandidateSummary) -> Vec<ExamPaperId> {
    r.papers.iter().map(|p| p.paper_id).collect()
}

/// Registration id helper.
pub fn rid(id: Uuid) -> ExamRegistrationId {
    ExamRegistrationId(id)
}

/// Today's business date for the session's institution.
pub async fn today_for(
    st: &ExamsState,
    scope: &ExecutionScope,
    s: &ExamSessionSummary,
) -> Result<NaiveDate, KernelError> {
    business_today(st, scope, s.institution_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_positions() {
        let d = |m, day| NaiveDate::from_ymd_opt(2026, m, day).unwrap_or_default();
        let s = ExamSessionSummary {
            id: ExamSessionId(Uuid::nil()),
            institution_id: sutra_kernel::scope::InstitutionId(Uuid::nil()),
            academic_year_id: None,
            academic_term_id: Uuid::nil(),
            code: "ESE".into(),
            name: "x".into(),
            kind: ExamSessionKind::Regular,
            status: sutra_contracts::exams::status::ExamSessionStatus::Active,
            policy_id: None,
            grading_scheme_id: None,
            starts_on: None,
            ends_on: None,
            registration_opens_on: Some(d(10, 1)),
            registration_closes_on: Some(d(11, 10)),
            version: 1,
        };
        assert_eq!(window_of(&s, d(9, 30)), Window::NotYetOpen);
        assert_eq!(window_of(&s, d(11, 10)), Window::Open);
        assert_eq!(window_of(&s, d(11, 11)), Window::Closed);
    }
}
