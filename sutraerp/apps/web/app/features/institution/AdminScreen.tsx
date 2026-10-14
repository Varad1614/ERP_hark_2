import { BentoGrid, BentoTile, PageHeader, Select } from "@sutra/ui";
import { useEffect, useRef, type ReactNode } from "react";
import { NavLink } from "react-router";
import { useSession } from "../../lib/auth";
import { activeTab, type TabDef } from "./index";
import { selectInstitution, StatusBadge, useInstitutions } from "./hooks";
import type { Institution } from "./types";

/**
 * Frame of the Batch 2 admin screens: administrator gate, heading with the institution picker
 * and the section tabs; `children(institution, tab)` renders the active tab.
 */
export function AdminScreen({
  title,
  base,
  tabs,
  tab,
  actions,
  children,
}: {
  title: string;
  base: string;
  tabs: readonly TabDef[];
  tab: string | undefined;
  /** Extra header actions (e.g. the Academics → Academic delivery link). */
  actions?: ReactNode;
  children: (institution: Institution | null, tab: TabDef) => ReactNode;
}) {
  const { role } = useSession();
  const current = activeTab(tabs, tab);
  const { list, current: institution, query } = useInstitutions();
  // Long tab strips scroll sideways: keep the current tab in view.
  const nav = useRef<HTMLElement>(null);
  useEffect(() => {
    const strip = nav.current;
    const on = strip?.querySelector<HTMLElement>(".on");
    if (strip && on && strip.scrollWidth > strip.clientWidth) {
      const left = on.getBoundingClientRect().left - strip.getBoundingClientRect().left + strip.scrollLeft;
      strip.scrollLeft = Math.max(0, left - (strip.clientWidth - on.offsetWidth) / 2);
    }
  }, [current.key]);
  if (role !== "administrator") {
    return (
      <BentoGrid data-screen={base.slice(1)}>
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>{title} is for administrators.</p>
        </BentoTile>
      </BentoGrid>
    );
  }
  return (
    <BentoGrid data-screen={base.slice(1)} data-tab={current.key}>
      <PageHeader
        title={title}
        subtitle={
          institution ? (
            <span data-current-institution={institution.id}>
              {current.label} · {institution.name} · {institution.code} · {institution.timezone} <StatusBadge status={institution.status} />
            </span>
          ) : query.isLoading ? (
            "Loading institutions…"
          ) : (
            "No institution registered yet."
          )
        }
        actions={
          list.length > 1 && institution ? (
            <>
              {actions}
              <div style={{ minWidth: 240 }}>
                <Select
                  label="Institution"
                  name="institution"
                  value={institution.id}
                  options={list.map((i) => ({ value: i.id, label: `${i.name} (${i.code})` }))}
                  onChange={(e) => selectInstitution(e.target.value)}
                />
              </div>
            </>
          ) : (
            actions
          )
        }
      >
        <nav ref={nav} className="sb-tabs" aria-label={`${title} sections`}>
          {tabs.map((t) => (
            <NavLink key={t.key} to={`${base}/${t.key}`} className={t.key === current.key ? "on" : ""} aria-current={t.key === current.key ? "page" : undefined} data-tab-link={t.key}>
              {t.label}
            </NavLink>
          ))}
        </nav>
      </PageHeader>
      {children(institution, current)}
    </BentoGrid>
  );
}
