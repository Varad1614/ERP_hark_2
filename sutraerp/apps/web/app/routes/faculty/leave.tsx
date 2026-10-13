import { PageHeader, Tabs, type NavLinkLikeProps } from "@sutra/ui";
import type { ComponentType } from "react";
import { NavLink, useParams } from "react-router";
import { RoleScreen } from "../../features/academic/RoleScreen";
import { activeTab, MY_HR_TABS, myHrPath } from "../../features/people";
import { MyAttendancePanel, MyLeavePanel, MyLeaveStats, MyWorkloadPanel } from "../../features/people/leave";
import { MyPayslipsPanel } from "../../features/people/payroll";
import { MyProfilePanel } from "../../features/people/workforce";

export function meta() {
  return [{ title: "Leave · SutraERP" }];
}

const TabLink = NavLink as unknown as ComponentType<NavLinkLikeProps>;

/**
 * Faculty Leave (`/leave/:tab?`, My HR): leave balances, requests and manager approvals (7B),
 * the staff record (7A), attendance and workload (7B) and payslips (7C), one tab each.
 */
export default function Leave() {
  const { tab } = useParams();
  const current = activeTab(MY_HR_TABS, tab);
  return (
    <RoleScreen screen="leave" roles={["faculty"]} title="Leave" note="is the staff view; HR uses People.">
      {() => (
        <>
          <PageHeader title="Leave" subtitle={`My HR · ${current.label} · leave, your staff record, attendance, workload and payslips`} data-page-head="leave" data-tab={current.key}>
            <Tabs label="My HR sections" linkComponent={TabLink} items={MY_HR_TABS.map((t) => ({ id: t.key, label: t.label, to: myHrPath(t.key) }))} />
          </PageHeader>
          {current.key === "profile" ? (
            <MyProfilePanel />
          ) : current.key === "attendance" ? (
            <MyAttendancePanel />
          ) : current.key === "workload" ? (
            <MyWorkloadPanel />
          ) : current.key === "payslips" ? (
            <MyPayslipsPanel />
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
