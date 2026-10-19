import { ArrowLeft, ArrowRight, Check, X } from "lucide-react";
import type { ReactNode } from "react";
import { Modal } from "./Modal";

/** Horizontal step indicator (prototype `.x-stp`). `current` is 0-based. */
export function Stepper({ steps, current }: { steps: string[]; current: number }) {
  return (
    <ol className="sb-stepper" aria-label="Steps">
      {steps.map((s, i) => {
        const ok = i < current;
        const cur = i === current;
        return (
          <li key={s} className={ok ? "ok" : cur ? "cur" : undefined} aria-current={cur ? "step" : undefined}>
            <span className="n">{ok ? <Check strokeWidth={2.5} /> : i + 1}</span>
            <span className="sb-step-label">{s}</span>
          </li>
        );
      })}
    </ol>
  );
}

export interface StepDialogProps {
  open: boolean;
  onClose: () => void;
  /** "Students · New admission" (area · action). */
  eyebrow?: string;
  title: string;
  /** Step labels; omit for a single-step dialog. */
  steps?: string[];
  /** Current step (0-based). */
  step?: number;
  children: ReactNode;
  /** Primary pill: "Next" with an arrow on intermediate steps, the given label on the last. */
  primary?: { label?: string; onClick?: () => void; disabled?: boolean; busy?: boolean; type?: "button" | "submit"; form?: string };
  /** Back button on steps after the first (default: none). */
  onBack?: () => void;
  /** Ghost secondary (default "Cancel" → onClose). `null` hides it. */
  cancelLabel?: string | null;
  /** Right side of the footer (e.g. "Step 2 of 3", a total). */
  note?: ReactNode;
  /** Replace the whole footer. */
  footer?: ReactNode;
  [key: `data-${string}`]: string | undefined;
}

/**
 * Dialog in the prototype `.md` style: eyebrow + h2 + round close X, optional horizontal stepper,
 * body, then the `.go` primary pill and a ghost Cancel (docs/reskin2-contracts.md §2.4).
 */
export function StepDialog({ open, onClose, eyebrow, title, steps, step = 0, children, primary, onBack, cancelLabel = "Cancel", note, footer, ...rest }: StepDialogProps) {
  const last = !steps || step >= steps.length - 1;
  const primaryLabel = primary?.label ?? (last ? "Save" : "Next");
  return (
    <Modal open={open} onClose={onClose} label={title} padded={false}>
      <div className="sb-step-dialog" {...rest}>
        <div className="sb-dialog-head">
          <div>
            {eyebrow && <span className="sb-dialog-eyebrow">{eyebrow}</span>}
            <h2>{title}</h2>
          </div>
          <button type="button" className="sb-icon-btn" aria-label="Close" onClick={onClose}>
            <X strokeWidth={1.75} />
          </button>
        </div>
        {steps && steps.length > 1 && <Stepper steps={steps} current={step} />}
        {children}
        {footer ?? (
          <div className="sb-dialog-foot">
            {onBack && step > 0 && (
              <button type="button" className="sb-btn sb-btn--ghost" onClick={onBack}>
                <ArrowLeft strokeWidth={1.75} aria-hidden="true" />
                Back
              </button>
            )}
            {primary && (
              <button type={primary.type ?? "button"} form={primary.form} className="sb-btn" disabled={primary.disabled || primary.busy} onClick={primary.onClick} data-dialog-primary="">
                {primary.busy ? "Working…" : primaryLabel}
                {!last && !primary.busy && <ArrowRight strokeWidth={1.75} aria-hidden="true" />}
              </button>
            )}
            {cancelLabel !== null && (
              <button type="button" className="sb-btn sb-btn--ghost" onClick={onClose}>
                {cancelLabel}
              </button>
            )}
            {note && <span className="sb-dialog-note">{note}</span>}
          </div>
        )}
      </div>
    </Modal>
  );
}
