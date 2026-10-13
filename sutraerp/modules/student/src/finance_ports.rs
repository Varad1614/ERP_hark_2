//! Domain C adapter of the port Domain F (Batch 5) calls — **Batch 5 foundation-owned**.
//!
//! * [`PgAidDecisionSource`] — `AidDecisionSource` over 3C's `student.scholarship_eligibility`
//!   (rows with `handed_off_at` set, L3-C-071) joined with `student.scholarship` for the
//!   benefit definition. Read only.
//!
//! Finance never queries the `student` schema; it receives this through
//! `sutra_api::app::finance_deps`.

use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use sutra_contracts::student::ids::StudentId;
use sutra_contracts::student::ports::{AidDecision, AidDecisionSource};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// [`AidDecisionSource`] over 3C's scholarship tables.
#[derive(Clone)]
pub struct PgAidDecisionSource {
    pool: PgPool,
}

impl PgAidDecisionSource {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

const SELECT: &str = "SELECT e.id, e.scholarship_id, s.code, s.name, s.sponsor_type, \
    s.benefit_type, s.benefit_value, s.currency, e.institution_id, e.student_id, \
    e.student_context_id, e.academic_year_id, e.status, e.valid_from, e.valid_until, \
    e.renewal_count, e.handed_off_at \
    FROM student.scholarship_eligibility e JOIN student.scholarship s ON s.id = e.scholarship_id";

fn decision(r: &PgRow) -> Result<AidDecision, sqlx::Error> {
    Ok(AidDecision {
        eligibility_id: r.try_get("id")?,
        scholarship_id: r.try_get("scholarship_id")?,
        scholarship_code: r.try_get("code")?,
        scholarship_name: r.try_get("name")?,
        sponsor_type: r.try_get("sponsor_type")?,
        benefit_type: r.try_get("benefit_type")?,
        benefit_value: r.try_get("benefit_value")?,
        currency: r.try_get("currency")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        student_id: StudentId(r.try_get("student_id")?),
        student_context_id: r.try_get("student_context_id")?,
        academic_year_id: r.try_get("academic_year_id")?,
        status: r.try_get("status")?,
        valid_from: r.try_get("valid_from")?,
        valid_until: r.try_get("valid_until")?,
        renewal_count: r.try_get("renewal_count")?,
        handed_off_at: r.try_get("handed_off_at")?,
    })
}

#[async_trait::async_trait]
impl AidDecisionSource for PgAidDecisionSource {
    async fn handed_off_decisions(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        student_id: Option<StudentId>,
    ) -> Result<Vec<AidDecision>, KernelError> {
        let rows = sqlx::query(&format!(
            "{SELECT} WHERE e.tenant_id = $1 AND e.institution_id = $2 \
             AND e.handed_off_at IS NOT NULL AND ($3::uuid IS NULL OR e.student_id = $3) \
             ORDER BY e.handed_off_at DESC, e.id"
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(student_id.map(|s| s.as_uuid()))
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        rows.iter()
            .map(decision)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }

    async fn decision(
        &self,
        scope: &ExecutionScope,
        eligibility_id: Uuid,
    ) -> Result<Option<AidDecision>, KernelError> {
        let row = sqlx::query(&format!(
            "{SELECT} WHERE e.tenant_id = $1 AND e.id = $2 AND e.handed_off_at IS NOT NULL"
        ))
        .bind(scope.tenant_id.as_uuid())
        .bind(eligibility_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.as_ref().map(decision).transpose().map_err(map_db_error)
    }
}
