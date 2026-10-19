import { Check, X } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { ReactNode } from "react";
import { Badge, type BadgeKind } from "./Badge";
import { cx } from "./cx";
import { IconChip } from "./IconChip";
import type { Tone } from "./tones";

export interface ApprovalCardProps {
  icon: LucideIcon;
  tone?: Tone;
  /** "Riya Kapoor · Merit scholarship 25%". */
  title: ReactNode;
  /** "SIT25EC047 · B.Tech Electronics · −₹46,250". */
  meta?: ReactNode;
  onApprove?: () => void;
  onReject?: () => void;
  approveLabel?: string;
  rejectLabel?: string;
  busy?: boolean;
  /** Once decided: the outcome badge replaces the buttons. */
  decided?: { label: string; kind?: BadgeKind };
  /** Opens the full request (side panel). */
  onOpen?: () => void;
  [key: `data-${string}`]: string | undefined;
}

/** Approval card (prototype finance "Fee adjustments"): icon chip, title + meta, Approve pill + ghost Reject. */
export function ApprovalCard({ icon, tone = "green", title, meta, onApprove, onReject, approveLabel = "Approve", rejectLabel = "Reject", busy, decided, onOpen, ...rest }: ApprovalCardProps) {
  const main = (
    <>
      <b>{title}</b>
      {meta && <small>{meta}</small>}
    </>
  );
  return (
    <div className={cx("sb-approval", decided && "is-decided")} data-approval="" {...rest}>
      <IconChip icon={icon} tone={tone} />
      {onOpen ? (
        <button type="button" className="sb-approval-main" style={{ textAlign: "left" }} onClick={onOpen}>
          {main}
        </button>
      ) : (
        <div className="sb-approval-main">{main}</div>
      )}
      <div className="sb-approval-acts">
        {decided ? (
          <Badge kind={decided.kind ?? "neutral"}>{decided.label}</Badge>
        ) : (
          <>
            {onApprove && (
              <button type="button" className="sb-btn" disabled={busy} onClick={onApprove} data-approve="">
                <Check strokeWidth={2} aria-hidden="true" />
                {approveLabel}
              </button>
            )}
            {onReject && (
              <button type="button" className="sb-btn sb-btn--ghost" disabled={busy} onClick={onReject} data-reject="">
                <X strokeWidth={2} aria-hidden="true" />
                {rejectLabel}
              </button>
            )}
          </>
        )}
      </div>
    </div>
  );
}

/** Vertical stack of approval cards. */
export function ApprovalList({ children }: { children: ReactNode }) {
  return <div className="sb-approvals">{children}</div>;
}
