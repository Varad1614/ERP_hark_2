import { useQuery } from "@tanstack/react-query";
import { ApiProblem, createSutraClient, isProblem, type Workspace, type WorkspaceList } from "@sutra/api-client";
import catalogJson from "../../../../catalog/workspaces.json";
import type { Role } from "./roles";

/**
 * `mock`: everything resolved in the browser. `api`: sutra-api (same origin) with the dev
 * authenticator (login id + password, then TOTP). `oidc`: sutra-api in Keycloak mode — sign-in is
 * a redirect to `GET /api/v1/auth/oidc/start`.
 *
 * Audit 2026-10 (C1/M10): a build without `VITE_AUTH_MODE` is `oidc` (real sign-in); only the
 * dev server / Vitest (`import.meta.env.DEV`) fall back to `mock`.
 */
export type AuthMode = "mock" | "api" | "oidc";
export const AUTH_MODE: AuthMode =
  import.meta.env.VITE_AUTH_MODE === "api"
    ? "api"
    : import.meta.env.VITE_AUTH_MODE === "oidc"
      ? "oidc"
      : import.meta.env.VITE_AUTH_MODE === "mock" || import.meta.env.DEV
        ? "mock"
        : "oidc";
/** True when the API (not the in-browser mock) is the source of truth. */
export const USES_API = AUTH_MODE !== "mock";

/**
 * Synchronizer CSRF token of the current server-side session (ADR-L5-0038). Kept in memory only
 * (never in storage): it is re-read from `GET /api/v1/auth/session` on every page load and sent
 * as `X-CSRF-Token` on POST/PUT/PATCH/DELETE. Tenant scope is never sent; the API takes it from
 * the session.
 */
let csrfToken: string | undefined;
export function setCsrfToken(token: string | undefined) {
  csrfToken = token;
}
export function getCsrfToken(): string | undefined {
  return csrfToken;
}

export const api = createSutraClient({ baseUrl: import.meta.env.VITE_API_BASE ?? "", csrfToken: getCsrfToken });

type FetchOptions = { fetch?: (input: Request) => Promise<Response>; baseUrl?: string };

/**
 * Login-CSRF bootstrap: fetches the pre-session double-submit token (`GET /api/v1/auth/csrf`,
 * which also sets the `__Host-sutra_login_csrf` cookie) and makes it the token the client sends
 * on the following login / MFA-verify requests. Replaced by the session token after sign-in.
 */
export async function bootstrapLoginCsrf(opts: FetchOptions = {}): Promise<string> {
  const res = unwrap(await api.GET("/api/v1/auth/csrf", opts));
  setCsrfToken(res.csrfToken);
  return res.csrfToken;
}

/**
 * A same-origin path to return to after sign-in, else `/` (no protocol-relative `//host`, no
 * absolute URLs, no backslash tricks; the API re-validates `returnTo` too).
 */
export function safeReturnTo(raw: string | null | undefined): string {
  if (!raw || !raw.startsWith("/") || raw.startsWith("//") || raw.startsWith("/\\") || raw.includes("\\")) return "/";
  if (raw.startsWith("/sign-in")) return "/";
  return raw;
}

/** URL that starts the Keycloak (OIDC) sign-in for `returnTo` (same-origin path). */
export function oidcStartUrl(returnTo = "/", stepUp = false): string {
  const q = new URLSearchParams({ returnTo: safeReturnTo(returnTo) });
  if (stepUp) q.set("stepUp", "true");
  return `${import.meta.env.VITE_API_BASE ?? ""}/api/v1/auth/oidc/start?${q.toString()}`;
}

/** Shared copy of `catalog/workspaces.json` (the same file the API embeds). */
export const CATALOG = catalogJson as unknown as Workspace[];

export function filterCatalog(role: Role, catalog: Workspace[] = CATALOG): WorkspaceList {
  const items = catalog
    .filter((w) => w.roles.includes(role))
    .map((w) => ({ ...w, workflows: w.workflows.filter((f) => f.roles.includes(role)) }));
  return { items, total: items.length, workflowTotal: items.reduce((n, w) => n + w.workflows.length, 0) };
}

export function unwrap<T>(result: { data?: T; error?: unknown }): T {
  if (result.error !== undefined) {
    if (isProblem(result.error)) throw new ApiProblem(result.error);
    throw new Error("Request failed");
  }
  if (result.data === undefined) throw new Error("Empty response");
  return result.data;
}

export function useWorkspaces(role: Role) {
  return useQuery({
    queryKey: ["platform", "workspaces", role, AUTH_MODE],
    queryFn: async (): Promise<WorkspaceList> =>
      USES_API ? unwrap(await api.GET("/api/v1/platform/workspaces", { params: { query: { role } } })) : filterCatalog(role),
  });
}
