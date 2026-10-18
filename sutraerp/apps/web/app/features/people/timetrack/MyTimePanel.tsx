/**
 * ERP_EXTENSIONS 5C: My time (My HR → Workload). Timer, the week per project and day, add an
 * entry by duration (`1h30`, `1.5`, `90m`) or start / end, submit a project's week, weeks I
 * approve as PI / manager, CSV import (Toggl / Clockify / Harvest) and export.
 */
import { Timer } from "lucide-react";
import { Badge, BentoTile, Button, Checkbox, DataTable, EmptyState, FileField, formatDate, formatDateShort, FormGrid, FormSelect, KeyValue, TextField } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useCommand } from "../../institution/hooks";
import type { TimeApi } from "./api";
import { defaultTimeApi } from "./TimeTrackingTile";
import { addDaysIso, clock, human, parseDuration, STATUS_LABEL, statusKind } from "./timetrack";
import type { ImportResult } from "./types";

const WEEKDAY = (iso: string) => new Date(`${iso}T00:00:00Z`).toLocaleDateString("en-IN", { weekday: "short", timeZone: "UTC" });

export function MyTimePanel({ api = defaultTimeApi() }: { api?: TimeApi }) {
  const { run, busy, dialog } = useCommand();
  const [week, setWeek] = useState<string | undefined>(undefined);
  const [form, setForm] = useState({ projectId: "", date: "", duration: "", start: "", end: "", task: "", billable: true });
  const [timerProject, setTimerProject] = useState("");
  const [csv, setCsv] = useState<{ name: string; text: string } | null>(null);
  const [preview, setPreview] = useState<ImportResult | null>(null);
  // Prefix `institution`: `useCommand` refreshes every `institution` query after a command.
  const mine = useQuery({ queryKey: ["institution", "time", "me", week ?? "this"], queryFn: () => api.myWeek(week), retry: false });
  const approvals = useQuery({ queryKey: ["institution", "time", "me", "approvals"], queryFn: () => api.myApprovals(), retry: false });
  const w = mine.data;

  if (mine.isError) {
    return (
      <BentoTile span={12} title="My time" data-slice="5C" data-my-time>
        <EmptyState icon={Timer} title="Time tracking isn’t available">Your login is not linked to a staff record yet.</EmptyState>
      </BentoTile>
    );
  }
  const projectOptions = [{ value: "", label: "Choose a project…" }, ...(w?.projects ?? []).map((p) => ({ value: p.projectId, label: p.fundingAgency ? `${p.code} · ${p.fundingAgency}` : p.code }))];
  const parsed = form.duration.trim() ? parseDuration(form.duration) : null;
  const date = form.date || w?.today || "";

  return (
    <>
      <BentoTile span={12} title="My time" data-slice="5C" data-my-time>
        {w?.timer ? (
          <div className="flex gap-2 items-center" data-timer-running>
            <KeyValue label="Timer" value={`${w.timer.projectCode}${w.timer.task ? ` · ${w.timer.task}` : ""} — running ${w.timer.elapsed}`} />
            <Button size="sm" data-action="stop-timer" disabled={busy} onClick={() => void run(() => api.stopTimer(), (e) => `Recorded ${human(e.seconds)}`)}>Stop</Button>
            <Button size="sm" variant="ghost" data-action="discard-timer" disabled={busy} onClick={() => void run(() => api.discardTimer(), "Timer discarded")}>Discard</Button>
          </div>
        ) : (
          <FormGrid columns={3}>
            <FormSelect label="Start a timer on" name="timerProject" value={timerProject} options={projectOptions} onChange={(e) => setTimerProject(e.target.value)} />
            <Button size="sm" data-action="start-timer" disabled={busy || !timerProject} onClick={() => void run(() => api.startTimer(timerProject), "Timer started")}>Start timer</Button>
          </FormGrid>
        )}

        {w && (
          <>
            <div className="flex gap-2 items-center">
              <Button size="sm" variant="ghost" data-action="prev-week" onClick={() => setWeek(addDaysIso(w.weekStart, -7))}>Previous week</Button>
              <KeyValue label="Week" value={`${formatDateShort(w.weekStart)} – ${formatDate(addDaysIso(w.weekStart, 6))} · ${w.weekTotal}`} />
              <Button size="sm" variant="ghost" data-action="next-week" onClick={() => setWeek(addDaysIso(w.weekStart, 7))}>Next week</Button>
            </div>
            <DataTable caption="This week by project" rows={w.projects} rowKey={(p) => p.projectId}
              empty={<EmptyState icon={Timer} title="No projects">Ask the research office to add you to a project or grant.</EmptyState>}
              columns={[
                { key: "p", header: "Project", render: (p) => <span data-week-project={p.code}>{p.code}</span> },
                ...w.days.map((d, i) => ({ key: `d${i}`, header: `${WEEKDAY(d)} ${d.slice(8)}`, render: (p: (typeof w.projects)[number]) => (p.daySeconds[i] ? clock(p.daySeconds[i]!) : "") })),
                { key: "t", header: "Total", render: (p) => clock(p.seconds) },
                { key: "s", header: "Timesheet", render: (p) => (p.timesheet ? <Badge kind={statusKind(p.timesheet.status)}>{STATUS_LABEL[p.timesheet.status]}</Badge> : "—") },
                { key: "a", header: "", render: (p) => {
                  const s = p.timesheet?.status;
                  if (s === "submitted") return <Button size="sm" variant="ghost" data-action="withdraw-week" disabled={busy} onClick={() => void run(() => api.withdraw(p.timesheet!.id, p.timesheet!.version), "Week withdrawn")}>Withdraw</Button>;
                  if (s === "approved" || !p.seconds) return null;
                  return <Button size="sm" data-action="submit-week" disabled={busy} onClick={() => void run(() => api.submitWeek(p.projectId, w.weekStart), "Week submitted for approval")}>Submit week</Button>;
                } },
              ]} />
            {w.projects.filter((p) => p.timesheet?.status === "rejected" && p.timesheet.decisionNote).map((p) => (
              <p key={p.projectId} role="alert">{p.code} was returned: {p.timesheet!.decisionNote}</p>
            ))}
            <DataTable caption="Entries" rows={w.entries} rowKey={(e) => e.id}
              empty={<EmptyState icon={Timer} title="No time this week" compact>Start the timer or add an entry below.</EmptyState>}
              columns={[
                { key: "d", header: "Day", render: (e) => `${WEEKDAY(e.date)} ${formatDateShort(e.date)}` },
                { key: "p", header: "Project", render: (e) => e.projectCode },
                { key: "w", header: "When", render: (e) => (e.start && e.end ? `${e.start}–${e.end}${e.endsNextDay ? " +1" : ""}` : "—") },
                { key: "h", header: "Time", render: (e) => e.duration },
                { key: "t", header: "Task", render: (e) => e.task || "—" },
                { key: "c", header: "Chargeable", render: (e) => (e.billable ? "Yes" : "No") },
                { key: "a", header: "", render: (e) => (e.locked ? <Badge kind="info">Locked</Badge> : (
                  <Button size="sm" variant="ghost" data-action="delete-entry" disabled={busy} onClick={() => void run(() => api.deleteEntry(e.id, e.version), "Entry removed")}>Remove</Button>
                )) },
              ]} />
            <FormGrid columns={3}>
              <FormSelect label="Project" name="entryProject" value={form.projectId} options={projectOptions} onChange={(e) => {
                const p = w.projects.find((x) => x.projectId === e.target.value);
                setForm({ ...form, projectId: e.target.value, billable: p?.billableDefault ?? true });
              }} />
              <TextField label="Date" type="date" name="entryDate" value={date} onChange={(e) => setForm({ ...form, date: e.target.value })} />
              <TextField label="Duration" name="entryDuration" value={form.duration} placeholder="1h30, 1.5 or 90m" hint={form.duration.trim() ? (parsed == null ? "Not a duration" : human(parsed)) : "Or give a start and an end"} onChange={(e) => setForm({ ...form, duration: e.target.value })} />
              <TextField label="Start" type="time" name="entryStart" value={form.start} onChange={(e) => setForm({ ...form, start: e.target.value })} />
              <TextField label="End (earlier = next day)" type="time" name="entryEnd" value={form.end} onChange={(e) => setForm({ ...form, end: e.target.value })} />
              <TextField label="Task" name="entryTask" value={form.task} placeholder="Field survey, report writing…" onChange={(e) => setForm({ ...form, task: e.target.value })} />
            </FormGrid>
            <Checkbox label="Chargeable to the project" checked={form.billable} onChange={(e) => setForm({ ...form, billable: e.target.checked })} />
            <Button size="sm" data-action="add-entry" disabled={busy || !form.projectId || !date || (!form.duration.trim() && !(form.start && form.end))} onClick={() => void run(async () => {
              await api.addEntry({ projectId: form.projectId, date, duration: form.duration.trim() || null, start: form.start || null, end: form.end || null, task: form.task.trim() || null, billable: form.billable });
              setForm({ ...form, duration: "", start: "", end: "", task: "" });
            }, "Time logged")}>Add entry</Button>
          </>
        )}
        {dialog}
      </BentoTile>

      <BentoTile span={6} title="Weeks to approve" data-slice="5C" data-time-approvals>
        <DataTable caption="Submitted to me" rows={approvals.data ?? []} rowKey={(s) => s.id}
          empty={<EmptyState icon={Timer} title="Nothing waiting" compact>Weeks of projects you lead, and of staff who report to you, appear here.</EmptyState>}
          columns={[
            { key: "n", header: "Staff", render: (s) => s.staffName },
            { key: "p", header: "Project", render: (s) => s.projectCode },
            { key: "w", header: "Week", render: (s) => formatDateShort(s.weekStart) },
            { key: "h", header: "Time", render: (s) => human(s.totalSeconds) },
            { key: "a", header: "", render: (s) => (
              <span className="flex gap-1">
                <Button size="sm" data-action="pi-approve" disabled={busy} onClick={() => void run(() => api.decide(s.id, s.version, true), "Week approved")}>Approve</Button>
                <Button size="sm" variant="ghost" data-action="pi-return" disabled={busy} onClick={() => void run(() => api.decide(s.id, s.version, false, "Please correct and resubmit"), "Week returned")}>Return</Button>
              </span>
            ) },
          ]} />
      </BentoTile>

      <BentoTile span={6} title="Import / export" data-slice="5C" data-time-import>
        <FileField label="CSV from Toggl, Clockify or Harvest" accept=".csv,text/csv" name="timeCsv" fileName={csv?.name} onFile={(f) => {
          setPreview(null);
          if (!f) return setCsv(null);
          void f.text().then((text) => setCsv({ name: f.name, text }));
        }} />
        <div className="flex gap-2">
          <Button size="sm" variant="ghost" data-action="preview-import" disabled={busy || !csv} onClick={() => void run(async () => setPreview(await api.importCsv(csv!.text, true)))}>Preview</Button>
          <Button size="sm" data-action="run-import" disabled={busy || !preview || preview.added === 0} onClick={() => void run(async () => {
            const r = await api.importCsv(csv!.text, false);
            setPreview(null);
            setCsv(null);
            return r;
          }, (r) => `Imported ${r?.added ?? 0} entries`)}>Import</Button>
        </div>
        {preview && (
          <>
            <KeyValue label={`${preview.source} file`} value={`${preview.added} to add · ${preview.skipped} already logged · ${preview.invalid.length} skipped`} />
            {preview.invalid.slice(0, 8).map((x) => <p key={x.line}>Line {x.line}: {x.reason}</p>)}
          </>
        )}
        {w && <a className="sb-link" data-action="my-time-csv" href={api.myCsvUrl(addDaysIso(w.weekStart, -28), addDaysIso(w.weekStart, 6))} download>Download my last five weeks (CSV)</a>}
      </BentoTile>
    </>
  );
}
