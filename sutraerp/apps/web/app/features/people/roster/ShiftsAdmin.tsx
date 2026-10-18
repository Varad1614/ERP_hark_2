/**
 * ERP_EXTENSIONS 5B: the manager side of shifts (ported from Everything's shifts module, MIT).
 * Approve or reject swaps agreed by colleagues, approve / return / lock timesheets, send a
 * period to payroll (and download its CSV), roster reports (hours by department, overtime,
 * coverage, swaps) and the timesheet and pay settings. Rendered inside `DutyRosterTile`.
 */
import { CalendarClock } from "lucide-react";
import { Badge, Button, DataTable, EmptyState, formatDate, formatDateShort, formatINR, FormGrid, FormSelect, KeyValue, plural, TextField, type BadgeKind } from "@sutra/ui";
import { useState } from "react";
import { REASON } from "../../institution/forms";
import { useAsk } from "../ui";
import { defaultShiftsApi, payrollExportCsvUrl, reportsCsvUrl, timesheetCsvUrl, useRosterReport, useShiftCommands, useSwaps, useTimesheets, useTimesheetSettings, type ShiftsApi } from "./shifts-api";
import { duration, swapActions, swapStatusLabel, swapTone, SWAP_KIND_LABELS, timesheetActions, timesheetStatusLabel, timesheetTone, totalsSummary, sumTotals, type Tone } from "./shifts";
import type { RosterSwap, RosterTimesheet, RosterTimesheetSettings, SwapShiftSnapshot, SwapStatus, TimesheetStatus } from "./shifts-types";

export type ShiftsTab = "swaps" | "timesheets" | "reports" | "settings";
const TABS: { value: ShiftsTab; label: string }[] = [
  { value: "swaps", label: "Swaps" },
  { value: "timesheets", label: "Timesheets" },
  { value: "reports", label: "Reports" },
  { value: "settings", label: "Pay rules" },
];

const TONE_KIND: Record<Tone, BadgeKind> = { neutral: "neutral", warning: "attention", success: "done", danger: "review" };

const today = () => new Date().toISOString().slice(0, 10);
const monthStart = (iso: string) => `${iso.slice(0, 7)}-01`;
function monthEnd(iso: string): string {
  const [y, m] = iso.slice(0, 7).split("-").map(Number);
  return new Date(Date.UTC(y, m, 0)).toISOString().slice(0, 10);
}

const num = (s: string): number | null => (s.trim() === "" ? null : Number(s));
const shiftText = (s: SwapShiftSnapshot | null | undefined) => (s ? `${formatDateShort(s.date)} ${s.start}–${s.end} · ${s.role}` : "—");

interface CoverageRole {
  role: string;
  days: number;
  missing: number;
}
interface Coverage {
  gapDays: number;
  missing: number;
  byRole: CoverageRole[];
}
/** `report.coverage` of Everything's free-form report (`reports.rs` `Coverage`). */
function coverageOf(report: Record<string, unknown> | undefined): Coverage | null {
  const c = report?.coverage as Partial<Coverage> | undefined;
  if (!c || typeof c !== "object") return null;
  return { gapDays: Number(c.gapDays ?? 0), missing: Number(c.missing ?? 0), byRole: Array.isArray(c.byRole) ? c.byRole : [] };
}

const SWAP_FILTER: { value: string; label: string }[] = [
  { value: "awaiting_manager", label: "Awaiting manager" },
  { value: "", label: "All" },
  { value: "awaiting_peer", label: "Awaiting colleague" },
  { value: "approved", label: "Approved" },
  { value: "declined", label: "Declined" },
  { value: "cancelled", label: "Withdrawn" },
];

const TIMESHEET_FILTER: { value: string; label: string }[] = [
  { value: "", label: "All" },
  { value: "submitted", label: "Submitted" },
  { value: "returned", label: "Returned" },
  { value: "approved", label: "Approved" },
  { value: "locked", label: "Locked" },
];

