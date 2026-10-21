/**
 * Batch 3 (Domain C) web feature — tab definitions (foundation-owned, docs/batch3-contracts.md §10).
 *
 * Each slice owns one folder and fills the placeholder components already wired into the routes:
 * `record/` (3A), `registration/` (3B), `completion/` (3C). Route modules stay thin.
 */
export { activeTab, type TabDef } from "../institution";

/** Tabs of Admin → Students (`/students/:tab?`, first = default) with their owning slice. */
export const STUDENTS_TABS = [
  { key: "directory", label: "Directory", slice: "3A" },
  { key: "admissions", label: "Admissions", slice: "3A" },
  { key: "registration", label: "Registration", slice: "3B" },
  { key: "progression", label: "Progression", slice: "3B" },
  { key: "exits", label: "Completion & exits", slice: "3C" },
  { key: "guardians", label: "Guardians", slice: "3C" },
  { key: "scholarships", label: "Scholarships", slice: "3C" },
] as const;

/**
 * Tabs of Student 360 (`/students/record/:studentId/:tab?`). The first five are the prototype's
 * header tabs (Overview / Academics / Attendance / Fees / Documents); the `record` ones are the
 * lifecycle sections reached from the header "More" menu (re-skin v2, R2).
 */
export const STUDENT_360_TABS = [
  { key: "overview", label: "Overview", slice: "3A", group: "main" },
  { key: "academics", label: "Academics", slice: "R2", group: "main" },
  { key: "attendance", label: "Attendance", slice: "R2", group: "main" },
  { key: "fees", label: "Fees", slice: "R2", group: "main" },
  { key: "documents", label: "Documents", slice: "3A", group: "main" },
  { key: "enrolment", label: "Enrolment", slice: "3A", group: "record" },
  { key: "registration", label: "Registration", slice: "3B", group: "record" },
  { key: "progression", label: "Progression & credits", slice: "3B", group: "record" },
  { key: "guardians", label: "Guardians", slice: "3C", group: "record" },
  { key: "scholarships", label: "Scholarships", slice: "3C", group: "record" },
  { key: "completion", label: "Completion & exit", slice: "3C", group: "record" },
] as const;

/**
 * Audit C3 (M-2): Student 360 sections faculty never see — they read only the students they
 * teach (`student.read.assigned`); enrolment, documents, guardians, scholarships and completion
 * stay with the student office (`student.read`, the API answers 403). Registration and
 * progression are hidden too: their tabs also read options, holds, summary, ledger, breaks,
 * transfers and ABC sync, which remain on `student.read`.
 */
export const FACULTY_HIDDEN_360_TABS: readonly string[] = ["enrolment", "documents", "guardians", "scholarships", "completion", "registration", "progression"];

/** The Student 360 tabs a role sees (faculty without the admin-only sections). */
export function student360TabsFor(role: string | undefined): readonly (typeof STUDENT_360_TABS)[number][] {
  return role === "faculty" ? STUDENT_360_TABS.filter((t) => !FACULTY_HIDDEN_360_TABS.includes(t.key)) : STUDENT_360_TABS;
}

/** Path of a student's 360 view. */
export function student360Path(studentId: string, tab?: string): string {
  return `/students/record/${studentId}${tab ? `/${tab}` : ""}`;
}
