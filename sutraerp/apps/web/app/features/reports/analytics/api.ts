/**
 * Slice-8C reports & analytics API layer: one interface, an HTTP implementation (CSRF,
 * `Idempotency-Key` on creates, `If-Match` on state commands, problem+json → `ApiProblem`) and an
 * in-memory mock (`./mock`). Every path template is checked against the generated OpenAPI `paths`.
 */
import { ApiProblem, isProblem, isUnsafeMethod, newIdempotencyKey, type paths } from "@sutra/api-client";
import { getCsrfToken } from "../../../lib/api";
import { fill } from "../../institution/api";
import type {
  Cycle,
  CycleDetail,
  Dashboard,
  DashboardAssignment,
  DashboardCommand,
  DashboardRender,
  Decision,
  Framework,
  FrameworkCommand,
  Items,
  MetricValue,
  NewCycle,
  NewDashboard,
  NewFramework,
  NewWidget,
  OperationalReport,
  PackageCommand,
  ReportKind,
  Snapshot,
  SnapshotCommand,
  SubmissionPackage,
  Widget,
  WidgetCommand,
} from "./types";

export interface AnalyticsApi {
  // figures
  metrics(institutionId: string, domain?: string): Promise<MetricValue[]>;
  report(institutionId: string, kind: ReportKind): Promise<OperationalReport>;
  // dashboards (H07)
  widgets(institutionId: string): Promise<Widget[]>;
  createWidget(institutionId: string, input: NewWidget): Promise<Widget>;
  widgetCommand(id: string, version: number, cmd: WidgetCommand, reason?: string): Promise<Widget>;
  dashboards(institutionId: string): Promise<Dashboard[]>;
  createDashboard(institutionId: string, input: NewDashboard): Promise<Dashboard>;
  createVersion(id: string, widgetIds: string[], changeNote?: string): Promise<Dashboard>;
  dashboardCommand(id: string, version: number, cmd: DashboardCommand, reason?: string): Promise<Dashboard>;
  decideDashboard(id: string, version: number, decision: Decision, reason?: string): Promise<Dashboard>;
  rollback(id: string, version: number, versionNo: number, reason: string): Promise<Dashboard>;
  assign(id: string, role: string): Promise<DashboardAssignment>;
  revoke(dashboardId: string, assignmentId: string, version: number, reason?: string): Promise<DashboardAssignment>;
  render(id: string): Promise<DashboardRender>;
  // regulatory (H08)
  frameworks(institutionId: string): Promise<Framework[]>;
  createFramework(institutionId: string, input: NewFramework): Promise<Framework>;
  frameworkCommand(id: string, version: number, cmd: FrameworkCommand): Promise<Framework>;
  decideFramework(id: string, version: number, decision: Decision, reason?: string): Promise<Framework>;
  cycles(institutionId: string): Promise<Cycle[]>;
  createCycle(frameworkId: string, input: NewCycle): Promise<Cycle>;
  cycle(id: string): Promise<CycleDetail>;
  collect(cycleId: string, manualValues: Record<string, string>): Promise<Snapshot>;
  snapshot(id: string): Promise<Snapshot>;
  snapshotCommand(id: string, version: number, cmd: SnapshotCommand, reason?: string): Promise<Snapshot>;
  createPackage(cycleId: string, snapshotId?: string): Promise<SubmissionPackage>;
  packageCommand(id: string, version: number, cmd: PackageCommand, reason?: string): Promise<SubmissionPackage>;
  decidePackage(id: string, version: number, decision: Decision, reason?: string): Promise<SubmissionPackage>;
  acknowledge(id: string, version: number, reference: string): Promise<SubmissionPackage>;
  closeCycle(id: string, version: number, reason?: string): Promise<CycleDetail>;
}

