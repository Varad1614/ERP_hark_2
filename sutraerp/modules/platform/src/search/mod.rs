//! OpenSearch projections — **owned by slice 9C** (`docs/batch9-contracts.md` §8).
//!
//! * [`projector::SearchProjector`] (job `search_project`) reads `platform.outbox` after each
//!   projection's checkpoint (`platform.search_checkpoint`), asks the owning module's
//!   [`SearchSource`] which documents an event touches, re-reads those documents and writes them
//!   with external versions (`external_gte`), so replays are idempotent.
//! * [`projector::SearchRebuild`] (job `search_rebuild`) recreates an index and re-reads every
//!   document through the [`SearchSource`] when a projection has no checkpoint yet (first start)
//!   or `rebuild_requested_at` is set (operators, DR restore).
//! * Read API `GET /api/v1/platform/search?q=&projection=&limit=` (tag `platform-search`,
//!   operation id `platform_search_query`): always filtered on the session's tenant; permission
//!   per projection (`students` → `student.read`, `staff` → `people.read`,
//!   `library-catalogue` → `campus.read`); `503 search_disabled` when `SUTRA_SEARCH=off`.

pub mod projector;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use std::sync::Arc;
use sutra_contracts::platform::search::{
    SearchIndex, SearchResults, SearchSource, PROJECTIONS, PROJECTION_LIBRARY_CATALOGUE,
    PROJECTION_STAFF, PROJECTION_STUDENTS,
};
use sutra_kernel::auth::Permission;
use sutra_kernel::error::{FieldViolation, KernelError};
use sutra_web::{ApiResult, Authenticated, Problem};
use tracing::Instrument;
use utoipa_axum::{router::OpenApiRouter, routes};

/// Problem `errorCode` when search is switched off.
pub const SEARCH_DISABLED: &str = "search_disabled";

/// The search engine plus every projection's source.
pub struct SearchRuntime {
    pub index: Arc<dyn SearchIndex>,
    pub sources: Vec<Arc<dyn SearchSource>>,
}

impl SearchRuntime {
    pub fn source(&self, projection: &str) -> Option<&Arc<dyn SearchSource>> {
        self.sources.iter().find(|s| s.projection() == projection)
    }
}

/// Handler state: `None` = `SUTRA_SEARCH=off`.
#[derive(Clone, Default)]
pub struct SearchState {
    runtime: Option<Arc<SearchRuntime>>,
}

impl SearchState {
    pub fn disabled() -> Self {
        Self { runtime: None }
    }

    pub fn enabled(index: Arc<dyn SearchIndex>, sources: Vec<Arc<dyn SearchSource>>) -> Self {
        Self {
            runtime: Some(Arc::new(SearchRuntime { index, sources })),
        }
    }

    pub fn runtime(&self) -> Option<Arc<SearchRuntime>> {
        self.runtime.clone()
    }

    pub fn is_enabled(&self) -> bool {
        self.runtime.is_some()
    }
}

/// Read permission of a projection.
pub fn permission_for(projection: &str) -> Option<Permission> {
    match projection {
        PROJECTION_STUDENTS => Some(Permission::StudentRead),
        PROJECTION_STAFF => Some(Permission::PeopleRead),
        PROJECTION_LIBRARY_CATALOGUE => Some(Permission::CampusRead),
        _ => None,
    }
}

#[derive(Debug, Default, serde::Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SearchQuery {
    /// Free text (prefix match on names / numbers / titles); empty = every document.
    pub q: Option<String>,
    /// `students` | `staff` | `library-catalogue`.
    pub projection: String,
    /// Page size (1–100, default 20).
    pub limit: Option<u32>,
}

fn disabled_problem() -> Response {
    Problem::new(
        StatusCode::SERVICE_UNAVAILABLE,
        SEARCH_DISABLED,
        "Search is disabled",
    )
    .detail("OpenSearch projections are switched off in this environment (SUTRA_SEARCH=off)")
    .into_response()
}

/// Full-text search over one projection of the caller's tenant.
#[utoipa::path(
    get,
    path = "/api/v1/platform/search",
    operation_id = "platform_search_query",
    tag = "platform-search",
    params(SearchQuery),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = SearchResults),
        (status = 401, body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing the projection's read permission", body = Problem, content_type = "application/problem+json"),
        (status = 422, description = "Unknown projection or bad query", body = Problem, content_type = "application/problem+json"),
        (status = 503, description = "`search_disabled` (SUTRA_SEARCH=off) or OpenSearch unavailable", body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn query(
    State(st): State<SearchState>,
    auth: Authenticated,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Response> {
    let projection = q.projection.trim().to_owned();
    let Some(permission) = permission_for(&projection) else {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "projection",
            "unknown",
            format!("expected one of {}", PROJECTIONS.join(", ")),
        )])
        .into());
    };
    let ctx = auth.require(permission)?;
    let text = q.q.unwrap_or_default();
    if text.chars().count() > 200 {
        return Err(KernelError::Validation(vec![FieldViolation::new(
            "q",
            "too_long",
            "at most 200 characters",
        )])
        .into());
    }
    let Some(rt) = st.runtime() else {
        return Ok(disabled_problem());
    };
    let limit = q.limit.unwrap_or(20).clamp(1, 100);
    let tenant = ctx.scope.tenant_id.as_uuid();
    let res = rt
        .index
        .search(&projection, tenant, &text, limit)
        .instrument(tracing::info_span!("sutra.search.query", projection = %projection))
        .await?;
    Ok(Json(res).into_response())
}

pub fn router(state: SearchState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(query))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_projection_has_a_read_permission() {
        for p in PROJECTIONS {
            assert!(permission_for(p).is_some(), "{p}");
        }
        assert_eq!(permission_for("students"), Some(Permission::StudentRead));
        assert_eq!(permission_for("staff"), Some(Permission::PeopleRead));
        assert_eq!(
            permission_for("library-catalogue"),
            Some(Permission::CampusRead)
        );
        assert_eq!(permission_for("audit"), None);
        assert!(!SearchState::disabled().is_enabled());
    }
}
