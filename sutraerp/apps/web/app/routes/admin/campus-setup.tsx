import { useParams } from "react-router";
import { CAMPUS_TABS } from "../../features/campus";
import { FacilitiesAdminTab, InfrastructureAdminTab, LabsAdminTab } from "../../features/campus/facilities";
import { LibraryAdminTab } from "../../features/campus/library";
import { HostelAdminTab, TransportAdminTab } from "../../features/campus/residence";
import { AssetsAdminTab, InventoryAdminTab, ProcurementAdminTab } from "../../features/campus/resources";
import { GovernanceAdminTab } from "../../features/campus/governance"; // 9E
import { AdminScreen } from "../../features/institution/AdminScreen";
import { CampusesTab } from "../../features/institution/CampusesTab";
import { InstitutionTab } from "../../features/institution/InstitutionTab";
import { OrganisationTab } from "../../features/institution/OrganisationTab";
import { PoliciesTab } from "../../features/institution/PoliciesTab";
import { PositionsTab } from "../../features/institution/PositionsTab";
import { ReadinessTab } from "../../features/institution/ReadinessTab";

export function meta() {
  return [{ title: "Campus · SutraERP" }];
}

/**
 * Admin → Campus (`/campus/:tab?`): the Batch 2 campus setup tabs (institution, readiness,
 * campuses, organisation, positions, policies — slice 2C) followed by the Batch 8 tabs, each
 * already wired to its slice's component (docs/batch8-contracts.md §10): infrastructure,
 * facilities, labs (8A), hostel, transport (8B), library (8C), procurement, inventory, assets (8D).
 * Slices never edit this file.
 */
export default function CampusSetup() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Campus" base="/campus" tabs={CAMPUS_TABS} tab={tab}>
      {(institution, current) => {
        switch (current.key) {
          case "readiness":
            return <ReadinessTab institution={institution} />;
          case "campuses":
            return <CampusesTab institution={institution} />;
          case "organisation":
            return <OrganisationTab institution={institution} />;
          case "positions":
            return <PositionsTab institution={institution} />;
          case "policies":
            return <PoliciesTab institution={institution} />;
          case "infrastructure":
            return <InfrastructureAdminTab institution={institution} />;
          case "facilities":
            return <FacilitiesAdminTab institution={institution} />;
          case "labs":
            return <LabsAdminTab institution={institution} />;
          case "hostel":
            return <HostelAdminTab institution={institution} />;
          case "transport":
            return <TransportAdminTab institution={institution} />;
          case "library":
            return <LibraryAdminTab institution={institution} />;
          case "procurement":
            return <ProcurementAdminTab institution={institution} />;
          case "inventory":
            return <InventoryAdminTab institution={institution} />;
          case "assets":
            return <AssetsAdminTab institution={institution} />;
          case "governance": // 9E
            return <GovernanceAdminTab institution={institution} />;
          default:
            return <InstitutionTab institution={institution} />;
        }
      }}
    </AdminScreen>
  );
}
