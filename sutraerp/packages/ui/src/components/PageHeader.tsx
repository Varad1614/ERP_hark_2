import type { ReactNode } from "react";
import { BentoTile } from "./BentoTile";

/**
 * Prototype page header (`.ph`): white 12-column tile with the screen title, a subtitle line,
 * optional actions on the right and an optional row below (section tabs).
 */
export function PageHeader({ title, eyebrow, subtitle, actions, children, ...rest }: { title: ReactNode; eyebrow?: ReactNode; subtitle?: ReactNode; actions?: ReactNode; children?: ReactNode; [key: `data-${string}`]: string | undefined }) {
  return (
    <BentoTile span={12} className="sb-page-head-tile" {...rest}>
      <div className="sb-page-head">
        <div>
          {eyebrow && <span className="sb-page-eyebrow">{eyebrow}</span>}
          <h1>{title}</h1>
          {subtitle && <p>{subtitle}</p>}
        </div>
        {actions && <div style={{ marginLeft: "auto", display: "flex", gap: 10, alignItems: "center", flexWrap: "wrap" }}>{actions}</div>}
      </div>
      {children}
    </BentoTile>
  );
}
