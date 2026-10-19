//! **2C** — activation readiness checklist endpoint over `crate::shared::readiness::evaluate`.
//!
//! Path: `GET /api/v1/institution/institutions/{id}/activation-readiness`
//! (`institution.readiness.read`). operation_id prefix: `readiness_`; tag `readiness`.
//! Readiness is computed on read from the three slice providers (2A lifecycle/org, 2B campus +
//! curriculum, 2C calendar); nothing is persisted (L2-B01).

use crate::shared::readiness::{evaluate, ReadinessReport};
use crate::shared::scope::institution_scope;
use crate::shared::InstitutionState;
use axum::extract::{Path, State};
use axum::Json;
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa_axum::{router::OpenApiRouter, routes};

/// Activation readiness checklist of an institution (computed live).
///
/// `ready` is true when no required item is `unmet`. Items carry a `link` to the screen that
/// fixes them.
#[utoipa::path(
    get,
    path = "/api/v1/institution/institutions/{id}/activation-readiness",
    operation_id = "readiness_get",
    tag = "readiness",
    params(("id" = String, Path, format = Uuid)),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = ReadinessReport),
        (status = 401, description = "No live session", body = Problem, content_type = "application/problem+json"),
        (status = 403, description = "Missing `institution.readiness.read`", body = Problem, content_type = "application/problem+json"),
        (status = 404, body = Problem, content_type = "application/problem+json"),
    )
)]
pub async fn readiness_get(
    State(st): State<InstitutionState>,
    auth: Authenticated,
    Path(id): Path<InstitutionId>,
) -> ApiResult<Json<ReadinessReport>> {
    let ctx = auth.require(Permission::InstitutionReadinessRead)?;
    let scope = institution_scope(ctx, id);
    Ok(Json(evaluate(&st.pool, &scope, id).await?))
}

/// Routes of this area. Mounted by `crate::router` (already registered — do not edit lib.rs).
pub fn router(state: InstitutionState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(readiness_get))
        .with_state(state)
}
