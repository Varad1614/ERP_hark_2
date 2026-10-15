/**
 * Slice **9E** — Admin → Reports → Accreditation (`AccreditationAdminTab`, L2-H09, contract batch9
 * §12.2). Every screen carries `data-slice="9E"` and uses @sutra/ui only (no custom styling).
 *
 * - Frameworks (H-061): NAAC / NBA / NIRF criteria with weights, versioned; submit → approval →
 *   active (supersedes the previous version).
 * - Cycles (H-062, H-071): a cycle on the active framework version, its assessment plan (every
 *   criterion with an owner and a due date) → approval → active; closed after the self-study
 *   report is handed off.
 * - Records (H-063…071): compliance obligations with requirements, evidence (citing published
 *   governance evidence and validated regulatory snapshots by id), internal audits with checklists,
 *   external assessments, findings with CAPA actions, quality improvements, readiness decisions,
 *   periodic compliance reviews and the self-study report (approval + hand-off). Decisions are
 *   `services.report.approve` with step-up. Figures only through the metrics port (server side).
 */
import { BookCheck, ClipboardList, FileCheck2, Gauge, ListChecks, ShieldCheck } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, FilterChip, StatTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Actions, fmtDate, NoInstitution, REASON, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { approvalStateLabel } from "../../campus/approval";
import { gkeys, useGovernanceApi } from "../../campus/governance/hooks";
import { ackeys, useAccreditationApi } from "./hooks";
import { needsCycle, RECORD_LABELS, recordItemKinds } from "./rules";
import type { AccreditationCycle, AccreditationFramework, AccreditationItem, AccreditationRecord, Criterion, RecordKind } from "./types";

export { AccreditationApiProvider } from "./hooks";

const BODIES = ["NAAC", "NBA", "NIRF", "Other"] as const;
const SEVERITIES = ["low", "medium", "high", "critical"] as const;

/** Record families shown as filter chips. */
export const RECORD_FAMILIES: { key: string; label: string; kinds: RecordKind[] }[] = [
  { key: "all", label: "All", kinds: [] },
  { key: "compliance", label: "Compliance", kinds: ["obligation", "compliance_review"] },
  { key: "evidence", label: "Evidence & readiness", kinds: ["evidence", "readiness"] },
  { key: "audits", label: "Audits & assessments", kinds: ["internal_audit", "external_assessment"] },
  { key: "quality", label: "Findings & improvement", kinds: ["finding", "improvement"] },
  { key: "report", label: "Self-study report", kinds: ["self_study"] },
];

/** `C1|Curricular aspects|15|student.active_students,x; C2|…` → criteria (invalid entries dropped). */
export function parseCriteria(text: string): Criterion[] {
  return text
    .split(";")
    .map((part) => part.split("|").map((x) => x.trim()))
    .filter(([code, title, weight]) => !!code && !!title && !!weight && Number.isInteger(Number(weight)))
    .map(([code, title, weight, metrics]) => ({
      code: code!,
      title: title!,
      weight: Number(weight),
      metricKeys: (metrics ?? "")
        .split(",")
        .map((m) => m.trim())
        .filter(Boolean),
    }));
}

/** Next steps offered for a framework version. */
export function frameworkActions(f: Pick<AccreditationFramework, "status" | "approvalState">): Array<"submit" | "approve" | "reject" | "cycle"> {
  if (f.status === "active") return ["cycle"];
  if (f.status !== "draft") return [];
  if (f.approvalState === "pending") return ["approve", "reject"];
  return ["submit"];
}

/** Next steps offered for a cycle. */
export function cycleActions(c: Pick<AccreditationCycle, "status" | "approvalState" | "plan">): Array<"plan" | "submit" | "approve" | "reject" | "close"> {
  if (c.status === "planning") {
    if (c.approvalState === "pending") return ["approve", "reject"];
    return c.plan.length ? ["plan", "submit"] : ["plan"];
  }
  if (c.status === "submitted") return ["close"];
  return [];
}

