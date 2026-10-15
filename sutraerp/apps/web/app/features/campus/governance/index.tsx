/**
 * Slice **9E** — Admin → Campus → Governance (`GovernanceAdminTab`, L2-G00, contract batch9 §12.1).
 * Every screen carries `data-slice="9E"` and uses @sutra/ui only (no custom styling).
 *
 * - Governance cases (G-001, 003…013): readiness / health / maturity assessments with rated
 *   criteria, operational and executive reviews with recommendations, the risk register, action
 *   plans with verified closure, SLA deviations over the 8A service tickets, KPI reviews over the
 *   campus figures, findings → corrective actions, escalations and exceptions. Lifecycle
 *   draft → open → pending decision → closed; decisions are `campus.approve` with step-up.
 * - Governance configuration (G-014): versioned settings (readiness threshold, risk appetite,
 *   review cadence, KPI thresholds) → approval → effective, with the effective policy.
 * - Governance evidence (G-002, G-015): evidence of a closed case, published for accreditation
 *   (`sutraerp.campus.governance_evidence.published`).
 */
import { AlertTriangle, ClipboardCheck, FileCheck2, Flag, Gauge, ListChecks, Settings2, ShieldCheck, TrendingUp, type LucideIcon } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, FilterChips, formatNumber, KeyValue, KpiTile, ListRow, SidePanel, statusKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, NoInstitution, REASON, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { Actions, CodeName, rowProps, stageOf } from "../ui";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { approvalStateLabel } from "../approval";
import { gkeys, useGovernanceApi } from "./hooks";
import { itemKindsFor } from "./rules";
import type { GovernanceCase, GovernanceConfigKey, GovernanceConfigVersion, GovernanceEvidence, GovernanceItem, GovernanceKind } from "./types";

export { GovernanceApiProvider } from "./hooks";

/** Case families shown as filter chips. */
export const FAMILIES: { key: string; label: string; kinds: GovernanceKind[] }[] = [
  { key: "all", label: "All", kinds: [] },
  { key: "assessments", label: "Assessments", kinds: ["readiness_assessment", "health_assessment", "maturity_assessment"] },
  { key: "reviews", label: "Reviews", kinds: ["operational_review", "executive_review", "kpi_review"] },
  { key: "risks", label: "Risks", kinds: ["risk"] },
  { key: "actions", label: "Action plans & SLA", kinds: ["action_plan", "sla_deviation"] },
  { key: "issues", label: "Findings, escalations, exceptions", kinds: ["finding", "escalation", "exception"] },
];

export const KIND_LABELS: Record<GovernanceKind, string> = {
  readiness_assessment: "Readiness assessment",
  health_assessment: "Operational health assessment",
  maturity_assessment: "Maturity assessment",
  operational_review: "Operational review",
  executive_review: "Executive review",
  risk: "Operational risk",
  action_plan: "Action plan",
  sla_deviation: "SLA deviation",
  kpi_review: "KPI review",
  finding: "Finding",
  escalation: "Escalation",
  exception: "Exception",
};

/** Icon of a case row by kind. */
const KIND_ICON: Partial<Record<GovernanceKind, LucideIcon>> = {
  readiness_assessment: ListChecks,
  health_assessment: ListChecks,
  maturity_assessment: ListChecks,
  operational_review: TrendingUp,
  executive_review: TrendingUp,
  kpi_review: TrendingUp,
  risk: AlertTriangle,
  action_plan: ClipboardCheck,
  sla_deviation: Gauge,
  finding: Flag,
  escalation: Flag,
  exception: Flag,
};

/** Case lifecycle shown as the side panel's stages. */
const CASE_STAGES = ["draft", "open", "pending_decision", "closed"] as const;

