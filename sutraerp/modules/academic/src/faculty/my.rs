//! Faculty "My classes" / "My students" read models (contract doc §5.3): the sections the
//! caller actively teaches and the students seated in them.

use crate::shared::scope::tenant;
use crate::shared::AcademicState;
use axum::extract::State;
use axum::Json;
use sutra_contracts::academic::status::{
    CourseOfferingStatus, FacultyAssignmentStatus, FacultyRole,
};
use sutra_kernel::scope::ExecutionScope;
use sutra_web::{ApiResult, Authenticated, Problem};
use uuid::Uuid;

/// A section the faculty member teaches.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyClass {
    pub section_id: Uuid,
    pub offering_id: Uuid,
    pub offering_code: String,
    pub title: String,
    pub section_code: String,
    pub academic_term_id: Uuid,
    pub role: FacultyRole,
    pub seated_count: i64,
    pub offering_status: CourseOfferingStatus,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MyClassList {
    pub items: Vec<MyClass>,
}

/// A student seated in one of the faculty member's sections.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MyStudent {
    pub student_id: Uuid,
    pub student_number: String,
    pub display_name: String,
    pub section_id: Uuid,
    pub offering_code: String,
    pub section_code: String,
}

#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct MyStudentList {
    pub items: Vec<MyStudent>,
}

/// Sections I actively teach (`academic.teach`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/my/classes",
    operation_id = "faculty_self_classes",
    tag = "faculty-assignments",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = MyClassList),
        (status = 403, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn classes(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyClassList>> {
    let sections = crate::shared::faculty::my_sections(&st, &auth.0).await?;
    let scope = ExecutionScope::tenant(tenant(&auth.0));
    let mut items = Vec::new();
    for s in sections {
        let faculty = st.offerings.faculty_of_section(&scope, s.id).await?;
        let role = faculty
            .iter()
            .find(|f| {
                f.status == FacultyAssignmentStatus::Active
                    && f.faculty_subject_ref == auth.0.subject
            })
            .map(|f| f.role)
            .unwrap_or(FacultyRole::PrimaryInstructor);
        let seated = st.offerings.roster(&scope, s.id, true).await?.len() as i64;
        items.push(MyClass {
            section_id: s.id.as_uuid(),
            offering_id: s.offering_id.as_uuid(),
            offering_code: s.offering_code.clone(),
            title: s.offering_title.clone(),
            section_code: s.section_code.clone(),
            academic_term_id: s.academic_term_id,
            role,
            seated_count: seated,
            offering_status: s.offering_status,
        });
    }
    items.sort_by(|a, b| {
        (a.offering_code.as_str(), a.section_code.as_str())
            .cmp(&(b.offering_code.as_str(), b.section_code.as_str()))
    });
    Ok(Json(MyClassList { items }))
}

/// Students seated in the sections I teach (`academic.teach`).
#[utoipa::path(
    get,
    path = "/api/v1/academic/my/students",
    operation_id = "faculty_self_students",
    tag = "faculty-assignments",
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = MyStudentList),
        (status = 403, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn students(
    State(st): State<AcademicState>,
    auth: Authenticated,
) -> ApiResult<Json<MyStudentList>> {
    let sections = crate::shared::faculty::my_sections(&st, &auth.0).await?;
    let scope = ExecutionScope::tenant(tenant(&auth.0));
    let mut items = Vec::new();
    for s in sections {
        for r in st.offerings.roster(&scope, s.id, true).await? {
            items.push(MyStudent {
                student_id: r.student_id.as_uuid(),
                student_number: r.student_number,
                display_name: r.display_name,
                section_id: s.id.as_uuid(),
                offering_code: s.offering_code.clone(),
                section_code: s.section_code.clone(),
            });
        }
    }
    items.sort_by(|a, b| {
        (
            a.offering_code.as_str(),
            a.section_code.as_str(),
            a.student_number.as_str(),
        )
            .cmp(&(
                b.offering_code.as_str(),
                b.section_code.as_str(),
                b.student_number.as_str(),
            ))
    });
    Ok(Json(MyStudentList { items }))
}
