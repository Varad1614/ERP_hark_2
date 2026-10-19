/**
 * Slice **8C** web components (reports and analytics). Export names are imported by the foundation
 * frame `routes/admin/reports.tsx` (docs/batch8-contracts.md §10): keep them. Every screen carries
 * `data-slice="8C"` and uses @sutra/ui only (no custom styling). Figures are shown exactly as the
 * API sends them (decimal strings from every domain's `OperationalMetricsPort`).
 *
 * - `DashboardsAdminTab` — Admin → Reports → Dashboards (L2-H07): the live view of a published
 *   dashboard, governed dashboards (versions → review → publish → rollback, role assignments),
 *   widget definitions bound to metric keys, the metrics catalogue and the library /
 *   infrastructure operational reports (G-154, G-320).
 * - `RegulatoryAdminTab` — Admin → Reports → Regulatory reporting (L2-H08): reporting frameworks
 *   (AISHE / NIRF / NAAC data definitions; submit → approve → activate), cycles with their
 *   collection plan, collected snapshots with validation issues (validate / reject) and submission
 *   packages (approval → submitted → acknowledged → correction) up to closing the cycle.
 */
import { BarChart3, CheckCircle2, ClipboardList, FileCheck2, FileText, Gauge, LayoutDashboard, ListChecks, Send, ShieldCheck, Users } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, FilterChip, KpiTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, NoInstitution, REASON, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { ApprovalBadge, Actions } from "../../campus/ui";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { akeys, useAnalyticsApi } from "./hooks";
import type { Cycle, CycleDetail, Dashboard, DashboardAssignment, DashboardCategory, DashboardVersion, Framework, MetricValue, RenderedWidget, ReportKind, Snapshot, SnapshotValue, SubmissionPackage, Widget, WidgetVisualization } from "./types";
import { formatInr } from "../../finance";

export { AnalyticsApiProvider } from "./hooks";

const CATEGORIES: DashboardCategory[] = ["operational", "finance", "academic", "campus", "people", "regulatory"];
const VISUALIZATIONS: WidgetVisualization[] = ["stat", "gauge", "table"];
const ROLES = ["administrator", "faculty", "student"] as const;
const REGULATORS = ["AISHE", "NIRF", "NAAC", "Other"] as const;
const REPORTS: { key: ReportKind; label: string }[] = [
  { key: "library", label: "Library" },
  { key: "infrastructure", label: "Infrastructure" },
];

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="8C">
      <NoInstitution />
    </BentoTile>
  );
}

/** A figure with its unit (`inr` → ₹, `percent` → %, `days`); missing → "—". */
export function figure(m: Pick<MetricValue, "value" | "unit"> | null | undefined): string {
  if (!m) return "—";
  switch (m.unit) {
    case "inr":
      return formatInr(m.value);
    case "percent":
      return `${m.value}%`;
    case "days":
      return `${m.value} days`;
    default:
      return m.value;
  }
}

/** ISO timestamp → date text. */
const when = (ts: string | null | undefined) => (ts ? fmtDate(ts.slice(0, 10)) : "—");

// ================================================================== action rules (pure, tested)

/** Next steps offered for a widget definition. */
export function widgetActions(w: Pick<Widget, "status">): Array<"activate" | "retire"> {
  if (w.status === "draft") return ["activate", "retire"];
  if (w.status === "active") return ["retire"];
  return [];
}

/** Next steps offered for a dashboard, from the states of its versions. */
export function dashboardActions(d: Pick<Dashboard, "status" | "versions" | "currentVersionId">): Array<"version" | "submit" | "approve" | "reject" | "publish" | "rollback" | "assign"> {
  if (d.status === "retired") return [];
  const has = (s: DashboardVersion["status"]) => d.versions.some((v) => v.status === s);
  const out: Array<"version" | "submit" | "approve" | "reject" | "publish" | "rollback" | "assign"> = [];
  if (has("draft")) out.push("submit");
  if (has("in_review")) out.push("approve", "reject");
  if (has("approved")) out.push("publish");
  if (!has("in_review") && !has("approved")) out.push("version");
  if (has("superseded") && d.currentVersionId) out.push("rollback");
  out.push("assign");
  return out;
}

/** Next steps offered for a reporting framework version. */
export function frameworkActions(f: Pick<Framework, "status" | "approvalState">): Array<"submit" | "approve" | "reject" | "activate" | "cycle"> {
  if (f.status === "active") return ["cycle"];
  if (f.status !== "draft") return [];
  if (f.approvalState === "pending") return ["approve", "reject"];
  if (f.approvalState === "approved") return ["activate"];
  return ["submit"];
}

/** Next steps offered for a snapshot: one with open issues can only be rejected. */
export function snapshotActions(s: Pick<Snapshot, "status" | "issues">): Array<"validate" | "reject"> {
  if (s.status !== "collected") return [];
  return s.issues.length ? ["reject"] : ["validate", "reject"];
}

