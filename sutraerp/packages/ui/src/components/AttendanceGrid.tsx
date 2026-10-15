import { initialsOf } from "./format";
import { cx } from "./cx";

export type Mark = "present" | "absent" | "late";

export interface AttendanceStudent {
  id: string;
  name: string;
  /** Short label under the avatar (default the first name). */
  short?: string;
}

/** Next mark on tap: present → absent → late → present (prototype quick grid). */
export function nextMark(m: Mark | undefined): Mark {
  return m === "absent" ? "late" : m === "late" ? "present" : "absent";
}

/** Counts of a marks map (unmarked students count as present when `unmarkedAsPresent`). */
export function countMarks(students: AttendanceStudent[], marks: Record<string, Mark | undefined>, unmarkedAsPresent = true) {
  let present = 0;
  let absent = 0;
  let late = 0;
  let unmarked = 0;
  for (const s of students) {
    const m = marks[s.id];
    if (m === "absent") absent++;
    else if (m === "late") late++;
    else if (m === "present") present++;
    else if (unmarkedAsPresent) present++;
    else unmarked++;
  }
  return { present, absent, late, unmarked };
}

/**
 * Tap-to-mark roster (prototype faculty attendance quick grid `.dqgg`): every student is a tile;
 * tap = absent, tap twice = late, a third tap = present. Unmarked tiles render as present
 * unless `showUnmarked`.
 */
export function AttendanceGrid({ students, marks, onChange, disabled, showUnmarked, hint = "Tap anyone who is absent. Tap twice for late. Everyone else is marked present when you submit." }: { students: AttendanceStudent[]; marks: Record<string, Mark | undefined>; onChange: (id: string, mark: Mark) => void; disabled?: boolean; showUnmarked?: boolean; hint?: string | null }) {
  return (
    <div data-attendance-grid="">
      {hint && <p className="sb-att-hint">{hint}</p>}
      <div className="sb-att-grid">
        {students.map((s) => {
          const m = marks[s.id];
          const state = m ?? (showUnmarked ? "unmarked" : "present");
          return (
            <button
              key={s.id}
              type="button"
              className={cx("sb-att", `is-${state}`)}
              disabled={disabled}
              aria-label={`${s.name}: ${state === "unmarked" ? "not marked" : state}`}
              aria-pressed={state === "absent" || state === "late"}
              data-mark={state}
              data-student={s.id}
              onClick={() => onChange(s.id, nextMark(m))}
            >
              <span>{initialsOf(s.name)}</span>
              <small>{s.short ?? s.name.split(/\s+/).find((w) => !/^(dr|prof|mr|ms|mrs)\.?$/i.test(w)) ?? s.name}</small>
              {state === "absent" && <em>A</em>}
              {state === "late" && <em>L</em>}
            </button>
          );
        })}
      </div>
    </div>
  );
}
