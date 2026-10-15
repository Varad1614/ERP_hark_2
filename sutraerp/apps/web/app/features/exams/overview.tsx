/**
 * Re-skin v2 (R4) — Admin → Examinations frame and Overview (prototype `adm-examinations.png`):
 *
 *   AreaHeader   "Examinations" + live summary (session · dates · papers) | Overview · Schedule ·
 *                Marks · Results · More ▾ (Registrations & hall tickets, Malpractice, Revaluation,
 *                Sessions & policy)
 *   KpiTile ×3   Papers scheduled · Halls in use · Mark sheets verified
 *   DateList     the session's examination calendar (sittings of the live timetable by day)
 *   ApprovalCard "Result verification": evaluated mark sheets (Verify) and validated result
 *                batches awaiting approval (Approve / Reject)
 *   DataTable    hall and seating plan (venue allocations of the live timetable)
 *
 * Every figure comes from the exams API (no invented numbers).
 */
import { CalendarCheck, CalendarDays, CheckCircle2, ClipboardCheck, GraduationCap, Inbox, Sofa } from "lucide-react";
import { ApprovalCard, ApprovalList, AreaHeader, BentoGrid, BentoTile, DataTable, DateList, EmptyState, FormSelect, KeyTag, KpiTile, formatDateShort, formatNumber, formatTime, plural, type NavLinkLikeProps } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import type { ReactNode } from "react";
import { Link, NavLink } from "react-router";
import { useSession as useAuthSession } from "../../lib/auth";
import { REASON } from "../institution/forms";
import { selectInstitution, statusLabel, useCommand, useInstitutions } from "../institution/hooks";
import type { Institution } from "../institution/types";
import { activeTab, EXAM_TABS, examinationsPath, type TabDef } from "./index";
import { mkeys, useMarksApi } from "./marks/hooks";
import { rkeys, useResultsApi } from "./results/hooks";
import { ekeys, useExamSetupApi } from "./setup/hooks";
import type { ExamSchedule, ExamSession } from "./setup/types";
import { timeRange, useExamAsk } from "./ui";

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const LIVE_SCHEDULE = ["published", "active", "approved", "under_review", "draft"];
const VERIFIED = ["validated", "approved", "finalised"];
/** Right-aligned muted note of a tile header (prototype "21–27 Sep"). */
export const NOTE = { color: "var(--sb-mute-2)", fontSize: 13, fontWeight: 500 } as const;

/** "2026-09-21".."2026-09-27" → "21–27 Sep"; across months "28 Sep – 3 Oct". */
export function dateSpan(from?: string | null, to?: string | null): string {
  if (!from && !to) return "";
  if (!from || !to || from === to) return formatDateShort(from ?? to);
  const [y1, m1, d1] = from.split("-").map(Number);
  const [y2, m2, d2] = to.split("-").map(Number);
  if (y1 === y2 && m1 === m2) return `${d1}–${d2} ${MONTHS[(m1 ?? 1) - 1]}`;
  return `${formatDateShort(from)} – ${formatDateShort(to)}`;
}

/** The operational session (active / reopened), else the newest one. */
export function currentSession(list: readonly ExamSession[]): ExamSession | undefined {
  return list.find((s) => ["active", "reopened"].includes(s.status)) ?? list.find((s) => s.status === "scheduled") ?? list[0];
}

/** The timetable that is in force (published first, else the one being prepared). */
export function liveSchedule(list: readonly ExamSchedule[]): ExamSchedule | undefined {
  for (const st of LIVE_SCHEDULE) {
    const hit = list.find((s) => s.status === st);
    if (hit) return hit;
  }
  return undefined;
}

/** Sessions of the institution (shared query key with the Sessions & policy tab). */
export function useExamSessions(institution: Institution | null) {
  const api = useExamSetupApi();
  const inst = institution?.id ?? "-";
  return useQuery({ queryKey: ekeys.scoped(inst, "sessions"), queryFn: () => api.sessions(inst), enabled: !!institution });
}

function TabLink({ to, className, end, children }: NavLinkLikeProps) {
  const key = to.split("/")[2] ?? "overview";
  return (
    <NavLink to={to} end={end} className={className} data-tab-link={key}>
      {children}
    </NavLink>
  );
}

/** Live one-line summary of the header: "End-semester examinations · 21–27 Sep · 14 papers". */
function useSummary(institution: Institution | null, loading: boolean): ReactNode {
  const sessions = useExamSessions(institution);
  if (!institution) return loading ? "Loading institutions…" : "No institution registered yet.";
  if (!sessions.data) return "Loading examination sessions…";
  const s = currentSession(sessions.data);
  if (!s) return "No examination session yet · create one under Sessions & policy";
  const span = dateSpan(s.startsOn, s.endsOn);
  return [s.name, span, plural(s.papers.length, "paper"), s.status === "active" ? null : statusLabel(s.status)].filter(Boolean).join(" · ");
}

