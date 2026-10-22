/**
 * Re-skin v2 (R3): student and faculty Timetable (prototype `fac-timetable.png` /
 * `stu-timetable.png`): AreaHeader with the week, 3 KPI tiles, the hour × day `WeekGrid` with the
 * Today pill and a slot `SidePanel`; students keep their per-course attendance table.
 */
import { CalendarClock, CalendarDays, ClipboardCheck, Clock, Layers, Percent } from "lucide-react";
import { AreaHeader, Badge, BentoTile, DataTable, EmptyState, formatDate, formatNumber, KpiTile, SidePanel, WeekGrid, type WeekSlot } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { dkeys, useDeliveryApi } from "../delivery/hooks";
import type { MyAttendanceLine, TimetableSlot } from "../delivery/types";
import { attendanceKind, courseCode, DAY_LONG, hhmm, pctText, sectionLabel, slotHours, weekOf } from "../kit";

function Week({ slots, empty, onOpen }: { slots: TimetableSlot[]; empty: string; onOpen: (s: TimetableSlot) => void }) {
  if (slots.length === 0)
    return (
      <EmptyState icon={CalendarDays} title="No classes scheduled">
        {empty}
      </EmptyState>
    );
  const w = weekOf(slots);
  return <WeekGrid days={w.days} slots={w.grid} todayIndex={w.todayIndex} startHour={w.startHour} endHour={w.endHour} onSlotClick={(g: WeekSlot) => onOpen(slots.find((s) => s.id === g.id)!)} label="Weekly timetable" />;
}

function SlotPanel({ slot, onClose, attendance }: { slot: TimetableSlot | null; onClose: () => void; attendance?: MyAttendanceLine }) {
  if (!slot) return null;
  return (
    <SidePanel
      open
      onClose={onClose}
      eyebrow={`Timetable · ${DAY_LONG[slot.dayOfWeek]}`}
      title={slot.title}
      chip={{ label: courseCode(slot.offeringCode), kind: "info" }}
      facts={[
        { label: "When", value: `${DAY_LONG[slot.dayOfWeek]}, ${hhmm(slot.startsAt)}–${hhmm(slot.endsAt)}` },
        { label: "Room", value: slot.roomLabel ?? "To be announced" },
        { label: "Section", value: sectionLabel(slot.sectionCode) },
        { label: "Faculty", value: slot.facultyNames.join(", ") || "To be announced" },
        ...(attendance && attendance.total > 0 ? [{ label: "Your attendance", value: `${pctText(attendance.percent)} · ${attendance.attended} of ${attendance.total} sessions` }] : []),
      ]}
    />
  );
}

const weekLine = (weekOf?: string) => (weekOf ? `Week of ${formatDate(weekOf)}` : "This week");

export function StudentTimetableScreen() {
  const api = useDeliveryApi();
  const [open, setOpen] = useState<TimetableSlot | null>(null);
  const week = useQuery({ queryKey: dkeys.scoped("me", "timetable"), queryFn: () => api.myTimetable() });
  const att = useQuery({ queryKey: dkeys.scoped("me", "attendance"), queryFn: () => api.myAttendance() });
  const slots = week.data?.items ?? [];
  const lines = att.data?.items ?? [];
  const totals = att.data?.overall;
  const courses = new Set(slots.map((s) => s.offeringId)).size;
  return (
    <>
      <AreaHeader title="Timetable" summary={`${weekLine(week.data?.weekOf)} · ${slots.length} ${slots.length === 1 ? "class" : "classes"} across ${courses} ${courses === 1 ? "course" : "courses"}`} data-slice="4B" />
      <KpiTile span={4} icon={CalendarClock} tone="violet" label="Classes this week" value={formatNumber(slots.length)} sub={`${courses} ${courses === 1 ? "course" : "courses"}`} loading={week.isLoading} />
      <KpiTile span={4} icon={Clock} tone="blue" label="Scheduled hours" value={`${formatNumber(slotHours(slots))} h`} sub="In class this week" loading={week.isLoading} />
      <KpiTile
        span={4}
        icon={Percent}
        tone="green"
        label="Attendance"
        value={totals?.percent != null ? pctText(totals.percent) : "—"}
        sub={totals?.percent != null ? `${totals.attended} of ${totals.total} sessions attended` : "No finalised sessions yet"}
        loading={att.isLoading}
      />
      <BentoTile span={12} data-student-timetable>
        <Week slots={slots} empty="Your classes appear here once your courses have an operational timetable." onOpen={setOpen} />
      </BentoTile>
      <BentoTile span={12} title="My attendance">
        <DataTable
          caption="My attendance"
          rows={lines}
          rowKey={(l) => l.sectionId}
          empty={
            <EmptyState icon={ClipboardCheck} title="No attendance yet" compact>
              Attendance appears once sessions are finalised.
            </EmptyState>
          }
          columns={[
            { key: "c", header: "Course", render: (l) => <b>{l.title}</b> },
            { key: "k", header: "Code", render: (l) => courseCode(l.offeringCode), muted: true },
            { key: "n", header: "Sessions", render: (l) => `${l.attended} / ${l.total}`, muted: true },
            {
              key: "p",
              header: "Attendance",
              render: (l) =>
                l.total === 0 ? (
                  "—"
                ) : l.belowThreshold ? (
                  <Badge kind="attention">{`${pctText(l.percent)} · needs ${l.thresholdPercent}%`}</Badge>
                ) : (
                  <span data-attendance-ok>
                    <Badge kind={attendanceKind(l.percent, l.thresholdPercent)}>{pctText(l.percent)}</Badge>
                  </span>
                ),
            },
          ]}
        />
      </BentoTile>
      <SlotPanel slot={open} onClose={() => setOpen(null)} attendance={open ? lines.find((l) => l.sectionId === open.sectionId) : undefined} />
    </>
  );
}

export function FacultyTimetableScreen() {
  const api = useDeliveryApi();
  const [open, setOpen] = useState<TimetableSlot | null>(null);
  const week = useQuery({ queryKey: dkeys.scoped("my", "timetable"), queryFn: () => api.teachingTimetable() });
  const slots = week.data?.items ?? [];
  const sections = [...new Map(slots.map((s) => [s.sectionId, s])).values()];
  return (
    <>
      <AreaHeader title="Timetable" summary={`${weekLine(week.data?.weekOf)} · ${slots.length} teaching ${slots.length === 1 ? "slot" : "slots"} · ${formatNumber(slotHours(slots))} scheduled hours`} data-slice="4B" />
      <KpiTile span={4} icon={CalendarClock} tone="blue" label="Teaching slots" value={formatNumber(slots.length)} sub="This week" loading={week.isLoading} />
      <KpiTile span={4} icon={Clock} tone="violet" label="Scheduled hours" value={`${formatNumber(slotHours(slots))} h`} sub="Contact hours this week" loading={week.isLoading} />
      <KpiTile
        span={4}
        icon={Layers}
        tone="teal"
        label="Sections"
        value={formatNumber(sections.length)}
        sub={sections.map((s) => `${courseCode(s.offeringCode)} · ${s.sectionCode}`).join(", ") || "None scheduled"}
        loading={week.isLoading}
      />
      <BentoTile span={12} data-faculty-timetable>
        <Week slots={slots} empty="Sections you teach appear here once the timetable is operational." onOpen={setOpen} />
      </BentoTile>
      <SlotPanel slot={open} onClose={() => setOpen(null)} />
    </>
  );
}
