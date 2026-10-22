/**
 * Live Home tiles (UI re-skin, prototype layout): the dark "Right now" card, the four KPI tiles
 * and the announcement, each read from the API that exists today (mock APIs in mock mode).
 * A figure whose backend has not landed yet renders as a pending tile ("Arrives in Batch N"),
 * never as an invented number. Sparklines only draw real series.
 */
import { ChevronRight, GraduationCap, Inbox, Megaphone, PencilLine, Presentation, UserCheck, Users, Wallet, Award, Wrench } from "lucide-react";
import { BentoTile, IconChip, StatTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { Children, type ReactElement, type ReactNode } from "react";
import { useDeliveryApi, dkeys } from "../academic/delivery/hooks";
import { pctText } from "../academic/kit";
import type { TimetableSlot } from "../academic/delivery/types";
import { formatInr } from "../finance";
import { pkeys, usePaymentsApi } from "../finance/payments/hooks";
import { akeys, useAnalyticsApi } from "../reports/analytics/hooks";
import { useInstitutions } from "../institution/hooks";
import { useWorkforceApi, wkeys } from "../people/workforce/hooks";
import { mkeys, useMarksApi } from "../exams/marks/hooks";
import { toReviewCount } from "../exams/marks";
import { rkeys as resultKeys, useResultsApi } from "../exams/results/hooks";
import { latestResult } from "../exams/results";
import { useSession } from "../../lib/auth";
import { inboxKeys, useInbox } from "../../lib/inbox";
import type { Role } from "../../lib/roles";

type Go = (to: string) => void;

/** One tile of a KPI group (customizable Home: each metric is its own tile), or all of them. */
function pick(all: ReactElement<{ children?: ReactNode }>, only: number | undefined): ReactNode {
  return only === undefined ? all : (Children.toArray(all.props.children)[only] ?? null);
}

/** ISO weekday (1 = Monday … 7 = Sunday) of a date. */
export function isoDay(d: Date): number {
  return d.getDay() === 0 ? 7 : d.getDay();
}

const hhmm = (t: string) => t.slice(0, 5);
const nowHHMM = (d: Date) => `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;

/**
 * Agenda of the "Right now" card from a weekly timetable: the class in progress (or the next one
 * today), then the following slots of the week in order.
 */
export function agendaFrom(slots: readonly TimetableSlot[], now: Date = new Date()): { current: TimetableSlot | null; live: boolean; later: { slot: TimetableSlot; day: number }[] } {
  const today = isoDay(now);
  const t = nowHHMM(now);
  const ordered = [...slots].sort((a, b) => a.dayOfWeek - b.dayOfWeek || a.startsAt.localeCompare(b.startsAt));
  const rank = (s: TimetableSlot) => ((s.dayOfWeek - today + 7) % 7) * 10000 + Number(hhmm(s.startsAt).replace(":", ""));
  const upcoming = ordered.filter((s) => !(s.dayOfWeek === today && hhmm(s.endsAt) <= t)).sort((a, b) => rank(a) - rank(b));
  const current = upcoming[0] ?? null;
  const live = !!current && current.dayOfWeek === today && hhmm(current.startsAt) <= t && t < hhmm(current.endsAt);
  return { current, live, later: upcoming.slice(1, 3).map((slot) => ({ slot, day: slot.dayOfWeek })) };
}

const DAY = ["", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function DarkCard({ label, time, title, sub, rows, state }: { label: string; time: string; title: string; sub: string; rows: [string, string, string][]; state: string }) {
  return (
    <BentoTile span={5} variant="dark" className="sb-agenda" data-right-now={state}>
      <div className="sb-agenda-label">
        <span>{label}</span>
        <span>{time}</span>
      </div>
      <h3>{title}</h3>
      <small>{sub}</small>
      <div className="sb-agenda-next">
        {rows.map(([when, what, where]) => (
          <div key={`${when}${what}`} className="sb-agenda-item">
            <time>{when}</time>
            {what}
            <span>{where}</span>
          </div>
        ))}
      </div>
    </BentoTile>
  );
}

function TimetableNow({ role }: { role: "faculty" | "student" }) {
  const api = useDeliveryApi();
  const q = useQuery({
    queryKey: role === "faculty" ? dkeys.scoped("my", "timetable") : dkeys.scoped("me", "timetable"),
    queryFn: () => (role === "faculty" ? api.teachingTimetable() : api.myTimetable()),
  });
  const now = new Date();
  if (q.isLoading) return <DarkCard label="Right now" time={nowHHMM(now)} title="Loading your timetable…" sub="" rows={[]} state="loading" />;
  if (q.isError) return <DarkCard label="Right now" time={nowHHMM(now)} title="Timetable unavailable" sub="Try again shortly." rows={[]} state="error" />;
  const { current, live, later } = agendaFrom(q.data?.items ?? [], now);
  if (!current)
    return <DarkCard label="Right now" time={nowHHMM(now)} title="No classes scheduled" sub="Your week appears here once a timetable is published." rows={[]} state="empty" />;
  const today = isoDay(now);
  const dayLabel = (d: number) => (d === today ? "" : `${DAY[d]} `);
  return (
    <DarkCard
      label={live ? "Right now" : "Up next"}
      time={`${dayLabel(current.dayOfWeek)}${hhmm(current.startsAt)}`}
      title={current.title}
      sub={[`${current.offeringCode} · Section ${current.sectionCode}`, current.roomLabel, role === "student" ? current.facultyNames.join(", ") : null].filter(Boolean).join(" · ")}
      rows={later.map(({ slot, day }) => [`${dayLabel(day)}${hhmm(slot.startsAt)}`.trim(), slot.title, slot.roomLabel ?? `Section ${slot.sectionCode}`])}
      state="ready"
    />
  );
}

/** Administrator: the next due open to-dos (there is no meetings calendar backend yet). */
function AdminNow() {
  const { role } = useSession();
  const inbox = useInbox();
  const q = useQuery({ queryKey: inboxKeys.todos(role, "open"), queryFn: () => inbox.todos("open") });
  const now = new Date();
  const due = (q.data?.items ?? []).filter((t) => t.status === "open" && t.dueAt).sort((a, b) => (a.dueAt ?? "").localeCompare(b.dueAt ?? ""));
  const fmt = (iso: string) => new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short" }).format(new Date(iso));
  if (q.isLoading) return <DarkCard label="Right now" time={nowHHMM(now)} title="Loading…" sub="" rows={[]} state="loading" />;
  const first = due[0];
  if (!first)
    return (
      <DarkCard
        label="Right now"
        time={nowHHMM(now)}
        title="Nothing due right now"
        sub={q.data?.openCount ? `${q.data.openCount} open to-do${q.data.openCount === 1 ? "" : "s"} without a due date` : "Your queue is clear"}
        rows={[]}
        state="empty"
      />
    );
  return (
    <DarkCard
      label="Next due"
      time={fmt(first.dueAt!)}
      title={first.title}
      sub={first.detail || "From your to-dos"}
      rows={due.slice(1, 3).map((t) => [fmt(t.dueAt!), t.title, t.overdue ? "Overdue" : "To-do"])}
      state="ready"
    />
  );
}

export function RightNowTile({ role }: { role: Role }) {
  return role === "administrator" ? <AdminNow /> : <TimetableNow role={role} />;
}

/* ------------------------------------------------------------------ KPIs */

/** Figure value of `key` from the services metrics catalogue (decimal string), `undefined` when not reported. */
export function metricValue(items: readonly { key: string; value: string }[] | undefined, key: string): string | undefined {
  return items?.find((m) => m.key === key)?.value;
}

/** `12345` → `12,345` (whole numbers; anything else unchanged). */
const count = (v: string) => (/^-?\d+(\.0+)?$/.test(v) ? Number(v).toLocaleString("en-IN") : v);

/**
 * 9E: administrator KPIs from `GET /api/v1/services/institutions/{id}/metrics` (every domain's
 * `OperationalMetricsPort`) — students enrolled, fee collection, open work orders — plus faculty
 * on roll from the staff records. A figure no domain reports renders as a pending tile.
 */
export function AdminKpis({ go, only }: { go: Go; only?: number }) {
  const { current: institution, query: instQ } = useInstitutions();
  const inst = institution?.id ?? "-";
  const analytics = useAnalyticsApi();
  const metrics = useQuery({ queryKey: akeys(inst, "metrics"), queryFn: () => analytics.metrics(institution!.id), enabled: !!institution });
  const workforce = useWorkforceApi();
  const staff = useQuery({ queryKey: wkeys.scoped(inst, "staff:"), queryFn: () => workforce.staff(institution!.id), enabled: !!institution });
  const faculty = (staff.data ?? []).filter((s) => s.staffCategory === "teaching" && s.engagement?.status === "active");
  const pendingStaff = (staff.data ?? []).filter((s) => s.engagement?.status === "pending").length;
  const noInst = !instQ.isLoading && !institution;
  const m = metrics.data;
  const enrolled = metricValue(m, "student.enrolled_students") ?? metricValue(m, "student.active_students");
  const active = metricValue(m, "student.active_students");
  const collected = metricValue(m, "finance.payments_collected");
  const outstanding = metricValue(m, "finance.receivables_outstanding");
  const workOrders = metricValue(m, "campus.open_work_orders");
  const pastSla = metricValue(m, "campus.tickets_past_sla");
  const loadingOr = (v: string | undefined, f: (x: string) => string) => (v !== undefined ? f(v) : noInst ? "—" : metrics.isLoading ? "…" : "—");
  const pendingIf = (v: string | undefined) => (m && v === undefined ? "Not reported yet" : undefined);
  const inr = (v: string) => formatInr(v).replace(/\.00$/, "");
  const all = (
    <>
      <StatTile
        icon={GraduationCap}
        tone="orange"
        label="Students enrolled"
        value={loadingOr(enrolled, count)}
        hint={noInst ? "No institution yet" : metrics.isError ? "Figures unavailable" : active ? `${count(active)} active student records` : "Student records"}
        pending={pendingIf(enrolled)}
        onClick={() => go("students")}
        data-kpi="students"
      />
      <StatTile
        icon={Users}
        tone="green"
        label="Faculty on roll"
        value={staff.data ? faculty.length.toLocaleString("en-IN") : noInst ? "—" : "…"}
        hint={noInst ? "No institution yet" : staff.isError ? "Staff records unavailable" : pendingStaff ? `${pendingStaff} joining (engagement pending)` : "Teaching staff, engagement active"}
        onClick={() => go("people")}
        data-kpi="faculty"
      />
      <StatTile
        icon={Wallet}
        tone="violet"
        label="Fee collection"
        value={loadingOr(collected, inr)}
        hint={outstanding !== undefined ? `${inr(outstanding)} outstanding` : metrics.isError ? "Figures unavailable" : "Payments collected"}
        pending={pendingIf(collected)}
        onClick={() => go("finance/receivables")}
        data-kpi="fees"
      />
      <StatTile
        icon={Wrench}
        tone="blue"
        label="Open work orders"
        value={loadingOr(workOrders, count)}
        hint={pastSla !== undefined ? `${count(pastSla)} service ticket(s) past SLA` : "Campus maintenance"}
        pending={pendingIf(workOrders)}
        onClick={() => go("campus/facilities")}
        data-kpi="work-orders"
      />
    </>
  );
  return pick(all, only);
}

export function FacultyKpis({ go, only }: { go: Go; only?: number }) {
  const api = useDeliveryApi();
  const tt = useQuery({ queryKey: dkeys.scoped("my", "timetable"), queryFn: () => api.teachingTimetable() });
  const taught = useQuery({ queryKey: dkeys.scoped("my", "attendance"), queryFn: () => api.facultyAttendance() });
  const marksApi = useMarksApi();
  const assessments = useQuery({ queryKey: mkeys("me", "assessments"), queryFn: () => marksApi.myAssessments() });
  const reviewPapers = (assessments.data ?? []).filter((x) => x.acceptsMarks && x.candidates > x.entered);
  const today = isoDay(new Date());
  const todays = (tt.data?.items ?? []).filter((s) => s.dayOfWeek === today).sort((a, b) => a.startsAt.localeCompare(b.startsAt));
  const sections = taught.data ?? [];
  const seated = sections.reduce((n, s) => n + s.seatedCount, 0);
  // Attendance of finalised sessions, oldest first: share of marked records that are present/late.
  const sessions = sections
    .flatMap((s) => s.sessions)
    .filter((x) => x.records.some((r) => r.mark))
    .sort((a, b) => `${a.sessionDate}${a.startsAt}`.localeCompare(`${b.sessionDate}${b.startsAt}`));
  const pct = (recs: { mark?: string | null }[]) => {
    const marked = recs.filter((r) => r.mark);
    return marked.length ? Math.round((marked.filter((r) => r.mark === "present" || r.mark === "late").length / marked.length) * 100) : 0;
  };
  const series = sessions.slice(-7).map((x) => pct(x.records));
  const avg = sessions.length ? pct(sessions.flatMap((x) => x.records)) : null;
  const all = (
    <>
      <StatTile
        icon={Presentation}
        tone="orange"
        label="Classes today"
        value={tt.data ? String(todays.length) : "…"}
        hint={todays[0] ? `First at ${hhmm(todays[0].startsAt)}` : tt.data ? "No classes today" : "Timetable"}
        onClick={() => go("classes")}
        data-kpi="classes"
      />
      <StatTile icon={Users} tone="green" label="My students" value={taught.data ? String(seated) : "…"} hint={`Across ${sections.length} section${sections.length === 1 ? "" : "s"}`} onClick={() => go("my-students")} data-kpi="students" />
      <StatTile
        icon={Inbox}
        tone="violet"
        label="To review"
        value={assessments.data ? String(toReviewCount(assessments.data)) : assessments.isError ? "—" : "…"}
        hint={assessments.data ? (reviewPapers[0] ? `${reviewPapers[0].paperCode}${reviewPapers.length > 1 ? ` +${reviewPapers.length - 1} more` : ""} · marks to enter` : "No marks pending") : "Examination marks"}
        onClick={() => go("assessments")}
        data-kpi="review"
      />
      <StatTile
        icon={UserCheck}
        tone="blue"
        label="Avg attendance"
        value={avg === null ? (taught.data ? "—" : "…") : `${avg}%`}
        hint={avg === null ? "No marked sessions yet" : `${sessions.length} marked session${sessions.length === 1 ? "" : "s"}`}
        trend={series.length > 1 ? series : undefined}
        trendLabel={series.length > 1 ? `Last ${series.length} sessions` : undefined}
        onClick={() => go("attendance")}
        data-kpi="attendance"
      />
    </>
  );
  return pick(all, only);
}

export function StudentKpis({ go, only }: { go: Go; only?: number }) {
  const api = useDeliveryApi();
  const payments = usePaymentsApi();
  const att = useQuery({ queryKey: dkeys.scoped("me", "attendance"), queryFn: () => api.myAttendance() });
  const asg = useQuery({ queryKey: dkeys.scoped("me", "assignments"), queryFn: () => api.myAssignments() });
  const acc = useQuery({ queryKey: pkeys.scoped("me", "accounts"), queryFn: () => payments.myAccounts() });
  const resultsApi = useResultsApi();
  const results = useQuery({ queryKey: resultKeys.scoped("me", "results"), queryFn: () => resultsApi.myResults() });
  const latest = results.data ? latestResult(results.data) : undefined;
  const lines = att.data?.items ?? [];
  // One server-side definition (MyAttendance.overall), the same figure the Timetable shows.
  const overall = att.data?.overall;
  const below = lines.filter((l) => l.belowThreshold).length;
  const now = Date.now();
  const week = now + 7 * 86_400_000;
  const due = (asg.data ?? []).filter((a) => {
    const at = new Date(a.extendedDueAt ?? a.dueAt).getTime();
    return at >= now && at <= week && !a.submission?.submittedAt;
  });
  const outstanding = (acc.data ?? []).map((a) => a.account.balance.outstanding);
  const owes = outstanding.find((o) => /[1-9]/.test(o));
  const nextDue = (acc.data ?? []).flatMap((a) => a.charges).filter((c) => c.dueDate && /[1-9]/.test(c.outstandingAmount)).sort((a, b) => (a.dueDate ?? "").localeCompare(b.dueDate ?? ""))[0];
  const all = (
    <>
      <StatTile
        icon={UserCheck}
        tone="orange"
        label="Attendance"
        value={overall ? (overall.percent != null ? pctText(overall.percent) : "—") : "…"}
        hint={overall ? (overall.percent != null ? (below ? `Below the minimum in ${below} course${below === 1 ? "" : "s"}` : `${overall.attended} of ${overall.total} sessions`) : "No sessions recorded yet") : "My courses"}
        onClick={() => go("timetable")}
        data-kpi="attendance"
      />
      <StatTile
        icon={Award}
        tone="green"
        label="CGPA"
        value={results.data ? (latest?.cgpa ?? "—") : results.isError ? "—" : "…"}
        hint={results.data ? (latest?.sgpa ? `This term SGPA ${latest.sgpa}` : "No published results yet") : "Results and grade cards"}
        onClick={() => go("results")}
        data-kpi="cgpa"
      />
      <StatTile
        icon={PencilLine}
        tone="violet"
        label="Due this week"
        value={asg.data ? String(due.length) : "…"}
        hint={asg.data ? (due[0] ? `Next: ${due[0].title}` : "Nothing due") : "Assignments"}
        onClick={() => go("assignments")}
        data-kpi="assignments"
      />
      <StatTile
        icon={Wallet}
        tone="blue"
        label="Fees"
        value={acc.data ? (owes ? formatInr(owes).replace(/\.00$/, "") : "Paid") : acc.isError ? "—" : "…"}
        hint={acc.data ? (owes ? (nextDue?.dueDate ? `Next due ${new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short" }).format(new Date(nextDue.dueDate))}` : "Outstanding") : "Nothing outstanding") : acc.isError ? "Fees unavailable" : "My fees"}
        onClick={() => go("fees")}
        data-kpi="fees"
      />
    </>
  );
  return pick(all, only);
}

