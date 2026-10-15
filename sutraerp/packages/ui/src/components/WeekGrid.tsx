import type { CSSProperties, ReactNode } from "react";
import { cx } from "./cx";
import type { Tone } from "./tones";

export interface WeekSlot {
  id: string;
  /** Column index into `days` (0 = first day). */
  day: number;
  /** "09:00" (24 h). */
  start: string;
  end: string;
  title: string;
  /** "09:00–10:00 · B-204" — defaults to the time range. */
  meta?: ReactNode;
  tone?: Tone;
}

const toMin = (t: string) => {
  const m = /^(\d{1,2}):(\d{2})/.exec(t);
  return m ? Number(m[1]) * 60 + Number(m[2]) : 0;
};
const hh = (h: number) => `${String(h).padStart(2, "0")}:00`;

/**
 * Week timetable (prototype `.x-tt`): hour rows × day columns, coloured blocks, "Today" pill.
 * Half-hour resolution; each hour is 60 px. Blocks are buttons when `onSlotClick` is set.
 */
export function WeekGrid({ days, slots, todayIndex, startHour = 9, endHour = 17, onSlotClick, label = "Week timetable" }: { days: string[]; slots: WeekSlot[]; todayIndex?: number; startHour?: number; endHour?: number; onSlotClick?: (slot: WeekSlot) => void; label?: string }) {
  const rows = Math.max(1, (endHour - startHour) * 2);
  const style: CSSProperties = { gridTemplateColumns: `52px repeat(${days.length}, minmax(112px, 1fr))`, gridTemplateRows: `40px repeat(${rows}, 30px)` };
  const rowOf = (t: string) => Math.min(rows + 2, Math.max(2, 2 + Math.round((toMin(t) - startHour * 60) / 30)));
  return (
    <div className="sb-week-wrap" role="region" aria-label={label}>
      <div className="sb-week" style={style}>
        {days.map((d, i) => (
          <div key={`h-${d}`} className={cx("sb-week-dh", i === todayIndex && "on")} style={{ gridColumn: i + 2 }}>
            {d}
            {i === todayIndex && <span>Today</span>}
          </div>
        ))}
        {days.map((d, i) => (
          <div key={`c-${d}`} className={cx("sb-week-col", i === todayIndex && "on")} style={{ gridColumn: i + 2, gridRow: `2 / ${rows + 2}` }} />
        ))}
        {Array.from({ length: endHour - startHour }, (_, h) => (
          <div key={`t-${h}`} className="sb-week-tm" style={{ gridRow: 2 + h * 2 }}>
            {hh(startHour + h)}
          </div>
        ))}
        {slots
          .filter((s) => s.day >= 0 && s.day < days.length)
          .map((s) => {
            const from = rowOf(s.start);
            const to = Math.max(from + 1, rowOf(s.end));
            const tone = s.tone ?? "blue";
            const blockStyle: CSSProperties = { gridColumn: s.day + 2, gridRow: `${from} / ${to}`, background: `var(--tint-${tone}-bg)`, color: `var(--tint-${tone}-fg)` };
            const body = (
              <>
                <b>{s.title}</b>
                <span>{s.meta ?? `${s.start}–${s.end}`}</span>
              </>
            );
            return onSlotClick ? (
              <button key={s.id} type="button" className="sb-week-blk" style={blockStyle} onClick={() => onSlotClick(s)} data-slot={s.id} data-rows={`${from}-${to}`}>
                {body}
              </button>
            ) : (
              <div key={s.id} className="sb-week-blk" style={blockStyle} data-slot={s.id} data-rows={`${from}-${to}`}>
                {body}
              </div>
            );
          })}
      </div>
    </div>
  );
}
