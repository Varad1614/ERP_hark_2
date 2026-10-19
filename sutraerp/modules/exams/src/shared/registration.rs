//! The **only** inserter of examination registrations and registered papers (contract doc
//! §3.3). Every registered paper gets its `exams.paper_mark` row (status `pending`,
//! `external_max` = the paper's `max_marks`) in the same transaction, so 6B always finds one
//! mark row per candidate × paper and 6C one input per result line.
//!
//! 6A calls these from its registration flows (self-service, bulk, late, administrative,
//! amendments, supplementary registration of L3-I-030); tests seed through
//! `apps/api/tests/support/b6.rs`, which calls the same functions.

use crate::shared::lifecycle::{record_history, HistoryEntry, StatusChange};
use crate::shared::numbering::{self, kind};
use crate::shared::rows::{lock_paper, lock_registration, registration_paper_from_row};
use crate::shared::Actor;
use sqlx::PgConnection;
use sutra_contracts::exams::ids::{ExamPaperId, ExamRegistrationId, ExamSessionId};
use sutra_contracts::exams::ports::{CandidatePaper, CandidateSummary};
use sutra_contracts::exams::status::{
    AttemptKind, ExamPaperStatus, ExamRegistrationStatus, RegistrationChannel,
    RegistrationPaperStatus,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// A paper to register.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRegistrationPaper {
    pub paper_id: ExamPaperId,
    pub attempt_kind: AttemptKind,
    pub attempt_no: i32,
}

/// A registration to insert.
#[derive(Clone, Debug)]
pub struct NewRegistration {
    pub tenant: TenantId,
    pub institution: InstitutionId,
    pub session_id: ExamSessionId,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub channel: RegistrationChannel,
    /// Initial status (`draft`, `pending_validation`, … or `confirmed` for administrative /
    /// bulk registrations the slice has already validated).
    pub status: ExamRegistrationStatus,
    pub papers: Vec<NewRegistrationPaper>,
}

/// Inserts a registration (`EXR-` number) with its papers and their `paper_mark` rows. A second
/// live registration of the same candidate in the session is a `409` (unique index, L3-I-032).
/// Every paper must belong to the session and not be `cancelled` (`409`).
pub async fn insert_registration(
    conn: &mut PgConnection,
    r: &NewRegistration,
    cause_event_type: &'static str,
    actor: &Actor,
) -> Result<CandidateSummary, KernelError> {
    let live: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM exams.exam_registration WHERE tenant_id = $1 AND session_id = $2 \
         AND student_id = $3 AND status NOT IN ('withdrawn', 'rejected', 'archived')",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.session_id.as_uuid())
    .bind(r.student_id.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    if live.is_some() {
        return Err(KernelError::Conflict(
            "the candidate already holds a live registration in this session".into(),
        ));
    }
    let number = numbering::next(conn, r.tenant, r.institution, kind::REGISTRATION).await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO exams.exam_registration (tenant_id, institution_id, session_id, student_id, \
         student_context_id, registration_number, student_number, display_name, subject_ref, \
         channel, status, status_changed_at, created_by) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, now(), $12) RETURNING id",
    )
    .bind(r.tenant.as_uuid())
    .bind(r.institution.as_uuid())
    .bind(r.session_id.as_uuid())
    .bind(r.student_id.as_uuid())
    .bind(r.student_context_id.as_uuid())
    .bind(&number)
    .bind(&r.student_number)
    .bind(&r.display_name)
    .bind(r.subject_ref.as_deref())
    .bind(r.channel.as_str())
    .bind(r.status.as_str())
    .bind(actor.id.as_deref())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    record_history(
        conn,
        HistoryEntry {
            tenant: r.tenant,
            institution: r.institution,
            aggregate_type: "exam_registration",
            aggregate_id: id,
            from: None,
            to: r.status.as_str(),
        },
        &StatusChange {
            cause_event_type,
            reason: Some("registered"),
            process_instance_id: None,
        },
        actor,
    )
    .await?;
    let mut reg = lock_registration(conn, r.tenant, ExamRegistrationId(id)).await?;
    for p in &r.papers {
        let paper = add_paper(conn, &reg, p).await?;
        reg.papers.push(paper);
    }
    Ok(reg)
}

