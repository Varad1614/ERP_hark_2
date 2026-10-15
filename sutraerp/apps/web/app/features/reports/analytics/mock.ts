/**
 * In-memory stand-in of the slice-8C reports & analytics API (mock mode and tests): a metrics
 * catalogue over four domains, three active widgets, a published campus dashboard (v1 superseded
 * by v2, assigned to administrators), an active NIRF framework with one closed cycle and one cycle
 * whose snapshot still has a validation issue. Legal transitions only (409 otherwise), mirroring
 * the API's state machines; figures stay decimal strings.
 */
import { ApiProblem } from "@sutra/api-client";
import type { AnalyticsApi } from "./api";
import type { Cycle, Dashboard, DashboardAssignment, DashboardVersion, Framework, ManualField, MetricValue, RenderedWidget, Snapshot, SubmissionPackage, Widget } from "./types";

const conflict = (detail: string) => new ApiProblem({ type: "about:blank", title: "Conflict", status: 409, errorCode: "conflict", detail });
const notFound = () => new ApiProblem({ type: "about:blank", title: "Not found", status: 404, errorCode: "not-found" });
const invalid = (detail: string) => new ApiProblem({ type: "about:blank", title: "Unprocessable", status: 422, errorCode: "validation-failed", detail });

const INST = "00000000-0000-7000-8000-0000000b8c01";
let seq = 800;
const uid = () => `00000000-0000-7000-8000-${String(++seq).padStart(12, "0")}`;
const now = () => new Date().toISOString();
const pad = (n: number) => String(n).padStart(6, "0");

const metric = (domain: string, key: string, label: string, value: string, unit = "count"): MetricValue => ({ domain, key: `${domain}.${key}`, label, value, unit });

