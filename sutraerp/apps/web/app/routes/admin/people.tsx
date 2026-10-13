import { useParams } from "react-router";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { PEOPLE_TABS } from "../../features/people";
import { AttendanceAdminTab, LeaveAdminTab, WorkloadAdminTab } from "../../features/people/leave";
import { PayrollAdminTab } from "../../features/people/payroll";
import { PositionsAdminTab, RecruitmentAdminTab, StaffAdminTab } from "../../features/people/workforce";

export function meta() {
  return [{ title: "People · SutraERP" }];
}

/**
 * Admin → People (`/people/:tab?`). Foundation frame: every tab is already wired to its slice's
 * component (docs/batch7-contracts.md §10), so slices never edit this file.
 */
export default function People() {
  const { tab } = useParams();
  return (
    <AdminScreen title="People" base="/people" tabs={PEOPLE_TABS} tab={tab}>
      {(institution, current) => {
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
      }}
    </AdminScreen>
  );
}
