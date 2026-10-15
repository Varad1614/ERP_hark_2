/**
 * Slice **7C** web components (payroll). Export names are imported by the foundation frames
 * (`routes/admin/people.tsx`, `routes/faculty/leave.tsx`); real screens carry `data-slice="7C"`.
 * Money and days stay decimal strings from the API (never parsed).
 *
 * - `PayrollAdminTab` — Admin → People → Payroll: configuration versions, periods, runs
 *   (calculate → validate → submit → approve → finalise → post → payslips → close), results,
 *   final settlements.
 * - `MyPayslipsPanel` — My HR (`/leave`): the caller's own payslips.
 */
import { Banknote, CalendarRange, FileText, Layers, Settings2, UserCheck, UserMinus } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, FormSelect, KpiTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, NoInstitution, REASON, todayIso } from "../../institution/forms";
import { Acts, days, inr as formatInr, useAsk, type RowAction } from "../ui";
import { useWorkforceApi, wkeys } from "../workforce/hooks";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { pkeys, runActions, usePayrollApi } from "./hooks";
import type { PayrollRun } from "./types";

export { PayrollApiProvider } from "./hooks";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const monthLabel = (year: number, month: number) => `${MONTHS[month - 1] ?? month} ${year}`;
const inr = (v: string | null | undefined) => (v ? formatInr(v) : "—");
/** The run's next forward step: the row's one primary pill (the rest go in its kebab). */
const NEXT_RUN_STEP: Record<string, string> = { created: "calculate", reprocessing: "calculate", calculated: "validate", validating: "submit", pending_approval: "approve", approved: "finalise", finalized: "post", posted: "issue", failed: "reprocess" };
const COMPONENT_KINDS = [
  { value: "earning", label: "Earning" },
  { value: "deduction", label: "Deduction" },
  { value: "employer_contribution", label: "Employer contribution" },
];
const BASES = [
  { value: "fixed", label: "Fixed amount" },
  { value: "percent_of_basic", label: "% of basic" },
  { value: "percent_of_gross", label: "% of gross" },
  { value: "statutory", label: "Statutory rule" },
];

