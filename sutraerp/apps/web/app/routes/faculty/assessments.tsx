import { RoleScreen } from "../../features/academic/RoleScreen";
import { FacultyAssessmentsPanel } from "../../features/exams/marks";

export function meta() {
  return [{ title: "Assessments · SutraERP" }];
}

/**
 * Faculty Assessments (`/assessments`). Foundation frame (docs/batch6-contracts.md §10): 6B renders
 * marks entry and the gradebook for the papers of the sections the faculty member teaches.
 */
export default function Assessments() {
  return (
    <RoleScreen screen="assessments" roles={["faculty"]} title="Assessments" note="is the faculty view; the exam cell uses Examinations." heading={() => "Examination marks and the gradebook for the sections you teach"}>
      {() => <FacultyAssessmentsPanel />}
    </RoleScreen>
  );
}
