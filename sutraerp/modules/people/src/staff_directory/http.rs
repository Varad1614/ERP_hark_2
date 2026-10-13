//! HTTP API of the staff directory (tag `people-directory`; contract §5.2).

use crate::shared::guard::require_institution;
use crate::shared::me::my_staff;
use crate::shared::rows::current_engagement;
use crate::shared::scope::institution_scope;
use crate::shared::PeopleState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sutra_contracts::people::ids::{StaffEngagementId, StaffMemberId};
use sutra_contracts::people::ports::{EngagementSummary, StaffSummary};
use sutra_contracts::people::status::{
    EmploymentType, EngagementStatus, StaffCategory, StaffStatus,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::InstitutionId;
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// An engagement as the directory shows it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct EngagementView {
    pub id: StaffEngagementId,
    #[schema(example = "ENG-000001")]
    pub engagement_number: String,
    pub org_unit_id: Option<Uuid>,
    pub campus_id: Option<Uuid>,
    pub staff_category: StaffCategory,
    pub employment_type: EmploymentType,
    pub designation: String,
    pub grade: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub last_working_date: Option<NaiveDate>,
    pub probation_end_date: Option<NaiveDate>,
    pub reports_to_staff_id: Option<StaffMemberId>,
    pub status: EngagementStatus,
    pub version: i64,
}

impl From<&EngagementSummary> for EngagementView {
    fn from(e: &EngagementSummary) -> Self {
        Self {
            id: e.id,
            engagement_number: e.engagement_number.clone(),
            org_unit_id: e.org_unit_id,
            campus_id: e.campus_id,
            staff_category: e.staff_category,
            employment_type: e.employment_type,
            designation: e.designation.clone(),
            grade: e.grade.clone(),
            start_date: e.start_date,
            end_date: e.end_date,
            last_working_date: e.last_working_date,
            probation_end_date: e.probation_end_date,
            reports_to_staff_id: e.reports_to_staff_id,
            status: e.status,
            version: e.version,
        }
    }
}

/// A staff record with its current engagement.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaffView {
    pub id: StaffMemberId,
    pub institution_id: Uuid,
    #[schema(example = "EMP-000001")]
    pub staff_number: String,
    pub subject_ref: Option<String>,
    pub given_name: String,
    pub family_name: Option<String>,
    pub display_name: String,
    pub work_email: Option<String>,
    pub staff_category: StaffCategory,
    pub status: StaffStatus,
    pub canonical_staff_id: Option<StaffMemberId>,
    pub engagement: Option<EngagementView>,
    pub version: i64,
}

impl StaffView {
    pub fn of(s: &StaffSummary, e: Option<&EngagementSummary>) -> Self {
        Self {
            id: s.id,
            institution_id: s.institution_id.as_uuid(),
            staff_number: s.staff_number.clone(),
            subject_ref: s.subject_ref.clone(),
            given_name: s.given_name.clone(),
            family_name: s.family_name.clone(),
            display_name: s.display_name.clone(),
            work_email: s.work_email.clone(),
            staff_category: s.staff_category,
            status: s.status,
            canonical_staff_id: s.canonical_staff_id,
            engagement: e.map(EngagementView::from),
            version: s.version,
        }
    }
}

/// Staff records of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StaffList {
    pub items: Vec<StaffView>,
    /// Staff number to continue after (`?cursor=`), when more rows exist.
    pub next_cursor: Option<String>,
}

/// Filters of the HR staff list.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct StaffListQuery {
    /// `StaffStatus` wire name.
    pub status: Option<String>,
    /// Case-insensitive match on name, staff number or work e-mail.
    pub q: Option<String>,
    /// ≤ 200 (default 50).
    pub limit: Option<i64>,
    /// Staff number to continue after.
    pub cursor: Option<String>,
}

/// One entry of the basic staff directory (what every staff member may see).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    pub staff_id: StaffMemberId,
    pub staff_number: String,
    pub display_name: String,
    pub staff_category: StaffCategory,
    pub designation: Option<String>,
    pub org_unit_id: Option<Uuid>,
    pub work_email: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct DirectoryList {
    pub items: Vec<DirectoryEntry>,
}

/// Filters of the staff directory.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DirectoryQuery {
    /// Case-insensitive match on name or designation.
    pub q: Option<String>,
}

fn invalid(field: &str, code: &str, message: &str) -> KernelError {
    KernelError::Validation(vec![FieldViolation::new(field, code, message)])
}

fn matches_q(q: Option<&str>, hay: &[Option<&str>]) -> bool {
    let Some(q) = q.map(str::trim).filter(|q| !q.is_empty()) else {
        return true;
    };
    let q = q.to_lowercase();
    hay.iter().flatten().any(|h| h.to_lowercase().contains(&q))
}

