import { RoleScreen } from "../../features/academic/RoleScreen";
import { FacultyClassesScreen } from "../../features/academic/offering";

export function meta() {
  return [{ title: "My classes · SutraERP" }];
}

/** Faculty My classes (`/classes`). Slice 4A owns this frame (docs/batch4-contracts.md §10). */
export default function Screen() {
  return (
    <RoleScreen screen="classes" roles={["faculty"]} title="My classes" note="is the faculty view of the sections they teach.">
      {() => <FacultyClassesScreen />}
    </RoleScreen>
  );
}
