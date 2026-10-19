/**
 * ERP_EXTENSIONS 5C: time tracking set-up and oversight (Admin → People → Workload). Research
 * projects / sponsored grants / committees with members and budget bars, the timesheet approval
 * queue, the effort report (hours, % effort, person-months, cost; CSV) and staff hourly rates.
 * Maths ported from Everything's time-tracking (MIT); the server is the source of truth.
 */
import { Timer } from "lucide-react";
import { Badge, BentoTile, Button, Checkbox, DataTable, EmptyState, formatDate, formatINR, FormGrid, FormSelect, KeyValue, plural, ProgressBar, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../lib/api";
import { PersonPicker } from "../../campus/PersonPicker";
import { useCommand } from "../../institution/hooks";
import { createHttpTimeApi, type TimeApi } from "./api";
import { createMockTimeApi } from "./mock";
import { budgetBar, FUNDING_AGENCIES, human, KIND_LABEL, monthLabel, ROLE_LABEL, shiftMonth, STATUS_LABEL, statusKind } from "./timetrack";
import type { MemberRole, ProjectKind, TimeProjectInput, TimeSettings } from "./types";

let fallback: TimeApi | null = null;
export function defaultTimeApi(): TimeApi {
  fallback ??= USES_API ? createHttpTimeApi() : createMockTimeApi();
  return fallback;
}

type Tab = "projects" | "approvals" | "effort" | "rates" | "settings";
const TABS: { value: Tab; label: string }[] = [
  { value: "projects", label: "Projects & grants" },
  { value: "approvals", label: "Approvals" },
  { value: "effort", label: "Effort report" },
  { value: "rates", label: "Rates" },
  { value: "settings", label: "Settings" },
];

const KIND_OPTIONS = (Object.keys(KIND_LABEL) as ProjectKind[]).map((k) => ({ value: k, label: KIND_LABEL[k] }));
const ROLE_OPTIONS = (Object.keys(ROLE_LABEL) as MemberRole[]).filter((r) => r !== "pi").map((r) => ({ value: r, label: ROLE_LABEL[r] }));
const AGENCY_OPTIONS = [{ value: "", label: "—" }, ...FUNDING_AGENCIES.map((a) => ({ value: a, label: a }))];
const ROUNDING_OPTIONS = [0, 5, 6, 10, 15, 30].map((m) => ({ value: String(m), label: m ? `Up to ${m} minutes` : "No rounding" }));
const blank = (s: string) => (s.trim() === "" ? null : s.trim());

const EMPTY_FORM = { code: "", name: "", kind: "sponsored_research" as ProjectKind, fundingAgency: "SERB (ANRF)", sanctionNumber: "", sanctionedAmount: "", manpowerBudget: "", startDate: "", endDate: "", piStaffId: "", costCentre: "", budgetHours: "", openToAll: false };

export function TimeTrackingTile({ institutionId, api = defaultTimeApi() }: { institutionId: string; api?: TimeApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("projects");
  const thisMonth = new Date().toISOString().slice(0, 7);
  const [range, setRange] = useState({ from: shiftMonth(thisMonth, -1), to: thisMonth, approvedOnly: false });
  const [form, setForm] = useState(EMPTY_FORM);
  const [members, setMembers] = useState<{ staffId: string; role: MemberRole }[]>([]);
  const [pick, setPick] = useState({ staffId: "", role: "jrf" as MemberRole });
  const [rateDraft, setRateDraft] = useState({ staffId: "", rate: "", from: "" });
  const [draft, setDraft] = useState<TimeSettings | null>(null);
  // Prefix `institution`: `useCommand` refreshes every `institution` query after a command.
  const key = (...p: (string | boolean)[]) => ["institution", "time", institutionId, ...p];
  const projects = useQuery({ queryKey: key("projects"), queryFn: () => api.projects(institutionId), enabled: tab === "projects" });
  const queue = useQuery({ queryKey: key("timesheets", "submitted"), queryFn: () => api.timesheets(institutionId, "submitted"), enabled: tab === "approvals" });
  const effort = useQuery({ queryKey: key("effort", range.from, range.to, range.approvedOnly), queryFn: () => api.effort(institutionId, range.from, range.to, range.approvedOnly), enabled: tab === "effort" });
  const rates = useQuery({ queryKey: key("rates"), queryFn: () => api.rates(institutionId), enabled: tab === "rates" });
  const settings = useQuery({ queryKey: key("settings"), queryFn: () => api.settings(institutionId), enabled: tab === "settings" });
  const shown = draft ?? settings.data ?? null;

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-time-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );

  const sponsored = form.kind === "sponsored_research";
  const createInput = (): TimeProjectInput => ({
    code: form.code.trim(),
    name: form.name.trim(),
    kind: form.kind,
    fundingAgency: blank(form.fundingAgency),
    sanctionNumber: blank(form.sanctionNumber),
    sanctionedAmount: blank(form.sanctionedAmount),
    manpowerBudget: blank(form.manpowerBudget),
    startDate: blank(form.startDate),
    endDate: blank(form.endDate),
    piStaffId: blank(form.piStaffId),
    costCentre: blank(form.costCentre),
    budgetKind: blank(form.budgetHours) ? "hours" : null,
    budgetPeriod: blank(form.budgetHours) ? "total" : null,
    budgetValue: blank(form.budgetHours),
    billableDefault: form.kind !== "committee",
    openToAll: form.openToAll,
    members,
  });

  return (
    <BentoTile span={12} title="Time tracking" data-slice="5C" data-time-tracking action={tabs}>
      {tab === "projects" && (
        <>
          <DataTable caption="Projects, grants and committees" rows={projects.data ?? []} rowKey={(p) => p.id}
            empty={<EmptyState icon={Timer} title="No projects yet">Add a sponsored grant, an internal project or a committee below.</EmptyState>}
            columns={[
              { key: "c", header: "Code", render: (p) => <span data-project-code={p.code}>{p.code}</span> },
              { key: "n", header: "Project", render: (p) => (p.fundingAgency ? `${p.name} · ${p.fundingAgency}` : p.name) },
              { key: "k", header: "Kind", render: (p) => KIND_LABEL[p.kind] },
              { key: "pi", header: "PI", render: (p) => p.piName ?? "—" },
              { key: "m", header: "Team", render: (p) => plural(p.members.length, "member") },
              { key: "per", header: "Period", render: (p) => (p.startDate && p.endDate ? `${formatDate(p.startDate)} – ${formatDate(p.endDate)}` : "—") },
              { key: "s", header: "Sanctioned", render: (p) => formatINR(p.sanctionedAmount) },
              { key: "h", header: "Logged", render: (p) => human(p.loggedSeconds) },
              { key: "b", header: "Budget", render: (p) => {
                if (!p.budgetProgress) return "—";
                const b = budgetBar(p.budgetProgress.ratio);
                return <ProgressBar value={b.pct} kind={b.kind} label={`${b.pct}% of budget`} />;
              } },
              { key: "st", header: "Status", render: (p) => <Badge kind={p.status === "active" ? "done" : "info"}>{p.status === "active" ? "Active" : p.status === "closed" ? "Closed" : "Archived"}</Badge> },
            ]} />
          <FormGrid columns={3}>
            <TextField label="Code" name="timeProjectCode" value={form.code} placeholder="SERB-CRG/2026/014" onChange={(e) => setForm({ ...form, code: e.target.value })} />
            <TextField label="Name" name="timeProjectName" value={form.name} onChange={(e) => setForm({ ...form, name: e.target.value })} />
            <FormSelect label="Kind" name="timeProjectKind" value={form.kind} options={KIND_OPTIONS} onChange={(e) => setForm({ ...form, kind: e.target.value as ProjectKind })} />
            {sponsored && <FormSelect label="Funding agency" name="timeProjectAgency" value={form.fundingAgency} options={AGENCY_OPTIONS} onChange={(e) => setForm({ ...form, fundingAgency: e.target.value })} />}
            {sponsored && <TextField label="Sanction order no." value={form.sanctionNumber} onChange={(e) => setForm({ ...form, sanctionNumber: e.target.value })} />}
            {sponsored && <TextField label="Sanctioned amount (₹)" inputMode="decimal" value={form.sanctionedAmount} onChange={(e) => setForm({ ...form, sanctionedAmount: e.target.value })} />}
            {sponsored && <TextField label="Manpower head (₹)" inputMode="decimal" value={form.manpowerBudget} onChange={(e) => setForm({ ...form, manpowerBudget: e.target.value })} />}
            <TextField label="Starts" type="date" value={form.startDate} onChange={(e) => setForm({ ...form, startDate: e.target.value })} />
            <TextField label="Ends" type="date" value={form.endDate} onChange={(e) => setForm({ ...form, endDate: e.target.value })} />
            <PersonPicker institutionId={institutionId} kind="staff" label="Principal investigator / chair" name="timeProjectPi" value={form.piStaffId} onChange={(id) => setForm({ ...form, piStaffId: id })} />
            <TextField label="Cost centre" value={form.costCentre} placeholder="RES-SERB-14" onChange={(e) => setForm({ ...form, costCentre: e.target.value })} />
            <TextField label="Hours budget (total)" inputMode="decimal" value={form.budgetHours} onChange={(e) => setForm({ ...form, budgetHours: e.target.value })} />
          </FormGrid>
          <Checkbox label="Anyone may log time (committees, institution-wide work)" checked={form.openToAll} onChange={(e) => setForm({ ...form, openToAll: e.target.checked })} />
          <FormGrid columns={3}>
            <PersonPicker institutionId={institutionId} kind="staff" label="Add a member" name="timeProjectMember" value={pick.staffId} onChange={(id) => setPick({ ...pick, staffId: id })} />
            <FormSelect label="Role" name="timeProjectRole" value={pick.role} options={ROLE_OPTIONS} onChange={(e) => setPick({ ...pick, role: e.target.value as MemberRole })} />
            <Button size="sm" variant="ghost" data-action="add-member" disabled={!pick.staffId || members.some((m) => m.staffId === pick.staffId)} onClick={() => {
              setMembers([...members, pick]);
              setPick({ staffId: "", role: "jrf" });
            }}>Add member</Button>
          </FormGrid>
          {members.length > 0 && <KeyValue label="Members" value={members.map((m) => ROLE_LABEL[m.role]).join(", ")} />}
          <Button size="sm" data-action="create-time-project" disabled={busy || !form.code.trim() || !form.name.trim() || (sponsored && (!form.startDate || !form.endDate))} onClick={() => void run(async () => {
            await api.createProject(institutionId, createInput());
            setForm(EMPTY_FORM);
            setMembers([]);
          }, "Project added")}>Add project</Button>
        </>
      )}

      {tab === "approvals" && (
        <DataTable caption="Timesheets awaiting approval" rows={queue.data ?? []} rowKey={(s) => s.id}
          empty={<EmptyState icon={Timer} title="Nothing to approve">Submitted weeks appear here until the PI or manager decides.</EmptyState>}
          columns={[
            { key: "w", header: "Week", render: (s) => `${formatDate(s.weekStart)} – ${formatDate(s.weekEnd)}` },
            { key: "n", header: "Staff", render: (s) => s.staffName },
            { key: "p", header: "Project", render: (s) => s.projectCode },
            { key: "h", header: "Time", render: (s) => human(s.totalSeconds) },
            { key: "s", header: "Status", render: (s) => <Badge kind={statusKind(s.status)}>{STATUS_LABEL[s.status]}</Badge> },
            { key: "a", header: "", render: (s) => (
              <span className="flex gap-1">
                <Button size="sm" data-action="approve-timesheet" disabled={busy} onClick={() => void run(() => api.decide(s.id, s.version, true), "Week approved")}>Approve</Button>
                <Button size="sm" variant="ghost" data-action="return-timesheet" disabled={busy} onClick={() => void run(() => api.decide(s.id, s.version, false, "Please correct and resubmit"), "Week returned")}>Return</Button>
              </span>
            ) },
          ]} />
      )}

      {tab === "effort" && (
        <>
          <FormGrid columns={3}>
            <TextField label="From month" type="month" value={range.from} onChange={(e) => setRange({ ...range, from: e.target.value })} />
            <TextField label="To month" type="month" value={range.to} onChange={(e) => setRange({ ...range, to: e.target.value })} />
            <Checkbox label="Approved weeks only" checked={range.approvedOnly} onChange={(e) => setRange({ ...range, approvedOnly: e.target.checked })} />
          </FormGrid>
          <DataTable caption="Utilisation by project" rows={effort.data?.projects ?? []} rowKey={(p) => p.projectId}
            empty={<EmptyState icon={Timer} title="No time in this range" compact>Pick another range.</EmptyState>}
            columns={[
              { key: "c", header: "Project", render: (p) => (p.fundingAgency ? `${p.code} · ${p.fundingAgency}` : p.code) },
              { key: "h", header: "Hours", render: (p) => p.hours.toLocaleString("en-IN") },
              { key: "pm", header: "Person-months", render: (p) => p.personMonths.toFixed(2) },
              { key: "cost", header: "Cost", render: (p) => formatINR(p.cost) },
              { key: "u", header: "Of manpower head", render: (p) => (p.utilisationPercent == null ? "—" : `${p.utilisationPercent}%`) },
            ]} />
          <DataTable caption="Effort by staff and month" rows={effort.data?.rows ?? []} rowKey={(r) => `${r.projectId}:${r.staffId}:${r.month}`}
            columns={[
              { key: "m", header: "Month", render: (r) => monthLabel(r.month) },
              { key: "s", header: "Staff", render: (r) => r.staffName },
              { key: "p", header: "Project", render: (r) => r.projectCode },
              { key: "h", header: "Hours", render: (r) => r.hours },
              { key: "e", header: "Effort", render: (r) => `${r.percent}%` },
              { key: "pm", header: "Person-months", render: (r) => r.personMonths.toFixed(3) },
              { key: "c", header: "Cost", render: (r) => formatINR(r.cost) },
            ]} />
          <a className="sb-link" data-action="effort-csv" href={api.effortCsvUrl(institutionId, range.from, range.to)} download>Download effort report (CSV)</a>
          {" · "}
          <a className="sb-link" data-action="entries-csv" href={api.entriesCsvUrl(institutionId, `${range.from}-01`, `${range.to}-28`)} download>All entries (CSV)</a>
        </>
      )}

      {tab === "rates" && (
        <>
          <DataTable caption="Hourly cost rates" rows={rates.data ?? []} rowKey={(r) => r.staffId}
            empty={<EmptyState icon={Timer} title="No staff">Active staff appear here.</EmptyState>}
            columns={[
              { key: "n", header: "Staff", render: (r) => r.name },
              { key: "o", header: "Own rate", render: (r) => (r.periods.length ? r.periods.map((p) => `${formatINR(p.rate)}${p.from ? ` from ${formatDate(p.from)}` : ""}`).join(", ") : "—") },
              { key: "p", header: "From salary", render: (r) => formatINR(r.payrollHourly) },
              { key: "e", header: "In use today", render: (r) => `${formatINR(r.effectiveToday)}/h` },
              { key: "s", header: "Source", render: (r) => ({ staff: "Own rate", payroll: "Payroll", default: "Default", none: "None" })[r.source] },
            ]} />
          <FormGrid columns={3}>
            <PersonPicker institutionId={institutionId} kind="staff" label="Staff" name="timeRateStaff" value={rateDraft.staffId} onChange={(id) => setRateDraft({ ...rateDraft, staffId: id })} />
            <TextField label="Rate (₹ per hour)" inputMode="decimal" value={rateDraft.rate} onChange={(e) => setRateDraft({ ...rateDraft, rate: e.target.value })} />
            <TextField label="From (leave blank to edit the current rate)" type="date" value={rateDraft.from} onChange={(e) => setRateDraft({ ...rateDraft, from: e.target.value })} />
          </FormGrid>
          <Button size="sm" data-action="save-time-rate" disabled={busy || !rateDraft.staffId} onClick={() => void run(async () => {
            await api.setRate(rateDraft.staffId, rateDraft.rate.trim() === "" ? null : Number(rateDraft.rate), blank(rateDraft.from));
            setRateDraft({ staffId: "", rate: "", from: "" });
          }, "Rate saved")}>Save rate</Button>
        </>
      )}

      {tab === "settings" && shown && (
        <>
          <FormGrid columns={3}>
            <FormSelect label="Weeks start on" value={String(shown.weekStart)} options={[{ value: "1", label: "Monday" }, { value: "0", label: "Sunday" }]} onChange={(e) => setDraft({ ...shown, weekStart: Number(e.target.value) })} />
            <FormSelect label="Round chargeable time" value={String(shown.roundingMinutes)} options={ROUNDING_OPTIONS} onChange={(e) => setDraft({ ...shown, roundingMinutes: Number(e.target.value) })} />
            <TextField label="Working hours in a month" inputMode="decimal" value={String(shown.standardMonthlyHours)} onChange={(e) => setDraft({ ...shown, standardMonthlyHours: Number(e.target.value) })} />
            <TextField label="Default rate (₹ per hour)" inputMode="decimal" value={typeof shown.defaultHourlyRate === "number" ? String(shown.defaultHourlyRate) : ""} onChange={(e) => setDraft({ ...shown, defaultHourlyRate: e.target.value.trim() === "" ? null : Number(e.target.value) })} />
            <TextField label="Trim a forgotten timer at (hours)" inputMode="decimal" value={String(shown.timerCapHours)} onChange={(e) => setDraft({ ...shown, timerCapHours: Number(e.target.value) })} />
          </FormGrid>
          <Button size="sm" data-action="save-time-settings" disabled={busy || !draft} onClick={() => void run(async () => {
            await api.saveSettings(institutionId, draft!);
            setDraft(null);
          }, "Settings saved")}>Save settings</Button>
        </>
      )}
      {dialog}
    </BentoTile>
  );
}
