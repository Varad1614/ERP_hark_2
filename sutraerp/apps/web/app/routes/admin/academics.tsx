import { useParams } from "react-router";
import { ACADEMICS_TABS } from "../../features/institution";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { CalendarTab } from "../../features/institution/CalendarTab";
import { CurriculumTab, ProgrammesTab, SchoolModelTab, StructureTab } from "../../features/institution/CurriculumTabs";

export function meta() {
  return [{ title: "Academics · SutraERP" }];
}

/** Admin → Academics (`/academics/:tab?`): structure, programmes, curriculum, school model, calendar. Slice 2C. */
export default function Academics() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Academics" base="/academics" tabs={ACADEMICS_TABS} tab={tab}>
      {(institution, current) => {
        switch (current.key) {
          case "programmes":
            return <ProgrammesTab institution={institution} />;
          case "curriculum":
            return <CurriculumTab institution={institution} />;
          case "school-model":
            return <SchoolModelTab institution={institution} />;
          case "calendar":
            return <CalendarTab institution={institution} />;
          default:
            return <StructureTab institution={institution} />;
        }
      }}
    </AdminScreen>
  );
}
