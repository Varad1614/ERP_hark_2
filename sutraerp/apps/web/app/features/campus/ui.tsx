/**
 * Re-skin v2 (R7) building blocks shared by the Campus, Reports and Library screens
 * (docs/reskin2-contracts.md §2.2, §4): one primary pill per row with every other action in the
 * kebab, codes as a muted `KeyTag` beside the human name, and money through `formatINR`.
 *
 * `Actions` keeps the signature of `features/institution/forms` `Actions` (same `RowAction[]`,
 * same `data-action` ids) so every slice-8/9 screen switches by changing one import; the kebab
 * items carry `data-menu-item=<id>` (e2e opens `[data-row-menu]` first).
 */
import { Badge, KeyTag, RowActions, formatINR, statusKind, type ListRowProps, type MenuItem } from "@sutra/ui";
import type { ReactNode } from "react";
import type { RowAction } from "../institution/forms";
import { approvalBadge, approvalStateLabel } from "./approval";

/** Destructive commands are tinted in the kebab (Suspend, Cancel, Decommission …). */
const DANGER = /(suspend|cancel|decommission|archive|reject|withdraw|retire|vacate|deactivate|remove|delete|revoke|dispose|write-?off|terminate|breakdown|blacklist|not-fixed|wo-reject)/;

/** Row actions: the first primary-variant action (else the first action, as a ghost pill) + kebab. */
export function Actions({ actions, disabled, fallbackPrimary = true }: { actions: RowAction[]; disabled?: boolean; /** `false`: with no primary-variant action, everything goes into the kebab. */ fallbackPrimary?: boolean }) {
  if (actions.length === 0) return null;
  const pi = actions.findIndex((a) => a.variant === "primary" || a.variant === "accent");
  // No primary step: a destructive first action (Cancel, Suspend …) never becomes the visible pill.
  if (pi < 0 && (!fallbackPrimary || DANGER.test(actions[0]!.id))) {
    return <RowActions menu={actions.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.id) }))} />;
  }
  const idx = pi >= 0 ? pi : 0;
  const p = actions[idx]!;
  const menu: MenuItem[] = actions
    .filter((_, i) => i !== idx)
    .map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.id) }));
  return <RowActions primary={{ label: p.label, onClick: p.onClick, disabled, ghost: pi < 0, "data-action": p.id }} menu={menu} />;
}

/** "Room 101 [MAIN-B1-101]": the human name first, the code as a muted key tag. */
export function CodeName({ name, code, sub }: { name: ReactNode; code?: string | null; sub?: ReactNode }) {
  return (
    <span style={{ display: "inline-flex", flexDirection: "column", gap: 2 }}>
      <span style={{ display: "inline-flex", alignItems: "center", gap: 8, flexWrap: "wrap" }}>
        <b style={{ fontWeight: 600 }}>{name}</b>
        {code ? <KeyTag>{code}</KeyTag> : null}
      </span>
      {sub ? <small style={{ color: "var(--sb-muted, var(--mu))" }}>{sub}</small> : null}
    </span>
  );
}

/** Option label for selects (plain text): "Room 101 · MAIN-B1-101". */
export const nameCode = (name: string, code?: string | null) => (code && code !== name ? `${name} · ${code}` : name);

/** Money for tables: `formatINR` (₹65,000; paise only when present); "—" when empty. */
export const inr = (v: string | number | null | undefined) => (v === null || v === undefined || v === "" ? "—" : formatINR(v));

/** `ListRow` props (`primary` + `menu`) from a `RowAction[]`, with the same primary rule as `Actions`. */
export function rowProps(actions: RowAction[], disabled?: boolean): Pick<ListRowProps, "primary" | "menu"> {
  if (actions.length === 0) return {};
  const pi = actions.findIndex((a) => a.variant === "primary" || a.variant === "accent");
  if (pi < 0 && DANGER.test(actions[0]!.id)) return { menu: actions.map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.id) })) };
  const idx = pi >= 0 ? pi : 0;
  const p = actions[idx]!;
  return {
    primary: { label: p.label, onClick: p.onClick, disabled, ghost: pi < 0, "data-action": p.id },
    menu: actions.filter((_, i) => i !== idx).map((a) => ({ id: a.id, label: a.label, onSelect: a.onClick, disabled, danger: DANGER.test(a.id) })),
  };
}

/** Workflow stage index of a governed lifecycle status (for `SidePanel` stages). */
export function stageOf(stages: readonly string[], status: string): number {
  const i = stages.indexOf(status);
  return i >= 0 ? i : 0;
}

/** Approval state as a badge with its human label ("Not submitted", "Awaiting approval" …), never the wire value. */
export function ApprovalBadge({ state }: { state: string | null | undefined }) {
  return <Badge kind={statusKind(approvalBadge(state))}>{approvalStateLabel(state)}</Badge>;
}