/**
 * Frame of Admin → Examinations: administrator gate, AreaHeader (≤5 tabs incl. More) and the
 * institution picker when several institutions exist; `children(institution, tab)` renders the tab.
 */
export function ExamsFrame({ tab, children }: { tab: string | undefined; children: (institution: Institution | null, tab: TabDef) => ReactNode }) {
  const { role } = useAuthSession();
  const current = activeTab(EXAM_TABS, tab);
  const { list, current: institution, query } = useInstitutions();
  const summary = useSummary(institution, query.isLoading);
  if (role !== "administrator")
    return (
      <BentoGrid data-screen="examinations">
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Examinations is for administrators.</p>
        </BentoTile>
      </BentoGrid>
    );
  return (
    <BentoGrid data-screen="examinations" data-tab={current.key}>
      <AreaHeader
        title="Examinations"
        summary={<span data-current-institution={institution?.id}>{summary}</span>}
        tabs={EXAM_TABS.map((t) => ({ id: t.key, label: t.label, to: examinationsPath(t.key) }))}
        activeTab={current.key}
        linkComponent={TabLink}
        tabsLabel="Examinations sections"
        actions={
          list.length > 1 && institution ? (
            <div style={{ minWidth: 220 }}>
              <FormSelect label="Institution" hideLabel name="institution" value={institution.id} options={list.map((i) => ({ value: i.id, label: i.name }))} onChange={(e) => selectInstitution(e.target.value)} />
            </div>
          ) : undefined
        }
      />
      {children(institution, current)}
    </BentoGrid>
  );
}

