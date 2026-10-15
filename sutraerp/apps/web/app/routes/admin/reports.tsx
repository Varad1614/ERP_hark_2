import { useParams } from "react-router";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { REPORTS_TABS } from "../../features/reports";
import { DashboardsAdminTab, RegulatoryAdminTab } from "../../features/reports/analytics";
import { AccreditationAdminTab } from "../../features/reports/accreditation"; // 9E

export function meta() {
  return [{ title: "Reports · SutraERP" }];
}

/**
 * Admin → Reports (`/reports/:tab?`). Foundation frame: dashboards and regulatory reporting are
 * wired to slice 8C's components (docs/batch8-contracts.md §10), so 8C never edits this file.
 */
export default function Reports() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Reports" base="/reports" tabs={REPORTS_TABS} tab={tab}>
      {(institution, current) =>
        current.key === "regulatory" ? (
          <RegulatoryAdminTab institution={institution} />
        ) : current.key === "accreditation" ? ( // 9E
          <AccreditationAdminTab institution={institution} />
        ) : (
          <DashboardsAdminTab institution={institution} />
        )
      }
    </AdminScreen>
  );
}
