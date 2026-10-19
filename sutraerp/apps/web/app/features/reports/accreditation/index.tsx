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
import { BentoTile, Button, ClassCard, EmptyState, FilterChips, formatNumber, KpiTile, ListRow, plural, SidePanel, statusKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, NoInstitution, REASON, useAsk, type FieldSpec, type RowAction } from "../../institution/forms";
import { Actions, rowProps, stageOf } from "../../campus/ui";
import { statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { approvalStateLabel } from "../../campus/approval";
import { gkeys, useGovernanceApi } from "../../campus/governance/hooks";
import { ackeys, useAccreditationApi } from "./hooks";
import { needsCycle, RECORD_LABELS, recordItemKinds } from "./rules";
import type { AccreditationCycle, AccreditationFramework, AccreditationItem, AccreditationRecord, Criterion, RecordKind } from "./types";

export { AccreditationApiProvider } from "./hooks";

/** Lifecycles shown as side-panel stages. */
const CYCLE_STAGES = ["planning", "active", "submitted", "closed"] as const;
const RECORD_STAGES = ["draft", "open", "pending_decision", "closed"] as const;

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
  const [openCycle, setOpenCycle] = useState<string | null>(null);
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
  const cycle = cycles.find((c) => c.id === openCycle) ?? null;
  const recordMenu = (r: AccreditationRecord) => recordRowActions(r).filter((a) => a.id !== "record-open");
  const frameworkMenu = (f: AccreditationFramework) => [...frameworkRowActions(f), ...(f.status === "active" ? [{ id: "framework-version", label: "New version", onClick: () => newFramework(f) } as RowAction] : [])];

  return (
    <>
      <KpiTile span={3} icon={ShieldCheck} tone="blue" label="Active frameworks" value={formatNumber(active.length)} sub={active.map((f) => `${f.code} v${f.versionNo}`).join(", ") || "None active yet"} data-slice="9E" data-kpi="accreditation-frameworks" />
      <KpiTile span={3} icon={ClipboardList} tone="violet" label="Live cycles" value={formatNumber(liveCycles.length)} sub="Planning, active or submitted" data-kpi="accreditation-cycles" />
      <KpiTile span={3} icon={Gauge} tone="amber" label="Open records" value={formatNumber(openRecords.length)} sub="Obligations, audits, findings …" data-kpi="accreditation-open" />
      <KpiTile span={3} icon={FileCheck2} tone="green" label="Approved evidence" value={formatNumber(approvedEvidence.length)} sub="Ready to cite in the self-study" data-kpi="accreditation-evidence" />
      <BentoTile
        span={12}
        title="Accreditation frameworks"
        data-slice="9E"
        action={
          <Button size="sm" variant="ghost" data-action="accreditation-framework-new" onClick={() => newFramework()}>
            Define framework
          </Button>
        }
      >
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {frameworks.length ? (
            frameworks.map((f) => (
              <ListRow
                key={f.id}
                icon={ShieldCheck}
                tone={f.status === "active" ? "green" : "grey"}
                title={f.name}
                meta={[`${f.code} v${f.versionNo}`, f.body, plural(f.criteria.length, "criterion", "criteria"), f.approvalState && f.approvalState !== "not_requested" ? approvalStateLabel(f.approvalState) : null].filter(Boolean).join(" · ")}
                badge={{ label: statusLabel(f.status), kind: statusKind(f.status) }}
                {...rowProps(frameworkMenu(f), busy)}
                data-framework-row={f.code}
              />
            ))
          ) : (
            <EmptyState icon={ShieldCheck} title="No accreditation frameworks yet" compact>
              Define the NAAC, NBA or NIRF criteria and their weights.
            </EmptyState>
          )}
        </div>
      </BentoTile>
      {cycles.length ? (
        cycles.map((c) => {
          const planned = c.criteria.length ? Math.round((c.plan.length / c.criteria.length) * 100) : 0;
          return (
            <ClassCard
              key={c.id}
              span={4}
              code={c.number}
              title={c.title}
              meta={`${c.frameworkCode} v${c.frameworkVersionNo} · ${fmtDate(c.periodStart)} – ${fmtDate(c.periodEnd)}`}
              icon={ClipboardList}
              tone="violet"
              badge={{ label: statusLabel(c.status), kind: statusKind(c.status) }}
              facts={[
                { label: "Records", value: formatNumber(c.records.reduce((n, x) => n + x.count, 0)) },
                { label: "Approval", value: approvalStateLabel(c.approvalState) },
              ]}
              progress={{ label: `Assessment plan · ${c.plan.length} of ${plural(c.criteria.length, "criterion", "criteria")}`, value: planned }}
              onClick={() => setOpenCycle(c.id)}
              data-cycle={c.number}
              data-slice="9E"
            />
          );
        })
      ) : (
        <BentoTile span={12} title="Accreditation cycles" data-slice="9E">
          <EmptyState icon={ClipboardList} title="No cycles yet" compact>
            Open a cycle on an active framework, then plan every criterion.
          </EmptyState>
        </BentoTile>
      )}
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
        <FilterChips
          label="Records"
          options={RECORD_FAMILIES.map((f) => ({ id: f.key, label: f.label, count: f.kinds.length ? records.filter((r) => f.kinds.includes(r.kind)).length : records.length }))}
          value={family}
          onChange={setFamily}
        />
        <div style={{ display: "flex", flexDirection: "column", gap: 4, marginTop: 12 }}>
          {shown.length ? (
            shown.map((r) => (
              <ListRow
                key={r.id}
                icon={BookCheck}
                tone={r.severity === "critical" || r.severity === "high" ? "rose" : "blue"}
                title={r.title}
                meta={[r.number, RECORD_LABELS[r.kind] ?? statusLabel(r.kind), r.cycleId ? `${cycleLabel(r.cycleId)}${r.criterionCode ? ` · ${r.criterionCode}` : ""}` : null, r.outcome ? statusLabel(r.outcome) : null, r.dueOn ? `due ${fmtDate(r.dueOn)}` : null].filter(Boolean).join(" · ")}
                badge={{ label: statusLabel(r.handedOffAt ? "handed_off" : r.status), kind: statusKind(r.handedOffAt ? "done" : r.status) }}
                selected={r.id === current?.id}
                onOpen={() => setSelected(r.id)}
                {...rowProps(recordMenu(r), busy)}
                data-record-row={r.number}
              />
            ))
          ) : (
            <EmptyState icon={BookCheck} title="No records" compact>
              Track obligations, evidence, audits, findings, improvements and the self-study report here.
            </EmptyState>
          )}
        </div>
      </BentoTile>
      {cycle && (
        <SidePanel
          open
          onClose={() => setOpenCycle(null)}
          eyebrow={`${cycle.number} · ${cycle.frameworkCode} v${cycle.frameworkVersionNo}`}
          title={cycle.title}
          chip={{ label: statusLabel(cycle.status), kind: statusKind(cycle.status) }}
          stages={CYCLE_STAGES.map((st) => statusLabel(st))}
          stage={stageOf(CYCLE_STAGES, cycle.status)}
          facts={[
            { label: "Period", value: `${fmtDate(cycle.periodStart)} – ${fmtDate(cycle.periodEnd)}` },
            { label: "Approval", value: approvalStateLabel(cycle.approvalState) },
            { label: "Plan", value: `${cycle.plan.length} of ${plural(cycle.criteria.length, "criterion", "criteria")}` },
            ...cycle.records.map((x) => ({ label: RECORD_LABELS[x.kind as RecordKind] ?? statusLabel(x.kind), value: formatNumber(x.count) })),
          ]}
          actions={<Actions actions={cycleRowActions(cycle)} disabled={busy} />}
        >
          <div data-cycle-panel={cycle.number} />
        </SidePanel>
      )}
      {current && (
        <SidePanel
          open
          onClose={() => setSelected(null)}
          eyebrow={`${current.number} · ${RECORD_LABELS[current.kind] ?? current.kind}`}
          title={current.title}
          chip={{ label: statusLabel(current.status), kind: statusKind(current.status) }}
          stages={RECORD_STAGES.map((st) => statusLabel(st))}
          stage={stageOf(RECORD_STAGES, current.status)}
          facts={[
            { label: "Approval", value: approvalStateLabel(current.approvalState) },
            { label: "Outcome", value: current.outcome ? statusLabel(current.outcome) : "—" },
            { label: "Owner", value: current.owner ?? "—" },
            { label: "Due", value: fmtDate(current.dueOn) },
            { label: "Severity", value: statusLabel(current.severity) },
            { label: "Cites", value: refsText(current.refs) },
            ...(current.handoffReference ? [{ label: "Hand-off reference", value: current.handoffReference }] : []),
            ...(current.statusReason ? [{ label: "Reason", value: current.statusReason }] : []),
            ...facts.map(([k, v]) => ({ label: statusLabel(k), value: String(v) })),
          ]}
          actions={<Actions actions={recordMenu(current)} disabled={busy} />}
        >
          <div data-record={current.number}>
            {recordItemKinds(current.kind).length > 0 && (
              <>
                <h3 style={{ fontSize: 15, margin: "18px 0 8px" }}>Items</h3>
                {current.items.length ? (
                  current.items.map((i) => (
                    <ListRow
                      key={i.id}
                      icon={ListChecks}
                      tone="blue"
                      title={i.title}
                      meta={[statusLabel(i.kind), i.owner, i.evidenceRef].filter(Boolean).join(" · ")}
                      badge={{ label: statusLabel(i.status), kind: statusKind(i.status) }}
                      {...rowProps(itemRowActions(current, i), busy)}
                      data-item-row={i.title}
                    />
                  ))
                ) : (
                  <EmptyState icon={ListChecks} title="No items yet" compact>
                    Add requirements, checklist entries or CAPA actions while the record is open.
                  </EmptyState>
                )}
              </>
            )}
          </div>
        </SidePanel>
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
