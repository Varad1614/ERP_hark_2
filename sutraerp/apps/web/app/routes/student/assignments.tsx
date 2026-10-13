import { RoleScreen } from "../../features/academic/RoleScreen";
import { StudentAssignmentsScreen } from "../../features/academic/delivery";

export function meta() {
  return [{ title: "Assignments · SutraERP" }];
}

/** Student Assignments (`/assignments`). Slice 4B owns this frame (docs/batch4-contracts.md §10). */
export default function Screen() {
  return (
    <RoleScreen screen="assignments" roles={["student"]} title="Assignments" note="is the students’ coursework view; staff use Academic delivery → Internal assessment.">
      {() => <StudentAssignmentsScreen />}
    </RoleScreen>
  );
}
