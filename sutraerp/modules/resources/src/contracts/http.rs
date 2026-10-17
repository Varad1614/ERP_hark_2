//! placeholder
use crate::shared::ResourcesState;
use utoipa_axum::router::OpenApiRouter;
pub fn router(st: ResourcesState) -> OpenApiRouter {
    OpenApiRouter::new().with_state(st)
}
