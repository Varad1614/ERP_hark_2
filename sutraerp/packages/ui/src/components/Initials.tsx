import type { ReactNode } from "react";
import { cx } from "./cx";
import { initialsOf, toneFor } from "./format";
import { toneStyle, type Tone } from "./tones";

export type InitialsSize = "sm" | "md" | "lg" | "xl";

/**
 * Initials avatar (prototype `.x-av` 36px round, `.x-pav` 56/96px rounded square). `tone="accent"`
 * uses the role accent (people tables); a Tone gives the pastel person-card look; default is a
 * stable tone derived from the name.
 */
export function Initials({ name, size = "sm", tone, className }: { name: string; size?: InitialsSize; tone?: Tone | "accent"; className?: string }) {
  const t = tone ?? toneFor(name);
  return (
    <span className={cx("sb-initials", `sb-initials--${size}`, t === "accent" && "sb-initials--accent", className)} style={t === "accent" ? undefined : toneStyle(t)} aria-hidden="true">
      {initialsOf(name)}
    </span>
  );
}

/** Table name cell: initials avatar + name (prototype students table). Clickable when `onClick` is set. */
export function NameCell({ name, sub, onClick, tone = "accent" }: { name: string; sub?: ReactNode; onClick?: () => void; tone?: Tone | "accent" }) {
  const body = (
    <>
      <Initials name={name} tone={tone} />
      <span>
        {name}
        {sub && <small style={{ display: "block", color: "var(--sb-mute-2)", fontWeight: 500, fontSize: 12.5 }}>{sub}</small>}
      </span>
    </>
  );
  return onClick ? (
    <button type="button" className="sb-name-cell" onClick={onClick}>
      {body}
    </button>
  ) : (
    <span className="sb-name-cell">{body}</span>
  );
}
