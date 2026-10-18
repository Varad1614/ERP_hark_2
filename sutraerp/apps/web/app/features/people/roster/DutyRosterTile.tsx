/**
 * ERP_EXTENSIONS 3A: duty roster (ported from Everything's shifts scheduling, MIT). A week of
 * shifts with overlap, availability and working-time flags; plan a shift, see who could take an
 * open one, fill the week, publish it; coverage gaps against requirements; settings.
 */
import { CalendarClock } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, formatDate, formatDateShort, FormGrid, FormSelect, KeyValue, plural, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { USES_API } from "../../../lib/api";
import { useCommand } from "../../institution/hooks";
import { createHttpRosterApi, type RosterApi } from "./api";
import { createMockRosterApi } from "./mock";
import { ShiftsAdmin } from "./ShiftsAdmin"; // ERP_EXTENSIONS 5B
import type { ShiftsApi } from "./shifts-api";
import { addDaysIso, flags, hours, mondayOf, timeRange, WEEKDAYS } from "./roster";
import type { Candidate, RosterSettings } from "./types";

let fallback: RosterApi | null = null;
export function defaultRosterApi(): RosterApi {
  fallback ??= USES_API ? createHttpRosterApi() : createMockRosterApi();
  return fallback;
}

type Tab = "week" | "staff" | "settings" | "shifts";
const TABS: { value: Tab; label: string }[] = [
  { value: "week", label: "Week" },
  { value: "staff", label: "Staff hours" },
  { value: "settings", label: "Rules" },
  { value: "shifts", label: "Swaps & timesheets" },
];

const num = (s: string) => (s.trim() === "" ? null : Number(s));

