import { AreaHeader, BentoGrid, BentoTile, MenuButton, StatusBadge, type MenuItem } from "@sutra/ui";
import { ChevronDown } from "lucide-react";
import { Link, useParams } from "react-router";
import { useSession } from "../../lib/auth";
import { activeTab, STUDENT_360_TABS, student360Path } from "../../features/student";
import { TabLink } from "../../features/student/StudentsArea";
import { Student360Completion, Student360Guardians, Student360Scholarships } from "../../features/student/completion";
import {
  FeeBadge,
  Student360Academics,
  Student360Attendance,
  Student360Documents,
  Student360Enrolment,
  Student360Fees,
  Student360Overview,
  useStudent,
  useStudentFigures,
  useStudentRecordActions,
} from "../../features/student/record";
import { useProgrammeNames } from "../../features/student/record/AdminTabs";
import { Student360Progression, Student360Registration } from "../../features/student/registration";
import { toMenu } from "../../features/student/ui";

export function meta() {
  return [{ title: "Student 360 · SutraERP" }];
}

/**
 * Student 360 (`/students/record/:studentId/:tab?`), re-skin v2 (R2): back link "Students", the
 * student's name, a live meta line (roll no. · programme · semester · fee / status badge), the five
 * prototype tabs right-aligned and a "More" menu with the lifecycle sections and the record
 * actions. Staff only (faculty read-only, administrators get the actions).
 */
export default function Student360() {
  const { studentId = "", tab } = useParams();
  const { role } = useSession();
  const current = activeTab(STUDENT_360_TABS, tab);
  const student = useStudent(role === "student" ? "" : studentId);
  const fig = useStudentFigures(student.data);
  const { name: progName } = useProgrammeNames(fig.inst);
  const rec = useStudentRecordActions(student.data);
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
      case "academics":
        return <Student360Academics studentId={studentId} />;
      case "attendance":
        return <Student360Attendance studentId={studentId} />;
      case "fees":
        return <Student360Fees studentId={studentId} />;
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
  const s = student.data;
  const c = s?.currentContext;
  const main = STUDENT_360_TABS.filter((t) => t.group === "main");
  const record = STUDENT_360_TABS.filter((t) => t.group === "record");
  const inRecord = record.find((t) => t.key === current.key);
  const sections: MenuItem[] = record.map((t, i) => ({
    id: `section-${t.key}`,
    label: t.label,
    group: i === 0 ? "Record" : undefined,
    active: t.key === current.key,
    render: ({ className, close }) => (
      <Link key={t.key} to={student360Path(studentId, t.key)} className={className || undefined} onClick={close} data-tab-link={t.key}>
        {t.label}
      </Link>
    ),
  }));
  const actions = toMenu(rec.actions, rec.busy).map((m, i) => ({ ...m, group: i === 0 ? "Actions" : undefined, separated: i === 0 }));
  const badge =
    s && s.status !== "active" ? <StatusBadge status={s.status} /> : c && c.status !== "active" ? <StatusBadge status={c.status} /> : fig.fee ? <FeeBadge status={fig.fee} /> : null;
  const summary = s ? (
    <span className="st-meta-inline" data-student-meta="">
      {[c?.rollNumber ?? s.studentNumber, c?.programmeId ? progName(c.programmeId) : null, c?.currentTermNo ? `Semester ${c.currentTermNo}` : null].filter(Boolean).join(" · ")}
      {badge && <span aria-hidden="true">·</span>}
      {badge}
    </span>
  ) : student.isLoading ? (
    "Loading student…"
  ) : (
    "Student not found"
  );
  return (
    <BentoGrid data-screen="student-360" data-tab={current.key}>
      <AreaHeader
        title={<span data-student-name="">{s?.displayName ?? (student.isLoading ? "Loading student…" : "Student")}</span>}
        summary={summary}
        back={role === "administrator" ? { label: "Students", to: "/students" } : { label: "My students", to: "/my-students" }}
        tabs={main.map((t) => ({ id: t.key, label: t.label, to: student360Path(studentId, t.key === "overview" ? undefined : t.key) }))}
        activeTab={current.key}
        linkComponent={TabLink}
        tabsLabel="Student 360 sections"
        actions={
          <MenuButton
            label="More student sections"
            items={[...sections, ...actions]}
            trigger={(p) => (
              <button type="button" className={`sb-tab-more${inRecord ? " on" : ""}`} data-tab-more="" data-student-more="" {...p}>
                {inRecord ? `More · ${inRecord.label}` : "More"}
                <ChevronDown strokeWidth={2} aria-hidden="true" />
              </button>
            )}
          />
        }
      />
      {body()}
      {rec.ui}
    </BentoGrid>
  );
}
