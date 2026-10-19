//! HTTP API of the Campus directory (tag `campus-directory`; contract §5.2).

use crate::shared::guard::require_institution;
use crate::shared::me::{my_person, PersonRef};
use crate::shared::rows::{node_from_row, InfrastructureNode, NODE_COLUMNS};
use crate::shared::scope::institution_scope;
use crate::shared::CampusState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sutra_contracts::campus::ids::InfrastructureNodeId;
use sutra_contracts::campus::status::{
    InfrastructureNodeType, InfrastructureStatus, PersonKind, SpaceType,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_kernel::scope::InstitutionId;
use sutra_persistence::map_db_error;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// An infrastructure node as screens show it (reusable by the slices).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureNodeView {
    pub id: InfrastructureNodeId,
    pub institution_id: Uuid,
    pub campus_id: Uuid,
    pub parent_id: Option<InfrastructureNodeId>,
    pub node_type: InfrastructureNodeType,
    #[schema(example = "MAIN-B1-101")]
    pub code: String,
    pub name: String,
    pub space_type: Option<SpaceType>,
    pub capacity: Option<i32>,
    pub floor_number: Option<i32>,
    #[schema(value_type = Option<String>, example = "64.50")]
    pub area_sq_m: Option<String>,
    #[schema(value_type = Object)]
    pub attributes: serde_json::Value,
    pub status: InfrastructureStatus,
    pub status_reason: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub version: i64,
}

impl From<&InfrastructureNode> for InfrastructureNodeView {
    fn from(n: &InfrastructureNode) -> Self {
        Self {
            id: n.id,
            institution_id: n.institution_id.as_uuid(),
            campus_id: n.campus_id,
            parent_id: n.parent_id,
            node_type: n.node_type,
            code: n.code.clone(),
            name: n.name.clone(),
            space_type: n.space_type,
            capacity: n.capacity,
            floor_number: n.floor_number,
            area_sq_m: n.area_sq_m.map(crate::shared::decimal::fmt2),
            attributes: n.attributes.clone(),
            status: n.status,
            status_reason: n.status_reason.clone(),
            effective_from: n.effective_from,
            version: n.version,
        }
    }
}

/// The infrastructure nodes of an institution, ordered by code (parents before children is
/// the client's job: `parentId` links them).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct InfrastructureTree {
    pub items: Vec<InfrastructureNodeView>,
}

/// Filters of the infrastructure tree.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase")]
pub struct InfrastructureTreeQuery {
    /// Domain B campus.
    pub campus_id: Option<Uuid>,
    /// `InfrastructureNodeType` wire name.
    pub node_type: Option<String>,
    /// `InfrastructureStatus` wire name.
    pub status: Option<String>,
    /// `SpaceType` wire name.
    pub space_type: Option<String>,
}

/// The caller as a campus person.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CampusPersonView {
    pub kind: PersonKind,
    /// Domain C student id or Domain E staff id.
    pub id: Uuid,
    /// Student number / staff number.
    pub number: String,
    pub display_name: String,
}

impl From<&PersonRef> for CampusPersonView {
    fn from(p: &PersonRef) -> Self {
        Self {
            kind: p.kind,
            id: p.id,
            number: p.number.clone(),
            display_name: p.display_name.clone(),
        }
    }
}

fn parse_filter<T>(
    field: &str,
    raw: Option<&str>,
    parse: fn(&str) -> Option<T>,
) -> Result<Option<T>, KernelError> {
    raw.map(|r| {
        parse(r).ok_or_else(|| {
            KernelError::Validation(vec![FieldViolation::new(field, "invalid", "unknown value")])
        })
    })
    .transpose()
}

/// Infrastructure nodes of an institution (`campus.read`).
#[utoipa::path(get, path = "/api/v1/campus/institutions/{id}/infrastructure-tree", operation_id = "campus_directory_tree", tag = "campus-directory",
    params(("id" = String, Path, format = Uuid), InfrastructureTreeQuery), security(("session_cookie" = [])),
    responses((status = 200, body = InfrastructureTree), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn tree(
    State(st): State<CampusState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<InfrastructureTreeQuery>,
) -> ApiResult<Json<InfrastructureTree>> {
    let ctx = auth.require(Permission::CampusRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let node_type = parse_filter(
        "nodeType",
        q.node_type.as_deref(),
        InfrastructureNodeType::parse,
    )?;
    let status = parse_filter("status", q.status.as_deref(), InfrastructureStatus::parse)?;
    let space_type = parse_filter("spaceType", q.space_type.as_deref(), SpaceType::parse)?;
    let sql = format!(
        "SELECT {NODE_COLUMNS} FROM campus.infrastructure_node WHERE tenant_id = $1 \
         AND institution_id = $2 AND ($3::uuid IS NULL OR campus_id = $3) \
         AND ($4::text IS NULL OR node_type = $4) AND ($5::text IS NULL OR status = $5) \
         AND ($6::text IS NULL OR space_type = $6) ORDER BY code LIMIT 2000"
    );
    let rows = sqlx::query(&sql)
        .bind(ctx.scope.tenant_id.as_uuid())
        .bind(id)
        .bind(q.campus_id)
        .bind(node_type.map(|t| t.as_str()))
        .bind(status.map(|s| s.as_str()))
        .bind(space_type.map(|s| s.as_str()))
        .fetch_all(&st.pool)
        .await
        .map_err(map_db_error)?;
    let mut items = Vec::with_capacity(rows.len());
    for r in &rows {
        items.push(InfrastructureNodeView::from(
            &node_from_row(r).map_err(map_db_error)?,
        ));
    }
    Ok(Json(InfrastructureTree { items }))
}

/// The caller as a campus person: their student record, else their staff record (`campus.self`;
/// 404 when the login has neither).
#[utoipa::path(get, path = "/api/v1/campus/me/person", operation_id = "campus_directory_self_person", tag = "campus-directory",
    security(("session_cookie" = [])),
    responses((status = 200, body = CampusPersonView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn me_person(
    State(st): State<CampusState>,
    auth: Authenticated,
) -> ApiResult<Json<CampusPersonView>> {
    let ctx = auth.require(Permission::CampusSelfService)?;
    let p = my_person(&st, ctx).await?;
    Ok(Json(CampusPersonView::from(&p)))
}

pub fn router(state: CampusState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(tree))
        .routes(routes!(me_person))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_parse_strictly() {
        assert_eq!(
            parse_filter("status", Some("active"), InfrastructureStatus::parse).ok(),
            Some(Some(InfrastructureStatus::Active))
        );
        assert!(parse_filter("status", Some("bogus"), InfrastructureStatus::parse).is_err());
        assert!(matches!(
            parse_filter("status", None, InfrastructureStatus::parse),
            Ok(None)
        ));
    }
}
