/**
 * Admin → Examinations → Conduct (DEF-I-007). The exam office runs examination day in the app:
 *
 * - **Sittings** — one row per sitting of the official (published) timetable with its seating
 *   plan, invigilator staffing and conduct record. ONE primary action per row moves the sitting
 *   on (publish seating → open conduct → start → record attendance); the rest sit in the kebab.
 *   Recording attendance completes the conduct record and marks the paper `conducted`, which is
 *   what lets the mark sheet open under Marks.
 * - **Invigilation** — duties of every sitting (assign, confirm, release, replace).
 * - **Exceptions** — examination exception cases (raise → classify / review → resolve → close).
 *
 * Built only on the existing `exam-scheduling` endpoints (`./api`) and @sutra/ui components.
 */
import { AlertTriangle, ClipboardCheck, ShieldAlert, UserCheck, Users } from "lucide-react";
import { BentoTile, Button, DataTable, EmptyState, KeyTag, KpiTile, ListRow, formatNumber, formatTime, plural, statusKind } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { fmtDate, REASON, type FieldSpec, type RowAction } from "../../institution/forms";
import { statusLabel, useCommand } from "../../institution/hooks";
import type { Institution } from "../../institution/types";
import { useWorkforceApi, wkeys } from "../../people/workforce/hooks";
import { ekeys, useExamSetupApi } from "../setup/hooks";
import type { ExamRegistration, ExamSession, ExamSitting } from "../setup/types";
import { SessionSelect, splitActions, timeRange, ExamActions as Actions, useExamAsk as useAsk } from "../ui";
import { ckeys, useExamConductApi } from "./hooks";
import type { AttendanceEntry, ConductRecord, ExamException, InvigilationDuty, SittingBoard } from "./types";

export { ExamConductApiProvider } from "./hooks";

/** Registration statuses that make a candidate effective (server `sitting_candidates`). */
const EFFECTIVE_REG = ["approved", "confirmed", "locked", "amended"];
const EFFECTIVE_PAPER = ["registered", "appeared", "absent"];
const LIVE_DUTY = ["assigned", "confirmed"];
const OUTCOMES = [
  { value: "present", label: "Present" },
  { value: "absent", label: "Absent" },
  { value: "denied_entry", label: "Denied entry" },
];
const ROLES = [
  { value: "invigilator", label: "Invigilator" },
  { value: "chief_invigilator", label: "Chief invigilator" },
  { value: "practical_examiner", label: "Practical examiner" },
  { value: "external_examiner", label: "External examiner" },
];
const CATEGORIES = [
  { value: "", label: "Choose a category" },
  { value: "conduct", label: "Conduct" },
  { value: "venue", label: "Venue" },
  { value: "paper", label: "Question paper" },
  { value: "candidate", label: "Candidate" },
  { value: "technical", label: "Technical" },
  { value: "other", label: "Other" },
];
const SEVERITIES = [
  { value: "", label: "Not set" },
  { value: "low", label: "Low" },
  { value: "medium", label: "Medium" },
  { value: "high", label: "High" },
  { value: "critical", label: "Critical" },
];

/** Candidates of a sitting: effective registrations holding the sitting's paper. */
export function sittingCandidates(regs: readonly ExamRegistration[], paperId: string): ExamRegistration[] {
  return regs.filter((r) => EFFECTIVE_REG.includes(r.status) && r.papers.some((p) => p.paperId === paperId && EFFECTIVE_PAPER.includes(p.status)));
}

/** Form field name of a candidate's attendance outcome (`att-<student number>`). */
export const attendanceField = (r: Pick<ExamRegistration, "studentNumber">) => `att-${r.studentNumber}`;

/** Attendance entries from the dialog values (one per candidate). */
export function attendanceFrom(candidates: readonly ExamRegistration[], values: Record<string, string>): AttendanceEntry[] {
  return candidates.map((r) => ({ registrationId: r.id, outcome: (values[attendanceField(r)] || "present") as AttendanceEntry["outcome"] }));
}

function useStaffOptions(institutionId: string | null) {
  const api = useWorkforceApi();
  const q = useQuery({ queryKey: wkeys.scoped(institutionId ?? "-", "invigilator-options"), enabled: !!institutionId, queryFn: () => api.staff(institutionId!) });
  const linked = (q.data ?? []).filter((s) => s.subjectRef && !["archived", "suspended"].includes(s.status));
  return [{ value: "", label: linked.length ? "Choose a staff member" : "No staff with a sign-in account" }, ...linked.map((s) => ({ value: s.subjectRef!, label: `${s.displayName} (${s.staffNumber})` }))];
}

