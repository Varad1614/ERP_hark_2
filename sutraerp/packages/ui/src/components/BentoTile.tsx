import type { ElementType, HTMLAttributes, ReactNode } from "react";
import { cx } from "./cx";

export type Span = 3 | 4 | 5 | 6 | 7 | 8 | 12;

export function BentoGrid({ className, ...rest }: HTMLAttributes<HTMLDivElement>) {
  return <div className={cx("sb-grid", className)} {...rest} />;
}

export interface BentoTileProps extends HTMLAttributes<HTMLElement> {
  span?: Span;
  /** `accent` = role-tinted pastel, `dark` = ink tile. */
  variant?: "default" | "accent" | "dark";
  /** Render as another element (e.g. `button` for clickable tiles). */
  as?: ElementType;
  interactive?: boolean;
  title?: string;
  action?: ReactNode;
  children?: ReactNode;
  [key: string]: unknown;
}

/** The core rounded bento surface. */
export function BentoTile({ span, variant = "default", as, interactive, title, action, className, children, ...rest }: BentoTileProps) {
  const Comp: ElementType = as ?? "div";
  return (
    <Comp
      className={cx(
        "sb-tile",
        span && `sb-span-${span}`,
        variant === "accent" && "sb-tile--accent",
        variant === "dark" && "sb-tile--dark",
        interactive && "sb-tile--interactive",
        className,
      )}
      {...rest}
    >
      {title !== undefined && (
        <h4>
          {title}
          {action != null && action !== false && <span className="sb-tile-actions">{action}</span>}
        </h4>
      )}
      {children}
    </Comp>
  );
}
