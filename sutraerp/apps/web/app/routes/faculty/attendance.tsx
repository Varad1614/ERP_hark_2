import { RoleScreen } from "../../features/academic/RoleScreen";
import { FacultyAttendanceScreen } from "../../features/academic/delivery";

export function meta() {
  return [{ title: "Attendance · SutraERP" }];
}

/** Faculty Attendance (`/attendance`). Slice 4B owns this frame (docs/batch4-contracts.md §10). */
export default function Screen() {
  return (
    <RoleScreen screen="attendance" roles={["faculty"]} title="Attendance" note="is the faculty view; administrators use Academic delivery → Attendance.">
      {() => <FacultyAttendanceScreen />}
    </RoleScreen>
  );
}
