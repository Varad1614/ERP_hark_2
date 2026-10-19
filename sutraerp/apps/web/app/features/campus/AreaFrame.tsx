/**
 * Re-skin v2 (R7) frame of Admin → Campus and Admin → Reports (docs/reskin2-contracts.md §2):
 * administrator gate, `AreaHeader` (h1 + one live summary line, ≤5 right-aligned tabs, the
 * institution picker when there are several) and an optional row of sub-section chips (links, so
 * every section keeps its own URL). Replaces `AdminScreen` + its clipped 16-tab strip for R7.
 */
import { AreaHeader, BentoGrid, BentoTile, Select, type AreaTab, type NavLinkLikeProps } from "@sutra/ui";
import type { ReactNode } from "react";
import { Link, useLocation } from "react-router";
import { useSession } from "../../lib/auth";
import { selectInstitution, useInstitutions } from "../institution/hooks";
import type { Institution } from "../institution/types";

export interface AreaGroupTab extends AreaTab {
  /** Paths (besides `to`) that count as this tab (the group's sections). */
  match?: string[];
}

/** Router link for `AreaHeader` tabs: active when the current path is the tab's or one of its `match` paths. */
function makeTabLink(tabs: AreaGroupTab[]) {
  return function TabLink({ to, className, children }: NavLinkLikeProps) {
    const { pathname } = useLocation();
    const tab = tabs.find((t) => t.to === to);
    const paths = [to, ...(tab?.match ?? [])];
    const isActive = paths.some((p) => pathname === p || pathname === `${p}/`);
    return (
      <Link to={to} className={typeof className === "function" ? className({ isActive }) : className} aria-current={isActive ? "page" : undefined} data-tab-link={tab?.id}>
        {children}
      </Link>
    );
  };
}

const linkCache = new WeakMap<AreaGroupTab[], ReturnType<typeof makeTabLink>>();

export function AreaFrame({
  screen,
  title,
  summary,
  tabs,
  activeTab,
  chips,
  activeChip,
  children,
}: {
  /** `data-screen` value ("campus", "reports"). */
  screen: string;
  title: string;
  summary?: ReactNode;
  tabs: AreaGroupTab[];
  activeTab: string;
  /** Sub-section chips of the active tab (links). */
  chips?: { id: string; label: string; to: string }[];
  activeChip?: string;
  children: (institution: Institution | null) => ReactNode;
}) {
  const { role } = useSession();
  const { list, current: institution, query } = useInstitutions();
  let TabLink = linkCache.get(tabs);
  if (!TabLink) {
    TabLink = makeTabLink(tabs);
    linkCache.set(tabs, TabLink);
  }
  if (role !== "administrator") {
    return (
      <BentoGrid data-screen={screen}>
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>{title} is for administrators.</p>
        </BentoTile>
      </BentoGrid>
    );
  }
  const picker =
    list.length > 1 && institution ? (
      <div style={{ minWidth: 220 }}>
        <Select
          label="Institution"
          name="institution"
          value={institution.id}
          options={list.map((i) => ({ value: i.id, label: i.name }))}
          onChange={(e) => selectInstitution(e.target.value)}
        />
      </div>
    ) : undefined;
  return (
    <BentoGrid data-screen={screen} data-tab={activeChip ?? activeTab} data-group={activeTab}>
      <AreaHeader
        title={title}
        summary={
          institution ? (
            <span data-current-institution={institution.id}>{summary}</span>
          ) : query.isLoading ? (
            "Loading…"
          ) : (
            "No institution registered yet — start in Setup."
          )
        }
        tabs={tabs}
        activeTab={activeTab}
        linkComponent={TabLink}
        tabsLabel={`${title} sections`}
        actions={picker}
      />
      {chips && chips.length > 0 && (
        <div className="sb-chip-rows">
          <nav className="sb-chip-row" aria-label={`${tabs.find((t) => t.id === activeTab)?.label ?? title} sections`} data-subnav={activeTab}>
            {chips.map((c) => (
              <Link key={c.id} to={c.to} className={c.id === activeChip ? "sb-filter on" : "sb-filter"} aria-current={c.id === activeChip ? "page" : undefined} data-tab-link={c.id} data-chip={c.id}>
                {c.label}
              </Link>
            ))}
          </nav>
        </div>
      )}
      {children(institution)}
    </BentoGrid>
  );
}
