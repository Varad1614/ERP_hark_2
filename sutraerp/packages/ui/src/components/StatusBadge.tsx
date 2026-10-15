import { Badge, type BadgeKind } from "./Badge";
import { humanize, statusKind } from "./format";

/**
 * Badge for a wire status code: `approval_pending` → "Approval pending" (review), `overdue` →
 * "Overdue" (attention). `labels` overrides the text per code (domain wording), `kind` the colour.
 */
export function StatusBadge({ status, labels, kind }: { status: string | null | undefined; labels?: Record<string, string>; kind?: BadgeKind }) {
  if (!status) return null;
  return <Badge kind={kind ?? statusKind(status)}>{labels?.[status] ?? humanize(status)}</Badge>;
}
