import { AreaHeader, formatDays, type NavLinkLikeProps } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import type { ComponentType } from "react";
import { NavLink, useParams } from "react-router";
import { RoleScreen } from "../../features/academic/RoleScreen";
import { activeTab, MY_HR_TABS, myHrPath } from "../../features/people";
import { MyAttendancePanel, MyLeavePanel, MyLeaveStats, MyWorkloadPanel } from "../../features/people/leave";
import { lkeys, useLeaveApi } from "../../features/people/leave/hooks";
import { MyPayslipsPanel } from "../../features/people/payroll";
import { MyProfilePanel } from "../../features/people/workforce";
import { MyExpensesTile } from "../../features/campus/resources/expenses/MyExpensesTile";
import { MyTimePanel } from "../../features/people/timetrack/MyTimePanel"; // ERP_EXTENSIONS 5C
import { MyDutyRoster } from "../../features/people/roster/MyDutyRoster"; // ERP_EXTENSIONS 5B

export function meta() {
  return [{ title: "Leave · SutraERP" }];
}

const TabLink = NavLink as unknown as ComponentType<NavLinkLikeProps>;

/** Live summary: "Leave year 2026 · 20 days left · 1 application awaiting approval". */
function useLeaveSummary(): string {
  const api = useLeaveApi();
  const balances = useQuery({ queryKey: lkeys("me", "balances"), queryFn: () => api.myBalances(), retry: false });
  const requests = useQuery({ queryKey: lkeys("me", "requests"), queryFn: () => api.myRequests(), retry: false });
  if (balances.isError) return "Leave, your staff record, attendance, workload and payslips";
  const rows = (balances.data ?? []).filter((b) => Number(b.entitled) > 0);
  const year = rows[0]?.leaveYear ?? new Date().getFullYear();
  const left = rows.reduce((n, b) => n + Math.max(0, Number(b.available)), 0);
  const waiting = (requests.data ?? []).filter((r) => r.status === "pending_approval").length;
  return [`Leave year ${year}`, rows.length ? `${formatDays(left)} left` : null, waiting ? `${waiting} ${waiting === 1 ? "application" : "applications"} awaiting approval` : null].filter(Boolean).join(" · ");
}

function LeaveHeader({ current }: { current: string }) {
  const summary = useLeaveSummary();
  return (
    <AreaHeader
      title="Leave"
      summary={summary}
      tabs={MY_HR_TABS.map((t) => ({ id: t.key, label: t.label, to: myHrPath(t.key) }))}
      activeTab={current}
      linkComponent={TabLink}
      tabsLabel="My HR sections"
      data-page-head="leave"
      data-tab={current}
    />
  );
}

/**
 * Faculty Leave (`/leave/:tab?`, My HR), re-skin v2 (prototype fac-leave.png): balance tiles, the
 * inline Apply form and History on the default tab; My record, Attendance, Workload and Payslips
 * stay one header tab each (5 tabs, docs/reskin2-contracts.md §2.1).
 */
export default function Leave() {
  const { tab } = useParams();
  const current = activeTab(MY_HR_TABS, tab);
  return (
    <RoleScreen screen="leave" roles={["faculty"]} title="Leave" note="is the staff view; HR uses People.">
      {() => (
        <>
          <LeaveHeader current={current.key} />
          {current.key === "profile" ? (
            <MyProfilePanel />
          ) : current.key === "attendance" ? (
            <>
              <MyAttendancePanel />
              <MyDutyRoster />
            </>
          ) : current.key === "workload" ? (
            <>
              <MyWorkloadPanel />
              <MyTimePanel />
            </>
          ) : current.key === "payslips" ? (
            <>
              <MyPayslipsPanel />
              <MyExpensesTile />
            </>
          ) : (
            <>
              <MyLeaveStats />
              <MyLeavePanel />
            </>
          )}
        </>
      )}
    </RoleScreen>
  );
}
