import { RoleScreen } from "../../features/academic/RoleScreen";
import { StudentResultsPanel } from "../../features/exams/results";
import { StudentExamPanel } from "../../features/exams/setup";

export function meta() {
  return [{ title: "Results · SutraERP" }];
}

/**
 * Student Results (`/results`). Foundation frame (docs/batch6-contracts.md §10): 6C renders
 * published results, grade cards and revaluation requests, 6A the registration / hall ticket /
 * exam timetable panel; neither edits this file.
 */
export default function Results() {
  return (
    <RoleScreen screen="results" roles={["student"]} title="Results" note="is the students’ view; staff use Examinations." heading={() => "Examination registration, hall tickets, results and grade cards"}>
      {() => (
        <>
          <StudentResultsPanel />
          <StudentExamPanel />
        </>
      )}
    </RoleScreen>
  );
}
