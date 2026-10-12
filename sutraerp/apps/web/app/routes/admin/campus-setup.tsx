import { useParams } from "react-router";
import { CAMPUS_SETUP_TABS } from "../../features/institution";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { CampusesTab } from "../../features/institution/CampusesTab";
import { InstitutionTab } from "../../features/institution/InstitutionTab";
import { OrganisationTab } from "../../features/institution/OrganisationTab";
import { PoliciesTab } from "../../features/institution/PoliciesTab";
import { PositionsTab } from "../../features/institution/PositionsTab";
import { ReadinessTab } from "../../features/institution/ReadinessTab";

export function meta() {
  return [{ title: "Campus setup · SutraERP" }];
}

/** Admin → Campus setup (`/campus/:tab?`): institution, readiness, campuses, organisation, positions, policies. Slice 2C. */
export default function CampusSetup() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Campus setup" base="/campus" tabs={CAMPUS_SETUP_TABS} tab={tab}>
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
          default:
            return <InstitutionTab institution={institution} />;
        }
      }}
    </AdminScreen>
  );
}
