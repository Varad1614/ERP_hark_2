import { Navigate, useParams } from "react-router";
import { CAMPUS_GROUPS, campusGroupOf, SECTION_LABELS } from "../../features/campus";
import { AreaFrame, type AreaGroupTab } from "../../features/campus/AreaFrame";
import { FacilitiesAdminTab, InfrastructureAdminTab, LabsAdminTab } from "../../features/campus/facilities";
import { LibraryAdminTab } from "../../features/campus/library";
import { campusSummary, CampusOverview, useCampusFigures } from "../../features/campus/Overview";
import { HostelAdminTab, TransportAdminTab } from "../../features/campus/residence";
import { AssetsAdminTab, InventoryAdminTab, ProcurementAdminTab } from "../../features/campus/resources";
import { useInstitutions } from "../../features/institution/hooks";
import { CampusesTab } from "../../features/institution/CampusesTab";
import { InstitutionTab } from "../../features/institution/InstitutionTab";
import { OrganisationTab } from "../../features/institution/OrganisationTab";
import { PoliciesTab } from "../../features/institution/PoliciesTab";
import { PositionsTab } from "../../features/institution/PositionsTab";
import { ReadinessTab } from "../../features/institution/ReadinessTab";

export function meta() {
  return [{ title: "Campus · SutraERP" }];
}

/** Header tabs: Overview + four groups; each group tab opens its first section and stays active on the others. */
const TABS: AreaGroupTab[] = CAMPUS_GROUPS.map((g) => ({
  id: g.key,
  label: g.label,
  to: g.sections.length ? `/campus/${g.sections[0]}` : "/campus",
  match: g.key === "overview" ? ["/campus/overview"] : (g.sections as readonly string[]).map((s) => `/campus/${s}`),
}));

/**
 * Admin → Campus (`/campus/:tab?`), re-skin v2 (R7): five header tabs — Overview (the prototype
 * Campus page), Spaces (rooms & buildings, laboratories, hostel), Services (service desk,
 * transport, library), Resources (procurement, inventory, assets) and Setup (institution,
 * readiness, campuses, organisation, positions, policies) — with each group's sections as
 * sub-section chips. Every old `/campus/<section>` URL still opens its section; Governance moved
 * to Reports (`/campus/governance` redirects).
 */
export default function CampusSetup() {
  const { tab } = useParams();
  const { current } = useInstitutions();
  const figures = useCampusFigures(current?.id ?? null);
  if (tab === "governance") return <Navigate to="/reports/governance" replace />;
  const group = campusGroupOf(tab);
  const sections = group.sections as readonly string[];
  const section = sections.length ? (tab && sections.includes(tab) ? tab : sections[0]) : undefined;
  const chips = (group.sections as readonly string[]).map((s) => ({ id: s, label: SECTION_LABELS[s] ?? s, to: `/campus/${s}` }));
  return (
    <AreaFrame screen="campus" title="Campus" summary={campusSummary(figures)} tabs={TABS} activeTab={group.key} chips={chips} activeChip={section}>
      {(institution) => {
        switch (section) {
          case undefined:
            // No institution yet → the registration flow of Setup → Institution.
            return institution ? <CampusOverview institutionId={institution.id} /> : <InstitutionTab institution={null} />;
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
          default:
            return <InstitutionTab institution={institution} />;
        }
      }}
    </AreaFrame>
  );
}
