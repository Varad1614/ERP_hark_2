//! HTTP API of the metrics catalogue (tag `services-directory`; contract §5.2).

use crate::shared::guard::require_institution;
use crate::shared::scope::institution_scope;
use crate::shared::ServicesState;
use axum::extract::{Path, Query, State};
use axum::Json;
use serde::{Deserialize, Serialize};
use sutra_contracts::services::ports::MetricValue;
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// One operational figure (decimal string).
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MetricValueView {
    #[schema(example = "campus")]
    pub domain: String,
    #[schema(example = "campus.active_spaces")]
    pub key: String,
    pub label: String,
    #[schema(value_type = String, example = "42")]
    pub value: String,
    #[schema(example = "count")]
    pub unit: String,
}

impl From<&MetricValue> for MetricValueView {
    fn from(m: &MetricValue) -> Self {
        Self {
            domain: m.domain.clone(),
            key: m.key.clone(),
            label: m.label.clone(),
            value: m.value.normalize().to_string(),
            unit: m.unit.clone(),
        }
    }
}

/// The figures of an institution.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct MetricsView {
    pub items: Vec<MetricValueView>,
}

/// Filters of the metrics catalogue.
#[derive(Debug, Clone, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct MetricsQuery {
    /// Only one domain (`student`, `people`, `finance`, `campus`, `resources`).
    pub domain: Option<String>,
}

/// Every operational figure of an institution (`services.report.read`).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/metrics", operation_id = "services_directory_metrics", tag = "services-directory",
    params(("id" = String, Path, format = Uuid), MetricsQuery), security(("session_cookie" = [])),
    responses((status = 200, body = MetricsView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn metrics(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
    Query(q): Query<MetricsQuery>,
) -> ApiResult<Json<MetricsView>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let all = st.all_metrics(&scope, institution).await?;
    let items = all
        .iter()
        .filter(|m| q.domain.as_deref().is_none_or(|d| m.domain == d))
        .map(MetricValueView::from)
        .collect();
    Ok(Json(MetricsView { items }))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(metrics))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    #[test]
    fn values_render_as_plain_decimal_strings() {
        let m = MetricValue::new("finance", "x", "X", Decimal::new(125_000, 2), "inr");
        assert_eq!(MetricValueView::from(&m).value, "1250");
        let c = MetricValue::count("campus", "active_spaces", "Active spaces", 7);
        assert_eq!(MetricValueView::from(&c).value, "7");
    }
}
