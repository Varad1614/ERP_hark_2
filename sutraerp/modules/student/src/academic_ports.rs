//! Domain C adapters of the ports Domain D (Batch 4) calls — **Batch 4 foundation-owned**.
//!
//! * [`PgRegistrationReader`] — `RegistrationReader` over 3B's `student.registration` /
//!   `registration_line` (read only). 4A gates offering enrolment on it.
//! * [`PgCreditPosting`] — `CreditPostingPort`: appends a `recognised` entry to 3B's
//!   `student.credit_ledger_entry` through 3B's own ledger helpers (`credit_ledger.updated`
//!   event + audit in the same Domain C transaction). Idempotent on `(source_type, source_id)`.
//!
//! Domain D never queries the `student` schema; it receives these through
//! `sutra_api::app::academic_deps`.

use crate::progression::repo::{append_entry, ledger_updated, NewEntry};
use crate::shared::publish::audit;
use crate::shared::student::{live_context, lock_context};
use crate::shared::Actor;
use serde_json::json;
use sqlx::{PgPool, Row};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_contracts::student::ports::{
    CreditPostingPort, CreditPostingReceipt, CreditPostingRequest, RegisteredComponent,
    RegistrationReader, TermRegistration,
};
use sutra_contracts::student::status::CreditEntryKind;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// [`RegistrationReader`] over 3B's registration tables.
#[derive(Clone)]
pub struct PgRegistrationReader {
    pool: PgPool,
}

impl PgRegistrationReader {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait::async_trait]
impl RegistrationReader for PgRegistrationReader {
    async fn term_registration(
        &self,
        scope: &ExecutionScope,
        student_id: StudentId,
        academic_term_id: Uuid,
    ) -> Result<Option<TermRegistration>, KernelError> {
        let head = sqlx::query(
            "SELECT id, student_context_id, institution_id, status FROM student.registration \
             WHERE tenant_id = $1 AND student_id = $2 AND academic_term_id = $3 \
               AND status NOT IN ('cancelled', 'archived') \
             ORDER BY created_at DESC LIMIT 1",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(student_id.as_uuid())
        .bind(academic_term_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        let Some(h) = head else { return Ok(None) };
        let registration_id: Uuid = h.try_get("id").map_err(map_db_error)?;
        let lines = sqlx::query(
            "SELECT id, curriculum_component_id, kind, status, credits::float8 AS credits \
             FROM student.registration_line WHERE registration_id = $1 AND tenant_id = $2 \
             ORDER BY created_at",
        )
        .bind(registration_id)
        .bind(scope.tenant_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?
        .iter()
        .map(|r| {
            Ok(RegisteredComponent {
                registration_line_id: r.try_get("id")?,
                curriculum_component_id: r.try_get("curriculum_component_id")?,
                kind: r.try_get("kind")?,
                status: r.try_get("status")?,
                credits: r.try_get("credits")?,
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()
        .map_err(map_db_error)?;
        Ok(Some(TermRegistration {
            registration_id,
            student_id,
            student_context_id: StudentContextId(
                h.try_get("student_context_id").map_err(map_db_error)?,
            ),
            institution_id: InstitutionId(h.try_get("institution_id").map_err(map_db_error)?),
            academic_term_id,
            status: h.try_get("status").map_err(map_db_error)?,
            lines,
        }))
    }
}

/// [`CreditPostingPort`] into 3B's credit ledger.
#[derive(Clone)]
pub struct PgCreditPosting {
    pool: PgPool,
}

impl PgCreditPosting {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn invalid(msg: &str) -> KernelError {
    KernelError::BadRequest(msg.to_owned())
}

#[async_trait::async_trait]
impl CreditPostingPort for PgCreditPosting {
    async fn post_credits(
        &self,
        scope: &ExecutionScope,
        req: CreditPostingRequest,
    ) -> Result<CreditPostingReceipt, KernelError> {
        if !(req.credits > 0.0 && req.credits <= 1000.0) {
            return Err(invalid("credits must be greater than 0 and at most 1000"));
        }
        if req.source_type.is_empty() || req.source_type == "manual" {
            return Err(invalid("source_type must name the posting domain"));
        }
        let tenant = scope.tenant_id;
        let ctx = live_context(&self.pool, tenant, req.student_id, req.institution_id)
            .await?
            .ok_or_else(|| {
                KernelError::Conflict(
                    "the student has no live context in this institution; credits cannot be posted"
                        .into(),
                )
            })?;
        let mut tx = self.pool.begin().await.map_err(map_db_error)?;
        let ctx = lock_context(&mut tx, tenant, ctx.id).await?;
        // Idempotent on (source_type, source_id): the context row lock serialises postings.
        let existing: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM student.credit_ledger_entry WHERE tenant_id = $1 AND student_id = $2 \
             AND source_type = $3 AND source_id = $4 ORDER BY recorded_at LIMIT 1",
        )
        .bind(tenant.as_uuid())
        .bind(req.student_id.as_uuid())
        .bind(req.source_type)
        .bind(req.source_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(map_db_error)?;
        if let Some(id) = existing {
            tx.commit().await.map_err(map_db_error)?;
            return Ok(CreditPostingReceipt {
                ledger_entry_id: id,
                already_posted: true,
            });
        }
        let actor = Actor {
            id: req.actor.clone(),
            correlation_id: sutra_web::request_id::current_correlation_id(),
        };
        let inst_scope = ExecutionScope::institution(tenant, ctx.institution_id);
        let id = append_entry(
            &mut tx,
            &ctx,
            &NewEntry {
                kind: CreditEntryKind::Recognised,
                credits: req.credits,
                component: req.curriculum_component_id,
                term: req.academic_term_id,
                source_type: req.source_type,
                source_id: Some(req.source_id),
                reverses: None,
                approved: true,
                note: req.note.as_deref(),
            },
            &actor,
        )
        .await?;
        ledger_updated(&mut tx, &inst_scope, &ctx, vec![id], &actor).await?;
        audit(
            &mut tx,
            &inst_scope,
            "student.credit_ledger.post_external",
            "credit_ledger_entry",
            id.to_string(),
            json!({"sourceType": req.source_type, "sourceId": req.source_id, "credits": req.credits}),
            &actor,
        )
        .await?;
        tx.commit().await.map_err(map_db_error)?;
        Ok(CreditPostingReceipt {
            ledger_entry_id: id,
            already_posted: false,
        })
    }
}
