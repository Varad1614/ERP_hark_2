//! Course offering lifecycle (L3-D-001…004, 010, 013): establishment, approval through the
//! WorkflowPort (the reference approval of Batch 4), publication, enrolment opening / closing,
//! activation with readiness, suspension / reactivation, freeze, completion, archive.

use super::sections::{insert_section, NewOfferingSection};
use super::support::*;
use crate::shared::approval::{business_key, process, request_approval, ApprovalOutcome};
use crate::shared::guard::{require_institution_writable, require_offering_changeable};
use crate::shared::http::{actor, with_etag};
use crate::shared::lifecycle::{
    change_assignment_status, change_enrolment_status, change_offering_status,
    change_section_status, StatusChange,
};
use crate::shared::offering::{
    lock_assignment, lock_enrolment, lock_section, offering_from_row, OfferingRow, OFFERING_COLUMNS,
};
use crate::shared::scope::{institution_scope, tenant};
use crate::shared::time::business_today;
use crate::shared::{AcademicState, Actor};
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::Response;
use axum::Json;
use sqlx::PgConnection;
use std::collections::BTreeMap;
use sutra_contracts::academic::events::offering as ev;
use sutra_contracts::academic::events::offering::FacultyAssignmentV1;
use sutra_contracts::academic::ids::{
    CourseOfferingId, FacultyAssignmentId, OfferingSectionId, SectionEnrolmentId,
};
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, FacultyRole, SectionEnrolmentStatus,
    SectionStatus,
};
use sutra_contracts::institution::status::{CampusStatus, OrgUnitStatus, PublicationStatus};
use sutra_kernel::auth::{AuthContext, Permission};
use sutra_kernel::concurrency::Version;
use sutra_kernel::error::KernelError;
use sutra_kernel::scope::{ExecutionScope, InstitutionId};
use sutra_persistence::map_db_error;
use sutra_web::idempotency::require_idempotency_key;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

macro_rules! expect_version {
    ($headers:expr) => {
        match $crate::shared::http::if_match(&$headers)? {
            Ok(v) => v,
            Err(res) => return Ok(res),
        }
    };
}
pub(crate) use expect_version;

pub fn etag(status: StatusCode, o: &Offering) -> Response {
    with_etag(status, o, Version(o.version))
}

// ================================================================== requests

#[derive(Debug, Clone, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreateOffering {
    pub academic_term_id: Uuid,
    pub curriculum_version_id: Uuid,
    /// The curriculum component offered; alternatively give its `slotCode` in the version.
    pub curriculum_component_id: Option<Uuid>,
    /// Slot code of the component in the curriculum version (e.g. `CS101`) when the id is not known.
    pub slot_code: Option<String>,
    /// Business code, unique per institution and term among live offerings (e.g. `CS101-T1`).
    pub offering_code: String,
    /// Defaults to the curriculum component title.
    pub title: Option<String>,
    pub campus_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    /// `in_person` (default) | `online` | `hybrid`.
    pub delivery_mode: Option<String>,
    /// Offering-wide seat cap (sections carry their own); omitted = unlimited.
    pub capacity: Option<i32>,
    /// Initial sections (planned), e.g. `[{"sectionCode": "A", "capacity": 60}]`.
    #[serde(default)]
    pub sections: Vec<NewOfferingSection>,
}

#[derive(Debug, Clone, Default, serde::Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOffering {
    pub offering_code: Option<String>,
    pub title: Option<String>,
    pub delivery_mode: Option<String>,
    pub campus_id: Option<Uuid>,
    pub org_unit_id: Option<Uuid>,
    pub capacity: Option<i32>,
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct OfferingQuery {
    #[param(inline)]
    pub status: Option<CourseOfferingStatus>,
    pub academic_term_id: Option<Uuid>,
}

/// Activation readiness (L4-D-001-BH-012…014).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OfferingReadiness {
    pub ready: bool,
    pub problems: Vec<String>,
}

// ================================================================== validation

/// Validates Domain B references of a new offering (L4-D-001-BH-001/002).
pub struct OfferingRefs {
    pub academic_year_id: Uuid,
    pub component: sutra_contracts::institution::reference::CurriculumComponentRef,
}

#[allow(clippy::too_many_arguments)]
pub async fn validate_refs(
    st: &AcademicState,
    scope: &ExecutionScope,
    institution: InstitutionId,
    term_id: Uuid,
    curriculum_version_id: Uuid,
    component_id: Uuid,
    campus_id: Option<Uuid>,
    org_unit_id: Option<Uuid>,
) -> Result<OfferingRefs, KernelError> {
    let cv = st
        .references
        .curriculum_version(scope, curriculum_version_id)
        .await?
        .filter(|c| c.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                "curriculumVersionId",
                "not_found",
                "no such curriculum version in this institution",
            )
        })?;
    if cv.status != PublicationStatus::Published {
        return Err(invalid(
            "curriculumVersionId",
            "not_published",
            "offerings need a published curriculum version",
        ));
    }
    let component = st
        .references
        .curriculum_components(scope, curriculum_version_id)
        .await?
        .into_iter()
        .find(|c| c.id == component_id)
        .ok_or_else(|| {
            invalid(
                "curriculumComponentId",
                "not_found",
                "not a component of this curriculum version",
            )
        })?;
    let term = st
        .references
        .academic_term(scope, term_id)
        .await?
        .filter(|t| t.institution_id == institution)
        .ok_or_else(|| {
            invalid(
                "academicTermId",
                "not_found",
                "no such academic term in this institution",
            )
        })?;
    if term.calendar_version_status != PublicationStatus::Published {
        return Err(invalid(
            "academicTermId",
            "not_published",
            "the term belongs to an unpublished calendar version",
        ));
    }
    let today = business_today(st, scope, institution).await?;
    if term.ends_on < today {
        return Err(KernelError::Conflict(format!(
            "the academic term {} ended on {}; it no longer accepts offerings",
            term.code, term.ends_on
        )));
    }
    if let Some(c) = campus_id {
        let campus = st
            .references
            .campus(scope, c)
            .await?
            .filter(|x| x.institution_id == institution)
            .ok_or_else(|| {
                invalid(
                    "campusId",
                    "not_found",
                    "no such campus in this institution",
                )
            })?;
        if campus.status != CampusStatus::Active {
            return Err(invalid(
                "campusId",
                "not_active",
                "the campus is not active",
            ));
        }
    }
    if let Some(u) = org_unit_id {
        let unit = st
            .references
            .org_unit(scope, u)
            .await?
            .filter(|x| x.institution_id == institution)
            .ok_or_else(|| {
                invalid(
                    "orgUnitId",
                    "not_found",
                    "no such org unit in this institution",
                )
            })?;
        if unit.status != OrgUnitStatus::Active {
            return Err(invalid(
                "orgUnitId",
                "not_active",
                "the org unit is not active",
            ));
        }
    }
    Ok(OfferingRefs {
        academic_year_id: term.academic_year_id,
        component,
    })
}

