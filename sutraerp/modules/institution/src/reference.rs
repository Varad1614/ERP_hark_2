//! [`InstitutionReferencePort`] over the Domain B tables (Batch 3 foundation-owned).
//!
//! Read-only lookups for Domain C (and later D/F/I), which may not query the `institution`
//! schema themselves. Every query filters on the scope tenant; missing / foreign rows are `None`.
//! Numeric credit columns are read as `float8`.

use chrono::NaiveDate;
use sqlx::postgres::PgRow;
use sqlx::{PgPool, Row};
use sutra_contracts::institution::reference::{
    AcademicTermRef, AcademicYearRef, CampusRef, CurriculumComponentRef, CurriculumVersionRef,
    InstitutionRef, InstitutionReferencePort, KeyDateRef, OrgUnitRef, ProgrammeRef,
};
use sutra_contracts::institution::status::{
    AcademicYearStatus, CampusStatus, InstitutionStatus, OrgUnitStatus, PublicationStatus,
};
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Postgres adapter of [`InstitutionReferencePort`].
#[derive(Clone)]
pub struct PgInstitutionReferences {
    pool: PgPool,
}

impl PgInstitutionReferences {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    async fn one<T>(
        &self,
        sql: &str,
        scope: &ExecutionScope,
        id: Uuid,
        map: fn(&PgRow) -> Result<T, sqlx::Error>,
    ) -> Result<Option<T>, KernelError> {
        let row = sqlx::query(sql)
            .bind(id)
            .bind(scope.tenant_id.as_uuid())
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        row.as_ref().map(map).transpose().map_err(map_db_error)
    }

    async fn many<T>(
        &self,
        sql: &str,
        scope: &ExecutionScope,
        id: Uuid,
        map: fn(&PgRow) -> Result<T, sqlx::Error>,
    ) -> Result<Vec<T>, KernelError> {
        let rows = sqlx::query(sql)
            .bind(id)
            .bind(scope.tenant_id.as_uuid())
            .fetch_all(&self.pool)
            .await
            .map_err(map_db_error)?;
        rows.iter()
            .map(map)
            .collect::<Result<_, _>>()
            .map_err(map_db_error)
    }
}

fn status<T>(r: &PgRow, col: &str, parse: fn(&str) -> Option<T>) -> Result<T, sqlx::Error> {
    let raw: String = r.try_get(col)?;
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown {col} `{raw}`").into()))
}

