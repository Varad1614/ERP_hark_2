import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { cx } from "./cx";

/**
 * Empty state (prototype `.empty`): icon + one line, centred inside its tile. `action` adds one
 * button (e.g. "New admission"); `compact` is for small tiles. One empty state per page section:
 * never stack several empty tiles (docs/reskin2-contracts.md §3.6).
 */
export function EmptyState({ icon: Icon, title, children, action, compact }: { icon?: LucideIcon; title: string; children?: ReactNode; action?: ReactNode; compact?: boolean }) {
  return (
    <div className={cx("sb-empty", compact && "sb-empty--compact")} role="status">
      {Icon && <Icon strokeWidth={1.75} aria-hidden="true" />}
      <b>{title}</b>
      {children && <p>{children}</p>}
      {action}
    </div>
  );
}
