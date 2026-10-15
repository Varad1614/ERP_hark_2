/**
 * Re-skin v2 (R3): faculty My classes (prototype `fac-classes.png` → class detail
 * `fac-classes-CS201*.png`) and My students (`fac-students.png`, `-dlg-fa-stu.png`).
 *
 * - `/classes`: `ClassCard`s (code chip, students, next session from the teaching timetable,
 *   role, status) → `/classes?class=<sectionId>` detail with back eyebrow and Roster / Materials /
 *   Sessions tabs.
 * - `/my-students`: header search, `NameCell` table with section and attendance badge (section
 *   attendance summaries), `SidePanel` with the facts and a link to the read-only Student 360.
 */
import { ArrowRight, CalendarDays, ChevronRight, ExternalLink, FileText, Link2, Presentation, Users } from "lucide-react";
import { AreaHeader, Badge, BentoTile, CardGrid, ClassCard, DataTable, EmptyState, formatWeekdayDate, Initials, ListRow, NameCell, SidePanel, type Tone } from "@sutra/ui";
import { useQueries, useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { Link, useSearchParams } from "react-router";
import { statusLabel } from "../../institution/hooks";
import { dkeys, useDeliveryApi } from "../delivery/hooks";
import type { AttendanceSummary, TimetableSlot } from "../delivery/types";
import { fkeys, useFlexibleApi } from "../flexible/hooks";
import { okeys, useOfferingApi } from "../offering/hooks";
import type { MyClass, MyStudent } from "../offering/types";
import { useSyllabus } from "./syllabus";
import { attendanceKind, courseCode, hhmm, nextSlot, pctText, PlainLink, sectionLabel, slotWhen, toneOf } from "../kit";

const CARD_TONES: Tone[] = ["blue", "violet", "teal", "orange", "rose", "green"];
const roleLabel = (r: string) => r.replace(/_/g, " ").replace(/^./, (c) => c.toUpperCase());

/** Attendance summaries of the given sections (one query each). */
function useSummaries(sectionIds: string[]) {
  const api = useDeliveryApi();
  const qs = useQueries({ queries: sectionIds.map((id) => ({ queryKey: dkeys.scoped(id, "attendance-summary"), queryFn: () => api.summary(id) })) });
  const bySection = new Map<string, AttendanceSummary>();
  qs.forEach((q, i) => q.data && bySection.set(sectionIds[i]!, q.data));
  return bySection;
}

function AttendanceBadge({ percent, total, threshold }: { percent?: number; total?: number; threshold?: number }) {
  if (percent === undefined || !total) return <span className="ac-muted">—</span>;
  return <Badge kind={attendanceKind(percent, threshold)}>{pctText(percent)}</Badge>;
}

// ================================================================== My classes

export function FacultyClassesScreen() {
  const api = useOfferingApi();
  const delivery = useDeliveryApi();
  const [params, setParams] = useSearchParams();
  const q = useQuery({ queryKey: okeys.scoped("my", "classes"), queryFn: () => api.myClasses() });
  const week = useQuery({ queryKey: dkeys.scoped("my", "timetable"), queryFn: () => delivery.teachingTimetable() });
  const rows = q.data ?? [];
  const syllabus = useSyllabus(rows.map((c) => c.sectionId));
  const open = params.get("class");
  const current = rows.find((c) => c.sectionId === open);
  if (open && current) return <ClassDetail cls={current} slots={(week.data?.items ?? []).filter((s) => s.sectionId === current.sectionId)} />;
  const students = rows.reduce((n, c) => n + c.seatedCount, 0);
  const courses = new Set(rows.map((c) => c.offeringId)).size;
  return (
    <>
      <AreaHeader
        title="My classes"
        summary={`${rows.length} ${rows.length === 1 ? "section" : "sections"} · ${courses} ${courses === 1 ? "course" : "courses"} · ${students} ${students === 1 ? "student" : "students"}`}
        data-slice="4A"
      />
      {rows.length === 0 ? (
        <BentoTile span={12}>
          <EmptyState icon={Presentation} title="No active teaching assignment">
            Your classes appear once an administrator activates your assignment.
          </EmptyState>
        </BentoTile>
      ) : (
        <CardGrid data-my-classes="">
          {rows.map((c, i) => {
            const slots = (week.data?.items ?? []).filter((s) => s.sectionId === c.sectionId);
            const next = nextSlot(slots);
            const syl = syllabus.get(c.sectionId);
            return (
              <ClassCard
                key={c.sectionId}
                data-my-class={`${c.offeringCode}/${c.sectionCode}`}
                icon={Presentation}
                tone={CARD_TONES[i % CARD_TONES.length]}
                code={`${courseCode(c.offeringCode)} · ${sectionLabel(c.sectionCode)}`}
                title={c.title}
                meta={`${c.seatedCount} ${c.seatedCount === 1 ? "student" : "students"} · ${roleLabel(c.role)}`}
                badge={c.offeringStatus === "active" ? undefined : { label: statusLabel(c.offeringStatus), kind: "info" }}
                facts={[
                  { label: "Next session", value: next ? slotWhen(next) : "Not timetabled yet" },
                  ...(syl ? [] : [{ label: "Weekly slots", value: slots.length ? `${slots.length} · ${slots.map((s) => s.roomLabel).filter(Boolean)[0] ?? "room TBA"}` : "—" }]),
                ]}
                progress={syl ? { label: `Syllabus · ${syl.done} of ${syl.total} units`, value: syl.percent } : undefined}
                onClick={() => setParams({ class: c.sectionId })}
              />
            );
          })}
        </CardGrid>
      )}
    </>
  );
}

type DetailTab = "roster" | "materials" | "sessions";

function ClassDetail({ cls, slots }: { cls: MyClass; slots: TimetableSlot[] }) {
  const offering = useOfferingApi();
  const delivery = useDeliveryApi();
  const flexible = useFlexibleApi();
  const [tab, setTab] = useState<DetailTab>("roster");
  const students = useQuery({ queryKey: okeys.scoped("my", "students"), queryFn: () => offering.myStudents() });
  const summaries = useSummaries([cls.sectionId]);
  const sessions = useQuery({ queryKey: dkeys.scoped("my", "attendance"), queryFn: () => delivery.facultyAttendance(), enabled: tab === "sessions" });
  const resources = useQuery({ queryKey: fkeys.scoped("my", "resources"), queryFn: () => flexible.myResources(), enabled: tab === "materials" });
  const roster = (students.data ?? []).filter((s) => s.sectionId === cls.sectionId);
  const summary = summaries.get(cls.sectionId);
  const next = nextSlot(slots);
  const taught = (sessions.data ?? []).find((s) => s.sectionId === cls.sectionId);
  const items = (resources.data?.items ?? []).filter((r) => r.sectionId === cls.sectionId || (!r.sectionId && r.offeringId === cls.offeringId));
  return (
    <>
      <AreaHeader
        title={`${cls.title} · ${sectionLabel(cls.sectionCode)}`}
        back={{ label: "My classes", to: "/classes" }}
        linkComponent={PlainLink}
        summary={[courseCode(cls.offeringCode), `${cls.seatedCount} students`, next ? `Next: ${slotWhen(next)}` : null].filter(Boolean).join(" · ")}
        tabs={[
          { id: "roster", label: "Roster" },
          { id: "materials", label: "Materials" },
          { id: "sessions", label: "Sessions" },
        ]}
        activeTab={tab}
        onTabChange={(t) => setTab(t as DetailTab)}
        data-slice="4A"
        data-class-detail={`${cls.offeringCode}/${cls.sectionCode}`}
      />
      {tab === "roster" && (
        <BentoTile span={12}>
          <div className="ac-th">
            <h2>Roster</h2>
            <span className="ac-th-end ac-muted">{roster.length} students</span>
          </div>
          <DataTable
            caption="Roster"
            rows={roster}
            rowKey={(s) => s.studentId}
            empty={
              <EmptyState icon={Users} title="Nobody seated yet" compact>
                Students appear once they take a seat in this section.
              </EmptyState>
            }
            columns={[
              { key: "n", header: "Name", render: (s) => <b data-roster-student={s.studentNumber}>{s.displayName}</b> },
              { key: "r", header: "Roll no.", render: (s) => s.studentNumber, muted: true },
              {
                key: "a",
                header: "Attendance",
                render: (s) => {
                  const l = summary?.items.find((x) => x.studentId === s.studentId);
                  return <AttendanceBadge percent={l?.percent} total={l?.total} threshold={summary?.thresholdPercent} />;
                },
              },
              {
                key: "s",
                header: "Sessions",
                render: (s) => {
                  const l = summary?.items.find((x) => x.studentId === s.studentId);
                  return l && l.total ? `${l.attended} of ${l.total}` : "—";
                },
                muted: true,
              },
            ]}
          />
        </BentoTile>
      )}
      {tab === "materials" && (
        <BentoTile span={12}>
          <div className="ac-th">
            <h2>Materials</h2>
            <span className="ac-th-sub">{items.length} items</span>
            <div className="ac-th-end">
              <Link to="/resources" className="sb-btn sb-btn--ghost">
                Manage in Resources
                <ArrowRight strokeWidth={1.75} aria-hidden="true" />
              </Link>
            </div>
          </div>
          {items.length === 0 ? (
            <EmptyState icon={FileText} title="No materials yet" compact>
              Upload slides, handouts or links from Resources.
            </EmptyState>
          ) : (
            <div className="sb-list-rows">
              {items.map((r) => (
                <ListRow
                  key={r.id}
                  icon={r.kind === "link" ? Link2 : FileText}
                  tone={toneOf(r.kind)}
                  title={r.title}
                  meta={[statusLabel(r.kind), r.description].filter(Boolean).join(" · ")}
                  badge={r.status === "published" ? { label: "Visible to students", kind: "info" } : { label: statusLabel(r.status), kind: "neutral" }}
                  primary={{ label: "Open", ghost: true, icon: ExternalLink, onClick: () => window.open(r.url, "_blank", "noopener,noreferrer") }}
                />
              ))}
            </div>
          )}
        </BentoTile>
      )}
      {tab === "sessions" && (
        <BentoTile span={12}>
          <div className="ac-th">
            <h2>Sessions</h2>
            <span className="ac-th-sub">{taught?.sessions.length ?? 0} sessions</span>
            <div className="ac-th-end">
              <Link to="/attendance" className="sb-btn">
                Take attendance
              </Link>
            </div>
          </div>
          {!taught || taught.sessions.length === 0 ? (
            <EmptyState icon={CalendarDays} title="No sessions yet" compact>
              Schedule a session from Attendance.
            </EmptyState>
          ) : (
            <div className="sb-list-rows">
              {[...taught.sessions]
                .sort((a, b) => `${b.sessionDate}${b.startsAt}`.localeCompare(`${a.sessionDate}${a.startsAt}`))
                .map((s) => {
                  const present = s.records.filter((r) => r.mark === "present" || r.mark === "late").length;
                  return (
                    <ListRow
                      key={s.id}
                      icon={CalendarDays}
                      tone={s.attendanceStatus === "finalised" ? "green" : "blue"}
                      title={`${formatWeekdayDate(s.sessionDate)} · ${hhmm(s.startsAt)}–${hhmm(s.endsAt)}`}
                      meta={[s.topic, s.attendanceStatus === "finalised" ? `${present} of ${s.records.length} present` : null].filter(Boolean).join(" · ") || "No topic"}
                      badge={{ label: statusLabel(s.attendanceStatus === "finalised" ? "finalised" : s.status), kind: s.attendanceStatus === "finalised" ? "done" : "neutral" }}
                    />
                  );
                })}
            </div>
          )}
        </BentoTile>
      )}
    </>
  );
}

// ================================================================== My students

interface StudentRow {
  studentId: string;
  displayName: string;
  studentNumber: string;
  sections: MyStudent[];
}

export function FacultyStudentsScreen() {
  const api = useOfferingApi();
  const [search, setSearch] = useState("");
  const [open, setOpen] = useState<StudentRow | null>(null);
  const q = useQuery({ queryKey: okeys.scoped("my", "students"), queryFn: () => api.myStudents() });
  const seats = q.data ?? [];
  const summaries = useSummaries([...new Set(seats.map((s) => s.sectionId))]);
  // One row per student (a student seated in two of my sections is listed once).
  const byStudent = new Map<string, StudentRow>();
  for (const s of seats) {
    const r = byStudent.get(s.studentId) ?? { studentId: s.studentId, displayName: s.displayName, studentNumber: s.studentNumber, sections: [] };
    r.sections.push(s);
    byStudent.set(s.studentId, r);
  }
  const rows = [...byStudent.values()];
  const linesOf = (r: StudentRow) => r.sections.map((s) => ({ seat: s, line: summaries.get(s.sectionId)?.items.find((x) => x.studentId === r.studentId) }));
  const totals = (r: StudentRow) => linesOf(r).reduce((a, { line }) => ({ attended: a.attended + (line?.attended ?? 0), total: a.total + (line?.total ?? 0), below: a.below || !!line?.belowThreshold }), { attended: 0, total: 0, below: false });
  const threshold = [...summaries.values()][0]?.thresholdPercent ?? 75;
  const below = rows.filter((r) => totals(r).below).length;
  const needle = search.trim().toLowerCase();
  const shown = needle ? rows.filter((r) => `${r.displayName} ${r.studentNumber} ${r.sections.map((s) => s.offeringCode).join(" ")}`.toLowerCase().includes(needle)) : rows;
  const ot = open ? totals(open) : null;
  return (
    <>
      <AreaHeader
        title="Students"
        summary={`${needle ? `${shown.length} of ` : ""}${rows.length} ${rows.length === 1 ? "student" : "students"} in your sections · ${below} below the ${threshold}% attendance minimum`}
        search={{ label: "Search name or roll no.", value: search, onChange: setSearch }}
        data-slice="4A"
      />
      <BentoTile span={12}>
        <DataTable
          caption="My students"
          rows={shown}
          rowKey={(r) => r.studentId}
          empty={
            <EmptyState icon={Users} title={needle ? "No match" : "No students yet"} compact>
              {needle ? "Try another name or roll number." : "Students appear once they take a seat in one of your sections."}
            </EmptyState>
          }
          columns={[
            {
              key: "n",
              header: "Name",
              render: (r) => (
                <Link to={`/students/record/${r.studentId}`} data-my-student={r.studentNumber} className="sb-name-cell">
                  <Initials name={r.displayName} tone="accent" />
                  <span>{r.displayName}</span>
                </Link>
              ),
            },
            { key: "num", header: "Roll no.", render: (r) => r.studentNumber, muted: true },
            { key: "c", header: "Classes", render: (r) => r.sections.map((s) => `${courseCode(s.offeringCode)} · Sec ${s.sectionCode}`).join(", "), muted: true },
            {
              key: "a",
              header: "Attendance",
              render: (r) => {
                const t = totals(r);
                return <AttendanceBadge percent={t.total ? (t.attended / t.total) * 100 : undefined} total={t.total} threshold={t.below ? 101 : threshold} />;
              },
            },
            {
              key: "o",
              header: "",
              render: (r) => (
                <button type="button" className="sb-kebab" aria-label={`Details of ${r.displayName}`} data-row-open="" onClick={() => setOpen(r)}>
                  <ChevronRight strokeWidth={1.75} aria-hidden="true" />
                </button>
              ),
            },
          ]}
        />
      </BentoTile>
      {open && ot && (
        <SidePanel
          open
          onClose={() => setOpen(null)}
          eyebrow="Students · Profile"
          title={open.displayName}
          chip={ot.below ? { label: "Below attendance minimum", kind: "attention" } : { label: `${open.sections.length} ${open.sections.length === 1 ? "class" : "classes"} with you`, kind: "info" }}
          facts={[
            { label: "Roll no.", value: open.studentNumber },
            ...linesOf(open).map(({ seat, line }) => ({
              label: `${courseCode(seat.offeringCode)} · ${sectionLabel(seat.sectionCode)}`,
              value: line && line.total ? `${pctText(line.percent)} · ${line.attended} of ${line.total} sessions` : "No finalised sessions yet",
            })),
          ]}
          actions={
            <Link to={`/students/record/${open.studentId}`} className="sb-btn">
              Open Student 360
            </Link>
          }
        >
          <NameCell name={open.displayName} sub={open.studentNumber} />
        </SidePanel>
      )}
    </>
  );
}
