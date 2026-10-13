import { RoleScreen } from "../../features/academic/RoleScreen";
import { FacultyTimetableScreen, StudentTimetableScreen } from "../../features/academic/delivery";

export function meta() {
  return [{ title: "Timetable · SutraERP" }];
}

/** Student and faculty Timetable (`/timetable`), switched by role. Slice 4B owns this frame (docs/batch4-contracts.md §10). */
export default function Timetable() {
  return (
    <RoleScreen screen="timetable" roles={["student", "faculty"]} title="Timetable" heading={(r) => (r === "faculty" ? "Your teaching week, slot by slot." : "Your week of classes, and your attendance in each course.")} note="is the student and faculty view; administrators use Academic delivery → Timetable.">
      {(role) => (role === "faculty" ? <FacultyTimetableScreen /> : <StudentTimetableScreen />)}
    </RoleScreen>
  );
}
