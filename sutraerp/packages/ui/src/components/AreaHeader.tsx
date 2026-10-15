import { ArrowLeft, ChevronDown } from "lucide-react";
import type { ComponentType, ReactNode } from "react";
import { BentoTile } from "./BentoTile";
import { cx } from "./cx";
import { SearchField } from "./Input";
import { MenuList, useMenu, type MenuItem } from "./Menu";
import type { NavLinkLikeProps } from "./Shell";

export interface AreaTab {
  id: string;
  label: string;
  /** Route of the tab (link mode, needs `linkComponent`). */
  to?: string;
  /** Optional count shown as "Label · 12" (prototype "Directory · 12"). */
  count?: number;
}

export interface AreaHeaderProps {
  title: ReactNode;
  /** One live summary line built from real data ("4,812 enrolled · 25 recent records shown"). Never institution metadata. */
  summary?: ReactNode;
  /** Back link eyebrow for detail pages ("Students"). */
  back?: { label: string; to: string };
  /** Header tabs: at most 5 are shown; from the 6th on, the 5th slot becomes a "More" menu. */
  tabs?: AreaTab[];
  /** Id of the active tab (required for button tabs; for link tabs it lets "More" show the active overflow tab). */
  activeTab?: string;
  /** Button mode: called with the tab id (no `linkComponent` / `to`). */
  onTabChange?: (id: string) => void;
  linkComponent?: ComponentType<NavLinkLikeProps>;
  /** Header search pill. */
  search?: { label: string; placeholder?: string; value: string; onChange: (v: string) => void };
  /** Right-aligned actions: one primary `Button` + at most one ghost button. */
  actions?: ReactNode;
  /** Label of the tab strip for assistive tech (default "Sections"). */
  tabsLabel?: string;
  children?: ReactNode;
  [key: `data-${string}`]: string | undefined;
}

export const MAX_AREA_TABS = 5;

/** Splits tabs into the visible strip and the overflow menu (≤5 visible; 6+ → 4 + "More"). */
export function splitTabs<T>(tabs: T[], max = MAX_AREA_TABS): { visible: T[]; overflow: T[] } {
  if (tabs.length <= max) return { visible: tabs, overflow: [] };
  return { visible: tabs.slice(0, max - 1), overflow: tabs.slice(max - 1) };
}

const tabText = (t: AreaTab) => (t.count !== undefined ? `${t.label} · ${t.count.toLocaleString("en-IN")}` : t.label);

/**
 * Area overview header (prototype `.ph`): h1 + one live summary line on the left; tabs, search
 * and the primary action right-aligned in the same tile. Replaces `PageHeader` + breadcrumb
 * subtitles on every area page (docs/reskin2-contracts.md §2).
 */
export function AreaHeader({ title, summary, back, tabs, activeTab, onTabChange, linkComponent: Link, search, actions, tabsLabel = "Sections", children, ...rest }: AreaHeaderProps) {
  const { visible, overflow } = splitTabs(tabs ?? []);
  const hasSide = visible.length > 0 || search || actions;
  return (
    <BentoTile span={12} className="sb-area-head" data-area-head="" {...rest}>
      <div className="sb-area-title">
        {back &&
          (Link ? (
            <Link to={back.to} className="sb-back">
              <ArrowLeft strokeWidth={1.75} aria-hidden="true" />
              {back.label}
            </Link>
          ) : (
            <a href={back.to} className="sb-back">
              <ArrowLeft strokeWidth={1.75} aria-hidden="true" />
              {back.label}
            </a>
          ))}
        <h1>{title}</h1>
        {summary && (
          <p className="sb-area-summary" data-area-summary="">
            {summary}
          </p>
        )}
      </div>
      {hasSide && (
        <div className="sb-area-side">
          {search && <SearchField label={search.label} placeholder={search.placeholder ?? search.label} value={search.value} onChange={(e) => search.onChange(e.target.value)} />}
          {visible.length > 0 && (
            <div className="sb-tabs-wrap">
              <nav className="sb-tabs" aria-label={tabsLabel}>
                {visible.map((t) =>
                  Link && t.to ? (
                    <Link key={t.id} to={t.to} end className={({ isActive }) => cx(isActive && "on")}>
                      {tabText(t)}
                    </Link>
                  ) : (
                    <button key={t.id} type="button" className={cx(activeTab === t.id && "on")} aria-pressed={activeTab === t.id} data-tab={t.id} onClick={() => onTabChange?.(t.id)}>
                      {tabText(t)}
                    </button>
                  ),
                )}
                {overflow.length > 0 && <MoreTabs tabs={overflow} activeTab={activeTab} onTabChange={onTabChange} Link={Link} />}
              </nav>
            </div>
          )}
          {actions}
        </div>
      )}
      {children}
    </BentoTile>
  );
}

function MoreTabs({ tabs, activeTab, onTabChange, Link }: { tabs: AreaTab[]; activeTab?: string; onTabChange?: (id: string) => void; Link?: ComponentType<NavLinkLikeProps> }) {
  const { open, setOpen, close, ref } = useMenu();
  const current = tabs.find((t) => t.id === activeTab);
  const items: MenuItem[] = tabs.map((t) => ({
    id: t.id,
    label: tabText(t),
    active: t.id === activeTab,
    onSelect: Link && t.to ? undefined : () => onTabChange?.(t.id),
    render:
      Link && t.to
        ? ({ className, close: done }) => (
            <span role="none" onClick={done}>
              <Link to={t.to ?? ""} end className={({ isActive }) => cx(className, isActive && "on")}>
                {tabText(t)}
              </Link>
            </span>
          )
        : undefined,
  }));
  return (
    <span ref={ref} style={{ position: "relative", display: "inline-flex" }}>
      <button type="button" className={cx("sb-tab-more", current && "on")} aria-haspopup="menu" aria-expanded={open} data-tab-more="" onClick={() => setOpen((o) => !o)}>
        {current ? `More · ${current.label}` : "More"}
        <ChevronDown strokeWidth={2} aria-hidden="true" />
      </button>
      {open && <MenuList items={items} close={close} label="More sections" />}
    </span>
  );
}