/** Salary structures (by grade) and per-engagement salary assignments (staff picker, no raw ids). */
function SalarySetup({ institutionId }: { institutionId: string }) {
  const api = usePayrollApi();
  const workforce = useWorkforceApi();
  const { submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const structures = useQuery({ queryKey: pkeys.scoped(institutionId, "structures"), queryFn: () => api.structures(institutionId) }).data ?? [];
  const staff = useQuery({ queryKey: wkeys.scoped(institutionId, "staff:"), queryFn: () => workforce.staff(institutionId) }).data ?? [];
  const engaged = staff.filter((x) => x.engagement && !["closed", "cancelled"].includes(x.engagement.status));
  const [picked, setPicked] = useState("");
  const person = engaged.find((x) => x.id === picked) ?? engaged[0];
  const engagementId = person?.engagement?.id ?? "";
  const assignments = useQuery({ queryKey: pkeys.scoped(engagementId || "none", "assignments"), queryFn: () => api.assignments(engagementId), enabled: !!engagementId }).data ?? [];
  return (
    <>
      <BentoTile
        span={6}
        title="Salary structures"
        data-slice="7C"
        action={
          <Button
            size="sm"
            variant="ghost"
            data-action="structure-new"
            disabled={busy}
            onClick={() =>
              ask({
                title: "New salary structure",
                name: "structure-create",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "AP-1" },
                  { name: "name", label: "Name", required: true, placeholder: "Assistant Professor 1" },
                  { name: "grade", label: "Grade (assigned by default)", placeholder: "AP-1" },
                  { name: "basic", label: "Basic pay per month", required: true, placeholder: "30000.00" },
                ],
                submitLabel: "Create",
                onSubmit: (v) =>
                  submit(
                    () => api.createStructure(institutionId, { code: (v.code ?? "").trim(), name: v.name ?? "", grade: v.grade || undefined, lines: [{ componentCode: "BASIC", amount: v.basic ?? "" }] }),
                    (st) => `${st.code} created`,
                  ),
              })
            }
          >
            New structure
          </Button>
        }
      >
        <DataTable
          caption="Salary structures"
          rows={structures}
          rowKey={(st) => st.id}
          empty={<EmptyState icon={Layers} title="No salary structures" compact>Create one per grade with its basic pay.</EmptyState>}
          columns={[
            { key: "code", header: "Structure", render: (st) => `${st.code} · ${st.name}` },
            { key: "grade", header: "Grade", render: (st) => st.grade ?? "—", muted: true },
            { key: "lines", header: "Pay lines", render: (st) => st.lines.map((l) => `${l.componentCode} ${l.amount ? inr(l.amount) : `${l.rate ?? ""}%`}`).join(", ") },
            { key: "status", header: "Status", render: (st) => <StatusBadge status={st.status} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={6}
        title="Salary assignments"
        data-slice="7C"
        action={
          <Button
            size="sm"
            variant="ghost"
            data-action="salary-assign-new"
            disabled={busy || engaged.length === 0 || structures.length === 0}
            onClick={() =>
              ask({
                title: "Assign a salary structure",
                name: "salary-assign",
                fields: [
                  { name: "engagementId", label: "Staff member", type: "select", required: true, options: engaged.map((x) => ({ value: x.engagement?.id ?? "", label: `${x.staffNumber} · ${x.displayName}` })), initial: engagementId },
                  { name: "structureId", label: "Structure", type: "select", required: true, options: structures.map((st) => ({ value: st.id, label: `${st.code} · ${st.name}` })), initial: structures[0]?.id ?? "" },
                  { name: "effectiveFrom", label: "Effective from", type: "date", required: true, initial: todayIso() },
                ],
                submitLabel: "Assign",
                onSubmit: (v) => {
                  const target = engaged.find((x) => x.engagement?.id === v.engagementId);
                  if (target) setPicked(target.id);
                  return submit(() => api.assignSalary(v.engagementId ?? "", { structureId: v.structureId || undefined, effectiveFrom: v.effectiveFrom ?? "" }), (a) => `${a.structureCode} assigned`);
                },
              })
            }
          >
            Assign salary
          </Button>
        }
      >
        {engaged.length > 0 && <FormSelect label="Staff member" value={person?.id ?? ""} options={engaged.map((x) => ({ value: x.id, label: `${x.staffNumber} · ${x.displayName}` }))} onChange={(e) => setPicked(e.target.value)} />}
        <DataTable
          caption="Salary assignments"
          rows={assignments.map((a) => ({ ...a, who: person ? `${person.staffNumber} · ${person.displayName}` : "—" }))}
          rowKey={(a) => a.id}
          empty={<EmptyState icon={UserCheck} title="No salary assignment" compact>Assign a structure before running payroll.</EmptyState>}
          columns={[
            { key: "who", header: "Staff", render: (a) => a.who },
            { key: "structure", header: "Structure", render: (a) => a.structureCode },
            { key: "from", header: "From", render: (a) => `${fmtDate(a.effectiveFrom)}${a.effectiveTo ? ` – ${fmtDate(a.effectiveTo)}` : ""}`, muted: true },
            { key: "status", header: "Status", render: (a) => <StatusBadge status={a.status} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

export function PayrollAdminTab({ institution }: { institution: Institution | null }) {
  if (!institution) {
    return (
      <BentoTile span={12} data-slice="7C">
        <NoInstitution />
      </BentoTile>
    );
  }
  return <PayrollAdmin institution={institution} />;
}

function PayrollAdmin({ institution }: { institution: Institution }) {
  const api = usePayrollApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution.id;
  const configs = useQuery({ queryKey: pkeys.scoped(inst, "configs"), queryFn: () => api.configs(inst) });
  const periods = useQuery({ queryKey: pkeys.scoped(inst, "periods"), queryFn: () => api.periods(inst) });
  const settlements = useQuery({ queryKey: pkeys.scoped(inst, "settlements"), queryFn: () => api.settlements(inst) });
  const [picked, setPicked] = useState("");
  const periodList = periods.data ?? [];
  const periodId = picked || periodList[0]?.id || "";
  const runs = useQuery({ queryKey: pkeys.scoped(periodId || "none", "runs"), queryFn: () => api.runs(periodId), enabled: !!periodId });
  const runList = runs.data ?? [];
  const [runPicked, setRunPicked] = useState("");
  const current = runList.find((r) => r.id === runPicked) ?? runList[0];
  const results = useQuery({ queryKey: pkeys.scoped(current?.id ?? "none", "results"), queryFn: () => api.results(current?.id ?? ""), enabled: !!current });
  const active = (configs.data ?? []).find((c) => c.status === "active");
  const workforce = useWorkforceApi();
  const staff = useQuery({ queryKey: wkeys.scoped(inst, "staff:"), queryFn: () => workforce.staff(inst) }).data ?? [];
  const settledIds = new Set((settlements.data ?? []).map((x) => x.engagementId));
  const closed = staff.filter((x) => x.engagement?.status === "closed" && !settledIds.has(x.engagement.id));

  const runAct = (r: PayrollRun, a: ReturnType<typeof runActions>[number]) => {
    switch (a) {
      case "approve":
        return run(() => api.decideRun(r.id, r.version, "approve"), `${r.number} approved`);
      case "reject":
        return ask({
          title: `Reject ${r.number}`,
          name: "run-reject",
          fields: [REASON],
          submitLabel: "Reject",
          onSubmit: (v) => submit(() => api.decideRun(r.id, r.version, "reject", v.reason), `${r.number} sent back`),
        });
      case "issue":
        return run(() => api.issuePayslips(r.id), (s) => `${s.processed} payslips issued, ${s.skipped} already issued`);
      default:
        return run(() => api.runCommand(r.id, r.version, a), `${r.number}: ${statusLabel(a)}`);
    }
  };

  return (
    <>
      <KpiTile span={4} icon={Settings2} tone="violet" label="Active configuration" value={active ? `v${active.versionNo}` : "None"} sub={active ? `${active.name} · from ${fmtDate(active.effectiveFrom)}` : "Activate a version to run payroll"} data-kpi="payroll-config" />
      <KpiTile span={4} icon={Banknote} tone="green" label="Net pay (selected run)" value={current ? inr(current.netPay) : "—"} sub={current ? `${current.number} · ${current.headcount} staff` : "No run yet"} data-kpi="payroll-net" />
      <KpiTile span={4} icon={UserMinus} tone="orange" label="Final settlements" value={String((settlements.data ?? []).filter((s) => s.status !== "completed").length)} sub="Open settlements" data-kpi="payroll-fnf" />

      <BentoTile
        span={12}
        title="Payroll configuration"
        data-slice="7C"
        action={
          <Button
            size="sm"
            variant="ghost"
            data-action="new-config"
            disabled={busy}
            onClick={() =>
              ask({
                title: "New configuration version",
                name: "config-create",
                fields: [
                  { name: "name", label: "Name", required: true },
                  { name: "effectiveFrom", label: "Effective from", type: "date", required: true },
                ],
                submitLabel: "Create draft",
                onSubmit: (v) => submit(() => api.createConfig(inst, { name: v.name ?? "", effectiveFrom: v.effectiveFrom ?? "", copyFrom: active?.id }), (c) => `Draft v${c.versionNo} created`),
              })
            }
          >
            New version
          </Button>
        }
      >
        <DataTable caption="Configuration versions" rows={configs.data ?? []} rowKey={(c) => c.id}
          empty={<EmptyState icon={Settings2} title="No configuration" compact>Create a version with pay components and statutory rules.</EmptyState>}
          columns={[
            { key: "v", header: "Version", render: (c) => `v${c.versionNo} · ${c.name}` },
            { key: "from", header: "Effective from", render: (c) => fmtDate(c.effectiveFrom) },
            { key: "components", header: "Components", render: (c) => (c.components.length ? c.components.map((x) => x.code).join(", ") : "—"), muted: true },
            { key: "status", header: "Status", render: (c) => <StatusBadge status={c.status} /> },
            {
              key: "actions",
              header: "",
              render: (c) => {
                const acts: RowAction[] = [];
                if (c.status === "draft")
                  acts.push({
                    id: "config-component",
                    label: "Add component",
                    onClick: () =>
                      ask({
                        title: `Pay component · v${c.versionNo}`,
                        name: "config-component",
                        fields: [
                          { name: "code", label: "Code", required: true, placeholder: "BASIC" },
                          { name: "name", label: "Name", required: true, placeholder: "Basic pay" },
                          { name: "kind", label: "Kind", type: "select", required: true, options: COMPONENT_KINDS, initial: "earning" },
                          { name: "basis", label: "Basis", type: "select", required: true, options: BASES, initial: "fixed" },
                          { name: "amount", label: "Amount (fixed)", placeholder: "0.00" },
                          { name: "rate", label: "Rate % (percent basis)", placeholder: "40" },
                        ],
                        submitLabel: "Add",
                        onSubmit: (v) =>
                          submit(
                            () => api.addComponent(c.id, c.version, { code: (v.code ?? "").trim().toUpperCase(), name: v.name ?? "", kind: v.kind ?? "earning", basis: v.basis ?? "fixed", amount: v.amount || undefined, rate: v.rate || undefined }),
                            "Component added",
                          ),
                      }),
                  });
                if (c.status === "draft") acts.push({ id: "config-submit", label: "Submit", onClick: () => void run(() => api.configCommand(c.id, c.version, "submit"), `v${c.versionNo} submitted`) });
                if (c.status === "under_review" || c.status === "pending_approval") {
                  acts.push({ id: "config-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideConfig(c.id, c.version, "approve"), `v${c.versionNo} approved`) });
                  acts.push({ id: "config-reject", label: "Reject", onClick: () => ask({ title: `Reject v${c.versionNo}`, name: "config-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.decideConfig(c.id, c.version, "reject", v.reason), `v${c.versionNo} sent back`) }) });
                }
                if (c.status === "approved") acts.push({ id: "config-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.configCommand(c.id, c.version, "activate"), `v${c.versionNo} active`) });
                return <Acts disabled={busy} actions={acts} />;
              },
            },
          ]}
        />
      </BentoTile>

      <SalarySetup institutionId={inst} />

      <BentoTile
        span={12}
        title="Payroll runs"
        data-slice="7C"
        action={
          <Acts
            disabled={busy}
            actions={[
              {
                id: "new-period",
                label: "Open period",
                onClick: () =>
                  ask({
                    title: "Open payroll period",
                    name: "period-create",
                    fields: [
                      { name: "year", label: "Year", type: "number", required: true },
                      { name: "month", label: "Month (1–12)", type: "number", required: true },
                    ],
                    submitLabel: "Open period",
                    onSubmit: (v) => submit(() => api.createPeriod(inst, Number(v.year), Number(v.month)), (p) => `${monthLabel(p.year, p.month)} opened`),
                  }),
              },
              ...(periodId
                ? [
                    { id: "new-run", label: "Regular run", onClick: () => void run(() => api.createRun(periodId, "regular"), (r) => `${r.number} created`) },
                    { id: "new-retro", label: "Retro run", onClick: () => void run(() => api.createRun(periodId, "retro"), (r) => `${r.number} (retro) created`) },
                  ]
                : []),
            ]}
          />
        }
      >
        {periodList.length > 0 && (
          <FormSelect
            label="Period"
            value={periodId}
            onChange={(e) => {
              setPicked(e.target.value);
              setRunPicked("");
            }}
            options={periodList.map((p) => ({ value: p.id, label: `${monthLabel(p.year, p.month)} (${statusLabel(p.status)})` }))}
            data-field="period"
          />
        )}
        {periodList.length === 0 ? (
          <EmptyState icon={CalendarRange} title="No payroll period" compact>Open the month you want to pay.</EmptyState>
        ) : (
          <DataTable caption="Runs" rows={runList} rowKey={(r) => r.id}
            empty={<EmptyState icon={Banknote} title="No runs" compact>Create the regular run of the period.</EmptyState>}
            columns={[
              { key: "number", header: "Run", render: (r) => (
                  <Button size="sm" variant="ghost" onClick={() => setRunPicked(r.id)} data-run={r.id}>
                    {`${r.number} · ${statusLabel(r.kind)}`}
                  </Button>
                ) },
              { key: "staff", header: "Staff", render: (r) => String(r.headcount) },
              { key: "gross", header: "Gross", render: (r) => inr(r.grossEarnings) },
              { key: "ded", header: "Deductions", render: (r) => inr(r.employeeDeductions) },
              { key: "net", header: "Net pay", render: (r) => inr(r.netPay) },
              { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
              { key: "je", header: "Journal", render: (r) => r.journalEntryNumber ?? "—", muted: true },
              {
                key: "actions",
                header: "",
                render: (r) => (
                  <Acts
                    disabled={busy}
                    actions={runActions(r.status).map((a) => ({ id: `run-${a}`, label: statusLabel(a), variant: a === NEXT_RUN_STEP[r.status] ? "primary" : "ghost", onClick: () => void runAct(r, a) }))}
                  />
                ),
              },
            ]}
          />
        )}
        {current && current.exceptions.length > 0 && (
          <ul data-exceptions={current.id}>
            {current.exceptions.map((x, i) => (
              <li key={`${x.code}-${i}`}>
                {x.staffNumber ?? "—"}: {x.message}
              </li>
            ))}
          </ul>
        )}
      </BentoTile>

      <BentoTile span={12} title={current ? `Results · ${current.number}` : "Results"} data-slice="7C">
        <DataTable caption="Payroll results" rows={results.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={FileText} title="No results" compact>Calculate the run to produce per-employee results.</EmptyState>}
          columns={[
            { key: "staff", header: "Staff", render: (r) => `${r.staffNumber} · ${r.displayName}` },
            { key: "rev", header: "Rev.", render: (r) => String(r.revision), muted: true },
            { key: "lop", header: "Loss of pay", render: (r) => days(r.lopDays) },
            { key: "gross", header: "Gross", render: (r) => inr(r.grossEarnings) },
            { key: "ded", header: "Deductions", render: (r) => inr(r.totalDeductions) },
            { key: "net", header: "Net pay", render: (r) => inr(r.netPay) },
            { key: "status", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
          ]}
        />
      </BentoTile>

      <BentoTile
        span={12}
        title="Final settlements"
        data-slice="7C"
        action={
          <Button
            size="sm"
            variant="ghost"
            data-action="settlement-new"
            disabled={busy || closed.length === 0}
            onClick={() =>
              ask({
                title: "Start a final settlement",
                name: "settlement-create",
                fields: [
                  { name: "engagementId", label: "Closed engagement", type: "select", required: true, options: closed.map((x) => ({ value: x.engagement?.id ?? "", label: `${x.staffNumber} · ${x.displayName}` })), initial: closed[0]?.engagement?.id ?? "" },
                  { name: "recovery", label: "Recovery amount (optional)", placeholder: "0.00" },
                  { name: "notes", label: "Notes", wide: true },
                ],
                submitLabel: "Start",
                onSubmit: (v) =>
                  submit(
                    () => api.createSettlement(v.engagementId ?? "", { recoveries: v.recovery ? [{ code: "RECOVERY", name: "Recovery", amount: v.recovery }] : undefined, notes: v.notes || undefined }),
                    "Final settlement started",
                  ),
              })
            }
          >
            Start settlement
          </Button>
        }
      >
        <DataTable caption="Final settlements" rows={settlements.data ?? []} rowKey={(s) => s.id}
          empty={<EmptyState icon={UserMinus} title="No settlements" compact>Settlements start from closed engagements.</EmptyState>}
          columns={[
            { key: "number", header: "Settlement", render: (s) => s.number ?? "—" },
            { key: "lwd", header: "Last working day", render: (s) => fmtDate(s.lastWorkingDate) },
            { key: "salary", header: "Salary", render: (s) => inr(s.salaryAmount) },
            { key: "enc", header: "Encashment", render: (s) => inr(s.encashmentAmount) },
            { key: "rec", header: "Recoveries", render: (s) => inr(s.recoveryAmount) },
            { key: "net", header: "Net pay", render: (s) => inr(s.netPay) },
            { key: "status", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            {
              key: "actions",
              header: "",
              render: (f) => {
                const acts: RowAction[] = [];
                if (f.status === "initiated" || f.status === "calculated") acts.push({ id: "settlement-calculate", label: "Calculate", onClick: () => void run(() => api.settlementCommand(f.id, f.version, "calculate"), "Settlement calculated") });
                if (f.status === "pending_approval") acts.push({ id: "settlement-approve", label: "Approve", variant: "primary", onClick: () => void run(() => api.decideSettlement(f.id, f.version, "approve"), "Settlement approved") });
                if (f.status === "approved") acts.push({ id: "settlement-complete", label: "Complete", variant: "primary", onClick: () => void run(() => api.settlementCommand(f.id, f.version, "complete"), (r) => `${r.number ?? "Settlement"} completed`) });
                return <Acts disabled={busy} actions={acts} />;
              },
            },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

/** My HR (`/leave`): own payslips. */
export function MyPayslipsPanel() {
  const api = usePayrollApi();
  const slips = useQuery({ queryKey: pkeys.scoped("me", "payslips"), queryFn: () => api.myPayslips(), retry: false });
  const list = slips.data ?? [];
  return (
    <BentoTile span={12} title="My payslips" data-slice="7C">
      {list.length === 0 ? (
        <EmptyState icon={FileText} title="No payslips yet" compact>Payslips appear here once payroll is finalised.</EmptyState>
      ) : (
        <DataTable caption="Payslips" rows={list} rowKey={(p) => p.id}
          columns={[
            { key: "number", header: "Payslip", render: (p) => p.number },
            { key: "period", header: "Period", render: (p) => `${fmtDate(p.periodStart)} – ${fmtDate(p.periodEnd)}` },
            { key: "gross", header: "Gross", render: (p) => inr(p.grossEarnings) },
            { key: "ded", header: "Deductions", render: (p) => inr(p.totalDeductions) },
            { key: "net", header: "Net pay", render: (p) => inr(p.netPay) },
            { key: "lop", header: "Loss of pay", render: (p) => days(p.lopDays), muted: true },
            { key: "status", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
          ]}
        />
      )}
    </BentoTile>
  );
}