const DOMAINS = ["facility", "maintenance", "hostel", "transport", "library", "laboratory", "it", "other"] as const;
const SEVERITIES = ["low", "medium", "high", "critical"] as const;
const CONFIG_KEYS: { key: GovernanceConfigKey; label: string; fields: FieldSpec[] }[] = [
  { key: "readiness_threshold", label: "Readiness threshold", fields: [{ name: "minCriterionScore", label: "Lowest criterion rating for ready (1–5)", type: "number", required: true, initial: "3" }] },
  { key: "risk_appetite", label: "Risk appetite", fields: [{ name: "maxResidualScore", label: "Highest residual score a mitigated risk may close with (1–25)", type: "number", required: true, initial: "6" }] },
  {
    key: "review_cadence",
    label: "Review cadence",
    fields: [
      { name: "operationalDays", label: "Operational review every (days)", type: "number", required: true, initial: "90" },
      { name: "executiveDays", label: "Executive review every (days)", type: "number", required: true, initial: "180" },
    ],
  },
  { key: "kpi_thresholds", label: "KPI thresholds", fields: [{ name: "kpis", label: "Figure maxima (key=value; …)", required: true, wide: true, placeholder: "campus.tickets_past_sla=2; campus.open_work_orders=10" }] },
];

/** `campus.tickets_past_sla=2; x=1.5` → `{ "campus.tickets_past_sla": 2, x: 1.5 }` (invalid pairs dropped). */
export function parseKpiThresholds(text: string): Record<string, number> {
  const out: Record<string, number> = {};
  for (const part of text.split(";")) {
    const [k, v] = part.split("=").map((x) => x.trim());
    if (k && v !== undefined && v !== "" && Number.isFinite(Number(v))) out[k] = Number(v);
  }
  return out;
}

/** Next steps offered for a governance case. */
export function caseActions(c: Pick<GovernanceCase, "status" | "kind" | "approvalState">): Array<"start" | "item" | "refresh" | "submit" | "approve" | "reject" | "escalate" | "cancel" | "evidence"> {
  switch (c.status) {
    case "draft":
      return ["start", "cancel"];
    case "open": {
      const out: Array<"item" | "refresh" | "submit" | "escalate" | "cancel"> = [];
      if (itemKindsFor(c.kind).length) out.push("item");
      if (c.kind === "sla_deviation" || c.kind === "kpi_review") out.push("refresh");
      out.push("submit");
      if (c.kind !== "escalation") out.push("escalate");
      out.push("cancel");
      return out;
    }
    case "pending_decision":
      return c.kind === "escalation" ? ["approve", "reject"] : ["approve", "reject", "escalate"];
    case "closed":
      return ["evidence"];
    default:
      return [];
  }
}

/** Next steps offered for a case item (criterion rating, progress, verified closure). */
export function itemActions(i: Pick<GovernanceItem, "status" | "kind">, caseOpen: boolean): Array<"rate" | "start" | "done" | "verify" | "reopen"> {
  if (!caseOpen) return [];
  const out: Array<"rate" | "start" | "done" | "verify" | "reopen"> = [];
  if (i.kind === "criterion" && i.status !== "verified" && i.status !== "cancelled") out.push("rate");
  if (i.status === "open") out.push("start", "done");
  if (i.status === "in_progress") out.push("done");
  if (i.status === "done") out.push("verify", "reopen");
  return out;
}

/** Next steps offered for a configuration version. */
export function configActions(c: Pick<GovernanceConfigVersion, "status" | "approvalState">): Array<"submit" | "approve" | "reject"> {
  if (c.status !== "draft") return [];
  if (c.approvalState === "pending") return ["approve", "reject"];
  return ["submit"];
}

/** Next steps offered for governance evidence. */
export function evidenceActions(e: Pick<GovernanceEvidence, "status">): Array<"publish"> {
  return e.status === "draft" ? ["publish"] : [];
}

const progressOf = (c: GovernanceCase) => {
  if (!c.items.length) return "—";
  const finished = c.items.filter((i) => i.status === "verified" || i.status === "done" || i.status === "cancelled").length;
  return `${finished}/${c.items.length}`;
};

const settingsText = (s: unknown) =>
  Object.entries((s ?? {}) as Record<string, unknown>)
    .map(([k, v]) => `${k} ${String(v)}`)
    .join(" · ") || "—";

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="9E">
      <NoInstitution />
    </BentoTile>
  );
}

