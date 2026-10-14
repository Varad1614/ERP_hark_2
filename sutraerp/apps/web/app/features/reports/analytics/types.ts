/**
 * Slice-8C reports & analytics wire types — straight from the generated OpenAPI client
 * (`services-dashboards`, `services-regulatory`, `services-directory`). Figures stay the decimal
 * strings the API sends (`MetricValueView.value`), never floats.
 */
import type { components } from "@sutra/api-client";

type S = components["schemas"];

export interface Items<T> {
  items: T[];
}

export type MetricValue = S["MetricValueView"];
export type OperationalReport = S["OperationalReportView"];
export type Widget = S["WidgetView"];
export type NewWidget = S["WidgetCreate"];
export type WidgetVisualization = S["WidgetVisualization"];
export type Dashboard = S["DashboardView"];
export type NewDashboard = S["DashboardCreate"];
export type DashboardCategory = S["DashboardCategory"];
export type DashboardVersion = S["DashboardVersionView"];
export type DashboardAssignment = S["DashboardAssignmentView"];
export type DashboardRender = S["DashboardRenderView"];
export type RenderedWidget = S["RenderedWidgetView"];
export type Framework = S["ReportingFrameworkView"];
export type NewFramework = S["ReportingFrameworkCreate"];
export type ManualField = S["ReportingManualField"];
export type Cycle = S["ReportingCycleView"];
export type NewCycle = S["ReportingCycleCreate"];
export type CycleDetail = S["ReportingCycleDetail"];
export type Snapshot = S["ReportSnapshotView"];
export type SnapshotValue = S["ReportSnapshotValueView"];
export type SubmissionPackage = S["SubmissionPackageView"];

export type Decision = "approve" | "reject";
export type WidgetCommand = "activate" | "retire";
export type DashboardCommand = "submit" | "publish";
export type FrameworkCommand = "submit" | "activate";
export type SnapshotCommand = "validate" | "reject";
export type PackageCommand = "submit" | "correct";
export type ReportKind = "library" | "infrastructure";
