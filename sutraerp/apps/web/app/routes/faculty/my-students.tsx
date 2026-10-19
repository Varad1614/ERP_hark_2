import { RoleScreen } from "../../features/academic/RoleScreen";
import { FacultyStudentsScreen } from "../../features/academic/offering";

export function meta() {
  return [{ title: "My students · SutraERP" }];
}

/** Faculty My students (`/my-students`). Slice 4A owns this frame (docs/batch4-contracts.md §10). */
export default function Screen() {
  return (
    <RoleScreen screen="my-students" roles={["faculty"]} title="My students" note="lists the students of the sections a faculty member teaches; administrators use Students.">
      {() => <FacultyStudentsScreen />}
    </RoleScreen>
  );
}
