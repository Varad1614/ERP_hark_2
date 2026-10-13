//! Slice-4A helpers shared by the offering and faculty areas: request bodies, validation errors,
//! response views of the shared delivery tables, the command transaction and event helpers.

use crate::shared::guard::require_institution_writable;
use crate::shared::http::actor;
use crate::shared::offering::{
    assignment_from_row, lock_offering, offering_from_row, section_from_row, FacultyAssignmentRow,
    OfferingRow, SectionRow, ASSIGNMENT_COLUMNS, OFFERING_COLUMNS, SECTION_COLUMNS,
};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::{publish, AcademicState, Actor};
use axum::body::Bytes;
use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use sqlx::{PgConnection, PgExecutor, Postgres, Transaction};
use sutra_contracts::academic::events::offering::CourseOfferingV1;
use sutra_contracts::academic::events::{ModifiedV1, StatusChangedV1};
use sutra_contracts::academic::ids::{CourseOfferingId, OfferingSectionId};
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, FacultyRole, SectionStatus,
};
use sutra_kernel::auth::AuthContext;
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::{ExecutionScope, TenantId};
use sutra_persistence::map_db_error;
use uuid::Uuid;

// ------------------------------------------------------------------ request helpers

/// Parses an optional JSON body (empty body → `T::default()`).
pub fn body<T: DeserializeOwned + Default>(raw: &Bytes) -> Result<T, KernelError> {
    if raw.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(raw)
        .map_err(|e| KernelError::BadRequest(format!("invalid JSON body: {e}")))
}

/// `{ "reason": "…" }` of state commands.
#[derive(Debug, Default, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = OfferingStepReason)]
pub struct StepReason {
    pub reason: Option<String>,
}

impl StepReason {
    /// Trimmed reason; `required` → 422 when missing.
    pub fn validated(self, required: bool) -> Result<Option<String>, KernelError> {
        clean_reason(self.reason, required)
    }
}

/// Trimmed optional reason (≤ 1000 chars); `required` → 422 when missing.
pub fn clean_reason(r: Option<String>, required: bool) -> Result<Option<String>, KernelError> {
    let r = r.map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
    if required && r.is_none() {
        return Err(invalid("reason", "required", "a reason is required"));
    }
    if r.as_ref().is_some_and(|r| r.chars().count() > 1000) {
        return Err(invalid("reason", "too_long", "at most 1000 characters"));
    }
    Ok(r)
}

/// Approve / reject decision of a pending approval step.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize, utoipa::ToSchema,
)]
#[serde(rename_all = "snake_case")]
#[schema(as = OfferingStepDecision)]
pub enum StepDecision {
    Approve,
    Reject,
}

/// Body of the explicit decision endpoints (`AwaitDecision`).
#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
#[schema(as = OfferingStepDecisionBody)]
pub struct StepDecisionBody {
    pub decision: StepDecision,
    pub reason: Option<String>,
}

/// One field violation as a 422.
pub fn invalid(field: &str, code: &str, message: impl Into<String>) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

pub fn conflict(msg: impl Into<String>) -> KernelError {
    KernelError::Conflict(msg.into())
}

/// Optimistic concurrency check (412 on mismatch).
pub fn check_version(actual: Version, expected: Version) -> Result<(), KernelError> {
    actual.expect(expected)
}

/// Validated business code (`^[A-Za-z0-9][A-Za-z0-9/_.-]{0,max-1}$`).
pub fn check_code(
    field: &str,
    raw: &str,
    max: usize,
    allow_slash_dot: bool,
) -> Result<String, KernelError> {
    let c = raw.trim();
    let ok = !c.is_empty()
        && c.len() <= max
        && c.chars().next().is_some_and(|f| f.is_ascii_alphanumeric())
        && c.chars().all(|ch| {
            ch.is_ascii_alphanumeric()
                || ch == '_'
                || ch == '-'
                || (allow_slash_dot && (ch == '/' || ch == '.'))
        });
    if ok {
        Ok(c.to_owned())
    } else {
        Err(invalid(
            field,
            "invalid_code",
            format!("letters, digits and - _ only, at most {max} characters"),
        ))
    }
}

/// Validated free text (1..=max characters after trimming).
pub fn check_text(field: &str, raw: &str, max: usize) -> Result<String, KernelError> {
    let t = raw.trim();
    if t.is_empty() || t.chars().count() > max {
        return Err(invalid(
            field,
            "invalid_length",
            format!("1 to {max} characters"),
        ));
    }
    Ok(t.to_owned())
}

pub fn check_delivery_mode(raw: &str) -> Result<String, KernelError> {
    match raw {
        "in_person" | "online" | "hybrid" => Ok(raw.to_owned()),
        _ => Err(invalid(
            "deliveryMode",
            "invalid",
            "one of in_person, online, hybrid",
        )),
    }
}

