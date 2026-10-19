import type { ComponentType, ReactNode } from "react";

export interface NavItem {
  id: string;
  label: string;
  to: string;
  /** Extra path prefixes that mark this pill active (screens reached from its page). */
  match?: string[];
}

export interface NavLinkLikeProps {
  to: string;
  className?: string | ((state: { isActive: boolean }) => string);
  end?: boolean;
  /** Extra path prefixes that count as active (see `NavItem.match`). */
  match?: string[];
  children: ReactNode;
}

export interface ShellProps {
  nav: NavItem[];
  /** Router link component (e.g. React Router `NavLink`). */
  linkComponent: ComponentType<NavLinkLikeProps>;
  actions?: ReactNode;
  children: ReactNode;
  brand?: ReactNode;
  footer?: ReactNode;
}

export function Logo({ children = "sutra" }: { children?: ReactNode }) {
  return (
    <div className="sb-logo">
      <i aria-hidden="true" />
      {children}
    </div>
  );
}

/** Top pill navigation. */
export function PillNav({ nav, linkComponent: Link }: Pick<ShellProps, "nav" | "linkComponent">) {
  return (
    <nav className="sb-pills" aria-label="Main">
      {nav.map((n) => (
        <Link key={n.id} to={n.to} end={n.to === "/"} match={n.match} className={({ isActive }) => (isActive ? "on" : "")}>
          {n.label}
        </Link>
      ))}
    </nav>
  );
}

/** Application shell: logo, pill nav, header actions, main content. */
export function Shell({ nav, linkComponent, actions, children, brand, footer }: ShellProps) {
  return (
    <div className="sb-app">
      <header className="sb-header">
        {brand ?? <Logo />}
        <PillNav nav={nav} linkComponent={linkComponent} />
        <div className="sb-header-actions">{actions}</div>
      </header>
      <main>{children}</main>
      {footer}
    </div>
  );
}

export function IconButton({ label, children, onClick, dot }: { label: string; children: ReactNode; onClick?: () => void; dot?: boolean }) {
  return (
    <button type="button" className="sb-icon-btn" aria-label={label} title={label} onClick={onClick}>
      {children}
      {dot && <span className="sb-dot" />}
    </button>
  );
}

export function Avatar({ initials, label, onClick }: { initials: string; label?: string; onClick?: () => void }) {
  if (!onClick) return <span className="sb-avatar" aria-hidden="true">{initials}</span>;
  return (
    <button type="button" className="sb-avatar" aria-label={label ?? "Account menu"} onClick={onClick}>
      {initials}
    </button>
  );
}
