/**
 * ERP_EXTENSIONS 1E: periodic billing runs (ported from Everything's childcare billing run, MIT).
 * Billing plans bill a fee head every week, month, quarter, half-year or financial year; students
 * are enrolled at the plan's amount or their own. Preview a run for a period, run it (one
 * published charge per student, safe to repeat), turn on a frequency's automatic run, and look
 * back at past runs. The server decides everything; `periodFor` only labels the period picked.
 */
import { CalendarClock, Plus } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, FormGrid, FormSelect, KeyValue, TextField, formatDate } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { todayIso } from "../../institution/forms";
import { useCommand } from "../../institution/hooks";
import { PersonPicker } from "../../campus/PersonPicker";
import { fkeys, useChargesApi } from "./hooks";
import { FREQUENCIES, periodFor } from "./billing";
import type { BillingFrequency, BillingPlan, BillingRun } from "./types";

const SKIP_WORDS: Record<string, string> = {
  inactive: "Not active",
  already_billed: "Already billed",
  not_started: "Not started yet",
  ended: "Enrolment ended",
  on_hold: "On hold",
  excluded: "Left out",
  nothing_to_bill: "Nothing to bill",
};

export const skipWords = (reason: string) => SKIP_WORDS[reason] ?? reason;

const freqLabel = (f: string) => FREQUENCIES.find((x) => x.value === f)?.label ?? f;
const rupees = (s: string) => `₹${Number(s).toLocaleString("en-IN", { minimumFractionDigits: 2, maximumFractionDigits: 2 })}`;

