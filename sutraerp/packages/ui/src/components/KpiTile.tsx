import type { LucideIcon } from "lucide-react";
import { BentoTile, type Span } from "./BentoTile";
import { ProgressBar } from "./Cards";
import { cx } from "./cx";
import { IconChip } from "./IconChip";
import { Sparkline } from "./Sparkline";
import type { Tone } from "./tones";

export interface KpiTileProps {
  icon: LucideIcon;
  tone: Tone;
  label: string;
  /** Already formatted (formatINR / formatNumber / formatDays). */
  value: string;
  /** Sub line ("Since 1 April", "Across 3 instalment cycles"). */
  sub?: string;
  /** Real daily series, oldest first (never invented) → sparkline + `trendLabel`. */
  trend?: readonly number[];
  /** Sparkline caption (default "7 days"). */
  trendLabel?: string;
  /** Progress 0–100 instead of a sparkline (budget used, capacity). */
  progress?: number;
  span?: Span;
  onClick?: () => void;
  /** Shows a muted value while loading. */
  loading?: boolean;
  [key: `data-${string}`]: string | undefined;
}

/** KPI tile (prototype `.k` + `.dspw`): icon chip, label, 30/800 value, sub, sparkline with "7 days". */
export function KpiTile({ icon, tone, label, value, sub, trend, trendLabel = "7 days", progress, span = 3, onClick, loading, ...rest }: KpiTileProps) {
  return (
    <BentoTile as={onClick ? "button" : "div"} type={onClick ? "button" : undefined} span={span} interactive={!!onClick} className={cx("sb-stat sb-kpi", loading && "is-pending")} onClick={onClick} data-kpi-tile="" {...rest}>
      <IconChip icon={icon} tone={tone} />
      <span>{label}</span>
      <b>{loading ? "…" : value}</b>
      {sub && <small>{sub}</small>}
      {trend && trend.length > 1 && <Sparkline values={trend} color={`var(--tint-${tone}-fg)`} label={trendLabel} />}
      {progress !== undefined && <ProgressBar value={progress} label={label} />}
    </BentoTile>
  );
}
