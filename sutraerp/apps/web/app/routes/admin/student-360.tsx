import { BentoGrid, BentoTile } from "@sutra/ui";
import { NavLink, useParams } from "react-router";
import { useSession } from "../../lib/auth";
import { activeTab, STUDENT_360_TABS, student360Path } from "../../features/student";
import { Student360Completion, Student360Guardians, Student360Scholarships } from "../../features/student/completion";
import { Student360Documents, Student360Enrolment, Student360Overview } from "../../features/student/record";
import { Student360Progression, Student360Registration } from "../../features/student/registration";

export function meta() {
  return [{ title: "Student 360 · SutraERP" }];
}

/**
 * Student 360 (`/students/record/:studentId/:tab?`). Frame owned by 3A; each tab renders its
 * slice's component (docs/batch3-contracts.md §10). Staff only (faculty read-only, admin write).
 */
export default function Student360() {
  const { studentId = "", tab } = useParams();
  const { role } = useSession();
  const current = activeTab(STUDENT_360_TABS, tab);
  if (role === "student") {
    return (
      <BentoGrid data-screen="student-360">
        <BentoTile variant="accent" className="sb-soon">
          <h1>This screen isn’t available</h1>
          <p>Student 360 is for staff.</p>
        </BentoTile>
      </BentoGrid>
    );
  }
  const body = () => {
    switch (current.key) {
      case "enrolment":
        return <Student360Enrolment studentId={studentId} />;
      case "documents":
        return <Student360Documents studentId={studentId} />;
      case "registration":
        return <Student360Registration studentId={studentId} />;
      case "progression":
        return <Student360Progression studentId={studentId} />;
      case "guardians":
        return <Student360Guardians studentId={studentId} />;
      case "scholarships":
        return <Student360Scholarships studentId={studentId} />;
      case "completion":
        return <Student360Completion studentId={studentId} />;
      default:
        return <Student360Overview studentId={studentId} />;
    }
  };
  return (
    <BentoGrid data-screen="student-360" data-tab={current.key}>
      <BentoTile variant="accent">
        <h1>Student 360</h1>
        <nav className="sb-actions-row" aria-label="Student 360 sections">
          {STUDENT_360_TABS.map((t) => (
            <NavLink key={t.key} to={student360Path(studentId, t.key)} aria-current={t.key === current.key ? "page" : undefined}>
              {t.label}
            </NavLink>
          ))}
        </nav>
      </BentoTile>
      {body()}
    </BentoGrid>
  );
}
