//! Workspace catalog + module registry endpoints.

use axum::extract::{Path, Query, State};
use axum::Json;
use std::sync::OnceLock;
use sutra_contracts::platform::auth::Role;
use sutra_contracts::platform::catalog::{Workspace, WorkspaceList};
use sutra_kernel::error::KernelError;
use sutra_kernel::module::ModuleDescriptor;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Embedded copy of `catalog/workspaces.json` (shared with the web app).
const CATALOG_JSON: &str = include_str!("../../../catalog/workspaces.json");

pub fn catalog() -> &'static [Workspace] {
    static CATALOG: OnceLock<Vec<Workspace>> = OnceLock::new();
    CATALOG.get_or_init(|| match serde_json::from_str(CATALOG_JSON) {
        Ok(v) => v,
        Err(e) => {
            tracing::error!(error = %e, "workspace catalog is invalid");
            Vec::new()
        }
    })
}

#[derive(Debug, serde::Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct WorkspaceQuery {
    /// Only workspaces visible to this role.
    pub role: Option<Role>,
    /// Navigation group filter (e.g. `students`, `finance`).
    pub group: Option<String>,
}

/// List workspaces (navigation catalog). Audit A3 (SEC-3 / M-1): the full feature / workflow
/// catalogue is only for signed-in users (the sign-in page needs none of it); `401` otherwise.
#[utoipa::path(
    get,
    path = "/api/v1/platform/workspaces",
    tag = "platform",
    params(WorkspaceQuery),
    security(("session_cookie" = [])),
    responses((status = 200, body = WorkspaceList), (status = 401, body = Problem, content_type = "application/problem+json"))
)]
pub async fn list_workspaces(
    _auth: Authenticated,
    Query(q): Query<WorkspaceQuery>,
) -> Json<WorkspaceList> {
    Json(filter_workspaces(&q))
}

/// The catalogue filtered by role / group (workflows trimmed to the role).
pub fn filter_workspaces(q: &WorkspaceQuery) -> WorkspaceList {
    let items: Vec<Workspace> = catalog()
        .iter()
        .filter(|w| q.role.is_none_or(|r| w.roles.contains(&r)))
        .filter(|w| q.group.as_deref().is_none_or(|g| w.group == g))
        .map(|w| match q.role {
            // Only expose the workflows the role can see.
            Some(r) => Workspace {
                workflows: w
                    .workflows
                    .iter()
                    .filter(|f| f.roles.contains(&r))
                    .cloned()
                    .collect(),
                ..w.clone()
            },
            None => w.clone(),
        })
        .collect();
    let workflow_total = items.iter().map(|w| w.workflows.len()).sum();
    WorkspaceList {
        total: items.len(),
        workflow_total,
        items,
    }
}

/// Get one workspace by key.
#[utoipa::path(
    get,
    path = "/api/v1/platform/workspaces/{key}",
    tag = "platform",
    params(("key" = String, Path, description = "Workspace key, e.g. W-C01")),
    security(("session_cookie" = [])),
    responses((status = 200, body = Workspace), (status = "4XX", body = Problem, content_type = "application/problem+json"))
)]
pub async fn get_workspace(
    _auth: Authenticated,
    Path(key): Path<String>,
) -> ApiResult<Json<Workspace>> {
    find_workspace(&key)
}

/// One workspace by key (`404` when unknown).
pub fn find_workspace(key: &str) -> ApiResult<Json<Workspace>> {
    catalog()
        .iter()
        .find(|w| w.key == key)
        .cloned()
        .map(Json)
        .ok_or_else(|| {
            KernelError::NotFound {
                entity: "workspace",
            }
            .into()
        })
}

/// Modules composed into this modular-monolith build.
#[utoipa::path(get, path = "/api/v1/platform/modules", tag = "platform", responses((status = 200, body = Vec<ModuleDescriptor>)))]
pub async fn list_modules(
    State(modules): State<Vec<ModuleDescriptor>>,
) -> Json<Vec<ModuleDescriptor>> {
    Json(modules)
}

pub fn router(modules: Vec<ModuleDescriptor>) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(list_workspaces))
        .routes(routes!(get_workspace))
        .routes(routes!(list_modules))
        .with_state(modules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_matches_prototype_totals() {
        let c = catalog();
        assert_eq!(c.len(), 99);
        assert_eq!(c.iter().map(|w| w.workflows.len()).sum::<usize>(), 595);
        let faculty = c
            .iter()
            .filter(|w| w.roles.contains(&Role::Faculty))
            .count();
        let student = c
            .iter()
            .filter(|w| w.roles.contains(&Role::Student))
            .count();
        assert_eq!((faculty, student), (50, 44));
    }

    #[test]
    fn role_filter_trims_workflows() {
        let list = filter_workspaces(&WorkspaceQuery {
            role: Some(Role::Student),
            group: None,
        });
        assert_eq!(list.total, 44);
        assert!(list
            .items
            .iter()
            .all(|w| w.workflows.iter().all(|f| f.roles.contains(&Role::Student))));
    }

    #[test]
    fn unknown_workspace_is_404() {
        assert!(find_workspace("W-NOPE").is_err());
        assert!(find_workspace("W-A01").is_ok());
    }
}
