import { Check, ChevronRight, MoreHorizontal } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Badge, type BadgeKind } from "./Badge";
import { cx } from "./cx";
import { IconChip } from "./IconChip";
import { Initials } from "./Initials";
import { MenuButton, type MenuItem } from "./Menu";
import type { Tone } from "./tones";

export interface ListRowProps {
  title: ReactNode;
  /** One muted meta line ("SIT25EC047 · B.Tech Electronics · 15 Jul"). */
  meta?: ReactNode;
  /** Leading visual: a person's name (initials avatar), an icon chip, or a checkbox (to-dos). */
  avatar?: string;
  icon?: LucideIcon;
  tone?: Tone;
  check?: { checked: boolean; onChange: (checked: boolean) => void; label: string };
  badge?: { label: string; kind?: BadgeKind };
  /** Trailing value (amount, date, percent) before the action. */
  value?: ReactNode;
  /** The ONE primary action shown on the row (pill button). */
  primary?: { label: string; onClick: () => void; disabled?: boolean; icon?: LucideIcon; ghost?: boolean; [key: `data-${string}`]: string | undefined };
  /** Every other action goes into the kebab menu. */
  menu?: MenuItem[];
  menuLabel?: string;
  /** Row click opens details (side panel); shows a chevron when there is no primary action. */
  onOpen?: () => void;
  selected?: boolean;
  done?: boolean;
  className?: string;
  [key: `data-${string}`]: string | undefined;
}

/**
 * List row (prototype `.rc` / `.tk` / `.x-rs`): leading avatar / icon chip / checkbox, title +
 * meta, badge, ONE primary pill and a kebab menu for the rest (docs/reskin2-contracts.md §3).
 */
export function ListRow({ title, meta, avatar, icon, tone = "grey", check, badge, value, primary, menu, menuLabel, onOpen, selected, done, className, ...rest }: ListRowProps) {
  const main = (
    <>
      <b>{title}</b>
      {meta && <small>{meta}</small>}
    </>
  );
  const PrimaryIcon = primary?.icon;
  const primaryData = dataAttrs(primary);
  return (
    <div className={cx("sb-lrow", selected && "is-selected", done && "is-done", className)} {...rest}>
      {check && (
        <button type="button" className="sb-check" role="checkbox" aria-checked={check.checked} aria-label={check.label} onClick={() => check.onChange(!check.checked)} style={check.checked ? { background: "var(--sb-success)", borderColor: "var(--sb-success)", color: "#fff" } : undefined}>
          {check.checked && <Check strokeWidth={3} style={{ width: 14, height: 14 }} />}
        </button>
      )}
      {avatar && <Initials name={avatar} tone="accent" />}
      {icon && <IconChip icon={icon} tone={tone} />}
      {onOpen ? (
        <button type="button" className="sb-lrow-main" onClick={onOpen} data-row-open="">
          {main}
        </button>
      ) : (
        <div className="sb-lrow-main">{main}</div>
      )}
      <div className="sb-lrow-end">
        {value != null && <span style={{ fontWeight: 700, fontSize: 14 }}>{value}</span>}
        {badge && <Badge kind={badge.kind ?? "neutral"}>{badge.label}</Badge>}
        {primary && (
          <button type="button" className={cx("sb-btn", primary.ghost && "sb-btn--ghost")} disabled={primary.disabled} onClick={primary.onClick} {...primaryData}>
            {PrimaryIcon && <PrimaryIcon strokeWidth={1.75} aria-hidden="true" />}
            {primary.label}
          </button>
        )}
        {menu && menu.length > 0 && (
          <MenuButton
            label={menuLabel ?? "More actions"}
            items={menu}
            trigger={(p) => (
              <button type="button" className="sb-kebab" aria-label={menuLabel ?? "More actions"} data-row-menu="" {...p}>
                <MoreHorizontal strokeWidth={1.75} aria-hidden="true" />
              </button>
            )}
          />
        )}
        {onOpen && !primary && !menu?.length && <ChevronRight className="sb-lrow-chevron" strokeWidth={1.75} aria-hidden="true" />}
      </div>
    </div>
  );
}

/** The `data-*` keys of an object (e2e hooks such as `data-action`). */
function dataAttrs(o: object | undefined): Record<string, string | undefined> {
  const out: Record<string, string | undefined> = {};
  if (!o) return out;
  for (const [k, v] of Object.entries(o)) if (k.startsWith("data-")) out[k] = v as string | undefined;
  return out;
}

/** Kebab-only row actions for table cells (`RowActions`): one primary pill + menu. */
export function RowActions({ primary, menu, label = "More actions" }: { primary?: ListRowProps["primary"]; menu?: MenuItem[]; label?: string }) {
  const PrimaryIcon = primary?.icon;
  return (
    <span style={{ display: "inline-flex", gap: 8, alignItems: "center", justifyContent: "flex-end" }}>
      {primary && (
        <button type="button" className={cx("sb-btn", primary.ghost && "sb-btn--ghost")} style={{ padding: "8px 16px", fontSize: 13 }} disabled={primary.disabled} onClick={primary.onClick} {...dataAttrs(primary)}>
          {PrimaryIcon && <PrimaryIcon strokeWidth={1.75} aria-hidden="true" />}
          {primary.label}
        </button>
      )}
      {menu && menu.length > 0 && (
        <MenuButton
          label={label}
          items={menu}
          trigger={(p) => (
            <button type="button" className="sb-kebab" aria-label={label} data-row-menu="" {...p}>
              <MoreHorizontal strokeWidth={1.75} aria-hidden="true" />
            </button>
          )}
        />
      )}
    </span>
  );
}
