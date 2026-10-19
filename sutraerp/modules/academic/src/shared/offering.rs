//! Row types and loaders of the shared delivery tables (`academic.course_offering`,
//! `academic.offering_section`, `academic.faculty_assignment`, `academic.section_enrolment`),
//! created by the Batch 4 foundation migration.
//!
//! **Writes:** only slice **4A** inserts these rows and changes their attributes; **status**
//! changes go only through [`crate::shared::lifecycle`]. **Reads:** 4A uses these loaders; 4B and
//! 4C read offerings, sections, rosters and faculty through `AcademicState::offerings`
//! (`OfferingDirectory`) and may use the `load_*` functions here for a row they need inside a
//! transaction (read only, never `lock_*` — locks belong to the writer).

use chrono::{DateTime, NaiveDate, Utc};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, PgExecutor, Row};
use sutra_contracts::academic::ids::{
    CourseOfferingId, FacultyAssignmentId, OfferingSectionId, SectionEnrolmentId,
};
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, FacultyRole, SectionEnrolmentStatus,
    SectionStatus,
};
use sutra_contracts::student::ids::{StudentContextId, StudentId};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{InstitutionId, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

/// Columns of [`OfferingRow`].
pub const OFFERING_COLUMNS: &str = "id, tenant_id, institution_id, campus_id, org_unit_id, \
    academic_year_id, academic_term_id, curriculum_version_id, curriculum_component_id, \
    offering_code, title, course_code, category, credits::float8 AS credits, delivery_mode, \
    capacity, status, status_reason, status_changed_at, process_instance_id, merged_into_id, \
    split_from_id, version, created_at, updated_at";

/// Columns of [`SectionRow`].
pub const SECTION_COLUMNS: &str = "id, tenant_id, institution_id, offering_id, section_code, \
    campus_id, capacity, status, status_reason, status_changed_at, merged_into_id, version, \
    created_at, updated_at";

/// Columns of [`FacultyAssignmentRow`].
pub const ASSIGNMENT_COLUMNS: &str = "id, tenant_id, institution_id, offering_id, section_id, \
    faculty_subject_ref, faculty_name, staff_id, role, weekly_hours::float8 AS weekly_hours, \
    effective_from, effective_to, status, status_reason, status_changed_at, process_instance_id, \
    version, created_at, updated_at";

/// Columns of [`SectionEnrolmentRow`].
pub const ENROLMENT_COLUMNS: &str = "id, tenant_id, institution_id, offering_id, section_id, \
    student_id, student_context_id, student_number, display_name, subject_ref, registration_id, \
    registration_line_id, origin, status, status_reason, status_changed_at, transferred_from_id, \
    enrolled_at, ended_at, version, created_at, updated_at";

#[derive(Clone, Debug, PartialEq)]
pub struct OfferingRow {
    pub id: CourseOfferingId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub campus_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub academic_year_id: Option<Uuid>,
    pub academic_term_id: Uuid,
    pub curriculum_version_id: Uuid,
    pub curriculum_component_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub course_code: Option<String>,
    pub category: Option<String>,
    pub credits: f64,
    pub delivery_mode: String,
    pub capacity: Option<i32>,
    pub status: CourseOfferingStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub process_instance_id: Option<String>,
    pub merged_into_id: Option<CourseOfferingId>,
    pub split_from_id: Option<CourseOfferingId>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionRow {
    pub id: OfferingSectionId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub offering_id: CourseOfferingId,
    pub section_code: String,
    pub campus_id: Option<Uuid>,
    pub capacity: Option<i32>,
    pub status: SectionStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub merged_into_id: Option<OfferingSectionId>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FacultyAssignmentRow {
    pub id: FacultyAssignmentId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub offering_id: CourseOfferingId,
    pub section_id: OfferingSectionId,
    pub faculty_subject_ref: String,
    pub faculty_name: Option<String>,
    pub staff_id: Option<Uuid>,
    pub role: FacultyRole,
    pub weekly_hours: Option<f64>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    pub status: FacultyAssignmentStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub process_instance_id: Option<String>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionEnrolmentRow {
    pub id: SectionEnrolmentId,
    pub tenant_id: TenantId,
    pub institution_id: InstitutionId,
    pub offering_id: CourseOfferingId,
    pub section_id: OfferingSectionId,
    pub student_id: StudentId,
    pub student_context_id: StudentContextId,
    pub student_number: String,
    pub display_name: String,
    pub subject_ref: Option<String>,
    pub registration_id: Option<Uuid>,
    pub registration_line_id: Option<Uuid>,
    pub origin: String,
    pub status: SectionEnrolmentStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub transferred_from_id: Option<SectionEnrolmentId>,
    pub enrolled_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub version: Version,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn decode<T>(raw: String, parse: fn(&str) -> Option<T>, what: &str) -> Result<T, sqlx::Error> {
    parse(&raw).ok_or_else(|| sqlx::Error::Decode(format!("unknown {what} `{raw}`").into()))
}

pub fn offering_from_row(r: &PgRow) -> Result<OfferingRow, sqlx::Error> {
    Ok(OfferingRow {
        id: CourseOfferingId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        campus_id: r.try_get("campus_id")?,
        org_unit_id: r.try_get("org_unit_id")?,
        academic_year_id: r.try_get("academic_year_id")?,
        academic_term_id: r.try_get("academic_term_id")?,
        curriculum_version_id: r.try_get("curriculum_version_id")?,
        curriculum_component_id: r.try_get("curriculum_component_id")?,
        offering_code: r.try_get("offering_code")?,
        title: r.try_get("title")?,
        course_code: r.try_get("course_code")?,
        category: r.try_get("category")?,
        credits: r.try_get("credits")?,
        delivery_mode: r.try_get("delivery_mode")?,
        capacity: r.try_get("capacity")?,
        status: decode(
            r.try_get("status")?,
            CourseOfferingStatus::parse,
            "offering status",
        )?,
        status_reason: r.try_get("status_reason")?,
        status_changed_at: r.try_get("status_changed_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        merged_into_id: r
            .try_get::<Option<Uuid>, _>("merged_into_id")?
            .map(CourseOfferingId),
        split_from_id: r
            .try_get::<Option<Uuid>, _>("split_from_id")?
            .map(CourseOfferingId),
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

pub fn section_from_row(r: &PgRow) -> Result<SectionRow, sqlx::Error> {
    Ok(SectionRow {
        id: OfferingSectionId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        offering_id: CourseOfferingId(r.try_get("offering_id")?),
        section_code: r.try_get("section_code")?,
        campus_id: r.try_get("campus_id")?,
        capacity: r.try_get("capacity")?,
        status: decode(r.try_get("status")?, SectionStatus::parse, "section status")?,
        status_reason: r.try_get("status_reason")?,
        status_changed_at: r.try_get("status_changed_at")?,
        merged_into_id: r
            .try_get::<Option<Uuid>, _>("merged_into_id")?
            .map(OfferingSectionId),
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

pub fn assignment_from_row(r: &PgRow) -> Result<FacultyAssignmentRow, sqlx::Error> {
    Ok(FacultyAssignmentRow {
        id: FacultyAssignmentId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        offering_id: CourseOfferingId(r.try_get("offering_id")?),
        section_id: OfferingSectionId(r.try_get("section_id")?),
        faculty_subject_ref: r.try_get("faculty_subject_ref")?,
        faculty_name: r.try_get("faculty_name")?,
        staff_id: r.try_get("staff_id")?,
        role: decode(r.try_get("role")?, FacultyRole::parse, "faculty role")?,
        weekly_hours: r.try_get("weekly_hours")?,
        effective_from: r.try_get("effective_from")?,
        effective_to: r.try_get("effective_to")?,
        status: decode(
            r.try_get("status")?,
            FacultyAssignmentStatus::parse,
            "faculty assignment status",
        )?,
        status_reason: r.try_get("status_reason")?,
        status_changed_at: r.try_get("status_changed_at")?,
        process_instance_id: r.try_get("process_instance_id")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

pub fn enrolment_from_row(r: &PgRow) -> Result<SectionEnrolmentRow, sqlx::Error> {
    Ok(SectionEnrolmentRow {
        id: SectionEnrolmentId(r.try_get("id")?),
        tenant_id: TenantId(r.try_get("tenant_id")?),
        institution_id: InstitutionId(r.try_get("institution_id")?),
        offering_id: CourseOfferingId(r.try_get("offering_id")?),
        section_id: OfferingSectionId(r.try_get("section_id")?),
        student_id: StudentId(r.try_get("student_id")?),
        student_context_id: StudentContextId(r.try_get("student_context_id")?),
        student_number: r.try_get("student_number")?,
        display_name: r.try_get("display_name")?,
        subject_ref: r.try_get("subject_ref")?,
        registration_id: r.try_get("registration_id")?,
        registration_line_id: r.try_get("registration_line_id")?,
        origin: r.try_get("origin")?,
        status: decode(
            r.try_get("status")?,
            SectionEnrolmentStatus::parse,
            "section enrolment status",
        )?,
        status_reason: r.try_get("status_reason")?,
        status_changed_at: r.try_get("status_changed_at")?,
        transferred_from_id: r
            .try_get::<Option<Uuid>, _>("transferred_from_id")?
            .map(SectionEnrolmentId),
        enrolled_at: r.try_get("enrolled_at")?,
        ended_at: r.try_get("ended_at")?,
        version: Version(r.try_get("version")?),
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
    })
}

async fn one<'e, E: PgExecutor<'e>, T>(
    exec: E,
    sql: &str,
    id: Uuid,
    tenant: TenantId,
    map: fn(&PgRow) -> Result<T, sqlx::Error>,
    entity: &'static str,
) -> Result<T, KernelError> {
    let row = sqlx::query(sql)
        .bind(id)
        .bind(tenant.as_uuid())
        .fetch_optional(exec)
        .await
        .map_err(map_db_error)?
        .ok_or(KernelError::NotFound { entity })?;
    map(&row).map_err(map_db_error)
}

/// Loads an offering of the tenant (404 when missing / foreign).
pub async fn load_offering<'e, E: PgExecutor<'e>>(
    exec: E,
    tenant: TenantId,
    id: CourseOfferingId,
) -> Result<OfferingRow, KernelError> {
    let sql = format!(
        "SELECT {OFFERING_COLUMNS} FROM academic.course_offering WHERE id = $1 AND tenant_id = $2"
    );
    one(
        exec,
        &sql,
        id.as_uuid(),
        tenant,
        offering_from_row,
        "course offering",
    )
    .await
}

/// `SELECT … FOR UPDATE` of an offering inside the writer's transaction (4A only).
pub async fn lock_offering(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: CourseOfferingId,
) -> Result<OfferingRow, KernelError> {
    let sql = format!(
        "SELECT {OFFERING_COLUMNS} FROM academic.course_offering WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    one(
        &mut *conn,
        &sql,
        id.as_uuid(),
        tenant,
        offering_from_row,
        "course offering",
    )
    .await
}

/// Loads a section of the tenant.
pub async fn load_section<'e, E: PgExecutor<'e>>(
    exec: E,
    tenant: TenantId,
    id: OfferingSectionId,
) -> Result<SectionRow, KernelError> {
    let sql = format!(
        "SELECT {SECTION_COLUMNS} FROM academic.offering_section WHERE id = $1 AND tenant_id = $2"
    );
    one(
        exec,
        &sql,
        id.as_uuid(),
        tenant,
        section_from_row,
        "section",
    )
    .await
}

/// Locks a section (4A only).
pub async fn lock_section(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: OfferingSectionId,
) -> Result<SectionRow, KernelError> {
    let sql = format!(
        "SELECT {SECTION_COLUMNS} FROM academic.offering_section WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    one(
        &mut *conn,
        &sql,
        id.as_uuid(),
        tenant,
        section_from_row,
        "section",
    )
    .await
}

/// Loads a faculty assignment of the tenant.
pub async fn load_assignment<'e, E: PgExecutor<'e>>(
    exec: E,
    tenant: TenantId,
    id: FacultyAssignmentId,
) -> Result<FacultyAssignmentRow, KernelError> {
    let sql = format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM academic.faculty_assignment WHERE id = $1 AND tenant_id = $2"
    );
    one(
        exec,
        &sql,
        id.as_uuid(),
        tenant,
        assignment_from_row,
        "faculty assignment",
    )
    .await
}

/// Locks a faculty assignment (4A only).
pub async fn lock_assignment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: FacultyAssignmentId,
) -> Result<FacultyAssignmentRow, KernelError> {
    let sql = format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM academic.faculty_assignment WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    one(
        &mut *conn,
        &sql,
        id.as_uuid(),
        tenant,
        assignment_from_row,
        "faculty assignment",
    )
    .await
}

/// Loads a section enrolment of the tenant.
pub async fn load_enrolment<'e, E: PgExecutor<'e>>(
    exec: E,
    tenant: TenantId,
    id: SectionEnrolmentId,
) -> Result<SectionEnrolmentRow, KernelError> {
    let sql = format!(
        "SELECT {ENROLMENT_COLUMNS} FROM academic.section_enrolment WHERE id = $1 AND tenant_id = $2"
    );
    one(
        exec,
        &sql,
        id.as_uuid(),
        tenant,
        enrolment_from_row,
        "section enrolment",
    )
    .await
}

/// Locks a section enrolment (4A only).
pub async fn lock_enrolment(
    conn: &mut PgConnection,
    tenant: TenantId,
    id: SectionEnrolmentId,
) -> Result<SectionEnrolmentRow, KernelError> {
    let sql = format!(
        "SELECT {ENROLMENT_COLUMNS} FROM academic.section_enrolment WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    );
    one(
        &mut *conn,
        &sql,
        id.as_uuid(),
        tenant,
        enrolment_from_row,
        "section enrolment",
    )
    .await
}

/// Seated (`enrolled`) students of a section, ordered by student number.
pub async fn seated_roster<'e, E: PgExecutor<'e>>(
    exec: E,
    tenant: TenantId,
    section: OfferingSectionId,
) -> Result<Vec<SectionEnrolmentRow>, KernelError> {
    let sql = format!(
        "SELECT {ENROLMENT_COLUMNS} FROM academic.section_enrolment \
         WHERE section_id = $1 AND tenant_id = $2 AND status = 'enrolled' ORDER BY student_number"
    );
    let rows = sqlx::query(&sql)
        .bind(section.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_all(exec)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(enrolment_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}
