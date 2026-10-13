/**
 * Live Home tiles (UI re-skin, prototype layout): the dark "Right now" card, the four KPI tiles
 * and the announcement, each read from the API that exists today (mock APIs in mock mode).
 * A figure whose backend has not landed yet renders as a pending tile ("Arrives in Batch N"),
 * never as an invented number. Sparklines only draw real series.
 */
import { ChevronRight, GraduationCap, Inbox, Megaphone, PencilLine, Presentation, UserCheck, Users, Wallet, Award } from "lucide-react";
import { BentoTile, IconChip, StatTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useNavigate } from "react-router";
import { useDeliveryApi, dkeys } from "../academic/delivery/hooks";
import type { TimetableSlot } from "../academic/delivery/types";
import { formatInr } from "../finance";
import { pkeys, usePaymentsApi } from "../finance/payments/hooks";
import { useInstitutions } from "../institution/hooks";
import { rkeys, useRecordApi } from "../student/record/hooks";
import { useSession } from "../../lib/auth";
import { inboxKeys, useInbox } from "../../lib/inbox";
import type { Role } from "../../lib/roles";

type Go = (to: string) => void;

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

export function AdminKpis({ go }: { go: Go }) {
  const { current: institution, query: instQ } = useInstitutions();
  const inst = institution?.id ?? "-";
  const record = useRecordApi();
  const payments = usePaymentsApi();
  const students = useQuery({ queryKey: rkeys.scoped(inst, "directory", ""), queryFn: () => record.directory(institution!.id), enabled: !!institution });
  const recv = useQuery({ queryKey: pkeys.scoped(inst, "receivables"), queryFn: () => payments.receivables(institution!.id), enabled: !!institution });
  const noInst = !instQ.isLoading && !institution;
  return (
    <>
      <StatTile
        icon={GraduationCap}
        tone="orange"
        label="Students"
        value={students.data ? students.data.length.toLocaleString("en-IN") : noInst ? "—" : "…"}
        hint={noInst ? "No institution yet" : students.isError ? "Directory unavailable" : "In the student directory"}
        onClick={() => go("students")}
        data-kpi="students"
      />
      <StatTile icon={Users} tone="green" label="Faculty on roll" value="—" hint="Staff records" pending="Arrives in Batch 7" onClick={() => go("people")} data-kpi="faculty" />
      <StatTile
        icon={Wallet}
        tone="violet"
        label="Fees outstanding"
        value={recv.data ? formatInr(recv.data.totalOutstanding).replace(/\.00$/, "") : noInst ? "—" : "…"}
        hint={recv.data ? `${formatInr(recv.data.totalOverdue).replace(/\.00$/, "")} overdue` : recv.isError ? "Receivables unavailable" : "Receivables"}
        onClick={() => go("finance/receivables")}
        data-kpi="fees"
      />
      <StatTile icon={UserCheck} tone="blue" label="Attendance today" value="—" hint="Campus-wide" pending="Arrives in Batch 9" onClick={() => go("reports")} data-kpi="attendance" />
    </>
  );
}

export function FacultyKpis({ go }: { go: Go }) {
  const api = useDeliveryApi();
  const tt = useQuery({ queryKey: dkeys.scoped("my", "timetable"), queryFn: () => api.teachingTimetable() });
  const taught = useQuery({ queryKey: dkeys.scoped("my", "attendance"), queryFn: () => api.facultyAttendance() });
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
  return (
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
      <StatTile icon={Inbox} tone="violet" label="To review" value="—" hint="Assessment submissions" pending="Arrives in Batch 6" onClick={() => go("assessments")} data-kpi="review" />
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
}

export function StudentKpis({ go }: { go: Go }) {
  const api = useDeliveryApi();
  const payments = usePaymentsApi();
  const att = useQuery({ queryKey: dkeys.scoped("me", "attendance"), queryFn: () => api.myAttendance() });
  const asg = useQuery({ queryKey: dkeys.scoped("me", "assignments"), queryFn: () => api.myAssignments() });
  const acc = useQuery({ queryKey: pkeys.scoped("me", "accounts"), queryFn: () => payments.myAccounts() });
  const lines = att.data ?? [];
  const attended = lines.reduce((n, l) => n + l.attended, 0);
  const total = lines.reduce((n, l) => n + l.total, 0);
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
  return (
    <>
      <StatTile
        icon={UserCheck}
        tone="orange"
        label="Attendance"
        value={att.data ? (total ? `${Math.round((attended / total) * 100)}%` : "—") : "…"}
        hint={att.data ? (total ? (below ? `Below the minimum in ${below} course${below === 1 ? "" : "s"}` : `${attended} of ${total} sessions`) : "No sessions recorded yet") : "My courses"}
        onClick={() => go("timetable")}
        data-kpi="attendance"
      />
      <StatTile icon={Award} tone="green" label="CGPA" value="—" hint="Results and grade cards" pending="Arrives in Batch 6" onClick={() => go("results")} data-kpi="cgpa" />
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