/** Next steps offered for a record. */
export function recordActions(r: Pick<AccreditationRecord, "status" | "kind" | "handedOffAt">): Array<"start" | "item" | "submit" | "approve" | "reject" | "cancel" | "handoff"> {
  switch (r.status) {
    case "draft":
      return ["start", "cancel"];
    case "open":
      return recordItemKinds(r.kind).length ? ["item", "submit", "cancel"] : ["submit", "cancel"];
    case "pending_decision":
      return ["approve", "reject"];
    case "closed":
      return r.kind === "self_study" && !r.handedOffAt ? ["handoff"] : [];
    default:
      return [];
  }
}

/** Next steps offered for a record item (requirement, checklist, CAPA action). */
export function recordItemActions(i: Pick<AccreditationItem, "status">, recordOpen: boolean): Array<"start" | "done" | "verify"> {
  if (!recordOpen) return [];
  if (i.status === "open") return ["start", "done"];
  if (i.status === "in_progress") return ["done"];
  if (i.status === "done") return ["verify"];
  return [];
}

const ids = (text: string | undefined) =>
  (text ?? "")
    .split(",")
    .map((x) => x.trim())
    .filter(Boolean);

const refsText = (refs: unknown) => {
  const r = (refs ?? {}) as Record<string, unknown>;
  const parts: string[] = [];
  if (typeof r.sourceUrl === "string") parts.push(r.sourceUrl);
  if (Array.isArray(r.governanceEvidenceIds) && r.governanceEvidenceIds.length) parts.push(`${r.governanceEvidenceIds.length} governance evidence`);
  if (Array.isArray(r.regulatorySnapshotIds) && r.regulatorySnapshotIds.length) parts.push(`${r.regulatorySnapshotIds.length} regulatory snapshot(s)`);
  return parts.join(" · ") || "—";
};

function NoInstitutionTile() {
  return (
    <BentoTile span={12} data-slice="9E">
      <NoInstitution />
    </BentoTile>
  );
}