/// Inserts a draft offering row (no event); returns it.
#[allow(clippy::too_many_arguments)]
pub async fn insert_offering(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    institution: InstitutionId,
    refs: &OfferingRefs,
    term_id: Uuid,
    curriculum_version_id: Uuid,
    code: &str,
    title: &str,
    campus_id: Option<Uuid>,
    org_unit_id: Option<Uuid>,
    delivery_mode: &str,
    capacity: Option<i32>,
    status: CourseOfferingStatus,
    split_from: Option<CourseOfferingId>,
) -> Result<OfferingRow, KernelError> {
    let sql = format!(
        "INSERT INTO academic.course_offering (tenant_id, institution_id, campus_id, org_unit_id, \
         academic_year_id, academic_term_id, curriculum_version_id, curriculum_component_id, \
         offering_code, title, course_code, category, credits, delivery_mode, capacity, status, split_from_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17) RETURNING {OFFERING_COLUMNS}"
    );
    let row = sqlx::query(&sql)
        .bind(scope.tenant_id.as_uuid())
        .bind(institution.as_uuid())
        .bind(campus_id)
        .bind(org_unit_id)
        .bind(refs.academic_year_id)
        .bind(term_id)
        .bind(curriculum_version_id)
        .bind(refs.component.id)
        .bind(code)
        .bind(title)
        .bind(&refs.component.course_code)
        .bind(&refs.component.category)
        .bind(refs.component.credits)
        .bind(delivery_mode)
        .bind(capacity)
        .bind(status.as_str())
        .bind(split_from.map(|x| x.as_uuid()))
        .fetch_one(&mut *conn)
        .await
        .map_err(|e| {
            unique_code(
                e,
                "offeringCode",
                "an offering with this code is already live in the term",
            )
        })?;
    offering_from_row(&row).map_err(map_db_error)
}

/// Maps a unique violation to a 409 with `msg`.
pub fn unique_code(e: sqlx::Error, _field: &str, msg: &str) -> KernelError {
    if let sqlx::Error::Database(d) = &e {
        if d.code().as_deref() == Some("23505") {
            return KernelError::Conflict(msg.to_owned());
        }
    }
    map_db_error(e)
}

// ================================================================== create / read / update

/// Establish a course offering in `draft` (L3-D-001, `offering.manage`).
#[utoipa::path(
    post,
    path = "/api/v1/academic/institutions/{id}/offerings",
    operation_id = "offering_create",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid, description = "Institution id"),
        ("Idempotency-Key" = String, Header, description = "Required. Retry-safe key."),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = CreateOffering,
    responses(
        (status = 201, body = Offering, headers(("ETag" = String))),
        (status = 403, body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, description = "Duplicate code, closed term or read-only institution", body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn create(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
    headers: HeaderMap,
    Json(cmd): Json<CreateOffering>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    require_idempotency_key(&headers)?;
    let o = create_offering(&st, ctx, id, cmd).await?;
    Ok(etag(StatusCode::CREATED, &o))
}

pub async fn create_offering(
    st: &AcademicState,
    ctx: &AuthContext,
    institution: InstitutionId,
    cmd: CreateOffering,
) -> Result<Offering, KernelError> {
    let scope = institution_scope(ctx, institution);
    require_institution_writable(st, &scope, institution).await?;
    let code = check_code("offeringCode", &cmd.offering_code, 40, true)?;
    let mode = check_delivery_mode(cmd.delivery_mode.as_deref().unwrap_or("in_person"))?;
    let capacity = check_capacity("capacity", cmd.capacity)?;
    let component_id = match (cmd.curriculum_component_id, cmd.slot_code.as_deref()) {
        (Some(c), _) => c,
        (None, Some(slot)) => st
            .references
            .curriculum_components(&scope, cmd.curriculum_version_id)
            .await?
            .into_iter()
            .find(|c| c.slot_code.eq_ignore_ascii_case(slot.trim()))
            .map(|c| c.id)
            .ok_or_else(|| {
                invalid(
                    "slotCode",
                    "not_found",
                    "no component with this slot code in the curriculum version",
                )
            })?,
        (None, None) => {
            return Err(invalid(
                "curriculumComponentId",
                "required",
                "give curriculumComponentId or slotCode",
            ))
        }
    };
    let refs = validate_refs(
        st,
        &scope,
        institution,
        cmd.academic_term_id,
        cmd.curriculum_version_id,
        component_id,
        cmd.campus_id,
        cmd.org_unit_id,
    )
    .await?;
    let title = match cmd.title.as_deref() {
        Some(t) => check_title("title", t, 200)?,
        None => refs.component.title.clone(),
    };
    let a = actor(ctx);
    let mut tx = st.pool.begin().await.map_err(map_db_error)?;
    let o = insert_offering(
        &mut tx,
        &scope,
        institution,
        &refs,
        cmd.academic_term_id,
        cmd.curriculum_version_id,
        &code,
        &title,
        cmd.campus_id,
        cmd.org_unit_id,
        &mode,
        capacity,
        CourseOfferingStatus::Draft,
        None,
    )
    .await?;
    for s in &cmd.sections {
        insert_section(st, &mut tx, &scope, &o, s).await?;
    }
    crate::shared::publish::audit(
        &mut tx,
        &scope,
        "academic.course_offering.create",
        "course_offering",
        o.id.to_string(),
        serde_json::json!({"offeringCode": o.offering_code, "sections": cmd.sections.len()}),
        &a,
    )
    .await?;
    let v = offering_view(&mut tx, &o).await?;
    tx.commit().await.map_err(map_db_error)?;
    Ok(v)
}

/// Course offerings of an institution (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/institutions/{id}/offerings",
    operation_id = "offering_list",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid, description = "Institution id"), OfferingQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = OfferingList),
        (status = 403, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn list(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<OfferingQuery>,
) -> ApiResult<Json<OfferingList>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let items = list_offerings(&st, tenant(ctx), id, q.status, q.academic_term_id).await?;
    Ok(Json(OfferingList { items }))
}

