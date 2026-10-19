/**
 * Slice **6A** web components (exam setup). The export names are imported by the route frames
 * (docs/batch6-contracts.md §10); real screens carry `data-slice="6A"` and use @sutra/ui only.
 *
 * - `SessionsAdminTab` — Admin → Examinations → Sessions & policy (policy versions, sessions:
 *   review, approval, scheduling, activation, suspension / reopening; papers).
 * - `ScheduleAdminTab` — Admin → Examinations → Timetable & seating (schedules: approval,
 *   publication, revision, withdrawal; sittings; venues and allocation; seating plans).
 * - `RegistrationsAdminTab` — Admin → Examinations → Registrations & hall tickets (bulk and
 *   administrative registration, withdrawal / reinstatement, finalisation, hall tickets).
 * - `StudentExamPanel` — student `/results`: register, hall ticket, timetable.
 */
import { AlertTriangle, CalendarCheck, CalendarDays, ClipboardList, FileBadge, Inbox, ScrollText, ShieldCheck, Ticket } from "lucide-react";
import { BentoTile, Button, DataTable, DateList, EmptyState, KeyTag, KpiTile, ListRow, NameCell, RowActions, formatDate, formatINR, formatNumber, formatTime, plural, statusKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, REASON, type RowAction } from "../../institution/forms";
import { ExamActions as Actions, SessionSelect, timeRange, useExamAsk as useAsk } from "../ui";
import { StatusBadge, statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { ekeys, useExamSetupApi } from "./hooks";
import { useOfferingOptions, useSchemeOptions, useStudentOptions, useTermOptions } from "./pickers";
import type { ExamPolicy, ExamRegistration, ExamSession, HallTicket, TimetableEntry } from "./types";

export { ExamSetupApiProvider } from "./hooks";

function NoInstitutionTile({ title }: { title: string }) {
  return (
    <BentoTile span={12} title={title} data-slice="6A">
      <EmptyState icon={Inbox} title="No institution selected">
        Register an institution in Campus setup → Institution first.
      </EmptyState>
    </BentoTile>
  );
}

/** Sessions of the institution and the picked one (first operational by default). */
function useSessionPicker(institution: Institution | null) {
  const api = useExamSetupApi();
  const inst = institution?.id ?? "-";
  const sessions = useQuery({ queryKey: ekeys.scoped(inst, "sessions"), queryFn: () => api.sessions(inst), enabled: !!institution });
  const [picked, setPicked] = useState<string>("");
  const list = sessions.data ?? [];
  const current: ExamSession | undefined = list.find((s) => s.id === picked) ?? list.find((s) => ["active", "reopened"].includes(s.status)) ?? list[0];
  const picker = <SessionSelect value={current?.id ?? ""} onChange={setPicked} options={list.map((s) => ({ value: s.id, label: `${s.name} · ${statusLabel(s.status)}` }))} />;
  return { current, picker, sessions };
}

// ------------------------------------------------------------------ Sessions & policy

export function SessionsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useExamSetupApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const policies = useQuery({ queryKey: ekeys.scoped(inst, "policies"), queryFn: () => api.policies(inst), enabled: !!institution });
  const sessions = useQuery({ queryKey: ekeys.scoped(inst, "sessions"), queryFn: () => api.sessions(inst), enabled: !!institution });
  const termOptions = useTermOptions(institution?.id ?? null);
  const schemeOptions = useSchemeOptions(institution?.id ?? null);
  const offeringOptions = useOfferingOptions(institution?.id ?? null);
  if (!institution) return <NoInstitutionTile title="Sessions & policy" />;
  const sessionList = sessions.data ?? [];
  const operational = sessionList.filter((s) => ["active", "reopened"].includes(s.status));
  const activePolicy = (policies.data ?? []).find((p) => p.status === "active");
  const paperCount = operational.reduce((n, s) => n + s.papers.length, 0);
  const policyActions = (p: ExamPolicy): RowAction[] => {
    const a: RowAction[] = [];
    if (p.status === "draft") a.push({ id: "policy-submit", label: "Submit", onClick: () => void run(() => api.policyCommand(p.id, p.version, "submit"), "Policy submitted") });
    if (p.status === "approved" || p.status === "scheduled") a.push({ id: "policy-activate", label: "Activate", variant: "primary", onClick: () => void run(() => api.policyCommand(p.id, p.version, "activate"), "Policy activated") });
    if (["active", "superseded", "approved"].includes(p.status))
      a.push({ id: "policy-retire", label: "Retire", onClick: () => ask({ title: `Retire ${p.code} v${p.versionNo}`, name: "policy-retire", fields: [REASON], submitLabel: "Retire", onSubmit: (v) => submit(() => api.policyCommand(p.id, p.version, "retire", v.reason!), "Policy retired") }) });
    return a;
  };
  const sessionActions = (s: ExamSession): RowAction[] => {
    const a: RowAction[] = [];
    const cmd = (c: Parameters<typeof api.sessionCommand>[2], label: string, msg: string, primary = false) =>
      a.push({ id: `session-${c}`, label, variant: primary ? "primary" : "ghost", onClick: () => void run(() => api.sessionCommand(s.id, s.version, c), msg) });
    if (s.status === "draft" && !s.mergedIntoId) cmd("submit", "Submit for approval", "Session submitted");
    if (s.status === "approved") cmd("schedule", "Schedule", "Session scheduled");
    if (s.status === "scheduled") cmd("activate", "Activate", "Session active — registration and conduct may start", true);
    if (["active", "reopened"].includes(s.status))
      a.push({ id: "session-suspend", label: "Suspend", onClick: () => ask({ title: `Suspend ${s.code}`, name: "session-suspend", fields: [REASON], submitLabel: "Suspend", onSubmit: (v) => submit(() => api.sessionCommand(s.id, s.version, "suspend", v.reason!), "Session suspended") }) });
    if (s.status === "suspended") cmd("reopen", "Reopen", "Session reopened");
    if (["draft", "approved", "scheduled", "active", "reopened"].includes(s.status)) {
      a.push({ id: "paper-generate", label: "Generate papers", onClick: () => void run(() => api.generatePapers(s.id), (r) => `${r.processed} paper(s) added, ${r.skipped} skipped`) });
      a.push({ id: "paper-add", label: "Add paper", onClick: () => ask({ title: `Add a paper to ${s.code}`, name: "paper-add", fields: [{ name: "offeringId", label: "Course offering", type: "select", required: true, wide: true, options: offeringOptions }], submitLabel: "Add paper", onSubmit: (v) => submit(() => api.addPaper(s.id, v.offeringId!), "Paper added") }) });
    }
    return a;
  };
  return (
    <>
      <KpiTile
        span={4}
        icon={CalendarCheck}
        tone="violet"
        label="Active sessions"
        value={sessions.data ? formatNumber(operational.length) : "…"}
        sub={operational[0] ? operational[0].name : `${sessionList.length} session${sessionList.length === 1 ? "" : "s"} in all`}
        data-kpi="exam-sessions"
      />
      <KpiTile span={4} icon={ClipboardList} tone="green" label="Papers in session" value={sessions.data ? formatNumber(paperCount) : "…"} sub="Across active sessions" data-kpi="exam-papers" />
      <KpiTile
        span={4}
        icon={ShieldCheck}
        tone="orange"
        label="Examination policy"
        value={policies.data ? (activePolicy ? `v${activePolicy.versionNo}` : "None") : "…"}
        sub={activePolicy ? `${activePolicy.code} is active` : "Draft and activate a policy"}
        data-kpi="exam-policy"
      />
      <BentoTile
        span={12}
        title="Examination policy"
        data-slice="6A"
        action={
          <Button
            icon={<ScrollText strokeWidth={1.75} />}
            data-new-policy
            onClick={() =>
              ask({
                title: "New policy version",
                name: "policy-create",
                submitLabel: "Create draft",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "EXAM-POLICY" },
                  { name: "name", label: "Name", placeholder: "Examination policy" },
                  { name: "minAttendancePercent", label: "Minimum attendance %", placeholder: "75" },
                  { name: "lateFeeCode", label: "Late fee code", placeholder: "EXAM-LATE" },
                  { name: "lateFeeAmount", label: "Late fee (₹)", placeholder: "500.00" },
                  { name: "requireDuesCleared", label: "Withhold hall tickets while dues are outstanding", type: "checkbox", initial: "true", wide: true },
                ],
                onSubmit: (v) =>
                  submit(
                    () =>
                      api.createPolicy(inst, {
                        code: v.code,
                        name: v.name || undefined,
                        minAttendancePercent: v.minAttendancePercent || undefined,
                        lateFeeCode: v.lateFeeCode || undefined,
                        lateFeeAmount: v.lateFeeAmount || undefined,
                        requireDuesCleared: v.requireDuesCleared === "true",
                      }),
                    (p) => `Policy ${p.code} v${p.versionNo} drafted`,
                  ),
              })
            }
          >
            New version
          </Button>
        }
      >
        <DataTable
          caption="Policy versions"
          rows={policies.data ?? []}
          rowKey={(p) => p.id}
          empty={<EmptyState icon={ScrollText} title="No examination policy yet">Draft the institution's first policy version.</EmptyState>}
          columns={[
            { key: "c", header: "Policy", render: (p) => (
                <span data-policy={p.code}>
                  <b>{p.name || "Examination policy"}</b> <KeyTag>{`${p.code} v${p.versionNo}`}</KeyTag>
                </span>
              ) },
            { key: "a", header: "Attendance", render: (p) => (p.minAttendancePercent ? `≥ ${p.minAttendancePercent}%` : "—") },
            { key: "d", header: "Dues", render: (p) => (p.requireDuesCleared ? "Must be cleared" : "Not checked") },
            { key: "l", header: "Late fee", render: (p) => (p.lateFeeAmount ? formatINR(p.lateFeeAmount) : "—") },
            { key: "s", header: "Status", render: (p) => <StatusBadge status={p.status} /> },
            { key: "x", header: "", render: (p) => <Actions actions={policyActions(p)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Examination sessions"
        data-slice="6A"
        action={
          <Button
            icon={<CalendarDays strokeWidth={1.75} />}
            data-new-session
            onClick={() =>
              ask({
                title: "New examination session",
                name: "session-create",
                submitLabel: "Create draft",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "ESE-NOV-2026" },
                  { name: "name", label: "Name", required: true, placeholder: "End-semester examinations" },
                  { name: "academicTermId", label: "Academic term", type: "select", required: true, wide: true, options: termOptions },
                  { name: "gradingSchemeId", label: "Grading scheme", type: "select", wide: true, options: schemeOptions },
                  { name: "startsOn", label: "Starts on", type: "date" },
                  { name: "endsOn", label: "Ends on", type: "date" },
                  { name: "registrationOpensOn", label: "Registration opens", type: "date" },
                  { name: "registrationClosesOn", label: "Registration closes", type: "date" },
                ],
                onSubmit: (v) =>
                  submit(
                    () =>
                      api.createSession(inst, Object.fromEntries(Object.entries(v).filter(([, x]) => x !== ""))),
                    (s) => `Session ${s.code} drafted`,
                  ),
              })
            }
          >
            New session
          </Button>
        }
      >
        <DataTable
          caption="Sessions"
          rows={sessions.data ?? []}
          rowKey={(s) => s.id}
          empty={<EmptyState icon={CalendarDays} title="No examination sessions">Create a session for an academic term.</EmptyState>}
          columns={[
            { key: "c", header: "Session", render: (s) => (
                <span data-session={s.code}>
                  <b>{s.name}</b> <KeyTag>{s.code}</KeyTag>
                </span>
              ) },
            { key: "w", header: "Dates", render: (s) => `${fmtDate(s.startsOn)} – ${fmtDate(s.endsOn)}` },
            { key: "p", header: "Papers", render: (s) => s.papers.map((p) => p.paperCode).join(", ") || "—" },
            { key: "r", header: "Revisions", render: (s) => (s.mergedIntoId ? "Merged (duplicate)" : formatNumber(s.revisions)) },
            { key: "s", header: "Status", render: (s) => <StatusBadge status={s.status} /> },
            { key: "x", header: "", render: (s) => <Actions actions={sessionActions(s)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Timetable & seating

export function ScheduleAdminTab({ institution }: { institution: Institution | null }) {
  const api = useExamSetupApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const { current, picker } = useSessionPicker(institution);
  const sid = current?.id ?? "-";
  const schedules = useQuery({ queryKey: ekeys.scoped(sid, "schedules"), queryFn: () => api.schedules(sid), enabled: !!current });
  const venues = useQuery({ queryKey: ekeys.scoped(inst, "venues"), queryFn: () => api.venues(inst), enabled: !!institution });
  if (!institution) return <NoInstitutionTile title="Timetable & seating" />;
  const live = (schedules.data ?? []).find((s) => ["draft", "under_review", "approved", "published", "active"].includes(s.status));
  const scheduleActions = (): RowAction[] => {
    if (!live) return [];
    const a: RowAction[] = [];
    if (live.status === "draft") {
      a.push({ id: "schedule-submit", label: "Submit for approval", onClick: () => void run(() => api.scheduleCommand(live.id, live.version, "submit"), "Timetable submitted") });
      a.push({
        id: "sitting-add",
        label: "Add sitting",
        onClick: () =>
          ask({
            title: "Add a sitting",
            name: "sitting-add",
            submitLabel: "Add",
            fields: [
              { name: "paperId", label: "Paper", type: "select", required: true, options: [{ value: "", label: "Choose a paper" }, ...(current?.papers ?? []).map((p) => ({ value: p.id, label: `${p.paperCode} — ${p.title}` }))] },
              { name: "sitsOn", label: "Date", type: "date", required: true },
              { name: "startsAt", label: "Start (HH:MM)", required: true, initial: "09:30" },
            ],
            onSubmit: (v) => submit(() => api.addSitting(live.id, { paperId: v.paperId!, sitsOn: v.sitsOn!, startsAt: v.startsAt! }), "Sitting added"),
          }),
      });
    }
    if (live.status === "approved") a.push({ id: "schedule-publish", label: "Publish timetable", variant: "primary", onClick: () => void run(() => api.scheduleCommand(live.id, live.version, "publish"), "Official timetable published — students and faculty notified") });
    if (live.status === "published") {
      a.push({ id: "schedule-revise", label: "Revise", onClick: () => ask({ title: "Revise the timetable", name: "schedule-revise", fields: [REASON], submitLabel: "Open revision", onSubmit: (v) => submit(() => api.scheduleCommand(live.id, live.version, "revise", v.reason!), "Revision opened") }) });
      a.push({ id: "schedule-withdraw", label: "Withdraw", onClick: () => ask({ title: "Withdraw the timetable", name: "schedule-withdraw", fields: [REASON], submitLabel: "Withdraw", onSubmit: (v) => submit(() => api.scheduleCommand(live.id, live.version, "withdraw", v.reason!), "Timetable withdrawn — stakeholders notified") }) });
    }
    return a;
  };
  return (
    <>
      <BentoTile
        span={8}
        title="Timetable"
        data-slice="6A"
        action={
          current && !live ? (
            <Button icon={<CalendarDays strokeWidth={1.75} />} data-new-schedule onClick={() => void run(() => api.createSchedule(current.id, `${current.name} timetable`), "Timetable drafted")}>
              Plan timetable
            </Button>
          ) : undefined
        }
      >
        {picker}
        {live ? (
          <>
            <div data-schedule-status={live.status} style={{ display: "flex", alignItems: "center", gap: 12, flexWrap: "wrap", justifyContent: "space-between", marginBottom: 8 }}>
              <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
                <b>{live.name}</b> <KeyTag>{`Version ${live.versionNo}`}</KeyTag> <StatusBadge status={live.status} />
              </span>
              <Actions actions={scheduleActions()} disabled={busy} label="Timetable actions" />
            </div>
            {live.conflicts.length > 0 && (
              <div data-conflicts>
                {live.conflicts.map((c, i) => (
                  <ListRow key={i} icon={AlertTriangle} tone="orange" title={c.detail} meta={plural(c.count, "sitting")} />
                ))}
              </div>
            )}
            <DataTable
              caption="Sittings"
              rows={live.sittings}
              rowKey={(x) => x.id}
              empty={<EmptyState icon={CalendarDays} title="No sittings yet">Add a sitting for each paper.</EmptyState>}
              columns={[
                { key: "p", header: "Paper", render: (x) => (
                    <span data-sitting={x.paperCode}>
                      <b>{x.paperCode}</b> · {x.title}
                    </span>
                  ) },
                { key: "d", header: "When", render: (x) => `${fmtDate(x.sitsOn)} · ${timeRange(formatTime(x.startsAt), x.durationMinutes)}` },
                { key: "v", header: "Venues", render: (x) => x.venues.map((v) => `${v.venueName} · ${formatNumber(v.seats)} seats`).join(", ") || "—" },
                { key: "s", header: "Status", render: (x) => <StatusBadge status={x.status} /> },
                {
                  key: "x",
                  header: "",
                  render: (x) => (
                    <Actions
                      disabled={busy}
                      actions={[
                        {
                          id: "venue-allocate",
                          label: "Allocate venue",
                          onClick: () =>
                            ask({
                              title: `Allocate a venue to ${x.paperCode}`,
                              name: "venue-allocate",
                              submitLabel: "Allocate",
                              fields: [{ name: "venueId", label: "Venue", type: "select", required: true, options: [{ value: "", label: "Choose a venue" }, ...(venues.data ?? []).map((v) => ({ value: v.id, label: `${v.code} — ${v.name} (${v.capacity})` }))] }],
                              onSubmit: (v) => submit(() => api.allocate(x.id, v.venueId!), "Venue allocated"),
                            }),
                        },
                        { id: "seating-generate", label: "Generate seating", onClick: () => void run(() => api.generateSeating(x.id), (p) => `Seating plan drafted for ${p.candidates} candidate(s)`) },
                      ]}
                    />
                  ),
                },
              ]}
            />
          </>
        ) : (
          <EmptyState icon={CalendarDays} title={current ? "No live timetable" : "No examination session"}>
            {current ? "Plan the session's timetable, then submit it for approval and publish it." : "Create a session under Sessions & policy first."}
          </EmptyState>
        )}
      </BentoTile>
      <BentoTile
        span={4}
        title="Venues"
        data-slice="6A"
        action={
          <Button
            data-new-venue
            onClick={() =>
              ask({
                title: "New venue",
                name: "venue-create",
                submitLabel: "Add venue",
                fields: [
                  { name: "code", label: "Code", required: true, placeholder: "HALL-A" },
                  { name: "name", label: "Name", required: true },
                  { name: "capacity", label: "Capacity", type: "number", required: true, initial: "60" },
                ],
                onSubmit: (v) => submit(() => api.createVenue(inst, { code: v.code!, name: v.name!, capacity: Number(v.capacity) }), "Venue added"),
              })
            }
          >
            Add venue
          </Button>
        }
      >
        <DataTable
          caption="Venues"
          rows={venues.data ?? []}
          rowKey={(v) => v.id}
          empty={<EmptyState icon={ClipboardList} title="No venues">Add the examination halls and rooms.</EmptyState>}
          columns={[
            { key: "c", header: "Venue", render: (v) => (
                <span>
                  <b>{v.name}</b> <KeyTag>{v.code}</KeyTag>
                </span>
              ) },
            { key: "n", header: "Capacity", render: (v) => formatNumber(v.capacity) },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Registrations & hall tickets

export function RegistrationsAdminTab({ institution }: { institution: Institution | null }) {
  const api = useExamSetupApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const { current, picker } = useSessionPicker(institution);
  const sid = current?.id ?? "-";
  const regs = useQuery({ queryKey: ekeys.scoped(sid, "registrations"), queryFn: () => api.registrations(sid), enabled: !!current });
  const tickets = useQuery({ queryKey: ekeys.scoped(sid, "tickets"), queryFn: () => api.hallTickets(sid), enabled: !!current });
  const studentOptions = useStudentOptions(institution?.id ?? null);
  if (!institution) return <NoInstitutionTile title="Registrations & hall tickets" />;
  const regActions = (r: ExamRegistration): RowAction[] => {
    const a: RowAction[] = [];
    if (r.status === "amended") a.push({ id: "registration-confirm", label: "Confirm", onClick: () => void run(() => api.registrationCommand(r.id, r.version, "confirm"), "Registration confirmed") });
    if (["confirmed", "amended", "locked", "approved"].includes(r.status))
      a.push({ id: "registration-withdraw", label: "Withdraw", onClick: () => ask({ title: `Withdraw ${r.registrationNumber}`, name: "registration-withdraw", fields: [REASON], submitLabel: "Withdraw", onSubmit: (v) => submit(() => api.registrationCommand(r.id, r.version, "withdraw", v.reason!), "Registration withdrawn — hall ticket invalidated") }) });
    if (r.status === "withdrawn")
      a.push({ id: "registration-reinstate", label: "Reinstate", onClick: () => ask({ title: `Reinstate ${r.registrationNumber}`, name: "registration-reinstate", fields: [REASON], submitLabel: "Reinstate", onSubmit: (v) => submit(() => api.registrationCommand(r.id, r.version, "reinstate", v.reason!), "Registration reinstated") }) });
    return a;
  };
  const ticketActions = (t: HallTicket): RowAction[] => {
    const a: RowAction[] = [];
    if (t.status === "withheld") a.push({ id: "ticket-release", label: "Re-check & release", variant: "primary", onClick: () => void run(() => api.ticketCommand(t.id, t.version, "release"), "Hall ticket released") });
    if (t.status === "active") {
      a.push({ id: "ticket-withhold", label: "Withhold", onClick: () => ask({ title: `Withhold ${t.number}`, name: "ticket-withhold", fields: [REASON], submitLabel: "Withhold", onSubmit: (v) => submit(() => api.ticketCommand(t.id, t.version, "withhold", v.reason!), "Hall ticket withheld") }) });
      a.push({ id: "ticket-reissue", label: "Reissue", onClick: () => ask({ title: `Reissue ${t.number}`, name: "ticket-reissue", fields: [REASON], submitLabel: "Reissue", onSubmit: (v) => submit(() => api.ticketCommand(t.id, t.version, "reissue", v.reason!), (n) => `Reissued as ${n.number}`) }) });
    }
    return a;
  };
  const regList = regs.data ?? [];
  const ticketList = tickets.data ?? [];
  const confirmed = regList.filter((r) => ["confirmed", "approved", "amended", "locked"].includes(r.status)).length;
  const issued = ticketList.filter((t) => t.status === "active").length;
  const withheld = ticketList.filter((t) => t.status === "withheld").length;
  return (
    <>
      <KpiTile span={4} icon={ClipboardList} tone="violet" label="Registrations" value={regs.data ? formatNumber(confirmed) : current ? "…" : "—"} sub={`${regList.length} in all${current ? ` · ${current.name}` : ""}`} data-kpi="exam-registrations" />
      <KpiTile span={4} icon={Ticket} tone="green" label="Hall tickets issued" value={tickets.data ? formatNumber(issued) : current ? "…" : "—"} sub="Active tickets" data-kpi="hall-tickets" />
      <KpiTile span={4} icon={FileBadge} tone="orange" label="Withheld" value={tickets.data ? formatNumber(withheld) : current ? "…" : "—"} sub="Dues or attendance conditions" data-kpi="hall-tickets-withheld" />
      <BentoTile
        span={12}
        title="Registrations"
        data-slice="6A"
        action={
          current ? (
            <>
              <Button icon={<ClipboardList strokeWidth={1.75} />} data-bulk-register onClick={() => void run(() => api.bulkRegister(current.id), (r) => `${r.processed} registered, ${r.skipped} already registered, ${r.failed.length} not eligible`)}>
                Register from rosters
              </Button>
              <RowActions
                label="Registration actions"
                menu={[
                  {
                    id: "register",
                    label: "Register candidate",
                    disabled: busy,
                    onSelect: () =>
                      ask({ title: "Register a candidate", name: "registration-create", submitLabel: "Register", fields: [{ name: "studentId", label: "Student", type: "select", required: true, wide: true, options: studentOptions }], onSubmit: (v) => submit(() => api.register(current.id, v.studentId!), (r) => `${r.registrationNumber}: ${statusLabel(r.status)}`) }),
                  },
                  { id: "finalise", label: "Finalise population", disabled: busy, onSelect: () => void run(() => api.finalisePopulation(current.id), (r) => `${r.processed} registration(s) locked`) },
                ]}
              />
            </>
          ) : undefined
        }
      >
        {picker}
        <DataTable
          caption="Registrations"
          rows={regs.data ?? []}
          rowKey={(r) => r.id}
          empty={<EmptyState icon={ClipboardList} title="No registrations">Students register themselves, or register them from the rosters.</EmptyState>}
          columns={[
            { key: "n", header: "Registration", render: (r) => <span data-registration={r.registrationNumber}>{r.registrationNumber}</span> },
            { key: "w", header: "Candidate", render: (r) => <NameCell name={r.displayName} sub={r.studentNumber} /> },
            { key: "c", header: "Channel", render: (r) => statusLabel(r.channel) },
            { key: "p", header: "Papers", render: (r) => r.papers.filter((p) => p.status !== "withdrawn").map((p) => p.paperCode).join(", ") || "—" },
            { key: "s", header: "Status", render: (r) => <StatusBadge status={r.status} /> },
            { key: "x", header: "", render: (r) => <Actions actions={regActions(r)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      <BentoTile
        span={12}
        title="Hall tickets"
        data-slice="6A"
        action={
          current ? (
            <Button icon={<Ticket strokeWidth={1.75} />} data-issue-tickets onClick={() => void run(() => api.issueHallTickets(current.id), (r) => `${r.processed} hall ticket(s) issued or withheld, ${r.skipped} unchanged`)}>
              Issue hall tickets
            </Button>
          ) : undefined
        }
      >
        <DataTable
          caption="Hall tickets"
          rows={tickets.data ?? []}
          rowKey={(t) => t.id}
          empty={<EmptyState icon={Ticket} title="No hall tickets">Issue them once registrations are confirmed.</EmptyState>}
          columns={[
            { key: "n", header: "Ticket", render: (t) => <span data-ticket={t.number}>{t.number}</span> },
            { key: "r", header: "Registration", render: (t) => (regs.data ?? []).find((r) => r.id === t.registrationId)?.registrationNumber ?? "—" },
            { key: "w", header: "Condition", render: (t) => t.withheldReason ?? (t.specialArrangements ? `Arrangements: ${t.specialArrangements}` : "—") },
            { key: "s", header: "Status", render: (t) => <StatusBadge status={t.status} /> },
            { key: "x", header: "", render: (t) => <Actions actions={ticketActions(t)} disabled={busy} /> },
          ]}
        />
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

// ------------------------------------------------------------------ Student

const MON = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/** The student's sittings grouped by day for a `DateList` (prototype calendar chips). */
function timetableDays(list: readonly TimetableEntry[]) {
  const days = new Map<string, TimetableEntry[]>();
  for (const e of [...list].sort((a, b) => a.sitsOn.localeCompare(b.sitsOn) || a.startsAt.localeCompare(b.startsAt))) days.set(e.sitsOn, [...(days.get(e.sitsOn) ?? []), e]);
  return [...days.entries()].map(([date, items]) => {
    const [, m, d] = date.split("-").map(Number);
    return {
      key: date,
      day: String(d),
      month: MON[(m ?? 1) - 1] ?? "",
      items: items.map((e) => ({
        meta: timeRange(formatTime(e.startsAt), e.durationMinutes),
        label: (
          <strong data-my-sitting={e.paperCode} style={{ fontWeight: 500 }}>
            {`${e.paperCode} · ${e.title}`}
            {e.seatNo ? ` · seat ${e.seatNo}` : ""}
            {e.postponedFrom ? ` (moved from ${fmtDate(e.postponedFrom)})` : ""}
          </strong>
        ),
      })),
    };
  });
}

/** Student Results: registration, hall ticket and exam timetable (secondary tile, ListRows). */
export function StudentExamPanel({ span = 6 }: { span?: 4 | 6 | 12 }) {
  const api = useExamSetupApi();
  const { run, busy, dialog } = useCommand();
  const options = useQuery({ queryKey: ekeys.scoped("me", "options"), queryFn: () => api.myOptions() });
  const regs = useQuery({ queryKey: ekeys.scoped("me", "registrations"), queryFn: () => api.myRegistrations() });
  const tickets = useQuery({ queryKey: ekeys.scoped("me", "tickets"), queryFn: () => api.myHallTickets() });
  const timetable = useQuery({ queryKey: ekeys.scoped("me", "timetable"), queryFn: () => api.myTimetable() });
  const open = (options.data ?? []).filter((o) => !o.registered && (o.window === "open" || o.window === "override") && o.papers.some((p) => p.covered));
  const nothing = !open.length && !(regs.data ?? []).length && !(tickets.data ?? []).length && !(timetable.data ?? []).length;
  const badge = (st: string) => ({ label: statusLabel(st), kind: statusKind(st) });
  return (
    <BentoTile span={span} title="My examinations" data-slice="6A">
      {nothing ? (
        <EmptyState icon={FileBadge} title="No examination registration" compact>
          Registration opens when the examination office activates a session.
        </EmptyState>
      ) : null}
      {open.map((o) => (
        <ListRow
          key={o.sessionId}
          data-exam-option={o.code}
          icon={ClipboardList}
          tone="violet"
          title={o.name}
          meta={`Registration closes ${fmtDate(o.registrationClosesOn)} · ${o.papers.filter((p) => p.covered).map((p) => p.paperCode).join(", ")}`}
          primary={{ label: "Register", disabled: busy, "data-register-me": "", onClick: () => void run(() => api.registerMe(o.sessionId), (r) => (r.status === "confirmed" ? `Registered — ${r.registrationNumber}` : `Not accepted: ${r.statusReason ?? statusLabel(r.status)}`)) }}
        />
      ))}
      {(regs.data ?? []).map((r) => (
        <ListRow
          key={r.id}
          data-my-registration={r.registrationNumber}
          icon={FileBadge}
          tone="blue"
          title={`Registration ${r.registrationNumber}`}
          meta={r.statusReason && r.status === "rejected" ? r.statusReason : `${plural(r.papers.filter((p) => p.status !== "withdrawn").length, "paper")} registered`}
          badge={badge(r.status)}
        />
      ))}
      {(tickets.data ?? []).map((t) => (
        <ListRow key={t.ticket.id} data-my-ticket={t.ticket.number} icon={Ticket} tone="green" title={`Hall ticket ${t.ticket.number}`} meta={t.ticket.withheldReason ?? (t.ticket.issuedAt ? `Issued ${formatDate(t.ticket.issuedAt)}` : "Hall ticket")} badge={badge(t.ticket.status)} />
      ))}
      {(timetable.data ?? []).length > 0 && (
        <div data-my-timetable style={{ marginTop: 8 }}>
          <DateList days={timetableDays(timetable.data ?? [])} />
        </div>
      )}
      {dialog}
    </BentoTile>
  );
}
