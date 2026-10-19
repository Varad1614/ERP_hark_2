import { RoleScreen } from "../../features/academic/RoleScreen";
import { StudentCoursesScreen } from "../../features/academic/offering";

export function meta() {
  return [{ title: "Courses · SutraERP" }];
}

/** Student Courses (`/courses`). Slice 4A owns this frame (docs/batch4-contracts.md §10). */
export default function Screen() {
  return (
    <RoleScreen screen="courses" roles={["student"]} title="Courses" note="is the students’ view; staff use Academic delivery → Offerings.">
      {() => <StudentCoursesScreen />}
    </RoleScreen>
  );
}