export function BillingRunsTile({ institutionId }: { institutionId: string }) {
  const api = useChargesApi();
  const { run, busy, dialog } = useCommand();
  const [view, setView] = useState<"plans" | "run" | "schedules" | "history">("plans");
  const [planId, setPlanId] = useState<string | null>(null);
  const [frequency, setFrequency] = useState<BillingFrequency>("monthly");
  const [day, setDay] = useState(todayIso());
  const [dueDays, setDueDays] = useState("7");
  const [result, setResult] = useState<BillingRun | null>(null);
  const [draft, setDraft] = useState({ code: "", name: "", feeHeadId: "", amount: "", frequency: "monthly" as BillingFrequency, proration: "none" as "none" | "daily" });
  const [enrol, setEnrol] = useState({ studentId: "", amount: "", startDate: "", endDate: "" });
  const plans = useQuery({ queryKey: fkeys.scoped(institutionId, "billing-plans"), queryFn: () => api.billingPlans(institutionId) });
  const heads = useQuery({ queryKey: fkeys.scoped(institutionId, "fee-heads"), queryFn: () => api.feeHeads(institutionId) });
  const enrolments = useQuery({ queryKey: fkeys.scoped(planId ?? "-", "billing-enrolments"), queryFn: () => api.billingEnrolments(planId!), enabled: !!planId });
  const schedules = useQuery({ queryKey: fkeys.scoped(institutionId, "billing-schedules"), queryFn: () => api.billingSchedules(institutionId), enabled: view === "schedules" });
  const history = useQuery({ queryKey: fkeys.scoped(institutionId, "billing-runs"), queryFn: () => api.billingRuns(institutionId), enabled: view === "history" });
  const due = Number.parseInt(dueDays, 10);
  const request = { frequency, day, dueDays: Number.isFinite(due) ? due : 7 };
  const period = periodFor(frequency, day);
  const plan: BillingPlan | undefined = (plans.data ?? []).find((p) => p.id === planId);
  const tabs = (
    <div className="flex gap-1" role="tablist">
      {(["plans", "run", "schedules", "history"] as const).map((v) => (
        <Button key={v} size="sm" variant={view === v ? "primary" : "ghost"} data-billing-view={v} onClick={() => setView(v)}>
          {v === "plans" ? "Plans" : v === "run" ? "Run" : v === "schedules" ? "Automatic" : "History"}
        </Button>
      ))}
    </div>
  );
  return (
    <BentoTile span={12} title="Billing runs" data-slice="1E" data-billing-runs action={tabs}>
      {view === "plans" && (
        <>
          <DataTable caption="Billing plans" rows={plans.data ?? []} rowKey={(p) => p.id}
            empty={<EmptyState icon={CalendarClock} title="No billing plans">Create a plan (hostel, transport, mess…) and enrol students on it.</EmptyState>}
            columns={[
              { key: "c", header: "Plan", render: (p) => <button type="button" className="underline" data-billing-plan={p.code} onClick={() => setPlanId(p.id)}>{`${p.code} · ${p.name}`}</button> },
              { key: "f", header: "Every", render: (p) => freqLabel(p.frequency) },
              { key: "a", header: "Amount", render: (p) => rupees(p.amount) },
              { key: "h", header: "Fee head", render: (p) => p.feeHeadCode },
              { key: "p", header: "Part period", render: (p) => (p.proration === "daily" ? "By day" : "Full") },
              { key: "e", header: "Students", render: (p) => p.enrolled },
              { key: "s", header: "Status", render: (p) => <Badge kind={p.status === "active" ? "done" : "neutral"}>{p.status}</Badge> },
              { key: "x", header: "", render: (p) => (
                <Button size="sm" variant="ghost" data-action="toggle-plan" disabled={busy} onClick={() => void run(() => api.updateBillingPlan(p.id, { status: p.status === "active" ? "retired" : "active" }), p.status === "active" ? "Plan retired" : "Plan reactivated")}>
                  {p.status === "active" ? "Retire" : "Reactivate"}
                </Button>
              ) },
            ]} />
          <FormGrid columns={3}>
            <TextField label="Code" value={draft.code} placeholder="HOSTEL-MONTHLY" onChange={(e) => setDraft({ ...draft, code: e.target.value.toUpperCase() })} />
            <TextField label="Name" value={draft.name} placeholder="Hostel rent" onChange={(e) => setDraft({ ...draft, name: e.target.value })} />
            <FormSelect label="Fee head" value={draft.feeHeadId} onChange={(e) => setDraft({ ...draft, feeHeadId: e.target.value })}
              options={[{ value: "", label: "Choose…" }, ...(heads.data ?? []).filter((h) => h.status === "active").map((h) => ({ value: h.id, label: `${h.code} · ${h.name}` }))]} />
            <TextField label="Amount (₹)" value={draft.amount} placeholder="4500.00" onChange={(e) => setDraft({ ...draft, amount: e.target.value })} />
            <FormSelect label="Every" value={draft.frequency} onChange={(e) => setDraft({ ...draft, frequency: e.target.value as BillingFrequency })} options={FREQUENCIES} />
            <FormSelect label="Part period" value={draft.proration} onChange={(e) => setDraft({ ...draft, proration: e.target.value as "none" | "daily" })}
              options={[{ value: "none", label: "Bill in full" }, { value: "daily", label: "Prorate by day" }]} />
          </FormGrid>
          <Button size="sm" data-action="new-billing-plan" disabled={busy || !draft.code || !draft.name || !draft.feeHeadId || !draft.amount}
            onClick={() => void run(() => api.createBillingPlan(institutionId, draft), "Billing plan created")}>
            <Plus size={14} /> Plan
          </Button>
          {plan && (
            <div data-billing-enrolments={plan.code}>
              <DataTable caption={`Students on ${plan.name}`} rows={enrolments.data ?? []} rowKey={(e) => e.id}
                empty={<EmptyState icon={CalendarClock} title="Nobody enrolled">Enrol students to bill them on this plan.</EmptyState>}
                columns={[
                  { key: "n", header: "Student", render: (e) => `${e.studentName}${e.studentNumber ? ` · ${e.studentNumber}` : ""}` },
                  { key: "a", header: "Amount", render: (e) => (e.amount != null ? rupees(e.amount) : `${rupees(plan.amount)} (plan)`) },
                  { key: "f", header: "From", render: (e) => (e.startDate ? formatDate(e.startDate) : "—") },
                  { key: "t", header: "Until", render: (e) => (e.endDate ? formatDate(e.endDate) : "—") },
                  { key: "h", header: "", render: (e) => (
                    <span className="flex gap-1">
                      <Button size="sm" variant="ghost" data-action="hold" disabled={busy} onClick={() => void run(() => api.enrol(plan.id, { studentId: e.studentId, amount: e.amount ?? null, startDate: e.startDate ?? null, endDate: e.endDate ?? null, onHold: !e.onHold }), e.onHold ? "Billing resumed" : "Put on hold")}>
                        {e.onHold ? "Resume" : "Hold"}
                      </Button>
                      <Button size="sm" variant="ghost" data-action="unenrol" disabled={busy} onClick={() => void run(() => api.unenrol(plan.id, e.studentId), "Removed from plan")}>Remove</Button>
                    </span>
                  ) },
                ]} />
              <FormGrid columns={3}>
                <PersonPicker institutionId={institutionId} kind="student" label="Student" name="billingStudentId" value={enrol.studentId} onChange={(id) => setEnrol({ ...enrol, studentId: id })} />
                <TextField label="Own amount (₹, optional)" value={enrol.amount} onChange={(e) => setEnrol({ ...enrol, amount: e.target.value })} />
                <TextField label="From" type="date" value={enrol.startDate} onChange={(e) => setEnrol({ ...enrol, startDate: e.target.value })} />
                <TextField label="Until" type="date" value={enrol.endDate} onChange={(e) => setEnrol({ ...enrol, endDate: e.target.value })} />
              </FormGrid>
              <Button size="sm" data-action="enrol" disabled={busy || !enrol.studentId}
                onClick={() => void run(() => api.enrol(plan.id, { studentId: enrol.studentId, amount: enrol.amount || null, startDate: enrol.startDate || null, endDate: enrol.endDate || null }), "Student enrolled")}>
                Enrol
              </Button>
            </div>
          )}
        </>
      )}
      {view === "run" && (
        <>
          <FormGrid columns={3}>
            <FormSelect label="Frequency" value={frequency} onChange={(e) => setFrequency(e.target.value as BillingFrequency)} options={FREQUENCIES} />
            <TextField label="A day in the period" type="date" value={day} onChange={(e) => setDay(e.target.value)} />
            <TextField label="Due after (days)" value={dueDays} onChange={(e) => setDueDays(e.target.value)} />
          </FormGrid>
          <KeyValue label="Period" value={`${period.label} · ${formatDate(period.start)} to ${formatDate(period.end)}`} />
          <div className="flex gap-2">
            <Button size="sm" variant="ghost" data-action="preview-billing" disabled={busy} onClick={() => void run(async () => setResult(await api.billingRun(institutionId, request, true)), "Preview ready")}>Preview</Button>
            <Button size="sm" data-action="run-billing" disabled={busy || !result?.preview || result.billed === 0}
              onClick={() => void run(async () => setResult(await api.billingRun(institutionId, request, false)), "Billing run done")}>
              Run billing
            </Button>
          </div>
          {result && (
            <div data-billing-result={result.preview ? "preview" : "run"}>
              <KeyValue label={result.preview ? "Would bill" : "Billed"} value={`${result.billed} students · ${rupees(result.total)}`} />
              <KeyValue label="Skipped" value={String(result.skipped)} />
              <KeyValue label="Could not bill" value={String(result.failed)} />
              <KeyValue label="Period" value={result.periodLabel} />
              <DataTable caption="Charges" rows={result.items} rowKey={(i) => i.studentId}
                empty={<EmptyState icon={CalendarClock} title="Nothing to bill">Everyone on these plans is billed for this period already.</EmptyState>}
                columns={[
                  { key: "n", header: "Student", render: (i) => i.studentName },
                  { key: "l", header: "Plans", render: (i) => i.lines.map((l) => `${l.description}${l.proratedDays ? ` (${l.proratedDays}/${l.periodDays} days)` : ""}`).join("; ") },
                  { key: "t", header: "Amount", render: (i) => rupees(i.total) },
                  { key: "d", header: "Due", render: (i) => formatDate(i.dueDate) },
                  { key: "c", header: "Charge", render: (i) => (i.error ? <Badge kind="attention">{i.error}</Badge> : i.chargeNumber ?? "—") },
                ]} />
              {result.skips.length > 0 && (
                <DataTable caption="Skipped" rows={result.skips} rowKey={(s) => `${s.studentId}-${s.reason}`}
                  columns={[
                    { key: "n", header: "Student", render: (s) => s.studentName },
                    { key: "r", header: "Why", render: (s) => skipWords(s.reason) },
                  ]} />
              )}
            </div>
          )}
        </>
      )}
      {view === "schedules" && (
        <DataTable caption="Automatic runs" rows={schedules.data ?? []} rowKey={(s) => s.frequency}
          columns={[
            { key: "f", header: "Frequency", render: (s) => freqLabel(s.frequency) },
            { key: "n", header: "Current period", render: (s) => s.currentPeriodLabel },
            { key: "d", header: "Runs on day", render: (s) => s.runDay },
            { key: "u", header: "Due after", render: (s) => `${s.dueDays} days` },
            { key: "l", header: "Last billed", render: (s) => s.lastPeriodKey ?? "—" },
            { key: "e", header: "", render: (s) => (
              <Button size="sm" variant={s.enabled ? "ghost" : "primary"} data-action="toggle-schedule" data-frequency={s.frequency} disabled={busy}
                onClick={() => void run(() => api.setBillingSchedule(institutionId, s.frequency, { enabled: !s.enabled, runDay: s.runDay, dueDays: s.dueDays }), s.enabled ? "Automatic run off" : "Automatic run on")}>
                {s.enabled ? "Turn off" : "Turn on"}
              </Button>
            ) },
          ]} />
      )}
      {view === "history" && (
        <DataTable caption="Past runs" rows={history.data ?? []} rowKey={(r) => r.id}
          empty={<EmptyState icon={CalendarClock} title="No runs yet">Preview and run billing for a period.</EmptyState>}
          columns={[
            { key: "p", header: "Period", render: (r) => r.periodLabel },
            { key: "f", header: "Frequency", render: (r) => freqLabel(r.frequency) },
            { key: "d", header: "Run on", render: (r) => formatDate(r.runDate) },
            { key: "t", header: "How", render: (r) => (r.trigger === "schedule" ? "Automatic" : "By hand") },
            { key: "b", header: "Billed", render: (r) => `${r.billed} · ${rupees(r.total)}` },
            { key: "x", header: "Skipped / failed", render: (r) => `${r.skipped} / ${r.failed}` },
          ]} />
      )}
      {dialog}
    </BentoTile>
  );
}