pub fn check_capacity(field: &str, c: Option<i32>) -> Result<Option<i32>, KernelError> {
    if c.is_some_and(|c| !(0..=100_000).contains(&c)) {
        return Err(invalid(field, "out_of_range", "0 to 100000"));
    }
    Ok(c)
}

// ------------------------------------------------------------------ views

/// Faculty member on a section (§5.3).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct FacultyBrief {
    pub assignment_id: Uuid,
    pub faculty_subject_ref: String,
    pub faculty_name: Option<String>,
    pub role: FacultyRole,
    pub status: FacultyAssignmentStatus,
}

/// Teaching section of an offering.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingSection {
    pub id: Uuid,
    pub offering_id: Uuid,
    pub section_code: String,
    pub campus_id: Option<Uuid>,
    pub capacity: Option<i32>,
    pub seats_taken: i64,
    pub status: SectionStatus,
    pub status_reason: Option<String>,
    pub merged_into_id: Option<Uuid>,
    pub version: i64,
    pub faculty: Vec<FacultyBrief>,
}

/// Course offering with its sections (§5.3).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Offering {
    pub id: Uuid,
    pub institution_id: Uuid,
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
    pub seats_taken: i64,
    /// Waitlist entries still waiting or holding an offer.
    pub waitlisted: i64,
    pub status: CourseOfferingStatus,
    pub status_reason: Option<String>,
    pub status_changed_at: DateTime<Utc>,
    pub process_instance_id: Option<String>,
    pub merged_into_id: Option<Uuid>,
    pub split_from_id: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub sections: Vec<OfferingSection>,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct OfferingList {
    pub items: Vec<Offering>,
}

pub fn faculty_brief(a: &FacultyAssignmentRow) -> FacultyBrief {
    FacultyBrief {
        assignment_id: a.id.as_uuid(),
        faculty_subject_ref: a.faculty_subject_ref.clone(),
        faculty_name: a.faculty_name.clone(),
        role: a.role,
        status: a.status,
    }
}

/// Seated (`enrolled`) count of one section.
pub async fn seated_in_section<'e, E: PgExecutor<'e>>(
    exec: E,
    section: OfferingSectionId,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM academic.section_enrolment WHERE section_id = $1 AND status = 'enrolled'",
    )
    .bind(section.as_uuid())
    .fetch_one(exec)
    .await
    .map_err(map_db_error)
}

/// Seated (`enrolled`) count of one offering.
pub async fn seated_in_offering<'e, E: PgExecutor<'e>>(
    exec: E,
    offering: CourseOfferingId,
) -> Result<i64, KernelError> {
    sqlx::query_scalar(
        "SELECT count(*) FROM academic.section_enrolment WHERE offering_id = $1 AND status = 'enrolled'",
    )
    .bind(offering.as_uuid())
    .fetch_one(exec)
    .await
    .map_err(map_db_error)
}

