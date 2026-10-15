import { AreaHeader, BentoGrid, BentoTile, FormSelect, plural, type NavLinkLikeProps } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import type { ComponentType } from "react";
import { NavLink, useParams } from "react-router";
import { useSession } from "../../lib/auth";
import { selectInstitution, useInstitutions } from "../../features/institution/hooks";
import { activeTab, PEOPLE_TABS, peoplePath } from "../../features/people";
import { AttendanceAdminTab, LeaveAdminTab, WorkloadAdminTab } from "../../features/people/leave";
import { lkeys, useLeaveApi } from "../../features/people/leave/hooks";
import { PayrollAdminTab } from "../../features/people/payroll";
import { PositionsAdminTab, RecruitmentAdminTab, StaffAdminTab } from "../../features/people/workforce";
import { useWorkforceApi, wkeys } from "../../features/people/workforce/hooks";

export function meta() {
  return [{ title: "People · SutraERP" }];
}

const TabLink = NavLink as unknown as ComponentType<NavLinkLikeProps>;

/**
 * Admin → People (`/people/:tab?`), re-skin v2 (prototype adm-people.png): one area header with a
 * live summary and Directory · n / Leave requests · n / Payroll / Recruitment + More (positions,
 * attendance, workload). Every tab keeps its slice component and behaviour.
 */
export default function People() {
  const { tab } = useParams();
  const { role } = useSession();
  const current = activeTab(PEOPLE_TABS, tab);
  const { list, current: institution, query } = useInstitutions();
  const inst = institution?.id ?? "";
  const workforce = useWorkforceApi();
  const leave = useLeaveApi();
  const staff = useQuery({ queryKey: wkeys.scoped(inst, "staff:"), queryFn: () => workforce.staff(inst), enabled: !!inst && role === "administrator" }).data;
  const pending = useQuery({ queryKey: lkeys(inst, "requests:pending_approval"), queryFn: () => leave.requests(inst, "pending_approval"), enabled: !!inst && role === "administrator" }).data;
  if (role !== "administrator") {
    return (
      <BentoGrid data-screen="people">
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>People is for administrators.</p>
        </BentoTile>
      </BentoGrid>
    );
  }
  const faculty = staff?.filter((s) => s.staffCategory === "teaching" && s.engagement?.status === "active").length;
  const pendingCount = pending?.items.length;
  const summary = !institution
    ? query.isLoading
      ? "Loading institutions…"
      : "No institution registered yet."
    : staff
      ? [`${faculty ?? 0} faculty on roll`, plural(staff.length, "staff record"), pendingCount !== undefined ? `${pendingCount.toLocaleString("en-IN")} leave ${pendingCount === 1 ? "request" : "requests"} to review` : null].filter(Boolean).join(" · ")
      : "Loading staff…";
  const counts: Record<string, number | undefined> = { staff: staff?.length, leave: pendingCount };
  return (
    <BentoGrid data-screen="people" data-tab={current.key}>
      <AreaHeader
        title="People"
        summary={<span data-current-institution={institution?.id}>{summary}</span>}
        tabs={PEOPLE_TABS.map((t) => ({ id: t.key, label: t.label, to: peoplePath(t.key), count: counts[t.key] }))}
        activeTab={current.key}
        linkComponent={TabLink}
        tabsLabel="People sections"
        actions={
          list.length > 1 && institution ? (
            <div style={{ minWidth: 220 }}>
              <FormSelect label="Institution" hideLabel name="institution" value={institution.id} options={list.map((i) => ({ value: i.id, label: i.name }))} onChange={(e) => selectInstitution(e.target.value)} />
            </div>
          ) : undefined
        }
        data-page-head="people"
      />
      {(() => {
        switch (current.key) {
          case "recruitment":
            return <RecruitmentAdminTab institution={institution} />;
          case "positions":
            return <PositionsAdminTab institution={institution} />;
          case "leave":
            return <LeaveAdminTab institution={institution} />;
          case "attendance":
            return <AttendanceAdminTab institution={institution} />;
          case "workload":
            return <WorkloadAdminTab institution={institution} />;
          case "payroll":
            return <PayrollAdminTab institution={institution} />;
          default:
            return <StaffAdminTab institution={institution} />;
        }
      })()}
    </BentoGrid>
  );
}
