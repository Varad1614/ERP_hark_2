import { index, layout, route, type RouteConfig } from "@react-router/dev/routes";

export default [
  route("sign-in", "routes/sign-in.tsx"),
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
    // One route module serves every workspace in the catalog (99); unknown keys render a 404 tile.
    route("workspaces/:key/:tab?", "routes/workspace.tsx"),
    // Role navigation screens (students, finance, timetable, …) — stubs until their batch lands.
    route(":screen", "routes/screen.tsx"),
  ]),
] satisfies RouteConfig;
