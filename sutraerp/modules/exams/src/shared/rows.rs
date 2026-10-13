//! Row decoders of the foundation tables (`exams.exam_session`, `exam_paper`,
//! `exam_registration(_paper)`, `paper_mark`, `grading_scheme` / `grade_band`). Used by the
//! foundation directory / marks reader and by the slices' own queries, so every slice decodes the
//! shared rows the same way.

use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use sutra_contracts::exams::ids::{
    ExamPaperId, ExamRegistrationId, ExamRegistrationPaperId, ExamSessionId, GradingSchemeId,
    PaperMarkId,
};
use sutra_contracts::exams::ports::{
    CandidatePaper, CandidateSummary, ExamPaperSummary, ExamSessionSummary, GradeBand,
    GradingSchemeSummary, PaperMarkSummary,
};
use sutra_contracts::exams::status::{
    AttemptKind, ExamMode, ExamPaperStatus, ExamRegistrationStatus, ExamSessionKind,
    ExamSessionStatus, GradingSchemeStatus, MarkStatus, RegistrationChannel,
    RegistrationPaperStatus,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Strict decode of a stored status / classification name.
pub fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown value `{raw}`").into()))
}

// ------------------------------------------------------------------ session

pub const SESSION_COLUMNS: &str = "id, institution_id, academic_year_id, academic_term_id, code, \
    name, kind, status, policy_id, grading_scheme_id, starts_on, ends_on, registration_opens_on, \
    registration_closes_on, version";

