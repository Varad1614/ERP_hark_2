/**
 * Slice **3A** web components (admissions, student record, Student 360, Student Documents).
 * The routes import these names (docs/batch3-contracts.md §10); the API layer is `./api`
 * (HTTP) / `./mock` (mock mode), React glue in `./hooks`.
 */
export { AdmissionsTab, StudentsDirectoryTab } from "./AdminTabs";
export { Student360Academics, Student360Attendance, Student360Documents, Student360Enrolment, Student360Fees, Student360Overview, useStudent, useStudentFigures, useStudentRecordActions } from "./Student360Tabs";
export { FeeBadge, NewAdmissionDialog } from "./AdminTabs";
export { StudentDocumentsScreen } from "./StudentDocuments";
export { RecordApiProvider, useRecordApi } from "./hooks";
