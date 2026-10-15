import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Badge, KeyTag, type BadgeKind } from "./Badge";
import { BentoTile, type Span } from "./BentoTile";
import { cx } from "./cx";
import { IconChip } from "./IconChip";
import { Initials } from "./Initials";
import { toneFor } from "./format";
import type { Tone } from "./tones";

/** Progress bar (prototype `.x-bar`): `value` 0–100. */
export function ProgressBar({ value, kind, label }: { value: number; kind?: "accent" | "done" | "attention" | "review"; label?: string }) {
  const pct = Math.max(0, Math.min(100, Number.isFinite(value) ? value : 0));
  return (
    <div className={cx("sb-bar", kind && kind !== "accent" && `sb-bar--${kind}`)} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)} aria-label={label}>
      <span style={{ width: `${pct}%` }} />
    </div>
  );
}

/** Label / value line (prototype `.x-row`): "Next session …… Today, 10:30 · B-204". */
export function KeyValue({ label, value }: { label: ReactNode; value: ReactNode }) {
  return (
    <div className="sb-kv">
      <span>{label}</span>
      <b>{value}</b>
    </div>
  );
}

/** Grid that holds cards (prototype `.x-pgrid`, dense flow so wide cards pack). */
export function CardGrid({ children, ...rest }: { children: ReactNode; [key: `data-${string}`]: string | undefined }) {
  return (
    <div className="sb-card-grid" {...rest}>
      {children}
    </div>
  );
}

export interface PersonCardProps {
  name: string;
  role?: ReactNode;
  /** Extra line (e-mail · room) shown on wide cards. */
  detail?: ReactNode;
  tags?: Array<{ label: string; kind?: BadgeKind }>;
  wide?: boolean;
  tone?: Tone;
  onClick?: () => void;
  span?: Span;
  [key: `data-${string}`]: string | undefined;
}

/** Person card (prototype `.x-sc` / `.x-sc.w`): pastel initials square, name, role, tag chips. */
export function PersonCard({ name, role, detail, tags, wide, tone, onClick, span, ...rest }: PersonCardProps) {
  return (
    <BentoTile as={onClick ? "button" : "div"} type={onClick ? "button" : undefined} interactive={!!onClick} span={span ?? (wide ? 6 : 3)} className={cx("sb-card sb-person-card", wide && "sb-person-card--wide")} onClick={onClick} {...rest}>
      <Initials name={name} size={wide ? "xl" : "lg"} tone={tone ?? toneFor(name)} />
      <div style={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0 }}>
        <h3>{name}</h3>
        {role && <p>{role}</p>}
        {wide && detail && <p>{detail}</p>}
        {tags && tags.length > 0 && (
          <div className="sb-card-foot">
            {tags.map((t) => (
              <Badge key={t.label} kind={t.kind ?? "neutral"}>
                {t.label}
              </Badge>
            ))}
          </div>
        )}
      </div>
    </BentoTile>
  );
}

export interface ClassCardProps {
  /** Code chip ("CS201 · Section A"). */
  code: string;
  title: string;
  /** Muted line under the title ("64 students · B-204"). */
  meta?: ReactNode;
  icon: LucideIcon;
  tone?: Tone;
  /** Label / value lines in the foot ("Next session" → "Today, 10:30 · B-204"). */
  facts?: Array<{ label: ReactNode; value: ReactNode }>;
  /** Progress (0–100) with its label ("Syllabus · 2 of 5 units"). */
  progress?: { label: ReactNode; value: number };
  badge?: { label: string; kind?: BadgeKind };
  onClick?: () => void;
  span?: Span;
  [key: `data-${string}`]: string | undefined;
}

/** Class / course / programme card (prototype `.x-cc`): icon chip, code chip, title, facts, progress bar. */
export function ClassCard({ code, title, meta, icon, tone = "blue", facts, progress, badge, onClick, span = 6, ...rest }: ClassCardProps) {
  return (
    <BentoTile as={onClick ? "button" : "div"} type={onClick ? "button" : undefined} interactive={!!onClick} span={span} className="sb-card sb-class-card" onClick={onClick} {...rest}>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start" }}>
        <IconChip icon={icon} tone={tone} />
        {badge && <Badge kind={badge.kind ?? "neutral"}>{badge.label}</Badge>}
      </div>
      <KeyTag>{code}</KeyTag>
      <h3>{title}</h3>
      {meta && <small>{meta}</small>}
      {((facts && facts.length > 0) || progress) && (
        <div className="sb-card-foot">
          {facts?.map((f, i) => <KeyValue key={i} label={f.label} value={f.value} />)}
          {progress && (
            <>
              <KeyValue label={progress.label} value={`${Math.round(progress.value)}%`} />
              <ProgressBar value={progress.value} />
            </>
          )}
        </div>
      )}
    </BentoTile>
  );
}

/** Course card (student Courses): a ClassCard with faculty + attendance facts. */
export function CourseCard({ faculty, attendance, units, ...rest }: Omit<ClassCardProps, "facts" | "progress"> & { faculty?: string; attendance?: string; units?: { done: number; total: number } }) {
  const facts = [faculty ? { label: "Faculty", value: faculty } : null, attendance ? { label: "Attendance", value: attendance } : null].filter((f): f is { label: string; value: string } => f !== null);
  return <ClassCard {...rest} facts={facts} progress={units && units.total > 0 ? { label: `Units · ${units.done} of ${units.total}`, value: (units.done / units.total) * 100 } : undefined} />;
}

/** Report card (prototype `.x-rpt`): icon chip, title, description, a small chart at the bottom. */
export function ReportCard({ icon, tone = "orange", title, description, chart, onClick, span = 4, ...rest }: { icon: LucideIcon; tone?: Tone; title: string; description?: ReactNode; chart?: ReactNode; onClick?: () => void; span?: Span; [key: `data-${string}`]: string | undefined }) {
  return (
    <BentoTile as={onClick ? "button" : "div"} type={onClick ? "button" : undefined} interactive={!!onClick} span={span} className="sb-card sb-report-card" onClick={onClick} {...rest}>
      <IconChip icon={icon} tone={tone} />
      <h3>{title}</h3>
      {description && <p>{description}</p>}
      {chart && <div className="sb-report-chart">{chart}</div>}
    </BentoTile>
  );
}