/* ------------------------------------------------------------------ announcement */

export function AnnouncementTile() {
  const { role } = useSession();
  const inbox = useInbox();
  const navigate = useNavigate();
  const q = useQuery({ queryKey: inboxKeys.notices(role, "current"), queryFn: () => inbox.notices("current") });
  const items = [...(q.data?.items ?? [])].sort((a, b) => Number(b.pinned) - Number(a.pinned) || b.publishAt.localeCompare(a.publishAt));
  const n = items[0];
  const when = (iso: string) => new Intl.DateTimeFormat("en-IN", { day: "numeric", month: "short" }).format(new Date(iso));
  return (
    <BentoTile className="sb-notice" data-announcement={n ? "ready" : q.isLoading ? "loading" : "empty"}>
      <IconChip icon={Megaphone} tone="green" size="md" />
      <div>
        <b>{n ? n.title : q.isLoading ? "Loading announcements…" : "No announcements right now"}</b>
        <small>{n ? `${n.authorName} · ${when(n.publishAt)}${items.length > 1 ? ` · ${items.length - 1} more` : ""}` : "Notices for your role appear here."}</small>
      </div>
      <button type="button" className="sb-icon-btn" aria-label="All notices" onClick={() => navigate("/notices")}>
        <ChevronRight strokeWidth={1.75} />
      </button>
    </BentoTile>
  );
}

