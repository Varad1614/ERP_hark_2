import { useParams } from "react-router";
import { AcademicsOverview, academicsLine, useAcademicsData } from "../../features/academic/admin/overview";
import { ACADEMICS_AREA_TABS, tabKey } from "../../features/academic/admin/tabs";
import { AcademicFrame } from "../../features/academic/kit";
import { CalendarTab } from "../../features/institution/CalendarTab";
import { CurriculumTab, ProgrammesTab, SchoolModelTab, StructureTab } from "../../features/institution/CurriculumTabs";
import { useInstitutions } from "../../features/institution/hooks";

export function meta() {
  return [{ title: "Academics · SutraERP" }];
}

/**
 * Admin → Academics (`/academics/:tab?`), re-skin v2 (R3): overview (programme cards + courses),
 * curriculum, calendar, delivery (→ /academic-delivery) + More: structure, programmes, school model.
 */
export default function Academics() {
  const { tab } = useParams();
  const current = tabKey(ACADEMICS_AREA_TABS, tab);
  const { current: inst } = useInstitutions();
  const data = useAcademicsData(inst?.id);
  return (
    <AcademicFrame title="Academics" base="/academics" tabs={ACADEMICS_AREA_TABS} current={current} summary={() => academicsLine(data)}>
      {(institution) => {
        switch (current) {
          case "programmes":
            return <ProgrammesTab institution={institution} />;
          case "curriculum":
            return <CurriculumTab institution={institution} />;
          case "school-model":
            return <SchoolModelTab institution={institution} />;
          case "calendar":
            return <CalendarTab institution={institution} />;
          case "structure":
            return <StructureTab institution={institution} />;
          default:
            return <AcademicsOverview institution={institution} />;
        }
      }}
    </AcademicFrame>
  );
}