pub fn session_from_row(r: &PgRow) -> Result<ExamSessionSummary, sqlx::Error> {
    Ok(ExamSessionSummary {
        id: ExamSessionId(r.try_get("id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        academic_year_id: r.try_get("academic_year_id")?,
        academic_term_id: r.try_get("academic_term_id")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        kind: decode(r.try_get("kind")?, ExamSessionKind::parse)?,
        status: decode(r.try_get("status")?, ExamSessionStatus::parse)?,
        policy_id: r.try_get("policy_id")?,
        grading_scheme_id: r
            .try_get::<Option<Uuid>, _>("grading_scheme_id")?
            .map(GradingSchemeId),
        starts_on: r.try_get("starts_on")?,
        ends_on: r.try_get("ends_on")?,
        registration_opens_on: r.try_get("registration_opens_on")?,
        registration_closes_on: r.try_get("registration_closes_on")?,
        version: r.try_get("version")?,
    })
}

/// Locks a session row (`FOR UPDATE`) in the caller's transaction (404 when missing / foreign).
pub async fn lock_session(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ExamSessionId,
) -> Result<ExamSessionSummary, KernelError> {
    let sql = format!(
        "SELECT {SESSION_COLUMNS} FROM exams.exam_session WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "examination session",
        })?;
    session_from_row(&row).map_err(map_db_error)
}

// ------------------------------------------------------------------ paper

pub const PAPER_COLUMNS: &str = "id, session_id, institution_id, offering_id, \
    curriculum_component_id, academic_term_id, paper_code, title, credits, mode, max_marks, \
    pass_percent, internal_weight, duration_minutes, status, version";

pub fn paper_from_row(r: &PgRow) -> Result<ExamPaperSummary, sqlx::Error> {
    Ok(ExamPaperSummary {
        id: ExamPaperId(r.try_get("id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        offering_id: r.try_get("offering_id")?,
        curriculum_component_id: r.try_get("curriculum_component_id")?,
        academic_term_id: r.try_get("academic_term_id")?,
        paper_code: r.try_get("paper_code")?,
        title: r.try_get("title")?,
        credits: r.try_get("credits")?,
        mode: decode(r.try_get("mode")?, ExamMode::parse)?,
        max_marks: r.try_get("max_marks")?,
        pass_percent: r.try_get("pass_percent")?,
        internal_weight: r.try_get("internal_weight")?,
        duration_minutes: r.try_get("duration_minutes")?,
        status: decode(r.try_get("status")?, ExamPaperStatus::parse)?,
        version: r.try_get("version")?,
    })
}

/// Locks a paper row (`FOR UPDATE`) in the caller's transaction.
pub async fn lock_paper(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ExamPaperId,
) -> Result<ExamPaperSummary, KernelError> {
    let sql = format!(
        "SELECT {PAPER_COLUMNS} FROM exams.exam_paper WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "examination paper",
        })?;
    paper_from_row(&row).map_err(map_db_error)
}

// ------------------------------------------------------------------ registration

pub const REGISTRATION_COLUMNS: &str = "id, session_id, institution_id, student_id, \
    student_context_id, registration_number, student_number, display_name, subject_ref, channel, \
    status, version";

pub const REGISTRATION_PAPER_COLUMNS: &str =
    "id, registration_id, paper_id, attempt_kind, attempt_no, status";

/// Registration header without its papers (`papers` empty; fill with [`attach_papers`]).
pub fn registration_from_row(r: &PgRow) -> Result<CandidateSummary, sqlx::Error> {
    Ok(CandidateSummary {
        registration_id: ExamRegistrationId(r.try_get("id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        student_id: StudentId(r.try_get("student_id")?),
        student_context_id: StudentContextId(r.try_get("student_context_id")?),
        registration_number: r.try_get("registration_number")?,
        student_number: r.try_get("student_number")?,
        display_name: r.try_get("display_name")?,
        subject_ref: r.try_get("subject_ref")?,
        channel: decode(r.try_get("channel")?, RegistrationChannel::parse)?,
        status: decode(r.try_get("status")?, ExamRegistrationStatus::parse)?,
        papers: Vec::new(),
        version: r.try_get("version")?,
    })
}

/// `(registration_id, paper)` of a registration paper row.
pub fn registration_paper_from_row(r: &PgRow) -> Result<(Uuid, CandidatePaper), sqlx::Error> {
    Ok((
        r.try_get("registration_id")?,
        CandidatePaper {
            registration_paper_id: ExamRegistrationPaperId(r.try_get("id")?),
            paper_id: ExamPaperId(r.try_get("paper_id")?),
            attempt_kind: decode(r.try_get("attempt_kind")?, AttemptKind::parse)?,
            attempt_no: r.try_get("attempt_no")?,
            status: decode(r.try_get("status")?, RegistrationPaperStatus::parse)?,
        },
    ))
}

/// Loads the papers of `regs` (one query) and attaches them, ordered by creation.
pub async fn attach_papers<'c, E>(
    executor: E,
    tenant: TenantId,
    mut regs: Vec<CandidateSummary>,
) -> Result<Vec<CandidateSummary>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres>,
{
    if regs.is_empty() {
        return Ok(regs);
    }
    let ids: Vec<Uuid> = regs.iter().map(|r| r.registration_id.as_uuid()).collect();
    let sql = format!(
        "SELECT {REGISTRATION_PAPER_COLUMNS} FROM exams.exam_registration_paper \
         WHERE tenant_id = $1 AND registration_id = ANY($2) ORDER BY created_at, id"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(&ids)
        .fetch_all(executor)
        .await
        .map_err(map_db_error)?;
    for row in &rows {
        let (reg, paper) = registration_paper_from_row(row).map_err(map_db_error)?;
        if let Some(r) = regs.iter_mut().find(|r| r.registration_id.as_uuid() == reg) {
            r.papers.push(paper);
        }
    }
    Ok(regs)
}

/// Locks a registration row (`FOR UPDATE`) and loads its papers, in the caller's transaction.
pub async fn lock_registration(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: ExamRegistrationId,
) -> Result<CandidateSummary, KernelError> {
    let sql = format!(
        "SELECT {REGISTRATION_COLUMNS} FROM exams.exam_registration WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    let row = sqlx::query(&sql)
        .bind(id.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "examination registration",
        })?;
    let reg = registration_from_row(&row).map_err(map_db_error)?;
    let mut regs = attach_papers(&mut *conn, tenant, vec![reg]).await?;
    regs.pop().ok_or(KernelError::NotFound {
        entity: "examination registration",
    })
}

// ------------------------------------------------------------------ marks

pub const MARK_COLUMNS: &str = "id, session_id, paper_id, registration_id, registration_paper_id, \
    student_id, internal_marks, internal_max, external_marks, external_max, grace_marks, absent, \
    status, internal_source_id, version";

pub fn mark_from_row(r: &PgRow) -> Result<PaperMarkSummary, sqlx::Error> {
    Ok(PaperMarkSummary {
        id: PaperMarkId(r.try_get("id")?),
        session_id: ExamSessionId(r.try_get("session_id")?),
        paper_id: ExamPaperId(r.try_get("paper_id")?),
        registration_id: ExamRegistrationId(r.try_get("registration_id")?),
        registration_paper_id: ExamRegistrationPaperId(r.try_get("registration_paper_id")?),
        student_id: StudentId(r.try_get("student_id")?),
        internal_marks: r.try_get("internal_marks")?,
        internal_max: r.try_get("internal_max")?,
        external_marks: r.try_get("external_marks")?,
        external_max: r.try_get("external_max")?,
        grace_marks: r.try_get("grace_marks")?,
        absent: r.try_get("absent")?,
        status: decode(r.try_get("status")?, MarkStatus::parse)?,
        internal_source_id: r.try_get("internal_source_id")?,
        version: r.try_get("version")?,
    })
}

// ------------------------------------------------------------------ grading schemes

/// Loads a grading scheme with its bands (highest `min_percent` first); `None` when missing.
pub async fn load_scheme<'c, E>(
    executor: E,
    tenant: TenantId,
    id: GradingSchemeId,
) -> Result<Option<GradingSchemeSummary>, KernelError>
where
    E: sqlx::Executor<'c, Database = sqlx::Postgres> + Copy,
{
    let Some(r) = sqlx::query(
        "SELECT id, institution_id, code, name, status, version FROM exams.grading_scheme \
         WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id.as_uuid())
    .bind(tenant.as_uuid())
    .fetch_optional(executor)
    .await
    .map_err(map_db_error)?
    else {
        return Ok(None);
    };
    let bands = sqlx::query(
        "SELECT grade, min_percent, grade_point, is_pass FROM exams.grade_band \
         WHERE scheme_id = $1 ORDER BY min_percent DESC",
    )
    .bind(id.as_uuid())
    .fetch_all(executor)
    .await
    .map_err(map_db_error)?
    .iter()
    .map(|b| {
        Ok(GradeBand {
            grade: b.try_get("grade")?,
            min_percent: b.try_get("min_percent")?,
            grade_point: b.try_get("grade_point")?,
            is_pass: b.try_get("is_pass")?,
        })
    })
    .collect::<Result<Vec<_>, sqlx::Error>>()
    .map_err(map_db_error)?;
    let scheme = (|| -> Result<GradingSchemeSummary, sqlx::Error> {
        Ok(GradingSchemeSummary {
            id: GradingSchemeId(r.try_get("id")?),
            institution_id: InstitutionId(r.try_get("institution_id")?),
            code: r.try_get("code")?,
            name: r.try_get("name")?,
            bands,
            status: decode(r.try_get("status")?, GradingSchemeStatus::parse)?,
            version: r.try_get("version")?,
        })
    })()
    .map_err(map_db_error)?;
    Ok(Some(scheme))
}