/// One course offering with sections, faculty and seat counts (`academic.read`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}",
    operation_id = "offering_get",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = Offering, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::AcademicRead)?;
    Ok(etag(
        StatusCode::OK,
        &load_view(&st, tenant(ctx), id).await?,
    ))
}

/// Edit a draft offering (code, title, delivery mode, campus, org unit, capacity). Later changes
/// go through governed revisions (L3-D-009).
#[utoipa::path(
    patch,
    path = "/api/v1/academic/offerings/{id}",
    operation_id = "offering_update",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the offering"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = UpdateOffering,
    responses(
        (status = 200, body = Offering, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn update(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(cmd): Json<UpdateOffering>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    let v = expect_version!(headers);
    let mut c = begin(&st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::Draft {
        return Err(conflict(format!(
            "the course offering is {}; only drafts are edited directly — request a revision instead",
            c.o.status
        ))
        .into());
    }
    let code = match &cmd.offering_code {
        Some(x) => check_code("offeringCode", x, 40, true)?,
        None => c.o.offering_code.clone(),
    };
    let title = match &cmd.title {
        Some(x) => check_title("title", x, 200)?,
        None => c.o.title.clone(),
    };
    let mode = match &cmd.delivery_mode {
        Some(x) => check_delivery_mode(x)?,
        None => c.o.delivery_mode.clone(),
    };
    let capacity = match cmd.capacity {
        Some(x) => check_capacity("capacity", Some(x))?,
        None => c.o.capacity,
    };
    if cmd.campus_id.is_some() || cmd.org_unit_id.is_some() {
        validate_refs(
            &st,
            &c.scope,
            c.o.institution_id,
            c.o.academic_term_id,
            c.o.curriculum_version_id,
            c.o.curriculum_component_id,
            cmd.campus_id,
            cmd.org_unit_id,
        )
        .await?;
    }
    sqlx::query(
        "UPDATE academic.course_offering SET offering_code = $2, title = $3, delivery_mode = $4, \
         campus_id = COALESCE($5, campus_id), org_unit_id = COALESCE($6, org_unit_id), capacity = $7, \
         version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(c.o.id.as_uuid())
    .bind(&code)
    .bind(&title)
    .bind(&mode)
    .bind(cmd.campus_id)
    .bind(cmd.org_unit_id)
    .bind(capacity)
    .execute(&mut *c.tx)
    .await
    .map_err(|e| unique_code(e, "offeringCode", "an offering with this code is already live in the term"))?;
    c.audit(
        "academic.course_offering.update",
        serde_json::json!({"offeringCode": code}),
    )
    .await?;
    Ok(etag(StatusCode::OK, &c.finish().await?))
}

// ================================================================== state commands

/// Changes the offering status inside the command and emits a `StatusChangedV1` event.
pub async fn set_status(
    c: &mut Cmd,
    to: CourseOfferingStatus,
    ty: &'static str,
    schema: &'static str,
    reason: Option<&str>,
    pid: Option<&str>,
) -> Result<(), KernelError> {
    let from = c.o.status;
    c.o = change_offering_status(
        &mut c.tx,
        &c.o,
        to,
        &StatusChange {
            cause_event_type: ty,
            reason,
            process_instance_id: pid,
        },
        &c.actor,
    )
    .await?;
    let data = status_payload(&c.o, Some(from), reason);
    offering_event(&mut c.tx, &c.scope, &c.o, ty, schema, &data, &c.actor).await
}

/// Moves every section in `from` to `to` (e.g. planned → active when enrolment opens).
pub async fn move_sections(
    c: &mut Cmd,
    from: &[SectionStatus],
    to: SectionStatus,
    cause: &'static str,
    reason: Option<&str>,
) -> Result<usize, KernelError> {
    let mut n = 0;
    for s in sections_of(&mut c.tx, c.o.tenant_id, c.o.id).await? {
        if from.contains(&s.status) {
            let s = lock_section(&mut c.tx, c.o.tenant_id, s.id).await?;
            change_section_status(
                &mut c.tx,
                &s,
                to,
                &StatusChange {
                    cause_event_type: cause,
                    reason,
                    process_instance_id: None,
                },
                &c.actor,
            )
            .await?;
            n += 1;
        }
    }
    Ok(n)
}

/// Ends the live faculty assignments of an offering (or one section): active → `active_to`
/// (completed / revoked), not yet active → withdrawn / rejected. Emits `faculty_assignment.ended`
/// for active ones.
#[allow(clippy::too_many_arguments)]
pub async fn end_assignments(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    o: &OfferingRow,
    section: Option<OfferingSectionId>,
    active_to: FacultyAssignmentStatus,
    cause: &'static str,
    reason: Option<&str>,
    actor: &Actor,
) -> Result<(), KernelError> {
    for a in assignments_of_offering(&mut *conn, o.tenant_id, o.id).await? {
        if section.is_some_and(|s| s != a.section_id) {
            continue;
        }
        let to = match a.status {
            FacultyAssignmentStatus::Active => active_to,
            FacultyAssignmentStatus::Draft
            | FacultyAssignmentStatus::Submitted
            | FacultyAssignmentStatus::Approved => FacultyAssignmentStatus::Withdrawn,
            FacultyAssignmentStatus::UnderReview => FacultyAssignmentStatus::Rejected,
            _ => continue,
        };
        let row = lock_assignment(&mut *conn, o.tenant_id, a.id).await?;
        let row = change_assignment_status(
            &mut *conn,
            &row,
            to,
            &StatusChange {
                cause_event_type: cause,
                reason,
                process_instance_id: None,
            },
            actor,
        )
        .await?;
        if a.status == FacultyAssignmentStatus::Active {
            emit_assignment_ended(&mut *conn, scope, &row, actor).await?;
        }
    }
    Ok(())
}

pub async fn emit_assignment_ended(
    conn: &mut PgConnection,
    scope: &ExecutionScope,
    a: &crate::shared::offering::FacultyAssignmentRow,
    actor: &Actor,
) -> Result<(), KernelError> {
    crate::shared::publish::event(
        conn,
        scope,
        ev::FACULTY_ASSIGNMENT_ENDED,
        ev::FACULTY_ASSIGNMENT_ENDED_SCHEMA,
        a.id.to_string(),
        &assignment_v1(a),
        actor,
    )
    .await
}

pub fn assignment_v1(a: &crate::shared::offering::FacultyAssignmentRow) -> FacultyAssignmentV1 {
    FacultyAssignmentV1 {
        assignment_id: a.id.as_uuid(),
        section_id: a.section_id.as_uuid(),
        offering_id: a.offering_id.as_uuid(),
        institution_id: a.institution_id.as_uuid(),
        faculty_subject_ref: a.faculty_subject_ref.clone(),
        role: a.role.as_str().to_owned(),
        status: a.status.as_str().to_owned(),
        version: a.version.0,
    }
}

/// Ends every seated enrolment of the offering with `to` (e.g. completed).
pub async fn end_seats(
    c: &mut Cmd,
    to: SectionEnrolmentStatus,
    cause: &'static str,
    reason: Option<&str>,
) -> Result<usize, KernelError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM academic.section_enrolment WHERE offering_id = $1 AND status = 'enrolled'",
    )
    .bind(c.o.id.as_uuid())
    .fetch_all(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    for id in &ids {
        let e = lock_enrolment(&mut c.tx, c.o.tenant_id, SectionEnrolmentId(*id)).await?;
        change_enrolment_status(
            &mut c.tx,
            &e,
            to,
            &StatusChange {
                cause_event_type: cause,
                reason,
                process_instance_id: None,
            },
            &c.actor,
        )
        .await?;
    }
    Ok(ids.len())
}

/// Removes live waitlist entries of the offering (cancellation, merge, archive guard).
pub async fn clear_waitlist(c: &mut Cmd, reason: &str) -> Result<u64, KernelError> {
    let r = sqlx::query(
        "UPDATE academic.offering_waitlist_entry SET status = 'removed', status_reason = $2, \
         resolved_at = now(), version = version + 1, updated_at = now() \
         WHERE offering_id = $1 AND status IN ('waiting', 'offered')",
    )
    .bind(c.o.id.as_uuid())
    .bind(reason)
    .execute(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    Ok(r.rows_affected())
}

/// Effective faculty assignment policy of an institution: `(max_weekly_hours,
/// allowed_roles, require_primary_instructor)`.
pub async fn effective_policy(
    conn: &mut PgConnection,
    tenant: sutra_kernel::scope::TenantId,
    institution: InstitutionId,
) -> Result<Option<(Option<f64>, Vec<String>, bool)>, KernelError> {
    let r: Option<(Option<f64>, Vec<String>, bool)> = sqlx::query_as(
        "SELECT max_weekly_hours::float8, allowed_roles, require_primary_instructor \
         FROM academic.faculty_assignment_policy WHERE tenant_id = $1 AND institution_id = $2 AND status = 'effective'",
    )
    .bind(tenant.as_uuid())
    .bind(institution.as_uuid())
    .fetch_optional(&mut *conn)
    .await
    .map_err(map_db_error)?;
    Ok(r)
}

/// Activation / reactivation readiness: at least one active section; every active section is
/// taught by an active faculty assignment (a primary instructor when the effective policy says
/// so).
pub async fn readiness(
    conn: &mut PgConnection,
    o: &OfferingRow,
) -> Result<OfferingReadiness, KernelError> {
    let mut problems = Vec::new();
    let require_primary = effective_policy(&mut *conn, o.tenant_id, o.institution_id)
        .await?
        .is_none_or(|p| p.2);
    let sections = sections_of(&mut *conn, o.tenant_id, o.id).await?;
    let active: Vec<_> = sections
        .iter()
        .filter(|s| s.status == SectionStatus::Active)
        .collect();
    if active.is_empty() {
        problems.push("no active section".to_owned());
    }
    let assignments = assignments_of_offering(&mut *conn, o.tenant_id, o.id).await?;
    for s in active {
        let ok = assignments.iter().any(|a| {
            a.section_id == s.id
                && a.status == FacultyAssignmentStatus::Active
                && (!require_primary || a.role == FacultyRole::PrimaryInstructor)
        });
        if !ok {
            problems.push(format!(
                "section {} has no active {}",
                s.section_code,
                if require_primary {
                    "primary instructor"
                } else {
                    "faculty assignment"
                }
            ));
        }
    }
    Ok(OfferingReadiness {
        ready: problems.is_empty(),
        problems,
    })
}

async fn require_ready(c: &mut Cmd) -> Result<(), KernelError> {
    let r = readiness(&mut c.tx, &c.o).await?;
    if r.ready {
        Ok(())
    } else {
        Err(conflict(format!(
            "the course offering is not ready: {}",
            r.problems.join("; ")
        )))
    }
}

/// Submit a draft for approval through the WorkflowPort (`course_offering_approval`).
pub async fn submit_offering(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
) -> Result<Offering, KernelError> {
    // 1. validate (read only)
    let t = tenant(ctx);
    let o = crate::shared::offering::load_offering(&st.pool, t, id).await?;
    check_version(o.version, v)?;
    if o.status != CourseOfferingStatus::Draft {
        return Err(conflict(format!(
            "the course offering is {}; only drafts are submitted",
            o.status
        )));
    }
    let scope = institution_scope(ctx, o.institution_id);
    let live: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM academic.offering_section WHERE offering_id = $1 AND status NOT IN ('cancelled', 'merged')",
    )
    .bind(o.id.as_uuid())
    .fetch_one(&st.pool)
    .await
    .map_err(map_db_error)?;
    if live == 0 {
        return Err(conflict(
            "the draft is incomplete: add at least one section before submitting",
        ));
    }
    // 2. approval before the tx (deterministic key)
    let outcome = request_approval(
        st,
        &scope,
        process::COURSE_OFFERING_APPROVAL,
        business_key("course_offering", o.id, o.version.0),
        BTreeMap::from([
            (
                "offeringCode".to_owned(),
                serde_json::json!(o.offering_code),
            ),
            (
                "institutionId".to_owned(),
                serde_json::json!(o.institution_id),
            ),
        ]),
    )
    .await?;
    // 3. one tx
    let mut c = begin(st, ctx, id, Some(v)).await?;
    let pid = outcome.process_instance_id().to_owned();
    // The reason tells the (pure) inbox rule whether a human decision is awaited.
    let note = match outcome {
        ApprovalOutcome::Approved { .. } => "auto-approved",
        ApprovalOutcome::Pending { .. } => "awaiting decision",
    };
    set_status(
        &mut c,
        CourseOfferingStatus::PendingApproval,
        ev::COURSE_OFFERING_SUBMITTED,
        ev::COURSE_OFFERING_SUBMITTED_SCHEMA,
        Some(note),
        Some(&pid),
    )
    .await?;
    c.audit(
        "academic.course_offering.submit",
        serde_json::json!({"processInstanceId": pid}),
    )
    .await?;
    if let ApprovalOutcome::Approved { .. } = outcome {
        let system = Actor::system(c.actor.correlation_id.clone());
        let user = std::mem::replace(&mut c.actor, system);
        set_status(
            &mut c,
            CourseOfferingStatus::Approved,
            ev::COURSE_OFFERING_APPROVED,
            ev::COURSE_OFFERING_APPROVED_SCHEMA,
            Some("auto-approved"),
            Some(&pid),
        )
        .await?;
        c.audit(
            "academic.course_offering.approve",
            serde_json::json!({"processInstanceId": pid}),
        )
        .await?;
        c.actor = user;
    }
    c.finish().await
}

/// Explicit decision on a pending offering (approve → approved; reject → draft with reason).
pub async fn decide_offering(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    d: StepDecisionBody,
) -> Result<Offering, KernelError> {
    let reason = clean_reason(d.reason, d.decision == StepDecision::Reject)?;
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::PendingApproval {
        return Err(conflict(format!(
            "the course offering is {}; nothing awaits a decision",
            c.o.status
        )));
    }
    let pid = c.o.process_instance_id.clone();
    match d.decision {
        StepDecision::Approve => {
            set_status(
                &mut c,
                CourseOfferingStatus::Approved,
                ev::COURSE_OFFERING_APPROVED,
                ev::COURSE_OFFERING_APPROVED_SCHEMA,
                reason.as_deref(),
                pid.as_deref(),
            )
            .await?;
            c.audit(
                "academic.course_offering.approve",
                serde_json::json!({"reason": reason}),
            )
            .await?;
        }
        StepDecision::Reject => {
            let from = c.o.status;
            c.o = change_offering_status(
                &mut c.tx,
                &c.o,
                CourseOfferingStatus::Draft,
                &StatusChange {
                    cause_event_type: ev::COURSE_OFFERING_SUBMITTED,
                    reason: reason.as_deref(),
                    process_instance_id: pid.as_deref(),
                },
                &c.actor,
            )
            .await?;
            c.audit(
                "academic.course_offering.reject",
                serde_json::json!({"reason": reason, "from": from.as_str()}),
            )
            .await?;
        }
    }
    c.finish().await
}

/// approved → published (prerequisites: a live section and a defined capacity).
pub async fn publish_offering(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::Approved {
        return Err(conflict(format!(
            "the course offering is {}; only approved offerings are published",
            c.o.status
        )));
    }
    let sections = sections_of(&mut c.tx, c.o.tenant_id, c.o.id).await?;
    let live: Vec<_> = sections
        .iter()
        .filter(|s| matches!(s.status, SectionStatus::Planned | SectionStatus::Active))
        .collect();
    if live.is_empty() {
        return Err(conflict(
            "publication prerequisites not met: no live section",
        ));
    }
    if c.o.capacity.is_none() && live.iter().any(|s| s.capacity.is_none()) {
        return Err(conflict(
            "publication prerequisites not met: set the offering capacity or every section's capacity",
        ));
    }
    let from = c.o.status;
    c.o = change_offering_status(
        &mut c.tx,
        &c.o,
        CourseOfferingStatus::Published,
        &StatusChange {
            cause_event_type: ev::COURSE_OFFERING_PUBLISHED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &c.actor,
    )
    .await?;
    let data = offering_v1(&c.o);
    offering_event(
        &mut c.tx,
        &c.scope,
        &c.o,
        ev::COURSE_OFFERING_PUBLISHED,
        ev::COURSE_OFFERING_PUBLISHED_SCHEMA,
        &data,
        &c.actor,
    )
    .await?;
    c.audit(
        "academic.course_offering.publish",
        serde_json::json!({"from": from.as_str()}),
    )
    .await?;
    c.finish().await
}

/// Opens enrolment on the command's offering (published / enrollment_closed → enrollment_open):
/// planned sections become active; a scheduled window covering today opens.
pub async fn open_enrolment_in(
    c: &mut Cmd,
    st: &AcademicState,
    reason: Option<&str>,
) -> Result<(), KernelError> {
    if !matches!(
        c.o.status,
        CourseOfferingStatus::Published | CourseOfferingStatus::EnrollmentClosed
    ) {
        return Err(conflict(format!(
            "the course offering is {}; enrolment opens only on published (or closed) offerings",
            c.o.status
        )));
    }
    move_sections(
        c,
        &[SectionStatus::Planned],
        SectionStatus::Active,
        ev::COURSE_OFFERING_ENROLLMENT_OPENED,
        reason,
    )
    .await?;
    let today = business_today(st, &c.scope, c.o.institution_id).await?;
    sqlx::query(
        "UPDATE academic.offering_enrolment_window SET status = 'open', version = version + 1, updated_at = now() \
         WHERE offering_id = $1 AND status = 'scheduled' AND opens_on <= $2 AND closes_on >= $2",
    )
    .bind(c.o.id.as_uuid())
    .bind(today)
    .execute(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    set_status(
        c,
        CourseOfferingStatus::EnrollmentOpen,
        ev::COURSE_OFFERING_ENROLLMENT_OPENED,
        ev::COURSE_OFFERING_ENROLLMENT_OPENED_SCHEMA,
        reason,
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.open_enrolment",
        serde_json::json!({"reason": reason}),
    )
    .await
}

/// Closes enrolment (enrollment_open → enrollment_closed); open windows close.
pub async fn close_enrolment_in(c: &mut Cmd, reason: Option<&str>) -> Result<(), KernelError> {
    if c.o.status != CourseOfferingStatus::EnrollmentOpen {
        return Err(conflict(format!(
            "the course offering is {}; enrolment is not open",
            c.o.status
        )));
    }
    sqlx::query(
        "UPDATE academic.offering_enrolment_window SET status = 'closed', version = version + 1, updated_at = now() \
         WHERE offering_id = $1 AND status IN ('open', 'suspended')",
    )
    .bind(c.o.id.as_uuid())
    .execute(&mut *c.tx)
    .await
    .map_err(map_db_error)?;
    set_status(
        c,
        CourseOfferingStatus::EnrollmentClosed,
        ev::COURSE_OFFERING_ENROLLMENT_CLOSED,
        ev::COURSE_OFFERING_ENROLLMENT_CLOSED_SCHEMA,
        reason,
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.close_enrolment",
        serde_json::json!({"reason": reason}),
    )
    .await
}

pub async fn open_enrolment(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    open_enrolment_in(&mut c, st, reason.as_deref()).await?;
    c.finish().await
}

pub async fn close_enrolment(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    close_enrolment_in(&mut c, reason.as_deref()).await?;
    c.finish().await
}

/// enrollment_closed → active when readiness succeeds (failed readiness keeps enrollment_closed).
pub async fn activate(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::EnrollmentClosed {
        return Err(conflict(format!(
            "the course offering is {}; activation follows the enrolment close",
            c.o.status
        )));
    }
    require_ready(&mut c).await?;
    let from = c.o.status;
    c.o = change_offering_status(
        &mut c.tx,
        &c.o,
        CourseOfferingStatus::Active,
        &StatusChange {
            cause_event_type: ev::COURSE_OFFERING_ACTIVATED,
            reason: reason.as_deref(),
            process_instance_id: None,
        },
        &c.actor,
    )
    .await?;
    let data = offering_v1(&c.o);
    offering_event(
        &mut c.tx,
        &c.scope,
        &c.o,
        ev::COURSE_OFFERING_ACTIVATED,
        ev::COURSE_OFFERING_ACTIVATED_SCHEMA,
        &data,
        &c.actor,
    )
    .await?;
    c.audit(
        "academic.course_offering.activate",
        serde_json::json!({"from": from.as_str()}),
    )
    .await?;
    c.finish().await
}

/// active → suspended (reason required; seats and registrations are untouched).
pub async fn suspend(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::Active {
        return Err(conflict(format!(
            "the course offering is {}; only active offerings are suspended",
            c.o.status
        )));
    }
    set_status(
        &mut c,
        CourseOfferingStatus::Suspended,
        ev::COURSE_OFFERING_SUSPENDED,
        ev::COURSE_OFFERING_SUSPENDED_SCHEMA,
        reason.as_deref(),
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.suspend",
        serde_json::json!({"reason": reason}),
    )
    .await?;
    c.finish().await
}

/// suspended → active once the suspension reason is resolved (resolution required) and
/// readiness holds.
pub async fn reactivate(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::Suspended {
        return Err(conflict(format!(
            "the course offering is {}; only suspended offerings are reactivated",
            c.o.status
        )));
    }
    require_ready(&mut c).await?;
    set_status(
        &mut c,
        CourseOfferingStatus::Active,
        ev::COURSE_OFFERING_REACTIVATED,
        ev::COURSE_OFFERING_REACTIVATED_SCHEMA,
        reason.as_deref(),
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.reactivate",
        serde_json::json!({"resolution": reason}),
    )
    .await?;
    c.finish().await
}

/// active → frozen (reason required). The pre-freeze position (`active`) is preserved in the
/// status history and restored by the governed unfreeze.
pub async fn freeze(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::Active {
        return Err(conflict(format!(
            "the course offering is {}; only active offerings are frozen",
            c.o.status
        )));
    }
    set_status(
        &mut c,
        CourseOfferingStatus::Frozen,
        ev::COURSE_OFFERING_FROZEN,
        ev::COURSE_OFFERING_FROZEN_SCHEMA,
        reason.as_deref(),
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.freeze",
        serde_json::json!({"reason": reason, "preFreezeStatus": "active"}),
    )
    .await?;
    c.finish().await
}

/// active → completed: active sections complete, seats complete, active faculty assignments
/// complete.
pub async fn complete(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if c.o.status != CourseOfferingStatus::Active {
        return Err(conflict(format!(
            "the course offering is {}; only active offerings complete",
            c.o.status
        )));
    }
    let r = reason.as_deref();
    end_seats(
        &mut c,
        SectionEnrolmentStatus::Completed,
        ev::COURSE_OFFERING_COMPLETED,
        r,
    )
    .await?;
    let (scope, o, a) = (c.scope, c.o.clone(), c.actor.clone());
    end_assignments(
        &mut c.tx,
        &scope,
        &o,
        None,
        FacultyAssignmentStatus::Completed,
        ev::COURSE_OFFERING_COMPLETED,
        r,
        &a,
    )
    .await?;
    move_sections(
        &mut c,
        &[SectionStatus::Active],
        SectionStatus::Completed,
        ev::COURSE_OFFERING_COMPLETED,
        r,
    )
    .await?;
    move_sections(
        &mut c,
        &[SectionStatus::Planned],
        SectionStatus::Cancelled,
        ev::COURSE_OFFERING_COMPLETED,
        Some("offering completed"),
    )
    .await?;
    clear_waitlist(&mut c, "offering completed").await?;
    set_status(
        &mut c,
        CourseOfferingStatus::Completed,
        ev::COURSE_OFFERING_COMPLETED,
        ev::COURSE_OFFERING_COMPLETED_SCHEMA,
        r,
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.complete",
        serde_json::json!({"reason": reason}),
    )
    .await?;
    c.finish().await
}

/// completed | cancelled → archived when nothing blocks (no seat, no live waitlist entry, no
/// open governed request). Archived offerings are immutable (DB trigger).
pub async fn archive(
    st: &AcademicState,
    ctx: &AuthContext,
    id: CourseOfferingId,
    v: Version,
    reason: Option<String>,
) -> Result<Offering, KernelError> {
    let mut c = begin(st, ctx, id, Some(v)).await?;
    if !matches!(
        c.o.status,
        CourseOfferingStatus::Completed | CourseOfferingStatus::Cancelled
    ) {
        return Err(conflict(format!(
            "the course offering is {}; only completed or cancelled offerings are archived",
            c.o.status
        )));
    }
    let mut blocking = Vec::new();
    if seated_in_offering(&mut *c.tx, c.o.id).await? > 0 {
        blocking.push("students are still seated");
    }
    let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.offering_waitlist_entry WHERE offering_id = $1 AND status IN ('waiting', 'offered')")
        .bind(c.o.id.as_uuid()).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    if waiting > 0 {
        blocking.push("waitlist entries are unresolved");
    }
    let open: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM academic.offering_revision WHERE offering_id = $1 AND status IN ('requested', 'validated', 'approved')) \
              + (SELECT count(*) FROM academic.offering_reorganisation WHERE offering_id = $1 AND status IN ('requested', 'validated', 'approved'))",
    )
    .bind(c.o.id.as_uuid()).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    if open > 0 {
        blocking.push("governed change requests are still open");
    }
    let live_assign: i64 = sqlx::query_scalar("SELECT count(*) FROM academic.faculty_assignment WHERE offering_id = $1 AND status IN ('draft', 'submitted', 'under_review', 'approved', 'active')")
        .bind(c.o.id.as_uuid()).fetch_one(&mut *c.tx).await.map_err(map_db_error)?;
    if live_assign > 0 {
        blocking.push("faculty assignments are still live");
    }
    if !blocking.is_empty() {
        return Err(conflict(format!(
            "archival is blocked: {}",
            blocking.join("; ")
        )));
    }
    set_status(
        &mut c,
        CourseOfferingStatus::Archived,
        ev::COURSE_OFFERING_ARCHIVED,
        ev::COURSE_OFFERING_ARCHIVED_SCHEMA,
        reason.as_deref(),
        None,
    )
    .await?;
    c.audit(
        "academic.course_offering.archive",
        serde_json::json!({"reason": reason}),
    )
    .await?;
    c.finish().await
}

/// Activation readiness of an offering (`academic.read`; read only).
#[utoipa::path(
    get,
    path = "/api/v1/academic/offerings/{id}/readiness",
    operation_id = "offering_readiness",
    tag = "offerings",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = OfferingReadiness),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn get_readiness(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
) -> ApiResult<Json<OfferingReadiness>> {
    let ctx = auth.require(Permission::AcademicRead)?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let o = crate::shared::offering::load_offering(&mut *conn, tenant(ctx), id).await?;
    Ok(Json(readiness(&mut conn, &o).await?))
}

// ================================================================== handlers

macro_rules! offering_command {
    ($fn:ident, $path:literal, $op:literal, $doc:literal, $perm:expr, $reason_required:expr, $logic:path) => {
        #[doc = $doc]
        #[utoipa::path(
            post,
            path = $path,
            operation_id = $op,
            tag = "offerings",
            params(
                ("id" = String, Path, format = Uuid),
                ("If-Match" = String, Header, description = "ETag of the offering"),
                ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
            ),
            security(("session_cookie" = [], "csrf_token" = [])),
            request_body(content = Option<StepReason>),
            responses(
                (status = 200, body = Offering, headers(("ETag" = String))),
                (status = 403, body = Problem, content_type = "application/problem+json"),
                (status = 404, body = Problem, content_type = "application/problem+json"),
                (status = 409, description = "Illegal transition or prerequisites not met", body = Problem, content_type = "application/problem+json"),
                (status = 412, body = Problem, content_type = "application/problem+json"),
                (status = 422, body = Problem, content_type = "application/problem+json"),
                (status = 428, body = Problem, content_type = "application/problem+json"),
            )
        )]
        pub async fn $fn(
            State(st): State<AcademicState>,
            auth: Authenticated,
            Path(id): Path<CourseOfferingId>,
            headers: HeaderMap,
            raw: Bytes,
        ) -> ApiResult<Response> {
            let ctx = auth.require($perm)?;
            let v = expect_version!(headers);
            let reason = body::<StepReason>(&raw)?.validated($reason_required)?;
            let o = $logic(&st, ctx, id, v, reason).await?;
            Ok(etag(StatusCode::OK, &o))
        }
    };
}