/// Staff records of an institution with their current engagement (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/staff", operation_id = "people_directory_staff_list", tag = "people-directory",
    params(("id" = String, Path, format = Uuid), StaffListQuery), security(("session_cookie" = [])),
    responses((status = 200, body = StaffList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn staff_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<StaffListQuery>,
) -> ApiResult<Json<StaffList>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let status = match q.status.as_deref() {
        None => None,
        Some(s) => Some(
            StaffStatus::parse(s)
                .ok_or_else(|| invalid("status", "invalid", "unknown staff status"))?,
        ),
    };
    let limit = usize::try_from(q.limit.unwrap_or(50).clamp(1, 200)).unwrap_or(50);
    let all = st
        .directory
        .staff_of_institution(&scope, institution, status)
        .await?;
    let mut rows: Vec<StaffSummary> = all
        .into_iter()
        .filter(|s| {
            q.cursor
                .as_deref()
                .is_none_or(|c| s.staff_number.as_str() > c)
        })
        .filter(|s| {
            matches_q(
                q.q.as_deref(),
                &[
                    Some(s.display_name.as_str()),
                    Some(s.staff_number.as_str()),
                    s.work_email.as_deref(),
                ],
            )
        })
        .collect();
    let next_cursor = if rows.len() > limit {
        rows.truncate(limit);
        rows.last().map(|s| s.staff_number.clone())
    } else {
        None
    };
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for s in &rows {
        let e = current_engagement(&mut conn, ctx.scope.tenant_id, s.id).await?;
        items.push(StaffView::of(s, e.as_ref()));
    }
    Ok(Json(StaffList { items, next_cursor }))
}

/// One staff record with its current engagement (`people.read`).
#[utoipa::path(get, path = "/api/v1/people/staff/{id}", operation_id = "people_directory_staff_get", tag = "people-directory",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = StaffView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn staff_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<StaffView>> {
    let ctx = auth.require(Permission::PeopleRead)?;
    let scope = crate::shared::scope::tenant_scope(ctx);
    let s = st
        .directory
        .staff(&scope, StaffMemberId(id))
        .await?
        .ok_or(KernelError::NotFound {
            entity: "staff member",
        })?;
    let e = st.directory.current_engagement(&scope, s.id).await?;
    Ok(Json(StaffView::of(&s, e.as_ref())))
}

/// The basic staff directory of an institution: staff with a working (`active`) engagement,
/// name, designation, department and work e-mail only (`people.self`).
#[utoipa::path(get, path = "/api/v1/people/institutions/{id}/directory", operation_id = "people_directory_list", tag = "people-directory",
    params(("id" = String, Path, format = Uuid), DirectoryQuery), security(("session_cookie" = [])),
    responses((status = 200, body = DirectoryList), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn directory_list(
    State(st): State<PeopleState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<DirectoryQuery>,
) -> ApiResult<Json<DirectoryList>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let staff = st
        .directory
        .staff_of_institution(&scope, institution, Some(StaffStatus::Active))
        .await?;
    let mut conn = st.pool.acquire().await.map_err(map_db_error)?;
    let mut items = Vec::new();
    for s in &staff {
        let Some(e) = current_engagement(&mut conn, ctx.scope.tenant_id, s.id).await? else {
            continue;
        };
        if !e.status.is_working() {
            continue;
        }
        if !matches_q(
            q.q.as_deref(),
            &[Some(s.display_name.as_str()), Some(e.designation.as_str())],
        ) {
            continue;
        }
        items.push(DirectoryEntry {
            staff_id: s.id,
            staff_number: s.staff_number.clone(),
            display_name: s.display_name.clone(),
            staff_category: s.staff_category,
            designation: Some(e.designation.clone()),
            org_unit_id: e.org_unit_id,
            work_email: s.work_email.clone(),
        });
    }
    Ok(Json(DirectoryList { items }))
}

/// The caller's own staff record with its current engagement (`people.self`; 404 when the login
/// has no staff record).
#[utoipa::path(get, path = "/api/v1/people/me/staff", operation_id = "people_directory_self_get", tag = "people-directory",
    security(("session_cookie" = [])),
    responses((status = 200, body = StaffView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn my_staff_get(
    State(st): State<PeopleState>,
    auth: Authenticated,
) -> ApiResult<Json<StaffView>> {
    let ctx = auth.require(Permission::PeopleSelfService)?;
    let s = my_staff(&st, ctx).await?;
    let scope = crate::shared::scope::tenant_scope(ctx);
    let e = st.directory.current_engagement(&scope, s.id).await?;
    Ok(Json(StaffView::of(&s, e.as_ref())))
}

pub fn router(state: PeopleState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(staff_list))
        .routes(routes!(staff_get))
        .routes(routes!(directory_list))
        .routes(routes!(my_staff_get))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_matches_any_field_case_insensitively() {
        assert!(matches_q(None, &[Some("Meera")]));
        assert!(matches_q(Some("  "), &[Some("Meera")]));
        assert!(matches_q(Some("mee"), &[None, Some("Dr. Meera Shah")]));
        assert!(!matches_q(Some("zz"), &[Some("Meera"), None]));
    }
}