export function DutyRosterTile({ institutionId, api = defaultRosterApi(), shiftsApi }: { institutionId: string; api?: RosterApi; shiftsApi?: ShiftsApi }) {
  const { run, busy, dialog } = useCommand();
  const [tab, setTab] = useState<Tab>("week");
  const [from, setFrom] = useState(() => mondayOf(new Date().toISOString().slice(0, 10)));
  const to = addDaysIso(from, 6);
  const [form, setForm] = useState({ date: from, start: "09:00", end: "17:00", role: "", location: "", staffId: "" });
  const [warnings, setWarnings] = useState<string[]>([]);
  const [picking, setPicking] = useState<{ id: string; items: Candidate[] } | null>(null);
  const [draft, setDraft] = useState<RosterSettings | null>(null);
  // Prefix `institution`: `useCommand` refreshes every `institution` query after a command.
  const key = (...p: string[]) => ["institution", "roster", institutionId, ...p];
  const board = useQuery({ queryKey: key("board", from), queryFn: () => api.board(institutionId, from, to), enabled: tab === "week" || tab === "staff" });
  const settings = useQuery({ queryKey: key("settings"), queryFn: () => api.settings(institutionId), enabled: tab === "settings" });
  const shown = draft ?? settings.data ?? null;
  const staffOptions = [{ value: "", label: "Open (no one yet)" }, ...(board.data?.staff ?? []).filter((s) => s.rosterable).map((s) => ({ value: s.staffId, label: s.name }))];

  const tabs = (
    <div className="flex gap-1" role="tablist">
      {TABS.map((t) => (
        <Button key={t.value} size="sm" variant={tab === t.value ? "primary" : "ghost"} data-roster-tab={t.value} onClick={() => setTab(t.value)}>
          {t.label}
        </Button>
      ))}
    </div>
  );

  return (
    <BentoTile span={12} title="Duty roster" data-slice="3A" data-duty-roster action={tabs}>
      {(tab === "week" || tab === "staff") && (
        <div className="flex gap-2 items-center">
          <Button size="sm" variant="ghost" data-action="prev-week" onClick={() => setFrom(addDaysIso(from, -7))}>Previous week</Button>
          <KeyValue label="Week" value={`${formatDateShort(from)} – ${formatDate(to)}`} />
          <Button size="sm" variant="ghost" data-action="next-week" onClick={() => setFrom(addDaysIso(from, 7))}>Next week</Button>
        </div>
      )}

      {tab === "week" && board.data && (
        <>
          <KeyValue label="This week" value={`${plural(board.data.shifts.length, "shift")} · ${board.data.openShifts} open · ${plural(board.data.gaps.length, "coverage gap")}`} />
          <DataTable caption="Shifts" rows={board.data.shifts} rowKey={(s) => s.id}
            empty={<EmptyState icon={CalendarClock} title="No shifts this week">Plan one below, or copy last week's pattern.</EmptyState>}
            columns={[
              { key: "d", header: "Day", render: (s) => `${WEEKDAYS[new Date(`${s.date}T00:00:00Z`).getUTCDay()]} ${formatDateShort(s.date)}` },
              { key: "t", header: "Time", render: (s) => timeRange(s) },
              { key: "r", header: "Duty", render: (s) => (s.location ? `${s.role} · ${s.location}` : s.role) },
              { key: "w", header: "Who", render: (s) => s.staffName ?? <Badge kind="attention">Open</Badge> },
              { key: "f", header: "Flags", render: (s) => flags(s).join(", ") || "—" },
              { key: "s", header: "Status", render: (s) => <Badge kind={s.status === "published" ? "done" : "info"}>{s.status === "published" ? "Published" : "Draft"}</Badge> },
              { key: "a", header: "", render: (s) => (
                <span className="flex gap-1">
                  {!s.staffId && (
                    <Button size="sm" variant="ghost" data-action="suggest" disabled={busy} onClick={() => void run(async () => setPicking({ id: s.id, items: await api.suggestions(s.id) }))}>Who can take it?</Button>
                  )}
                  <Button size="sm" variant="ghost" data-action="delete-shift" disabled={busy} onClick={() => void run(() => api.deleteShift(s.id, s.version), "Shift removed")}>Remove</Button>
                </span>
              ) },
            ]} />
          {picking && (
            <DataTable caption="Suggested staff" rows={picking.items} rowKey={(c) => c.employeeId}
              empty={<EmptyState icon={CalendarClock} title="Nobody is free" compact>Everyone is off, busy or at their weekly maximum.</EmptyState>}
              columns={[
                { key: "n", header: "Name", render: (c) => c.name },
                { key: "r", header: "Why", render: (c) => c.reasons.join(" · ") },
                { key: "a", header: "", render: (c) => (
                  <Button size="sm" data-action="assign" disabled={busy} onClick={() => void run(async () => {
                    const s = board.data!.shifts.find((x) => x.id === picking.id)!;
                    const r = await api.updateShift(s.id, s.version, { date: s.date, start: s.start, end: s.end, breakMinutes: s.breakMinutes, role: s.role, location: s.location, note: s.note, staffId: c.employeeId });
                    setWarnings(r.warnings);
                    setPicking(null);
                  }, "Assigned")}>Assign</Button>
                ) },
              ]} />
          )}
          {board.data.gaps.length > 0 && (
            <DataTable caption="Coverage gaps" rows={board.data.gaps} rowKey={(g) => `${g.date}:${g.requirementId}`}
              columns={[
                { key: "d", header: "Date", render: (g) => formatDate(g.date) },
                { key: "r", header: "Duty", render: (g) => g.role },
                { key: "m", header: "Short by", render: (g) => `${g.missing} (has ${g.scheduled})` },
              ]} />
          )}
          <FormGrid columns={3}>
            <TextField label="Date" type="date" value={form.date} onChange={(e) => setForm({ ...form, date: e.target.value })} />
            <TextField label="Starts" type="time" value={form.start} onChange={(e) => setForm({ ...form, start: e.target.value })} />
            <TextField label="Ends (earlier = next day)" type="time" value={form.end} onChange={(e) => setForm({ ...form, end: e.target.value })} />
            <TextField label="Duty" value={form.role} placeholder="Hostel night warden" onChange={(e) => setForm({ ...form, role: e.target.value })} />
            <TextField label="Where" value={form.location} onChange={(e) => setForm({ ...form, location: e.target.value })} />
            <FormSelect label="Staff" value={form.staffId} onChange={(e) => setForm({ ...form, staffId: e.target.value })} options={staffOptions} />
          </FormGrid>
          {warnings.map((w) => <p key={w} role="alert">{w}</p>)}
          <div className="flex gap-2">
            <Button size="sm" data-action="add-shift" disabled={busy || !form.role.trim() || form.start === form.end} onClick={() => void run(async () => {
              const r = await api.createShift(institutionId, { date: form.date, start: form.start, end: form.end, role: form.role.trim(), location: form.location.trim() || null, staffId: form.staffId || null });
              setWarnings(r.warnings);
            }, "Shift planned")}>Plan shift</Button>
            <Button size="sm" variant="ghost" data-action="fill" disabled={busy || !board.data.openShifts} onClick={() => void run(async () => {
              const p = await api.fill(institutionId, from, to, true);
              setWarnings(p.nobody ? [`${p.nobody} open shift${p.nobody === 1 ? "" : "s"} had nobody free.`] : []);
            }, "Open shifts filled")}>Fill open shifts</Button>
            <Button size="sm" variant="ghost" data-action="publish" disabled={busy || !board.data.shifts.some((s) => s.status === "draft")} onClick={() => void run(() => api.publish(institutionId, from, to), "Week published")}>Publish week</Button>
          </div>
        </>
      )}

      {tab === "staff" && board.data && (
        <DataTable caption="Hours this week" rows={board.data.staff} rowKey={(s) => s.staffId}
          empty={<EmptyState icon={CalendarClock} title="No staff">Active staff appear here.</EmptyState>}
          columns={[
            { key: "n", header: "Name", render: (s) => s.name },
            { key: "r", header: "Usual duty", render: (s) => s.role || "—" },
            { key: "h", header: "Hours", render: (s) => hours(s.weekMinutes) },
            { key: "l", header: "Target / max", render: (s) => `${s.targetHours ?? "—"} / ${s.maxHours ?? "—"}` },
            { key: "s", header: "", render: (s) => (s.hoursStatus === "ok" ? null : <Badge kind={s.hoursStatus === "over" ? "attention" : "info"}>{s.hoursStatus === "over" ? "Over max" : "Under target"}</Badge>) },
            { key: "w", header: "Limits", render: (s) => s.ruleWarnings.map((w) => w.short).join(", ") || "—" },
          ]} />
      )}

      {tab === "settings" && shown && (
        <>
          <FormGrid columns={3}>
            <TextField label="Minimum rest between shifts (hours)" inputMode="decimal" value={shown.rules.minRestHours?.toString() ?? ""} onChange={(e) => setDraft({ ...shown, rules: { ...shown.rules, minRestHours: num(e.target.value) } })} />
            <TextField label="Maximum hours in a day" inputMode="decimal" value={shown.rules.maxDailyHours?.toString() ?? ""} onChange={(e) => setDraft({ ...shown, rules: { ...shown.rules, maxDailyHours: num(e.target.value) } })} />
            <TextField label="Maximum days in a row" inputMode="numeric" value={shown.rules.maxConsecutiveDays?.toString() ?? ""} onChange={(e) => setDraft({ ...shown, rules: { ...shown.rules, maxConsecutiveDays: num(e.target.value) } })} />
          </FormGrid>
          <DataTable caption="Coverage requirements" rows={shown.requirements} rowKey={(q) => `${q.role}:${q.days.join("")}:${q.start ?? ""}`}
            empty={<EmptyState icon={CalendarClock} title="No requirements" compact>Say how many people each duty needs, and when.</EmptyState>}
            columns={[
              { key: "r", header: "Duty", render: (q) => q.role },
              { key: "d", header: "Days", render: (q) => q.days.map((d) => WEEKDAYS[d]).join(" ") },
              { key: "c", header: "People", render: (q) => q.count },
              { key: "w", header: "Window", render: (q) => (q.start && q.end ? `${q.start}–${q.end}` : "All day") },
            ]} />
          <Button size="sm" data-action="save-roster-settings" disabled={busy || !draft} onClick={() => void run(async () => {
            await api.saveSettings(institutionId, draft!);
            setDraft(null);
          }, "Rules saved")}>Save rules</Button>
        </>
      )}
      {tab === "shifts" && <ShiftsAdmin institutionId={institutionId} api={shiftsApi} />}
      {dialog}
    </BentoTile>
  );
}
