import { FacultyAssessmentsPanel } from "../../features/exams/marks";
import { ExamRoleGate } from "../../features/exams/ui";

export function meta() {
  return [{ title: "Assessments · SutraERP" }];
}

/**
 * Faculty Assessments (`/assessments`). Re-skin v2 (R4, prototype `fac-assessments.png`):
 * AreaHeader with a live summary, KPIs and the papers as `.rc` rows; a row opens its mark sheet.
 */
export default function Assessments() {
  return (
    <ExamRoleGate screen="assessments" roles={["faculty"]} title="Assessments" note="is the faculty view; the exam cell uses Examinations.">
      {() => <FacultyAssessmentsPanel />}
    </ExamRoleGate>
  );
}