fn institution(r: &PgRow) -> Result<InstitutionRef, sqlx::Error> {
    Ok(InstitutionRef {
        id: InstitutionId(r.try_get("id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        status: status(r, "status", InstitutionStatus::parse)?,
        timezone: r.try_get("timezone")?,
    })
}

fn campus(r: &PgRow) -> Result<CampusRef, sqlx::Error> {
    Ok(CampusRef {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        status: status(r, "status", CampusStatus::parse)?,
        timezone: r.try_get("timezone")?,
    })
}

fn org_unit(r: &PgRow) -> Result<OrgUnitRef, sqlx::Error> {
    Ok(OrgUnitRef {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        unit_type: r.try_get("unit_type")?,
        status: status(r, "status", OrgUnitStatus::parse)?,
    })
}

fn programme(r: &PgRow) -> Result<ProgrammeRef, sqlx::Error> {
    Ok(ProgrammeRef {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        org_unit_id: r.try_get("org_unit_id")?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        level: r.try_get("level")?,
        duration_terms: r.try_get("duration_terms")?,
        campus_ids: r.try_get("campus_ids")?,
        status: status(r, "status", PublicationStatus::parse)?,
    })
}

const CURRICULUM_VERSION_COLUMNS: &str = "id, curriculum_id, programme_id, institution_id, \
    version_no, status, effective_from, term_structure, term_count, credit_framework, \
    min_total_credits::float8 AS min_total_credits, \
    min_credits_per_term::float8 AS min_credits_per_term, \
    max_credits_per_term::float8 AS max_credits_per_term, multiple_entry_exit, abc_enabled, \
    nheqf_level::float8 AS nheqf_level";

fn curriculum_version(r: &PgRow) -> Result<CurriculumVersionRef, sqlx::Error> {
    Ok(CurriculumVersionRef {
        id: r.try_get("id")?,
        curriculum_id: r.try_get("curriculum_id")?,
        programme_id: r.try_get("programme_id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        version_no: r.try_get("version_no")?,
        status: status(r, "status", PublicationStatus::parse)?,
        effective_from: r.try_get("effective_from")?,
        term_structure: r.try_get("term_structure")?,
        term_count: r.try_get("term_count")?,
        credit_framework: r.try_get("credit_framework")?,
        min_total_credits: r.try_get("min_total_credits")?,
        min_credits_per_term: r.try_get("min_credits_per_term")?,
        max_credits_per_term: r.try_get("max_credits_per_term")?,
        multiple_entry_exit: r.try_get("multiple_entry_exit")?,
        abc_enabled: r.try_get("abc_enabled")?,
        nheqf_level: r.try_get("nheqf_level")?,
    })
}

fn component(r: &PgRow) -> Result<CurriculumComponentRef, sqlx::Error> {
    Ok(CurriculumComponentRef {
        id: r.try_get("id")?,
        curriculum_version_id: r.try_get("curriculum_version_id")?,
        term_no: r.try_get("term_no")?,
        slot_code: r.try_get("slot_code")?,
        title: r.try_get("title")?,
        category: r.try_get("category")?,
        credits: r.try_get("credits")?,
        course_code: r.try_get("course_code")?,
    })
}

fn academic_year(r: &PgRow) -> Result<AcademicYearRef, sqlx::Error> {
    Ok(AcademicYearRef {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        calendar_id: r.try_get("calendar_id")?,
        label: r.try_get("label")?,
        status: status(r, "status", AcademicYearStatus::parse)?,
        starts_on: r.try_get("starts_on")?,
        ends_on: r.try_get("ends_on")?,
    })
}

fn academic_term(r: &PgRow) -> Result<AcademicTermRef, sqlx::Error> {
    Ok(AcademicTermRef {
        id: r.try_get("id")?,
        institution_id: InstitutionId(r.try_get("institution_id")?),
        calendar_version_id: r.try_get("calendar_version_id")?,
        academic_year_id: r.try_get("academic_year_id")?,
        calendar_version_status: status(r, "version_status", PublicationStatus::parse)?,
        code: r.try_get("code")?,
        name: r.try_get("name")?,
        sequence: r.try_get("sequence")?,
        starts_on: r.try_get("starts_on")?,
        ends_on: r.try_get("ends_on")?,
    })
}

fn key_date(r: &PgRow) -> Result<KeyDateRef, sqlx::Error> {
    Ok(KeyDateRef {
        id: r.try_get("id")?,
        calendar_version_id: r.try_get("calendar_version_id")?,
        kind: r.try_get("kind")?,
        name: r.try_get("name")?,
        starts_on: r.try_get("starts_on")?,
        ends_on: r.try_get("ends_on")?,
    })
}

#[async_trait::async_trait]
impl InstitutionReferencePort for PgInstitutionReferences {
    async fn institution(
        &self,
        scope: &ExecutionScope,
        id: InstitutionId,
    ) -> Result<Option<InstitutionRef>, KernelError> {
        self.one(
            "SELECT id, code, name, status, timezone FROM institution.institution \
             WHERE id = $1 AND tenant_id = $2",
            scope,
            id.as_uuid(),
            institution,
        )
        .await
    }

    async fn campus(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<CampusRef>, KernelError> {
        self.one(
            "SELECT id, institution_id, code, name, status, timezone FROM institution.campus \
             WHERE id = $1 AND tenant_id = $2",
            scope,
            id,
            campus,
        )
        .await
    }

    async fn org_unit(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<OrgUnitRef>, KernelError> {
        self.one(
            "SELECT id, institution_id, code, name, unit_type, status FROM institution.org_unit \
             WHERE id = $1 AND tenant_id = $2",
            scope,
            id,
            org_unit,
        )
        .await
    }

    async fn programme(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<ProgrammeRef>, KernelError> {
        self.one(
            "SELECT id, institution_id, org_unit_id, code, name, level, duration_terms, \
             campus_ids, status FROM institution.programme WHERE id = $1 AND tenant_id = $2",
            scope,
            id,
            programme,
        )
        .await
    }

    async fn programme_by_code(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        code: &str,
    ) -> Result<Option<ProgrammeRef>, KernelError> {
        let row = sqlx::query(
            "SELECT id, institution_id, org_unit_id, code, name, level, duration_terms, \
             campus_ids, status FROM institution.programme \
             WHERE tenant_id = $1 AND institution_id = $2 AND upper(code) = upper($3)",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(code.trim())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.as_ref()
            .map(programme)
            .transpose()
            .map_err(map_db_error)
    }

    async fn campus_by_code(
        &self,
        scope: &ExecutionScope,
        institution_id: InstitutionId,
        code: &str,
    ) -> Result<Option<CampusRef>, KernelError> {
        let row = sqlx::query(
            "SELECT id, institution_id, code, name, status, timezone FROM institution.campus \
             WHERE tenant_id = $1 AND institution_id = $2 AND upper(code) = upper($3)",
        )
        .bind(scope.tenant_id.as_uuid())
        .bind(institution_id.as_uuid())
        .bind(code.trim())
        .fetch_optional(&self.pool)
        .await
        .map_err(map_db_error)?;
        row.as_ref().map(campus).transpose().map_err(map_db_error)
    }

    async fn curriculum_version(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<CurriculumVersionRef>, KernelError> {
        let sql = format!(
            "SELECT {CURRICULUM_VERSION_COLUMNS} FROM institution.curriculum_version \
             WHERE id = $1 AND tenant_id = $2"
        );
        self.one(&sql, scope, id, curriculum_version).await
    }

    async fn current_curriculum_version(
        &self,
        scope: &ExecutionScope,
        programme_id: Uuid,
        on: NaiveDate,
    ) -> Result<Option<CurriculumVersionRef>, KernelError> {
        let sql = format!(
            "SELECT {CURRICULUM_VERSION_COLUMNS} FROM institution.curriculum_version \
             WHERE programme_id = $1 AND tenant_id = $2 AND status = 'published' \
               AND effective_from <= $3 \
             ORDER BY effective_from DESC, version_no DESC LIMIT 1"
        );
        let row = sqlx::query(&sql)
            .bind(programme_id)
            .bind(scope.tenant_id.as_uuid())
            .bind(on)
            .fetch_optional(&self.pool)
            .await
            .map_err(map_db_error)?;
        row.as_ref()
            .map(curriculum_version)
            .transpose()
            .map_err(map_db_error)
    }

    async fn curriculum_components(
        &self,
        scope: &ExecutionScope,
        curriculum_version_id: Uuid,
    ) -> Result<Vec<CurriculumComponentRef>, KernelError> {
        self.many(
            "SELECT id, curriculum_version_id, term_no, slot_code, title, category, \
             credits::float8 AS credits, course_code FROM institution.curriculum_component \
             WHERE curriculum_version_id = $1 AND tenant_id = $2 \
             ORDER BY term_no, sort_order, slot_code",
            scope,
            curriculum_version_id,
            component,
        )
        .await
    }

    async fn academic_year(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<AcademicYearRef>, KernelError> {
        self.one(
            "SELECT id, institution_id, calendar_id, label, status, starts_on, ends_on \
             FROM institution.academic_year WHERE id = $1 AND tenant_id = $2",
            scope,
            id,
            academic_year,
        )
        .await
    }

    async fn academic_term(
        &self,
        scope: &ExecutionScope,
        id: Uuid,
    ) -> Result<Option<AcademicTermRef>, KernelError> {
        self.one(
            "SELECT t.id, t.institution_id, t.calendar_version_id, v.academic_year_id, \
             v.status AS version_status, t.code, t.name, t.sequence, t.starts_on, t.ends_on \
             FROM institution.academic_term t \
             JOIN institution.calendar_version v ON v.id = t.calendar_version_id \
             WHERE t.id = $1 AND t.tenant_id = $2",
            scope,
            id,
            academic_term,
        )
        .await
    }

    async fn key_dates(
        &self,
        scope: &ExecutionScope,
        calendar_version_id: Uuid,
    ) -> Result<Vec<KeyDateRef>, KernelError> {
        self.many(
            "SELECT id, calendar_version_id, kind, name, starts_on, ends_on \
             FROM institution.calendar_key_date \
             WHERE calendar_version_id = $1 AND tenant_id = $2 ORDER BY starts_on, name",
            scope,
            calendar_version_id,
            key_date,
        )
        .await
    }
}
