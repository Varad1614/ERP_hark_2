import { Building2 } from "lucide-react";
import { BentoGrid, BentoTile, Select } from "@sutra/ui";
import type { ReactNode } from "react";
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
  children,
}: {
  title: string;
  base: string;
  tabs: readonly TabDef[];
  tab: string | undefined;
  children: (institution: Institution | null, tab: TabDef) => ReactNode;
}) {
  const { role } = useSession();
  const current = activeTab(tabs, tab);
  const { list, current: institution, query } = useInstitutions();
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
      <BentoTile span={12} variant="accent">
        <div className="sb-page-head">
          <div>
            <span className="sb-chip">
              <Building2 strokeWidth={1.75} /> {title}
            </span>
            <h1>{current.label}</h1>
            {institution ? (
              <p data-current-institution={institution.id}>
                {institution.name} · {institution.code} · {institution.timezone} <StatusBadge status={institution.status} />
              </p>
            ) : (
              <p>{query.isLoading ? "Loading institutions…" : "No institution registered yet."}</p>
            )}
          </div>
          {list.length > 1 && institution && (
            <div style={{ minWidth: 240 }}>
              <Select
                label="Institution"
                name="institution"
                value={institution.id}
                options={list.map((i) => ({ value: i.id, label: `${i.name} (${i.code})` }))}
                onChange={(e) => selectInstitution(e.target.value)}
              />
            </div>
          )}
        </div>
        <nav className="sb-tabs" aria-label={`${title} sections`}>
          {tabs.map((t) => (
            <NavLink key={t.key} to={`${base}/${t.key}`} className={t.key === current.key ? "on" : ""} aria-current={t.key === current.key ? "page" : undefined} data-tab-link={t.key}>
              {t.label}
            </NavLink>
          ))}
        </nav>
      </BentoTile>
      {children(institution, current)}
    </BentoGrid>
  );
}
