/**
 * Re-skin v2 (R3): faculty Attendance (prototype `fac-attendance.png`, `-dlg-qgsub.png`):
 * class pill switcher, Roster tile with the tap-to-mark `AttendanceGrid` (Quick grid / List),
 * "Today's count" cells, a Submit tile and the confirm dialog. The marks map feeds the same API
 * calls as before: create session → start → capture (If-Match) → complete → finalise.
 */
import { CalendarDays, ClipboardCheck, LayoutGrid, List, Play, Presentation, Send } from "lucide-react";
import { AreaHeader, AttendanceGrid, BentoTile, countMarks, EmptyState, formatWeekdayDate, ListRow, StatCells, StatusBadge, type AttendanceStudent, type Mark } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useEffect, useMemo, useState } from "react";
import { useCommand } from "../../institution/hooks";
import { dkeys, useDeliveryApi } from "../delivery/hooks";
import type { AttendanceMark, TaughtSection, TeachingSession } from "../delivery/types";
import { okeys, useOfferingApi } from "../offering/hooks";
import { courseCode, hhmm, sectionLabel, slotWhen, todayIso, useAsk } from "../kit";

const MARKS: AttendanceMark[] = ["present", "absent", "late", "excused"];
const MARK_LABEL: Record<AttendanceMark, string> = { present: "Present", absent: "Absent", late: "Late", excused: "Excused" };
const OPEN_STATES = ["started", "in_progress", "paused", "completed"];

/** The session to work on: today's open one, else the newest not finalised one. */
function pickSession(sec: TaughtSection | undefined): TeachingSession | undefined {
  if (!sec) return undefined;
  const live = sec.sessions.filter((s) => s.attendanceStatus !== "finalised" && s.status !== "cancelled" && s.status !== "abandoned");
  const today = todayIso();
  return live.find((s) => s.sessionDate === today) ?? [...live].sort((a, b) => `${b.sessionDate}${b.startsAt}`.localeCompare(`${a.sessionDate}${a.startsAt}`))[0];
}