function GovernanceScreen({ institutionId }: { institutionId: string }) {
  const api = useGovernanceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [family, setFamily] = useState("all");
  const [selected, setSelected] = useState<string | null>(null);
  const cases = useQuery({ queryKey: gkeys(institutionId, "cases"), queryFn: () => api.cases(institutionId) }).data ?? [];
  const configs = useQuery({ queryKey: gkeys(institutionId, "configs"), queryFn: () => api.configs(institutionId) }).data ?? [];
  const policy = useQuery({ queryKey: gkeys(institutionId, "policy"), queryFn: () => api.policy(institutionId) }).data;
  const evidence = useQuery({ queryKey: gkeys(institutionId, "evidence"), queryFn: () => api.evidence(institutionId) }).data ?? [];

  const kinds = FAMILIES.find((f) => f.key === family)?.kinds ?? [];
  const shown = kinds.length ? cases.filter((c) => kinds.includes(c.kind)) : cases;
  const current = cases.find((c) => c.id === selected) ?? null;
  const openCases = cases.filter((c) => c.status === "draft" || c.status === "open");
  const pending = cases.filter((c) => c.status === "pending_decision");
  const openRisks = cases.filter((c) => c.kind === "risk" && c.status !== "closed" && c.status !== "cancelled");
  const ready = evidence.filter((e) => e.accreditationReady);

  const newCase = () =>
    ask({
      title: "Open a governance case",
      name: "governance-case-new",
      fields: [
        { name: "kind", label: "Kind", type: "select", required: true, initial: "readiness_assessment", options: (Object.keys(KIND_LABELS) as GovernanceKind[]).filter((k) => k !== "escalation").map((k) => ({ value: k, label: KIND_LABELS[k] })) },
        { name: "title", label: "Title", required: true, wide: true },
        { name: "domains", label: "Domains (comma separated)", wide: true, placeholder: DOMAINS.join(", ") },
        { name: "severity", label: "Severity", type: "select", initial: "medium", options: SEVERITIES.map((s) => ({ value: s, label: statusLabel(s) })) },
        { name: "owner", label: "Owner" },
        { name: "dueOn", label: "Due on", type: "date" },
        { name: "likelihood", label: "Risk likelihood (1–5)", type: "number" },
        { name: "impact", label: "Risk impact (1–5)", type: "number" },
        { name: "exceptionType", label: "Exception type", type: "select", initial: "", options: [{ value: "", label: "—" }, { value: "evidence", label: "Evidence" }, { value: "dependency", label: "Dependency" }] },
        { name: "expiresOn", label: "Exception expires on", type: "date" },
      ],
      submitLabel: "Open case",
      onSubmit: (v) =>
        submit(
          () =>
            api.createCase(institutionId, {
              kind: v.kind as GovernanceKind,
              title: v.title!.trim(),
              domains: (v.domains ?? "")
                .split(",")
                .map((d) => d.trim())
                .filter(Boolean),
              severity: (v.severity || "medium") as (typeof SEVERITIES)[number],
              owner: v.owner || undefined,
              dueOn: v.dueOn || undefined,
              likelihood: v.likelihood ? Number(v.likelihood) : undefined,
              impact: v.impact ? Number(v.impact) : undefined,
              exceptionType: v.exceptionType ? (v.exceptionType as "evidence" | "dependency") : undefined,
              expiresOn: v.expiresOn || undefined,
            }),
          (c) => {
            setSelected(c.id);
            return `${c.number} opened`;
          },
        ),
    });

  const caseRowActions = (c: GovernanceCase): RowAction[] => {
    const acts: RowAction[] = [{ id: "case-open", label: c.id === current?.id ? "Open ✓" : "Open", onClick: () => setSelected(c.id) }];
    for (const a of caseActions(c)) {
      switch (a) {
        case "start":
          acts.push({ id: "case-start", label: "Start", variant: "primary", onClick: () => void run(() => api.caseCommand(c.id, c.version, "start"), `${c.number} open`) });
          break;
        case "item":
          acts.push({
            id: "case-item",
            label: "Add item",
            onClick: () =>
              ask({
                title: `Add an item to ${c.number}`,
                name: "governance-item",
                fields: [
                  { name: "kind", label: "Kind", type: "select", required: true, initial: itemKindsFor(c.kind)[0], options: itemKindsFor(c.kind).map((k) => ({ value: k, label: statusLabel(k) })) },
                  { name: "title", label: "Title", required: true, wide: true },
                  { name: "domain", label: "Domain", type: "select", initial: c.domains[0] ?? "", options: [{ value: "", label: "—" }, ...(c.domains.length ? c.domains : DOMAINS).map((d) => ({ value: d, label: statusLabel(d) }))] },
                  { name: "owner", label: "Owner" },
                  { name: "dueOn", label: "Due on", type: "date" },
                ],
                submitLabel: "Add",
                onSubmit: (v) =>
                  submit(
                    () => api.addItem(c.id, { kind: v.kind as GovernanceItem["kind"], title: v.title!.trim(), domain: v.domain || undefined, owner: v.owner || undefined, dueOn: v.dueOn || undefined }),
                    (r) => {
                      setSelected(r.id);
                      return `Item added to ${r.number}`;
                    },
                  ),
              }),
          });
          break;
        case "refresh":
          acts.push({ id: "case-refresh", label: "Refresh figures", onClick: () => void run(() => api.caseCommand(c.id, c.version, "refresh"), `${c.number} figures refreshed`) });
          break;
        case "submit":
          acts.push({
            id: "case-submit",
            label: "Submit outcome",
            variant: "primary",
            onClick: () =>
              ask({
                title: `Submit ${c.number} for decision`,
                name: "governance-submit",
                fields: [
                  { name: "outcome", label: "Outcome", type: "select", required: true, initial: c.allowedOutcomes[0], options: c.allowedOutcomes.map((o) => ({ value: o, label: statusLabel(o) })) },
                  { name: "rationale", label: "Rationale", wide: true },
                  ...(c.kind === "risk" ? [{ name: "residualScore", label: "Residual score (1–25)", type: "number" } as FieldSpec] : []),
                ],
                submitLabel: "Submit",
                onSubmit: (v) =>
                  submit(
                    () => api.submitCase(c.id, c.version, v.outcome!, v.rationale || undefined, v.residualScore ? Number(v.residualScore) : undefined),
                    (r) => `${r.number} ${r.status === "closed" ? `closed · ${statusLabel(r.outcome ?? "")}` : "awaits decision"}`,
                  ),
              }),
          });
          break;
        case "approve":
          acts.push({ id: "case-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideCase(c.id, c.version, "approve"), `${c.number} closed`) });
          break;
        case "reject":
          acts.push({
            id: "case-reject",
            label: "Reject",
            onClick: () => ask({ title: `Reject ${c.number}`, name: "governance-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideCase(c.id, c.version, "reject", v.reason), `${c.number} back to open`) }),
          });
          break;
        case "escalate":
          acts.push({
            id: "case-escalate",
            label: "Escalate",
            onClick: () =>
              ask({
                title: `Escalate ${c.number} to executive resolution`,
                name: "governance-escalate",
                fields: [{ name: "title", label: "Title", required: true, wide: true, initial: `Escalation: ${c.title}` }, REASON],
                submitLabel: "Escalate",
                onSubmit: (v) => submit(() => api.escalate(c.id, v.title!.trim(), v.reason!), (e) => `${e.number} escalated`),
              }),
          });
          break;
        case "cancel":
          acts.push({
            id: "case-cancel",
            label: "Cancel",
            onClick: () => ask({ title: `Cancel ${c.number}`, name: "governance-cancel", fields: [REASON], submitLabel: "Cancel case", onSubmit: (v) => submit(() => api.caseCommand(c.id, c.version, "cancel", v.reason), `${c.number} cancelled`) }),
          });
          break;
        default:
          if (!evidence.some((e) => e.caseId === c.id))
            acts.push({
              id: "case-evidence",
              label: "Record evidence",
              onClick: () =>
                ask({
                  title: `Governance evidence of ${c.number}`,
                  name: "governance-evidence",
                  fields: [
                    { name: "title", label: "Title", wide: true, initial: `${c.number} · ${c.title}` },
                    { name: "summary", label: "Summary", wide: true },
                  ],
                  submitLabel: "Record",
                  onSubmit: (v) => submit(() => api.createEvidence(institutionId, c.id, v.title || undefined, v.summary || undefined), (e) => `Evidence "${e.title}" drafted`),
                }),
            });
      }
    }
    return acts;
  };

  const itemRowActions = (c: GovernanceCase, i: GovernanceItem): RowAction[] =>
    itemActions(i, c.status === "open" || c.status === "draft").map((a): RowAction => {
      switch (a) {
        case "rate":
          return {
            id: "item-rate",
            label: "Rate",
            onClick: () =>
              ask({
                title: `Rate “${i.title}”`,
                name: "governance-rate",
                fields: [{ name: "score", label: "Rating (1–5)", type: "number", required: true, initial: i.score ? String(i.score) : "3" }],
                submitLabel: "Save rating",
                onSubmit: (v) => submit(() => api.progressItem(i.id, i.version, { score: Number(v.score) }), `“${i.title}” rated ${v.score}`),
              }),
          };
        case "start":
          return { id: "item-start", label: "Start", onClick: () => void run(() => api.progressItem(i.id, i.version, { status: "in_progress" }), `“${i.title}” in progress`) };
        case "done":
          return {
            id: "item-done",
            label: "Done",
            variant: "primary",
            onClick: () =>
              ask({
                title: `Complete “${i.title}”`,
                name: "governance-done",
                fields: [{ name: "evidenceRef", label: "Evidence reference", wide: true }],
                submitLabel: "Mark done",
                onSubmit: (v) => submit(() => api.progressItem(i.id, i.version, { status: "done", evidenceRef: v.evidenceRef || undefined }), `“${i.title}” done`),
              }),
          };
        case "verify":
          return {
            id: "item-verify",
            label: "Verify",
            variant: "primary",
            onClick: () =>
              ask({
                title: `Verify “${i.title}”`,
                name: "governance-verify",
                fields: [{ name: "evidenceRef", label: "Evidence reference", required: !i.evidenceRef, wide: true, initial: i.evidenceRef ?? "" }],
                submitLabel: "Verify",
                onSubmit: (v) => submit(() => api.verifyItem(i.id, i.version, v.evidenceRef || undefined), `“${i.title}” verified`),
              }),
          };
        default:
          return { id: "item-reopen", label: "Reopen", onClick: () => void run(() => api.progressItem(i.id, i.version, { status: "in_progress" }), `“${i.title}” reopened`) };
      }
    });

  const newConfig = () =>
    ask({
      title: "Draft a governance configuration version",
      name: "governance-config-key",
      fields: [{ name: "key", label: "Setting", type: "select", required: true, initial: CONFIG_KEYS[0]!.key, options: CONFIG_KEYS.map((k) => ({ value: k.key, label: k.label })) }],
      submitLabel: "Next",
      onSubmit: async (v) => {
        const spec = CONFIG_KEYS.find((k) => k.key === v.key) ?? CONFIG_KEYS[0]!;
        // Opens the second step once this dialog has closed.
        setTimeout(
          () =>
            ask({
              title: `${spec.label} — new version`,
              name: "governance-config",
              fields: spec.fields,
              submitLabel: "Draft version",
              onSubmit: (s) => {
                const settings: Record<string, unknown> = spec.key === "kpi_thresholds" ? parseKpiThresholds(s.kpis ?? "") : Object.fromEntries(spec.fields.map((f) => [f.name, Number(s[f.name])]));
                return submit(() => api.createConfig(institutionId, spec.key, settings), (c) => `${spec.label} v${c.versionNo} drafted`);
              },
            }),
          0,
        );
      },
    });

  const configRowActions = (c: GovernanceConfigVersion): RowAction[] =>
    configActions(c).map((a): RowAction => {
      if (a === "submit") return { id: "config-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.submitConfig(c.id, c.version), (r) => `${statusLabel(r.key)} v${r.versionNo} ${statusLabel(r.status).toLowerCase()}`) };
      if (a === "approve") return { id: "config-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideConfig(c.id, c.version, "approve"), `${statusLabel(c.key)} v${c.versionNo} effective`) };
      return {
        id: "config-reject",
        label: "Reject",
        onClick: () => ask({ title: `Reject ${statusLabel(c.key)} v${c.versionNo}`, name: "config-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideConfig(c.id, c.version, "reject", v.reason), "Version rejected") }),
      };
    });

  const evidenceRowActions = (e: GovernanceEvidence): RowAction[] =>
    evidenceActions(e).map((): RowAction => ({ id: "evidence-publish", label: "Publish for accreditation", variant: "primary", onClick: () => void run(() => api.publishEvidence(e.id, e.version), `“${e.title}” published for accreditation`) }));

  const facts = current ? Object.entries((current.facts ?? {}) as Record<string, unknown>).filter(([, v]) => v === null || ["string", "number", "boolean"].includes(typeof v)) : [];
  const caseMenu = (c: GovernanceCase) => caseRowActions(c).filter((a) => a.id !== "case-open");

  return (
    <>
      <KpiTile span={3} icon={ClipboardCheck} tone="blue" label="Open cases" value={formatNumber(openCases.length)} sub="Drafts and cases in progress" data-slice="9E" data-kpi="governance-open" />
      <KpiTile span={3} icon={Gauge} tone="amber" label="Awaiting decision" value={formatNumber(pending.length)} sub="Submitted outcomes to approve" data-kpi="governance-pending" />
      <KpiTile span={3} icon={AlertTriangle} tone="rose" label="Open risks" value={formatNumber(openRisks.length)} sub="On the campus risk register" data-kpi="governance-risks" />
      <KpiTile span={3} icon={FileCheck2} tone="green" label="Evidence for accreditation" value={formatNumber(ready.length)} sub="Published from closed cases" data-kpi="governance-evidence" />
      <BentoTile
        span={12}
        title="Governance cases"
        data-slice="9E"
        action={
          <Button size="sm" data-action="governance-case-new" onClick={newCase}>
            Open case
          </Button>
        }
      >
        <FilterChips
          label="Workflow"
          options={FAMILIES.map((f) => ({ id: f.key, label: f.label, count: f.kinds.length ? cases.filter((c) => f.kinds.includes(c.kind)).length : cases.length }))}
          value={family}
          onChange={setFamily}
        />
        <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 12 }}>
          {shown.length ? (
            shown.map((c) => (
              <ListRow
                key={c.id}
                icon={KIND_ICON[c.kind] ?? ShieldCheck}
                tone={c.severity === "critical" || c.severity === "high" ? "rose" : "violet"}
                title={c.title}
                meta={[c.number, KIND_LABELS[c.kind] ?? statusLabel(c.kind), c.severity ? `${statusLabel(c.severity)} severity` : null, c.items.length ? `${progressOf(c)} items done` : null, c.dueOn ? `due ${fmtDate(c.dueOn)}` : null].filter(Boolean).join(" · ")}
                badge={{ label: statusLabel(c.status), kind: statusKind(c.status) }}
                selected={c.id === current?.id}
                onOpen={() => setSelected(c.id)}
                {...rowProps(caseMenu(c), busy)}
                data-case-row={c.number}
              />
            ))
          ) : (
            <EmptyState icon={ShieldCheck} title="No governance cases" compact>
              Open an assessment, review, risk, action plan or finding to govern campus operations.
            </EmptyState>
          )}
        </div>
      </BentoTile>
      {current && (
        <SidePanel
          open
          onClose={() => setSelected(null)}
          eyebrow={`${current.number} · ${KIND_LABELS[current.kind] ?? current.kind}`}
          title={current.title}
          chip={{ label: statusLabel(current.status), kind: statusKind(current.status) }}
          stages={CASE_STAGES.map((st) => statusLabel(st))}
          stage={stageOf(CASE_STAGES, current.status)}
          facts={[
            { label: "Approval", value: approvalStateLabel(current.approvalState) },
            { label: "Outcome", value: current.outcome ? statusLabel(current.outcome) : "—" },
            { label: "Owner", value: current.owner ?? "—" },
            { label: "Due", value: fmtDate(current.dueOn) },
            ...(current.domains.length ? [{ label: "Domains", value: current.domains.map(statusLabel).join(", ") }] : []),
            ...(current.averageScore !== null && current.averageScore !== undefined ? [{ label: "Average rating", value: formatNumber(current.averageScore) }] : []),
            ...(current.inherentScore ? [{ label: "Inherent / residual", value: `${current.inherentScore} / ${current.residualScore ?? "—"}` }] : []),
            ...(current.statusReason ? [{ label: "Reason", value: current.statusReason }] : []),
            ...facts.map(([k, v]) => ({ label: statusLabel(k), value: String(v) })),
          ]}
          actions={<Actions actions={caseMenu(current)} disabled={busy} />}
        >
          <div data-case={current.number}>
            <h3 style={{ fontSize: 15, margin: "18px 0 8px" }}>Items</h3>
            {current.items.length ? (
              current.items.map((i) => (
                <ListRow
                  key={i.id}
                  icon={ClipboardCheck}
                  tone="blue"
                  title={i.title}
                  meta={[statusLabel(i.kind), i.domain ? statusLabel(i.domain) : null, i.score != null ? `rated ${i.score}` : null, i.evidenceRef].filter(Boolean).join(" · ")}
                  badge={{ label: statusLabel(i.status), kind: statusKind(i.status) }}
                  {...rowProps(itemRowActions(current, i), busy)}
                  data-item-row={i.title}
                />
              ))
            ) : (
              <EmptyState icon={ClipboardCheck} title="No items yet" compact>
                Add criteria, recommendations or actions while the case is open.
              </EmptyState>
            )}
          </div>
        </SidePanel>
      )}
      <BentoTile
        span={8}
        title="Governance configuration"
        data-slice="9E"
        action={
          <Button size="sm" variant="ghost" data-action="governance-config-new" onClick={newConfig}>
            New version
          </Button>
        }
      >
        <DataTable<GovernanceConfigVersion>
          caption="Configuration versions"
          rows={configs}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={Settings2} title="Defaults in force" compact>No configuration version yet: the default readiness threshold and risk appetite apply.</EmptyState>}
          columns={[
            { key: "key", header: "Setting", render: (c) => `${statusLabel(c.key)} · v${c.versionNo}` },
            { key: "settings", header: "Values", render: (c) => settingsText(c.settings), muted: true },
            { key: "approval", header: "Approval", render: (c) => approvalStateLabel(c.approvalState), muted: true },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            { key: "actions", header: "", render: (c) => <Actions actions={configRowActions(c)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile span={4} title="Effective policy" data-slice="9E">
        {policy ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
            <KeyValue label="Lowest criterion rating for ready" value={formatNumber(policy.policy.minCriterionScore)} />
            <KeyValue label="Highest residual risk score to close" value={formatNumber(policy.policy.maxResidualScore)} />
            <KeyValue label="Effective versions" value={formatNumber(policy.effective.length)} />
          </div>
        ) : (
          <EmptyState icon={Settings2} title="Reading the effective policy…" compact />
        )}
      </BentoTile>
      <BentoTile span={12} title="Governance evidence" data-slice="9E">
        <DataTable<GovernanceEvidence>
          caption="Governance evidence"
          rows={evidence}
          rowKey={(e) => e.id}
          empty={<EmptyState icon={FileCheck2} title="No evidence yet" compact>Record evidence of a closed case, then publish it so accreditation can cite it.</EmptyState>}
          columns={[
            { key: "title", header: "Evidence", render: (e) => <CodeName name={e.title} code={e.caseNumber} sub={KIND_LABELS[e.caseKind] ?? statusLabel(e.caseKind)} /> },
            { key: "outcome", header: "Outcome", render: (e) => (e.caseOutcome ? statusLabel(e.caseOutcome) : "—"), muted: true },
            { key: "published", header: "Published", render: (e) => (e.publishedAt ? fmtDate(e.publishedAt.slice(0, 10)) : "—"), muted: true },
            { key: "status", header: "Status", render: (e) => <StatusBadge status={e.status} /> },
            { key: "actions", header: "", render: (e) => <Actions actions={evidenceRowActions(e)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Campus → Governance. */
export function GovernanceAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <GovernanceScreen institutionId={institution.id} />;
}
