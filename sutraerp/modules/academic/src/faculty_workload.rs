//! [`FacultyWorkloadPort`] over the 4A tables (Batch 7 foundation, contract change to Domain D
//! recorded in `docs/batch7-contracts.md` §6.2): a faculty member's approved / active faculty
//! assignments with weekly contact hours and the institution's effective assignment policy
//! maximum, read-only, for Domain E (7B workload plans, L3-E-030). People never reads the
//! `academic` schema itself.

use rust_decimal::Decimal;
use sqlx::{PgPool, Row};
use sutra_contracts::academic::ids::{CourseOfferingId, FacultyAssignmentId, OfferingSectionId};
use sutra_contracts::academic::ports::{FacultyWorkload, FacultyWorkloadPort, TeachingLoadItem};
use sutra_contracts::academic::status::{FacultyAssignmentStatus, FacultyRole};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

#[derive(Clone)]
pub struct PgFacultyWorkload {
    pool: PgPool,
}

impl PgFacultyWorkload {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

fn decode<T>(raw: String, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown value `{raw}`").into()))
}

#[async_trait::async_trait]
impl FacultyWorkloadPort for PgFacultyWorkload {
    async fn teaching_load(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        faculty_subject_ref: &str,
        academic_term_id: Option<Uuid>,
    ) -> Result<FacultyWorkload, KernelError> {
        let rows = sqlx::query(
            "SELECT fa.id, fa.offering_id, fa.section_id, o.academic_term_id, o.offering_code, \
             o.title, fa.role, fa.status, fa.weekly_hours \
             FROM academic.faculty_assignment fa \
             JOIN academic.course_offering o ON o.id = fa.offering_id \
             WHERE fa.tenant_id = $1 AND fa.institution_id = $2 AND fa.faculty_subject_ref = $3 \
             AND fa.status IN ('approved', 'active') \
             AND ($4::uuid IS NULL OR o.academic_term_id = $4) \
             ORDER BY o.offering_code, fa.id",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(faculty_subject_ref)
        .bind(academic_term_id)
        .fetch_all(&self.pool)
        .await
        .map_err(map_db_error)?;
        let items = rows
            .iter()
            .map(|r| {
                Ok(TeachingLoadItem {
                    assignment_id: FacultyAssignmentId(r.try_get("id")?),
                    offering_id: CourseOfferingId(r.try_get("offering_id")?),
                    section_id: OfferingSectionId(r.try_get("section_id")?),
                    academic_term_id: r.try_get("academic_term_id")?,
                    offering_code: r.try_get("offering_code")?,
                    title: r.try_get("title")?,
                    role: decode(r.try_get("role")?, FacultyRole::parse)?,
                    status: decode(r.try_get("status")?, FacultyAssignmentStatus::parse)?,
                    weekly_hours: r.try_get("weekly_hours")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(map_db_error)?;
        let policy_max_weekly_hours: Option<Decimal> = sqlx::query_scalar(
            "SELECT max_weekly_hours FROM academic.faculty_assignment_policy \
             WHERE tenant_id = $1 AND institution_id = $2 AND status = 'effective' LIMIT 1",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?
        .flatten();
        let total_weekly_hours = items
            .iter()
            .filter_map(|i| i.weekly_hours)
            .fold(Decimal::ZERO, |a, b| a + b);
        Ok(FacultyWorkload {
            faculty_subject_ref: faculty_subject_ref.to_owned(),
            items,
            total_weekly_hours,
            policy_max_weekly_hours,
        })
    }
}