export function FacultyAttendanceScreen() {
  const api = useDeliveryApi();
  const offering = useOfferingApi();
  const { run, submit, busy, dialog } = useCommand();
  const { ask, prompt } = useAsk();
  const q = useQuery({ queryKey: dkeys.scoped("my", "attendance"), queryFn: () => api.facultyAttendance() });
  const students = useQuery({ queryKey: okeys.scoped("my", "students"), queryFn: () => offering.myStudents() });
  const week = useQuery({ queryKey: dkeys.scoped("my", "timetable"), queryFn: () => api.teachingTimetable() });
  const sections = q.data ?? [];
  const [chosen, setChosen] = useState<string>("");
  const [picked, setPicked] = useState<string | null>(null);
  const [mode, setMode] = useState<"grid" | "list">("grid");
  const sec = sections.find((s) => s.sectionId === chosen) ?? sections[0];
  const listed = (picked ? sec?.sessions.find((s) => s.id === picked) : undefined) ?? pickSession(sec);
  // The list carries no roster: read the session itself for its seated records and marks.
  const detail = useQuery({ queryKey: dkeys.scoped(listed?.id ?? "-", "session"), queryFn: () => api.getSession(listed!.id), enabled: !!listed });
  const session = detail.data && detail.data.id === listed?.id ? detail.data : listed;
  const [marks, setMarks] = useState<Record<string, AttendanceMark>>({});
  // Reset the marks whenever another session (or a refreshed version of it) is shown.
  const sig = session ? `${session.id}:${session.version}` : `none:${sec?.sectionId ?? ""}`;
  useEffect(() => {
    setMarks(Object.fromEntries((session?.records ?? []).filter((r) => r.mark).map((r) => [r.studentId, r.mark as AttendanceMark])));
  }, [sig]); // eslint-disable-line react-hooks/exhaustive-deps

  const roster: AttendanceStudent[] = useMemo(() => {
    const seated = (session?.records ?? []).filter((r) => r.seated);
    if (seated.length) return seated.map((r) => ({ id: r.studentId, name: r.displayName }));
    return (students.data ?? []).filter((s) => s.sectionId === sec?.sectionId).map((s) => ({ id: s.studentId, name: s.displayName }));
  }, [session, students.data, sec?.sectionId]);
  const numberOf = useMemo(() => new Map([...(session?.records ?? []).map((r) => [r.studentId, r.studentNumber] as const), ...(students.data ?? []).map((s) => [s.studentId, s.studentNumber] as const)]), [session, students.data]);

  const finalised = session?.attendanceStatus === "finalised";
  const canMark = !!session && !finalised && (session.status === "scheduled" || OPEN_STATES.includes(session.status));
  const gridMarks = Object.fromEntries(Object.entries(marks).filter(([, m]) => m !== "excused")) as Record<string, Mark>;
  const counts = countMarks(roster, gridMarks);
  const excused = Object.values(marks).filter((m) => m === "excused").length;
  const present = counts.present - excused;
  const slot = sec ? (week.data?.items ?? []).filter((s) => s.sectionId === sec.sectionId).sort((a, b) => a.dayOfWeek - b.dayOfWeek || a.startsAt.localeCompare(b.startsAt))[0] : undefined;

  const newSession = (s: TaughtSection) =>
    ask({
      eyebrow: `Attendance · ${courseCode(s.offeringCode)} · ${sectionLabel(s.sectionCode)}`,
      title: "Schedule a session",
      name: "new-session",
      submitLabel: "Schedule",
      columns: 3,
      fields: [
        { name: "sessionDate", label: "Date", type: "date", required: true, initial: todayIso() },
        { name: "startsAt", label: "Starts", type: "time", required: true, initial: slot ? hhmm(slot.startsAt) : "09:00" },
        { name: "endsAt", label: "Ends", type: "time", required: true, initial: slot ? hhmm(slot.endsAt) : "10:00" },
        { name: "topic", label: "Topic", wide: true, placeholder: "For example, Linked lists" },
      ],
      onSubmit: async (v) => {
        const created = await submit(() => api.createSession(s.sectionId, { sessionDate: v.sessionDate!, startsAt: v.startsAt, endsAt: v.endsAt, topic: v.topic || null }), "Session scheduled");
        if (created) setPicked(created.id);
      },
    });

  const records = () => roster.map((st) => ({ studentId: st.id, mark: marks[st.id] ?? ("present" as AttendanceMark) }));
  const confirmSubmit = (s: TeachingSession, title: string) =>
    ask({
      eyebrow: `${courseCode(sec!.offeringCode)} · ${sectionLabel(sec!.sectionCode)} · ${formatWeekdayDate(s.sessionDate)}, ${hhmm(s.startsAt)}`,
      title: `Submit attendance for ${title}?`,
      name: "attendance-submit",
      submitLabel: "Confirm and submit",
      fields: [],
      intro: (
        <>
          <StatCells
            columns={3}
            cells={[
              { label: "Present", value: String(present + excused), kind: "done", sub: excused ? `${excused} excused` : undefined },
              { label: "Absent", value: String(counts.absent), kind: "attention" },
              { label: "Late", value: String(counts.late), kind: "review" },
            ]}
          />
          <p className="ac-ask-note">Once submitted the register is final; changes need a correction request.</p>
        </>
      ),
      onSubmit: async () => {
        const done = await submit(async () => {
          let cur = await api.getSession(s.id);
          if (cur.status === "scheduled") cur = await api.sessionCommand(cur.id, cur.version, "start");
          cur = await api.capture(cur.id, cur.version, records());
          if (cur.status === "started" || cur.status === "in_progress") cur = await api.sessionCommand(cur.id, cur.version, "complete");
          return api.finalise(cur.id, cur.version);
        }, "Attendance submitted");
        if (done) setPicked(done.id);
      },
    });

  if (!q.isLoading && sections.length === 0)
    return (
      <>
        <AreaHeader title="Attendance" summary="No sections to teach yet" data-slice="4B" data-faculty-attendance="" />
        <BentoTile span={12}>
          <EmptyState icon={ClipboardCheck} title="No sections to teach">
            Sections appear here once you hold an active teaching assignment.
          </EmptyState>
        </BentoTile>
      </>
    );

  const summary = sec
    ? `${formatWeekdayDate(session?.sessionDate ?? todayIso())} · ${sec.title} · ${sectionLabel(sec.sectionCode)} · ${sec.seatedCount} ${sec.seatedCount === 1 ? "student" : "students"}`
    : "Loading your sections…";
  return (
    <>
      <AreaHeader title="Attendance" summary={summary} data-slice="4B" data-faculty-attendance="" />
      <div className="ac-classes" role="tablist" aria-label="Classes">
        {sections.map((s) => {
          const sl = (week.data?.items ?? []).find((x) => x.sectionId === s.sectionId);
          const on = s.sectionId === sec?.sectionId;
          return (
            <button
              key={s.sectionId}
              type="button"
              role="tab"
              aria-selected={on}
              className={on ? "ac-class-pill on" : "ac-class-pill"}
              data-class-pill={`${s.offeringCode}/${s.sectionCode}`}
              data-taught-section={s.sectionId}
              onClick={() => {
                setChosen(s.sectionId);
                setPicked(null);
              }}
            >
              <Presentation strokeWidth={1.75} aria-hidden="true" />
              {courseCode(s.offeringCode)} {s.title} · Sec {s.sectionCode}
              <small>
                {sl ? `${slotWhen(sl)} · ` : ""}
                {s.seatedCount} students
              </small>
            </button>
          );
        })}
      </div>
      {sec && (
        <>
          <BentoTile span={8} data-roster="">
            <div className="ac-th">
              <h2>Roster</h2>
              <span className="ac-th-sub">
                {roster.length} {roster.length === 1 ? "student" : "students"}
                {session ? ` · ${formatWeekdayDate(session.sessionDate)}, ${hhmm(session.startsAt)}–${hhmm(session.endsAt)}${session.topic ? ` · ${session.topic}` : ""}` : " · no session scheduled"}
              </span>
              <div className="ac-th-end">
                {session && (
                  <span data-session-state="">
                    <StatusBadge status={finalised ? "finalised" : session.status} />
                  </span>
                )}
                <div className="ac-seg" role="radiogroup" aria-label="Roster view">
                  <button type="button" role="radio" aria-checked={mode === "grid"} className={mode === "grid" ? "on" : ""} onClick={() => setMode("grid")} data-roster-view="grid">
                    <LayoutGrid strokeWidth={1.75} aria-hidden="true" />
                    Quick grid
                  </button>
                  <button type="button" role="radio" aria-checked={mode === "list"} className={mode === "list" ? "on" : ""} onClick={() => setMode("list")} data-roster-view="list">
                    <List strokeWidth={1.75} aria-hidden="true" />
                    List
                  </button>
                </div>
              </div>
            </div>
            {roster.length === 0 ? (
              <EmptyState icon={ClipboardCheck} title="Nobody seated yet" compact>
                Students appear once they take a seat in this section.
              </EmptyState>
            ) : mode === "grid" ? (
              <AttendanceGrid
                students={roster}
                marks={gridMarks}
                disabled={!canMark || busy}
                hint={finalised ? "This register is finalised." : canMark ? undefined : "Schedule today’s session to take attendance."}
                onChange={(id, m) => setMarks((x) => ({ ...x, [id]: m }))}
              />
            ) : (
              <div className="sb-list-rows">
                {roster.map((st) => (
                  <div key={st.id} data-roster-student={numberOf.get(st.id) ?? st.id}>
                    <ListRow
                      avatar={st.name}
                      title={st.name}
                      meta={numberOf.get(st.id)}
                      value={
                        <span className="ac-marks">
                          {MARKS.map((m) => (
                            <button key={m} type="button" className={(marks[st.id] ?? (canMark ? "present" : undefined)) === m ? "ac-mark on" : "ac-mark"} data-mark={m} disabled={!canMark || busy} onClick={() => setMarks((x) => ({ ...x, [st.id]: m }))}>
                              {MARK_LABEL[m]}
                            </button>
                          ))}
                        </span>
                      }
                    />
                  </div>
                ))}
              </div>
            )}
            {roster.length > 0 && (
              <div className="ac-att-foot">
                <span data-att-count="">
                  {counts.absent} absent · {counts.late} late · {present} present{excused ? ` · ${excused} excused` : ""}
                </span>
                {canMark && session && (
                  <button type="button" className="sb-btn" disabled={busy} data-submit-attendance="" onClick={() => confirmSubmit(session, sec.title)}>
                    <Send strokeWidth={1.75} aria-hidden="true" />
                    Mark the rest present and submit
                  </button>
                )}
              </div>
            )}
          </BentoTile>
          <div style={{ gridColumn: "span 4", display: "flex", flexDirection: "column", gap: "var(--sb-gap, 16px)" }}>
            <BentoTile span={12} title="Today’s count" data-today-count="">
              <StatCells
                columns={3}
                cells={[
                  { label: "Present", value: String(roster.length ? present : 0), kind: "done" },
                  { label: "Absent", value: String(counts.absent), kind: "attention" },
                  { label: "Late", value: String(counts.late), kind: "review" },
                ]}
              />
            </BentoTile>
            <BentoTile span={12} title={finalised ? "Submitted" : "Submit"}>
              <div className="ac-stack">
                {!session ? (
                  <>
                    <p className="ac-muted">No open session for this class. Schedule today’s session, then mark and submit.</p>
                    <button type="button" className="sb-btn" data-new-session="" onClick={() => newSession(sec)}>
                      <CalendarDays strokeWidth={1.75} aria-hidden="true" />
                      Start today’s session
                    </button>
                  </>
                ) : finalised ? (
                  <p className="ac-muted">The register for {formatWeekdayDate(session.sessionDate)} is finalised.</p>
                ) : (
                  <>
                    <p className="ac-muted">{session.status === "scheduled" ? "Session scheduled — start it when the class begins." : `Everyone not tapped is marked present.`}</p>
                    {session.status === "scheduled" && (
                      <button type="button" className="sb-btn sb-btn--ghost" disabled={busy} data-session-start="" onClick={() => void run(() => api.sessionCommand(session.id, session.version, "start"), "Session started")}>
                        <Play strokeWidth={1.75} aria-hidden="true" />
                        Start session
                      </button>
                    )}
                    {canMark && (
                      <button type="button" className="sb-btn" disabled={busy} data-submit-attendance="" onClick={() => confirmSubmit(session, sec.title)}>
                        <Send strokeWidth={1.75} aria-hidden="true" />
                        Submit attendance
                      </button>
                    )}
                    {OPEN_STATES.includes(session.status) && (
                      <button type="button" className="sb-btn sb-btn--ghost" disabled={busy || Object.keys(marks).length === 0} data-save-marks="" onClick={() => void run(() => api.capture(session.id, session.version, Object.entries(marks).map(([studentId, mark]) => ({ studentId, mark }))), "Attendance saved")}>
                        Save marks
                      </button>
                    )}
                  </>
                )}
              </div>
            </BentoTile>
          </div>
          <BentoTile span={12} data-sessions="">
            <div className="ac-th">
              <h2>Sessions</h2>
              <span className="ac-th-sub">
                {sec.sessions.length} {sec.sessions.length === 1 ? "session" : "sessions"} · {courseCode(sec.offeringCode)} · {sectionLabel(sec.sectionCode)}
              </span>
              {session && (
                <div className="ac-th-end">
                  <button type="button" className="sb-btn sb-btn--ghost" data-new-session="" onClick={() => newSession(sec)}>
                    New session
                  </button>
                </div>
              )}
            </div>
            {sec.sessions.length === 0 ? (
              <EmptyState icon={CalendarDays} title="No sessions yet" compact>
                Schedule today’s session to take attendance.
              </EmptyState>
            ) : (
              <div className="sb-list-rows">
                {[...sec.sessions]
                  .sort((a, b) => `${b.sessionDate}${b.startsAt}`.localeCompare(`${a.sessionDate}${a.startsAt}`))
                  .map((s) => (
                    <ListRow
                      key={s.id}
                      data-session-row={s.id}
                      icon={CalendarDays}
                      tone={s.attendanceStatus === "finalised" ? "green" : "blue"}
                      title={`${formatWeekdayDate(s.sessionDate)} · ${hhmm(s.startsAt)}–${hhmm(s.endsAt)}`}
                      meta={s.topic ?? "No topic"}
                      selected={s.id === session?.id}
                      badge={{ label: s.attendanceStatus === "finalised" ? "Finalised" : s.attendanceStatus === "not_started" ? (s.status === "scheduled" ? "Scheduled" : "Not marked") : s.attendanceStatus === "capturing" ? "Marking" : "Submitted", kind: s.attendanceStatus === "finalised" ? "done" : "neutral" }}
                      primary={s.id === session?.id ? undefined : { label: "Open", ghost: true, onClick: () => setPicked(s.id), "data-session": s.id }}
                    />
                  ))}
              </div>
            )}
          </BentoTile>
        </>
      )}
      {prompt}
      {dialog}
    </>
  );
}
