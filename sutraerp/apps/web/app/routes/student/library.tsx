import { RoleScreen } from "../../features/academic/RoleScreen";
import { StudentLibraryScreen } from "../../features/academic/flexible";

export function meta() {
  return [{ title: "Library · SutraERP" }];
}

/** Student Library links (`/library`). Slice 4C owns this frame (docs/batch4-contracts.md §10). */
export default function Screen() {
  return (
    <RoleScreen screen="library" roles={["student"]} title="Library" note="is the students’ view of course resources and library links.">
      {() => <StudentLibraryScreen />}
    </RoleScreen>
  );
}
