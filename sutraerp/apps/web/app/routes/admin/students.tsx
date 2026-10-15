import { useParams } from "react-router";
import { StudentsArea } from "../../features/student/StudentsArea";
import { ExitsAdminTab, GuardiansAdminTab, ScholarshipsAdminTab } from "../../features/student/completion";
import { AdmissionsTab, StudentsDirectoryTab } from "../../features/student/record";
import { ProgressionAdminTab, RegistrationAdminTab } from "../../features/student/registration";

export function meta() {
  return [{ title: "Students · SutraERP" }];
}

/**
 * Admin → Students (`/students/:tab?`). Re-skin v2 (R2): `StudentsArea` renders the prototype
 * area header (live summary, ≤5 tabs + More, search, New admission); each tab is its slice's component.
 */
export default function Students() {
  const { tab } = useParams();
  return (
    <StudentsArea tab={tab}>
      {(institution, key, q) => {
        switch (key) {
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
            return <StudentsDirectoryTab institution={institution} q={q} />;
        }
      }}
    </StudentsArea>
  );
}