/** Admin → Examinations → Overview. */
export function ExamsOverview({ institution }: { institution: Institution | null }) {
  const setup = useExamSetupApi();
  const marksApi = useMarksApi();
  const resultsApi = useResultsApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useExamAsk();
  const inst = institution?.id ?? "-";
  const sessions = useExamSessions(institution);
  const session = currentSession(sessions.data ?? []);
  const sid = session?.id ?? "-";
  const schedules = useQuery({ queryKey: ekeys.scoped(sid, "schedules"), queryFn: () => setup.schedules(sid), enabled: !!session });
  const papers = useQuery({ queryKey: mkeys(inst, "papers"), queryFn: () => marksApi.institutionPapers(inst), enabled: !!institution });
  const batches = useQuery({ queryKey: rkeys.scoped(sid, "batches"), queryFn: () => resultsApi.batches(sid), enabled: !!session });
  if (!institution)
    return (
      <BentoTile span={12} data-slice="R4">
        <EmptyState icon={Inbox} title="No institution selected">
          Register an institution in Campus setup → Institution first.
        </EmptyState>
      </BentoTile>
    );

  const live = liveSchedule(schedules.data ?? []);
  const sittings = [...(live?.sittings ?? [])].filter((x) => x.status !== "cancelled").sort((a, b) => a.sitsOn.localeCompare(b.sitsOn) || a.startsAt.localeCompare(b.startsAt));
  const sheets = (papers.data ?? []).filter((p) => !session || p.sessionId === session.id);
  const withSheet = sheets.filter((p) => p.sheetStatus);
  const verified = withSheet.filter((p) => VERIFIED.includes(p.sheetStatus ?? "")).length;
  const toVerify = sheets.filter((p) => p.sheetStatus === "evaluated");
  const batch = (batches.data ?? []).find((b) => b.status === "validated" && b.processInstanceId);
  // Candidates per paper (mark-sheet listing) → "Allotted" = candidates seated, never more than the hall's seats.
  const candidates = new Map((papers.data ?? []).map((p) => [p.paperId, p.candidates]));
  const allocations = sittings.flatMap((x) => {
    const live = x.venues.filter((v) => v.status !== "released");
    return live.map((v) => ({ ...v, sitting: x, allotted: Math.min(v.seats, Math.ceil((candidates.get(x.paperId) ?? v.seats) / live.length)) }));
  });
  const halls = new Set(allocations.map((a) => a.venueId));
  const seats = allocations.reduce((n, a) => n + a.allotted, 0);
  const days = new Map<string, typeof sittings>();
  for (const x of sittings) days.set(x.sitsOn, [...(days.get(x.sitsOn) ?? []), x]);
  const loading = !sessions.data;

  const verify = (paperId: string, code: string) =>
    run(async () => {
      const m = await marksApi.marks(paperId);
      if (!m.sheet) throw new Error(`No mark sheet for ${code}`);
      return marksApi.sheetCommand(m.sheet.id, m.sheet.version, "verify");
    }, `${code}: marks verified`);

  return (
    <>
      <KpiTile
        span={4}
        icon={CalendarCheck}
        tone="violet"
        label="Papers scheduled"
        value={loading ? "…" : formatNumber(live ? sittings.length : (session?.papers.length ?? 0))}
        sub={live ? `${plural(days.size, "exam day")} · timetable ${statusLabel(live.status).toLowerCase()}` : session ? `${plural(session.papers.length, "paper")} · no timetable yet` : "No examination session"}
        loading={loading}
        data-kpi="exam-papers-scheduled"
      />
      <KpiTile span={4} icon={Sofa} tone="green" label="Halls in use" value={loading ? "…" : formatNumber(halls.size)} sub={`${formatNumber(seats)} seats allotted`} loading={loading} data-kpi="exam-halls" />
      <KpiTile
        span={4}
        icon={CheckCircle2}
        tone="green"
        label="Mark sheets verified"
        value={papers.data ? `${formatNumber(verified)} / ${formatNumber(withSheet.length)}` : "…"}
        sub={toVerify.length ? `${plural(toVerify.length, "sheet")} waiting for verification` : "Semester mark sheets"}
        loading={!papers.data}
        data-kpi="exam-verified"
      />
      <BentoTile span={8} title="Examination calendar" action={session ? <span style={NOTE}>{dateSpan(session.startsOn, session.endsOn) || session.name}</span> : undefined} data-exam-calendar="" data-slice="R4">
        {sittings.length ? (
          <DateList
            days={[...days.entries()].map(([date, list]) => {
              const [, m, d] = date.split("-").map(Number);
              return {
                key: date,
                day: String(d),
                month: MONTHS[(m ?? 1) - 1] ?? "",
                items: list.map((x) => ({ meta: timeRange(formatTime(x.startsAt), x.durationMinutes), label: <strong data-calendar-sitting={x.paperCode} style={{ fontWeight: 500 }}>{`${x.paperCode} · ${x.title}`}</strong> })),
              };
            })}
          />
        ) : (
          <EmptyState icon={CalendarDays} title={session ? "No sittings scheduled yet" : "No examination session yet"} compact action={<Link to={examinationsPath(session ? "schedule" : "sessions")} className="sb-btn sb-btn--ghost">{session ? "Plan the timetable" : "Create a session"}</Link>}>
            {session ? "Sittings of the session's timetable appear here by day." : "Create a session under Sessions & policy, then plan its timetable."}
          </EmptyState>
        )}
      </BentoTile>
      <BentoTile span={4} title="Result verification" data-verification="">
        {toVerify.length || batch ? (
          <ApprovalList>
            {toVerify.map((p) => (
              <ApprovalCard
                key={p.paperId}
                icon={ClipboardCheck}
                tone="violet"
                title={`${p.paperCode} · ${p.title}`}
                meta={[plural(p.candidates, "student"), p.sections.length ? `Section ${p.sections.join(", ")}` : null].filter(Boolean).join(" · ")}
                approveLabel="Verify"
                busy={busy}
                onApprove={() => void verify(p.paperId, p.paperCode)}
                data-verify={p.paperCode}
              />
            ))}
            {batch && session && (
              <ApprovalCard
                icon={GraduationCap}
                tone="green"
                title={`Results · ${session.name}`}
                meta={`${plural(batch.computedCount, "result")} computed · ${formatNumber(batch.withheldCount)} withheld`}
                busy={busy}
                onApprove={() => void run(() => resultsApi.decideBatch(batch.id, batch.version, "approve"), "Results approved")}
                onReject={() => ask({ title: "Send the results back", name: "batch-reject", submitLabel: "Reject", fields: [REASON], onSubmit: (v) => submit(() => resultsApi.decideBatch(batch.id, batch.version, "reject", v.reason), "Results sent back") })}
                data-batch-approval=""
              />
            )}
          </ApprovalList>
        ) : (
          <EmptyState icon={CheckCircle2} title="Nothing waiting for verification" compact>
            Mark sheets submitted by faculty appear here to verify.
          </EmptyState>
        )}
      </BentoTile>
      {allocations.length > 0 && (
        <BentoTile span={12} title="Hall and seating plan" action={<span style={NOTE}>Seats allotted per sitting</span>} data-seating-plan="">
          <DataTable
            caption="Hall and seating plan"
            rows={allocations}
            rowKey={(a) => a.id}
            columns={[
              { key: "hall", header: "Hall", render: (a) => <b>{a.venueName}</b> },
              { key: "cap", header: "Capacity", render: (a) => formatNumber(a.capacity) },
              { key: "seats", header: "Allotted", render: (a) => <b>{formatNumber(a.allotted)}</b> },
              { key: "paper", header: "Paper", render: (a) => `${a.sitting.paperCode} · ${a.sitting.title}`, muted: true },
              { key: "when", header: "Sitting", render: (a) => `${formatDateShort(a.sitting.sitsOn)} · ${timeRange(formatTime(a.sitting.startsAt), a.sitting.durationMinutes)}` },
              { key: "code", header: "Venue code", render: (a) => <KeyTag>{a.venueCode}</KeyTag> },
            ]}
          />
        </BentoTile>
      )}
      {prompt}
      {dialog}
    </>
  );
}
