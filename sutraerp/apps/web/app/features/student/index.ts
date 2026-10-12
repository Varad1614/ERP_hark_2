/**
 * Batch 3 (Domain C) web feature — tab definitions (foundation-owned, docs/batch3-contracts.md §10).
 *
 * Each slice owns one folder and fills the placeholder components already wired into the routes:
 * `record/` (3A), `registration/` (3B), `completion/` (3C). Route modules stay thin.
 */
export { activeTab, type TabDef } from "../institution";

/** Tabs of Admin → Students (`/students/:tab?`, first = default) with their owning slice. */
export const STUDENTS_TABS = [
  { key: "directory", label: "Students", slice: "3A" },
  { key: "admissions", label: "Admissions", slice: "3A" },
  { key: "registration", label: "Registration", slice: "3B" },
  { key: "progression", label: "Progression", slice: "3B" },
  { key: "exits", label: "Completion & exits", slice: "3C" },
  { key: "guardians", label: "Guardians", slice: "3C" },
  { key: "scholarships", label: "Scholarships", slice: "3C" },
] as const;

/** Tabs of Student 360 (`/students/record/:studentId/:tab?`) with their owning slice. */
export const STUDENT_360_TABS = [
  { key: "overview", label: "Overview", slice: "3A" },
  { key: "enrolment", label: "Enrolment", slice: "3A" },
  { key: "documents", label: "Documents", slice: "3A" },
  { key: "registration", label: "Registration", slice: "3B" },
  { key: "progression", label: "Progression & credits", slice: "3B" },
  { key: "guardians", label: "Guardians", slice: "3C" },
  { key: "scholarships", label: "Scholarships", slice: "3C" },
  { key: "completion", label: "Completion & exit", slice: "3C" },
] as const;

/** Path of a student's 360 view. */
export function student360Path(studentId: string, tab?: string): string {
  return `/students/record/${studentId}${tab ? `/${tab}` : ""}`;
}
