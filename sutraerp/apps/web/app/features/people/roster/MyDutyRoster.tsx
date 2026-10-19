/**
 * ERP_EXTENSIONS 5B: My duty roster (My HR → Attendance). The staff self-service side of the
 * 3A roster: my published upcoming shifts (`GET /me/roster`) with "Request swap" (give away,
 * swap for a colleague's shift, or ask for cover), clock in / out on today's shift, open shifts
 * to claim, swap requests to and from me (accept / decline / withdraw), my timesheet for the
 * period with totals and submit, and the `.ics` calendar download.
 */
import { CalendarClock } from "lucide-react";
import { Badge, BentoTile, Button, DataTable, EmptyState, formatDate, formatDateShort, FormGrid, FormSelect, KeyValue, TextField, type BadgeKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { useWorkforceApi, wkeys } from "../workforce/hooks";
import { timeRange, WEEKDAYS } from "./roster";
import { defaultShiftsApi, myRosterIcsUrl, shiftKeys, useMySwaps, useMyTimesheet, useOpenShifts, useShiftCommands, type ShiftsApi } from "./shifts-api";
import { duration, SWAP_KIND_LABELS, swapActions, swapStatusLabel, swapTone, timesheetActions, timesheetStatusLabel, timesheetTone, totalsSummary, type Tone } from "./shifts";
import type { RosterSwap, SwapRequestInput, SwapShiftSnapshot } from "./shifts-types";
import type { DutyShift } from "./types";

/** `MyRoster` of `GET /api/v1/people/me/roster` (published shifts I hold in the range). */
export interface MyRosterView {
  from: string;
  to: string;
  items: DutyShift[];
  minutes: number;
}

export type FetchMyRoster = () => Promise<MyRosterView>;

/** HTTP read of my published duties (session cookie; problem+json / non-2xx throw). */
export const fetchMyRoster: FetchMyRoster = async () => {
  const res = await fetch(`${import.meta.env.VITE_API_BASE ?? ""}/api/v1/people/me/roster`, { method: "GET", credentials: "include", headers: { Accept: "application/json" } });
  if (!res.ok) throw new Error(`My roster failed (${res.status})`);
  return (await res.json()) as MyRosterView;
};

const TONE_KIND: Record<Tone, BadgeKind> = { neutral: "neutral", warning: "attention", success: "done", danger: "review" };

const localToday = (): string => {
  const d = new Date();
  return `${d.getFullYear()}-${String(d.getMonth() + 1).padStart(2, "0")}-${String(d.getDate()).padStart(2, "0")}`;
};

const day = (iso: string) => `${WEEKDAYS[new Date(`${iso}T00:00:00Z`).getUTCDay()]} ${formatDateShort(iso)}`;
const snapText = (s?: SwapShiftSnapshot | null) => (s ? `${day(s.date)} ${s.start}–${s.end} · ${s.role}` : "—");

type SwapKindChoice = SwapRequestInput["kind"];
const KIND_OPTIONS: { value: SwapKindChoice; label: string }[] = [
  { value: "cover", label: "Ask anyone to cover" },
  { value: "give_away", label: "Give away to a colleague" },
  { value: "swap", label: "Swap for a colleague's shift" },
];

export function MyDutyRoster({ api = defaultShiftsApi(), loadRoster = fetchMyRoster }: { api?: ShiftsApi; loadRoster?: FetchMyRoster }) {
  const cmd = useShiftCommands(api);
  const { busy, dialog } = cmd;
  const today = localToday();
  // Prefix `institution`: `useCommand` refreshes every `institution` query after a command.
  const roster = useQuery({ queryKey: shiftKeys.mine("shifts"), queryFn: () => loadRoster(), retry: false });
  const swaps = useMySwaps(api, roster.isSuccess);
  const open = useOpenShifts({}, api, roster.isSuccess);
  const sheet = useMyTimesheet({}, api, roster.isSuccess);
  const people = useWorkforceApi();
  const profile = useQuery({ queryKey: wkeys.scoped("me", "profile"), queryFn: () => people.myProfile(), retry: false });
  const myStaffId = profile.data?.staff?.id ?? roster.data?.items[0]?.staffId ?? sheet.data?.saved?.staffId ?? null;

  const [asking, setAsking] = useState<string | null>(null);
  const [form, setForm] = useState<{ kind: SwapKindChoice; toStaffId: string; otherShiftId: string; reason: string }>({ kind: "cover", toStaffId: "", otherShiftId: "", reason: "" });
  const [note, setNote] = useState("");

  if (roster.isError) {
    return (
      <BentoTile span={12} title="My duty roster" data-slice="5B" data-my-duty-roster>
        <EmptyState icon={CalendarClock} title="No duty roster" compact>Your duties appear once HR links your login to a staff record.</EmptyState>
      </BentoTile>
    );
  }

  const shifts = (roster.data?.items ?? []).filter((s) => s.date >= today);
  const lineOf = (shiftId: string) => sheet.data?.lines.find((l) => l.shiftId === shiftId);
  const swapList = swaps.data ?? [];
  // Colleagues seen on my swap requests (there is no self-service staff directory).
  const colleagues = new Map<string, string>();
  for (const s of swapList) {
    if (s.fromStaffId && s.fromStaffId !== myStaffId) colleagues.set(s.fromStaffId, s.fromStaffName ?? s.fromStaffId);
    if (s.toStaffId && s.toStaffId !== myStaffId) colleagues.set(s.toStaffId, s.toStaffName ?? s.toStaffId);
  }
  const colleagueOptions = [{ value: "", label: "Choose a colleague…" }, ...[...colleagues].map(([value, label]) => ({ value, label }))];
  const needsColleague = form.kind !== "cover";
  const canSend = !!asking && (!needsColleague || !!form.toStaffId.trim()) && (form.kind !== "swap" || !!form.otherShiftId.trim());

  const sendSwap = async () => {
    if (!asking) return;
    const r = await cmd.requestSwap({
      shiftId: asking,
      kind: form.kind,
      toStaffId: needsColleague ? form.toStaffId.trim() : form.toStaffId.trim() || null,
      otherShiftId: form.kind === "swap" ? form.otherShiftId.trim() : null,
      reason: form.reason.trim() || null,
    });
    if (r) {
      setAsking(null);
      setForm({ kind: "cover", toStaffId: "", otherShiftId: "", reason: "" });
    }
  };

  const ts = sheet.data;
  const tsStatus = ts?.saved?.status ?? null;
  const canSubmit = !!ts && timesheetActions(tsStatus, { own: true }).includes("submit");

  return (
    <BentoTile
      span={12}
      title="My duty roster"
      data-slice="5B"
      data-my-duty-roster
      action={
        <Button size="sm" variant="ghost" data-action="download-ics" disabled={busy} onClick={() => void cmd.download(myRosterIcsUrl(), "my-roster.ics")}>
          Download calendar (.ics)
        </Button>
      }
    >
      {roster.data && <KeyValue label="This period" value={`${formatDateShort(roster.data.from)} – ${formatDate(roster.data.to)} · ${duration(roster.data.minutes)} on duty`} />}

      <DataTable
        caption="My upcoming shifts"
        rows={shifts}
        rowKey={(s) => s.id}
        empty={<EmptyState icon={CalendarClock} title="No upcoming duties" compact>Published shifts assigned to you appear here.</EmptyState>}
        columns={[
          { key: "d", header: "Day", render: (s) => day(s.date) },
          { key: "t", header: "Time", render: (s) => timeRange(s) },
          { key: "r", header: "Duty", render: (s) => (s.location ? `${s.role} · ${s.location}` : s.role) },
          { key: "p", header: "Punch", render: (s) => {
            const l = lineOf(s.id);
            return l?.clockIn ? `In ${l.clockIn}${l.clockOut ? ` · Out ${l.clockOut}` : ""}` : "—";
          } },
          { key: "a", header: "", render: (s) => {
            const l = lineOf(s.id);
            return (
              <span className="flex gap-1">
                {s.date === today && !l?.clockIn && (
                  <Button size="sm" data-action="clock-in" disabled={busy} onClick={() => void cmd.clock(s.id, "in")}>Clock in</Button>
                )}
                {s.date === today && !!l?.clockIn && !l.clockOut && (
                  <Button size="sm" data-action="clock-out" disabled={busy} onClick={() => void cmd.clock(s.id, "out")}>Clock out</Button>
                )}
                <Button size="sm" variant="ghost" data-action="request-swap" disabled={busy} onClick={() => setAsking(asking === s.id ? null : s.id)}>Request swap</Button>
              </span>
            );
          } },
        ]}
      />

      {asking && (
        <div data-swap-form>
          <FormGrid columns={3}>
            <FormSelect label="What do you need?" name="swapKind" value={form.kind} options={KIND_OPTIONS} onChange={(e) => setForm({ ...form, kind: e.target.value as SwapKindChoice })} />
            {needsColleague && colleagues.size > 0 && (
              <FormSelect label="Colleague" name="swapColleague" value={colleagues.has(form.toStaffId) ? form.toStaffId : ""} options={colleagueOptions} onChange={(e) => setForm({ ...form, toStaffId: e.target.value })} />
            )}
            {needsColleague && (
              <TextField label="Colleague's staff ID" value={form.toStaffId} onChange={(e) => setForm({ ...form, toStaffId: e.target.value })} />
            )}
            {form.kind === "swap" && (
              <TextField label="Their shift ID (you take it in exchange)" value={form.otherShiftId} onChange={(e) => setForm({ ...form, otherShiftId: e.target.value })} />
            )}
            <TextField label="Reason (optional)" value={form.reason} onChange={(e) => setForm({ ...form, reason: e.target.value })} />
          </FormGrid>
          <div className="flex gap-2">
            <Button size="sm" data-action="send-swap" disabled={busy || !canSend} onClick={() => void sendSwap()}>Send request</Button>
            <Button size="sm" variant="ghost" data-action="close-swap" onClick={() => setAsking(null)}>Cancel</Button>
          </div>
        </div>
      )}

      <DataTable
        caption="Open shifts"
        rows={open.data?.items ?? []}
        rowKey={(s) => s.id}
        empty={<EmptyState icon={CalendarClock} title="No open shifts" compact>Unassigned published shifts you could take appear here.</EmptyState>}
        columns={[
          { key: "d", header: "Day", render: (s) => day(s.date) },
          { key: "t", header: "Time", render: (s) => timeRange(s) },
          { key: "r", header: "Duty", render: (s) => (s.location ? `${s.role} · ${s.location}` : s.role) },
          { key: "a", header: "", render: (s) => (
            <Button size="sm" data-action="claim-shift" disabled={busy} onClick={() => void cmd.claim(s.id)}>Claim</Button>
          ) },
        ]}
      />

      <DataTable
        caption="Swap requests"
        rows={swapList}
        rowKey={(s: RosterSwap) => s.id}
        empty={<EmptyState icon={CalendarClock} title="No swap requests" compact>Requests you send or receive appear here.</EmptyState>}
        columns={[
          { key: "k", header: "Kind", render: (s) => SWAP_KIND_LABELS[s.kind] ?? s.kind },
          { key: "sh", header: "Shift", render: (s) => snapText(s.snapshot.shift) },
          { key: "o", header: "In exchange", render: (s) => (s.snapshot.other ? snapText(s.snapshot.other) : "—") },
          { key: "w", header: "Between", render: (s) => `${s.fromStaffName ?? "—"} → ${s.toStaffName ?? (s.kind === "cover" ? "anyone" : "—")}` },
          { key: "s", header: "Status", render: (s) => <Badge kind={TONE_KIND[swapTone(s.status)]}>{swapStatusLabel(s.status)}</Badge> },
          { key: "a", header: "", render: (s) => {
            const acts = swapActions(s, { staffId: myStaffId });
            return (
              <span className="flex gap-1">
                {acts.includes("respond") && (
                  <>
                    <Button size="sm" data-action="accept-swap" disabled={busy} onClick={() => void cmd.respondSwap(s, true)}>Accept</Button>
                    <Button size="sm" variant="ghost" data-action="decline-swap" disabled={busy} onClick={() => void cmd.respondSwap(s, false)}>Decline</Button>
                  </>
                )}
                {acts.includes("cancel") && (
                  <Button size="sm" variant="ghost" data-action="cancel-swap" disabled={busy} onClick={() => void cmd.cancelSwap(s)}>Cancel</Button>
                )}
              </span>
            );
          } },
        ]}
      />

      {ts && (
        <div data-my-timesheet>
          <div className="flex gap-2 items-center">
            <KeyValue label="My timesheet" value={`${formatDateShort(ts.from)} – ${formatDate(ts.to)}`} />
            {tsStatus && <Badge kind={TONE_KIND[timesheetTone(tsStatus)]}>{timesheetStatusLabel(tsStatus)}</Badge>}
          </div>
          <p data-timesheet-totals>{totalsSummary(ts.totals)}</p>
          {ts.saved?.decisionReason && <p role="alert">{ts.saved.decisionReason}</p>}
          {canSubmit && (
            <div className="flex gap-2 items-end">
              <TextField label="Note (optional)" value={note} onChange={(e) => setNote(e.target.value)} />
              <Button size="sm" data-action="submit-timesheet" disabled={busy} onClick={() => void (async () => {
                if (await cmd.submitTimesheet({ from: ts.from, to: ts.to, note: note.trim() || null })) setNote("");
              })()}>Submit timesheet</Button>
            </div>
          )}
        </div>
      )}
      {dialog}
    </BentoTile>
  );
}
