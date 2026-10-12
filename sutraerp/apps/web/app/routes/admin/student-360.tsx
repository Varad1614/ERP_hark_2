import { BentoGrid, BentoTile } from "@sutra/ui";
import { useQuery } from "@tanstack/react-query";
import { GraduationCap } from "lucide-react";
import { Link, NavLink, useParams } from "react-router";
import { useSession } from "../../lib/auth";
import { StatusBadge } from "../../features/institution/hooks";
import { rkeys, useRecordApi } from "../../features/student/record/hooks";
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
  const api = useRecordApi();
  // Same query key as the Overview tab, so the header and the tab share one request.
  const student = useQuery({ queryKey: rkeys.scoped("student", studentId), queryFn: () => api.getStudent(studentId), enabled: !!studentId && role !== "student" });
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
      <BentoTile span={12} variant="accent">
        <div className="sb-page-head">
          <div>
            <span className="sb-chip">
              <GraduationCap strokeWidth={1.75} /> Student 360
            </span>
            <h1 data-student-name>{student.data?.displayName ?? (student.isLoading ? "Loading student…" : "Student")}</h1>
            {student.data && (
              <p>
                {student.data.studentNumber}
                {student.data.currentContext?.enrolmentNumber ? ` · ${student.data.currentContext.enrolmentNumber}` : ""} <StatusBadge status={student.data.status} />
              </p>
            )}
          </div>
          {role === "administrator" && (
            <Link to="/students" className="sb-chip" style={{ marginLeft: "auto" }}>
              All students
            </Link>
          )}
        </div>
        <nav className="sb-tabs" aria-label="Student 360 sections">
          {STUDENT_360_TABS.map((t) => (
            <NavLink
              key={t.key}
              to={student360Path(studentId, t.key)}
              className={t.key === current.key ? "on" : ""}
              aria-current={t.key === current.key ? "page" : undefined}
              data-tab-link={t.key}
            >
              {t.label}
            </NavLink>
          ))}
        </nav>
      </BentoTile>
      {body()}
    </BentoGrid>
  );
}
