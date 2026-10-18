import { index, layout, route, type RouteConfig } from "@react-router/dev/routes";

export default [
  route("sign-in", "routes/sign-in.tsx"),
  // 5A public careers page: outside `app-shell`, so it renders without a session.
  route("careers/:slug", "routes/careers.tsx"),
  // Re-skin v2 component catalogue (dev / VITE_UI_CATALOGUE=1 builds only; docs/reskin2-contracts.md §6).
  route("dev/catalogue", "routes/dev/catalogue.tsx"),
  layout("routes/app-shell.tsx", [
    index("routes/home.tsx"),
    route("workspaces", "routes/workspaces.tsx"),
    // Two-step verification (TOTP enrolment); also the landing page when enrolment is required.
    route("account/mfa", "routes/mfa-enrol.tsx"),
    // Batch 1D self-service screens (before `:screen`, which would otherwise catch them).
    route("account/security", "routes/security.tsx"),
    route("notifications", "routes/notifications.tsx"),
    route("notices", "routes/notices.tsx"),
    route("todos", "routes/todos.tsx"),
    route("profile", "routes/profile.tsx"),
    // Batch 2 (Domain B) admin screens — slice 2C fills these files (docs/batch2-contracts.md §8).
    route("academics/:tab?", "routes/admin/academics.tsx"),
    route("campus/:tab?", "routes/admin/campus-setup.tsx"),
    // Batch 3 (Domain C) screens — owners in docs/batch3-contracts.md §10.
    route("students/record/:studentId/:tab?", "routes/admin/student-360.tsx"),
    route("students/:tab?", "routes/admin/students.tsx"),
    route("documents", "routes/student/documents.tsx"),
    route("registration", "routes/student/registration.tsx"),
    // Batch 4 (Domain D) screens — owners in docs/batch4-contracts.md §10.
    route("academic-delivery/:tab?", "routes/admin/academic-delivery.tsx"),
    route("courses", "routes/student/courses.tsx"),
    route("assignments", "routes/student/assignments.tsx"),
    route("library", "routes/student/library.tsx"),
    route("timetable", "routes/shared/timetable.tsx"),
    route("classes", "routes/faculty/classes.tsx"),
    route("my-students", "routes/faculty/my-students.tsx"),
    route("attendance", "routes/faculty/attendance.tsx"),
    route("resources", "routes/faculty/resources.tsx"),
    // Batch 5 (Domain F) screens — owners in docs/batch5-contracts.md §10.
    route("finance/:tab?", "routes/admin/finance.tsx"),
    route("fees", "routes/student/fees.tsx"),
    // Batch 6 (Domain I) screens — owners in docs/batch6-contracts.md §10.
    route("examinations/:tab?", "routes/admin/examinations.tsx"),
    route("assessments", "routes/faculty/assessments.tsx"),
    route("results", "routes/student/results.tsx"),
    // Batch 7 (Domain E) screens — owners in docs/batch7-contracts.md §10.
    route("people/:tab?", "routes/admin/people.tsx"),
    route("leave/:tab?", "routes/faculty/leave.tsx"),
    // Batch 8 (Domains G · H · J) screens — owners in docs/batch8-contracts.md §10. Admin Campus
    // reuses `campus/:tab?` above (Batch 2 setup + Batch 8 tabs); Student Library reuses `library`.
    route("reports/:tab?", "routes/admin/reports.tsx"),
    // Part 2 · S4-ui-import: Admin → Data import (CSV / XLSX migration).
    route("data-import/:tab?", "routes/admin/data-import.tsx"),
    // One route module serves every workspace in the catalog (99); unknown keys render a 404 tile.
    route("workspaces/:key/:tab?", "routes/workspace.tsx"),
    // Role navigation screens (students, finance, timetable, …) — stubs until their batch lands.
    route(":screen", "routes/screen.tsx"),
  ]),
] satisfies RouteConfig;
