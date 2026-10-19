import type { ReactNode } from "react";
import { Badge, type BadgeKind } from "./Badge";
import { Drawer } from "./Drawer";
import { Steps } from "./Steps";

export interface SidePanelProps {
  open: boolean;
  onClose: () => void;
  title: string;
  /** "Students · Record". */
  eyebrow?: ReactNode;
  /** Status chip above the eyebrow. */
  chip?: { label: string; kind?: BadgeKind };
  /** Label → value facts (prototype `dl.dl`). Values must be human (formatted, no raw ids). */
  facts?: Array<{ label: string; value: ReactNode }>;
  /** Workflow stages (prototype `ol.st`) and the current index. */
  stages?: string[];
  stage?: number;
  /** Footer actions (`.acts`): one primary + ghost buttons; secondary record actions live here. */
  actions?: ReactNode;
  children?: ReactNode;
}

/**
 * Record / request detail panel (prototype `.dr`): chip, eyebrow, h2, facts, stepper, actions.
 * Every action that does not fit a row's single primary button belongs here or in the kebab.
 */
export function SidePanel({ open, onClose, title, eyebrow, chip, facts, stages, stage = 0, actions, children }: SidePanelProps) {
  return (
    <Drawer
      open={open}
      onClose={onClose}
      title={title}
      eyebrow={eyebrow}
      media={chip ? <span className="sb-panel-chip"><Badge kind={chip.kind ?? "neutral"}>{chip.label}</Badge></span> : undefined}
      footer={actions}
    >
      {facts && facts.length > 0 && (
        <dl className="sb-dl">
          {facts.map((f) => (
            <FactRow key={f.label} label={f.label} value={f.value} />
          ))}
        </dl>
      )}
      {stages && stages.length > 0 && <Steps stages={stages} current={stage} />}
      {children}
    </Drawer>
  );
}

function FactRow({ label, value }: { label: string; value: ReactNode }) {
  return (
    <>
      <dt>{label}</dt>
      <dd>{value}</dd>
    </>
  );
}
