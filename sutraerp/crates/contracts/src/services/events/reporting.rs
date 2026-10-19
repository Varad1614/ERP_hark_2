//! **8C** — dashboards, widgets, dashboard assignments (H07) and regulatory frameworks, cycles, snapshots,
//! submission packages (H08) (L3-H-048, 050, 052, 054…060).
//!
//! Owned by slice 8C (`docs/batch8-contracts.md` §4.3). Payload: [`super::StatusChangedV1`] unless noted.

use super::event_type;

event_type!(
    /// Dashboard version published (L3-H-048 / 052).
    DASHBOARD_PUBLISHED, DASHBOARD_PUBLISHED_SCHEMA, "dashboard.published");
event_type!(
    /// Dashboard rolled back to an earlier version (L3-H-052).
    DASHBOARD_ROLLED_BACK, DASHBOARD_ROLLED_BACK_SCHEMA, "dashboard.rolled_back");
event_type!(
    /// Widget available (L3-H-054).
    WIDGET_DEFINITION_ACTIVATED, WIDGET_DEFINITION_ACTIVATED_SCHEMA, "widget_definition.activated");
event_type!(
    /// Reporting definition active (L3-H-055).
    REPORTING_FRAMEWORK_ACTIVATED, REPORTING_FRAMEWORK_ACTIVATED_SCHEMA, "reporting_framework.activated");
event_type!(
    /// Snapshot validated (L3-H-057).
    REPORT_SNAPSHOT_VALIDATED, REPORT_SNAPSHOT_VALIDATED_SCHEMA, "report_snapshot.validated");
event_type!(
    /// Package approved (L3-H-058).
    SUBMISSION_PACKAGE_APPROVED, SUBMISSION_PACKAGE_APPROVED_SCHEMA, "submission_package.approved");
event_type!(
    /// Submission acknowledged (L3-H-059).
    SUBMISSION_PACKAGE_ACKNOWLEDGED, SUBMISSION_PACKAGE_ACKNOWLEDGED_SCHEMA, "submission_package.acknowledged");
event_type!(
    /// Cycle closed (L3-H-060).
    REPORTING_CYCLE_CLOSED, REPORTING_CYCLE_CLOSED_SCHEMA, "reporting_cycle.closed");

/// Event types of this file (keep in sync; `all_event_types` collects them).
pub const EVENT_TYPES: &[&str] = &[
    DASHBOARD_PUBLISHED,
    DASHBOARD_ROLLED_BACK,
    WIDGET_DEFINITION_ACTIVATED,
    REPORTING_FRAMEWORK_ACTIVATED,
    REPORT_SNAPSHOT_VALIDATED,
    SUBMISSION_PACKAGE_APPROVED,
    SUBMISSION_PACKAGE_ACKNOWLEDGED,
    REPORTING_CYCLE_CLOSED,
];
