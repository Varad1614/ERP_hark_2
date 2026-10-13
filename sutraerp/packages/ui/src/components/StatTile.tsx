import type { LucideIcon } from "lucide-react";
import { BentoTile, type Span } from "./BentoTile";
import { IconChip } from "./IconChip";
import { Sparkline } from "./Sparkline";
import type { Tone } from "./tones";

export interface StatTileProps {
  icon: LucideIcon;
  tone: Tone;
  label: string;
  value: string;
  hint?: string;
  span?: Span;
  onClick?: () => void;
  /** Real series for the sparkline (oldest first); omitted = no sparkline (never invented). */
  trend?: readonly number[];
  trendLabel?: string;
  /** No backend for this figure yet: muted value and a "pending" tag (e.g. "Arrives in Batch 7"). */
  pending?: string;
  [key: `data-${string}`]: string | undefined;
}

/** KPI tile: icon chip, label, big value, hint, optional sparkline. Clickable when `onClick` is set. */
export function StatTile({ icon, tone, label, value, hint, span = 3, onClick, trend, trendLabel, pending, ...rest }: StatTileProps) {
  return (
    <BentoTile
      as={onClick ? "button" : "div"}
      type={onClick ? "button" : undefined}
      span={span}
      interactive={!!onClick}
      className={pending ? "sb-stat is-pending" : "sb-stat"}
      onClick={onClick}
      {...rest}
    >
      <IconChip icon={icon} tone={tone} />
      <span>{label}</span>
      <b>{value}</b>
      {hint && <small>{hint}</small>}
      {trend && trend.length > 1 && <Sparkline values={trend} color={`var(--tint-${tone}-fg)`} label={trendLabel} />}
      {pending && <i className="sb-pending-tag">{pending}</i>}
    </BentoTile>
  );
}