/// Adds one paper (and its `paper_mark` row) to a registration (amendment, scope change,
/// supplementary attempt). `409` when already registered, the paper belongs to another session
/// or is cancelled.
pub async fn add_paper(
    conn: &mut PgConnection,
    reg: &CandidateSummary,
    p: &NewRegistrationPaper,
) -> Result<CandidatePaper, KernelError> {
    let tenant = TenantId(tenant_of(conn, reg.registration_id).await?);
    let paper = lock_paper(conn, tenant, p.paper_id).await?;
    if paper.session_id != reg.session_id {
        return Err(KernelError::Conflict(format!(
            "paper {} belongs to another examination session",
            paper.paper_code
        )));
    }
    if paper.status == ExamPaperStatus::Cancelled {
        return Err(KernelError::Conflict(format!(
            "paper {} is cancelled",
            paper.paper_code
        )));
    }
    if reg.papers.iter().any(|x| x.paper_id == p.paper_id) {
        return Err(KernelError::Conflict(format!(
            "paper {} is already on the registration",
            paper.paper_code
        )));
    }
    let row = sqlx::query(
        "INSERT INTO exams.exam_registration_paper (tenant_id, institution_id, registration_id, \
         paper_id, attempt_kind, attempt_no) VALUES ($1, $2, $3, $4, $5, $6) \
         RETURNING id, registration_id, paper_id, attempt_kind, attempt_no, status",
    )
    .bind(tenant.as_uuid())
    .bind(reg.institution_id.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(p.paper_id.as_uuid())
    .bind(p.attempt_kind.as_str())
    .bind(p.attempt_no)
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    let (_, cp) = registration_paper_from_row(&row).map_err(map_db_error)?;
    sqlx::query(
        "INSERT INTO exams.paper_mark (tenant_id, institution_id, session_id, paper_id, \
         registration_id, registration_paper_id, student_id, external_max) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(tenant.as_uuid())
    .bind(reg.institution_id.as_uuid())
    .bind(reg.session_id.as_uuid())
    .bind(p.paper_id.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(cp.registration_paper_id.as_uuid())
    .bind(reg.student_id.as_uuid())
    .bind(paper.max_marks)
    .execute(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(cp)
}

/// Sets the status of one registered paper (withdrawn, absent, appeared, debarred) — 6A
/// (withdrawal, conduct attendance) and 6B (malpractice debarment). Returns the updated paper.
pub async fn set_paper_status(
    conn: &mut PgConnection,
    tenant: TenantId,
    reg: &CandidateSummary,
    paper_id: ExamPaperId,
    to: RegistrationPaperStatus,
    reason: Option<&str>,
) -> Result<CandidatePaper, KernelError> {
    let row = sqlx::query(
        "UPDATE exams.exam_registration_paper SET status = $4, status_reason = $5, \
         version = version + 1, updated_at = now() \
         WHERE tenant_id = $1 AND registration_id = $2 AND paper_id = $3 \
         RETURNING id, registration_id, paper_id, attempt_kind, attempt_no, status",
    )
    .bind(tenant.as_uuid())
    .bind(reg.registration_id.as_uuid())
    .bind(paper_id.as_uuid())
    .bind(to.as_str())
    .bind(reason)
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?
    .ok_or(KernelError::NotFound {
        entity: "registered paper",
    })?;
    registration_paper_from_row(&row)
        .map(|(_, p)| p)
        .map_err(map_db_error)
}

async fn tenant_of(conn: &mut PgConnection, id: ExamRegistrationId) -> Result<Uuid, KernelError> {
    sqlx::query_scalar("SELECT tenant_id FROM exams.exam_registration WHERE id = $1")
        .bind(id.as_uuid())
        .fetch_optional(&mut *conn)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound {
            entity: "examination registration",
        })
}