/// Submit a draft offering for approval through the WorkflowPort (L3-D-001, `offering.manage`).
/// With auto-approval (Batch 4) the offering comes back `approved`.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/submit",
    operation_id = "offering_submit",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the offering"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    responses(
        (status = 200, body = Offering, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn submit(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingManage)?;
    let v = expect_version!(headers);
    Ok(etag(
        StatusCode::OK,
        &submit_offering(&st, ctx, id, v).await?,
    ))
}

/// Decide a pending offering (`offering.decide`): approve → approved, reject → draft (reason
/// required). Used when approvals await a human decision.
#[utoipa::path(
    post,
    path = "/api/v1/academic/offerings/{id}/decision",
    operation_id = "offering_decision",
    tag = "offerings",
    params(
        ("id" = String, Path, format = Uuid),
        ("If-Match" = String, Header, description = "ETag of the offering"),
        ("X-CSRF-Token" = String, Header, description = "Required. Session CSRF token."),
    ),
    security(("session_cookie" = [], "csrf_token" = [])),
    request_body = StepDecisionBody,
    responses(
        (status = 200, body = Offering, headers(("ETag" = String))),
        (status = 404, body = Problem, content_type = "application/problem+json"),
        (status = 409, body = Problem, content_type = "application/problem+json"),
        (status = 412, body = Problem, content_type = "application/problem+json"),
        (status = 422, body = Problem, content_type = "application/problem+json"),
        (status = 428, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn decision(
    State(st): State<AcademicState>,
    auth: Authenticated,
    Path(id): Path<CourseOfferingId>,
    headers: HeaderMap,
    Json(d): Json<StepDecisionBody>,
) -> ApiResult<Response> {
    let ctx = auth.require(Permission::OfferingDecide)?;
    let v = expect_version!(headers);
    Ok(etag(
        StatusCode::OK,
        &decide_offering(&st, ctx, id, v, d).await?,
    ))
}

offering_command!(
    publish,
    "/api/v1/academic/offerings/{id}/publish",
    "offering_publish",
    "Publish an approved offering (`offering.decide`; needs a live section and a capacity).",
    Permission::OfferingDecide,
    false,
    publish_offering
);
offering_command!(open_enrolment_cmd, "/api/v1/academic/offerings/{id}/open-enrolment", "offering_open_enrolment",
    "Open enrolment (published / enrollment_closed → enrollment_open; planned sections activate) (`offering.decide`).",
    Permission::OfferingDecide, false, open_enrolment);
offering_command!(
    close_enrolment_cmd,
    "/api/v1/academic/offerings/{id}/close-enrolment",
    "offering_close_enrolment",
    "Close enrolment (enrollment_open → enrollment_closed) (`offering.decide`).",
    Permission::OfferingDecide,
    false,
    close_enrolment
);
offering_command!(activate_cmd, "/api/v1/academic/offerings/{id}/activate", "offering_activate",
    "Activate (enrollment_closed → active) when readiness holds: every active section has an active instructor (`offering.decide`).",
    Permission::OfferingDecide, false, activate);
offering_command!(
    suspend_cmd,
    "/api/v1/academic/offerings/{id}/suspend",
    "offering_suspend",
    "Suspend an active offering (L3-D-002, reason required, `offering.decide`).",
    Permission::OfferingDecide,
    true,
    suspend
);
offering_command!(reactivate_cmd, "/api/v1/academic/offerings/{id}/reactivate", "offering_reactivate",
    "Reactivate a suspended offering once its suspension reason is resolved (L3-D-003, resolution required, `offering.decide`).",
    Permission::OfferingDecide, true, reactivate);
offering_command!(freeze_cmd, "/api/v1/academic/offerings/{id}/freeze", "offering_freeze",
    "Freeze an active offering (L3-D-013, reason required, `offering.decide`); only a governed unfreeze restores it.",
    Permission::OfferingDecide, true, freeze);
offering_command!(complete_cmd, "/api/v1/academic/offerings/{id}/complete", "offering_complete",
    "Complete an active offering: seats, sections and faculty assignments complete (`offering.decide`).",
    Permission::OfferingDecide, false, complete);
offering_command!(archive_cmd, "/api/v1/academic/offerings/{id}/archive", "offering_archive",
    "Archive a completed or cancelled offering when nothing blocks (L3-D-010, `offering.lifecycle`, step-up).",
    Permission::OfferingLifecycle, false, archive);

/// Changeable gate re-exported for sibling files.
pub fn changeable(o: &OfferingRow) -> Result<(), KernelError> {
    require_offering_changeable(o.status)
}

/// Unused-import guard for ids only referenced in signatures of sibling modules.
#[allow(dead_code)]
fn _ids(_: FacultyAssignmentId) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readiness_serialises() {
        let r = OfferingReadiness {
            ready: false,
            problems: vec!["x".into()],
        };
        let v = serde_json::to_value(r).unwrap_or_default();
        assert_eq!(v["ready"], false);
    }
}
