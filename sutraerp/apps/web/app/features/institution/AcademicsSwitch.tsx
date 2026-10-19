import { NavLink } from "react-router";

/**
 * Admin "Academics" pill covers two screens (prototype navigation): programme setup (/academics,
 * Batch 2) and academic delivery (/academic-delivery, Batch 4). This switch sits in both headers.
 */
export function AcademicsSwitch({ current }: { current: "setup" | "delivery" }) {
  return (
    <nav className="sb-tabs" aria-label="Academics area" data-academics-switch={current}>
      <NavLink to="/academics" className={current === "setup" ? "on" : ""} aria-current={current === "setup" ? "page" : undefined}>
        Programmes &amp; calendar
      </NavLink>
      <NavLink to="/academic-delivery" className={current === "delivery" ? "on" : ""} aria-current={current === "delivery" ? "page" : undefined} data-academic-delivery-link>
        Academic delivery
      </NavLink>
    </nav>
  );
}
