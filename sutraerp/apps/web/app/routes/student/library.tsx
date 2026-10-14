import { RoleScreen } from "../../features/academic/RoleScreen";
import { StudentLibraryScreen } from "../../features/academic/flexible";
import { MyLibraryPanel } from "../../features/campus/library";

export function meta() {
  return [{ title: "Library · SutraERP" }];
}

/**
 * Student Library (`/library`): the Batch 8 library panel (catalogue, loans, reservations, fines —
 * slice 8C, `MyLibraryPanel`) above the Batch 4 course resources and library links (slice 4C,
 * docs/batch4-contracts.md §10). Foundation frame since Batch 8 (docs/batch8-contracts.md §10).
 */
export default function Screen() {
  return (
    <RoleScreen screen="library" roles={["student"]} title="Library" heading={() => "Your library loans, reservations and fines; reading lists, library links and flexible learning from your courses."} note="is the students’ view of the library and course resources.">
      {() => (
        <>
          <MyLibraryPanel />
          <StudentLibraryScreen />
        </>
      )}
    </RoleScreen>
  );
}
