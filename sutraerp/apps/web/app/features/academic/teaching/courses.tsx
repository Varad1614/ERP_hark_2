/**
 * Re-skin v2 (R3): student Courses (prototype `stu-courses.png` → `stu-courses-CS201.png`):
 * `CourseCard`s (code chip, faculty · section · credits, attendance from the student's
 * attendance lines) → `/courses?course=<offeringId>` detail (back eyebrow, attendance tile,
 * weekly schedule, coursework, materials). Registration is a header action; offerings open for
 * enrolment are `ListRow`s with Enrol / Join waitlist (same API calls).
 */
import { BookOpen, CalendarDays, ClipboardList, FileText, Layers, Link2 } from "lucide-react";
import { AreaHeader, BentoTile, CardGrid, CourseCard, EmptyState, formatDateTime, ListRow, ProgressBar } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { Link, useSearchParams } from "react-router";
import { statusLabel, useCommand } from "../../institution/hooks";
import { dkeys, useDeliveryApi } from "../delivery/hooks";
import type { MyAttendanceLine } from "../delivery/types";
import { fkeys, useFlexibleApi } from "../flexible/hooks";
import { okeys, useOfferingApi } from "../offering/hooks";
import type { MyCourse } from "../offering/types";
import { courseCode, DAY_SHORT, hhmm, pctText, PlainLink, sectionLabel, toneOf } from "../kit";

const facultyOf = (c: MyCourse) => c.faculty.map((f) => f.facultyName ?? "Faculty").join(", ") || "Faculty to be announced";
const credits = (n: number) => `${n} ${n === 1 ? "credit" : "credits"}`;

export function StudentCoursesScreen() {
  const api = useOfferingApi();
  const delivery = useDeliveryApi();
  const { run, busy, dialog } = useCommand();
  const [params, setParams] = useSearchParams();
  const mine = useQuery({ queryKey: okeys.scoped("me", "courses"), queryFn: () => api.myCourses() });
  const open = useQuery({ queryKey: okeys.scoped("me", "available"), queryFn: () => api.availableCourses() });
  const att = useQuery({ queryKey: dkeys.scoped("me", "attendance"), queryFn: () => delivery.myAttendance() });
  const courses = mine.data ?? [];
  const lines = att.data?.items ?? [];
  const total = courses.reduce((n, c) => n + c.credits, 0);
  const joinable = (open.data ?? []).filter((c) => !c.seated);
  const selected = courses.find((c) => c.offeringId === params.get("course"));
  if (selected) return <CourseDetail course={selected} line={lines.find((l) => l.sectionId === selected.sectionId)} />;
  return (
    <>
      <AreaHeader
        title="Courses"
        summary={`${courses.length} ${courses.length === 1 ? "course" : "courses"} · ${credits(total)}${joinable.length ? ` · ${joinable.length} open for enrolment` : ""}`}
        actions={
          <Link to="/registration" className="sb-btn sb-btn--ghost" data-registration-link="">
            Registration &amp; add/drop
          </Link>
        }
        data-slice="4A"
      />
      {courses.length === 0 ? (
        <BentoTile span={12}>
          <EmptyState icon={BookOpen} title="No courses yet">
            Once your registration is approved, take a seat in an open course below.
          </EmptyState>
        </BentoTile>
      ) : (
        <CardGrid data-my-courses="">
          {courses.map((c) => {
            const l = lines.find((x) => x.sectionId === c.sectionId);
            return (
              <CourseCard
                key={c.enrolmentId}
                data-my-course={c.offeringCode}
                span={4}
                icon={BookOpen}
                tone={toneOf(c.offeringCode)}
                code={courseCode(c.offeringCode, c.courseCode)}
                title={c.title}
                meta={`${facultyOf(c)} · ${sectionLabel(c.sectionCode)} · ${credits(c.credits)}`}
                badge={c.status === "active" ? undefined : { label: statusLabel(c.status), kind: "info" }}
                attendance={l && l.total > 0 ? pctText(l.percent) : "No sessions yet"}
                onClick={() => setParams({ course: c.offeringId })}
              />
            );
          })}
        </CardGrid>
      )}
      {joinable.length > 0 && (
        <BentoTile span={12} data-open-courses="">
          <div className="ac-th">
            <h2>Open for enrolment</h2>
            <span className="ac-th-sub">{joinable.length} courses</span>
          </div>
          <div className="sb-list-rows">
            {joinable.map((c) => {
              const free = c.sections.find((s) => !s.full);
              const primary = !c.covered
                ? undefined
                : free
                  ? { label: `Enrol · Sec ${free.sectionCode}`, disabled: busy, onClick: () => void run(() => api.enrolMe(free.sectionId), `Enrolled in ${c.offeringCode}`), "data-action": "course-enrol" }
                  : c.waitlisted
                    ? undefined
                    : { label: "Join waitlist", ghost: true, disabled: busy, onClick: () => void run(() => api.joinWaitlist(c.offeringId), (w) => `On the waitlist (position ${w.position})`), "data-action": "course-waitlist" };
              return (
                <div key={c.offeringId} data-open-course={c.offeringCode}>
                  <ListRow
                    icon={Layers}
                    tone={toneOf(c.offeringCode)}
                    title={c.title}
                    meta={`${courseCode(c.offeringCode)} · ${credits(c.credits)}`}
                    badge={c.waitlisted ? { label: "On the waitlist", kind: "review" } : c.covered ? { label: "Covered", kind: "done" } : { label: "Not on your registration", kind: "neutral" }}
                    primary={primary}
                  />
                </div>
              );
            })}
          </div>
        </BentoTile>
      )}
      {dialog}
    </>
  );
}