function AccreditationScreen({ institutionId }: { institutionId: string }) {
  const api = useAccreditationApi();
  const governance = useGovernanceApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const [family, setFamily] = useState("all");
  const [selected, setSelected] = useState<string | null>(null);
  const frameworks = useQuery({ queryKey: ackeys(institutionId, "frameworks"), queryFn: () => api.frameworks(institutionId) }).data ?? [];
  const cycles = useQuery({ queryKey: ackeys(institutionId, "cycles"), queryFn: () => api.cycles(institutionId) }).data ?? [];
  const records = useQuery({ queryKey: ackeys(institutionId, "records"), queryFn: () => api.records(institutionId) }).data ?? [];
  // Published governance evidence (G-015) the evidence records may cite — read through the campus API, never by SQL.
  const govEvidence = (useQuery({ queryKey: gkeys(institutionId, "evidence"), queryFn: () => governance.evidence(institutionId) }).data ?? []).filter((e) => e.accreditationReady);

  const active = frameworks.filter((f) => f.status === "active");
  const liveCycles = cycles.filter((c) => c.status !== "closed");
  const openRecords = records.filter((r) => r.status === "draft" || r.status === "open" || r.status === "pending_decision");
  const approvedEvidence = records.filter((r) => r.kind === "evidence" && r.outcome === "approved_evidence");
  const kinds = RECORD_FAMILIES.find((f) => f.key === family)?.kinds ?? [];
  const shown = kinds.length ? records.filter((r) => kinds.includes(r.kind)) : records;
  const current = records.find((r) => r.id === selected) ?? null;
  const cycleLabel = (id: string | null | undefined) => (id ? (cycles.find((c) => c.id === id)?.number ?? "—") : "—");

  const newFramework = (base?: AccreditationFramework) =>
    ask({
      title: base ? `New version of ${base.code}` : "Define an accreditation framework",
      name: "accreditation-framework",
      fields: [
        { name: "code", label: "Code", required: true, initial: base?.code ?? "NAAC" },
        { name: "body", label: "Body", type: "select", required: true, initial: base?.body ?? "NAAC", options: BODIES.map((b) => ({ value: b, label: b })) },
        { name: "name", label: "Name", required: true, wide: true, initial: base?.name ?? "" },
        {
          name: "criteria",
          label: "Criteria (code|title|weight|figure keys; …) — weights add up to 100",
          required: true,
          wide: true,
          initial: base ? base.criteria.map((c) => `${c.code}|${c.title}|${c.weight}|${(c.metricKeys ?? []).join(",")}`).join("; ") : "",
          placeholder: "C1|Curricular aspects|50|; C2|Teaching-learning|50|student.active_students",
        },
      ],
      submitLabel: "Draft version",
      onSubmit: (v) => submit(() => api.createFramework(institutionId, { code: v.code!.trim().toUpperCase(), body: v.body!, name: v.name!.trim(), criteria: parseCriteria(v.criteria ?? "") }), (f) => `${f.code} v${f.versionNo} drafted`),
    });

  const newCycle = (f: AccreditationFramework) =>
    ask({
      title: `Open a ${f.code} cycle`,
      name: "accreditation-cycle",
      fields: [
        { name: "title", label: "Title", required: true, wide: true, initial: `${f.code} cycle` },
        { name: "periodStart", label: "Period from", type: "date", required: true },
        { name: "periodEnd", label: "Period to", type: "date", required: true },
      ],
      submitLabel: "Open cycle",
      onSubmit: (v) => submit(() => api.createCycle(institutionId, { frameworkId: f.id, title: v.title!.trim(), periodStart: v.periodStart!, periodEnd: v.periodEnd! }), (c) => `${c.number} opened — plan it next`),
    });

  const frameworkRowActions = (f: AccreditationFramework): RowAction[] =>
    frameworkActions(f).map((a): RowAction => {
      switch (a) {
        case "submit":
          return { id: "framework-submit", label: "Submit", variant: "primary", onClick: () => void run(() => api.submitFramework(f.id, f.version), (r) => `${r.code} v${r.versionNo} ${statusLabel(r.status).toLowerCase()}`) };
        case "approve":
          return { id: "framework-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideFramework(f.id, f.version, "approve"), `${f.code} v${f.versionNo} active`) };
        case "reject":
          return {
            id: "framework-reject",
            label: "Reject",
            onClick: () => ask({ title: `Reject ${f.code} v${f.versionNo}`, name: "framework-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideFramework(f.id, f.version, "reject", v.reason), `${f.code} rejected`) }),
          };
        default:
          return { id: "cycle-new", label: "Open cycle", onClick: () => newCycle(f) };
      }
    });

  const plan = (c: AccreditationCycle) =>
    ask({
      title: `Assessment plan of ${c.number}`,
      name: "accreditation-plan",
      fields: c.criteria.flatMap((cr): FieldSpec[] => {
        const entry = c.plan.find((p) => p.criterionCode === cr.code);
        return [
          { name: `owner_${cr.code}`, label: `${cr.code} · ${cr.title} — owner`, required: true, initial: entry?.owner ?? "" },
          { name: `due_${cr.code}`, label: `${cr.code} due on`, type: "date", required: true, initial: entry?.dueOn ?? c.periodEnd },
        ];
      }),
      submitLabel: "Save plan",
      onSubmit: (v) => submit(() => api.planCycle(c.id, c.version, c.criteria.map((cr) => ({ criterionCode: cr.code, owner: (v[`owner_${cr.code}`] ?? "").trim(), dueOn: v[`due_${cr.code}`] ?? "" }))), (r) => `${r.number}: plan saved`),
    });

  const cycleRowActions = (c: AccreditationCycle): RowAction[] =>
    cycleActions(c).map((a): RowAction => {
      switch (a) {
        case "plan":
          return { id: "cycle-plan", label: c.plan.length ? "Edit plan" : "Plan", onClick: () => plan(c) };
        case "submit":
          return { id: "cycle-submit", label: "Submit plan", variant: "primary", onClick: () => void run(() => api.submitCycle(c.id, c.version), (r) => `${r.number} ${statusLabel(r.status).toLowerCase()}`) };
        case "approve":
          return { id: "cycle-approve", label: "Approve plan", variant: "primary", onClick: () => void run(() => api.decideCycle(c.id, c.version, "approve"), `${c.number} active`) };
        case "reject":
          return {
            id: "cycle-reject",
            label: "Reject plan",
            onClick: () => ask({ title: `Reject the plan of ${c.number}`, name: "cycle-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideCycle(c.id, c.version, "reject", v.reason), `${c.number} back to planning`) }),
          };
        default:
          return { id: "cycle-close", label: "Close cycle", onClick: () => void run(() => api.closeCycle(c.id, c.version), `${c.number} closed`) };
      }
    });

  const newRecord = () =>
    ask({
      title: "New accreditation record",
      name: "accreditation-record",
      fields: [
        { name: "kind", label: "Kind", type: "select", required: true, initial: "obligation", options: (Object.keys(RECORD_LABELS) as RecordKind[]).map((k) => ({ value: k, label: RECORD_LABELS[k] })) },
        { name: "title", label: "Title", required: true, wide: true },
        { name: "cycleId", label: "Cycle (evidence, audits, assessments, readiness, self-study)", type: "select", initial: liveCycles[0]?.id ?? "", options: [{ value: "", label: "—" }, ...liveCycles.map((c) => ({ value: c.id, label: `${c.number} · ${c.title}` }))] },
        { name: "criterionCode", label: "Criterion code" },
        { name: "severity", label: "Severity", type: "select", initial: "medium", options: SEVERITIES.map((s) => ({ value: s, label: statusLabel(s) })) },
        { name: "owner", label: "Owner" },
        { name: "dueOn", label: "Due on", type: "date" },
        { name: "metricKey", label: "Figure key (improvements)", placeholder: "campus.tickets_past_sla" },
        { name: "sourceUrl", label: "Source document URL", wide: true },
        { name: "governanceEvidenceId", label: "Cite governance evidence", type: "select", initial: "", options: [{ value: "", label: "—" }, ...govEvidence.map((e) => ({ value: e.id, label: `${e.caseNumber} · ${e.title}` }))] },
        { name: "regulatorySnapshotIds", label: "Regulatory snapshot ids (comma separated)", wide: true },
      ],
      submitLabel: "Create",
      onSubmit: (v) => {
        const kind = v.kind as RecordKind;
        return submit(
          () =>
            api.createRecord(institutionId, {
              kind,
              title: v.title!.trim(),
              cycleId: needsCycle(kind) ? v.cycleId || undefined : undefined,
              criterionCode: v.criterionCode || undefined,
              severity: (v.severity || "medium") as (typeof SEVERITIES)[number],
              owner: v.owner || undefined,
              dueOn: v.dueOn || undefined,
              metricKey: v.metricKey || undefined,
              sourceUrl: v.sourceUrl || undefined,
              governanceEvidenceIds: v.governanceEvidenceId ? [v.governanceEvidenceId] : [],
              regulatorySnapshotIds: ids(v.regulatorySnapshotIds),
            }),
          (r) => {
            setSelected(r.id);
            return `${r.number} created`;
          },
        );
      },
    });

  const recordRowActions = (r: AccreditationRecord): RowAction[] => {
    const acts: RowAction[] = [{ id: "record-open", label: r.id === current?.id ? "Open ✓" : "Open", onClick: () => setSelected(r.id) }];
    for (const a of recordActions(r)) {
      switch (a) {
        case "start":
          acts.push({ id: "record-start", label: "Start", variant: "primary", onClick: () => void run(() => api.recordCommand(r.id, r.version, "start"), `${r.number} open`) });
          break;
        case "item":
          acts.push({
            id: "record-item",
            label: "Add item",
            onClick: () =>
              ask({
                title: `Add an item to ${r.number}`,
                name: "accreditation-item",
                fields: [
                  { name: "kind", label: "Kind", type: "select", required: true, initial: recordItemKinds(r.kind)[0], options: recordItemKinds(r.kind).map((k) => ({ value: k, label: statusLabel(k) })) },
                  { name: "title", label: "Title", required: true, wide: true },
                  { name: "owner", label: "Owner" },
                  { name: "dueOn", label: "Due on", type: "date" },
                ],
                submitLabel: "Add",
                onSubmit: (v) =>
                  submit(
                    () => api.addItem(r.id, { kind: v.kind as AccreditationItem["kind"], title: v.title!.trim(), owner: v.owner || undefined, dueOn: v.dueOn || undefined }),
                    (x) => {
                      setSelected(x.id);
                      return `Item added to ${x.number}`;
                    },
                  ),
              }),
          });
          break;
        case "submit":
          acts.push({
            id: "record-submit",
            label: "Submit outcome",
            variant: "primary",
            onClick: () =>
              ask({
                title: `Submit ${r.number} for decision`,
                name: "accreditation-submit",
                fields: [
                  { name: "outcome", label: "Outcome", type: "select", required: true, initial: r.allowedOutcomes[0], options: r.allowedOutcomes.map((o) => ({ value: o, label: statusLabel(o) })) },
                  { name: "rationale", label: "Rationale", wide: true },
                  ...(r.kind === "external_assessment" ? [{ name: "grade", label: "Awarded grade / score", required: true } as FieldSpec] : []),
                ],
                submitLabel: "Submit",
                onSubmit: (v) => submit(() => api.submitRecord(r.id, r.version, v.outcome!, v.rationale || undefined, v.grade || undefined), (x) => `${x.number} ${x.status === "closed" ? `closed · ${statusLabel(x.outcome ?? "")}` : "awaits decision"}`),
              }),
          });
          break;
        case "approve":
          acts.push({ id: "record-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideRecord(r.id, r.version, "approve"), `${r.number} approved`) });
          break;
        case "reject":
          acts.push({
            id: "record-reject",
            label: "Reject",
            onClick: () => ask({ title: `Reject ${r.number}`, name: "record-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideRecord(r.id, r.version, "reject", v.reason), `${r.number} back to open`) }),
          });
          break;
        case "cancel":
          acts.push({
            id: "record-cancel",
            label: "Cancel",
            onClick: () => ask({ title: `Cancel ${r.number}`, name: "record-cancel", fields: [REASON], submitLabel: "Cancel record", onSubmit: (v) => submit(() => api.recordCommand(r.id, r.version, "cancel", v.reason), `${r.number} cancelled`) }),
          });
          break;
        default:
          acts.push({
            id: "record-handoff",
            label: "Hand off",
            variant: "primary",
            onClick: () =>
              ask({
                title: `Hand off ${r.number}`,
                name: "accreditation-handoff",
                fields: [{ name: "reference", label: "Submission reference (portal acknowledgement)", required: true, wide: true }],
                submitLabel: "Record hand-off",
                onSubmit: (v) => submit(() => api.handoff(r.id, r.version, v.reference!.trim()), (x) => `${x.number} handed off · ${x.handoffReference ?? ""}`),
              }),
          });
      }
    }
    return acts;
  };

  const itemRowActions = (r: AccreditationRecord, i: AccreditationItem): RowAction[] =>
    recordItemActions(i, r.status === "open" || r.status === "draft").map((a): RowAction => {
      if (a === "start") return { id: "item-start", label: "Start", onClick: () => void run(() => api.progressItem(i.id, i.version, { status: "in_progress" }), `“${i.title}” in progress`) };
      if (a === "done")
        return {
          id: "item-done",
          label: "Done",
          variant: "primary",
          onClick: () =>
            ask({
              title: `Complete “${i.title}”`,
              name: "accreditation-done",
              fields: [{ name: "evidenceRef", label: "Evidence reference", wide: true }],
              submitLabel: "Mark done",
              onSubmit: (v) => submit(() => api.progressItem(i.id, i.version, { status: "done", evidenceRef: v.evidenceRef || undefined }), `“${i.title}” done`),
            }),
        };
      return {
        id: "item-verify",
        label: "Verify",
        variant: "primary",
        onClick: () =>
          ask({
            title: `Verify “${i.title}”`,
            name: "accreditation-verify",
            fields: [{ name: "evidenceRef", label: "Evidence reference", required: !i.evidenceRef, wide: true, initial: i.evidenceRef ?? "" }],
            submitLabel: "Verify",
            onSubmit: (v) => submit(() => api.verifyItem(i.id, i.version, v.evidenceRef || undefined), `“${i.title}” verified`),
          }),
      };
    });

  const facts = current ? Object.entries((current.facts ?? {}) as Record<string, unknown>).filter(([, v]) => v === null || ["string", "number", "boolean"].includes(typeof v)) : [];

  return (
    <>
      <StatTile span={3} icon={ShieldCheck} tone="blue" label="Active frameworks" value={String(active.length)} hint={active.map((f) => `${f.code} v${f.versionNo}`).join(", ") || undefined} data-slice="9E" data-kpi="accreditation-frameworks" />
      <StatTile span={3} icon={ClipboardList} tone="violet" label="Live cycles" value={String(liveCycles.length)} data-kpi="accreditation-cycles" />
      <StatTile span={3} icon={Gauge} tone="amber" label="Open records" value={String(openRecords.length)} data-kpi="accreditation-open" />
      <StatTile span={3} icon={FileCheck2} tone="green" label="Approved evidence" value={String(approvedEvidence.length)} data-kpi="accreditation-evidence" />
      <BentoTile
        span={12}
        title="Accreditation frameworks"
        data-slice="9E"
        action={
          <Button size="sm" data-action="accreditation-framework-new" onClick={() => newFramework()}>
            Define framework
          </Button>
        }
      >
        <DataTable<AccreditationFramework>
          caption="Accreditation frameworks"
          rows={frameworks}
          rowKey={(f) => f.id}
          empty={<EmptyState icon={ShieldCheck} title="No accreditation frameworks yet">Define the NAAC, NBA or NIRF criteria and their weights.</EmptyState>}
          columns={[
            { key: "code", header: "Framework", render: (f) => `${f.code} v${f.versionNo} · ${f.name}` },
            { key: "body", header: "Body", render: (f) => f.body, muted: true },
            { key: "criteria", header: "Criteria", render: (f) => `${f.criteria.length} criteria`, muted: true },
            { key: "approval", header: "Approval", render: (f) => approvalStateLabel(f.approvalState), muted: true },
            { key: "status", header: "Status", render: (f) => <StatusBadge status={f.status} /> },
            {
              key: "actions",
              header: "",
              render: (f) => <Actions actions={[...frameworkRowActions(f), ...(f.status === "active" ? [{ id: "framework-version", label: "New version", onClick: () => newFramework(f) } as RowAction] : [])]} disabled={busy} />,
            },
          ]}
        />
      </BentoTile>
      <BentoTile span={12} title="Accreditation cycles" data-slice="9E">
        <DataTable<AccreditationCycle>
          caption="Accreditation cycles"
          rows={cycles}
          rowKey={(c) => c.id}
          empty={<EmptyState icon={ClipboardList} title="No cycles yet">Open a cycle on an active framework, then plan every criterion.</EmptyState>}
          columns={[
            { key: "number", header: "Cycle", render: (c) => `${c.number} · ${c.title}` },
            { key: "framework", header: "Framework", render: (c) => `${c.frameworkCode} v${c.frameworkVersionNo}`, muted: true },
            { key: "period", header: "Period", render: (c) => `${fmtDate(c.periodStart)} – ${fmtDate(c.periodEnd)}`, muted: true },
            { key: "plan", header: "Plan", render: (c) => `${c.plan.length}/${c.criteria.length} criteria`, muted: true },
            { key: "records", header: "Records", render: (c) => String(c.records.reduce((s, x) => s + x.count, 0)), muted: true },
            { key: "approval", header: "Approval", render: (c) => approvalStateLabel(c.approvalState), muted: true },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            { key: "actions", header: "", render: (c) => <Actions actions={cycleRowActions(c)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Accreditation records"
        data-slice="9E"
        action={
          <Button size="sm" data-action="accreditation-record-new" onClick={newRecord}>
            New record
          </Button>
        }
      >
        <div className="sb-row-actions">
          {RECORD_FAMILIES.map((f) => (
            <FilterChip key={f.key} active={family === f.key} label={f.label} count={f.kinds.length ? records.filter((r) => f.kinds.includes(r.kind)).length : records.length} onClick={() => setFamily(f.key)} />
          ))}
        </div>
        <DataTable<AccreditationRecord>
          caption="Accreditation records"
          rows={shown}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={BookCheck} title="No records">Track obligations, evidence, audits, findings, improvements and the self-study report here.</EmptyState>}
          columns={[
            { key: "number", header: "Record", render: (r) => `${r.number} · ${r.title}` },
            { key: "kind", header: "Kind", render: (r) => RECORD_LABELS[r.kind] ?? statusLabel(r.kind), muted: true },
            { key: "cycle", header: "Cycle", render: (r) => `${cycleLabel(r.cycleId)}${r.criterionCode ? ` · ${r.criterionCode}` : ""}`, muted: true },
            { key: "outcome", header: "Outcome", render: (r) => (r.outcome ? statusLabel(r.outcome) : "—"), muted: true },
            { key: "approval", header: "Approval", render: (r) => approvalStateLabel(r.approvalState), muted: true },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.handedOffAt ? "handed_off" : r.status} /> },
            { key: "actions", header: "", render: (r) => <Actions actions={recordRowActions(r)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {current && (
        <BentoTile span={12} title={`${current.number} · ${RECORD_LABELS[current.kind] ?? current.kind}`} data-slice="9E" data-record={current.number}>
          <DataTable
            caption="Record summary"
            rows={[
              { k: "Status", v: `${statusLabel(current.status)} · ${approvalStateLabel(current.approvalState)}` },
              { k: "Outcome", v: current.outcome ? statusLabel(current.outcome) : "—" },
              { k: "Owner · due", v: `${current.owner ?? "—"} · ${fmtDate(current.dueOn)}` },
              { k: "Severity", v: statusLabel(current.severity) },
              { k: "Cites", v: refsText(current.refs) },
              ...(current.handoffReference ? [{ k: "Hand-off reference", v: current.handoffReference }] : []),
              ...(current.statusReason ? [{ k: "Reason", v: current.statusReason }] : []),
              ...facts.map(([k, v]) => ({ k: statusLabel(k), v: String(v) })),
            ]}
            rowKey={(x) => x.k}
            columns={[
              { key: "k", header: "Field", render: (x) => x.k, muted: true },
              { key: "v", header: "Value", render: (x) => x.v },
            ]}
          />
          {recordItemKinds(current.kind).length > 0 && (
            <DataTable<AccreditationItem>
              caption="Items"
              rows={current.items}
              rowKey={(i) => i.id}
              empty={<EmptyState icon={ListChecks} title="No items yet">Add requirements, checklist entries or CAPA actions while the record is open.</EmptyState>}
              columns={[
                { key: "title", header: "Item", render: (i) => i.title },
                { key: "kind", header: "Kind", render: (i) => statusLabel(i.kind), muted: true },
                { key: "owner", header: "Owner", render: (i) => i.owner ?? "—", muted: true },
                { key: "evidence", header: "Evidence", render: (i) => i.evidenceRef ?? "—", muted: true },
                { key: "status", header: "Status", render: (i) => <StatusBadge status={i.status} /> },
                { key: "actions", header: "", render: (i) => <Actions actions={itemRowActions(current, i)} disabled={busy} /> },
              ]}
            />
          )}
        </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}

/** Admin → Reports → Accreditation. */
export function AccreditationAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) return <NoInstitutionTile />;
  return <AccreditationScreen institutionId={institution.id} />;
}
