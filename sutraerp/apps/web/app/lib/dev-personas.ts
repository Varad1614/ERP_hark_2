/**
 * Audit 2026-10 (D1 · SEC-2 · SEC-7 · M-3): the seeded dev personas exist only in development
 * builds. They are on under `react-router dev` / Vitest (`import.meta.env.DEV`), in a build made
 * with `VITE_DEV_PERSONAS=true` (CI E2E builds), and in the in-browser `mock` mode. Every other
 * build — the default `oidc` build and a plain `api` build — compiles these checks to `false`, so
 * the persona login ids below are tree-shaken out of the bundle.
 *
 * No password ships with them: in `api` mode the dev password is always typed by the user.
 * Read `import.meta.env` at the use site (not through a shared helper call) so Vite can fold the
 * condition at build time.
 */
import type { Role } from "./roles";

/** True when the dev persona picker and the account-menu role switcher may exist. */
export function devPersonasEnabled(): boolean {
  return import.meta.env.DEV || import.meta.env.VITE_DEV_PERSONAS === "true" || import.meta.env.VITE_AUTH_MODE === "mock";
}

/** The seeded dev login id of a persona; `undefined` outside dev-persona builds. */
export function devPersonaLoginId(role: Role): string | undefined {
  return import.meta.env.DEV || import.meta.env.VITE_DEV_PERSONAS === "true" || import.meta.env.VITE_AUTH_MODE === "mock"
    ? { administrator: "admin@sutra.dev", faculty: "meera.shah@sutra.dev", student: "aarav.desai@sutra.dev" }[role]
    : undefined;
}

/** `dev|<login id>` of a dev persona (form defaults in dev builds); "" outside dev-persona builds. */
export function devPersonaSubject(role: Role): string {
  const id = devPersonaLoginId(role);
  return id ? `dev|${id}` : "";
}