function conductLine(c: ConductRecord | null): string {
  if (!c) return "Not opened";
  if (c.status === "completed") return `${statusLabel(c.status)} · ${c.presentCount} present, ${c.absentCount} absent${c.deniedCount ? `, ${c.deniedCount} denied` : ""}`;
  if (c.pendingPostpone) return `${statusLabel(c.status)} · postponement awaiting decision`;
  return statusLabel(c.status);
}

export function ConductAdminTab({ institution }: { institution: Institution | null }) {
  const setup = useExamSetupApi();
  const api = useExamConductApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const inst = institution?.id ?? "-";
  const sessions = useQuery({ queryKey: ekeys.scoped(inst, "sessions"), queryFn: () => setup.sessions(inst), enabled: !!institution });
  const [picked, setPicked] = useState("");
  const list = sessions.data ?? [];
  const current: ExamSession | undefined = list.find((s) => s.id === picked) ?? list.find((s) => ["active", "reopened"].includes(s.status)) ?? list[0];
  const sid = current?.id ?? "-";
  const schedules = useQuery({ queryKey: ekeys.scoped(sid, "schedules"), queryFn: () => setup.schedules(sid), enabled: !!current });
  const regs = useQuery({ queryKey: ekeys.scoped(sid, "registrations"), queryFn: () => setup.registrations(sid), enabled: !!current });
  const exceptions = useQuery({ queryKey: ckeys.scoped(sid, "exceptions"), queryFn: () => api.exceptions(sid), enabled: !!current });
  const official = (schedules.data ?? []).find((s) => ["published", "active"].includes(s.status));
  const sittings: ExamSitting[] = (official?.sittings ?? []).filter((x) => x.status !== "cancelled");
  const board = useQuery({
    queryKey: ckeys.scoped(official?.id ?? "-", `board:${sittings.map((x) => `${x.id}.${x.version}`).join(",")}`),
    enabled: sittings.length > 0,
    queryFn: async (): Promise<Record<string, SittingBoard>> => {
      const rows = await Promise.all(
        sittings.map(async (x) => {
          const [conduct, seating, staffing] = await Promise.all([api.conduct(x.id), api.seating(x.id), api.invigilation(x.id)]);
          return [x.id, { conduct, seating, staffing }] as const;
        }),
      );
      return Object.fromEntries(rows);
    },
  });
  const staffOptions = useStaffOptions(institution?.id ?? null);

  if (!institution)
    return (
      <BentoTile span={12} title="Conduct" data-slice="6A">
        <EmptyState icon={ClipboardCheck} title="No institution selected">
          Register an institution in Campus setup → Institution first.
        </EmptyState>
      </BentoTile>
    );

  const regList = regs.data ?? [];
  const boards = board.data ?? {};
  const sittingOptions = [{ value: "", label: "Choose a sitting" }, ...sittings.map((x) => ({ value: x.id, label: `${x.paperCode} — ${fmtDate(x.sitsOn)} ${formatTime(x.startsAt)}` }))];
  const sittingOf = (id?: string | null) => sittings.find((x) => x.id === id);
  const conducted = sittings.filter((x) => boards[x.id]?.conduct?.status === "completed").length;
  const shortage = sittings.reduce((n, x) => n + (boards[x.id]?.staffing?.shortage ?? 0), 0);
  const openExceptions = (exceptions.data ?? []).filter((e) => e.status !== "closed").length;

  // ---------------------------------------------------------------- dialogs
  const assignInvigilator = (sittingId = "") =>
    ask({
      title: "Assign an invigilator",
      name: "invigilator-assign",
      submitLabel: "Assign",
      intro: "Pick a staff member with a sign-in account, or enter the sign-in account directly.",
      fields: [
        { name: "sittingId", label: "Sitting", type: "select", required: true, wide: true, options: sittingOptions, initial: sittingId },
        { name: "staffRef", label: "Staff member", type: "select", wide: true, options: staffOptions },
        { name: "subjectRef", label: "Or sign-in account", placeholder: "dev|name@college.edu" },
        { name: "name", label: "Display name", placeholder: "Dr. A. Rao" },
        { name: "role", label: "Duty", type: "select", options: ROLES, initial: "invigilator" },
      ],
      onSubmit: (v) => {
        const subjectRef = v.staffRef || v.subjectRef?.trim();
        if (!subjectRef) throw new Error("Choose a staff member or enter a sign-in account");
        const staffName = v.staffRef ? staffOptions.find((o) => o.value === v.staffRef)?.label.replace(/ \([^)]*\)$/, "") : undefined;
        return submit(() => api.assignDuty(v.sittingId!, { subjectRef, name: v.name?.trim() || staffName || undefined, role: v.role || undefined }), (d) => `${d.name ?? d.subjectRef} assigned`);
      },
    });

  const recordAttendance = (x: ExamSitting, c: ConductRecord) => {
    const candidates = sittingCandidates(regList, x.paperId);
    const fields: FieldSpec[] = candidates.map((r) => ({
      name: attendanceField(r),
      label: `${r.displayName} (${r.studentNumber})${r.hallTicket && r.hallTicket.status !== "active" ? ` · hall ticket ${statusLabel(r.hallTicket.status).toLowerCase()}` : ""}`,
      type: "select" as const,
      options: OUTCOMES,
      initial: "present",
    }));
    ask({
      title: `Attendance — ${x.paperCode}`,
      name: "conduct-complete",
      eyebrow: "Examination conduct",
      submitLabel: "Complete conduct",
      intro: candidates.length
        ? `Record each candidate's attendance. Completing marks ${x.paperCode} conducted so its mark sheet can be opened; a candidate whose hall ticket is not active is recorded as denied entry.`
        : `No registered candidates for ${x.paperCode}. Completing still marks the paper conducted.`,
      fields,
      onSubmit: (v) => submit(() => api.completeConduct(c.id, c.version, attendanceFrom(candidates, v)), (r) => `${x.paperCode} conducted — ${r.presentCount} present, ${r.absentCount} absent`),
    });
  };

  const raiseException = (sittingId = "") =>
    ask({
      title: "Raise an exception",
      name: "exception-raise",
      eyebrow: "Examination conduct",
      submitLabel: "Raise",
      fields: [
        { name: "category", label: "Category", type: "select", required: true, options: CATEGORIES },
        { name: "severity", label: "Severity", type: "select", options: SEVERITIES },
        { name: "sittingId", label: "Sitting", type: "select", wide: true, options: [{ value: "", label: "Whole session" }, ...sittingOptions.slice(1)], initial: sittingId },
        { name: "registrationId", label: "Candidate", type: "select", wide: true, options: [{ value: "", label: "No particular candidate" }, ...regList.map((r) => ({ value: r.id, label: `${r.displayName} (${r.registrationNumber})` }))] },
        { name: "description", label: "What happened", required: true, wide: true },
      ],
      onSubmit: (v) =>
        submit(
          () =>
            api.raiseException(current!.id, {
              category: v.category!,
              description: v.description!,
              severity: v.severity || undefined,
              sittingId: v.sittingId || undefined,
              paperId: sittingOf(v.sittingId)?.paperId,
              registrationId: v.registrationId || undefined,
            }),
          (e) => `Exception ${e.number} raised`,
        ),
    });

  // ---------------------------------------------------------------- row actions
  const sittingActions = (x: ExamSitting): RowAction[] => {
    const b = boards[x.id];
    const c = b?.conduct ?? null;
    const plan = b?.seating ?? null;
    const a: RowAction[] = [];
    const live = c && !["postponed", "terminated"].includes(c.status) ? c : null;
    // The ONE primary step of the sitting.
    if (!live && x.status === "planned") {
      if (plan?.status === "draft") a.push({ id: "seating-publish", label: "Publish seating", variant: "primary", onClick: () => void run(() => api.publishSeating(plan.id, plan.version), "Seating plan published — candidates see their seats") });
      else a.push({ id: "conduct-open", label: "Open conduct", variant: "primary", onClick: () => void run(() => api.openConduct(x.id), `Conduct of ${x.paperCode} opened`) });
    }
    if (live?.status === "scheduled" && !live.pendingPostpone) a.push({ id: "conduct-start", label: "Start", variant: "primary", onClick: () => void run(() => api.conductCommand(live.id, live.version, "start"), `${x.paperCode} in progress`) });
    if (live?.status === "in_progress" && !live.pendingPostpone) a.push({ id: "conduct-complete", label: "Record attendance", variant: "primary", onClick: () => recordAttendance(x, live) });
    if (live?.status === "suspended" && !live.pendingPostpone) a.push({ id: "conduct-resume", label: "Resume", variant: "primary", onClick: () => void run(() => api.conductCommand(live.id, live.version, "resume"), `${x.paperCode} resumed`) });
    if (live?.pendingPostpone) {
      a.push({ id: "postpone-approve", label: "Approve postponement", variant: "primary", onClick: () => void run(() => api.conductDecision(live.id, live.version, "approve"), `${x.paperCode} postponed`) });
      a.push({ id: "postpone-reject", label: "Reject postponement", onClick: () => ask({ title: `Reject the postponement of ${x.paperCode}`, name: "postpone-reject", fields: [REASON], submitLabel: "Reject", onSubmit: (v) => submit(() => api.conductDecision(live.id, live.version, "reject", v.reason!), "Postponement rejected") }) });
    }
    // Seating (kebab unless it is the primary step).
    if (x.status === "planned" && !live) {
      if (plan?.status === "draft" && a.every((r) => r.id !== "seating-publish")) a.push({ id: "seating-publish", label: "Publish seating", onClick: () => void run(() => api.publishSeating(plan.id, plan.version), "Seating plan published — candidates see their seats") });
      if (!plan || !["draft", "published"].includes(plan.status)) a.push({ id: "seating-generate", label: "Generate seating", onClick: () => void run(() => api.generateSeating(x.id), (p) => `Seating plan drafted for ${p.candidates} candidate(s)`) });
    }
    if (plan?.status === "published" && (!c || c.status !== "completed"))
      a.push({ id: "seating-revise", label: "Revise seating", onClick: () => ask({ title: `Revise the seating of ${x.paperCode}`, name: "seating-revise", fields: [REASON], submitLabel: "Revise", onSubmit: (v) => submit(() => api.reviseSeating(plan.id, plan.version, v.reason!), "Seating plan revised — publish the new draft") }) });
    if (x.status === "planned") a.push({ id: "invigilator-assign", label: "Assign invigilator", onClick: () => assignInvigilator(x.id) });
    if (live?.status === "in_progress")
      a.push({ id: "conduct-suspend", label: "Suspend", onClick: () => ask({ title: `Suspend ${x.paperCode}`, name: "conduct-suspend", fields: [REASON], submitLabel: "Suspend", onSubmit: (v) => submit(() => api.conductCommand(live.id, live.version, "suspend", v.reason!), `${x.paperCode} suspended`) }) });
    if (live && ["scheduled", "in_progress", "suspended"].includes(live.status) && !live.pendingPostpone)
      a.push({
        id: "conduct-postpone",
        label: "Postpone",
        onClick: () =>
          ask({
            title: `Postpone ${x.paperCode}`,
            name: "conduct-postpone",
            submitLabel: "Request postponement",
            fields: [
              { name: "newDate", label: "New date", type: "date", required: true },
              { name: "newStartsAt", label: "Start (HH:MM)", required: true, initial: formatTime(x.startsAt) },
              REASON,
            ],
            onSubmit: (v) => submit(() => api.postpone(live.id, live.version, { newDate: v.newDate!, newStartsAt: v.newStartsAt!, reason: v.reason! }), "Postponement requested — awaiting the contingency decision"),
          }),
      });
    a.push({ id: "exception-raise", label: "Raise exception", onClick: () => raiseException(x.id) });
    return a;
  };

  const dutyActions = (d: InvigilationDuty): RowAction[] => {
    const a: RowAction[] = [];
    if (d.status === "assigned") a.push({ id: "duty-confirm", label: "Confirm", variant: "primary", onClick: () => void run(() => api.dutyCommand(d.id, d.version, "confirm"), "Duty confirmed") });
    if (LIVE_DUTY.includes(d.status)) {
      a.push({
        id: "duty-replace",
        label: "Replace",
        onClick: () =>
          ask({
            title: `Replace ${d.name ?? d.subjectRef}`,
            name: "duty-replace",
            submitLabel: "Replace",
            fields: [
              { name: "staffRef", label: "Staff member", type: "select", wide: true, options: staffOptions },
              { name: "subjectRef", label: "Or sign-in account", placeholder: "dev|name@college.edu" },
              { name: "name", label: "Display name" },
              REASON,
            ],
            onSubmit: (v) => {
              const subjectRef = v.staffRef || v.subjectRef?.trim();
              if (!subjectRef) throw new Error("Choose a staff member or enter a sign-in account");
              return submit(() => api.replaceDuty(d.id, d.version, { subjectRef, name: v.name?.trim() || undefined, reason: v.reason! }), "Duty replaced");
            },
          }),
      });
      a.push({ id: "duty-release", label: "Release", onClick: () => ask({ title: `Release ${d.name ?? d.subjectRef}`, name: "duty-release", fields: [REASON], submitLabel: "Release", onSubmit: (v) => submit(() => api.dutyCommand(d.id, d.version, "release", v.reason!), "Duty released") }) });
    }
    return a;
  };

  const exceptionActions = (e: ExamException): RowAction[] => {
    const a: RowAction[] = [];
    const classify: RowAction = {
      id: "exception-classify",
      label: "Classify",
      variant: e.status === "raised" ? "primary" : undefined,
      onClick: () =>
        ask({
          title: `Classify ${e.number}`,
          name: "exception-classify",
          submitLabel: "Classify",
          fields: [
            { name: "classification", label: "Classification", required: true, placeholder: "Late start" },
            { name: "severity", label: "Severity", type: "select", options: SEVERITIES, initial: e.severity ?? "" },
          ],
          onSubmit: (v) => submit(() => api.classifyException(e.id, e.version, { classification: v.classification!, severity: v.severity || undefined }), `${e.number} classified`),
        }),
    };
    const resolve: RowAction = {
      id: "exception-resolve",
      label: "Resolve",
      variant: "primary",
      onClick: () => ask({ title: `Resolve ${e.number}`, name: "exception-resolve", submitLabel: "Resolve", fields: [{ name: "resolution", label: "Resolution", required: true, wide: true }], onSubmit: (v) => submit(() => api.resolveException(e.id, e.version, v.resolution!), `${e.number} resolved`) }),
    };
    const review: RowAction = { id: "exception-review", label: "Take under review", onClick: () => void run(() => api.exceptionCommand(e.id, e.version, "review"), `${e.number} under review`) };
    if (e.status === "raised") a.push(classify, review);
    if (e.status === "classified") a.push(resolve, review);
    if (e.status === "under_review") a.push(resolve, { ...classify, variant: undefined });
    if (e.status === "resolved") a.push({ id: "exception-close", label: "Close", variant: "primary", onClick: () => void run(() => api.exceptionCommand(e.id, e.version, "close"), `${e.number} closed`) }, { ...review, label: "Reopen review" });
    return a;
  };

  const duties = sittings.flatMap((x) => (boards[x.id]?.staffing?.duties ?? []).map((d) => ({ d, x })));
  const badge = (st: string) => ({ label: statusLabel(st), kind: statusKind(st) });
  const picker = <SessionSelect value={current?.id ?? ""} onChange={setPicked} options={list.map((s) => ({ value: s.id, label: `${s.name} · ${statusLabel(s.status)}` }))} />;

  return (
    <>
      <KpiTile span={4} icon={ClipboardCheck} tone="violet" label="Sittings conducted" value={board.data || !sittings.length ? `${formatNumber(conducted)} / ${formatNumber(sittings.length)}` : "…"} sub={current ? current.name : "No examination session"} data-kpi="exam-conducted" />
      <KpiTile span={4} icon={Users} tone={shortage ? "orange" : "green"} label="Invigilator shortage" value={board.data || !sittings.length ? formatNumber(shortage) : "…"} sub="Against the policy ratio" data-kpi="invigilator-shortage" />
      <KpiTile span={4} icon={ShieldAlert} tone="orange" label="Open exceptions" value={exceptions.data ? formatNumber(openExceptions) : current ? "…" : "—"} sub="Raised, under review or resolved" data-kpi="open-exceptions" />
      <BentoTile span={12} title="Sittings" data-slice="6A" data-conduct="">
        {picker}
        {official ? (
          <DataTable
            caption="Sittings"
            rows={sittings}
            rowKey={(x) => x.id}
            empty={<EmptyState icon={ClipboardCheck} title="No sittings">Add sittings to the timetable under Schedule.</EmptyState>}
            columns={[
              { key: "p", header: "Paper", render: (x) => (
                  <span data-conduct-sitting={x.paperCode}>
                    <b>{x.paperCode}</b> · {x.title}
                  </span>
                ) },
              { key: "d", header: "When", render: (x) => `${fmtDate(x.sitsOn)} · ${timeRange(formatTime(x.startsAt), x.durationMinutes)}` },
              { key: "s", header: "Seating", render: (x) => {
                  const p = boards[x.id]?.seating;
                  return <span data-seating-status={p?.status ?? "none"}>{p ? `${statusLabel(p.status)} · ${plural(p.candidates, "seat")}` : "Not generated"}</span>;
                } },
              { key: "i", header: "Invigilators", render: (x) => {
                  const st = boards[x.id]?.staffing;
                  if (!st) return "…";
                  return <span data-staffing={st.shortage ? "short" : "ok"}>{`${st.assigned} of ${st.required}${st.shortage ? ` · ${st.shortage} short` : ""}`}</span>;
                } },
              { key: "c", header: "Conduct", render: (x) => <span data-conduct-status={boards[x.id]?.conduct?.status ?? "none"}>{conductLine(boards[x.id]?.conduct ?? null)}</span> },
              { key: "x", header: "", render: (x) => <Actions actions={sittingActions(x)} disabled={busy || !board.data} /> },
            ]}
          />
        ) : (
          <EmptyState icon={ClipboardCheck} title={current ? "No official timetable" : "No examination session"}>
            {current ? "Publish the session's timetable under Schedule; its sittings are conducted here." : "Create a session under Sessions & policy first."}
          </EmptyState>
        )}
      </BentoTile>
      <BentoTile
        span={6}
        title="Invigilation"
        data-slice="6A"
        data-invigilation=""
        action={
          sittings.length ? (
            <Button icon={<UserCheck strokeWidth={1.75} />} data-assign-invigilator onClick={() => assignInvigilator()}>
              Assign
            </Button>
          ) : undefined
        }
      >
        {duties.length === 0 ? (
          <EmptyState icon={UserCheck} title="No duties assigned" compact>
            Assign invigilators to each sitting before the examination opens.
          </EmptyState>
        ) : (
          duties.map(({ d, x }) => {
            const { primary, menu } = splitActions(dutyActions(d), busy);
            return (
              <ListRow
                key={d.id}
                data-duty={d.subjectRef}
                avatar={d.name ?? d.subjectRef}
                title={d.name ?? d.subjectRef}
                meta={
                  <>
                    {`${x.paperCode} · ${ROLES.find((r) => r.value === d.role)?.label ?? statusLabel(d.role)} · `}
                    <KeyTag>{d.subjectRef}</KeyTag>
                  </>
                }
                badge={badge(d.status)}
                primary={primary}
                menu={menu}
              />
            );
          })
        )}
      </BentoTile>
      <BentoTile
        span={6}
        title="Exceptions"
        data-slice="6A"
        data-exceptions=""
        action={
          current ? (
            <Button icon={<AlertTriangle strokeWidth={1.75} />} data-raise-exception onClick={() => raiseException()}>
              Raise
            </Button>
          ) : undefined
        }
      >
        {(exceptions.data ?? []).length === 0 ? (
          <EmptyState icon={ShieldAlert} title="No exceptions" compact>
            Record incidents at a sitting — late starts, venue issues, candidate cases.
          </EmptyState>
        ) : (
          (exceptions.data ?? []).map((e) => {
            const { primary, menu } = splitActions(exceptionActions(e), busy);
            const at = sittingOf(e.sittingId);
            return (
              <ListRow
                key={e.id}
                data-exception={e.number}
                icon={AlertTriangle}
                tone={e.severity === "high" || e.severity === "critical" ? "orange" : "grey"}
                title={`${e.number} · ${CATEGORIES.find((c) => c.value === e.category)?.label ?? statusLabel(e.category)}${e.classification ? ` · ${e.classification}` : ""}`}
                meta={`${at ? `${at.paperCode} · ` : ""}${e.resolution ?? e.description}`}
                badge={badge(e.status)}
                primary={primary}
                menu={menu}
              />
            );
          })
        )}
      </BentoTile>
      {prompt}
      {dialog}
    </>
  );
}

