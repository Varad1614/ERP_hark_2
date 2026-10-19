import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import type { BadgeKind } from "./Badge";
import { ProgressBar } from "./Cards";
import { cx } from "./cx";
import { IconChip } from "./IconChip";
import type { Tone } from "./tones";

/** Stat cells (prototype `.x-mt` 2×2 "At a glance", `.x-cn` 3-up counts). */
export function StatCells({ cells, columns = 2 }: { cells: Array<{ label: string; value: ReactNode; sub?: ReactNode; kind?: "done" | "attention" | "review" }>; columns?: 2 | 3 | 4 }) {
  return (
    <div className="sb-cells" style={{ ["--cols" as string]: columns }}>
      {cells.map((c) => (
        <div key={c.label} className={c.kind ? `is-${c.kind}` : undefined}>
          {columns === 3 ? (
            <>
              <b>{c.value}</b>
              <span>{c.label}</span>
            </>
          ) : (
            <>
              <span>{c.label}</span>
              <b>{c.value}</b>
              {c.sub && <small>{c.sub}</small>}
            </>
          )}
        </div>
      ))}
    </div>
  );
}

/** Balance tile body (prototype faculty leave `.x-bal`): "8 / 12 days" + bar + caption. */
export function Balance({ icon, tone = "blue", label, used, total, unit = "days", caption }: { icon?: LucideIcon; tone?: Tone; label: string; used: number; total: number; unit?: string; caption?: ReactNode }) {
  const left = Math.max(0, total - used);
  const fmt = (n: number) => String(Number.isInteger(n) ? n : Number(n.toFixed(1)));
  return (
    <div className="sb-balance">
      {icon && <IconChip icon={icon} tone={tone} />}
      <span>{label}</span>
      <b>
        {fmt(left)} <em>/ {fmt(total)} {unit}</em>
      </b>
      <ProgressBar value={total > 0 ? (left / total) * 100 : 0} label={label} />
      {caption && <span>{caption}</span>}
    </div>
  );
}

export interface BarDatum {
  label: string;
  value: number;
  /** Formatted value printed above the bar ("₹92 L"). */
  display?: string;
  /** Lighter bar (month to date, forecast). */
  soft?: boolean;
}

/** Bar chart (prototype monthly collections / `.dch`): value labels on top, month labels below. */
export function BarChart({ data, height = 220, note, label, onSelect }: { data: BarDatum[]; height?: number; note?: ReactNode; label: string; onSelect?: (d: BarDatum) => void }) {
  const max = Math.max(1, ...data.map((d) => d.value));
  return (
    <figure aria-label={label} style={{ margin: 0 }}>
      <div className="sb-bars" style={{ height }}>
        {data.map((d) => {
          const body = (
            <>
              <b>{d.display ?? String(d.value)}</b>
              <i style={{ height: `${Math.max(2, (d.value / max) * 78)}%` }} />
              <small>{d.label}</small>
            </>
          );
          return onSelect ? (
            <button key={d.label} type="button" className={cx(d.soft && "is-soft")} onClick={() => onSelect(d)}>
              {body}
            </button>
          ) : (
            <div key={d.label} className={cx(d.soft && "is-soft")}>
              {body}
            </div>
          );
        })}
      </div>
      {note && <figcaption className="sb-bars-note">{note}</figcaption>}
    </figure>
  );
}

/** Dated list (prototype exam calendar `.x-day`): a date chip per day + its items. */
export function DateList({ days }: { days: Array<{ key: string; day: string; month: string; items: Array<{ label: ReactNode; meta?: ReactNode }> }> }) {
  return (
    <div className="sb-days">
      {days.map((d) => (
        <div key={d.key} className="sb-day">
          <div className="sb-day-chip">
            <b>{d.day}</b>
            <span>{d.month}</span>
          </div>
          <ul>
            {d.items.map((it, i) => (
              <li key={i}>
                {it.meta && <span>{it.meta}</span>}
                {it.label}
              </li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

/** Badge kind → ProgressBar kind helper for status-coloured bars. */
export function barKind(kind: BadgeKind): "accent" | "done" | "attention" | "review" {
  return kind === "done" ? "done" : kind === "attention" ? "attention" : kind === "review" ? "review" : "accent";
}
