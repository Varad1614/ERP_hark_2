/**
 * Admin → Students area frame (re-skin v2, R2): one `AreaHeader` — "Students", a live summary,
 * the header search (directory), ≤5 tabs (Directory · n / Admissions · n / Registration /
 * Progression + More: Completion & exits, Guardians, Scholarships) and the primary "New admission".
 * Replaces the Batch 2 `AdminScreen` (breadcrumb subtitle + 7-tab strip) for `/students/:tab?`.
 */
import { UserPlus } from "lucide-react";
import { AreaHeader, BentoGrid, BentoTile, FormSelect, plural, type AreaTab, type NavLinkLikeProps } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { useState, type ReactNode } from "react";
import { NavLink } from "react-router";
import { useSession } from "../../lib/auth";
import { selectInstitution, useInstitutions } from "../institution/hooks";
import type { Institution } from "../institution/types";
import { activeTab, STUDENTS_TABS } from "./index";
import { NewAdmissionDialog, useDirectory } from "./record/AdminTabs";
import { rkeys, useRecordApi } from "./record/hooks";

/** Router link for `AreaHeader` tabs and back links. */
export function TabLink({ to, className, end, children }: NavLinkLikeProps) {
  return (
    <NavLink to={to} end={end} className={className as never}>
      {children}
    </NavLink>
  );
}

const IN_PROGRESS = new Set(["draft", "submitted", "under_review"]);

export function StudentsArea({ tab, children }: { tab: string | undefined; children: (institution: Institution | null, key: string, q: string) => ReactNode }) {
  const { role } = useSession();
  const current = activeTab(STUDENTS_TABS, tab);
  const { list, current: institution, query } = useInstitutions();
  const [q, setQ] = useState("");
  const [adding, setAdding] = useState(false);
  const api = useRecordApi();
  const all = useDirectory(role === "administrator" ? institution?.id : null);
  const apps = useQuery({ queryKey: rkeys.scoped(institution?.id ?? "-", "applications"), queryFn: () => api.listApplications(institution!.id), enabled: !!institution && role === "administrator" });
  if (role !== "administrator") {
    return (
      <BentoGrid data-screen="students">
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Students is for administrators.</p>
        </BentoTile>
      </BentoGrid>
    );
  }
  const students = all.data ?? [];
  const enrolled = students.filter((s) => s.currentContext?.status === "active").length;
  const pending = (apps.data ?? []).filter((a) => IN_PROGRESS.has(a.status)).length;
  const tabs: AreaTab[] = STUDENTS_TABS.map((t) => ({
    id: t.key,
    label: t.label,
    to: t.key === "directory" ? "/students" : `/students/${t.key}`,
    count: t.key === "directory" && all.data ? students.length : t.key === "admissions" && apps.data ? pending : undefined,
  }));
  const summary = !institution
    ? query.isLoading
      ? "Loading…"
      : "No institution registered yet."
    : all.data
      ? [`${enrolled.toLocaleString("en-IN")} enrolled`, pending ? `${plural(pending, "application")} in progress` : null, current.key === "directory" && q ? `searching “${q}”` : null].filter(Boolean).join(" · ")
      : "Loading students…";
  return (
    <BentoGrid data-screen="students" data-tab={current.key}>
      <AreaHeader
        title="Students"
        summary={<span data-current-institution={institution?.id}>{summary}</span>}
        tabs={tabs}
        activeTab={current.key}
        linkComponent={TabLink}
        tabsLabel="Students sections"
        search={current.key === "directory" ? { label: "Search students", placeholder: "Search name or roll no.", value: q, onChange: setQ } : undefined}
        actions={
          <>
            {list.length > 1 && institution && (
              <div style={{ minWidth: 220 }}>
                <FormSelect label="Institution" hideLabel name="institution" value={institution.id} options={list.map((i) => ({ value: i.id, label: i.name }))} onChange={(e) => selectInstitution(e.target.value)} />
              </div>
            )}
            <button type="button" className="sb-btn" disabled={!institution} onClick={() => setAdding(true)} data-new-application="">
              <UserPlus strokeWidth={1.75} aria-hidden="true" />
              New admission
            </button>
          </>
        }
      />
      {children(institution, current.key, q)}
      {institution && <NewAdmissionDialog institution={institution} open={adding} onClose={() => setAdding(false)} />}
    </BentoGrid>
  );
}