/** Next steps offered for a submission package. */
export function packageActions(p: Pick<SubmissionPackage, "status">): Array<"submit" | "approve" | "reject" | "acknowledge" | "correct"> {
  switch (p.status) {
    case "draft":
    case "approved":
      return ["submit"];
    case "pending_approval":
      return ["approve", "reject"];
    case "submitted":
      return ["acknowledge", "correct"];
    case "acknowledged":
      return ["correct"];
    default:
      return [];
  }
}

/** Next steps offered for a reporting cycle given its snapshots and packages. */
export function cycleActions(d: CycleDetail): Array<"collect" | "package" | "close"> {
  const s = d.cycle.status;
  const out: Array<"collect" | "package" | "close"> = [];
  if (s === "planned" || s === "collecting" || s === "validating") out.push("collect");
  if (s === "validating" && d.snapshots.some((x) => x.status === "validated")) out.push("package");
  const last = d.packages[0];
  if (s === "submitted" && last?.status === "acknowledged") out.push("close");
  return out;
}

// ================================================================== Admin → Reports → Dashboards

function DashboardsScreen({ institutionId }: { institutionId: string }) {
  const api = useAnalyticsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [selected, setSelected] = useState<string | null>(null);
  const [domain, setDomain] = useState("all");
  const [reportKind, setReportKind] = useState<ReportKind>("library");
  const metrics = useQuery({ queryKey: akeys(institutionId, "metrics"), queryFn: () => api.metrics(institutionId) }).data ?? [];
  const widgets = useQuery({ queryKey: akeys(institutionId, "widgets"), queryFn: () => api.widgets(institutionId) }).data ?? [];
  const dashboards = useQuery({ queryKey: akeys(institutionId, "dashboards"), queryFn: () => api.dashboards(institutionId) }).data ?? [];
  const report = useQuery({ queryKey: akeys(institutionId, "report", reportKind), queryFn: () => api.report(institutionId, reportKind) }).data;
  const current = dashboards.find((d) => d.id === selected) ?? dashboards.find((d) => d.status === "published" || !!d.currentVersionId) ?? dashboards[0] ?? null;
  const currentId = current?.id ?? "";
  const live = !!current?.currentVersionId;
  const view = useQuery({ queryKey: akeys(currentId || "none", "render", String(current?.currentVersionNo ?? "")), queryFn: () => api.render(currentId), enabled: live }).data;

  const domains = ["all", ...Array.from(new Set(metrics.map((m) => m.domain)))];
  const shown = domain === "all" ? metrics : metrics.filter((m) => m.domain === domain);
  const active = widgets.filter((w) => w.status === "active");
  const published = dashboards.filter((d) => !!d.currentVersionId);
  const assignments = dashboards.flatMap((d) => d.assignments).filter((a) => a.status === "active");
  const byKey = (k: string) => metrics.find((m) => m.key === k);

  const newWidget = () =>
    ask({
      title: "Define a widget",
      name: "widget-new",
      fields: [
        { name: "code", label: "Code", required: true, placeholder: "W-OVERDUE" },
        { name: "title", label: "Title", required: true },
        { name: "metricKey", label: "Figure", type: "select", required: true, wide: true, options: metrics.map((m) => ({ value: m.key, label: `${m.label} · ${statusLabel(m.domain)}` })) },
        { name: "visualization", label: "Display", type: "select", required: true, initial: "stat", options: VISUALIZATIONS.map((v) => ({ value: v, label: statusLabel(v) })) },
        { name: "description", label: "Description", wide: true },
      ],
      submitLabel: "Define",
      onSubmit: (v) =>
        submit(
          () => api.createWidget(institutionId, { code: v.code!.trim().toUpperCase(), title: v.title!, metricKey: v.metricKey!, visualization: v.visualization as WidgetVisualization, description: v.description || undefined }),
          (w) => `Widget ${w.code} defined`,
        ),
    });

  const widgetRowActions = (w: Widget): RowAction[] =>
    widgetActions(w).map((a): RowAction =>
      a === "activate"
        ? { id: "widget-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.widgetCommand(w.id, w.version, "activate"), `${w.code} active`) }
        : {
            id: "widget-retire",
            label: "Retire",
            onClick: () => ask({ title: `Retire ${w.code}`, name: "widget-retire", fields: [REASON], submitLabel: "Retire", onSubmit: (v) => submit(() => api.widgetCommand(w.id, w.version, "retire", v.reason), `${w.code} retired`) }),
          },
    );

  const newDashboard = () =>
    ask({
      title: "New dashboard",
      name: "dashboard-new",
      fields: [
        { name: "code", label: "Code", required: true, placeholder: "FIN-KPI" },
        { name: "name", label: "Name", required: true },
        { name: "category", label: "Category", type: "select", required: true, initial: "operational", options: CATEGORIES.map((c) => ({ value: c, label: statusLabel(c) })) },
        { name: "purpose", label: "What it must answer, for whom", required: true, wide: true },
      ],
      submitLabel: "Create",
      onSubmit: (v) =>
        submit(
          () => api.createDashboard(institutionId, { code: v.code!.trim().toUpperCase(), name: v.name!, purpose: v.purpose!, category: v.category as DashboardCategory }),
          (d) => {
            setSelected(d.id);
            return `Dashboard ${d.code} recorded`;
          },
        ),
    });

  const newVersion = (d: Dashboard) => {
    const inUse = new Set((d.versions.find((x) => x.status === "draft") ?? d.versions.find((x) => x.status === "published"))?.widgets.map((w) => w.widgetId) ?? []);
    const fields: FieldSpec[] = active.map((w, i): FieldSpec => ({ name: `w_${i}`, label: `${w.title} (${w.metricKey})`, type: "checkbox", initial: inUse.has(w.id) ? "true" : "false" }));
    ask({
      title: `Layout of ${d.code}`,
      name: "dashboard-version",
      fields: [...fields, { name: "changeNote", label: "Change note", wide: true }],
      submitLabel: "Save draft version",
      onSubmit: (v) =>
        submit(
          () =>
            api.createVersion(
              d.id,
              active.filter((_, i) => v[`w_${i}`] === "true").map((w) => w.id),
              v.changeNote,
            ),
          (r) => `${r.code}: draft version ready`,
        ),
    });
  };

  const dashboardRowActions = (d: Dashboard): RowAction[] => {
    const acts: RowAction[] = [{ id: "dashboard-open", label: d.id === currentId ? "Open ✓" : "Open", onClick: () => setSelected(d.id) }];
    for (const a of dashboardActions(d)) {
      if (a === "version") acts.push({ id: "dashboard-version", label: "New version", onClick: () => newVersion(d) });
      if (a === "submit") acts.push({ id: "dashboard-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.dashboardCommand(d.id, d.version, "submit"), (r) => `${r.code} submitted for review`) });
      if (a === "approve") acts.push({ id: "dashboard-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideDashboard(d.id, d.version, "approve"), (r) => `${r.code} version approved`) });
      if (a === "reject")
        acts.push({
          id: "dashboard-reject",
          label: "Reject",
          onClick: () => ask({ title: `Reject the ${d.code} version`, name: "dashboard-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideDashboard(d.id, d.version, "reject", v.reason), (r) => `${r.code} version rejected`) }),
        });
      if (a === "publish") acts.push({ id: "dashboard-publish", label: "Publish", variant: "primary", onClick: () => void run(() => api.dashboardCommand(d.id, d.version, "publish"), (r) => `${r.code} v${r.currentVersionNo ?? ""} published`) });
      if (a === "rollback")
        acts.push({
          id: "dashboard-rollback",
          label: "Roll back",
          onClick: () =>
            ask({
              title: `Roll back ${d.code}`,
              name: "dashboard-rollback",
              fields: [
                { name: "versionNo", label: "Re-publish version", type: "select", required: true, options: d.versions.filter((x) => x.status === "superseded").map((x) => ({ value: String(x.versionNo), label: `v${x.versionNo}${x.changeNote ? ` · ${x.changeNote}` : ""}` })) },
                REASON,
              ],
              submitLabel: "Roll back",
              onSubmit: (v) => submit(() => api.rollback(d.id, d.version, Number(v.versionNo), v.reason ?? ""), (r) => `${r.code} rolled back to v${r.currentVersionNo ?? v.versionNo}`),
            }),
        });
      if (a === "assign")
        acts.push({
          id: "dashboard-assign",
          label: "Assign",
          onClick: () =>
            ask({
              title: `Assign ${d.code} to a role`,
              name: "dashboard-assign",
              fields: [{ name: "role", label: "Role", type: "select", required: true, initial: "administrator", options: ROLES.map((r) => ({ value: r, label: statusLabel(r) })) }],
              submitLabel: "Assign",
              onSubmit: (v) => submit(() => api.assign(d.id, v.role!), (r) => `${d.code} assigned to ${r.role}`),
            }),
        });
    }
    return acts;
  };

  const assignmentActions = (d: Dashboard, a: DashboardAssignment): RowAction[] =>
    a.status === "active"
      ? [{ id: "assignment-revoke", label: "Revoke", onClick: () => ask({ title: `Revoke ${d.code} for ${a.role}`, name: "assignment-revoke", fields: [REASON], submitLabel: "Revoke", onSubmit: (v) => submit(() => api.revoke(d.id, a.id, a.version, v.reason), `${d.code} revoked for ${a.role}`) }) }]
      : [];

  const liveWidgets = view?.widgets ?? [];

  return (
    <>
      <KpiTile span={3} icon={BarChart3} tone="blue" label="Figures" value={String(metrics.length)} sub={`${domains.length - 1} domain(s) reporting`} data-slice="8C" data-kpi="report-figures" />
      <KpiTile span={3} icon={Gauge} tone="violet" label="Active widgets" value={String(active.length)} data-kpi="report-widgets" />
      <KpiTile span={3} icon={LayoutDashboard} tone="green" label="Published dashboards" value={String(published.length)} sub={`${dashboards.length} in total`} data-kpi="report-dashboards" />
      <KpiTile span={3} icon={Users} tone="amber" label="Role assignments" value={String(assignments.length)} data-kpi="report-assignments" />
      <BentoTile span={12} title={view ? `Live view · ${view.name} · v${view.versionNo}` : "Live view"} data-slice="8C" data-dashboard-view>
        {published.map((d) => (
          <FilterChip key={d.id} active={d.id === currentId} label={d.name} onClick={() => setSelected(d.id)} />
        ))}
        {view ? (
          <DataTable<RenderedWidget>
            caption="Dashboard figures"
            rows={[...liveWidgets].sort((a, b) => a.position - b.position)}
            rowKey={(w) => w.widgetId}
            columns={[
              { key: "title", header: "Widget", render: (w) => w.title },
              { key: "value", header: "Figure", render: (w) => (w.metric ? figure(w.metric) : "No longer reported") },
              { key: "about", header: "Measures", render: (w) => w.metric?.label ?? byKey(w.metricKey)?.label ?? "—", muted: true },
              { key: "viz", header: "Display", render: (w) => statusLabel(w.visualization), muted: true },
            ]}
          />
        ) : (
          <EmptyState icon={LayoutDashboard} title={current ? `${current.name} is not published yet` : "No dashboards yet"} compact>
            Define widgets over the figures below, lay them out in a dashboard version, submit it for review and publish it.
          </EmptyState>
        )}
        {view && <p className="sb-foot">{`Figures as of ${when(view.generatedAt)} · category ${statusLabel(view.category).toLowerCase()}`}</p>}
      </BentoTile>
      <BentoTile
        span={12}
        title="Dashboards"
        data-slice="8C"
        action={
          <Button size="sm" data-action="dashboard-new" onClick={newDashboard}>
            New dashboard
          </Button>
        }
      >
        <DataTable<Dashboard>
          caption="Dashboards"
          rows={dashboards}
          rowKey={(d) => d.id}
          empty={<EmptyState icon={LayoutDashboard} title="No dashboards yet" compact>Record a dashboard requirement: what it must answer and for whom.</EmptyState>}
          columns={[
            { key: "name", header: "Dashboard", render: (d) => `${d.code} · ${d.name}` },
            { key: "category", header: "Category", render: (d) => statusLabel(d.category), muted: true },
            { key: "current", header: "Live version", render: (d) => (d.currentVersionNo ? `v${d.currentVersionNo}` : "—"), muted: true },
            { key: "roles", header: "Roles", render: (d) => d.assignments.filter((a) => a.status === "active").map((a) => statusLabel(a.role)).join(", ") || "—", muted: true },
            { key: "status", header: "Status", render: (d) => <StatusBadge status={d.status} /> },
            { key: "actions", header: "", render: (d) => <Actions actions={dashboardRowActions(d)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {current && (
        <>
          <BentoTile span={7} title={`Versions · ${current.code}`} data-slice="8C">
            <DataTable<DashboardVersion>
              caption="Dashboard versions"
              rows={current.versions}
              rowKey={(v) => v.id}
              empty={<EmptyState icon={ListChecks} title="No versions yet" compact>Lay out a first version from the active widgets.</EmptyState>}
              columns={[
                { key: "no", header: "Version", render: (v) => `v${v.versionNo}` },
                { key: "widgets", header: "Widgets", render: (v) => v.widgets.map((w) => w.title).join(", ") || "—", muted: true },
                { key: "note", header: "Note", render: (v) => v.statusReason ?? v.changeNote ?? "—", muted: true },
                { key: "published", header: "Published", render: (v) => when(v.publishedAt), muted: true },
                { key: "status", header: "Status", render: (v) => <StatusBadge status={v.status} /> },
              ]}
            />
          </BentoTile>
          <BentoTile span={5} title={`Assignments · ${current.code}`} data-slice="8C">
            <DataTable<DashboardAssignment>
              caption="Dashboard assignments"
              rows={current.assignments}
              rowKey={(a) => a.id}
              empty={<EmptyState icon={Users} title="Not assigned" compact>Assign the dashboard to a role; it shows on their home once published.</EmptyState>}
              columns={[
                { key: "role", header: "Role", render: (a) => statusLabel(a.role) },
                { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
                { key: "actions", header: "", render: (a) => <Actions actions={assignmentActions(current, a)} disabled={busy} /> },
              ]}
            />
          </BentoTile>
        </>
      )}
      <BentoTile
        span={12}
        title="Widgets"
        data-slice="8C"
        action={
          <Button size="sm" data-action="widget-new" onClick={newWidget} disabled={!metrics.length}>
            Define widget
          </Button>
        }
      >
        <DataTable<Widget>
          caption="Widgets"
          rows={widgets}
          rowKey={(w) => w.id}
          empty={<EmptyState icon={Gauge} title="No widgets yet" compact>A widget binds one figure of the catalogue to a display.</EmptyState>}
          columns={[
            { key: "code", header: "Widget", render: (w) => `${w.code} · ${w.title}` },
            { key: "metric", header: "Figure", render: (w) => byKey(w.metricKey)?.label ?? w.metricKey, muted: true },
            { key: "now", header: "Now", render: (w) => figure(byKey(w.metricKey)) },
            { key: "viz", header: "Display", render: (w) => statusLabel(w.visualization), muted: true },
            { key: "status", header: "Status", render: (w) => <StatusBadge status={w.status} /> },
            { key: "actions", header: "", render: (w) => <Actions actions={widgetRowActions(w)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Figures catalogue" data-slice="8C">
        {domains.map((d) => (
          <FilterChip key={d} active={domain === d} label={d === "all" ? "All" : statusLabel(d)} count={d === "all" ? metrics.length : metrics.filter((m) => m.domain === d).length} onClick={() => setDomain(d)} />
        ))}
        <DataTable<MetricValue>
          caption="Operational figures"
          rows={shown}
          rowKey={(m) => m.key}
          empty={<EmptyState icon={BarChart3} title="No figures reported" compact>Every domain reports its operational figures here.</EmptyState>}
          columns={[
            { key: "label", header: "Figure", render: (m) => m.label },
            { key: "value", header: "Value", render: (m) => figure(m) },
            { key: "key", header: "Key", render: (m) => m.key, muted: true },
          ]}
        />
      </BentoTile>
      <BentoTile span={6} title="Operational reports" data-slice="8C" data-report={reportKind}>
        {REPORTS.map((r) => (
          <FilterChip key={r.key} active={reportKind === r.key} label={r.label} onClick={() => setReportKind(r.key)} />
        ))}
        <DataTable<MetricValue>
          caption={`${reportKind} report`}
          rows={report?.items ?? []}
          rowKey={(m) => m.key}
          empty={<EmptyState icon={FileText} title="Nothing to report yet" compact>The figures appear once the area records activity.</EmptyState>}
          columns={[
            { key: "label", header: "Figure", render: (m) => m.label },
            { key: "value", header: "Value", render: (m) => figure(m) },
          ]}
        />
        {report && <p className="sb-foot">{`Generated ${when(report.generatedAt)}`}</p>}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Reports → Dashboards. */
export function DashboardsAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <DashboardsScreen institutionId={institution.id} />;
}

// ================================================================== Admin → Reports → Regulatory reporting

/** `key|Label|required; …` → manual fields. */
export function parseManualFields(text: string): { key: string; label: string; required: boolean }[] {
  return text
    .split(/[;\n]/)
    .map((x) => x.trim())
    .filter(Boolean)
    .map((x) => {
      const [key = "", label = "", req = ""] = x.split("|").map((p) => p.trim());
      return { key, label: label || key, required: /^(required|yes|true)$/i.test(req) };
    });
}

function RegulatoryScreen({ institutionId }: { institutionId: string }) {
  const api = useAnalyticsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [selected, setSelected] = useState<string | null>(null);
  const [snapshotId, setSnapshotId] = useState<string | null>(null);
  const metrics = useQuery({ queryKey: akeys(institutionId, "metrics"), queryFn: () => api.metrics(institutionId) }).data ?? [];
  const frameworks = useQuery({ queryKey: akeys(institutionId, "frameworks"), queryFn: () => api.frameworks(institutionId) }).data ?? [];
  const cycles = useQuery({ queryKey: akeys(institutionId, "cycles"), queryFn: () => api.cycles(institutionId) }).data ?? [];
  const cycle = cycles.find((c) => c.id === selected) ?? cycles.find((c) => c.status !== "closed") ?? cycles[0] ?? null;
  const cycleId = cycle?.id ?? "";
  const detail = useQuery({ queryKey: akeys(cycleId || "none", "cycle"), queryFn: () => api.cycle(cycleId), enabled: cycleId !== "" }).data;
  const snapshots = detail?.snapshots ?? [];
  const packages = detail?.packages ?? [];
  const shownSnapshot = snapshots.find((s) => s.id === snapshotId) ?? snapshots[0] ?? null;
  const shownId = shownSnapshot?.id ?? "";
  const values = useQuery({ queryKey: akeys(shownId || "none", "snapshot", String(shownSnapshot?.version ?? "")), queryFn: () => api.snapshot(shownId), enabled: shownId !== "" }).data;

  const activeFrameworks = frameworks.filter((f) => f.status === "active");
  const open = cycles.filter((c) => c.status !== "closed");
  const submitted = cycles.filter((c) => c.status === "submitted" || c.status === "closed");
  const nextDue = open.filter((c) => c.status !== "submitted").map((c) => c.dueOn).filter((d): d is string => !!d).sort()[0];
  const seqOf = (id: string) => snapshots.find((s) => s.id === id)?.seq;
  const numberOf = (id: string | null | undefined) => (id ? (packages.find((p) => p.id === id)?.number ?? "earlier package") : null);

  const newFramework = () => {
    const metricFields: FieldSpec[] = metrics.map((m, i): FieldSpec => ({ name: `m_${i}`, label: `${m.label} (${m.key})`, type: "checkbox" }));
    ask({
      title: "Define a reporting framework",
      name: "framework-new",
      fields: [
        { name: "code", label: "Code", required: true, placeholder: "AISHE" },
        { name: "regulator", label: "Regulator", type: "select", required: true, initial: "AISHE", options: REGULATORS.map((r) => ({ value: r, label: r })) },
        { name: "name", label: "Name", required: true, wide: true, initial: "AISHE annual data capture" },
        ...metricFields,
        { name: "manualFields", label: "Manual fields (key|Label|required; …)", wide: true, placeholder: "naac_grade|NAAC grade|required; remarks|Remarks" },
      ],
      submitLabel: "Define",
      onSubmit: (v) =>
        submit(
          () =>
            api.createFramework(institutionId, {
              code: v.code!.trim().toUpperCase(),
              regulator: v.regulator!,
              name: v.name!,
              metricKeys: metrics.filter((_, i) => v[`m_${i}`] === "true").map((m) => m.key),
              manualFields: parseManualFields(v.manualFields ?? ""),
            }),
          (f) => `${f.code} v${f.versionNo} drafted`,
        ),
    });
  };

  const frameworkRowActions = (f: Framework): RowAction[] =>
    frameworkActions(f).map((a): RowAction => {
      switch (a) {
        case "submit":
          return { id: "framework-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.frameworkCommand(f.id, f.version, "submit"), (r) => `${r.code} ${statusLabel(r.approvalState).toLowerCase()}`) };
        case "approve":
          return { id: "framework-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideFramework(f.id, f.version, "approve"), `${f.code} approved`) };
        case "reject":
          return {
            id: "framework-reject",
            label: "Reject",
            onClick: () => ask({ title: `Reject ${f.code} v${f.versionNo}`, name: "framework-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideFramework(f.id, f.version, "reject", v.reason), `${f.code} rejected`) }),
          };
        case "activate":
          return { id: "framework-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.frameworkCommand(f.id, f.version, "activate"), `${f.code} v${f.versionNo} active`) };
        default:
          return {
            id: "cycle-new",
            label: "Open cycle",
            onClick: () =>
              ask({
                title: `Open a ${f.code} cycle`,
                name: "cycle-new",
                fields: [
                  { name: "periodLabel", label: "Period", required: true, placeholder: "2026-27" },
                  { name: "dueOn", label: "Due on", type: "date" },
                ],
                submitLabel: "Open",
                onSubmit: (v) =>
                  submit(
                    () => api.createCycle(f.id, { periodLabel: v.periodLabel!, dueOn: v.dueOn || undefined }),
                    (c) => {
                      setSelected(c.id);
                      setSnapshotId(null);
                      return `${c.number} opened for ${c.periodLabel}`;
                    },
                  ),
              }),
          };
      }
    });

  const collect = (c: Cycle) =>
    ask({
      title: `Collect ${c.number}`,
      name: "cycle-collect",
      fields: c.collectionPlan.manualFields.map((f) => ({ name: f.key, label: f.required ? `${f.label} (required)` : f.label, wide: true })),
      submitLabel: "Collect figures",
      onSubmit: (v) =>
        submit(
          () => api.collect(c.id, Object.fromEntries(Object.entries(v).filter(([, x]) => x.trim() !== ""))),
          (s) => {
            setSnapshotId(s.id);
            return s.issues.length ? `Snapshot #${s.seq}: ${s.issues.length} issue(s) to resolve` : `Snapshot #${s.seq} collected — ready to validate`;
          },
        ),
    });

  const cycleButtons = (d: CycleDetail) =>
    cycleActions(d).map((a) => {
      if (a === "collect")
        return (
          <Button key={a} size="sm" data-action="cycle-collect" onClick={() => collect(d.cycle)} disabled={busy}>
            Collect
          </Button>
        );
      if (a === "package")
        return (
          <Button key={a} size="sm" data-action="package-new" onClick={() => void run(() => api.createPackage(d.cycle.id), (p) => `${p.number} assembled`)} disabled={busy}>
            Assemble package
          </Button>
        );
      return (
        <Button key={a} size="sm" variant="ghost" data-action="cycle-close" onClick={() => void run(() => api.closeCycle(d.cycle.id, d.cycle.version), `${d.cycle.number} closed`)} disabled={busy}>
          Close cycle
        </Button>
      );
    });

  const snapshotRowActions = (s: Snapshot): RowAction[] => {
    const acts: RowAction[] = [{ id: "snapshot-open", label: s.id === shownId ? "Values ✓" : "Values", onClick: () => setSnapshotId(s.id) }];
    for (const a of snapshotActions(s)) {
      if (a === "validate") acts.push({ id: "snapshot-validate", label: "Validate", variant: "primary", onClick: () => void run(() => api.snapshotCommand(s.id, s.version, "validate"), `Snapshot #${s.seq} validated`) });
      else
        acts.push({
          id: "snapshot-reject",
          label: "Reject",
          onClick: () => ask({ title: `Reject snapshot #${s.seq}`, name: "snapshot-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.snapshotCommand(s.id, s.version, "reject", v.reason), `Snapshot #${s.seq} rejected`) }),
        });
    }
    return acts;
  };

  const packageRowActions = (p: SubmissionPackage): RowAction[] =>
    packageActions(p).map((a): RowAction => {
      switch (a) {
        case "submit":
          return p.status === "draft"
            ? { id: "package-submit", label: "Submit for approval", variant: "primary", onClick: () => void run(() => api.packageCommand(p.id, p.version, "submit"), (r) => `${r.number} ${statusLabel(r.status).toLowerCase()}`) }
            : { id: "package-file", label: "Submit to regulator", variant: "primary", onClick: () => void run(() => api.packageCommand(p.id, p.version, "submit"), (r) => `${r.number} submitted`) };
        case "approve":
          return { id: "package-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decidePackage(p.id, p.version, "approve"), `${p.number} approved`) };
        case "reject":
          return {
            id: "package-reject",
            label: "Reject",
            onClick: () => ask({ title: `Reject ${p.number}`, name: "package-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decidePackage(p.id, p.version, "reject", v.reason), `${p.number} back to draft`) }),
          };
        case "acknowledge":
          return {
            id: "package-acknowledge",
            label: "Acknowledged",
            onClick: () =>
              ask({
                title: `Acknowledgement of ${p.number}`,
                name: "package-acknowledge",
                fields: [{ name: "reference", label: "Regulator reference", required: true, wide: true }],
                submitLabel: "Record",
                onSubmit: (v) => submit(() => api.acknowledge(p.id, p.version, v.reference!.trim()), (r) => `${r.number} acknowledged · ${r.acknowledgementRef ?? ""}`),
              }),
          };
        default:
          return {
            id: "package-correct",
            label: "Correct",
            onClick: () => ask({ title: `Correct ${p.number}`, name: "package-correct", fields: [REASON], submitLabel: "Start correction", onSubmit: (v) => submit(() => api.packageCommand(p.id, p.version, "correct", v.reason), `${p.number} under correction — collect a new snapshot`) }),
          };
      }
    });

  const plan = cycle?.collectionPlan;
  const planRows = plan
    ? [
        ...plan.metricKeys.map((k) => ({ key: k, label: metrics.find((m) => m.key === k)?.label ?? k, source: "Figure", required: true })),
        ...plan.manualFields.map((f) => ({ key: f.key, label: f.label, source: "Manual", required: !!f.required })),
      ]
    : [];

  return (
    <>
      <KpiTile span={3} icon={ShieldCheck} tone="blue" label="Active frameworks" value={String(activeFrameworks.length)} sub={activeFrameworks.map((f) => f.code).join(", ") || undefined} data-slice="8C" data-kpi="regulatory-frameworks" />
      <KpiTile span={3} icon={ClipboardList} tone="violet" label="Open cycles" value={String(open.length)} data-kpi="regulatory-open" />
      <KpiTile span={3} icon={Send} tone="green" label="Submitted" value={String(submitted.length)} data-kpi="regulatory-submitted" />
      <KpiTile span={3} icon={CheckCircle2} tone="amber" label="Next due" value={nextDue ? fmtDate(nextDue) : "—"} data-kpi="regulatory-due" />
      <BentoTile
        span={12}
        title="Reporting frameworks"
        data-slice="8C"
        action={
          <Button size="sm" data-action="framework-new" onClick={newFramework}>
            Define framework
          </Button>
        }
      >
        <DataTable<Framework>
          caption="Reporting frameworks"
          rows={frameworks}
          rowKey={(f) => f.id}
          empty={<EmptyState icon={ShieldCheck} title="No reporting frameworks yet" compact>Define what AISHE, NIRF or NAAC ask for: figures from every domain plus manual fields.</EmptyState>}
          columns={[
            { key: "code", header: "Framework", render: (f) => `${f.code} v${f.versionNo} · ${f.name}` },
            { key: "regulator", header: "Regulator", render: (f) => f.regulator, muted: true },
            { key: "plan", header: "Collects", render: (f) => `${f.metricKeys.length} figure(s) · ${f.manualFields.length} manual`, muted: true },
            { key: "approval", header: "Approval", render: (f) => <ApprovalBadge state={f.approvalState} /> },
            { key: "status", header: "Status", render: (f) => <StatusBadge status={f.status} /> },
            { key: "actions", header: "", render: (f) => <Actions actions={frameworkRowActions(f)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Reporting cycles" data-slice="8C">
        <DataTable<Cycle>
          caption="Reporting cycles"
          rows={cycles}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={ClipboardList} title="No cycles yet" compact>Open a cycle on an active framework for the reporting period.</EmptyState>}
          columns={[
            { key: "number", header: "Cycle", render: (c) => `${c.number} · ${c.frameworkCode} v${c.frameworkVersionNo}` },
            { key: "period", header: "Period", render: (c) => c.periodLabel },
            { key: "due", header: "Due", render: (c) => fmtDate(c.dueOn), muted: true },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            {
              key: "actions",
              header: "",
              render: (c) => (
                <Actions
                  actions={[
                    {
                      id: "cycle-open",
                      label: c.id === cycleId ? "Open ✓" : "Open",
                      onClick: () => {
                        setSelected(c.id);
                        setSnapshotId(null);
                      },
                    },
                  ]}
                  disabled={busy}
                />
              ),
            },
          ]}
        />
      </BentoTile>
      {cycle && (
        <>
          <BentoTile span={5} title={`Collection plan · ${cycle.number}`} data-slice="8C" data-cycle={cycle.number} action={detail ? <>{cycleButtons(detail)}</> : undefined}>
            <DataTable
              caption="Collection plan"
              rows={planRows}
              rowKey={(r) => r.key}
              empty={<EmptyState icon={ListChecks} title="Empty plan" compact>The framework defines no figures or fields.</EmptyState>}
              columns={[
                { key: "label", header: "Item", render: (r) => r.label },
                { key: "source", header: "Source", render: (r) => r.source, muted: true },
                { key: "req", header: "Required", render: (r) => (r.required ? "Yes" : "No"), muted: true },
              ]}
            />
          </BentoTile>
          <BentoTile span={7} title={`Snapshots · ${cycle.number}`} data-slice="8C">
            <DataTable<Snapshot>
              caption="Snapshots"
              rows={snapshots}
              rowKey={(s) => s.id}
              empty={<EmptyState icon={FileCheck2} title="Nothing collected yet" compact>Collect the figures of the plan; they are frozen in a snapshot and checked.</EmptyState>}
              columns={[
                { key: "seq", header: "Snapshot", render: (s) => `#${s.seq}` },
                { key: "when", header: "Collected", render: (s) => when(s.createdAt), muted: true },
                { key: "issues", header: "Issues", render: (s) => (s.issues.length ? `${s.issues.length} to resolve` : "None") },
                { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
                { key: "actions", header: "", render: (s) => <Actions actions={snapshotRowActions(s)} disabled={busy} /> },
              ]}
            />
          </BentoTile>
          {shownSnapshot && (
            <BentoTile span={12} title={`Snapshot #${shownSnapshot.seq} values`} data-slice="8C" data-snapshot={String(shownSnapshot.seq)}>
              <DataTable<SnapshotValue>
                caption="Snapshot values"
                rows={values?.values ?? []}
                rowKey={(x) => x.key}
                empty={<EmptyState icon={FileCheck2} title="No values" compact>Loading the frozen values…</EmptyState>}
                columns={[
                  { key: "label", header: "Item", render: (x) => x.label },
                  { key: "value", header: "Value", render: (x) => (x.value !== null && x.value !== undefined ? figure({ value: x.value, unit: x.unit ?? "count" }) : (x.valueText ?? "—")) },
                  { key: "source", header: "Source", render: (x) => statusLabel(x.source), muted: true },
                ]}
              />
              {(values?.issues ?? shownSnapshot.issues).length > 0 && (
                <DataTable
                  caption="Validation issues"
                  rows={values?.issues ?? shownSnapshot.issues}
                  rowKey={(i) => `${i.key}-${i.issue}`}
                  columns={[
                    { key: "key", header: "Issue on", render: (i) => (values?.values ?? []).find((x) => x.key === i.key)?.label ?? plan?.manualFields.find((f) => f.key === i.key)?.label ?? metrics.find((m) => m.key === i.key)?.label ?? i.key },
                    { key: "issue", header: "Problem", render: (i) => i.issue },
                  ]}
                />
              )}
            </BentoTile>
          )}
          <BentoTile span={12} title={`Submission packages · ${cycle.number}`} data-slice="8C">
            <DataTable<SubmissionPackage>
              caption="Submission packages"
              rows={packages}
              rowKey={(p) => p.id}
              empty={<EmptyState icon={Send} title="No packages yet" compact>Assemble a package from a validated snapshot; it is approved, then submitted to the regulator.</EmptyState>}
              columns={[
                { key: "number", header: "Package", render: (p) => p.number },
                { key: "snapshot", header: "Snapshot", render: (p) => `#${seqOf(p.snapshotId) ?? "—"}`, muted: true },
                { key: "submitted", header: "Submitted", render: (p) => when(p.submittedAt), muted: true },
                { key: "ack", header: "Acknowledgement", render: (p) => p.acknowledgementRef ?? "—", muted: true },
                { key: "corrects", header: "Corrects", render: (p) => numberOf(p.correctedFromId) ?? "—", muted: true },
                { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
                { key: "actions", header: "", render: (p) => <Actions actions={packageRowActions(p)} disabled={busy} /> },
              ]}
            />
          </BentoTile>
        </>
      )}
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Reports → Regulatory reporting. */
export function RegulatoryAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <RegulatoryScreen institutionId={institution.id} />;
}