/// Sections of an offering (all statuses, by code).
pub async fn sections_of(
    conn: &mut PgConnection,
    tenant: TenantId,
    offering: CourseOfferingId,
) -> Result<Vec<SectionRow>, KernelError> {
    let sql = format!(
        "SELECT {SECTION_COLUMNS} FROM academic.offering_section WHERE offering_id = $1 AND tenant_id = $2 ORDER BY section_code, created_at"
    );
    let rows = sqlx::query(&sql)
        .bind(offering.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(section_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

/// Faculty assignments of an offering (all statuses).
pub async fn assignments_of_offering(
    conn: &mut PgConnection,
    tenant: TenantId,
    offering: CourseOfferingId,
) -> Result<Vec<FacultyAssignmentRow>, KernelError> {
    let sql = format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM academic.faculty_assignment WHERE offering_id = $1 AND tenant_id = $2 ORDER BY created_at"
    );
    let rows = sqlx::query(&sql)
        .bind(offering.as_uuid())
        .bind(tenant.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    rows.iter()
        .map(assignment_from_row)
        .collect::<Result<_, _>>()
        .map_err(map_db_error)
}

pub async fn section_view(
    conn: &mut PgConnection,
    s: &SectionRow,
) -> Result<OfferingSection, KernelError> {
    let seats = seated_in_section(&mut *conn, s.id).await?;
    let sql = format!(
        "SELECT {ASSIGNMENT_COLUMNS} FROM academic.faculty_assignment WHERE section_id = $1 \
         AND status NOT IN ('archived', 'withdrawn', 'rejected') ORDER BY created_at"
    );
    let rows = sqlx::query(&sql)
        .bind(s.id.as_uuid())
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let faculty = rows
        .iter()
        .map(|r| assignment_from_row(r).map(|a| faculty_brief(&a)))
        .collect::<Result<_, _>>()
        .map_err(map_db_error)?;
    Ok(OfferingSection {
        id: s.id.as_uuid(),
        offering_id: s.offering_id.as_uuid(),
        section_code: s.section_code.clone(),
        campus_id: s.campus_id,
        capacity: s.capacity,
        seats_taken: seats,
        status: s.status,
        status_reason: s.status_reason.clone(),
        merged_into_id: s.merged_into_id.map(|x| x.as_uuid()),
        version: s.version.0,
        faculty,
    })
}

/// Full offering view (sections, faculty, seat counts).
pub async fn offering_view(
    conn: &mut PgConnection,
    o: &OfferingRow,
) -> Result<Offering, KernelError> {
    let mut sections = Vec::new();
    for s in sections_of(&mut *conn, o.tenant_id, o.id).await? {
        sections.push(section_view(&mut *conn, &s).await?);
    }
    let seats = seated_in_offering(&mut *conn, o.id).await?;
    let waitlisted: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.offering_waitlist_entry WHERE offering_id = $1 AND status IN ('waiting', 'offered')",
    )
    .bind(o.id.as_uuid())
    .fetch_one(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(Offering {
        id: o.id.as_uuid(),
        institution_id: o.institution_id.as_uuid(),
        campus_id: o.campus_id,
        org_unit_id: o.org_unit_id,
        academic_year_id: o.academic_year_id,
        academic_term_id: o.academic_term_id,
        curriculum_version_id: o.curriculum_version_id,
        curriculum_component_id: o.curriculum_component_id,
        offering_code: o.offering_code.clone(),
        title: o.title.clone(),
        course_code: o.course_code.clone(),
        category: o.category.clone(),
        credits: o.credits,
        delivery_mode: o.delivery_mode.clone(),
        capacity: o.capacity,
        seats_taken: seats,
        waitlisted,
        status: o.status,
        status_reason: o.status_reason.clone(),
        status_changed_at: o.status_changed_at,
        process_instance_id: o.process_instance_id.clone(),
        merged_into_id: o.merged_into_id.map(|x| x.as_uuid()),
        split_from_id: o.split_from_id.map(|x| x.as_uuid()),
        version: o.version.0,
        created_at: o.created_at,
        updated_at: o.updated_at,
        sections,
    })
}

/// Offering view read on the pool.
pub async fn load_view(
    st: &AcademicState,
    tenant: TenantId,
    id: CourseOfferingId,
) -> Result<Offering, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let o = crate::shared::offering::load_offering(&mut *conn, tenant, id).await?;
    offering_view(&mut conn, &o).await
}

/// Offerings of an institution (optionally one status / term).
pub async fn list_offerings(
    st: &AcademicState,
    tenant: TenantId,
    institution: Uuid,
    status: Option<CourseOfferingStatus>,
    term: Option<Uuid>,
) -> Result<Vec<Offering>, KernelError> {
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let sql = format!(
        "SELECT {OFFERING_COLUMNS} FROM academic.course_offering WHERE tenant_id = $1 AND institution_id = $2 \
         AND ($3::text IS NULL OR status = $3) AND ($4::uuid IS NULL OR academic_term_id = $4) \
         ORDER BY offering_code, created_at LIMIT 500"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant.as_uuid())
        .bind(institution)
        .bind(status.map(|s| s.as_str()))
        .bind(term)
        .fetch_all(&mut *conn)
        .await
        .map_err(map_db_error)?;
    let mut out = Vec::new();
    for r in &rows {
        let o = offering_from_row(r).map_err(map_db_error)?;
        out.push(offering_view(&mut conn, &o).await?);
    }
    Ok(out)
}

// ------------------------------------------------------------------ command transaction

/// An offering command in progress: owner-local tx + locked offering + scope + actor.
pub struct Cmd {
    pub tx: Transaction<'static, Postgres>,
    pub o: OfferingRow,
    pub scope: ExecutionScope,
    pub actor: Actor,
}

/// Loads the offering, checks the institution is writable, opens the tx, locks the row and
/// checks `If-Match` (when given).
pub async fn begin(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    expected: Option<Version>,
) -> Result<Cmd, KernelError> {
    let t = tenant(ctx);
    let o = crate::shared::offering::load_offering(&st.pool, t, id).await?;
    let scope = institution_scope(ctx, o.institution_id);
    require_institution_writable(st, &scope, o.institution_id).await?;
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = lock_offering(&mut tx, t, id).await?;
    if let Some(v) = expected {
        check_version(o.version, v)?;
    }
    Ok(Cmd {
        tx,
        o,
        scope,
        actor: actor(ctx),
    })
}

impl Cmd {
    /// Commits and returns the fresh offering view.
    pub async fn finish(mut self) -> Result<Offering, KernelError> {
        let o = crate::shared::offering::load_offering(&mut *self.tx, self.o.tenant_id, self.o.id)
            .await?;
        let v = offering_view(&mut self.tx, &o).await?;
        self.tx.commit().await.map_err(map_db_error)?;
        Ok(v)
    }

    /// Bumps the offering version (attribute changes) and reloads the row.
    pub async fn bump(&mut self) -> Result<(), KernelError> {
        sqlx::query(
            "UPDATE academic.course_offering SET version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(self.o.id.as_uuid())
        .execute(&mut *self.tx)
        .await
        .map_err(map_db_error)?;
        self.o = lock_offering(&mut self.tx, self.o.tenant_id, self.o.id).await?;
        Ok(())
    }

    pub async fn audit(
        &mut self,
        action: &str,
        details: serde_json::Value,
    ) -> Result<(), KernelError> {
        publish::audit(
            &mut self.tx,
            &self.scope,
            action,
            "course_offering",
            self.o.id.to_string(),
            details,
            &self.actor,
        )
        .await
    }
}

// ------------------------------------------------------------------ events

pub fn status_payload(
    o: &OfferingRow,
    from: Option<CourseOfferingStatus>,
    reason: Option<&str>,
) -> StatusChangedV1 {
    StatusChangedV1 {
        aggregate_id: o.id.as_uuid(),
        institution_id: o.institution_id.as_uuid(),
        offering_id: Some(o.id.as_uuid()),
        section_id: None,
        student_id: None,
        from_status: from.map(|f| f.as_str().to_owned()),
        to_status: o.status.as_str().to_owned(),
        version: o.version.0,
        reason: reason.map(str::to_owned),
        process_instance_id: o.process_instance_id.clone(),
        student_subject_ref: None,
        faculty_subject_ref: None,
    }
}

pub fn offering_v1(o: &OfferingRow) -> CourseOfferingV1 {
    CourseOfferingV1 {
        offering_id: o.id.as_uuid(),
        institution_id: o.institution_id.as_uuid(),
        campus_id: o.campus_id,
        academic_term_id: o.academic_term_id,
        curriculum_component_id: o.curriculum_component_id,
        offering_code: o.offering_code.clone(),
        title: o.title.clone(),
        credits: o.credits,
        status: o.status.as_str().to_owned(),
        version: o.version.0,
    }
}

/// Enqueues an offering event with `data` (subject = offering id).
pub async fn offering_event<T: serde::Serialize>(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    o: &OfferingRow,
    ty: &'static str,
    schema: &'static str,
    data: &T,
    actor: &Actor,
) -> Result<(), KernelError> {
    publish::event(conn, scope, ty, schema, o.id.to_string(), data, actor).await
}

/// `ModifiedV1` for an aggregate of an offering.
pub fn modified(
    aggregate_id: Uuid,
    o: &OfferingRow,
    section_id: Option<Uuid>,
    version: i64,
    fields: &[&str],
) -> ModifiedV1 {
    ModifiedV1 {
        aggregate_id,
        institution_id: o.institution_id.as_uuid(),
        offering_id: Some(o.id.as_uuid()),
        section_id,
        version,
        changed_fields: fields.iter().map(|f| (*f).to_owned()).collect(),
    }
}

/// Attribute snapshot of an offering (revision baselines / rollback targets).
pub fn snapshot(o: &OfferingRow) -> serde_json::Value {
    serde_json::json!({
        "title": o.title,
        "deliveryMode": o.delivery_mode,
        "campusId": o.campus_id,
        "orgUnitId": o.org_unit_id,
        "capacity": o.capacity,
        "curriculumComponentId": o.curriculum_component_id,
        "courseCode": o.course_code,
        "category": o.category,
        "credits": o.credits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_reasons() {
        assert!(check_code("c", "CS101-T1", 40, true).is_ok());
        assert!(check_code("c", "-bad", 40, true).is_err());
        assert!(check_code("c", "A/B", 20, false).is_err());
        assert!(clean_reason(Some("  ".into()), true).is_err());
        assert_eq!(
            clean_reason(Some(" ok ".into()), true).unwrap_or_default(),
            Some("ok".into())
        );
        assert!(check_capacity("c", Some(-1)).is_err());
        assert!(check_delivery_mode("hybrid").is_ok());
        assert!(check_delivery_mode("x").is_err());
        assert!(check_text("t", "", 10).is_err());
    }
}