const B = "/api/v1/services";
export const PATHS = {
  metrics: `${B}/institutions/{id}/metrics`,
  libraryReport: `${B}/institutions/{id}/reports/library`,
  infrastructureReport: `${B}/institutions/{id}/reports/infrastructure`,
  widgets: `${B}/institutions/{id}/widgets`,
  widgetActivate: `${B}/widgets/{id}/activate`,
  widgetRetire: `${B}/widgets/{id}/retire`,
  dashboards: `${B}/institutions/{id}/dashboards`,
  dashboard: `${B}/dashboards/{id}`,
  versions: `${B}/dashboards/{id}/versions`,
  dashboardSubmit: `${B}/dashboards/{id}/submit`,
  dashboardDecision: `${B}/dashboards/{id}/decision`,
  dashboardPublish: `${B}/dashboards/{id}/publish`,
  rollback: `${B}/dashboards/{id}/rollback`,
  assignments: `${B}/dashboards/{id}/assignments`,
  revoke: `${B}/dashboards/{id}/assignments/{assignmentId}/revoke`,
  render: `${B}/dashboards/{id}/view`,
  frameworks: `${B}/institutions/{id}/reporting-frameworks`,
  frameworkSubmit: `${B}/reporting-frameworks/{id}/submit`,
  frameworkDecision: `${B}/reporting-frameworks/{id}/decision`,
  frameworkActivate: `${B}/reporting-frameworks/{id}/activate`,
  frameworkCycles: `${B}/reporting-frameworks/{id}/cycles`,
  cycles: `${B}/institutions/{id}/reporting-cycles`,
  cycle: `${B}/reporting-cycles/{id}`,
  collect: `${B}/reporting-cycles/{id}/collect`,
  cycleClose: `${B}/reporting-cycles/{id}/close`,
  packages: `${B}/reporting-cycles/{id}/packages`,
  snapshot: `${B}/report-snapshots/{id}`,
  snapshotValidate: `${B}/report-snapshots/{id}/validate`,
  snapshotReject: `${B}/report-snapshots/{id}/reject`,
  packageSubmit: `${B}/submission-packages/{id}/submit`,
  packageDecision: `${B}/submission-packages/{id}/decision`,
  packageAcknowledge: `${B}/submission-packages/{id}/acknowledge`,
  packageCorrect: `${B}/submission-packages/{id}/correct`,
} as const satisfies Record<string, keyof paths>;

const WIDGET_CMD: Record<WidgetCommand, string> = { activate: PATHS.widgetActivate, retire: PATHS.widgetRetire };
const DASHBOARD_CMD: Record<DashboardCommand, string> = { submit: PATHS.dashboardSubmit, publish: PATHS.dashboardPublish };
const FRAMEWORK_CMD: Record<FrameworkCommand, string> = { submit: PATHS.frameworkSubmit, activate: PATHS.frameworkActivate };
const SNAPSHOT_CMD: Record<SnapshotCommand, string> = { validate: PATHS.snapshotValidate, reject: PATHS.snapshotReject };
const PACKAGE_CMD: Record<PackageCommand, string> = { submit: PATHS.packageSubmit, correct: PATHS.packageCorrect };
const REPORT: Record<ReportKind, string> = { library: PATHS.libraryReport, infrastructure: PATHS.infrastructureReport };

type Fetch = (input: string, init: RequestInit) => Promise<Response>;

interface RequestOptions {
  body?: unknown;
  ifMatch?: number;
  idempotent?: boolean;
}

const reasonBody = (reason?: string) => ({ reason: reason || undefined });
const decisionBody = (decision: Decision, reason?: string) => ({ decision, reason: reason || undefined });

