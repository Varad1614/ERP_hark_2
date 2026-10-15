import { BentoTile } from "@sutra/ui";
import { useParams } from "react-router";
import { GovernanceAdminTab } from "../../features/campus/governance"; // 9E (moved from Campus in re-skin v2)
import { AreaFrame, type AreaGroupTab } from "../../features/campus/AreaFrame";
import { NoInstitution } from "../../features/institution/forms";
import { activeTab, REPORTS_TABS } from "../../features/reports";
import { AccreditationAdminTab } from "../../features/reports/accreditation"; // 9E
import { DashboardsAdminTab, RegulatoryAdminTab } from "../../features/reports/analytics";
import { ReportsOverview, STANDARD_REPORTS } from "../../features/reports/Overview";

export function meta() {
  return [{ title: "Reports · SutraERP" }];
}

const TABS: AreaGroupTab[] = REPORTS_TABS.map((t) => ({ id: t.key, label: t.label, to: t.key === "overview" ? "/reports" : `/reports/${t.key}`, match: t.key === "overview" ? ["/reports/overview"] : [] }));

const SUMMARY: Record<string, string> = {
  overview: `${STANDARD_REPORTS.length} standard reports from live figures · open one for detail and a CSV export`,
  dashboards: "Dashboards built from the institution’s figures · versioned, approved and assigned by role",
  regulatory: "Regulatory returns · frameworks, collection cycles, validation and submission",
  accreditation: "Accreditation · frameworks, cycles, evidence records and self-study",
  governance: "Campus governance · assessments, reviews, risks and action plans",
};

/**
 * Admin → Reports (`/reports/:tab?`), re-skin v2 (R7): Overview (the prototype report cards with a
 * chart dialog + CSV), Dashboards and Regulatory (slice 8C), Accreditation (H09) and Governance
 * (G00, moved here from Campus) — five header tabs, no breadcrumb subtitle.
 */
export default function Reports() {
  const { tab } = useParams();
  const current = activeTab(REPORTS_TABS, tab);
  return (
    <AreaFrame screen="reports" title="Reports" summary={SUMMARY[current.key]} tabs={TABS} activeTab={current.key}>
      {(institution) => {
        switch (current.key) {
          case "dashboards":
            return <DashboardsAdminTab institution={institution} />;
          case "regulatory":
            return <RegulatoryAdminTab institution={institution} />;
          case "accreditation":
            return <AccreditationAdminTab institution={institution} />;
          case "governance":
            return <GovernanceAdminTab institution={institution} />;
          default:
            return institution ? (
              <ReportsOverview institutionId={institution.id} />
            ) : (
              <BentoTile span={12} title="Standard reports" data-slice="8C">
                <NoInstitution />
              </BentoTile>
            );
        }
      }}
    </AreaFrame>
  );
}