export function createMockAnalyticsApi(): AnalyticsApi {
  const metrics: MetricValue[] = [
    metric("student", "active_students", "Active students", "1240"),
    metric("student", "enrolled_students", "Enrolled students", "1198"), // 9E: home KPIs
    metric("people", "working_staff", "Working staff", "186"),
    metric("people", "teaching_staff", "Teaching staff", "112"),
    metric("finance", "receivables_outstanding", "Receivables outstanding", "1845000.00", "inr"),
    metric("finance", "payments_collected", "Payments collected", "9230500.00", "inr"),
    metric("finance", "open_charges", "Open charges", "318"),
    metric("campus", "active_buildings", "Active buildings", "6"),
    metric("campus", "active_spaces", "Active spaces", "148"),
    metric("campus", "seating_capacity", "Seating capacity", "5320"),
    metric("campus", "open_tickets", "Open service tickets", "14"),
    metric("campus", "tickets_past_sla", "Tickets past SLA", "2"),
    metric("campus", "open_work_orders", "Open work orders", "9"), // 9E: home KPIs
    metric("campus", "library_members", "Library members", "980"),
    metric("campus", "library_titles", "Catalogued titles", "4210"),
    metric("campus", "library_loans_active", "Loans", "312"),
    metric("campus", "library_loans_overdue", "Overdue loans", "27"),
    metric("campus", "library_fines_unsettled", "Unsettled library fines", "1460.00", "inr"),
    metric("resources", "active_items", "Active items", "540"),
  ];
  const fig = (key: string) => metrics.find((m) => m.key === key);

  const widget = (code: string, title: string, metricKey: string, visualization: Widget["visualization"] = "stat"): Widget => ({
    id: uid(), institutionId: INST, code, title, metricKey, visualization, status: "active", version: 2,
  });
  const widgets: Widget[] = [
    widget("W-STUDENTS", "Active students", "student.active_students"),
    widget("W-SPACES", "Active spaces", "campus.active_spaces"),
    widget("W-TICKETS", "Open service tickets", "campus.open_tickets", "gauge"),
    widget("W-LOANS", "Library loans", "campus.library_loans_active"),
    widget("W-RECEIVABLES", "Receivables outstanding", "finance.receivables_outstanding"),
  ];
  const slots = (ids: string[]): DashboardVersion["widgets"] =>
    ids.map((wid, i) => {
      const w = widgets.find((x) => x.id === wid);
      if (!w) throw invalid(`unknown widget ${wid}`);
      return { widgetId: w.id, code: w.code, title: w.title, metricKey: w.metricKey, visualization: w.visualization, position: i + 1 };
    });
  const campusId = uid();
  const v1: DashboardVersion = { id: uid(), versionNo: 1, status: "superseded", createdAt: "2026-09-01T09:00:00Z", publishedAt: "2026-09-02T09:00:00Z", widgets: slots([widgets[0]!.id, widgets[1]!.id]) };
  const v2: DashboardVersion = { id: uid(), versionNo: 2, status: "published", changeNote: "Add service tickets and library loans", createdAt: "2026-10-01T09:00:00Z", publishedAt: "2026-10-02T09:00:00Z", widgets: slots([widgets[0]!.id, widgets[1]!.id, widgets[2]!.id, widgets[3]!.id]) };
  const dashboards: Dashboard[] = [
    {
      id: campusId, institutionId: INST, code: "CAMPUS-OPS", name: "Campus operations", purpose: "Weekly campus review for the principal’s office", category: "campus", status: "published",
      currentVersionId: v2.id, currentVersionNo: 2, version: 6, versions: [v2, v1],
      assignments: [{ id: uid(), dashboardId: campusId, role: "administrator", status: "active", version: 1 }],
    },
  ];

  const plan = { metricKeys: ["student.active_students", "people.teaching_staff", "campus.seating_capacity", "finance.payments_collected"], manualFields: [{ key: "naac_grade", label: "NAAC grade", required: true }, { key: "remarks", label: "Remarks", required: false }] as ManualField[] };
  const nirf: Framework = {
    id: uid(), institutionId: INST, code: "NIRF", name: "NIRF data capture", regulator: "NIRF", versionNo: 1, status: "active", approvalState: "approved", version: 3, ...plan,
  };
  const frameworks: Framework[] = [nirf];
  const cycle = (n: number, periodLabel: string, status: Cycle["status"]): Cycle => ({
    id: uid(), institutionId: INST, number: `RC-${pad(n)}`, frameworkId: nirf.id, frameworkCode: nirf.code, frameworkVersionNo: 1, periodLabel, status, dueOn: n === 1 ? "2026-01-31" : "2027-01-31",
    collectionPlan: { metricKeys: [...plan.metricKeys], manualFields: [...plan.manualFields] }, version: 4,
  });
  const cycles: Cycle[] = [cycle(2, "2025-26", "validating"), cycle(1, "2024-25", "closed")];
  const valuesOf = (c: Cycle, manual: Record<string, string>): Snapshot["values"] => [
    ...c.collectionPlan.metricKeys.map((k) => ({ key: k, label: fig(k)?.label ?? k, source: "metric", unit: fig(k)?.unit ?? null, value: fig(k)?.value ?? null })),
    ...c.collectionPlan.manualFields.filter((f) => manual[f.key]).map((f) => ({ key: f.key, label: f.label, source: "manual", valueText: manual[f.key] ?? null })),
  ];
  const issuesOf = (c: Cycle, manual: Record<string, string>): Snapshot["issues"] => [
    ...c.collectionPlan.metricKeys.filter((k) => !fig(k)).map((k) => ({ key: k, issue: "figure not reported by its owning domain" })),
    ...c.collectionPlan.manualFields.filter((f) => f.required && !(manual[f.key] ?? "").trim()).map((f) => ({ key: f.key, issue: "required manual field missing" })),
    ...Object.keys(manual).filter((k) => !c.collectionPlan.manualFields.some((f) => f.key === k)).map((k) => ({ key: k, issue: "not a field of this collection plan" })),
  ];
  const snap = (c: Cycle, n: number, status: Snapshot["status"], manual: Record<string, string>): Snapshot => ({
    id: uid(), cycleId: c.id, seq: n, status, createdAt: now(), values: valuesOf(c, manual), issues: issuesOf(c, manual), version: 1,
  });
  const closedSnap = snap(cycles[1]!, 1, "validated", { naac_grade: "A" });
  const snapshots: Snapshot[] = [snap(cycles[0]!, 1, "collected", {}), closedSnap];
  const packages: SubmissionPackage[] = [
    {
      id: uid(), institutionId: INST, cycleId: cycles[1]!.id, number: "SUB-000001", snapshotId: closedSnap.id, status: "acknowledged", submittedAt: "2026-01-20T10:00:00Z",
      acknowledgedAt: "2026-01-25T10:00:00Z", acknowledgementRef: "NIRF-2025-0042", version: 5,
    },
  ];

  const find = <T extends { id: string; version: number }>(list: T[], id: string, version?: number): T => {
    const x = list.find((i) => i.id === id);
    if (!x) throw notFound();
    if (version !== undefined && x.version !== version) throw new ApiProblem({ type: "about:blank", title: "Precondition failed", status: 412, errorCode: "precondition-failed" });
    return x;
  };
  const bump = <T extends { version: number }>(x: T, patch: Partial<T>): T => Object.assign(x, patch, { version: x.version + 1 });
  const versionIn = (d: Dashboard, status: DashboardVersion["status"]) => d.versions.find((v) => v.status === status);
  const detail = (c: Cycle) => ({ cycle: c, snapshots: snapshots.filter((s) => s.cycleId === c.id).sort((a, b) => b.seq - a.seq), packages: packages.filter((p) => p.cycleId === c.id).reverse() });

  return {
    metrics: async (_inst, domain) => metrics.filter((m) => !domain || m.domain === domain),
    report: async (_inst, kind) => ({
      institutionId: INST,
      report: kind,
      generatedAt: now(),
      items: kind === "library" ? metrics.filter((m) => m.key.startsWith("campus.library_")) : ["campus.active_buildings", "campus.active_spaces", "campus.seating_capacity", "campus.open_tickets", "campus.tickets_past_sla"].flatMap((k) => metrics.filter((m) => m.key === k)),
    }),

    widgets: async () => [...widgets],
    createWidget: async (_inst, input) => {
      if (!fig(input.metricKey)) throw invalid(`unknown metric \`${input.metricKey}\``);
      if (widgets.some((w) => w.code === input.code)) throw conflict(`widget ${input.code} exists`);
      const w: Widget = { id: uid(), institutionId: INST, code: input.code, title: input.title, description: input.description ?? null, metricKey: input.metricKey, visualization: input.visualization ?? "stat", status: "draft", version: 1 };
      widgets.push(w);
      return w;
    },
    widgetCommand: async (id, v, cmd, reason) => {
      const w = find(widgets, id, v);
      if (cmd === "activate") {
        if (w.status !== "draft") throw conflict("only a draft widget is activated");
        return bump(w, { status: "active" });
      }
      if (w.status === "retired") throw conflict("already retired");
      return bump(w, { status: "retired", statusReason: reason ?? null });
    },

    dashboards: async () => [...dashboards],
    createDashboard: async (_inst, input) => {
      if (dashboards.some((d) => d.code === input.code)) throw conflict(`dashboard ${input.code} exists`);
      const d: Dashboard = { id: uid(), institutionId: INST, code: input.code, name: input.name, purpose: input.purpose, category: input.category ?? "operational", status: "draft", version: 1, versions: [], assignments: [] };
      dashboards.unshift(d);
      return d;
    },
    createVersion: async (id, widgetIds, changeNote) => {
      const d = find(dashboards, id);
      if (versionIn(d, "in_review") || versionIn(d, "approved")) throw conflict("a version is in review");
      if (!widgetIds.length) throw invalid("pick at least one widget");
      for (const wid of widgetIds) {
        const w = find(widgets, wid);
        if (w.status !== "active") throw invalid(`widget ${w.code} is not active`);
        if (d.category === "finance" && !w.metricKey.startsWith("finance.")) throw invalid(`\`${w.metricKey}\` is not a canonical Finance figure`);
      }
      const draft = versionIn(d, "draft");
      if (draft) Object.assign(draft, { widgets: slots(widgetIds), changeNote: changeNote ?? null });
      else d.versions.unshift({ id: uid(), versionNo: Math.max(0, ...d.versions.map((x) => x.versionNo)) + 1, status: "draft", createdAt: now(), changeNote: changeNote ?? null, widgets: slots(widgetIds) });
      return bump(d, {});
    },
    dashboardCommand: async (id, v, cmd) => {
      const d = find(dashboards, id, v);
      if (cmd === "submit") {
        const draft = versionIn(d, "draft");
        if (!draft) throw conflict("no draft version to submit");
        draft.status = "in_review";
        return bump(d, { status: d.status === "published" ? "published" : "in_review" });
      }
      const approved = versionIn(d, "approved");
      if (!approved) throw conflict("no approved version to publish");
      const current = versionIn(d, "published");
      if (current) current.status = "superseded";
      Object.assign(approved, { status: "published", publishedAt: now() });
      return bump(d, { status: "published", currentVersionId: approved.id, currentVersionNo: approved.versionNo });
    },
    decideDashboard: async (id, v, decision, reason) => {
      const d = find(dashboards, id, v);
      const review = versionIn(d, "in_review");
      if (!review) throw conflict("no version in review");
      if (decision === "reject" && !reason) throw invalid("a rejection needs a reason");
      Object.assign(review, { status: decision === "approve" ? "approved" : "rejected", statusReason: reason ?? null });
      return bump(d, { status: d.currentVersionId ? "published" : "draft" });
    },
    rollback: async (id, v, versionNo, reason) => {
      const d = find(dashboards, id, v);
      const target = d.versions.find((x) => x.versionNo === versionNo);
      if (!target || target.status !== "superseded") throw conflict(`version ${versionNo} is not an earlier published version`);
      if (!reason) throw invalid("a reason is required");
      const current = versionIn(d, "published");
      if (current) current.status = "superseded";
      Object.assign(target, { status: "published", publishedAt: now() });
      return bump(d, { currentVersionId: target.id, currentVersionNo: target.versionNo, statusReason: reason });
    },
    assign: async (id, role) => {
      const d = find(dashboards, id);
      if (d.assignments.some((a) => a.role === role && a.status === "active")) throw conflict(`already assigned to ${role}`);
      const a: DashboardAssignment = { id: uid(), dashboardId: id, role, status: "active", version: 1 };
      d.assignments.push(a);
      return a;
    },
    revoke: async (dashboardId, assignmentId, v) => {
      const d = find(dashboards, dashboardId);
      const a = find(d.assignments, assignmentId, v);
      if (a.status !== "active") throw conflict("already revoked");
      return bump(a, { status: "revoked" });
    },
    render: async (id) => {
      const d = find(dashboards, id);
      const pub = versionIn(d, "published");
      if (!pub) throw conflict("the dashboard is not published yet");
      const rendered: RenderedWidget[] = pub.widgets.map((w) => ({ ...w, metric: fig(w.metricKey) ?? null }));
      return { dashboardId: d.id, institutionId: INST, code: d.code, name: d.name, category: d.category, versionNo: pub.versionNo, generatedAt: now(), widgets: rendered };
    },

    frameworks: async () => [...frameworks],
    createFramework: async (_inst, input) => {
      const unknown = input.metricKeys.filter((k) => !fig(k));
      if (unknown.length) throw invalid(`unknown figures: ${unknown.join(", ")}`);
      const prev = frameworks.filter((f) => f.code === input.code);
      const f: Framework = {
        id: uid(), institutionId: INST, code: input.code, name: input.name, regulator: input.regulator, metricKeys: input.metricKeys, manualFields: input.manualFields ?? [],
        versionNo: prev.length + 1, predecessorId: prev.find((p) => p.status === "active")?.id ?? null, status: "draft", approvalState: "not_requested", version: 1,
      };
      frameworks.unshift(f);
      return f;
    },
    frameworkCommand: async (id, v, cmd) => {
      const f = find(frameworks, id, v);
      if (f.status !== "draft") throw conflict("only a draft framework changes");
      if (cmd === "submit") {
        if (f.approvalState === "pending" || f.approvalState === "approved") throw conflict("already submitted");
        return bump(f, { approvalState: "pending" });
      }
      if (f.approvalState !== "approved") throw conflict("approve the framework first");
      for (const o of frameworks) if (o.code === f.code && o.status === "active") bump(o, { status: "superseded" });
      return bump(f, { status: "active" });
    },
    decideFramework: async (id, v, decision, reason) => {
      const f = find(frameworks, id, v);
      if (f.approvalState !== "pending") throw conflict("the framework is not awaiting a decision");
      return bump(f, { approvalState: decision === "approve" ? "approved" : "rejected", statusReason: reason ?? null });
    },

    cycles: async () => [...cycles],
    createCycle: async (frameworkId, input) => {
      const f = find(frameworks, frameworkId);
      if (f.status !== "active") throw conflict("only an active framework opens cycles");
      if (cycles.some((c) => c.frameworkId === f.id && c.periodLabel === input.periodLabel)) throw conflict(`${input.periodLabel} is already open`);
      const c: Cycle = {
        id: uid(), institutionId: INST, number: `RC-${pad(cycles.length + 1)}`, frameworkId: f.id, frameworkCode: f.code, frameworkVersionNo: f.versionNo, periodLabel: input.periodLabel,
        dueOn: input.dueOn ?? null, status: "planned", collectionPlan: { metricKeys: [...f.metricKeys], manualFields: [...f.manualFields] }, version: 1,
      };
      cycles.unshift(c);
      return c;
    },
    cycle: async (id) => detail(find(cycles, id)),
    collect: async (cycleId, manualValues) => {
      const c = find(cycles, cycleId);
      if (!["planned", "collecting", "validating"].includes(c.status)) throw conflict(`a ${c.status} cycle does not collect`);
      for (const s of snapshots) if (s.cycleId === c.id && s.status === "collected") bump(s, { status: "rejected", statusReason: "superseded by a new collection" });
      const n = Math.max(0, ...snapshots.filter((s) => s.cycleId === c.id).map((s) => s.seq)) + 1;
      const s = snap(c, n, "collected", manualValues);
      snapshots.push(s);
      bump(c, { status: "validating" });
      return s;
    },
    snapshot: async (id) => find(snapshots, id),
    snapshotCommand: async (id, v, cmd, reason) => {
      const s = find(snapshots, id, v);
      if (s.status !== "collected") throw conflict(`a ${s.status} snapshot is decided`);
      const c = find(cycles, s.cycleId);
      if (cmd === "validate") {
        if (s.issues.length) throw conflict(`${s.issues.length} validation issue(s) remain`);
        return bump(s, { status: "validated" });
      }
      if (!reason) throw invalid("a reason is required");
      bump(c, { status: "collecting" });
      return bump(s, { status: "rejected", statusReason: reason });
    },
    createPackage: async (cycleId, snapshotId) => {
      const c = find(cycles, cycleId);
      if (c.status !== "validating") throw conflict(`a ${c.status} cycle is not packaged`);
      const s = snapshotId ? find(snapshots, snapshotId) : snapshots.filter((x) => x.cycleId === c.id && x.status === "validated").sort((a, b) => b.seq - a.seq)[0];
      if (!s || s.status !== "validated") throw conflict("validate a snapshot first");
      const corrected = packages.filter((p) => p.cycleId === c.id && p.status === "corrected").pop();
      const p: SubmissionPackage = { id: uid(), institutionId: INST, cycleId: c.id, number: `SUB-${pad(packages.length + 1)}`, snapshotId: s.id, correctedFromId: corrected?.id ?? null, status: "draft", version: 1 };
      packages.push(p);
      bump(c, { status: "packaged" });
      return p;
    },
    packageCommand: async (id, v, cmd, reason) => {
      const p = find(packages, id, v);
      const c = find(cycles, p.cycleId);
      if (cmd === "submit") {
        if (p.status === "draft") return bump(p, { status: "pending_approval" });
        if (p.status !== "approved") throw conflict(`a ${p.status} package is not submitted`);
        bump(c, { status: "submitted" });
        return bump(p, { status: "submitted", submittedAt: now() });
      }
      if (p.status !== "submitted" && p.status !== "acknowledged") throw conflict("only a submitted package is corrected");
      if (!reason) throw invalid("a reason is required");
      bump(c, { status: "collecting" });
      return bump(p, { status: "corrected", statusReason: reason });
    },
    decidePackage: async (id, v, decision, reason) => {
      const p = find(packages, id, v);
      if (p.status !== "pending_approval") throw conflict("the package is not awaiting approval");
      return bump(p, { status: decision === "approve" ? "approved" : "draft", statusReason: reason ?? null, approvedBy: decision === "approve" ? "Registrar" : null });
    },
    acknowledge: async (id, v, reference) => {
      const p = find(packages, id, v);
      if (p.status !== "submitted") throw conflict("only a submitted package is acknowledged");
      return bump(p, { status: "acknowledged", acknowledgementRef: reference, acknowledgedAt: now() });
    },
    closeCycle: async (id, v, reason) => {
      const c = find(cycles, id, v);
      const last = packages.filter((p) => p.cycleId === c.id).pop();
      if (c.status !== "submitted" || last?.status !== "acknowledged") throw conflict("close a cycle once its submission is acknowledged");
      bump(c, { status: "closed", statusReason: reason ?? null });
      return detail(c);
    },
  };
}