export function createHttpAnalyticsApi(fetchImpl: Fetch = (i, init) => fetch(i, init), baseUrl: string = import.meta.env.VITE_API_BASE ?? ""): AnalyticsApi {
  async function request<T>(method: string, path: string, opts: RequestOptions = {}): Promise<T> {
    const headers: Record<string, string> = { Accept: "application/json" };
    const unsafe = isUnsafeMethod(method);
    if (unsafe) {
      headers["Content-Type"] = "application/json";
      const csrf = getCsrfToken();
      if (csrf) headers["X-CSRF-Token"] = csrf;
    }
    if (opts.ifMatch !== undefined) headers["If-Match"] = `"${opts.ifMatch}"`;
    if (opts.idempotent) headers["Idempotency-Key"] = newIdempotencyKey();
    const res = await fetchImpl(`${baseUrl}${path}`, { method, headers, credentials: "include", body: unsafe ? JSON.stringify(opts.body ?? {}) : undefined });
    const text = await res.text();
    let data: unknown = undefined;
    if (text) {
      try {
        data = JSON.parse(text);
      } catch {
        data = undefined;
      }
    }
    if (!res.ok) {
      if (isProblem(data)) throw new ApiProblem(data);
      throw new ApiProblem({ type: "about:blank", title: `Request failed (${res.status})`, status: res.status, errorCode: `http-${res.status}` });
    }
    return data as T;
  }
  const items = async <T>(path: string) => (await request<Items<T>>("GET", path)).items;
  const id = (template: string, value: string) => fill(template, { id: value });
  return {
    metrics: (inst, domain) => items(`${id(PATHS.metrics, inst)}${domain ? `?domain=${encodeURIComponent(domain)}` : ""}`),
    report: (inst, kind) => request("GET", id(REPORT[kind], inst)),
    widgets: (inst) => items(id(PATHS.widgets, inst)),
    createWidget: (inst, input) => request("POST", id(PATHS.widgets, inst), { body: input, idempotent: true }),
    widgetCommand: (wid, v, cmd, reason) => request("POST", id(WIDGET_CMD[cmd], wid), { body: reasonBody(reason), ifMatch: v }),
    dashboards: (inst) => items(id(PATHS.dashboards, inst)),
    createDashboard: (inst, input) => request("POST", id(PATHS.dashboards, inst), { body: input, idempotent: true }),
    createVersion: (did, widgetIds, changeNote) => request("POST", id(PATHS.versions, did), { body: { widgetIds, changeNote: changeNote || undefined }, idempotent: true }),
    dashboardCommand: (did, v, cmd, reason) => request("POST", id(DASHBOARD_CMD[cmd], did), { body: reasonBody(reason), ifMatch: v }),
    decideDashboard: (did, v, decision, reason) => request("POST", id(PATHS.dashboardDecision, did), { body: decisionBody(decision, reason), ifMatch: v }),
    rollback: (did, v, versionNo, reason) => request("POST", id(PATHS.rollback, did), { body: { versionNo, reason }, ifMatch: v }),
    assign: (did, role) => request("POST", id(PATHS.assignments, did), { body: { role }, idempotent: true }),
    revoke: (did, aid, v, reason) => request("POST", fill(PATHS.revoke, { id: did, assignmentId: aid }), { body: reasonBody(reason), ifMatch: v }),
    render: (did) => request("GET", id(PATHS.render, did)),
    frameworks: (inst) => items(id(PATHS.frameworks, inst)),
    createFramework: (inst, input) => request("POST", id(PATHS.frameworks, inst), { body: input, idempotent: true }),
    frameworkCommand: (fid, v, cmd) => request("POST", id(FRAMEWORK_CMD[cmd], fid), { body: {}, ifMatch: v }),
    decideFramework: (fid, v, decision, reason) => request("POST", id(PATHS.frameworkDecision, fid), { body: decisionBody(decision, reason), ifMatch: v }),
    cycles: (inst) => items(id(PATHS.cycles, inst)),
    createCycle: (fid, input) => request("POST", id(PATHS.frameworkCycles, fid), { body: input, idempotent: true }),
    cycle: (cid) => request("GET", id(PATHS.cycle, cid)),
    collect: (cid, manualValues) => request("POST", id(PATHS.collect, cid), { body: { manualValues }, idempotent: true }),
    snapshot: (sid) => request("GET", id(PATHS.snapshot, sid)),
    snapshotCommand: (sid, v, cmd, reason) => request("POST", id(SNAPSHOT_CMD[cmd], sid), { body: reasonBody(reason), ifMatch: v }),
    createPackage: (cid, snapshotId) => request("POST", id(PATHS.packages, cid), { body: { snapshotId: snapshotId || undefined }, idempotent: true }),
    packageCommand: (pid, v, cmd, reason) => request("POST", id(PACKAGE_CMD[cmd], pid), { body: reasonBody(reason), ifMatch: v }),
    decidePackage: (pid, v, decision, reason) => request("POST", id(PATHS.packageDecision, pid), { body: decisionBody(decision, reason), ifMatch: v }),
    acknowledge: (pid, v, reference) => request("POST", id(PATHS.packageAcknowledge, pid), { body: { reference }, ifMatch: v }),
    closeCycle: (cid, v, reason) => request("POST", id(PATHS.cycleClose, cid), { body: reasonBody(reason), ifMatch: v }),
  };
}
