import { useParams } from "react-router";
import { AdminScreen } from "../../features/institution/AdminScreen";
import { STUDENTS_TABS } from "../../features/student";
import { ExitsAdminTab, GuardiansAdminTab, ScholarshipsAdminTab } from "../../features/student/completion";
import { AdmissionsTab, StudentsDirectoryTab } from "../../features/student/record";
import { ProgressionAdminTab, RegistrationAdminTab } from "../../features/student/registration";

export function meta() {
  return [{ title: "Students · SutraERP" }];
}

/**
 * Admin → Students (`/students/:tab?`). Frame owned by 3A; every tab is already wired to its
 * slice's component (docs/batch3-contracts.md §10), so 3B/3C never edit this file.
 */
export default function Students() {
  const { tab } = useParams();
  return (
    <AdminScreen title="Students" base="/students" tabs={STUDENTS_TABS} tab={tab}>
      {(institution, current) => {
        switch (current.key) {
          case "admissions":
            return <AdmissionsTab institution={institution} />;
          case "registration":
            return <RegistrationAdminTab institution={institution} />;
          case "progression":
            return <ProgressionAdminTab institution={institution} />;
          case "exits":
            return <ExitsAdminTab institution={institution} />;
          case "guardians":
            return <GuardiansAdminTab institution={institution} />;
          case "scholarships":
            return <ScholarshipsAdminTab institution={institution} />;
          default:
            return <StudentsDirectoryTab institution={institution} />;
        }
      }}
    </AdminScreen>
  );
}
