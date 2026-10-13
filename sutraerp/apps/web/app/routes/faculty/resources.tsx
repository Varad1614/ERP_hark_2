import { RoleScreen } from "../../features/academic/RoleScreen";
import { FacultyResourcesScreen } from "../../features/academic/flexible";

export function meta() {
  return [{ title: "Resources · SutraERP" }];
}

/** Faculty Resources (`/resources`). Slice 4C owns this frame (docs/batch4-contracts.md §10). */
export default function Screen() {
  return (
    <RoleScreen screen="resources" roles={["faculty"]} title="Resources" heading={() => "Course material you share with the sections you teach."} note="is the faculty view of course material for their sections.">
      {() => <FacultyResourcesScreen />}
    </RoleScreen>
  );
}