function CourseDetail({ course: c, line }: { course: MyCourse; line?: MyAttendanceLine }) {
  const api = useOfferingApi();
  const delivery = useDeliveryApi();
  const flexible = useFlexibleApi();
  const { run, busy, dialog } = useCommand();
  const week = useQuery({ queryKey: dkeys.scoped("me", "timetable"), queryFn: () => delivery.myTimetable() });
  const work = useQuery({ queryKey: dkeys.scoped("me", "assignments"), queryFn: () => delivery.myAssignments() });
  const lib = useQuery({ queryKey: fkeys.scoped("me", "library"), queryFn: () => flexible.myLibrary() });
  const slots = (week.data?.items ?? []).filter((s) => s.sectionId === c.sectionId).sort((a, b) => a.dayOfWeek - b.dayOfWeek || a.startsAt.localeCompare(b.startsAt));
  const tasks = (work.data ?? []).filter((a) => a.sectionId === c.sectionId);
  const materials = (lib.data ?? []).filter((m) => m.offeringId === c.offeringId);
  const room = slots.map((s) => s.roomLabel).find(Boolean);
  return (
    <>
      <AreaHeader
        title={c.title}
        back={{ label: "Courses", to: "/courses" }}
        linkComponent={PlainLink}
        summary={[courseCode(c.offeringCode, c.courseCode), facultyOf(c), room, credits(c.credits)].filter(Boolean).join(" · ")}
        actions={
          c.status === "enrollment_open" ? (
            <button type="button" className="sb-btn sb-btn--ghost" disabled={busy} data-action="course-drop" onClick={() => void run(() => api.dropMine(c.enrolmentId, c.enrolmentVersion), `${c.offeringCode} dropped`)}>
              Drop course
            </button>
          ) : undefined
        }
        data-slice="4A"
        data-course-detail={c.offeringCode}
      />
      <BentoTile span={8}>
        <div className="ac-th">
          <h2>Weekly schedule</h2>
          <span className="ac-th-end ac-muted">{slots.length} classes a week</span>
        </div>
        {slots.length === 0 ? (
          <EmptyState icon={CalendarDays} title="Not timetabled yet" compact>
            Class times appear once the timetable is operational.
          </EmptyState>
        ) : (
          <div className="sb-list-rows">
            {slots.map((s) => (
              <ListRow key={s.id} icon={CalendarDays} tone={toneOf(c.offeringCode)} title={`${DAY_SHORT[s.dayOfWeek]} · ${hhmm(s.startsAt)}–${hhmm(s.endsAt)}`} meta={[s.roomLabel ?? "Room to be announced", s.facultyNames.join(", ")].filter(Boolean).join(" · ")} />
            ))}
          </div>
        )}
      </BentoTile>
      <BentoTile span={4} title="Attendance" data-course-attendance="">
        {line && line.total > 0 ? (
          <div className="ac-stack">
            <span className="ac-big">{pctText(line.percent)}</span>
            <span className="ac-muted">
              {line.attended} of {line.total} sessions attended
            </span>
            <ProgressBar value={line.percent} kind={line.belowThreshold ? "attention" : "done"} label="Attendance" />
            <span className="ac-muted">Minimum to sit exams: {line.thresholdPercent}%</span>
          </div>
        ) : (
          <EmptyState icon={ClipboardList} title="No sessions yet" compact>
            Attendance appears once sessions are finalised.
          </EmptyState>
        )}
      </BentoTile>
      <BentoTile span={8}>
        <div className="ac-th">
          <h2>Materials</h2>
          <span className="ac-th-end ac-muted">{materials.length} items</span>
        </div>
        {materials.length === 0 ? (
          <EmptyState icon={FileText} title="No materials yet" compact>
            Your faculty’s slides, readings and links appear here.
          </EmptyState>
        ) : (
          <div className="sb-list-rows">
            {materials.map((m) => (
              <ListRow key={m.id} icon={m.kind === "link" ? Link2 : FileText} tone="violet" title={m.title} meta={[statusLabel(m.kind), m.description].filter(Boolean).join(" · ")} primary={{ label: "Open", ghost: true, onClick: () => window.open(m.url, "_blank", "noopener,noreferrer") }} />
            ))}
          </div>
        )}
      </BentoTile>
      <BentoTile span={4}>
        <div className="ac-th">
          <h2>Coursework</h2>
          <span className="ac-th-end ac-muted">{tasks.length}</span>
        </div>
        {tasks.length === 0 ? (
          <EmptyState icon={ClipboardList} title="Nothing assigned" compact />
        ) : (
          <div className="sb-list-rows">
            {tasks.map((a) => (
              <ListRow key={a.id} icon={ClipboardList} tone="amber" title={a.title} meta={`Due ${formatDateTime(a.extendedDueAt ?? a.dueAt)}`} badge={{ label: a.submission ? statusLabel(a.submission.status) : statusLabel(a.status === "closed" ? "closed" : "pending"), kind: a.submission ? "done" : "neutral" }} />
            ))}
          </div>
        )}
        <Link to="/assignments" className="sb-btn sb-btn--ghost" style={{ marginTop: 12 }}>
          All assignments
        </Link>
      </BentoTile>
      {dialog}
    </>
  );
}
