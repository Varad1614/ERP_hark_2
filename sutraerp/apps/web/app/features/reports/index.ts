/**
 * Batch 8 Admin → Reports (`/reports/:tab?`) — foundation-owned tab definitions
 * (docs/batch8-contracts.md §10). Slice 8C fills `analytics/` (dashboards, regulatory reporting);
 * figures come from `GET /api/v1/services/institutions/{id}/metrics` (every domain's
 * `OperationalMetricsPort`).
 */
export { activeTab, REPORTS_TABS, reportsPath } from "../campus";