export function ShiftsAdmin({ institutionId, api = defaultShiftsApi(), initialTab = "swaps" }: { institutionId: string; api?: ShiftsApi; initialTab?: ShiftsTab }) {
  const cmd = useShiftCommands(api);
  const { busy, dialog } = cmd;
  const { ask, prompt } = useAsk("Roster");
  const [tab, setTab] = useState<ShiftsTab>(initialTab);
  const [swapStatus, setSwapStatus] = useState<string>("awaiting_manager");
  const [tsStatus, setTsStatus] = useState<string>("");
  const [range, setRange] = useState(() => ({ from: monthStart(today()), to: monthEnd(today()) }));
  const [draft, setDraft] = useState<RosterTimesheetSettings | null>(null);

  const swaps = useSwaps(institutionId, (swapStatus || undefined) as SwapStatus | undefined, api, tab === "swaps");
  const timesheets = useTimesheets(institutionId, { from: range.from, to: range.to, status: (tsStatus || undefined) as TimesheetStatus | undefined }, api, tab === "timesheets");
  const report = useRosterReport(institutionId, range, api, tab === "reports");
  const settings = useTimesheetSettings(institutionId, api, tab === "settings");
  const shown = draft ?? settings.data ?? null;
  const viewer = { manager: true, payroll: true };

  const rejectSwap = (s: RosterSwap) =>
    ask({
      title: "Reject swap",
      name: "reject-swap",
      fields: [{ ...REASON, required: false }],
      submitLabel: "Reject swap",
      onSubmit: async (v) => {
        await cmd.decideSwap(s, { decision: "reject", reason: v.reason?.trim() || null });
      },
    });
  const rejectTimesheet = (t: RosterTimesheet) =>
    ask({
      title: `Return ${t.staffName}'s timesheet`,
      name: "reject-timesheet",
      fields: [REASON],
      submitLabel: "Reject timesheet",
      onSubmit: async (v) => {
        await cmd.decideTimesheet(t, { decision: "reject", reason: v.reason.trim() });
      },
    });

  const period = (
    <div className="flex flex-wrap gap-2 items-end" data-shifts-period="">
      <TextField label="From" type="date" value={range.from} onChange={(e) => setRange({ ...range, from: e.target.value })} />
      <TextField label="To" type="date" value={range.to} onChange={(e) => setRange({ ...range, to: e.target.value })} />
      <Button size="sm" variant="ghost" data-action="this-month" onClick={() => setRange({ from: monthStart(today()), to: monthEnd(today()) })}>This month</Button>
    </div>
  );

  const setNum = (k: keyof RosterTimesheetSettings, required: boolean) => (e: { target: { value: string } }) => {
    if (!shown) return;
    const v = num(e.target.value);
    setDraft({ ...shown, [k]: required ? (v ?? 0) : v });
  };

  return (
    <div className="flex flex-col gap-2" data-shifts-admin="" data-slice="5B">
      <div className="flex gap-1" role="tablist">
        {TABS.map((t) => (
          <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-shifts-tab={t.value} onClick={() => setTab(t.value)}>
            {t.label}
          </Button>
        ))}
      </div>

      {tab === "swaps" && (
        <>
          <div className="flex gap-2 items-end">
            <FormSelect label="Show" name="swap-status" value={swapStatus} options={SWAP_FILTER} onChange={(e) => setSwapStatus(e.target.value)} />
          </div>
          {swaps.data && (
            <DataTable caption="Shift swaps" rows={swaps.data} rowKey={(s) => s.id}
              empty={<EmptyState icon={CalendarClock} title="No swap requests" compact>Requests agreed by a colleague wait here for approval.</EmptyState>}
              columns={[
                { key: "k", header: "Kind", render: (s) => SWAP_KIND_LABELS[s.kind] ?? s.kind },
                { key: "f", header: "From", render: (s) => s.fromStaffName ?? "—" },
                { key: "t", header: "To", render: (s) => s.toStaffName ?? (s.kind === "cover" ? "Anyone" : "—") },
                { key: "sh", header: "Shift", render: (s) => shiftText(s.snapshot?.shift) },
                { key: "o", header: "In return", render: (s) => shiftText(s.snapshot?.other) },
                { key: "r", header: "Reason", render: (s) => s.reason || "—", muted: true },
                { key: "w", header: "Warnings", render: (s) => s.warnings.join(" · ") || "—", muted: true },
                { key: "s", header: "Status", render: (s) => <Badge kind={TONE_KIND[swapTone(s.status)]}>{swapStatusLabel(s.status)}</Badge> },
                { key: "a", header: "", render: (s) => (swapActions(s, viewer).includes("decide") ? (
                  <span className="flex gap-1">
                    <Button size="sm" data-action="approve-swap" disabled={busy} onClick={() => void cmd.decideSwap(s, { decision: "approve" })}>Approve swap</Button>
                    <Button size="sm" variant="ghost" data-action="reject-swap" disabled={busy} onClick={() => rejectSwap(s)}>Reject swap</Button>
                  </span>
                ) : null) },
              ]} />
          )}
        </>
      )}

      {tab === "timesheets" && (
        <>
          {period}
          <div className="flex flex-wrap gap-2 items-end">
            <FormSelect label="Status" name="timesheet-status" value={tsStatus} options={TIMESHEET_FILTER} onChange={(e) => setTsStatus(e.target.value)} />
            <Button size="sm" data-action="payroll-export" disabled={busy || !range.from || !range.to} onClick={() => void cmd.payrollExport(institutionId, range)}>Send to payroll</Button>
            <Button size="sm" variant="ghost" data-action="download-payroll-csv" disabled={busy || !range.from || !range.to} onClick={() => void cmd.download(payrollExportCsvUrl(institutionId, range), `payroll-export-${range.from}-${range.to}.csv`)}>Download payroll CSV</Button>
            <Button size="sm" variant="ghost" data-action="download-timesheet-csv" disabled={busy} onClick={() => void cmd.download(timesheetCsvUrl(institutionId, range), `timesheets-${range.from}-${range.to}.csv`)}>Timesheet CSV</Button>
          </div>
          {timesheets.data && (
            <>
              {timesheets.data.length > 0 && <KeyValue label={`${plural(timesheets.data.length, "timesheet")} in this period`} value={totalsSummary(sumTotals(timesheets.data.map((t) => t.totals)))} />}
              <DataTable caption="Timesheets" rows={timesheets.data} rowKey={(t) => t.id}
                empty={<EmptyState icon={CalendarClock} title="No timesheets" compact>Submitted timesheets for this period appear here.</EmptyState>}
                columns={[
                  { key: "n", header: "Staff", render: (t) => (t.staffNumber ? `${t.staffName} · ${t.staffNumber}` : t.staffName) },
                  { key: "p", header: "Period", render: (t) => `${formatDateShort(t.periodFrom)} – ${formatDate(t.periodTo)}` },
                  { key: "t", header: "Totals", render: (t) => totalsSummary(t.totals) },
                  { key: "s", header: "Status", render: (t) => (
                    <span className="flex gap-1 items-center">
                      <Badge kind={TONE_KIND[timesheetTone(t.status)]}>{timesheetStatusLabel(t.status)}</Badge>
                      {t.exportedAt && <Badge kind="info">Sent to payroll</Badge>}
                    </span>
                  ) },
                  { key: "a", header: "", render: (t) => {
                    const acts = timesheetActions(t.status, { ...viewer, exported: !!t.exportedAt });
                    return (
                      <span className="flex gap-1">
                        {acts.includes("decide") && (
                          <>
                            <Button size="sm" data-action="approve-timesheet" disabled={busy} onClick={() => void cmd.decideTimesheet(t, { decision: "approve" })}>Approve timesheet</Button>
                            <Button size="sm" variant="ghost" data-action="reject-timesheet" disabled={busy} onClick={() => rejectTimesheet(t)}>Reject timesheet</Button>
                          </>
                        )}
                        {acts.includes("lock") && <Button size="sm" variant="ghost" data-action="lock-timesheet" disabled={busy} onClick={() => void cmd.lockTimesheet(t)}>Lock</Button>}
                      </span>
                    );
                  } },
                ]} />
            </>
          )}
        </>
      )}

      {tab === "reports" && (
        <>
          {period}
          <div className="flex gap-2">
            <Button size="sm" variant="ghost" data-action="download-report-csv" disabled={busy} onClick={() => void cmd.download(reportsCsvUrl(institutionId, { ...range, section: "hours" }), `roster-report-${range.from}-${range.to}.csv`)}>Download report CSV</Button>
          </div>
          {report.data && (() => {
            const r = report.data;
            const cov = coverageOf(r.report);
            return (
              <>
                <KeyValue label="Swaps" value={`${r.swaps.total} total · ${r.swaps.pending} pending · ${r.swaps.approved} approved · ${r.swaps.declined} declined · ${r.swaps.cancelled} withdrawn`} />
                {Object.keys(r.swaps.byKind).length > 0 && (
                  <KeyValue label="By kind" value={Object.entries(r.swaps.byKind).map(([k, n]) => `${SWAP_KIND_LABELS[k as keyof typeof SWAP_KIND_LABELS] ?? k}: ${n}`).join(" · ")} />
                )}
                <p className="text-sm">Hours by department</p>
                <DataTable caption="Hours by department" rows={r.departments} rowKey={(d) => d.department}
                  empty={<EmptyState icon={CalendarClock} title="No shifts in this period" compact />}
                  columns={[
                    { key: "d", header: "Department", render: (d) => d.department || "—" },
                    { key: "p", header: "Staff", render: (d) => d.staff },
                    { key: "s", header: "Shifts", render: (d) => d.shifts },
                    { key: "sc", header: "Scheduled", render: (d) => duration(d.scheduledMinutes) },
                    { key: "w", header: "Worked", render: (d) => duration(d.workedMinutes) },
                    { key: "o", header: "Overtime", render: (d) => duration(d.overtimeMinutes) },
                  ]} />
                <p className="text-sm">Overtime and attendance</p>
                <DataTable caption="Overtime" rows={r.overtime} rowKey={(o) => o.staffId}
                  empty={<EmptyState icon={CalendarClock} title="No overtime" compact />}
                  columns={[
                    { key: "n", header: "Staff", render: (o) => o.name },
                    { key: "d", header: "Department", render: (o) => o.department || "—", muted: true },
                    { key: "w", header: "Worked", render: (o) => duration(o.workedMinutes) },
                    { key: "o", header: "Overtime", render: (o) => duration(o.overtimeMinutes) },
                    { key: "l", header: "Late", render: (o) => duration(o.lateMinutes) },
                    { key: "e", header: "Left early", render: (o) => duration(o.earlyLeaveMinutes) },
                    { key: "m", header: "Missed punches", render: (o) => o.missedPunches },
                    { key: "nh", header: "Night / holiday", render: (o) => `${o.nightShifts} / ${o.holidayShifts}` },
                  ]} />
                <p className="text-sm">Coverage</p>
                {cov && <KeyValue label="Coverage gaps" value={`${plural(cov.gapDays, "day")} short · ${cov.missing} people missing`} />}
                <DataTable caption="Coverage by duty" rows={cov?.byRole ?? []} rowKey={(g) => g.role}
                  empty={<EmptyState icon={CalendarClock} title="Fully covered" compact>Every requirement was met in this period.</EmptyState>}
                  columns={[
                    { key: "r", header: "Duty", render: (g) => g.role },
                    { key: "d", header: "Days short", render: (g) => g.days },
                    { key: "m", header: "People short", render: (g) => g.missing },
                  ]} />
              </>
            );
          })()}
        </>
      )}

      {tab === "settings" && shown && (
        <>
          <FormGrid columns={3}>
            <TextField label="Late grace (minutes)" name="lateGraceMinutes" inputMode="numeric" value={String(shown.lateGraceMinutes)} onChange={setNum("lateGraceMinutes", true)} />
            <TextField label="Early-leave grace (minutes)" name="earlyLeaveGraceMinutes" inputMode="numeric" value={String(shown.earlyLeaveGraceMinutes)} onChange={setNum("earlyLeaveGraceMinutes", true)} />
            <TextField label="Standard hours per day" name="standardHoursPerDay" inputMode="decimal" value={String(shown.standardHoursPerDay)} onChange={setNum("standardHoursPerDay", true)} />
            <TextField label="Overtime after (hours a day)" name="dailyOvertimeHours" inputMode="decimal" value={shown.dailyOvertimeHours?.toString() ?? ""} onChange={setNum("dailyOvertimeHours", false)} />
            <TextField label="Overtime after (hours a week)" name="weeklyOvertimeHours" inputMode="decimal" value={shown.weeklyOvertimeHours?.toString() ?? ""} onChange={setNum("weeklyOvertimeHours", false)} />
            <TextField label="Overtime multiplier" name="overtimeMultiplier" inputMode="decimal" value={String(shown.overtimeMultiplier)} onChange={setNum("overtimeMultiplier", true)} />
            <TextField label="Overtime rate (₹ per hour)" name="overtimeRatePerHour" inputMode="decimal" value={shown.overtimeRatePerHour?.toString() ?? ""} hint={shown.overtimeRatePerHour != null ? formatINR(shown.overtimeRatePerHour) : "Blank: from each salary"} onChange={setNum("overtimeRatePerHour", false)} />
            <TextField label="Night shift allowance (₹)" name="nightShiftAllowance" inputMode="decimal" value={shown.nightShiftAllowance?.toString() ?? ""} onChange={setNum("nightShiftAllowance", false)} />
            <TextField label="Holiday shift allowance (₹)" name="holidayShiftAllowance" inputMode="decimal" value={shown.holidayShiftAllowance?.toString() ?? ""} onChange={setNum("holidayShiftAllowance", false)} />
          </FormGrid>
          <div className="flex gap-2">
            <Button size="sm" data-action="save-timesheet-settings" disabled={busy || !draft} onClick={() => void (async () => {
              try {
                await cmd.saveTimesheetSettings(institutionId, draft!);
                setDraft(null);
              } catch {
                // already toasted (submit rethrows field errors)
              }
            })()}>Save</Button>
            {draft && <Button size="sm" variant="ghost" data-action="reset-timesheet-settings" disabled={busy} onClick={() => setDraft(null)}>Discard changes</Button>}
          </div>
        </>
      )}
      {prompt}
      {dialog}
    </div>
  );
}
