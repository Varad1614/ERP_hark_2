//! Operational reports built from the `OperationalMetricsPort` catalogue (never cross-schema SQL):
//! G-154 library operational reporting (the `campus.library_*` figures Domain G reports) and
//! G-320 infrastructure portfolio (buildings, spaces, capacity, service desk load).

use crate::directory::http::MetricValueView;
use crate::shared::guard::require_institution;
use crate::shared::scope::institution_scope;
use crate::shared::ServicesState;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Serialize;
use sutra_contracts::services::ports::MetricValue;
use sutra_kernel::auth::Permission;
use sutra_kernel::scope::InstitutionId;
use sutra_web::{ApiResult, Authenticated, Problem};
use utoipa::ToSchema;
use utoipa_axum::{router::OpenApiRouter, routes};
use uuid::Uuid;

/// Figures of the infrastructure portfolio report (G-320).
pub const INFRASTRUCTURE_KEYS: &[&str] = &[
    "campus.active_buildings",
    "campus.active_spaces",
    "campus.seating_capacity",
    "campus.open_tickets",
    "campus.tickets_past_sla",
];

/// An operational report: the figures of one area, as of `generatedAt`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct OperationalReportView {
    pub institution_id: Uuid,
    #[schema(example = "library")]
    pub report: String,
    pub generated_at: DateTime<Utc>,
    pub items: Vec<MetricValueView>,
}

/// Library figures: every `campus.library_*` key.
pub fn library_figures(all: &[MetricValue]) -> Vec<MetricValueView> {
    all.iter()
        .filter(|m| m.key.starts_with("campus.library_"))
        .map(MetricValueView::from)
        .collect()
}

/// Infrastructure figures in report order.
pub fn infrastructure_figures(all: &[MetricValue]) -> Vec<MetricValueView> {
    INFRASTRUCTURE_KEYS
        .iter()
        .filter_map(|k| all.iter().find(|m| m.key == *k))
        .map(MetricValueView::from)
        .collect()
}

/// Library operational report (`services.report.read`; L3-G-154).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/reports/library", operation_id = "services_dashboard_report_library", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = OperationalReportView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn library(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<OperationalReportView>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let all = st.all_metrics(&scope, institution).await?;
    Ok(Json(OperationalReportView {
        institution_id: id,
        report: "library".into(),
        generated_at: Utc::now(),
        items: library_figures(&all),
    }))
}

/// Infrastructure portfolio report (`services.report.read`; L3-G-320).
#[utoipa::path(get, path = "/api/v1/services/institutions/{id}/reports/infrastructure", operation_id = "services_dashboard_report_infrastructure", tag = "services-dashboards",
    params(("id" = String, Path, format = Uuid)), security(("session_cookie" = [])),
    responses((status = 200, body = OperationalReportView), (status = "4XX", body = Problem, content_type = "application/problem+json")))]
pub async fn infrastructure(
    State(st): State<ServicesState>,
    auth: Authenticated,
    Path(id): Path<Uuid>,
) -> ApiResult<Json<OperationalReportView>> {
    let ctx = auth.require(Permission::ReportRead)?;
    let institution = InstitutionId(id);
    let scope = institution_scope(ctx, institution);
    require_institution(&st, &scope, institution).await?;
    let all = st.all_metrics(&scope, institution).await?;
    Ok(Json(OperationalReportView {
        institution_id: id,
        report: "infrastructure".into(),
        generated_at: Utc::now(),
        items: infrastructure_figures(&all),
    }))
}

pub fn router(state: ServicesState) -> OpenApiRouter {
    OpenApiRouter::new()
        .routes(routes!(library))
        .routes(routes!(infrastructure))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_pick_their_figures() {
        let all = vec![
            MetricValue::count("campus", "active_spaces", "Spaces", 3),
            MetricValue::count("campus", "library_loans_active", "Loans", 2),
            MetricValue::count("finance", "open_charges", "Charges", 1),
        ];
        assert_eq!(library_figures(&all).len(), 1);
        let infra = infrastructure_figures(&all);
        assert_eq!(infra.len(), 1);
        assert_eq!(infra[0].key, "campus.active_spaces");
    }
}
