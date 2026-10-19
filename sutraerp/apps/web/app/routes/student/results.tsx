import { StudentResultsPanel } from "../../features/exams/results";
import { StudentExamPanel } from "../../features/exams/setup";
import { ExamRoleGate } from "../../features/exams/ui";

export function meta() {
  return [{ title: "Results · SutraERP" }];
}

/**
 * Student Results (`/results`). Re-skin v2 (R4, prototype `stu-results.png`): AreaHeader with a
 * tab per published result, KPIs, grade table; 6A "My examinations" (registration, hall ticket,
 * timetable) as the secondary tile beside "Grade cards & reviews".
 */
export default function Results() {
  return (
    <ExamRoleGate screen="results" roles={["student"]} title="Results" note="is the students’ view; staff use Examinations.">
      {() => <StudentResultsPanel aside={(span) => <StudentExamPanel span={span} />} />}
    </ExamRoleGate>
  );
}
